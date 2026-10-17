//! Windows implementations of [`Desktop`] and [`ClipboardBackend`].

use std::ptr::null_mut;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, GlobalFree, ERROR_ACCESS_DENIED, HANDLE, HWND};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::{
    CF_BITMAP, CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT, CF_ENHMETAFILE, CF_GDIOBJFIRST, CF_GDIOBJLAST, CF_METAFILEPICT,
    CF_OWNERDISPLAY, CF_PALETTE, CF_PRIVATEFIRST, CF_PRIVATELAST, CF_UNICODETEXT,
};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, VK_RETURN, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, IsIconic, IsWindow,
    SetForegroundWindow, ShowWindow, GUITHREADINFO, SW_RESTORE,
};

use super::clipboard::{ClipboardBackend, ClipboardSnapshot, SnapshotError};
use super::{Desktop, WindowHandle, WindowInfo};
use crate::hotkey::{vk, Hotkey};
use crate::platform::wide;

/// Marks SwiftVoice's synthetic key events (`dwExtraInfo`).
const SWIFTVOICE_TAG: usize = 0x5357_5643;
/// An unassigned virtual key; tapping it while Alt or Win is held keeps
/// Windows from opening a menu or Start when they are released.
const VK_MASK: u16 = 0xE8;

fn key(vk: u16, flags: u32) -> INPUT {
    // SAFETY: plain Win32 call.
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    let extended = matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0x5D | 0x6F | 0x90 | vk::RCONTROL | vk::RMENU);
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 },
                time: 0,
                dwExtraInfo: SWIFTVOICE_TAG,
            },
        },
    }
}

fn unicode(unit: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: SWIFTVOICE_TAG,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: slice of initialised INPUT structures.
    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!(
            "Windows accepted {sent} of {} key events (error {}); input may be blocked by another program or a secure desktop",
            inputs.len(),
            unsafe { GetLastError() }
        );
    }
    Ok(())
}

fn is_down(code: u16) -> bool {
    (unsafe { GetAsyncKeyState(code as i32) } as u16 & 0x8000) != 0
}

const MODIFIERS: [u16; 8] = [vk::LCONTROL, vk::RCONTROL, vk::LMENU, vk::RMENU, vk::LSHIFT, vk::RSHIFT, vk::LWIN, vk::RWIN];

pub struct WinDesktop {
    own_pid: u32,
    elevated: bool,
}

impl Default for WinDesktop {
    fn default() -> Self {
        WinDesktop { own_pid: unsafe { GetCurrentProcessId() }, elevated: crate::platform::is_elevated() }
    }
}

fn process_name(pid: u32) -> (String, bool) {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            // Access denied: almost always an elevated or protected process.
            return (format!("process {pid}"), GetLastError() == ERROR_ACCESS_DENIED);
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let name = if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0 {
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            full.rsplit(['\\', '/']).next().unwrap_or(&full).to_lowercase()
        } else {
            format!("process {pid}")
        };
        let mut token: HANDLE = null_mut();
        let elevated = if OpenProcessToken(h, TOKEN_QUERY, &mut token) != 0 {
            let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut ret = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                (&mut e as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            );
            CloseHandle(token);
            ok != 0 && e.TokenIsElevated != 0
        } else {
            GetLastError() == ERROR_ACCESS_DENIED
        };
        CloseHandle(h);
        (name, elevated)
    }
}

/// Class name of a window ("" when it no longer exists).
unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

impl Desktop for WinDesktop {
    fn foreground(&self) -> Option<WindowInfo> {
        unsafe {
            let hwnd: HWND = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == self.own_pid {
                return Some(WindowInfo { handle: hwnd as isize, process: "swiftvoice.exe".into(), own: true, elevated: self.elevated });
            }
            let (process, elevated) = process_name(pid);
            Some(WindowInfo { handle: hwnd as isize, process, own: false, elevated })
        }
    }

    fn focused_editable(&self) -> Option<bool> {
        // SAFETY: plain Win32 queries; a dead window yields an empty class name.
        unsafe {
            let fg: HWND = GetForegroundWindow();
            if fg.is_null() {
                return None; // covered by the no-focus fallback in `Inserter::insert`
            }
            // The desktop, the taskbar and their icon lists never hold a text
            // field; anything typed there can only be lost.
            if matches!(class_name(fg).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
                return Some(false);
            }
            let mut pid = 0u32;
            let thread = GetWindowThreadProcessId(fg, &mut pid);
            let mut gti: GUITHREADINFO = std::mem::zeroed();
            gti.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            if GetGUIThreadInfo(thread, &mut gti) == 0 {
                return None;
            }
            // No control holds the keyboard focus, or an icon/file list does:
            // nothing in that window can receive text.
            if gti.hwndFocus.is_null() || class_name(gti.hwndFocus) == "SysListView32" {
                return Some(false);
            }
            None
        }
    }

