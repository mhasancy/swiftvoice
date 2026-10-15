//! NVIDIA Parakeet TDT (Token-and-Duration Transducer) on ONNX Runtime.
//!
//! Model layout: the sherpa-onnx export of the NeMo checkpoint
//! (`encoder*.onnx`, `decoder*.onnx`, `joiner*.onnx`, `tokens.txt`):
//!
//! * encoder: `audio_signal [N, n_mels, T]`, `length [N]` (i64) ->
//!   `outputs [N, D, T/8]`, `encoded_lengths [N]`
//! * decoder (prediction network, 2-layer LSTM): `targets [N, 1]` (i32),
//!   `target_length [N]` (i32), two LSTM states `[2, N, H]` ->
//!   `outputs [N, H, 1]`, lengths, two new states
//! * joiner: `encoder_outputs [N, D, 1]`, `decoder_outputs [N, H, 1]` ->
//!   logits `[N, 1, 1, V + K]`: V token logits (blank last) followed by K
//!   duration logits for durations 0..K-1.
//!
//! Greedy TDT decoding: at frame t pick the best token and the best duration;
//! emit non-blank tokens (advancing the prediction network), then jump
//! ahead by the predicted duration. Skipping frames is what makes TDT
//! decoding faster than classic RNN-T.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ort::session::Session;
use ort::value::TensorRef;

use super::features::NemoMel;
use super::runtime;
use super::tokens::TokenTable;

const MAX_SYMBOLS_PER_FRAME: usize = 10;

pub struct ParakeetTdt {
    encoder: Session,
    decoder: Session,
    joiner: Session,
    tokens: TokenTable,
    mel: NemoMel,
    enc_in: [String; 2],
    enc_out: [String; 2],
    dec_in: [String; 4],
    dec_out: [String; 4],
    join_in: [String; 2],
    join_out: String,
    enc_dim: usize,
    hidden: usize,
    state_layers: usize,
    num_durations: usize,
}

