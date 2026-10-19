//! The application state machine.
//!
//! Runs on the UI thread and owns no threads itself: it reacts to
//! [`AppEvent`]s and drives the audio thread, recogniser, post-processing
//! and inserter through the [`Ports`] trait, and the tray/overlay through
//! the [`Ui`] trait. Both are faked in the tests below, so every dictation
//! flow (push-to-talk, toggle with VAD endpointing, continuous mode, long
//! dictation with commits, cancellation, errors) is unit tested without
//! Windows.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::asr::catalog;
use crate::asr::download;
use crate::asr::worker::{AsrJob, WorkerConfig, WorkerEvent, WorkerMsg};
use crate::audio::processor::VadChoice;
use crate::audio::{AudioCmd, AudioEvent, CaptureSettings, SegEvent, SegmenterConfig, SessionMode, SessionParams, SnapKey, SnapshotKind};
use crate::config::{self, Config, HotkeyConfig, InsertMethod, PostMode, Setting};
use crate::events::{AppEvent, DownloadEvent, HotkeyEvent, HotkeyKind, TrayCommand};
use crate::insertion::{InsertOutcome, InsertReport, InsertRequest, InsertSettings, WindowHandle};
use crate::overlay::{tail_text, OverlayView, Tone};
use crate::postprocess::{vocabulary::Vocabulary, PostProcessor, PostResult};
use crate::tray::{build_menu, MenuEntry, MenuState, TrayStatus};

/// Side effects on the desktop UI. Implementations must not block or
/// re-enter the controller (modal dialogs are handled by the UI layer).
pub trait Ui {
    fn set_status(&mut self, status: &TrayStatus, tooltip: &str);
    fn overlay(&mut self, view: OverlayView, hide_after_ms: Option<u32>);
    fn notify(&mut self, title: &str, body: &str, tone: Tone);
    /// Register the cancel shortcut while listening, unregister otherwise.
    fn set_listening(&mut self, active: bool);
    fn apply_hotkeys(&mut self, cfg: &HotkeyConfig) -> Result<(), String>;
    fn foreground_window(&self) -> Option<WindowHandle>;
    fn open_path(&mut self, path: &Path);
    fn set_autostart(&mut self, on: bool) -> Result<(), String>;
    fn autostart_enabled(&self) -> bool;
    /// Ask (later, outside the controller) whether to download a model.
    fn ask_download(&mut self, model_id: &str, reason: &str);
    fn quit(&mut self);
    /// Overlay position/opacity changed.
    fn configure_overlay(&mut self, _cfg: &crate::config::OverlayConfig) {}
}

/// Connections to the worker threads.
pub trait Ports {
    fn audio(&self, cmd: AudioCmd);
    fn asr(&self, msg: WorkerMsg) -> bool;
    fn insert(&self, req: InsertRequest, clipboard_only: bool) -> Result<(), String>;
    /// Run Smart post-processing off the UI thread; deliver the result as
    /// `AppEvent::Postprocessed`.
    #[allow(clippy::too_many_arguments)]
    fn smart(&self, session: u64, utterance: u32, raw: String, post: Arc<PostProcessor>, language: Option<String>, english: bool);
    /// Start a model download; returns its cancel flag.
    fn download(&self, model: String, models_dir: PathBuf) -> Arc<AtomicBool>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Capturing,
    Waiting,
    Postprocessing,
    Ready,
    Done,
}

struct Capture {
    session: u64,
    mode: SessionMode,
    model: String,
    language: Option<String>,
    english: bool,
    started: Instant,
    original: Option<WindowHandle>,
    speech: bool,
    finishing: bool,
}

struct Utterance {
    model: String,
    language: Option<String>,
    english: bool,
    original: Option<WindowHandle>,
    results: HashMap<SnapKey, Result<String, String>>,
    commits: Vec<SnapKey>,
    end: Option<Option<SnapKey>>,
    partial: Option<(SnapKey, String)>,
    stage: Stage,
    output: Option<Result<PostResult, String>>,
    ended_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelState {
    Loading,
    Ready,
    Failed(String),
}

pub struct Controller<U: Ui, P: Ports> {
    pub ui: U,
    ports: P,
    cfg: Config,
    config_path: PathBuf,
    vocab: Vec<(String, String)>,
    post: Arc<PostProcessor>,
    status: TrayStatus,
    capture: Option<Capture>,
    utterances: BTreeMap<(u64, u32), Utterance>,
    next_session: u64,
    next_insert: u64,
    in_flight: Option<(u64, (u64, u32))>,
    insertion_stalled: bool,
    retry_clipboard: Option<(u64, u32)>,
    pending_worker: Option<WorkerConfig>,
    devices: Vec<String>,
    models: HashMap<String, ModelState>,
    download: Option<(String, Arc<AtomicBool>)>,
    download_percent: Option<u32>,
    warned: HashSet<String>,
    quitting: bool,
}

fn vocab_path(cfg: &Config, config_path: &Path) -> PathBuf {
    cfg.vocabulary_path(config_path)
}

impl<U: Ui, P: Ports> Controller<U, P> {
    pub fn new(ui: U, ports: P, cfg: Config, config_path: PathBuf, vocab: Vec<(String, String)>) -> Self {
        let post = Arc::new(PostProcessor::new(&cfg.postprocess, vocab.clone()));
        Controller {
            ui,
            ports,
            cfg,
            config_path,
            vocab,
            post,
            status: TrayStatus::Starting,
            capture: None,
            utterances: BTreeMap::new(),
            next_session: 1,
            next_insert: 1,
            in_flight: None,
            insertion_stalled: false,
            retry_clipboard: None,
            pending_worker: None,
            devices: Vec::new(),
            models: HashMap::new(),
            download: None,
            download_percent: None,
            warned: HashSet::new(),
            quitting: false,
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn is_listening(&self) -> bool {
        self.capture.is_some()
    }

    /// Settings for the audio thread.
    pub fn capture_settings(cfg: &Config) -> CaptureSettings {
        CaptureSettings {
            device: cfg.audio.device.clone(),
            gain_db: cfg.audio.gain_db,
            noise_suppression: cfg.audio.noise_suppression,
            keep_warm_secs: cfg.audio.keep_warm_secs,
            pre_roll_ms: cfg.vad.pre_roll_ms,
            vad: VadChoice {
                engine: cfg.vad.engine,
                silero_model: Some(cfg.models_dir().join("silero-vad").join("silero_vad.onnx")),
                energy_margin_db: cfg.vad.energy_margin_db,
            },
        }
    }

    /// Settings for the recogniser thread.
    pub fn worker_config(cfg: &Config, vocab: &[(String, String)]) -> WorkerConfig {
        let terms = Vocabulary::new(vocab.to_vec(), false, false).bias_terms();
        WorkerConfig {
            models_dir: cfg.models_dir(),
            threads: cfg.asr.threads,
            low_memory: cfg.asr.low_memory,
            unload_after_idle_secs: cfg.asr.unload_after_idle_secs,
            warm_up: cfg.asr.warm_up,
            whisper_prompt: if terms.is_empty() { String::new() } else { format!("{}.", terms.join(", ")) },
        }
    }

    /// Called once after the UI exists.
    pub fn startup(&mut self) {
        self.ui.configure_overlay(&self.cfg.overlay);
        if let Err(e) = self.ui.apply_hotkeys(&self.cfg.hotkeys) {
            self.ui.notify("Hotkey problem", &e, Tone::Warning);
        }
        if self.cfg.general.autostart != self.ui.autostart_enabled() {
            if let Err(e) = self.ui.set_autostart(self.cfg.general.autostart) {
                log::warn!("cannot update autostart: {e}");
            }
        }
        self.ports.audio(AudioCmd::RefreshDevices);
        let dir = self.cfg.models_dir();
        match catalog::find(&self.cfg.asr.model) {
            Some(info) if download::is_installed(&dir, info) => {
                self.models.insert(info.id.into(), ModelState::Loading);
                self.ports.asr(WorkerMsg::Preload(info.id.into()));
            }
            Some(info) => {
                self.ui.ask_download(info.id, "SwiftVoice needs a speech model before you can dictate.");
            }
            None => self.ui.notify("Unknown model", &format!("asr.model = \"{}\" is not a known model.", self.cfg.asr.model), Tone::Error),
        }
        self.refresh_status();
    }

    pub fn menu(&self) -> Vec<MenuEntry> {
        let dir = self.cfg.models_dir();
        build_menu(&MenuState {
            cfg: &self.cfg,
            status: &self.status,
            listening: self.capture.is_some(),
            devices: &self.devices,
            models_dir: &dir,
            downloading: self.download.as_ref().map(|(m, _)| m.as_str()),
            autostart: self.ui.autostart_enabled(),
        })
    }

    pub fn quitting(&self) -> bool {
        self.quitting
    }

    /// Ask the audio thread to re-list microphones (used when the tray
    /// menu opens, so the next menu shows newly plugged-in devices).
    pub fn refresh_devices(&self) {
        self.ports.audio(AudioCmd::RefreshDevices);
    }

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Hotkey(h) => self.on_hotkey(h),
            AppEvent::Tray(cmd) => self.on_tray(cmd),
            AppEvent::Audio(a) => self.on_audio(a),
            AppEvent::Asr(w) => self.on_asr(w),
            AppEvent::Inserted(r) => self.on_inserted(r),
            AppEvent::Postprocessed { session, utterance, result } => {
                if let Some(u) = self.utterances.get_mut(&(session, utterance)) {
                    u.output = Some(Ok(result));
                    u.stage = Stage::Ready;
                }
                self.flush_ready();
            }
            AppEvent::Download(d) => self.on_download(d),
            AppEvent::ConfigChanged => self.reload_config(),
            AppEvent::PowerResume => {
                log::info!("resumed from sleep: releasing the audio device");
                self.cancel_capture("Dictation stopped (the computer went to sleep)");
                self.ports.audio(AudioCmd::ReleaseDevice);
                if let Err(e) = self.ui.apply_hotkeys(&self.cfg.hotkeys) {
                    log::warn!("re-registering hotkeys after resume: {e}");
                }
            }
            AppEvent::SessionLocked => self.cancel_capture("Dictation stopped (the session was locked)"),
        }
        self.sync_worker();
        self.refresh_status();
    }

