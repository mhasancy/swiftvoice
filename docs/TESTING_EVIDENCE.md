# Testing evidence

Executed results only, newest first. Benchmark tables are in
[BENCHMARKS.md](BENCHMARKS.md); manual Windows checks still to run are in
[../tests/MANUAL_TESTS.md](../tests/MANUAL_TESTS.md). Results from CI are on
the pull requests and the Actions tab.

## 2026-10-20 — 0.1.1-rc.1 Windows release candidate

Run locally on Windows x64, Intel Core i5-12400 (12 logical CPUs), Rust
1.99.0 and Visual Studio 18/MSVC. The complete package includes Whisper,
Microsoft ONNX Runtime 1.31.0, app-local VC++ runtime DLLs, licences and
release notes. Models and build tools remain outside the package.

| Check | Result |
|---|---|
| `scripts/build-windows.ps1` with strict model-test fixtures | Pass. Native release build, 176 unit tests, 2 real-model tests covering 9 model/clip cases, doc tests and packaged-executable smoke checks |
| Final format and lint | `cargo fmt --all -- --check`, `git diff --check`, and `cargo clippy --release --all-targets --locked --offline --features whisper -- -D warnings`: pass |
| Real-model cases | Parakeet v3: en/de/es/fr/0/1; Parakeet CTC 110M: 0/1/en. All expected transcripts match after test normalization. Missing fixtures fail through `SWIFTVOICE_TEST_STRICT=1` |
| Feature parity | Skipped: `SWIFTVOICE_FEATURE_REF` is absent. Its successful test status does not establish numerical feature parity |
| English pipeline benchmark | Public `en.wav`, 3.85 s: 0% raw and simulated push-to-talk WER on 17 reference words. Four repetitions with 300 ms gaps, 16.58 s: 0% on 68 words through 4 recognizer jobs. Silero, Raw mode, noise suppression off; clipboard insertion is simulated |
| Whisper smoke test | The pinned `whisper-base` model loads and transcribes the public English reference phrase correctly. This does not evaluate the default turbo model or Bengali accuracy |
| Native CPU target | CMake cache verifies `GGML_NATIVE=OFF`, `GGML_AVX2=ON`, `GGML_AVX512=OFF`. The packager clears the native fallback cache because whisper-rs-sys does not track these flags for Cargo reuse |
| Runtime and launcher smoke tests | Version `0.1.1-rc.1`; `.com` output/exit-code relay; packaged ONNX Runtime loads; telemetry disabled |
| Locked dependency audit | `cargo audit`: 0 vulnerabilities and no informational warnings against 1,338 RustSec advisories (database commit `999ec89d5d3f10f20be14750ec8c2d8b44e0ac9b`, updated 2026-10-17) |
| Package | `dist/SwiftVoice-0.1.1-rc.1-win-x64.zip`, approximately 8.9 MB; 24.9 MB extracted, models excluded. SHA-256 is in the adjacent `.zip.sha256` file |

Pinned model archives, reference clips and the Microsoft runtime archive
were verified before use. Reference outputs and benchmark JSON are under
`target/golden-audio`; build logs and the audit result are under
`target/release-tools`. An initial strict run failed because two reference
WAV files were missing; after preparing those fixtures, all nine cases ran
and passed. No microphone recording or user-clipboard operation was used.

These public clips do not reproduce the user's reported English errors.
The candidate keeps that issue open. Live microphone, target-application
insertion, clipboard history, hotkeys, Narrator and sign-in behavior remain
manual checks. No public release was published in this preparation.

## 2026-10-20 — review fixes on Windows

