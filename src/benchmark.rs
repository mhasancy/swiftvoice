//! `swiftvoice --benchmark --audio file.wav [--reference file.txt]`
//!
//! Measures, on this machine and with the real models:
//!
//! * ONNX Runtime load, model load and warm-up time
//! * full-utterance transcription time over several runs (median/min/max)
//!   and the real-time factor (RTF = transcription time / audio duration)
//! * post-processing time for the configured mode
//! * word error rate against a reference transcript (raw and final text)
//! * peak and post-load resident memory
//! * perceived latency, by replaying the audio through the real VAD and
//!   segmenter and simulating the recogniser queue with measured inference
//!   times:
//!   - push-to-talk: time from key release to text ready, with the release
//!     `--release-delay-ms` after the last word (default 300 ms)
//!   - toggle mode: time after the VAD endpoint is confirmed
//!   - naive: transcribing the whole utterance after it ends (what a
//!     non-speculative dictation app does)
//!
//! Insertion itself (clipboard + Ctrl+V, typically a few ms) cannot be
//! measured without a target window and is reported by the app's log.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::asr::{self, catalog};
use crate::audio::processor::{make_vad, VadChoice};
use crate::audio::segmenter::{SegEvent, Segmenter, SegmenterConfig, SnapKey, SnapshotKind};
use crate::audio::vad::{FRAME_MS, FRAME_SAMPLES};
use crate::audio::wav;
use crate::config::{Config, PostMode};
use crate::memory;
use crate::postprocess::PostProcessor;

#[derive(Debug, Clone)]
pub struct BenchArgs {
    pub audio: PathBuf,
    pub reference: Option<String>,
    pub model: Option<String>,
    pub models_dir: Option<PathBuf>,
    pub mode: Option<PostMode>,
    pub runs: usize,
    pub release_delay_ms: u32,
    pub threads: Option<u32>,
    pub low_memory: Option<bool>,
    pub commit_after_secs: Option<u32>,
    pub language: Option<String>,
    pub json: Option<PathBuf>,
    pub process_start: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct UtteranceLatency {
    pub index: usize,
    pub speech_end_ms: f64,
    pub audio_ms: f64,
    /// Time after the VAD endpoint until the text was ready (toggle mode).
    pub toggle_after_endpoint_ms: f64,
    /// Last word -> text ready in toggle mode (includes the endpoint wait).
    pub toggle_speech_end_to_text_ms: f64,
    pub asr_jobs_run: usize,
    pub asr_jobs_dropped: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub swiftvoice_version: String,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub logical_cpus: usize,
    pub model: String,
    pub vad: String,
    pub low_memory: bool,
    pub audio_file: String,
    pub audio_secs: f64,
    pub audio_sample_rate: u32,
    pub ort_load_ms: f64,
    pub model_load_ms: f64,
    pub warmup_ms: f64,
    pub process_start_to_ready_ms: f64,
    pub runs: usize,
    pub transcribe_ms_median: f64,
    pub transcribe_ms_min: f64,
    pub transcribe_ms_max: f64,
    pub rtf: f64,
    pub raw_text: String,
    pub final_text: String,
    pub post_mode: String,
    pub postprocess_ms: f64,
    pub reference_words: Option<usize>,
    pub wer_raw: Option<f64>,
    pub wer_final: Option<f64>,
    pub rss_before_model_mb: Option<f64>,
    pub rss_after_model_mb: Option<f64>,
    pub peak_rss_mb: Option<f64>,
    /// After dropping the model (what `asr.unload_after_idle_secs` gives).
    pub rss_after_unload_mb: Option<f64>,
    pub release_delay_ms: u32,
    /// Push-to-talk: key release -> final text ready (+ post-processing).
    pub ptt_release_to_text_ms: f64,
    /// Naive approach: transcribe the whole utterance after release.
    pub naive_release_to_text_ms: f64,
    /// `asr.commit_after_secs` used by the simulation.
    pub commit_after_secs: u32,
    /// Push-to-talk: recogniser jobs run / dropped as superseded.
    pub ptt_jobs_run: usize,
    pub ptt_jobs_dropped: usize,
    /// Text the push-to-talk simulation inserts (committed windows joined,
    /// then post-processed), and its word error rate.
    pub ptt_text: String,
    pub wer_ptt: Option<f64>,
    pub utterances: Vec<UtteranceLatency>,
}

/// Word error rate details.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wer {
    pub wer: f64,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    pub reference_words: usize,
}

