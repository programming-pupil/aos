#!/usr/bin/env bash
# Upgrade an extracted AOS Offline installation while preserving all local data.

set -euo pipefail

SOURCE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TARGET_ROOT=""
DATA_DIR=""
ENV_FILE=""
PORT=""

usage() {
  cat <<'USAGE'
Usage: ./scripts/aos-upgrade.sh --target PATH [options]

Run this script from a newly extracted AOS Offline package. It stops the old
installation, backs up its complete data directory, swaps release files, starts
the new version, and automatically rolls back if readiness fails.

Options:
  --target PATH     Existing AOS Offline installation to upgrade (required).
  --data-dir PATH   Existing data directory (default: TARGET/.aos-data).
  --env-file PATH   Existing environment file (default: TARGET/.env).
  --port PORT       Port used for the post-upgrade health check.
  -h, --help        Show this help text.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target) [ "$#" -ge 2 ] || { echo "--target needs a path" >&2; exit 2; }; TARGET_ROOT="$2"; shift ;;
    --data-dir) [ "$#" -ge 2 ] || { echo "--data-dir needs a path" >&2; exit 2; }; DATA_DIR="$2"; shift ;;
    --env-file) [ "$#" -ge 2 ] || { echo "--env-file needs a path" >&2; exit 2; }; ENV_FILE="$2"; shift ;;
    --port) [ "$#" -ge 2 ] || { echo "--port needs a value" >&2; exit 2; }; PORT="$2"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

[ -n "$TARGET_ROOT" ] || { echo "--target is required" >&2; usage; exit 2; }
[ -d "$TARGET_ROOT" ] || { echo "target installation does not exist: $TARGET_ROOT" >&2; exit 1; }
TARGET_ROOT="$(cd "$TARGET_ROOT" && pwd -P)"

