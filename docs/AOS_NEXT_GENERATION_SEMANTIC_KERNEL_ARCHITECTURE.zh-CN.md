# AOS 下一代语义内核架构与顶级效果验收规格

> 状态：Developer review candidate
> 日期：2026-08-17
> AOS 审计基线：`7a8b14fad65be817816757df11c3b7db0e5ce479`
> Codex 对照基线：`89e297729ebc1a8c243cef73c7ab64cd842ecd5a`
> DeepSeek Harness 对照基线：`47f943859bef60e4160492346772ded9b24f765a`
> 当前缺口事实源：`docs/AOS_SEMANTIC_KERNEL_GAP_AUDIT.zh-CN.md`
> 既有总体规格：`docs/AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md`
> 适用团队：Runtime、Gateway、Web、Memory、PM、NL2SQL、安全、评测、前端和开源发布

## 0. 文档结论与使用规则

本文不是再增加一套抽象，也不是用更多 Agent、Prompt、表或微服务证明 AOS 更强。本文的目标是把当前审计发现的缺陷收敛成一套可以直接开发、迁移和验收的下一代内核，并建立真实效果超过 Codex 和 DeepSeek Harness（下文简称 DSH）的证据标准。

AOS 应追求的不是“所有方向都复制 Codex”，而是：

1. 通用 Harness 的正确性、恢复性、安全性和效率至少不弱于 Codex/DSH；
2. Memory 与 Compaction 在长期业务事实、冲突、时效和污染控制上形成可测优势；
3. 产品需求挖掘在需求遗漏、证据质量、提问成本和最终可评审性上形成显著优势；
4. NL2SQL 在业务口径正确性而非“SQL 能运行”上形成显著优势；
5. 所有优势可在相同模型、工具、数据、权限和预算下重放，而不是来自更贵模型或更多人工配置。

本文与其他文档冲突时，判定顺序如下：

1. 当前代码和可复现测试结果；
2. `AOS_SEMANTIC_KERNEL_GAP_AUDIT.zh-CN.md` 的最新源码审计；
3. 本文的目标设计和验收要求；
4. 旧重构规格、自检、矩阵或发布说明。

如果研发已在审计基线之后修复某项缺口，应补充生产路径、事务边界、故障注入和效果证据后关闭对应任务，不应仅凭类型、表、测试名称或注释存在而关闭。

## 1. “赶超”的可测定义

### 1.1 三道门槛

任何“领先”声明必须同时通过三道门槛：

| 门槛 | 含义 | 不可替代的证据 |
| --- | --- | --- |
| 内核正确性 | 状态、事务、恢复、安全和权限在故障下仍成立 | 进程级 kill/restart、故障注入、重放、损坏恢复和后端协议 TCK |
| 领域效果 | Memory、PM、NL2SQL 的答案语义正确且用户成本合理 | 隐藏答案集、业务专家标注、确定性 verifier 和人工盲评 |
| 相对优势 | 在相同资源条件下优于当前最强对手 | 同模型、同工具、同权限、同数据、同 Token/时间预算的配对实验 |

只完成架构和测试，不等于效果领先；只赢少数 Demo，不等于内核领先；只提高效果但成本、延迟或拒绝率失控，也不算真实领先。

### 1.2 首版发布目标

以下是本次重构的候选硬门槛。评测委员会可以基于公开基线提高门槛，但不得在结果出来后为了发布而静默降低。

| 领域 | 绝对门槛 | 相对门槛 |
| --- | --- | --- |
| Harness 恢复 | 所有声明的故障点 100% 保持 Ledger、projection、checkpoint、预算和 next request hash 一致；外部副作用遵守 adapter 声明的幂等语义，未知结果不自动重试 | Codex/DSH 可执行的同类恢复集不得出现 AOS 独有失败 |
| Memory | 事实 precision `>= 97%`、关键事实 recall `>= 95%`、false-memory rate `<= 0.5%`、污染内容未经独立确认晋升率 `0`、supersession/forgetting 状态准确率 `>= 99%` | 综合质量相对最佳对手至少 `+5` 个百分点，且 95% 置信区间下界大于 `0` |
| Compaction | 关键约束 recall `>= 99%`、无证据新增事实率 `<= 0.5%`、压缩后任务继续成功率 `>= 95%`、有效 Token 降幅 `>= 40%` | 同预算下任务继续成功率高于最佳对手，或成功率不降且成本至少降低 `15%` |
| PM | 关键需求遗漏率 `<= 5%`、证据支持 precision `>= 95%`、无支持确定性断言率 `<= 1%`、专家判定可评审率 `>= 85%` | 质量得分相对最佳对手至少 `+5` 个百分点，完成同等质量的用户回答负担不高于对手 |
| NL2SQL | 冻结工作负载的可证明支持覆盖率 `>= 90%`、支持子集 semantic execution accuracy `>= 95%`、关键口径错误释放率 `<= 0.5%`、无法证明时正确拒绝/澄清率 `>= 99%` | semantic accuracy 相对最佳对手至少 `+5` 个百分点，执行成本和澄清轮次不显著恶化 |
| 综合体验 | 任务完成率、恢复可理解性和用户控制感达到发布基线 | 任务完成率至少 `+5` 个百分点；p95 延迟和平均成本原则上不得劣化超过 `15%` |

相对门槛使用配对 bootstrap 或等价统计方法报告置信区间。样本不足、测试集泄漏、模型版本不一致或工具权限不一致时，结果只能标记为 `inconclusive`。

## 2. 当前必须关闭的缺口

### 2.1 P0 正确性缺口

| ID | 当前缺口 | 本文目标章节 |
| --- | --- | --- |
| K-P0-01 | `AskUserQuestion` 没有 durable create/answer/consume/restart 协议 | 第 6 节 |
| K-P0-02 | Memory structured fact 与 projection 双权威、跨事务写入、删除/替代/污染不完整 | 第 7 节 |
| K-P0-03 | turn terminal 与完整 Session checkpoint 分事务提交 | 第 10 节 |
| K-P0-04 | Compaction provenance 绑定线程全历史而非本次 archive window | 第 8 节 |
| K-P0-05 | Prompt Manifest 未绑定最终 wire tool schema 和具体 provider attempt | 第 9 节 |
| K-P0-06 | key rotation 未覆盖全部密文，datasource/Git token 没有统一 key-id/退役协议 | 第 14 节 |
| K-P0-07 | NL2SQL 关键语义仍依赖字符串和 `contains` 启发式 | 第 13 节 |
| K-P0-08 | behavior gate 没有证明 dataset 的生产符号存在且被真实命中 | 第 15 节 |

### 2.2 P1 效果和竞争力缺口

| ID | 当前缺口 | 本文目标章节 |
| --- | --- | --- |
| K-P1-01 | PM 单一 Tool 证据即可确认高影响断言，authority policy 偏弱 | 第 12 节 |
| K-P1-02 | PM 信息增益的 prior/posterior 主要来自模型自报，未校准 | 第 12 节 |
| K-P1-03 | Memory 缺自动全局 consolidation lease、污染排除和遗忘闭环 | 第 7 节 |
| K-P1-04 | 缺 backend-neutral persistence/replay/process fault TCK | 第 15 节 |

