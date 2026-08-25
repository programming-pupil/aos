#!/usr/bin/env bash
# Start the open-source AOS demo with its embedded SQLite platform database.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
DEMO_DATA_DIR="${AOS_DEMO_DATA_DIR:-$ROOT_DIR/.aos-demo-data}"
WEB_PORT="${AOS_DEMO_WEB_PORT:-5173}"
API_ADDR="${AOS_DEMO_API_ADDR:-0.0.0.0:3001}"
API_READY_TIMEOUT_SECS="${AOS_DEMO_API_READY_TIMEOUT_SECS:-1800}"

usage() {
  cat <<'USAGE'
Usage: ./scripts/aos-demo-start.sh

Starts one Rust web-server with a local aos.db and the Vite WebUI.

Environment overrides:
  AOS_DEMO_DATA_DIR=.aos-demo-data
  AOS_DEMO_ENV_FILE=.aos-demo-data/.env
  AOS_DEMO_WEB_PORT=5173
  AOS_DEMO_API_ADDR=0.0.0.0:3001
  AOS_DEMO_API_READY_TIMEOUT_SECS=1800
  AOS_DEMO_API_READY_URL=http://127.0.0.1:3001/api/v1/setup/check
  AOS_DEMO_API_PROXY_TARGET=http://127.0.0.1:3001
  JWT_SECRET=...
  ENCRYPTION_KEY=...
  TOKEN_ENCRYPTION_KEY=...
  DEFAULT_MODEL=...
USAGE
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi
if [ "$#" -ne 0 ]; then
  echo "unknown argument: $1" >&2
  usage
  exit 2
fi

# Runtime workspace validation requires an absolute data directory. Normalize
# both the default and user-supplied relative overrides before deriving paths.
case "$DEMO_DATA_DIR" in
  /*) ;;
  *) DEMO_DATA_DIR="$ROOT_DIR/$DEMO_DATA_DIR" ;;
esac
mkdir -p "$DEMO_DATA_DIR"
DEMO_DATA_DIR="$(cd "$DEMO_DATA_DIR" && pwd -P)"
DEMO_ENV_FILE="${AOS_DEMO_ENV_FILE:-$DEMO_DATA_DIR/.env}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

process_is_running() {
  local target_pid="$1"
  local process_state
  kill -0 "$target_pid" >/dev/null 2>&1 || return 1
  process_state="$(ps -p "$target_pid" -o stat= 2>/dev/null | awk '{print $1}')"
  case "$process_state" in
    ""|Z*) return 1 ;;
    *) return 0 ;;
  esac
}

wait_for_backend() {
  local pid="$1"
  local started_at="$(date +%s)"
  local elapsed=0
  local server_status=0

  echo "==> Waiting for web-server readiness at ${API_READY_URL}"
  while [ "$elapsed" -lt "$API_READY_TIMEOUT_SECS" ]; do
    if ! process_is_running "$pid"; then
      wait "$pid" || server_status="$?"
      echo "web-server exited before becoming ready (status ${server_status})" >&2
      return 1
    fi
    if curl --connect-timeout 1 --max-time 2 -fsS "$API_READY_URL" >/dev/null 2>&1; then
      echo "==> Web-server is ready"
      return 0
    fi
    sleep 1
    elapsed=$(( $(date +%s) - started_at ))
    if [ $((elapsed % 15)) -eq 0 ]; then
      echo "    still waiting for web-server (${elapsed}s elapsed)"
    fi
  done

  echo "web-server did not become ready within ${API_READY_TIMEOUT_SECS}s" >&2
  return 1
}

cleanup() {
  if [ -n "${SERVER_PID:-}" ] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  if [ -n "${WEB_PID:-}" ] && kill -0 "$WEB_PID" >/dev/null 2>&1; then
    kill "$WEB_PID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

require_cmd cargo
require_cmd npm
require_cmd openssl
require_cmd curl

case "$API_READY_TIMEOUT_SECS" in
  ''|*[!0-9]*)
    echo "AOS_DEMO_API_READY_TIMEOUT_SECS must be a positive integer" >&2
    exit 2
    ;;
esac
if [ "$API_READY_TIMEOUT_SECS" -lt 1 ]; then
  echo "AOS_DEMO_API_READY_TIMEOUT_SECS must be a positive integer" >&2
  exit 2
fi

API_PORT="${API_ADDR##*:}"
API_PORT="${API_PORT%]}"
case "$API_PORT" in
  ''|*[!0-9]*)
    echo "cannot derive readiness port from AOS_DEMO_API_ADDR=${API_ADDR}" >&2
    exit 2
    ;;
esac
if [ "$API_PORT" -lt 1 ] || [ "$API_PORT" -gt 65535 ]; then
  echo "AOS_DEMO_API_ADDR port must be between 1 and 65535" >&2
  exit 2
fi
API_READY_URL="${AOS_DEMO_API_READY_URL:-http://127.0.0.1:${API_PORT}/api/v1/setup/check}"
export AOS_DEMO_API_PROXY_TARGET="${AOS_DEMO_API_PROXY_TARGET:-http://127.0.0.1:${API_PORT}}"

if [[ ! -f "$DEMO_ENV_FILE" ]]; then
  "$ROOT_DIR/scripts/generate-env.sh" "$DEMO_ENV_FILE"
fi

read_demo_env() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key { sub(/^[^=]*=/, ""); gsub(/^"|"$/, ""); print; exit }' "$DEMO_ENV_FILE"
}

export JWT_SECRET="${JWT_SECRET:-$(read_demo_env JWT_SECRET)}"
export ENCRYPTION_KEY="${ENCRYPTION_KEY:-$(read_demo_env ENCRYPTION_KEY)}"
export TOKEN_ENCRYPTION_KEY="${TOKEN_ENCRYPTION_KEY:-$(read_demo_env TOKEN_ENCRYPTION_KEY)}"
export DEFAULT_MODEL="${DEFAULT_MODEL:-$(read_demo_env DEFAULT_MODEL)}"
export BASE_URL="${BASE_URL:-http://localhost:${WEB_PORT}}"
export RUST_LOG="${RUST_LOG:-web_server=info,agent_gateway=info,runtime=info,tower_http=info,billing=info}"

echo "==> Starting web-server with ${DEMO_DATA_DIR}/aos.db (${API_ADDR})"
(
  cd "$ROOT_DIR/rust"
  exec cargo run -p web-server --bin web-server --features full -- --addr "$API_ADDR" --data-dir "$DEMO_DATA_DIR"
) &
SERVER_PID="$!"

echo "==> Installing WebUI dependencies if needed"
(
  cd "$ROOT_DIR/webui"
  if [ ! -d node_modules ]; then
    npm ci
  fi
)

wait_for_backend "$SERVER_PID"

echo "==> Starting WebUI on http://localhost:${WEB_PORT}"
(
  cd "$ROOT_DIR/webui"
  npm run dev -- --host 0.0.0.0 --port "$WEB_PORT"
) &
WEB_PID="$!"

cat <<EOF

AOS demo is ready.

Open:
  http://localhost:${WEB_PORT}

First run:
  1. Complete setup and create an admin user.
  2. Open Dashboard.
  3. Click one of the four Open-source Wow Demo cards.

Stop:
  Press Ctrl+C in this terminal.
EOF

wait "$SERVER_PID" "$WEB_PID"
