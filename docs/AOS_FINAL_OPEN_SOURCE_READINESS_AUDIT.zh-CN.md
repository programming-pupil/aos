# AOS 最终开源就绪审计与实现核销

> 审计与实现日期：2026-08-20
> AOS 实现基线：本次最终 merge commit（由紧随其后的审计记录提交固化实际 commit hash）
> OpenAI Codex 对照：`3929c99a97d1aa0fb8000903a4b57b24fbabe742`（2026-08-19）
> DeepSeek Harness 对照：`99f6f02fecdb7dff40c3fbc9470f5907c29f74ca`（2026-08-17）
> 性质：源码级发布审计、缺口修复记录和发布声明边界

## 1. 结论

原审计识别的八项 P0 已全部产生生产实现，不再处于“只有表、trait、测试名或文档”的状态。关键请求路径现在具备 canonical durable interaction、canonical memory、原子终态、精确压缩来源、四层 manifest、统一密文轮换、AST 语义校验和可反证行为门禁。Codex 的两阶段 Memory 运行纪律已落为 `memory_extraction_outbox`、`memory_consolidation_batches` 和 `memory_embedding_rebuild_outbox` 驱动的持久化 governance worker；DeepSeek Harness 的 replay/fail-closed 思路已进入 provider fixture 和行为证据。

这份结论刻意区分两件事：

- **源码可以直接开源和评审**：迁移、生产接线、负向测试及机器可读证据生成均在仓库内，不依赖私有补丁。
- **不能提前宣称“效果全面超过 Codex/DSH”**：三方同模型、同权限、同预算的盲测属于外部实验事实，不可能由架构代码自动证明；在 raw benchmark 和置信区间产出前，只声明机制对齐或增强。

## 2. 对照源码范围

本次直接检查了两个上游仓库的最新源码，而非只参考 README。

Codex 的主要对照面：

- `codex-rs/memories/write/src/phase1.rs`、`phase2.rs`、`storage.rs`、`control.rs`：bounded extraction、lease、heartbeat、retry/backoff、cooldown、污染排除和全局整合。
- `codex-rs/core/src/context_manager/`、`compact.rs` 及 session tests：上下文重建、压缩和恢复不变量。
- provider/tool request 构造路径：模型实际可见工具与请求级 lineage。

DeepSeek Harness 的主要对照面：

- `packages/session/session-persistence/` 与 checkpoint crash-recovery E2E：后端契约、冷恢复和进程故障语义。
- `packages/compaction/compaction-basic/`、`compaction-tool-result-pruner/`、`spill/`：压缩、模型可见 surface 和大输出保留。
- `packages/interaction/user-questions/`：用户提问 seam；AOS 在此基础上额外实现多租户授权、加密答案、过期和重启后的 exactly-once consume。

两者定位不同：Codex 是成熟 coding agent，DSH 强在可组合 harness，AOS 的差异化是多租户 durable control plane、业务语义内核、PM/NL2SQL 和 typed memory。没有复制与这些目标无关的 UI 或代码编辑复杂度。

## 3. 八项 P0 核销

