//! Inserting text at the cursor of the focused application.
//!
//! Strategy (`insertion.method = "auto"`):
//! 1. Wait briefly for the user to release Ctrl/Alt/Shift/Win (otherwise
//!    our Ctrl+V would become Ctrl+Alt+V); release them synthetically if
//!    they stay down.
//! 2. Check the target: no focused window or SwiftVoice itself -> the text
//!    goes to the clipboard with a notice; a window running as
//!    administrator (when SwiftVoice is not) -> same, because Windows blocks
//!    simulated input into it (UIPI) without telling the sender.
//! 3. Paste through the clipboard (fast, exact Unicode, works in browsers,
//!    Office, Electron apps, terminals), restoring the previous clipboard.
//!    Apps listed in `type_apps`, or a clipboard we cannot snapshot, get
//!    typed text (Unicode key events) instead, which never touches the
//!    clipboard.

pub mod clipboard;
#[cfg(windows)]
pub mod win;

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError, SendTimeoutError, Sender};

use crate::config::{InsertMethod, InsertTarget, InsertionConfig};
use crate::hotkey::Hotkey;
use clipboard::{perform_restore, schedule, ClipboardBackend, PendingRestore, RestoreOutcome, SnapshotError};

/// Opaque window handle (HWND on Windows).
pub type WindowHandle = isize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub handle: WindowHandle,
    /// Executable file name, lower case (e.g. "code.exe").
    pub process: String,
    /// The window belongs to SwiftVoice itself.
    pub own: bool,
    /// The owning process runs elevated (or we could not inspect it,
    /// which on Windows almost always means it is elevated).
    pub elevated: bool,
}

