//! Custom vocabulary and voice commands.
//!
//! Rules map a spoken phrase (matched case-insensitively on whole words,
//! ignoring the punctuation the recogniser put around them) to a
//! replacement. Replacements made only of line breaks become paragraph or
//! line breaks; single punctuation characters attach to the neighbouring
//! word. Longest phrase wins; user rules override built-in commands with
//! the same phrase.
//!
//! `vocabulary.toml`:
//!
//! ```toml
//! [[replace]]
//! from = "cube and eighties"
//! to = "Kubernetes"
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::text::Piece;

#[derive(Debug, Clone, PartialEq)]
pub enum Replacement {
    Text(String),
    Newline(usize),
    AttachLeft(String),
    AttachRight(String),
}

impl Replacement {
    pub fn from_target(to: &str) -> Replacement {
        if !to.is_empty() && to.chars().all(|c| c == '\n' || c == '\r') {
            return Replacement::Newline(to.chars().filter(|&c| c == '\n').count().max(1));
        }
        let trimmed = to.trim();
        if trimmed.chars().count() == 1 || trimmed == "..." {
            let c = trimmed.chars().next().unwrap_or(' ');
            if matches!(c, ',' | '.' | '?' | '!' | ':' | ';' | ')' | ']' | '}' | '…' | '”' | '’' | '।') {
                return Replacement::AttachLeft(trimmed.into());
            }
            if matches!(c, '(' | '[' | '{' | '“' | '‘' | '¿' | '¡') {
                return Replacement::AttachRight(trimmed.into());
            }
        }
        Replacement::Text(to.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    /// Lower-case words of the spoken phrase.
    pub words: Vec<String>,
    pub to: Replacement,
    /// True for commands (their own punctuation is dropped).
    pub command: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct VocabFile {
    #[serde(default)]
    replace: Vec<VocabEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VocabEntry {
    from: String,
    to: String,
}

pub const VOCABULARY_TEMPLATE: &str = r#"# SwiftVoice custom vocabulary
#
# Each [[replace]] entry turns a spoken phrase into the text you want.
# Matching ignores case and the punctuation around the words. Used in the
# Clean and Smart modes (Raw mode inserts exactly what the model heard).
#
# Special targets: "\n" = new line, "\n\n" = new paragraph, and a single
# punctuation character such as "(" or ")" attaches to the next/previous word.

# [[replace]]
# from = "cube and eighties"
# to = "Kubernetes"

# [[replace]]
# from = "swift voice"
# to = "SwiftVoice"

# Bangla-English mixed phrases work the same way:
# [[replace]]
# from = "dhonnobad"
# to = "ধন্যবাদ"
"#;

fn split_words(s: &str) -> Vec<String> {
    s.split_whitespace().map(|w| super::text::split_token(w).1.to_lowercase()).filter(|w| !w.is_empty()).collect()
}

/// Commands enabled by `postprocess.voice_commands`.
const COMMANDS: &[(&str, &str)] = &[
    ("new line", "\n"),
    ("newline", "\n"),
    ("new paragraph", "\n\n"),
    ("open bracket", "("),
    ("close bracket", ")"),
    ("open parenthesis", "("),
    ("close parenthesis", ")"),
    ("open paren", "("),
    ("close paren", ")"),
    ("open square bracket", "["),
    ("close square bracket", "]"),
    ("open brace", "{"),
    ("close brace", "}"),
    ("open curly brace", "{"),
    ("close curly brace", "}"),
    ("open quote", "“"),
    ("close quote", "”"),
    ("end quote", "”"),
    ("question mark", "?"),
    ("exclamation mark", "!"),
    ("exclamation point", "!"),
    ("full stop", "."),
];

/// Commands enabled by `postprocess.punctuation_commands` (off by default
/// because the words also occur in normal prose).
const PUNCTUATION_COMMANDS: &[(&str, &str)] =
    &[("comma", ","), ("period", "."), ("colon", ":"), ("semicolon", ";"), ("semi colon", ";"), ("ellipsis", "…")];

#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    rules: Vec<Rule>,
}

impl Vocabulary {
    pub fn new(user: Vec<(String, String)>, voice_commands: bool, punctuation_commands: bool) -> Vocabulary {
        let mut rules: Vec<Rule> = Vec::new();
        let mut push = |from: &str, to: &str, command: bool| {
            let words = split_words(from);
            if words.is_empty() {
                return;
            }
            if let Some(existing) = rules.iter_mut().find(|r| r.words == words) {
                existing.to = Replacement::from_target(to);
                existing.command = command;
            } else {
                rules.push(Rule { words, to: Replacement::from_target(to), command });
            }
        };
        if voice_commands {
            for (f, t) in COMMANDS {
                push(f, t, true);
            }
        }
        if punctuation_commands {
            for (f, t) in PUNCTUATION_COMMANDS {
                push(f, t, true);
            }
        }
        for (f, t) in &user {
            // A user rule whose target is structural also behaves as a command.
            let structural = !matches!(Replacement::from_target(t), Replacement::Text(_));
            push(f, t, structural);
        }
        // Longest phrases first.
        rules.sort_by_key(|r| std::cmp::Reverse(r.words.len()));
        Vocabulary { rules }
    }

    /// Parse `vocabulary.toml` text into (from, to) pairs.
    pub fn parse_file(text: &str) -> Result<Vec<(String, String)>> {
        let f: VocabFile = toml::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(f.replace.into_iter().map(|e| (e.from, e.to)).collect())
    }

    /// Load the vocabulary file, creating a commented template if missing.
    pub fn load_user_file(path: &Path) -> Result<Vec<(String, String)>> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(path, VOCABULARY_TEMPLATE).with_context(|| format!("cannot create {}", path.display()))?;
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        Vocabulary::parse_file(&text).with_context(|| format!("invalid vocabulary file {}", path.display()))
    }

    /// Read the vocabulary file without creating it (command-line tools).
    pub fn read_user_file(path: &Path) -> Result<Vec<(String, String)>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        Vocabulary::parse_file(&text).with_context(|| format!("invalid vocabulary file {}", path.display()))
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Plain-text targets (names, jargon) used to bias Whisper.
    pub fn bias_terms(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter(|r| !r.command)
            .filter_map(|r| match &r.to {
                Replacement::Text(t) if t.chars().any(|c| c.is_alphabetic()) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    /// Apply all rules to the pieces.
    pub fn apply(&self, pieces: Vec<Piece>) -> Vec<Piece> {
        if self.rules.is_empty() {
            return pieces;
        }
        let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
        let mut i = 0;
        while i < pieces.len() {
            let mut matched = false;
            for rule in &self.rules {
                let n = rule.words.len();
                if i + n > pieces.len() || !matches_at(&pieces[i..i + n], &rule.words) {
                    continue;
                }
                let (lead, trail) = match (&pieces[i], &pieces[i + n - 1]) {
                    (Piece::Word { lead, .. }, Piece::Word { trail, .. }) => (lead.clone(), trail.clone()),
                    _ => (String::new(), String::new()),
                };
                match &rule.to {
                    Replacement::Text(t) => out.push(Piece::Word { lead, core: t.clone(), trail }),
                    Replacement::Newline(k) => {
                        // Punctuation the recogniser put on the command
                        // itself ("New line.") is dropped.
                        out.push(Piece::Newline(*k));
                    }
                    Replacement::AttachLeft(s) => {
                        // Replace punctuation the recogniser already added
                        // to the previous word ("Hello, comma" -> "Hello,").
                        if let Some(Piece::Word { trail: prev_trail, .. }) = out.last_mut() {
                            let kept: String = prev_trail.chars().filter(|c| matches!(c, '"' | '”' | '\'' | '’' | ')' | ']')).collect();
                            *prev_trail = kept;
                        }
                        out.push(Piece::AttachLeft(s.clone()));
                    }
                    Replacement::AttachRight(s) => out.push(Piece::AttachRight(s.clone())),
                }
                i += n;
                matched = true;
                break;
            }
            if !matched {
                out.push(pieces[i].clone());
                i += 1;
            }
        }
        out
    }
}

/// Do `pieces` spell `words`? Inner words may not carry sentence
/// punctuation (a phrase never spans "new. Line").
fn matches_at(pieces: &[Piece], words: &[String]) -> bool {
    for (k, (p, w)) in pieces.iter().zip(words).enumerate() {
        match p {
            Piece::Word { core, trail, .. } => {
                if core.to_lowercase() != *w {
                    return false;
                }
                if k + 1 < words.len() && trail.chars().any(|c| matches!(c, '.' | '?' | '!' | ';' | ':')) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postprocess::text::{render, tokenize};

    fn run(v: &Vocabulary, s: &str) -> String {
        render(&v.apply(tokenize(s)))
    }

    #[test]
    fn replaces_phrases_case_insensitively_keeping_punctuation() {
        let v = Vocabulary::new(vec![("cube and eighties".into(), "Kubernetes".into())], false, false);
        assert_eq!(run(&v, "Deploy it to Cube and Eighties."), "Deploy it to Kubernetes.");
        assert_eq!(run(&v, "cube and eighties, then"), "Kubernetes, then");
        // No match across sentence boundaries.
        assert_eq!(run(&v, "A cube. And eighties"), "A cube. And eighties");
    }

    #[test]
    fn voice_commands_produce_structure() {
        let v = Vocabulary::new(vec![], true, false);
        assert_eq!(run(&v, "First line new line second line."), "First line\nsecond line.");
        assert_eq!(run(&v, "Intro. New paragraph. Body"), "Intro.\n\nBody");
        assert_eq!(run(&v, "call open bracket maybe close bracket now"), "call (maybe) now");
        assert_eq!(run(&v, "Really question mark"), "Really?");
        // Recogniser punctuation before a spoken mark is replaced.
        assert_eq!(run(&v, "Wait, exclamation mark"), "Wait!");
    }

    #[test]
    fn punctuation_commands_are_opt_in() {
        let off = Vocabulary::new(vec![], true, false);
        assert_eq!(run(&off, "the period of time"), "the period of time");
        let on = Vocabulary::new(vec![], true, true);
        assert_eq!(run(&on, "Hello comma world period"), "Hello, world.");
    }

    #[test]
    fn longest_match_wins_and_user_rules_override() {
        let v = Vocabulary::new(vec![("open bracket".into(), "[".into()), ("open".into(), "OPEN".into())], true, false);
        assert_eq!(run(&v, "open bracket x"), "[x");
        assert_eq!(run(&v, "open door"), "OPEN door");
    }

    #[test]
    fn unicode_phrases_match() {
        let v = Vocabulary::new(vec![("dhonnobad".into(), "ধন্যবাদ".into()), ("আমি".into(), "I".into())], false, false);
        assert_eq!(run(&v, "Dhonnobad, bondhu"), "ধন্যবাদ, bondhu");
        assert_eq!(run(&v, "আমি ভালো"), "I ভালো");
    }

    #[test]
    fn parses_the_file_format() {
        let pairs = Vocabulary::parse_file("[[replace]]\nfrom = \"a b\"\nto = \"AB\"\n[[replace]]\nfrom = \"nl\"\nto = \"\\n\"\n").unwrap();
        assert_eq!(pairs, vec![("a b".to_string(), "AB".to_string()), ("nl".to_string(), "\n".to_string())]);
        assert!(Vocabulary::parse_file("[[replace]]\nfrom = \"x\"\n").is_err());
        assert!(Vocabulary::parse_file(VOCABULARY_TEMPLATE).unwrap().is_empty());
    }

    #[test]
    fn bias_terms_exclude_commands() {
        let v = Vocabulary::new(vec![("cube and eighties".into(), "Kubernetes".into()), ("nl".into(), "\n".into())], true, false);
        assert_eq!(v.bias_terms(), vec!["Kubernetes".to_string()]);
    }
}
