//! NeMo-compatible log-mel front end for Parakeet / FastConformer models.
//!
//! Re-implements NeMo's `AudioToMelSpectrogramPreprocessor` (inference mode)
//! as configured for Parakeet: 16 kHz input, pre-emphasis 0.97, 25 ms
//! symmetric Hann window (400 samples) centred in a 512-point FFT, 10 ms hop,
//! zero ("constant") padding of n_fft/2 on both sides, power spectrum,
//! Slaney-normalised mel filter bank (librosa defaults, 0-8 kHz), natural
//! log with a 2^-24 guard, and per-feature (per mel bin) mean/std
//! normalisation with Bessel's correction and a 1e-5 floor. No dither is
//! applied (NeMo only dithers in training mode).
//!
//! Like NeMo, the STFT yields `N / 160 + 1` frames but only the first
//! `N / 160` are valid (`get_seq_len`); statistics use the valid frames and
//! the remaining frame is zeroed. Verified numerically against the
//! `nemo128.onnx` / `nemo80.onnx` preprocessor exports shipped with
//! onnx-asr (see tests/feature_parity.rs).

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = 512;
pub const WIN_LENGTH: usize = 400;
pub const HOP: usize = 160;
const PREEMPH: f32 = 0.97;
const LOG_GUARD: f32 = 5.960_464_5e-8; // 2^-24
const STD_FLOOR: f32 = 1e-5;
const N_BINS: usize = N_FFT / 2 + 1;

/// One triangular mel filter, stored sparsely.
struct MelFilter {
    first_bin: usize,
    weights: Vec<f32>,
}

pub struct NemoMel {
    n_mels: usize,
    window: Vec<f32>,
    filters: Vec<MelFilter>,
    fft: Arc<dyn RealToComplex<f32>>,
    frame: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    power: Vec<f32>,
}

impl NemoMel {
    pub fn new(n_mels: usize) -> NemoMel {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N_FFT);
        let frame = fft.make_input_vec();
        let spectrum = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        NemoMel {
            n_mels,
            window: padded_hann(),
            filters: slaney_mel_filters(n_mels),
            fft,
            frame,
            spectrum,
            scratch,
            power: vec![0.0; N_BINS],
        }
    }

    pub fn n_mels(&self) -> usize {
        self.n_mels
    }

    /// Number of STFT frames NeMo computes for `n` samples.
    pub fn num_frames(n: usize) -> usize {
        n / HOP + 1
    }

    /// Number of valid frames (NeMo's `seq_len`), at least 1.
    pub fn valid_frames(n: usize) -> usize {
        (n / HOP).max(1)
    }

    /// Features laid out as `[n_mels][frames]` (i.e. `[1, n_mels, T]`),
    /// plus the valid length to pass to the encoder.
    pub fn compute(&mut self, audio: &[f32]) -> (Vec<f32>, usize, usize) {
        let n = audio.len();
        let frames = Self::num_frames(n);
        let valid = Self::valid_frames(n).min(frames);
        // Pre-emphasis, then zero padding of N_FFT/2 on both sides.
        let pad = N_FFT / 2;
        let mut padded = vec![0.0f32; n + 2 * pad];
        if n > 0 {
            padded[pad] = audio[0];
            for i in 1..n {
                padded[pad + i] = audio[i] - PREEMPH * audio[i - 1];
            }
        }
        let mut feats = vec![0.0f32; self.n_mels * frames];
        for t in 0..frames {
            let start = t * HOP;
            for (k, (dst, &w)) in self.frame.iter_mut().zip(&self.window).enumerate() {
                *dst = padded[start + k] * w;
            }
            self.fft
                .process_with_scratch(&mut self.frame, &mut self.spectrum, &mut self.scratch)
                .expect("FFT buffers have the planned sizes");
            for (p, c) in self.power.iter_mut().zip(&self.spectrum) {
                *p = c.re * c.re + c.im * c.im;
            }
            for (m, f) in self.filters.iter().enumerate() {
                let bins = &self.power[f.first_bin..f.first_bin + f.weights.len()];
                let e: f32 = bins.iter().zip(&f.weights).map(|(p, w)| p * w).sum();
                feats[m * frames + t] = (e + LOG_GUARD).ln();
            }
        }
        normalize_per_feature(&mut feats, self.n_mels, frames, valid);
        (feats, frames, valid)
    }
}

/// 400-sample symmetric Hann window, zero-padded to 512 and centred, which
/// is what `torch.stft(win_length=400, n_fft=512)` applies.
fn padded_hann() -> Vec<f32> {
    let mut w = vec![0.0f32; N_FFT];
    let offset = (N_FFT - WIN_LENGTH) / 2;
    for i in 0..WIN_LENGTH {
        let v = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (WIN_LENGTH - 1) as f64).cos();
        w[offset + i] = v as f32;
    }
    w
}

fn hz_to_mel(f: f64) -> f64 {
    // Slaney mel scale (librosa htk=False).
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f64.ln() / 27.0;
    if f >= MIN_LOG_HZ {
        min_log_mel + (f / MIN_LOG_HZ).ln() / logstep
    } else {
        f / F_SP
    }
}

