# AOS Semantic Kernel implementation notes

This file is the implementation record for `AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md`.
The refactor is deliberately staged: the existing session, Memory, PM and
NL2SQL paths remain readable while the new contracts are introduced at durable
boundaries. This avoids a destructive big-bang migration and gives every
domain a rollback boundary. PM stage/history projection and NL2SQL semantic
auditing are now live; legacy rows remain readable as compatibility projections.

## Delivered in this change

| Spec area | Implementation | Verification |
| --- | --- | --- |
| Unified Agent Protocol | `rust/crates/agent-protocol`: versioned event envelope, stable sequence, idempotency key, payload hash, lifecycle states, child settlement and executor SPI | 10 unit tests, including stale writer fencing, idempotency, unknown schema and torn-tail recovery |
| Execution ledger | SQLite durable PM stage ledger in `semantic_kernel_store.rs`, with writer lease/fencing, idempotency, torn-tail repair and terminal projection; migration `0018_semantic_kernel_runtime.sql` | Web-server ledger tests; stale writer, duplicate payload and corruption cases fail closed |
| Semantic State Kernel | `rust/crates/semantic-core`: assertion/decision/evidence/snapshot types, temporal conflict/supersession reducer, context manifest and snapshot hash | reducer idempotency, missing-evidence, conflict and context-budget tests |
| Memory 2.0 | `rust/crates/memory-engine`: dual continuity/long-term channels, secret rejection, scope/temporal filtering, lexical/entity/authority/recency rerank and conflict bundles; the production compaction hook uses the tenant chat model, records source evidence/channel provenance, advances a durable scope cursor, and the consolidation API applies auditable supersession/conflict diffs | dual-channel, secret-filter, current-version, conflict-package and cursor/relation transaction tests |
| Compaction 2.0 | Runtime compaction hook persists protected source coverage and checkpoint hashes into `compaction_checkpoints` before the runtime replacement commits; legacy session archive remains the exact source; the fully framed continuation replacement is compared with the archived source window and fails closed when it would grow the model-visible context | runtime compaction tests, growth-guard property coverage and semantic checkpoint persistence path |
| Sensitive projections | raw/model/client/telemetry projection contract with source hash and redaction provenance | credential/PII leak regression tests |
| Capability and artifacts | capability-token expiry/use-count/child intersection plus tenant/owner-scoped artifact plane | scope-expansion, one-time token, tenant isolation and deletion tests |
| Resource budget | reserve/commit/release and parent-child sub-allocation with no second debit | conservation and insufficient-budget tests |
| Requirement Discovery | `pm-domain::requirement_state`: persistent state delta, readiness gate and information-value next-question policy | incremental delta and question-ranking tests |
| Analytics Semantic Compiler | `nl2sql-core::semantic_ir` plus live `web-server/routes/nl2sql/semantic_audit.rs`: parse final policy-rewritten SQL, build `AnalyticIntentIR`, run deterministic verification and persist release decision; successful datasource execution adds scoped row/column/latency evidence without converting unresolved semantic checks into passes | compiler tests, query-path audit persistence and tenant-isolated execution-evidence tests; unsafe/fanout rejection remains fail-closed |
| Versioned metric/join binding | NL2SQL loads active tenant contracts before generation, binds aliases to an explicit version, and passes join contracts into the deterministic verifier | malformed contract rejection, tenant isolation and semantic release decision tests |
| Attribution evidence levels | Production attribution reports normalize to L0 descriptive or L1 decomposition, every driver cites a usable evidence step, and the UI labels contribution directions rather than asserting causes | sanitizer regression tests and typed WebUI rendering |
| Runtime resource settlement | SQLite execution kernel reserves/settles tool, web, datasource, model token and artifact-byte dimensions; terminal failures release model reservations without changing side-effect truth | runtime-kernel lifecycle and recovery tests |
| Child Thread production lineage | Specialist subtasks write spawn/settlement into `child_thread_edges` and the unified event ledger; duplicate/late settlement cannot replace the first terminal result and parent cancellation propagates downward | tenant-lineage, idempotency and first-settlement tests |
| Feedback learning and calibration | Safe corrections create stable regression events; only explicitly approved tenant/datasource-scoped exemplars enter retrieval; human feedback labels confidence observations and stats report ECE/Brier | unsafe correction rejection, exemplar isolation and calibration tests |
| Prompt lineage | Runtime context manifests persist the effective model, active tools, prompt/message hashes and budgets before every provider call; PM also writes versioned prompt manifests | manifest replay and sensitive-projection tests |
| Artifact recovery | Typed text/log/search/table/JSON/binary reducers persist the complete protected payload, expose bounded model/client/telemetry projections, and require an explicit owner-scoped `source` read for exact paging recovery; migration `0023_artifact_legacy_bridge.sql` backfills and trigger-bridges the two legacy artifact writers | artifact access, UTF-8, row/byte accounting, projection isolation, legacy bridge and deletion tests |
| Provider replay TCK | `eval-harness::replay`: canonical request hash, deterministic frames, stable script key and `assert_consumed` | missing/extra frame and tool-call tests |
| No-leak recall probe | Zero Loss questions identify only the fact category and source turn; the expected fact is kept out of the prompt and used only by the scorer | regression test asserts the probe does not contain the target fact |
| Storage boundary | tenant-scoped shadow tables for ledger, evidence, semantic state, compaction, context, requirement, metric/join contracts, IR, verification, feedback calibration, artifacts, budget, eval and Memory consolidation | migrations `0017`-`0023`, idempotency, legacy bridge and required-table checks |

