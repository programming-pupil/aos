#!/usr/bin/env bash
# Check or install the native AOS build/runtime toolchain on macOS and Linux.

set -euo pipefail

MODE="check"

usage() {
  cat <<'USAGE'
Usage: ./scripts/setup-environment.sh [--check|--install]

Options:
  --check     Only report missing or incompatible tools (default).
  --install   Install missing tools with Homebrew, apt, or dnf, then re-check.
  -h, --help  Show this help text.

The script detects a source checkout versus a prebuilt release package. Source
checkouts require the Rust/C build toolchain; release packages only require the
runtime tools used by AOS, Skills, repositories, and MCP servers.

Install mode uses official package managers plus the official rustup and uv
installers. It may ask for sudo on Linux. It never writes API keys.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) MODE="check" ;;
    --install) MODE="install" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SOURCE_LAYOUT="0"
if [ -f "$ROOT_DIR/rust/Cargo.toml" ] && [ -f "$ROOT_DIR/webui/package.json" ]; then
  SOURCE_LAYOUT="1"
fi
user_home="${HOME:-}"
if [ -n "$user_home" ]; then
  export PATH="$user_home/.cargo/bin:$user_home/.local/bin:$PATH"
fi

version_at_least() {
  local actual="$1"
  local required="$2"
  awk -v actual="$actual" -v required="$required" 'BEGIN {
    split(actual, a, "."); split(required, r, ".");
    for (i = 1; i <= 3; i++) {
      av = (a[i] == "" ? 0 : a[i]) + 0;
      rv = (r[i] == "" ? 0 : r[i]) + 0;
      if (av > rv) exit 0;
      if (av < rv) exit 1;
    }
    exit 0;
  }'
}

node_supported() {
  local version="$1"
  local major minor
  major="${version%%.*}"
  minor="${version#*.}"
  minor="${minor%%.*}"
  if [ "$major" -eq 20 ]; then
    [ "$minor" -ge 19 ]
  elif [ "$major" -eq 21 ]; then
    return 1
  else
    [ "$major" -ge 22 ]
  fi
}

failures=0

ok() { printf '  [ok]      %s\n' "$1"; }
missing() { printf '  [missing] %s\n' "$1"; failures=$((failures + 1)); }
invalid() { printf '  [version] %s\n' "$1"; failures=$((failures + 1)); }

check_command() {
  local command_name="$1"
  local description="$2"
  if command -v "$command_name" >/dev/null 2>&1; then
    ok "$description ($(command -v "$command_name"))"
  else
    missing "$description"
  fi
}

check_environment() {
  failures=0
  echo "==> AOS environment check"
  if [ "$SOURCE_LAYOUT" = "1" ]; then
    echo "  Layout: source checkout (build + runtime tools)"
  else
    echo "  Layout: prebuilt release package (runtime tools)"
  fi

  check_command git "Git"
  check_command curl "curl"
  check_command openssl "OpenSSL"
  check_command rg "ripgrep (rg)"

  if [ "$SOURCE_LAYOUT" = "1" ]; then
    if command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
      local rust_version
      rust_version="$(rustc --version | awk '{print $2}')"
      if version_at_least "$rust_version" "1.85.0"; then
        ok "Rust $rust_version with Cargo"
      else
        invalid "Rust $rust_version found; 1.85.0+ is required"
      fi
    else
      missing "Rust 1.85.0+ and Cargo"
    fi
  fi

  if command -v node >/dev/null 2>&1; then
    local node_version
    node_version="$(node --version | sed 's/^v//')"
    if node_supported "$node_version"; then
      ok "Node.js $node_version"
    else
      invalid "Node.js $node_version found; use 20.19+ or 22.12+"
    fi
  else
    missing "Node.js 20.19+ or 22.12+"
  fi
  check_command npm "npm"
  check_command npx "npx (required by npm-based MCP servers)"

  if command -v python3 >/dev/null 2>&1; then
    local python_version
    python_version="$(python3 -c 'import platform; print(platform.python_version())')"
    if version_at_least "$python_version" "3.9.0"; then
      ok "Python $python_version (AOS helper scripts use only the standard library)"
    else
      invalid "Python $python_version found; 3.9.0+ is required"
    fi
  else
    missing "Python 3.9.0+"
  fi
  check_command uv "uv"
  check_command uvx "uvx (required by Python-based MCP servers)"

  if [ "$SOURCE_LAYOUT" = "1" ] && [ "$(uname -s)" = "Linux" ]; then
    check_command cc "C/C++ build toolchain"
    check_command pkg-config "pkg-config"
  fi

  local token_present="false"
  if [ -n "${AOSD_GITHUB_TOKEN:-}" ]; then
    token_present="true"
  elif [ -f "$ROOT_DIR/.env" ] && awk -F= '$1 == "AOSD_GITHUB_TOKEN" { sub(/^[^=]*=/, ""); gsub(/^\"|\"$/, ""); if (length($0) > 0) found=1 } END { exit(found ? 0 : 1) }' "$ROOT_DIR/.env"; then
    token_present="true"
  fi
  if [ "$token_present" = "true" ]; then
    ok "GitHub token configured for Skill repository operations"
  else
    printf '  [optional] AOSD_GITHUB_TOKEN is empty; public GitHub rate limits may slow Skill scans\n'
  fi

  if [ "$failures" -eq 0 ]; then
    echo "Environment is ready."
    return 0
  fi
  echo "$failures required check(s) failed." >&2
  return 1
}

