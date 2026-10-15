//! ONNX Runtime loading and session construction.
//!
//! ONNX Runtime is loaded dynamically from the library shipped next to the
//! executable (`onnxruntime.dll`, Microsoft's official 1.31.0 CPU build).
//! Loading by absolute path avoids picking up a different `onnxruntime.dll`
//! from System32 or PATH, and lets the tray app start (and explain the
//! problem) even when the library is missing.
//!
//! Telemetry: Microsoft's builds collect usage telemetry by default. The
//! Windows build logs ETW events, which SwiftVoice turns off with
//! `DisableTelemetryEvents` when it creates the environment. The Linux and
//! macOS builds of 1.31 also contain a 1DS uploader that starts when the
//! environment is created (before `DisableTelemetryEvents` can run) and
//! connects to mobile.events.data.microsoft.com; only the
//! `ORT_DISABLE_TELEMETRY` environment variable, read at initialisation,
//! stops it from being created at all. [`disable_telemetry`] sets it, on
//! every platform, at process start.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Context, Result};
use ort::environment::Environment;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::Session;

/// ONNX Runtime release SwiftVoice is built and tested against.
pub const ORT_VERSION: &str = "1.31.0";

#[cfg(windows)]
const LIB_NAMES: &[&str] = &["onnxruntime.dll"];
#[cfg(target_os = "macos")]
const LIB_NAMES: &[&str] = &["libonnxruntime.dylib"];
#[cfg(all(unix, not(target_os = "macos")))]
const LIB_NAMES: &[&str] = &["libonnxruntime.so", "libonnxruntime.so.1", "libonnxruntime.so.1.31.0"];

static ENV: OnceLock<Environment> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());
static LOW_MEMORY: AtomicBool = AtomicBool::new(true);

/// Models smaller than this always pre-pack: the copy is small and the
/// per-token decoder/joiner calls are latency-sensitive.
const LOW_MEMORY_MIN_FILE_BYTES: u64 = 64 << 20;

/// `asr.low_memory`: when true, large sessions created afterwards skip
/// weight pre-packing. Pre-packing keeps a second, kernel-friendly copy of
/// the MatMul weights (~250 MB extra for the Parakeet 0.6B int8 encoder) for
/// ~10-15% faster inference.
pub fn set_low_memory(enabled: bool) {
    LOW_MEMORY.store(enabled, Ordering::Relaxed);
}

/// Set `ORT_DISABLE_TELEMETRY=1` for this process. Call at the very start
/// of `main`, before any thread exists (changing the environment while
/// other threads may read it is not thread-safe on Unix).
pub fn disable_telemetry() {
    std::env::set_var("ORT_DISABLE_TELEMETRY", "1");
}

/// Whether ONNX Runtime's environment-variable telemetry switch is on.
pub fn telemetry_disabled_by_environment() -> bool {
    std::env::var("ORT_DISABLE_TELEMETRY")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on" | "y"))
        .unwrap_or(false)
}

/// Candidate library paths, in priority order.
pub fn library_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["SWIFTVOICE_ORT_DYLIB", "ORT_DYLIB_PATH"] {
        if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            out.push(PathBuf::from(v));
        }
    }
    let exe = crate::paths::exe_dir();
    for name in LIB_NAMES {
        out.push(exe.join(name));
        out.push(exe.join("lib").join(name));
    }
    out
}

/// Locate the ONNX Runtime library or explain how to install it.
pub fn find_library() -> Result<PathBuf> {
    let candidates = library_candidates();
    if let Some(found) = candidates.iter().find(|p| p.is_file()) {
        return Ok(found.clone());
    }
    let looked: Vec<String> = candidates.iter().map(|p| format!("  {}", p.display())).collect();
    Err(anyhow!(
        "ONNX Runtime {ORT_VERSION} was not found. SwiftVoice needs {} next to swiftvoice.exe.\n\
         Re-run the installer / scripts\\build-windows.ps1 (it downloads Microsoft's official build), \
         or set SWIFTVOICE_ORT_DYLIB to its full path.\nLooked in:\n{}",
        LIB_NAMES[0],
        looked.join("\n")
    ))
}

/// The process-wide ONNX Runtime environment (loaded on first use).
pub fn environment() -> Result<&'static Environment> {
    if let Some(env) = ENV.get() {
        return Ok(env);
    }
    let _guard = INIT_LOCK.lock().map_err(|_| anyhow!("ONNX Runtime init lock poisoned"))?;
    if let Some(env) = ENV.get() {
        return Ok(env);
    }
    let path = find_library()?;
    if !telemetry_disabled_by_environment() {
        log::warn!("ORT_DISABLE_TELEMETRY is not set; non-Windows ONNX Runtime builds may start their telemetry uploader");
    }
    let started = std::time::Instant::now();
    let builder = ort::init_from(&path).map_err(|e| anyhow!("cannot load ONNX Runtime: {e}"))?;
    let env = builder
        .with_name("swiftvoice")
        .with_telemetry(false)
        .build()
        .map_err(|e| anyhow!("cannot create the ONNX Runtime environment: {e}"))?;
    log::info!("ONNX Runtime loaded from {} in {} ms (telemetry disabled)", path.display(), started.elapsed().as_millis());
    let _ = ENV.set(env);
    Ok(ENV.get().expect("environment was just set"))
}

/// True once the runtime library has been loaded successfully.
pub fn is_loaded() -> bool {
    ENV.get().is_some()
}

fn builder_err(e: ort::Error<SessionBuilder>) -> anyhow::Error {
    anyhow!("{e}")
}

/// Common session options: full graph optimisation, the requested number
/// of intra-op threads (0 = ONNX Runtime default, one per physical core),
/// no memory arena, so memory used by one long utterance is returned
/// instead of being held for the life of the process, and weight
/// pre-packing per [`set_low_memory`].
pub fn load_session(path: &Path, threads: u32, what: &str) -> Result<Session> {
    let env = environment()?;
    if !path.is_file() {
        return Err(anyhow!("{what}: model file {} is missing", path.display()));
    }
    let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let skip_prepacking = LOW_MEMORY.load(Ordering::Relaxed) && file_bytes >= LOW_MEMORY_MIN_FILE_BYTES;
    let mut builder = Session::builder(env)
        .map_err(|e| anyhow!("{e}"))?
        .with_optimization_level(GraphOptimizationLevel::All)
        .map_err(builder_err)?
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
        .map_err(builder_err)?
        .with_prepacking(!skip_prepacking)
        .map_err(builder_err)?;
    if threads > 0 {
        builder = builder.with_intra_threads(threads as usize).map_err(builder_err)?;
    }
    builder.commit_from_file(path).map_err(|e| anyhow!("{e}")).with_context(|| format!("{what}: cannot load {}", path.display()))
}

/// Shape of a session input (dynamic dimensions are -1).
pub fn input_shape(session: &Session, index: usize) -> Option<Vec<i64>> {
    session.inputs().get(index).and_then(|o| o.dtype().tensor_shape().map(|s| s.to_vec()))
}

/// Shape of a session output (dynamic dimensions are -1).
pub fn output_shape(session: &Session, index: usize) -> Option<Vec<i64>> {
    session.outputs().get(index).and_then(|o| o.dtype().tensor_shape().map(|s| s.to_vec()))
}

/// Names of all inputs, in model order.
pub fn input_names(session: &Session) -> Vec<String> {
    session.inputs().iter().map(|o| o.name().to_string()).collect()
}

/// Names of all outputs, in model order.
pub fn output_names(session: &Session) -> Vec<String> {
    session.outputs().iter().map(|o| o.name().to_string()).collect()
}
