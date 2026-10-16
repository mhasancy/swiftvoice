//! The audio thread: owns the microphone stream and the frame processor.
//!
//! Commands come from the controller (start, finish, cancel, configure);
//! events go back through a sink. The microphone is opened when a session
//! starts and closed `keep_warm_secs` after it ends. While warm, the most
//! recent `pre_roll_ms` of audio is kept in memory (never written anywhere)
//! so speech that starts a moment before the hotkey is not lost.

use std::collections::VecDeque;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use super::capture::{self, CaptureError, CaptureErrorKind, CaptureStream};
use super::processor::{make_vad, FrameProcessor, VadChoice};
use super::segmenter::{SegEvent, SegmenterConfig};
use super::vad::{Vad, FRAME_MS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    PushToTalk,
    Toggle { continuous: bool },
}

#[derive(Debug, Clone)]
pub struct SessionParams {
    pub session: u64,
    pub mode: SessionMode,
    pub seg: SegmenterConfig,
    /// When the hotkey was pressed, for the hotkey-to-audio latency metric.
    pub requested_at: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaptureSettings {
    pub device: String,
    pub gain_db: f32,
    pub noise_suppression: bool,
    pub keep_warm_secs: u32,
    pub pre_roll_ms: u32,
    pub vad: VadChoice,
}

pub enum AudioCmd {
    Start(SessionParams),
    /// End the utterance; keep recording up to `tail_ms` if mid-word.
    Finish {
        session: u64,
        tail_ms: u32,
    },
    Cancel {
        session: u64,
    },
    Configure(CaptureSettings),
    /// Close the device (sleep/resume, device change) and reopen on demand.
    ReleaseDevice,
    RefreshDevices,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// First live audio of a session arrived.
    Started {
        session: u64,
        device: String,
        sample_rate: u32,
        start_latency_ms: u32,
        vad: &'static str,
    },
    Level {
        session: u64,
        level: f32,
    },
    Seg {
        session: u64,
        event: SegEvent,
    },
    /// No more events will arrive for this session.
    Ended {
        session: u64,
    },
    Warning(String),
    Failed {
        session: Option<u64>,
        error: CaptureError,
    },
    Devices(Result<Vec<String>, String>),
}

pub type AudioSink = Box<dyn Fn(AudioEvent) + Send + 'static>;

pub struct AudioHandle {
    pub tx: Sender<AudioCmd>,
    join: Option<JoinHandle<()>>,
}

impl AudioHandle {
    pub fn send(&self, cmd: AudioCmd) {
        if self.tx.send(cmd).is_err() {
            log::error!("audio thread is not running");
        }
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(AudioCmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn spawn(settings: CaptureSettings, sink: AudioSink) -> std::io::Result<AudioHandle> {
    let (tx, rx) = crossbeam_channel::bounded(64);
    let join = std::thread::Builder::new().name("swiftvoice-audio".into()).spawn(move || {
        let mut p = Pipeline::new(settings, sink);
        p.run(rx);
    })?;
    Ok(AudioHandle { tx, join: Some(join) })
}

struct Active {
    session: u64,
    mode: SessionMode,
    requested_at: Instant,
    started_reported: bool,
    silence_warned: bool,
    /// Remaining release-tail frames once a finish was requested.
    finishing: Option<u64>,
    finish_from_frame: u64,
}

struct Pipeline {
    settings: CaptureSettings,
    pending_settings: Option<CaptureSettings>,
    sink: AudioSink,
    stream: Option<CaptureStream>,
    processor: Option<FrameProcessor>,
    spare_vad: Option<Box<dyn Vad>>,
    vad_warning_sent: bool,
    active: Option<Active>,
    warm_until: Option<Instant>,
    preroll: VecDeque<f32>,
    buf: Vec<f32>,
    events: Vec<SegEvent>,
    last_level: Instant,
    dropped_reported: u64,
    xrun_warned_at: Option<Instant>,
}

impl Pipeline {
    fn new(settings: CaptureSettings, sink: AudioSink) -> Pipeline {
        Pipeline {
            settings,
            pending_settings: None,
            sink,
            stream: None,
            processor: None,
            spare_vad: None,
            vad_warning_sent: false,
            active: None,
            warm_until: None,
            preroll: VecDeque::new(),
            buf: vec![0.0; 4800],
            events: Vec::with_capacity(16),
            last_level: Instant::now(),
            dropped_reported: 0,
            xrun_warned_at: None,
        }
    }

    fn run(&mut self, rx: Receiver<AudioCmd>) {
        // Load the VAD up front so the first dictation does not pay for it.
        self.ensure_vad();
        loop {
            let cmd = if self.stream.is_some() {
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            };
            if let Some(cmd) = cmd {
                if !self.handle(cmd) {
                    return;
                }
                while let Ok(cmd) = rx.try_recv() {
                    if !self.handle(cmd) {
                        return;
                    }
                }
            }
            self.pump();
            if let Some(deadline) = self.warm_until {
                if self.active.is_none() && Instant::now() >= deadline {
                    self.close_stream("keep-warm period ended");
                }
            }
        }
    }

    /// Returns false on shutdown.
    fn handle(&mut self, cmd: AudioCmd) -> bool {
        match cmd {
            AudioCmd::Start(params) => self.start(params),
            AudioCmd::Finish { session, tail_ms } => {
                let needs_tail = self.processor.as_ref().map(|p| p.segmenter().needs_tail()).unwrap_or(false);
                let frames_now = self.processor.as_ref().map(|p| p.frames()).unwrap_or(0);
                if let Some(a) = self.active.as_mut().filter(|a| a.session == session) {
                    if needs_tail && tail_ms > 0 {
                        a.finishing = Some((tail_ms / FRAME_MS).max(1) as u64);
                        a.finish_from_frame = frames_now;
                    } else {
                        self.finish_now();
                    }
                }
            }
            AudioCmd::Cancel { session } => {
                if self.active.as_ref().map(|a| a.session) == Some(session) {
                    self.end_session();
                }
            }
            AudioCmd::Configure(s) => {
                if self.active.is_some() {
                    self.pending_settings = Some(s);
                } else {
                    self.apply_settings(s);
                }
            }
            AudioCmd::ReleaseDevice => {
                if let Some(a) = self.active.as_ref() {
                    let session = a.session;
                    (self.sink)(AudioEvent::Failed {
                        session: Some(session),
                        error: CaptureError::new(CaptureErrorKind::DeviceLost, "The audio device changed."),
                    });
                    self.end_session();
                }
                self.close_stream("device released");
            }
            AudioCmd::RefreshDevices => {
                let list = capture::list_input_devices().map_err(|e| e.to_string());
                (self.sink)(AudioEvent::Devices(list));
            }
            AudioCmd::Shutdown => {
                self.close_stream("shutdown");
                return false;
            }
        }
        true
    }

    fn apply_settings(&mut self, s: CaptureSettings) {
        let device_changed = s.device != self.settings.device;
        let dsp_changed = s.gain_db != self.settings.gain_db || s.noise_suppression != self.settings.noise_suppression;
        let vad_changed = s.vad != self.settings.vad;
        self.settings = s;
        if vad_changed {
            self.processor = None;
            self.spare_vad = None;
            self.vad_warning_sent = false;
            self.ensure_vad();
        } else if dsp_changed {
            if let Some(p) = self.processor.take() {
                self.spare_vad = Some(p.into_vad());
            }
        }
        if device_changed {
            self.close_stream("microphone changed");
        }
    }

    fn ensure_vad(&mut self) {
        if self.spare_vad.is_none() && self.processor.is_none() {
            let (vad, warning) = make_vad(&self.settings.vad);
            if let Some(w) = warning {
                if !self.vad_warning_sent {
                    log::warn!("{w}");
                    (self.sink)(AudioEvent::Warning(w));
                    self.vad_warning_sent = true;
                }
            }
            self.spare_vad = Some(vad);
        }
    }

    fn start(&mut self, params: SessionParams) {
        if self.active.is_some() {
            self.end_session();
        }
        if self.stream.is_none() {
            match capture::open(&self.settings.device) {
                Ok((stream, warning)) => {
                    if let Some(w) = warning {
                        (self.sink)(AudioEvent::Warning(w));
                    }
                    let rate_changed = self.processor.as_ref().map(|p| p.input_rate() != stream.sample_rate).unwrap_or(false);
                    if rate_changed {
                        if let Some(p) = self.processor.take() {
                            self.spare_vad = Some(p.into_vad());
                        }
                    }
                    self.dropped_reported = stream.consumer.dropped();
                    self.preroll.clear();
                    self.stream = Some(stream);
                }
                Err(error) => {
                    log::error!("cannot open microphone: {error}");
                    (self.sink)(AudioEvent::Failed { session: Some(params.session), error });
                    (self.sink)(AudioEvent::Ended { session: params.session });
                    return;
                }
            }
        }
        let rate = self.stream.as_ref().map(|s| s.sample_rate).unwrap_or(48_000);
        if self.processor.is_none() {
            self.ensure_vad();
            let vad = self.spare_vad.take().expect("ensure_vad provides a VAD");
            self.processor =
                Some(FrameProcessor::new(rate, self.settings.gain_db, self.settings.noise_suppression, vad, params.seg.clone()));
        }
        let processor = self.processor.as_mut().expect("processor exists");
        processor.start_session(params.seg.clone());
        // Replay the warm pre-roll so speech just before the hotkey counts.
        if !self.preroll.is_empty() {
            let pre: Vec<f32> = self.preroll.drain(..).collect();
            self.events.clear();
            if let Err(e) = processor.push(&pre, &mut self.events) {
                log::warn!("pre-roll processing failed: {e:#}");
            }
        }
        self.warm_until = None;
        self.active = Some(Active {
            session: params.session,
            mode: params.mode,
            requested_at: params.requested_at,
            started_reported: false,
            silence_warned: false,
            finishing: None,
            finish_from_frame: 0,
        });
        self.last_level = Instant::now();
        self.forward_events();
    }

    /// Drain the ring buffer and run the processor.
    fn pump(&mut self) {
        let Some(stream) = self.stream.as_mut() else { return };
        if let Ok(err) = stream.errors.try_recv() {
            if err.kind == CaptureErrorKind::Xrun {
                // The driver dropped or duplicated samples (common while a call
                // app shares the microphone). The stream keeps running, so log
                // the glitch and carry on instead of ending the session.
                note_xrun(&mut self.xrun_warned_at, &err);
            } else {
                log::error!("microphone stream error: {err}");
                let session = self.active.as_ref().map(|a| a.session);
                (self.sink)(AudioEvent::Failed { session, error: err });
                if self.active.is_some() {
                    self.end_session();
                }
                self.close_stream("stream error");
                return;
            }
        }
        let dropped = stream.consumer.dropped();
        if dropped > self.dropped_reported {
            let n = dropped - self.dropped_reported;
            self.dropped_reported = dropped;
            log::warn!("audio overrun: {n} samples dropped (the audio thread was starved)");
        }
        let rate = stream.sample_rate as usize;
        loop {
            let stream = self.stream.as_mut().expect("checked above");
            let n = stream.consumer.pop_slice(&mut self.buf);
            if n == 0 {
                break;
            }
            match (self.active.as_mut(), self.processor.as_mut()) {
                (Some(active), Some(processor)) => {
                    if !active.started_reported {
                        active.started_reported = true;
                        let ms = active.requested_at.elapsed().as_millis().min(u32::MAX as u128) as u32;
                        log::info!("hotkey -> first audio: {ms} ms");
                        (self.sink)(AudioEvent::Started {
                            session: active.session,
                            device: stream.device_name.clone(),
                            sample_rate: stream.sample_rate,
                            start_latency_ms: ms,
                            vad: processor.vad_name(),
                        });
                    }
                    self.events.clear();
                    if let Err(e) = processor.push(&self.buf[..n], &mut self.events) {
                        log::error!("audio processing failed: {e:#}");
                        let session = active.session;
                        (self.sink)(AudioEvent::Failed {
                            session: Some(session),
                            error: CaptureError::new(CaptureErrorKind::Other, format!("Audio processing failed: {e:#}.")),
                        });
                        self.end_session();
                        return;
                    }
                    self.forward_events();
                    if self.active.is_none() {
                        continue;
                    }
                }
                _ => {
                    // Warm but idle: keep a short rolling pre-roll only.
                    let cap = rate * self.settings.pre_roll_ms as usize / 1000;
                    self.preroll.extend(&self.buf[..n]);
                    while self.preroll.len() > cap {
                        self.preroll.pop_front();
                    }
                }
            }
        }
        self.after_pump();
    }

    fn after_pump(&mut self) {
        let (Some(active), Some(processor)) = (self.active.as_mut(), self.processor.as_mut()) else { return };
        // Silent-microphone check after ~1.5 s.
        if !active.silence_warned && processor.frames() >= 47 && processor.all_silent_so_far() {
            active.silence_warned = true;
            (self.sink)(AudioEvent::Warning(
                "The microphone is sending pure silence. It may be muted, or Windows privacy settings may block microphone access for desktop apps.".into(),
            ));
        }
        if self.last_level.elapsed() >= Duration::from_millis(80) {
            self.last_level = Instant::now();
            if let Some(level) = processor.take_level() {
                (self.sink)(AudioEvent::Level { session: active.session, level });
            }
        }
        if let Some(tail) = active.finishing {
            let elapsed = processor.frames().saturating_sub(active.finish_from_frame);
            if elapsed >= tail || !processor.segmenter().needs_tail() {
                self.finish_now();
            }
        }
    }

    fn finish_now(&mut self) {
        if let (Some(_), Some(processor)) = (self.active.as_ref(), self.processor.as_mut()) {
            self.events.clear();
            processor.finish(&mut self.events);
            self.forward_events();
            if self.active.is_some() {
                self.end_session();
            }
        }
    }

    /// Send segmenter events and end the session when they say so.
    fn forward_events(&mut self) {
        let Some(active) = self.active.as_ref() else { return };
        let session = active.session;
        let mode = active.mode;
        let mut end = false;
        for ev in self.events.drain(..) {
            match &ev {
                SegEvent::UtteranceEnd { .. } => {
                    if mode != (SessionMode::Toggle { continuous: true }) {
                        end = true;
                    }
                }
                SegEvent::NoSpeech | SegEvent::IdleTimeout => end = true,
                _ => {}
            }
            (self.sink)(AudioEvent::Seg { session, event: ev });
        }
        if end {
            self.end_session();
        }
    }

    fn end_session(&mut self) {
        if let Some(active) = self.active.take() {
            (self.sink)(AudioEvent::Ended { session: active.session });
        }
        if let Some(s) = self.pending_settings.take() {
            self.apply_settings(s);
        }
        if self.settings.keep_warm_secs == 0 {
            self.close_stream("dictation ended");
        } else if self.stream.is_some() {
            self.warm_until = Some(Instant::now() + Duration::from_secs(self.settings.keep_warm_secs as u64));
        }
    }

    fn close_stream(&mut self, why: &str) {
        if let Some(s) = self.stream.take() {
            log::info!("microphone \"{}\" closed ({why})", s.device_name);
        }
        self.warm_until = None;
        self.preroll.clear();
    }
}

/// Buffer glitches (xruns) are not fatal: cpal reports one when the driver
/// dropped or duplicated samples, but the stream keeps running. Log the first
/// glitch in a while and keep recording.
fn note_xrun(warned_at: &mut Option<Instant>, err: &CaptureError) {
    let now = Instant::now();
    let recent = warned_at.is_some_and(|t| now.duration_since(t) < Duration::from_secs(10));
    *warned_at = Some(now);
    if recent {
        log::debug!("microphone glitch: {}", err.message);
    } else {
        log::warn!("microphone glitch (recording continues): {}", err.message);
    }
}
