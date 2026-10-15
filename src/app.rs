//! The Windows tray application: one hidden window and one message loop on
//! the main thread, plus the audio, recogniser and inserter threads.
//!
//! Worker threads never touch UI state. They push [`AppEvent`]s into a
//! bounded channel and post `WM_APP_WAKE`; the window procedure drains the
//! channel into the [`Controller`]. Some Win32 calls (shell execute, menus,
//! message boxes) run nested message loops, so the application state is
//! borrowed with `try_borrow_mut`: if a message arrives while the
//! controller is busy, the work is retried a moment later instead of
//! re-entering it.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::asr::worker::{self, WorkerHandle, WorkerMsg};
use crate::asr::{catalog, download};
use crate::audio::{self, AudioCmd, AudioHandle};
use crate::config::{Config, HotkeyConfig, OverlayConfig};
use crate::controller::{Controller, Ports, Ui};
use crate::events::{AppEvent, DownloadEvent, HotkeyKind, TrayCommand};
use crate::hotkey::win::{HotkeyRegistry, TIMER_PTT_POLL, WM_SV_HOTKEY};
use crate::insertion::win::{WinClipboard, WinDesktop};
use crate::insertion::{self, InsertRequest, Inserter, InserterHandle, WindowHandle};
use crate::overlay::win::Overlay;
use crate::overlay::{OverlayView, Tone};
use crate::platform::{self, wide};
use crate::postprocess::vocabulary::Vocabulary;
use crate::postprocess::PostProcessor;
use crate::tray::win::{show_menu, Tray, WM_TRAY};
use crate::tray::TrayStatus;

const WM_APP_WAKE: u32 = WM_APP + 10;
const WM_APP_ASK: u32 = WM_APP + 11;
const TIMER_CONFIG_POLL: usize = 0x5330;
const TIMER_RETRY: usize = 0x5331;
const MAIN_CLASS: &str = "SwiftVoiceMain";

/// Cross-thread event delivery to the UI thread.
#[derive(Clone)]
pub struct Sink {
    tx: Sender<AppEvent>,
    hwnd: isize,
}

impl Sink {
    pub fn send(&self, ev: AppEvent) {
        match self.tx.try_send(ev) {
            Ok(()) => unsafe {
                PostMessageW(self.hwnd as HWND, WM_APP_WAKE, 0, 0);
            },
            Err(_) => log::error!("UI event queue is full; an event was dropped"),
        }
    }
}

pub struct WinUi {
    hwnd: HWND,
    tray: Tray,
    overlay: Option<Overlay>,
    hotkeys: HotkeyRegistry,
    pending_ask: Option<(String, String)>,
    asked: HashSet<String>,
}

impl Ui for WinUi {
    fn set_status(&mut self, status: &TrayStatus, tooltip: &str) {
        self.tray.set(status.tone(), tooltip);
    }

    fn overlay(&mut self, view: OverlayView, hide_after_ms: Option<u32>) {
        if let Some(o) = self.overlay.as_mut() {
            o.show(view, hide_after_ms);
        }
    }

    fn notify(&mut self, title: &str, body: &str, tone: Tone) {
        match tone {
            Tone::Error => log::error!("{title}: {body}"),
            Tone::Warning => log::warn!("{title}: {body}"),
            _ => log::info!("{title}: {body}"),
        }
        self.tray.balloon(title, body, tone);
    }

    fn set_listening(&mut self, active: bool) {
        self.hotkeys.set_cancel_active(active);
    }

    fn apply_hotkeys(&mut self, cfg: &HotkeyConfig) -> Result<(), String> {
        self.hotkeys.apply(cfg)
    }

    fn foreground_window(&self) -> Option<WindowHandle> {
        let h = unsafe { GetForegroundWindow() };
        (!h.is_null()).then_some(h as isize)
    }

    fn open_path(&mut self, path: &Path) {
        if let Err(e) = platform::open_path(path) {
            self.notify("Cannot open", &e, Tone::Error);
        }
    }

    fn set_autostart(&mut self, on: bool) -> Result<(), String> {
        platform::set_autostart(on)
    }

    fn autostart_enabled(&self) -> bool {
        platform::autostart_enabled()
    }

