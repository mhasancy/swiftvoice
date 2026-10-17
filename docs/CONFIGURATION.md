# Configuration

## Files

| File | Location | Purpose |
|---|---|---|
| `config.toml` | `%APPDATA%\SwiftVoice\` | all settings; created with comments on first run |
| `vocabulary.toml` | next to `config.toml` (or `postprocess.vocabulary_file`) | your phrase replacements |
| logs | `%APPDATA%\SwiftVoice\logs\swiftvoice.log` (rotated at 5 MB, 3 kept) | diagnostics, no transcripts below `trace` |
| models | `%LOCALAPPDATA%\SwiftVoice\models\<id>\` (or `general.models_dir`) | downloaded models |

Other layouts:
- **Portable mode:** create an empty `swiftvoice.portable` next to
  `swiftvoice.exe` (`scripts\build-windows.ps1 -Portable`); `config.toml`,
  `vocabulary.toml` and `logs\` then live in `data\` and models in
  `models\`, next to the exe.
- **`SWIFTVOICE_HOME=<folder>`** puts everything under that folder.
- **`--config FILE`** uses another settings file.

Save the file and SwiftVoice applies it within about a second: the file is
checked once per second. If any value is invalid, a notification lists
*all* problems and the previous settings stay in effect. Changes made from
the tray menu rewrite only the affected key and keep your comments.
Unknown keys are ignored with a warning in the log (`swiftvoice --doctor`
lists them), so a file from an older version keeps working after an option
is removed.

`swiftvoice --doctor` checks the configuration, ONNX Runtime, telemetry
state, models and microphones.

## [general]

| Key | Default | Values | Meaning |
|---|---|---|---|
| `autostart` | `false` | bool | start at sign-in (`HKCU\…\Run\SwiftVoice`); also in the tray menu |
| `log_level` | `"info"` | error, warn, info, debug, trace | `--verbose` forces debug and echoes the log to the console |
| `models_dir` | `""` | path, `%VARS%` expanded | empty = `%LOCALAPPDATA%\SwiftVoice\models` |

## [hotkeys]

Format: modifiers `Ctrl`, `Alt`, `Shift`, `Win` joined with `+`, then one key.
- **Modifier keys:** `RAlt`, `LAlt`; either may also be used alone as the key
  (for example `toggle = "RAlt"`: press to start, press again to stop).
- **Letters and digits:** `A`-`Z`, `0`-`9`.
- **Function keys:** `F1`-`F24`.
- **Navigation and editing:** `Space`, `Enter`, `Tab`, `Escape`,
  `Backspace`, `Insert`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`,
  `Left`, `Up`, `Right`, `Down`.
- **Lock and system keys:** `Pause`, `ScrollLock`, `CapsLock`,
  `PrintScreen`, `NumLock`, `Apps`.
- **Numpad:** `Numpad0`-`Numpad9`, `NumpadAdd`, `NumpadSubtract`,
  `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`.
- **Punctuation keys:** `;` `=` `,` `-` `.` `/` `` ` `` `[` `\` `]` `'`.

A key without a modifier is only allowed for F1-F24, Pause, Scroll Lock,
Print Screen, RAlt and LAlt. Any other bare key would be blocked in every
application. RAlt/LAlt are watched with a low-level keyboard hook: the key
is captured before any application sees it, and on layouts where Right Alt
doubles as AltGr the AltGr combinations stop working. Empty string =
shortcut off.

| Key | Default | Meaning |
|---|---|---|
| `push_to_talk` | `"Ctrl+Alt+Space"` | hold to dictate, release to insert |
| `toggle` | `"Ctrl+Alt+V"` | press to start; a pause inserts the text (`endpoint_ms`); press again to stop — with `endpoint_ms = 0` only the key stops it |
| `cancel` | `"Escape"` | discard the current dictation; registered only while listening |
| `min_hold_ms` | `180` | shorter push-to-talk taps are ignored |
| `release_tail_ms` | `150` | if you release mid-word, recording continues this long |

Change hotkeys from the tray menu: *Hotkeys > Change push-to-talk…*, then
press the new combination (Right Alt alone is accepted too). The new
shortcut is checked against other programs before it is saved.

## [audio]

| Key | Default | Range | Meaning |
|---|---|---|---|
| `device` | `"default"` | `"default"` or a device name from the tray menu | a missing device falls back to the default with a notice |
| `gain_db` | `0.0` | -20 … 30 | digital gain before the VAD and recogniser |
| `noise_suppression` | `false` | bool | RNNoise, running locally; helps with fans and keyboards |
| `keep_warm_secs` | `15` | 0 … 3600 | keep the microphone open after a dictation so the next one starts instantly; 0 closes it immediately (the Windows mic indicator turns off sooner) |

## [vad]

