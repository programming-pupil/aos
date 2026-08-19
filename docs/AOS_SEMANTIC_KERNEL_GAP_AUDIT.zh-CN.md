# AOS Semantic Kernel / Harness 最新缺口审计

> 审计日期：2026-08-17
> AOS 快照：`7a8b14fad65be817816757df11c3b7db0e5ce479`（`Complete semantic kernel production control plane`）
> Codex 对照：`89e297729ebc1a8c243cef73c7ab64cd842ecd5a`
> DeepSeek Harness 对照：`47f943859bef60e4160492346772ded9b24f765a`
> 规格：`docs/AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md`
> 一致性矩阵：`docs/AOS_SEMANTIC_KERNEL_CONFORMANCE_MATRIX.zh-CN.md`
> 可执行数据集：`eval/datasets/semantic-kernel-conformance.json`

## 1. 最终结论

**本轮修复有实质进展，但没有全部完成。**

AOS 已经补齐大量控制面基础：durable Ledger、精确 Session recovery payload、工具 start/outcome、预算、Artifact、Compaction prepare/commit/abort、PM Requirement State、NL2SQL canonical IR、provider wire lineage、child capability、Web Approval suspend/resume 等都已进入生产路径。

但是，当前仍不能宣称“Semantic Kernel P0 已全部关闭”或“已达到可立即以生产级完整实现开源”的标准。主要原因不是架构复杂度不够，而是以下生产语义仍不成立：

1. `AskUserQuestion` durable suspend/resume 只有表和拒绝分支，没有生产协议实现；
2. Memory 仍存在 structured fact 与 text projection 双事实源、跨事务写入和污染状态不生效；
3. turn terminal 与完整 Session checkpoint 分两个事务，存在崩溃窗口；
4. Compaction provenance 绑定线程全历史，而不是本次 archive window；
5. Prompt Manifest 的 `tool_schema_hash` 实际是工具名称列表 hash，不是最终 wire JSON schema hash；
6. key-ring 轮换没有覆盖全部密文列，也没有统一 datasource/Git token 的 key-id 与退役协议；
7. NL2SQL verifier 的关键业务语义仍主要靠字符串启发式，不是 AST/relational semantic proof；
8. 31-case behavior gate 只证明匹配测试通过，没有证明 dataset 声明的生产符号真实存在并被测试命中。

因此，当前状态应判定为：

| 维度 | 结论 |
| --- | --- |
| 控制面工程成熟度 | 明显提升，若干部分已达到可合并水平 |
| Semantic Kernel 完整性 | `partial`，仍有 P0 语义与恢复缺口 |
| Memory 核心 | `partial`，不能宣称事实源统一或超过 Codex |
| PM 核心 | durable state/gate 已接通；证据 authority 与信息增益效果仍不足 |
| NL2SQL 核心 | canonical IR/control plane 已接通；语义证明强度仍不足 |
| 可立即开源 | 可作为明确标注 `preview/experimental` 的源码发布；不能按“生产级完整、P0 全关闭、已领先 Codex/DSH”发布 |

## 2. 已确认完成的部分

以下结论经过生产入口、事务边界与恢复路径复核，可以继续保留为“已完成”：

| 能力 | 当前真实状态 |
| --- | --- |
| Ledger 与 Session recovery | Runtime 使用 durable Ledger envelope 和 hash 绑定的加密 recovery payload；恢复校验 tenant/user/session，JSONL 不再是权威恢复源 |
| 工具生命周期与 Artifact | tool intent/start/outcome、幂等键、预算与 Ledger 已进入生产事务；大结果保存 typed Artifact 和受保护完整 payload |
| Compaction 事务骨架 | prepare/commit/abort 已接通；commit 可原子写 archive、facts、projection、cursor、checkpoint 与 Ledger，失败 abort |
| Context Compiler | compiler 选择结果真实构造 provider request，并保存 exact protected manifest 与预算 |
| PM Requirement State | Planner delta、core-question gate、research evidence 回写和 final delivery gate 已进入生产路径 |
| NL2SQL canonical control plane | `/query`、cache lineage、`/execute` 重校验、EXPLAIN/repair audit 已接通首次持久化的 canonical IR |
| Child capability | durable parent lineage、scope intersection、递归撤销、owner scope 和 slot settlement 已实现 |
| Web Approval | durable approval request、owner/expiry/单次消费、SSE suspend/resume 和刷新恢复已实现；该能力不能替代通用 `AskUserQuestion` |
| Provider attempt lineage | 最终 wire request hash、真实 wire tool JSON schema hash、重试 parent attempt 和请求状态已记录 |
| 加密基础 | versioned key-ring、历史读取、bounded rotation worker 已存在，但覆盖面仍不完整 |