### 2.3 当前源码改造入口

以下位置是审计基线上的直接改造入口。研发应以最新代码重新定位符号；符号移动不代表问题自动关闭。

| Workstream | 当前主要入口 | 关闭缺口时必须提交的证据 |
| --- | --- | --- |
| Durable Question | `rust/crates/web-server/sqlite-migrations/0033_semantic_kernel_completion.sql`、`rust/crates/agent-gateway/src/runtime_builder.rs` | create/answer/consume API、真实 Gateway/Web restart e2e、重复响应和越权测试 |
| Memory single writer | `rust/crates/web-server/src/routes/memory_continuity.rs`、`rust/crates/agent-gateway/src/runtime_builder.rs` | 所有写入口清单、统一 Repository 调用图、事务故障注入和 projection rebuild |
| Atomic terminal/checkpoint | `rust/crates/runtime/src/conversation.rs`、`rust/crates/web-server/src/semantic_kernel_store.rs` | 单事务 API、逐边界 kill/restart、next request hash 对比 |
| Compaction provenance | `rust/crates/web-server/src/routes/super_assistant.rs`、`rust/crates/web-server/src/semantic_kernel_store.rs` | exact source window、三次嵌套 compaction fixture、越界/缺失 sequence fail closed |
| Prompt/Wire lineage | `rust/crates/runtime/src/conversation.rs`、`rust/crates/agent-gateway/src/runtime_builder.rs` | final JSON schema hash、attempt FK/ID、ToolSearch/权限/retry/fallback 测试 |
| Key rotation | `rust/crates/web-server/src/semantic_kernel_store.rs`、`rust/crates/web-server/src/routes/data_sources.rs`、`rust/crates/agent-gateway/src/gitlab.rs` | registry 覆盖报告、无损迁移、旧 key 引用为零和移除旧 key 后 e2e |
| NL2SQL proof | `rust/crates/nl2sql-core/src/semantic_ir.rs` | AST/relational plan proof、adversarial corpus、关键路径无 substring 放行 |
| Behavior gate | `scripts/check-semantic-kernel-behavior.sh`、`eval/datasets/semantic-kernel-conformance.json` | production symbol/trace/assertion/process evidence，故意改走 fake 时 CI 失败 |
| PM authority/calibration | `rust/crates/semantic-core/src/reducer.rs`、`rust/crates/web-server/src/routes/agent/agent_pm_persist.rs`、`rust/crates/pm-domain/src/requirement_state.rs` | risk-based authority policy、历史校准集、question outcome 和 delivery gate 测试 |

## 3. 不变量先于模块

所有实现都必须服从以下内核不变量。违反任意一条时应 fail closed，不得用补偿 Prompt 掩盖。

1. **唯一事实源**：运行状态由 canonical execution events 决定；语义状态由 canonical semantic events 决定。表、缓存、搜索索引和摘要都只是 projection。
2. **单命令单事务**：一个用户可观察状态转换必须在一个数据库事务中提交 canonical event、必要 projection、预算变化和 outbox intent。
3. **外部副作用有意图**：调用 provider、工具、外部授权前先 durable 记录 intent；通过 idempotency key、claim lease 和 outcome 实现可恢复的 at-most-once 或显式 at-least-once 语义。
4. **摘要不是证据**：Compaction replacement、Memory summary 和模型输出不能成为最终事实源，必须能追溯到 exact archive、artifact、用户输入或权威数据。
5. **LLM 只提议**：状态转换、权限、预算、schema、证据准入和领域发布由确定性代码裁决。
6. **无法证明就澄清或拒绝**：NL2SQL 语义、关键需求、授权和恢复状态不明确时，不得猜测后继续。
7. **重放确定性**：相同 canonical events、schema 版本和 reducer 版本必须生成相同 projection hash。
8. **租户和主体全链路隔离**：tenant、user、session、turn、capability scope 不能只在 HTTP 层校验，Repository 和 command handler 必须再次校验。
9. **敏感数据最小投影**：raw、model、client、telemetry 使用不同 projection；密文可轮换、可审计、可删除。
10. **效果声明有证据**：没有固定环境和盲测结果时，只能声明机制已实现，不能声明超过竞品。

## 4. 目标架构与所有权边界

```text
HTTP / CLI / Bot / IDE adapters
             |
             v
Command API + Identity/Policy/Capability/Budget checks
             |
             v
Canonical Semantic Kernel Transaction
  +---------------- Execution Event Store ----------------+
  | Thread / Turn / Interaction / Tool / Child / Checkpoint|
  +---------------- Semantic Event Store -----------------+
  | Assertion / Decision / Requirement / Metric / Memory   |
  +---------------- Evidence & Artifact Plane ------------+
  | Exact archive / tool result / source / citation        |
  +---------------- Transactional Outbox -----------------+
             |
             v
Deterministic reducers and rebuildable projections
             |
             v
Context Compiler -> Prompt/Tool/Wire Manifests -> Provider
             |
             v
Memory Engine / PM Kernel / NL2SQL Semantic Compiler
```

### 4.1 逻辑所有权

第一步先建立 Rust API 和依赖方向，不要求立即拆成大量 crate。只有边界稳定且负载证明需要时再物理拆分。

| 边界 | 唯一职责 | 禁止承担的职责 |
| --- | --- | --- |
| `agent-protocol` | command/event 类型、状态机、idempotency、interaction/tool/child/checkpoint contract | HTTP、SQLite、具体 provider、业务 Prompt |
| `semantic-core` | assertion/decision/evidence/requirement/metric 类型和纯 reducer | 数据库、模型调用、向量检索 |
| `semantic-kernel-store` | 原子事务、event append、projection、outbox、lease、replay | 业务启发式、Prompt 拼装 |
| `memory-engine` | candidate extraction、admission、consolidation、retrieval、forgetting | 直接绕过 Repository 写表 |
| `context-compiler` | 预算内选择、排序、redaction、manifest 编译 | 修改 canonical state |
| `pm-domain` | requirement state、authority policy、question policy、delivery verifier | 直接信任模型自报置信度 |
| `nl2sql-core` | canonical IR、relational plan、semantic proof、result invariant | 只用 SQL 字符串相似度放行 |
| Gateway/Web/CLI | 协议 adapter、传输和 UI | 自建第二套状态机或直接写内核表 |

### 4.2 依赖方向

允许：`adapter -> command service -> domain/kernel -> store interface`。

禁止：

- Gateway 和 Web 各自维护 Memory、Approval、Question 或 checkpoint 写入逻辑；
- domain crate 反向依赖 HTTP route；
- projection table 触发 canonical event；
- Prompt 或 model output 直接更新 current fact；
- 测试复制一份 production reducer 后只验证复制实现。

## 5. Canonical Event-Sourced Semantic Kernel

### 5.1 双事件流，不做巨型统一事件

