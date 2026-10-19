//! "Press the new shortcut" dialog (Windows).
//!
//! A small modal window that records the next key combination. All global
//! shortcuts are suspended while it is open so the old ones can be pressed
//! and re-assigned. Escape (with no modifier) cancels.

use std::cell::RefCell;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::SS_NOPREFIX;
use windows_sys::Win32::UI::Accessibility::NotifyWinEvent;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{is_modifier_vk, vk, Hotkey, Modifiers};
use crate::platform::wide;

const CLASS: &str = "SwiftVoiceHotkeyCapture";
const CURRENT: i32 = 3;
const ERROR_TEXT: i32 = 4;

struct State {
    current: String,
    error: String,
    done: Option<Option<Hotkey>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn held(code: u16) -> bool {
    (unsafe { GetKeyState(code as i32) } as u16 & 0x8000) != 0
}

fn current_mods() -> Modifiers {
    Modifiers { ctrl: held(vk::CONTROL), alt: held(vk::MENU), shift: held(vk::SHIFT), win: held(vk::LWIN) || held(vk::RWIN) }
}

fn mods_text(m: Modifiers) -> String {
    let mut parts = Vec::new();
    if m.ctrl {
        parts.push("Ctrl");
    }
    if m.alt {
        parts.push("Alt");
    }
    if m.shift {
        parts.push("Shift");
    }
    if m.win {
        parts.push("Win");
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}+…", parts.join("+"))
    }
}