    // ----- hotkeys -------------------------------------------------------

    fn on_hotkey(&mut self, h: HotkeyEvent) {
        match h {
            HotkeyEvent::PttDown => {
                if self.capture.is_none() {
                    self.start_session(SessionMode::PushToTalk);
                }
            }
            HotkeyEvent::PttUp => {
                let Some(c) = self.capture.as_mut() else { return };
                if c.mode != SessionMode::PushToTalk || c.finishing {
                    return;
                }
                if c.started.elapsed().as_millis() < self.cfg.hotkeys.min_hold_ms as u128 && !c.speech {
                    let s = c.session;
                    self.ports.audio(AudioCmd::Cancel { session: s });
                    self.ports.asr(WorkerMsg::CancelSession(s));
                    self.drop_session_utterances(s);
                    self.end_capture();
                    self.show(OverlayView::Message { text: "Hold the shortcut while you speak".into(), tone: Tone::Info }, Some(1200));
                    return;
                }
                c.finishing = true;
                let s = c.session;
                self.ports.audio(AudioCmd::Finish { session: s, tail_ms: self.cfg.hotkeys.release_tail_ms });
            }
            HotkeyEvent::Toggle => match self.capture.as_mut() {
                None => self.start_session(SessionMode::Toggle { continuous: self.cfg.vad.continuous }),
                Some(c) if matches!(c.mode, SessionMode::Toggle { .. }) && !c.finishing => {
                    c.finishing = true;
                    let s = c.session;
                    self.ports.audio(AudioCmd::Finish { session: s, tail_ms: 0 });
                }
                Some(_) => {}
            },
            HotkeyEvent::Cancel => self.cancel_capture("Cancelled"),
        }
    }

    fn cancel_capture(&mut self, message: &str) {
        if let Some(c) = self.capture.take() {
            self.ports.audio(AudioCmd::Cancel { session: c.session });
            self.ports.asr(WorkerMsg::CancelSession(c.session));
            self.drop_session_utterances(c.session);
            self.ui.set_listening(false);
            self.show(OverlayView::Message { text: message.into(), tone: Tone::Info }, Some(1000));
        }
    }

    fn drop_session_utterances(&mut self, session: u64) {
        self.utterances.retain(|(s, _), _| *s != session);
        if self.retry_clipboard.is_some_and(|(s, _)| s == session) {
            self.retry_clipboard = None;
        }
    }

    fn start_session(&mut self, mode: SessionMode) {
        if self.insertion_stalled {
            self.insertion_stalled = false;
            self.retry_clipboard = self.utterances.keys().next().copied();
            if self.retry_clipboard.is_some() {
                self.flush_ready();
                return;
            }
        }
        if self.pending_worker.is_some() {
            self.ui.notify("Settings are pending", "Wait for the current dictation to finish before starting again.", Tone::Info);
            return;
        }
        if self.utterances.len() >= 32 {
            self.ui.notify("Insertion is busy", "Wait for the pending dictation to finish before starting again.", Tone::Warning);
            return;
        }
        let dir = self.cfg.models_dir();
        let language = self.cfg.asr.language.clone();
        let model = match catalog::route(&language, &self.cfg.asr.model, &self.cfg.asr.fallback_model) {
            Ok(m) => m.to_string(),
            Err(e) => {
                self.show(OverlayView::Message { text: e.clone(), tone: Tone::Error }, Some(4000));
                self.ui.notify("Language not supported", &e, Tone::Error);
                return;
            }
        };
        let info = catalog::find(&model).expect("route returns catalogue models");
        if !download::is_installed(&dir, info) {
            let reason = if model == self.cfg.asr.model {
                "SwiftVoice needs a speech model before you can dictate.".to_string()
            } else {
                format!("{} is transcribed with {}, which is not installed yet.", catalog::language_name(&language), info.display)
            };
            self.ui.ask_download(info.id, &reason);
            return;
        }
        if let Some(ModelState::Failed(e)) = self.models.get(&model) {
            let e = e.clone();
            self.show(OverlayView::Message { text: format!("Model failed to load: {}", tail_text(&e, 80)), tone: Tone::Error }, Some(5000));
            return;
        }
        if !self.models.contains_key(&model) {
            self.models.insert(model.clone(), ModelState::Loading);
        }
        let session = self.next_session;
        self.next_session += 1;
        let english = language == "en" || (language == "auto" && info.languages == ["en"]);
        let original = self.ui.foreground_window();
        let v = &self.cfg.vad;
        // The overlay draws a bare dot, so there is no live text to feed:
        // partial re-transcriptions would only burn CPU while you talk.
        let partial_ms = None;
        let seg = match mode {
            SessionMode::PushToTalk => SegmenterConfig::from_ms(
                v.threshold,
                v.pause_ms,
                None,
                v.pre_roll_ms,
                self.cfg.asr.commit_after_secs,
                partial_ms,
                None,
                None,
            ),
            SessionMode::Toggle { continuous } => SegmenterConfig::from_ms(
                v.threshold,
                v.pause_ms,
                (v.endpoint_ms > 0).then_some(v.endpoint_ms),
                v.pre_roll_ms,
                self.cfg.asr.commit_after_secs,
                partial_ms,
                Some(v.no_speech_timeout_ms),
                continuous.then_some(v.idle_stop_secs * 1000),
            ),
        };
        self.ports.audio(AudioCmd::Start(SessionParams { session, mode, seg, requested_at: Instant::now() }));
        self.capture = Some(Capture {
            session,
            mode,
            model: model.clone(),
            language: (language != "auto").then_some(language),
            english,
            started: Instant::now(),
            original,
            speech: false,
            finishing: false,
        });
        self.ui.set_listening(true);
        let hint = match self.models.get(&model) {
            Some(ModelState::Loading) => Some("Loading the model; keep talking…".to_string()),
            _ => None,
        };
        self.show(OverlayView::Listening { level: 0.0, text: String::new(), hint }, None);
    }

