//! Crash-safe file logging.
//!
//! Every record is written and flushed immediately (no background buffer
//! that could be lost in a crash), the file rotates at 5 MB keeping three
//! old files, and a panic hook records the panic message, location and
//! backtrace before the process exits. Logs never contain audio, transcripts
//! or clipboard contents unless `log_level = "trace"` is set explicitly (and
//! even then only transcript text, never audio or clipboard data).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const KEEP_FILES: usize = 3;

struct FileLogger {
    state: Mutex<LogState>,
    to_stderr: bool,
}

struct LogState {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

/// Path of the active log file, if logging was initialised.
pub fn log_file_path() -> Option<PathBuf> {
    LOGGER.get().map(|l| l.state.lock().map(|s| s.path.clone()).unwrap_or_default())
}

/// Initialise logging into `dir/swiftvoice.log`. Safe to call once; later
/// calls only update the level.
pub fn init(dir: &Path, level: LevelFilter, to_stderr: bool) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join("swiftvoice.log");
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let written = file.metadata().map(|m| m.len()).unwrap_or(0);
    let logger =
        LOGGER.get_or_init(|| FileLogger { state: Mutex::new(LogState { path: path.clone(), file: Some(file), written }), to_stderr });
    if log::set_logger(logger).is_ok() {
        install_panic_hook();
    }
    log::set_max_level(level);
    Ok(path)
}

/// Change the level at run time (config reload).
pub fn set_level(level: LevelFilter) {
    log::set_max_level(level);
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Third-party crates are noisy at debug/trace; keep them at info.
        let target = record.target();
        if !target.starts_with("swiftvoice") && record.level() > log::Level::Info {
            return;
        }
        let line = format!("{} {:<5} {}: {}\n", timestamp_utc(), record.level(), target, record.args());
        if self.to_stderr {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
        if let Ok(mut st) = self.state.lock() {
            st.write_line(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(f) = st.file.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

impl LogState {
    fn write_line(&mut self, bytes: &[u8]) {
        if self.written + bytes.len() as u64 > MAX_BYTES {
            self.rotate();
        }
        if let Some(f) = self.file.as_mut() {
            if f.write_all(bytes).is_ok() {
                let _ = f.flush();
                self.written += bytes.len() as u64;
            }
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        for i in (1..KEEP_FILES).rev() {
            let from = rotated(&self.path, i);
            let to = rotated(&self.path, i + 1);
            let _ = fs::rename(&from, &to);
        }
        let _ = fs::rename(&self.path, rotated(&self.path, 1));
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.written = 0;
    }
}

fn rotated(path: &Path, n: usize) -> PathBuf {
    path.with_extension(format!("log.{n}"))
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location =
            info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_else(|| "unknown location".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".into());
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!("PANIC in thread '{thread}' at {location}: {payload}\n{backtrace}");
        log::logger().flush();
        previous(info);
    }));
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` without pulling in a date/time crate.
pub fn timestamp_utc() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format_utc(now.as_secs() as i64, now.subsec_millis())
}

fn format_utc(secs: i64, millis: u32) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Howard Hinnant's days-to-civil algorithm (proleptic Gregorian, UTC).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_utc(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_utc(951_782_400, 5), "2000-02-29T00:00:00.005Z");
        assert_eq!(format_utc(1_792_011_506, 123), "2026-10-14T20:58:26.123Z");
    }

    #[test]
    fn rotation_keeps_bounded_history() {
        let dir = crate::paths::test_dir("log_rotate");
        let path = dir.join("swiftvoice.log");
        let mut st = LogState { path: path.clone(), file: File::create(&path).ok(), written: MAX_BYTES - 4 };
        st.write_line(b"0123456789\n");
        assert!(rotated(&path, 1).exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "0123456789\n");
        for _ in 0..5 {
            st.written = MAX_BYTES;
            st.write_line(b"x\n");
        }
        assert!(rotated(&path, KEEP_FILES).exists());
        assert!(!rotated(&path, KEEP_FILES + 1).exists());
        let _ = fs::remove_dir_all(dir);
    }
}
