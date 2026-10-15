# tools/model-export (developer-only)

Nothing in this folder is needed to build, install or run SwiftVoice. The app
never runs Python. It downloads ready-made ONNX exports and verifies their
SHA-256 (see [models/README.md](../../models/README.md)).

This folder holds the Python used to **verify** those exports, and documents
how to **re-create** them from NVIDIA's checkpoints if you prefer not to trust
a third-party export.

## Setup

Python 3.10 or newer, in a virtual environment:

```bash
python -m venv .venv
. .venv/bin/activate            # Windows: .venv\Scripts\activate
pip install -r tools/model-export/requirements.txt
```

`requirements.txt` pins the versions that produced the results in
[docs/BENCHMARKS.md](../../docs/BENCHMARKS.md).

Both scripts set `ORT_DISABLE_TELEMETRY=1` before they import ONNX Runtime.
Microsoft's Linux and macOS builds otherwise start a telemetry uploader.

## make_feature_refs.py: front-end parity

SwiftVoice computes NeMo's log-mel features in Rust (`src/asr/features.rs`).
This script runs NeMo's own preprocessor on the same audio, using the ONNX
export of it that ships inside the `onnx-asr` package (`nemo128.onnx` for
Parakeet 0.6B, `nemo80.onnx` for the 110M model). It writes reference
features for `tests/feature_parity.rs`.

```bash
python tools/model-export/make_feature_refs.py speech-16k.wav /tmp/featref
SWIFTVOICE_FEATURE_REF=/tmp/featref cargo test --release --no-default-features --test feature_parity -- --nocapture
```

The input must be 16 kHz mono. The `0.wav` inside the sherpa-onnx Parakeet
110M archive works. The test requires a max absolute difference below 2e-2
and a mean below 1e-3, on unit-variance features. The measured values are
in docs/BENCHMARKS.md, around 2e-4 max.

## sherpa_reference.py: transcript parity

This script transcribes WAV files with sherpa-onnx, k2-fsa's C++ runtime, from
the same model folder SwiftVoice loads. Use it to compare against
`swiftvoice --transcribe FILE.wav --mode raw` or `tests/models_e2e.rs`.

```bash
python tools/model-export/sherpa_reference.py ~/models/parakeet-tdt-0.6b-v3 tdt en.wav de.wav
python tools/model-export/sherpa_reference.py ~/models/parakeet-tdt-ctc-110m-en ctc 0.wav
```

## Re-exporting Parakeet from the NVIDIA checkpoint (optional)

SwiftVoice's catalogue uses the int8 exports published by the sherpa-onnx
project in its `asr-models` GitHub release. They were made with
sherpa-onnx's own scripts:

- `https://github.com/k2-fsa/sherpa-onnx/blob/master/scripts/nemo/parakeet-tdt-0.6b-v3/export_onnx.py`
  and `run.sh` in the same folder (v2: `scripts/nemo/parakeet-tdt-0.6b-v2/`).

These scripts download `parakeet-tdt-0.6b-v3.nemo` from Hugging Face
(`nvidia/parakeet-tdt-0.6b-v3`, CC-BY-4.0). They install `nemo_toolkit[asr]`
and export the encoder, decoder (prediction network) and joiner. Then they
quantise them with `onnxruntime.quantization.quantize_dynamic` (encoder
QUInt8, decoder/joiner QInt8) and write `tokens.txt` with `<blk>` as the last
id. To re-create the files:

1. Clone `https://github.com/k2-fsa/sherpa-onnx` and `cd scripts/nemo/parakeet-tdt-0.6b-v3`.
2. Run `run.sh`, or its `pip install` line followed by `python3 export_onnx.py`.
   This downloads the full-precision `.nemo` checkpoint: 600 M parameters
   in 32-bit floats, about 2.4 GB.
3. Copy `encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx` and
   `tokens.txt` into one folder.
4. Put the folder where SwiftVoice looks for that model:
   `<models folder>\parakeet-tdt-0.6b-v3\`. The models folder defaults to
   `%LOCALAPPDATA%\SwiftVoice\models`; the tray menu *Engine & model >
   Choose models folder…* changes it. Then create an empty file named
   `CUSTOM_EXPORT` in that sub-folder. Without the marker, SwiftVoice
   expects the exact pinned files. With it, the files only need to exist,
   and `swiftvoice models list` shows the model as "installed (custom
   export)".
5. Check it: `swiftvoice --transcribe test.wav --model parakeet-tdt-0.6b-v3 --mode raw`.

SwiftVoice reads input and output names and shapes from the files. It
checks the joiner's output size against `tokens.txt` plus the duration
outputs, so a mismatched export fails with a clear error instead of
producing garbage. The export was not re-run while developing
SwiftVoice: Hugging Face was unreachable from the build environment. The
pre-built files were verified instead, with the two scripts above.
