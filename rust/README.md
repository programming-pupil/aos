# AOS Rust Workspace (Web-First)

This workspace now targets **Web deployment first**.

## What is included

- `crates/web-server` — main HTTP API server for the Web UI
- `crates/agent-gateway` — session/runtime gateway used by web routes
- `crates/pm-orchestrator` — PM research orchestration budgets, events, planning DTOs, and persistence helpers
- `crates/pm-report` — PM research report DTOs, report JSON/HTML builders, and source/text utilities
- `crates/nl2sql-core` — NL2SQL domain models, requirement/metric rules, schema discovery/diff, join path, SQL safety, row decoding, result cache/validation, and datasource pool helpers
- `crates/runtime`, `crates/tools`, `crates/api`, `crates/plugins`, `crates/commands` — shared runtime/tooling layers

## What was removed

- Legacy CLI binary crates have been removed from this repository because the current product scope is Web-only.
- CLI release workflow and CLI-only harness scripts were removed accordingly.

## Run (development)

```bash
cd rust
# 1) Compile check
./scripts/dev_web_server.sh check

# 2) Build once
./scripts/dev_web_server.sh build

# 3) Run the compiled binary directly (recommended for daily debug)
./scripts/dev_web_server.sh run
```

Default bind: `127.0.0.1:8080` (can be changed via env/config used by `web-server`).

### Recommended daily workflow (important)

If you frequently edit Rust files, avoid `cargo run -p web-server` for daily
startup. It performs a Cargo dirty check before every run and can contend with
editor background checks. Build once, then run the compiled binary through the
helper script.
Multiple Cargo processes (or editor background checks) can contend for the same target lock and feel "stuck".

Use this helper script instead:

```bash
cd rust

# Full cycle: stop old process -> build -> run
./scripts/dev_web_server.sh run

# Fast compile check for the web server crate
./scripts/dev_web_server.sh check

# Full product-surface check (PM, NL2SQL, RD, projects, Bot Agents)
AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh check

# Restart without rebuilding (when code did not change)
./scripts/dev_web_server.sh quick

# Pass arguments to web-server after --
./scripts/dev_web_server.sh run -- --addr 0.0.0.0:3001

# Stop old web-server/cargo-run process
./scripts/dev_web_server.sh stop
```

Default local commands use the lightweight web-server feature set so ordinary
backend startup does not compile PM/NL2SQL/RD route trees. Use
`AOS_WEB_SERVER_FEATURES=full` for CI, release, or work on those modules.

When code changes, `run` is the safe default.  
When only env/config changes, `quick` is fastest.

`sccache` is intentionally not forced by this repository. Local incremental
checks are often non-cacheable, and contributors may not have `sccache`
installed. To opt in locally:

```bash
cd rust
AOS_DEV_USE_SCCACHE=1 ./scripts/dev_web_server.sh check
```

See [`BUILD_PERFORMANCE.md`](./BUILD_PERFORMANCE.md) for the current build
performance diagnosis and crate-splitting guardrails.

### PM Search Providers

For PM Assistant and Ops research, configure search from the WebUI:
`Operations Copilot` -> `Search Providers`.

The runtime order is:

1. First-party report evidence.
2. Model-native search when the selected model provider declares the capability.
3. MCP search/browser/fetch tools.
4. Tenant-scoped Search Providers stored in the database.
5. Local/RAG evidence.

Brave, Tavily, Serper, Exa, SearXNG, Generic JSON, and Internal HTTP are
provider templates in the WebUI. API keys are stored encrypted and are not
returned to the frontend.

Legacy env vars are still supported only as a compatibility/bootstrap path:
on first PM provider listing, `AOSD_BRAVE_API_KEY`, `AOSD_TAVILY_API_KEY`,
`AOSD_SERPER_API_KEY`, and `AOSD_EXA_API_KEY` are imported into tenant provider
configs if the tenant has no providers yet. New deployments should prefer the
WebUI provider registry.

Low-level WebSearch envs that still tune the shared search tool:

- `AOSD_WEB_SEARCH_PROVIDER=auto|brave|tavily|serper|exa|demo_search` (compatibility/default only)
- `AOSD_WEB_SEARCH_PROVIDER_ORDER=brave,tavily,serper,exa` (compatibility/default only)
- `AOSD_WEB_SEARCH_TIMEOUT_SECS=18`
- `AOSD_WEB_CONNECT_TIMEOUT_SECS=6`
- `AOSD_WEB_SEARCH_MAX_RETRIES=1`
- `AOSD_WEB_SEARCH_RETRY_BACKOFF_MS=200`
- `AOSD_WEB_SEARCH_RETRY_JITTER_MS=120`
- `AOSD_WEB_SEARCH_MAX_RESULTS=10`
- `AOSD_WEB_SEARCH_OUTPUT_HITS=8`
- `AOSD_WEB_SEARCH_COUNTRY=` (optional)
- `AOSD_WEB_SEARCH_LANGUAGE=` (optional)
- `AOSD_WEB_SEARCH_LOCATION=` (optional)
- `AOSD_WEB_SEARCH_ENRICH_ENABLED=true`
- `AOSD_WEB_SEARCH_ENRICH_TARGET_VALID_PAGES=3`
- `AOSD_WEB_SEARCH_ENRICH_INITIAL_FETCH_CANDIDATES=4`
- `AOSD_WEB_SEARCH_ENRICH_MAX_FETCH_CANDIDATES=7`
- `AOSD_WEB_SEARCH_ENRICH_MIN_CHARS=320`
- `AOSD_WEB_SEARCH_ENRICH_MAX_CHARS=1800`
- `AOSD_WEB_SEARCH_ENRICH_FETCH_TIMEOUT_SECS=8`
- `AOSD_WEB_SEARCH_ENRICH_CONNECT_TIMEOUT_SECS=3`
- `PM_PREFLIGHT_ENABLE_RETRIEVAL_PROBE=false` (default; PM search uses native/MCP/configured providers instead of built-in public-engine probes)
- `PM_MAX_ATTEMPTS=2`
- `PM_RETRIEVE_MAX_TOOL_CALLS=4`
- `PM_RETRIEVE_SEARCH_ONLY=true` (only blocks unsafe crawler/browser tools; MCP search/fetch remains allowed)
- `PM_PARALLEL_SUBTASK_MAX_CANDIDATES=10` (hard upper bound, not a fixed target)
- `PM_PARALLEL_SUBTASK_MAX_CONCURRENCY=4`
- `PM_PARALLEL_SUBTASK_MAX_ATTEMPTS=2`

If no native/MCP/configured provider is available, PM Assistant reports that
external search is unavailable and continues only from first-party/local
evidence.

PM route defaults are tuned for low cost/high stability:

- Single source channel by default (`web.search.general`)
- Retrieval turns are `WebSearch`-first, with provider fallback inside `WebSearch`

## Build

```bash
cd rust
AOS_WEB_SERVER_FEATURES=full cargo build -p web-server --release --features full
```

## Notes for contributors

- Keep route handlers thin: request parsing, service call, response DTO.
- Prefer service/domain modules over growing large route files.
- Avoid broad `use super::*` in new modules; import only what is needed.
- For large modules, split by capability before the file grows beyond handler-level responsibility.
- Keep Web API contracts stable for `webui/src/api/index.ts`.
