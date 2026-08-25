#!/usr/bin/env bash
# Shared local-embedding runtime preparation for source and offline launchers.

AOS_LOCAL_EMBEDDING_REVISION="faf4aa4225822f3bc6376869cb1164e8e3feedd0"
AOS_LOCAL_EMBEDDING_MODEL_DIR="models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q"

aos_local_embedding_expected_sha() {
  case "$1" in
    model_optimized.onnx) echo "634d0f66c29dc934c8fa72b8a4fe91dd4d420a22f1d82a241058d4316e659a99" ;;
    tokenizer.json) echo "fa685fc160bbdbab64058d4fc91b60e62d207e8dc60b9af5c002c5ab946ded00" ;;
    config.json) echo "c8ec081fdad2df991bf5abbf18418fec7a5cdaa421f60ffb060a30040b8c376f" ;;
    special_tokens_map.json) echo "8c785abebea9ae3257b61681b4e6fd8365ceafde980c21970d001e834cf10835" ;;
    tokenizer_config.json) echo "0666eebf692422757e1dddf3c9fb1ded73ba3dc726c5828671fc89e45bf3609f" ;;
    *) return 1 ;;
  esac
}

aos_local_embedding_sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "shasum or sha256sum is required to verify the bundled model" >&2
    return 1
  fi
}

# Usage: aos_prepare_local_embedding_runtime ROOT_DIR SOURCE_LAYOUT [BACKEND_BIN]
# SOURCE_LAYOUT is 1 for a git checkout and 0 for an extracted offline package.
aos_prepare_local_embedding_runtime() {
  local root_dir="$1"
  local source_layout="$2"
  local backend_bin="${3:-}"
  local ort_library=""
  local ort_runtime_dir=""
  local model_snapshot=""
  local model_complete="1"
  local model_file=""
  local actual_model_sha=""
  local expected_sha=""

  case "$(uname -s)" in
    Darwin) ort_library="$root_dir/runtime/onnxruntime/lib/libonnxruntime.dylib" ;;
    Linux) ort_library="$root_dir/runtime/onnxruntime/lib/libonnxruntime.so" ;;
  esac
  if [ -z "$ort_library" ] || [ ! -f "$ort_library" ]; then
    if [ "$source_layout" = "1" ]; then
      ort_runtime_dir="$root_dir/.aos-runtime/onnxruntime"
      "$root_dir/scripts/setup-onnxruntime.sh" --dir "$ort_runtime_dir"
      case "$(uname -s)" in
        Darwin) ort_library="$ort_runtime_dir/lib/libonnxruntime.dylib" ;;
        Linux) ort_library="$ort_runtime_dir/lib/libonnxruntime.so" ;;
      esac
    else
      echo "AOS Offline package is incomplete: bundled ONNX Runtime is missing" >&2
      echo "Runtime downloads are disabled. Re-extract a complete AOS Offline archive." >&2
      return 1
    fi
  fi
  export ORT_DYLIB_PATH="${ORT_DYLIB_PATH:-$ort_library}"

  if [ "$source_layout" = "0" ]; then
    export AOS_LOCAL_EMBEDDING_CACHE_DIR="${AOS_LOCAL_EMBEDDING_CACHE_DIR:-$root_dir/models/fastembed}"
  else
    export AOS_LOCAL_EMBEDDING_CACHE_DIR="${AOS_LOCAL_EMBEDDING_CACHE_DIR:-$root_dir/.aos-runtime/models/fastembed}"
  fi
  model_snapshot="$AOS_LOCAL_EMBEDDING_CACHE_DIR/$AOS_LOCAL_EMBEDDING_MODEL_DIR/snapshots/$AOS_LOCAL_EMBEDDING_REVISION"
  for model_file in model_optimized.onnx tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
    if [ ! -f "$model_snapshot/$model_file" ]; then
      model_complete="0"
    fi
  done
  if [ "$model_complete" != "1" ]; then
    if [ "$source_layout" = "1" ]; then
      echo "==> Downloading the pinned local embedding model for this source checkout"
      "$root_dir/scripts/download-local-embedding.sh" --dir "$AOS_LOCAL_EMBEDDING_CACHE_DIR"
      if [ -n "$backend_bin" ] && [ -x "$backend_bin" ]; then
        "$backend_bin" --warm-local-embedding "$AOS_LOCAL_EMBEDDING_CACHE_DIR"
      fi
    else
      echo "AOS Offline package is incomplete: bundled local embedding files are missing" >&2
      echo "Runtime downloads are disabled. Re-extract a complete AOS Offline archive." >&2
      return 1
    fi
  fi
  for model_file in model_optimized.onnx tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
    actual_model_sha="$(aos_local_embedding_sha256_file "$model_snapshot/$model_file")"
    expected_sha="$(aos_local_embedding_expected_sha "$model_file")"
    if [ "$actual_model_sha" != "$expected_sha" ]; then
      echo "AOS local embedding checksum mismatch for $model_file; refusing to start" >&2
      return 1
    fi
  done
}
