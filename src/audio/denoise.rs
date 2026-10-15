//! Optional local noise suppression with RNNoise (pure-Rust `nnnoiseless`).
//!
//! RNNoise works on 10 ms frames of 48 kHz audio scaled to the 16-bit
//! range. Audio is resampled to 48 kHz when needed, denoised, and returned
//! at 48 kHz; the caller then converts to the 16 kHz model rate. Output is
//! delayed by up to one frame (10 ms) while a frame fills.

use nnnoiseless::DenoiseState;

use super::resample::Resampler;

pub const DENOISE_RATE: u32 = 48_000;
const FRAME: usize = DenoiseState::FRAME_SIZE;

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    to_48k: Option<Resampler>,
    pending: Vec<f32>,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
    resampled: Vec<f32>,
}

impl Denoiser {
    /// `input_rate` is the rate of the audio passed to [`Denoiser::process`].
    pub fn new(input_rate: u32) -> Denoiser {
        Denoiser {
            state: DenoiseState::new(),
            to_48k: (input_rate != DENOISE_RATE).then(|| Resampler::new(input_rate, DENOISE_RATE)),
            pending: Vec::with_capacity(FRAME * 4),
            frame_in: vec![0.0; FRAME],
            frame_out: vec![0.0; FRAME],
            resampled: Vec::with_capacity(FRAME * 4),
        }
    }

    pub fn reset(&mut self) {
        self.state = DenoiseState::new();
        if let Some(r) = self.to_48k.as_mut() {
            r.reset();
        }
        self.pending.clear();
    }

    /// Denoise `input`; appends 48 kHz output to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        match self.to_48k.as_mut() {
            Some(r) => {
                self.resampled.clear();
                r.process(input, &mut self.resampled);
                self.pending.extend_from_slice(&self.resampled);
            }
            None => self.pending.extend_from_slice(input),
        }
        let mut consumed = 0;
        while self.pending.len() - consumed >= FRAME {
            for (dst, src) in self.frame_in.iter_mut().zip(&self.pending[consumed..consumed + FRAME]) {
                *dst = src * 32768.0;
            }
            self.state.process_frame(&mut self.frame_out, &self.frame_in);
            out.extend(self.frame_out.iter().map(|v| (v / 32768.0).clamp(-1.0, 1.0)));
            consumed += FRAME;
        }
        self.pending.drain(..consumed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_length_and_reduces_steady_noise() {
        let mut d = Denoiser::new(48_000);
        // Deterministic pseudo-random white noise.
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..48_000 * 2)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.1
            })
            .collect();
        let mut out = Vec::new();
        for c in noise.chunks(480 * 3 + 7) {
            d.process(c, &mut out);
        }
        assert!(out.len() <= noise.len() && noise.len() - out.len() < FRAME);
        let tail = &out[out.len() / 2..];
        let in_rms = super::super::dsp::rms_dbfs(&noise[noise.len() / 2..]);
        let out_rms = super::super::dsp::rms_dbfs(tail);
        assert!(out_rms < in_rms - 6.0, "noise {in_rms} dB -> {out_rms} dB");
    }

    #[test]
    fn resamples_other_rates() {
        let mut d = Denoiser::new(16_000);
        let mut out = Vec::new();
        d.process(&vec![0.0; 16_000], &mut out);
        // 1 s at 16 kHz becomes ~1 s at 48 kHz (minus filter/frame latency).
        assert!(out.len() > 46_000 && out.len() <= 48_000, "{}", out.len());
    }
}
