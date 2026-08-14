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
| Memory 2.0 | `rust/crates/memory-engine`: dual continuity/long-term channels, secret rejection, scope/temporal filtering, lexical/entity/authority/recency rerank and conflict bundles | dual-channel, secret-filter, current-version and conflict-package tests |
| Compaction 2.0 | Runtime compaction hook persists protected source coverage and checkpoint hashes into `compaction_checkpoints` before the runtime replacement commits; legacy session archive remains the exact source | runtime compaction tests plus semantic checkpoint persistence path |
| Sensitive projections | raw/model/client/telemetry projection contract with source hash and redaction provenance | credential/PII leak regression tests |
| Capability and artifacts | capability-token expiry/use-count/child intersection plus tenant/owner-scoped artifact plane | scope-expansion, one-time token, tenant isolation and deletion tests |
| Resource budget | reserve/commit/release and parent-child sub-allocation with no second debit | conservation and insufficient-budget tests |
| Requirement Discovery | `pm-domain::requirement_state`: persistent state delta, readiness gate and information-value next-question policy | incremental delta and question-ranking tests |
| Analytics Semantic Compiler | `nl2sql-core::semantic_ir` plus live `web-server/routes/nl2sql/semantic_audit.rs`: parse final policy-rewritten SQL, build `AnalyticIntentIR`, run deterministic verification and persist release decision | compiler tests and query-path audit persistence; unsafe/fanout rejection remains fail-closed |
| Provider replay TCK | `eval-harness::replay`: canonical request hash, deterministic frames, stable script key and `assert_consumed` | missing/extra frame and tool-call tests |
| Storage boundary | tenant-scoped shadow tables for ledger, evidence, semantic state, compaction, context, requirement, metric/join contracts, IR, verification, artifacts, budget and eval | migration idempotency and required-table checks |

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