    fn ask_download(&mut self, model_id: &str, reason: &str) {
        if self.asked.insert(model_id.to_string()) {
            self.pending_ask = Some((model_id.to_string(), reason.to_string()));
            unsafe { PostMessageW(self.hwnd, WM_APP_ASK, 0, 0) };
        } else {
            let display = catalog::find(model_id).map(|m| m.display).unwrap_or(model_id);
            self.notify("Model needed", &format!("{reason} Install {display} from the tray menu: Engine & model."), Tone::Warning);
        }
    }

    fn quit(&mut self) {
        unsafe { PostQuitMessage(0) };
    }

    fn configure_overlay(&mut self, cfg: &OverlayConfig) {
        if let Some(o) = self.overlay.as_mut() {
            o.configure(cfg.position, cfg.opacity);
        }
    }
}

pub struct WinPorts {
    audio: AudioHandle,
    worker: WorkerHandle,
    inserter: InserterHandle,
    sink: Sink,
}

impl Ports for WinPorts {
    fn audio(&self, cmd: AudioCmd) {
        self.audio.send(cmd);
    }

    fn asr(&self, msg: WorkerMsg) -> bool {
        self.worker.send(msg)
    }

    fn insert(&self, req: InsertRequest) {
        self.inserter.send(req);
    }

    fn smart(&self, session: u64, utterance: u32, raw: String, post: Arc<PostProcessor>, language: Option<String>, english: bool) {
        let sink = self.sink.clone();
        let spawned = std::thread::Builder::new().name("swiftvoice-smart".into()).spawn(move || {
            let result = post.process(&raw, language.as_deref(), english);
            sink.send(AppEvent::Postprocessed { session, utterance, result });
        });
        if let Err(e) = spawned {
            log::error!("cannot start Smart mode thread: {e}");
        }
    }

    fn download(&self, model: String, models_dir: PathBuf) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let sink = self.sink.clone();
        let spawned = std::thread::Builder::new().name("swiftvoice-download".into()).spawn(move || {
            let result = match catalog::find(&model) {
                Some(info) => {
                    let mut last: Option<(Option<u32>, download::Phase)> = None;
                    let mut progress = |p: download::Progress| {
                        let key = (p.percent(), p.phase);
                        if last != Some(key) {
                            last = Some(key);
                            sink.send(AppEvent::Download(DownloadEvent::Progress {
                                model: model.clone(),
                                percent: p.percent(),
                                phase: p.phase,
                            }));
                        }
                    };
                    download::install(&models_dir, info, &mut progress, &flag).map(|_| ()).map_err(|e| format!("{e:#}"))
                }
                None => Err(format!("unknown model {model}")),
            };
            sink.send(AppEvent::Download(DownloadEvent::Done { model, result }));
        });
        if let Err(e) = spawned {
            log::error!("cannot start download thread: {e}");
        }
        cancel
    }
}

/// Polls config.toml and vocabulary.toml modification times.
struct FileWatch {
    stamps: Vec<(PathBuf, Option<SystemTime>)>,
}

impl FileWatch {
    fn stamp(p: &Path) -> Option<SystemTime> {
        std::fs::metadata(p).and_then(|m| m.modified()).ok()
    }

    fn new(paths: &[PathBuf]) -> FileWatch {
        FileWatch { stamps: paths.iter().map(|p| (p.clone(), Self::stamp(p))).collect() }
    }

    /// Track `paths` and report whether any of them changed.
    fn changed(&mut self, paths: &[PathBuf]) -> bool {
        let mut changed = false;
        for p in paths {
            let now = Self::stamp(p);
            match self.stamps.iter_mut().find(|(q, _)| q == p) {
                Some((_, old)) => {
                    if *old != now {
                        *old = now;
                        changed = true;
                    }
                }
                None => self.stamps.push((p.clone(), now)),
            }
        }
        changed
    }
}

struct App {
    controller: Controller<WinUi, WinPorts>,
    rx: Receiver<AppEvent>,
    watch: FileWatch,
    config_path: PathBuf,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static PENDING: RefCell<VecDeque<AppEvent>> = const { RefCell::new(VecDeque::new()) };
    static MAIN: Cell<isize> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

fn main_hwnd() -> HWND {
    MAIN.with(|m| m.get()) as HWND
}

/// Run `f` with the application state unless it is already borrowed.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().map(f),
        Err(_) => None,
    })
}

