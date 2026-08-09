# Parity Status — aos vs. claw-code

Last updated: 2026-06-05

## What this document tracks

aos is a web-first derivative of [claw-code](https://github.com/ultraworkers/claw-code).
We replaced the upstream CLI front-end (`rusty-claude-cli`) with a web stack
(`web-server` + `agent-gateway` + `webui`), but we deliberately **reuse the
upstream coding engine unchanged where possible**: the `runtime`, `tools`, and
`api` crates. The goal is that code written through aos is indistinguishable in
quality from code written through claude-code / Cursor / Codex, because the
prompt, tool catalog, and agent loop are the same.

This file tracks how closely the aos coding engine matches upstream. It is about
**programming capability**, not UI/UX. UX divergence is expected and intentional.

## Upstream baseline

- Reference clone: `.aos-upstream-audit/` (read-only audit copy)
- Audited upstream commit: `eaa2e32` (2026-06-05)
- Re-audit cadence: whenever we pull upstream, diff the core crates below and
  update this table.

## Architecture: shared engine, swapped front-end

```
webui (React)
  → web-server (axum routes/agent/*)            [aos-specific]
    → agent-gateway::AgentSessionManager         [aos-specific bridge]
      → runtime::ConversationRuntime             [SHARED with upstream]
        → tools::execute_tool                    [SHARED with upstream]
          → runtime::{file_ops, bash, ...}       [SHARED with upstream]
        → api::providers::{anthropic, openai_compat} [SHARED with upstream]
```

The agent loop, system prompt, tool schemas, and provider adapters are the parts
that determine code quality. They live in the shared crates. The gateway and
web-server are thin orchestration layers around them.

## Core engine crate parity

| Crate / file | Status | Notes |
|--------------|--------|-------|
| `api/src/providers/anthropic.rs` | ✅ identical | Anthropic Messages API |
| `api/src/providers/openai_compat.rs` | ✅ identical | OpenAI-compatible providers (enables Cursor/Codex-style model switching) |
| `runtime/src/conversation.rs` (agent loop) | ✅ ahead | aos converted `run_turn` to `async` + added `RuntimeEventReporter` streaming (thinking/tool/text deltas) and `restore_session` for the web multi-session case. Core user→assistant→tool→tool_result loop preserved. |
| `runtime/src/prompt.rs` (system prompt) | ✅ ahead | aos: `FRONTIER_MODEL_NAME = Claude Opus 4.8` (upstream 4.6), added `ModelFamilyIdentity::from_model`, rebranded "Claude instructions" → "Project instructions". Prompt body governing code style/verification preserved. |
| `runtime/src/file_ops.rs` (read/write/edit/glob/grep) | ✅ aligned | Workspace-scoped search ported from upstream (see below). |
| `runtime/src/bash.rs` | ✅ aligned | async conversion; dropped CLI-only "ship prepared" git telemetry helpers (not a coding-capability feature). |
| `tools/src/lib.rs` (tool catalog) | ✅ 40/40 | Tool-name set is byte-for-byte identical to upstream. |

## Tool surface: 40/40

The set of tool names exposed to the model is identical to upstream. Verified by
extracting every `ToolSpec { name: ... }` from both `tools/src/lib.rs` files and
diffing — empty symmetric difference.

Built-ins: `bash`, `read_file`, `write_file`, `edit_file`, `glob_search`,
`grep_search`, `WebFetch`, `WebSearch`, `TodoWrite`, `Skill`, `Agent`,
`ToolSearch`, `NotebookEdit`, `Sleep`, `SendUserMessage`, `Config`,
`EnterPlanMode`, `ExitPlanMode`, `StructuredOutput`, `REPL`, `PowerShell`,
`AskUserQuestion`, `Task*` (Create/Get/List/Stop/Update/Output), `RunTaskPacket`,
`Worker*`, `Team*`, `Cron*`, `LSP`, `ListMcpResources`, `ReadMcpResource`,
`McpAuth`, `RemoteTrigger`, `MCP`.

### Stubs inherited from upstream (surface parity, limited behavior)

These are stubs in upstream too; they are not aos regressions. Prioritize for the
web context as needed:

| Tool | Status | aos consideration |
|------|--------|-------------------|
| `AskUserQuestion` | stub (stdin-based) | Needs web round-trip via SSE + a UI prompt to be usable in the browser. |
| `McpAuth` | stub | Needs OAuth UX surfaced through the web flow. |
| `RemoteTrigger` | stub | Needs HTTP client wiring. |
| `TestingPermission` | stub | Test-only, low priority. |

## Web-path hardening (aos-specific, beyond upstream CLI)

The CLI runs as the local user in a single workspace, so plain `glob_search` /
`grep_search` (process-cwd scoped) are acceptable there. The aos web path is
multi-tenant: each session has its own workspace and must not read across the
boundary. We therefore enforce a stricter contract on the web path.

| Item | Status | Notes |
|------|--------|-------|
| `read_file_in_workspace` / `write_file_in_workspace` / `edit_file_in_workspace` | ✅ | Canonicalize + boundary check before IO. |
| `glob_search_in_workspace` | ✅ ported | `WalkDir`-based, skips heavy dirs (`.git`, `node_modules`, `target`, `dist`, `build`, `coverage`), canonicalizes every matched file against the workspace root. |
| `grep_search_in_workspace` | ✅ ported | Canonicalizes the search base and every scanned file before reading; rejects symlink/`..` escapes. |
| Gateway routing | ✅ wired | `agent-gateway` routes `glob_search`/`grep_search` through the `*_in_workspace` variants (`GatewayToolExecutor::run_builtin_tool`), so the boundary is enforced on every web tool call, not just dead-code helpers. |
| Regression tests | ✅ | `runtime::file_ops` (5 new tests) + `agent-gateway` (`workspace_grep_search_rejects_symlink_escape`, `workspace_glob_search_finds_files_inside_workspace`). |

## Known gaps / follow-ups

- [ ] Web-usable `AskUserQuestion` (SSE round-trip instead of stdin).
- [ ] `McpAuth` OAuth flow through the web UI.
- [ ] `RemoteTrigger` HTTP client.
- [ ] Re-audit against upstream `eaa2e32+` on the next pull and refresh this table.
- [ ] Confirm output-truncation and token/cost-accounting parity for the web path
      (upstream lists these as open behavioral items too).

## Verification

```bash
cd rust
cargo check -p runtime
cargo test  -p runtime --lib file_ops
cargo test  -p agent-gateway --lib runtime_builder::tests
```