运行事实和语义事实需要分层，但通过强引用关联。

```rust
struct EventEnvelope<T> {
    event_id: EventId,
    stream_id: StreamId,
    stream_revision: u64,
    global_sequence: u64,
    tenant_id: TenantId,
    actor: ActorRef,
    occurred_at: Timestamp,
    schema_version: u32,
    causation_event_id: Option<EventId>,
    correlation_id: CorrelationId,
    idempotency_key: Option<IdempotencyKey>,
    payload_hash: Digest,
    payload: T,
}
```

`ExecutionEvent` 记录系统实际发生的动作，例如 turn started/terminal、interaction requested/responded、tool intended/started/settled、checkpoint committed、child spawned/cancelled。

`SemanticEvent` 记录系统当前相信什么以及为何改变，例如 candidate extracted、evidence attached、assertion confirmed、fact superseded、requirement resolved、metric contract versioned。

每个 `SemanticEvent` 必须引用至少一个 source evidence 或 execution event；高影响状态转换还必须记录 authority policy 结果。

### 5.2 Command Handler 标准流程

每个状态修改必须经过统一 command handler：

1. 解析 tenant/user/session/turn scope；
2. 校验 expected stream revision；
3. 校验权限、capability、预算和 idempotency；
4. 读取当前 canonical state；
5. 调用纯函数 `decide(command, state) -> events | rejection`；
6. 在同一事务 append events、运行 reducers、更新 projection、预算和 outbox；
7. commit 后 dispatcher 才允许执行外部副作用；
8. outcome 作为新 command 返回，不允许 dispatcher 直接改状态。

并发冲突必须返回显式 revision conflict 并重新决定，禁止 last-write-wins。

### 5.3 Projection 与快照

- projection 必须记录 `last_global_sequence`、`reducer_version` 和 `projection_hash`；
- 任意 projection 都可从 event store 和 artifact/evidence 引用重建；
- snapshot 只是加速点，必须带 source sequence range 和 state hash；
- schema upcast 必须是纯函数并有 golden fixture；
- canonical event 不因 projection 迁移而改写；
- telemetry、缓存命中和可丢弃 UI 状态不进入 canonical event store，避免无价值 event sourcing。

## 6. Unified Durable Interaction Protocol

Approval、用户提问、凭证请求和外部 OAuth/授权本质上都是“turn 因等待外部主体而暂停”。必须统一协议，UI 只根据 kind 渲染不同控件。

### 6.1 类型和状态机

```rust
enum InteractionKind {
    Approval,
    UserQuestion,
    CredentialRequest,
    ExternalAuthorization,
}

enum InteractionState {
    Pending,
    Responded,
    Granted,
    Rejected,
    Expired,
    Cancelled,
    Consumed,
}
```

合法主路径：

```text
create -> Pending -> suspend turn
Pending -> Responded/Granted/Rejected/Expired/Cancelled
Responded/Granted -> claim resume -> Consumed
```

`Consumed` 是 exactly-once resume 的终态。重复 answer、grant 或 resume 必须返回原结果，不能制造第二个 tool dispatch 或 turn。

### 6.2 持久化要求

建议以统一 `durable_interactions` 替代每种交互独立状态机，至少包含：

- `interaction_id/kind/state`；
- tenant/user/session/turn/invocation scope；
- request schema、choice schema 和 display projection；
- owner/allowed responders；
- capability requirement；
- expires_at、created event、response event、consumed event；
- idempotency key 和 expected turn revision；
- encrypted secret reference，而不是凭证明文。

凭证响应只保存 Secret Store 的 opaque reference。任何 event、trace、SSE 和 model context 都不得出现凭证明文。

### 6.3 事务要求

- create interaction、turn suspended、tool waiting projection 和 Ledger event 同事务；
- response、权限复检和 response event 同事务；
- consume、turn resume intent 和一次性 capability 消耗同事务；
- restart 后 dispatcher 只扫描 committed outbox，不扫描 UI projection 猜测待恢复动作；
- expire/cancel 也必须生成 terminal interaction event，并决定 turn 是继续、降级还是终止。

### 6.4 P0 验收

真实 Gateway/Web 进程执行：create -> kill -> restart -> answer -> resume。对每个事务前后 kill，验证 pending 可见、未重复回答、未重复 dispatch、未泄漏答案或凭证、owner 和 expiry 在恢复时重新校验。

## 7. Memory 3.0：唯一事实源、两阶段学习和污染免疫

### 7.1 权威模型

`structured_memory_facts` 或其继任的 canonical assertion stream 是唯一事实源。`agent_memory_items`、embedding、lexical index、summary、relation graph 都是可重建 projection。

所有入口必须使用同一个 `MemoryRepository/MemoryTransaction`：

- Web 手工新增/修改/删除；
- Gateway `memory_note`；
- compaction hook；
- PM/SQL 领域事实晋升；
- phase-1 extraction；
- phase-2 consolidation；
- forgetting 和用户纠正。

Repository 之外直接写 Memory 表应由代码所有权检查、SQL 权限或测试阻断。

### 7.2 事实模型

```rust
struct MemoryFact {
    fact_id: FactId,
    subject: EntityRef,
    predicate: PredicateRef,
    object: TypedValue,
    scope: MemoryScope,
    lifecycle: FactLifecycle,
    valid_time: TimeRange,
    recorded_at: Timestamp,
    confidence: CalibratedScore,
    authority: AuthoritySet,
    evidence: Vec<EvidenceRef>,
    conflict_set_id: Option<ConflictSetId>,
    supersedes: Vec<FactId>,
    source_event_ids: Vec<EventId>,
    sensitivity: SensitivityClass,
}
```

时间必须至少支持 valid time 和 recorded time。用户说“从下月开始时区改为 UTC”不能覆盖过去事实；迟到证据不能伪装成当时已经知道。

### 7.3 生命周期

```text
Candidate -> Quarantined -> Confirmed -> Superseded -> Forgotten
     |             |             |
     +-----------> Rejected <-----+
```

- `Candidate`：模型或规则抽取，尚未获得足够 authority；
- `Quarantined`：来源受污染、冲突、外部低可信或可能包含 prompt injection；
- `Confirmed`：满足当前事实类型的 authority policy；只有此状态可进入默认长期上下文；
- `Superseded`：被新版本替代，仍可按历史时间查询；
- `Forgotten`：因用户删除、保留期、低价值衰减或合规策略退出默认检索；
- `Rejected`：被证明错误、重复、越权或无法建立证据。

“current”不应成为独立可随意写入的布尔事实源；它由 lifecycle、valid time、scope 和 conflict resolution 确定性派生。

### 7.4 Evidence Authority

建议的默认等级：

1. Owner/User 显式确认；
2. tenant 管理的权威系统或已签名数据合同；
3. 两个相互独立、可定位且一致的来源；
4. 单一工具返回或外部网页；
5. 模型推断。

不同 fact type 定义自己的 admission policy。用户偏好可由用户单源确认；财务口径、权限、法律约束和高影响产品决策不得由单一 Tool 或模型确认。

