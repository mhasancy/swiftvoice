# Manual tests (Windows desktop)

Global hotkeys, synthetic input, the clipboard, the tray and the overlay
need a real interactive Windows session. CI and the automated tests cannot
cover them. The automated tests exercise the same decision logic through
fakes: `insertion::tests`, `controller::tests` and `hotkey::tests`.

Run this list on Windows 10 22H2 and Windows 11 before a release. Record the
build, Windows version and results in `docs/BENCHMARKS.md` (performance) or
the release notes (pass/fail).

Preparation:

```powershell
scripts\build-windows.ps1
dist\SwiftVoice\swiftvoice.exe models download parakeet-tdt-0.6b-v3 silero-vad
dist\SwiftVoice\swiftvoice.exe --doctor          # every line [ok]
dist\SwiftVoice\swiftvoice.exe --verbose         # tray app with a debug log
```

Logs: `%APPDATA%\SwiftVoice\logs\swiftvoice.log`. With `--verbose`, each
utterance logs `transcript ready N ms after it ended` and the recogniser
timings.

## A. Start-up and tray

| # | Steps | Expected |
|---|---|---|
| A1 | Start `swiftvoice.exe` with no models installed. | Tray icon appears within 1 s. A dialog names the model, size, source and licence and offers the download. *Yes* shows progress in the tray tooltip ("downloading … N%") and the menu. |
| A2 | Start a second copy. | It exits with "SwiftVoice is already running. Look for the microphone icon in the notification area…". |
| A3 | Right-click the tray icon. | Menu: status, Start listening, Hotkeys, Microphone, Engine & model, Language, Text, Options, Start with Windows, Open settings file, Open logs folder, About, Quit. |
| A4 | Restart Explorer (Task Manager > Windows Explorer > Restart). | The tray icon comes back. |
| A5 | *Quit*. | Icon disappears; no `swiftvoice.exe` remains in Task Manager. |

## B. Hotkeys

| # | Steps | Expected |
|---|---|---|
| B1 | Focus Notepad. Hold **Ctrl+Alt+Space**, say "testing one two three", release. | The red dot appears in its corner (its size follows your voice); no text card is ever shown. Then the text appears in Notepad: "Testing one two three." A bare green dot flashes where the dot was and disappears. |
| B2 | Tap Ctrl+Alt+Space briefly (< 180 ms). | Nothing is inserted (accidental tap). |
| B3 | Press **Ctrl+Alt+V**, speak a sentence, stop talking. | Text appears about 0.8 s after you stop (endpoint), listening ends. |
| B4 | Ctrl+Alt+V, speak, press **Escape** before the endpoint. | Nothing is inserted (the dot simply disappears). Escape works normally in other apps again afterwards (it is registered only while listening). |
| B5 | Tray > Hotkeys > Change push-to-talk…, press Ctrl+Shift+D. | The dialog shows the combination, closes; the tooltip/menu shows the new key; Ctrl+Shift+D now dictates and the old key no longer does. `config.toml` contains `push_to_talk = "Ctrl+Shift+D"` with comments preserved. |
| B6 | In the change dialog, press a combination another program has registered, or SwiftVoice's other shortcut. | A "Shortcut not changed" notification explains the conflict; the old shortcut keeps working and `config.toml` is unchanged. A bare key such as `A` is rejected inside the dialog (only F1-F24, Pause, Scroll Lock, Print Screen and RAlt may be used without a modifier). |
| B7 | Edit `config.toml`, set `toggle = "Ctrl+Alt+Q"`, save. | Within about 1 s the new hotkey works (file watch); no restart needed. |
| B8 | Hold push-to-talk while saying a word and release mid-word. | The last word is complete: recording continues for `release_tail_ms`. |
| B9 | Hotkey while a full-screen game or an elevated window is focused. | Dictation works or a notice explains why the text is on the clipboard (see C5). |
| B10 | In the change dialog, press and release **Right Alt** on its own. | The dialog accepts it as the shortcut ("RAlt"); the new key dictates. |
| B11 | Set `toggle = "RAlt"` and `vad.endpoint_ms = 0` in `config.toml`, save. Press Right Alt, speak with several pauses, press Right Alt again. | The first press starts (the red dot appears in its corner); pauses never stop it; the second press inserts everything at once. Right Alt reaches no other app while SwiftVoice runs (on AltGr layouts that disables AltGr). Escape still cancels. |

## C. Insertion

