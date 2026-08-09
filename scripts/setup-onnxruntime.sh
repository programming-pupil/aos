#!/usr/bin/env bash
# Install the exact ONNX Runtime used by AOS local embeddings.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
INSTALL_DIR="$ROOT_DIR/.aos-runtime/onnxruntime"
ORT_VERSION="1.23.2"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) [ "$#" -ge 2 ] || { echo "--dir needs a path" >&2; exit 2; }; INSTALL_DIR="$2"; shift ;;
    -h|--help) echo "Usage: ./scripts/setup-onnxruntime.sh [--dir PATH]"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

case "$(uname -s)-$(uname -m)" in
  Darwin-x86_64) package_platform="osx-x86_64"; library_name="libonnxruntime.dylib"; archive_sha="d10359e16347b57d9959f7e80a225a5b4a66ed7d7e007274a15cae86836485a6" ;;
  Darwin-arm64) package_platform="osx-arm64"; library_name="libonnxruntime.dylib"; archive_sha="b4d513ab2b26f088c66891dbbc1408166708773d7cc4163de7bdca0e9bbb7856" ;;
  Linux-x86_64) package_platform="linux-x64"; library_name="libonnxruntime.so"; archive_sha="1fa4dcaef22f6f7d5cd81b28c2800414350c10116f5fdd46a2160082551c5f9b" ;;
  Linux-aarch64|Linux-arm64) package_platform="linux-aarch64"; library_name="libonnxruntime.so"; archive_sha="7c63c73560ed76b1fac6cff8204ffe34fe180e70d6582b5332ec094810241e5c" ;;
  *) echo "AOS local embeddings do not yet support $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

library_path="$INSTALL_DIR/lib/$library_name"
version_path="$INSTALL_DIR/VERSION_NUMBER"
installed_version=""
if [ -f "$version_path" ]; then
  installed_version="$(tr -d '\r\n[:space:]' < "$version_path")"
fi
if [ -f "$library_path" ] && [ "$installed_version" = "$ORT_VERSION" ]; then
  echo "ONNX Runtime is ready: $library_path"
  exit 0
fi
if [ -f "$library_path" ]; then
  echo "==> Replacing ONNX Runtime ${installed_version:-unknown} with $ORT_VERSION"
fi

temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/aos-onnxruntime.XXXXXX")"
trap 'rm -rf -- "$temporary_root"' EXIT
archive="$temporary_root/onnxruntime.tgz"
url="https://github.com/microsoft/onnxruntime/releases/download/v$ORT_VERSION/onnxruntime-$package_platform-$ORT_VERSION.tgz"

echo "==> Downloading ONNX Runtime $ORT_VERSION for $package_platform"
curl --fail --location --retry 3 --connect-timeout 20 "$url" --output "$archive"
if command -v shasum >/dev/null 2>&1; then
  actual_archive_sha="$(shasum -a 256 "$archive" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  actual_archive_sha="$(sha256sum "$archive" | awk '{print $1}')"
else
  echo "shasum or sha256sum is required to verify ONNX Runtime" >&2
  exit 1
fi
[ "$actual_archive_sha" = "$archive_sha" ] || {
  echo "ONNX Runtime archive checksum mismatch for $package_platform" >&2
  exit 1
}
tar -xzf "$archive" -C "$temporary_root"
extracted_dir="$temporary_root/onnxruntime-$package_platform-$ORT_VERSION"
[ -f "$extracted_dir/lib/$library_name" ] || {
  echo "ONNX Runtime archive did not contain $library_name" >&2
  exit 1
}
mkdir -p "$INSTALL_DIR"
mkdir -p "$INSTALL_DIR/lib"
cp -L "$extracted_dir/lib/$library_name" "$INSTALL_DIR/lib/$library_name"
chmod 755 "$INSTALL_DIR/lib/$library_name"
for license_file in LICENSE README.md ThirdPartyNotices.txt VERSION_NUMBER; do
  if [ -f "$extracted_dir/$license_file" ]; then
    cp "$extracted_dir/$license_file" "$INSTALL_DIR/$license_file"
  fi
done
[ -f "$library_path" ] || { echo "ONNX Runtime installation failed" >&2; exit 1; }
installed_version="$(tr -d '\r\n[:space:]' < "$version_path")"
[ "$installed_version" = "$ORT_VERSION" ] || {
  echo "ONNX Runtime installation reported version $installed_version; expected $ORT_VERSION" >&2
  exit 1
}
echo "ONNX Runtime is ready: $library_path"
