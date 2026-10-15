//! Turning a raw transcript into the text that gets inserted.
//!
//! * **Raw**: exactly what the recogniser returned.
//! * **Clean** (default): fillers removed, stutters collapsed, custom
//!   vocabulary and voice commands applied, sentence capitalisation and
//!   punctuation tidied. Pure Rust, well under a millisecond.
//! * **Smart**: Clean, then an optional rewrite by a local LLM (Ollama /
//!   LM Studio) on this computer. Falls back to Clean on any problem.

pub mod clean;
pub mod smart;
pub mod text;
pub mod vocabulary;

/// Join the transcripts of consecutive windows of one utterance. A window
/// transcribed with context can start with punctuation the model emitted
/// late for the previous window's last word (", ask what"): it is attached
/// to the previous text when that text ends without punctuation, and
/// dropped otherwise.
pub fn join_windows<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for part in parts {
        let part = part.trim();
        let lead_len: usize = part.chars().take_while(|c| text::is_trailing_punct(*c)).map(char::len_utf8).sum();
        let (lead, rest) = part.split_at(lead_len);
        let rest = rest.trim_start();
        if !lead.is_empty() && out.chars().last().is_some_and(|c| !text::is_trailing_punct(c)) {
            out.push_str(lead);
        }
        if rest.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(rest);
    }
    out
}

use std::time::Instant;

use crate::config::{PostMode, PostprocessConfig};
use vocabulary::Vocabulary;

pub struct PostProcessor {
    cfg: PostprocessConfig,
    vocab: Vocabulary,
    smart: Option<Result<smart::SmartRewriter, String>>,
}

#[derive(Debug, Clone)]
pub struct PostResult {
    pub text: String,
    /// Mode actually applied (Smart can fall back to Clean).
    pub mode: PostMode,
    /// Why Smart mode fell back, if it did.
    pub note: Option<String>,
    pub clean_us: u64,
    pub smart_ms: u64,
}

impl PostProcessor {
    pub fn new(cfg: &PostprocessConfig, user_vocab: Vec<(String, String)>) -> PostProcessor {
        let vocab = Vocabulary::new(user_vocab, cfg.voice_commands, cfg.punctuation_commands);
        let smart = (cfg.mode == PostMode::Smart).then(|| smart::SmartRewriter::new(&cfg.smart).map_err(|e| e.to_string()));
        PostProcessor { cfg: cfg.clone(), vocab, smart }
    }

    pub fn mode(&self) -> PostMode {
        self.cfg.mode
    }

    pub fn vocabulary(&self) -> &Vocabulary {
        &self.vocab
    }

    /// Clean-mode transformation. `english` enables English-only rules
    /// (capitalising the pronoun "I").
    pub fn clean(&self, raw: &str, english: bool) -> String {
        let mut pieces = text::tokenize(raw);
        if self.cfg.remove_fillers {
            pieces = clean::remove_fillers(pieces, &self.cfg.fillers);
        }
        if self.cfg.remove_repeats {
            pieces = clean::remove_repeats(pieces);
        }
        pieces = self.vocab.apply(pieces);
        clean::tidy_punctuation(&mut pieces);
        if self.cfg.capitalize {
            clean::capitalize(&mut pieces, english);
        }
        let mut out = text::render(&pieces);
        if self.cfg.ensure_final_punctuation {
            clean::ensure_final_punctuation(&mut out);
        }
        out
    }

    /// Full processing for the configured mode. Smart mode blocks for up to
    /// `postprocess.smart.timeout_ms`; call it off the UI thread.
    pub fn process(&self, raw: &str, language: Option<&str>, english: bool) -> PostResult {
        let raw = raw.trim();
        if self.cfg.mode == PostMode::Raw || raw.is_empty() {
            return PostResult { text: raw.to_string(), mode: PostMode::Raw, note: None, clean_us: 0, smart_ms: 0 };
        }
        let t = Instant::now();
        let cleaned = self.clean(raw, english);
        let clean_us = t.elapsed().as_micros() as u64;
        if self.cfg.mode == PostMode::Clean || cleaned.is_empty() {
            return PostResult { text: cleaned, mode: PostMode::Clean, note: None, clean_us, smart_ms: 0 };
        }
        let t = Instant::now();
        let outcome = match &self.smart {
            Some(Ok(r)) => r.rewrite(&cleaned, language).map_err(|e| format!("{e:#}")),
            Some(Err(e)) => Err(e.clone()),
            None => Err("Smart mode is not configured".to_string()),
        };
        let smart_ms = t.elapsed().as_millis() as u64;
        match outcome {
            Ok(text) => PostResult { text, mode: PostMode::Smart, note: None, clean_us, smart_ms },
            Err(e) => {
                log::warn!("Smart mode fell back to Clean: {e}");
                PostResult { text: cleaned, mode: PostMode::Clean, note: Some(e), clean_us, smart_ms }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pp(mode: PostMode) -> PostProcessor {
        let cfg = PostprocessConfig { mode, ..PostprocessConfig::default() };
        PostProcessor::new(&cfg, vec![("cube and eighties".into(), "Kubernetes".into())])
    }

    #[test]
    fn windows_are_joined_with_late_punctuation_attached() {
        assert_eq!(join_windows(["for you", ", ask what you can do."]), "for you, ask what you can do.");
        assert_eq!(join_windows(["for you.", ", ask"]), "for you. ask");
        assert_eq!(join_windows(["  one ", "", "two", "."]), "one two.");
        assert_eq!(join_windows([", start"]), "start");
        assert_eq!(join_windows(["আমি", "। তুমি"]), "আমি। তুমি");
    }

    #[test]
    fn voice_commands_give_structure_in_clean_mode() {
        // tests/MANUAL_TESTS.md C9.
        let r = pp(PostMode::Clean).process("first line new line second line new paragraph open bracket aside close bracket", None, true);
        assert_eq!(r.text, "First line\nSecond line\n\n(Aside)");
    }

    #[test]
    fn raw_mode_is_untouched() {
        let p = pp(PostMode::Raw);
        let r = p.process("  um the the cube and eighties new line  ", None, true);
        assert_eq!(r.text, "um the the cube and eighties new line");
        assert_eq!(r.mode, PostMode::Raw);
    }

    #[test]
    fn clean_mode_end_to_end() {
        let p = pp(PostMode::Clean);
        let r = p.process("um, so i deployed the the app to cube and eighties. new line it works, uh, fine", Some("en"), true);
        assert_eq!(r.text, "So I deployed the app to Kubernetes.\nIt works fine");
        assert_eq!(r.mode, PostMode::Clean);
    }

    #[test]
    fn clean_mode_leaves_good_text_alone() {
        let p = pp(PostMode::Clean);
        let input = "Ask not what your country can do for you, ask what you can do for your country.";
        assert_eq!(p.process(input, Some("en"), true).text, input);
        let de = "Alles hat ein Ende, nur die Wurst hat zwei.";
        assert_eq!(p.process(de, Some("de"), false).text, de);
    }

    #[test]
    fn smart_mode_falls_back_to_clean_when_server_is_down() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut cfg = PostprocessConfig { mode: PostMode::Smart, ..PostprocessConfig::default() };
        cfg.smart.endpoint = format!("http://127.0.0.1:{port}");
        cfg.smart.timeout_ms = 300;
        let p = PostProcessor::new(&cfg, vec![]);
        let r = p.process("um hello world", None, true);
        assert_eq!(r.text, "Hello world");
        assert_eq!(r.mode, PostMode::Clean);
        assert!(r.note.is_some());
    }
}
