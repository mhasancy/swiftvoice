# samples

Audio for `swiftvoice --benchmark`. WAV files are not committed (see
`.gitignore`); `scripts/benchmark.ps1` downloads the default sample.

| File | What | Source |
|---|---|---|
| `test.wav` | 11.0 s, 16 kHz mono: President Kennedy, inaugural address, 20 January 1961. A US government work, so public domain. | `https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav` (352,078 bytes, SHA-256 `59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e`) |
| `test.txt` | Reference transcript of `test.wav`, used for word error rate | committed |

Download it by hand:

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav -OutFile samples\test.wav
(Get-FileHash samples\test.wav -Algorithm SHA256).Hash   # must be 59DFB9A4...7860E
swiftvoice --benchmark --audio samples\test.wav --reference samples\test.txt
```

Any 8, 16, 24 or 32-bit PCM or 32-bit float WAV works, mono or stereo, at any
sample rate. SwiftVoice mixes it to mono and resamples to 16 kHz. To measure
your own voice, record a sample with the app's microphone path:

```powershell
swiftvoice --record samples\me.wav --seconds 15
```

Then write what you said into `samples\me.txt` and pass it with `--reference`.

The sherpa-onnx model archives also contain test clips: `test_wavs/` in
each Parakeet archive. The project also publishes `en.wav`, `de.wav`,
`fr.wav` and `es.wav` in its `asr-models` release. `tests/models_e2e.rs`
uses those clips.