    fn wait_modifiers_released(&self, max: Duration) -> bool {
        let start = Instant::now();
        loop {
            if !MODIFIERS.iter().any(|&m| is_down(m)) {
                return true;
            }
            if start.elapsed() >= max {
                return false;
            }
            sleep(Duration::from_millis(5));
        }
    }

    fn release_modifiers(&self) {
        let held: Vec<u16> = MODIFIERS.iter().copied().filter(|&m| is_down(m)).collect();
        if held.is_empty() {
            return;
        }
        let mut inputs = Vec::new();
        if held.iter().any(|m| matches!(*m, vk::LMENU | vk::RMENU | vk::LWIN | vk::RWIN)) {
            inputs.push(key(VK_MASK, 0));
            inputs.push(key(VK_MASK, KEYEVENTF_KEYUP));
        }
        for m in held {
            inputs.push(key(m, KEYEVENTF_KEYUP));
        }
        if let Err(e) = send(&inputs) {
            log::warn!("releasing modifiers: {e:#}");
        }
    }

    fn focus(&self, window: WindowHandle) -> bool {
        let hwnd = window as HWND;
        unsafe {
            if IsWindow(hwnd) == 0 {
                return false;
            }
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            if SetForegroundWindow(hwnd) == 0 {
                // Borrow the foreground thread's input state, then retry.
                let fg = GetForegroundWindow();
                let fg_thread = GetWindowThreadProcessId(fg, null_mut());
                let me = GetCurrentThreadId();
                if fg_thread != 0 && fg_thread != me {
                    AttachThreadInput(me, fg_thread, 1);
                    SetForegroundWindow(hwnd);
                    BringWindowToTop(hwnd);
                    AttachThreadInput(me, fg_thread, 0);
                }
            }
            for _ in 0..10 {
                if GetForegroundWindow() == hwnd {
                    return true;
                }
                sleep(Duration::from_millis(10));
            }
            false
        }
    }

    fn send_shortcut(&self, shortcut: &Hotkey) -> Result<()> {
        let m = shortcut.mods;
        let mods: Vec<u16> = [(m.ctrl, vk::LCONTROL), (m.alt, vk::LMENU), (m.shift, vk::LSHIFT), (m.win, vk::LWIN)]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, k)| k)
            .collect();
        let mut inputs = Vec::with_capacity(mods.len() * 2 + 2);
        for &k in &mods {
            inputs.push(key(k, 0));
        }
        inputs.push(key(shortcut.vk, 0));
        inputs.push(key(shortcut.vk, KEYEVENTF_KEYUP));
        if m.alt || m.win {
            inputs.push(key(VK_MASK, 0));
            inputs.push(key(VK_MASK, KEYEVENTF_KEYUP));
        }
        for &k in mods.iter().rev() {
            inputs.push(key(k, KEYEVENTF_KEYUP));
        }
        send(&inputs)
    }

    fn type_text(&self, text: &str) -> Result<()> {
        let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
        for c in text.chars() {
            match c {
                '\r' => {}
                '\n' => {
                    inputs.push(key(VK_RETURN, 0));
                    inputs.push(key(VK_RETURN, KEYEVENTF_KEYUP));
                }
                '\t' => {
                    inputs.push(key(VK_TAB, 0));
                    inputs.push(key(VK_TAB, KEYEVENTF_KEYUP));
                }
                _ => {
                    let mut buf = [0u16; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        inputs.push(unicode(*unit, false));
                        inputs.push(unicode(*unit, true));
                    }
                }
            }
        }
        // Small batches with a short pause so slow apps keep up.
        for chunk in inputs.chunks(64) {
            send(chunk)?;
            sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    fn self_elevated(&self) -> bool {
        self.elevated
    }
}

/// Clipboard access through the Win32 API.
pub struct WinClipboard {
    exclude_monitor: u32,
    can_history: u32,
    can_cloud: u32,
}

impl Default for WinClipboard {
    fn default() -> Self {
        let reg = |name: &str| unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) };
        WinClipboard {
            exclude_monitor: reg("ExcludeClipboardContentFromMonitorProcessing"),
            can_history: reg("CanIncludeInClipboardHistory"),
            can_cloud: reg("CanUploadToCloudClipboard"),
        }
    }
}

