//! Small, allocation-free signal helpers used on the audio thread.

/// Average interleaved channels into mono, appending to `out`.
pub fn downmix_into(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let scale = 1.0 / channels as f32;
    out.extend(interleaved.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() * scale));
}

/// Convert decibels to a linear amplitude factor.
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Apply gain with a soft limiter so boosted peaks saturate smoothly instead
/// of hard-clipping (hard clipping creates harmonics that hurt recognition).
pub fn apply_gain(samples: &mut [f32], gain: f32) {
    if (gain - 1.0).abs() < 1e-6 {
        return;
    }
    for s in samples {
        *s = soft_clip(*s * gain);
    }
}

/// Identity below |0.8|, then a tanh knee that approaches ±1.
pub fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        (KNEE + (1.0 - KNEE) * over.tanh()).copysign(x)
    }
}

/// Root-mean-square level in dBFS (-100 for silence).
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -100.0;
    }
    let mean_sq = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    if mean_sq <= 1e-10 {
        -100.0
    } else {
        10.0 * mean_sq.log10()
    }
}

/// Map dBFS to a 0..1 meter value for the overlay (-60 dB .. 0 dB).
pub fn meter_level(dbfs: f32) -> f32 {
    ((dbfs + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// True when every sample is (near) zero: a muted or privacy-blocked mic.
pub fn is_digital_silence(samples: &[f32]) -> bool {
    samples.iter().all(|s| s.abs() < 1e-6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        let mut out = Vec::new();
        downmix_into(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
        out.clear();
        downmix_into(&[0.1, 0.2], 1, &mut out);
        assert_eq!(out, vec![0.1, 0.2]);
    }

    #[test]
    fn gain_and_soft_clip() {
        assert!((db_to_gain(6.0) - 1.995).abs() < 0.01);
        assert!((db_to_gain(-20.0) - 0.1).abs() < 1e-4);
        let mut x = [0.1, -0.2, 0.9, -3.0];
        apply_gain(&mut x, 2.0);
        assert!((x[0] - 0.2).abs() < 1e-6);
        assert!((x[1] + 0.4).abs() < 1e-6);
        assert!(x[2] > 0.8 && x[2] < 1.0);
        assert!(x[3] < -0.8 && x[3] >= -1.0);
        assert_eq!(soft_clip(0.5), 0.5);
    }

    #[test]
    fn levels() {
        assert_eq!(rms_dbfs(&[0.0; 10]), -100.0);
        assert!((rms_dbfs(&[1.0, -1.0]) - 0.0).abs() < 1e-4);
        assert!((rms_dbfs(&[0.1; 100]) + 20.0).abs() < 1e-3);
        assert_eq!(meter_level(-100.0), 0.0);
        assert_eq!(meter_level(0.0), 1.0);
        assert!(is_digital_silence(&[0.0, 0.0]));
        assert!(!is_digital_silence(&[0.0, 0.01]));
    }
}