fn retry_soon() {
    unsafe { SetTimer(main_hwnd(), TIMER_RETRY, 15, None) };
}

/// Queue an event produced on the UI thread and process the queue.
fn dispatch(ev: AppEvent) {
    PENDING.with(|p| p.borrow_mut().push_back(ev));
    pump();
}

/// Feed queued events to the controller.
fn pump() {
    let done = APP.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else { return false };
        let Some(app) = guard.as_mut() else { return true };
        loop {
            let ev = PENDING.with(|p| p.borrow_mut().pop_front()).or_else(|| app.rx.try_recv().ok());
            let Some(ev) = ev else { break };
            app.controller.handle(ev);
        }
        true
    });
    if !done {
        retry_soon();
    }
}

fn confirm_download(hwnd: HWND, id: &str, reason: Option<&str>) -> bool {
    let Some(info) = catalog::find(id) else { return false };
    let dir = with_app(|a| a.controller.config().models_dir()).unwrap_or_else(crate::paths::default_models_dir);
    let host = info.url.split('/').nth(2).unwrap_or(info.url);
    let text = format!(
        "{}Download {}?\n\n{}\nSize: {} from {}\nLicense: {} ({})\n\nThe file is verified against a pinned digest and saved in:\n{}\n\nThis is the only time SwiftVoice uses the network. Dictation itself stays fully offline.",
        reason.map(|r| format!("{r}\n\n")).unwrap_or_default(),
        info.display,
        info.summary(),
        catalog::human_bytes(info.download_bytes),
        host,
        info.license,
        info.model_card,
        dir.display()
    );
    platform::yes_no(hwnd, "SwiftVoice: download model", &text)
}

fn about_text(config_path: &Path) -> String {
    format!(
        "SwiftVoice {}\nFast, private, offline voice typing.\n\n\
Privacy: audio and text never leave this computer. There are no accounts and no telemetry (ONNX Runtime's own telemetry is switched off). The network is used only when you choose to download a model.\n\n\
Models and libraries:\n\
• NVIDIA Parakeet TDT 0.6B v2/v3 and TDT-CTC 110M: CC-BY-4.0, © NVIDIA. ONNX exports by the sherpa-onnx project (k2-fsa, Apache-2.0).\n\
• Whisper models: MIT, © OpenAI. whisper.cpp: MIT, © The ggml authors.\n\
• Silero VAD: MIT, © Silero Team.\n\
• ONNX Runtime {}: MIT, © Microsoft.\n\
• RNNoise (nnnoiseless): BSD-3-Clause.\n\n\
Settings: {}\nLogs: {}",
        env!("CARGO_PKG_VERSION"),
        crate::asr::runtime::ORT_VERSION,
        config_path.display(),
        crate::paths::logs_dir().display()
    )
}

fn handle_menu_command(hwnd: HWND, cmd: TrayCommand) {
    match cmd {
        TrayCommand::UiChangeHotkey(kind) => {
            let Some(cfg) = with_app(|a| {
                a.controller.ui.hotkeys.suspend();
                a.controller.config().hotkeys.clone()
            }) else {
                return;
            };
            let what = match kind {
                HotkeyKind::PushToTalk => "push-to-talk (hold to dictate)",
                HotkeyKind::Toggle => "start/stop listening",
            };
            let chosen = crate::hotkey::capture::capture_hotkey(hwnd, what);
            // Still suspended: only other applications' shortcuts conflict.
            let other = match kind {
                HotkeyKind::PushToTalk => &cfg.toggle,
                HotkeyKind::Toggle => &cfg.push_to_talk,
            };
            let problem = chosen.and_then(|hk| {
                if crate::hotkey::Hotkey::parse(other).ok() == Some(hk) {
                    Some(format!("{hk} is already SwiftVoice's other shortcut."))
                } else {
                    crate::hotkey::win::probe(hwnd, &hk).err()
                }
            });
            with_app(|a| {
                if let Err(e) = a.controller.ui.hotkeys.resume(&cfg) {
                    log::warn!("{e}");
                }
            });
            match (chosen, problem) {
                (Some(_), Some(e)) => {
                    with_app(|a| a.controller.ui.notify("Shortcut not changed", &e, Tone::Warning));
                }
                (Some(hk), None) => dispatch(AppEvent::Tray(TrayCommand::SetHotkey(kind, hk))),
                (None, _) => {}
            }
        }
        TrayCommand::UiChooseModelsFolder => {
            if let Some(dir) = platform::pick_folder(hwnd, "Choose the folder for SwiftVoice speech models") {
                dispatch(AppEvent::Tray(TrayCommand::SetModelsDir(dir)));
            }
        }
        TrayCommand::UiAbout => {
            let path = with_app(|a| a.config_path.clone()).unwrap_or_else(crate::paths::config_file);
            platform::info_box(hwnd, "About SwiftVoice", &about_text(&path));
        }
        TrayCommand::UiConfirmDownload(id) => {
            if confirm_download(hwnd, &id, None) {
                dispatch(AppEvent::Tray(TrayCommand::DownloadModel(id)));
            }
        }
        other => dispatch(AppEvent::Tray(other)),
    }
}

