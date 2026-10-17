//! User configuration (`config.toml`).
//!
//! The file is human-edited, so it is loaded leniently: every key has a
//! default and unknown keys are ignored with a warning, so a file written
//! by an older version keeps working. Bad values still produce an error
//! that names the key. Tray changes are written back with `toml_edit`,
//! which preserves the user's comments and formatting.

mod template;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::hotkey::Hotkey;

pub use template::DEFAULT_CONFIG_TOML;

/// Default prompt for Smart mode (local LLM rewrite).
pub const DEFAULT_SMART_PROMPT: &str = "You are a dictation editor. Rewrite the dictated text into clear, correctly punctuated prose. Keep the original language, meaning, names, numbers and technical terms. Do not add information, do not answer questions contained in the text, and do not add greetings, quotes or explanations. Output only the rewritten text.";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub hotkeys: HotkeyConfig,
    pub audio: AudioConfig,
    pub vad: VadConfig,
    pub asr: AsrConfig,
    pub postprocess: PostprocessConfig,
    pub insertion: InsertionConfig,
    pub overlay: OverlayConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Start SwiftVoice when you sign in to Windows.
    pub autostart: bool,
    /// error | warn | info | debug | trace
    pub log_level: String,
    /// Folder that holds downloaded models. Empty = default location.
    pub models_dir: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        GeneralConfig { autostart: false, log_level: "info".into(), models_dir: String::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    /// Hold to dictate, release to insert.
    pub push_to_talk: String,
    /// Press once to start, press again to stop; the VAD endpoint can also insert on a pause.
    pub toggle: String,
    /// Cancels the current dictation. Registered only while listening.
    pub cancel: String,
    /// Push-to-talk presses shorter than this with no speech are ignored.
    pub min_hold_ms: u32,
    /// Audio kept after releasing push-to-talk mid-word, so the last
    /// syllable is not clipped. Skipped when you already paused.
    pub release_tail_ms: u32,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        HotkeyConfig {
            push_to_talk: "Ctrl+Alt+Space".into(),
            toggle: "Ctrl+Alt+V".into(),
            cancel: "Escape".into(),
            min_hold_ms: 180,
            release_tail_ms: 150,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// "default" or the exact device name shown in the tray menu.
    pub device: String,
    /// Input gain in dB (-20 ..= 30).
    pub gain_db: f32,
    /// RNNoise-based local noise suppression.
    pub noise_suppression: bool,
    /// Keep the microphone open this long after a dictation so the next one
    /// starts instantly (0 = close immediately).
    pub keep_warm_secs: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig { device: "default".into(), gain_db: 0.0, noise_suppression: false, keep_warm_secs: 15 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VadEngine {
    Silero,
    Energy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VadConfig {
    /// silero (neural, recommended) | energy (built in, no model needed)
    pub engine: VadEngine,
    /// Speech probability that starts speech (0.1 ..= 0.95).
    pub threshold: f32,
    /// Silence after speech that triggers early (speculative) transcription.
    pub pause_ms: u32,
    /// Silence that ends a dictation in toggle mode; 0 = only the toggle key stops it.
    pub endpoint_ms: u32,
    /// Audio kept before detected speech onset.
    pub pre_roll_ms: u32,
    /// Toggle mode stops if no speech is heard for this long.
    pub no_speech_timeout_ms: u32,
    /// Toggle mode: keep listening and insert each utterance until stopped.
    pub continuous: bool,
    /// Continuous mode stops after this many seconds without speech.
    pub idle_stop_secs: u32,
    /// Energy VAD: dB above the tracked noise floor that counts as speech.
    pub energy_margin_db: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        VadConfig {
            engine: VadEngine::Silero,
            threshold: 0.5,
            pause_ms: 200,
            endpoint_ms: 800,
            pre_roll_ms: 300,
            no_speech_timeout_ms: 8000,
            continuous: false,
            idle_stop_secs: 20,
            energy_margin_db: 9.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AsrConfig {
    /// Model id from `swiftvoice models list`.
    pub model: String,
    /// "auto" or an ISO 639-1 code such as "en", "de", "bn".
    pub language: String,
    /// Model used for languages the main model does not support.
    pub fallback_model: String,
    /// Inference threads; 0 = ONNX Runtime / whisper.cpp default.
    pub threads: u32,
    /// Skip ONNX Runtime weight pre-packing for large models: Parakeet 0.6B
    /// needs ~250 MB less RAM and loads faster; transcription is ~10-15%
    /// slower (docs/BENCHMARKS.md).
    pub low_memory: bool,
    /// Commit dictation at the next pause after this many seconds, so each
    /// transcription (and the wait after you stop) stays short.
    pub commit_after_secs: u32,
    /// Unload the model after this many idle seconds (0 = keep loaded).
    pub unload_after_idle_secs: u32,
    /// Run one silent inference after loading so the first dictation is fast.
    pub warm_up: bool,
}

impl Default for AsrConfig {
    fn default() -> Self {
        AsrConfig {
            model: "parakeet-tdt-0.6b-v3".into(),
            language: "auto".into(),
            fallback_model: "whisper-large-v3-turbo-q5_0".into(),
            threads: 0,
            low_memory: true,
            commit_after_secs: 6,
            unload_after_idle_secs: 0,
            warm_up: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostMode {
    Raw,
    Clean,
    Smart,
}

impl PostMode {
    pub fn label(self) -> &'static str {
        match self {
            PostMode::Raw => "Raw",
            PostMode::Clean => "Clean",
            PostMode::Smart => "Smart (local LLM)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PostprocessConfig {
    /// raw | clean | smart
    pub mode: PostMode,
    pub remove_fillers: bool,
    pub fillers: Vec<String>,
    pub remove_repeats: bool,
    pub capitalize: bool,
    /// "new line", "new paragraph", "open bracket", ... become symbols.
    pub voice_commands: bool,
    /// Also treat "comma", "period", "colon", ... as punctuation commands.
    pub punctuation_commands: bool,
    /// Add a full stop when the text ends without punctuation.
    pub ensure_final_punctuation: bool,
    /// Custom vocabulary file. Empty = vocabulary.toml next to config.toml.
    pub vocabulary_file: String,
    pub smart: SmartConfig,
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        PostprocessConfig {
            mode: PostMode::Clean,
            remove_fillers: true,
            fillers: ["um", "umm", "uh", "uhh", "uhm", "er", "erm", "ah", "hmm", "mm", "mhm"].iter().map(|s| s.to_string()).collect(),
            remove_repeats: true,
            capitalize: true,
            voice_commands: true,
            punctuation_commands: false,
            ensure_final_punctuation: false,
            vocabulary_file: String::new(),
            smart: SmartConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmartProvider {
    /// Ollama native API (`/api/chat`).
    Ollama,
    /// OpenAI-compatible local server such as LM Studio (`/v1/chat/completions`).
    Openai,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartConfig {
    pub provider: SmartProvider,
    /// Must be a loopback http:// URL; anything else is rejected.
    pub endpoint: String,
    pub model: String,
    /// If the LLM is slower than this, the Clean result is inserted instead.
    pub timeout_ms: u32,
    pub temperature: f32,
    pub prompt: String,
}

impl Default for SmartConfig {
    fn default() -> Self {
        SmartConfig {
            provider: SmartProvider::Ollama,
            endpoint: "http://127.0.0.1:11434".into(),
            model: "llama3.2:3b".into(),
            timeout_ms: 4000,
            temperature: 0.2,
            prompt: DEFAULT_SMART_PROMPT.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InsertMethod {
    /// Paste through the clipboard (restored afterwards); type if the
    /// clipboard cannot be used.
    Auto,
    Paste,
    /// Type with Unicode keyboard events; never touches the clipboard.
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InsertTarget {
    /// Insert into whatever window has focus when the text is ready.
    Focused,
    /// Insert into the window that had focus when dictation started; if it
    /// cannot be focused again the text is left on the clipboard.
    Original,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InsertionConfig {
    pub method: InsertMethod,
    pub target: InsertTarget,
    /// Append a space so consecutive dictations do not run together.
    pub append_space: bool,
    pub restore_clipboard: bool,
    /// Delay before the previous clipboard content is restored after pasting.
    pub restore_delay_ms: u32,
    pub paste_shortcut: String,
    /// Wait up to this long for you to release Ctrl/Alt/Shift/Win.
    pub modifier_wait_ms: u32,
    /// Clipboard contents larger than this are not snapshotted; the text is
    /// typed instead so your clipboard is never lost.
    pub max_clipboard_snapshot_mb: u32,
    /// Process names (e.g. "game.exe") that always receive typed text.
    pub type_apps: Vec<String>,
    /// Per-process paste shortcut overrides (process name -> shortcut).
    pub paste_shortcuts: BTreeMap<String, String>,
}

impl Default for InsertionConfig {
    fn default() -> Self {
        let mut paste_shortcuts = BTreeMap::new();
        for (app, keys) in [
            ("alacritty.exe", "Ctrl+Shift+V"),
            ("mintty.exe", "Shift+Insert"),
            ("putty.exe", "Shift+Insert"),
            ("wezterm-gui.exe", "Ctrl+Shift+V"),
        ] {
            paste_shortcuts.insert(app.to_string(), keys.to_string());
        }
        InsertionConfig {
            method: InsertMethod::Auto,
            target: InsertTarget::Focused,
            append_space: true,
            restore_clipboard: true,
            restore_delay_ms: 300,
            paste_shortcut: "Ctrl+V".into(),
            modifier_wait_ms: 400,
            max_clipboard_snapshot_mb: 32,
            type_apps: Vec::new(),
            paste_shortcuts,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayPosition {
    Bottom,
    Top,
    BottomLeft,
    TopLeft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub position: OverlayPosition,
    /// 0.0 ..= 1.0; very low values make the dot faint.
    pub opacity: f32,
    /// How long the result stays visible after insertion.
    pub hide_after_ms: u32,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        OverlayConfig { enabled: true, position: OverlayPosition::Bottom, opacity: 0.94, hide_after_ms: 900 }
    }
}

fn collect_unknown(file: &toml::Value, known: &toml::Value, prefix: &str, out: &mut Vec<String>) {
    let (Some(file), Some(known)) = (file.as_table(), known.as_table()) else { return };
    for (key, value) in file {
        let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        match known.get(key.as_str()) {
            None => out.push(path),
            Some(known_value) => collect_unknown(value, known_value, &path, out),
        }
    }
}

impl Config {
    /// Dotted paths of keys in `text` that this build does not know. The
    /// loader warns about them and carries on; `--doctor` prints them.
    pub fn unknown_keys(text: &str) -> Vec<String> {
        let Ok(cfg) = toml::from_str::<Config>(text) else { return Vec::new() };
        let Ok(known_text) = toml::to_string(&cfg) else { return Vec::new() };
        let (Ok(file), Ok(known)) = (toml::from_str::<toml::Value>(text), toml::from_str::<toml::Value>(&known_text)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_unknown(&file, &known, "", &mut out);
        out
    }

    /// Parse and validate a configuration document. Unknown keys are
    /// ignored with a warning so config files from older versions keep
    /// working.
    pub fn from_toml_str(text: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
        for key in Config::unknown_keys(text) {
            log::warn!("config: ignoring unknown key \"{key}\"");
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load `path`, creating it from the commented template when missing.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).with_context(|| format!("cannot create config folder {}", dir.display()))?;
            }
            write_atomic(path, DEFAULT_CONFIG_TOML.as_bytes())
                .with_context(|| format!("cannot write default config {}", path.display()))?;
            return Ok(Config::default());
        }
        let text = fs::read_to_string(path).with_context(|| format!("cannot read config {}", path.display()))?;
        Config::from_toml_str(&text).with_context(|| format!("invalid config file {}", path.display()))
    }

    /// Check value ranges and cross-field rules. All problems are reported
    /// at once so the user can fix the file in one pass.
    pub fn validate(&self) -> Result<()> {
        let mut errs: Vec<String> = Vec::new();
        let level = self.general.log_level.to_ascii_lowercase();
        if !["error", "warn", "info", "debug", "trace"].contains(&level.as_str()) {
            errs.push(format!("general.log_level = \"{}\" must be one of error, warn, info, debug, trace", self.general.log_level));
        }
        let mut hotkeys = Vec::new();
        for (name, value, required) in [
            ("hotkeys.push_to_talk", &self.hotkeys.push_to_talk, false),
            ("hotkeys.toggle", &self.hotkeys.toggle, false),
            ("hotkeys.cancel", &self.hotkeys.cancel, false),
        ] {
            if value.trim().is_empty() {
                if required {
                    errs.push(format!("{name} must not be empty"));
                }
                continue;
            }
            match Hotkey::parse(value) {
                Ok(hk) => hotkeys.push((name, hk)),
                Err(e) => errs.push(format!("{name} = \"{value}\": {e}")),
            }
        }
        if self.hotkeys.push_to_talk.trim().is_empty() && self.hotkeys.toggle.trim().is_empty() {
            errs.push("at least one of hotkeys.push_to_talk and hotkeys.toggle must be set".into());
        }
        for i in 0..hotkeys.len() {
            for j in i + 1..hotkeys.len() {
                if hotkeys[i].1 == hotkeys[j].1 {
                    errs.push(format!("{} and {} use the same shortcut", hotkeys[i].0, hotkeys[j].0));
                }
            }
        }
        if let Err(e) = Hotkey::parse(&self.insertion.paste_shortcut) {
            errs.push(format!("insertion.paste_shortcut: {e}"));
        }
        for (app, keys) in &self.insertion.paste_shortcuts {
            if let Err(e) = Hotkey::parse(keys) {
                errs.push(format!("insertion.paste_shortcuts.\"{app}\": {e}"));
            }
        }
        range_f32(&mut errs, "audio.gain_db", self.audio.gain_db, -20.0, 30.0);
        range_u32(&mut errs, "audio.keep_warm_secs", self.audio.keep_warm_secs, 0, 3600);
        range_f32(&mut errs, "vad.threshold", self.vad.threshold, 0.1, 0.95);
        range_u32(&mut errs, "vad.pause_ms", self.vad.pause_ms, 64, 2000);
        range_u32(&mut errs, "vad.endpoint_ms", self.vad.endpoint_ms, 0, 10_000);
        if self.vad.endpoint_ms != 0 && self.vad.endpoint_ms < self.vad.pause_ms {
            errs.push("vad.endpoint_ms must be >= vad.pause_ms (or 0 to stop only with the toggle key)".into());
        }
        range_u32(&mut errs, "vad.pre_roll_ms", self.vad.pre_roll_ms, 0, 2000);
        range_u32(&mut errs, "vad.no_speech_timeout_ms", self.vad.no_speech_timeout_ms, 1000, 120_000);
        range_u32(&mut errs, "vad.idle_stop_secs", self.vad.idle_stop_secs, 2, 3600);
        range_f32(&mut errs, "vad.energy_margin_db", self.vad.energy_margin_db, 3.0, 30.0);
        if self.asr.model.trim().is_empty() {
            errs.push("asr.model must not be empty".into());
        }
        if !crate::asr::catalog::is_valid_language_setting(&self.asr.language) {
            errs.push(format!("asr.language = \"{}\" must be \"auto\" or a two- or three-letter language code", self.asr.language));
        }
        range_u32(&mut errs, "asr.threads", self.asr.threads, 0, 64);
        range_u32(&mut errs, "asr.commit_after_secs", self.asr.commit_after_secs, 4, 25);
        range_u32(&mut errs, "postprocess.smart.timeout_ms", self.postprocess.smart.timeout_ms, 200, 60_000);
        range_f32(&mut errs, "postprocess.smart.temperature", self.postprocess.smart.temperature, 0.0, 2.0);
        if self.postprocess.mode == PostMode::Smart || !self.postprocess.smart.endpoint.is_empty() {
            if let Err(e) = crate::postprocess::smart::LoopbackUrl::parse(&self.postprocess.smart.endpoint) {
                errs.push(format!("postprocess.smart.endpoint: {e}"));
            }
        }
        range_u32(&mut errs, "insertion.restore_delay_ms", self.insertion.restore_delay_ms, 50, 5000);
        range_u32(&mut errs, "insertion.modifier_wait_ms", self.insertion.modifier_wait_ms, 0, 3000);
        range_u32(&mut errs, "insertion.max_clipboard_snapshot_mb", self.insertion.max_clipboard_snapshot_mb, 1, 512);
        range_f32(&mut errs, "overlay.opacity", self.overlay.opacity, 0.0, 1.0);
        range_u32(&mut errs, "overlay.hide_after_ms", self.overlay.hide_after_ms, 0, 10_000);
        if errs.is_empty() {
            Ok(())
        } else {
            bail!("{}", errs.join("\n"))
        }
    }

    /// Resolve the models folder (`general.models_dir` or the default).
    pub fn models_dir(&self) -> PathBuf {
        if self.general.models_dir.trim().is_empty() {
            crate::paths::default_models_dir()
        } else {
            PathBuf::from(crate::paths::expand_env(self.general.models_dir.trim()))
        }
    }

    /// Resolve the vocabulary file path.
    pub fn vocabulary_path(&self, config_path: &Path) -> PathBuf {
        if self.postprocess.vocabulary_file.trim().is_empty() {
            config_path.with_file_name("vocabulary.toml")
        } else {
            PathBuf::from(crate::paths::expand_env(self.postprocess.vocabulary_file.trim()))
        }
    }

    pub fn log_level_filter(&self) -> log::LevelFilter {
        match self.general.log_level.to_ascii_lowercase().as_str() {
            "error" => log::LevelFilter::Error,
            "warn" => log::LevelFilter::Warn,
            "debug" => log::LevelFilter::Debug,
            "trace" => log::LevelFilter::Trace,
            _ => log::LevelFilter::Info,
        }
    }
}

fn range_u32(errs: &mut Vec<String>, name: &str, v: u32, lo: u32, hi: u32) {
    if v < lo || v > hi {
        errs.push(format!("{name} = {v} must be between {lo} and {hi}"));
    }
}

fn range_f32(errs: &mut Vec<String>, name: &str, v: f32, lo: f32, hi: f32) {
    if !v.is_finite() || v < lo || v > hi {
        errs.push(format!("{name} = {v} must be between {lo} and {hi}"));
    }
}

/// A single value written back to config.toml (from the tray menu).
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// Update one key (e.g. `("audio", "device")` or `("postprocess.smart",
/// "model")`) in place, keeping comments and layout, then re-validate.
pub fn update_setting(path: &Path, table: &str, key: &str, value: Setting) -> Result<Config> {
    let text = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?
    } else {
        DEFAULT_CONFIG_TOML.to_string()
    };
    let new_text = apply_setting(&text, table, key, value)?;
    let cfg = Config::from_toml_str(&new_text).with_context(|| format!("refusing to write an invalid value for {table}.{key}"))?;
    write_atomic(path, new_text.as_bytes()).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(cfg)
}

/// Pure text transform behind [`update_setting`].
pub fn apply_setting(text: &str, table: &str, key: &str, value: Setting) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("config.toml is not valid TOML")?;
    let mut item: &mut toml_edit::Item = doc.as_item_mut();
    for part in table.split('.').filter(|p| !p.is_empty()) {
        let tbl = item.as_table_like_mut().with_context(|| format!("[{table}] is not a table in config.toml"))?;
        if tbl.get(part).is_none() {
            tbl.insert(part, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        item = tbl.get_mut(part).expect("just inserted");
    }
    let tbl = item.as_table_like_mut().with_context(|| format!("[{table}] is not a table in config.toml"))?;
    let new_value = match value {
        Setting::Bool(b) => toml_edit::value(b),
        Setting::Int(i) => toml_edit::value(i),
        Setting::Float(f) => toml_edit::value(f),
        Setting::Str(s) => toml_edit::value(s),
    };
    // Keep the existing inline comment/decoration on the value if present.
    match tbl.get_mut(key) {
        Some(existing) if existing.is_value() => {
            let decor = existing.as_value().map(|v| v.decor().clone());
            *existing = new_value;
            if let (Some(decor), Some(v)) = (decor, existing.as_value_mut()) {
                *v.decor_mut() = decor;
            }
        }
        _ => {
            tbl.insert(key, new_value);
        }
    }
    Ok(doc.to_string())
}

/// Write via a temporary file and rename so a crash never leaves a
/// half-written config behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_code_defaults() {
        let parsed = Config::from_toml_str(DEFAULT_CONFIG_TOML).expect("template parses");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::from_toml_str("").unwrap(), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let cfg = Config::from_toml_str("[audio]\ngain_db = 6.0\n[postprocess]\nmode = \"raw\"\n").unwrap();
        assert_eq!(cfg.audio.gain_db, 6.0);
        assert_eq!(cfg.postprocess.mode, PostMode::Raw);
        assert_eq!(cfg.hotkeys, HotkeyConfig::default());
    }

    #[test]
    fn unknown_keys_are_ignored_with_a_warning() {
        let text = "[audio]\ngian_db = 3.0\n[nope]\nx = 1\n";
        let cfg = Config::from_toml_str(text).expect("unknown keys no longer fail the load");
        assert_eq!(cfg.audio.gain_db, 0.0);
        assert_eq!(Config::unknown_keys(text), ["audio.gian_db", "nope"]);
    }

    #[test]
    fn removed_partial_keys_from_old_configs_still_load() {
        let text = "[asr]\npartial_results = true\npartial_interval_ms = 700\n[overlay]\nshow_partials = true\n";
        let cfg = Config::from_toml_str(text).expect("config files from older versions keep loading");
        assert_eq!(cfg.asr.commit_after_secs, 6);
        let mut unknown = Config::unknown_keys(text);
        unknown.sort();
        assert_eq!(unknown, ["asr.partial_interval_ms", "asr.partial_results", "overlay.show_partials"]);
    }

    #[test]
    fn bad_enum_value_is_rejected() {
        let err = Config::from_toml_str("[postprocess]\nmode = \"fancy\"\n").unwrap_err().to_string();
        assert!(err.contains("fancy") || err.contains("variant"), "{err}");
    }

    #[test]
    fn out_of_range_values_are_all_reported() {
        let err = Config::from_toml_str("[vad]\nthreshold = 2.0\npause_ms = 10\n[overlay]\nopacity = 1.2\n").unwrap_err().to_string();
        assert!(err.contains("vad.threshold"), "{err}");
        assert!(err.contains("vad.pause_ms"), "{err}");
        assert!(err.contains("overlay.opacity"), "{err}");
    }

    #[test]
    fn overlay_accepts_corner_positions_and_low_opacity() {
        let cfg = Config::from_toml_str("[overlay]\nposition = \"bottom-left\"\nopacity = 0.05\n").unwrap();
        assert_eq!(cfg.overlay.position, OverlayPosition::BottomLeft);
        assert_eq!(cfg.overlay.opacity, 0.05);
    }

    #[test]
    fn invalid_hotkey_and_duplicate_hotkeys_are_rejected() {
        let err = Config::from_toml_str("[hotkeys]\npush_to_talk = \"Ctrl+Nope\"\n").unwrap_err().to_string();
        assert!(err.contains("hotkeys.push_to_talk"), "{err}");
        let err = Config::from_toml_str("[hotkeys]\npush_to_talk = \"Ctrl+Alt+V\"\ntoggle = \"ctrl+alt+v\"\n").unwrap_err().to_string();
        assert!(err.contains("same shortcut"), "{err}");
    }

    #[test]
    fn smart_endpoint_must_be_loopback() {
        let err = Config::from_toml_str("[postprocess]\nmode = \"smart\"\n[postprocess.smart]\nendpoint = \"http://example.com:11434\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("loopback"), "{err}");
        assert!(Config::from_toml_str(
            "[postprocess]\nmode = \"smart\"\n[postprocess.smart]\nendpoint = \"http://localhost:1234\"\nprovider = \"openai\"\n"
        )
        .is_ok());
    }

    #[test]
    fn apply_setting_preserves_comments() {
        let text = "# my settings\n[audio]\n# which mic\ndevice = \"default\" # keep\ngain_db = 0.0\n";
        let out = apply_setting(text, "audio", "device", Setting::Str("USB Mic".into())).unwrap();
        assert!(out.contains("# my settings"));
        assert!(out.contains("# which mic"));
        assert!(out.contains("device = \"USB Mic\""), "{out}");
        assert!(out.contains("# keep"), "{out}");
        let cfg = Config::from_toml_str(&out).unwrap();
        assert_eq!(cfg.audio.device, "USB Mic");
    }

    #[test]
    fn apply_setting_creates_missing_nested_tables() {
        let out = apply_setting("", "postprocess.smart", "model", Setting::Str("qwen2.5:3b".into())).unwrap();
        let cfg = Config::from_toml_str(&out).unwrap();
        assert_eq!(cfg.postprocess.smart.model, "qwen2.5:3b");
        let out = apply_setting(&out, "general", "autostart", Setting::Bool(true)).unwrap();
        assert!(Config::from_toml_str(&out).unwrap().general.autostart);
    }

    #[test]
    fn update_setting_round_trips_on_disk() {
        let dir = crate::paths::test_dir("config_update");
        let path = dir.join("config.toml");
        let cfg = Config::load_or_create(&path).unwrap();
        assert_eq!(cfg, Config::default());
        let cfg = update_setting(&path, "asr", "language", Setting::Str("de".into())).unwrap();
        assert_eq!(cfg.asr.language, "de");
        let reloaded = Config::load_or_create(&path).unwrap();
        assert_eq!(reloaded.asr.language, "de");
        // An invalid value is refused and the file is left unchanged.
        assert!(update_setting(&path, "vad", "threshold", Setting::Float(9.0)).is_err());
        assert_eq!(Config::load_or_create(&path).unwrap().vad.threshold, 0.5);
        let _ = fs::remove_dir_all(dir);
    }
}
