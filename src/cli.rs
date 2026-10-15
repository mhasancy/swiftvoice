//! Command-line interface.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

use crate::asr::{catalog, download};
use crate::benchmark::{self, BenchArgs};
use crate::config::{Config, PostMode};

pub const USAGE: &str = "\
SwiftVoice - fast, private, offline voice typing

USAGE:
  swiftvoice [--verbose] [--config FILE]        run the tray app (Windows)
  swiftvoice --benchmark --audio FILE.wav [OPTIONS]
  swiftvoice --transcribe FILE.wav [--model ID] [--mode raw|clean|smart] [--language CODE]
  swiftvoice --record FILE.wav [--seconds N]     record from the microphone (Windows)
  swiftvoice --doctor                           check the installation
  swiftvoice models list
  swiftvoice models download ID [ID ...]
  swiftvoice models verify [ID ...]
  swiftvoice models remove ID
  swiftvoice --version | --help

BENCHMARK OPTIONS:
  --reference FILE        reference transcript for word error rate
  --reference-text TEXT   reference transcript given inline
  --model ID              model to benchmark (default: config asr.model)
  --models-dir DIR        models folder (default: config / %LOCALAPPDATA%)
  --mode raw|clean|smart  post-processing mode (default: config)
  --runs N                transcription repetitions (default 5)
  --release-delay-ms MS   push-to-talk release after the last word (default 300)
  --threads N             inference threads (default: config, 0 = auto)
  --low-memory on|off     skip weight pre-packing (default: config asr.low_memory)
  --commit-after-secs N   commit long dictation at pauses after N s (default: config)
  --language CODE         language hint (default: config)
  --json FILE             also write the report as JSON

GLOBAL OPTIONS:
  -v, --verbose           debug logging, also printed to the console
  --config FILE           use this config.toml instead of the default
";

#[derive(Debug, Clone, PartialEq)]
pub enum ModelsCmd {
    List,
    Download(Vec<String>),
    Verify(Vec<String>),
    Remove(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Tray,
    Benchmark(BenchOpts),
    Transcribe { audio: PathBuf, model: Option<String>, models_dir: Option<PathBuf>, mode: Option<PostMode>, language: Option<String> },
    Record { out: PathBuf, seconds: u32 },
    Doctor,
    Models { cmd: ModelsCmd, models_dir: Option<PathBuf> },
    Version,
    Help,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BenchOpts {
    pub audio: Option<PathBuf>,
    pub reference_file: Option<PathBuf>,
    pub reference_text: Option<String>,
    pub model: Option<String>,
    pub models_dir: Option<PathBuf>,
    pub mode: Option<PostMode>,
    pub runs: Option<usize>,
    pub release_delay_ms: Option<u32>,
    pub threads: Option<u32>,
    pub low_memory: Option<bool>,
    pub commit_after_secs: Option<u32>,
    pub language: Option<String>,
    pub json: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub command: Command,
    pub verbose: bool,
    pub config: Option<PathBuf>,
}

fn parse_mode(s: &str) -> Result<PostMode> {
    match s.to_ascii_lowercase().as_str() {
        "raw" => Ok(PostMode::Raw),
        "clean" => Ok(PostMode::Clean),
        "smart" => Ok(PostMode::Smart),
        other => bail!("--mode must be raw, clean or smart (got \"{other}\")"),
    }
}

fn parse_on_off(s: &str) -> Result<bool> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => bail!("expected on or off (got \"{other}\")"),
    }
}

pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Invocation> {
    use lexopt::prelude::*;
    let mut parser = lexopt::Parser::from_iter(args);
    let mut verbose = false;
    let mut config = None;
    let mut command: Option<Command> = None;
    let mut bench = BenchOpts::default();
    let mut benchmark = false;
    let mut transcribe: Option<PathBuf> = None;
    let mut record: Option<PathBuf> = None;
    let mut seconds = 5u32;
    let mut models_words: Vec<String> = Vec::new();
    let mut models_mode = false;
    let set = |command: &mut Option<Command>, c: Command| -> Result<()> {
        if command.is_some() {
            bail!("only one command can be given at a time");
        }
        *command = Some(c);
        Ok(())
    };
    while let Some(arg) = parser.next()? {
        match arg {
            Short('v') | Long("verbose") => verbose = true,
            Short('h') | Long("help") => set(&mut command, Command::Help)?,
            Short('V') | Long("version") => set(&mut command, Command::Version)?,
            Long("config") => config = Some(PathBuf::from(parser.value()?)),
            Long("benchmark") => benchmark = true,
            Long("audio") => bench.audio = Some(PathBuf::from(parser.value()?)),
            Long("reference") => bench.reference_file = Some(PathBuf::from(parser.value()?)),
            Long("reference-text") => bench.reference_text = Some(parser.value()?.string()?),
            Long("model") => bench.model = Some(parser.value()?.string()?),
            Long("models-dir") => bench.models_dir = Some(PathBuf::from(parser.value()?)),
            Long("mode") => bench.mode = Some(parse_mode(&parser.value()?.string()?)?),
            Long("runs") => bench.runs = Some(parser.value()?.parse()?),
            Long("release-delay-ms") => bench.release_delay_ms = Some(parser.value()?.parse()?),
            Long("threads") => bench.threads = Some(parser.value()?.parse()?),
            Long("low-memory") => bench.low_memory = Some(parse_on_off(&parser.value()?.string()?)?),
            Long("commit-after-secs") => bench.commit_after_secs = Some(parser.value()?.parse()?),
            Long("language") => bench.language = Some(parser.value()?.string()?),
            Long("json") => bench.json = Some(PathBuf::from(parser.value()?)),
            Long("transcribe") => transcribe = Some(PathBuf::from(parser.value()?)),
            Long("record") => record = Some(PathBuf::from(parser.value()?)),
            Long("seconds") => seconds = parser.value()?.parse()?,
            Long("doctor") => set(&mut command, Command::Doctor)?,
            Value(v) if command.is_none() && !models_mode && v == "models" => models_mode = true,
            Value(v) if models_mode => models_words.push(v.string()?),
            other => return Err(anyhow!("{}", other.unexpected())),
        }
    }
    if benchmark {
        if bench.audio.is_none() {
            bail!("--benchmark needs --audio FILE.wav");
        }
        set(&mut command, Command::Benchmark(bench.clone()))?;
    }
    if let Some(audio) = transcribe {
        set(
            &mut command,
            Command::Transcribe {
                audio,
                model: bench.model.clone(),
                models_dir: bench.models_dir.clone(),
                mode: bench.mode,
                language: bench.language.clone(),
            },
        )?;
    }
    if let Some(out) = record {
        if seconds == 0 || seconds > 600 {
            bail!("--seconds must be between 1 and 600");
        }
        set(&mut command, Command::Record { out, seconds })?;
    }
    if models_mode {
        let mut words = models_words.into_iter();
        let cmd = match words.next().as_deref() {
            None | Some("list") => ModelsCmd::List,
            Some("download") => {
                let ids: Vec<String> = words.collect();
                if ids.is_empty() {
                    bail!("models download needs at least one model id (see: swiftvoice models list)");
                }
                ModelsCmd::Download(ids)
            }
            Some("verify") => ModelsCmd::Verify(words.collect()),
            Some("remove") => ModelsCmd::Remove(words.next().context("models remove needs a model id")?),
            Some(other) => bail!("unknown models command \"{other}\" (list, download, verify, remove)"),
        };
        set(&mut command, Command::Models { cmd, models_dir: bench.models_dir.clone() })?;
    }
    Ok(Invocation { command: command.unwrap_or(Command::Tray), verbose, config })
}

