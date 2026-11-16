#!/usr/bin/env bash
# Build and stage llama-helper for Meetily (macOS Metal by default).
#
# On macOS 27+ SDKs, llama-cpp-sys-2 0.1.146's vendored cpp-httplib fails to
# compile against OpenSSL headers that return const X509_NAME*. This script
# applies that one-line fix idempotently before building.
#
# Usage (from repo root or anywhere):
#   ./llama-helper/scripts/build-llama-helper.sh           # debug + metal on macOS
#   ./llama-helper/scripts/build-llama-helper.sh release   # release + metal on macOS
#   LLAMA_HELPER_FEATURES= ./llama-helper/scripts/build-llama-helper.sh  # CPU-only

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

PROFILE="${1:-debug}"
case "$PROFILE" in
  debug|release) ;;
  *)
    echo "Usage: $0 [debug|release]" >&2
    exit 2
    ;;
esac

# Do not inherit agent/sandbox target dirs that collide with workspace layout.
unset CARGO_TARGET_DIR

HOST="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -z "$HOST" ]]; then
  echo "error: could not determine rustc host triple" >&2
  exit 1
fi

# Cargo's package build dir is target/<profile>/build/llama-helper (a directory).
# tauri-build also copies the sidecar to that exact path. A leftover file there
# makes Cargo fail with ENOTDIR. Remove only a file, never the directory.
for stub in \
  "$REPO_ROOT/target/debug/build/llama-helper" \
  "$REPO_ROOT/target/release/build/llama-helper"
do
  if [[ -f "$stub" ]]; then
    echo "Removing file that blocks Cargo build dir: $stub"
    rm -f "$stub"
  fi
done

# --- Patch vendored httplib for macOS 27+ OpenSSL const correctness ---
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
PATCH_COUNT=0
while IFS= read -r -d '' httplib; do
  if grep -q 'X509_NAME \*name = X509_get_subject_name(cert);' "$httplib"; then
    echo "Patching OpenSSL const mismatch in: $httplib"
    perl -i -pe 's/^(\s*)X509_NAME \*name = X509_get_subject_name\(cert\);/${1}const X509_NAME *name = X509_get_subject_name(cert);/' "$httplib"
    PATCH_COUNT=$((PATCH_COUNT + 1))
  fi
done < <(find "$CARGO_HOME_DIR/registry/src" -path '*llama-cpp-sys-2-0.1.146*/httplib.cpp' -print0 2>/dev/null || true)

if [[ "$PATCH_COUNT" -eq 0 ]]; then
  echo "httplib patch: already applied or crate sources not downloaded yet (ok)"
fi

# --- Features ---
if [[ -n "${LLAMA_HELPER_FEATURES+x}" ]]; then
  FEATURES=("$LLAMA_HELPER_FEATURES")
elif [[ "$(uname -s)" == "Darwin" ]]; then
  FEATURES=("metal")
else
  FEATURES=()
fi

BUILD_ARGS=(-p meetily-llama-helper)
if [[ "$PROFILE" == "release" ]]; then
  BUILD_ARGS+=(--release)
fi
if [[ ${#FEATURES[@]} -gt 0 && -n "${FEATURES[0]}" ]]; then
  BUILD_ARGS+=(--features "${FEATURES[0]}")
  echo "Building llama-helper ($PROFILE) with features: ${FEATURES[0]}"
else
  echo "Building llama-helper ($PROFILE) with default features"
fi

# First attempt (may download crate then need patch + retry)
set +e
cargo build "${BUILD_ARGS[@]}"
STATUS=$?
set -e

if [[ $STATUS -ne 0 ]]; then
  # Crate may have been freshly extracted; re-apply patch and retry once.
  while IFS= read -r -d '' httplib; do
    if grep -q 'X509_NAME \*name = X509_get_subject_name(cert);' "$httplib"; then
      echo "Re-patching after failed build: $httplib"
      perl -i -pe 's/^(\s*)X509_NAME \*name = X509_get_subject_name\(cert\);/${1}const X509_NAME *name = X509_get_subject_name(cert);/' "$httplib"
    fi
  done < <(find "$CARGO_HOME_DIR/registry/src" -path '*llama-cpp-sys-2-0.1.146*/httplib.cpp' -print0 2>/dev/null || true)

  # Touch build.rs to force rebuild of llama-cpp-sys-2 after patch
  find "$CARGO_HOME_DIR/registry/src" -path '*llama-cpp-sys-2-0.1.146/build.rs' -exec touch {} \; 2>/dev/null || true

  cargo build "${BUILD_ARGS[@]}"
fi

# [[bin]] name stays llama-helper; only the package name differs.
if [[ "$PROFILE" == "release" ]]; then
  BUILT="$REPO_ROOT/target/release/llama-helper"
else
  BUILT="$REPO_ROOT/target/debug/llama-helper"
fi

if [[ ! -x "$BUILT" ]]; then
  echo "error: expected binary missing: $BUILT" >&2
  exit 1
fi

SIZE="$(wc -c < "$BUILT" | tr -d ' ')"
if [[ "$SIZE" -lt 1000000 ]]; then
  echo "error: staged candidate looks like a stub ($SIZE bytes): $BUILT" >&2
  exit 1
fi

DEST_DIR="$REPO_ROOT/frontend/src-tauri/binaries"
mkdir -p "$DEST_DIR"
DEST="$DEST_DIR/llama-helper-$HOST"
cp -f "$BUILT" "$DEST"
chmod +x "$DEST"

echo "Staged: $DEST ($(wc -c < "$DEST" | tr -d ' ') bytes)"
file "$DEST"
