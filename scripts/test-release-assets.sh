#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/aos-release-assets.XXXXXX")"
trap 'rm -rf "$fixture_dir"' EXIT

version="0.0.0-test"
archives=(
  "aos-offline-$version-linux-x86_64.tar.gz"
  "aos-offline-$version-linux-aarch64.tar.gz"
  "aos-offline-$version-darwin-x86_64.tar.gz"
  "aos-offline-$version-darwin-arm64.tar.gz"
  "aos-offline-$version-windows-x86_64.zip"
)

for archive in "${archives[@]}"; do
  printf 'fixture:%s\n' "$archive" > "$fixture_dir/$archive"
  hash="$(shasum -a 256 "$fixture_dir/$archive" | awk '{print $1}')"
  if [[ "$archive" == *.zip ]]; then
    printf '%s  %s\r\n' "$hash" "$archive" > "$fixture_dir/$archive.sha256"
  else
    printf '%s  %s\n' "$hash" "$archive" > "$fixture_dir/$archive.sha256"
  fi
done

"$root_dir/scripts/verify-release-assets.sh" "$fixture_dir" "$version"
(cd "$fixture_dir" && shasum -a 256 -c SHA256SUMS)

windows_archive="aos-offline-$version-windows-x86_64.zip"
windows_hash="$(shasum -a 256 "$fixture_dir/$windows_archive" | awk '{print $1}')"
printf '%s  wrong-name.zip\r\n' "$windows_hash" > "$fixture_dir/$windows_archive.sha256"
if "$root_dir/scripts/verify-release-assets.sh" "$fixture_dir" "$version" >/dev/null 2>&1; then
  echo "release verifier accepted a mismatched checksum filename" >&2
  exit 1
fi

echo "release asset verifier checks passed"
