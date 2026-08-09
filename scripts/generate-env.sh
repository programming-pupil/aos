#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TEMPLATE="$ROOT_DIR/.env.example"
ENV_FILE=""
REPAIR="0"

usage() {
  cat <<'USAGE'
Usage: ./scripts/generate-env.sh [--repair] [ENV_FILE]

Without --repair, create a new environment file and refuse to overwrite an
existing one. With --repair, preserve existing settings while replacing only
missing, malformed, or public-placeholder server secrets.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repair) REPAIR="1" ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "unknown argument: $1" >&2; usage; exit 2 ;;
    *)
      [ -z "$ENV_FILE" ] || { echo "only one environment file may be provided" >&2; exit 2; }
      ENV_FILE="$1"
      ;;
  esac
  shift
done

ENV_FILE="${ENV_FILE:-$ROOT_DIR/.env}"

if ! command -v openssl >/dev/null 2>&1; then
  echo "openssl is required to generate deployment secrets" >&2
  exit 1
fi
if [[ ! -f "$TEMPLATE" ]]; then
  echo "missing environment template: $TEMPLATE" >&2
  exit 1
fi
if [[ -e "$ENV_FILE" && "$REPAIR" != "1" ]]; then
  echo "refusing to overwrite existing environment file: $ENV_FILE" >&2
  exit 1
fi
if [[ ! -e "$ENV_FILE" && "$REPAIR" == "1" ]]; then
  REPAIR="0"
fi

umask 077
tmp_file="${ENV_FILE}.tmp.$$"
trap 'rm -f "$tmp_file" "${tmp_file}.next"' EXIT
if [[ "$REPAIR" == "1" ]]; then
  cp "$ENV_FILE" "$tmp_file"
else
  cp "$TEMPLATE" "$tmp_file"
fi

replace_env() {
  local key="$1"
  local value="$2"
  awk -v key="$key" -v value="$value" '
    index($0, key "=") == 1 {
      if (!replaced) print key "=" value
      replaced = 1
      next
    }
    { print }
    END { if (!replaced) print key "=" value }
  ' "$tmp_file" > "${tmp_file}.next"
  mv "${tmp_file}.next" "$tmp_file"
}

read_env_value() {
  local key="$1"
  awk -v key="$key" '
    index($0, key "=") == 1 {
      value = substr($0, length(key) + 2)
      found = 1
    }
    END { if (found) print value }
  ' "$tmp_file"
}

secret_is_valid() {
  local key="$1"
  local value="$2"
  local lower byte_length
  case "$value" in
    \"*\") value="${value#\"}"; value="${value%\"}" ;;
    \'*\') value="${value#\'}"; value="${value%\'}" ;;
  esac
  [ -n "$value" ] || return 1
  lower="$(printf '%s' "$value" | tr '[:upper:]' '[:lower:]')"
  case "$lower" in
    *change-me*|*replace-me*|your-*|dev-secret*) return 1 ;;
  esac
  [ "$value" != "12345678901234567890123456789012" ] || return 1
  byte_length="$(LC_ALL=C printf '%s' "$value" | wc -c | tr -d '[:space:]')"
  if [ "$key" = "ENCRYPTION_KEY" ]; then
    [ "$byte_length" -eq 32 ]
  else
    [ "$byte_length" -ge 32 ]
  fi
}

repaired_keys=""
repair_secret() {
  local key="$1"
  local bytes="$2"
  local current
  current="$(read_env_value "$key")"
  if ! secret_is_valid "$key" "$current"; then
    replace_env "$key" "$(openssl rand -hex "$bytes")"
    repaired_keys="${repaired_keys}${repaired_keys:+, }$key"
  fi
}

repair_secret "JWT_SECRET" 32
repair_secret "ENCRYPTION_KEY" 16
repair_secret "TOKEN_ENCRYPTION_KEY" 32
if [[ -n "${AOSD_GITHUB_TOKEN:-}" ]]; then
  replace_env "AOSD_GITHUB_TOKEN" "$AOSD_GITHUB_TOKEN"
fi

mv "$tmp_file" "$ENV_FILE"
trap - EXIT
if [[ "$REPAIR" == "1" ]]; then
  if [[ -n "$repaired_keys" ]]; then
    echo "repaired insecure or missing environment secrets in $ENV_FILE: $repaired_keys"
  else
    echo "environment secrets are valid: $ENV_FILE"
  fi
else
  echo "generated secure environment file: $ENV_FILE"
fi
