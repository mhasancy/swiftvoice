# Benchmarks

This page lists the targets, how to measure them, and what was measured
for this release. Every number below comes from `swiftvoice --benchmark` or
the test suite on the machine described. Nothing is extrapolated. Rows
marked *not measured* need an interactive Windows desktop. Measure them with
`tests/MANUAL_TESTS.md` section F and add them here.

## Targets

| Target | Goal | Status for this release |
|---|---|---|
| Cold start (launch to tray icon, model loading in the background) | < 1 s | not measured (Windows UI). The model itself is ready 1.5 s (110M) to 3.4 s (0.6B) after process start on the reference machine, with a warm disk cache. |
| Hotkey to recording | < 50 ms | not measured (Windows UI). The microphone stays open `keep_warm_secs` (15 s) after a dictation, and 300 ms of pre-roll is kept, so audio from the key press onward is never lost. |
| End of speech to text (push-to-talk) | < 300 ms | **met** for 0.6B v3 on the JFK clips (221-244 ms) and for 110M on every clip (24-125 ms). **Not met** for 0.6B on long-phoebe (501 ms; see below). |
| End of speech to text (toggle mode) | < 300 ms after the endpoint | met: 0-26 ms after the VAD endpoint. The endpoint itself waits `vad.endpoint_ms` (800 ms) of silence. |
| Idle RAM | < 150 MB | **met only with `asr.unload_after_idle_secs`**: 57-74 MB after the model is unloaded. With a model resident: about 250 MB (110M) or 850-1,070 MB (0.6B). |
| Installed size | < 30 MB | expected about 21 MB (17.3 MB `onnxruntime.dll` + VC++ runtime + exe). `scripts/build-windows.ps1` prints the exact size and warns above 30 MB. Models are separate downloads (100-465 MB). |
| Idle CPU | ~0 % | not measured. By design, nothing runs while idle: no polling except a 1 s settings-file check, and the push-to-talk key poll runs only while the key is held. |
| No dropouts over 10 minutes | 0 | not measured live (manual test D8). The audio thread drains a 2 s ring buffer every ~10 ms and logs any overrun. |

## Reference machine

- Intel Xeon @ 2.80 GHz cloud VM, 4 vCPUs (AVX-512 with VNNI), 16 GB RAM,
  Linux x86-64.
- ONNX Runtime 1.31.0, Microsoft's official CPU build.
- SwiftVoice 0.1.0 release build, `--no-default-features` (Whisper not
  compiled in).
- Default settings: `asr.low_memory = true`, `commit_after_secs = 6`,
  `pause_ms = 200`, `endpoint_ms = 800`, `release_tail_ms = 150`, Silero
  VAD v6.2. The simulated key release is 300 ms after the last word.

A cloud VM is a modest target. A current 8-core laptop or desktop CPU
should be faster, but that was not measured. Run `scripts/benchmark.ps1` on
yours.

## Results

Median of 5 runs. "Push-to-talk" is release-to-text: the time from
releasing the key to the final text, including post-processing, as
SwiftVoice's pipeline would deliver it (see *Method*). "Inserted WER" is the
word error rate of that text, after windows were committed and joined.
"Raw WER" is that of one transcription of the whole file.

| Model | Clip (length) | Load + warm-up | Whole-file transcription | RTF | Raw WER | Push-to-talk | Inserted WER |
|---|---|---|---|---|---|---|---|
| Parakeet TDT 0.6B v3 | jfk (11.0 s) | 2758 + 163 ms | 794 ms | 0.072 | 0.0 % | **221 ms** | 0.0 % |
| | long-jfk (34.2 s) | 2802 + 217 ms | 2521 ms | 0.074 | 0.0 % | **244 ms** | 0.0 % |
| | long-phoebe (31.5 s) | 2990 + 340 ms | 2412 ms | 0.076 | 1.1 % | 501 ms | 0.0 % |
| Parakeet TDT 0.6B v2 | jfk | 3185 + 181 ms | 766 ms | 0.070 | 0.0 % | 226 ms | 0.0 % |
| | long-jfk | 2780 + 199 ms | 2547 ms | 0.074 | 0.0 % | 216 ms | 0.0 % |
| | long-phoebe | 3030 + 182 ms | 2312 ms | 0.073 | 4.3 % | 515 ms | 4.3 % |
| Parakeet TDT-CTC 110M | jfk | 1448 + 53 ms | 261 ms | 0.024 | 0.0 % | **24 ms** | 0.0 % |
| | long-jfk | 1691 + 55 ms | 980 ms | 0.029 | 3.0 % | **27 ms** | 0.0 % |
| | long-phoebe | 1535 + 56 ms | 993 ms | 0.031 | 0.0 % | **125 ms** | 0.0 % |

Toggle mode: on every clip and model, the text was ready 0-26 ms after the
VAD endpoint, which is 800-864 ms after the last word. That interval is
the endpoint wait.

Post-processing (Clean mode) took 0.04-0.14 ms per utterance.

