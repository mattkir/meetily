# Phase 0 ASR baseline results

**Machine:** MacBook Pro (M4 Max, 128 GB), macOS 27.0.1 (Build 26A434), Xcode 27.2  
**Date:** 2026-11-15  
**Suite:** synthetic `say` → 16 kHz mono WAV under `audio/` (not private recordings)  
**Metric:** warm wall-clock transcription latency after discarded warm-up (unless noted). p50 = median of warm runs.

| Clip | Duration | Reference intent |
| --- | --- | --- |
| `short.wav` | 2.50 s | Short English command |
| `disfluent.wav` | 5.60 s | Fillers / disfluency |
| `technical.wav` | 6.06 s | Technical jargon |
| `ten_sec.wav` | 12.13 s | Longer multi-sentence (~10 s class) |

References: [`audio/references.json`](audio/references.json). Raw logs: [`logs/`](logs/).

---

## Summary table (warm p50)

| Engine | How invoked | Short p50 | Suite-wide warm p50 | Notes |
| --- | --- | --- | --- | --- |
| **FluidAudio Parakeet TDT v3 Core ML** | Swift SPM probe, local HF cache | **41.2 ms** | **54.9 ms** | Fastest; best technical accuracy on suite |
| Meetily Parakeet CPU ONNX (`parakeet-tdt-0.6b-v3-int8`) | Vendored Meetily `model.rs` + ort CPU | **55.1 ms** | **112.2 ms** | Same ONNX artifacts Meetily downloads; not via Tauri UI |
| Apple `SpeechTranscriber` | Swift probe, Speech framework | **65.3 ms** | **137.2 ms** | Strong short latency; weaker technical WER |
| Whisper `large-v3-turbo` Metal | Homebrew `whisper-cli` (Metal) | ~**322 ms** e2e* | ~**322 ms** e2e* | *After single process load; encode avg 229 ms |

\*Whisper: one process, 20 files; `load time = 1363.93 ms` once; `encode time` avg **229.47 ms**/file; warm e2e ≈ `(7801.06 − 1363.93) / 20 = 321.9 ms`. Per-process invocations (~1 s) include reload and are **not** warm.

---

## 1. FluidAudio Parakeet TDT v3 Core ML

- **Probe:** `fluidaudio-probe/` (SPM `FluidAudio` ≥ 0.7.9)
- **Models:** `~/Library/Application Support/FluidAudio/Models/parakeet-tdt-0.6b-v3-coreml` (already present; no download this run)
- **Warm-up:** one discarded pass on `short.wav`, then 5 timed runs/file
- **Log:** `logs/fluidaudio.out`

| File | warm n | p50 (ms) | min | max |
| --- | ---: | ---: | ---: | ---: |
| short | 5 | 41.2 | 40.0 | 41.5 |
| disfluent | 5 | 47.6 | 46.4 | 49.4 |
| technical | 5 | 55.6 | 54.9 | 58.7 |
| ten_sec | 5 | 86.1 | 82.8 | 90.1 |
| **all** | **20** | **54.9** | **40.0** | **90.1** |

Sample transcripts matched references closely (including “Kubernetes”, “P50 latency”).

---

## 2. Meetily Parakeet CPU ONNX

- **Probe:** `meetily-parakeet-bench/` — copies Meetily `frontend/src-tauri/src/parakeet_engine/model.rs`, ort `=2.0.0-rc.10`, CPU EP, int8
- **Models:** HF `istupakov/parakeet-tdt-0.6b-v3-onnx` @ `8f23f0c…` → `models/parakeet-tdt-0.6b-v3-int8/` (byte-exact vs Meetily `ArtifactSpec`)
- **Model load (cold):** 687.1 ms (not included in warm timings)
- **Blocked for in-app path:** Meetily Tauri app / `com.meetily.ai` app data was **not** present; no UI launch. This is the same ONNX stack Meetily uses, exercised out-of-process.
- **Log:** `logs/parakeet-cpu.out`

| File | warm n | p50 (ms) | min | max |
| --- | ---: | ---: | ---: | ---: |
| short | 5 | 55.1 | 51.2 | 56.7 |
| disfluent | 5 | 106.6 | 102.3 | 107.2 |
| technical | 5 | 117.4 | 112.2 | 137.4 |
| ten_sec | 5 | 207.9 | 204.5 | 231.1 |
| **all** | **20** | **112.2** | **51.2** | **231.1** |

---

## 3. Apple `SpeechTranscriber`

