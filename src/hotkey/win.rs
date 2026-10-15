//! Global hotkeys with `RegisterHotKey` plus release detection for
//! push-to-talk.
//!
//! `RegisterHotKey` is the documented way to own a system-wide shortcut: the
//! key press is consumed (it never reaches the focused app) and it works
//! even when an elevated window has focus. It reports only presses, so
//! while push-to-talk is held a 10 ms timer polls `GetAsyncKeyState`; the
//! recording stops when the main key or any of its modifiers is released.

use windows_sys::Win32::Foundation::{GetLastError, HWND};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{vk, Hotkey};
use crate::config::HotkeyConfig;
use crate::events::HotkeyEvent;

pub const ID_PTT: i32 = 0x5301;
pub const ID_TOGGLE: i32 = 0x5302;
pub const ID_CANCEL: i32 = 0x5303;
const ID_PROBE: i32 = 0x53FF;
pub const TIMER_PTT_POLL: usize = 0x5310;
const POLL_MS: u32 = 10;

fn is_down(code: u16) -> bool {
    // SAFETY: plain Win32 call.
    (unsafe { GetAsyncKeyState(code as i32) } as u16 & 0x8000) != 0
}

fn register(hwnd: HWND, id: i32, hk: &Hotkey) -> Result<(), String> {
    // SAFETY: hwnd is our window; id is unique within it.
    let ok = unsafe { RegisterHotKey(hwnd, id, hk.mods.register_flags() | MOD_NOREPEAT, hk.vk as u32) };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        return Err(if code == 1409 {
            format!("{hk} is already used by another application or by Windows; choose another shortcut")
        } else {
            format!("cannot register {hk} (Windows error {code})")
        });
    }
    Ok(())
}

/// Check that `hk` can be registered (no other application owns it) by
/// registering and releasing it under a scratch id. Call it while the
/// registry is suspended so SwiftVoice's own shortcuts do not count.
pub fn probe(hwnd: HWND, hk: &Hotkey) -> Result<(), String> {
    register(hwnd, ID_PROBE, hk)?;
    unregister(hwnd, ID_PROBE);
    Ok(())
}

fn unregister(hwnd: HWND, id: i32) {
    // SAFETY: unregistering an unknown id is harmless.
    unsafe {
        UnregisterHotKey(hwnd, id);
    }
}

pub struct HotkeyRegistry {
    hwnd: HWND,
    ptt: Option<Hotkey>,
    toggle: Option<Hotkey>,
    cancel: Option<Hotkey>,
    cancel_active: bool,
    ptt_held: bool,
    suspended: bool,
}

impl HotkeyRegistry {
    pub fn new(hwnd: HWND) -> HotkeyRegistry {
        HotkeyRegistry { hwnd, ptt: None, toggle: None, cancel: None, cancel_active: false, ptt_held: false, suspended: false }
    }

    fn unregister_all(&mut self) {
        unregister(self.hwnd, ID_PTT);
        unregister(self.hwnd, ID_TOGGLE);
        unregister(self.hwnd, ID_CANCEL);
        self.cancel_active = false;
    }

    /// (Re)register the configured shortcuts. Returns a combined message
    /// for any that could not be registered (the others still work).
    pub fn apply(&mut self, cfg: &HotkeyConfig) -> Result<(), String> {
        self.unregister_all();
        let mut errors = Vec::new();
        let mut parse = |name: &str, s: &str, allow_escape: bool| -> Option<Hotkey> {
            if s.trim().is_empty() {
                return None;
            }
            match Hotkey::parse(s).and_then(|h| h.check_global(allow_escape).map(|_| h)) {
                Ok(h) => Some(h),
                Err(e) => {
                    errors.push(format!("{name}: {e}"));
                    None
                }
            }
        };
        self.ptt = parse("Push-to-talk", &cfg.push_to_talk, false);
        self.toggle = parse("Toggle", &cfg.toggle, false);
        self.cancel = parse("Cancel", &cfg.cancel, true);
        if !self.suspended {
            if let Some(h) = self.ptt {
                if let Err(e) = register(self.hwnd, ID_PTT, &h) {
                    errors.push(format!("Push-to-talk: {e}"));
                }
            }
            if let Some(h) = self.toggle {
                if let Err(e) = register(self.hwnd, ID_TOGGLE, &h) {
                    errors.push(format!("Toggle: {e}"));
                }
            }
        }
        log::info!(
            "hotkeys: push-to-talk {}, toggle {}, cancel {}",
            self.ptt.map(|h| h.to_string()).unwrap_or_else(|| "off".into()),
            self.toggle.map(|h| h.to_string()).unwrap_or_else(|| "off".into()),
            self.cancel.map(|h| h.to_string()).unwrap_or_else(|| "off".into())
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    /// Register the cancel shortcut only while dictating, so Escape keeps
    /// working normally in every app the rest of the time.
    pub fn set_cancel_active(&mut self, active: bool) {
        if active == self.cancel_active || self.suspended {
            return;
        }
        match (active, self.cancel) {
            (true, Some(h)) => match register(self.hwnd, ID_CANCEL, &h) {
                Ok(()) => self.cancel_active = true,
                Err(e) => log::warn!("cancel shortcut unavailable: {e}"),
            },
            _ => {
                unregister(self.hwnd, ID_CANCEL);
                self.cancel_active = false;
            }
        }
    }

    /// Temporarily release every shortcut (while capturing a new one).
    pub fn suspend(&mut self) {
        self.unregister_all();
        self.suspended = true;
    }

    pub fn resume(&mut self, cfg: &HotkeyConfig) -> Result<(), String> {
        self.suspended = false;
        self.apply(cfg)
    }

    /// Handle WM_HOTKEY.
    pub fn on_hotkey(&mut self, id: i32) -> Option<HotkeyEvent> {
        match id {
            ID_PTT => {
                if self.ptt_held {
                    return None;
                }
                self.ptt_held = true;
                // SAFETY: timer on our own window.
                unsafe { SetTimer(self.hwnd, TIMER_PTT_POLL, POLL_MS, None) };
                Some(HotkeyEvent::PttDown)
            }
            ID_TOGGLE => Some(HotkeyEvent::Toggle),
            ID_CANCEL => Some(HotkeyEvent::Cancel),
            _ => None,
        }
    }

    /// Handle the push-to-talk polling timer.
    pub fn on_timer(&mut self) -> Option<HotkeyEvent> {
        let Some(h) = self.ptt.filter(|_| self.ptt_held) else {
            unsafe { KillTimer(self.hwnd, TIMER_PTT_POLL) };
            return None;
        };
        let main_up = !is_down(h.vk);
        let m = h.mods;
        let modifier_up = (m.ctrl && !is_down(vk::CONTROL))
            || (m.alt && !is_down(vk::MENU))
            || (m.shift && !is_down(vk::SHIFT))
            || (m.win && !is_down(vk::LWIN) && !is_down(vk::RWIN));
        if main_up || modifier_up {
            self.ptt_held = false;
            unsafe { KillTimer(self.hwnd, TIMER_PTT_POLL) };
            return Some(HotkeyEvent::PttUp);
        }
        None
    }
}

impl Drop for HotkeyRegistry {
    fn drop(&mut self) {
        self.unregister_all();
    }
}