## Compatibility and rollout

PM uses the SQLite semantic-kernel adapter as its authoritative stage/history
projection, while legacy PM task rows and chat messages remain readable for
backward compatibility and lazy backfill. Every completed PM task stores a
durable final-delivery artifact, and history replay returns artifacts for all
tasks in a session. NL2SQL keeps the existing provider-backed generator but
now writes a semantic IR and verifier result before exposing a generated SQL
candidate. The pure contracts remain reusable by native, Codex-compatible and
future DSH-compatible executors.

The SQLite migration is additive and idempotent. Existing data is not rewritten
or reclassified as confirmed facts. Completed pre-migration PM tasks are lazily
backfilled on first history read. Semantic IR and verification are audit
projections and never overwrite the original SQL or provider response.
Model/token reservations are released on terminal recovery; a tool result that
already succeeded is never rewritten as failed merely because artifact
accounting is exhausted.

The Web approval lifecycle is live for Server/Gateway/WebUI runtimes: the runtime
uses a durable defer prompter, writes the request before suspension, exposes a
redacted SSE handoff, and resumes only after an owner-scoped one-time decision
is rechecked against the current policy. After a browser reload, the WebUI
restores pending approvals from durable state and resumes them with reload-safe
stream handlers that refresh canonical history on completion. Native AOS Child Thread control is
capability-negotiated: cancellation is live and recoverable after restart;
`follow_up`/`steer`/`interrupt`/`resume` are durably recorded and explicitly
rejected when the selected executor does not advertise those capabilities.
External executor adapters remain the Phase 6 strategic option described in the
main specification, rather than an unverified compatibility claim.

## Explicit non-claims

The new verifier does not claim that parseable SQL is business-correct, and the
attribution path does not turn L0/L1 contribution evidence into causal claims.
No external Trino, provider, Codex or DeepSeek Harness endpoint is contacted by
these tests.  A real-provider test must be opted into with explicit fixtures or
environment variables and remains subject to the per-user three-request limit.

## Required checks before enabling a domain

```bash
cd rust
cargo fmt --all -- --check
cargo test --workspace --all-features

cd ../webui
npm run i18n:check
npm test
npm run build
```

The PM durable ledger and final-delivery artifact are authoritative for new
stage/history recovery. A failed semantic-audit persistence write leaves the
legacy NL2SQL path active and records the degradation in applied rules. A
future domain cutover must still pass the replay, semantic-accuracy and latency
gates in the main specification and record differences in `eval_case_results`.
