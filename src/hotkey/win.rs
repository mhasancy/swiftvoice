//! Global hotkeys with `RegisterHotKey` plus release detection for
//! push-to-talk.
//!
//! `RegisterHotKey` is the documented way to own a system-wide shortcut: the
//! key press is consumed (it never reaches the focused app) and it works
//! even when an elevated window has focus. It reports only presses, so
//! while push-to-talk is held a 10 ms timer polls `GetAsyncKeyState`; the
//! recording stops when the main key or any of its modifiers is released.
//!
//! Windows accepts a modifier-only shortcut such as RAlt in `RegisterHotKey`
//! but never posts its `WM_HOTKEY`, so those are owned by a low-level
//! keyboard hook instead. It consumes the key down and up and posts the
//! same events to the window a regular hotkey would.

use std::cell::RefCell;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, KillTimer, PostMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::{vk, Hotkey};
use crate::config::HotkeyConfig;
use crate::events::HotkeyEvent;

pub const ID_PTT: i32 = 0x5301;
pub const ID_TOGGLE: i32 = 0x5302;
pub const ID_CANCEL: i32 = 0x5303;
const ID_PROBE: i32 = 0x53FF;
pub const TIMER_PTT_POLL: usize = 0x5310;
/// Hotkey events from the keyboard hook are posted to the app window with
/// this message instead of `WM_HOTKEY`.
pub const WM_SV_HOTKEY: u32 = WM_APP + 0x51;
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

/// Modifier keys held right now. Alt keeps its sides: a lone RAlt shortcut
/// must not fire while the other Alt is down, and the other way around.
#[derive(Clone, Copy, Default)]
struct HeldMods {
    ctrl: bool,
    shift: bool,
    win: bool,
    alt_left: bool,
    alt_right: bool,
}

impl HeldMods {
    fn now() -> HeldMods {
        HeldMods {
            ctrl: is_down(vk::CONTROL),
            shift: is_down(vk::SHIFT),
            win: is_down(vk::LWIN) || is_down(vk::RWIN),
            alt_left: is_down(vk::LMENU),
            alt_right: is_down(vk::RMENU),
        }
    }

    /// `RegisterHotKey` matches modifiers exactly; for a right-Alt shortcut
    /// the left Alt is the "Alt" modifier, and the other way around.
    fn matches(self, hk: &Hotkey) -> bool {
        let other_alt = if hk.vk == vk::RMENU { self.alt_left } else { self.alt_right };
        self.ctrl == hk.mods.ctrl && self.shift == hk.mods.shift && self.win == hk.mods.win && other_alt == hk.mods.alt
    }
}

/// The modifier-only shortcuts the hook owns, plus the state needed to
/// behave like `RegisterHotKey`: presses are consumed, repeats while held
/// are ignored and the matching release is consumed too.
#[derive(Clone, Copy, Default)]
struct HookKeys {
    ptt: Option<Hotkey>,
    toggle: Option<Hotkey>,
    cancel: Option<Hotkey>,
    cancel_active: bool,
}

#[derive(Clone, Copy)]
struct HookState {
    hwnd: HWND,
    keys: HookKeys,
    /// The key whose press was consumed (0 = none).
    held: u16,
}

impl HookState {
    /// A key went down: (consume it, hotkey id to post).
    fn on_down(&mut self, code: u16) -> (bool, Option<i32>) {
        if self.held != 0 {
            return (self.held == code, None);
        }
        let mods = HeldMods::now();
        let keys = self.keys;
        let cancel = keys.cancel.filter(|_| keys.cancel_active);
        for (id, hk) in [(ID_PTT, keys.ptt), (ID_TOGGLE, keys.toggle), (ID_CANCEL, cancel)] {
            if let Some(hk) = hk {
                if hk.vk == code && mods.matches(&hk) {
                    self.held = code;
                    return (true, Some(id));
                }
            }
        }
        (false, None)
    }

    /// A key went up: swallow the release of a key whose press we consumed.
    fn on_up(&mut self, code: u16) -> bool {
        if self.held == code {
            self.held = 0;
            return true;
        }
        false
    }
}

thread_local! {
    static HOOK: RefCell<HookState> = const {
        RefCell::new(HookState {
            hwnd: null_mut(),
            keys: HookKeys { ptt: None, toggle: None, cancel: None, cancel_active: false },
            held: 0,
        })
    };
}

/// Low-level keyboard hook for modifier-only shortcuts. It runs on the
/// thread that installed it (the message loop owns it), consumes the
/// matched key down and up and posts [`WM_SV_HOTKEY`], so the rest of the
/// app cannot tell the difference from a regular hotkey.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // A negative code must reach the next hook without inspection.
    if code >= 0 {
        let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP);
        if down || up {
            let event = &*(lparam as *const KBDLLHOOKSTRUCT);
            let (consume, post) = HOOK.with(|h| {
                let mut h = h.borrow_mut();
                let (consume, fire) = if down { h.on_down(event.vkCode as u16) } else { (h.on_up(event.vkCode as u16), None) };
                (consume, fire.map(|id| (h.hwnd, id)))
            });
            if let Some((hwnd, id)) = post {
                PostMessageW(hwnd, WM_SV_HOTKEY, id as WPARAM, 0);
            }
            if consume {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

fn hook_install(hwnd: HWND) -> Result<HHOOK, String> {
    // SAFETY: the procedure lives for the whole run of the process.
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(null()), 0) };
    if hook.is_null() {
        return Err(format!("cannot watch modifier-only shortcuts (Windows error {})", unsafe { GetLastError() }));
    }
    HOOK.with(|h| h.borrow_mut().hwnd = hwnd);
    Ok(hook)
}

