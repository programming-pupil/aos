#!/usr/bin/env bash
# Build when necessary, then start the SQLite-only AOS backend and built WebUI.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
ENV_FILE=""
DATA_DIR=""
HOST_OVERRIDE=""
PORT_OVERRIDE=""
FOREGROUND="0"
AUTO_BUILD="1"

usage() {
  cat <<'USAGE'
Usage: ./scripts/aos-start.sh [options]

Options:
  --foreground       Keep AOS attached to this terminal.
  --no-build         Fail instead of building missing release artifacts.
  --env-file PATH    Environment file (default: .env).
  --data-dir PATH    Local data directory (default: .aos-data).
  --host HOST        Bind host (default: AOS_BIND_HOST or 127.0.0.1).
  --port PORT        Web/API port (default: AOS_WEB_PORT or 3000).
  -h, --help         Show this help text.

The Rust server serves both /api and the built WebUI from one local process.
USAGE
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

while [ "$#" -gt 0 ]; do
  case "$1" in
    --foreground) FOREGROUND="1" ;;
    --no-build) AUTO_BUILD="0" ;;
    --env-file) [ "$#" -ge 2 ] || { echo "--env-file needs a path" >&2; exit 2; }; ENV_FILE="$2"; shift ;;
    --data-dir) [ "$#" -ge 2 ] || { echo "--data-dir needs a path" >&2; exit 2; }; DATA_DIR="$2"; shift ;;
    --host) [ "$#" -ge 2 ] || { echo "--host needs a value" >&2; exit 2; }; HOST_OVERRIDE="$2"; shift ;;
    --port) [ "$#" -ge 2 ] || { echo "--port needs a value" >&2; exit 2; }; PORT_OVERRIDE="$2"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

ENV_FILE="${ENV_FILE:-$ROOT_DIR/.env}"
DATA_DIR="${DATA_DIR:-$ROOT_DIR/.aos-data}"
RUN_DIR="$ROOT_DIR/.run/aos"
PID_FILE="$RUN_DIR/web-server.pid"
LOG_FILE="$RUN_DIR/web-server.log"

