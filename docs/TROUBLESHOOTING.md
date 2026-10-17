# Troubleshooting

Start here:

```powershell
swiftvoice --doctor          # config, ONNX Runtime, telemetry off, models, microphones
swiftvoice --verbose         # run the tray app with debug logging (also printed to a console)
```

Logs are in `%APPDATA%\SwiftVoice\logs\swiftvoice.log`. Open them from the
tray: *Open logs folder*. Logs contain timings and errors, never your
dictated text, unless you set `log_level = "trace"`.

## Start-up

**"ONNX Runtime 1.31.0 was not found. SwiftVoice needs onnxruntime.dll next
to swiftvoice.exe."** Keep `onnxruntime.dll` in the same folder as the exe.
It is part of the zip from `scripts\build-windows.ps1`. To use a copy
elsewhere, set `SWIFTVOICE_ORT_DYLIB=C:\path\onnxruntime.dll`. SwiftVoice
deliberately does not load a `onnxruntime.dll` from System32 or PATH,
because a different version there could crash or misbehave.

**"The code execution cannot proceed because MSVCP140.dll /
VCRUNTIME140_1.dll was not found."** The four Visual C++ runtime DLLs were
removed from the folder. Restore them from the zip, or install the latest
Visual C++ Redistributable (x64) from
`https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist`.

**Build: "libclang.dll (LLVM) is needed to build the whisper.cpp fallback".**
The Whisper fallback's Rust bindings are generated at build time, which
needs libclang. Install LLVM (`winget install LLVM.LLVM`) or *C++ Clang
Compiler for Windows* in the Visual Studio Installer. Alternatively, set
`LIBCLANG_PATH` to the folder that contains `libclang.dll`, or skip the
fallback: `cargo build` without `--features whisper`, or
`scripts\build-windows.ps1 -NoWhisper`.

**Windows SmartScreen: "Windows protected your PC".** Your own build is not
code-signed. Click *More info > Run anyway*, or sign the exe with your
certificate.

**"SwiftVoice is already running."** Only one copy runs at a time. Look for
the microphone icon in the notification area; click `^` next to the clock
if it is hidden.

**No tray icon after Explorer crashed.** SwiftVoice re-adds it when
Explorer restarts. If it is still missing, quit from Task Manager and start
it again.

## Hotkeys

**"Ctrl+Alt+Space is already used by another application or by Windows;
choose another shortcut."** Another program (often a launcher, IME, game
overlay or remote-desktop client) registered the same combination first.
Pick another one: *Hotkeys > Change push-to-talk…*. Only one program can own
a global hotkey.

**The hotkey does nothing in one particular app.**
- If that app runs **as administrator** and SwiftVoice does not, Windows
  delivers the hotkey but blocks pasting into the app. SwiftVoice detects
  this, leaves the text on the clipboard, and tells you to press Ctrl+V.
  To dictate directly into elevated apps, run SwiftVoice elevated too:
  right-click > *Run as administrator*. Autostart does not do that.
- Full-screen exclusive games can swallow input. Try `type_apps` or
  windowed mode.

**AltGr / international keyboards.** Ctrl+Alt is the same as AltGr on many
layouts, so Ctrl+Alt+V may clash with typing a character. Choose a shortcut
with Win or Shift, or a bare F13-F24 key, for example on a macro keyboard.
A lone Right Alt shortcut (`toggle = "RAlt"`) takes the key over
system-wide too, so AltGr combinations stop working while SwiftVoice runs.

**Push-to-talk inserts nothing after short taps.** Holds shorter than
`hotkeys.min_hold_ms` (180 ms) are ignored on purpose.

## Microphone

**"Windows blocked the microphone."** Open *Settings > Privacy & security >
Microphone*, turn on *Microphone access* and *Let desktop apps access your
microphone*. No restart is needed.

**"The microphone is sending pure silence."** The device is muted (often a
hardware mute key or switch), or privacy settings block it. Check the input
level in *Settings > System > Sound > Input*.

**"Another app is using the microphone exclusively."** Close apps that use
WASAPI exclusive mode (some DAWs and call apps), or pick another device in
the tray.

**"microphone glitch (recording continues)" in the log during a call.**
Another app (Google Meet, Zoom, OBS) keeps the microphone open even while
muted, and the shared driver dropped a few samples. Dictation continues with
at most a tiny gap; no action is needed. Each dictation ends with a
`session N: ...` log line that counts how much audio arrived and how many
glitches or dropped samples it contained, so a word that went missing can be
checked against what the microphone actually delivered.

**Wrong microphone.** Tray: *Microphone* lists the devices. If a chosen
device disappears, SwiftVoice falls back to the default and says so.

