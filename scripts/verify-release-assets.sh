#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 RELEASE_DIRECTORY VERSION" >&2
  exit 2
fi

release_dir="$1"
version="$2"

test -d "$release_dir"
cd "$release_dir"

expected=(
  "aos-offline-$version-linux-x86_64.tar.gz"
  "aos-offline-$version-linux-aarch64.tar.gz"
  "aos-offline-$version-darwin-x86_64.tar.gz"
  "aos-offline-$version-darwin-arm64.tar.gz"
  "aos-offline-$version-windows-x86_64.zip"
)

for archive in "${expected[@]}"; do
  test -f "$archive"
  checksum_file="$archive.sha256"
  test -f "$checksum_file"

  checksum_line="$(tr -d '\r' < "$checksum_file")"
  expected_hash="${checksum_line%% *}"
  expected_name="${checksum_line#*  }"
  if [[ ! "$expected_hash" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "invalid SHA-256 value in $checksum_file" >&2
    exit 1
  fi
  if [ "$expected_name" != "$archive" ]; then
    echo "checksum filename mismatch in $checksum_file: $expected_name" >&2
    exit 1
  fi

  actual_hash="$(shasum -a 256 "$archive" | awk '{print $1}')"
  normalized_hash="$(printf '%s' "$expected_hash" | tr '[:upper:]' '[:lower:]')"
  if [ "$actual_hash" != "$normalized_hash" ]; then
    echo "checksum mismatch for $archive" >&2
    exit 1
  fi
done

asset_count="$(find . -maxdepth 1 -type f \( -name 'aos-offline-*.tar.gz' -o -name 'aos-offline-*.tar.gz.sha256' -o -name 'aos-offline-*.zip' -o -name 'aos-offline-*.zip.sha256' \) | wc -l | tr -d ' ')"
if [ "$asset_count" != 10 ]; then
  echo "expected 10 release assets, found $asset_count" >&2
  exit 1
fi

for archive in "${expected[@]}"; do
  shasum -a 256 "$archive"
done > SHA256SUMS

test "$(wc -l < SHA256SUMS | tr -d ' ')" = 5

