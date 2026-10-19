//! Token model shared by the Clean-mode passes.
//!
//! Text is split on whitespace into [`Piece`]s. A word keeps its leading
//! and trailing punctuation separately from its core so passes can match on
//! words ("um," matches the filler "um") while preserving punctuation. Voice
//! commands become structural pieces (newline, punctuation attached to the
//! previous or next word) and [`render`] applies the spacing rules.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Word {
        lead: String,
        core: String,
        trail: String,
    },
    /// `n` line breaks (1 = new line, 2 = new paragraph).
    Newline(usize),
    /// Punctuation glued to the previous word: , . ? ! ) ] }
    AttachLeft(String),
    /// Punctuation glued to the next word: ( [ { and opening quotes.
    AttachRight(String),
}

impl Piece {
    pub fn word(lead: &str, core: &str, trail: &str) -> Piece {
        Piece::Word { lead: lead.into(), core: core.into(), trail: trail.into() }
    }

    pub fn core_lower(&self) -> Option<String> {
        match self {
            Piece::Word { core, .. } => Some(core.to_lowercase()),
            _ => None,
        }
    }
}

/// Punctuation that may be split off the ends of a word. This is an explicit
/// list rather than "not alphanumeric" because many scripts (Bengali,
/// Hindi, Thai, ...) use combining marks such as vowel signs, which are not
/// alphanumeric but are part of the word.
pub(crate) fn is_edge_punct(c: char) -> bool {
    matches!(
        c,
        ',' | '.'
            | ';'
            | ':'
            | '!'
            | '?'
            | '"'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '*'
            | '~'
            | '`'
            | '^'
            | '|'
            | '/'
            | '\\'
            | '-'
            | '\''
            | '…'
            | '“'
            | '”'
            | '‘'
            | '’'
            | '«'
            | '»'
            | '—'
            | '–'
            | '।'
            | '॥'
            | '¿'
            | '¡'
            | '、'
            | '。'
            | '，'
            | '！'
            | '？'
            | '：'
            | '；'
    )
}

/// Punctuation that ends a word or clause (as opposed to opening brackets
/// and quotes).
pub fn is_trailing_punct(c: char) -> bool {
    matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '…' | '।' | '॥' | '、' | '。' | '，' | '！' | '？' | '：' | '；')
}

/// Split a whitespace-free token into (lead, core, trail).
pub fn split_token(tok: &str) -> (String, String, String) {
    let chars: Vec<char> = tok.chars().collect();
    let mut start = 0;
    while start < chars.len() && is_edge_punct(chars[start]) {
        start += 1;
    }
    let mut end = chars.len();
    while end > start && is_edge_punct(chars[end - 1]) {
        end -= 1;
    }
    // Keep apostrophes/quotes that only wrap the word in the lead/trail.
    let lead: String = chars[..start].iter().collect();
    let core: String = chars[start..end].iter().collect();
    let trail: String = chars[end..].iter().collect();
    (lead, core, trail)
}

/// Tokenise text into word pieces; existing line breaks are preserved.
pub fn tokenize(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            match out.last_mut() {
                Some(Piece::Newline(n)) => *n += 1,
                _ => out.push(Piece::Newline(1)),
            }
        }
        for tok in line.split_whitespace() {
            let (lead, core, trail) = split_token(tok);
            if core.is_empty() {
                // Pure punctuation token (e.g. "-" or "...").
                out.push(Piece::Word { lead: String::new(), core: String::new(), trail: tok.to_string() });
            } else {
                out.push(Piece::Word { lead, core, trail });
            }
        }
    }
    out
}

/// Join pieces with the spacing rules described in the module docs.
pub fn render(pieces: &[Piece]) -> String {
    let mut out = String::new();
    let mut glue_next = false;
    for p in pieces {
        match p {
            Piece::Word { lead, core, trail } => {
                if !out.is_empty() && !glue_next && !out.ends_with('\n') {
                    out.push(' ');
                }
                out.push_str(lead);
                out.push_str(core);
                out.push_str(trail);
                glue_next = false;
            }
            Piece::Newline(n) => {
                while out.ends_with(' ') {
                    out.pop();
                }
                for _ in 0..*n {
                    out.push('\n');
                }
                glue_next = false;
            }
            Piece::AttachLeft(s) => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push_str(s);
                glue_next = false;
            }
            Piece::AttachRight(s) => {
                if !out.is_empty() && !glue_next && !out.ends_with('\n') {
                    out.push(' ');
                }
                out.push_str(s);
                glue_next = true;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_punctuation_from_words() {
        assert_eq!(split_token("Hello,"), ("".into(), "Hello".into(), ",".into()));
        assert_eq!(split_token("\"quoted\"."), ("\"".into(), "quoted".into(), "\".".into()));
        assert_eq!(split_token("don't"), ("".into(), "don't".into(), "".into()));
        assert_eq!(split_token("(maybe)"), ("(".into(), "maybe".into(), ")".into()));
        assert_eq!(split_token("3.14"), ("".into(), "3.14".into(), "".into()));
        assert_eq!(split_token("আমি।"), ("".into(), "আমি".into(), "।".into()));
    }

    #[test]
    fn tokenize_render_round_trip() {
        for s in ["Hello, world.", "One (two) three!", "Line one\nLine two", "a - b ... c", "Para\n\nNext"] {
            assert_eq!(render(&tokenize(s)), s);
        }
    }

    #[test]
    fn render_applies_attachment_rules() {
        let p = vec![
            Piece::word("", "call", ""),
            Piece::AttachRight("(".into()),
            Piece::word("", "maybe", ""),
            Piece::AttachLeft(")".into()),
            Piece::word("", "now", ""),
            Piece::AttachLeft("?".into()),
            Piece::Newline(2),
            Piece::word("", "next", ""),
        ];
        assert_eq!(render(&p), "call (maybe) now?\n\nnext");
    }
}
