#!/usr/bin/env bash
set -euo pipefail

# Fast local workflow for web-server development.
# Why:
# - Avoids starting multiple `cargo run` processes that compete for Cargo target locks.
# - Lets you run the binary directly after a successful build.
#
# Usage:
#   ./scripts/dev_web_server.sh check
#   ./scripts/dev_web_server.sh build
#   ./scripts/dev_web_server.sh run
#   ./scripts/dev_web_server.sh quick
#   ./scripts/dev_web_server.sh stop
#   ./scripts/dev_web_server.sh restart
#
# Environment:
#   AOS_ENV_FILE=/path/to/.env ./scripts/dev_web_server.sh run
#   AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh build
# By default this helper loads ../.env from the repository root when present.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

load_env_file() {
  local env_file="$1"
  if [[ ! -f "$env_file" ]]; then
    return 0
  fi
  set -a
  # shellcheck disable=SC1090
  source "$env_file"
  set +a
  echo "Loaded env file: $env_file"
}

if [[ -n "${AOS_ENV_FILE:-}" ]]; then
  load_env_file "$AOS_ENV_FILE"
else
  load_env_file "$ROOT_DIR/../.env"
  load_env_file "$ROOT_DIR/.env"
fi

# Local dev bootstrap defaults. Override these in your shell for real setups.
export ENCRYPTION_KEY="${ENCRYPTION_KEY:-12345678901234567890123456789012}"
export JWT_SECRET="${JWT_SECRET:-dev-secret-change-in-production-0000}"
export TOKEN_ENCRYPTION_KEY="${TOKEN_ENCRYPTION_KEY:-dev-token-encryption-key-change-me-0000}"
export AOSD_WEB_SEARCH_PROVIDER="${AOSD_WEB_SEARCH_PROVIDER:-auto}"
export AOS_WEB_SERVER_FEATURES="${AOS_WEB_SERVER_FEATURES:-full}"
export AOS_ALLOW_INSECURE_DEV_SECRETS="${AOS_ALLOW_INSECURE_DEV_SECRETS:-1}"

# `sccache` is useful in some CI setups, but local incremental Rust checks are
# often non-cacheable. Keep it opt-in for this helper to avoid slow dirty checks.
if [[ "${AOS_DEV_USE_SCCACHE:-0}" != "1" ]]; then
  export RUSTC_WRAPPER=""
fi

MODE="${1:-run}"
shift || true
if [[ "${1:-}" == "--" ]]; then
  shift
fi

BIN="./target/debug/web-server"
FEATURE_STAMP="./target/debug/.web-server.features"
BUILD_FAILED_STAMP="./target/debug/.web-server.build-failed"
RUN_PATTERNS=(
  "cargo run -p web-server"
  "$BIN"
)

cargo_feature_args() {
  if [[ -n "${AOS_WEB_SERVER_FEATURES:-}" ]]; then
    printf '%s\n' --features "$AOS_WEB_SERVER_FEATURES"
  fi
}

cargo_feature_key() {
  if [[ -n "${AOS_WEB_SERVER_FEATURES:-}" ]]; then
    printf '%s\n' "$AOS_WEB_SERVER_FEATURES"
  else
    printf '%s\n' "__default__"
  fi
}

binary_mtime() {
  if [[ ! -x "$BIN" ]]; then
    printf '%s\n' "missing"
  elif stat -f '%Sm' "$BIN" >/dev/null 2>&1; then
    stat -f '%Sm' "$BIN"
  else
    stat -c '%y' "$BIN"
  fi
}

binary_hash() {
  if [[ ! -x "$BIN" ]]; then
    printf '%s\n' "missing"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$BIN" | awk '{print $1}'
  else
    sha256sum "$BIN" | awk '{print $1}'
  fi
}

write_feature_stamp() {
  printf '%s\n%s\n' "$(cargo_feature_key)" "$(binary_hash)" > "$FEATURE_STAMP"
}

print_binary_info() {
  echo "web-server binary: $ROOT_DIR/$BIN"
  echo "web-server features: $(cargo_feature_key)"
  echo "web-server binary mtime: $(binary_mtime)"
}

stop_running() {
  for pattern in "${RUN_PATTERNS[@]}"; do
    pkill -f "$pattern" >/dev/null 2>&1 || true
  done
}

build_web_server() {
  echo "Building web-server with features: $(cargo_feature_key)"
  mkdir -p "$(dirname "$BUILD_FAILED_STAMP")"
  : > "$BUILD_FAILED_STAMP"
  if ! cargo build -p web-server $(cargo_feature_args); then
    echo "web-server build failed; refusing to mark the existing binary as current" >&2
    return 1
  fi
  write_feature_stamp
  rm -f "$BUILD_FAILED_STAMP"
  print_binary_info
}

web_server_needs_rebuild() {
  if [[ -f "$BUILD_FAILED_STAMP" ]]; then
    return 0
  fi
  if [[ ! -x "$BIN" ]]; then
    return 0
  fi
  if [[ ! -f "$FEATURE_STAMP" ]] || [[ "$(sed -n '1p' "$FEATURE_STAMP")" != "$(cargo_feature_key)" ]]; then
    return 0
  fi
  if [[ "$(sed -n '2p' "$FEATURE_STAMP")" != "$(binary_hash)" ]]; then
    return 0
  fi
  # Avoid an expensive Cargo dirty check when nothing under the Rust workspace
  # changed since the last web-server binary was produced.
  if find crates Cargo.toml Cargo.lock -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) -newer "$BIN" -print -quit | grep -q .; then
    return 0
  fi
  return 1
}

build_web_server_if_needed() {
  if web_server_needs_rebuild; then
    if [[ "${AOS_DEV_ALLOW_STALE_BINARY:-0}" == "1" && ! -f "$BUILD_FAILED_STAMP" ]]; then
      echo "web-server source or build fingerprint changed; running the existing binary because AOS_DEV_ALLOW_STALE_BINARY=1"
    else
      build_web_server
    fi
  else
    echo "web-server binary is up to date; skipping cargo build"
  fi
}

check_web_server() {
  local timeout_secs="${AOS_DEV_CHECK_TIMEOUT_SECS:-120}"
  if command -v timeout >/dev/null 2>&1; then
    timeout "$timeout_secs" cargo check -p web-server -q $(cargo_feature_args)
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$timeout_secs" cargo check -p web-server -q $(cargo_feature_args)
  else
    cargo check -p web-server -q $(cargo_feature_args)
  fi
}

run_binary() {
  if [[ ! -x "$BIN" ]]; then
    build_web_server
  fi
  print_binary_info
  exec "$BIN" "$@"
}

case "$MODE" in
  check)
    check_web_server
    ;;
  build)
    build_web_server
    ;;
  run)
    stop_running
    build_web_server_if_needed
    run_binary "$@"
    ;;
  quick)
    stop_running
    run_binary "$@"
    ;;
  stop)
    stop_running
    ;;
  restart)
    stop_running
    build_web_server
    run_binary "$@"
    ;;
  *)
    echo "Unknown mode: $MODE"
    echo "Usage: $0 {check|build|run|quick|stop|restart} [-- web-server-args...]"
    exit 1
    ;;
esac
