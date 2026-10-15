#!/usr/bin/env python3
"""Developer-only: reference transcripts from sherpa-onnx (k2-fsa's C++ runtime)
for the same model files SwiftVoice loads, to compare with
tests/models_e2e.rs and `swiftvoice --transcribe`.

Usage:
  python sherpa_reference.py MODEL_DIR tdt|ctc FILE.wav [FILE.wav ...]

MODEL_DIR is an extracted sherpa-onnx Parakeet folder (encoder/decoder/
joiner .int8.onnx + tokens.txt for tdt, model.int8.onnx + tokens.txt for ctc).
"""
import os
import sys
import time

os.environ.setdefault("ORT_DISABLE_TELEMETRY", "1")

import sherpa_onnx  # noqa: E402
import soundfile as sf  # noqa: E402


def main() -> None:
    if len(sys.argv) < 4 or sys.argv[2] not in ("tdt", "ctc"):
        raise SystemExit(__doc__)
    d, kind, wavs = sys.argv[1], sys.argv[2], sys.argv[3:]
    if kind == "tdt":
        rec = sherpa_onnx.OfflineRecognizer.from_transducer(
            encoder=f"{d}/encoder.int8.onnx",
            decoder=f"{d}/decoder.int8.onnx",
            joiner=f"{d}/joiner.int8.onnx",
            tokens=f"{d}/tokens.txt",
            model_type="nemo_transducer",
            num_threads=4,
        )
    else:
        rec = sherpa_onnx.OfflineRecognizer.from_nemo_ctc(
            model=f"{d}/model.int8.onnx", tokens=f"{d}/tokens.txt", num_threads=4
        )
    for w in wavs:
        audio, sr = sf.read(w, dtype="float32")
        stream = rec.create_stream()
        stream.accept_waveform(sr, audio)
        t = time.time()
        rec.decode_stream(stream)
        ms = (time.time() - t) * 1000
        print(f"{os.path.basename(w)}\t{len(audio) / sr:.2f}s\t{ms:.0f}ms\t{stream.result.text}")


if __name__ == "__main__":
    main()
