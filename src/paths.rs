//! Where SwiftVoice keeps its files.
//!
//! | What        | Windows                              | Other (development) |
//! |-------------|--------------------------------------|---------------------|
//! | config.toml | `%APPDATA%\SwiftVoice`               | `~/.config/swiftvoice` |
//! | logs        | `%APPDATA%\SwiftVoice\logs`          | `~/.config/swiftvoice/logs` |
//! | models      | `%LOCALAPPDATA%\SwiftVoice\models`   | `~/.local/share/swiftvoice/models` |
//!
//! Two overrides exist: `SWIFTVOICE_HOME` puts everything under one folder
//! (used by tests and the benchmark scripts), and a file named
//! `swiftvoice.portable` next to the executable enables portable mode, which
//! keeps everything next to the executable.

use std::env;
use std::path::{Path, PathBuf};

const PORTABLE_MARKER: &str = "swiftvoice.portable";

/// Folder that contains the running executable.
pub fn exe_dir() -> PathBuf {
    env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}

fn home_override() -> Option<PathBuf> {
    env::var_os("SWIFTVOICE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn is_portable() -> bool {
    exe_dir().join(PORTABLE_MARKER).exists()
}

/// Folder for config.toml, vocabulary.toml and logs.
pub fn config_dir() -> PathBuf {
    if let Some(home) = home_override() {
        return home;
    }
    if is_portable() {
        return exe_dir().join("data");
    }
    platform_config_dir()
}

#[cfg(windows)]
fn platform_config_dir() -> PathBuf {
    match env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("SwiftVoice"),
        None => exe_dir().join("data"),
    }
}

#[cfg(not(windows))]
fn platform_config_dir() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("swiftvoice");
    }
    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".config").join("swiftvoice"),
        None => exe_dir().join("data"),
    }
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn logs_dir() -> PathBuf {
    config_dir().join("logs")
}

/// Default models folder (models are large, so on Windows they live in the
/// non-roaming %LOCALAPPDATA% rather than %APPDATA%).
pub fn default_models_dir() -> PathBuf {
    if let Some(home) = home_override() {
        return home.join("models");
    }
    if is_portable() {
        return exe_dir().join("models");
    }
    platform_models_dir()
}

#[cfg(windows)]
fn platform_models_dir() -> PathBuf {
    match env::var_os("LOCALAPPDATA") {
        Some(local) => PathBuf::from(local).join("SwiftVoice").join("models"),
        None => exe_dir().join("models"),
    }
}

#[cfg(not(windows))]
fn platform_models_dir() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("swiftvoice").join("models");
    }
    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".local").join("share").join("swiftvoice").join("models"),
        None => exe_dir().join("models"),
    }
}

/// Expand `%VAR%` (Windows style) and a leading `~` in user-supplied paths.
pub fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match env::var(name) {
                    Ok(value) => out.push_str(&value),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    if let Some(stripped) = out.strip_prefix('~') {
        if stripped.is_empty() || stripped.starts_with('/') || stripped.starts_with('\\') {
            if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
                return format!("{}{}", PathBuf::from(home).display(), stripped);
            }
        }
    }
    out
}

/// A fresh, empty scratch folder for tests (not used by the app itself).
#[doc(hidden)]
pub fn test_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!("swiftvoice-test-{}-{}-{}", name, std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_percent_variables() {
        env::set_var("SWIFTVOICE_TEST_VAR", "C:\\Data");
        assert_eq!(expand_env("%SWIFTVOICE_TEST_VAR%\\models"), "C:\\Data\\models");
        assert_eq!(expand_env("%NO_SUCH_VAR_123%\\x"), "%NO_SUCH_VAR_123%\\x");
        assert_eq!(expand_env("100% sure"), "100% sure");
        assert_eq!(expand_env("plain"), "plain");
    }
}
