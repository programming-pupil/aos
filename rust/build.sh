#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

if [[ "${AOS_DEV_USE_SCCACHE:-0}" != "1" ]]; then
  export RUSTC_WRAPPER=""
fi

echo "Rust: $(rustc --version)"
echo "Cargo: $(cargo --version)"

cargo fmt --check
if [[ -n "${AOS_WEB_SERVER_FEATURES:-}" ]]; then
  cargo build -p web-server --release --features "$AOS_WEB_SERVER_FEATURES"
else
  cargo build -p web-server --release
fi
