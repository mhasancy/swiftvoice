#!/usr/bin/env bash
# Golden-audio end-to-end transcription: downloads the pinned Parakeet model
# archives and reference clips, then runs tests/models_e2e.rs against them
# with skips turned into failures. This is what the `golden-audio` CI job
# runs; by hand it is the same on any Linux box.
#
#   scripts/golden-audio.sh
#   SWIFTVOICE_GOLDEN_DIR=/tmp/golden scripts/golden-audio.sh   # cache folder
#
# Model archives and clips come from the sherpa-onnx `asr-models` release
# (the same source the catalogue and samples/README.md pin), ONNX Runtime
# from Microsoft's release; every download is verified against its SHA-256.
# Needs curl, tar (with bzip2) and sha256sum. Delete the work folder to
# force a fresh download.
set -euo pipefail
cd "$(dirname "$0")/.."

work="${SWIFTVOICE_GOLDEN_DIR:-target/golden-audio}"
models="$work/models"
audio="$work/audio"
mkdir -p "$models" "$audio"

fetch() { # <url> <sha256> <destination>
    local url=$1 sum=$2 dest=$3
    if [ -f "$dest" ] && printf '%s  %s\n' "$sum" "$dest" | sha256sum -c - >/dev/null 2>&1; then
        return
    fi
    echo "downloading $(basename "$dest")"
    curl -fsSL --retry 3 --retry-delay 2 -o "$dest.part" "$url"
    printf '%s  %s\n' "$sum" "$dest.part" | sha256sum -c -
    mv "$dest.part" "$dest"
}

extract_model() { # <url> <archive sha256> <model id> <folder inside the archive>
    local url=$1 sum=$2 id=$3 inner=$4
    if [ -d "$models/$id" ]; then
        return
    fi
    local archive="$work/$id.tar.bz2"
    fetch "$url" "$sum" "$archive"
    echo "extracting $id"
    rm -rf "$work/unpack"
    mkdir -p "$work/unpack"
    tar -xjf "$archive" -C "$work/unpack"
    mv "$work/unpack/$inner" "$models/$id"
    rm -rf "$work/unpack" "$archive"
}

release=https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models

# The same pinned archives the app downloads (models/README.md).
extract_model \
    "$release/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2" \
    5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf \
    parakeet-tdt-0.6b-v3 sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8
extract_model \
    "$release/sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8.tar.bz2" \
    17f945007b52ccd8b7200ffc7c5652e9e8e961dfdf479cefcabd06cf5703630b \
    parakeet-tdt-ctc-110m-en sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8

# Reference clips: en/de/fr/es are release assets; 0.wav and 1.wav are the
# 110M archive's test_wavs (see samples/README.md).
fetch "$release/en.wav" 148b936b43ce7c546a866e64da059f0458aee2d65e617f16e9d94f06e8d99ed6 "$audio/en.wav"
fetch "$release/de.wav" 36d3c4845b9808a1656a2a2e92d884590e2db94389e6fe559643291ae0cd3710 "$audio/de.wav"
fetch "$release/fr.wav" b59be4349b92d344fb903677165eaf4694025d1ab119c608726ecbcb3164b528 "$audio/fr.wav"
fetch "$release/es.wav" 49fd2cfa4b62db7068143c582b35de9d31ec2733495ece3611105131d21de06c "$audio/es.wav"
cp -f "$models/parakeet-tdt-ctc-110m-en/test_wavs/0.wav" "$audio/0.wav"
cp -f "$models/parakeet-tdt-ctc-110m-en/test_wavs/1.wav" "$audio/1.wav"

# ONNX Runtime, Microsoft's official Linux x64 build; the Windows build
# script pins its zip the same way.
ort="$work/onnxruntime-linux-x64-1.31.0"
if [ ! -d "$ort" ]; then
    archive="$work/onnxruntime-linux-x64-1.31.0.tgz"
    fetch \
        "https://github.com/microsoft/onnxruntime/releases/download/v1.31.0/onnxruntime-linux-x64-1.31.0.tgz" \
        cc5c72baf5ae5c8238a6841f0897227be2d02826b9cf98eaf02fdefaeeb03a57 "$archive"
    tar -xzf "$archive" -C "$work"
    rm -f "$archive"
fi

export SWIFTVOICE_TEST_MODELS="$PWD/$models"
export SWIFTVOICE_TEST_AUDIO="$PWD/$audio"
export SWIFTVOICE_TEST_STRICT=1
export SWIFTVOICE_ORT_DYLIB="$PWD/$ort/lib/libonnxruntime.so.1.31.0"
export LD_LIBRARY_PATH="$PWD/$ort/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

echo "transcribing the golden audio"
cargo test --locked --release --no-default-features --test models_e2e -- --nocapture