struct Open;

impl Open {
    /// Open the clipboard, retrying while another program holds it.
    fn new() -> std::result::Result<Open, SnapshotError> {
        for _ in 0..25 {
            // SAFETY: NULL owner = current task (as arboard/clipboard-win do).
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return Ok(Open);
            }
            sleep(Duration::from_millis(10));
        }
        Err(SnapshotError::Busy)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

/// Formats whose handle is not an HGLOBAL (GDI objects, owner display).
fn skip_format(f: u32) -> bool {
    let f16 = f as u16;
    f <= u16::MAX as u32
        && (matches!(
            f16,
            CF_BITMAP
                | CF_METAFILEPICT
                | CF_PALETTE
                | CF_ENHMETAFILE
                | CF_OWNERDISPLAY
                | CF_DSPBITMAP
                | CF_DSPMETAFILEPICT
                | CF_DSPENHMETAFILE
        ) || (CF_PRIVATEFIRST..=CF_PRIVATELAST).contains(&f16)
            || (CF_GDIOBJFIRST..=CF_GDIOBJLAST).contains(&f16))
}

unsafe fn set_bytes(format: u32, data: &[u8]) -> bool {
    let h = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1));
    if h.is_null() {
        return false;
    }
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        GlobalFree(h);
        return false;
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
    GlobalUnlock(h);
    if SetClipboardData(format, h).is_null() {
        GlobalFree(h);
        return false;
    }
    true
}

impl WinClipboard {
    unsafe fn mark(&self, transient: bool) {
        // Keep dictated text out of cloud sync always, and out of clipboard
        // history / clipboard managers when it is only there for a paste.
        set_bytes(self.can_cloud, &0u32.to_le_bytes());
        if transient {
            set_bytes(self.can_history, &0u32.to_le_bytes());
            set_bytes(self.exclude_monitor, &[0u8]);
        }
    }
}

impl ClipboardBackend for WinClipboard {
    fn sequence(&self) -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    fn snapshot(&mut self, max_bytes: usize) -> std::result::Result<ClipboardSnapshot, SnapshotError> {
        let _open = Open::new()?;
        let mut snap = ClipboardSnapshot::default();
        let mut total = 0usize;
        let mut format = 0u32;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if skip_format(format) {
                continue;
            }
            unsafe {
                let h = GetClipboardData(format);
                if h.is_null() {
                    continue;
                }
                let size = GlobalSize(h);
                if size == 0 {
                    continue;
                }
                total += size;
                if total > max_bytes {
                    return Err(SnapshotError::TooLarge(total));
                }
                let p = GlobalLock(h) as *const u8;
                if p.is_null() {
                    continue;
                }
                let data = std::slice::from_raw_parts(p, size).to_vec();
                GlobalUnlock(h);
                snap.formats.push((format, data));
            }
        }
        Ok(snap)
    }

    fn set_text(&mut self, text: &str, transient: bool) -> Result<u32> {
        let _open = Open::new().map_err(|e| anyhow::anyhow!("clipboard unavailable: {e:?}"))?;
        unsafe {
            if EmptyClipboard() == 0 {
                bail!("cannot empty the clipboard (error {})", GetLastError());
            }
            let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(utf16.as_ptr() as *const u8, utf16.len() * 2);
            if !set_bytes(CF_UNICODETEXT as u32, bytes) {
                bail!("cannot place text on the clipboard (error {})", GetLastError());
            }
            self.mark(transient);
        }
        drop(_open);
        Ok(self.sequence())
    }

    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()> {
        let _open = Open::new().map_err(|e| anyhow::anyhow!("clipboard unavailable: {e:?}"))?;
        unsafe {
            if EmptyClipboard() == 0 {
                bail!("cannot empty the clipboard (error {})", GetLastError());
            }
            let mut failed = 0;
            for (format, data) in &snapshot.formats {
                if !set_bytes(*format, data) {
                    failed += 1;
                }
            }
            if !snapshot.formats.is_empty() {
                // The original content is already in history; do not add a
                // duplicate entry for the restore.
                self.mark(true);
            }
            if failed > 0 {
                log::warn!("{failed} clipboard format(s) could not be restored");
            }
        }
        Ok(())
    }
}