    fn end_capture(&mut self) {
        if self.capture.take().is_some() {
            self.ui.set_listening(false);
        }
    }

    // ----- audio events --------------------------------------------------

    fn on_audio(&mut self, ev: AudioEvent) {
        match ev {
            AudioEvent::Started { session, device, sample_rate, start_latency_ms, vad } => {
                log::info!("session {session}: recording from \"{device}\" at {sample_rate} Hz ({vad} VAD), {start_latency_ms} ms after the hotkey");
            }
            AudioEvent::Level { session, level } => {
                if self.capture.as_ref().map(|c| c.session) == Some(session) && self.cfg.overlay.enabled {
                    let text = self.live_text(session);
                    self.ui.overlay(OverlayView::Listening { level, text, hint: None }, None);
                }
            }
            AudioEvent::Seg { session, event } => self.on_seg(session, event),
            AudioEvent::Ended { session } => {
                if self.capture.as_ref().map(|c| c.session) == Some(session) {
                    self.end_capture();
                }
                // An utterance that never received its end (device lost
                // mid-sentence) can never complete; drop it so later
                // utterances are not held back.
                let stale: Vec<(u64, u32)> =
                    self.utterances.iter().filter(|((s, _), u)| *s == session && u.stage == Stage::Capturing).map(|(k, _)| *k).collect();
                for k in stale {
                    self.utterances.remove(&k);
                }
                self.flush_ready();
            }
            AudioEvent::Warning(w) => {
                if self.warned.insert(w.clone()) {
                    self.ui.notify("SwiftVoice", &w, Tone::Warning);
                }
            }
            AudioEvent::Failed { session, error } => {
                if session.is_some() && session == self.capture.as_ref().map(|c| c.session) {
                    let s = session.unwrap_or_default();
                    self.ports.asr(WorkerMsg::CancelSession(s));
                    self.drop_session_utterances(s);
                    self.end_capture();
                }
                let text = error.to_string();
                self.show(OverlayView::Message { text: tail_text(&text, 140), tone: Tone::Error }, Some(6000));
                // A recurring problem refreshes the overlay; one balloon per distinct failure.
                if self.warned.insert(text.clone()) {
                    self.ui.notify("Microphone problem", &text, Tone::Error);
                }
            }
            AudioEvent::Devices(list) => match list {
                Ok(list) => self.devices = list,
                Err(e) => log::warn!("cannot list microphones: {e}"),
            },
        }
    }

    fn on_seg(&mut self, session: u64, ev: SegEvent) {
        let Some(c) = self.capture.as_mut().filter(|c| c.session == session) else {
            // Events for a cancelled session.
            return;
        };
        match ev {
            SegEvent::SpeechStart { utterance } => {
                c.speech = true;
                let u = Utterance {
                    model: c.model.clone(),
                    language: c.language.clone(),
                    english: c.english,
                    original: c.original,
                    results: HashMap::new(),
                    commits: Vec::new(),
                    end: None,
                    partial: None,
                    stage: Stage::Capturing,
                    output: None,
                    ended_at: None,
                };
                self.utterances.insert((session, utterance), u);
            }
            SegEvent::Snapshot(s) => {
                let Some(u) = self.utterances.get(&(session, s.key.utterance)) else { return };
                let job = AsrJob {
                    session,
                    key: s.key,
                    kind: s.kind,
                    audio: s.audio,
                    context: s.context,
                    model: u.model.clone(),
                    language: u.language.clone(),
                    enqueued: Instant::now(),
                };
                if !self.ports.asr(WorkerMsg::Job(job)) && s.kind != SnapshotKind::Partial {
                    if let Some(u) = self.utterances.get_mut(&(session, s.key.utterance)) {
                        u.results.insert(s.key, Err("the recogniser queue is full".into()));
                    }
                }
            }
            SegEvent::Commit { key } => {
                if let Some(u) = self.utterances.get_mut(&(session, key.utterance)) {
                    u.commits.push(key);
                }
            }
            SegEvent::UtteranceEnd { utterance, last, .. } => {
                let continuous = matches!(c.mode, SessionMode::Toggle { continuous: true });
                if let Some(u) = self.utterances.get_mut(&(session, utterance)) {
                    u.end = Some(last);
                    u.stage = Stage::Waiting;
                    u.ended_at = Some(Instant::now());
                }
                if !continuous {
                    let text = self.live_text(session);
                    self.show(OverlayView::Processing { text }, None);
                }
                self.try_complete(session, utterance);
                if self.utterances.len() >= 32 {
                    if let Some(c) = self.capture.as_mut().filter(|c| !c.finishing) {
                        c.finishing = true;
                        self.ports.audio(AudioCmd::Finish { session: c.session, tail_ms: 0 });
                        self.ui.notify("Insertion is busy", "Dictation stopped while the pending text is inserted.", Tone::Warning);
                    }
                }
            }
            SegEvent::NoSpeech => {
                self.show(OverlayView::Message { text: "No speech detected".into(), tone: Tone::Info }, Some(1500));
            }
            SegEvent::IdleTimeout => {
                log::info!("continuous dictation stopped after {} s of silence", self.cfg.vad.idle_stop_secs);
            }
        }
    }

    /// Committed text plus the newest partial for the live overlay.
    fn live_text(&self, session: u64) -> String {
        let mut parts: Vec<String> = Vec::new();
        for ((s, _), u) in self.utterances.range((session, 0)..=(session, u32::MAX)) {
            if *s != session || u.stage == Stage::Done {
                continue;
            }
            for k in &u.commits {
                if let Some(Ok(t)) = u.results.get(k) {
                    parts.push(t.clone());
                }
            }
            if let Some((_, t)) = &u.partial {
                parts.push(t.clone());
            }
        }
        tail_text(crate::postprocess::join_windows(parts.iter().map(String::as_str)).trim(), 120)
    }

    // ----- recogniser events ---------------------------------------------

    fn on_asr(&mut self, ev: WorkerEvent) {
        match ev {
            WorkerEvent::Output(o) => {
                let key = (o.session, o.key.utterance);
                let Some(u) = self.utterances.get_mut(&key) else { return };
                if o.kind == SnapshotKind::Partial {
                    if let Ok(text) = o.result {
                        let newer = u.partial.as_ref().map(|(k, _)| o.key.window > k.window || o.key.end >= k.end).unwrap_or(true);
                        let committed_ahead = u.commits.iter().any(|c| c.window >= o.key.window);
                        if newer && !committed_ahead {
                            u.partial = Some((o.key, text));
                        }
                    }
                    if self.capture.as_ref().map(|c| c.session) == Some(o.session) && self.cfg.overlay.enabled {
                        let text = self.live_text(o.session);
                        self.ui.overlay(OverlayView::Listening { level: -1.0, text, hint: None }, None);
                    }
                    return;
                }
                u.results.insert(o.key, o.result);
                self.try_complete(key.0, key.1);
                if self.capture.as_ref().map(|c| c.session) == Some(o.session) && self.cfg.overlay.enabled {
                    let text = self.live_text(o.session);
                    if !text.is_empty() {
                        self.ui.overlay(OverlayView::Listening { level: -1.0, text, hint: None }, None);
                    }
                }
            }
            WorkerEvent::ModelLoading { model } => {
                self.models.insert(model, ModelState::Loading);
            }
            WorkerEvent::ModelLoaded { model, load_ms, warmup_ms } => {
                log::info!("{model} ready (load {load_ms} ms, warm-up {warmup_ms} ms)");
                self.models.insert(model, ModelState::Ready);
            }
            WorkerEvent::ModelFailed { model, error } => {
                self.ui.notify("Model failed to load", &format!("{model}: {error}"), Tone::Error);
                self.models.insert(model, ModelState::Failed(error));
            }
            WorkerEvent::ModelUnloaded { model } => {
                self.models.remove(&model);
            }
        }
    }

