# AOS Semantic Kernel implementation notes

This file records the production implementation of the AOS Semantic Kernel.
Legacy rows remain readable as compatibility projections, but new production
decisions are controlled by the semantic-kernel contracts. Durable Agent Ledger
recovery precedes JSONL, the Context Compiler constructs the provider request,
the MemoryEngine owns adapter policy, Requirement State gates PM
research/delivery, and canonical Analytic IR controls both SQL generation and
post-generation verification.

## Delivered in this change

| Spec area | Implementation | Verification |
| --- | --- | --- |
| Unified Agent Protocol | `rust/crates/agent-protocol`: versioned event envelope, stable sequence, idempotency key, payload hash, lifecycle states, child settlement and executor SPI; `agent-gateway` rebuilds the active runtime only from a redacted Ledger envelope plus hash-bound AES-GCM recovery payload | protocol suite plus exact encrypted no-JSONL runtime recovery and tamper test |
| Execution ledger | SQLite Agent Ledger in `semantic_kernel_store.rs`, with writer lease/fencing, idempotency, torn-tail repair, terminal projection, redacted envelopes and hash-bound AES-GCM recovery payloads; turn-owned and background-visible messages both enter the Ledger without creating ghost turns; migrations `0017_semantic_kernel_core.sql` and `0027_agent_event_recovery_payload.sql` | stale writer, duplicate payload, corruption, exact no-JSONL recovery, background-message recovery and ciphertext substitution cases fail closed |
| Semantic State Kernel | `rust/crates/semantic-core`: assertion/decision/evidence/snapshot types, temporal conflict/supersession reducer, context manifest and snapshot hash | reducer idempotency, missing-evidence, conflict and context-budget tests |
| Memory 2.0 | `rust/crates/memory-engine`: stateless admission/ranking/temporal policy kernel plus an in-memory reference repository; the production SQLite adapter owns durability but delegates policy decisions to the kernel. Compaction uses the tenant chat model, records source evidence/channel provenance, advances a durable scope cursor, and applies auditable supersession/conflict diffs | production-adapter policy equivalence, dual-channel, secret-filter, current-version, conflict-package and cursor/relation transaction tests |
| Compaction 2.0 | Runtime compaction uses a prepared/commit/abort transaction. The exact encrypted archive and redacted retrieval projection are written only at commit; the fully framed continuation replacement is compared with the archived source window and fails closed when it would grow the model-visible context. JSONL is export-only | runtime compaction tests, growth-guard property coverage, exact archive round-trip and injected commit rollback tests |
| Sensitive projections | raw/model/client/telemetry projection contract with source hash and redaction provenance | credential/PII leak regression tests |
| Capability and artifacts | capability-token expiry/use-count/child intersection plus tenant/owner-scoped artifact plane | scope-expansion, one-time token, tenant isolation and deletion tests |
| Resource budget | reserve/commit/release, parent-child conservation, hard-isolated general/final synthesis/domain verifier/user-visible error pools, and a parent-owned `child_slots` account; capability consumption, slot reservation, lineage and spawn event commit in one transaction | pure-ledger conservation plus SQLite behavior tests proving general exhaustion cannot consume the final reserve and a fourth concurrent child cannot oversell or leak partial state |
| Requirement Discovery | Planner emits the complete `REQUIREMENT_DELTA_V1`; durable `RequirementState` applies it before retrieval, research evidence returns as another delta, and final delivery reloads state. Core questions block retrieval; unresolved critical assumptions force Requirement Brief instead of PRD; malformed or unpersisted state fails closed | full-field SQLite round-trip/idempotency, production final-delivery gate, readiness and information-value tests |
| Analytics Semantic Compiler | NL first becomes an intent proposal, tenant Metric Contracts and the final Schema bind it to an immutable canonical `AnalyticIntentIR`; that exact JSON is injected into SQL generation and drives physical SQL-shape verification and the persisted release decision. Both `/query` and the single-datasource/federated/multi-step `Nl2SqlAgent` paths persist IR before their provider stage. SQL generation, semantic audit and later execution/EXPLAIN repairs cannot re-infer or overwrite intent; every repair is independently and immutably audited against the original IR and any non-`Release`, IR drift, or persistence failure blocks execution | schema/time-column binding, immutable durable IR (including semantic-audit and changed repair-audit overwrite attempts), generator-input/post-SQL identity, scope-preserving repair release, grain-drift repair rejection and tenant-isolated execution evidence tests |
| Versioned metric/join binding | NL2SQL loads active tenant contracts before generation, binds aliases to an explicit version, and passes join contracts into the deterministic verifier | malformed contract rejection, tenant isolation and semantic release decision tests |
| Attribution evidence levels | Production attribution reports normalize to L0 descriptive or L1 decomposition, every driver cites a usable evidence step, and the UI labels contribution directions rather than asserting causes | sanitizer regression tests and typed WebUI rendering |
| Runtime resource settlement | SQLite execution kernel reserves/settles tool, web, datasource, model token and artifact-byte dimensions; terminal failures release model reservations without changing side-effect truth | runtime-kernel lifecycle and recovery tests |
| Child Thread production lineage | Specialist subtasks write spawn/settlement into `child_thread_edges` and the unified event ledger; duplicate/late settlement cannot replace the first terminal result and parent cancellation propagates downward | tenant-lineage, idempotency and first-settlement tests |
| Feedback learning and calibration | Corrections must bind an authoritative query owned by the submitter and contain safe SQL; only datasource owners/admins can approve or revoke them. Current approval state and append-only approval audit commit atomically; only approved tenant/datasource-scoped exemplars enter retrieval. Human labels drive scoped ECE/Brier | unsafe/missing correction rejection, owner/admin approval, approval/revocation audit, exemplar isolation and scoped calibration tests |
| Prompt lineage | Runtime context manifests persist the effective model, active tools, prompt/message hashes and budgets before every provider call; PM also writes versioned prompt manifests | manifest replay and sensitive-projection tests |
| Artifact recovery | Typed text/log/search/table/JSON/binary reducers persist the complete protected payload, expose bounded model/client/telemetry projections, and require an explicit owner-scoped `source` read for exact paging recovery; migration `0023_artifact_legacy_bridge.sql` backfills and trigger-bridges the two legacy artifact writers | artifact access, UTF-8, row/byte accounting, projection isolation, legacy bridge and deletion tests |
| Provider replay TCK | `eval-harness::replay`: canonical request hash, deterministic frames, stable script key and `assert_consumed` | missing/extra frame and tool-call tests |
| No-leak recall probe | Zero Loss questions identify only the fact category and source turn; the expected fact is kept out of the prompt and used only by the scorer | regression test asserts the probe does not contain the target fact |
| Storage boundary | tenant-scoped tables for ledger, evidence, semantic state, compaction, context, requirement, metric/join contracts, immutable IR/repair verification, feedback approval/calibration, artifacts, stage-aware budget, eval and Memory consolidation | migrations `0017`-`0029`, idempotency, legacy bridge and required-table checks |
| Session deletion/retention | deletion revokes session Memory, relation/citation/summary, archives, context/prompt manifests, checkpoints, snapshots, PM delivery and traces even when no artifact exists; global Memory and compliance rows remain | no-artifact deletion and compliance-preservation regression tests |
| P0 conformance gate | 31 immutable P0 cases map spec section, production symbol, trigger, expected behavior and executable behavior-test symbol | traceability validation plus `scripts/check-semantic-kernel-behavior.sh`, which executes every mapped test and rejects zero-test filters |

