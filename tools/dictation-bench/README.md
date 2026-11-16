# Dictation bench (Phase 0)

Scratch harness for short-utterance ASR latency on this Mac. Not product code.

## Suite

Synthetic `say` → 16 kHz mono WAV under `audio/`:

| File | Intent | Duration (approx) |
| --- | --- | --- |
| `short.wav` | short English | ~2.5 s |
| `disfluent.wav` | fillers | ~5.6 s |
| `technical.wav` | jargon | ~6.1 s |
| `ten_sec.wav` | longer dictation | ~12.1 s |

References: `audio/references.json`.

## Probes

| Probe | Path |
| --- | --- |
| Apple `SpeechTranscriber` | `apple-speech-probe/` |
| FluidAudio Parakeet TDT v3 Core ML | `fluidaudio-probe/` |
| Meetily-compatible Parakeet CPU ONNX | `meetily-parakeet-bench/` (vendors Meetily `model.rs`) |
| Whisper large-v3-turbo Metal | `scripts/run_whisper_bench.sh` via Homebrew `whisper-cli` |

Results: [`RESULTS.md`](RESULTS.md) (Phase 0 ASR probes). Personal product E2E checklist: [`E2E.md`](E2E.md). Large models live under `models/` (gitignored).
