//! SentencePiece token tables (`tokens.txt`: one "<piece> <id>" per line).

use std::path::Path;

use anyhow::{bail, Context, Result};

pub struct TokenTable {
    pieces: Vec<String>,
    blank: usize,
}

impl TokenTable {
    pub fn load(path: &Path) -> Result<TokenTable> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read token table {}", path.display()))?;
        TokenTable::parse(&text).with_context(|| format!("invalid token table {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<TokenTable> {
        let mut entries: Vec<(usize, String)> = Vec::new();
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                continue;
            }
            let (piece, id) = line.rsplit_once(' ').with_context(|| format!("line {}: expected \"<piece> <id>\"", lineno + 1))?;
            let id: usize = id.trim().parse().with_context(|| format!("line {}: bad id \"{id}\"", lineno + 1))?;
            entries.push((id, piece.to_string()));
        }
        if entries.is_empty() {
            bail!("no tokens");
        }
        let max_id = entries.iter().map(|e| e.0).max().unwrap_or(0);
        if max_id + 1 != entries.len() {
            bail!("token ids are not contiguous (max id {max_id}, {} entries)", entries.len());
        }
        let mut pieces = vec![String::new(); entries.len()];
        for (id, piece) in entries {
            pieces[id] = piece;
        }
        let blank = pieces.iter().position(|p| p == "<blk>" || p == "<blank>").unwrap_or(pieces.len() - 1);
        Ok(TokenTable { pieces, blank })
    }

    /// Number of entries including the blank token.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    pub fn blank_id(&self) -> usize {
        self.blank
    }

    /// Join pieces, turning the SentencePiece word marker into spaces and
    /// dropping special tokens such as `<unk>` or `<|nospeech|>`.
    /// True if the piece starts a new word (SentencePiece `▁` prefix).
    pub fn is_word_start(&self, id: usize) -> bool {
        self.pieces.get(id).is_some_and(|p| p.starts_with('\u{2581}'))
    }

    pub fn decode(&self, ids: &[usize]) -> String {
        let mut out = String::new();
        for &id in ids {
            let Some(piece) = self.pieces.get(id) else { continue };
            if id == self.blank || (piece.starts_with('<') && piece.ends_with('>') && piece.len() > 2) {
                continue;
            }
            out.push_str(piece);
        }
        let text = out.replace('\u{2581}', " ");
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "<unk> 0\n\u{2581}hello 1\n, 2\n\u{2581}wor 3\nld 4\n<|nospeech|> 5\n\u{2581}\u{0995} 6\n<blk> 7\n";

    #[test]
    fn parses_and_finds_blank() {
        let t = TokenTable::parse(TABLE).unwrap();
        assert_eq!(t.len(), 8);
        assert_eq!(t.blank_id(), 7);
    }

    #[test]
    fn decodes_word_markers_and_skips_specials() {
        let t = TokenTable::parse(TABLE).unwrap();
        assert_eq!(t.decode(&[1, 2, 3, 4]), "hello, world");
        assert_eq!(t.decode(&[0, 5, 1, 7, 6]), "hello \u{0995}");
        assert_eq!(t.decode(&[]), "");
        assert_eq!(t.decode(&[99]), "");
    }

    #[test]
    fn rejects_gaps_and_garbage() {
        assert!(TokenTable::parse("a 0\nb 2\n").is_err());
        assert!(TokenTable::parse("nospace\n").is_err());
        assert!(TokenTable::parse("").is_err());
    }
}