## Compatibility and rollout

The SQLite semantic-kernel adapter is authoritative for new Agent, PM, Memory
and NL2SQL facts. JSONL is write-only diagnostic/export output and is never a
runtime recovery input. Legacy PM task/chat rows are projection-only. Every completed PM task stores a
durable final-delivery artifact, and history replay returns artifacts for all
tasks in a session. NL2SQL keeps the existing provider-backed generator but
now writes a semantic IR and verifier result before exposing a generated SQL
candidate. The pure contracts remain reusable by native and future external
executors.

The SQLite migration is additive and idempotent. Existing data is not rewritten
or reclassified as confirmed facts. Completed pre-migration PM tasks are lazily
backfilled on first history read. Semantic IR and verification never overwrite
the original SQL or provider response, but they are mandatory release gates: a
missing or non-`Release` result cannot fall through to the legacy SQL path.
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
External executor adapters remain an optional extension point rather than an
unverified compatibility claim.

## Explicit non-claims

The new verifier does not claim that parseable SQL is business-correct, and the
attribution path does not turn L0/L1 contribution evidence into causal claims.
No external Trino, analytics data source, or model-provider endpoint is contacted
by these tests. A real-provider test must be opted into with explicit fixtures or
environment variables and remains subject to the per-user three-request limit.

## Required checks before enabling a domain

```bash
cd rust
cargo fmt --all -- --check
cargo test --workspace --all-features
../scripts/check-semantic-kernel-behavior.sh

cd ../webui
npm run i18n:check
npm test
npm run build
```

The durable Agent Ledger and PM final-delivery artifact are authoritative for
new recovery. A failed semantic-audit persistence write blocks SQL release. A
future quality claim must still pass the replay, semantic-accuracy and latency
gates in the conformance matrix and record differences in `eval_case_results`.
