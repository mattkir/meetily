# Dictation personal E2E verification

**Machine:** MacBook Pro, Apple M4 Max, 128 GB, macOS 27.0.1 (Build 26A434)  
**Date:** 2026-11-15 (updated with product self-test CLI)  
**Privacy:** synthetic PCM / bench WAVs only; no private transcript text recorded here.

Gate for daily-ready (product): warm short **`e2e_ms` < 300** from product self-test (`--dictation-self-test` or Settings Self-test).

---

## Checklist summary

| # | Check | Result | Notes |
| --- | --- | --- | --- |
| 1 | Launch at login → tray (main hidden) | **manual** | LaunchAgent code present; not toggled this run. |
| 2 | Slack / Mail / Cursor / Notes paste (Fn hold) | **manual** | GUI paste still requires human confirmation. |
| 3 | Warm short product `e2e_ms` < 300 (Self-test) | **pass** | CLI `--dictation-self-test`: **e2e_ms=140**, `dailyReady=true` (Apple). |
| 4 | Helper killed → no stub paste | **pass** (protocol) | After `SIGKILL`, next stdin write → **BrokenPipe**; returncode -9. Product refuse-insert path exists; caret field not GUI-checked. |
| 5 | Meeting recording blocks dictation | **manual** | Code path `BlockedMeeting`; not started a meeting this run. |
| 6 | Switch Apple ↔ Parakeet; re-warm | **pass** (helper) | Both engines load/warm/stream via staged helper. In-app Settings switch not GUI-clicked. |
| 7 | HUD never steals key focus | **partial** | Built non-activating; live app focus not GUI-proven. |
| 8 | Hands-free silence / max-duration | **manual** | Implemented; not timed under speech this run. |

**Overall:** product latency gate is **green** on this Mac via headless CLI. Cross-app paste / login reboot / meeting block remain manual.

---

## Measured metrics (honest)

### Product Self-test (`--dictation-self-test`)

Raw: [`logs/product-self-test.json`](logs/product-self-test.json)

| Metric | Value |
| --- | --- |
| `engine` | `apple` |
| `graceMs` | 120 |
| `resampleMs` | 0 |
| `ipcMs` | 0 |
| `asrMs` | 13 |
| `polishMs` | 0 |
| `insertMs` | 0 (insert skipped in CLI) |
| **`e2eMs`** | **140** |
| `dailyReady` | **true** (< 300) |
| `helperHealthy` | true |
| process exit | 0 |

Command:

```bash
MEETILY_SELF_TEST_OUT=tools/dictation-bench/logs/product-self-test.json \
  frontend/src-tauri/target/debug/meetily --hidden --dictation-self-test
```

### Helper protocol smoke (staged binary)

Raw: [`logs/helper-protocol-smoke.json`](logs/helper-protocol-smoke.json)

| Engine | load_ms | warm_ms | stream_ms | partials | final | ok |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| apple | 69.7 | 62.1 | 53.8 | 4 | 1 | true |
| parakeet-v3-coreml | 188.1 | 72.9 | 46.9 | 0 | 1 | true |

Apple emitted live `partial` events while streaming `short.wav`. Parakeet finalized on release only.

### Fail-closed (helper kill)

| Check | Result |
| --- | --- |
| write after SIGKILL | `BrokenPipe` |
| returncode | -9 |
| transcript emitted | no |

---

## Unit tests

```text
dictation::polish::tests — 3 passed
audio::post_processor::tests — 2 passed
```

---

## Remaining manual steps (user)

1. Enable **Launch at login**, confirm tray/`--hidden` after logout/login (re-toggle after rebuilds — plist embeds `current_exe()`).
2. Fn-hold paste into **Notes, Cursor, Mail, Slack** with main window hidden; confirm caret target and HUD does not steal focus.
3. Start a meeting recording; confirm dictation blocked; stop; confirm dictation works again.
4. Switch Apple ↔ Parakeet in Settings; confirm re-warm and one short utterance each.
5. Hands-free (`ctrl+shift+space` default): silence end + optional max-duration cut-off.
