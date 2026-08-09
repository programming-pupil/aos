#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
UPSTREAM_DIR="${1:-${CLAW_CODE_UPSTREAM_DIR:-/tmp/aos-claw-code-audit}}"

if [[ ! -d "$UPSTREAM_DIR/rust/crates" ]]; then
  cat >&2 <<MSG
upstream claw-code checkout not found: $UPSTREAM_DIR

Usage:
  CLAW_CODE_UPSTREAM_DIR=/path/to/claw-code scripts/check-claw-code-parity.sh
  scripts/check-claw-code-parity.sh /path/to/claw-code
MSG
  exit 2
fi

python3 - "$UPSTREAM_DIR" "$ROOT_DIR" <<'PY'
import re
import sys
from pathlib import Path

upstream = Path(sys.argv[1])
root = Path(sys.argv[2])


def tool_names(path: Path) -> set[str]:
    text = path.read_text(encoding="utf-8")
    return set(re.findall(r'ToolSpec\s*\{[^{}]*?name:\s*"([^"]+)"', text, flags=re.S))


def rel_rs_modules(path: Path) -> set[str]:
    src = path / "rust" / "crates" / "runtime" / "src"
    return {p.name for p in src.glob("*.rs")}

up_tools = tool_names(upstream / "rust" / "crates" / "tools" / "src" / "lib.rs")
local_tools = tool_names(root / "rust" / "crates" / "tools" / "src" / "lib.rs")

print("== Tool surface ==")
print(f"upstream tools: {len(up_tools)}")
print(f"local tools:    {len(local_tools)}")
missing_tools = sorted(up_tools - local_tools)
extra_tools = sorted(local_tools - up_tools)
if missing_tools:
    print("missing local tools:")
    for name in missing_tools:
        print(f"  - {name}")
if extra_tools:
    print("extra local tools:")
    for name in extra_tools:
        print(f"  - {name}")
if not missing_tools and not extra_tools:
    print("tool names match")

print("\n== Runtime top-level modules ==")
up_modules = rel_rs_modules(upstream)
local_modules = rel_rs_modules(root)
module_equivalents = {
    "mcp.rs": [
        "rust/crates/runtime/src/mcp/mod.rs",
        "rust/crates/runtime/src/mcp/utils.rs",
        "rust/crates/runtime/src/mcp/session.rs",
        "rust/crates/runtime/src/mcp/http_stream_transport.rs",
        "rust/crates/runtime/src/mcp/sse_transport.rs",
        "rust/crates/runtime/src/mcp/stdio_transport.rs",
    ],
}
covered_modules = {
    module
    for module, paths in module_equivalents.items()
    if all((root / path).exists() for path in paths)
}
missing_modules = sorted((up_modules - local_modules) - covered_modules)
extra_modules = sorted(local_modules - up_modules)
if covered_modules:
    print("covered by local modular runtime implementation:")
    for name in sorted(covered_modules):
        print(f"  - {name}")
if missing_modules:
    print("missing local runtime modules:")
    for name in missing_modules:
        print(f"  - {name}")
else:
    print("no missing upstream runtime modules")
if extra_modules:
    print("extra local runtime modules:")
    for name in extra_modules:
        print(f"  - {name}")

print("\n== Runtime module audit decisions ==")
module_decisions = {
    "approval_tokens.rs": "audit/enterprise safety: useful for delegated approvals; not required for current diff-first coding path",
    "g004_conformance.rs": "audit/report contract: useful for machine-checkable governance; not required for model coding loop",
    "report_schema.rs": "audit/report schema: useful for shareable structured reports; not required for model coding loop",
    "trident.rs": "coding-context quality: ported and integrated into runtime auto compaction",
}
for module, note in module_decisions.items():
    status = "present" if module in local_modules else "missing"
    print(f"{module}: {status} — {note}")

print("\n== Required local parity hooks ==")
checks = {
    "gateway_permission_policy": root / "rust" / "crates" / "agent-gateway" / "src" / "runtime_builder.rs",
    "rd_validate_diff": root / "rust" / "crates" / "agent-gateway" / "src" / "runtime_builder.rs",
    "create_internal_session_in_workspace": root / "rust" / "crates" / "agent-gateway" / "src" / "session_manager.rs",
    "append_internal_context_message": root / "rust" / "crates" / "agent-gateway" / "src" / "session_manager.rs",
    "rd_thread_source_is_hidden_persistent_runtime": root / "rust" / "crates" / "agent-gateway" / "src" / "session_manager.rs",
    "run_rd_candidate_worktree_completion": root / "rust" / "crates" / "web-server" / "src" / "routes" / "rd.rs",
    "candidate_worktree_extracts_real_git_diff_end_to_end": root / "rust" / "crates" / "web-server" / "src" / "routes" / "rd.rs",
    "candidate_context_message_preserves_pending_diff_semantics": root / "rust" / "crates" / "web-server" / "src" / "routes" / "rd.rs",
    "direct_fallback_is_disabled_by_default_and_requires_explicit_opt_in": root / "rust" / "crates" / "web-server" / "src" / "routes" / "rd.rs",
    "rd_runtime_direct_fallback_enabled": root / "rust" / "crates" / "web-server" / "src" / "routes" / "rd.rs",
    "trident_compact_session": root / "rust" / "crates" / "runtime" / "src" / "trident.rs",
    "AOS_RUNTIME_TRIDENT_COMPACTION": root / "rust" / "crates" / "runtime" / "src" / "conversation.rs",
}
failed = False
for needle, path in checks.items():
    text = path.read_text(encoding="utf-8")
    ok = needle in text
    print(f"{needle}: {'ok' if ok else 'missing'}")
    failed = failed or not ok

if missing_tools or failed:
    sys.exit(1)
PY

if [[ "${AOS_PARITY_RUN_CHECKS:-0}" == "1" ]]; then
  echo
  echo "== Optional parity smoke checks =="
  (
    cd "$ROOT_DIR/rust"
    cargo test -p runtime trident
    cargo test -p agent-gateway rd_thread_source_is_hidden_persistent_runtime
    cargo check -p web-server --tests
    if [[ "${AOS_PARITY_RUN_E2E:-0}" == "1" ]]; then
      cargo test -p web-server rd::tests -- --nocapture
    fi
  )
else
  cat <<'MSG'

== Optional parity smoke checks skipped ==
Set AOS_PARITY_RUN_CHECKS=1 to run:
  cargo test -p runtime trident
  cargo test -p agent-gateway rd_thread_source_is_hidden_persistent_runtime
  cargo check -p web-server --tests
Set AOS_PARITY_RUN_CHECKS=1 AOS_PARITY_RUN_E2E=1 to additionally run:
  cargo test -p web-server rd::tests -- --nocapture
MSG
fi
