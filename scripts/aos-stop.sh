#!/usr/bin/env bash
# Stop the AOS process started by scripts/aos-start.sh.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PID_FILE="$ROOT_DIR/.run/aos/web-server.pid"

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

if [ ! -f "$PID_FILE" ]; then
  echo "AOS is not running (no PID file)."
  exit 0
fi

pid="$(tr -cd '0-9' < "$PID_FILE")"
if [ -z "$pid" ] || ! process_is_running "$pid"; then
  rm -f "$PID_FILE"
  echo "AOS is not running; removed stale PID file."
  exit 0
fi

command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
case "$command_line" in
  *web-server*) ;;
  *) echo "refusing to stop PID $pid because it is not an AOS web-server" >&2; exit 1 ;;
esac

echo "==> Stopping AOS (PID $pid)"
kill -TERM "$pid" >/dev/null 2>&1 || true
attempts=0
while process_is_running "$pid" && [ "$attempts" -lt 30 ]; do
  sleep 1
  attempts=$((attempts + 1))
done
if process_is_running "$pid"; then
  echo "graceful shutdown timed out; stopping PID $pid" >&2
  kill -KILL "$pid" >/dev/null 2>&1 || true
  attempts=0
  while process_is_running "$pid" && [ "$attempts" -lt 5 ]; do
    sleep 1
    attempts=$((attempts + 1))
  done
fi
if process_is_running "$pid"; then
  echo "failed to stop AOS PID $pid; PID file was preserved" >&2
  exit 1
fi
rm -f "$PID_FILE"
echo "AOS stopped."