## 3. P0：开源前必须继续实现或修复

### P0-1 `AskUserQuestion` durable protocol 未实现

**现状**

- migration 创建了 `durable_user_questions`：`rust/crates/web-server/sqlite-migrations/0033_semantic_kernel_completion.sql:110`；
- 生产 Rust 代码没有对该表的读写；
- Gateway 在 `rust/crates/agent-gateway/src/runtime_builder.rs:8701` 只拒绝 terminal-only `AskUserQuestion`，错误信息声称 Web/Gateway 应使用 durable protocol，但协议本身不存在；
- 当前所谓 Gateway durable-question tests 只验证拒绝，没有验证 create/answer/consume/restart。

**必须实现**

1. 在同一事务写 pending question、tool/turn suspend projection 和 Ledger event；
2. 提供 tenant/user/session/turn/invocation scoped 的查询与答复接口；
3. answer 使用幂等键，只允许授权用户提交一次；
4. resume 前重检过期、当前权限和策略，答案以 canonical event 注入恢复上下文；
5. crash/restart 后 pending question 可恢复，重复 answer/dispatch 不产生第二次副作用；
6. CLI stdin 只能作为该协议的 adapter，不能成为 Server/Gateway 实现。

**验收**：真实 Gateway/Web 进程级 e2e 覆盖 create -> kill -> restart -> answer -> exactly-once resume。

### P0-2 Memory 事实源、事务与污染治理未统一

**现状**

- `update_memory_item_internal` 只更新 `agent_memory_items`：`rust/crates/web-server/src/routes/memory_continuity.rs:1751`；
- `delete_memory_item_internal` 删除 projection、relation、citation、summary，但没有失效或删除 `structured_memory_facts`：同文件 `:1865`；
- manual consolidation 的 `supersedes` 只禁用旧 projection，没有同步 structured fact 的 `current/superseded_by`：同文件 `:2154`；
- `create_memory_item_internal` 在 structured table 不存在时吞掉错误并允许 projection-only commit：同文件 `:2303`、`:2383`；
- Gateway `memory_note` 先提交 `agent_memory_items`，再独立写 `structured_memory_facts`：`rust/crates/agent-gateway/src/runtime_builder.rs:8593`、`:8641`；
- AOS 会把外部搜索/文件上下文线程标记为 `polluted`，但自动 Memory 生成路径只屏蔽 `disabled`，compaction hook 也没有读取 pollution state。当前标记没有阻止污染内容进入长期候选，也没有 quarantine/独立确认语义：`rust/crates/web-server/src/routes/memory_continuity.rs:488`、`rust/crates/web-server/src/routes/super_assistant.rs:2403`、`runtime_builder.rs:4870`；

**必须实现**

1. 建立唯一 `MemoryRepository/MemoryTransaction`，所有 Web/Gateway/PM/compaction 写路径复用；
2. create/update/delete/supersede/enable/disable 必须在同一事务更新 structured fact、projection、relation、citation 和 cursor；
3. 新安装不得允许 structured table 缺失时降级为 projection-only；迁移异常必须 fail closed；
4. `structured_memory_facts` 成为 current/supersession 的唯一事实源，projection 只能重建，不能反向决定事实；
5. `polluted` 必须有明确策略：禁止自动抽取进入长期事实，或进入 quarantine 并在独立证据确认后晋升；用户显式记忆应单独按 user authority 处理。