/// First existing file among `names` in `dir`.
pub fn find_file(dir: &Path, names: &[&str]) -> Result<PathBuf> {
    for n in names {
        let p = dir.join(n);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("{} does not contain any of: {}", dir.display(), names.join(", "))
}

fn names<const N: usize>(v: Vec<String>, what: &str) -> Result<[String; N]> {
    let got = v.len();
    v.try_into().map_err(|_| anyhow::anyhow!("{what}: expected {N} tensors, found {got}"))
}

impl ParakeetTdt {
    /// True if `dir` looks like a Parakeet TDT transducer export.
    pub fn detect(dir: &Path) -> bool {
        find_file(dir, &["joiner.int8.onnx", "joiner.onnx"]).is_ok() && dir.join("tokens.txt").is_file()
    }

    pub fn load(dir: &Path, threads: u32) -> Result<ParakeetTdt> {
        let encoder_path = find_file(dir, &["encoder.int8.onnx", "encoder.onnx"])?;
        let decoder_path = find_file(dir, &["decoder.int8.onnx", "decoder.onnx"])?;
        let joiner_path = find_file(dir, &["joiner.int8.onnx", "joiner.onnx"])?;
        let tokens = TokenTable::load(&dir.join("tokens.txt"))?;

        let encoder = runtime::load_session(&encoder_path, threads, "Parakeet encoder")?;
        // The decoder and joiner run once per output step on tiny tensors;
        // a single thread avoids thread-pool hand-off overhead.
        let decoder = runtime::load_session(&decoder_path, 1, "Parakeet decoder")?;
        let joiner = runtime::load_session(&joiner_path, 1, "Parakeet joiner")?;

        let enc_in: [String; 2] = names(runtime::input_names(&encoder), "encoder inputs")?;
        let enc_out: [String; 2] = names(runtime::output_names(&encoder), "encoder outputs")?;
        let dec_in: [String; 4] = names(runtime::input_names(&decoder), "decoder inputs")?;
        let dec_out: [String; 4] = names(runtime::output_names(&decoder), "decoder outputs")?;
        let join_in: [String; 2] = names(runtime::input_names(&joiner), "joiner inputs")?;
        let join_out = runtime::output_names(&joiner).into_iter().next().context("joiner has no output")?;

        let in_shape = runtime::input_shape(&encoder, 0).context("encoder input has no shape")?;
        let n_mels = *in_shape.get(1).context("encoder input rank")?;
        if !(n_mels == 80 || n_mels == 128) {
            bail!("unexpected encoder feature size {n_mels} (expected 80 or 128)");
        }
        let out_shape = runtime::output_shape(&encoder, 0).context("encoder output has no shape")?;
        let enc_dim = *out_shape.get(1).context("encoder output rank")?;
        let state_shape = runtime::input_shape(&decoder, 2).context("decoder state has no shape")?;
        if state_shape.len() != 3 || state_shape[2] <= 0 || state_shape[0] <= 0 {
            bail!("unexpected decoder state shape {state_shape:?}");
        }
        let joiner_shape = runtime::output_shape(&joiner, 0).context("joiner output has no shape")?;
        let joint = *joiner_shape.last().context("joiner output rank")?;
        if enc_dim <= 0 || joint <= 0 {
            bail!("model has dynamic feature dimensions that SwiftVoice cannot infer");
        }
        let joint = joint as usize;
        if joint <= tokens.len() || joint - tokens.len() > 16 {
            bail!(
                "joiner emits {joint} logits for {} tokens; this is not a TDT model (for plain RNN-T or CTC use the matching model type)",
                tokens.len()
            );
        }
        let num_durations = joint - tokens.len();
        if tokens.blank_id() != tokens.len() - 1 {
            bail!("blank token must be the last entry in tokens.txt");
        }
        log::info!(
            "Parakeet TDT loaded from {}: {} tokens, {} durations, {} mel bins, encoder dim {}",
            dir.display(),
            tokens.len(),
            num_durations,
            n_mels,
            enc_dim
        );
        Ok(ParakeetTdt {
            encoder,
            decoder,
            joiner,
            tokens,
            mel: NemoMel::new(n_mels as usize),
            enc_in,
            enc_out,
            dec_in,
            dec_out,
            join_in,
            join_out,
            enc_dim: enc_dim as usize,
            hidden: state_shape[2] as usize,
            state_layers: state_shape[0] as usize,
            num_durations,
        })
    }

    /// Encoder frames as `[T][enc_dim]`.
    fn encode(&mut self, audio: &[f32]) -> Result<(Vec<f32>, usize)> {
        let (feats, t, valid) = self.mel.compute(audio);
        let n_mels = self.mel.n_mels();
        let len = [valid as i64];
        let outputs = self.encoder.run(ort::inputs![
            self.enc_in[0].as_str() => TensorRef::from_array_view(([1usize, n_mels, t], &feats[..]))?,
            self.enc_in[1].as_str() => TensorRef::from_array_view(([1usize], &len[..]))?,
        ])?;
        let (shape, data) = outputs[self.enc_out[0].as_str()].try_extract_tensor::<f32>()?;
        let (_, lens) = outputs[self.enc_out[1].as_str()].try_extract_tensor::<i64>()?;
        if shape.len() != 3 || shape[1] as usize != self.enc_dim {
            bail!("unexpected encoder output shape {:?}", &shape[..]);
        }
        let frames_out = shape[2] as usize;
        let frames = (lens.first().copied().unwrap_or(frames_out as i64).max(0) as usize).min(frames_out);
        let d = self.enc_dim;
        let mut out = vec![0.0f32; frames * d];
        for c in 0..d {
            let row = &data[c * frames_out..c * frames_out + frames];
            for (t, &v) in row.iter().enumerate() {
                out[t * d + c] = v;
            }
        }
        Ok((out, frames))
    }

    /// Run the prediction network for `token`, updating `state1`/`state2`
    /// in place and writing the decoder output into `dec_out`.
    fn predict(&mut self, token: i32, state1: &mut [f32], state2: &mut [f32], dec_out: &mut [f32]) -> Result<()> {
        let targets = [token];
        let target_len = [1i32];
        let state_shape = [self.state_layers, 1, self.hidden];
        let outputs = self.decoder.run(ort::inputs![
            self.dec_in[0].as_str() => TensorRef::from_array_view(([1usize, 1], &targets[..]))?,
            self.dec_in[1].as_str() => TensorRef::from_array_view(([1usize], &target_len[..]))?,
            self.dec_in[2].as_str() => TensorRef::from_array_view((state_shape, &state1[..]))?,
            self.dec_in[3].as_str() => TensorRef::from_array_view((state_shape, &state2[..]))?,
        ])?;
        let (_, out) = outputs[self.dec_out[0].as_str()].try_extract_tensor::<f32>()?;
        let (_, s1) = outputs[self.dec_out[2].as_str()].try_extract_tensor::<f32>()?;
        let (_, s2) = outputs[self.dec_out[3].as_str()].try_extract_tensor::<f32>()?;
        if out.len() != self.hidden || s1.len() != state1.len() || s2.len() != state2.len() {
            bail!("unexpected decoder output sizes ({}, {}, {})", out.len(), s1.len(), s2.len());
        }
        dec_out.copy_from_slice(out);
        state1.copy_from_slice(s1);
        state2.copy_from_slice(s2);
        Ok(())
    }

    fn joint(&mut self, enc_frame: &[f32], dec_out: &[f32], logits: &mut Vec<f32>) -> Result<()> {
        let outputs = self.joiner.run(ort::inputs![
            self.join_in[0].as_str() => TensorRef::from_array_view(([1usize, self.enc_dim, 1], enc_frame))?,
            self.join_in[1].as_str() => TensorRef::from_array_view(([1usize, self.hidden, 1], dec_out))?,
        ])?;
        let (_, out) = outputs[self.join_out.as_str()].try_extract_tensor::<f32>()?;
        logits.clear();
        logits.extend_from_slice(out);
        Ok(())
    }

    /// Greedy TDT decoding over encoder frames. Returns token ids and the
    /// encoder frame each was emitted at.
    fn decode(&mut self, frames: &[f32], t_max: usize) -> Result<(Vec<usize>, Vec<usize>)> {
        let blank = self.tokens.blank_id();
        let vocab = self.tokens.len();
        let state_len = self.state_layers * self.hidden;
        let mut s1 = vec![0.0f32; state_len];
        let mut s2 = vec![0.0f32; state_len];
        let mut dec_out = vec![0.0f32; self.hidden];
        let mut logits = Vec::with_capacity(vocab + self.num_durations);
        self.predict(blank as i32, &mut s1, &mut s2, &mut dec_out)?;
        let d = self.enc_dim;
        let mut ids = Vec::new();
        let mut at = Vec::new();
        let mut t = 0usize;
        let mut symbols = 0usize;
        while t < t_max {
            let frame = &frames[t * d..(t + 1) * d];
            self.joint(frame, &dec_out, &mut logits)?;
            if logits.len() != vocab + self.num_durations {
                bail!("joiner returned {} logits, expected {}", logits.len(), vocab + self.num_durations);
            }
            let token = argmax(&logits[..vocab]);
            let skip = argmax(&logits[vocab..]);
            if token != blank {
                ids.push(token);
                at.push(t);
                self.predict(token as i32, &mut s1, &mut s2, &mut dec_out)?;
                symbols += 1;
            }
            if skip > 0 {
                t += skip;
                symbols = 0;
            } else if token == blank || symbols >= MAX_SYMBOLS_PER_FRAME {
                t += 1;
                symbols = 0;
            }
        }
        Ok((ids, at))
    }
}

pub(crate) fn argmax(v: &[f32]) -> usize {
    let mut best = 0;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &x) in v.iter().enumerate() {
        if x > best_v {
            best_v = x;
            best = i;
        }
    }
    best
}

impl super::SpeechEngine for ParakeetTdt {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<String> {
        self.transcribe_after(audio, 0, language)
    }

    fn transcribe_after(&mut self, audio: &[f32], context: usize, _language: Option<&str>) -> Result<String> {
        let audio = super::pad_min_length(audio);
        let (frames, t) = self.encode(&audio)?;
        let (ids, at) = self.decode(&frames, t)?;
        if context == 0 {
            return Ok(self.tokens.decode(&ids));
        }
        Ok(super::text_after_context(&self.tokens, &ids, &at, context))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn argmax_picks_first_maximum() {
        assert_eq!(super::argmax(&[0.1, 3.0, 3.0, -1.0]), 1);
        assert_eq!(super::argmax(&[-5.0]), 0);
    }
}
