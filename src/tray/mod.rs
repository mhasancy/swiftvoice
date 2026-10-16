//! Tray icon status and menu model. `win.rs` renders it with the Win32
//! notification-area and popup-menu APIs.

pub mod icon_art;
#[cfg(windows)]
pub mod win;

use std::path::Path;

use crate::asr::catalog::{self, LANGUAGE_NAMES};
use crate::asr::download;
use crate::config::{Config, InsertMethod, PostMode};
use crate::events::{HotkeyKind, TrayCommand};
use icon_art::IconTone;

#[derive(Debug, Clone, PartialEq)]
pub enum TrayStatus {
    Starting,
    Loading(String),
    Ready,
    Listening,
    Processing,
    Downloading { model: String, percent: Option<u32> },
    NeedsModel,
    Error(String),
}

impl TrayStatus {
    pub fn tone(&self) -> IconTone {
        match self {
            TrayStatus::Ready => IconTone::Idle,
            TrayStatus::Listening => IconTone::Listening,
            TrayStatus::Processing => IconTone::Busy,
            TrayStatus::Starting | TrayStatus::Loading(_) | TrayStatus::Downloading { .. } => IconTone::Loading,
            TrayStatus::NeedsModel | TrayStatus::Error(_) => IconTone::Error,
        }
    }

    /// Tooltip text (Windows limits it to 127 characters).
    pub fn tooltip(&self, cfg: &Config) -> String {
        let body = match self {
            TrayStatus::Starting => "starting…".to_string(),
            TrayStatus::Loading(m) => format!("loading {m}…"),
            TrayStatus::Ready => format!("ready · hold {} or press {}", cfg.hotkeys.push_to_talk, cfg.hotkeys.toggle),
            TrayStatus::Listening => "listening…".into(),
            TrayStatus::Processing => "transcribing…".into(),
            TrayStatus::Downloading { model, percent } => match percent {
                Some(p) => format!("downloading {model} {p}%"),
                None => format!("downloading {model}…"),
            },
            TrayStatus::NeedsModel => "no speech model installed (right-click to download)".into(),
            TrayStatus::Error(e) => e.clone(),
        };
        let t = format!("SwiftVoice: {body}");
        t.chars().take(127).collect()
    }

