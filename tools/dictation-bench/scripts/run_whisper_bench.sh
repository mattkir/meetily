#!/usr/bin/env bash
# Baseline Whisper large-v3-turbo via brew whisper-cli (Metal), proxy for Meetily's whisper-rs Metal path.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODEL="${WHISPER_MODEL:-$ROOT/models/whisper/ggml-large-v3-turbo.bin}"
RUNS="${RUNS:-5}"
OUT="${OUT:-$ROOT/logs/whisper-bench.tsv}"

if [[ ! -f "$MODEL" ]]; then
  echo "ERROR: missing model at $MODEL" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUT")"
: >"$OUT"

echo "Using model: $MODEL" >&2
echo "Whisper CLI: $(command -v whisper-cli)" >&2

# Discarded warm-up
whisper-cli -m "$MODEL" -l en -np -nt "$ROOT/audio/short.wav" >/dev/null 2>"$ROOT/logs/whisper-warmup.err" || true

declare -a MS=()
for wav in short disfluent technical ten_sec; do
  path="$ROOT/audio/${wav}.wav"
  for ((i = 1; i <= RUNS; i++)); do
    start=$(python3 -c 'import time; print(time.perf_counter())')
    # Capture text; whisper-cli prints timing to stderr
    text=$(whisper-cli -m "$MODEL" -l en -np -nt "$path" 2>"$ROOT/logs/whisper-${wav}-${i}.err" | tr '\n' ' ' | sed 's/[[:space:]]\+/ /g' | sed 's/^ //;s/ $//')
    end=$(python3 -c 'import time; print(time.perf_counter())')
    ms=$(python3 -c "print(f'{($end - $start) * 1000:.1f}')")
    MS+=("$ms")
    # Prefer whisper's internal encode/decode timing if present
    internal=$(rg -o 'total time[[:space:]]*=[[:space:]]*[0-9.]+[[:space:]]*ms' "$ROOT/logs/whisper-${wav}-${i}.err" | tail -1 || true)
    printf 'WHISPER_METAL\t%s.wav\trun=%d\twarm=yes\t%sms\t%s\t%s\n' "$wav" "$i" "$ms" "$text" "$internal" | tee -a "$OUT"
  done
done

python3 - <<'PY' "$OUT"
import sys, statistics
path = sys.argv[1]
vals = []
with open(path) as f:
    for line in f:
        if not line.startswith("WHISPER_METAL"):
            continue
        parts = line.strip().split("\t")
        for p in parts:
            if p.endswith("ms") and p[:-2].replace(".", "", 1).isdigit():
                # wall-clock field is 4th (index 3) like "123.4ms"
                pass
        # field index 3 is wall ms with suffix
        ms = float(parts[3].removesuffix("ms"))
        vals.append(ms)
vals.sort()
if not vals:
    print("WHISPER_METAL_SUMMARY\twarm_n=0\tp50_ms=NA")
else:
    p50 = vals[len(vals)//2]
    print(f"WHISPER_METAL_SUMMARY\twarm_n={len(vals)}\tp50_ms={p50:.1f}\tmin_ms={vals[0]:.1f}\tmax_ms={vals[-1]:.1f}")
PY
