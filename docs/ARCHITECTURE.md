# Architecture

SwiftVoice is one native Win32 process. The package is:
- `swiftvoice.exe`: GUI subsystem, so there is no console window.
- `swiftvoice.com`: a small console launcher, so terminal commands wait
  and print. It works like Visual Studio's devenv.com.
- Microsoft's `onnxruntime.dll` (17.3 MB) and the four Visual C++ runtime
  DLLs.

You add the model you choose. It has no browser engine,
no Python, no service and no installer-managed components. This document explains how audio becomes
inserted text, and why it is fast.

## Threads

```
                         ┌────────────────────────── UI thread (main) ──────────────────────────┐
 RegisterHotKey ───────▶ │ Win32 message loop: hotkeys, tray menu, overlay, timers, power/session │
 Shell_NotifyIcon ◀───── │ Controller (state machine, no I/O, unit-tested)                       │
 layered overlay  ◀───── │   ▲ AppEvent via bounded channel + PostMessage(WM_APP_WAKE)            │
                         └───┼───────────────────────────────────────────────────────────────────┘
                             │ AudioCmd / WorkerMsg / InsertRequest (bounded channels, try_send)
     ┌───────────────────────┼──────────────────────┬──────────────────────────┐
     ▼                       │                      ▼                          ▼
 WASAPI callback ──SPSC ring──▶ audio thread    recogniser thread          inserter thread
 (cpal, no locks,  2 s)       resample → gain   Parakeet / Whisper         clipboard snapshot,
  no allocation)              → RNNoise → VAD   (ONNX Runtime / ggml),     Ctrl+V or SendInput,
                              → segmenter       job queue with supersede   restore clipboard
                                  │                 rules, model LRU (2)
                                  └── Snapshot jobs ──▶
 + short-lived threads: Smart-mode HTTP request (loopback only), model download
```

- **UI thread.** It owns every window, the tray icon and the `Controller`.
  Worker threads never touch UI state. They push `AppEvent`s into a bounded
  channel and post `WM_APP_WAKE`, and the UI thread drains the channel.
  Re-entrancy from modal loops (the tray menu, message boxes) is detected
  with `RefCell::try_borrow_mut`, and the event is retried 15 ms later.
- **Audio thread.** It owns the microphone stream (cpal on WASAPI shared
  mode) and a `FrameProcessor`. The driver callback only mixes to mono and
  writes into a lock-free single-producer/single-consumer ring of 2 s. The
  audio thread drains it every ~10 ms. Overruns are counted and logged,
  never blocking.
- **Recogniser thread.** It owns the loaded engines (at most two, LRU) and
  runs one job at a time. ONNX Runtime uses its own intra-op thread pool
  for the encoder.
- **Inserter thread.** It talks to the clipboard and `SendInput`, so a slow
  target application can never stall the UI or audio.

All channels are bounded. Senders on the latency path use `try_send`. A full
queue reports an error for that utterance instead of blocking the audio
thread.

## From sound to text

1. **Capture.** The device's native format (usually 48 kHz float stereo) is
   mixed to mono in the callback.
2. **Front-end DSP** (`audio::processor`, shared by the app and the
   benchmark):
   - optional RNNoise noise suppression at 48 kHz (`nnnoiseless`, pure Rust);
   - windowed-sinc resampling to 16 kHz with bounded history;
   - gain;
   - 512-sample (32 ms) frames.
3. **Voice activity.** Silero VAD v6.2, a 2.3 MB ONNX model run on every
   frame, with an energy detector as the fallback that needs no model. Hysteresis
   keeps short weak frames inside speech.
4. **Segmenter** (`audio::segmenter`): a state machine that turns frames
   into events:
   - `Snapshot(Pause)` after 200 ms of silence (`pause_ms`): the audio so
     far is transcribed *now*, while SwiftVoice waits to see whether the
     pause is the end;
   - `UtteranceEnd` at the endpoint (toggle) or key release (push-to-talk),
     naming the snapshot whose transcript is final. If nothing was said
     after the last pause, that is the pause snapshot already being
     transcribed, so the text is ready the moment the endpoint is
     confirmed. This is why toggle mode measures 0-26 ms after the endpoint;
   - `Commit`: once a window is longer than `commit_after_secs` (6 s), the
     next pause commits it and a new window starts. At the end of an
     utterance, if speech followed the last pause, the window is committed
     at that pause (its transcript is already running) and only the rest is
     transcribed. The work left after release is therefore about one
     phrase, however long you talked;
   - a hard cut at 16 s without any pause keeps memory and latency bounded
     and lands just before the quietest frame of the last 2 s, so the new
     window starts in a word gap instead of mid-word.