- **Probe:** `apple-speech-probe/` (`SpeechAnalyzer` + `SpeechTranscriber`, macOS 26+ / 27 SDK)
- **Assets:** system `AssetInventory` (install path exercised; assets already available enough to run)
- **Warm definition:** runs after first timed pass per file marked `warm=no` are excluded from p50; suite-wide p50 uses the 16 warm rows
- **Log:** `logs/apple-speech.out`

| File | warm n | p50 (ms) | min | max |
| --- | ---: | ---: | ---: | ---: |
| short | 4 | 65.3 | 65.1 | 66.8 |
| disfluent | 4 | 104.8 | 101.8 | 105.7 |
| technical | 4 | 138.3 | 137.2 | 140.7 |
| ten_sec | 4 | 194.9 | 192.2 | 200.6 |
| **all warm** | **16** | **137.2** | **65.1** | **200.6** |

Quality notes (synthetic voice, not user voice):

- Short / ten_sec: strong
- Disfluent: dropped leading “Um,”; “uh” → “ah”
- Technical: **“Kubernetes” → “Cubernates”**, **“latency” → “agency”**

---

## 4. Whisper large-v3-turbo Metal

- **Proxy for Meetily:** Homebrew `whisper-cli` with Metal backend on Apple M4 Max (Meetily uses `whisper-rs` Metal+CoreML in-process; **Tauri app not launched**)
- **Model:** `models/whisper/ggml-large-v3-turbo.bin` (HF `ggerganov/whisper.cpp`)
- **Best available warm numbers:** single-process multi-file run (`logs/whisper-multifile.err`)

| Metric | Value |
| --- | --- |
| Load (once) | 1363.93 ms |
| Encode avg (20 runs) | 229.47 ms |
| Warm e2e estimate / file | **321.9 ms** |
| Sample short transcript | Correct |

Separate per-process wall clocks (~985–1135 ms) include process + model load each time — reported in `logs/whisper-bench.tsv` but **not** used as warm p50.

`system_info` from CLI: `MTL` present; `COREML = 0` on this brew build (Meetily may enable CoreML separately).

---

## Recommendation (evidence-based)

**Keep the plan’s product default: Apple `SpeechTranscriber`, with FluidAudio Parakeet v3 Core ML as the high-speed opt-in.**

Reasons from this suite:

1. **Latency budget:** Short warm Apple **65 ms** and Fluid **41 ms** both sit well under the &lt;150 ms ASR / &lt;300 ms E2E dictation budgets. Whisper ~**322 ms** warm e2e is too slow as a dictation primary.
2. **Speed winner:** FluidAudio is clearly fastest and scales best to ~12 s audio (**86 ms** vs Apple **195 ms** vs Parakeet CPU **208 ms**).
3. **Accuracy on jargon:** FluidAudio and Meetily Parakeet ONNX (and Whisper) handled “Kubernetes” / “P50 latency”; Apple misheard both on this TTS clip. That supports Parakeet as the escape hatch for technical dictation — not a reason to abandon Apple for everyday English.
4. **Integration:** Apple needs no multi‑GB HF ASR download and offers streaming/volatile partials for HUD; FluidAudio models were already cached here (~461 MB Core ML bundle).
5. **Meetily meeting path:** Existing Parakeet CPU ONNX (~55 ms short) is already competitive for short clips; Whisper turbo remains the multilingual/batch engine, not the dictation hot path.

**Caveats:** Suite is macOS `say` TTS, not the user’s voice. Re-check Apple vs FluidAudio WER on private clips before locking UI defaults. Meetily baselines used out-of-app probes, not the Tauri process.

---

## What was not done (Phase 0 limits)

- No Phase 1–4 dictation product features (Phases 1–3 shipped later in-app; Phase 4 aligned meetings/Settings without Ultra)
- No Meetily desktop UI / end-to-end Tauri invoke of `parakeet_transcribe_audio` / `whisper_transcribe_audio`
- No private voice WER
- No commit / push / plan-file edits

## Phase 4 follow-up (meetings + Ultra)

**Meeting live STT:** Keep in-process Parakeet ONNX int8 + Whisper turbo. Do **not** reuse `dictation-helper` Parakeet ANE for meetings — dual-output recording/VAD, mic+system capture, and shutdown drain/flush must stay on the existing pipeline; the sidecar is short-utterance / mic-only / conflicts with meeting recording.

**Parakeet Ultra (`moondream/parakeet-ultra`):** Deferred as a future **opt-in** only. Evaluate on private clips against Apple vs FluidAudio Parakeet v3 Core ML before any product integration. Not added to meeting download catalogs or dictation engines in Phase 4.
