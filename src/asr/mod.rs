//! Speech recognition engines, model management and the recogniser thread.

pub mod catalog;
pub mod download;
pub mod features;
pub mod http;
pub mod nemo_ctc;
pub mod parakeet;
pub mod runtime;
pub mod tokens;
#[cfg(feature = "whisper")]
pub mod whisper;
pub mod worker;

use std::borrow::Cow;
use std::path::Path;

use anyhow::{bail, Result};

use catalog::{EngineKind, ModelInfo};

/// A loaded speech recogniser. Engines live on the recogniser thread only.
pub trait SpeechEngine {
    /// Transcribe 16 kHz mono audio. `language` is an ISO 639-1 hint
    /// (`None` = automatic); engines that detect language ignore it.
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<String>;

    /// Transcribe `audio` whose first `context` samples were already
    /// transcribed as the end of the previous window. The context lets the
    /// model continue the sentence (casing, punctuation, the first word);
    /// only text spoken after it is returned. Engines without token timing
    /// transcribe the new audio alone.
    fn transcribe_after(&mut self, audio: &[f32], context: usize, language: Option<&str>) -> Result<String> {
        self.transcribe(&audio[context.min(audio.len())..], language)
    }
}

/// Samples per encoder frame of NeMo FastConformer models: 10 ms feature
/// hop x 8 subsampling.
pub(crate) const ENCODER_FRAME_SAMPLES: usize = 1280;

/// Text spoken after the first `context` samples, from greedy-decoded
/// `ids` and the encoder frame each was emitted at. The boundary lies in a
/// pause, so pieces of a word that began in the context are late emissions
/// of its last word and are skipped. Punctuation emitted late for that word
/// is kept in front (", ask what"), so that
/// [`crate::postprocess::join_windows`] can attach it to the previous
/// window's text when that text has none.
pub(crate) fn text_after_context(tokens: &tokens::TokenTable, ids: &[usize], frames: &[usize], context: usize) -> String {
    let boundary = context.div_ceil(ENCODER_FRAME_SAMPLES);
    let mut k = frames.iter().position(|&f| f >= boundary).unwrap_or(ids.len());
    let mut lead = String::new();
    if k > 0 {
        while k < ids.len() && !tokens.is_word_start(ids[k]) {
            let piece = tokens.decode(&ids[k..k + 1]);
            if !piece.is_empty() && piece.chars().all(crate::postprocess::text::is_trailing_punct) {
                lead.push_str(&piece);
            }
            k += 1;
        }
    }
    let rest = tokens.decode(&ids[k..]);
    match (lead.is_empty(), rest.is_empty()) {
        (true, _) => rest,
        (false, true) => lead,
        (false, false) => format!("{lead} {rest}"),
    }
}

/// Minimum input length; shorter clips are padded with trailing silence.
const MIN_SAMPLES: usize = 16_000;

pub(crate) fn pad_min_length(audio: &[f32]) -> Cow<'_, [f32]> {
    if audio.len() >= MIN_SAMPLES {
        Cow::Borrowed(audio)
    } else {
        let mut v = audio.to_vec();
        v.resize(MIN_SAMPLES, 0.0);
        Cow::Owned(v)
    }
}

/// Load the engine for an installed catalogue model.
pub fn load_engine(info: &ModelInfo, models_dir: &Path, threads: u32, whisper_prompt: &str) -> Result<Box<dyn SpeechEngine>> {
    let dir = download::model_dir(models_dir, info);
    if !download::is_installed(models_dir, info) {
        bail!(
            "{} is not installed in {}. Download it from the tray menu (Engine & model) or run: swiftvoice models download {}",
            info.display,
            models_dir.display(),
            info.id
        );
    }
    load_engine_from_dir(info.engine, &dir, threads, whisper_prompt)
}

/// Load an engine from a folder (also used for user-supplied model folders).
pub fn load_engine_from_dir(kind: EngineKind, dir: &Path, threads: u32, whisper_prompt: &str) -> Result<Box<dyn SpeechEngine>> {
    match kind {
        EngineKind::ParakeetTdt => Ok(Box::new(parakeet::ParakeetTdt::load(dir, threads)?)),
        EngineKind::NemoCtc => Ok(Box::new(nemo_ctc::NemoCtc::load(dir, threads)?)),
        EngineKind::Whisper => load_whisper(dir, threads, whisper_prompt),
        EngineKind::Vad => bail!("the voice activity detector is not a speech recogniser"),
    }
}

#[cfg(feature = "whisper")]
fn load_whisper(dir: &Path, threads: u32, prompt: &str) -> Result<Box<dyn SpeechEngine>> {
    Ok(Box::new(whisper::WhisperEngine::load(dir, threads, prompt.to_string())?))
}

#[cfg(not(feature = "whisper"))]
fn load_whisper(_dir: &Path, _threads: u32, _prompt: &str) -> Result<Box<dyn SpeechEngine>> {
    bail!("this build of SwiftVoice was compiled without the whisper.cpp engine (cargo feature \"whisper\")")
}

/// Guess the engine of a user-supplied model folder.
pub fn detect_engine(dir: &Path) -> Option<EngineKind> {
    if parakeet::ParakeetTdt::detect(dir) {
        Some(EngineKind::ParakeetTdt)
    } else if nemo_ctc::NemoCtc::detect(dir) {
        Some(EngineKind::NemoCtc)
    } else if std::fs::read_dir(dir).ok()?.flatten().any(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        n.starts_with("ggml-") && n.ends_with(".bin")
    }) {
        Some(EngineKind::Whisper)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_short_audio_only() {
        assert_eq!(pad_min_length(&[0.5; 10]).len(), MIN_SAMPLES);
        assert!(matches!(pad_min_length(&vec![0.0; 20_000]), Cow::Borrowed(_)));
    }

    #[test]
    fn text_after_context_drops_context_words_and_keeps_late_punctuation() {
        // "<unk> 0, ▁hello 1, ',' 2, ▁wor 3, ld 4, ..., <blk> 7"
        let t = tokens::TokenTable::parse("<unk> 0\n\u{2581}hello 1\n, 2\n\u{2581}wor 3\nld 4\n<|nospeech|> 5\n\u{2581}x 6\n<blk> 7\n")
            .unwrap();
        let ctx = 2 * ENCODER_FRAME_SAMPLES; // boundary at frame 2
                                             // "hello" in the context, its comma emitted late, then "world".
        assert_eq!(text_after_context(&t, &[1, 2, 3, 4], &[0, 2, 3, 3], ctx), ", world");
        // The tail of a word that began in the context is skipped.
        assert_eq!(text_after_context(&t, &[3, 4, 1], &[1, 2, 4], ctx), "hello");
        // Nothing spoken after the context.
        assert_eq!(text_after_context(&t, &[1, 2], &[0, 1], ctx), "");
        // No context: everything.
        assert_eq!(text_after_context(&t, &[1, 2, 3, 4], &[0, 2, 3, 3], 0), "hello, world");
    }

    #[test]
    fn missing_model_error_explains_how_to_install() {
        let dir = crate::paths::test_dir("asr_missing");
        let info = catalog::find("parakeet-tdt-0.6b-v3").unwrap();
        let err = load_engine(info, &dir, 0, "").err().unwrap().to_string();
        assert!(err.contains("swiftvoice models download parakeet-tdt-0.6b-v3"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
