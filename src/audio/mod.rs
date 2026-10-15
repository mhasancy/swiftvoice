//! Audio capture and processing.
//!
//! Signal path: WASAPI callback -> lock-free ring -> audio thread ->
//! optional RNNoise (48 kHz) -> windowed-sinc resampler (16 kHz) -> gain ->
//! 32 ms frames -> VAD -> segmenter -> snapshot events for the recogniser.

pub mod capture;
pub mod denoise;
pub mod dsp;
pub mod pipeline;
pub mod processor;
pub mod resample;
pub mod ring;
pub mod segmenter;
pub mod vad;
pub mod wav;

pub use pipeline::{AudioCmd, AudioEvent, AudioHandle, CaptureSettings, SessionMode, SessionParams};
pub use segmenter::{EndReason, SegEvent, SegmenterConfig, SnapKey, Snapshot, SnapshotKind};