    fn try_complete(&mut self, session: u64, utterance: u32) {
        let post = self.post.clone();
        let Some(u) = self.utterances.get_mut(&(session, utterance)) else { return };
        if u.stage != Stage::Waiting {
            return;
        }
        let Some(last) = u.end else { return };
        let mut keys: Vec<SnapKey> = u.commits.clone();
        keys.extend(last);
        let mut texts = Vec::with_capacity(keys.len());
        for k in &keys {
            match u.results.get(k) {
                None => return,
                Some(Err(e)) => {
                    u.output = Some(Err(e.clone()));
                    u.stage = Stage::Ready;
                    self.flush_ready();
                    return;
                }
                Some(Ok(t)) => texts.push(t.clone()),
            }
        }
        let raw = crate::postprocess::join_windows(texts.iter().map(String::as_str));
        if let Some(at) = u.ended_at {
            log::info!("utterance {session}/{utterance}: transcript ready {} ms after it ended", at.elapsed().as_millis());
        }
        log::trace!("raw transcript: {raw:?}");
        if post.mode() == PostMode::Smart && !raw.is_empty() {
            u.stage = Stage::Postprocessing;
            let (lang, english) = (u.language.clone(), u.english);
            self.ports.smart(session, utterance, raw, post, lang, english);
        } else {
            let result = post.process(&raw, u.language.as_deref(), u.english);
            u.output = Some(Ok(result));
            u.stage = Stage::Ready;
            self.flush_ready();
        }
    }

    /// Insert finished utterances in order.
    fn flush_ready(&mut self) {
        if self.in_flight.is_some() || self.insertion_stalled {
            return;
        }
        loop {
            let Some((&key, u)) = self.utterances.iter().next() else { return };
            if u.stage != Stage::Ready {
                return;
            }
            let original = u.original;
            match u.output.clone() {
                Some(Ok(result)) => {
                    if let Some(note) = &result.note {
                        let n = format!("Smart mode unavailable, inserted the Clean text: {note}");
                        if self.warned.insert("smart-fallback".into()) {
                            self.ui.notify("Smart mode", &n, Tone::Warning);
                        }
                    }
                    if result.text.is_empty() {
                        self.utterances.remove(&key);
                        self.show(OverlayView::Message { text: "Didn't catch that".into(), tone: Tone::Info }, Some(1200));
                        continue;
                    }
                    let id = self.next_insert;
                    self.next_insert += 1;
                    let still_listening = self.capture.as_ref().map(|c| c.session) == Some(key.0);
                    if !still_listening {
                        self.show(OverlayView::Processing { text: tail_text(&result.text, 120) }, None);
                    }
                    let request =
                        InsertRequest { id, text: result.text, original, settings: InsertSettings::from_config(&self.cfg.insertion) };
                    let clipboard_only = self.retry_clipboard == Some(key);
                    if clipboard_only {
                        self.retry_clipboard = None;
                    }
                    match self.ports.insert(request, clipboard_only) {
                        Ok(()) => self.in_flight = Some((id, key)),
                        Err(e) => {
                            self.insertion_stalled = true;
                            self.ui.notify(
                                "Could not queue the text",
                                &format!("{e}. Press the dictation shortcut to recover it to the clipboard."),
                                Tone::Error,
                            );
                        }
                    }
                    return;
                }
                Some(Err(e)) => {
                    self.utterances.remove(&key);
                    self.show(
                        OverlayView::Message { text: format!("Transcription failed: {}", tail_text(&e, 100)), tone: Tone::Error },
                        Some(5000),
                    );
                    self.ui.notify("Transcription failed", &e, Tone::Error);
                }
                None => {
                    self.utterances.remove(&key);
                }
            }
        }
    }

    fn on_inserted(&mut self, r: InsertReport) {
        let Some((id, key)) = self.in_flight else {
            return;
        };
        if id != r.id {
            return;
        }
        self.in_flight = None;
        let retained = self.utterances.contains_key(&key);
        if !retained {
            self.flush_ready();
            return;
        }
        if retained && matches!(&r.outcome, InsertOutcome::Failed(_)) {
            self.insertion_stalled = true;
        } else {
            self.utterances.remove(&key);
        }
        if retained && matches!(&r.outcome, InsertOutcome::Clipboard { .. }) {
            self.insertion_stalled = true;
        }
        let listening = self.capture.is_some();
        match r.outcome {
            InsertOutcome::Inserted { .. } => {
                if !listening {
                    self.show(
                        OverlayView::Result { text: "Inserted".into(), tone: Tone::Success, detail: None },
                        Some(self.cfg.overlay.hide_after_ms),
                    );
                }
            }
            InsertOutcome::Clipboard { reason } => {
                self.show(
                    OverlayView::Result { text: "Copied to clipboard".into(), tone: Tone::Warning, detail: Some(reason.clone()) },
                    Some(5000),
                );
                self.ui.notify(
                    "Text copied to the clipboard",
                    &format!("{reason}. Paste this text before pressing the dictation shortcut to recover any remaining text."),
                    Tone::Warning,
                );
            }
            InsertOutcome::Failed(e) => {
                self.show(
                    OverlayView::Message { text: format!("Could not insert: {}", tail_text(&e, 100)), tone: Tone::Error },
                    Some(5000),
                );
                self.ui.notify(
                    "Could not insert the text",
                    &format!("{e}. Your text is retained. Press the dictation shortcut to recover it to the clipboard."),
                    Tone::Error,
                );
            }
            InsertOutcome::Empty => {
                if !listening {
                    self.ui.overlay(OverlayView::Hidden, None);
                }
            }
        }
        self.flush_ready();
    }

    // ----- tray commands -------------------------------------------------

    fn set(&mut self, table: &str, key: &str, value: Setting) {
        match config::update_setting(&self.config_path, table, key, value) {
            Ok(cfg) => self.apply_config(cfg, self.vocab.clone()),
            Err(e) => self.ui.notify("Cannot change the setting", &format!("{e:#}"), Tone::Error),
        }
    }