    /// One-line status for the top of the menu. Kept short so the popup
    /// stays narrow (`tooltip` carries the full text).
    pub fn menu_title(&self) -> String {
        let body = match self {
            TrayStatus::Starting => "starting…".to_string(),
            TrayStatus::Loading(m) => format!("loading {m}…"),
            TrayStatus::Ready => "ready".into(),
            TrayStatus::Listening => "listening…".into(),
            TrayStatus::Processing => "transcribing…".into(),
            TrayStatus::Downloading { model, percent } => match percent {
                Some(p) => format!("downloading {model} {p}%"),
                None => format!("downloading {model}…"),
            },
            TrayStatus::NeedsModel => "no speech model installed".into(),
            TrayStatus::Error(e) => e.clone(),
        };
        format!("SwiftVoice: {body}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MenuEntry {
    Item { label: String, cmd: TrayCommand, enabled: bool, checked: bool, bold: bool },
    Label(String),
    Separator,
    Submenu { label: String, items: Vec<MenuEntry>, enabled: bool },
}

fn item(label: impl Into<String>, cmd: TrayCommand) -> MenuEntry {
    MenuEntry::Item { label: label.into(), cmd, enabled: true, checked: false, bold: false }
}

fn check(label: impl Into<String>, cmd: TrayCommand, checked: bool) -> MenuEntry {
    MenuEntry::Item { label: label.into(), cmd, enabled: true, checked, bold: false }
}

pub struct MenuState<'a> {
    pub cfg: &'a Config,
    pub status: &'a TrayStatus,
    pub listening: bool,
    pub devices: &'a [String],
    pub models_dir: &'a Path,
    pub downloading: Option<&'a str>,
    pub autostart: bool,
}

pub fn build_menu(s: &MenuState) -> Vec<MenuEntry> {
    let cfg = s.cfg;
    let mut m = Vec::new();
    m.push(MenuEntry::Label(s.status.menu_title()));
    m.push(MenuEntry::Separator);
    let start_label =
        if s.listening { format!("Stop listening\t{}", cfg.hotkeys.toggle) } else { format!("Start listening\t{}", cfg.hotkeys.toggle) };
    m.push(MenuEntry::Item { label: start_label, cmd: TrayCommand::StartStop, enabled: true, checked: false, bold: true });
    m.push(MenuEntry::Label(format!("Push-to-talk\t{}", cfg.hotkeys.push_to_talk)));
    m.push(MenuEntry::Separator);

    m.push(MenuEntry::Submenu {
        label: "Hotkeys".into(),
        enabled: true,
        items: vec![
            item(format!("Change push-to-talk ({})…", cfg.hotkeys.push_to_talk), TrayCommand::UiChangeHotkey(HotkeyKind::PushToTalk)),
            item(format!("Change toggle ({})…", cfg.hotkeys.toggle), TrayCommand::UiChangeHotkey(HotkeyKind::Toggle)),
            item("Reset to defaults", TrayCommand::ResetHotkeys),
        ],
    });

    let mut mics = vec![check(
        "Default microphone",
        TrayCommand::SelectMicrophone("default".into()),
        cfg.audio.device.eq_ignore_ascii_case("default"),
    )];
    for d in s.devices {
        mics.push(check(d.clone(), TrayCommand::SelectMicrophone(d.clone()), *d == cfg.audio.device));
    }
    if s.devices.is_empty() {
        mics.push(MenuEntry::Label("(no other microphones found)".into()));
    }
    mics.push(MenuEntry::Separator);
    mics.push(check("Noise suppression", TrayCommand::ToggleNoiseSuppression, cfg.audio.noise_suppression));
    let gains = [-6.0f32, 0.0, 6.0, 12.0];
    mics.push(MenuEntry::Submenu {
        label: format!("Input gain ({:+.0} dB)", cfg.audio.gain_db),
        enabled: true,
        items: gains.iter().map(|g| check(format!("{g:+.0} dB"), TrayCommand::SetGain(*g), (cfg.audio.gain_db - g).abs() < 0.01)).collect(),
    });
    m.push(MenuEntry::Submenu { label: "Microphone".into(), items: mics, enabled: true });

    let mut models = Vec::new();
    for info in catalog::recognizers() {
        let installed = download::is_installed(s.models_dir, info);
        let selected = info.id == cfg.asr.model;
        let fallback = info.id == cfg.asr.fallback_model;
        let mut label = info.display.to_string();
        if fallback {
            label.push_str(" (fallback)");
        }
        label.push_str(&format!("  —  {}", info.summary()));
        let cmd = if installed {
            TrayCommand::SelectModel(info.id.into())
        } else {
            label = format!("Download {} ({})", info.display, catalog::human_bytes(info.download_bytes));
            TrayCommand::UiConfirmDownload(info.id.into())
        };
        let enabled = s.downloading.is_none() || installed;
        models.push(MenuEntry::Item { label, cmd, enabled, checked: selected && installed, bold: false });
    }
    let vad = catalog::find("silero-vad").expect("catalog has silero-vad");
    if !download::is_installed(s.models_dir, vad) {
        models.push(MenuEntry::Separator);
        models.push(item(
            format!("Download {} ({})", vad.display, catalog::human_bytes(vad.download_bytes)),
            TrayCommand::UiConfirmDownload(vad.id.into()),
        ));
    }
    if let Some(id) = s.downloading {
        models.push(MenuEntry::Separator);
        models.push(item(format!("Cancel download of {id}"), TrayCommand::CancelDownload));
    }
    models.push(MenuEntry::Separator);
    models.push(item("Open models folder", TrayCommand::OpenModelsFolder));
    models.push(item("Choose models folder…", TrayCommand::UiChooseModelsFolder));
    m.push(MenuEntry::Submenu { label: "Engine & model".into(), items: models, enabled: true });

    let main = catalog::find(&cfg.asr.model);
    let fb = catalog::find(&cfg.asr.fallback_model);
    let langs: Vec<MenuEntry> = LANGUAGE_NAMES
        .iter()
        .filter(|(code, _)| {
            main.map(|i| i.supports_language(code)).unwrap_or(false) || fb.map(|i| i.supports_language(code)).unwrap_or(false)
        })
        .map(|(code, name)| check(*name, TrayCommand::SetLanguage((*code).into()), cfg.asr.language == *code))
        .collect();
    m.push(MenuEntry::Submenu { label: format!("Language ({})", catalog::language_name(&cfg.asr.language)), items: langs, enabled: true });

    let modes = [PostMode::Raw, PostMode::Clean, PostMode::Smart];
    let mut text = Vec::new();
    for mode in modes {
        text.push(check(mode.label(), TrayCommand::SetMode(mode), cfg.postprocess.mode == mode));
    }
    text.push(MenuEntry::Separator);
    text.push(item("Edit vocabulary…", TrayCommand::EditVocabulary));
    m.push(MenuEntry::Submenu { label: format!("Text: {}", cfg.postprocess.mode.label()), items: text, enabled: true });

    let methods = [
        (InsertMethod::Auto, "Auto (paste, restore clipboard)"),
        (InsertMethod::Paste, "Always paste"),
        (InsertMethod::Type, "Always type (never uses clipboard)"),
    ];
    let mut options = Vec::new();
    for (method, label) in methods {
        options.push(check(label, TrayCommand::SetInsertMethod(method), cfg.insertion.method == method));
    }
    options.push(MenuEntry::Separator);
    options.push(check("Show overlay", TrayCommand::ToggleOverlay, cfg.overlay.enabled));
    options.push(check("Live partial text", TrayCommand::TogglePartials, cfg.overlay.show_partials && cfg.asr.partial_results));
    options.push(check("Keep listening after each utterance (toggle mode)", TrayCommand::ToggleContinuous, cfg.vad.continuous));
    m.push(MenuEntry::Submenu { label: "Options".into(), items: options, enabled: true });

    m.push(MenuEntry::Separator);
    m.push(check("Start with Windows", TrayCommand::ToggleAutostart, s.autostart));
    m.push(item("Open settings file…", TrayCommand::OpenSettings));
    m.push(item("Open logs folder", TrayCommand::OpenLogs));
    m.push(item("About SwiftVoice", TrayCommand::UiAbout));
    m.push(MenuEntry::Separator);
    m.push(item("Quit", TrayCommand::Quit));
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(entries: &[MenuEntry], out: &mut Vec<TrayCommand>) {
        for e in entries {
            match e {
                MenuEntry::Item { cmd, .. } => out.push(cmd.clone()),
                MenuEntry::Submenu { items, .. } => flatten(items, out),
                _ => {}
            }
        }
    }

    #[test]
    fn menu_contains_the_required_items() {
        let cfg = Config::default();
        let dir = crate::paths::test_dir("menu");
        let devices = vec!["USB Mic".to_string()];
        let s = MenuState {
            cfg: &cfg,
            status: &TrayStatus::Ready,
            listening: false,
            devices: &devices,
            models_dir: &dir,
            downloading: None,
            autostart: false,
        };
        let menu = build_menu(&s);
        let mut cmds = Vec::new();
        flatten(&menu, &mut cmds);
        for required in [
            TrayCommand::StartStop,
            TrayCommand::UiChangeHotkey(HotkeyKind::PushToTalk),
            TrayCommand::SelectMicrophone("USB Mic".into()),
            TrayCommand::UiConfirmDownload("parakeet-tdt-0.6b-v3".into()),
            TrayCommand::SetMode(PostMode::Smart),
            TrayCommand::OpenSettings,
            TrayCommand::ToggleAutostart,
            TrayCommand::Quit,
        ] {
            assert!(cmds.contains(&required), "missing {required:?}");
        }
        // Bengali is offered through the Whisper fallback.
        assert!(cmds.contains(&TrayCommand::SetLanguage("bn".into())));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tooltip_fits_the_windows_limit() {
        let cfg = Config::default();
        let long = TrayStatus::Error("x".repeat(300));
        assert!(long.tooltip(&cfg).chars().count() <= 127);
        assert!(TrayStatus::Ready.tooltip(&cfg).contains("Ctrl+Alt+Space"));
        assert_eq!(TrayStatus::Listening.tone(), IconTone::Listening);
    }

    #[test]
    fn menu_title_is_compact() {
        assert_eq!(TrayStatus::Ready.menu_title(), "SwiftVoice: ready");
        assert_eq!(TrayStatus::Listening.menu_title(), "SwiftVoice: listening…");
        assert!(TrayStatus::Ready.menu_title().chars().count() < TrayStatus::Ready.tooltip(&Config::default()).chars().count());
    }
}
