#!/usr/bin/env bash
# Download the exact local embedding snapshot used to build AOS Offline.

set -euo pipefail

REVISION="faf4aa4225822f3bc6376869cb1164e8e3feedd0"
REPOSITORY="Qdrant/paraphrase-multilingual-MiniLM-L12-v2-onnx-Q"
MODEL_DIR_NAME="models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q"
DESTINATION=""

usage() {
  cat <<'USAGE'
Usage: ./scripts/download-local-embedding.sh --dir PATH

Downloads the pinned AOS local embedding model into PATH and verifies every
file. This build-time/source-setup helper is not included in AOS Offline.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) [ "$#" -ge 2 ] || { echo "--dir needs a path" >&2; exit 2; }; DESTINATION="$2"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

[ -n "$DESTINATION" ] || { echo "--dir is required" >&2; exit 2; }
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }

snapshot="$DESTINATION/$MODEL_DIR_NAME/snapshots/$REVISION"
mkdir -p "$snapshot"
base_url="https://huggingface.co/$REPOSITORY/resolve/$REVISION"

expected_sha() {
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
    echo "shasum or sha256sum is required" >&2
    return 1
  fi
}

download_file() {
  local name="$1"
  local destination="$snapshot/$name"
  local partial="$destination.partial"
  local expected actual
  expected="$(expected_sha "$name")"
  if [ -f "$destination" ]; then
    actual="$(sha256_file "$destination")"
    if [ "$actual" = "$expected" ]; then
      echo "  [cached] $name"
      return
    fi
    echo "  [repair] $name has an unexpected checksum"
  else
    echo "  [download] $name"
  fi
  curl --fail --location --retry 5 --retry-all-errors --connect-timeout 20 \
    --output "$partial" "$base_url/$name?download=true"
  actual="$(sha256_file "$partial")"
  [ "$actual" = "$expected" ] || {
    echo "checksum mismatch for $name: expected $expected, got $actual" >&2
    return 1
  }
  mv -f "$partial" "$destination"
}

echo "==> Preparing pinned AOS local embedding model ($REVISION)"
for model_file in model_optimized.onnx tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
  download_file "$model_file"
done
echo "Pinned local embedding model is ready: $snapshot"
