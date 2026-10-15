//! The recogniser thread.
//!
//! Owns the loaded engines (models stay resident between dictations) and
//! processes snapshot jobs with these rules:
//!
//! * Pause/Final jobs for the same `(session, utterance, window)` supersede
//!   each other: only the longest one is still needed, so older queued ones
//!   are dropped unprocessed.
//! * Partial jobs (overlay preview) run only when nothing else is queued,
//!   and only the newest partial per session is kept.
//! * Jobs of a cancelled session are discarded.
//!
//! The job channel is bounded; the audio side never blocks on it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use super::catalog;
use super::SpeechEngine;
use crate::audio::{SnapKey, SnapshotKind};

#[derive(Debug, Clone)]
pub struct AsrJob {
    pub session: u64,
    pub key: SnapKey,
    pub kind: SnapshotKind,
    /// `context` samples of the previous window, then the new audio.
    pub audio: Arc<[f32]>,
    pub context: usize,
    pub model: String,
    pub language: Option<String>,
    pub enqueued: Instant,
}

#[derive(Debug, Clone)]
pub struct AsrOutput {
    pub session: u64,
    pub key: SnapKey,
    pub kind: SnapshotKind,
    pub model: String,
    pub result: Result<String, String>,
    pub queue_ms: u32,
    pub compute_ms: u32,
    pub audio_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerConfig {
    pub models_dir: PathBuf,
    pub threads: u32,
    /// See `asr.low_memory`.
    pub low_memory: bool,
    pub unload_after_idle_secs: u32,
    pub warm_up: bool,
    /// Vocabulary hint passed to Whisper as its initial prompt.
    pub whisper_prompt: String,
}

pub enum WorkerMsg {
    Job(AsrJob),
    CancelSession(u64),
    Preload(String),
    Configure(WorkerConfig),
    /// Drop all loaded models (e.g. after the model files were replaced).
    UnloadAll,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Output(AsrOutput),
    ModelLoading { model: String },
    ModelLoaded { model: String, load_ms: u32, warmup_ms: u32 },
    ModelFailed { model: String, error: String },
    ModelUnloaded { model: String },
}

pub type WorkerSink = Box<dyn Fn(WorkerEvent) + Send + 'static>;

pub struct WorkerHandle {
    tx: Sender<WorkerMsg>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Queue a message without blocking. Partial jobs are dropped when the
    /// queue is full; anything else is reported.
    pub fn send(&self, msg: WorkerMsg) -> bool {
        match self.tx.try_send(msg) {
            Ok(()) => true,
            Err(TrySendError::Full(WorkerMsg::Job(j))) if j.kind == SnapshotKind::Partial => false,
            Err(TrySendError::Full(_)) => {
                log::error!("recogniser queue is full; a request was dropped");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                log::error!("recogniser thread is not running");
                false
            }
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(WorkerMsg::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn spawn(config: WorkerConfig, sink: WorkerSink) -> std::io::Result<WorkerHandle> {
    let (tx, rx) = crossbeam_channel::bounded(256);
    let join = std::thread::Builder::new().name("swiftvoice-asr".into()).spawn(move || {
        Worker::new(config, sink).run(rx);
    })?;
    Ok(WorkerHandle { tx, join: Some(join) })
}

struct Loaded {
    id: String,
    engine: Box<dyn SpeechEngine>,
    last_used: Instant,
}

struct Worker {
    config: WorkerConfig,
    sink: WorkerSink,
    engines: Vec<Loaded>,
    pending: VecDeque<AsrJob>,
    cancelled_upto: Vec<u64>,
    last_activity: Instant,
    failed: Vec<(String, String)>,
}

const MAX_LOADED: usize = 2;

impl Worker {
    fn new(config: WorkerConfig, sink: WorkerSink) -> Worker {
        Worker {
            config,
            sink,
            engines: Vec::new(),
            pending: VecDeque::new(),
            cancelled_upto: Vec::new(),
            last_activity: Instant::now(),
            failed: Vec::new(),
        }
    }

    fn run(&mut self, rx: Receiver<WorkerMsg>) {
        loop {
            if self.pending.is_empty() {
                let msg = if self.config.unload_after_idle_secs > 0 && !self.engines.is_empty() {
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(m) => Some(m),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                } else {
                    match rx.recv() {
                        Ok(m) => Some(m),
                        Err(_) => return,
                    }
                };
                match msg {
                    Some(m) => {
                        if !self.handle(m) {
                            return;
                        }
                    }
                    None => {
                        self.maybe_unload();
                        continue;
                    }
                }
            }
            while let Ok(m) = rx.try_recv() {
                if !self.handle(m) {
                    return;
                }
            }
            if let Some(job) = self.next_job() {
                self.run_job(job);
            }
        }
    }

    fn handle(&mut self, msg: WorkerMsg) -> bool {
        match msg {
            WorkerMsg::Job(job) => {
                if !self.cancelled_upto.contains(&job.session) {
                    self.pending.push_back(job);
                }
            }
            WorkerMsg::CancelSession(s) => {
                self.pending.retain(|j| j.session != s);
                self.cancelled_upto.push(s);
                if self.cancelled_upto.len() > 64 {
                    self.cancelled_upto.remove(0);
                }
            }
            WorkerMsg::Preload(model) => {
                if let Err(e) = self.engine(&model) {
                    log::warn!("preloading {model} failed: {e}");
                }
            }
            WorkerMsg::Configure(cfg) => {
                let reload = cfg.models_dir != self.config.models_dir
                    || cfg.threads != self.config.threads
                    || cfg.low_memory != self.config.low_memory
                    || cfg.whisper_prompt != self.config.whisper_prompt;
                self.config = cfg;
                if reload {
                    self.unload_all();
                }
                self.failed.clear();
            }
            WorkerMsg::UnloadAll => {
                self.unload_all();
                self.failed.clear();
            }
            WorkerMsg::Shutdown => return false,
        }
        true
    }

    /// Apply the supersede/priority rules and pop the job to run.
    fn next_job(&mut self) -> Option<AsrJob> {
        // Drop Pause/Final jobs superseded by a longer one for the same window.
        let snapshot = self.pending.clone();
        self.pending.retain(|j| {
            if j.kind == SnapshotKind::Partial {
                return true;
            }
            !snapshot.iter().any(|o| {
                o.kind != SnapshotKind::Partial
                    && o.session == j.session
                    && o.key.utterance == j.key.utterance
                    && o.key.window == j.key.window
                    && o.key.end > j.key.end
            })
        });
        // Keep only the newest partial per session.
        let snapshot = self.pending.clone();
        self.pending.retain(|j| {
            j.kind != SnapshotKind::Partial
                || !snapshot.iter().any(|o| o.kind == SnapshotKind::Partial && o.session == j.session && o.enqueued > j.enqueued)
        });
        if let Some(pos) = self.pending.iter().position(|j| j.kind != SnapshotKind::Partial) {
            return self.pending.remove(pos);
        }
        self.pending.pop_back()
    }

    fn run_job(&mut self, job: AsrJob) {
        let queue_ms = job.enqueued.elapsed().as_millis() as u32;
        let audio_ms = (job.audio.len() as u64 * 1000 / 16_000) as u32;
        let started = Instant::now();
        let result = match self.engine(&job.model) {
            Ok(engine) => engine.transcribe_after(&job.audio, job.context, job.language.as_deref()).map_err(|e| format!("{e:#}")),
            Err(e) => Err(e),
        };
        let compute_ms = started.elapsed().as_millis() as u32;
        self.last_activity = Instant::now();
        match &result {
            Ok(text) => log::debug!(
                "asr {:?} {}/{}/{}: {audio_ms} ms audio in {compute_ms} ms (queued {queue_ms} ms), {} chars",
                job.kind,
                job.key.utterance,
                job.key.window,
                job.key.end,
                text.chars().count()
            ),
            Err(e) => log::error!("transcription failed: {e}"),
        }
        log::trace!("asr text: {:?}", result);
        (self.sink)(WorkerEvent::Output(AsrOutput {
            session: job.session,
            key: job.key,
            kind: job.kind,
            model: job.model,
            result,
            queue_ms,
            compute_ms,
            audio_ms,
        }));
    }

    fn engine(&mut self, id: &str) -> Result<&mut Box<dyn SpeechEngine>, String> {
        if let Some(pos) = self.engines.iter().position(|l| l.id == id) {
            let l = &mut self.engines[pos];
            l.last_used = Instant::now();
            return Ok(&mut self.engines[pos].engine);
        }
        if let Some((_, err)) = self.failed.iter().find(|(m, _)| m == id) {
            return Err(err.clone());
        }
        (self.sink)(WorkerEvent::ModelLoading { model: id.to_string() });
        let started = Instant::now();
        super::runtime::set_low_memory(self.config.low_memory);
        let loaded = catalog::find(id)
            .ok_or_else(|| anyhow::anyhow!("unknown model \"{id}\""))
            .and_then(|info| super::load_engine(info, &self.config.models_dir, self.config.threads, &self.config.whisper_prompt));
        let mut engine = match loaded {
            Ok(e) => e,
            Err(e) => {
                let msg = format!("{e:#}");
                log::error!("cannot load model {id}: {msg}");
                (self.sink)(WorkerEvent::ModelFailed { model: id.to_string(), error: msg.clone() });
                self.failed.push((id.to_string(), msg.clone()));
                return Err(msg);
            }
        };
        let load_ms = started.elapsed().as_millis() as u32;
        let mut warmup_ms = 0;
        if self.config.warm_up {
            let w = Instant::now();
            // One second of very quiet noise exercises every operator.
            let mut seed = 1u32;
            let audio: Vec<f32> = (0..16_000)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 1e-3
                })
                .collect();
            if let Err(e) = engine.transcribe(&audio, None) {
                log::warn!("warm-up inference failed: {e:#}");
            }
            warmup_ms = w.elapsed().as_millis() as u32;
        }
        log::info!("model {id} ready: load {load_ms} ms, warm-up {warmup_ms} ms");
        (self.sink)(WorkerEvent::ModelLoaded { model: id.to_string(), load_ms, warmup_ms });
        if self.engines.len() >= MAX_LOADED {
            // Evict the least recently used engine.
            if let Some(pos) = self.engines.iter().enumerate().min_by_key(|(_, l)| l.last_used).map(|(i, _)| i) {
                let old = self.engines.remove(pos);
                log::info!("unloading model {} to make room", old.id);
                let id = old.id.clone();
                drop(old);
                crate::memory::release_free_memory();
                (self.sink)(WorkerEvent::ModelUnloaded { model: id });
            }
        }
        self.engines.push(Loaded { id: id.to_string(), engine, last_used: Instant::now() });
        self.last_activity = Instant::now();
        Ok(&mut self.engines.last_mut().expect("just pushed").engine)
    }

    fn maybe_unload(&mut self) {
        let idle = self.config.unload_after_idle_secs;
        if idle > 0 && !self.engines.is_empty() && self.last_activity.elapsed() >= Duration::from_secs(idle as u64) {
            log::info!("unloading models after {idle} s idle");
            self.unload_all();
        }
    }

    fn unload_all(&mut self) {
        let ids: Vec<String> = self.engines.drain(..).map(|l| l.id).collect();
        if !ids.is_empty() {
            crate::memory::release_free_memory();
        }
        for id in ids {
            (self.sink)(WorkerEvent::ModelUnloaded { model: id });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(session: u64, utterance: u32, window: u32, end: usize, kind: SnapshotKind) -> AsrJob {
        AsrJob {
            session,
            key: SnapKey { utterance, window, end },
            kind,
            audio: Arc::from(vec![0.0f32; 16]),
            context: 0,
            model: "m".into(),
            language: None,
            enqueued: Instant::now(),
        }
    }

    fn worker() -> Worker {
        Worker::new(
            WorkerConfig {
                models_dir: PathBuf::from("."),
                threads: 0,
                low_memory: true,
                unload_after_idle_secs: 0,
                warm_up: false,
                whisper_prompt: String::new(),
            },
            Box::new(|_| {}),
        )
    }

    #[test]
    fn superseded_pause_jobs_are_dropped_and_partials_wait() {
        let mut w = worker();
        w.pending.push_back(job(1, 0, 0, 100, SnapshotKind::Partial));
        w.pending.push_back(job(1, 0, 0, 200, SnapshotKind::Pause));
        w.pending.push_back(job(1, 0, 0, 300, SnapshotKind::Partial));
        w.pending.push_back(job(1, 0, 0, 400, SnapshotKind::Pause));
        w.pending.push_back(job(1, 0, 1, 50, SnapshotKind::Pause));
        let first = w.next_job().unwrap();
        assert_eq!((first.key.window, first.key.end), (0, 400));
        let second = w.next_job().unwrap();
        assert_eq!((second.key.window, second.key.end), (1, 50));
        let third = w.next_job().unwrap();
        assert_eq!(third.kind, SnapshotKind::Partial);
        assert_eq!(third.key.end, 300, "only the newest partial survives");
        assert!(w.next_job().is_none());
    }

    #[test]
    fn cancelled_sessions_are_discarded() {
        let mut w = worker();
        w.handle(WorkerMsg::Job(job(7, 0, 0, 10, SnapshotKind::Pause)));
        w.handle(WorkerMsg::CancelSession(7));
        assert!(w.next_job().is_none());
        w.handle(WorkerMsg::Job(job(7, 0, 0, 20, SnapshotKind::Pause)));
        assert!(w.next_job().is_none(), "late jobs of a cancelled session are ignored");
        w.handle(WorkerMsg::Job(job(8, 0, 0, 20, SnapshotKind::Final)));
        assert_eq!(w.next_job().unwrap().session, 8);
    }

    #[test]
    fn missing_model_reports_a_clear_error() {
        let mut w = worker();
        let err = w.engine("not-a-model").err().unwrap();
        assert!(err.contains("unknown model"), "{err}");
    }
}
