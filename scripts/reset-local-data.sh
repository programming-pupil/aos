#!/usr/bin/env bash
# Remove only AOS runtime data created inside this checkout/package.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
DATA_DIR="$ROOT_DIR/.aos-data"
RESET_ALL="0"
ASSUME_YES="0"

usage() {
  cat <<'USAGE'
Usage: ./scripts/reset-local-data.sh [options]

Options:
  --data-dir PATH  Reset a data directory inside this AOS checkout/package.
  --all            Also remove known demo/smoke/runtime-state directories.
  --yes            Do not ask for confirmation.
  -h, --help       Show this help text.

This permanently deletes tenants, users, keys, tasks, histories, embeddings,
uploads, and other runtime state in the selected local data directory.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --data-dir) [ "$#" -ge 2 ] || { echo "--data-dir needs a path" >&2; exit 2; }; DATA_DIR="$2"; shift ;;
    --all) RESET_ALL="1" ;;
    --yes) ASSUME_YES="1" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

case "$DATA_DIR" in
  /*) candidate="$DATA_DIR" ;;
  *) candidate="$ROOT_DIR/$DATA_DIR" ;;
esac
candidate_parent="$(cd "$(dirname "$candidate")" && pwd)"
candidate="$candidate_parent/$(basename "$candidate")"

case "$candidate" in
  "$ROOT_DIR"/*) ;;
  *) echo "refusing to remove a data directory outside $ROOT_DIR" >&2; exit 1 ;;
esac
case "$candidate" in
  "$ROOT_DIR"|"$ROOT_DIR/."|/|"${HOME:-__unset__}")
    echo "refusing unsafe data directory: $candidate" >&2
    exit 1
    ;;
esac

targets=("$candidate" "$ROOT_DIR/.run")
if [ "$RESET_ALL" = "1" ]; then
  targets+=(
    "$ROOT_DIR/.aos-data-smoke"
    "$ROOT_DIR/.aos-demo-data"
    "$ROOT_DIR/rust/.claw"
  )
fi

existing=()
for target in "${targets[@]}"; do
  if [ -e "$target" ]; then
    existing+=("$target")
  fi
done
if [ "${#existing[@]}" -eq 0 ]; then
  echo "No AOS runtime data found."
  exit 0
fi

echo "The following AOS runtime paths will be permanently removed:"
printf '  %s\n' "${existing[@]}"
if [ "$ASSUME_YES" != "1" ]; then
  printf 'Type RESET to continue: '
  read -r confirmation
  [ "$confirmation" = "RESET" ] || { echo "Cancelled."; exit 1; }
fi

if [ -x "$ROOT_DIR/scripts/aos-stop.sh" ]; then
  "$ROOT_DIR/scripts/aos-stop.sh"
fi
for target in "${existing[@]}"; do
  rm -rf -- "$target"
done
echo "AOS runtime data removed. The next start will open the first-run setup wizard."
