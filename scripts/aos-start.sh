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
case "$(uname -s)" in
  Darwin) ort_library="$ROOT_DIR/runtime/onnxruntime/lib/libonnxruntime.dylib" ;;
  Linux) ort_library="$ROOT_DIR/runtime/onnxruntime/lib/libonnxruntime.so" ;;
  *) ort_library="" ;;
esac
if [ -z "$ort_library" ] || [ ! -f "$ort_library" ]; then
  if [ "$SOURCE_LAYOUT" = "1" ]; then
    ort_runtime_dir="$ROOT_DIR/.aos-runtime/onnxruntime"
    "$ROOT_DIR/scripts/setup-onnxruntime.sh" --dir "$ort_runtime_dir"
    case "$(uname -s)" in
      Darwin) ort_library="$ort_runtime_dir/lib/libonnxruntime.dylib" ;;
      Linux) ort_library="$ort_runtime_dir/lib/libonnxruntime.so" ;;
    esac
  else
    echo "AOS Offline package is incomplete: bundled ONNX Runtime is missing" >&2
    echo "Runtime downloads are disabled. Re-extract a complete AOS Offline archive." >&2
    exit 1
  fi
fi
export ORT_DYLIB_PATH="${ORT_DYLIB_PATH:-$ort_library}"

if [ "$SOURCE_LAYOUT" = "0" ]; then
  export AOS_LOCAL_EMBEDDING_CACHE_DIR="${AOS_LOCAL_EMBEDDING_CACHE_DIR:-$ROOT_DIR/models/fastembed}"
else
  export AOS_LOCAL_EMBEDDING_CACHE_DIR="${AOS_LOCAL_EMBEDDING_CACHE_DIR:-$ROOT_DIR/.aos-runtime/models/fastembed}"
fi
model_snapshot="$AOS_LOCAL_EMBEDDING_CACHE_DIR/models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q/snapshots/faf4aa4225822f3bc6376869cb1164e8e3feedd0"
expected_model_sha() {
  case "$1" in
    model_optimized.onnx) echo "634d0f66c29dc934c8fa72b8a4fe91dd4d420a22f1d82a241058d4316e659a99" ;;
    tokenizer.json) echo "fa685fc160bbdbab64058d4fc91b60e62d207e8dc60b9af5c002c5ab946ded00" ;;
    config.json) echo "c8ec081fdad2df991bf5abbf18418fec7a5cdaa421f60ffb060a30040b8c376f" ;;
    special_tokens_map.json) echo "8c785abebea9ae3257b61681b4e6fd8365ceafde980c21970d001e834cf10835" ;;
    tokenizer_config.json) echo "0666eebf692422757e1dddf3c9fb1ded73ba3dc726c5828671fc89e45bf3609f" ;;
    *) return 1 ;;
  esac
}
sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "shasum or sha256sum is required to verify the bundled model" >&2
    return 1
  fi
}
model_complete="1"
for model_file in model_optimized.onnx tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
  if [ ! -f "$model_snapshot/$model_file" ]; then
    model_complete="0"
  fi
done
if [ "$model_complete" != "1" ]; then
  if [ "$SOURCE_LAYOUT" = "1" ]; then
    echo "==> Downloading the pinned local embedding model for this source checkout"
    "$ROOT_DIR/scripts/download-local-embedding.sh" --dir "$AOS_LOCAL_EMBEDDING_CACHE_DIR"
    "$BACKEND_BIN" --warm-local-embedding "$AOS_LOCAL_EMBEDDING_CACHE_DIR"
  else
    echo "AOS Offline package is incomplete: bundled local embedding files are missing" >&2
    echo "Runtime downloads are disabled. Re-extract a complete AOS Offline archive." >&2
    exit 1
  fi
fi
for model_file in model_optimized.onnx tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
  actual_model_sha="$(sha256_file "$model_snapshot/$model_file")"
  expected_sha="$(expected_model_sha "$model_file")"
  [ "$actual_model_sha" = "$expected_sha" ] || {
    echo "AOS local embedding checksum mismatch for $model_file; refusing to start" >&2
    exit 1
  }
done

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