5. **Context across windows.** A window after a commit is transcribed with
   the previous window's last phrase prepended (1-2.5 s, starting at an
   earlier pause point). The model then continues the sentence: casing,
   punctuation, and a short first phrase all come out right. The engine
   drops tokens emitted before the boundary, using the TDT/CTC frame index
   of each token (80 ms frames). This is needed because the Parakeet
   exports sometimes return *nothing* for a clip that starts in the middle
   of a sentence. On one test clip this happened for 33 of 72 arbitrary
   5 s cuts in SwiftVoice and 28 of 72 in sherpa-onnx. For clips that start
   at a pause it happened 1 time in 92. Context that begins at a pause
   avoids it.
6. **Recogniser** (`asr::worker`): jobs for the same window supersede each
   other. Only the longest queued Pause/Final job survives.
7. **Engines.**
   - *Parakeet TDT* (`asr::parakeet`). A Rust NeMo log-mel front end:
     pre-emphasis 0.97, 25 ms Hann window in a 512-point FFT, 10 ms hop,
     Slaney mel filters, log, per-feature normalisation. It is verified
     against NeMo's own preprocessor to 2e-4. Then the FastConformer
     encoder on ONNX Runtime, and greedy Token-and-Duration Transducer
     decoding: the joiner predicts a token *and* how many frames to skip,
     which makes decoding cheaper than RNN-T.
   - *NeMo CTC* (`asr::nemo_ctc`) for the 110M model.
   - *Whisper* via whisper.cpp (`asr::whisper`, `whisper-rs`) for languages
     Parakeet lacks, such as Bengali. It is compiled in by the default
     `whisper` feature.
8. **Post-processing** (`postprocess`):
   - *Raw*: the model's text.
   - *Clean* (default, < 0.2 ms): custom vocabulary and voice commands
     ("new line", "open bracket", …, longest match first), filler and
     stutter removal, capitalisation, spacing.
   - *Smart* (opt-in): sends the Clean text to a local LLM server. It
     uses Ollama or an OpenAI-compatible server such as LM Studio, through
     a minimal HTTP/1.1 client that accepts loopback addresses only, and it
     enforces a timeout. An empty rewrite or an implausible length change
     (more than twice as long, or less than 40 % of the input) is
     rejected, and the Clean text is used. So is any error or timeout.
9. **Insertion** (`insertion`): see below.

Utterances of one session are inserted strictly in order, even when a later
one finishes first (`BTreeMap` keyed by utterance).

## Insertion

The default `method = "auto"`:

1. **Wait for the hotkey's modifiers to be released.** Otherwise Ctrl+V
   would arrive as Ctrl+Alt+V. Modifiers still held after
   `modifier_wait_ms` are released synthetically. A VK 0xE8 no-op key
   prevents Alt from opening a menu.
2. **Check the target.**
   - No focused window, or SwiftVoice's own window: the text goes to the
     clipboard, with a notice.
   - The target runs elevated and SwiftVoice does not: Windows silently
     blocks synthetic input (UIPI). SwiftVoice detects this with
     `OpenProcessToken`/`TokenElevation` and puts the text on the clipboard,
     telling you to press Ctrl+V.
3. **Paste.**
   - Snapshot every clipboard format that is a memory block (GDI handles
     and private formats are skipped), up to `max_clipboard_snapshot_mb`.
   - Set the text. The text is marked with
     `ExcludeClipboardContentFromMonitorProcessing`,
     `CanIncludeInClipboardHistory = 0` and `CanUploadToCloudClipboard = 0`,
     so dictation never lands in Win+V history or cloud clipboard sync.
   - Send the paste shortcut. It is per application; terminals such as
     mintty and PuTTY use Shift+Insert.
   - After `restore_delay_ms`, restore the snapshot, but only if
     `GetClipboardSequenceNumber` shows nobody else changed the clipboard
     in the meantime.
4. **Type.** `type_apps`, or a clipboard that cannot be snapshotted, gets
   the text as `KEYEVENTF_UNICODE` key events, which never touch the
   clipboard.

## Why it is fast and light

- **No round trip.** Audio never leaves the machine, so there is no upload,
  queueing or TLS handshake.
- **Speculative transcription at pauses.** Most of the work is done while
  you are still deciding whether you have finished. The worker drops work
  that a newer snapshot made obsolete.
- **Commit and split.** The text after release only needs the last
  phrase, not the whole dictation.
- **Models stay loaded and warm.** There is a warm-up inference at load,
  and the microphone stays open briefly after each dictation, with 300 ms
  of pre-roll.
