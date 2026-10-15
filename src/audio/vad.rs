//! Voice activity detection on 32 ms frames of 16 kHz audio.
//!
//! * [`SileroVad`]: Silero VAD v5/v6 (MIT licence, 2.3 MB ONNX model).
//!   Neural, robust to keyboard and fan noise. Recommended.
//! * [`EnergyVad`]: adaptive energy detector with a tracked noise floor.
//!   Needs no model; used when the Silero file is absent or as configured.

use std::path::Path;

use anyhow::{bail, Context, Result};
use ort::session::Session;
use ort::value::TensorRef;

use super::dsp::rms_dbfs;

/// Model sample rate and frame size shared by every VAD.
pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME_SAMPLES: usize = 512;
pub const FRAME_MS: u32 = 32;

pub trait Vad: Send {
    /// Probability (0..1) that `frame` (exactly [`FRAME_SAMPLES`] samples)
    /// contains speech.
    fn speech_prob(&mut self, frame: &[f32]) -> Result<f32>;
    /// Forget the recurrent state (call between recordings).
    fn reset(&mut self);
    fn name(&self) -> &'static str;
}

/// Silero VAD v5/v6: input `[1, 64 + 512]` (64 samples of context from the
/// previous frame), state `[2, 1, 128]`, scalar `sr`; outputs a probability.
pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
    context: [f32; CONTEXT],
    input: Vec<f32>,
    names: [String; 3],
}

const CONTEXT: usize = 64;
const STATE_LEN: usize = 2 * 128;

impl SileroVad {
    pub fn load(path: &Path) -> Result<SileroVad> {
        // The VAD model is tiny; one thread avoids contending with ASR.
        let session = crate::asr::runtime::load_session(path, 1, "Silero VAD")?;
        let inputs = crate::asr::runtime::input_names(&session);
        if inputs.len() != 3 || inputs[0] != "input" || inputs[1] != "state" || inputs[2] != "sr" {
            bail!("{} is not a Silero VAD v5/v6 model (inputs {:?}, expected [input, state, sr])", path.display(), inputs);
        }
        let outputs = crate::asr::runtime::output_names(&session);
        if outputs.len() != 2 {
            bail!("{}: unexpected Silero VAD outputs {:?}", path.display(), outputs);
        }
        Ok(SileroVad {
            session,
            state: vec![0.0; STATE_LEN],
            context: [0.0; CONTEXT],
            input: vec![0.0; CONTEXT + FRAME_SAMPLES],
            names: [outputs[0].clone(), outputs[1].clone(), String::new()],
        })
    }
}

impl Vad for SileroVad {
    fn speech_prob(&mut self, frame: &[f32]) -> Result<f32> {
        if frame.len() != FRAME_SAMPLES {
            bail!("VAD frame must be {FRAME_SAMPLES} samples, got {}", frame.len());
        }
        self.input[..CONTEXT].copy_from_slice(&self.context);
        self.input[CONTEXT..].copy_from_slice(frame);
        let sr = [SAMPLE_RATE as i64];
        let outputs = self.session.run(ort::inputs![
            "input" => TensorRef::from_array_view(([1usize, CONTEXT + FRAME_SAMPLES], &self.input[..]))?,
            "state" => TensorRef::from_array_view(([2usize, 1, 128], &self.state[..]))?,
            "sr" => TensorRef::from_array_view(((), &sr[..]))?,
        ])?;
        let (_, prob) = outputs[self.names[0].as_str()].try_extract_tensor::<f32>()?;
        let p = *prob.first().context("Silero VAD returned no probability")?;
        let (_, new_state) = outputs[self.names[1].as_str()].try_extract_tensor::<f32>()?;
        if new_state.len() == STATE_LEN {
            self.state.copy_from_slice(new_state);
        }
        self.context.copy_from_slice(&frame[FRAME_SAMPLES - CONTEXT..]);
        Ok(p)
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|v| *v = 0.0);
        self.context = [0.0; CONTEXT];
    }

    fn name(&self) -> &'static str {
        "silero"
    }
}

/// Adaptive energy VAD.
///
/// Tracks the noise floor (falls quickly to quieter frames, rises slowly)
/// and maps the frame level above that floor to a pseudo-probability with
/// a logistic curve centred on `margin_db`.
pub struct EnergyVad {
    margin_db: f32,
    floor_db: f32,
    frames: u32,
}

impl EnergyVad {
    pub fn new(margin_db: f32) -> EnergyVad {
        EnergyVad { margin_db, floor_db: -60.0, frames: 0 }
    }
}

impl Vad for EnergyVad {
    fn speech_prob(&mut self, frame: &[f32]) -> Result<f32> {
        let db = rms_dbfs(frame);
        // Absolute gate: nothing below -58 dBFS is speech.
        if db < -58.0 {
            let target = db.max(-90.0);
            if target < self.floor_db {
                self.floor_db = 0.9 * self.floor_db + 0.1 * target;
            }
            self.frames = self.frames.saturating_add(1);
            return Ok(0.0);
        }
        if self.frames < 8 {
            // Learn the room for the first quarter second.
            self.floor_db = if self.frames == 0 { db } else { self.floor_db.min(db) };
        } else if db < self.floor_db {
            self.floor_db = 0.7 * self.floor_db + 0.3 * db;
        } else {
            // Rise ~1.5 dB per second so a speaker never becomes "noise".
            self.floor_db += 0.05;
        }
        self.frames = self.frames.saturating_add(1);
        let above = db - self.floor_db;
        Ok(1.0 / (1.0 + (-(above - self.margin_db) * 0.6).exp()))
    }

    fn reset(&mut self) {
        self.floor_db = -60.0;
        self.frames = 0;
    }

    fn name(&self) -> &'static str {
        "energy"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, amp: f32, seed: &mut u32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((*seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amp
            })
            .collect()
    }

    #[test]
    fn energy_vad_separates_loud_from_background() {
        let mut vad = EnergyVad::new(9.0);
        let mut seed = 7;
        // Room noise around -50 dBFS.
        for _ in 0..30 {
            let p = vad.speech_prob(&noise(FRAME_SAMPLES, 0.005, &mut seed)).unwrap();
            assert!(p < 0.2, "background scored {p}");
        }
        // "Speech" 30 dB louder.
        let loud: Vec<f32> = (0..FRAME_SAMPLES).map(|i| 0.2 * (i as f32 * 0.07).sin()).collect();
        assert!(vad.speech_prob(&loud).unwrap() > 0.9);
        // Digital silence is never speech.
        assert_eq!(vad.speech_prob(&[0.0; FRAME_SAMPLES]).unwrap(), 0.0);
    }
}
