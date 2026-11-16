# dictation-helper sidecar protocol (Phase 2+)

Staged like `llama-helper` under `frontend/src-tauri/binaries/` as
`dictation-helper-<target-triple>`. Override with `MEETILY_DICTATION_HELPER`.

## Transport

Line-delimited JSON on stdin / stdout. One request or response per line.

## Commands (Rust → helper)

```json
{"cmd":"load","id":"1","engine":"apple"}
{"cmd":"warm","id":"2"}
{"cmd":"transcribe","id":"3","format":"f32le","sample_rate":16000,"pcm_b64":"..."}
{"cmd":"stream_start","id":"4"}
{"cmd":"stream_audio","id":"4","format":"f32le","sample_rate":16000,"pcm_b64":"..."}
{"cmd":"stream_finalize","id":"4","format":"f32le","sample_rate":16000,"pcm_b64":""}
{"cmd":"unload","id":"5"}
{"cmd":"ping","id":"6"}
```

Engines: `apple` (SpeechTranscriber, default), `parakeet-v3-coreml` (FluidAudio).

Audio: mono f32 little-endian @ 16 kHz, base64-encoded.

### Batch vs streaming

- **transcribe**: one-shot PCM → optional `partial` events → single `final`.
- **stream_***: hold-to-talk path. Same `id` for start / audio / finalize.
  - Apple: emit live `partial` events while `stream_audio` chunks arrive; `final` on `stream_finalize`.
  - Parakeet: buffer PCM across chunks; emit `final` only on `stream_finalize` (no live partials).
- After each `stream_audio`, helper emits zero or more `partial` events, then `acked`.
- `stream_finalize` may include trailing PCM in `pcm_b64` (grace tail); empty string is ok.

## Responses (helper → Rust)

```json
{"id":"1","ok":true,"event":"loaded","engine":"apple"}
{"id":"2","ok":true,"event":"ready"}
{"id":"3","ok":true,"event":"partial","text":"...","partial":true}
{"id":"3","ok":true,"event":"final","text":"...","partial":false}
{"id":"4","ok":true,"event":"stream_ready"}
{"id":"4","ok":true,"event":"acked"}
{"id":"5","ok":true,"event":"unloaded"}
{"id":"6","ok":true,"event":"pong"}
{"id":"...","ok":false,"error":"..."}
```

Never log full transcript text in Meetily. If the helper binary is missing or
unhealthy, Meetily refuses insert (fail-closed; no stub paste into real apps).