/// Operations on the interactive desktop.
pub trait Desktop {
    fn foreground(&self) -> Option<WindowInfo>;
    /// Wait until no modifier key is held; returns false on timeout.
    fn wait_modifiers_released(&self, max: Duration) -> bool;
    fn release_modifiers(&self);
    fn focus(&self, window: WindowHandle) -> bool;
    fn send_shortcut(&self, shortcut: &Hotkey) -> Result<()>;
    fn type_text(&self, text: &str) -> Result<()>;
    fn self_elevated(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct InsertSettings {
    pub method: InsertMethod,
    pub target: InsertTarget,
    pub append_space: bool,
    pub restore_clipboard: bool,
    pub restore_delay: Duration,
    pub paste_shortcut: Hotkey,
    pub paste_overrides: Vec<(String, Hotkey)>,
    pub type_apps: Vec<String>,
    pub modifier_wait: Duration,
    pub max_snapshot_bytes: usize,
}

impl InsertSettings {
    pub fn from_config(c: &InsertionConfig) -> InsertSettings {
        let parse =
            |s: &str| Hotkey::parse(s).unwrap_or(Hotkey { mods: crate::hotkey::Modifiers { ctrl: true, ..Default::default() }, vk: 0x56 });
        InsertSettings {
            method: c.method,
            target: c.target,
            append_space: c.append_space,
            restore_clipboard: c.restore_clipboard,
            restore_delay: Duration::from_millis(c.restore_delay_ms as u64),
            paste_shortcut: parse(&c.paste_shortcut),
            paste_overrides: c.paste_shortcuts.iter().map(|(k, v)| (k.to_lowercase(), parse(v))).collect(),
            type_apps: c.type_apps.iter().map(|s| s.to_lowercase()).collect(),
            modifier_wait: Duration::from_millis(c.modifier_wait_ms as u64),
            max_snapshot_bytes: c.max_clipboard_snapshot_mb as usize * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InsertRequest {
    pub id: u64,
    pub text: String,
    /// Window focused when dictation started (for `target = "original"`).
    pub original: Option<WindowHandle>,
    pub settings: InsertSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted {
        method: &'static str,
        app: String,
    },
    /// Text left on the clipboard for the user to paste.
    Clipboard {
        reason: String,
    },
    Failed(String),
    Empty,
}

#[derive(Debug, Clone)]
pub struct InsertReport {
    pub id: u64,
    pub outcome: InsertOutcome,
    pub elapsed_ms: u32,
}

pub struct Inserter<D: Desktop, C: ClipboardBackend> {
    pub desktop: D,
    pub clipboard: C,
    pending: Option<PendingRestore>,
}

impl<D: Desktop, C: ClipboardBackend> Inserter<D, C> {
    pub fn new(desktop: D, clipboard: C) -> Self {
        Inserter { desktop, clipboard, pending: None }
    }

    /// Restore the clipboard if a scheduled restore is due.
    pub fn tick(&mut self) {
        if self.pending.as_ref().map(|p| Instant::now() >= p.due).unwrap_or(false) {
            self.flush_restore();
        }
    }

    pub fn has_pending_restore(&self) -> bool {
        self.pending.is_some()
    }

    /// Restore immediately (before another paste, or at shutdown).
    pub fn flush_restore(&mut self) {
        if let Some(p) = self.pending.take() {
            match perform_restore(&mut self.clipboard, p) {
                RestoreOutcome::Restored => log::debug!("clipboard restored"),
                RestoreOutcome::SkippedChanged => log::info!("clipboard changed after paste; keeping the newer content"),
                RestoreOutcome::Failed(e) => log::warn!("could not restore the clipboard: {e}"),
            }
        }
    }

    fn clipboard_fallback(&mut self, text: &str, reason: String) -> InsertOutcome {
        self.flush_restore();
        match self.clipboard.set_text(text, false) {
            Ok(_) => InsertOutcome::Clipboard { reason },
            Err(e) => InsertOutcome::Failed(format!("{reason}, and the clipboard could not be set: {e:#}")),
        }
    }

    pub fn insert(&mut self, req: &InsertRequest) -> InsertOutcome {
        let s = &req.settings;
        let mut text = req.text.clone();
        if text.trim().is_empty() {
            return InsertOutcome::Empty;
        }
        if s.append_space && !text.ends_with(['\n', ' ']) {
            text.push(' ');
        }
        if !self.desktop.wait_modifiers_released(s.modifier_wait) {
            log::debug!("modifiers still held after {:?}; releasing them", s.modifier_wait);
            self.desktop.release_modifiers();
        }
        let mut target = self.desktop.foreground();
        if s.target == InsertTarget::Original {
            if let Some(orig) = req.original {
                if target.as_ref().map(|t| t.handle) != Some(orig) {
                    if !self.desktop.focus(orig) {
                        return self.clipboard_fallback(
                            &text,
                            "The original window could not be focused again; the text is on the clipboard".into(),
                        );
                    }
                    target = self.desktop.foreground();
                }
            }
        }
        let Some(target) = target else {
            return self.clipboard_fallback(&text, "No window has keyboard focus; the text is on the clipboard".into());
        };
        if target.own {
            return self.clipboard_fallback(&text, "No text field is focused; the text is on the clipboard".into());
        }
        if target.elevated && !self.desktop.self_elevated() {
            return self.clipboard_fallback(
                &text,
                format!(
                    "{} runs as administrator, and Windows blocks typing into it from normal apps. The text is on the clipboard; press Ctrl+V",
                    target.process
                ),
            );
        }
        let typed_app = s.type_apps.contains(&target.process);
        let method = match s.method {
            InsertMethod::Type => InsertMethod::Type,
            InsertMethod::Paste => InsertMethod::Paste,
            InsertMethod::Auto if typed_app => InsertMethod::Type,
            InsertMethod::Auto => InsertMethod::Paste,
        };
        if method == InsertMethod::Type {
            return match self.desktop.type_text(&text) {
                Ok(()) => InsertOutcome::Inserted { method: "type", app: target.process },
                Err(e) => InsertOutcome::Failed(format!("typing failed: {e:#}")),
            };
        }
        let shortcut = s.paste_overrides.iter().find(|(app, _)| *app == target.process).map(|(_, hk)| *hk).unwrap_or(s.paste_shortcut);
        self.flush_restore();
        let snapshot = if s.restore_clipboard {
            match self.clipboard.snapshot(s.max_snapshot_bytes) {
                Ok(snap) => Some(snap),
                Err(SnapshotError::TooLarge(n)) if s.method == InsertMethod::Auto => {
                    log::info!("clipboard holds {n} bytes (over the snapshot limit); typing instead of pasting");
                    return self.type_fallback(&text, target.process);
                }
                Err(SnapshotError::Busy) if s.method == InsertMethod::Auto => {
                    log::info!("clipboard is busy; typing instead of pasting");
                    return self.type_fallback(&text, target.process);
                }
                Err(e) => return InsertOutcome::Failed(format!("cannot read the clipboard: {e:?}")),
            }
        } else {
            None
        };
        let seq = match self.clipboard.set_text(&text, snapshot.is_some()) {
            Ok(seq) => seq,
            Err(e) if s.method == InsertMethod::Auto => {
                log::info!("cannot set the clipboard ({e:#}); typing instead");
                return self.type_fallback(&text, target.process);
            }
            Err(e) => return InsertOutcome::Failed(format!("cannot set the clipboard: {e:#}")),
        };
        if let Err(e) = self.desktop.send_shortcut(&shortcut) {
            if let Some(snap) = snapshot {
                let _ = self.clipboard.restore(&snap);
            }
            return InsertOutcome::Failed(format!("sending {shortcut} failed: {e:#}"));
        }
        if let Some(snap) = snapshot {
            self.pending = Some(schedule(snap, seq, s.restore_delay));
        }
        InsertOutcome::Inserted { method: "paste", app: target.process }
    }

    fn type_fallback(&mut self, text: &str, app: String) -> InsertOutcome {
        match self.desktop.type_text(text) {
            Ok(()) => InsertOutcome::Inserted { method: "type", app },
            Err(e) => InsertOutcome::Failed(format!("typing failed: {e:#}")),
        }
    }
}

/// Handle to the inserter thread.
pub struct InserterHandle {
    tx: Sender<InsertRequest>,
    join: Option<JoinHandle<()>>,
}

/// Why a request could not be handed to the inserter thread, carrying the
/// text back so the caller can keep it recoverable. The request is boxed
/// because it is far larger than the `Ok` value and only comes back on this
/// cold path.
#[derive(Debug)]
pub enum EnqueueError {
    /// The queue stayed full for the whole wait.
    Full(Box<InsertRequest>),
    /// The inserter thread has exited (shutdown, or a panic).
    Closed(Box<InsertRequest>),
}

/// How long [`InserterHandle::send`] waits for room in the queue before the
/// request comes back to the caller. The inserter thread works through
/// requests one at a time (modifier wait, paste, scheduled restore), so a
/// queue that is still full after this long is stuck, not merely busy.
const ENQUEUE_WAIT: Duration = Duration::from_millis(2_000);

impl InserterHandle {
    /// Hand `req` to the inserter thread, waiting up to `ENQUEUE_WAIT` for
    /// room so a momentary backlog costs a little time, not the text. On
    /// failure the request comes back: the caller must keep the text
    /// recoverable (put it on the clipboard) and tell the user instead of
    /// dropping it silently (see `WinPorts::insert`).
    pub fn send(&self, req: InsertRequest) -> std::result::Result<(), EnqueueError> {
        self.send_within(req, ENQUEUE_WAIT)
    }

    fn send_within(&self, req: InsertRequest, wait: Duration) -> std::result::Result<(), EnqueueError> {
        match self.tx.send_timeout(req, wait) {
            Ok(()) => Ok(()),
            Err(SendTimeoutError::Timeout(req)) => {
                log::error!("insertion queue full for {wait:?}; text handed back to the caller");
                Err(EnqueueError::Full(Box::new(req)))
            }
            Err(SendTimeoutError::Disconnected(req)) => {
                log::error!("inserter thread exited; text handed back to the caller");
                Err(EnqueueError::Closed(Box::new(req)))
            }
        }
    }
}

impl Drop for InserterHandle {
    fn drop(&mut self) {
        // Closing the channel ends the thread after a final restore.
        let (dead_tx, _) = crossbeam_channel::bounded(1);
        drop(std::mem::replace(&mut self.tx, dead_tx));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Run an inserter on its own thread so waiting for modifier release or a
/// clipboard restore never blocks the UI or the recogniser.
pub fn spawn<D, C, F>(make: F, report: Box<dyn Fn(InsertReport) + Send>) -> std::io::Result<InserterHandle>
where
    D: Desktop + 'static,
    C: ClipboardBackend + 'static,
    F: FnOnce() -> Inserter<D, C> + Send + 'static,
{
    let (tx, rx): (Sender<InsertRequest>, Receiver<InsertRequest>) = crossbeam_channel::bounded(32);
    let join = std::thread::Builder::new().name("swiftvoice-insert".into()).spawn(move || {
        let mut inserter = make();
        loop {
            let wait = if inserter.has_pending_restore() { Duration::from_millis(20) } else { Duration::from_secs(3600) };
            match rx.recv_timeout(wait) {
                Ok(req) => {
                    let started = Instant::now();
                    let outcome = inserter.insert(&req);
                    let elapsed_ms = started.elapsed().as_millis() as u32;
                    match &outcome {
                        InsertOutcome::Inserted { method, app } => {
                            log::info!("inserted {} chars by {method} into {app} in {elapsed_ms} ms", req.text.chars().count())
                        }
                        other => log::warn!("insertion: {other:?}"),
                    }
                    report(InsertReport { id: req.id, outcome, elapsed_ms });
                }
                Err(RecvTimeoutError::Timeout) => inserter.tick(),
                Err(RecvTimeoutError::Disconnected) => {
                    inserter.flush_restore();
                    return;
                }
            }
        }
    })?;
    Ok(InserterHandle { tx, join: Some(join) })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::clipboard::MemoryClipboard;
    use super::*;

    #[derive(Default)]
    struct FakeDesktop {
        fg: RefCell<Option<WindowInfo>>,
        modifiers_stuck: bool,
        elevated_self: bool,
        log: RefCell<Vec<String>>,
        focusable: Vec<WindowHandle>,
    }

    impl Desktop for FakeDesktop {
        fn foreground(&self) -> Option<WindowInfo> {
            self.fg.borrow().clone()
        }
        fn wait_modifiers_released(&self, _max: Duration) -> bool {
            !self.modifiers_stuck
        }
        fn release_modifiers(&self) {
            self.log.borrow_mut().push("release".into());
        }
        fn focus(&self, w: WindowHandle) -> bool {
            if self.focusable.contains(&w) {
                *self.fg.borrow_mut() = Some(win(w, "orig.exe"));
                true
            } else {
                false
            }
        }
        fn send_shortcut(&self, hk: &Hotkey) -> Result<()> {
            self.log.borrow_mut().push(format!("shortcut {hk}"));
            Ok(())
        }
        fn type_text(&self, text: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("type {text}"));
            Ok(())
        }
        fn self_elevated(&self) -> bool {
            self.elevated_self
        }
    }

    fn win(handle: WindowHandle, process: &str) -> WindowInfo {
        WindowInfo { handle, process: process.into(), own: false, elevated: false }
    }

    fn settings() -> InsertSettings {
        InsertSettings::from_config(&InsertionConfig { restore_delay_ms: 50, ..InsertionConfig::default() })
    }

    fn req(text: &str) -> InsertRequest {
        InsertRequest { id: 1, text: text.into(), original: Some(42), settings: settings() }
    }

    fn inserter(fg: Option<WindowInfo>) -> Inserter<FakeDesktop, MemoryClipboard> {
        let d = FakeDesktop { fg: RefCell::new(fg), ..Default::default() };
        Inserter::new(d, MemoryClipboard::with_text("precious clipboard"))
    }

    #[test]
    fn pastes_then_restores_the_clipboard() {
        let mut ins = inserter(Some(win(1, "notepad.exe")));
        let out = ins.insert(&req("Hello world"));
        assert_eq!(out, InsertOutcome::Inserted { method: "paste", app: "notepad.exe".into() });
        assert_eq!(ins.clipboard.text().unwrap(), "Hello world ");
        assert_eq!(ins.desktop.log.borrow().as_slice(), ["shortcut Ctrl+V"]);
        std::thread::sleep(Duration::from_millis(60));
        ins.tick();
        assert_eq!(ins.clipboard.text().unwrap(), "precious clipboard");
        assert!(ins.clipboard.history.is_empty());
    }

    #[test]
    fn back_to_back_insertions_restore_the_original_clipboard() {
        let mut ins = inserter(Some(win(1, "notepad.exe")));
        ins.insert(&req("one"));
        ins.insert(&req("two"));
        ins.flush_restore();
        assert_eq!(ins.clipboard.text().unwrap(), "precious clipboard");
    }

    #[test]
    fn per_app_paste_shortcut_and_typed_apps() {
        let mut ins = inserter(Some(win(1, "mintty.exe")));
        ins.insert(&req("ls"));
        assert_eq!(ins.desktop.log.borrow().last().unwrap(), "shortcut Shift+Insert");
        let mut s = settings();
        s.type_apps = vec!["game.exe".into()];
        let mut ins = inserter(Some(win(1, "game.exe")));
        let out = ins.insert(&InsertRequest { id: 2, text: "gg".into(), original: None, settings: s });
        assert_eq!(out, InsertOutcome::Inserted { method: "type", app: "game.exe".into() });
        assert_eq!(ins.clipboard.text().unwrap(), "precious clipboard", "typing must not touch the clipboard");
    }

    #[test]
    fn large_clipboard_falls_back_to_typing() {
        let mut ins = inserter(Some(win(1, "word.exe")));
        ins.clipboard = MemoryClipboard::with_text(&"x".repeat(2000));
        let mut s = settings();
        s.max_snapshot_bytes = 1000;
        let out = ins.insert(&InsertRequest { id: 3, text: "hi".into(), original: None, settings: s });
        assert_eq!(out, InsertOutcome::Inserted { method: "type", app: "word.exe".into() });
        assert_eq!(ins.clipboard.text().unwrap().len(), 2000);
    }

    #[test]
    fn elevated_or_missing_targets_get_the_clipboard() {
        let mut admin = win(1, "regedit.exe");
        admin.elevated = true;
        let mut ins = inserter(Some(admin));
        assert!(matches!(ins.insert(&req("x")), InsertOutcome::Clipboard { .. }));
        assert_eq!(ins.clipboard.text().unwrap(), "x ");
        assert_eq!(ins.clipboard.history, vec!["x ".to_string()], "fallback text stays in history so it can be pasted");
        let mut ins = inserter(None);
        assert!(matches!(ins.insert(&req("y")), InsertOutcome::Clipboard { .. }));
        let mut own = win(1, "swiftvoice.exe");
        own.own = true;
        let mut ins = inserter(Some(own));
        assert!(matches!(ins.insert(&req("z")), InsertOutcome::Clipboard { .. }));
    }

    #[test]
    fn elevated_target_works_when_we_are_elevated_too() {
        let mut admin = win(1, "regedit.exe");
        admin.elevated = true;
        let mut ins = inserter(Some(admin));
        ins.desktop.elevated_self = true;
        assert!(matches!(ins.insert(&req("x")), InsertOutcome::Inserted { .. }));
    }

    #[test]
    fn original_target_refocuses_or_falls_back() {
        let mut s = settings();
        s.target = InsertTarget::Original;
        let mut ins = inserter(Some(win(7, "other.exe")));
        ins.desktop.focusable = vec![42];
        let out = ins.insert(&InsertRequest { id: 4, text: "a".into(), original: Some(42), settings: s.clone() });
        assert_eq!(out, InsertOutcome::Inserted { method: "paste", app: "orig.exe".into() });
        let mut ins = inserter(Some(win(7, "other.exe")));
        let out = ins.insert(&InsertRequest { id: 5, text: "a".into(), original: Some(42), settings: s });
        assert!(matches!(out, InsertOutcome::Clipboard { .. }));
    }

    #[test]
    fn stuck_modifiers_are_released_first() {
        let mut ins = inserter(Some(win(1, "notepad.exe")));
        ins.desktop.modifiers_stuck = true;
        ins.insert(&req("x"));
        assert_eq!(ins.desktop.log.borrow()[0], "release");
    }

    #[test]
    fn empty_text_is_not_inserted_and_newlines_get_no_space() {
        let mut ins = inserter(Some(win(1, "notepad.exe")));
        assert_eq!(ins.insert(&req("   ")), InsertOutcome::Empty);
        ins.insert(&req("line\n"));
        assert_eq!(ins.clipboard.text().unwrap(), "line\n");
    }

    #[test]
    fn send_waits_for_a_slow_consumer_instead_of_dropping() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let h = InserterHandle { tx, join: None };
        assert!(h.send_within(req("in flight"), Duration::from_millis(10)).is_ok());
        let consumer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            // Take both requests before the receiver drops, or a send that
            // waited for room races the drop and fails as closed.
            let first = rx.recv().unwrap().text;
            let second = rx.recv_timeout(Duration::from_secs(5)).expect("the queued request must arrive, not be dropped").text;
            (first, second)
        });
        assert!(h.send_within(req("queued"), Duration::from_secs(5)).is_ok());
        assert_eq!(consumer.join().unwrap(), ("in flight".to_string(), "queued".to_string()));
    }

    #[test]
    fn send_returns_a_stuck_request_to_the_caller() {
        let (tx, rx) = crossbeam_channel::bounded(2);
        let h = InserterHandle { tx, join: None };
        assert!(h.send_within(req("one"), Duration::from_millis(10)).is_ok());
        assert!(h.send_within(req("two"), Duration::from_millis(10)).is_ok());
        let started = Instant::now();
        match h.send_within(req("three"), Duration::from_millis(60)) {
            Err(EnqueueError::Full(r)) => assert_eq!(r.text, "three"),
            Ok(()) => panic!("a full queue must time out, not accept the request"),
            Err(EnqueueError::Closed(_)) => panic!("the channel is still open"),
        }
        assert!(started.elapsed() >= Duration::from_millis(50), "a full queue must be waited on, not failed at once");
        assert_eq!(rx.recv().unwrap().text, "one");
        assert!(h.send_within(req("three"), Duration::from_millis(10)).is_ok());
    }

    #[test]
    fn send_returns_the_request_when_the_inserter_thread_is_gone() {
        let (tx, rx) = crossbeam_channel::bounded(2);
        let h = InserterHandle { tx, join: None };
        drop(rx);
        match h.send_within(req("gone"), Duration::from_millis(10)) {
            Err(EnqueueError::Closed(r)) => assert_eq!(r.text, "gone"),
            Ok(()) => panic!("a dropped receiver must not accept the request"),
            Err(EnqueueError::Full(_)) => panic!("a dropped receiver fails at once"),
        }
    }
}