/// Config for command-line tools: the file if it exists, else defaults
/// (CLI commands never create files in the user's profile).
pub fn load_config_readonly(path: &Path) -> Result<Config> {
    if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        Config::from_toml_str(&text).with_context(|| format!("invalid config file {}", path.display()))
    } else {
        Ok(Config::default())
    }
}

pub fn run(inv: Invocation, process_start: Instant) -> i32 {
    let config_path = inv.config.clone().unwrap_or_else(crate::paths::config_file);
    let is_tray = inv.command == Command::Tray;
    let level = if inv.verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info };
    let log_dir = crate::paths::logs_dir();
    if let Err(e) = crate::logging::init(&log_dir, level, inv.verbose || !is_tray) {
        eprintln!("warning: cannot write logs to {}: {e}", log_dir.display());
    }
    let result = match inv.command {
        Command::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Command::Version => {
            println!(
                "SwiftVoice {} (ONNX Runtime {}, whisper.cpp {})",
                env!("CARGO_PKG_VERSION"),
                crate::asr::runtime::ORT_VERSION,
                if cfg!(feature = "whisper") { "enabled" } else { "not built" }
            );
            Ok(())
        }
        Command::Tray => run_tray(&config_path, inv.verbose, process_start),
        Command::Benchmark(opts) => run_benchmark(&config_path, opts, process_start),
        Command::Transcribe { audio, model, models_dir, mode, language } => {
            run_transcribe(&config_path, &audio, model, models_dir, mode, language)
        }
        Command::Record { out, seconds } => run_record(&config_path, &out, seconds),
        Command::Doctor => run_doctor(&config_path),
        Command::Models { cmd, models_dir } => run_models(&config_path, cmd, models_dir),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            log::error!("{e:#}");
            eprintln!("error: {e:#}");
            #[cfg(windows)]
            if is_tray {
                crate::platform::error_box("SwiftVoice could not start", &format!("{e:#}"));
            }
            1
        }
    }
}

#[cfg(windows)]
fn run_tray(config_path: &Path, verbose: bool, process_start: Instant) -> Result<()> {
    crate::app::run(config_path, verbose, process_start)
}

#[cfg(not(windows))]
fn run_tray(_config_path: &Path, _verbose: bool, _process_start: Instant) -> Result<()> {
    bail!("the tray app is Windows-only; on this platform use --benchmark, --transcribe or models (see --help)")
}

fn run_benchmark(config_path: &Path, o: BenchOpts, process_start: Instant) -> Result<()> {
    let config = load_config_readonly(config_path)?;
    let reference = match (&o.reference_text, &o.reference_file) {
        (Some(t), _) => Some(t.clone()),
        (None, Some(f)) => Some(std::fs::read_to_string(f).with_context(|| format!("cannot read reference {}", f.display()))?),
        (None, None) => None,
    };
    let args = BenchArgs {
        audio: o.audio.clone().expect("checked by the parser"),
        reference,
        model: o.model,
        models_dir: o.models_dir,
        mode: o.mode,
        runs: o.runs.unwrap_or(5),
        release_delay_ms: o.release_delay_ms.unwrap_or(300),
        threads: o.threads,
        low_memory: o.low_memory,
        commit_after_secs: o.commit_after_secs,
        language: o.language,
        json: o.json,
        process_start,
    };
    let report = benchmark::run(&args, &config)?;
    println!("SwiftVoice benchmark\n{}", benchmark::format_report(&report));
    Ok(())
}

