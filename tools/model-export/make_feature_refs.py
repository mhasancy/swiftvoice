#!/usr/bin/env python3
"""Developer-only: NeMo reference features for tests/feature_parity.rs.

Runs NeMo's own audio preprocessor, as exported to ONNX by the onnx-asr
package (nemo128.onnx for Parakeet TDT 0.6B, nemo80.onnx for the 110M
model), on a 16 kHz speech WAV and on a synthetic tone, and writes:

  <out>/speech.f32, <out>/synthetic.f32          input audio (float32 LE)
  <out>/<signal>_nemo{128,80}.f32                features [mels, frames]
  <out>/meta.json                                shapes and valid lengths

Then run:  SWIFTVOICE_FEATURE_REF=<out> cargo test --test feature_parity

Usage:  python make_feature_refs.py SPEECH_16K.wav OUT_DIR [--pre-dir DIR]
"""
import argparse
import json
import os
from pathlib import Path

# Before importing onnxruntime: Microsoft's non-Windows builds otherwise start
# a telemetry uploader (see swiftvoice/src/asr/runtime.rs).
os.environ.setdefault("ORT_DISABLE_TELEMETRY", "1")

import numpy as np  # noqa: E402
import onnxruntime as ort  # noqa: E402
import soundfile as sf  # noqa: E402


def default_pre_dir() -> Path:
    import onnx_asr  # noqa: F401  (only to locate its bundled preprocessors)

    return Path(onnx_asr.__file__).parent / "preprocessors" / "data"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("wav", help="16 kHz mono speech WAV")
    ap.add_argument("out", help="output folder")
    ap.add_argument("--pre-dir", help="folder containing nemo128.onnx and nemo80.onnx (default: from onnx-asr)")
    args = ap.parse_args()

    pre_dir = Path(args.pre_dir) if args.pre_dir else default_pre_dir()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    audio, sr = sf.read(args.wav, dtype="float32")
    if sr != 16000 or audio.ndim != 1:
        raise SystemExit(f"{args.wav}: need 16 kHz mono, got {sr} Hz with shape {audio.shape}")
    rng = np.random.default_rng(0)
    t = np.arange(24000) / 16000
    synthetic = (0.2 * np.sin(2 * np.pi * 440 * t) + 0.01 * rng.standard_normal(24000)).astype(np.float32)

    meta = {}
    for name, sig in [("speech", audio), ("synthetic", synthetic)]:
        sig.astype("<f4").tofile(out / f"{name}.f32")
        for mels in (128, 80):
            session = ort.InferenceSession(str(pre_dir / f"nemo{mels}.onnx"), providers=["CPUExecutionProvider"])
            feats, lens = session.run(
                None, {"waveforms": sig[None, :], "waveforms_lens": np.array([len(sig)], dtype=np.int64)}
            )
            feats[0].astype("<f4").tofile(out / f"{name}_nemo{mels}.f32")
            meta[f"{name}_nemo{mels}"] = {"shape": list(feats.shape[1:]), "len": int(lens[0]), "samples": len(sig)}
    (out / "meta.json").write_text(json.dumps(meta, indent=1))
    print(json.dumps(meta))


if __name__ == "__main__":
    main()