| ID | 实现状态 | 生产实现与不变量 | 反证/恢复证据 |
| --- | --- | --- | --- |
| K-P0-01 Durable Question | `implemented` | Runtime 在 executor 前截获 `AskUserQuestion`；问题与 suspended terminal/checkpoint 同事务；Web API 只接受 canonical `durable_interactions`；批量答案在一笔事务中完成授权、幂等、响应事件、outbox、turn revision 和 exactly-once consume | owner scope、重启恢复、重复相同答案、跨 owner、批次第二项失败时第一项保持 pending、直接 executor fail-closed |
| K-P0-02 Memory single source | `implemented` | `structured_memory_facts` 为 canonical owner，`agent_memory_items` 为可重建 projection；create/update/delete/supersede/compaction/Gateway 写入同事务；active read 必须存在 current canonical fact；polluted/disabled 禁止抽取 | migration 全量 backfill；删除/覆盖同步；projection-only 创建 fail-closed；phase-2 worker 污染 quarantine 测试 |
| K-P0-03 Atomic terminal/checkpoint | `implemented` | Runtime 生产终态只调用 `finish_turn_with_checkpoint`；预算 settlement、terminal event、checkpoint event、完整 session checkpoint 和 projection 单事务提交 | 相同命令 lost-ack 重试幂等；terminal 与 checkpoint 同 source revision/hash 测试 |
| K-P0-04 Exact compaction provenance | `implemented` | archive unit 按 ordinal+message hash 映射 exact durable runtime events；保存显式 source event set、parent compaction IDs 和 unit hashes；missing/extra/hash mismatch fail-closed | 测试证明 warning/窗口外事件不会进入 coverage，manifest 不再使用 thread 全历史 |
| K-P0-05 Prompt/Wire lineage | `implemented` | Context、Prompt、Tool Schema、Provider Attempt 分离为 immutable manifest；dispatch 前以最终序列化 request 建 attempt-specific 绑定；DB trigger 阻止 lineage mutation | provider 测试断言三个 manifest ID；唯一索引阻止同 iteration/attempt 并发重复 dispatch |
| K-P0-06 Complete key rotation | `implemented` | registry 覆盖 12 个受保护字段；统一 `aosenc:v1:key-id`；Git token、Datasource JSON 和 Question legacy 数据在线迁移；durable job 记录 heartbeat/count/status；旧 key 引用归零才 completed | legacy payload rotation、CAS 更新、zero-old-key count；新写入统一 envelope |
| K-P0-07 NL2SQL semantic proof | `implemented-supported-scope` | verifier 使用 sqlparser AST 的 projection、predicate、relation、aggregate、DISTINCT、日期边界、时区和 denominator subtree；精确 identifier，不再用 substring/synonym family 放行；unsupported fail-closed | 全语义测试及 comment/substring 污染 adversarial cases |
| K-P0-08 Production behavior gate | `implemented` | conformance case 必须同时给 production symbol/trace anchor 和真实 assertion/awaited invocation；脚本生成逐 case JSONL evidence；ProviderReplay 要求 safe fixture、精确 tool-call 数、可选 terminal projection hash 和 fault 全消费 | fake-helper negative test；重复 tool call、未消费 fault、unsafe fixture 均失败 |

“implemented”表示当前 SQLite 生产主路径和受支持范围已具备关闭条件，不表示未来任意存储后端已自动获得相同行为。新增后端必须运行同一 contract/TCK，不能以接口可编译代替恢复语义。

## 4. Memory 3.0 最终模型

Memory 当前链路是：

```text
runtime/gateway/compaction command
  -> structured_memory_facts (canonical fact)
  -> agent_memory_items (same-transaction projection)
  -> memory_extraction_outbox / memory_consolidation_batches
  -> leased governance worker
  -> conservative global canonical fact + projection
```

关键规则：

1. projection 无对应 `current=1` canonical fact时不能参与 active recall。
2. update、delete、supersede、forget 先改变 canonical 状态，并在同事务维护 projection。
3. 自动抽取只接受 clean session；`polluted` 和 `disabled` 都 fail-closed。
4. 全局晋升只接受有证据、满足 lifecycle/authority 规则且未污染的事实。
5. extraction、consolidation、embedding 三类 outbox 都带 lease expiry、attempt/backoff、fencing 和幂等 source window。
6. 晋升事务、projection 更新和 outbox settlement 原子提交；worker 崩溃后 lease 到期可安全重放。

这吸收了 Codex phase-1/phase-2 的运行纪律，同时保留 AOS 的 typed fact、evidence、sensitivity、tenant isolation、supersession 和污染闭环。

## 5. 上下文、压缩和请求真实性

上下文“看起来一样”不足以恢复。AOS 现在分别记录：

- Context Manifest：选择后的分层上下文及 iteration。
- Tool Schema Manifest：最终 provider 可见的 canonical JSON schema，而非工具名数组。
- Prompt Manifest：stable system、message surface、context/tool manifest 和 model 的不可变绑定。
- Provider Attempt：每次 retry/fallback 的 request hash、model、context、prompt、tool schema IDs。

压缩 manifest 的 source coverage 只包含本次实际被替换的 message units。parent compaction references 使后续嵌套压缩可递归追溯；archive、replacement 和 memory candidates 均加密，并进入统一 key registry。

## 6. Agent 编排与 durable control plane

- `AskUserQuestion` 是 runtime 特殊控制工具，不允许普通 tool executor 绕过 suspend/resume 协议。
- Deferred 和 Completed 是不同 durable tool outcomes，幂等键包含 outcome state，恢复后不会把完成结果误判成旧的 deferred 记录。
- turn terminal、预算 settlement、事件和 checkpoint 以一个命令提交；网络 lost-ack 可重复提交同一确定性状态。
- capability、审批、child thread、artifact 和 budget 继续复用既有 durable plane，本轮没有引入第二套 orchestration authority。
- PM Confirmed/Accepted 的 authority 已按风险分级；Confidential/Secret 只接受 User/Owner，模型/工具不能自我批准决策。

## 7. NL2SQL 支持边界

