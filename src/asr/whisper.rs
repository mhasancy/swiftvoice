//! whisper.cpp fallback engine (via `whisper-rs`), for languages Parakeet
//! does not cover, including Bengali. Models are ggml files from the
//! whisper.cpp project; quantised `q5_0` variants trade a little accuracy
//! for much lower memory and faster CPU inference.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct WhisperEngine {
    // The state keeps the context alive internally; keep the context too so
    // drop order is explicit.
    state: WhisperState,
    _ctx: WhisperContext,
    threads: i32,
    initial_prompt: String,
}

impl WhisperEngine {
    pub fn load(dir: &Path, threads: u32, initial_prompt: String) -> Result<WhisperEngine> {
        let path = find_model(dir)?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(false);
        let ctx = WhisperContext::new_with_params(&path, params)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("cannot load whisper model {}", path.display()))?;
        let state = ctx.create_state().map_err(|e| anyhow!("cannot create whisper state: {e}"))?;
        let threads = if threads == 0 {
            std::thread::available_parallelism().map(|n| n.get().clamp(1, 8)).unwrap_or(4) as i32
        } else {
            threads as i32
        };
        log::info!("whisper.cpp model loaded from {} ({threads} threads)", path.display());
        Ok(WhisperEngine { state, _ctx: ctx, threads, initial_prompt })
    }
}

fn find_model(dir: &Path) -> Result<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
    let mut found: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file() && p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("ggml-") && n.ends_with(".bin")).unwrap_or(false)
        })
        .collect();
    found.sort();
    found.into_iter().next().ok_or_else(|| anyhow!("{} contains no ggml-*.bin whisper model", dir.display()))
}

/// Remove whisper.cpp's non-speech annotations: anything in square brackets
/// ("[BLANK_AUDIO]", "[MUSIC]") and well-known sound tags in parentheses
/// ("(music)", "(applause)"). Other parenthesised text is real dictation.
fn strip_annotations(text: &str) -> String {
    const SOUND_TAGS: &[&str] = &[
        "music",
        "applause",
        "laughter",
        "laughs",
        "silence",
        "noise",
        "inaudible",
        "coughs",
        "coughing",
        "sighs",
        "background noise",
        "static",
        "blank_audio",
        "no speech",
    ];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['[', '(']) {
        let open = rest.as_bytes()[i] as char;
        let close = if open == '[' { ']' } else { ')' };
        match rest[i + 1..].find(close) {
            Some(j) => {
                let inner = rest[i + 1..i + 1 + j].trim().to_lowercase();
                let drop = open == '[' || SOUND_TAGS.contains(&inner.as_str());
                out.push_str(&rest[..i]);
                if !drop {
                    out.push_str(&rest[i..i + 2 + j]);
                }
                rest = &rest[i + 2 + j..];
            }
            None => {
                out.push_str(&rest[..=i]);
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl super::SpeechEngine for WhisperEngine {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<String> {
        // whisper.cpp ignores input shorter than ~1 s; pad with silence.
        let mut padded;
        let audio = if audio.len() < 17_600 {
            padded = audio.to_vec();
            padded.resize(17_600, 0.0);
            &padded[..]
        } else {
            audio
        };
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_translate(false);
        params.set_language(Some(language.unwrap_or("auto")));
        params.set_no_context(true);
        params.set_no_timestamps(true);
        params.set_single_segment(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        // No temperature fallback: one decoding pass keeps latency bounded.
        params.set_temperature(0.0);
        params.set_temperature_inc(0.0);
        if !self.initial_prompt.is_empty() {
            params.set_initial_prompt(&self.initial_prompt);
        }
        self.state.full(params, audio).map_err(|e| anyhow!("whisper inference failed: {e}"))?;
        let mut text = String::new();
        for segment in self.state.as_iter() {
            let s = segment.to_str_lossy().map_err(|e| anyhow!("{e}"))?;
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(s.trim());
        }
        Ok(strip_annotations(&text))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_non_speech_annotations() {
        assert_eq!(super::strip_annotations("[BLANK_AUDIO]"), "");
        assert_eq!(super::strip_annotations(" Hello (music) world [noise] "), "Hello world");
        assert_eq!(super::strip_annotations("a ) b ("), "a ) b (");
        assert_eq!(super::strip_annotations("Call me (see above) later (Applause)"), "Call me (see above) later");
    }
}
