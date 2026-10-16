//! Microphone capture (WASAPI shared mode through `cpal`).
//!
//! The driver callback only converts samples to f32, downmixes to mono and
//! pushes them into a lock-free ring; everything else happens on the audio
//! thread. Streams are opened with the device's native format so Windows
//! does no extra conversion, and resampled to 16 kHz later.

use std::fmt;

use super::ring::Consumer;

/// Why capture failed, with advice the user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureErrorKind {
    NoDevice,
    PermissionDenied,
    DeviceLost,
    Busy,
    Unsupported,
    /// The driver dropped or duplicated some samples; the stream keeps running.
    Xrun,
    Other,
}

#[derive(Debug, Clone)]
pub struct CaptureError {
    pub kind: CaptureErrorKind,
    pub message: String,
}

impl CaptureError {
    pub fn new(kind: CaptureErrorKind, message: impl Into<String>) -> CaptureError {
        CaptureError { kind, message: message.into() }
    }

    /// One-line advice for the overlay / notification.
    pub fn advice(&self) -> &'static str {
        match self.kind {
            CaptureErrorKind::NoDevice => "Connect a microphone or pick another one in the tray menu.",
            CaptureErrorKind::PermissionDenied => {
                "Windows blocked the microphone. Open Settings > Privacy & security > Microphone and turn on \"Let desktop apps access your microphone\"."
            }
            CaptureErrorKind::DeviceLost => "The microphone was disconnected. SwiftVoice will use it again when it returns.",
            CaptureErrorKind::Busy => "Another app is using the microphone exclusively. Close it or pick another microphone.",
            CaptureErrorKind::Unsupported => "This microphone's audio format is not supported; pick another device.",
            CaptureErrorKind::Xrun => "Some audio was dropped while another app shared the microphone; recording continues.",
            CaptureErrorKind::Other => "See the log file for details (tray menu > Open logs folder).",
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.message, self.advice())
    }
}

impl std::error::Error for CaptureError {}

/// An open input stream feeding a ring buffer.
pub struct CaptureStream {
    pub consumer: Consumer,
    pub sample_rate: u32,
    pub channels: u16,
    pub device_name: String,
    /// Errors reported asynchronously by the driver thread.
    pub errors: crossbeam_channel::Receiver<CaptureError>,
    /// Keeps the driver stream alive while the session is warm. `None` only
    /// in tests, which build a stream without a device.
    #[cfg(windows)]
    _stream: Option<cpal::Stream>,
}

#[cfg(all(test, windows))]
impl CaptureStream {
    /// A stream with no driver for tests: only the samples the test pushes
    /// into the ring arrive, and no errors are reported.
    pub(crate) fn new_for_test(consumer: Consumer, sample_rate: u32, device_name: &str) -> CaptureStream {
        let (_err_tx, errors) = crossbeam_channel::bounded(16);
        CaptureStream { consumer, sample_rate, channels: 1, device_name: device_name.to_string(), errors, _stream: None }
    }
}

#[cfg(all(test, not(windows)))]
impl CaptureStream {
    /// A stream with no driver for tests: only the samples the test pushes
    /// into the ring arrive, and no errors are reported.
    pub(crate) fn new_for_test(consumer: Consumer, sample_rate: u32, device_name: &str) -> CaptureStream {
        let (_err_tx, errors) = crossbeam_channel::bounded(16);
        CaptureStream { consumer, sample_rate, channels: 1, device_name: device_name.to_string(), errors }
    }
}