fn update<F: FnOnce(&mut State)>(hwnd: HWND, f: F) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            f(st);
        }
    });
    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            unsafe {
                let current = GetDlgItem(hwnd, CURRENT);
                let error = GetDlgItem(hwnd, ERROR_TEXT);
                SetWindowTextW(current, wide(format!("Shortcut: {}", st.current)).as_ptr());
                SetWindowTextW(error, wide(&st.error).as_ptr());
                NotifyWinEvent(EVENT_OBJECT_NAMECHANGE, current, OBJID_CLIENT, 0);
                if !st.error.is_empty() {
                    NotifyWinEvent(EVENT_SYSTEM_ALERT, error, OBJID_CLIENT, 0);
                }
            }
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let code = wparam as u16;
            let mods = current_mods();
            if code == vk::ESCAPE && !mods.any() {
                update(hwnd, |s| s.done = Some(None));
            } else if is_modifier_vk(code) {
                update(hwnd, |s| {
                    s.current = mods_text(mods);
                    s.error.clear();
                });
            } else {
                let hk = Hotkey::new(mods, code);
                match hk.check_global(false) {
                    Ok(()) => update(hwnd, |s| s.done = Some(Some(hk))),
                    Err(e) => update(hwnd, |s| {
                        s.current = hk.to_string();
                        s.error = e;
                    }),
                }
            }
            0
        }
        WM_KEYUP | WM_SYSKEYUP => {
            let code = wparam as u16;
            let right_alt = code == vk::MENU && lparam & (1 << 24) != 0;
            let mods = current_mods();
            if right_alt && !mods.ctrl && !mods.shift && !mods.win && !held(vk::LMENU) {
                // Right Alt released on its own: it is a valid shortcut.
                update(hwnd, |s| {
                    if s.error.is_empty() {
                        s.done = Some(Some(Hotkey::new(Modifiers::default(), vk::RMENU)));
                    }
                });
            } else {
                update(hwnd, |s| {
                    if s.error.is_empty() {
                        s.current = mods_text(mods);
                    }
                });
            }
            0
        }
        // Swallow characters (no beeps) and keep Alt from opening a menu.
        WM_CHAR | WM_SYSCHAR | WM_SYSCOMMAND if msg != WM_SYSCOMMAND || (wparam & 0xFFF0) == SC_KEYMENU as usize => 0,
        WM_CLOSE => {
            update(hwnd, |s| s.done = Some(None));
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn create_labels(hwnd: HWND, what: &str, scale: f32) -> Option<[HFONT; 2]> {
    let px = |v: f32| (v * scale) as i32;
    let font = |h: f32, weight: u32| {
        CreateFontW(
            -px(h),
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (DEFAULT_PITCH as u32) | (FF_SWISS as u32),
            wide("Segoe UI").as_ptr(),
        )
    };
    let normal = font(14.0, FW_NORMAL);
    let big = font(22.0, FW_SEMIBOLD);
    let labels = [
        (1, format!("Press the new shortcut for {what}."), 16.0, 22.0, normal),
        (2, "Hold Ctrl/Alt/Shift/Win + a key, or RAlt alone. Esc cancels.".into(), 38.0, 30.0, normal),
        (CURRENT, "Shortcut: waiting for keys".into(), 74.0, 34.0, big),
        (ERROR_TEXT, String::new(), 114.0, 44.0, normal),
    ];
    for (id, text, top, height, font) in labels {
        let label = CreateWindowExW(
            0,
            wide("STATIC").as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_NOPREFIX,
            px(20.0),
            px(top),
            px(440.0),
            px(height),
            hwnd,
            id as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        if label.is_null() {
            DeleteObject(normal);
            DeleteObject(big);
            return None;
        }
        SendMessageW(label, WM_SETFONT, font as WPARAM, 1);
    }
    Some([normal, big])
}

/// Show the dialog and return the chosen shortcut (`None` = cancelled).
// `HWND` is an opaque window handle that is only passed back to Win32 (which
// validates it), never dereferenced here.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn capture_hotkey(owner: HWND, what: &str) -> Option<Hotkey> {
    unsafe {
        let hinstance = GetModuleHandleW(null());
        let class = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        let monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 {
            dx = 96;
        }
        let scale = dx as f32 / 96.0;
        STATE.with(|s| *s.borrow_mut() = Some(State { current: String::new(), error: String::new(), done: None }));
        let (w, h) = ((480.0 * scale) as i32, (210.0 * scale) as i32);
        let work = mi.rcWork;
        let x = work.left + ((work.right - work.left) - w) / 2;
        let y = work.top + ((work.bottom - work.top) - h) / 3;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_DLGMODALFRAME,
            class.as_ptr(),
            wide("SwiftVoice: change shortcut").as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            w,
            h,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        );
        if hwnd.is_null() {
            STATE.with(|s| s.borrow_mut().take());
            return None;
        }
        let Some(fonts) = create_labels(hwnd, what, scale) else {
            DestroyWindow(hwnd);
            STATE.with(|s| s.borrow_mut().take());
            log::error!("cannot create shortcut dialog labels");
            return None;
        };
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let done = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.done));
            if done.is_some() {
                break;
            }
            let r = GetMessageW(&mut msg, null_mut(), 0, 0);
            if r == 0 {
                // WM_QUIT: hand it back to the main loop.
                PostQuitMessage(msg.wParam as i32);
                break;
            }
            if r == -1 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        DestroyWindow(hwnd);
        for font in fonts {
            DeleteObject(font);
        }
        STATE.with(|s| s.borrow_mut().take()).and_then(|st| st.done).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

    #[test]
    fn shortcut_instructions_are_exposed_to_ui_automation() {
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let hwnd = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("test").as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                480,
                210,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!hwnd.is_null());
            let fonts = create_labels(hwnd, "push-to-talk", 1.0).unwrap();
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER).unwrap();
            let instruction = uia.ElementFromHandle(windows::Win32::Foundation::HWND(GetDlgItem(hwnd, 1))).unwrap();
            assert_eq!(instruction.CurrentName().unwrap().to_string(), "Press the new shortcut for push-to-talk.");
            STATE.with(|s| *s.borrow_mut() = Some(State { current: String::new(), error: String::new(), done: None }));
            update(hwnd, |s| s.error = "Choose a modifier with this key".into());
            let error = uia.ElementFromHandle(windows::Win32::Foundation::HWND(GetDlgItem(hwnd, ERROR_TEXT))).unwrap();
            assert_eq!(error.CurrentName().unwrap().to_string(), "Choose a modifier with this key");
            STATE.with(|s| s.borrow_mut().take());
            DestroyWindow(hwnd);
            for font in fonts {
                DeleteObject(font);
            }
            drop(error);
            drop(instruction);
            drop(uia);
            if initialized {
                CoUninitialize();
            }
        }
    }
}
