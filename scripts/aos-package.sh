#!/usr/bin/env bash
# Build a self-contained, same-platform AOS release archive.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
OUTPUT_DIR="$ROOT_DIR/dist"
SKIP_BUILD="0"

usage() {
  cat <<'USAGE'
Usage: ./scripts/aos-package.sh [--skip-build] [--output-dir PATH]

Builds a release backend and WebUI, then creates a same-OS/same-architecture
tar.gz package that contains no database, runtime history, .env, or credentials.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD="1" ;;
    --output-dir) [ "$#" -ge 2 ] || { echo "--output-dir needs a path" >&2; exit 2; }; OUTPUT_DIR="$2"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

if [ "$SKIP_BUILD" != "1" ]; then
  "$ROOT_DIR/install.sh" --release
fi

BACKEND_BIN="$ROOT_DIR/rust/target/release/web-server"
WEB_DIR="$ROOT_DIR/webui/dist"
[ -x "$BACKEND_BIN" ] || { echo "missing release backend: $BACKEND_BIN" >&2; exit 1; }
[ -f "$WEB_DIR/index.html" ] || { echo "missing built WebUI: $WEB_DIR/index.html" >&2; exit 1; }

version="$(awk -F'"' '/^version =/ { print $2; exit }' "$ROOT_DIR/rust/Cargo.toml")"
version="${version:-0.1.0}"
os_name="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch_name="$(uname -m)"
package_name="aos-offline-$version-$os_name-$arch_name"
temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/aos-package.XXXXXX")"
stage="$temporary_root/$package_name"
trap 'rm -rf -- "$temporary_root"' EXIT

mkdir -p "$stage/bin" "$stage/web" "$stage/scripts" "$stage/docs/evals" "$stage/licenses" "$stage/models/fastembed"
mkdir -p "$stage/docs/assets"
cp "$BACKEND_BIN" "$stage/bin/web-server"
cp -R "$WEB_DIR"/. "$stage/web/"
cp "$ROOT_DIR/.env.example" "$stage/.env.example"
cp "$ROOT_DIR/README.md" "$ROOT_DIR/README.zh-CN.md" "$ROOT_DIR/LICENSE" "$ROOT_DIR/NOTICE.md" "$stage/"
cp "$ROOT_DIR"/licenses/*.txt "$stage/licenses/"
cp "$ROOT_DIR/docs/INSTALL.md" \
  "$ROOT_DIR/docs/OPEN_SOURCE_DEPLOYMENT.zh-CN.md" \
  "$ROOT_DIR/docs/AOS_ENGINEERING_DESIGN_CENTER.zh-CN.md" \
  "$ROOT_DIR/docs/OPEN_SOURCE_TEST_GUIDE.zh-CN.md" \
  "$stage/docs/"
cp "$ROOT_DIR/docs/evals/BOT_GATEWAY_COMPLETE_TEST_GUIDE.zh-CN.md" \
  "$ROOT_DIR/docs/evals/DATA_ATTRIBUTION_COMPLETE_TEST_GUIDE.zh-CN.md" \
  "$ROOT_DIR/docs/evals/NL2SQL_ADVANCED_CONFIGURATION_TEST_GUIDE.zh-CN.md" \
  "$stage/docs/evals/"
cp "$ROOT_DIR/docs/assets/aos-hero.svg" \
  "$ROOT_DIR/docs/assets/aos-menu-map.svg" \
  "$stage/docs/assets/"
cp "$ROOT_DIR/scripts/generate-env.sh" \
  "$ROOT_DIR/scripts/setup-environment.sh" \
  "$ROOT_DIR/scripts/aos-start.sh" \
  "$ROOT_DIR/scripts/aos-stop.sh" \
  "$ROOT_DIR/scripts/aos-upgrade.sh" \
  "$ROOT_DIR/scripts/reset-local-data.sh" \
  "$ROOT_DIR/scripts/setup-onnxruntime.sh" \
  "$stage/scripts/"
chmod +x "$stage/bin/web-server" "$stage/scripts/"*.sh

echo "==> Bundling the built-in multilingual embedding model"
onnxruntime_cache="${AOS_ONNXRUNTIME_CACHE_DIR:-$ROOT_DIR/.aos-runtime/onnxruntime-package}"
"$ROOT_DIR/scripts/setup-onnxruntime.sh" --dir "$onnxruntime_cache"
mkdir -p "$stage/runtime/onnxruntime"
cp -R "$onnxruntime_cache"/. "$stage/runtime/onnxruntime/"
source_model_cache="${AOS_LOCAL_EMBEDDING_CACHE_DIR:-$ROOT_DIR/.aos-runtime/models/fastembed}"
"$ROOT_DIR/scripts/download-local-embedding.sh" --dir "$source_model_cache"
cp -R "$source_model_cache"/. "$stage/models/fastembed/"
case "$(uname -s)" in
  Darwin) ORT_DYLIB_PATH="$stage/runtime/onnxruntime/lib/libonnxruntime.dylib" ;;
  Linux) ORT_DYLIB_PATH="$stage/runtime/onnxruntime/lib/libonnxruntime.so" ;;
esac
export ORT_DYLIB_PATH
"$BACKEND_BIN" --warm-local-embedding "$stage/models/fastembed"

model_snapshot="$stage/models/fastembed/models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q/snapshots/faf4aa4225822f3bc6376869cb1164e8e3feedd0"
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
for model_file in model_optimized.onnx tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
  [ -f "$model_snapshot/$model_file" ] || {
    echo "package validation failed: pinned local embedding file is missing: $model_file" >&2
    exit 1
  }
  actual_model_sha="$(sha256_file "$model_snapshot/$model_file")"
  expected_sha="$(expected_model_sha "$model_file")"
  [ "$actual_model_sha" = "$expected_sha" ] || {
    echo "package validation failed: local embedding checksum mismatch for $model_file" >&2
    exit 1
  }
done

echo "==> Creating release integrity manifest"
(
  cd "$stage"
  find bin web runtime models scripts docs licenses \
    .env.example README.md README.zh-CN.md LICENSE NOTICE.md -type f -print \
    | LC_ALL=C sort \
    | while IFS= read -r relative; do
        printf '%s  %s\n' "$(sha256_file "$relative")" "$relative"
      done > RELEASE-MANIFEST.sha256
)

mkdir -p "$OUTPUT_DIR"
archive="$OUTPUT_DIR/$package_name.tar.gz"
tar -C "$temporary_root" -czf "$archive" "$package_name"

archive_listing="$temporary_root/archive-contents.txt"
tar -tzf "$archive" > "$archive_listing"

if rg -q '(^|/)([.]env$|aos[.]db|aos[.]db-wal|aos[.]db-shm|[.]claw/|[.]run/)' "$archive_listing"; then
  echo "package validation failed: runtime data or secrets were included" >&2
  exit 1
fi

if ! rg -q '/models/fastembed/.+' "$archive_listing"; then
  echo "package validation failed: built-in embedding model was not included" >&2
  exit 1
fi
if ! rg -q '/runtime/onnxruntime/lib/libonnxruntime[.]' "$archive_listing"; then
  echo "package validation failed: ONNX Runtime was not included" >&2
  exit 1
fi

echo "AOS release package created: $archive"
echo "After extraction: ./$package_name/scripts/aos-start.sh"
