#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

if [[ "${AOS_DEV_USE_SCCACHE:-0}" != "1" ]]; then
  export RUSTC_WRAPPER=""
fi

if [[ -n "${AOS_WEB_SERVER_FEATURES:-}" ]]; then
  cargo check -p web-server -q --features "$AOS_WEB_SERVER_FEATURES"
else
  cargo check -p web-server -q
fi