/// Lower-case, strip punctuation, split into words.
pub fn wer_words(s: &str) -> Vec<String> {
    // Explicit punctuation list, not "non-alphanumeric": vowel signs in
    // Bengali and other scripts are combining marks that belong to the word.
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            '\u{2019}' => '\'',
            '\'' => '\'',
            c if crate::postprocess::text::is_edge_punct(c) => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Levenshtein alignment over words.
pub fn word_error_rate(reference: &str, hypothesis: &str) -> Wer {
    let r = wer_words(reference);
    let h = wer_words(hypothesis);
    let (n, m) = (r.len(), h.len());
    // dp[i][j] = (cost, subs, dels, ins)
    let mut dp = vec![vec![(0usize, 0usize, 0usize, 0usize); m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate().skip(1) {
        row[0] = (i, 0, i, 0);
    }
    for (j, cell) in dp[0].iter_mut().enumerate().skip(1) {
        *cell = (j, 0, 0, j);
    }
    for i in 1..=n {
        for j in 1..=m {
            let same = r[i - 1] == h[j - 1];
            let (c, s, d, ins) = dp[i - 1][j - 1];
            let mut best = if same { (c, s, d, ins) } else { (c + 1, s + 1, d, ins) };
            let (c, s, d, ins) = dp[i - 1][j];
            if c + 1 < best.0 {
                best = (c + 1, s, d + 1, ins);
            }
            let (c, s, d, ins) = dp[i][j - 1];
            if c + 1 < best.0 {
                best = (c + 1, s, d, ins + 1);
            }
            dp[i][j] = best;
        }
    }
    let (cost, s, d, i) = dp[n][m];
    Wer {
        wer: if n == 0 {
            if m == 0 {
                0.0
            } else {
                1.0
            }
        } else {
            cost as f64 / n as f64
        },
        substitutions: s,
        deletions: d,
        insertions: i,
        reference_words: n,
    }
}

/// A recogniser job in the latency simulation.
#[derive(Debug, Clone)]
pub struct SimJob {
    pub emit_ms: f64,
    pub key: SnapKey,
    pub audio: Arc<[f32]>,
    pub context: usize,
}

/// Simulate the single recogniser thread: jobs start when emitted and the
/// worker is free; a queued job is dropped when a longer snapshot of the
/// same window was emitted before it started (as in `asr::worker`).
/// Returns finish times per key and the number of dropped jobs.
pub fn simulate_worker(jobs: &[SimJob], mut compute_ms: impl FnMut(&SimJob) -> f64) -> (HashMap<SnapKey, f64>, usize) {
    let mut remaining: Vec<usize> = (0..jobs.len()).collect();
    let mut finish = HashMap::new();
    let mut dropped = 0;
    let mut t = 0.0f64;
    while !remaining.is_empty() {
        let first = remaining.iter().map(|&i| jobs[i].emit_ms).fold(f64::INFINITY, f64::min);
        t = t.max(first);
        let available: Vec<usize> = remaining.iter().copied().filter(|&i| jobs[i].emit_ms <= t).collect();
        let superseded: Vec<usize> = available
            .iter()
            .copied()
            .filter(|&i| {
                available.iter().any(|&j| {
                    jobs[j].key.utterance == jobs[i].key.utterance
                        && jobs[j].key.window == jobs[i].key.window
                        && jobs[j].key.end > jobs[i].key.end
                })
            })
            .collect();
        dropped += superseded.len();
        remaining.retain(|i| !superseded.contains(i));
        let pick = remaining
            .iter()
            .copied()
            .filter(|&i| jobs[i].emit_ms <= t)
            .min_by(|&a, &b| jobs[a].emit_ms.partial_cmp(&jobs[b].emit_ms).unwrap_or(std::cmp::Ordering::Equal))
            .expect("at least the first job is available");
        t += compute_ms(&jobs[pick]);
        finish.insert(jobs[pick].key, t);
        remaining.retain(|&i| i != pick);
    }
    (finish, dropped)
}

struct Replay {
    jobs: Vec<SimJob>,
    /// (utterance, needed keys, emit time of the end event, speech end time)
    ends: Vec<(u32, Vec<SnapKey>, f64, f64)>,
    /// End of the last frame the VAD classified as speech.
    last_speech_ms: f64,
}

/// Feed 16 kHz audio through VAD + segmenter, recording event times.
/// `release_at_ms`: push-to-talk release time (then finish with the tail).
fn replay(audio: &[f32], vad: &mut dyn crate::audio::vad::Vad, cfg: SegmenterConfig, release_at_ms: Option<(f64, u32)>) -> Result<Replay> {
    vad.reset();
    let threshold = cfg.threshold;
    let mut seg = Segmenter::new(cfg);
    let mut events = Vec::new();
    let mut jobs = Vec::new();
    let mut commits: BTreeMap<u32, Vec<SnapKey>> = BTreeMap::new();
    let mut ends = Vec::new();
    let mut last_speech_ms = 0.0;
    let mut finishing: Option<u32> = None;
    let handle = |events: &mut Vec<SegEvent>,
                  now_ms: f64,
                  last_speech_ms: f64,
                  jobs: &mut Vec<SimJob>,
                  commits: &mut BTreeMap<u32, Vec<SnapKey>>,
                  ends: &mut Vec<(u32, Vec<SnapKey>, f64, f64)>| {
        for ev in events.drain(..) {
            match ev {
                SegEvent::Snapshot(s) if s.kind != SnapshotKind::Partial => {
                    jobs.push(SimJob { emit_ms: now_ms, key: s.key, audio: s.audio, context: s.context })
                }
                SegEvent::Commit { key } => commits.entry(key.utterance).or_default().push(key),
                SegEvent::UtteranceEnd { utterance, last, .. } => {
                    let mut keys = commits.remove(&utterance).unwrap_or_default();
                    keys.extend(last);
                    ends.push((utterance, keys, now_ms, last_speech_ms));
                }
                _ => {}
            }
        }
    };
    let frames = audio.len() / FRAME_SAMPLES;
    for f in 0..frames {
        let frame = &audio[f * FRAME_SAMPLES..(f + 1) * FRAME_SAMPLES];
        let now_ms = ((f + 1) as u32 * FRAME_MS) as f64;
        let prob = vad.speech_prob(frame)?;
        if prob >= threshold {
            last_speech_ms = now_ms;
        }
        seg.push_frame(frame, prob, &mut events);
        handle(&mut events, now_ms, last_speech_ms, &mut jobs, &mut commits, &mut ends);
        if let Some((release_ms, tail_ms)) = release_at_ms {
            if finishing.is_none() && now_ms >= release_ms {
                finishing = Some(if seg.needs_tail() { tail_ms / FRAME_MS } else { 0 });
            }
            if let Some(left) = finishing.as_mut() {
                if *left == 0 || !seg.needs_tail() {
                    seg.finish(&mut events);
                    handle(&mut events, now_ms, last_speech_ms, &mut jobs, &mut commits, &mut ends);
                    break;
                }
                *left -= 1;
            }
        }
    }
    if release_at_ms.is_some() && ends.is_empty() {
        let now_ms = (frames as u32 * FRAME_MS) as f64;
        seg.finish(&mut events);
        handle(&mut events, now_ms, last_speech_ms, &mut jobs, &mut commits, &mut ends);
    }
    Ok(Replay { jobs, ends, last_speech_ms })
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn cpu_name() -> String {
    #[cfg(target_os = "linux")]
    if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") {
        if let Some(line) = info.lines().find(|l| l.starts_with("model name")) {
            if let Some((_, v)) = line.split_once(':') {
                return v.trim().to_string();
            }
        }
    }
    #[cfg(windows)]
    if let Some(name) = crate::platform::cpu_name() {
        return name;
    }
    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".into())
}

pub fn run(args: &BenchArgs, config: &Config) -> Result<BenchReport> {
    let model_id = args.model.clone().unwrap_or_else(|| config.asr.model.clone());
    let info = catalog::find(&model_id).with_context(|| format!("unknown model \"{model_id}\" (see: swiftvoice models list)"))?;
    if !info.is_recognizer() {
        bail!("{model_id} is not a speech recogniser");
    }
    let models_dir = args.models_dir.clone().unwrap_or_else(|| config.models_dir());
    let threads = args.threads.unwrap_or(config.asr.threads);
    let low_memory = args.low_memory.unwrap_or(config.asr.low_memory);
    let commit_after_secs = args.commit_after_secs.unwrap_or(config.asr.commit_after_secs);
    crate::asr::runtime::set_low_memory(low_memory);
    let language = args.language.clone().unwrap_or_else(|| config.asr.language.clone());
    let lang_hint = (language != "auto").then_some(language.as_str());

    let input = wav::read_wav(&args.audio)?;
    let audio: Vec<f32> = input.to_16k();
    let audio_secs = input.duration_secs();
    if audio.len() < FRAME_SAMPLES * 4 {
        bail!("{} is too short to benchmark", args.audio.display());
    }

    let rss_before = memory::rss_bytes().map(|(c, _)| memory::mb(c));
    let t = Instant::now();
    crate::asr::runtime::environment()?;
    let ort_load_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let mut engine = asr::load_engine(info, &models_dir, threads, "")?;
    let model_load_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let _ = engine.transcribe(&vec![0.0f32; 16_000], lang_hint)?;
    let warmup_ms = t.elapsed().as_secs_f64() * 1000.0;
    let process_start_to_ready_ms = args.process_start.elapsed().as_secs_f64() * 1000.0;
    let rss_after = memory::rss_bytes().map(|(c, _)| memory::mb(c));

    // Full-utterance transcription.
    let runs = args.runs.max(1);
    let mut times = Vec::with_capacity(runs);
    let mut raw_text = String::new();
    for _ in 0..runs {
        let t = Instant::now();
        raw_text = engine.transcribe(&audio, lang_hint)?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let tmin = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let tmax = times.iter().cloned().fold(0.0, f64::max);
    let tmed = median(&mut times);

    // Post-processing.
    let mut pcfg = config.postprocess.clone();
    if let Some(mode) = args.mode {
        pcfg.mode = mode;
    }
    let english = language == "en" || (language == "auto" && info.languages == ["en"]);
    let post = PostProcessor::new(&pcfg, Vec::new());
    let t = Instant::now();
    let result = post.process(&raw_text, lang_hint, english);
    let postprocess_ms = t.elapsed().as_secs_f64() * 1000.0;

    let (wer_raw, wer_final, reference_words) = match &args.reference {
        Some(r) => {
            let a = word_error_rate(r, &raw_text);
            let b = word_error_rate(r, &result.text);
            (Some(a.wer), Some(b.wer), Some(a.reference_words))
        }
        None => (None, None, None),
    };

    // Perceived latency simulation.
    let vad_choice = VadChoice {
        engine: config.vad.engine,
        silero_model: Some(models_dir.join("silero-vad").join("silero_vad.onnx")),
        energy_margin_db: config.vad.energy_margin_db,
    };
    let (mut vad, vad_warning) = make_vad(&vad_choice);
    if let Some(w) = &vad_warning {
        log::warn!("{w}");
    }
    let vad_name = vad.name().to_string();
    let v = &config.vad;
    let toggle_cfg =
        SegmenterConfig::from_ms(v.threshold, v.pause_ms, Some(v.endpoint_ms), v.pre_roll_ms, commit_after_secs, None, None, None);
    // Recordings often stop right after the last word; append silence so the
    // endpoint (toggle) and the key release (push-to-talk) happen after the
    // speech, as they would live.
    let pad_ms = v.endpoint_ms.max(args.release_delay_ms + config.hotkeys.release_tail_ms) + 1000;
    let mut sim_audio = audio.clone();
    sim_audio.resize(audio.len() + pad_ms as usize * 16, 0.0);
    let toggle = replay(&sim_audio, vad.as_mut(), toggle_cfg, None)?;
    // Push-to-talk: one hold over the whole file, released after the last word.
    let last_speech_ms = if toggle.last_speech_ms > 0.0 { toggle.last_speech_ms } else { audio_secs * 1000.0 };
    let release_ms = last_speech_ms + args.release_delay_ms as f64;
    let ptt_cfg = SegmenterConfig::from_ms(v.threshold, v.pause_ms, None, v.pre_roll_ms, commit_after_secs, None, None, None);
    let ptt = replay(&sim_audio, vad.as_mut(), ptt_cfg, Some((release_ms, config.hotkeys.release_tail_ms)))?;

    // The event timeline is deterministic; recogniser times are not. Run
    // the simulation `runs` times with fresh transcriptions and report
    // medians.
    let mut toggle_runs: Vec<Vec<f64>> = vec![Vec::new(); toggle.ends.len()];
    let mut ptt_runs = Vec::new();
    let mut utterances = Vec::new();
    let (mut ptt_jobs_run, mut ptt_jobs_dropped) = (0, 0);
    let mut ptt_raw = String::new();
    for run in 0..runs {
        let mut cache: HashMap<SnapKey, (f64, String)> = HashMap::new();
        let mut compute = |job: &SimJob| -> f64 {
            if let Some((ms, _)) = cache.get(&job.key) {
                return *ms;
            }
            let t = Instant::now();
            let text = engine.transcribe_after(&job.audio, job.context, lang_hint).unwrap_or_default();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            cache.insert(job.key, (ms, text));
            ms
        };
        let (finish, dropped) = simulate_worker(&toggle.jobs, &mut compute);
        for (i, (_, keys, end_ms, _)) in toggle.ends.iter().enumerate() {
            let ready = keys.iter().filter_map(|k| finish.get(k)).cloned().fold(0.0, f64::max);
            toggle_runs[i].push((ready - end_ms).max(0.0) + postprocess_ms);
        }
        if run == 0 {
            for (i, (_, keys, _, speech_end_ms)) in toggle.ends.iter().enumerate() {
                utterances.push(UtteranceLatency {
                    index: i,
                    speech_end_ms: *speech_end_ms,
                    audio_ms: keys.iter().map(|k| k.end as f64 / 16.0).sum::<f64>(),
                    toggle_after_endpoint_ms: 0.0,
                    toggle_speech_end_to_text_ms: 0.0,
                    asr_jobs_run: finish.len(),
                    asr_jobs_dropped: dropped,
                });
            }
        }
        let (ptt_finish, dropped) = simulate_worker(&ptt.jobs, &mut compute);
        if let Some((_, keys, _, _)) = ptt.ends.last() {
            let ready = keys.iter().filter_map(|k| ptt_finish.get(k)).cloned().fold(0.0, f64::max);
            ptt_runs.push((ready - release_ms).max(0.0) + postprocess_ms);
        }
        if run == 0 {
            (ptt_jobs_run, ptt_jobs_dropped) = (ptt_finish.len(), dropped);
            for j in &ptt.jobs {
                log::debug!(
                    "push-to-talk job utterance {} window {} end {}: {:.0} ms of audio (+{:.0} ms context) emitted at {:.0} ms, ready at {}: {:?}",
                    j.key.utterance,
                    j.key.window,
                    j.key.end,
                    (j.audio.len() - j.context) as f64 / 16.0,
                    j.context as f64 / 16.0,
                    j.emit_ms,
                    ptt_finish.get(&j.key).map(|t| format!("{t:.0} ms")).unwrap_or_else(|| "(dropped)".into()),
                    cache.get(&j.key).map(|(_, t)| t.as_str()).unwrap_or("")
                );
            }
            log::debug!("push-to-talk: last word at {last_speech_ms:.0} ms, key released at {release_ms:.0} ms");
            if let Some((_, keys, _, _)) = ptt.ends.last() {
                ptt_raw = crate::postprocess::join_windows(keys.iter().filter_map(|k| cache.get(k).map(|(_, t)| t.as_str())));
            }
        }
    }
    for (u, times) in utterances.iter_mut().zip(toggle_runs.iter_mut()) {
        u.toggle_after_endpoint_ms = median(times);
        let (_, _, end_ms, speech_end_ms) = toggle.ends[u.index];
        u.toggle_speech_end_to_text_ms = (end_ms - speech_end_ms) + u.toggle_after_endpoint_ms;
    }
    let ptt_release_to_text_ms = if ptt_runs.is_empty() { f64::NAN } else { median(&mut ptt_runs) };
    let ptt_text = post.process(&ptt_raw, lang_hint, english).text;
    let wer_ptt = args.reference.as_ref().map(|r| word_error_rate(r, &ptt_text).wer);
    let naive_release_to_text_ms = tmed + postprocess_ms;

    let peak = memory::rss_bytes().map(|(_, p)| memory::mb(p));
    drop(engine);
    memory::release_free_memory();
    let rss_after_unload = memory::rss_bytes().map(|(c, _)| memory::mb(c));
    let report = BenchReport {
        swiftvoice_version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        cpu: cpu_name(),
        logical_cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        model: model_id,
        vad: vad_name,
        low_memory,
        commit_after_secs,
        ptt_jobs_run,
        ptt_jobs_dropped,
        ptt_text,
        wer_ptt,
        audio_file: args.audio.display().to_string(),
        audio_secs,
        audio_sample_rate: input.sample_rate,
        ort_load_ms,
        model_load_ms,
        warmup_ms,
        process_start_to_ready_ms,
        runs,
        transcribe_ms_median: tmed,
        transcribe_ms_min: tmin,
        transcribe_ms_max: tmax,
        rtf: tmed / (audio_secs * 1000.0),
        raw_text,
        final_text: result.text,
        post_mode: format!("{:?}", result.mode).to_lowercase(),
        postprocess_ms,
        reference_words,
        wer_raw,
        wer_final,
        rss_before_model_mb: rss_before,
        rss_after_model_mb: rss_after,
        peak_rss_mb: peak,
        rss_after_unload_mb: rss_after_unload,
        release_delay_ms: args.release_delay_ms,
        ptt_release_to_text_ms,
        naive_release_to_text_ms,
        utterances,
    };
    if let Some(path) = &args.json {
        std::fs::write(path, serde_json::to_string_pretty(&report)?).with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(report)
}

fn opt_ms(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.0} MB")).unwrap_or_else(|| "n/a".into())
}

/// Human-readable report.
pub fn format_report(r: &BenchReport) -> String {
    let mut s = String::new();
    let mut line = |k: &str, v: String| s.push_str(&format!("  {k:<38} {v}\n"));
    line("Machine", format!("{} ({} logical CPUs), {}/{}", r.cpu, r.logical_cpus, r.os, r.arch));
    line("Model / VAD", format!("{} / {}, low_memory {}", r.model, r.vad, if r.low_memory { "on" } else { "off" }));
    line("Audio", format!("{} ({:.2} s, {} Hz)", r.audio_file, r.audio_secs, r.audio_sample_rate));
    line("ONNX Runtime load", format!("{:.0} ms", r.ort_load_ms));
    line("Model load + warm-up", format!("{:.0} ms + {:.0} ms", r.model_load_ms, r.warmup_ms));
    line("Process start -> model ready", format!("{:.0} ms", r.process_start_to_ready_ms));
    line(
        "Transcription time (full utterance)",
        format!(
            "{:.0} ms median ({} runs, min {:.0}, max {:.0})",
            r.transcribe_ms_median, r.runs, r.transcribe_ms_min, r.transcribe_ms_max
        ),
    );
    line("Real-time factor (RTF)", format!("{:.3} ({:.0}x faster than real time)", r.rtf, 1.0 / r.rtf.max(1e-9)));
    line("Post-processing", format!("{:.2} ms ({})", r.postprocess_ms, r.post_mode));
    if let (Some(a), Some(b), Some(n)) = (r.wer_raw, r.wer_final, r.reference_words) {
        line("Word error rate (raw / final)", format!("{:.1}% / {:.1}% ({} reference words)", a * 100.0, b * 100.0, n));
    } else {
        line("Word error rate", "n/a (pass --reference)".into());
    }
    line(
        "Memory before model / loaded / peak",
        format!("{} / {} / {}", opt_ms(r.rss_before_model_mb), opt_ms(r.rss_after_model_mb), opt_ms(r.peak_rss_mb)),
    );
    line("Memory after unloading the model", opt_ms(r.rss_after_unload_mb));
    line(
        "Perceived latency, push-to-talk",
        format!(
            "{:.0} ms from key release to text, median (release {} ms after the last word)",
            r.ptt_release_to_text_ms, r.release_delay_ms
        ),
    );
    line(
        "  recogniser jobs run / dropped",
        format!("{} / {} (commit at pauses after {} s)", r.ptt_jobs_run, r.ptt_jobs_dropped, r.commit_after_secs),
    );
    if let Some(w) = r.wer_ptt {
        line("  word error rate of the inserted text", format!("{:.1}%", w * 100.0));
    }
    line("  same, without speculative decoding", format!("{:.0} ms", r.naive_release_to_text_ms));
    for u in &r.utterances {
        line(
            &format!("Toggle mode, utterance {}", u.index + 1),
            format!(
                "{:.0} ms after the VAD endpoint, median ({:.0} ms after the last word incl. the endpoint wait)",
                u.toggle_after_endpoint_ms, u.toggle_speech_end_to_text_ms
            ),
        );
    }
    s.push_str(&format!("\n  Raw:   {}\n  Final: {}\n  Push-to-talk: {}\n", r.raw_text, r.final_text, r.ptt_text));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wer_words_keep_combining_marks_and_apostrophes() {
        assert_eq!(wer_words("আমি ভাত খাই।"), vec!["আমি", "ভাত", "খাই"]);
        assert_eq!(wer_words("Don’t stop, ok? 'Fine.'"), vec!["don't", "stop", "ok", "fine"]);
    }

    #[test]
    fn wer_counts_edit_types() {
        let w = word_error_rate("the cat sat on the mat", "the cat sat on mat");
        assert_eq!((w.deletions, w.substitutions, w.insertions), (1, 0, 0));
        assert!((w.wer - 1.0 / 6.0).abs() < 1e-9);
        let w = word_error_rate("Hello, world!", "hello world");
        assert_eq!(w.wer, 0.0);
        let w = word_error_rate("a b c", "a x c d");
        assert_eq!((w.substitutions, w.insertions), (1, 1));
        assert_eq!(word_error_rate("", "").wer, 0.0);
        assert_eq!(word_error_rate("", "x").wer, 1.0);
        assert_eq!(word_error_rate("don't stop", "Don't stop.").wer, 0.0);
    }

    fn key(window: u32, end: usize) -> SnapKey {
        SnapKey { utterance: 0, window, end }
    }

    fn job(emit_ms: f64, window: u32, end: usize) -> SimJob {
        SimJob { emit_ms, key: key(window, end), audio: Arc::from(vec![0.0f32; 1]), context: 0 }
    }

    #[test]
    fn worker_simulation_overlaps_and_supersedes() {
        // Pause snapshot at 1000 ms takes 300 ms; a later snapshot of the
        // same window emitted at 1100 ms (while the first runs) starts at
        // 1300 ms. A third emitted at 1200 ms supersedes the second.
        let jobs = vec![job(1000.0, 0, 100), job(1100.0, 0, 200), job(1200.0, 0, 300)];
        let (finish, dropped) = simulate_worker(&jobs, |_| 300.0);
        assert_eq!(dropped, 1);
        assert_eq!(finish[&key(0, 100)], 1300.0);
        assert_eq!(finish[&key(0, 300)], 1600.0);
        assert!(!finish.contains_key(&key(0, 200)));
        // Jobs in different windows are never dropped.
        let jobs = vec![job(0.0, 0, 100), job(10.0, 1, 50)];
        let (finish, dropped) = simulate_worker(&jobs, |_| 100.0);
        assert_eq!((dropped, finish.len()), (0, 2));
    }

    #[test]
    fn replay_finds_utterances_in_synthetic_audio() {
        // 0.5 s silence, 1.5 s "speech" (loud tone), 1.5 s silence.
        let mut audio = vec![0.0f32; 8000];
        audio.extend((0..24_000).map(|i| 0.3 * (i as f32 * 0.09).sin()));
        audio.extend(std::iter::repeat_n(0.0, 24_000));
        let mut vad = crate::audio::vad::EnergyVad::new(9.0);
        let cfg = SegmenterConfig::from_ms(0.5, 200, Some(800), 300, 12, None, None, None);
        let r = replay(&audio, &mut vad, cfg, None).unwrap();
        assert_eq!(r.ends.len(), 1);
        let (_, keys, end_ms, speech_end_ms) = &r.ends[0];
        assert_eq!(keys.len(), 1);
        assert!((speech_end_ms - 2000.0).abs() <= 64.0, "speech end {speech_end_ms}");
        assert!(end_ms > speech_end_ms);
        // The pause snapshot is emitted long before the endpoint.
        assert!(r.jobs.iter().any(|j| j.key == keys[0] && j.emit_ms < *end_ms));
        let cfg = SegmenterConfig::from_ms(0.5, 200, None, 300, 12, None, None, None);
        let r = replay(&audio, &mut vad, cfg, Some((2300.0, 150))).unwrap();
        assert_eq!(r.ends.len(), 1);
    }
}