run_as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    echo "sudo is required to install system packages" >&2
    return 1
  fi
}

install_rust() {
  if ! command -v rustc >/dev/null 2>&1 || ! version_at_least "$(rustc --version | awk '{print $2}')" "1.85.0"; then
    echo "==> Installing current stable Rust with rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  fi
}

install_uv() {
  if ! command -v uv >/dev/null 2>&1 || ! command -v uvx >/dev/null 2>&1; then
    echo "==> Installing uv and uvx"
    curl -LsSf https://astral.sh/uv/install.sh | sh
  fi
}

install_macos() {
  if ! command -v brew >/dev/null 2>&1; then
    echo "Homebrew is required for automatic macOS setup." >&2
    echo "Install it from https://brew.sh and rerun this command." >&2
    return 1
  fi
  echo "==> Installing macOS packages"
  brew install git node python uv ripgrep openssl
  if [ "$SOURCE_LAYOUT" = "1" ]; then
    brew install pkg-config rustup-init
    if ! command -v rustc >/dev/null 2>&1; then
      rustup-init -y --no-modify-path
    fi
  fi
}

install_linux() {
  if command -v apt-get >/dev/null 2>&1; then
    echo "==> Installing Debian/Ubuntu AOS packages"
    run_as_root apt-get update
    run_as_root apt-get install -y ca-certificates curl git openssl ripgrep python3 python3-venv
    if [ "$SOURCE_LAYOUT" = "1" ]; then
      run_as_root apt-get install -y build-essential pkg-config libssl-dev xz-utils
    fi
    if ! command -v node >/dev/null 2>&1 || ! node_supported "$(node --version | sed 's/^v//')"; then
      echo "==> Installing Node.js 22 from NodeSource"
      node_setup_script="$(mktemp "${TMPDIR:-/tmp}/aos-nodesource.XXXXXX")"
      curl -fsSL https://deb.nodesource.com/setup_22.x -o "$node_setup_script"
      run_as_root bash "$node_setup_script"
      rm -f "$node_setup_script"
      run_as_root apt-get install -y nodejs
    fi
  elif command -v dnf >/dev/null 2>&1; then
    echo "==> Installing Fedora/RHEL AOS packages"
    run_as_root dnf install -y ca-certificates curl git openssl ripgrep python3
    if [ "$SOURCE_LAYOUT" = "1" ]; then
      run_as_root dnf install -y gcc gcc-c++ make pkgconf-pkg-config openssl-devel xz
    fi
    if ! command -v node >/dev/null 2>&1 || ! node_supported "$(node --version | sed 's/^v//')"; then
      echo "==> Installing Node.js 22 from NodeSource"
      node_setup_script="$(mktemp "${TMPDIR:-/tmp}/aos-nodesource-rpm.XXXXXX")"
      curl -fsSL https://rpm.nodesource.com/setup_22.x -o "$node_setup_script"
      run_as_root bash "$node_setup_script"
      rm -f "$node_setup_script"
      run_as_root dnf install -y nodejs
    fi
  else
    echo "unsupported Linux package manager; install the tools shown by --check" >&2
    return 1
  fi
  if [ "$SOURCE_LAYOUT" = "1" ]; then
    install_rust
  fi
  install_uv
}

if [ "$MODE" = "check" ]; then
  check_environment
  exit $?
fi

case "$(uname -s)" in
  Darwin) install_macos ;;
  Linux) install_linux ;;
  *) echo "automatic installation supports macOS and Linux only" >&2; exit 1 ;;
esac

hash -r
check_environment