**Why long-phoebe is slower.** Its last sentence, "It is certainly very like
the old portrait.", is a 3.9 s phrase after a short pause. When the key is
released, the recogniser still has to process that phrase plus its context
(the previous phrase). With an offline (non-streaming) model, the wait after
release is roughly the cost of the last phrase. A dictation that ends in a
long unbroken phrase waits longer. The 110M model handles the same case in
125 ms.

### Memory

Resident set (RSS) of the benchmark process, in MB:

| Model | `low_memory` | Model loaded | Peak (incl. a 31-34 s single transcription) | After unloading |
|---|---|---|---|---|
| 0.6B v3 | on (default) | 849-1070 | 919-1330 | 58-74 |
| 0.6B v3 | off | 1098-1334 | 1179-1584 | 58-74 |
| 0.6B v2 | on | 829-1059 | 911-1315 | 58-74 |
| 110M | on | 250-253 | 328-488 | 57-68 |

`low_memory` skips ONNX Runtime's weight pre-packing for model files over
64 MB. Alternating runs measured 230-270 MB less RAM. The cost was 10-15 %
slower whole-file transcription (781-861 ms vs 698-762 ms on jfk) and about
+50 ms push-to-talk latency (232-244 ms vs 174-194 ms). The peak column
includes the benchmark's whole-file transcription of a 34 s clip. In the
app, a window is committed at the next pause after 6 s and is cut at 16 s
at the latest, so dictation peaks lower.

"After unloading" is what `asr.unload_after_idle_secs` gives you: the model
is dropped, and freed memory is returned to the OS (`malloc_trim` on Linux,
`HeapCompact` on Windows). Without that call, RSS stayed at 827 MB after the
drop.

### Accuracy cross-checks

- **Front end.** Rust NeMo log-mel features against NeMo's own
  preprocessor (onnx-asr's `nemo128.onnx` / `nemo80.onnx`) on a 7.43 s
  speech clip: max |diff| 2.32e-4 (128 mels) and 1.72e-4 (80 mels); mean
  |diff| 8.1e-7 and 6.0e-7 (`tests/feature_parity.rs`).
- **Transcripts.** Identical to sherpa-onnx (k2-fsa's C++ runtime) on the
  same files for en, de, fr and the 110M clips, with one exception. For es,
  SwiftVoice writes the correct "qué" where sherpa-onnx writes "que"
  (`tests/models_e2e.rs`).
- **Long dictation.** With windows committed at pauses and pause-anchored
  context (see ARCHITECTURE.md), the text that push-to-talk inserts had
  0.0 % WER on long-jfk and long-phoebe (v3).

### Test clips

| Clip | Content | How it was made |
|---|---|---|
| jfk | `samples/test.wav` (whisper.cpp's `jfk.wav`, public domain); reference in `samples/test.txt` | as published |
| long-jfk | jfk three times, with 0.6 s gaps | concatenated, plus light Gaussian noise (σ = 8 LSB) so gaps are not digital silence |
| long-phoebe | the `0.wav` LibriSpeech-style clip from the sherpa-onnx 110M archive, four times | same |

A two-speaker concatenation (JFK followed by a different speaker) was
excluded from the headline numbers. Parakeet v3 drops the second speaker's
first phrase in some windows. sherpa-onnx's reference runtime does the same
on the same audio, so it is model behaviour rather than a SwiftVoice bug.
Dictation is single-speaker.

## Method

`swiftvoice --benchmark --audio FILE.wav [--reference REF.txt]`:

1. Loads ONNX Runtime and the model and runs a 1 s warm-up, timing each.
2. Transcribes the whole file `--runs` times. It reports the median, the
   RTF (median / audio duration), and the WER against the reference.
   Words are lowercased and stripped of punctuation before alignment.
3. Replays the file through the real VAD and segmenter, frame by frame.
   The segmenter code is the same one the app uses. Silence (`endpoint_ms`
   + 1 s) is appended so the endpoint and the key release happen after
   the speech.
4. Simulates the single recogniser thread over the resulting jobs, with
   their real emission times. It uses each job's real measured inference
   time and the worker's rules for dropping superseded jobs. It reports
   when the final text would be ready, in toggle mode (per utterance,
   after the endpoint) and in push-to-talk (one hold, released
   `--release-delay-ms` after the last word). This runs `--runs` times
   with fresh transcriptions; the median is shown.
5. Reports RSS before and after loading, the peak, and after unloading.

The simulation leaves out these delays:
- **Capture buffering:** WASAPI shared mode, typically 10-20 ms.
- **Message passing:** thread hand-off, a few µs.
- **Insertion:** the paste keystroke, plus the target application's own
  reaction time.

## Running it yourself

```powershell
scripts\build-windows.ps1
scripts\benchmark.ps1 -Download                                   # default model, samples\test.wav
scripts\benchmark.ps1 -Model parakeet-tdt-ctc-110m-en -Download
scripts\benchmark.ps1 -Extra @('--low-memory','off')
swiftvoice --benchmark --audio my.wav --reference my.txt --runs 10 --json out.json
```

The JSON contains every field of the report, including per-utterance
latencies and job counts.