**Words cut off at the start.** Raise `vad.pre_roll_ms` (default 300), or
lower `vad.threshold` (default 0.5) for quiet voices. Words cut off at the
end in toggle mode: raise `vad.endpoint_ms` (or set it to 0 to stop only
with the toggle key).

**False starts from background noise.** Turn on *Noise suppression*, raise
`vad.threshold` to 0.6-0.7, or use push-to-talk.

## Text and insertion

**Text appears in the wrong window.** By default text goes to the window
focused when it is ready. Use `insertion.target = "original"` to always
use the window that was focused when you started.

**Nothing is pasted in a terminal.** Some terminals paste with Shift+Insert
or Ctrl+Shift+V. Add your terminal's exe to `[insertion.paste_shortcuts]`,
or use `method = "type"`.

**My clipboard content was replaced.** If you copy something during the
`restore_delay_ms` window (300 ms), your new copy wins and the old content
is not restored; this is intended. A clipboard manager that rewrites the
clipboard on every change can also stop the restore. In both cases the log
says "clipboard changed after paste".

**A card says insertion is busy or stopped.** The inserter thread could
not take the text in time, so it was copied to the clipboard instead;
press Ctrl+V. If every dictation says "Insertion stopped", the inserter
thread has exited: restart SwiftVoice.

**Typed text has wrong characters.** Remote-desktop clients and some games
handle Unicode key events poorly. Use the default paste method for those
apps. If an app mis-handles paste, add it to `type_apps`.

**Fillers, voice commands or capitals are not applied.** Check the mode.
*Raw* inserts the model's output unchanged. Use *Clean*.

**A spoken word is always misrecognised.** Add a `[[replace]]` entry to
`vocabulary.toml` (tray: *Text > Edit vocabulary…*). See CONFIGURATION.md.

## Models

**"No speech model installed".** Tray: *Engine & model > Download …*, or
`swiftvoice models download parakeet-tdt-0.6b-v3 silero-vad`.

**Download fails behind a proxy or firewall.** Downloads use Windows'
WinHTTP with automatic proxy detection, falling back to the WinHTTP default
proxy (`netsh winhttp show proxy`). They come from `github.com`, which
redirects to GitHub's download servers, `raw.githubusercontent.com` (Silero
VAD) and `huggingface.co` (Whisper). You can also download on another computer and
copy the folder:
1. The URL and digest are in `models/README.md`.
2. Extract the archive into `%LOCALAPPDATA%\SwiftVoice\models\<id>\`, so
   that `encoder.int8.onnx` etc. sit directly in that folder.
3. Run `swiftvoice models verify`.

**"… is corrupted (SHA-256 …, expected …)".** The file does not match the
pinned digest. Remove it (`swiftvoice models remove <id>`) and download it
again. SwiftVoice never loads an unverified model.

**"this build of SwiftVoice was compiled without the whisper.cpp engine".**
The build did not include the `whisper` cargo feature: a plain
`cargo build`, or `scripts\build-windows.ps1 -NoWhisper`. Rebuild with
`--features whisper` (needs CMake and libclang) to use Bengali and other
non-Parakeet languages.

**"Parakeet TDT 0.6B v3 does not support Bengali; set asr.fallback_model to
a Whisper model to use it".** Set `asr.fallback_model` to one of the
`whisper-*` ids.

**First dictation after start-up is slow.** The model loads in the
background at start: about 3 s for the 0.6B models and 1.5 s for 110M, plus
disk read time on a cold start. Keep `asr.warm_up = true`. If
`unload_after_idle_secs` is set, the reload happens on the next dictation.

**High memory use.** The 0.6B models need about 0.85-1.1 GB while loaded.
Use the 110M model (about 250 MB), keep `asr.low_memory = true`, or set
`asr.unload_after_idle_secs`. See docs/BENCHMARKS.md.

## Smart mode

**"Smart mode unavailable, inserted the Clean text: …".** The local server
did not answer in time (`timeout_ms`), is not running, or the model name is
wrong. Check it outside SwiftVoice:

```powershell
curl http://127.0.0.1:11434/api/tags          # Ollama: lists installed models
ollama pull llama3.2:3b
```

**"postprocess.smart.endpoint: host must be localhost, 127.x.x.x or ::1".**
This is by design. Smart mode never sends text to another machine, not even
on your LAN.

## Reporting a problem

Attach `swiftvoice --doctor` output and the relevant part of
`swiftvoice.log` from a `--verbose` run. Logs contain no dictated text at
the default level. Record a short sample with
`swiftvoice --record sample.wav --seconds 10` and include
`swiftvoice --transcribe sample.wav --mode raw` if recognition is the
problem.
