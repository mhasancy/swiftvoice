//! Notification-area icon, popup menu and balloon notifications (Win32).

use std::collections::HashMap;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::icon_art::{render_rgba, IconTone};
use super::MenuEntry;
use crate::events::TrayCommand;
use crate::overlay::Tone;

/// Callback message for tray mouse events.
pub const WM_TRAY: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
const FIRST_MENU_ID: usize = 1000;

fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16()) {
        *d = c;
        n += 1;
    }
    let end = n.min(dst.len() - 1);
    dst[end] = 0;
}

/// Build an HICON from the procedural artwork.
fn make_icon(tone: IconTone, size: i32) -> HICON {
    let rgba = render_rgba(size as u32, tone);
    unsafe {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let screen = GetDC(null_mut());
        let mut bits: *mut core::ffi::c_void = null_mut();
        let color = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        ReleaseDC(null_mut(), screen);
        if color.is_null() || bits.is_null() {
            return null_mut();
        }
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, rgba.len());
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
        let mask_bits = vec![0u8; ((size as usize).div_ceil(16) * 2) * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr().cast());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

pub struct Tray {
    hwnd: HWND,
    icons: HashMap<IconTone, HICON>,
    tone: IconTone,
    tooltip: String,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        Tray { hwnd, icons: HashMap::new(), tone: IconTone::Loading, tooltip: "SwiftVoice".into(), added: false }
    }

    fn icon(&mut self, tone: IconTone) -> HICON {
        if let Some(&i) = self.icons.get(&tone) {
            return i;
        }
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64);
        let icon = make_icon(tone, size);
        self.icons.insert(tone, icon);
        icon
    }

    fn data(&mut self) -> NOTIFYICONDATAW {
        let icon = self.icon(self.tone);
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = self.hwnd;
        d.uID = TRAY_ID;
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = icon;
        copy_wide(&mut d.szTip, &self.tooltip);
        d
    }

    /// Add the icon (again after Explorer restarts: "TaskbarCreated").
    pub fn add(&mut self) -> Result<(), String> {
        let d = self.data();
        // SAFETY: fully initialised structure.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &d) } == 0 {
            self.added = false;
            return Err("cannot add the notification-area icon".into());
        }
        self.added = true;
        Ok(())
    }

    pub fn set(&mut self, tone: IconTone, tooltip: &str) {
        self.tone = tone;
        self.tooltip = tooltip.to_string();
        let d = self.data();
        unsafe {
            if !self.added || Shell_NotifyIconW(NIM_MODIFY, &d) == 0 {
                self.added = Shell_NotifyIconW(NIM_ADD, &d) != 0;
            }
        }
    }

    /// Windows 10/11 shows this as a toast from the tray icon.
    pub fn balloon(&mut self, title: &str, body: &str, tone: Tone) {
        let mut d = self.data();
        d.uFlags |= NIF_INFO;
        copy_wide(&mut d.szInfoTitle, title);
        copy_wide(&mut d.szInfo, body);
        d.dwInfoFlags = match tone {
            Tone::Error => NIIF_ERROR,
            Tone::Warning => NIIF_WARNING,
            Tone::Info | Tone::Success => NIIF_INFO | NIIF_NOSOUND,
        } | NIIF_RESPECT_QUIET_TIME;
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            let d = self.data();
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &d);
            }
            self.added = false;
        }
    }
}

/// Show the menu at the cursor and return the chosen command. A free
/// function (not a `Tray` method) so callers never hold application state
/// borrowed while the menu's modal loop dispatches other messages.
// `HWND` is an opaque window handle that is only passed back to Win32 (which
// validates it), never dereferenced here.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn show_menu(hwnd: HWND, entries: &[MenuEntry]) -> Option<TrayCommand> {
    let mut commands: Vec<TrayCommand> = Vec::new();
    unsafe {
        let menu = CreatePopupMenu();
        build(menu, entries, &mut commands);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let id = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_BOTTOMALIGN, pt.x, pt.y, hwnd, null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if id <= 0 {
            return None;
        }
        commands.get(id as usize - FIRST_MENU_ID).cloned()
    }
}

unsafe fn build(menu: HMENU, entries: &[MenuEntry], commands: &mut Vec<TrayCommand>) {
    for e in entries {
        match e {
            MenuEntry::Item { label, cmd, enabled, checked, bold } => {
                let id = FIRST_MENU_ID + commands.len();
                commands.push(cmd.clone());
                let mut flags = MF_STRING;
                if *checked {
                    flags |= MF_CHECKED;
                }
                if !*enabled {
                    flags |= MF_GRAYED;
                }
                let text = crate::platform::wide(label.replace('&', "&&"));
                AppendMenuW(menu, flags, id, text.as_ptr());
                if *bold {
                    SetMenuDefaultItem(menu, id as u32, 0);
                }
            }
            MenuEntry::Label(label) => {
                let text = crate::platform::wide(label.replace('&', "&&"));
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, text.as_ptr());
            }
            MenuEntry::Separator => {
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
            }
            MenuEntry::Submenu { label, items, enabled } => {
                let sub = CreatePopupMenu();
                build(sub, items, commands);
                let text = crate::platform::wide(label.replace('&', "&&"));
                let mut flags = MF_POPUP;
                if !*enabled {
                    flags |= MF_GRAYED;
                }
                AppendMenuW(menu, flags, sub as usize, text.as_ptr());
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        for (_, icon) in self.icons.drain() {
            if !icon.is_null() {
                unsafe {
                    DestroyIcon(icon);
                }
            }
        }
    }
}