case "$DATA_DIR" in
  /*)
    case "$DATA_DIR" in
      "$ROOT_DIR"/*) ;;
      *) echo "AOS data directory must be inside $ROOT_DIR" >&2; exit 1 ;;
    esac
    ;;
  *)
    case "/$DATA_DIR/" in
      */../*) echo "AOS data directory cannot contain '..' path segments" >&2; exit 1 ;;
    esac
    DATA_DIR="$ROOT_DIR/$DATA_DIR"
    ;;
esac
mkdir -p "$DATA_DIR"
DATA_DIR="$(cd "$DATA_DIR" && pwd -P)"
case "$DATA_DIR" in
  "$ROOT_DIR"/*) ;;
  *) echo "AOS data directory must be inside $ROOT_DIR" >&2; exit 1 ;;
esac

if [ -x "$ROOT_DIR/bin/web-server" ] && [ -f "$ROOT_DIR/web/index.html" ]; then
  BACKEND_BIN="$ROOT_DIR/bin/web-server"
  WEB_DIR="$ROOT_DIR/web"
  SOURCE_LAYOUT="0"
else
  BACKEND_BIN="$ROOT_DIR/rust/target/release/web-server"
  WEB_DIR="$ROOT_DIR/webui/dist"
  SOURCE_LAYOUT="1"
fi

if ! "$ROOT_DIR/scripts/setup-environment.sh" --check; then
  echo "AOS environment is incomplete. Run ./scripts/setup-environment.sh --install, then start again." >&2
  exit 1
fi

if [ ! -x "$BACKEND_BIN" ] || [ ! -f "$WEB_DIR/index.html" ]; then
  if [ "$SOURCE_LAYOUT" = "1" ] && [ "$AUTO_BUILD" = "1" ]; then
    echo "==> Release artifacts are missing; building AOS"
    "$ROOT_DIR/install.sh" --release
  else
    echo "release artifacts are missing; build the source tree or extract a complete AOS package" >&2
    exit 1
  fi
fi

if [ ! -f "$ENV_FILE" ]; then
  echo "==> Generating local environment file"
  "$ROOT_DIR/scripts/generate-env.sh" "$ENV_FILE"
else
  # Older checkouts may already have an .env containing public placeholders or
  # no encryption keys. Preserve every other setting and repair only secrets
  # that the server would reject at startup.
  "$ROOT_DIR/scripts/generate-env.sh" --repair "$ENV_FILE"
fi

set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a

BIND_HOST="${HOST_OVERRIDE:-${AOS_BIND_HOST:-127.0.0.1}}"
CONFIGURED_WEB_PORT="${AOS_WEB_PORT:-3000}"
WEB_PORT="${PORT_OVERRIDE:-$CONFIGURED_WEB_PORT}"
case "$WEB_PORT" in
  ''|*[!0-9]*) echo "invalid port: $WEB_PORT" >&2; exit 2 ;;
esac
if [ "$WEB_PORT" -lt 1 ] || [ "$WEB_PORT" -gt 65535 ]; then
  echo "port must be between 1 and 65535" >&2
  exit 2
fi

export AOS_BIND_HOST="$BIND_HOST"
export AOS_WEB_PORT="$WEB_PORT"
if [ -n "$PORT_OVERRIDE" ]; then
  case "${BASE_URL:-}" in
    ""|"http://localhost:$CONFIGURED_WEB_PORT"|"http://127.0.0.1:$CONFIGURED_WEB_PORT")
      BASE_URL="http://localhost:$WEB_PORT"
      ;;
  esac
fi
export BASE_URL="${BASE_URL:-http://localhost:$WEB_PORT}"
export RUST_LOG="${RUST_LOG:-web_server=info,agent_gateway=info,runtime=info,tower_http=info,billing=info}"
# shellcheck disable=SC1091
. "$ROOT_DIR/scripts/lib/local-embedding-runtime.sh"
aos_prepare_local_embedding_runtime "$ROOT_DIR" "$SOURCE_LAYOUT" "$BACKEND_BIN"

mkdir -p "$RUN_DIR"
if [ -f "$PID_FILE" ]; then
  old_pid="$(tr -cd '0-9' < "$PID_FILE")"
  if [ -n "$old_pid" ] && process_is_running "$old_pid"; then
    echo "AOS is already running (PID $old_pid): http://localhost:$WEB_PORT"
    exit 0
  fi
  rm -f "$PID_FILE"
fi

filesystem_type=""
if stat -f -c %T "$DATA_DIR" >/dev/null 2>&1; then
  filesystem_type="$(stat -f -c %T "$DATA_DIR")"
elif stat -f %T "$DATA_DIR" >/dev/null 2>&1; then
  filesystem_type="$(stat -f %T "$DATA_DIR")"
fi
case "$filesystem_type" in
  nfs*|smb*|cifs*|fuse.sshfs*)
    echo "AOS SQLite data must be on a local filesystem, not $filesystem_type" >&2
    exit 1
    ;;
esac

start_server() {
  exec "$BACKEND_BIN" \
    --addr "$BIND_HOST:$WEB_PORT" \
    --data-dir "$DATA_DIR" \
    --web-dir "$WEB_DIR"
}

wait_until_ready() {
  local pid="$1"
  local attempts=0
  while [ "$attempts" -lt 120 ]; do
    if ! process_is_running "$pid"; then
      return 1
    fi
    if curl -fsS "http://127.0.0.1:$WEB_PORT/api/v1/setup/check" >/dev/null 2>&1; then
      return 0
    fi
    attempts=$((attempts + 1))
    sleep 1
  done
  return 1
}

if [ "$FOREGROUND" = "1" ]; then
  stop_foreground_server() {
    trap - EXIT INT TERM
    if [ -n "${server_pid:-}" ] && process_is_running "$server_pid"; then
      kill -TERM "$server_pid" >/dev/null 2>&1 || true
      attempts=0
      while process_is_running "$server_pid" && [ "$attempts" -lt 30 ]; do
        sleep 1
        attempts=$((attempts + 1))
      done
      if process_is_running "$server_pid"; then
        echo "graceful shutdown timed out; stopping PID $server_pid" >&2
        kill -KILL "$server_pid" >/dev/null 2>&1 || true
      fi
      wait "$server_pid" 2>/dev/null || true
    fi
    rm -f "$PID_FILE"
  }
  trap 'stop_foreground_server; exit 130' INT
  trap 'stop_foreground_server; exit 143' TERM
  trap stop_foreground_server EXIT
  echo "==> Starting AOS in the foreground"
  start_server &
  server_pid="$!"
  printf '%s\n' "$server_pid" > "$PID_FILE"
  echo "Open http://localhost:$WEB_PORT"
  set +e
  wait "$server_pid"
  server_status="$?"
  set -e
  trap - EXIT INT TERM
  rm -f "$PID_FILE"
  exit "$server_status"
else
  echo "==> Starting AOS"
  nohup bash -c 'exec "$@"' bash \
    "$BACKEND_BIN" \
    --addr "$BIND_HOST:$WEB_PORT" \
    --data-dir "$DATA_DIR" \
    --web-dir "$WEB_DIR" \
    >"$LOG_FILE" 2>&1 &
  server_pid="$!"
  printf '%s\n' "$server_pid" > "$PID_FILE"
  if ! wait_until_ready "$server_pid"; then
    echo "AOS failed to become ready. Recent log output:" >&2
    tail -n 60 "$LOG_FILE" >&2 || true
    kill "$server_pid" >/dev/null 2>&1 || true
    rm -f "$PID_FILE"
    exit 1
  fi
  cat <<EOF
AOS is ready: http://localhost:$WEB_PORT
PID: $server_pid
Log: $LOG_FILE
Stop: ./scripts/aos-stop.sh
EOF
fi
