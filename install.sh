#!/usr/bin/env bash
# AOS native release builder.
#
# This script verifies the complete local toolchain and builds the Rust server
# plus the React WebUI. The server can serve the built UI directly.

set -euo pipefail

BUILD_PROFILE="${AOS_BUILD_PROFILE:-debug}"
SKIP_FRONTEND="${AOS_SKIP_FRONTEND_BUILD:-0}"
SKIP_VERIFY="${AOS_SKIP_VERIFY:-0}"
WEB_SERVER_FEATURES="${AOS_WEB_SERVER_FEATURES:-full}"

usage() {
  cat <<'USAGE'
Usage: ./install.sh [options]

Options:
  --release          Build the backend with the release profile.
  --debug            Build the backend with the debug profile (default).
  --skip-frontend    Skip npm install/build for the Web UI.
  --no-verify        Skip post-build smoke checks.
  -h, --help         Show this help text.

Environment overrides:
  AOS_BUILD_PROFILE=debug|release
  AOS_SKIP_FRONTEND_BUILD=1
  AOS_SKIP_VERIFY=1
  AOS_WEB_SERVER_FEATURES=full
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --release) BUILD_PROFILE="release" ;;
    --debug) BUILD_PROFILE="debug" ;;
    --skip-frontend) SKIP_FRONTEND="1" ;;
    --no-verify) SKIP_VERIFY="1" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

case "$BUILD_PROFILE" in
  debug|release) ;;
  *) echo "invalid build profile: $BUILD_PROFILE" >&2; exit 2 ;;
esac

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    return 1
  fi
}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
RUST_DIR="$SCRIPT_DIR/rust"
WEBUI_DIR="$SCRIPT_DIR/webui"

printf '\n==> Checking prerequisites\n'
"$SCRIPT_DIR/scripts/setup-environment.sh" --check

[ -f "$RUST_DIR/Cargo.toml" ] || { echo "missing rust/Cargo.toml" >&2; exit 1; }
[ -f "$WEBUI_DIR/package.json" ] || { echo "missing webui/package.json" >&2; exit 1; }

printf '\n==> Building backend (%s)\n' "$BUILD_PROFILE"
(
  cd "$RUST_DIR"
  if [ "$BUILD_PROFILE" = "release" ]; then
    cargo build -p web-server --release --features "$WEB_SERVER_FEATURES"
  else
    cargo build -p web-server --features "$WEB_SERVER_FEATURES"
  fi
)

BACKEND_BIN="$RUST_DIR/target/$BUILD_PROFILE/web-server"
[ -x "$BACKEND_BIN" ] || { echo "expected backend binary not found: $BACKEND_BIN" >&2; exit 1; }

if [ "$SKIP_FRONTEND" != "1" ]; then
  printf '\n==> Building frontend\n'
  (
    cd "$WEBUI_DIR"
    npm ci
    npm run build:ci
  )
fi

if [ "$SKIP_VERIFY" != "1" ]; then
  printf '\n==> Smoke checks\n'
  "$BACKEND_BIN" --help >/dev/null 2>&1 || true
  "$BACKEND_BIN" --help | rg -q -- '--web-dir' || {
    echo "backend binary does not expose --web-dir" >&2
    exit 1
  }
  if [ "$SKIP_FRONTEND" != "1" ]; then
    [ -d "$WEBUI_DIR/dist" ] || { echo "frontend dist/ was not generated" >&2; exit 1; }
    (
      cd "$WEBUI_DIR"
      npm test
    )
  fi
  (
    cd "$RUST_DIR"
    cargo test -p web-server --features "$WEB_SERVER_FEATURES" sqlite_tenant_defaults_seed_without_duplicates
  )
  "$SCRIPT_DIR/scripts/check-platform-sqlite-boundary.sh"
fi

cat <<EOF2

AOS build is ready.

Backend binary:
  $BACKEND_BIN

Start the backend and built WebUI:
  ./scripts/aos-start.sh

Create a same-platform release archive:
  ./scripts/aos-package.sh --skip-build
EOF2
