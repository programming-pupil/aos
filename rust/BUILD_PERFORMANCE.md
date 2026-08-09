# Rust Build Performance

This workspace is web-first and currently has one intentionally large HTTP
entry crate: `web-server`. Keep build performance in mind when adding new
features, because Rust checks operate at crate boundaries rather than at route
file boundaries.

## Current Diagnosis

Local measurements on a warm development target showed:

- `crates/web-server/src`: about 89.3k lines after PM and NL2SQL core splits.
- `crates/nl2sql-core/src`: about 6.6k lines.
- `crates/pm-report/src`: about 4.3k lines.
- `crates/pm-orchestrator/src`: about 2.0k lines.
- Largest route files are several thousand lines each, especially PM, Agent,
  NL2SQL, data sources, MCP, and Skills routes.
- `web-server` has many heavyweight direct dependencies, including `sqlx`,
  `axum`, `reqwest`, `clickhouse`, `trino-rust-client`, `rusqlite`, `scraper`,
  `zip`, `hnsw_rs`, and `lettre`.
- Local incremental state can become very large. In one observed workspace,
  `target/debug/incremental/web_server-*` entries alone exceeded 19 GB.

The important conclusion: a small route edit can still cause Cargo/rustc to
check the `web_server` crate as one large compilation unit.

## Split Progress

The first safe splits have been completed:

- `crates/pm-orchestrator` now owns PM research orchestration budgets, events,
  planning DTOs, repair/retrieve/synthesize helpers, and PM persistence helpers.
- `crates/pm-report` now owns PM report DTOs, report JSON/HTML construction,
  source URL filtering, and report-oriented text utilities.
- `crates/nl2sql-core` now owns NL2SQL schema discovery, schema diffing, join
  path generation, result cache/validation, datasource pool caching, refresh
  locks, cross-datasource discovery helpers, coreference helpers, SQL safety
  classification, MySQL/PostgreSQL row-cell decoders, query-understanding
  public domain models, and deterministic requirement/metric constraint rules.
- `crates/web-server` consumes it as a normal workspace dependency and keeps
  HTTP route composition in place.

These are intentionally service/domain-layer splits, not route splits. They
reduce the business logic owned by `web-server` without changing public HTTP
behavior.

## sccache Policy

The repository does not force `sccache` via `.cargo/config.toml`.

Reason:

- `sccache` is not installed on every contributor machine.
- Local incremental `cargo check` calls are commonly non-cacheable.
- In observed dirty-check runs, forcing `sccache` did not help and could make
  checks slower.

If a developer or CI environment benefits from `sccache`, enable it locally:

```bash
export RUSTC_WRAPPER=sccache
```

The local helper script keeps `sccache` opt-in:

```bash
cd rust
AOS_DEV_USE_SCCACHE=1 ./scripts/dev_web_server.sh check
```

## Recommended Local Commands

For daily backend work:

```bash
cd rust
./scripts/dev_web_server.sh check
./scripts/dev_web_server.sh run
```

For a broader Rust check:

```bash
cd rust
./check.sh
```

For release build:

```bash
cd rust
./build.sh
```

For local timing diagnostics:

```bash
cd rust
./scripts/cargo_timings.sh check
./scripts/cargo_timings.sh build
AOS_WEB_SERVER_FEATURES=full ./scripts/cargo_timings.sh both
```

Cargo writes HTML timing reports under `target/cargo-timings/`. Keep these
local reports out of normal source changes; use them to identify the slowest
crate/dependency edge before doing a larger Rust split.

Avoid running multiple Cargo commands at the same time. They contend for the
same target directory lock and can make the build appear stuck.

## Architecture Guardrails

When adding or changing backend features:

- Do not keep growing giant route files. Split large features by capability.
- Keep route handlers thin: parse request, call service/domain code, return DTO.
- Move heavy business logic out of `routes/*` as soon as it becomes reusable or
  exceeds a small handler-level responsibility.
- Avoid broad `use super::*` in new modules. Import only what is needed.
- Keep provider/client adapters behind narrow interfaces so they can be moved
  to smaller crates later.

## Planned Crate Split Direction

The long-term fix is to reduce the size of `web-server` by moving cohesive
feature areas into smaller crates. A safe split order is:

1. PM/Agent orchestration and report support code. Started with
   `pm-orchestrator` and `pm-report`.
2. NL2SQL route/service support code. Started with `nl2sql-core`; next targets
   should be DTOs/pure prompt helpers and then database-heavy execution adapters
   so pure logic can compile without every SQL client dependency.
3. Integration-heavy modules such as Skills, MCP, data sources, and materials.
4. Shared web DTO/error/state helpers.

The final shape should keep `web-server` focused on:

- App state construction.
- Axum router composition.
- Auth/setup middleware.
- Server startup and graceful shutdown.

Feature crates should expose small `routes(state) -> Router<AppState>` style
entrypoints and avoid depending on unrelated feature areas.
