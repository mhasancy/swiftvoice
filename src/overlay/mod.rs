//! The small floating status window shown while dictating.
//!
//! The view model is platform independent; `win.rs` draws it as a
//! click-through, never-focused, always-on-top layered window so it can
//! never steal the caret from the app you are typing into.

#[cfg(windows)]
pub mod win;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OverlayView {
    Hidden,
    /// Recording; `text` is the live partial transcript (may be empty).
    Listening {
        level: f32,
        text: String,
        hint: Option<String>,
    },
    /// Speech ended; transcription or rewrite in progress.
    Processing {
        text: String,
    },
    /// Text was inserted (or placed on the clipboard).
    Result {
        text: String,
        tone: Tone,
        detail: Option<String>,
    },
    /// A status message (model loading, errors, downloads).
    Message {
        text: String,
        tone: Tone,
    },
}

impl OverlayView {
    pub fn is_hidden(&self) -> bool {
        matches!(self, OverlayView::Hidden)
    }
}

/// Keep the overlay text short: the last `max` characters, cut at a word.
pub fn tail_text(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - max).collect();
    match tail.find(' ') {
        Some(i) if i < tail.len() / 2 => format!("…{}", &tail[i + 1..]),
        _ => format!("…{tail}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tail_text_keeps_the_end() {
        assert_eq!(super::tail_text("short", 10), "short");
        let t = super::tail_text("the quick brown fox jumps over the lazy dog", 15);
        assert!(t.starts_with('…'));
        assert!(t.ends_with("lazy dog"));
        assert!(t.chars().count() <= 16);
    }
}
