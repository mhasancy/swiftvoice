//! Clean-mode passes: filler removal, stutter/duplicate cleanup,
//! capitalisation and punctuation tidying. Every pass is conservative: when
//! a rule could change meaning it is not applied.

use super::text::Piece;

/// Words whose immediate repetition is often intentional ("had had",
/// "that that", "no no no").
const REPEAT_OK: &[&str] = &[
    "had", "that", "is", "do", "very", "really", "so", "no", "yes", "bye", "ha", "haha", "now", "well", "far", "more", "again", "go",
    "knock", "tut", "chop", "aye", "ok", "okay",
];

/// Abbreviations after which a full stop does not end a sentence.
const ABBREVIATIONS: &[&str] =
    &["e.g", "i.e", "etc", "mr", "mrs", "ms", "dr", "prof", "vs", "st", "no", "approx", "inc", "ltd", "jr", "sr"];

fn ends_sentence(trail: &str) -> bool {
    trail.chars().any(|c| matches!(c, '.' | '!' | '?' | '…' | '।'))
}

/// Remove filler words ("um", "uh", ...) and the commas that framed them.
pub fn remove_fillers(pieces: Vec<Piece>, fillers: &[String]) -> Vec<Piece> {
    if fillers.is_empty() {
        return pieces;
    }
    let fillers: Vec<String> = fillers.iter().map(|f| f.to_lowercase()).collect();
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for p in pieces {
        let is_filler = match &p {
            Piece::Word { core, .. } => fillers.contains(&core.to_lowercase()),
            _ => false,
        };
        if !is_filler {
            out.push(p);
            continue;
        }
        let Piece::Word { trail, .. } = p else { unreachable!() };
        if let Some(Piece::Word { trail: prev_trail, .. }) = out.last_mut() {
            if ends_sentence(&trail) && (prev_trail.is_empty() || prev_trail == ",") {
                // "and so um." -> "and so."; "that's it, um." -> "that's it."
                *prev_trail = trail.chars().filter(|c| matches!(c, '.' | '!' | '?' | '…' | '।')).collect();
            } else if prev_trail == "," {
                // "I was, uh, thinking" -> "I was thinking"
                prev_trail.clear();
            }
        }
    }
    out
}

/// Remove stutters: a word or two-word phrase immediately repeated
/// ("the the", "I think I think") unless the repetition is idiomatic.
pub fn remove_repeats(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for p in pieces {
        // Single word repeat: previous word identical and not punctuated.
        if let (Piece::Word { core, .. }, Some(Piece::Word { core: prev, trail: prev_trail, .. })) = (&p, out.last()) {
            let c = core.to_lowercase();
            if !c.is_empty() && c == prev.to_lowercase() && prev_trail.is_empty() && !REPEAT_OK.contains(&c.as_str()) {
                out.pop();
                out.push(p);
                continue;
            }
        }
        out.push(p);
        // Two-word phrase repeat: ... a b a b -> ... a b
        let n = out.len();
        if n >= 4 {
            let key = |i: usize| match &out[i] {
                Piece::Word { core, trail, .. } if !core.is_empty() => Some((core.to_lowercase(), trail.is_empty())),
                _ => None,
            };
            if let (Some((a1, t1)), Some((b1, t2)), Some((a2, _)), Some((b2, _))) = (key(n - 4), key(n - 3), key(n - 2), key(n - 1)) {
                if a1 == a2 && b1 == b2 && t1 && t2 && a1 != b1 {
                    out.remove(n - 4);
                    out.remove(n - 4);
                }
            }
        }
    }
    out
}

fn capitalize_first(s: &mut String) {
    if let Some(first) = s.chars().next() {
        if first.is_lowercase() {
            let upper: String = first.to_uppercase().collect();
            s.replace_range(..first.len_utf8(), &upper);
        }
    }
}

