//! NeMo CTC models (e.g. the CTC branch of Parakeet TDT-CTC 110M).
//!
//! Layout: `model*.onnx` + `tokens.txt`. Input `audio_signal [N, n_mels, T]`
//! and `length [N]`; output log-probabilities `[N, T/8, V]` with blank last.
//! Greedy CTC: best token per frame, collapse repeats, drop blanks.

use std::path::Path;

use anyhow::{bail, Context, Result};
use ort::session::Session;
use ort::value::TensorRef;

use super::features::NemoMel;
use super::parakeet::{argmax, find_file};
use super::runtime;
use super::tokens::TokenTable;

pub struct NemoCtc {
    model: Session,
    tokens: TokenTable,
    mel: NemoMel,
    inputs: [String; 2],
    output: String,
}

impl NemoCtc {
    pub fn load(dir: &Path, threads: u32) -> Result<NemoCtc> {
        let path = find_file(dir, &["model.int8.onnx", "model.onnx"])?;
        let tokens = TokenTable::load(&dir.join("tokens.txt"))?;
        let model = runtime::load_session(&path, threads, "NeMo CTC model")?;
        let in_names = runtime::input_names(&model);
        if in_names.len() != 2 {
            bail!("CTC model must have 2 inputs (audio_signal, length), found {in_names:?}");
        }
        let output = runtime::output_names(&model).into_iter().next().context("CTC model has no output")?;
        let n_mels = runtime::input_shape(&model, 0).and_then(|s| s.get(1).copied()).context("CTC input shape")?;
        if !(n_mels == 80 || n_mels == 128) {
            bail!("unexpected CTC feature size {n_mels}");
        }
        let vocab = runtime::output_shape(&model, 0).and_then(|s| s.last().copied()).unwrap_or(-1);
        if vocab > 0 && vocab as usize != tokens.len() {
            bail!("model emits {vocab} classes but tokens.txt has {} entries", tokens.len());
        }
        log::info!("NeMo CTC model loaded from {}: {} tokens, {} mel bins", dir.display(), tokens.len(), n_mels);
        Ok(NemoCtc { model, tokens, mel: NemoMel::new(n_mels as usize), inputs: [in_names[0].clone(), in_names[1].clone()], output })
    }
}

impl super::SpeechEngine for NemoCtc {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<String> {
        self.transcribe_after(audio, 0, language)
    }

    fn transcribe_after(&mut self, audio: &[f32], context: usize, _language: Option<&str>) -> Result<String> {
        let audio = super::pad_min_length(audio);
        let (feats, t, valid) = self.mel.compute(&audio);
        let n_mels = self.mel.n_mels();
        let len = [valid as i64];
        let outputs = self.model.run(ort::inputs![
            self.inputs[0].as_str() => TensorRef::from_array_view(([1usize, n_mels, t], &feats[..]))?,
            self.inputs[1].as_str() => TensorRef::from_array_view(([1usize], &len[..]))?,
        ])?;
        let (shape, logp) = outputs[self.output.as_str()].try_extract_tensor::<f32>()?;
        if shape.len() != 3 {
            bail!("unexpected CTC output shape {:?}", &shape[..]);
        }
        let (frames, vocab) = (shape[1] as usize, shape[2] as usize);
        let blank = self.tokens.blank_id();
        let mut ids = Vec::new();
        let mut at = Vec::new();
        let mut prev = usize::MAX;
        for f in 0..frames {
            let best = argmax(&logp[f * vocab..(f + 1) * vocab]);
            if best != prev && best != blank {
                ids.push(best);
                at.push(f);
            }
            prev = best;
        }
        if context == 0 {
            return Ok(self.tokens.decode(&ids));
        }
        Ok(super::text_after_context(&self.tokens, &ids, &at, context))
    }
}