| Key | Default | Range | Meaning |
|---|---|---|---|
| `engine` | `"silero"` | silero, energy | Silero VAD (needs the `silero-vad` model, 2.3 MB) or an energy detector |
| `threshold` | `0.5` | 0.1 … 0.95 | speech probability; raise it in noisy rooms |
| `pause_ms` | `200` | 64 … 2000 | a pause this long starts early transcription and is where long dictation is committed |
| `endpoint_ms` | `800` | 0, or 200 … 10000 and ≥ `pause_ms` | toggle mode: silence that ends the dictation; lower is snappier but may cut you off; 0 = never stop automatically, only the toggle key stops it |
| `pre_roll_ms` | `300` | 0 … 2000 | audio kept from before speech onset |
| `no_speech_timeout_ms` | `8000` | 1000 … 120000 | toggle mode gives up when nothing is said |
| `continuous` | `false` | bool | toggle mode keeps listening after each utterance (hands-free) |
| `idle_stop_secs` | `20` | 2 … 3600 | continuous mode stops after this much silence |
| `energy_margin_db` | `9.0` | 3 … 30 | energy engine: dB above the noise floor that counts as speech |

## [asr]

| Key | Default | Range | Meaning |
|---|---|---|---|
| `model` | `"parakeet-tdt-0.6b-v3"` | an id from `swiftvoice models list` | main recogniser |
| `language` | `"auto"` | `auto` or ISO 639-1/-3 (`en`, `de`, `bn`, …) | `auto` uses the main model's own language detection; a language the main model lacks is routed to `fallback_model` |
| `fallback_model` | `"whisper-large-v3-turbo-q5_0"` | a Whisper id | used for languages Parakeet does not support (e.g. Bengali) |
| `threads` | `0` | 0 … 64 | inference threads; 0 = automatic (one per physical core) |
| `low_memory` | `true` | bool | skip ONNX Runtime weight pre-packing for large models: about 250 MB less RAM for the 0.6B models, 10-15 % slower transcription (docs/BENCHMARKS.md) |
| `commit_after_secs` | `6` | 4 … 25 | commit dictation at the next pause after this many seconds; shorter means less work after you stop, longer gives the model more context |
| `unload_after_idle_secs` | `0` | 0 = never | drop the model after this much idle time and return the memory (57-74 MB remained in the Linux benchmark); the next dictation reloads it (1.5-3.5 s) |
| `warm_up` | `true` | bool | run one silent inference after loading so the first dictation is fast |

## [postprocess]

| Key | Default | Meaning |
|---|---|---|
| `mode` | `"clean"` | `raw`: exactly what the model produced. `clean`: vocabulary, voice commands, filler and repeat removal, capitalisation, spacing (< 1 ms). `smart`: Clean, then a local LLM rewrite (below). |
| `remove_fillers` | `true` | drop the words in `fillers` |
| `fillers` | `["um", "umm", "uh", "uhh", "uhm", "er", "erm", "ah", "hmm", "mm", "mhm"]` | case-insensitive whole words |
| `remove_repeats` | `true` | "the the" becomes "the" |
| `capitalize` | `true` | capitalise sentence starts and "I" (English) |
| `voice_commands` | `true` | "new line", "new paragraph", "open/close bracket", "open/close parenthesis/paren", "open/close square bracket", "open/close (curly) brace", "open quote", "close/end quote", "question mark", "exclamation mark/point", "full stop" |
| `punctuation_commands` | `false` | also "comma", "period", "colon", "semicolon", "ellipsis"; off by default because the words occur in prose |
| `ensure_final_punctuation` | `false` | add a full stop if the text ends without punctuation |
| `vocabulary_file` | `""` | empty = `vocabulary.toml` next to `config.toml` |

### [postprocess.smart]

Smart mode is **off by default** and **local only**. The endpoint must be a
plain-HTTP loopback address: `127.0.0.0/8`, `::1` or `localhost`. Anything
else is rejected when the settings are loaded. Text is never sent anywhere
else. If the server is not running, is slow, or returns something
implausible, the Clean text is inserted and you get one notification.

| Key | Default | Meaning |
|---|---|---|
| `provider` | `"ollama"` | `ollama` (`/api/chat`) or `openai` (OpenAI-compatible: LM Studio, llama.cpp server, …; `/v1/chat/completions`) |
| `endpoint` | `"http://127.0.0.1:11434"` | server base URL |
| `model` | `"llama3.2:3b"` | model name as the server knows it |
| `timeout_ms` | `4000` | 200 … 60000; slower answers fall back to Clean |
| `temperature` | `0.2` | 0 … 2 |
| `prompt` | dictation-editor instruction | system prompt; the dictated text is the user message |

Smart mode adds the LLM's response time to every dictation. That time
depends on your model and hardware, and was not measured for this release.
That is why Smart mode is opt-in, and why `timeout_ms` caps the wait.

## [insertion]

