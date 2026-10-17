//! Win32 overlay window. While you dictate it is a small, translucent,
//! rounded status pill near the bottom (or top) of the monitor you are
//! working on: a red level dot with the "Listening" title and the live
//! transcript. A successful insertion flashes a bare green dot at the
//! middle-left; other results and messages show as the same status pill.
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

use super::{OverlayView, Tone};
use crate::config::OverlayPosition;
use crate::platform::wide;

pub const TIMER_OVERLAY_HIDE: usize = 0x5320;
const CLASS: &str = "SwiftVoiceOverlay";
/// Layered color key of the dot window: pixels painted in this color become
/// fully transparent. Only applied while the dot indicator is shown.
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

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// Whether the current view is drawn as the bare dot indicator.
fn dot_mode() -> bool {
    STATE.with(|s| s.borrow().view.is_dot())
}

/// Layered-window transparency: uniform alpha for the card; for the dot the
/// background is keyed out, so only the dot remains visible.
unsafe fn apply_style(hwnd: HWND, opacity: u8, dot: bool) {
    if dot {
        SetLayeredWindowAttributes(hwnd, TRANSPARENT_KEY, opacity, LWA_ALPHA | LWA_COLORKEY);
    } else {
        SetLayeredWindowAttributes(hwnd, 0, opacity, LWA_ALPHA);
    }
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
        self.opacity = (opacity.clamp(0.3, 1.0) * 255.0) as u8;
        unsafe { apply_style(self.hwnd, self.opacity, dot_mode()) };
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

    /// Position on the monitor that holds the foreground window: the status
    /// card near the bottom (or top) while dictating and for messages, the
    /// bare insert flash at the middle-left.
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
        let dot = dot_mode();
        let side = (44.0 * scale) as i32;
        let w = if dot { side } else { (460.0 * scale) as i32 };
        let h = if dot { side } else { (60.0 * scale) as i32 };
        let (x, y) = if dot {
            let margin = (20.0 * scale) as i32;
            (work.left + margin, work.top + ((work.bottom - work.top) - h) / 2)
        } else {
            let x = work.left + ((work.right - work.left) - w) / 2;
            let margin = (56.0 * scale) as i32;
            let y = match self.position {
                OverlayPosition::Bottom => work.bottom - h - margin,
                OverlayPosition::Top => work.top + margin,
            };
            (x, y)
        };
        if self.size != (w, h) {
            let rgn = if dot {
                CreateRectRgn(0, 0, w + 1, h + 1)
            } else {
                let radius = (22.0 * scale) as i32;
                CreateRoundRectRgn(0, 0, w + 1, h + 1, radius, radius)
            };
            SetWindowRgn(self.hwnd, rgn, 0);
            self.size = (w, h);
        }
        apply_style(self.hwnd, self.opacity, dot);
        SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
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

struct Look {
    dot: u32,
    title: String,
    body: String,
}

fn look(view: &OverlayView) -> Look {
    let tone_color = |t: Tone| match t {
        Tone::Info => rgb(0x4C, 0x9A, 0xFF),
        Tone::Success => rgb(0x3D, 0xD6, 0x8C),
        Tone::Warning => rgb(0xFF, 0xB0, 0x20),
        Tone::Error => rgb(0xFF, 0x5A, 0x52),
    };
    match view {
        OverlayView::Hidden => Look { dot: 0, title: String::new(), body: String::new() },
        OverlayView::Listening { text, hint, .. } => Look {
            dot: rgb(0xFF, 0x45, 0x3A),
            title: "Listening".into(),
            body: if text.is_empty() { hint.clone().unwrap_or_else(|| "Speak now…".into()) } else { text.clone() },
        },
        OverlayView::Processing { text } => Look { dot: rgb(0xFF, 0xB0, 0x20), title: "Transcribing…".into(), body: text.clone() },
        OverlayView::Result { text, tone, detail } => {
            Look { dot: tone_color(*tone), title: text.clone(), body: detail.clone().unwrap_or_default() }
        }
        OverlayView::Message { text, tone } => Look { dot: tone_color(*tone), title: text.clone(), body: String::new() },
    }
}

unsafe fn font(height: i32, weight: u32) -> HFONT {
    CreateFontW(
        -height,
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
}

unsafe fn draw_text(hdc: HDC, text: &str, rect: &mut RECT) {
    if text.is_empty() {
        return;
    }
    let w: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(hdc, w.as_ptr(), w.len() as i32, rect, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS | DT_NOPREFIX);
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
    if view.is_dot() {
        // Dot indicator: the background is keyed out, only the dot remains.
        let bg = CreateSolidBrush(TRANSPARENT_KEY);
        FillRect(mem, &rc, bg);
        DeleteObject(bg);
        let r = px(7.0 + 6.0 * level.clamp(0.0, 1.0));
        let (cx, cy) = (w / 2, h / 2);
        // Insertion confirmation: the dot flashes green, then the hide
        // timer makes it disappear.
        let color =
            if matches!(&view, OverlayView::Result { tone: Tone::Success, .. }) { rgb(0x3D, 0xD6, 0x8C) } else { rgb(0xFF, 0x45, 0x3A) };
        let dot = CreateSolidBrush(color);
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
        return;
    }
    let bg = CreateSolidBrush(rgb(0x1E, 0x20, 0x24));
    FillRect(mem, &rc, bg);
    DeleteObject(bg);
    let l = look(&view);
    // Status dot; while listening its size follows the input level.
    let r = if matches!(view, OverlayView::Listening { .. }) { px(6.0 + 6.0 * level.clamp(0.0, 1.0)) } else { px(7.0) };
    let cx = px(30.0);
    let cy = h / 2;
    let dot = CreateSolidBrush(l.dot);
    let old_brush = SelectObject(mem, dot);
    let old_pen = SelectObject(mem, GetStockObject(NULL_PEN));
    Ellipse(mem, cx - r, cy - r, cx + r + 1, cy + r + 1);
    SelectObject(mem, old_pen);
    SelectObject(mem, old_brush);
    DeleteObject(dot);
    SetBkMode(mem, TRANSPARENT as i32);
    let title_font = font(px(15.0), FW_SEMIBOLD);
    let body_font = font(px(13.0), FW_NORMAL);
    let left = px(52.0);
    let right = w - px(18.0);
    if l.body.is_empty() {
        let old = SelectObject(mem, title_font);
        SetTextColor(mem, rgb(0xF2, 0xF3, 0xF5));
        let mut r = RECT { left, top: 0, right, bottom: h };
        draw_text(mem, &l.title, &mut r);
        SelectObject(mem, old);
    } else {
        let old = SelectObject(mem, title_font);
        SetTextColor(mem, rgb(0xF2, 0xF3, 0xF5));
        let mut r = RECT { left, top: px(7.0), right, bottom: h / 2 + px(1.0) };
        draw_text(mem, &l.title, &mut r);
        SelectObject(mem, body_font);
        SetTextColor(mem, rgb(0xB8, 0xBC, 0xC4));
        let mut r = RECT { left, top: h / 2 - px(1.0), right, bottom: h - px(7.0) };
        draw_text(mem, &l.body, &mut r);
        SelectObject(mem, old);
    }
    DeleteObject(title_font);
    DeleteObject(body_font);
    BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_bmp);
    DeleteObject(bmp);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}
