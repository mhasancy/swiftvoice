//! Messages delivered to the controller (on the UI thread).

use std::path::PathBuf;

use crate::asr::worker::WorkerEvent;
use crate::audio::AudioEvent;
use crate::config::{InsertMethod, PostMode};
use crate::hotkey::Hotkey;
use crate::insertion::InsertReport;
use crate::postprocess::PostResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    PttDown,
    PttUp,
    Toggle,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyKind {
    PushToTalk,
    Toggle,
}

/// Everything the tray menu can ask for. Modal steps (picking a folder,
/// capturing a hotkey, confirming a download) are completed by the UI layer
/// before the command reaches the controller.
#[derive(Debug, Clone, PartialEq)]
pub enum TrayCommand {
    StartStop,
    SetHotkey(HotkeyKind, Hotkey),
    ResetHotkeys,
    SelectMicrophone(String),
    ToggleNoiseSuppression,
    SetGain(f32),
    SelectModel(String),
    DownloadModel(String),
    CancelDownload,
    SetModelsDir(PathBuf),
    SetLanguage(String),
    SetMode(PostMode),
    SetInsertMethod(InsertMethod),
    ToggleOverlay,
    TogglePartials,
    ToggleContinuous,
    ToggleAutostart,
    OpenSettings,
    EditVocabulary,
    OpenLogs,
    OpenModelsFolder,
    Quit,
    /// Items the UI handles itself before calling the controller.
    UiChangeHotkey(HotkeyKind),
    UiChooseModelsFolder,
    UiAbout,
    UiConfirmDownload(String),
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Progress { model: String, percent: Option<u32>, phase: crate::asr::download::Phase },
    Done { model: String, result: Result<(), String> },
}

#[derive(Debug)]
pub enum AppEvent {
    Hotkey(HotkeyEvent),
    Tray(TrayCommand),
    Audio(AudioEvent),
    Asr(WorkerEvent),
    Inserted(InsertReport),
    Postprocessed {
        session: u64,
        utterance: u32,
        result: PostResult,
    },
    Download(DownloadEvent),
    /// config.toml or vocabulary.toml changed on disk.
    ConfigChanged,
    PowerResume,
    SessionLocked,
}
