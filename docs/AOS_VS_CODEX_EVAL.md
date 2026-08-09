# AOS vs Codex Evaluation Protocol

This repository contains a deterministic harness and a synthetic contract
fixture. It does not contain measured proof that AOS outperforms Codex.

## Two Different Evaluation Modes

### Contract Fixture

`eval/datasets/codex-parity-gaps.seed.json` checks that all required scenario
categories are wired into the harness and that report/threshold behavior is
deterministic. Its case values are fixed synthetic values:

- metric names start with `contract_fixture_`;
- `codexBaseline` is deliberately absent;
- the generated delta is therefore absent;
- scores must not be published as answer-quality results.

Run it with:

```bash
cd rust
cargo run -p eval-harness
```

The command exits non-zero when a thresholded contract scenario fails.

### Empirical Online A/B

A comparative release claim requires a separate, versioned result dataset. Run
the same immutable cases against AOS and the declared Codex product/version,
then score outputs without revealing the system identity to reviewers.

Required dimensions:

| Dimension | Primary measures |
|---|---|
| General chat | factual correctness, instruction following, usefulness |
| Live lookup | freshness, citation correctness, evidence coverage |
| SQL attribution | datasource routing, executable rate, business-semantic correctness |
| Deep report | claim grounding, source diversity, conflict handling, decision usefulness |
| Coding | task success, test pass rate, unintended-change rate |
| Memory | recent-turn fidelity, early-turn recall, attachment/SQL exact recall |
| Recovery | reload/disconnect completion and duplicate/lost-message rate |
| Efficiency | p50/p95 latency, input/output/cache tokens, estimated cost |

Record for every case:

- immutable case ID and input artifact hashes;
- model/provider and reasoning settings;
- enabled tools, Skills, MCP servers, network policy, and time budget;
- raw output references and execution evidence;
- AOS score, Codex score, reviewer rubric, and reviewer count;
- failures, timeouts, exclusions, and exclusion reason.

Do not silently drop failed cases. Report both macro averages and raw case
counts so a high score cannot hide a small or selectively filtered sample.

The checked-in empirical manifest is
`eval/datasets/super-assistant-parity-180.json`. It expands deterministically
to 180 cases across chat, live web, code, file/SQL workspace, NL2SQL,
attribution, long context, and recovery/isolation. Every case is run exactly
three times. The expansion count is enforced by `eval-harness` tests.

Run the real comparison with:

```bash
cd rust

# AOS can use the real HTTP Super Assistant endpoint.
export AOS_EVAL_BASE_URL=http://127.0.0.1:8080
export AOS_EVAL_BEARER_TOKEN=...
export AOS_EVAL_MODEL=...

# Codex is invoked directly through `codex exec --json --ephemeral`.
export AOS_EVAL_CODEX_PROGRAM=/path/to/codex
export AOS_EVAL_CODEX_MODEL=...
export AOS_EVAL_CODEX_REASONING_EFFORT=high

cargo run -p eval-harness -- --parity
```

File, code, SQL-workspace, long-context, and isolation cases need authenticated
fixture setup. For those runs, configure an AOS command adapter instead of the
basic HTTP adapter:

```bash
unset AOS_EVAL_BASE_URL
export AOS_EVAL_AOS_PROGRAM=/path/to/aos-eval-adapter
export AOS_EVAL_AOS_ARGS_JSON='[]'
export AOS_EVAL_AOS_MODEL=...
export AOS_EVAL_AOS_FIXTURE_ROOT=../eval/fixtures
```

Each run writes `raw/aos`, `raw/codex`, `operational-metrics.json`, an anonymous
`blind-review.json`, and a separately held `blind-key.json`. A run summary uses
`correctnessStatus=pending_blind_review`; adapter completion alone is never
treated as answer correctness.

## Reproducibility

Fixed datasets belong under `eval/datasets/` and must include a stable seed and
case IDs. Online evidence should be immutable once published; corrections
create a new version rather than editing prior results.

## Required Disclaimer

裸编码准确度受模型 × harness 协同影响、非本 spec 承诺范围。本 harness
用于客观量化差距，不承诺在裸编码准确度维度上追平或超越 Codex。

## Positioning

AOS 的差异化定位为模型无关 + 企业级 + 深度分析：可插拔运行时、多租户与
审计、可执行的数据分析，以及证据可追溯的深度研究能力。差异化能力不等于
自动获得更高回答准确率，准确率仍以在线盲测结果为准。