fn show_tray_menu(hwnd: HWND) {
    let Some(entries) = with_app(|a| {
        a.controller.refresh_devices();
        a.controller.menu()
    }) else {
        return;
    };
    if let Some(cmd) = show_menu(hwnd, &entries) {
        handle_menu_command(hwnd, cmd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_WAKE => {
            pump();
            0
        }
        WM_HOTKEY => {
            match with_app(|a| a.controller.ui.hotkeys.on_hotkey(wparam as i32)) {
                Some(Some(ev)) => dispatch(AppEvent::Hotkey(ev)),
                Some(None) => {}
                // Busy in a nested loop: try again shortly.
                None => {
                    PostMessageW(hwnd, WM_HOTKEY, wparam, lparam);
                }
            }
            0
        }
        // Modifier-only shortcuts (RAlt/LAlt) arrive from the keyboard hook.
        WM_SV_HOTKEY => {
            match with_app(|a| a.controller.ui.hotkeys.on_hotkey(wparam as i32)) {
                Some(Some(ev)) => dispatch(AppEvent::Hotkey(ev)),
                Some(None) => {}
                None => {
                    PostMessageW(hwnd, WM_SV_HOTKEY, wparam, lparam);
                }
            }
            0
        }
        WM_TIMER => {
            match wparam {
                TIMER_PTT_POLL => {
                    if let Some(Some(ev)) = with_app(|a| a.controller.ui.hotkeys.on_timer()) {
                        dispatch(AppEvent::Hotkey(ev));
                    }
                }
                TIMER_CONFIG_POLL => {
                    let changed = with_app(|a| {
                        let vocab = a.controller.config().vocabulary_path(&a.config_path);
                        let paths = [a.config_path.clone(), vocab];
                        a.watch.changed(&paths)
                    });
                    if changed == Some(true) {
                        dispatch(AppEvent::ConfigChanged);
                    }
                }
                TIMER_RETRY => {
                    KillTimer(hwnd, TIMER_RETRY);
                    pump();
                }
                _ => {}
            }
            0
        }
        WM_TRAY => {
            let event = (lparam & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP || event == WM_CONTEXTMENU {
                show_tray_menu(hwnd);
            }
            0
        }
        WM_APP_ASK => {
            if let Some(Some((id, reason))) = with_app(|a| a.controller.ui.pending_ask.take()) {
                if confirm_download(hwnd, &id, Some(&reason)) {
                    dispatch(AppEvent::Tray(TrayCommand::DownloadModel(id)));
                }
            }
            0
        }
        WM_POWERBROADCAST => {
            if wparam as u32 == PBT_APMRESUMEAUTOMATIC {
                dispatch(AppEvent::PowerResume);
            }
            1
        }
        WM_WTSSESSION_CHANGE => {
            if wparam as u32 == windows_sys::Win32::UI::WindowsAndMessaging::WTS_SESSION_LOCK {
                dispatch(AppEvent::SessionLocked);
            }
            0
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                // Windows is shutting down: stop threads and restore the
                // clipboard now; the process may not get another chance.
                APP.with(|cell| {
                    if let Ok(mut g) = cell.try_borrow_mut() {
                        g.take();
                    }
                });
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.with(|t| t.get()) => {
            // Explorer restarted: the tray icon must be added again.
            with_app(|a| {
                if let Err(e) = a.controller.ui.tray.add() {
                    log::warn!("{e}");
                }
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_main_window() -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(null());
        let class = wide(MAIN_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(anyhow!("cannot register the main window class"));
        }
        // A real (hidden, never shown) top-level window: message-only
        // windows do not receive power and session broadcasts.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            wide("SwiftVoice").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        );
        if hwnd.is_null() {
            return Err(anyhow!("cannot create the main window (error {})", windows_sys::Win32::Foundation::GetLastError()));
        }
        Ok(hwnd)
    }
}

/// Run the tray application until the user quits.
pub fn run(config_path: &Path, verbose: bool, process_start: Instant) -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
    }
    let Some(_instance) = platform::SingleInstance::acquire().map_err(|e| anyhow!(e))? else {
        platform::info_box(
            null_mut(),
            "SwiftVoice",
            "SwiftVoice is already running. Look for the microphone icon in the notification area (click ^ next to the clock if it is hidden).",
        );
        return Ok(());
    };
    let (cfg, cfg_error) = match Config::load_or_create(config_path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };
    if !verbose {
        crate::logging::set_level(cfg.log_level_filter());
    }
    log::info!(
        "SwiftVoice {} starting (config {}, models {})",
        env!("CARGO_PKG_VERSION"),
        config_path.display(),
        cfg.models_dir().display()
    );
    let vocab_path = cfg.vocabulary_path(config_path);
    let (vocab, vocab_error) = match Vocabulary::load_user_file(&vocab_path) {
        Ok(v) => (v, None),
        Err(e) => (Vec::new(), Some(format!("{e:#}"))),
    };

    let hwnd = create_main_window()?;
    MAIN.with(|m| m.set(hwnd as isize));
    TASKBAR_CREATED.with(|t| t.set(unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) }));

    let (tx, rx) = crossbeam_channel::bounded(4096);
    let sink = Sink { tx, hwnd: hwnd as isize };
    let audio_handle = {
        let s = sink.clone();
        audio::pipeline::spawn(Controller::<WinUi, WinPorts>::capture_settings(&cfg), Box::new(move |ev| s.send(AppEvent::Audio(ev))))?
    };
    let worker_handle = {
        let s = sink.clone();
        worker::spawn(Controller::<WinUi, WinPorts>::worker_config(&cfg, &vocab), Box::new(move |ev| s.send(AppEvent::Asr(ev))))?
    };
    let inserter_handle = {
        let s = sink.clone();
        insertion::spawn(
            || Inserter::new(WinDesktop::default(), WinClipboard::default()),
            Box::new(move |r| s.send(AppEvent::Inserted(r))),
        )?
    };

    let mut tray = Tray::new(hwnd);
    if let Err(e) = tray.add() {
        log::warn!("{e}; it will be added when the taskbar is available");
    }
    let overlay = match Overlay::create() {
        Ok(o) => Some(o),
        Err(e) => {
            log::warn!("{e}");
            None
        }
    };
    let ui = WinUi { hwnd, tray, overlay, hotkeys: HotkeyRegistry::new(hwnd), pending_ask: None, asked: HashSet::new() };
    let ports = WinPorts { audio: audio_handle, worker: worker_handle, inserter: inserter_handle, sink };
    let mut controller = Controller::new(ui, ports, cfg, config_path.to_path_buf(), vocab);
    controller.startup();
    if let Some(e) = cfg_error {
        controller.ui.notify("Settings file has errors", &format!("{e}\nUsing default settings until it is fixed."), Tone::Error);
    }
    if let Some(e) = vocab_error {
        controller.ui.notify("Vocabulary file has errors", &e, Tone::Error);
    }
    let watch = FileWatch::new(&[config_path.to_path_buf(), vocab_path]);
    APP.with(|a| *a.borrow_mut() = Some(App { controller, rx, watch, config_path: config_path.to_path_buf() }));

    unsafe {
        SetTimer(hwnd, TIMER_CONFIG_POLL, 1000, None);
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
    }
    log::info!("tray ready {} ms after process start", process_start.elapsed().as_millis());
    pump();

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    log::info!("shutting down");
    // Dropping the app stops the worker threads (the inserter restores the
    // clipboard first) and removes the tray icon.
    APP.with(|a| a.borrow_mut().take());
    Ok(())
}