**验收**：注入每一个第二写失败、进程崩溃、重复请求、旧事实删除和污染线程场景，重启后 structured fact 与 projection 必须可重建且一致。

### P0-3 turn terminal 与完整 Session checkpoint 非原子

**现状**

`ConversationRuntime::finish_turn_in_kernel` 先调用 `kernel.finish_turn(...)`，成功后再调用 `checkpoint_session(...)`：`rust/crates/runtime/src/conversation.rs:2811`。SQLite `finish_turn` 和 `checkpoint_session` 各自开启并提交独立事务：`rust/crates/web-server/src/semantic_kernel_store.rs:2913`、`:3324`。

两次提交之间崩溃时，turn/budget/Ledger 已是 terminal，但完整 Session 中的 prompt history、context baseline、fork/runtime state 仍是旧 checkpoint。

**必须实现**

- 优先方案：新增 `finish_turn_with_checkpoint`，在同一 SQLite 事务提交 terminal、预算 settlement、Ledger terminal event、session checkpoint event 和 checkpoint projection；
- 若跨存储无法原子提交，则使用显式 prepare/commit protocol，并在恢复时把 terminal-without-matching-checkpoint 视为待修复状态，禁止直接继续生成。

**验收**：在 terminal event 提交前后逐边界 kill，恢复后的 next provider request hash、turn 状态、预算和 Session JSON 必须完全一致。

### P0-4 Compaction provenance 不是本次窗口的精确覆盖

**现状**

`ledger_sequences_for_thread` 返回线程全部 durable Ledger sequence：`rust/crates/web-server/src/semantic_kernel_store.rs:4647`。Compaction hook 将该全量列表绑定到本次 `archived_messages`：`rust/crates/web-server/src/routes/super_assistant.rs:2403`。

多次压缩后，每个 compaction transaction 都会声称覆盖线程全历史，无法证明某条 replacement/fact 来源于本次 archive window，也会重复绑定旧事件。

**必须实现**

- archived message 必须携带或可反查 canonical event/turn sequence；
- source coverage 只允许本窗口的连续/显式 sequence 集；
- commit 校验 archive hash、window boundary、source sequence 与 replacement evidence 一致；
- 后续 compaction 引用前次 compaction item 时，保留嵌套 lineage，不重复冒充原始窗口。

**验收**：连续三次 compaction 后，每次 source set 不重叠或以显式 parent compaction 引用连接，任意缺失/越界 sequence 均 fail closed。

### P0-5 Prompt Manifest 与最终 wire schema lineage 不一致

**现状**

- Runtime 的 `PromptManifest.tool_schema_hash` 对 `active_tool_names()` 数组求 hash：`rust/crates/runtime/src/conversation.rs:1793`；
- Gateway 的 `provider_request_attempts.tool_schema_hash` 才对最终 request 的真实 JSON schema 求 hash：`rust/crates/agent-gateway/src/runtime_builder.rs:784`；
- 两者没有强制 hash 相等，也没有通过 FK/request hash 将 Prompt Manifest 与具体 provider attempt 不可变绑定；
- `PROMPT-001` 测试没有断言 manifest hash 等于最终 wire schema hash。

**必须实现**

1. Prompt Manifest 改存 canonical final tool schema hash，工具名称列表另设字段；
2. provider attempt 保存 `prompt_manifest_id/context_manifest_id` 的强关联；
3. dispatch 前校验 model、context hash、schema hash、request hash 的同一 lineage；
4. retry/fallback 每个 attempt 都保存自己的最终 wire schema，不继承错误 hash。

**验收**：动态 ToolSearch 激活、权限删减、provider 格式转换和 retry fallback 场景中，manifest 与 wire schema hash 必须逐 attempt 相等。

### P0-6 加密轮换不能安全退役旧 key

**现状**

`rotate_encrypted_payload_batch` 只扫描四列：

- `api_keys.encrypted_key`
- `bot_channels.auth_secret_ciphertext`
- `agent_event_ledger.raw_payload_ciphertext`
- `context_packet_manifests.raw_manifest_ciphertext`