| Key | Default | Meaning |
|---|---|---|
| `method` | `"auto"` | `auto`: paste through the clipboard and restore it, typing instead when the clipboard is unavailable. `paste`: always paste. `type`: Unicode key events, never touches the clipboard (slower for long text; some apps, games and remote desktops handle it better) |
| `target` | `"focused"` | `focused`: the window focused when the text is ready. `original`: the window focused when dictation started; if it is gone, the text stays on the clipboard |
| `append_space` | `true` | add a space after the inserted text (unless it ends with a space or new line), so the next dictation does not run into it |
| `restore_clipboard` | `true` | put your previous clipboard back after pasting, unless something else changed it meanwhile |
| `restore_delay_ms` | `300` | 50 … 5000; how long the target app gets to read the clipboard before it is restored |
| `paste_shortcut` | `"Ctrl+V"` | default paste keys |
| `modifier_wait_ms` | `400` | 0 … 3000; wait for you to release the hotkey modifiers before pasting |
| `max_clipboard_snapshot_mb` | `32` | 1 … 512; a larger clipboard is not snapshotted, and the text is typed instead |
| `type_apps` | `[]` | executable names that always get typed text, e.g. `["game.exe"]` |
| `[insertion.paste_shortcuts]` | alacritty `Ctrl+Shift+V`, mintty `Shift+Insert`, putty `Shift+Insert`, wezterm-gui `Ctrl+Shift+V` | per-executable paste keys (case-insensitive exe name) |

## [overlay]

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | small click-through status window (it never takes focus) |
| `position` | `"bottom"` | bottom or top of the monitor that shows the focused window |
| `opacity` | `0.94` | 0.3 … 1.0 |
| `hide_after_ms` | `900` | 0 … 10000; how long the result stays visible |

While you dictate, a card near the bottom (or top) of the monitor that
holds the focused window shows the live transcript as it comes in; a red
dot on the card follows your voice. When the text has been inserted a bare
green dot flashes at the middle-left for `hide_after_ms` and disappears —
a successful insertion shows no card. When nothing can take the text (no
focused window, the desktop, the taskbar, an icon or file list) the card
says so and the text stays on the clipboard instead.

## Vocabulary (`vocabulary.toml`)

```toml
# Each entry turns a spoken phrase into the text you want. Matching ignores
# case and surrounding punctuation; the longest phrase wins. Used in Clean
# and Smart modes.

[[replace]]
from = "cube and eighties"
to = "Kubernetes"

[[replace]]
from = "swift voice"
to = "SwiftVoice"

[[replace]]
from = "my email"
to = "name@example.com"

# Special targets: "\n" new line, "\n\n" new paragraph; a single punctuation
# character such as "(" or ")" attaches to the next/previous word.
[[replace]]
from = "new bullet"
to = "\n• "

# Bangla-English mixed dictation: map romanised or misrecognised phrases to
# Bengali script (or the other way round).
[[replace]]
from = "dhonnobad"
to = "ধন্যবাদ"
```

Your entries override built-in voice commands with the same phrase. With
Whisper, your `to` terms are also passed as a recognition hint, the
initial prompt, so names are spelled your way more often.

## Examples

**Fastest English setup (older PCs):**

```toml
[asr]
model = "parakeet-tdt-ctc-110m-en"
language = "en"
```

**Hands-free continuous dictation:**

```toml
[vad]
continuous = true
endpoint_ms = 700
idle_stop_secs = 30
```

**Bengali with Bangla-English mixing:**

```toml
[asr]
language = "bn"                                  # routed to the Whisper fallback
fallback_model = "whisper-large-v3-turbo-q5_0"
```

Then download the model: `swiftvoice models download whisper-large-v3-turbo-q5_0`.

**Smart mode with Ollama:**

```toml
[postprocess]
mode = "smart"

[postprocess.smart]
provider = "ollama"
endpoint = "http://127.0.0.1:11434"
model = "llama3.2:3b"     # ollama pull llama3.2:3b
```

**Smart mode with LM Studio** (start its local server; default port 1234):

```toml
[postprocess.smart]
provider = "openai"
endpoint = "http://127.0.0.1:1234"
model = "the-model-id-shown-in-lm-studio"
```

**Never touch the clipboard:**

```toml
[insertion]
method = "type"
```

**Low memory while idle:**

```toml
[asr]
unload_after_idle_secs = 300   # unload after 5 minutes without dictation
```

## Command line

```
swiftvoice [--verbose] [--config FILE]        run the tray app
swiftvoice --benchmark --audio FILE.wav [--reference REF.txt] [--model ID] [--runs N] [--json OUT]
           [--mode raw|clean|smart] [--language CODE] [--threads N] [--low-memory on|off]
           [--commit-after-secs N] [--release-delay-ms MS] [--models-dir DIR]
swiftvoice --transcribe FILE.wav [--model ID] [--mode MODE] [--language CODE]
swiftvoice --record FILE.wav [--seconds N]    record from the microphone (Windows)
swiftvoice --doctor
swiftvoice models list | download ID… | verify [ID…] | remove ID
swiftvoice --version | --help
```
