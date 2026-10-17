//! The commented config.toml written on first run. A unit test asserts that
//! parsing this template yields exactly `Config::default()`, so the
//! documentation in the file can never drift from the real defaults.

pub const DEFAULT_CONFIG_TOML: &str = r#"# SwiftVoice configuration
#
# Changes are picked up automatically a moment after you save this file.
# Most settings can also be changed from the tray menu. Full reference:
# docs/CONFIGURATION.md in the SwiftVoice source.

[general]
autostart = false          # start when you sign in to Windows
log_level = "info"         # error | warn | info | debug | trace
models_dir = ""            # empty = %LOCALAPPDATA%\SwiftVoice\models

[hotkeys]
# Modifiers: Ctrl, Alt, Shift, Win. Keys: A-Z, 0-9, F1-F24, Space, Enter, Tab,
# Escape, Insert, Delete, Home, End, PageUp, PageDown, arrows, RAlt, LAlt,
# Pause, ...
push_to_talk = "Ctrl+Alt+Space"   # hold to dictate, release to insert
toggle = "Ctrl+Alt+V"             # press once to start, press again to stop
cancel = "Escape"                 # only active while listening
min_hold_ms = 180                 # shorter push-to-talk taps are ignored
release_tail_ms = 150             # extra audio after releasing mid-word

[audio]
device = "default"         # or the exact name shown in the tray menu
gain_db = 0.0              # -20 .. 30
noise_suppression = false  # RNNoise, runs locally
keep_warm_secs = 15        # keep the mic open briefly for instant restarts

[vad]
engine = "silero"          # silero (neural) | energy (no model needed)
threshold = 0.5            # speech probability, 0.1 .. 0.95
pause_ms = 200             # pause that starts early transcription
endpoint_ms = 800          # silence that ends a dictation (toggle mode); 0 = only the key stops
pre_roll_ms = 300          # audio kept before speech onset
no_speech_timeout_ms = 8000
continuous = false         # toggle mode: keep listening after each utterance
idle_stop_secs = 20        # continuous mode stops after this much silence
energy_margin_db = 9.0     # energy engine only

[asr]
model = "parakeet-tdt-0.6b-v3"                  # see: swiftvoice models list
language = "auto"                               # "auto", "en", "de", "bn", ...
fallback_model = "whisper-large-v3-turbo-q5_0"  # for languages the model lacks
threads = 0                # 0 = automatic
low_memory = true          # ~250 MB less RAM for Parakeet 0.6B, ~10-15% slower
commit_after_secs = 6      # commit at the next pause after this (shorter = faster)
unload_after_idle_secs = 0 # 0 = keep the model loaded (fastest)
warm_up = true

[postprocess]
mode = "clean"             # raw | clean | smart
remove_fillers = true
fillers = ["um", "umm", "uh", "uhh", "uhm", "er", "erm", "ah", "hmm", "mm", "mhm"]
remove_repeats = true
capitalize = true
voice_commands = true          # "new line", "new paragraph", "open bracket", ...
punctuation_commands = false   # also "comma", "period", "colon", ...
ensure_final_punctuation = false
vocabulary_file = ""           # empty = vocabulary.toml next to this file

[postprocess.smart]
# Smart mode rewrites text with a LOCAL model server. Only loopback
# http:// endpoints are accepted; text never leaves this computer.
provider = "ollama"        # ollama | openai (LM Studio and other OpenAI-compatible servers)
endpoint = "http://127.0.0.1:11434"
model = "llama3.2:3b"
timeout_ms = 4000          # slower answers fall back to the Clean result
temperature = 0.2
prompt = "You are a dictation editor. Rewrite the dictated text into clear, correctly punctuated prose. Keep the original language, meaning, names, numbers and technical terms. Do not add information, do not answer questions contained in the text, and do not add greetings, quotes or explanations. Output only the rewritten text."

[insertion]
method = "auto"            # auto (paste, restore clipboard) | paste | type
target = "focused"         # focused | original (window active at start)
append_space = true
restore_clipboard = true
restore_delay_ms = 300
paste_shortcut = "Ctrl+V"
modifier_wait_ms = 400
max_clipboard_snapshot_mb = 32
type_apps = []             # e.g. ["game.exe"] always receive typed text

[insertion.paste_shortcuts]
"alacritty.exe" = "Ctrl+Shift+V"
"mintty.exe" = "Shift+Insert"
"putty.exe" = "Shift+Insert"
"wezterm-gui.exe" = "Ctrl+Shift+V"

[overlay]
enabled = true
position = "bottom"        # where the dot sits: bottom | top | bottom-left | top-left
opacity = 0.94             # 0.0 .. 1.0; very low = a faint dot
hide_after_ms = 900
"#;