独立来源按 source lineage 判断，两个引用同一原始网页的搜索结果不能计为两个来源。

### 7.5 Phase 1：局部候选抽取

在 turn terminal 或 compaction commit 后执行 bounded extraction：

1. 只读取本次 source window 和已授权 baseline；
2. 先做 secret/PII/prompt-injection admission；
3. 生成 typed candidates、evidence spans 和 temporal hints；
4. deterministic schema 校验；
5. polluted source 默认进入 quarantine；
6. 在同一事务 append candidate events、更新 cursor 和 extraction checkpoint；
7. 重试使用 source window hash 作为 idempotency key。

Phase 1 不直接重写全局 summary，也不因为模型高置信就自动确认高影响事实。

### 7.6 Phase 2：全局 Consolidation

吸收 Codex 的两阶段思路，但落到 AOS 的 structured semantic state：

- tenant-scoped lease，带 fencing token；
- cursor 按 canonical sequence 增量前进；
- retry/backoff/cooldown 和 poison-batch 隔离；
- 跨 thread 去重、冲突聚类、时间合并和 supersession；
- 已污染 thread/candidate 默认排除；
- 高影响冲突触发 durable user question，不自动选边；
- consolidation 输出 events，不直接改 projection；
- worker crash 后相同 batch 可幂等重放。

### 7.7 污染治理

不能只保存一个 thread-level `polluted` 标记。每条 evidence/fact 都要携带 source trust 和污染 lineage。

以下来源默认 quarantine：

- 未验证网页或搜索摘要中的指令性内容；
- 外部文件内要求改变系统行为的文本；
- 与用户/系统策略冲突的记忆写入请求；
- 无法定位原文 span 的模型总结；
- 从被撤销 capability 获得的内容。

晋升必须使用不共享污染 lineage 的新证据或用户显式确认。污染内容进入默认长期上下文的允许率为 `0`。

### 7.8 检索与上下文注入

检索分两步：candidate retrieval 和 policy rerank。

最终分数至少考虑 lexical/vector relevance、authority、recency、valid time、scope、conflict、lifecycle、sensitivity 和当前任务类型。默认只注入 Confirmed/current；历史、冲突和 quarantined 仅在任务明确需要时以带标签的 evidence packet 注入。

返回模型的每条事实必须带 `fact_id/version/evidence_ref`，使模型输出可以反向引用，而不是只收到不可审计文本块。

### 7.9 Projection 重建和删除

- create/update/delete/supersede/enable/disable/forget 必须以 event 表达；
- structured state、search projection、relation、citation、cursor 同事务更新；
- 删除 projection 不能删除 canonical history，除非合规擦除策略要求 crypto-shredding；
- 提供 `rebuild_memory_projection --tenant --verify-hash`；
- 重建前后 current fact set 和 retrieval golden results 必须一致；
- structured table 缺失、版本不兼容或 projection hash 错误时 fail closed，禁止 projection-only 降级。

## 8. Proof-Carrying Compaction

### 8.1 Compaction 是受证明的状态变换

一次 compaction 必须形成不可变 `CompactionManifest`：

```rust
struct CompactionManifest {
    compaction_id: CompactionId,
    thread_id: ThreadId,
    source_window: SourceWindow,
    source_event_sequences: Vec<u64>,
    source_message_ids: Vec<MessageId>,
    source_archive_hash: Digest,
    parent_compactions: Vec<CompactionId>,
    replacement_artifact_id: ArtifactId,
    replacement_hash: Digest,
    extracted_fact_ids: Vec<FactId>,
    baseline_manifest_id: ContextManifestId,
    source_tokens: u64,
    replacement_tokens: u64,
    proof_result: CompactionProofResult,
}
```

`source_window` 必须精确表示本次 archive 的起止边界。禁止把线程全部 Ledger sequence 绑定到每次 compaction。

### 8.2 嵌套溯源

第二次压缩如果包含第一次 replacement，应引用 parent compaction，形成 DAG。查询原始证据时递归展开到 exact archive；不得把 parent 覆盖的原事件重新声明为本次直接 source，也不得丢失边界。

### 8.3 Prepare/Commit/Abort

1. `prepare` 锁定 source window、expected revision 和 source hash；
2. 在事务外生成 replacement 和 candidates，但不能改变 current view；
3. deterministic proof 校验 replacement、evidence spans、边界和 Token；
4. `commit` 在一个事务写 archive、manifest、replacement projection、memory candidate events、cursor、checkpoint 和 Ledger event；
5. revision/source hash 变化则 abort 并重新选择窗口；
6. crash 后 recovery 根据 transaction state 决定 resume、abort 或 verify，不得生成半替换会话。

### 8.4 Loss/Growth Gate

发布前至少检查：

- replacement tokens 必须显著小于 source，建议 `<= 60%`；
- 当前任务、未决 interaction、用户硬约束、关键数字、决策和 tool outcome 覆盖率 `100%`；
- replacement 中每个确定性事实都能定位 source span/fact；
- unsupported assertion 为 `0` 才能 commit；
- span boundary 和 message ordering 稳定；
- 被 pruning 的 tool result 已保存完整 artifact 和 locator；
- probe 不得包含隐藏答案或关键词泄漏。

### 8.5 Context Baseline 重注入

吸收 Codex 的 canonical initial-context reinjection：压缩后必须重新编译当前 system/policy/user profile、workspace/datasource contract、active task、pending interaction、capability 和 budget baseline。不能假设旧摘要仍包含这些世界状态。

## 9. Context、Prompt、Tool Schema 与 Wire Manifest

### 9.1 四份不可混用的 Manifest

| Manifest | 记录内容 | 权威边界 |
| --- | --- | --- |
| `ContextManifest` | 被选中的 fact/evidence/artifact/message、顺序、版本、token、redaction 和选择原因 | provider 实际可见上下文 |
| `PromptManifest` | system/policy/domain template 版本、变量 hash、rendered prompt hash、semantic snapshot | Prompt 组成 |
| `ToolManifest` | 权限过滤、ToolSearch 激活和 provider 转换后的最终 JSON schema | 本次 attempt 可调用的真实工具面 |
| `WireAttemptManifest` | model、sampling、cache、request bytes hash、stream lineage、上述 manifest ID | provider 实际收到的请求 |

工具名称列表只能作为诊断字段，不能代替 final canonical JSON schema hash。

### 9.2 Attempt-specific Immutable Lineage

每次 retry、fallback、模型切换、cache variant 或 schema 降级都是新 attempt，必须保存自己的：

- `prompt_manifest_id/context_manifest_id/tool_manifest_id`；
- model/provider/endpoint 和 capability profile version；
- final request hash；
- final wire tool schema hash；
- parent attempt 和 retry reason；
- cache key/hit/miss；
- output/stream artifact 和 terminal status。

dispatch 前强制校验 manifest 的 tenant/session/turn、model、schema hash、context hash 和 request hash 属于同一 lineage。校验不通过不得调用 provider。

### 9.3 Prompt 设计

