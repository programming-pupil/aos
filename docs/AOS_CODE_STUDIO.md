# AOS Code Studio

AOS Code Studio is the coding workspace for AOS. It combines a Cursor/Codex-style chat workflow with a Kiro-style structured planning workflow, while keeping every code change behind Diff-first approval.

## Modes

### Vibe Mode

Vibe Mode is the default entry for `/agent`.

It is designed for fast coding work:

- Ask questions about a repository.
- Explain errors and logs.
- Request code changes in natural language.
- Watch the Agent timeline while it reads files, searches context, runs commands, and generates diffs.
- Review diffs before applying them to the main repository.
- Run tests and inspect terminal output in the same workspace.
- Continue a task in the same thread with previous diff, test, and answer context.

### Plan Mode

Plan Mode is the structured development entry for complex work. It follows:

```text
Requirement -> Spec -> Design -> Tasks -> Implementation -> Verify -> Final Report
```

Spec, Design, and Tasks are persistent backend artifacts in `rd_specs`. The user must approve each major phase before the next phase can proceed.

Implementation creates real `rd_tasks` for approved task items. Each task still uses the same RD runtime, candidate workspace, AgentOps task, trace events, and Diff-first approval path.

## Diff-First Safety

The Agent may write inside a candidate workspace, but it must not silently modify the main repository.

The normal flow is:

```text
Agent runs in candidate workspace
-> collect unified diff
-> validate paths and ownership
-> store rd_file_changes
-> user reviews diff
-> user applies all/file/selected hunks
```

This keeps Code Studio suitable for enterprise review and open-source demonstrations.

## Runtime And WatchDog Integration

Every real RD task is linked to AgentOps and can be inspected by WatchDog. The workbench API returns:

- RD task detail
- AgentOps task
- runtime session
- runtime processes
- runtime artifacts
- trace events
- RD task events
- file changes
- test runs
- linked Plan Mode spec/task item
- suggested next actions

This means WebUI, Bot-created RD tasks, and WatchDog all inspect the same execution evidence.

## Hybrid Code Intelligence

Code Studio uses a hybrid code intelligence stack:

- Persistent per-repository LSP sessions first for definition, references, hover, document symbols, workspace symbols, and diagnostics.
- Repository symbol index fallback when a language server is not installed or not ready.
- `rg` fallback for symbol token search when both LSP and the symbol index miss.

Supported P0 language server commands are:

```text
TypeScript/JavaScript: typescript-language-server --stdio
Rust: rust-analyzer
Python: pyright-langserver --stdio
Go: gopls
Java: jdtls
C/C++: clangd
```

`POST /code-intel/restart` tears down in-memory language-server processes for the repository and resets database status so the next query starts a fresh session. If a language server is missing, the UI stays usable and shows degraded status instead of failing the editor. All file paths are resolved under the repository root with safe-join checks.

## Preview Debug

Preview Debug starts a frontend dev server through Agent Runtime in an isolated preview workspace. The main repository is not modified.

The preview flow is:

```text
Start preview
-> create runtime session
-> prepare candidate preview workspace
-> run dev command
-> open proxied iframe
-> capture console/network errors
-> store events and screenshot artifacts
-> optionally send evidence to the RD Agent
```

The iframe uses a controlled AOS proxy to `127.0.0.1:{port}`. HTML, JavaScript, and CSS responses are rewritten so absolute asset paths keep working through the proxy. HTML pages receive a small capture script that sends console errors, unhandled rejections, failing `fetch`, and failing XHR events to the parent Code Studio page via `postMessage`; the parent page records those events with the normal authenticated API.

Screenshot capture is artifact-backed. The server uses the repository runtime session and Playwright CLI through `npx --yes playwright screenshot`. If Playwright or Node is unavailable, Code Studio records a structured `screenshot.failed` event instead of pretending success.

WatchDog can answer Code Studio questions such as:

- Which dev server is running?
- What console errors were captured?
- Why is LSP jump-to-definition degraded?
- Is this RD task blocked in runtime, tests, diff approval, or preview?

When a new RD task mentions preview/browser/console/network/dev-server problems, the backend automatically injects recent Preview Debug evidence for the same repository/task into the Agent prompt. The Agent still must inspect real files before changing code.

## Backend Surface

Plan Mode APIs:

```http
GET  /api/v1/rd/specs
POST /api/v1/rd/specs
GET  /api/v1/rd/specs/:id
PATCH /api/v1/rd/specs/:id
GET  /api/v1/rd/specs/:id/events
POST /api/v1/rd/specs/:id/generate-spec
POST /api/v1/rd/specs/:id/approve-spec
POST /api/v1/rd/specs/:id/generate-design
POST /api/v1/rd/specs/:id/approve-design
POST /api/v1/rd/specs/:id/generate-tasks
POST /api/v1/rd/specs/:id/approve-tasks
POST /api/v1/rd/specs/:id/implement-task
POST /api/v1/rd/specs/:id/implement-all
POST /api/v1/rd/specs/:id/final-report
```

Workbench API:

```http
GET /api/v1/rd/tasks/:id/workbench
```

Code Intelligence APIs:

```http
GET  /api/v1/rd/repositories/:id/code-intel/status
POST /api/v1/rd/repositories/:id/code-intel/query
POST /api/v1/rd/repositories/:id/code-intel/restart
```

Preview Debug APIs:

```http
POST /api/v1/rd/repositories/:id/preview-sessions
GET  /api/v1/rd/preview-sessions/:session_id
GET  /api/v1/rd/preview-sessions/:session_id/proxy/*
POST /api/v1/rd/preview-sessions/:session_id/stop
GET  /api/v1/rd/preview-sessions/:session_id/logs
POST /api/v1/rd/preview-sessions/:session_id/screenshot
POST /api/v1/rd/preview-sessions/:session_id/console-event
```

## Open-Source Extension Points

The recommended extension path is:

- Add new Code Studio prompts in `rust/crates/web-server/src/routes/rd/prompts.rs`.
- Add new RD Agent profiles or workflows instead of hardcoding behavior in route handlers.
- Add new runtime capability through the Agent Runtime layer.
- Keep UI additions under `webui/src/pages/rdStudio/`.
- Keep all user-facing strings in `zh-CN.json` and `en-US.json`.

## Quality Gate

Before release, run:

```bash
cd rust
cargo fmt --all --check
cargo check -p web-server --features bot-agents
cargo test -p aos-contract-tests

cd ../webui
npm run i18n:check
npm run typecheck
npm run code-studio:check
```
