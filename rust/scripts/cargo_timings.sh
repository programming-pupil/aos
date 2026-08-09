#!/usr/bin/env bash
set -euo pipefail

# Produce local Cargo timing reports for the web-server crate.
# Usage:
#   ./scripts/cargo_timings.sh check
#   ./scripts/cargo_timings.sh build
#   ./scripts/cargo_timings.sh both
#   AOS_WEB_SERVER_FEATURES=full ./scripts/cargo_timings.sh both

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

MODE="${1:-both}"
shift || true
if [[ "${1:-}" == "--" ]]; then
  shift
fi

cargo_feature_args() {
  if [[ -n "${AOS_WEB_SERVER_FEATURES:-}" ]]; then
    printf '%s\n' --features "$AOS_WEB_SERVER_FEATURES"
  fi
}

run_check() {
  cargo check -p web-server --timings $(cargo_feature_args) "$@"
}

run_build() {
  cargo build -p web-server --timings $(cargo_feature_args) "$@"
}

case "$MODE" in
  check)
    run_check "$@"
    ;;
  build)
    run_build "$@"
    ;;
  both)
    run_check "$@"
    run_build "$@"
    ;;
  *)
    echo "Unknown mode: $MODE"
    echo "Usage: $0 {check|build|both} [-- cargo-args...]"
    exit 1
    ;;
esac

echo
echo "Timing reports are written under target/cargo-timings/."