#[cfg(windows)]
mod imp {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SizedSample};

    use super::{CaptureError, CaptureErrorKind, CaptureStream};
    use crate::audio::ring::{ring, Producer};

    fn map_err(e: &cpal::Error, context: &str) -> CaptureError {
        use cpal::ErrorKind as K;
        let kind = match e.kind() {
            K::PermissionDenied => CaptureErrorKind::PermissionDenied,
            K::DeviceNotAvailable | K::StreamInvalidated => CaptureErrorKind::DeviceLost,
            K::DeviceBusy => CaptureErrorKind::Busy,
            K::UnsupportedConfig | K::UnsupportedOperation => CaptureErrorKind::Unsupported,
            // The driver dropped or duplicated samples; the stream keeps running.
            K::Xrun => CaptureErrorKind::Xrun,
            _ => CaptureErrorKind::Other,
        };
        // Driver messages often end with a period already; keep exactly one.
        let detail = e.to_string();
        CaptureError::new(kind, format!("{context}: {}.", detail.trim_end_matches('.')))
    }

    fn device_name(d: &cpal::Device) -> String {
        d.description().map(|desc| desc.name().to_string()).unwrap_or_else(|_| "Unknown microphone".into())
    }

    /// Names of all input devices (for the tray menu).
    pub fn list_input_devices() -> Result<Vec<String>, CaptureError> {
        let host = cpal::default_host();
        let devices = host.input_devices().map_err(|e| map_err(&e, "Cannot list microphones"))?;
        let mut names: Vec<String> = devices.map(|d| device_name(&d)).collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn default_device_name() -> Option<String> {
        cpal::default_host().default_input_device().map(|d| device_name(&d))
    }

    fn pick_device(preference: &str) -> Result<(cpal::Device, Option<String>), CaptureError> {
        let host = cpal::default_host();
        if !preference.eq_ignore_ascii_case("default") && !preference.trim().is_empty() {
            if let Ok(mut devices) = host.input_devices() {
                if let Some(d) = devices.find(|d| device_name(d) == preference) {
                    return Ok((d, None));
                }
            }
            let fallback =
                host.default_input_device().ok_or_else(|| CaptureError::new(CaptureErrorKind::NoDevice, "No microphone is available."))?;
            let warning = format!("Microphone \"{preference}\" was not found; using \"{}\".", device_name(&fallback));
            return Ok((fallback, Some(warning)));
        }
        host.default_input_device()
            .map(|d| (d, None))
            .ok_or_else(|| CaptureError::new(CaptureErrorKind::NoDevice, "No microphone is available."))
    }

    fn build<T>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        mut producer: Producer,
        err_tx: crossbeam_channel::Sender<CaptureError>,
    ) -> Result<cpal::Stream, cpal::Error>
    where
        T: SizedSample + Send + 'static,
        f32: FromSample<T>,
    {
        let channels = config.channels.max(1) as usize;
        let mut mono = [0f32; 1024];
        device.build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _info: &cpal::InputCallbackInfo| {
                // Runs on the driver thread: no locks, no allocation.
                for chunk in data.chunks(mono.len() * channels) {
                    let frames = chunk.len() / channels;
                    for (i, frame) in chunk.chunks_exact(channels).enumerate() {
                        let mut acc = 0.0f32;
                        for &s in frame {
                            acc += s.to_sample::<f32>();
                        }
                        mono[i] = acc / channels as f32;
                    }
                    producer.push_slice(&mono[..frames]);
                }
            },
            move |err: cpal::Error| {
                let _ = err_tx.try_send(map_err(&err, "Microphone error"));
            },
            None,
        )
    }

    /// Open `preference` ("default" or a device name). Returns the stream
    /// and an optional warning (e.g. the named device was not found).
    pub fn open(preference: &str) -> Result<(CaptureStream, Option<String>), CaptureError> {
        let (device, warning) = pick_device(preference)?;
        let name = device_name(&device);
        let supported = device.default_input_config().map_err(|e| map_err(&e, &format!("Cannot read the format of \"{name}\"")))?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let config = supported.config();
        // Two seconds of headroom; the audio thread drains every ~10 ms.
        let (producer, consumer) = ring(sample_rate as usize * 2);
        let (err_tx, err_rx) = crossbeam_channel::bounded(16);
        use cpal::SampleFormat as F;
        let stream = match supported.sample_format() {
            F::F32 => build::<f32>(&device, config, producer, err_tx),
            F::F64 => build::<f64>(&device, config, producer, err_tx),
            F::I16 => build::<i16>(&device, config, producer, err_tx),
            F::I32 => build::<i32>(&device, config, producer, err_tx),
            F::I8 => build::<i8>(&device, config, producer, err_tx),
            F::U8 => build::<u8>(&device, config, producer, err_tx),
            F::U16 => build::<u16>(&device, config, producer, err_tx),
            other => {
                return Err(CaptureError::new(
                    CaptureErrorKind::Unsupported,
                    format!("\"{name}\" uses the unsupported sample format {other:?}."),
                ))
            }
        }
        .map_err(|e| map_err(&e, &format!("Cannot open \"{name}\"")))?;
        stream.play().map_err(|e| map_err(&e, &format!("Cannot start \"{name}\"")))?;
        log::info!("microphone \"{name}\" opened: {sample_rate} Hz, {channels} ch, {:?}", supported.sample_format());
        Ok((CaptureStream { consumer, sample_rate, channels, device_name: name, errors: err_rx, _stream: Some(stream) }, warning))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn xrun_has_its_own_kind() {
            let e: cpal::Error = cpal::ErrorKind::Xrun.into();
            assert_eq!(map_err(&e, "Microphone error").kind, CaptureErrorKind::Xrun);
        }

        #[test]
        fn mapped_messages_keep_a_single_trailing_period() {
            let e: cpal::Error = cpal::ErrorKind::Xrun.into();
            let msg = map_err(&e, "Microphone error").message;
            assert!(msg.ends_with('.') && !msg.ends_with(".."), "{msg}");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{CaptureError, CaptureErrorKind, CaptureStream};

    const MSG: &str =
        "Live microphone capture is implemented for Windows only. On this platform use --benchmark or --transcribe with WAV files.";

    pub fn list_input_devices() -> Result<Vec<String>, CaptureError> {
        Err(CaptureError::new(CaptureErrorKind::Unsupported, MSG))
    }

    pub fn default_device_name() -> Option<String> {
        None
    }

    pub fn open(_preference: &str) -> Result<(CaptureStream, Option<String>), CaptureError> {
        Err(CaptureError::new(CaptureErrorKind::Unsupported, MSG))
    }
}

pub use imp::{default_device_name, list_input_devices, open};
