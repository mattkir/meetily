#!/usr/bin/env bash
# Build dictation-helper and stage it like llama-helper for Tauri externalBin.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HELPER_DIR="$ROOT/dictation-helper"
BINARIES_DIR="$ROOT/frontend/src-tauri/binaries"
CONFIG="${1:-release}"

if [[ "$CONFIG" != "release" && "$CONFIG" != "debug" ]]; then
  echo "Usage: $0 [release|debug]" >&2
  exit 2
fi

TARGET_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
if [[ -z "$TARGET_TRIPLE" ]]; then
  echo "Could not detect host triple from rustc" >&2
  exit 1
fi

echo "Building dictation-helper ($CONFIG)…"
cd "$HELPER_DIR"
if [[ "$CONFIG" == "release" ]]; then
  swift build -c release
  SRC="$HELPER_DIR/.build/release/dictation-helper"
else
  swift build
  SRC="$HELPER_DIR/.build/debug/dictation-helper"
fi

if [[ ! -x "$SRC" ]]; then
  echo "Built binary missing at $SRC" >&2
  exit 1
fi

mkdir -p "$BINARIES_DIR"
DEST="$BINARIES_DIR/dictation-helper-$TARGET_TRIPLE"
# Remove prior staged copies (including unsuffixed symlink if present).
find "$BINARIES_DIR" -maxdepth 1 -name 'dictation-helper*' -delete
cp "$SRC" "$DEST"
chmod +x "$DEST"

echo "Staged: $DEST"
echo "Override at runtime with MEETILY_DICTATION_HELPER=$DEST"