Run locally on Windows x64 with Rust 1.99.0 and MSVC, on
`agents/fix-review-findings` after baseline `aa7a51e`. All 17 review findings
were addressed. These checks use default Cargo features (Whisper disabled).

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` and `git diff --check` | Pass |
| `cargo clippy --all-targets --locked --offline -- -D warnings` | Pass |
| `cargo test --all-targets --locked --offline --quiet` | 175 unit tests pass. The feature-parity test and 2 model tests return early because their fixtures are absent; these 3 skips provide no inference or numerical-parity evidence |
| Added/updated regression coverage | Forced-cut sample retention; newline commands through insertion handoff; paste grace and history exclusion; safe Smart HTTP errors; VAD fallback invalidation; cancel state; atomic hotkey reset; bounded ASR backlog; nonblocking insertion and retained clipboard recovery; cancelled acknowledgements; deferred settings; language routing/validation; autostart command |
| Native UI Automation | A hidden window test reads shortcut instructions and an updated validation error through UI Automation. No visible dialog or screen reader was exercised |
| `cargo build --release --locked --offline` | Pass; Windows executables linked successfully |
| Release command-line smoke checks | EXE `--version` passes. Launcher copied as `swiftvoice.com` relays version/help output and invalid-option exit code 2. Isolated output is under `target/review-checks/smoke`; no microphone or clipboard access |
| Targeted reviewer rechecks | Correctness, security, reliability, UX and compatibility reviewers found no remaining issue in the fixes they rechecked. These were source reviews |

The suite ran with temporary files under `target/review-temp` and loopback
access for synthetic HTTP tests. No models or runtime packages were downloaded.
Live tray/overlay/hotkeys, microphone capture, pasting into real applications,
clipboard history, Narrator, registry/sign-in autostart and model inference
remain untested; use the manual checklist. Whisper-enabled compilation was
not run because this host has no `libclang.dll`. Full release packaging was
also not run.

## 2026-10-15 — first release (developed in mhasancy/py-labs PR #20, moved here)

Run by the agent (Claude Code) on a Linux x86-64 cloud VM: Intel Xeon @ 2.80 GHz, 4 vCPUs (AVX-512 VNNI), 16 GB RAM. Rust 1.97.0. ONNX Runtime 1.31.0, Microsoft's official Linux x64 build, loaded dynamically. Scope: the platform-independent core and the real models on Linux, plus a cross-target compile check of the Windows code. Nothing ran on Windows. Paths below are relative to this repository; the crate then lived in `swiftvoice/` of mhasancy/py-labs.

| Check | Result |
|---|---|
| `cargo fmt --check` | Pass |
| `cargo clippy --all-targets --locked -- -D warnings` (whisper.cpp built with CMake 3.28) | Pass, 0 findings |
| `cargo clippy --all-targets --locked --no-default-features -- -D warnings` | Pass |
| `cargo clippy --all-targets --locked --no-default-features --target x86_64-pc-windows-msvc -- -D warnings` | Pass. The Windows code type-checks and lints from Linux; it was not linked or run |
| `cargo test --release --locked` with real models (`SWIFTVOICE_TEST_MODELS`, `SWIFTVOICE_TEST_AUDIO`, `SWIFTVOICE_FEATURE_REF`) | 141 unit tests, 2 model end-to-end tests (Parakeet v3: en/de/es/fr/0/1 clips; 110M: 0/1/en), 1 feature-parity test: all pass |
| Front end vs NeMo preprocessor (onnx-asr 0.12.0 `nemo128.onnx`/`nemo80.onnx`, 7.43 s speech) | max \|diff\| 2.32e-4 / 1.72e-4, mean 8.1e-7 / 6.0e-7. `make_feature_refs.py` regenerated byte-identical references |
| Transcripts vs sherpa-onnx 1.13.8 on the same model files | Identical, except es: SwiftVoice "qué" vs sherpa "que" |
| `swiftvoice --benchmark`, 3 models x 3 clips, 5 runs, defaults | Raw WER 0-4.3 %; inserted-text WER 0 % except v2 long-phoebe 4.3 %. Push-to-talk release-to-text: v3 221/244/501 ms, v2 226/216/515 ms, 110M 24/27/125 ms. RSS loaded: v3 849-1070 MB, 110M 250-253 MB; after unload 57-74 MB. Full table: `docs/BENCHMARKS.md` |
| `asr.low_memory` on vs off, alternating, 3 x 7 runs (v3, jfk) | -230 to -270 MB RSS; transcription 781-861 vs 698-762 ms; push-to-talk 232-244 vs 174-194 ms |
| ONNX Runtime telemetry | Before the fix, each v3 benchmark run made 1-2 CONNECT attempts to `mobile.events.data.microsoft.com:443`; the sandbox proxy denied them (proxy log), and `~/.cache/Microsoft/DeveloperTools/.onnxruntime/onnxruntime.db` was rewritten. With `ORT_DISABLE_TELEMETRY=1` set at start-up: a 44 s v3 run made no attempt, and the file was not touched. Microsoft's Windows `onnxruntime.dll` 1.31.0 contains no 1DS uploader strings, only the ETW provider |
| Model downloader (`models download whisper-base`) | Failed cleanly: the sandbox proxy returns HTTP 403 for huggingface.co. Parakeet/Silero archives and digests were verified by downloading from GitHub |
| PowerShell scripts | `build-windows.ps1` and `benchmark.ps1` parse (PowerShell 7.4.6 on Linux). The ORT zip verification and extraction block ran against the real 1.31.0 zip. `benchmark.ps1` ran end to end on Linux with explicit paths |
| Secret scan | py-labs `script/scan-secrets.py` (that repository's tree): pass, 0 findings. gitleaks 8.30.1 and trufflehog 3.97.4 on the crate's 77 files plus the workflow: 0 findings |
| CI round 1 (py-labs PR #20, commit b2f58a3) | `linux` failed: Rust 1.99's new clippy lint `chunks_exact_to_as_chunks` (local had 1.97). `windows` failed: `whisper-rs-sys` bundled Linux bindings, forced by `WHISPER_DONT_GENERATE_BINDINGS`, fail their `_IO_FILE`/`_G_fpos_t` size asserts on MSVC. After the fix (toolchain pinned to 1.99.0, bindings generated by bindgen with libclang 18), locally: `cargo fmt --check` passes; clippy `-D warnings` passes in the three configurations; `cargo test --release --locked` passes 141 unit, 2 model end-to-end and 1 parity tests. (py-labs' own Next.js checks also ran on that PR; not relevant here) |
| CI round 2 (py-labs PR #20, commit 2dbe975) | `linux` and the Windows clippy/build/test steps passed on windows-latest (Visual Studio 18, MSVC 14.51). 141 unit tests passed; the model tests skip without models. `build-windows.ps1` packaged `dist\SwiftVoice`: 24.8 MB installed, 8.9 MB zip. Its smoke tests passed: version, `swiftvoice.com` output and exit-code relay, ONNX Runtime loading, telemetry off. The step still exited 1, because GitHub's pwsh wrapper propagates `$LASTEXITCODE` from the last native command (`--doctor`, which exits 1 without models). Reproduced locally with pwsh 7.4.6 and that wrapper; fixed with an explicit `exit 0` |
| Not run | Anything interactive on Windows: tray, hotkeys, overlay, insertion into applications, microphone capture, device changes, sleep/resume (`tests/MANUAL_TESTS.md`). Also: the Windows link and package (left to the CI workflow); Whisper inference (no model download possible); cold-start, hotkey-latency, idle-CPU and install-size targets; a 10-minute live dictation. The benchmark matrix ran before the window-join punctuation fix; that fix changes punctuation only, and the inserted-text WER was re-checked at 0 % |