fn run_transcribe(
    config_path: &Path,
    audio: &Path,
    model: Option<String>,
    models_dir: Option<PathBuf>,
    mode: Option<PostMode>,
    language: Option<String>,
) -> Result<()> {
    let config = load_config_readonly(config_path)?;
    let model_id = model.unwrap_or_else(|| config.asr.model.clone());
    let info = catalog::find(&model_id).with_context(|| format!("unknown model \"{model_id}\""))?;
    let models_dir = models_dir.unwrap_or_else(|| config.models_dir());
    let language = language.unwrap_or_else(|| config.asr.language.clone());
    let lang = (language != "auto").then_some(language.as_str());
    let input = crate::audio::wav::read_wav(audio)?;
    crate::asr::runtime::set_low_memory(config.asr.low_memory);
    let mut engine = crate::asr::load_engine(info, &models_dir, config.asr.threads, "")?;
    let t = Instant::now();
    let raw = engine.transcribe(&input.to_16k(), lang)?;
    let ms = t.elapsed().as_millis();
    let mut pcfg = config.postprocess.clone();
    if let Some(m) = mode {
        pcfg.mode = m;
    }
    let vocab = crate::postprocess::vocabulary::Vocabulary::read_user_file(&config.vocabulary_path(config_path))?;
    let post = crate::postprocess::PostProcessor::new(&pcfg, vocab);
    let english = language == "en" || (language == "auto" && info.languages == ["en"]);
    let out = post.process(&raw, lang, english);
    println!("{}", out.text);
    eprintln!("({:.2} s audio, transcribed in {ms} ms, {} mode)", input.duration_secs(), format!("{:?}", out.mode).to_lowercase());
    Ok(())
}

#[cfg(windows)]
fn run_record(config_path: &Path, out: &Path, seconds: u32) -> Result<()> {
    let config = load_config_readonly(config_path)?;
    let (mut stream, warning) = crate::audio::capture::open(&config.audio.device).map_err(|e| anyhow!("{e}"))?;
    if let Some(w) = warning {
        eprintln!("{w}");
    }
    eprintln!("Recording {seconds} s from \"{}\" ({} Hz)... speak now.", stream.device_name, stream.sample_rate);
    let rate = stream.sample_rate;
    let total = rate as usize * seconds as usize;
    let mut samples = Vec::with_capacity(total);
    let mut buf = vec![0f32; 4800];
    let started = Instant::now();
    while samples.len() < total && started.elapsed().as_secs() < seconds as u64 + 5 {
        let n = stream.consumer.pop_slice(&mut buf);
        samples.extend_from_slice(&buf[..n.min(total - samples.len())]);
        if n == 0 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if let Ok(err) = stream.errors.try_recv() {
            bail!("{err}");
        }
    }
    let resampled = crate::audio::resample::resample_all(&samples, rate, 16_000);
    crate::audio::wav::write_wav(out, &resampled, 16_000)?;
    eprintln!("Saved {} ({:.1} s, 16 kHz mono).", out.display(), resampled.len() as f64 / 16_000.0);
    Ok(())
}

#[cfg(not(windows))]
fn run_record(_config_path: &Path, _out: &Path, _seconds: u32) -> Result<()> {
    bail!("--record uses the Windows microphone path and is only available on Windows")
}

