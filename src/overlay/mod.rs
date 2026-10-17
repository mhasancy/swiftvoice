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

    /// The overlay is always drawn as a bare dot, never a text card; this is
    /// the dot's color. `Hidden` maps to the transparent key 0 (invisible).
    pub fn dot_color(&self) -> u32 {
        fn rgb(r: u32, g: u32, b: u32) -> u32 {
            r | g << 8 | b << 16
        }
        match self {
            OverlayView::Hidden => 0,
            OverlayView::Listening { .. } => rgb(0xFF, 0x45, 0x3A),
            OverlayView::Processing { .. } => rgb(0xFF, 0xB0, 0x20),
            OverlayView::Result { tone, .. } | OverlayView::Message { tone, .. } => match tone {
                Tone::Info => rgb(0x4C, 0x9A, 0xFF),
                Tone::Success => rgb(0x3D, 0xD6, 0x8C),
                Tone::Warning => rgb(0xFF, 0xB0, 0x20),
                Tone::Error => rgb(0xFF, 0x5A, 0x52),
            },
        }
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

    #[test]
    fn every_state_is_a_dot_with_distinct_result_colors() {
        use super::{OverlayView, Tone};
        assert_eq!(OverlayView::Hidden.dot_color(), 0, "hidden draws nothing");
        let listening = OverlayView::Listening { level: 0.0, text: "hello".into(), hint: None };
        let processing = OverlayView::Processing { text: "hello".into() };
        let inserted = OverlayView::Result { text: "Inserted".into(), tone: Tone::Success, detail: None };
        let copied = OverlayView::Result { text: "Copied to clipboard".into(), tone: Tone::Warning, detail: None };
        let failed = OverlayView::Message { text: "No speech detected".into(), tone: Tone::Error };
        for v in [&listening, &processing, &inserted, &copied, &failed] {
            assert_ne!(v.dot_color(), 0, "{v:?}");
        }
        assert_ne!(inserted.dot_color(), copied.dot_color(), "inserted and copied flash in different colors");
        assert_ne!(inserted.dot_color(), failed.dot_color());
        assert_ne!(copied.dot_color(), failed.dot_color());
    }
}
