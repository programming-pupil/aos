#!/usr/bin/env bash
# Fail fast when the local-embedding crate and packaged ONNX Runtime drift.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
RUNTIME_MANIFEST="$ROOT_DIR/rust/crates/runtime/Cargo.toml"
CARGO_LOCK="$ROOT_DIR/rust/Cargo.lock"

lock_version() {
  awk -F'"' -v package="$1" '
    /^\[\[package\]\]$/ { name = ""; next }
    /^name = / { name = $2; next }
    name == package && /^version = / { print $2; exit }
  ' "$CARGO_LOCK"
}

manifest_fastembed="$(sed -n 's/^fastembed = { version = "=\([^"]*\)".*/\1/p' "$RUNTIME_MANIFEST")"
locked_fastembed="$(lock_version fastembed)"
locked_ort="$(lock_version ort)"
locked_ort_sys="$(lock_version ort-sys)"
unix_runtime="$(sed -n 's/^ORT_VERSION="\([^"]*\)"/\1/p' "$ROOT_DIR/scripts/setup-onnxruntime.sh")"
windows_runtime="$(awk -F"'" '/^\$Version = / { print $2; exit }' "$ROOT_DIR/scripts/setup-onnxruntime.ps1")"

expected_fastembed="5.13.0"
expected_ort="2.0.0-rc.11"
expected_runtime="1.23.2"

[ "$manifest_fastembed" = "$expected_fastembed" ] || {
  echo "fastembed must remain at $expected_fastembed for ONNX Runtime API 23 and macOS Intel support; found ${manifest_fastembed:-missing}" >&2
  exit 1
}
[ "$locked_fastembed" = "$manifest_fastembed" ] || {
  echo "Cargo.lock fastembed $locked_fastembed does not match runtime manifest $manifest_fastembed" >&2
  exit 1
}
[ "$locked_ort" = "$expected_ort" ] && [ "$locked_ort_sys" = "$expected_ort" ] || {
  echo "ort and ort-sys must remain at $expected_ort; found ort=$locked_ort ort-sys=$locked_ort_sys" >&2
  exit 1
}
[ "$unix_runtime" = "$expected_runtime" ] && [ "$windows_runtime" = "$expected_runtime" ] || {
  echo "packaged ONNX Runtime must be $expected_runtime; found unix=$unix_runtime windows=$windows_runtime" >&2
  exit 1
}

echo "ONNX Runtime contract is compatible: fastembed $locked_fastembed, ort $locked_ort, runtime $unix_runtime."