Prompt 分层保持短、稳定和可评测：

1. invariant system policy；
2. tenant/domain policy；
3. task contract；
4. semantic state packet；
5. evidence/artifact locator；
6. output schema。

不要把 Memory、权限、NL2SQL 口径或 PM 完整性规则只写在自然语言 Prompt。Prompt Registry 需要版本、owner、适用模型、回滚点、实验 ID 和离线 eval 结果；上线必须通过 paired canary。

## 10. Atomic Turn Terminal 与 Checkpoint Recovery

### 10.1 唯一提交 API

新增或等价实现：

```rust
finish_turn_with_checkpoint(FinishTurnCommand) -> FinishTurnResult
```

该命令在一个事务内完成：

- 校验 turn revision 和非 terminal；
- settlement 所有预算 reservation；
- 关闭未完成的同步 step，或把外部等待显式留为 suspended；
- append turn terminal event；
- append checkpoint committed event；
- 保存完整加密 Session recovery payload；
- 保存 semantic/context/prompt baseline references；
- 更新 turn/session projection；
- 创建后续 extraction/consolidation outbox intent。

禁止调用者先 `finish_turn` 再独立 `checkpoint_session`。

### 10.2 恢复判定

恢复时读取 canonical events 和最近有效 checkpoint：

- terminal event 和 checkpoint source revision/hash 匹配才可继续下一 turn；
- checkpoint 缺失、hash 不匹配或 decrypt 失败时进入 `RecoveryRequired`，不直接生成；
- 可从 event/artifact 重建时写 repair event 和新 checkpoint；
- 无法确定性修复时 fail closed，并向用户显示可理解的恢复状态；
- open tool intent 根据 idempotency/outcome 查询决定 settle、retry 或人工确认，不能默认为失败后重跑。

### 10.3 故障矩阵

至少在以下边界逐点 kill：terminal event 前、预算 settlement 后、checkpoint ciphertext 写入前后、transaction commit 前后、outbox claim 前后、provider/tool outcome 返回前后。每个点都比较 Ledger hash、projection hash、checkpoint hash、预算余额、pending interaction 和 next request hash。

## 11. Tool、Capability、Budget、Artifact 与 Child Runtime

### 11.1 Tool Intent 和幂等语义

每次工具调用必须经历：

```text
Proposed -> Authorized -> Intended -> Claimed -> Running -> Settled
                                              -> UnknownOutcome
                                              -> Cancelled/Expired
```

写 `Intended` 后才允许调用外部系统。工具 adapter 必须声明幂等等级：`NativeIdempotent`、`QueryBeforeRetry`、`AtLeastOnce`、`NonRetryable`。`UnknownOutcome` 不得自动重跑高风险副作用。

### 11.2 Capability

- capability token durable、带 tenant/actor/session/tool/resource/operation scope；
- child 只能继承父级交集，并受 expiry、revocation 和 depth 限制；
- Approval 是 capability 的一次性扩展，不是绕过 policy；
- executor、tool router 和 store 三层都校验 capability；
- shell 字符串分类只能作为风险提示，不能成为授权边界。

### 11.3 Budget Ledger

统一使用 `reserve -> commit/release`：

- Token、reasoning、context、output、tool call、外部费用、并发 slot、child depth；
- 父子预算守恒，child reservation 从父级可用余额扣除；
- settlement 幂等；
- terminal final response 可使用单独保留预算；
- 并发下禁止超卖；
- budget event 与 turn/tool event 同事务。

### 11.4 Artifact/Spill Plane

吸收 DSH 的 output retention 和 model-free tool result pruning：

- 所有大结果保存完整 typed artifact；
- model view 使用结构化 head/tail、表 schema、行数、准确省略量和 opaque locator；
- pruning 不调用模型，不改写原始结果；
- artifact tenant-scoped、加密、可分页、可过期、可删除；
- preview、raw、client、telemetry 使用不同投影；
- compaction/replay 通过 artifact ID 和 hash 引用，不复制大 payload。

### 11.5 Child Runtime

child spawn、capability、budget、checkpoint、cancel 和 settlement 都进入同一协议。不同 executor 不支持的能力必须显式 `Unsupported`，不能模拟成功。父级恢复时根据 child canonical state 重建，不从最终文本猜测 child 是否完成。

## 12. PM Domain Kernel：从会写 PRD 到能证明需求成立

### 12.1 Requirement State

PM 的 canonical state 至少包含：

- problem、target user、job/scenario；
- goal/non-goal、constraint、stakeholder；
- assumption、risk、open question；
- decision 和 rejected alternative；
- success metric、baseline、target、guardrail；
- evidence、experiment、outcome；
- dependency、priority、release slice；
- 每项状态、authority、valid time、conflict 和 source lineage。

聊天记录和最终 PRD 都只是该状态的输入/输出，不是事实源。

### 12.2 Authority Policy

按风险设置确认门槛：

| 类型 | 最低确认要求 |
| --- | --- |
| 用户偏好、措辞 | 用户显式回答或 owner 确认 |
| 业务目标、范围、优先级 | owner/user 确认；冲突时保留 decision record |
| 市场/竞品事实 | 可定位来源；高影响结论至少双源或 owner 接受为假设 |
| 数字、指标、合规、财务 | 权威数据合同/系统或 owner 明确确认 |
| 模型推断 | 只能是 assumption，不得直接成为 Confirmed |

单一 `Tool` 只能证明“工具返回了内容”，不能自动证明业务事实正确。

### 12.3 Calibrated Information Gain

下一问不能依赖模型自报概率直接排序。建议：

```text
question_value = calibrated_expected_decision_reduction
               + expected_risk_reduction
               + expected_rework_reduction
               - user_effort_cost
               - delay_cost
```

- prior/posterior 使用历史回答分布、相似任务命中率和真实 decision change 校准；
- 模型估计做 clipping、isotonic/Platt calibration 或分桶校准；
- 新领域无数据时明确使用 conservative prior；
- 记录“问了什么、用户是否回答、是否改变需求/决策、花费多久”；
- 提问停止条件由剩余关键风险和边际价值决定，不按固定问题数。

### 12.4 Delivery Verifier

最终输出前重新读取 durable Requirement State，并验证：

- 所有关键字段已 confirmed，或显式标为 assumption/open question；
- 高影响断言满足 authority policy；
- success metric 有定义、baseline/target、时间窗和 owner；
- 冲突未被静默覆盖；
- evidence citation 可打开且支持对应 claim；
- 未达到 PRD gate 时只能输出 Requirement Brief/Research Brief，不能伪装为完整 PRD。

### 12.5 PM 效果壁垒

真正需要积累的是匿名化 requirement evolution dataset、问题价值、遗漏标签、review change、上线结果和失败原因。不要把“更多研究 Agent”当壁垒；相同模型接入相同搜索工具很容易复制工作流，难复制的是状态、verifier、校准数据和反馈闭环。

## 13. NL2SQL Domain Kernel：从可执行 SQL 到业务语义证明

### 13.1 编译链