case "$TARGET_ROOT" in
  /|/usr|/usr/*|/opt|/Applications) echo "refusing unsafe target path: $TARGET_ROOT" >&2; exit 1 ;;
esac
if [ "$TARGET_ROOT" = "$SOURCE_ROOT" ]; then
  echo "source and target are the same directory; extract the new package beside the old installation" >&2
  exit 1
fi
[ -x "$SOURCE_ROOT/bin/web-server" ] || { echo "new package is missing bin/web-server" >&2; exit 1; }
[ -f "$SOURCE_ROOT/web/index.html" ] || { echo "new package is missing web/index.html" >&2; exit 1; }
[ -x "$TARGET_ROOT/scripts/aos-start.sh" ] || { echo "target is not an AOS Offline installation" >&2; exit 1; }

DATA_DIR="${DATA_DIR:-$TARGET_ROOT/.aos-data}"
ENV_FILE="${ENV_FILE:-$TARGET_ROOT/.env}"
case "$DATA_DIR" in
  /*) ;;
  *) DATA_DIR="$TARGET_ROOT/$DATA_DIR" ;;
esac
case "$ENV_FILE" in
  /*) ;;
  *) ENV_FILE="$TARGET_ROOT/$ENV_FILE" ;;
esac
mkdir -p "$DATA_DIR"
DATA_DIR="$(cd "$DATA_DIR" && pwd -P)"
ENV_PARENT="$(cd "$(dirname "$ENV_FILE")" && pwd -P)"
ENV_FILE="$ENV_PARENT/$(basename "$ENV_FILE")"
case "$DATA_DIR" in
  "$TARGET_ROOT"/*) ;;
  *) echo "data directory must remain inside the target installation: $TARGET_ROOT" >&2; exit 1 ;;
esac
case "$ENV_FILE" in
  "$TARGET_ROOT"/*) ;;
  *) echo "environment file must remain inside the target installation: $TARGET_ROOT" >&2; exit 1 ;;
esac
[ -f "$ENV_FILE" ] || { echo "target environment file is missing: $ENV_FILE" >&2; exit 1; }

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

if [ -f "$SOURCE_ROOT/RELEASE-MANIFEST.sha256" ]; then
  echo "==> Verifying new release manifest"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    expected="${line%%  *}"
    relative="${line#*  }"
    [ -f "$SOURCE_ROOT/$relative" ] || { echo "release file is missing: $relative" >&2; exit 1; }
    actual="$(sha256_file "$SOURCE_ROOT/$relative")"
    [ "$actual" = "$expected" ] || { echo "release checksum mismatch: $relative" >&2; exit 1; }
  done < "$SOURCE_ROOT/RELEASE-MANIFEST.sha256"
fi

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
BACKUP_ROOT="$TARGET_ROOT/.aos-backups/upgrade-$timestamp"
ROLLBACK_ROOT="$BACKUP_ROOT/release"
FAILED_ROOT="$BACKUP_ROOT/failed-new-release"
mkdir -p "$ROLLBACK_ROOT" "$FAILED_ROOT"

DATA_PARENT="$(dirname "$DATA_DIR")"
DATA_NAME="$(basename "$DATA_DIR")"
DATA_ARCHIVE="$BACKUP_ROOT/data-before-upgrade.tar.gz"
ASSETS="bin web runtime models scripts docs licenses examples .env.example README.md README.zh-CN.md LICENSE NOTICE.md RELEASE-MANIFEST.sha256"
rollback_required="0"
rollback_running="0"

restore_previous_release() {
  [ "$rollback_required" = "1" ] || return 0
  [ "$rollback_running" = "0" ] || return 0
  rollback_running="1"
  echo "==> Upgrade failed; restoring the previous AOS release" >&2
  if [ -x "$TARGET_ROOT/scripts/aos-stop.sh" ]; then
    "$TARGET_ROOT/scripts/aos-stop.sh" >/dev/null 2>&1 || true
  fi
  for asset in $ASSETS; do
    if [ -e "$TARGET_ROOT/$asset" ]; then
      mkdir -p "$FAILED_ROOT/$(dirname "$asset")"
      mv "$TARGET_ROOT/$asset" "$FAILED_ROOT/$asset" 2>/dev/null || true
    fi
    if [ -e "$ROLLBACK_ROOT/$asset" ]; then
      mkdir -p "$TARGET_ROOT/$(dirname "$asset")"
      mv "$ROLLBACK_ROOT/$asset" "$TARGET_ROOT/$asset"
    fi
  done
  if [ -f "$DATA_ARCHIVE" ]; then
    if [ -e "$DATA_DIR" ]; then
      mv "$DATA_DIR" "$BACKUP_ROOT/data-after-failed-upgrade" 2>/dev/null || true
    fi
    tar -C "$DATA_PARENT" -xzf "$DATA_ARCHIVE"
  fi
  if [ -x "$TARGET_ROOT/scripts/aos-start.sh" ]; then
    start_args=(--no-build --env-file "$ENV_FILE" --data-dir "$DATA_DIR")
    [ -z "$PORT" ] || start_args+=(--port "$PORT")
    "$TARGET_ROOT/scripts/aos-start.sh" "${start_args[@]}" || true
  fi
  echo "Previous release restored. Backup: $BACKUP_ROOT" >&2
}

on_exit() {
  status="$?"
  if [ "$status" -ne 0 ]; then
    restore_previous_release
  fi
  exit "$status"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "==> Stopping the existing AOS instance"
"$TARGET_ROOT/scripts/aos-stop.sh"

echo "==> Backing up AOS data"
tar -C "$DATA_PARENT" -czf "$DATA_ARCHIVE" "$DATA_NAME"
cp "$ENV_FILE" "$BACKUP_ROOT/env.before-upgrade"
if [ -f "$DATA_DIR/aos.db" ]; then
  sha256_file "$DATA_DIR/aos.db" > "$BACKUP_ROOT/aos.db.before.sha256"
fi
rollback_required="1"

echo "==> Installing the new release files"
for asset in $ASSETS; do
  [ -e "$SOURCE_ROOT/$asset" ] || continue
  if [ -e "$TARGET_ROOT/$asset" ]; then
    mkdir -p "$ROLLBACK_ROOT/$(dirname "$asset")"
    mv "$TARGET_ROOT/$asset" "$ROLLBACK_ROOT/$asset"
  fi
  mkdir -p "$TARGET_ROOT/$(dirname "$asset")"
  cp -pR "$SOURCE_ROOT/$asset" "$TARGET_ROOT/$asset"
done
chmod +x "$TARGET_ROOT/bin/web-server" "$TARGET_ROOT/scripts/"*.sh

echo "==> Starting upgraded AOS"
start_args=(--no-build --env-file "$ENV_FILE" --data-dir "$DATA_DIR")
[ -z "$PORT" ] || start_args+=(--port "$PORT")
"$TARGET_ROOT/scripts/aos-start.sh" "${start_args[@]}"

[ -f "$DATA_DIR/aos.db" ] || { echo "upgraded AOS did not preserve aos.db" >&2; exit 1; }
rollback_required="0"
trap - EXIT INT TERM
echo "AOS upgrade completed. Data and configuration were preserved."
echo "Pre-upgrade backup: $BACKUP_ROOT"