代码：`rust/crates/web-server/src/semantic_kernel_store.rs:7072`。

未覆盖至少以下密文：

- `provider_request_attempts.tool_schema_ciphertext`
- `compaction_transactions.source_archive_ciphertext`
- `compaction_transactions.replacement_ciphertext`
- `compaction_transactions.memory_candidates_ciphertext`

Datasource config 使用另一套无 key-id 的 AES-GCM JSON envelope：`rust/crates/web-server/src/routes/data_sources.rs:436`。Git token 使用 `aosgcm:v1` 和独立 `TOKEN_ENCRYPTION_KEY`，也没有 key id/online rotation：`rust/crates/agent-gateway/src/gitlab.rs:825`。

**必须实现**

1. 建立统一 ciphertext registry，所有受保护列声明 codec、key namespace、扫描器和 CAS update；
2. 所有新密文携带 key id；
3. rotation worker 覆盖 nullable/multi-column 行，并持久化进度、失败与重试；
4. 提供“旧 key 可退役”审计：所有注册 store 中旧 key 引用计数必须为零；
5. datasource/Git token 完成无损迁移，禁止依赖用户手工重新保存。

**验收**：使用 active + retired key 启动，完成全库轮换后移除 retired key，所有恢复、compaction、provider replay、datasource 和 Git 操作仍可读取。

### P0-7 NL2SQL verifier 仍不是业务语义证明器

**现状**

Canonical IR、contract scope 和执行前重校验已接通，但 verifier 的关键判断仍大量依赖规范化字符串和 `contains`：

- mandatory filter：`rust/crates/nl2sql-core/src/semantic_ir.rs:650`
- denominator expression：同文件 `:971`
- population subject/dedup/exclusion：同文件 `:1015`
- dimension/metric family：同文件 `:1198`

这无法可靠证明 alias、CTE、CASE、NULL、DISTINCT、窗口、join fanout、timezone、comparison period、population exclusion 和代数等价。当前 fail-closed 能减少错误释放，但不能把启发式测试通过写成“业务语义已验证”。

**必须实现**

1. 基于 SQL AST 构建 normalized relational/metric plan；
2. 将 canonical IR 编译为可验证约束：population、grain、metric numerator/denominator、dedup、filters、timezone、comparison、join cardinality；
3. 对 projection、selection、group、join、window 和 CTE 做符号绑定与 lineage；
4. 无法证明时返回 `NeedsClarification/Reject`，不得用名称相似或 substring 释放；
5. 建立 adversarial 业务集，覆盖“SQL 可执行但口径错误”。

**验收**：同义正确 SQL 可通过，名称相似但 denominator/population/timezone/comparison 错误的 SQL 必须稳定拒绝。

### P0-8 31-case behavior gate 不能证明生产可达

**现状**

`scripts/check-semantic-kernel-behavior.sh` 只读取 dataset 的 `test` 字段并执行 Cargo test filter，确认至少一个测试通过；它完全没有读取或校验 `production` 字段。测试名称匹配也不能证明测试调用了声明的生产符号。

**必须实现**

1. 校验每个 `production` 文件和符号存在；
2. 每个 case 使用明确 production-path fixture/trace marker，禁止只测复制实现；
3. CI 保存 case -> production symbol -> test symbol -> assertion 输出；
4. 对关键事务增加进程级 fault injection，不以单元测试替代 crash recovery。

**验收**：故意删除/改名 production symbol、让测试改走 fake helper、制造零生产 trace 时，behavior gate 必须失败。

## 4. P1：核心效果与长期竞争力缺口

### P1-1 PM evidence authority 标准偏弱

`SemanticReducer` 只要求 Confirmed assertion 至少有一个 authority 非 `Model`：`rust/crates/semantic-core/src/reducer.rs:57`。PM research evidence 默认标记为 `EvidenceAuthority::Tool`：`rust/crates/web-server/src/routes/agent/agent_pm_persist.rs:161`。因此单个 Tool evidence 即可把 assertion 置为 Confirmed，不等价于用户、owner、权威数据库或多源独立证实。