```text
Natural Language
  -> Canonical Analytics IR
  -> Bound Semantic Plan
  -> Candidate SQL AST
  -> Normalized Relational Plan
  -> Proof Obligations
  -> Policy/Cost/EXPLAIN
  -> Execute
  -> Result Invariants
  -> Answer with lineage
```

Canonical IR 首次确认后不可被 SQL 反向改写。repair 必须重新证明仍满足原 IR；无法证明时回到澄清，不允许用字符串相似度放行。

### 13.2 Bound Semantic Plan

至少绑定：

- metric definition、numerator、denominator、unit 和 aggregation；
- population subject、include/exclude 和 mandatory filters；
- grain、dimensions 和 ordering；
- time column、timezone、calendar、window 和 comparison period；
- dedup key 和 late-arrival policy；
- datasource/schema/contract version；
- join path、cardinality、fanout policy 和 null semantics；
- row-level security 和 tenant scope。

每个 logical symbol 必须绑定唯一 schema/contract symbol；歧义必须产生 clarification。

### 13.3 AST 到 Normalized Relational Plan

在现有 SQL AST 基础上构建符号表和关系代数节点：scan、filter、project、aggregate、join、window、distinct、set operation、CTE/subquery。完成 column lineage、alias resolution 和 expression normalization。

第一阶段只支持可证明的 SQL 子集。遇到 correlated subquery、复杂 UDF、动态 SQL、无法确定的 dialect behavior 或未知函数时，返回 `UnsupportedForSemanticProof`，不得退回 `contains` 启发式后释放。

### 13.4 Proof Obligations

verifier 必须逐项给出 `Proved/Disproved/Unknown`：

1. metric expression 与 contract 代数等价；
2. denominator 和 zero/null handling 正确；
3. population filter 完整且没有额外改变人群；
4. grain 与 group/project 一致；
5. dedup 在正确实体和时间范围执行；
6. timezone 和边界转换正确；
7. comparison period 等长、对齐且无重叠错误；
8. join cardinality 不引入 fanout 或有显式消除；
9. mandatory tenant/security filter 不可移除；
10. unit conversion 和 rounding 符合 contract；
11. repair 没有改变 canonical scope。

关键 obligation 只要出现 `Unknown` 就不得执行；可选展示类 obligation 可在带警告的 policy 下放行。

### 13.5 Result Invariants

SQL 执行成功后仍需验证：

- schema/type 与 IR 一致；
- unique grain 未重复；
- ratio 范围、分母、空值和单位合理；
- total 与分组 reconciliation；
- comparison rows 完整；
- row count/cost 不越界；
- data freshness 满足 contract；
- 关键异常触发停止或人工确认，而不是由模型自由解释。

### 13.6 Adversarial 数据集

必须覆盖“SQL 可运行但业务错误”：错误分母、错人群、错时区、错比较期、join fanout、重复用户、NULL 被丢弃、单位错千倍、最新合同版本未生效、repair 偷换指标、同名列绑定错误和 RLS 缺失。

## 14. Encryption、Key Registry 与可证明退役

### 14.1 统一 Ciphertext Registry

所有密文 store 注册：

```rust
struct CiphertextStoreDescriptor {
    store_id: &'static str,
    key_namespace: KeyNamespace,
    codec_version: u32,
    scanner: ScannerId,
    cas_rewriter: RewriterId,
    retention_policy: RetentionPolicy,
}
```

至少覆盖 API key、bot secret、Ledger raw payload、context manifest、provider tool schema、compaction archive/replacement/candidates、Session checkpoint、artifact、datasource config、Git token 和 durable credential reference。

### 14.2 Envelope

所有新密文携带 `key_namespace/key_id/codec_version/nonce/aad_hash/ciphertext`。AAD 至少绑定 tenant、store、row identity 和 schema version，防止跨行搬运。

### 14.3 Rotation 与退役

- rotation worker 按 registry 扫描，不维护手工列白名单；
- CAS update 防止覆盖并发新写；
- durable cursor、失败原因、重试和速率限制；
- active/retiring/retired/revoked key 状态；
- datasource/Git token 无损迁移，不要求用户重新保存；
- 只有所有注册 store 的旧 key 引用计数为零、抽样 decrypt 通过、备份策略确认后，才能生成 retirement certificate；
- 移除旧 key 后运行全恢复、replay、datasource 和 Git e2e。

## 15. Backend-neutral Protocol TCK、Replay 与故障注入

### 15.1 TCK 结构

吸收 DSH 的 persistence contract、cold repair、LLM replay 和 E2E fixture，形成 AOS 自己的公开 TCK：

```text
tck/
  protocol/
  persistence/
  interactions/
  compaction/
  memory/
  provider-replay/
  process-faults/
  security/
  fixtures/
```

同一 contract suite 必须能运行 SQLite store 和未来 store adapter。测试只依赖 public repository/command contract，不依赖 SQLite 私有 helper。

### 15.2 Provider Recorder/Replay

- 从真实 attempt 导出脱敏 request manifest 和完整 stream frames；
- 支持 partial frame、malformed frame、throw、hang、timeout、cancel、late result 和 duplicate terminal；
- fixture 带 expected request hash 和 `assert_consumed`；
- replay 不访问网络，可复现生产失败；
- model output 中的敏感原文使用受控 encrypted fixture，不进入公开仓库。

### 15.3 Process Fault Injector

测试必须启动真实 server/gateway 子进程，在命名 fault point 发送 kill，并重启同一数据库。至少覆盖 turn、interaction、tool、artifact、compaction、memory、rotation 和 migration。

单元测试中的 transaction rollback 不能替代进程级测试。

### 15.4 Behavior Gate 修复

`semantic-kernel-conformance.json` 的每个 case 必须输出：

- production file/symbol 存在；
- test symbol 存在；
- runtime trace marker 证明 production path 被命中；
- assertion 数量和关键 invariant；
- process fault case 的 exit/restart 证据。

故意删除生产符号、改走 fake helper、产生零 trace 或只匹配到测试名称时，CI 必须失败。

### 15.5 生产可观测性

正确性指标必须从 canonical state 派生，不能通过日志文本猜测：

- command conflict、stale writer、projection lag/hash mismatch；
- checkpoint repair、`RecoveryRequired`、open intent 和 `UnknownOutcome`；
- interaction pending age、expiry、duplicate response 和 resume latency；
- Memory candidate/quarantine/promotion/rejection、conflict age、consolidation lease/retry；
- compaction source/replacement ratio、proof rejection、archive expansion latency；
- manifest/schema/request hash mismatch 和 provider replay coverage；
- NL2SQL obligation 的 Proved/Disproved/Unknown 分布、支持覆盖率和错误释放；
- ciphertext registry coverage、rotation lag、per-key reference count 和 decrypt failure；
- outbox age、budget reservation leak、child settlement lag。

告警必须指向 tenant-safe opaque ID、runbook 和可执行修复动作。metrics/trace 不得携带 raw prompt、凭证、SQL 结果明文或用户原文；需要调试 exact payload 时走受审批的加密 Artifact 访问。

