# SwiftVoice

**Fast, private, offline voice typing for Windows 10/11.** Hold a key,
speak, release, and the text appears at your cursor in any app. Speech is
recognised on your own PC by NVIDIA's Parakeet models. No account, no cloud
and no telemetry, and the network is never used after the one-time model
download.

- **Push-to-talk** `Ctrl+Alt+Space` (hold) and **toggle** `Ctrl+Alt+V`
  (press, speak, pause). Both can be remapped from the tray.
- **Fast:** in the measured runs, 24-125 ms (110M model) or 216-515 ms
  (0.6B models) from key release to text on a 4-vCPU cloud VM; see
  [docs/BENCHMARKS.md](docs/BENCHMARKS.md).
- **Languages:** 25 European languages with Parakeet TDT 0.6B v3, a light
  English model, and a whisper.cpp fallback for other languages, such as
  Bengali.
- **Raw, Clean and Smart text modes.** Smart uses a *local* LLM through
  Ollama or LM Studio, is off by default, and never leaves your machine.
- **Custom vocabulary and voice commands:** "new line", "new paragraph",
  "open bracket", Bangla-English phrases.
- **Clipboard-safe insertion.** Your clipboard is restored, and dictated
  text is kept out of Win+V history and cloud clipboard sync.
- **One small native process:** Rust and Win32, with no Electron, browser
  runtime, Python or background service.

## Why it is faster and lighter than cloud dictation