/// Capitalise sentence starts (text start, after . ! ? and line breaks)
/// and, for English, the pronoun "I".
pub fn capitalize(pieces: &mut [Piece], english: bool) {
    let mut sentence_start = true;
    for p in pieces.iter_mut() {
        match p {
            Piece::Word { lead, core, trail } => {
                if core.is_empty() {
                    if ends_sentence(trail) {
                        sentence_start = true;
                    }
                    continue;
                }
                if sentence_start && lead.chars().all(|c| matches!(c, '"' | '“' | '(' | '[' | '\'' | '‘' | '¿' | '¡')) {
                    capitalize_first(core);
                }
                if english {
                    let lower = core.to_lowercase();
                    if lower == "i" || lower.starts_with("i'") || lower.starts_with("i’") {
                        capitalize_first(core);
                    }
                }
                let abbreviation =
                    ABBREVIATIONS.contains(&core.to_lowercase().as_str()) || (core.chars().count() == 1 && trail.starts_with('.'));
                sentence_start = ends_sentence(trail) && !abbreviation;
            }
            Piece::Newline(_) => sentence_start = true,
            Piece::AttachLeft(s) => sentence_start = ends_sentence(s),
            Piece::AttachRight(_) => {}
        }
    }
}

/// Collapse doubled punctuation the recogniser and voice commands can
/// produce (",," -> ",", ".." -> "." but keep "...").
pub fn tidy_punctuation(pieces: &mut [Piece]) {
    for p in pieces.iter_mut() {
        if let Piece::Word { trail, .. } = p {
            if trail.len() >= 2 && trail != "..." && !trail.contains('…') {
                let mut out = String::with_capacity(trail.len());
                let mut prev: Option<char> = None;
                for c in trail.chars() {
                    if Some(c) == prev && matches!(c, ',' | '.' | '!' | '?' | ';' | ':') {
                        continue;
                    }
                    out.push(c);
                    prev = Some(c);
                }
                *trail = out;
            }
        }
    }
}

/// Append a full stop if the text ends with a word character.
pub fn ensure_final_punctuation(text: &mut String) {
    if let Some(last) = text.chars().last() {
        if last.is_alphanumeric() {
            text.push('.');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postprocess::text::{render, tokenize};

    fn fillers() -> Vec<String> {
        ["um", "uh", "erm", "hmm"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn removes_fillers_and_their_commas() {
        let f = fillers();
        assert_eq!(render(&remove_fillers(tokenize("Um, so we should go."), &f)), "so we should go.");
        assert_eq!(render(&remove_fillers(tokenize("I was, uh, thinking about it"), &f)), "I was thinking about it");
        assert_eq!(render(&remove_fillers(tokenize("and that's it, um."), &f)), "and that's it.");
        assert_eq!(render(&remove_fillers(tokenize("and so um."), &f)), "and so.");
        assert_eq!(render(&remove_fillers(tokenize("The drum is loud"), &f)), "The drum is loud");
    }

    #[test]
    fn removes_stutters_but_keeps_idioms() {
        assert_eq!(render(&remove_repeats(tokenize("the the cat"))), "the cat");
        assert_eq!(render(&remove_repeats(tokenize("I I think so"))), "I think so");
        assert_eq!(render(&remove_repeats(tokenize("I think I think we should"))), "I think we should");
        assert_eq!(render(&remove_repeats(tokenize("I had had enough"))), "I had had enough");
        assert_eq!(render(&remove_repeats(tokenize("no no no"))), "no no no");
        assert_eq!(render(&remove_repeats(tokenize("Wait. Wait for me"))), "Wait. Wait for me");
        assert_eq!(render(&remove_repeats(tokenize("bye bye"))), "bye bye");
    }

    #[test]
    fn capitalizes_sentences_and_pronoun() {
        let mut p = tokenize("so i think i'm done. next one? sure\nnew line e.g. this");
        capitalize(&mut p, true);
        assert_eq!(render(&p), "So I think I'm done. Next one? Sure\nNew line e.g. this");
        let mut p = tokenize("ami bhalo achi. tumi?");
        capitalize(&mut p, false);
        assert_eq!(render(&p), "Ami bhalo achi. Tumi?");
    }

    #[test]
    fn tidies_doubled_punctuation() {
        let mut p = tokenize("Hello,, world.. wait... ok?!");
        tidy_punctuation(&mut p);
        assert_eq!(render(&p), "Hello, world. wait... ok?!");
    }

    #[test]
    fn final_punctuation() {
        let mut s = "Send it".to_string();
        ensure_final_punctuation(&mut s);
        assert_eq!(s, "Send it.");
        let mut s = "Done?".to_string();
        ensure_final_punctuation(&mut s);
        assert_eq!(s, "Done?");
    }
}
