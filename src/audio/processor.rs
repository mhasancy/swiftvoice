//! Device audio -> 16 kHz frames -> VAD -> segmenter.
//!
//! Platform independent so the live pipeline and the benchmark (which feeds
//! WAV files) run exactly the same signal path.

use std::path::PathBuf;

use anyhow::Result;

use super::denoise::{Denoiser, DENOISE_RATE};
use super::dsp;
use super::resample::Resampler;
use super::segmenter::{SegEvent, Segmenter, SegmenterConfig};
use super::vad::{EnergyVad, SileroVad, Vad, FRAME_SAMPLES, SAMPLE_RATE};
use crate::config::VadEngine;

/// Which VAD to build.
#[derive(Debug, Clone, PartialEq)]
pub struct VadChoice {
    pub engine: VadEngine,
    pub silero_model: Option<PathBuf>,
    pub energy_margin_db: f32,
}

/// Build the configured VAD. Falls back to the energy VAD (and says why)
/// when the Silero model is missing or fails to load.
pub fn make_vad(choice: &VadChoice) -> (Box<dyn Vad>, Option<String>) {
    if choice.engine == VadEngine::Silero {
        match &choice.silero_model {
            Some(path) if path.is_file() => match SileroVad::load(path) {
                Ok(v) => return (Box::new(v), None),
                Err(e) => {
                    return (
                        Box::new(EnergyVad::new(choice.energy_margin_db)),
                        Some(format!("Silero VAD failed to load ({e:#}); using the energy detector")),
                    )
                }
            },
            _ => return (
                Box::new(EnergyVad::new(choice.energy_margin_db)),
                Some(
                    "Silero VAD model is not installed; using the energy detector (install it with: swiftvoice models download silero-vad)"
                        .into(),
                ),
            ),
        }
    }
    (Box::new(EnergyVad::new(choice.energy_margin_db)), None)
}

pub struct FrameProcessor {
    input_rate: u32,
    denoiser: Option<Denoiser>,
    resampler: Resampler,
    gain: f32,
    pending: Vec<f32>,
    scratch: Vec<f32>,
    denoised: Vec<f32>,
    vad: Box<dyn Vad>,
    seg: Segmenter,
    level_sum: f32,
    level_count: usize,
    frames: u64,
    first_audio_silent: bool,
}

impl FrameProcessor {
    pub fn new(input_rate: u32, gain_db: f32, noise_suppression: bool, vad: Box<dyn Vad>, seg: SegmenterConfig) -> FrameProcessor {
        let (denoiser, resampler) = if noise_suppression {
            (Some(Denoiser::new(input_rate)), Resampler::new(DENOISE_RATE, SAMPLE_RATE))
        } else {
            (None, Resampler::new(input_rate, SAMPLE_RATE))
        };
        FrameProcessor {
            input_rate,
            denoiser,
            resampler,
            gain: dsp::db_to_gain(gain_db),
            pending: Vec::with_capacity(FRAME_SAMPLES * 8),
            scratch: Vec::with_capacity(4096),
            denoised: Vec::with_capacity(4096),
            vad,
            seg: Segmenter::new(seg),
            level_sum: 0.0,
            level_count: 0,
            frames: 0,
            first_audio_silent: true,
        }
    }

    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    pub fn segmenter(&self) -> &Segmenter {
        &self.seg
    }

    /// Replace the segmenter for a new session, keeping DSP state warm.
    pub fn start_session(&mut self, seg: SegmenterConfig) {
        self.seg = Segmenter::new(seg);
        self.vad.reset();
        self.pending.clear();
        self.frames = 0;
        self.first_audio_silent = true;
    }

    /// Feed mono samples at the device rate; emits segmenter events.
    pub fn push(&mut self, mono: &[f32], out: &mut Vec<SegEvent>) -> Result<()> {
        self.scratch.clear();
        match self.denoiser.as_mut() {
            Some(d) => {
                self.denoised.clear();
                d.process(mono, &mut self.denoised);
                self.resampler.process(&self.denoised, &mut self.scratch);
            }
            None => self.resampler.process(mono, &mut self.scratch),
        }
        dsp::apply_gain(&mut self.scratch, self.gain);
        self.pending.extend_from_slice(&self.scratch);
        self.run_frames(out)
    }

    /// Feed audio that is already 16 kHz mono (pre-roll replay, tests).
    pub fn push_16k(&mut self, samples: &[f32], out: &mut Vec<SegEvent>) -> Result<()> {
        self.pending.extend_from_slice(samples);
        self.run_frames(out)
    }

    fn run_frames(&mut self, out: &mut Vec<SegEvent>) -> Result<()> {
        let mut consumed = 0;
        while self.pending.len() - consumed >= FRAME_SAMPLES {
            let frame = &self.pending[consumed..consumed + FRAME_SAMPLES];
            if self.first_audio_silent && !dsp::is_digital_silence(frame) {
                self.first_audio_silent = false;
            }
            let prob = self.vad.speech_prob(frame)?;
            let db = dsp::rms_dbfs(frame);
            self.level_sum += db;
            self.level_count += 1;
            self.seg.push_frame(frame, prob, out);
            self.frames += 1;
            consumed += FRAME_SAMPLES;
        }
        self.pending.drain(..consumed);
        Ok(())
    }

    /// End the utterance (push-to-talk release / stop).
    pub fn finish(&mut self, out: &mut Vec<SegEvent>) {
        self.seg.finish(out);
    }

    /// Average level since the last call, as a 0..1 meter value.
    pub fn take_level(&mut self) -> Option<f32> {
        if self.level_count == 0 {
            return None;
        }
        let avg = self.level_sum / self.level_count as f32;
        self.level_sum = 0.0;
        self.level_count = 0;
        Some(dsp::meter_level(avg))
    }

    /// Frames processed in this session.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// True if every frame so far was exact digital zero (muted mic or
    /// microphone access blocked by Windows privacy settings).
    pub fn all_silent_so_far(&self) -> bool {
        self.first_audio_silent
    }

    pub fn vad_name(&self) -> &'static str {
        self.vad.name()
    }

    /// Recover the (expensive to load) VAD when the processor is rebuilt.
    pub fn into_vad(self) -> Box<dyn Vad> {
        self.vad
    }
}
