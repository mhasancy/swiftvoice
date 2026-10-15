//! WAV file reading and writing (benchmarks, `--transcribe`, `--record`).

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::resample::resample_all;
use super::vad::SAMPLE_RATE;

/// Decoded audio: mono samples in [-1, 1] and the file's sample rate.
pub struct WavAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl WavAudio {
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate as f64
    }

    /// Resample to the 16 kHz model rate.
    pub fn to_16k(&self) -> Vec<f32> {
        resample_all(&self.samples, self.sample_rate, SAMPLE_RATE)
    }
}

/// Read any PCM (8/16/24/32-bit int) or 32-bit float WAV and downmix to mono.
pub fn read_wav(path: &Path) -> Result<WavAudio> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("cannot open WAV file {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 {
        bail!("{}: invalid WAV header (channels {}, rate {})", path.display(), spec.channels, spec.sample_rate);
    }
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => {
            if spec.bits_per_sample != 32 {
                bail!("{}: {}-bit float WAV is not supported", path.display(), spec.bits_per_sample);
            }
            reader.samples::<f32>().collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            if !(8..=32).contains(&bits) {
                bail!("{}: {bits}-bit integer WAV is not supported", path.display());
            }
            let scale = 1.0 / (1u64 << (bits - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let channels = spec.channels as usize;
    let mut samples = Vec::with_capacity(interleaved.len() / channels);
    super::dsp::downmix_into(&interleaved, channels, &mut samples);
    Ok(WavAudio { samples, sample_rate: spec.sample_rate, channels: spec.channels })
}

/// Write mono 16-bit PCM.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).with_context(|| format!("cannot create {}", path.display()))?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
    }
    w.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = crate::paths::test_dir("wav");
        let path = dir.join("t.wav");
        let tone: Vec<f32> = (0..8000).map(|i| 0.5 * (i as f32 * 0.05).sin()).collect();
        write_wav(&path, &tone, 8000).unwrap();
        let back = read_wav(&path).unwrap();
        assert_eq!(back.sample_rate, 8000);
        assert_eq!(back.samples.len(), 8000);
        assert!((back.duration_secs() - 1.0).abs() < 1e-9);
        for (a, b) in tone.iter().zip(&back.samples) {
            assert!((a - b).abs() < 1.0 / 16000.0);
        }
        let up = back.to_16k();
        assert!((up.len() as i64 - 16000).abs() <= 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reports_missing_files_clearly() {
        let err = read_wav(Path::new("definitely/not/here.wav")).err().unwrap().to_string();
        assert!(err.contains("not/here.wav"), "{err}");
    }
}