建议按 assertion 风险定义 authority policy：高影响需求/数字/决策至少要求 Owner/User，或两个独立且可校验来源；Tool 只能证明“工具返回了该内容”，不能自动证明业务事实正确。

### P1-2 PM 下一问信息增益仍依赖模型自报参数

AOS 会确定性重算 expected information gain，但 branch probability、prior/posterior uncertainty 和 decision effect 仍来自模型：`rust/crates/pm-domain/src/requirement_state.rs:84`、`:472`。机制成立，校准和真实提问收益未被证明。

建议把历史回答分布、问题命中率、决策变化和用户成本写成可学习 calibration dataset；对模型自报概率做 clipping、校准和低置信降权。

### P1-3 Memory 全局 consolidation 与质量闭环弱于 Codex

Codex 当前源码仍有独立 memories read/write、stage/phase-1 extraction、带 lease/retry/cooldown 的 phase-2 global consolidation、polluted thread 排除和 forgetting enqueue。AOS 已有 compaction 抽取、cursor、manual consolidation 与 pollution marker，但尚未形成等价的自动全局 worker 和污染晋升/遗忘闭环。

建议吸收该机制，而不是照搬文件结构：AOS 应以自己的 structured fact 为事实源，增加“候选 -> quarantine -> confirmed/current -> superseded/forgotten”的 durable 状态机和全局 consolidation lease。

### P1-4 Harness TCK 与真实进程恢复仍不够

DeepSeek Harness 仍有独立 compaction-basic、tool-result-pruner、session-persistence contract/repair、JSONL/SQLite backend、output-retention、LLM replay 和 compaction E2E。AOS 的业务控制面更强，但当前 conformance 主要是仓内 Cargo tests，真实 server kill/restart、partial stream、late side effect 和跨版本 replay 仍未形成常态化 TCK。

建议增加后端无关的 protocol TCK：同一 fixture 可运行 SQLite、未来其他 store、真实 provider recorder/replay 和 process fault injector。

## 5. 竞品精华：值得吸收，但无需照搬

| 来源 | 值得吸收的机制 | AOS 适配方式 |
| --- | --- | --- |
| Codex | phase-1/phase-2 memory pipeline、global lease、pollution exclusion、forgetting | 落到 structured fact 状态机与 tenant-scoped consolidation worker，不回退为文件型双事实源 |
| Codex | compaction 后 canonical initial-context reinjection、rollout reconstruction/migration | 继续强化 AOS context baseline/checkpoint，增加跨版本 reconstruction golden tests |
| DSH | persistence contract、cold repair、backend-neutral invariant | 抽成 AOS Agent Protocol TCK，避免测试只绑定 SQLite 私有实现 |
| DSH | model-free tool-result pruning + output retention | 在 Artifact reducer 前增加结构化 pruning policy；保留完整 source artifact，不破坏可恢复性 |
| DSH | LLM replay 与 compaction E2E | 将 provider request attempt、exact context manifest 和 fault frames组合成可离线重放的公开 fixture |

不建议为了“看起来更强”复制竞品模块数量。AOS 应保持优势集中在 PM、NL2SQL、企业权限和可审计业务状态；需要吸收的是能提高语义正确性、恢复确定性和效果迭代速度的机制。

## 6. 文档与声明需要同步纠正

当前以下声明与源码不一致：

- 本文旧版第 2 节将 Memory structured fact/projection、AskUserQuestion、完整加密轮换和 Prompt schema lineage 写成已关闭；
- `docs/AOS_SEMANTIC_KERNEL_CONFORMANCE_MATRIX.zh-CN.md` 将 `PROMPT-001`、`MEM-003` 等标为 `automated_behavior_verified`，但覆盖范围不足；
- `docs/AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md:1568` 后的“本次实现自检”将 Prompt Manifest、Memory 双通道和全部生产路径写成“已接通”，应以本审计的 P0 状态为准；
- “31/31”只能写成“31 个匹配测试执行通过”，不能写成“31 个生产能力全部可达且正确”。

在代码修复前，README、release note、架构图和官网不得使用以下表述：