- **One small native process.**
  - UI: Win32, GDI and the system tray; no WebView, no runtime to start.
  - DLLs: one ONNX Runtime DLL loaded by absolute path from the exe folder,
    so another copy on PATH is never picked up.
  - Allocation: no per-frame allocation on the audio path.
  - Memory: the ONNX Runtime CPU memory arena is disabled, so the memory a
    long utterance used is returned afterwards.
  - Optional `low_memory` skips duplicate pre-packed weights (−250 MB).
  - Optional idle unload trims the heap back to the OS.

## Privacy

- **No network after model download.** Dictation needs no network at all.
  The only outbound connections are model downloads you start; they are
  pinned URLs with SHA-256/SHA-1 verification. Smart mode talks only to a
  loopback address, and the URL parser rejects anything else.
- **No telemetry.**
  - SwiftVoice has none.
  - ONNX Runtime's is switched off. `ORT_DISABLE_TELEMETRY=1` is set at
    process start. This stops the 1DS uploader that Microsoft's Linux and
    macOS 1.31 builds otherwise start. During development it was observed
    connecting to `mobile.events.data.microsoft.com` before the variable
    was set.
  - `DisableTelemetryEvents` turns off the Windows build's ETW events.
  - `--doctor` reports the state.
- **Nothing is recorded to disk.**
  - Audio stays in memory: the pre-roll buffer, and snapshots that are
    freed after transcription.
  - Logs contain timings and errors, not transcripts. The raw transcript
    is logged only at `trace` level, which you must enable explicitly.
- **Clipboard history and cloud sync are excluded** for dictated text, as
  described above.

## Robustness

- **Microphone permission denied, device unplugged, device busy.** Each
  gets a specific message (`CaptureErrorKind`). The device list refreshes
  from the tray. A missing named device falls back to the default, with a
  notice.
- **A muted or blocked microphone** sends pure digital silence. After
  1.5 s a warning explains the likely privacy-setting cause.
- **Buffer glitches while another app shares the microphone.** Driver
  xruns never end a session: they are counted per session, together with
  overruns (samples the ring had to drop), and each session ends with a
  one-line summary of its audio in the log, so a strange transcription can
  be checked against what the microphone actually delivered.
- **A stuck insertion queue.** Dictated text reaches the inserter thread
  through a bounded queue; if the queue stays full (or the thread has
  exited) the text is copied to the clipboard with a card explaining why,
  instead of being dropped with only a log line.
- **Sleep/resume and screen lock.** Listening stops on sleep and on lock.
  On resume the device is released and the hotkeys re-registered.
- **Explorer restart.** The tray icon is re-added on `TaskbarCreated`.
- **Settings edited by hand** are picked up within 1 s. Invalid values are
  reported together, and the previous settings stay in effect. Tray
  changes rewrite single keys with `toml_edit`, preserving your comments.
- **Single instance.** A named mutex makes a second launch explain where
  the icon is.
- **Crash safety.** The log is flushed per line and rotated at 5 MB (3
  files). A panic hook writes the panic and a backtrace to the log before
  exit.

## Module map

| Path | Responsibility |
|---|---|
| `src/main.rs`, `cli.rs` | entry point, command line (`--benchmark`, `--transcribe`, `--record`, `--doctor`, `models …`) |
| `src/bin/swiftvoice-com.rs` | `swiftvoice.com` console launcher: runs `swiftvoice.exe` with the terminal's handles, waits, relays the exit code and Ctrl+C |
| `app.rs`, `platform/` | Windows shell: message loop, `Ui`/`Ports` implementations, single instance, autostart, dialogs |
| `controller.rs` | dictation state machine; platform-independent and unit-tested with fakes |
| `audio/` | capture, ring buffer, resampler, DSP, denoise, VAD, segmenter, audio thread, WAV I/O |
| `asr/` | ONNX Runtime loading, NeMo features, Parakeet TDT, NeMo CTC, Whisper, worker, catalogue, downloader |
| `postprocess/` | tokenisation, vocabulary and voice commands, Clean rules, Smart (loopback LLM) |
| `insertion/` | strategy and clipboard restore logic (`mod.rs`, `clipboard.rs`, tested with fakes) and the Win32 backend (`win.rs`) |
| `hotkey/` | hotkey parsing/validation, registration, push-to-talk polling, the low-level hook for modifier-only keys, the capture dialog |
| `tray/`, `overlay/` | menu model and procedural icons; the Win32 tray icon and the layered overlay |
| `config/` | typed settings with validation, the commented template, comment-preserving edits |
| `benchmark.rs`, `memory.rs`, `logging.rs`, `paths.rs` | benchmark and latency simulation; RSS; file logger; folder layout |