| # | Target | Expected |
|---|---|---|
| C1 | Notepad, WordPad/Word, Outlook body | Text at the cursor; previous clipboard content restored (check with Ctrl+V into another app afterwards). |
| C2 | Chrome/Edge address bar and a Google Docs document; an Electron app (VS Code, Slack, Teams) | Text inserted once, no duplicate, no stray "v". |
| C3 | Windows Terminal, cmd.exe, PowerShell; mintty (Git Bash) and PuTTY | Text pasted. mintty/PuTTY use Shift+Insert (`insertion.paste_shortcuts`). Nothing is executed: the text contains no newline unless "new line" was spoken. |
| C4 | Clipboard holds an image (Win+Shift+S screenshot) before dictating | The image is still on the clipboard afterwards. Win+V clipboard history does **not** contain the dictated text. |
| C5 | Notepad started *as administrator*, SwiftVoice not elevated | Balloon: the window runs as administrator; the text is on the clipboard. Ctrl+V pastes it. |
| C6 | Copy something new within 300 ms after the text appears | The new copy wins: the old clipboard is not restored over it (log: "clipboard changed after paste"). |
| C7 | Set `method = "type"`, dictate "Grüße, naïve café — ধন্যবাদ" via a vocabulary entry | Exact Unicode text typed; clipboard untouched. |
| C8 | `target = "original"`, start toggle dictation in Notepad, click into WordPad before it ends | Text goes to Notepad (window active at start), or to the clipboard (the dot flashes amber) if Notepad was closed. |
| C9 | Say "first line new line second line new paragraph open bracket aside close bracket" | `First line⏎Second line⏎⏎(Aside)` in Clean mode (unit test `voice_commands_give_structure_in_clean_mode`). |
| C10 | Mode Raw | Exact model output, no filler removal or vocabulary. |
| C11 | Click an empty spot on the desktop, or the taskbar; or click an icon in a folder in File Explorer; dictate | Nothing is typed; the dot flashes amber and the dictated text replaces the previous clipboard content (it is not restored). Ctrl+V in any app pastes the sentence. A game with no text field still receives typed text when its exe is listed in `insertion.type_apps`. |

## D. Audio and devices

| # | Steps | Expected |
|---|---|---|
| D1 | Settings > Privacy > Microphone: deny desktop apps; dictate | Notice explaining how to allow microphone access; no crash. Allow, dictate again: works without restart. |
| D2 | Unplug the USB/Bluetooth microphone while idle, then dictate | Falls back to the default device with a notice, or "No microphone is available". Plug back in: tray > Microphone lists it. |
| D3 | Unplug while dictating | Recording stops with a notice; text spoken so far is still transcribed. |
| D4 | Tray > Microphone > choose a specific device; Input gain +10 dB; Noise suppression on | The settings persist in `config.toml`; dictation still works (noise suppression on a noisy fan should reduce false starts). |
| D5 | Mute the microphone in hardware, dictate | "The microphone sends silence" warning. |
| D6 | Sleep the PC while idle, wake it, dictate | Works; the log shows "resumed from sleep". |
| D7 | Lock the screen (Win+L) while toggle dictation is listening | Listening stops; nothing is typed onto the lock screen. |
| D8 | 10 minutes of continuous dictation (toggle + *Keep listening*), reading a text aloud | No gaps or dropped words at commit boundaries; `swiftvoice.log` shows no "audio overrun" warnings; Task Manager memory stays flat. |

## E. Models, languages and Smart mode

| # | Steps | Expected |
|---|---|---|
| E1 | Language > German; dictate German | German text with umlauts (Parakeet v3). |
| E2 | Language > Bengali with the Whisper fallback installed | Bengali script. The log shows the job running on `whisper-large-v3-turbo-q5_0`. Without it installed, the first dictation opens the download dialog for the fallback model. |
| E3 | Engine & model > Parakeet 110M | English dictation noticeably faster; RAM in Task Manager about 250 MB. |
| E4 | Smart mode without Ollama running | The Clean text is inserted; one notification "Smart mode unavailable, inserted the Clean text: …" names the connection error. |
| E5 | Smart mode with Ollama (`ollama run llama3.2:3b`) | Rewritten text within `timeout_ms`; Wireshark/Resource Monitor shows connections only to 127.0.0.1. |
| E6 | Set `smart.endpoint = "http://example.com"` in `config.toml` | "Settings not applied" notification: the endpoint must be a loopback address. The previous settings stay in effect. |

## F. Performance (record numbers in docs/BENCHMARKS.md)

| # | Measure | Target |
|---|---|---|
| F1 | Start-up: double-click to tray icon (stopwatch / Process Monitor) | < 1 s |
| F2 | Hotkey to recording: time from key-down to the overlay | < 50 ms |
| F3 | `scripts\benchmark.ps1` with the default model | release-to-text < 300 ms on a modern 8-core CPU |
| F4 | Task Manager, idle after 1 minute, model loaded / after `unload_after_idle_secs` | report both |
| F5 | Task Manager CPU, idle | ~0 % |
| F6 | `dist\SwiftVoice` folder size | < 30 MB |