fn run_models(config_path: &Path, cmd: ModelsCmd, models_dir: Option<PathBuf>) -> Result<()> {
    let config = load_config_readonly(config_path)?;
    let dir = models_dir.unwrap_or_else(|| config.models_dir());
    match cmd {
        ModelsCmd::List => {
            println!("Models folder: {}\n", dir.display());
            for m in catalog::CATALOG {
                let state = match (download::is_installed(&dir, m), download::is_custom_export(&dir, m)) {
                    (true, true) => "installed (custom export)",
                    (true, false) => "installed",
                    (false, _) => "not installed",
                };
                let active = if m.id == config.asr.model {
                    " (selected)"
                } else if m.id == config.asr.fallback_model {
                    " (fallback)"
                } else {
                    ""
                };
                println!("{:<30} {}{active}\n    {}\n    {} · {}\n", m.id, m.display, m.summary(), state, m.model_card);
            }
            Ok(())
        }
        ModelsCmd::Download(ids) => {
            for id in ids {
                let m = catalog::find(&id).with_context(|| format!("unknown model \"{id}\" (see: swiftvoice models list)"))?;
                if download::is_installed(&dir, m) {
                    println!("{id}: already installed");
                    continue;
                }
                println!("{id}: downloading {} from {}", catalog::human_bytes(m.download_bytes), m.url);
                let cancel = AtomicBool::new(false);
                let mut last = u32::MAX;
                let mut progress = |p: download::Progress| {
                    let pct = p.percent().unwrap_or(0);
                    if p.phase == download::Phase::Downloading && pct != last && pct.is_multiple_of(5) {
                        last = pct;
                        eprintln!("  {pct:>3}%  {}", catalog::human_bytes(p.done));
                    } else if p.phase == download::Phase::Extracting && last != 1000 {
                        last = 1000;
                        eprintln!("  extracting...");
                    }
                };
                let path = download::install(&dir, m, &mut progress, &cancel)?;
                println!("{id}: installed in {}", path.display());
            }
            Ok(())
        }
        ModelsCmd::Verify(ids) => {
            let targets: Vec<&catalog::ModelInfo> = if ids.is_empty() {
                catalog::CATALOG.iter().filter(|m| download::is_installed(&dir, m)).collect()
            } else {
                ids.iter().map(|id| catalog::find(id).with_context(|| format!("unknown model \"{id}\""))).collect::<Result<_>>()?
            };
            if targets.is_empty() {
                println!("No installed models in {}", dir.display());
            }
            let mut failed = 0;
            for m in targets {
                match download::verify(&dir, m) {
                    Ok(()) if download::is_custom_export(&dir, m) => println!("{}: OK (custom export, digests not checked)", m.id),
                    Ok(()) => println!("{}: OK", m.id),
                    Err(e) => {
                        failed += 1;
                        println!("{}: FAILED: {e:#}", m.id);
                    }
                }
            }
            if failed > 0 {
                bail!("{failed} model(s) failed verification; re-download them");
            }
            Ok(())
        }
        ModelsCmd::Remove(id) => {
            let m = catalog::find(&id).with_context(|| format!("unknown model \"{id}\""))?;
            download::remove(&dir, m)?;
            println!("{id}: removed");
            Ok(())
        }
    }
}