- “Semantic Kernel P0 全部完成”；
- “Memory 已形成唯一事实源”；
- “Web AskUserQuestion 已 durable 恢复”；
- “所有密文已支持在线 key rotation”；
- “NL2SQL 已证明业务口径正确”；
- “已经全面超过 Codex/DeepSeek Harness”。

## 7. 外部实证门槛

以下不能由本地单元测试替代：

| 项目 | 当前状态 | 必需证据 |
| --- | --- | --- |
| AOS/Codex/DSH 三方同条件质量对比 | `pending_blind_review` | 固定模型、工具、权限、预算、网络；保存 raw trace、成本、延迟和人工盲评 key |
| Memory recall/false-memory | `pending` | 隐藏事实集上的 recall、precision、时效、冲突、污染和遗忘准确率 |
| PM 需求挖掘 | `pending` | omission rate、问题数、决策变化、证据 support precision、用户成本、可评审率 |
| NL2SQL 业务语义 | `pending` | denominator/population/timezone/comparison/grain/join/null/dedup adversarial 集 |
| 真实进程恢复 | `pending_process_e2e` | 在每个事务边界 kill/restart，对比 Ledger、projection、checkpoint、next request hash 和预算 |
| 性能与体验 | `pending_benchmark` | p50/p95 延迟、token 成本、首个有效动作时间、任务完成率和恢复可理解性 |

## 8. 当前验证结果

| 验证 | 当前结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `.github/scripts/check_doc_source_of_truth.py` | 通过，但该脚本不证明生产可达 |
| WebUI tests/i18n/build | 通过：37 files、165 tests，i18n 0 missing，production build 成功 |
| 31-case behavior gate | 重跑已确认前两个 case 通过，第三个 case 触发完整 `web-server` 冷链接后中止重复执行，未取得 31-case 最终退出码；即使全通过，也只按 P0-8 的有限证据解释 |
| Rust workspace tests | `cargo test --workspace --all-features` 已运行到 `web-server` tests 和 doctests 后进程结束；所有可见输出均通过，但原工具会话丢失，无法取得最终退出码，因此不标记全绿 |
| semantic-kernel strict clippy | 按 CI 原命令通过，退出码 0：`runtime`、`agent-gateway`、`web-server`，`--no-deps --all-features --all-targets` |
| 真实 process crash e2e | 未完成 |
| 三方盲评 | 未完成 |

## 9. Release Gate

- [x] Ledger exact recovery、hash、scope 校验进入生产路径。
- [x] Tool start/outcome、预算、Artifact 和 child capability 主要事务已接通。
- [x] Compaction prepare/commit/abort 骨架已接通。
- [x] PM durable state/final gate 与 NL2SQL canonical IR/repair audit 已接通。
- [x] Web Approval durable suspend/resume 已接通。
- [ ] `AskUserQuestion` durable create/answer/consume/restart 协议完成。
- [ ] Memory 所有入口使用唯一事实源和同一事务；update/delete/supersede/pollution 全覆盖。
- [ ] turn terminal 与完整 Session checkpoint 原子提交或可确定性修复。
- [ ] Compaction source coverage 精确对应本次 archive window。
- [ ] Prompt Manifest 与最终 provider wire schema/request attempt 强绑定。
- [ ] 所有密文 store 完成统一 key-id、online rotation 和旧 key 退役审计。
- [ ] NL2SQL verifier 从 substring heuristic 升级为 AST/relational semantic proof，或明确降级为 beta 且禁止强声明。
- [ ] behavior gate 能证明 production symbol 存在且测试命中生产路径。
- [x] semantic-kernel strict clippy 按 CI 原命令通过并记录退出码。
- [ ] Rust workspace tests 与 31-case behavior gate 取得并记录最终退出码。
- [ ] 真实进程 crash/restart e2e 完成。
- [ ] Memory、PM、NL2SQL 和三方盲评效果门槛完成。

在上述 P0 未关闭前，最准确的发布定位是：**AOS 已具备较强的业务 Agent 控制面和一部分可恢复语义内核，但仍处于 production-hardening 阶段。**
