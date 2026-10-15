//! Windows services: console attachment, message boxes, autostart,
//! elevation, single instance, shell helpers.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, MAX_PATH};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_DISK, FILE_TYPE_PIPE};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{
    SHBrowseForFolderW, SHGetPathFromIDListW, ShellExecuteW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, SW_SHOWNORMAL,
};

/// NUL-terminated UTF-16 for Win32 calls.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Make println!/eprintln! work when the GUI-subsystem executable is run
/// from a terminal (directly, or through the `swiftvoice.com` launcher).
/// Output that is already redirected to a pipe or file (scripts, CI) is
/// left alone.
pub fn attach_parent_console() {
    // SAFETY: plain Win32 calls on the process's own standard handles;
    // failure just means there is no console.
    unsafe {
        let redirected = |which| {
            let h = GetStdHandle(which);
            !h.is_null() && matches!(GetFileType(h), FILE_TYPE_PIPE | FILE_TYPE_DISK)
        };
        if redirected(STD_OUTPUT_HANDLE) && redirected(STD_ERROR_HANDLE) {
            return;
        }
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn message_box(hwnd: HWND, title: &str, text: &str, style: u32) -> i32 {
    let t = wide(title);
    let b = wide(text);
    // SAFETY: valid NUL-terminated strings.
    unsafe { MessageBoxW(hwnd, b.as_ptr(), t.as_ptr(), style | MB_TOPMOST | MB_SETFOREGROUND) }
}

pub fn error_box(title: &str, text: &str) {
    message_box(null_mut(), title, text, MB_OK | MB_ICONERROR);
}

pub fn info_box(hwnd: HWND, title: &str, text: &str) {
    message_box(hwnd, title, text, MB_OK | MB_ICONINFORMATION);
}

pub fn yes_no(hwnd: HWND, title: &str, text: &str) -> bool {
    message_box(hwnd, title, text, MB_YESNO | MB_ICONQUESTION) == IDYES
}

/// Open a file or folder with its default application.
pub fn open_path(path: &Path) -> Result<(), String> {
    let verb = wide("open");
    let file = wide(path.as_os_str());
    // SAFETY: valid strings; result > 32 means success.
    let r = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), file.as_ptr(), null(), null(), SW_SHOWNORMAL) } as isize;
    if r > 32 {
        Ok(())
    } else {
        Err(format!("Windows could not open {} (error {r})", path.display()))
    }
}

/// Classic folder picker (works on every Windows 10/11 setup).
pub fn pick_folder(hwnd: HWND, title: &str) -> Option<PathBuf> {
    let title_w = wide(title);
    let mut display = vec![0u16; MAX_PATH as usize];
    // SAFETY: zeroed BROWSEINFOW is a valid "no options" value; we fill the
    // fields the API reads.
    let mut bi: BROWSEINFOW = unsafe { std::mem::zeroed() };
    bi.hwndOwner = hwnd;
    bi.pszDisplayName = display.as_mut_ptr();
    bi.lpszTitle = title_w.as_ptr();
    bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
    unsafe {
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let mut path = vec![0u16; 32_768];
        let ok = SHGetPathFromIDListW(pidl, path.as_mut_ptr());
        CoTaskMemFree(pidl as *const _);
        if ok == 0 {
            return None;
        }
        let len = path.iter().position(|&c| c == 0).unwrap_or(0);
        Some(PathBuf::from(String::from_utf16_lossy(&path[..len])))
    }
}

/// True when this process runs elevated (as administrator).
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "SwiftVoice";

fn autostart_command() -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("swiftvoice.exe"));
    format!("\"{}\"", exe.display())
}

/// Whether HKCU\...\Run points at this executable.
pub fn autostart_enabled() -> bool {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let mut buf = vec![0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buffer and size describe the same allocation.
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, null_mut(), buf.as_mut_ptr().cast(), &mut size)
    };
    if r != 0 {
        return false;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    String::from_utf16_lossy(&buf[..len]).eq_ignore_ascii_case(&autostart_command())
}

/// Add or remove the per-user "run at sign-in" entry (no admin rights).
pub fn set_autostart(enable: bool) -> Result<(), String> {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    unsafe {
        let mut hkey: HKEY = null_mut();
        let r = RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_SET_VALUE | KEY_QUERY_VALUE, &mut hkey);
        if r != 0 {
            return Err(format!("cannot open the Run registry key (error {r})"));
        }
        let r = if enable {
            let value = wide(autostart_command());
            RegSetValueExW(hkey, name.as_ptr(), 0, REG_SZ, value.as_ptr().cast(), (value.len() * 2) as u32)
        } else {
            match RegDeleteValueW(hkey, name.as_ptr()) {
                2 => 0, // ERROR_FILE_NOT_FOUND: already off
                other => other,
            }
        };
        RegCloseKey(hkey);
        if r != 0 {
            return Err(format!("cannot update the Run registry value (error {r})"));
        }
    }
    Ok(())
}

/// CPU brand string from the registry, for benchmark reports.
pub fn cpu_name() -> Option<String> {
    let key = wide(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0");
    let name = wide("ProcessorNameString");
    let mut buf = vec![0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, null_mut(), buf.as_mut_ptr().cast(), &mut size)
    };
    if r != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..len]).trim().to_string())
}

/// Held for the life of the process; a second copy of SwiftVoice exits.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// `Ok(None)` when another instance already runs in this session.
    pub fn acquire() -> Result<Option<SingleInstance>, String> {
        let name = wide("Local\\SwiftVoice.SingleInstance");
        unsafe {
            let h = CreateMutexW(null(), 0, name.as_ptr());
            if h.is_null() {
                return Err(format!("cannot create the single-instance mutex (error {})", GetLastError()));
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(h);
                return Ok(None);
            }
            Ok(Some(SingleInstance(h)))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