fn run_doctor(config_path: &Path) -> Result<()> {
    println!("SwiftVoice {} doctor\n", env!("CARGO_PKG_VERSION"));
    let mut problems = 0;
    let config = match load_config_readonly(config_path) {
        Ok(c) => {
            println!("[ok]   config: {}{}", config_path.display(), if config_path.exists() { "" } else { " (not created yet; defaults)" });
            c
        }
        Err(e) => {
            problems += 1;
            println!("[FAIL] config: {e:#}");
            Config::default()
        }
    };
    println!("[info] logs: {}", crate::paths::logs_dir().display());
    let dir = config.models_dir();
    println!("[info] models folder: {}", dir.display());
    match crate::asr::runtime::find_library() {
        Ok(p) => match crate::asr::runtime::environment() {
            Ok(_) => {
                println!("[ok]   ONNX Runtime: {}", p.display());
                if crate::asr::runtime::telemetry_disabled_by_environment() {
                    println!("[ok]   ONNX Runtime telemetry: off");
                } else {
                    problems += 1;
                    println!("[FAIL] ONNX Runtime telemetry: ORT_DISABLE_TELEMETRY is not set");
                }
            }
            Err(e) => {
                problems += 1;
                println!("[FAIL] ONNX Runtime at {}: {e:#}", p.display());
            }
        },
        Err(e) => {
            problems += 1;
            println!("[FAIL] {e:#}");
        }
    }
    for m in catalog::CATALOG {
        let installed = download::is_installed(&dir, m);
        let role = if m.id == config.asr.model {
            "selected model"
        } else if m.id == config.asr.fallback_model {
            "fallback model"
        } else if m.id == "silero-vad" {
            "voice activity detector"
        } else {
            continue;
        };
        if installed {
            println!("[ok]   {role}: {} installed", m.id);
        } else {
            if m.id == config.asr.model {
                problems += 1;
            }
            println!(
                "[{}] {role}: {} NOT installed -> swiftvoice models download {}",
                if m.id == config.asr.model { "FAIL" } else { "warn" },
                m.id,
                m.id
            );
        }
    }
    match crate::audio::capture::list_input_devices() {
        Ok(list) if list.is_empty() => {
            problems += 1;
            println!("[FAIL] no microphones found");
        }
        Ok(list) => println!("[ok]   microphones: {}", list.join(", ")),
        Err(e) => println!("[warn] microphones: {e}"),
    }
    #[cfg(windows)]
    {
        println!("[info] start with Windows: {}", if crate::platform::autostart_enabled() { "on" } else { "off" });
        println!("[info] running as administrator: {}", if crate::platform::is_elevated() { "yes" } else { "no" });
    }
    if problems == 0 {
        println!("\nEverything needed for dictation is in place.");
        Ok(())
    } else {
        bail!("{problems} problem(s) found")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Invocation> {
        parse(std::iter::once("swiftvoice").chain(args.iter().copied()).map(OsString::from))
    }

    #[test]
    fn no_arguments_runs_the_tray_app() {
        assert_eq!(p(&[]).unwrap().command, Command::Tray);
        let inv = p(&["--verbose", "--config", "x.toml"]).unwrap();
        assert!(inv.verbose);
        assert_eq!(inv.config, Some(PathBuf::from("x.toml")));
    }

    #[test]
    fn parses_the_benchmark_command() {
        let inv = p(&[
            "--benchmark",
            "--audio",
            "samples/test.wav",
            "--reference",
            "ref.txt",
            "--runs",
            "3",
            "--mode",
            "raw",
            "--json",
            "o.json",
        ])
        .unwrap();
        match inv.command {
            Command::Benchmark(b) => {
                assert_eq!(b.audio, Some(PathBuf::from("samples/test.wav")));
                assert_eq!(b.reference_file, Some(PathBuf::from("ref.txt")));
                assert_eq!(b.runs, Some(3));
                assert_eq!(b.mode, Some(PostMode::Raw));
                assert_eq!(b.json, Some(PathBuf::from("o.json")));
            }
            other => panic!("{other:?}"),
        }
        match p(&["--benchmark", "--audio", "a.wav", "--low-memory", "off"]).unwrap().command {
            Command::Benchmark(b) => assert_eq!(b.low_memory, Some(false)),
            other => panic!("{other:?}"),
        }
        assert!(p(&["--benchmark", "--audio", "a.wav", "--low-memory", "maybe"]).is_err());
        assert!(p(&["--benchmark"]).unwrap_err().to_string().contains("--audio"));
        assert!(p(&["--benchmark", "--audio", "a.wav", "--mode", "fancy"]).is_err());
    }

    #[test]
    fn parses_models_subcommands() {
        assert!(matches!(p(&["models"]).unwrap().command, Command::Models { cmd: ModelsCmd::List, .. }));
        match p(&["models", "download", "a", "b"]).unwrap().command {
            Command::Models { cmd: ModelsCmd::Download(ids), .. } => assert_eq!(ids, vec!["a", "b"]),
            other => panic!("{other:?}"),
        }
        assert!(p(&["models", "download"]).is_err());
        assert!(p(&["models", "frobnicate"]).is_err());
        assert!(matches!(p(&["models", "remove", "x"]).unwrap().command, Command::Models { cmd: ModelsCmd::Remove(_), .. }));
    }

    #[test]
    fn rejects_unknown_flags_and_conflicting_commands() {
        assert!(p(&["--frobnicate"]).is_err());
        assert!(p(&["--doctor", "--version"]).is_err());
        assert!(p(&["--record", "x.wav", "--seconds", "0"]).is_err());
        assert!(matches!(p(&["--transcribe", "a.wav", "--model", "m"]).unwrap().command, Command::Transcribe { .. }));
    }
}