Wispr Flow ("WhisperFlow") says in its own documentation that transcription
always runs in its cloud and that an internet connection is required
([data controls](https://wisprflow.ai/data-controls),
[system requirements](https://docs.wisprflow.ai/articles/1036674442-supported-devices-and-system-requirements)).
SwiftVoice works differently:

| | SwiftVoice |
|---|---|
| Where speech is recognised | on your CPU; audio never leaves the PC |
| Network during dictation | none; works offline and on planes |
| Waiting after you stop | only the last phrase. Audio is transcribed during your pauses, and long dictation is committed at pauses, so there is no upload round trip and no queue |
| Runtime | one native exe (Win32 tray + overlay) plus ONNX Runtime; no browser engine |
| Account / telemetry | none / none (ONNX Runtime's own telemetry is switched off too) |
| Cost | free; models are CC-BY-4.0 / MIT |

The comparison is architectural. Wispr Flow was not benchmarked for this
project. SwiftVoice's own numbers, including the ones that miss their
targets, are in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Install

### From a release zip

1. Download the latest `SwiftVoice-<version>-win-x64.zip` from the
   [Releases](https://github.com/mhasancy/swiftvoice/releases) page.
2. Unzip it anywhere, for example `%LOCALAPPDATA%\Programs\SwiftVoice`.
3. Run `swiftvoice.exe`. A microphone icon appears in the notification
   area.
4. The first run offers to download the speech model (465 MB for the
   default). Accept it, or choose another model under *Engine & model*.

The zip contains:
- `swiftvoice.exe`: the tray app.
- `swiftvoice.com`: a tiny console front end. Typing `swiftvoice …` in
  cmd or PowerShell runs it, so commands print to the terminal and return
  an exit code.
- Microsoft's `onnxruntime.dll` 1.31.0, the Visual C++ runtime DLLs, and
  the licences.

Nothing is installed system wide. To remove SwiftVoice, quit it and delete the folder, plus
`%APPDATA%\SwiftVoice` and `%LOCALAPPDATA%\SwiftVoice`.

### Build from source

Requirements:
- Windows 10/11 x64.
- Rust via `rustup`, with the MSVC toolchain. `rust-toolchain.toml` pins the exact version (1.99.0), and rustup installs it on first build. The minimum supported version is 1.92.
- Visual Studio 2022 or newer, or Build Tools, with *Desktop development
  with C++*.
- Only when building with the Whisper fallback (`--features whisper`; the
  build script adds it unless `-NoWhisper`):
  - *C++ CMake tools for Windows*, or CMake on PATH;
  - libclang, to generate the whisper.cpp bindings: LLVM (`winget install LLVM.LLVM`), or *C++ Clang Compiler for Windows* in the Visual Studio Installer.

```powershell
git clone https://github.com/mhasancy/swiftvoice; cd swiftvoice
powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1   # build, test, package
dist\SwiftVoice\swiftvoice.exe
```

The script:
- builds with `cargo build --release --locked --features whisper` (unless
  `-NoWhisper`) and runs `cargo test`;
- downloads ONNX Runtime 1.31.0 from Microsoft's GitHub release and
  checks its SHA-256;
- copies the VC++ runtime DLLs from your Visual Studio installation;
- smoke-tests the packaged exe;
- writes `dist\SwiftVoice-<version>-win-x64.zip`.

Options: `-NoWhisper` (no CMake or LLVM needed, no Whisper fallback), `-SkipTests`,
`-Portable` (`data\` and `models\` folders next to the exe), `-OrtZip <file>`
(offline build).

Individual commands:

```powershell
cargo build --release --locked                      # build (no Whisper fallback)
cargo build --release --locked --features whisper   # with whisper.cpp (CMake + libclang needed)
cargo test --release --locked                       # unit + integration tests
cargo clippy --all-targets -- -D warnings           # lint
cargo fmt --check                                   # formatting
```

## First run: models

```powershell
swiftvoice models list
swiftvoice models download parakeet-tdt-0.6b-v3 silero-vad    # default, 25 languages
swiftvoice models download parakeet-tdt-ctc-110m-en            # fastest, English, 100 MB
swiftvoice models download whisper-large-v3-turbo-q5_0         # fallback: Bengali and ~99 languages
swiftvoice models verify
```

The tray menu (*Engine & model*) does the same and shows each model's size,
RAM, languages and speed. Models are stored in
`%LOCALAPPDATA%\SwiftVoice\models`; you can choose another folder there.
Every download is resumable and checked against a pinned digest. See
[models/README.md](models/README.md) for sources, licences and the
catalogue.

## Using it

| Action | Default |
|---|---|
| Dictate while holding | **Ctrl+Alt+Space** (push-to-talk) |
| Start/stop listening | **Ctrl+Alt+V** (toggle; stops 0.8 s after you finish, or set `vad.endpoint_ms = 0` and press again to stop) |
| Cancel while listening | **Escape** |
| Change hotkeys | tray > *Hotkeys > Change push-to-talk…* (press the new keys) |

A modifier key can also be used alone: with `toggle = "RAlt"`, one press of
Right Alt starts dictation and the next press stops it.

While you dictate, a small card near the bottom shows the words as they
are recognised, with a red dot that follows your voice. When the text has
been inserted a green dot flashes at the middle-left and disappears. If
nothing can take text (no focused window, the desktop, the taskbar, an
icon or file list), the card says the text is on the clipboard so you can
paste it anywhere. Short messages and errors appear as the same card,
which never takes focus. The tray menu provides:
- start/stop;
- hotkey changes;
- microphone, gain and noise suppression;
- engine, model and language;
- the text mode (Raw / Clean / Smart) and *Edit vocabulary…*;
- the insertion method;
- start with Windows (off by default), settings, logs, About and Quit.

Voice commands in Clean mode: "new line", "new paragraph", "open/close
bracket", "open/close quote", "question mark", "exclamation mark", "full
stop". Optionally also "comma", "period", "colon" and more.

## Configuration

Settings live in `%APPDATA%\SwiftVoice\config.toml`. The file is created
with comments on first run and re-read automatically when you save it.
Full reference: [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

```toml
[hotkeys]
push_to_talk = "Ctrl+Alt+Space"
toggle = "Ctrl+Alt+V"

[asr]
model = "parakeet-tdt-0.6b-v3"   # or "parakeet-tdt-ctc-110m-en" for speed
language = "auto"                # "bn" routes to the Whisper fallback

[postprocess]
mode = "clean"                   # raw | clean | smart (local LLM, opt-in)

[insertion]
method = "auto"                  # paste + restore clipboard; "type" never touches it
```

Custom vocabulary in `%APPDATA%\SwiftVoice\vocabulary.toml`:

```toml
[[replace]]
from = "cube and eighties"
to = "Kubernetes"

[[replace]]
from = "dhonnobad"
to = "ধন্যবাদ"
```

## Benchmark

```powershell
scripts\benchmark.ps1 -Download                 # downloads samples\test.wav (public-domain JFK clip) if missing
swiftvoice --benchmark --audio samples\test.wav --reference samples\test.txt
```

The report covers:
- audio duration, model load and warm-up times;
- transcription time and real-time factor;
- peak RAM, and RAM after unloading the model;
- word error rate, both of a single transcription and of the text
  push-to-talk would insert;
- post-processing time;
- the perceived latency of push-to-talk and toggle, simulated over the
  app's real VAD, segmenter and recogniser scheduling with measured
  inference times.

Add `--json out.json` for machine-readable results. Targets, method and
measured results: [docs/BENCHMARKS.md](docs/BENCHMARKS.md). The record of
every verification run is in [docs/TESTING_EVIDENCE.md](docs/TESTING_EVIDENCE.md).

## Privacy

- **Offline after the model download.** No network access during
  dictation. Smart mode can only reach loopback addresses (127.0.0.0/8,
  ::1, localhost), and this is enforced when the settings load.
- **No accounts and no telemetry.** SwiftVoice sends nothing.
  - ONNX Runtime's telemetry is disabled at start-up
    (`ORT_DISABLE_TELEMETRY=1`, plus `DisableTelemetryEvents`).
  - `--doctor` confirms it.
- **Audio exists only in memory.** Nothing is written to disk unless you
  run `--record`.
- **Logs contain timings and errors, not your words**, unless you choose
  `log_level = "trace"`.
- **Dictated text is pasted with clipboard-history and cloud-sync opt-out
  flags.** Your previous clipboard content is put back.

## Known limitations

- **Interactive Windows behaviour was not exercised by hand for this
  release:** hotkeys, tray, overlay, insertion into real apps, device
  changes, sleep and resume. The Windows code compiles cleanly with clippy
  and builds in CI. The logic behind it is unit-tested with fakes. The
  manual checklist is [tests/MANUAL_TESTS.md](tests/MANUAL_TESTS.md).
  Speech recognition, segmentation, post-processing and the benchmark were
  tested on real models (on Linux).
- **Memory.** The 0.6B models keep about 0.85-1.1 GB resident while
  loaded, far above the 150 MB idle target. Use the 110M English model
  (about 250 MB) or `asr.unload_after_idle_secs`, which brings the process
  to about 60-75 MB at the cost of reloading.
- **Latency depends on how you finish.** The models are offline
  (non-streaming). After you release the key, the last phrase and its
  context still have to be transcribed. A long final phrase without a
  pause takes longer. That was 501 ms on the reference VM for a 3.9 s
  phrase with the 0.6B model.
- **Whisper fallback (Bengali and others) is untested end to end.** It
  builds and is wired in, but its model files could not be downloaded in
  the development environment, so its speed, memory and accuracy were not
  measured. Bengali accuracy is Whisper's, not Parakeet's.
- **CPU only.** There is no GPU, DirectML or NPU acceleration yet. x64
  only, with no ARM64 package.
- **Elevated windows.** To type into apps running as administrator,
  SwiftVoice must run as administrator too. Otherwise the text is left on
  the clipboard, with a notice.
- **Unsigned binaries** trigger SmartScreen on first launch.
- **Recognition quirks.** Parakeet can drop a phrase from a different
  speaker in the same window. sherpa-onnx's reference runtime does the
  same, so it is not specific to SwiftVoice. Use one speaker per
  dictation.

## Roadmap

- A streaming (cache-aware) FastConformer model, so latency stays flat
  however the dictation ends.
- DirectML and Windows ARM64 builds.
- An MSIX or winget package, with code signing.
- Per-application profiles: mode, language and insertion method per exe.
- Measured Whisper and Bengali numbers, and a Bengali-tuned model if a
  suitable open one appears.
- A settings window, in addition to the tray menu and the TOML file.

## Licences

SwiftVoice is MIT licensed ([LICENSE](LICENSE)). Models and libraries keep
their own licences:
- NVIDIA Parakeet: CC-BY-4.0. ONNX exports by the sherpa-onnx project.
- OpenAI Whisper weights and whisper.cpp: MIT.
- Silero VAD: MIT.
- ONNX Runtime: MIT. The package includes its ThirdPartyNotices.
- RNNoise (via nnnoiseless): BSD-3-Clause.

The package lists every Rust crate it contains in
`licenses\RUST-CRATES.txt`.

Developer-only verification tools (Python) are in
[tools/model-export](tools/model-export/README.md). They are never needed
at run time.
