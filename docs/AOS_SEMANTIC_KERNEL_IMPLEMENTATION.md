# AOS Semantic Kernel implementation notes

This file is the implementation record for `AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md`.
The refactor is deliberately staged: the existing session, Memory, PM and
NL2SQL paths remain readable while the new contracts are shadow-written and
replayed.  This avoids a destructive big-bang migration and gives every
domain a rollback boundary.

## Delivered in this change

| Spec area | Implementation | Verification |
| --- | --- | --- |
| Unified Agent Protocol | `rust/crates/agent-protocol`: versioned event envelope, stable sequence, idempotency key, payload hash, lifecycle states, child settlement and executor SPI | 10 unit tests, including stale writer fencing, idempotency, unknown schema and torn-tail recovery |
| Execution ledger | In-memory deterministic ledger used by replay/shadow code; SQLite shadow schema in migration `0017_semantic_kernel_core.sql` | Web-server SQLite migration tests; middle corruption fails closed and an uncommitted tail is discarded |
| Semantic State Kernel | `rust/crates/semantic-core`: assertion/decision/evidence/snapshot types, temporal conflict/supersession reducer, context manifest and snapshot hash | reducer idempotency, missing-evidence, conflict and context-budget tests |
| Memory 2.0 | `rust/crates/memory-engine`: dual continuity/long-term channels, secret rejection, scope/temporal filtering, lexical/entity/authority/recency rerank and conflict bundles | dual-channel, secret-filter, current-version and conflict-package tests |
| Compaction 2.0 | fail-closed checkpoint validator: source coverage, complete output, strict reduction and tool-boundary protection | compaction unit tests |
| Sensitive projections | raw/model/client/telemetry projection contract with source hash and redaction provenance | credential/PII leak regression tests |
| Capability and artifacts | capability-token expiry/use-count/child intersection plus tenant/owner-scoped artifact plane | scope-expansion, one-time token, tenant isolation and deletion tests |
| Resource budget | reserve/commit/release and parent-child sub-allocation with no second debit | conservation and insufficient-budget tests |
| Requirement Discovery | `pm-domain::requirement_state`: persistent state delta, readiness gate and information-value next-question policy | incremental delta and question-ranking tests |
| Analytics Semantic Compiler | `nl2sql-core::semantic_ir`: AnalyticIntentIR, metric/join contracts, fanout/grain/filter checks and non-fixed confidence basis | fanout rejection, clarification and confidence tests |
| Provider replay TCK | `eval-harness::replay`: canonical request hash, deterministic frames, stable script key and `assert_consumed` | missing/extra frame and tool-call tests |
| Storage boundary | tenant-scoped shadow tables for ledger, evidence, semantic state, compaction, context, requirement, metric/join contracts, IR, verification, artifacts, budget and eval | migration idempotency and required-table checks |

## Compatibility and rollout

`runtime::semantic_kernel::SemanticKernelBridge` is the first shadow adapter.
It is disabled by default and has no effect on legacy sessions.  When enabled
for a tenant it appends a canonical user-message event and accepts semantic
deltas through the deterministic reducer.  The bridge is intentionally small:
HTTP/database orchestration stays in `web-server`, while the pure contracts
remain reusable by native, Codex-compatible and future DSH-compatible
executors.

The SQLite migration is additive and idempotent. Existing data is not rewritten
or reclassified as confirmed facts.  The migration creates the storage needed
for subsequent shadow-write/read gates; production cutover still requires the
domain-specific replay and quality thresholds in the main specification.

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

Only after the corresponding replay, semantic-accuracy and latency gates pass
should a tenant feature flag move from shadow-write to shadow-read, and then to
authoritative read.  A failed gate must leave the legacy path active and record
the difference in `eval_case_results`.