本轮消除了最危险的一类误放行：SQL 注释、相似字段名、字符串包含或同义词族不能再伪造 denominator、population、filter、时间边界和时区证明。验证依据 AST 节点和等价子树。

当前可声明的是“对已建模 contract 的 supported scope 做结构化、fail-closed 语义校验”。复杂 CTE column lineage、任意 window rewrite、数据库特有表达式和一般关系代数等价仍应返回澄清/拒绝，不能宣称对任意 SQL 做完备定理证明。这个限制是安全边界，不是静默回退。

## 8. 密钥轮换覆盖

统一 registry 覆盖：API keys、bot channel secrets、Ledger raw payload、context raw manifest、provider tool schema、tool schema manifest、compaction 的 archive/replacement/memory candidates、GitLab repository token、datasource config envelope 和 durable question answer，共 12 个字段。

轮换使用 compare-and-set 更新，异构 legacy envelope 有明确 decoder/migrator。job 只有在 registry 的旧 key reference count 为零时才能完成；无法解密的 legacy 数据使任务失败，不允许假装已退役。

## 9. 验证与证据产物

发布前在干净 CI runner 执行：

```bash
cd rust
cargo fmt --all -- --check
cargo check --workspace --all-features
cargo test --workspace --all-features
cd ..
scripts/check-semantic-kernel-behavior.sh
git diff --check
```

行为门禁会生成 `target/conformance/semantic-kernel-behavior.jsonl`。该文件逐 case 记录 production reference、源码 hash、trace anchor 和实际通过的测试，便于 code review/CI 保存为 artifact。

本轮最终实现工作树执行并通过：

- `cargo fmt --all -- --check`；
- `cargo check --workspace --all-features`；
- `cargo test --workspace --all-features`，其中 `web-server` 1172 passed、`runtime` 561 passed、`nl2sql-core` 86 passed、`pm-domain` 70 passed、`eval-harness` 61 passed、`memory-engine` 6 passed，进程故障集成测试 2 passed；仅跳过明确要求外部 API、网络、真实 Trino 或本地 ONNX 环境的测试；
- WebUI `npm run typecheck`、38 个测试文件/167 个 Vitest 测试和 `npm run build`；
- `scripts/check-semantic-kernel-behavior.sh`，40/40 case 均执行真实生产路径并产生唯一 production trace，包括进程 kill/restart 和 key rotation/restart；
- `git diff --check` 与冲突标记扫描。

这些数字记录本次实现基线的本地可复现结果，不替代后续 CI，也不应被复用为未来提交的永久绿灯。

## 10. 仍需外部证据、但不应伪装成代码缺口的项目

- 更广的跨 OS/文件系统 process kill/restart 矩阵及不同 SQLite busy/IO fault 条件；当前 deterministic kill/restart TCK 已通过；
- 新存储 adapter 的 backend-neutral contract suite（当前正式支持边界为 SQLite）；
- Memory/Compaction hidden facts 的 precision、recall、false-memory、forgetting 和 continuation；
- PM 的 critical omission、unsupported claim、提问负担、返工和 calibration error；
- NL2SQL hidden corpus 的 semantic execution accuracy 和错误释放率；
- 固定相同 model/tools/permission/data/budget 的 AOS/Codex/DSH 去品牌盲测及 95% CI。

这些是发布质量和领先声明的 measurement gate。它们不会通过再增加一层抽象自然消失，也不应成为无限扩张架构的理由。

## 11. 开源发布声明

当前代码适合以可审计的 `0.x` 开源版本发布，并可准确声明 durable multi-tenant interaction、原子 session recovery、canonical typed memory 与持久化两阶段学习、proof-carrying exact-window compaction、四层 request lineage、registry-complete key migration、supported-scope AST NL2SQL verifier、PM authority/calibration 基础和 production-linked conformance evidence。

在第 10 节盲测完成前，不写“全面超过 Codex/DeepSeek Harness”“业界第一”或不带范围的 `production-ready`。最有说服力的开源姿态是公开不变量、迁移、负向 fixture 和可复现结果，让外部评审者能主动证伪，而不是靠宣传语替代证据。

## 12. 后续变更的关闭协议

任何新核心能力只有同时具备以下 bundle 才算完成：

```text
production entry
  + one canonical owner
  + atomic transaction or explicit recovery protocol
  + authorization and idempotency
  + negative/fault evidence
  + migration and rollback boundary
  + measured domain effect (when the claim concerns quality)
```

新增 P0 必须证明存在无法归入现有 interaction、memory、terminal、compaction、manifest、crypto、NL2SQL 或 TCK 的数据丢失、越权、不可恢复或业务语义错误。普通 helper、UI、缓存和上游目录变化不构成新的一级架构缺口。