fn hook_uninstall(hook: HHOOK) {
    // SAFETY: the handle came from hook_install and is removed once.
    unsafe {
        UnhookWindowsHookEx(hook);
    }
    HOOK.with(|h| {
        let mut h = h.borrow_mut();
        h.keys = HookKeys::default();
        h.held = 0;
    });
}

pub struct HotkeyRegistry {
    hwnd: HWND,
    ptt: Option<Hotkey>,
    toggle: Option<Hotkey>,
    cancel: Option<Hotkey>,
    cancel_active: bool,
    ptt_held: bool,
    suspended: bool,
    /// Owns the modifier-only shortcuts whenever any are configured.
    hook: Option<HHOOK>,
}

impl HotkeyRegistry {
    pub fn new(hwnd: HWND) -> HotkeyRegistry {
        HotkeyRegistry { hwnd, ptt: None, toggle: None, cancel: None, cancel_active: false, ptt_held: false, suspended: false, hook: None }
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
            if let Some(h) = self.ptt.filter(|h| !h.needs_keyboard_hook()) {
                if let Err(e) = register(self.hwnd, ID_PTT, &h) {
                    errors.push(format!("Push-to-talk: {e}"));
                }
            }
            if let Some(h) = self.toggle.filter(|h| !h.needs_keyboard_hook()) {
                if let Err(e) = register(self.hwnd, ID_TOGGLE, &h) {
                    errors.push(format!("Toggle: {e}"));
                }
            }
        }
        if let Err(e) = self.sync_hook() {
            errors.push(e);
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

    /// Start or stop the low-level keyboard hook. Only modifier-only
    /// shortcuts (RAlt, LAlt) need it: Windows registers them but never
    /// posts `WM_HOTKEY` for them.
    fn sync_hook(&mut self) -> Result<(), String> {
        let keys = HookKeys {
            ptt: self.ptt.filter(Hotkey::needs_keyboard_hook),
            toggle: self.toggle.filter(Hotkey::needs_keyboard_hook),
            cancel: self.cancel.filter(Hotkey::needs_keyboard_hook),
            cancel_active: self.cancel_active,
        };
        let wanted = !self.suspended && (keys.ptt.is_some() || keys.toggle.is_some() || keys.cancel.is_some());
        match (wanted, self.hook.is_some()) {
            (true, false) => match hook_install(self.hwnd) {
                Ok(hook) => self.hook = Some(hook),
                Err(e) => return Err(format!("Modifier-only shortcut: {e}")),
            },
            (false, true) => {
                if let Some(hook) = self.hook.take() {
                    hook_uninstall(hook);
                }
            }
            _ => {}
        }
        HOOK.with(|h| h.borrow_mut().keys = keys);
        Ok(())
    }

    /// Register the cancel shortcut only while dictating, so Escape keeps
    /// working normally in every app the rest of the time.
    pub fn set_cancel_active(&mut self, active: bool) {
        if active == self.cancel_active || self.suspended {
            return;
        }
        match (active, self.cancel) {
            (true, Some(h)) if h.needs_keyboard_hook() => self.cancel_active = true,
            (true, Some(h)) => match register(self.hwnd, ID_CANCEL, &h) {
                Ok(()) => self.cancel_active = true,
                Err(e) => log::warn!("cancel shortcut unavailable: {e}"),
            },
            _ => {
                unregister(self.hwnd, ID_CANCEL);
                self.cancel_active = false;
            }
        }
        HOOK.with(|h| h.borrow_mut().keys.cancel_active = self.cancel_active);
    }

    /// Temporarily release every shortcut (while capturing a new one).
    pub fn suspend(&mut self) {
        self.unregister_all();
        self.suspended = true;
        if let Err(e) = self.sync_hook() {
            log::warn!("{e}");
        }
    }

    pub fn resume(&mut self, cfg: &HotkeyConfig) -> Result<(), String> {
        self.suspended = false;
        self.apply(cfg)
    }

    /// Handle WM_HOTKEY (and [`WM_SV_HOTKEY`] from the keyboard hook).
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
        if let Some(hook) = self.hook.take() {
            hook_uninstall(hook);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::Modifiers;

    #[test]
    fn modifier_matching_is_exact() {
        let ralt = Hotkey::new(Modifiers::default(), vk::RMENU);
        assert!(HeldMods::default().matches(&ralt));
        assert!(!HeldMods { ctrl: true, ..HeldMods::default() }.matches(&ralt));
        assert!(!HeldMods { alt_left: true, ..HeldMods::default() }.matches(&ralt));
        let ctrl_ralt = Hotkey::new(Modifiers { ctrl: true, ..Modifiers::default() }, vk::RMENU);
        assert!(HeldMods { ctrl: true, ..HeldMods::default() }.matches(&ctrl_ralt));
        let alt_ralt = Hotkey::new(Modifiers { alt: true, ..Modifiers::default() }, vk::RMENU);
        assert!(HeldMods { alt_left: true, ..HeldMods::default() }.matches(&alt_ralt));
        assert!(!HeldMods { alt_right: true, ..HeldMods::default() }.matches(&alt_ralt));
    }
}