## 16. 无双权威的迁移方案

### 16.1 原则

迁移期间可以同时存在 canonical store 和兼容 projection，但不能存在两个可独立修改、可互相覆盖的事实源。禁止应用层分散 dual-write。

### 16.2 分阶段迁移

1. **Inventory**：冻结 schema 清单，枚举每个写入口、读入口、密文列、事件和 projection owner。
2. **Additive schema**：新增 event/interaction/manifest/registry 字段和表，默认关闭新路径。
3. **Offline backfill**：从当前权威记录生成带 `migration_source` 的 canonical events；记录 source row hash，不伪造历史时间和 authority。
4. **Verify**：比较 tenant 级数量、current fact set、supersession graph、checkpoint hash、检索 golden 和外键完整性。
5. **Single writer cutover**：按 tenant feature flag 将所有 command 路由到新 Repository；旧表仅由同事务 projection reducer 更新，直接写入口立即报错。
6. **Shadow read**：新 store 对外返回，旧 read 只做离线 diff，不参与业务决策，也不能回写。
7. **Projection rebuild**：从 events 重建旧兼容 projection，验证 hash 后移除旧 read。
8. **Cleanup**：至少经过一个发布周期和 rollback 演练后，删除旧写代码；是否删除旧表由保留和兼容策略决定。

### 16.3 Memory 特殊处理

- `structured_memory_facts` 优先作为当前结构化事实输入；
- 只有 `agent_memory_items` 存在的记录迁为 `Candidate`，不得自动标记 Confirmed；
- 两者冲突时创建 conflict set，保留两个 source row hash，交由 authority policy 或用户确认；
- 删除/disable 状态必须回填为 event，不能只迁移仍可见记录；
- embedding 全量重建，不继承无法证明与当前 fact version 对应的旧向量。

### 16.4 回滚

回滚只切换 reader/adapter 版本，不允许旧 writer 恢复独立写入。新 events 必须能继续投影到兼容 schema；无法向后表达的状态要在上线前阻断，而不是发布后丢弃。

## 17. 交付阶段与依赖

### P0：生产正确性和单一事实源

依赖顺序：

1. `P0-A` Command/Event/Repository 事务骨架和 projection hash；
2. `P0-B` atomic turn terminal + checkpoint；
3. `P0-C` Unified Durable Interaction；
4. `P0-D` Memory single writer、update/delete/supersede/pollution；
5. `P0-E` exact compaction window/proof manifest；
6. `P0-F` Context/Prompt/Tool/Wire attempt lineage；
7. `P0-G` Ciphertext Registry 和全量 rotation；
8. `P0-H` NL2SQL 可证明子集，删除关键 substring 放行；
9. `P0-I` backend-neutral TCK、production trace gate 和 process fault CI。

P0 结束条件：第 2 节八项 P0 全部有生产路径、事务证明、进程级故障测试和文档证据。未完成时只能发布 `preview/experimental`。

### P1：领域效果领先

1. Memory phase-1/phase-2、global lease、temporal conflict、forgetting；
2. PM authority policy、calibrated information gain、delivery verifier；
3. NL2SQL normalized relational plan、proof obligations、result invariants；
4. Compaction nested provenance、baseline reinjection、结构化 tool pruning；
5. 三领域隐藏评测集和 AOS/Codex/DSH adapter；
6. 用户可见的 Memory 纠正、证据、等待交互和恢复体验。

P1 结束条件：满足第 1.2 节绝对门槛，并完成至少一轮冻结版本的三方同条件盲测。

### P2：效率、自学习与生态

1. 基于真实 task outcome 的 context policy 和 question policy 学习；
2. provider/model-specific prompt/cache/schema 优化；
3. semantic snapshot 增量化和大租户索引优化；
4. 可选远程 store 和分布式 worker，但保持 TCK 一致；
5. 发布公开脱敏 replay fixtures、benchmark 和协议兼容包；
6. 用真实负载决定是否拆服务，不以组织边界预先微服务化。

## 18. 开发验收矩阵

| Workstream | 必须通过的验收 |
| --- | --- |
| Event Kernel | duplicate/out-of-order/conflict/upcast/rebuild；相同 events 得到相同 hash；stale writer 被 fencing |
| Interaction | approval/question/credential/OAuth 全部 create-kill-restart-respond-consume；重复响应和越权响应失败 |
| Memory | 所有入口单 Repository；第二写失败全回滚；projection 删除后可重建；污染不晋升；update/delete/supersede/forget 全链一致 |
| Compaction | 三次连续压缩的 source window 精确、parent DAG 可展开；边界/hash/Token/unsupported proof 任一失败都不替换 |
| Manifest | ToolSearch、权限裁剪、provider schema 转换、retry/fallback 下每个 attempt 的 schema/request hash 匹配 |
| Checkpoint | 所有 terminal/checkpoint fault point 重启后 next request、预算、状态和消息一致 |
| Tool/Budget | unknown outcome 不误重试；reservation 守恒；child 并发不超卖；canonical settlement exactly once；adapter 幂等语义可审计 |
| Artifact | 完整结果可分页恢复；model view 有准确省略量；跨租户访问失败；删除和 key rotation 可验证 |
| PM | 单 Tool 不能确认高影响 claim；冲突不覆盖；下一问校准记录完整；未过 gate 不产出完整 PRD |
| NL2SQL | 同义正确 SQL 通过；名称相似但分母/人群/时区/比较期/join 错误稳定拒绝；unsupported fail closed |
| Crypto | registry 覆盖扫描为 100%；旧 key 引用为零后移除旧 key，全链路仍可读 |
| TCK | SQLite/未来 adapter 同 contract；真实进程 kill/restart；fixture 未消费完、production trace 缺失时 CI 失败 |
| Migration | source row hash 可追溯；无独立旧 writer；shadow diff 归零；回滚演练不丢新状态 |

每个 workstream 的 PR 必须附：不变量、生产入口、事务边界、失败模式、测试证据、迁移影响、可观测指标和回滚方式。仅附 happy-path 单元测试不得标记完成。

## 19. 顶级效果评测设计

### 19.1 实验公平性

三方比较固定：

- 相同模型和版本，或相同能力/价格档位并单独报告；
- 相同 system policy 上限、工具、知识库、网络、数据库快照和权限；
- 相同最大 Token、wall-clock、并发和外部费用预算；
- 不向任一系统泄漏 rubric 或隐藏答案；
- raw trace、manifest、tool calls、成本和延迟全部保存；
- 输出去品牌化并随机排序，由至少两名标注者盲评；
- 分歧由第三方仲裁，报告 inter-rater agreement；
- 开发集和最终 test set 隔离，最终集在冻结提交后只运行一次。

### 19.2 Memory/Compaction

数据集覆盖跨周对话、事实更新、冲突、用户纠正、时效、删除、污染网页、相似实体、跨项目隔离和多次压缩。

指标：fact precision/recall、false memory、stale fact rate、pollution promotion、forgetting accuracy、evidence trace accuracy、关键约束保留、继续任务成功率、token/cost/latency。

