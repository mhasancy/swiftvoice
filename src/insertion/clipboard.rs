//! Clipboard abstraction used by paste insertion.
//!
//! The paste flow is: snapshot every clipboard format, put the dictated
//! text on the clipboard (marked so Windows clipboard history, cloud
//! clipboard and clipboard managers ignore it), send the paste shortcut,
//! and a moment later restore the snapshot. The restore only happens if the
//! clipboard still holds our text (its sequence number is unchanged); if
//! anything else copied in the meantime, that newer content is kept.

use std::time::{Duration, Instant};

use anyhow::Result;

/// A copy of all clipboard formats that can be saved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub formats: Vec<(u32, Vec<u8>)>,
}

impl ClipboardSnapshot {
    pub fn byte_size(&self) -> usize {
        self.formats.iter().map(|(_, d)| d.len()).sum()
    }
}

#[derive(Debug)]
pub enum SnapshotError {
    /// Larger than the configured limit; caller should type instead.
    TooLarge(usize),
    /// Another program holds the clipboard open.
    Busy,
    Other(String),
}

pub trait ClipboardBackend {
    /// Changes whenever the clipboard content changes.
    fn sequence(&self) -> u32;
    fn snapshot(&mut self, max_bytes: usize) -> std::result::Result<ClipboardSnapshot, SnapshotError>;
    /// Replace the clipboard with `text`. `transient` = paste-and-restore
    /// (also excluded from clipboard history). Returns the new sequence.
    fn set_text(&mut self, text: &str, transient: bool) -> Result<u32>;
    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()>;
}

/// A restore scheduled after a paste.
pub struct PendingRestore {
    pub snapshot: ClipboardSnapshot,
    pub our_sequence: u32,
    pub due: Instant,
}

/// What happened to a pending restore.
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    /// Something else changed the clipboard after our paste; kept it.
    SkippedChanged,
    Failed(String),
}

pub fn schedule(snapshot: ClipboardSnapshot, our_sequence: u32, delay: Duration) -> PendingRestore {
    PendingRestore { snapshot, our_sequence, due: Instant::now() + delay }
}

/// Restore now (if the clipboard still holds our text).
pub fn perform_restore<C: ClipboardBackend + ?Sized>(clip: &mut C, pending: PendingRestore) -> RestoreOutcome {
    if clip.sequence() != pending.our_sequence {
        return RestoreOutcome::SkippedChanged;
    }
    match clip.restore(&pending.snapshot) {
        Ok(()) => RestoreOutcome::Restored,
        Err(e) => RestoreOutcome::Failed(format!("{e:#}")),
    }
}

/// In-memory clipboard used by tests (and as executable documentation of
/// the contract the Windows backend implements).
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    pub formats: Vec<(u32, Vec<u8>)>,
    pub seq: u32,
    pub busy: bool,
    pub history: Vec<String>,
}

pub const FAKE_CF_UNICODETEXT: u32 = 13;

impl MemoryClipboard {
    pub fn with_text(text: &str) -> MemoryClipboard {
        MemoryClipboard { formats: vec![(FAKE_CF_UNICODETEXT, text.as_bytes().to_vec())], seq: 1, ..Default::default() }
    }

    pub fn text(&self) -> Option<String> {
        self.formats.iter().find(|(f, _)| *f == FAKE_CF_UNICODETEXT).map(|(_, d)| String::from_utf8_lossy(d).into_owned())
    }

    /// Simulate another program copying something.
    pub fn external_copy(&mut self, text: &str) {
        self.formats = vec![(FAKE_CF_UNICODETEXT, text.as_bytes().to_vec())];
        self.seq += 1;
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn sequence(&self) -> u32 {
        self.seq
    }

    fn snapshot(&mut self, max_bytes: usize) -> std::result::Result<ClipboardSnapshot, SnapshotError> {
        if self.busy {
            return Err(SnapshotError::Busy);
        }
        let snap = ClipboardSnapshot { formats: self.formats.clone() };
        if snap.byte_size() > max_bytes {
            return Err(SnapshotError::TooLarge(snap.byte_size()));
        }
        Ok(snap)
    }

    fn set_text(&mut self, text: &str, transient: bool) -> Result<u32> {
        if self.busy {
            anyhow::bail!("clipboard busy");
        }
        self.formats = vec![(FAKE_CF_UNICODETEXT, text.as_bytes().to_vec())];
        if !transient {
            self.history.push(text.to_string());
        }
        self.seq += 1;
        Ok(self.seq)
    }

    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()> {
        self.formats = snapshot.formats.clone();
        self.seq += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_previous_content_when_unchanged() {
        let mut c = MemoryClipboard::with_text("user data");
        let snap = c.snapshot(1024).unwrap();
        let seq = c.set_text("dictated", true).unwrap();
        assert_eq!(c.text().unwrap(), "dictated");
        let pending = schedule(snap, seq, Duration::from_millis(0));
        assert_eq!(perform_restore(&mut c, pending), RestoreOutcome::Restored);
        assert_eq!(c.text().unwrap(), "user data");
        assert!(c.history.is_empty(), "transient text must not enter history");
    }

    #[test]
    fn keeps_newer_content_copied_after_the_paste() {
        let mut c = MemoryClipboard::with_text("old");
        let snap = c.snapshot(1024).unwrap();
        let seq = c.set_text("dictated", true).unwrap();
        c.external_copy("copied later");
        let pending = schedule(snap, seq, Duration::from_millis(0));
        assert_eq!(perform_restore(&mut c, pending), RestoreOutcome::SkippedChanged);
        assert_eq!(c.text().unwrap(), "copied later");
    }

    #[test]
    fn empty_clipboard_is_restored_as_empty() {
        let mut c = MemoryClipboard::default();
        let snap = c.snapshot(1024).unwrap();
        assert!(snap.formats.is_empty());
        let seq = c.set_text("x", true).unwrap();
        assert_eq!(perform_restore(&mut c, schedule(snap, seq, Duration::ZERO)), RestoreOutcome::Restored);
        assert!(c.text().is_none());
    }

    #[test]
    fn multiple_formats_round_trip() {
        let mut c =
            MemoryClipboard { formats: vec![(13, b"t".to_vec()), (49_161, vec![1, 2, 3]), (8, vec![0; 64])], seq: 5, ..Default::default() };
        let snap = c.snapshot(1 << 20).unwrap();
        let seq = c.set_text("dictated", true).unwrap();
        perform_restore(&mut c, schedule(snap.clone(), seq, Duration::ZERO));
        assert_eq!(c.formats, snap.formats);
    }

    #[test]
    fn oversized_or_busy_clipboards_are_reported() {
        let mut c = MemoryClipboard::with_text(&"x".repeat(100));
        assert!(matches!(c.snapshot(10), Err(SnapshotError::TooLarge(100))));
        c.busy = true;
        assert!(matches!(c.snapshot(1000), Err(SnapshotError::Busy)));
    }
}