fn mel_to_hz(m: f64) -> f64 {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f64.ln() / 27.0;
    if m >= min_log_mel {
        MIN_LOG_HZ * (logstep * (m - min_log_mel)).exp()
    } else {
        F_SP * m
    }
}

/// librosa.filters.mel(sr=16000, n_fft=512, n_mels, fmin=0, fmax=8000,
/// htk=False, norm="slaney"), stored sparsely.
fn slaney_mel_filters(n_mels: usize) -> Vec<MelFilter> {
    let sr = SAMPLE_RATE as f64;
    let fft_freqs: Vec<f64> = (0..N_BINS).map(|k| k as f64 * sr / N_FFT as f64).collect();
    let (mel_lo, mel_hi) = (hz_to_mel(0.0), hz_to_mel(sr / 2.0));
    let mel_pts: Vec<f64> = (0..n_mels + 2).map(|i| mel_to_hz(mel_lo + (mel_hi - mel_lo) * i as f64 / (n_mels + 1) as f64)).collect();
    let mut filters = Vec::with_capacity(n_mels);
    for i in 0..n_mels {
        let (f0, f1, f2) = (mel_pts[i], mel_pts[i + 1], mel_pts[i + 2]);
        let enorm = 2.0 / (f2 - f0);
        let weights: Vec<f64> = fft_freqs
            .iter()
            .map(|&f| {
                let lower = (f - f0) / (f1 - f0);
                let upper = (f2 - f) / (f2 - f1);
                lower.min(upper).max(0.0) * enorm
            })
            .collect();
        let first = weights.iter().position(|&w| w > 0.0).unwrap_or(0);
        let last = weights.iter().rposition(|&w| w > 0.0).unwrap_or(first);
        filters.push(MelFilter { first_bin: first, weights: weights[first..=last].iter().map(|&w| w as f32).collect() });
    }
    filters
}

fn normalize_per_feature(feats: &mut [f32], n_mels: usize, frames: usize, valid: usize) {
    for m in 0..n_mels {
        let row = &mut feats[m * frames..(m + 1) * frames];
        let (head, tail) = row.split_at_mut(valid);
        let mean = head.iter().map(|&v| v as f64).sum::<f64>() / valid as f64;
        let var = if valid > 1 { head.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / (valid - 1) as f64 } else { 0.0 };
        let std = var.sqrt() as f32 + STD_FLOOR;
        let mean = mean as f32;
        for v in head.iter_mut() {
            *v = (*v - mean) / std;
        }
        // Frames beyond the valid length are padding (NeMo pad_value 0).
        tail.iter_mut().for_each(|v| *v = 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_matches_nemo() {
        assert_eq!(NemoMel::num_frames(16_000), 101);
        assert_eq!(NemoMel::valid_frames(16_000), 100);
        assert_eq!(NemoMel::num_frames(0), 1);
        assert_eq!(NemoMel::valid_frames(159), 1);
        assert_eq!(NemoMel::num_frames(160), 2);
        assert_eq!(NemoMel::valid_frames(320), 2);
    }

    #[test]
    fn slaney_scale_is_linear_below_1khz() {
        assert!((hz_to_mel(1000.0) - 15.0).abs() < 1e-9);
        assert!((hz_to_mel(500.0) - 7.5).abs() < 1e-9);
        for f in [0.0, 300.0, 999.0, 1000.0, 4000.0, 8000.0] {
            assert!((mel_to_hz(hz_to_mel(f)) - f).abs() < 1e-6);
        }
    }

    #[test]
    fn filters_tile_the_spectrum() {
        let filters = slaney_mel_filters(128);
        assert_eq!(filters.len(), 128);
        // Every filter has at least one non-zero weight and they ascend.
        for w in filters.windows(2) {
            assert!(w[1].first_bin >= w[0].first_bin);
        }
        assert!(filters.iter().all(|f| !f.weights.is_empty()));
        let filters80 = slaney_mel_filters(80);
        assert_eq!(filters80.len(), 80);
    }

    #[test]
    fn features_are_normalised_per_bin() {
        let mut mel = NemoMel::new(128);
        let audio: Vec<f32> = (0..16_000)
            .map(|i| 0.3 * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 16_000.0).sin() + 0.01 * ((i * 7919) % 101) as f32 / 101.0)
            .collect();
        let (f, t, valid) = mel.compute(&audio);
        assert_eq!((t, valid), (101, 100));
        assert_eq!(f.len(), 128 * 101);
        for m in [0usize, 20, 64, 127] {
            let row = &f[m * t..m * t + valid];
            let mean: f32 = row.iter().sum::<f32>() / valid as f32;
            assert!(mean.abs() < 1e-3, "bin {m} mean {mean}");
            assert!(row.iter().all(|v| v.is_finite()));
            assert_eq!(f[m * t + valid], 0.0, "padding frame is zeroed");
        }
    }

    #[test]
    fn single_frame_input_is_finite() {
        let mut mel = NemoMel::new(80);
        let (f, t, valid) = mel.compute(&[0.1; 100]);
        assert_eq!((t, valid), (1, 1));
        assert!(f.iter().all(|v| v.is_finite()));
    }
}
