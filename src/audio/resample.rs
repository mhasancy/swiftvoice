//! Streaming band-limited resampler (windowed sinc, arbitrary ratio).
//!
//! Microphones usually run at 44.1 or 48 kHz while the speech models expect
//! 16 kHz. Plain decimation would alias everything above 8 kHz back into the
//! speech band, so this resampler low-pass filters with a Blackman-windowed
//! sinc kernel (16 zero crossings per side) sampled into a lookup table. The
//! output is identical no matter how the input is split into chunks.

const TABLE_RES: usize = 512;
const ZERO_CROSSINGS: f64 = 16.0;
const ROLLOFF: f64 = 0.94;

pub struct Resampler {
    in_rate: u32,
    out_rate: u32,
    /// Input samples advanced per output sample.
    step: f64,
    half_width: usize,
    table: Vec<f32>,
    /// Kernel scale (cutoff relative to the input Nyquist).
    cutoff: f64,
    buf: Vec<f32>,
    /// Position of the next output sample, in `buf` coordinates.
    pos: f64,
    flushed: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Resampler {
        assert!(in_rate > 0 && out_rate > 0, "sample rates must be positive");
        let step = in_rate as f64 / out_rate as f64;
        let cutoff = (out_rate as f64 / in_rate as f64).min(1.0) * ROLLOFF;
        let half_width = (ZERO_CROSSINGS / cutoff).ceil() as usize;
        let n = half_width * TABLE_RES + 2;
        let mut table = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64 / TABLE_RES as f64;
            table.push(kernel(t, cutoff, half_width as f64) as f32);
        }
        Resampler { in_rate, out_rate, step, half_width, table, cutoff, buf: vec![0.0; half_width], pos: half_width as f64, flushed: false }
    }

    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    pub fn is_passthrough(&self) -> bool {
        self.in_rate == self.out_rate
    }

    /// Forget all history (e.g. when a new recording starts).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.buf.resize(self.half_width, 0.0);
        self.pos = self.half_width as f64;
        self.flushed = false;
    }

    /// Resample `input` and append the result to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        self.drain(out);
    }

    /// Emit the samples still held back for the filter's look-ahead.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if self.is_passthrough() || self.flushed {
            return;
        }
        self.flushed = true;
        let input_end = self.buf.len() as f64;
        self.buf.extend(std::iter::repeat_n(0.0, self.half_width + 1));
        while self.pos < input_end {
            out.push(self.sample_at(self.pos));
            self.pos += self.step;
        }
    }

    fn drain(&mut self, out: &mut Vec<f32>) {
        let w = self.half_width;
        while (self.pos.floor() as usize) + w < self.buf.len() {
            out.push(self.sample_at(self.pos));
            self.pos += self.step;
        }
        // Drop input that no future output sample can reach.
        let keep_from = (self.pos.floor() as usize).saturating_sub(w);
        if keep_from > 4096 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }

    fn sample_at(&self, p: f64) -> f32 {
        let w = self.half_width as isize;
        let base = p.floor() as isize;
        let frac = p - base as f64;
        let mut acc = 0.0f64;
        let lo = (base - w + 1).max(0);
        let hi = (base + w).min(self.buf.len() as isize - 1);
        for k in lo..=hi {
            let t = (frac - (k - base) as f64).abs();
            let idx = t * TABLE_RES as f64;
            let i = idx as usize;
            if i + 1 >= self.table.len() {
                continue;
            }
            let a = (idx - i as f64) as f32;
            let h = self.table[i] * (1.0 - a) + self.table[i + 1] * a;
            acc += (self.buf[k as usize] * h) as f64;
        }
        acc as f32
    }

    /// Cut-off frequency in Hz (for diagnostics and tests).
    pub fn cutoff_hz(&self) -> f64 {
        self.cutoff * self.in_rate as f64 / 2.0
    }
}

/// Windowed-sinc low-pass kernel evaluated at distance `t` input samples.
fn kernel(t: f64, cutoff: f64, half_width: f64) -> f64 {
    if t >= half_width {
        return 0.0;
    }
    let x = cutoff * t;
    let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
    let u = t / half_width;
    let window = 0.42 + 0.5 * (std::f64::consts::PI * u).cos() + 0.08 * (2.0 * std::f64::consts::PI * u).cos();
    cutoff * sinc * window
}

/// Convenience for whole buffers (WAV files, tests).
pub fn resample_all(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    let mut r = Resampler::new(in_rate, out_rate);
    let mut out = Vec::with_capacity((input.len() as f64 * out_rate as f64 / in_rate as f64) as usize + 16);
    r.process(input, &mut out);
    r.flush(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, secs: f64, amp: f32) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n).map(|i| amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Goertzel magnitude of `freq` in `x`, normalised to sine amplitude.
    fn tone_amplitude(x: &[f32], rate: u32, freq: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / rate as f64;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s0 = v as f64 + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
        2.0 * power.sqrt() / x.len() as f64
    }

    #[test]
    fn passthrough_is_exact() {
        let x = sine(440.0, 16000, 0.1, 0.5);
        assert_eq!(resample_all(&x, 16000, 16000), x);
    }

    #[test]
    fn output_length_matches_ratio() {
        for (inr, outr) in [(48000, 16000), (44100, 16000), (22050, 16000), (24000, 16000), (8000, 16000)] {
            let x = vec![0.1f32; inr as usize];
            let y = resample_all(&x, inr, outr);
            let expected = outr as i64;
            assert!((y.len() as i64 - expected).abs() <= 2, "{inr}->{outr}: {}", y.len());
        }
    }

    #[test]
    fn keeps_in_band_tones_and_dc() {
        for inr in [48000u32, 44100, 22050] {
            let x = sine(1000.0, inr, 1.0, 0.5);
            let y = resample_all(&x, inr, 16000);
            let mid = &y[2000..y.len() - 2000];
            let amp = tone_amplitude(mid, 16000, 1000.0);
            assert!((amp - 0.5).abs() < 0.01, "{inr}: amplitude {amp}");
            let dc = resample_all(&vec![0.25f32; inr as usize], inr, 16000);
            let mean: f32 = dc[2000..14000].iter().sum::<f32>() / 12000.0;
            assert!((mean - 0.25).abs() < 0.003, "{inr}: dc {mean}");
        }
    }

    #[test]
    fn removes_content_above_the_new_nyquist() {
        // 11 kHz cannot be represented at 16 kHz; it must be filtered, not
        // folded back to 5 kHz.
        let x = sine(11000.0, 48000, 1.0, 0.5);
        let y = resample_all(&x, 48000, 16000);
        let level = rms(&y[2000..14000]);
        assert!(level < 0.005, "aliasing leaked through: rms {level}");
    }

    #[test]
    fn chunking_does_not_change_the_output() {
        let x = sine(700.0, 44100, 0.5, 0.3);
        let whole = resample_all(&x, 44100, 16000);
        let mut r = Resampler::new(44100, 16000);
        let mut chunked = Vec::new();
        for c in x.chunks(441) {
            r.process(c, &mut chunked);
        }
        r.flush(&mut chunked);
        assert_eq!(whole.len(), chunked.len());
        for (a, b) in whole.iter().zip(&chunked) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn long_streams_keep_bounded_history() {
        let mut r = Resampler::new(48000, 16000);
        let chunk = vec![0.0f32; 480];
        let mut out = Vec::new();
        for _ in 0..10_000 {
            r.process(&chunk, &mut out);
            out.clear();
        }
        assert!(r.buf.len() < 10_000, "history grew to {}", r.buf.len());
    }
}
