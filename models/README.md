# Models

SwiftVoice ships without models. You pick and download them on first run,
from the tray menu (*Engine & model*) or the command line:

```powershell
swiftvoice models list
swiftvoice models download parakeet-tdt-0.6b-v3 silero-vad
swiftvoice models verify
```

Models live in `%LOCALAPPDATA%\SwiftVoice\models\<model id>\`. Change the
folder with `general.models_dir` or *Engine & model > Choose models
folder…*. Each download is resumable, so an interrupted one continues where
it stopped. It must match the size and digest pinned in
`src/asr/catalog.rs` before it is installed. A mismatch is deleted and
reported, never loaded. After the download, SwiftVoice makes no network
requests.

## Catalogue

| Id | Engine | Languages | Licence | Download | RAM loaded¹ | Use |
|---|---|---|---|---|---|---|
| `parakeet-tdt-0.6b-v3` (default) | Parakeet TDT, ONNX Runtime | 25 European: bg hr cs da nl en et fi fr de el hu it lv lt mt pl pt ro sk sl es sv ru uk | CC-BY-4.0 | 465 MB | 850 MB | best accuracy, multilingual, punctuation and capitals |
| `parakeet-tdt-0.6b-v2` | Parakeet TDT, ONNX Runtime | English | CC-BY-4.0 | 460 MB | 830 MB | English only |
| `parakeet-tdt-ctc-110m-en` | NeMo CTC, ONNX Runtime | English | CC-BY-4.0 | 100 MB | 250 MB | fastest and lightest (about 3x faster than 0.6B) |
| `whisper-large-v3-turbo-q5_0` (fallback) | whisper.cpp | ~99, incl. Bengali | MIT | 547 MiB | not measured | languages Parakeet lacks |
| `whisper-small` | whisper.cpp | ~99, incl. Bengali | MIT | 466 MiB | not measured | smaller fallback |
| `whisper-base` | whisper.cpp | ~99, incl. Bengali | MIT | 142 MiB | not measured | smallest fallback, lower accuracy |
| `silero-vad` | Silero VAD v6.2, ONNX Runtime | any | MIT | 2.3 MB | a few MB | speech detection (optional; an energy detector needs no model) |

¹ Resident memory of the benchmark process with the model loaded and
`asr.low_memory = true`. The hardware and method are in
[docs/BENCHMARKS.md](../docs/BENCHMARKS.md). The Whisper figures were not
measured, because the Whisper files could not be downloaded in the
development environment.

## Sources and integrity

| Id | URL | Pinned digest |
|---|---|---|
| parakeet-tdt-0.6b-v3 | `https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2` | SHA-256 `5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf`, 487,170,055 bytes |
| parakeet-tdt-0.6b-v2 | `…/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2` | SHA-256 `157c157bc51155e03e37d2466522a3a737dd9c72bb25f36eb18912964161e1ad`, 482,468,385 bytes |
| parakeet-tdt-ctc-110m-en | `…/asr-models/sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8.tar.bz2` | SHA-256 `17f945007b52ccd8b7200ffc7c5652e9e8e961dfdf479cefcabd06cf5703630b`, 104,337,827 bytes |
| whisper-large-v3-turbo-q5_0 | `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin` | SHA-1 `e050f7970618a659205450ad97eb95a18d69c9ee` |
| whisper-small | `…/resolve/main/ggml-small.bin` | SHA-1 `55356645c2b361a969dfd0ef2c5a50d530afd8d5` |
| whisper-base | `…/resolve/main/ggml-base.bin` | SHA-1 `465707469ff3a37a2b9b8d8f89f2f99de7299dac` |
| silero-vad | `https://raw.githubusercontent.com/snakers4/silero-vad/v6.2/src/silero_vad/data/silero_vad.onnx` | SHA-256 `1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3`, 2,327,524 bytes |

The Parakeet and Silero digests were computed from the downloaded files
while building this release. The Parakeet archives are the int8 exports by
the sherpa-onnx project of NVIDIA's NeMo checkpoints (model cards:
`https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3`, `…/parakeet-tdt-0.6b-v2`,
and NGC `nvidia/nemo/parakeet-tdt_ctc-110m`). SwiftVoice extracts only the
expected files: `encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx`
and `tokens.txt`, or `model.int8.onnx` and `tokens.txt` for the 110M model.
Their individual sizes and SHA-256 values are pinned as well. The Whisper
digests are the SHA-1 values that whisper.cpp publishes for its ggml files.

## Licences

- **Parakeet (NVIDIA): CC-BY-4.0.** Commercial use is allowed with
  attribution. SwiftVoice shows the attribution in *About SwiftVoice* and in
  the README. If you redistribute the model files, keep the attribution:
  "Parakeet TDT 0.6B v3/v2 and Parakeet TDT-CTC 110M by NVIDIA, licensed
  under CC-BY-4.0".
- **Whisper (OpenAI) weights and whisper.cpp: MIT.**
- **Silero VAD: MIT.**
- **ONNX Runtime (Microsoft): MIT.** The package includes its
  `ThirdPartyNotices.txt`.

## Which one?

- English, lowest latency or an older PC: `parakeet-tdt-ctc-110m-en`.
- European languages, or best accuracy: `parakeet-tdt-0.6b-v3` (default).
- Bengali, or any language outside the 25: select it under *Language*. A
  language Parakeet does not support is routed to the fallback model
  (`asr.fallback_model`, Whisper large-v3-turbo by default). Whisper is
  compiled in by the default `whisper` feature and needs CMake at build
  time. Its speed and memory were not measured for this release, because
  its files could not be downloaded in the development environment.
  Benchmark it on your machine with
  `swiftvoice --benchmark --audio x.wav --model whisper-large-v3-turbo-q5_0 --language bn`. With *Auto-detect*,
  Parakeet's own language detection is used, which only covers its 25
  languages. Choose Bengali explicitly to get Whisper.
- Low RAM: the 110M model (250 MB), or set `asr.unload_after_idle_secs`.
  The model is then dropped when idle. On the Linux reference run the
  process fell from about 850 MB to 58 MB; Windows was not measured. The
  next dictation pays the load time, 1.5-3.5 s on the reference machine.
