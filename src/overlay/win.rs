//! Win32 overlay window. While you dictate a small bare dot sits in the
//! corner of the monitor you are working on: red while recording (its size
//! follows your voice), amber while the text is recognised, then a short
//! flash in the result's color - green when the text was inserted, amber
//! when it went to the clipboard instead, red for failures.
//!
//! It is created with WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW
//! and shown with SW_SHOWNOACTIVATE, so it never takes focus, never receives
//! clicks and never appears in the taskbar or Alt+Tab. The dot window drops
//! its whole background out through the layered color key, leaving only the
//! dot itself visible.

use std::cell::RefCell;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::OverlayView;
use crate::config::OverlayPosition;
use crate::platform::wide;

pub const TIMER_OVERLAY_HIDE: usize = 0x5320;
const CLASS: &str = "SwiftVoiceOverlay";
/// Layered color key of the dot window: pixels painted in this color become
/// fully transparent. Only the dot itself is ever painted.
const TRANSPARENT_KEY: u32 = 0;

struct State {
    view: OverlayView,
    level: f32,
    scale: f32,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State { view: OverlayView::Hidden, level: 0.0, scale: 1.0 }) };
}

pub struct Overlay {
    hwnd: HWND,
    position: OverlayPosition,
    opacity: u8,
    visible: bool,
    size: (i32, i32),
}

/// Layered-window transparency: the background is keyed out, so only the dot
/// is drawn; `opacity` is its uniform alpha.
unsafe fn apply_style(hwnd: HWND, opacity: u8) {
    SetLayeredWindowAttributes(hwnd, TRANSPARENT_KEY, opacity, LWA_ALPHA | LWA_COLORKEY);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_TIMER if wparam == TIMER_OVERLAY_HIDE => {
            KillTimer(hwnd, TIMER_OVERLAY_HIDE);
            ShowWindow(hwnd, SW_HIDE);
            STATE.with(|s| s.borrow_mut().view = OverlayView::Hidden);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

impl Overlay {
    pub fn create() -> Result<Overlay, String> {
        unsafe {
            let hinstance = GetModuleHandleW(null());
            let class = wide(CLASS);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                class.as_ptr(),
                wide("SwiftVoice").as_ptr(),
                WS_POPUP,
                0,
                0,
                400,
                60,
                null_mut(),
                null_mut(),
                hinstance,
                null(),
            );
            if hwnd.is_null() {
                return Err(format!("cannot create the overlay window (error {})", windows_sys::Win32::Foundation::GetLastError()));
            }
            Ok(Overlay { hwnd, position: OverlayPosition::Bottom, opacity: 240, visible: false, size: (0, 0) })
        }
    }

    pub fn configure(&mut self, position: OverlayPosition, opacity: f32) {
        self.position = position;
        self.opacity = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
        unsafe { apply_style(self.hwnd, self.opacity) };
    }

    /// Show `view` (or hide it); `hide_after_ms` schedules an automatic hide.
    pub fn show(&mut self, view: OverlayView, hide_after_ms: Option<u32>) {
        unsafe {
            KillTimer(self.hwnd, TIMER_OVERLAY_HIDE);
            if view.is_hidden() {
                ShowWindow(self.hwnd, SW_HIDE);
                self.visible = false;
                STATE.with(|s| s.borrow_mut().view = OverlayView::Hidden);
                return;
            }
            // Level updates (-1 = keep the previous level) only repaint.
            let level = match &view {
                OverlayView::Listening { level, .. } => *level,
                _ => 0.0,
            };
            let same_kind = STATE.with(|s| std::mem::discriminant(&s.borrow().view) == std::mem::discriminant(&view));
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if let (OverlayView::Listening { hint: Some(h), .. }, OverlayView::Listening { hint: None, text, level: l }) =
                    (&s.view, &view)
                {
                    // Keep a hint (e.g. "loading the model") until text arrives.
                    let hint = if text.is_empty() { Some(h.clone()) } else { None };
                    s.view = OverlayView::Listening { level: *l, text: text.clone(), hint };
                } else {
                    s.view = view;
                }
                if level >= 0.0 {
                    s.level = level;
                }
            });
            if !self.visible || !same_kind {
                self.place();
            }
            InvalidateRect(self.hwnd, null(), 0);
            if let Some(ms) = hide_after_ms {
                SetTimer(self.hwnd, TIMER_OVERLAY_HIDE, ms.max(1), None);
            }
        }
    }

    /// Place the dot at its corner of the monitor that holds the foreground
    /// window: bottom or bottom-left put it above the taskbar on the left,
    /// top or top-left under the top edge.
    unsafe fn place(&mut self) {
        let fg = GetForegroundWindow();
        let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 {
            dx = 96;
        }
        let scale = dx as f32 / 96.0;
        STATE.with(|s| s.borrow_mut().scale = scale);
        let work = mi.rcWork;
        let side = (44.0 * scale) as i32;
        let margin = (20.0 * scale) as i32;
        let x = work.left + margin;
        let y = match self.position {
            OverlayPosition::Bottom | OverlayPosition::BottomLeft => work.bottom - side - margin,
            OverlayPosition::Top | OverlayPosition::TopLeft => work.top + margin,
        };
        if self.size != (side, side) {
            SetWindowRgn(self.hwnd, CreateRectRgn(0, 0, side + 1, side + 1), 0);
            self.size = (side, side);
        }
        apply_style(self.hwnd, self.opacity);
        SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, side, side, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        self.visible = true;
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    // Double buffer to avoid flicker while the level meter animates.
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem, bmp);
    let (view, level, scale) = STATE.with(|s| {
        let s = s.borrow();
        (s.view.clone(), s.level, s.scale)
    });
    let px = |v: f32| (v * scale) as i32;
    // The background is keyed out, so only the dot remains; while listening
    // its size follows the input level.
    let bg = CreateSolidBrush(TRANSPARENT_KEY);
    FillRect(mem, &rc, bg);
    DeleteObject(bg);
    let r = px(7.0 + 6.0 * level.clamp(0.0, 1.0));
    let (cx, cy) = (w / 2, h / 2);
    let dot = CreateSolidBrush(view.dot_color());
    let old_brush = SelectObject(mem, dot);
    let old_pen = SelectObject(mem, GetStockObject(NULL_PEN));
    Ellipse(mem, cx - r, cy - r, cx + r + 1, cy + r + 1);
    SelectObject(mem, old_pen);
    SelectObject(mem, old_brush);
    DeleteObject(dot);
    BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_bmp);
    DeleteObject(bmp);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}