    fn on_tray(&mut self, cmd: TrayCommand) {
        match cmd {
            TrayCommand::StartStop => self.on_hotkey(HotkeyEvent::Toggle),
            TrayCommand::SetHotkey(kind, hk) => {
                let key = match kind {
                    HotkeyKind::PushToTalk => "push_to_talk",
                    HotkeyKind::Toggle => "toggle",
                };
                self.set("hotkeys", key, Setting::Str(hk.to_string()));
            }
            TrayCommand::ResetHotkeys => {
                let d = HotkeyConfig::default();
                match config::update_settings(
                    &self.config_path,
                    vec![
                        ("hotkeys", "push_to_talk", Setting::Str(d.push_to_talk)),
                        ("hotkeys", "toggle", Setting::Str(d.toggle)),
                        ("hotkeys", "cancel", Setting::Str(d.cancel)),
                    ],
                ) {
                    Ok(cfg) => self.apply_config(cfg, self.vocab.clone()),
                    Err(e) => self.ui.notify("Cannot reset hotkeys", &format!("{e:#}"), Tone::Error),
                }
            }
            TrayCommand::SelectMicrophone(name) => self.set("audio", "device", Setting::Str(name)),
            TrayCommand::ToggleNoiseSuppression => {
                let v = !self.cfg.audio.noise_suppression;
                self.set("audio", "noise_suppression", Setting::Bool(v));
            }
            TrayCommand::SetGain(g) => self.set("audio", "gain_db", Setting::Float(g as f64)),
            TrayCommand::SelectModel(id) => self.set("asr", "model", Setting::Str(id)),
            TrayCommand::DownloadModel(id) => self.start_download(id),
            TrayCommand::CancelDownload => {
                if let Some((_, flag)) = &self.download {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            TrayCommand::SetModelsDir(path) => self.set("general", "models_dir", Setting::Str(path.display().to_string())),
            TrayCommand::SetLanguage(code) => self.set("asr", "language", Setting::Str(code)),
            TrayCommand::SetMode(mode) => {
                let v = match mode {
                    PostMode::Raw => "raw",
                    PostMode::Clean => "clean",
                    PostMode::Smart => "smart",
                };
                self.set("postprocess", "mode", Setting::Str(v.into()));
                if mode == PostMode::Smart {
                    self.ui.notify(
                        "Smart mode",
                        &format!(
                            "Smart mode rewrites text with the local model \"{}\" at {}. Start Ollama or LM Studio and load that model; if it is not reachable the Clean text is inserted.",
                            self.cfg.postprocess.smart.model, self.cfg.postprocess.smart.endpoint
                        ),
                        Tone::Info,
                    );
                }
            }
            TrayCommand::SetInsertMethod(m) => {
                let v = match m {
                    InsertMethod::Auto => "auto",
                    InsertMethod::Paste => "paste",
                    InsertMethod::Type => "type",
                };
                self.set("insertion", "method", Setting::Str(v.into()));
            }
            TrayCommand::ToggleOverlay => {
                let v = !self.cfg.overlay.enabled;
                self.set("overlay", "enabled", Setting::Bool(v));
            }
            TrayCommand::ToggleContinuous => {
                let v = !self.cfg.vad.continuous;
                self.set("vad", "continuous", Setting::Bool(v));
            }
            TrayCommand::ToggleAutostart => {
                let v = !self.ui.autostart_enabled();
                self.set("general", "autostart", Setting::Bool(v));
            }
            TrayCommand::OpenSettings => {
                if !self.config_path.exists() {
                    let _ = config::write_atomic(&self.config_path, config::DEFAULT_CONFIG_TOML.as_bytes());
                }
                let p = self.config_path.clone();
                self.ui.open_path(&p);
            }
            TrayCommand::EditVocabulary => {
                let p = vocab_path(&self.cfg, &self.config_path);
                if let Err(e) = Vocabulary::load_user_file(&p) {
                    log::warn!("{e:#}");
                }
                self.ui.open_path(&p);
            }
            TrayCommand::OpenLogs => {
                let p = crate::paths::logs_dir();
                self.ui.open_path(&p);
            }
            TrayCommand::OpenModelsFolder => {
                let p = self.cfg.models_dir();
                let _ = std::fs::create_dir_all(&p);
                self.ui.open_path(&p);
            }
            TrayCommand::Quit => {
                self.quitting = true;
                self.cancel_capture("Quitting");
                if let Some((_, flag)) = &self.download {
                    flag.store(true, Ordering::Relaxed);
                }
                self.ui.quit();
            }
            TrayCommand::UiChangeHotkey(_)
            | TrayCommand::UiChooseModelsFolder
            | TrayCommand::UiAbout
            | TrayCommand::UiConfirmDownload(_) => {
                log::debug!("UI-only command reached the controller: {cmd:?}");
            }
        }
    }

    fn start_download(&mut self, id: String) {
        if let Some((current, _)) = &self.download {
            self.ui.notify("Download in progress", &format!("{current} is still downloading."), Tone::Info);
            return;
        }
        let Some(info) = catalog::find(&id) else { return };
        let dir = self.cfg.models_dir();
        if download::is_installed(&dir, info) {
            return;
        }
        let flag = self.ports.download(id.clone(), dir);
        self.download = Some((id.clone(), flag));
        self.show(OverlayView::Message { text: format!("Downloading {}…", info.display), tone: Tone::Info }, Some(2500));
    }

    fn on_download(&mut self, d: DownloadEvent) {
        match d {
            DownloadEvent::Progress { percent, .. } => {
                self.download_percent = percent;
            }
            DownloadEvent::Done { model, result } => {
                self.download = None;
                self.download_percent = None;
                match result {
                    Ok(()) => {
                        let display = catalog::find(&model).map(|m| m.display).unwrap_or("Model");
                        self.ui.notify("Download complete", &format!("{display} is installed."), Tone::Success);
                        self.models.remove(&model);
                        self.pending_worker = Some(Self::worker_config(&self.cfg, &self.vocab));
                        self.sync_worker();
                        if model == "silero-vad" {
                            self.ports.audio(AudioCmd::Configure(Self::capture_settings(&self.cfg)));
                        } else if self.pending_worker.is_none() && (model == self.cfg.asr.model || model == self.cfg.asr.fallback_model) {
                            self.models.insert(model.clone(), ModelState::Loading);
                            self.ports.asr(WorkerMsg::Preload(model.clone()));
                        }
                        // Offer the voice activity detector after the first model.
                        let vad = catalog::find("silero-vad").expect("catalog has silero-vad");
                        if model != "silero-vad" && !download::is_installed(&self.cfg.models_dir(), vad) {
                            self.start_download("silero-vad".into());
                        }
                    }
                    Err(e) => {
                        self.ui.notify("Download failed", &format!("{model}: {e}"), Tone::Error);
                    }
                }
            }
        }
    }

    // ----- configuration -------------------------------------------------

    fn sync_worker(&mut self) {
        if self.capture.is_some() || !self.utterances.is_empty() {
            return;
        }
        let Some(cfg) = self.pending_worker.as_ref() else {
            return;
        };
        if self.ports.asr(WorkerMsg::Configure(cfg.clone())) {
            self.pending_worker = None;
            self.models.clear();
            if let Some(info) = catalog::find(&self.cfg.asr.model).filter(|info| download::is_installed(&self.cfg.models_dir(), info)) {
                self.models.insert(info.id.into(), ModelState::Loading);
                self.ports.asr(WorkerMsg::Preload(info.id.into()));
            }
        }
    }

    fn reload_config(&mut self) {
        let cfg = match Config::load_or_create(&self.config_path) {
            Ok(c) => c,
            Err(e) => {
                self.ui.notify("Settings not applied", &format!("{e:#}"), Tone::Error);
                return;
            }
        };
        let vocab = match Vocabulary::load_user_file(&vocab_path(&cfg, &self.config_path)) {
            Ok(v) => v,
            Err(e) => {
                self.ui.notify("Vocabulary not applied", &format!("{e:#}"), Tone::Error);
                self.vocab.clone()
            }
        };
        if cfg != self.cfg || vocab != self.vocab {
            log::info!("settings reloaded");
            self.apply_config(cfg, vocab);
        }
    }

    pub fn apply_config(&mut self, cfg: Config, vocab: Vec<(String, String)>) {
        let old = std::mem::replace(&mut self.cfg, cfg);
        let vocab_changed = vocab != self.vocab;
        self.vocab = vocab;
        crate::logging::set_level(self.cfg.log_level_filter());
        if old.hotkeys != self.cfg.hotkeys {
            if let Err(e) = self.ui.apply_hotkeys(&self.cfg.hotkeys) {
                self.ui.notify("Hotkey problem", &e, Tone::Warning);
            }
        }
        if Self::capture_settings(&old) != Self::capture_settings(&self.cfg) {
            self.ports.audio(AudioCmd::Configure(Self::capture_settings(&self.cfg)));
        }
        let new_worker = Self::worker_config(&self.cfg, &self.vocab);
        if Self::worker_config(&old, &self.vocab) != new_worker || vocab_changed {
            self.pending_worker = Some(new_worker);
            self.sync_worker();
        }
        if self.pending_worker.is_none() && (old.asr.model != self.cfg.asr.model || !self.models.contains_key(&self.cfg.asr.model)) {
            let dir = self.cfg.models_dir();
            if let Some(info) = catalog::find(&self.cfg.asr.model) {
                if download::is_installed(&dir, info) {
                    self.models.insert(info.id.into(), ModelState::Loading);
                    self.ports.asr(WorkerMsg::Preload(info.id.into()));
                }
            }
        }
        if old.postprocess != self.cfg.postprocess || vocab_changed {
            self.post = Arc::new(PostProcessor::new(&self.cfg.postprocess, self.vocab.clone()));
        }
        if self.cfg.general.autostart != self.ui.autostart_enabled() {
            if let Err(e) = self.ui.set_autostart(self.cfg.general.autostart) {
                self.ui.notify("Start with Windows", &e, Tone::Error);
            }
        }
        if old.overlay != self.cfg.overlay {
            self.ui.configure_overlay(&self.cfg.overlay);
        }
        if !self.cfg.overlay.enabled {
            self.ui.overlay(OverlayView::Hidden, None);
        }
    }

    // ----- status and overlay --------------------------------------------

    fn show(&mut self, view: OverlayView, hide_after_ms: Option<u32>) {
        let important = matches!(view, OverlayView::Message { tone: Tone::Error, .. } | OverlayView::Result { tone: Tone::Warning, .. });
        if self.cfg.overlay.enabled || important {
            self.ui.overlay(view, hide_after_ms);
        }
    }

    fn refresh_status(&mut self) {
        let dir = self.cfg.models_dir();
        let main_installed = catalog::find(&self.cfg.asr.model).map(|i| download::is_installed(&dir, i)).unwrap_or(false);
        let status = if self.capture.is_some() {
            TrayStatus::Listening
        } else if let Some((model, _)) = &self.download {
            TrayStatus::Downloading { model: model.clone(), percent: self.download_percent }
        } else if !self.utterances.is_empty() {
            TrayStatus::Processing
        } else if !main_installed {
            TrayStatus::NeedsModel
        } else if let Some(ModelState::Failed(e)) = self.models.get(&self.cfg.asr.model) {
            TrayStatus::Error(format!("model failed to load: {e}"))
        } else if let Some(ModelState::Loading) = self.models.get(&self.cfg.asr.model) {
            TrayStatus::Loading(self.cfg.asr.model.clone())
        } else {
            TrayStatus::Ready
        };
        if status != self.status {
            let tooltip = status.tooltip(&self.cfg);
            self.ui.set_status(&status, &tooltip);
            self.status = status;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::audio::capture::{CaptureError, CaptureErrorKind};
    use crate::audio::{EndReason, Snapshot};
    use crate::postprocess::PostResult;

    #[derive(Default)]
    struct Log {
        audio: Vec<String>,
        asr: Vec<WorkerMsg>,
        inserts: Vec<InsertRequest>,
        overlay: Vec<OverlayView>,
        notes: Vec<String>,
        smart: Vec<String>,
        listening: Vec<bool>,
        downloads: Vec<String>,
        asked: Vec<String>,
        reject_insert: bool,
        clipboard_requests: Vec<bool>,
    }

    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Log>>);

    struct FakeUi(Shared);
    struct FakePorts(Shared);

    impl Ui for FakeUi {
        fn set_status(&mut self, _s: &TrayStatus, _t: &str) {}
        fn overlay(&mut self, view: OverlayView, _h: Option<u32>) {
            self.0 .0.borrow_mut().overlay.push(view);
        }
        fn notify(&mut self, title: &str, body: &str, _tone: Tone) {
            self.0 .0.borrow_mut().notes.push(format!("{title}: {body}"));
        }
        fn set_listening(&mut self, active: bool) {
            self.0 .0.borrow_mut().listening.push(active);
        }
        fn apply_hotkeys(&mut self, _cfg: &HotkeyConfig) -> Result<(), String> {
            Ok(())
        }
        fn foreground_window(&self) -> Option<WindowHandle> {
            Some(77)
        }
        fn open_path(&mut self, _p: &Path) {}
        fn set_autostart(&mut self, _on: bool) -> Result<(), String> {
            Ok(())
        }
        fn autostart_enabled(&self) -> bool {
            false
        }
        fn ask_download(&mut self, id: &str, _reason: &str) {
            self.0 .0.borrow_mut().asked.push(id.into());
        }
        fn quit(&mut self) {}
    }

    impl Ports for FakePorts {
        fn audio(&self, cmd: AudioCmd) {
            let s = match cmd {
                AudioCmd::Start(p) => format!("start {} {:?}", p.session, p.mode),
                AudioCmd::Finish { session, tail_ms } => format!("finish {session} {tail_ms}"),
                AudioCmd::Cancel { session } => format!("cancel {session}"),
                AudioCmd::Configure(_) => "configure".into(),
                AudioCmd::ReleaseDevice => "release".into(),
                AudioCmd::RefreshDevices => "devices".into(),
                AudioCmd::Shutdown => "shutdown".into(),
            };
            self.0 .0.borrow_mut().audio.push(s);
        }
        fn asr(&self, msg: WorkerMsg) -> bool {
            self.0 .0.borrow_mut().asr.push(msg);
            true
        }
        fn insert(&self, req: InsertRequest, clipboard_only: bool) -> Result<(), String> {
            if self.0 .0.borrow().reject_insert {
                return Err("insertion is unavailable".into());
            }
            self.0 .0.borrow_mut().inserts.push(req);
            self.0 .0.borrow_mut().clipboard_requests.push(clipboard_only);
            Ok(())
        }
        fn smart(&self, _s: u64, _u: u32, raw: String, _p: Arc<PostProcessor>, _l: Option<String>, _e: bool) {
            self.0 .0.borrow_mut().smart.push(raw);
        }
        fn download(&self, model: String, _dir: PathBuf) -> Arc<AtomicBool> {
            self.0 .0.borrow_mut().downloads.push(model);
            Arc::new(AtomicBool::new(false))
        }
    }

    /// Controller with a fake "installed" main model.
    fn setup(mode: PostMode) -> (Controller<FakeUi, FakePorts>, Shared, PathBuf) {
        let dir = crate::paths::test_dir("controller");
        let models = dir.join("models");
        let info = catalog::find("parakeet-tdt-ctc-110m-en").unwrap();
        let mdir = download::model_dir(&models, info);
        std::fs::create_dir_all(&mdir).unwrap();
        for f in info.files {
            let file = std::fs::File::create(mdir.join(f.name)).unwrap();
            file.set_len(f.size).unwrap();
        }
        let mut cfg = Config::default();
        cfg.general.models_dir = models.display().to_string();
        cfg.asr.model = info.id.into();
        cfg.asr.language = "en".into();
        cfg.postprocess.mode = mode;
        let path = dir.join("config.toml");
        let text = toml::to_string(&cfg).unwrap();
        std::fs::write(&path, text).unwrap();
        let shared = Shared::default();
        let c = Controller::new(FakeUi(shared.clone()), FakePorts(shared.clone()), cfg, path, vec![]);
        (c, shared, dir)
    }

    fn snap(session: u64, utterance: u32, window: u32, end: usize, kind: SnapshotKind) -> AppEvent {
        AppEvent::Audio(AudioEvent::Seg {
            session,
            event: SegEvent::Snapshot(Snapshot {
                key: SnapKey { utterance, window, end },
                kind,
                audio: Arc::from(vec![0.0f32; 4]),
                context: 0,
            }),
        })
    }

    fn seg(session: u64, event: SegEvent) -> AppEvent {
        AppEvent::Audio(AudioEvent::Seg { session, event })
    }

    fn output(session: u64, utterance: u32, window: u32, end: usize, kind: SnapshotKind, text: &str) -> AppEvent {
        AppEvent::Asr(WorkerEvent::Output(crate::asr::worker::AsrOutput {
            session,
            key: SnapKey { utterance, window, end },
            kind,
            model: "m".into(),
            result: Ok(text.into()),
            queue_ms: 0,
            compute_ms: 10,
            audio_ms: 1000,
        }))
    }

    fn jobs(log: &Shared) -> Vec<(u64, SnapKey, SnapshotKind)> {
        log.0
            .borrow()
            .asr
            .iter()
            .filter_map(|m| match m {
                WorkerMsg::Job(j) => Some((j.session, j.key, j.kind)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn push_to_talk_flow_inserts_cleaned_text() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        c.startup();
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        assert_eq!(log.0.borrow().audio.last().unwrap(), "start 1 PushToTalk");
        assert!(c.is_listening());
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 16000, SnapshotKind::Pause));
        assert_eq!(jobs(&log).len(), 1);
        std::thread::sleep(std::time::Duration::from_millis(200));
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttUp));
        assert_eq!(log.0.borrow().audio.last().unwrap(), "finish 1 150");
        // The recogniser finishes the speculative snapshot...
        c.handle(output(1, 0, 0, 16000, SnapshotKind::Pause, "um, hello the the world"));
        assert!(log.0.borrow().inserts.is_empty(), "not inserted before the utterance ends");
        // ...and the utterance end reuses it: insertion is immediate.
        c.handle(seg(
            1,
            SegEvent::UtteranceEnd { utterance: 0, last: Some(SnapKey { utterance: 0, window: 0, end: 16000 }), reason: EndReason::Finish },
        ));
        let inserts = log.0.borrow().inserts.clone();
        assert_eq!(inserts.len(), 1);
        assert_eq!(inserts[0].text, "Hello the world");
        assert_eq!(inserts[0].original, Some(77));
        c.handle(AppEvent::Audio(AudioEvent::Ended { session: 1 }));
        assert!(!c.is_listening());
        assert_eq!(log.0.borrow().listening, vec![true, false]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn short_tap_without_speech_is_ignored() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttUp));
        assert_eq!(log.0.borrow().audio.last().unwrap(), "cancel 1");
        assert!(!c.is_listening());
        assert!(log.0.borrow().inserts.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn newline_commands_reach_insertion_and_failed_handoffs_retain_text() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        log.0.borrow_mut().reject_insert = true;
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 1000, SnapshotKind::Pause));
        c.handle(output(1, 0, 0, 1000, SnapshotKind::Pause, "new paragraph"));
        c.handle(seg(
            1,
            SegEvent::UtteranceEnd {
                utterance: 0,
                last: Some(SnapKey { utterance: 0, window: 0, end: 1000 }),
                reason: EndReason::Endpoint,
            },
        ));
        assert!(log.0.borrow().inserts.is_empty());
        assert_eq!(c.utterances.len(), 1, "a failed handoff must retain the output");
        assert!(c.insertion_stalled);
        let mut cfg = c.config().clone();
        cfg.asr.threads = 2;
        c.apply_config(cfg, vec![]);
        c.handle(AppEvent::Audio(AudioEvent::Ended { session: 1 }));
        assert!(c.pending_worker.is_some());
        log.0.borrow_mut().reject_insert = false;
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        assert_eq!(log.0.borrow().inserts[0].text, "\n\n");
        assert!(log.0.borrow().clipboard_requests[0], "deliberate retries recover through the clipboard");
        assert_eq!(c.utterances.len(), 1, "output stays retained until acknowledgement");
        c.handle(AppEvent::Inserted(InsertReport { id: 2, outcome: InsertOutcome::Failed("clipboard busy".into()), elapsed_ms: 0 }));
        assert_eq!(c.utterances.len(), 1, "failed clipboard recovery must also retain the output");
        assert!(c.insertion_stalled);
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        assert!(log.0.borrow().clipboard_requests[1]);
        c.handle(AppEvent::Inserted(InsertReport {
            id: 3,
            outcome: InsertOutcome::Clipboard { reason: "recovered".into() },
            elapsed_ms: 0,
        }));
        assert!(c.utterances.is_empty());
        assert!(c.pending_worker.is_none(), "successful recovery must unblock deferred settings");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn worker_settings_wait_until_current_dictation_is_acknowledged() {
        let (mut c, log, dir) = setup(PostMode::Raw);
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 1000, SnapshotKind::Pause));
        let mut cfg = c.config().clone();
        cfg.general.models_dir = dir.join("new-models").display().to_string();
        c.apply_config(cfg, vec![]);
        assert!(c.pending_worker.is_some());
        assert!(!log.0.borrow().asr.iter().any(|m| matches!(m, WorkerMsg::Configure(_))));
        c.handle(output(1, 0, 0, 1000, SnapshotKind::Pause, "kept from the original model"));
        c.handle(seg(
            1,
            SegEvent::UtteranceEnd {
                utterance: 0,
                last: Some(SnapKey { utterance: 0, window: 0, end: 1000 }),
                reason: EndReason::Endpoint,
            },
        ));
        c.handle(AppEvent::Audio(AudioEvent::Ended { session: 1 }));
        assert!(c.pending_worker.is_some());
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        assert!(!c.is_listening(), "new sessions must wait for deferred settings");
        c.handle(AppEvent::Inserted(InsertReport {
            id: 1,
            outcome: InsertOutcome::Inserted { method: "paste", app: "test.exe".into() },
            elapsed_ms: 0,
        }));
        assert!(c.pending_worker.is_none());
        assert!(log.0.borrow().asr.iter().any(|m| matches!(m, WorkerMsg::Configure(cfg) if cfg.models_dir == dir.join("new-models"))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clipboard_results_wait_for_deliberate_recovery_before_replacing_each_other() {
        let (mut c, log, dir) = setup(PostMode::Raw);
        c.cfg.vad.continuous = true;
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        for (utterance, text) in [(0, "one"), (1, "two")] {
            c.handle(seg(1, SegEvent::SpeechStart { utterance }));
            c.handle(snap(1, utterance, 0, 1000, SnapshotKind::Pause));
            c.handle(output(1, utterance, 0, 1000, SnapshotKind::Pause, text));
            c.handle(seg(
                1,
                SegEvent::UtteranceEnd { utterance, last: Some(SnapKey { utterance, window: 0, end: 1000 }), reason: EndReason::Endpoint },
            ));
        }
        c.handle(AppEvent::Inserted(InsertReport {
            id: 1,
            outcome: InsertOutcome::Clipboard { reason: "no target".into() },
            elapsed_ms: 0,
        }));
        assert_eq!(log.0.borrow().inserts.len(), 1, "a later result must not overwrite the recoverable clipboard");
        assert_eq!(c.utterances.len(), 1);
        c.handle(AppEvent::Audio(AudioEvent::Ended { session: 1 }));
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        assert_eq!(log.0.borrow().inserts[1].text, "two");
        assert!(log.0.borrow().clipboard_requests[1]);
        c.handle(AppEvent::Inserted(InsertReport {
            id: 2,
            outcome: InsertOutcome::Clipboard { reason: "recovered".into() },
            elapsed_ms: 0,
        }));
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        assert!(c.is_listening(), "after recovery, the shortcut starts new dictation normally");
        assert_eq!(c.retry_clipboard, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn late_failure_for_cancelled_output_does_not_affect_new_dictation() {
        let (mut c, log, dir) = setup(PostMode::Raw);
        c.cfg.vad.continuous = true;
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 1000, SnapshotKind::Pause));
        c.handle(output(1, 0, 0, 1000, SnapshotKind::Pause, "cancelled"));
        c.handle(seg(
            1,
            SegEvent::UtteranceEnd {
                utterance: 0,
                last: Some(SnapKey { utterance: 0, window: 0, end: 1000 }),
                reason: EndReason::Endpoint,
            },
        ));
        c.handle(AppEvent::Hotkey(HotkeyEvent::Cancel));
        c.handle(AppEvent::Inserted(InsertReport { id: 1, outcome: InsertOutcome::Failed("late failure".into()), elapsed_ms: 0 }));
        assert!(!c.insertion_stalled);
        assert_eq!(c.retry_clipboard, None);
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        assert!(c.is_listening());
        c.handle(seg(2, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(2, 0, 0, 1000, SnapshotKind::Pause));
        c.handle(output(2, 0, 0, 1000, SnapshotKind::Pause, "new words"));
        c.handle(seg(
            2,
            SegEvent::UtteranceEnd {
                utterance: 0,
                last: Some(SnapKey { utterance: 0, window: 0, end: 1000 }),
                reason: EndReason::Endpoint,
            },
        ));
        assert!(!log.0.borrow().clipboard_requests[1]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_dictation_joins_committed_windows_in_order() {
        let (mut c, log, dir) = setup(PostMode::Raw);
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 200_000, SnapshotKind::Pause));
        c.handle(seg(1, SegEvent::Commit { key: SnapKey { utterance: 0, window: 0, end: 200_000 } }));
        c.handle(snap(1, 0, 1, 50_000, SnapshotKind::Pause));
        c.handle(seg(
            1,
            SegEvent::UtteranceEnd {
                utterance: 0,
                last: Some(SnapKey { utterance: 0, window: 1, end: 50_000 }),
                reason: EndReason::Endpoint,
            },
        ));
        // Results arrive out of order: the second window first.
        c.handle(output(1, 0, 1, 50_000, SnapshotKind::Pause, "second part."));
        assert!(log.0.borrow().inserts.is_empty());
        c.handle(output(1, 0, 0, 200_000, SnapshotKind::Pause, "First part,"));
        assert_eq!(log.0.borrow().inserts[0].text, "First part, second part.");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn utterances_are_inserted_in_order_even_if_finished_out_of_order() {
        let (mut c, log, dir) = setup(PostMode::Raw);
        c.cfg.vad.continuous = true;
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        assert_eq!(log.0.borrow().audio.last().unwrap(), "start 1 Toggle { continuous: true }");
        for u in 0..2 {
            c.handle(seg(1, SegEvent::SpeechStart { utterance: u }));
            c.handle(snap(1, u, 0, 1000, SnapshotKind::Pause));
            c.handle(seg(
                1,
                SegEvent::UtteranceEnd {
                    utterance: u,
                    last: Some(SnapKey { utterance: u, window: 0, end: 1000 }),
                    reason: EndReason::Endpoint,
                },
            ));
        }
        c.handle(output(1, 1, 0, 1000, SnapshotKind::Pause, "two"));
        assert!(log.0.borrow().inserts.is_empty());
        c.handle(output(1, 0, 0, 1000, SnapshotKind::Pause, "one"));
        assert_eq!(log.0.borrow().inserts.len(), 1, "only one insertion may be outstanding");
        c.handle(AppEvent::Inserted(InsertReport {
            id: 1,
            outcome: InsertOutcome::Inserted { method: "paste", app: "test.exe".into() },
            elapsed_ms: 0,
        }));
        let texts: Vec<String> = log.0.borrow().inserts.iter().map(|r| r.text.clone()).collect();
        assert_eq!(texts, vec!["one", "two"]);
        assert!(c.is_listening(), "continuous mode keeps listening");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancel_discards_everything() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 1000, SnapshotKind::Pause));
        c.handle(AppEvent::Hotkey(HotkeyEvent::Cancel));
        assert_eq!(log.0.borrow().audio.last().unwrap(), "cancel 1");
        assert!(log.0.borrow().asr.iter().any(|m| matches!(m, WorkerMsg::CancelSession(1))));
        // Late results are ignored.
        c.handle(output(1, 0, 0, 1000, SnapshotKind::Pause, "should not appear"));
        c.handle(seg(1, SegEvent::UtteranceEnd { utterance: 0, last: None, reason: EndReason::Finish }));
        assert!(log.0.borrow().inserts.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn toggle_twice_stops_listening() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        assert_eq!(log.0.borrow().audio.last().unwrap(), "finish 1 0");
        // Push-to-talk is ignored while a toggle session runs.
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        assert_eq!(log.0.borrow().audio.iter().filter(|a| a.starts_with("start")).count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn smart_mode_runs_off_thread_then_inserts() {
        let (mut c, log, dir) = setup(PostMode::Smart);
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 1000, SnapshotKind::Pause));
        c.handle(seg(
            1,
            SegEvent::UtteranceEnd {
                utterance: 0,
                last: Some(SnapKey { utterance: 0, window: 0, end: 1000 }),
                reason: EndReason::Endpoint,
            },
        ));
        c.handle(output(1, 0, 0, 1000, SnapshotKind::Pause, "so we ship it"));
        assert_eq!(log.0.borrow().smart, vec!["so we ship it".to_string()]);
        assert!(log.0.borrow().inserts.is_empty());
        c.handle(AppEvent::Postprocessed {
            session: 1,
            utterance: 0,
            result: PostResult { text: "So we ship it.".into(), mode: PostMode::Smart, note: None, clean_us: 1, smart_ms: 300 },
        });
        assert_eq!(log.0.borrow().inserts[0].text, "So we ship it.");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn transcription_errors_are_reported_not_inserted() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        c.handle(AppEvent::Hotkey(HotkeyEvent::Toggle));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(seg(
            1,
            SegEvent::UtteranceEnd { utterance: 0, last: Some(SnapKey { utterance: 0, window: 0, end: 10 }), reason: EndReason::Endpoint },
        ));
        c.handle(AppEvent::Asr(WorkerEvent::Output(crate::asr::worker::AsrOutput {
            session: 1,
            key: SnapKey { utterance: 0, window: 0, end: 10 },
            kind: SnapshotKind::Final,
            model: "m".into(),
            result: Err("boom".into()),
            queue_ms: 0,
            compute_ms: 0,
            audio_ms: 0,
        })));
        assert!(log.0.borrow().inserts.is_empty());
        assert!(log.0.borrow().notes.iter().any(|n| n.contains("boom")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_model_asks_to_download_instead_of_recording() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        c.cfg.asr.language = "bn".into();
        c.cfg.asr.fallback_model = "whisper-base".into();
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        assert!(!c.is_listening());
        assert_eq!(log.0.borrow().asked, vec!["whisper-base".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn partial_results_update_the_overlay() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        c.handle(AppEvent::Hotkey(HotkeyEvent::PttDown));
        c.handle(seg(1, SegEvent::SpeechStart { utterance: 0 }));
        c.handle(snap(1, 0, 0, 8000, SnapshotKind::Partial));
        c.handle(output(1, 0, 0, 8000, SnapshotKind::Partial, "hello wor"));
        let last = log.0.borrow().overlay.last().cloned().unwrap();
        assert!(matches!(last, OverlayView::Listening { ref text, .. } if text == "hello wor"), "{last:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn microphone_problem_popup_fires_once_per_distinct_error() {
        let (mut c, log, dir) = setup(PostMode::Clean);
        let fail =
            |error: &str| AppEvent::Audio(AudioEvent::Failed { session: None, error: CaptureError::new(CaptureErrorKind::Other, error) });
        c.handle(fail("Microphone error: A buffer underrun or overrun occurred."));
        c.handle(fail("Microphone error: A buffer underrun or overrun occurred."));
        let notes = log.0.borrow().notes.clone();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("Microphone problem: "), "{}", notes[0]);
        assert_eq!(log.0.borrow().overlay.len(), 2, "the overlay still shows every occurrence");
        // A different failure is still announced.
        c.handle(fail("The audio device changed."));
        assert_eq!(log.0.borrow().notes.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
