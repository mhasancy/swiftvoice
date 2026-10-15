//! SwiftVoice: fast, private, offline voice typing for Windows.
//!
//! Module map (see docs/ARCHITECTURE.md):
//! * `audio`: capture, resampling, VAD, utterance segmentation
//! * `asr`: speech engines (Parakeet TDT / NeMo CTC on ONNX Runtime,
//!   whisper.cpp), model catalogue/downloads, recogniser thread
//! * `postprocess`: Raw / Clean / Smart text modes, vocabulary
//! * `insertion`: clipboard-paste and typed insertion
//! * `hotkey`, `tray`, `overlay`: desktop integration
//! * `controller`: the dictation state machine
//! * `app` / `platform`: Windows message loop and OS services

pub mod asr;
pub mod audio;
pub mod benchmark;
pub mod cli;
pub mod config;
pub mod controller;
pub mod events;
pub mod hotkey;
pub mod insertion;
pub mod logging;
pub mod memory;
pub mod overlay;
pub mod paths;
pub mod postprocess;
pub mod tray;

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod platform;