### 19.3 PM

数据集由真实但脱敏的模糊需求、访谈材料、冲突利益方、缺失指标、错误市场资料和后续决策变化组成。

指标：critical omission、unsupported claim、evidence support precision、question utility、用户回答时间/字数/轮次、需求变更捕获、专家可评审率、返工量和最终决策一致性。

不能只让 LLM Judge 评价文风。关键字段、证据支持和 omission 使用结构化标注与专家复核。

### 19.4 NL2SQL

同时提供数据库、数据合同、隐藏正确结果和 adversarial SQL。指标以 semantic execution accuracy 为主：结果和业务口径同时正确才算通过。

分项报告 metric、population、grain、timezone、comparison、dedup、join、null、unit、security、clarification、repair stability、execution cost 和 calibration（ECE/Brier）。SQL parse/execute rate 只能作为诊断指标。

### 19.5 Harness 与体验

测量 end-to-end task completion、first useful action、p50/p95 latency、tokens、外部成本、工具次数、恢复成功率、重复副作用、用户等待可理解性、取消成功率和需要人工救援的比例。

### 19.6 领先判定

每个领域分别给出：absolute pass/fail、相对最佳对手差值、95% CI、成本差、延迟差和失败案例。AOS 只有在第 1.2 节对应门槛全部满足时，才能声明该领域领先；不得用某一领域胜利写成“全面超过 Codex/DSH”。

## 20. 产品体验要求

底层正确性必须转化为用户可感知的体验：

- Memory 可查看“系统记住了什么、来源、有效期和冲突”，并能一键纠正/忘记；
- 等待用户回答、审批、授权或凭证时，用户看到明确原因、影响、到期和恢复位置；
- crash/restart 后回到同一任务，不出现重复消息、重复 SQL、重复工具副作用或幽灵 turn；
- PM 只问会改变决策的高价值问题，已知内容不重复询问；
- NL2SQL 不确定时明确指出缺的是指标、人群、时间还是粒度，而不是笼统说“信息不足”；
- 业务结论可展开到证据、口径、SQL、结果和 verifier；默认界面保持简洁，不把内部事件流暴露给普通用户；
- 取消、修改、纠正和删除是一级能力，不是失败后的补丁按钮。

## 21. 不应复制的复杂度

以下内容不会自然提高效果，本次重构不应作为目标：

- 为对齐竞品目录而复制其模块或 crate 数量；
- 再增加一层 Planner/Reviewer/Agent 来弥补确定性 verifier 缺失；
- 把所有 telemetry、UI state 和缓存命中都事件化；
- 在单机事务尚未正确前提前引入分布式 event bus 和微服务；
- 用通用知识图谱替代清晰的 typed fact/requirement/metric contract；
- 用更长 Prompt 代替 authority、transaction、proof 和 benchmark；
- 为追求低拒绝率，在语义未知时退回字符串启发式；
- 把代码 Agent 的编辑器、patch UI 和 terminal 深度作为 AOS 当前核心竞争目标。

AOS 应吸收竞品已经证明有效的协议机制，而不是复制其产品定位。

## 22. 竞品精华与 AOS 的增强方式

| 来源 | 应吸收 | AOS 应进一步增强 |
| --- | --- | --- |
| Codex | phase-1/phase-2 Memory、global lease、污染排除、forgetting | typed temporal facts、authority policy、冲突/替代、tenant-scoped consolidation 和可见纠正体验 |
| Codex | compaction 后 canonical context reinjection、rollout reconstruction | proof-carrying exact window、nested provenance、semantic snapshot 和跨版本 golden replay |
| DSH | persistence contract、cold repair、backend-neutral invariant | execution/semantic 双事件流、统一 interaction、预算/capability 和公开 TCK |
| DSH | tool-result pruning、output retention/spill | typed encrypted Artifact Plane、完整 lineage、tenant policy 和精确删除/轮换 |
| DSH | LLM replay、compaction E2E fixtures | attempt manifests、真实 process fault injector 和领域语义 replay |
| AOS 现有优势 | PM/NL2SQL、企业权限、durable Ledger、Artifact、Context Compiler | 用 domain state/verifier 和真实反馈数据形成难复制的效果壁垒 |

如果某项竞品机制不能改善正确性、效果、体验、成本或迭代速度，就不应为了“看起来完整”加入。

## 23. 开源发布与声明策略

### 23.1 可立即作为正式版开源的最低条件

- 第 17 节 P0 全部关闭；
- workspace tests、strict clippy、behavior gate 和 process TCK 有最终退出码；
- migration、rollback、backup/restore、key rotation 和 threat model 文档齐全；
- 默认配置无 secret、无跨租户泄漏、无 silent fallback；
- public fixture 不含用户数据或不可撤销敏感内容；
- README 明确支持范围、unsupported/fail-closed 行为和 benchmark 条件；
- 安装后不依赖私有内部服务才能通过核心 smoke test；
- release artifact、schema version 和 fixture 可复现。

P0 未关闭时可以开源代码，但版本和 README 必须标记 `preview/experimental`，不能写“生产级完整内核”。

### 23.2 声明等级

| 等级 | 允许表述 |
| --- | --- |
| Mechanism implemented | 机制已进入生产路径并通过内部正确性测试 |
| Production hardened | 进程故障、迁移、安全和 TCK 已通过 |
| Domain benchmark leading | 某领域在冻结的同条件盲测中达到领先门槛 |
| Broadly leading | 多个目标领域在不同数据集和模型上重复领先 |

在盲测完成前，禁止“全面超过 Codex/DSH”“零丢失”“Memory 最强”“NL2SQL 保证正确”等绝对表述。

## 24. Definition of Done

本次大重构完成，不以代码合并数量判断，而以以下事实同时成立判断：

1. 当前八项 P0 缺口全部关闭，且旧写路径已被删除或技术性阻断；
2. canonical events 能重建核心 projection，turn/checkpoint/interaction/tool/compaction/memory 在 crash 后确定性恢复；
3. Memory 有唯一事实源、两阶段 consolidation、污染/冲突/时效/遗忘闭环；
4. Prompt、Context、最终 Tool Schema 和每次 provider wire request 有不可变 lineage；
5. PM 的高影响结论受 authority 和 delivery verifier 约束，下一问经过真实数据校准；
6. NL2SQL 对支持子集执行 AST/relational semantic proof，未知语义 fail closed；
7. 所有密文可统一轮换并证明旧 key 可退役；
8. backend-neutral TCK、provider replay 和真实进程 fault injection 进入 CI；
9. migration 没有双权威，回滚不会恢复旧 writer 或丢失新状态；
10. Memory、Compaction、PM、NL2SQL 和综合体验达到绝对门槛；
11. 三方同条件盲测达到相对门槛后，才发布对应领域的领先声明。

最终目标不是得到一套“比竞品更复杂”的架构，而是得到一个在故障下可信、在业务语义上可证明、在用户效果上可重复领先的 AOS。
