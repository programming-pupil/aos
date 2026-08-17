# AOS Semantic Kernel / Harness 缺口审计

> 当前快照：2026-08-17，基于工作树 `1336865df12f1bf4a5de0de3f847d48762a908a7` 之后的未提交重构。
> 规格：`docs/AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md`
> 一致性矩阵：`docs/AOS_SEMANTIC_KERNEL_CONFORMANCE_MATRIX.zh-CN.md`
> 可执行数据集：`eval/datasets/semantic-kernel-conformance.json`

## 1. 当前结论

早期审计发现的生产主链缺口已经完成接线，并由 31 个逐项执行的行为用例验证。当前不能继续使用“存在函数或表”作为完成证据；每个关闭项都必须同时满足：

1. 生产入口真实可达；
2. canonical state 在副作用前持久化；
3. 失败路径 fail closed 且不留下半成品；
4. 可执行测试命中真实生产符号；
5. 文档、数据集和测试符号保持一一对应。

`scripts/check-semantic-kernel-behavior.sh` 本轮完整执行 31/31，通过预算、工具、Ledger、Artifact、Memory、Compaction、Context、PM、NL2SQL 和 Eval 主链。该结果证明机制和生产控制面已接通，不等于已经用外部数据证明 AOS 的准确率、召回率或用户效果超过 Codex / DeepSeek Harness。

## 2. 已关闭的生产缺口

| 早期缺口 | 当前实现 | 行为证据 |
| --- | --- | --- |
| Ledger 依赖 JSONL，恢复不精确 | Runtime 只从脱敏 Agent Ledger envelope 与 hash 绑定的 AES-GCM exact recovery payload 恢复；JSONL 仅作诊断/导出 | `PROTO-001`、`PROTO-002`、`PROTO-003` |
| event、projection、预算分开提交 | turn/tool/context/child/compaction 的 canonical event、projection、幂等键和预算在同一事务或 prepare/commit/abort 协议内提交 | `TOOL-001`、`BUDGET-001`、`CHILD-001`、`CMP-001` |
| Context Compiler 只是旁路 manifest | Compiler 的选择结果直接构造 provider request；manifest 记录最终可见块、预算、tool schema 和 request lineage | `CTX-001`、`PROMPT-001` |
| 工具 schema 全量暴露，ToolSearch 无闭环 | 默认只暴露核心工具；搜索命中经 registry、权限与 capability 校验后仅在当前 turn 临时激活 | `TOOL-002` |
| 大工具结果只有截断，没有完整恢复 | typed Artifact Plane 保存受保护完整 payload，并生成 model/client/telemetry/source 独立投影 | `ART-001` |
| Child capability 可扩权或只有生命周期总次数 | parent/child/grandchild lineage、policy/version/derivation、递归撤销和 owner scope 持久化；三并发槽位 settlement 后可复用 | `BUDGET-001`、`SEC-001` |
| 加密没有 key id/rotation | 新密文携带版本与 key id；active/retired key ring 支持历史读取和重加密；旧密文可迁移 | crypto rotation tests、`PROTO-002` |
| Memory 只有 free-form projection | `structured_memory_facts` 是结构化事实源；MemoryEngine 负责 admission、ranking、temporal relation；searchable projection 同事务写入 | `MEM-001`、`MEM-002`、`MEM-003` |
| Compaction 先写 Memory/checkpoint 再做 growth gate | 所有入口先 prepare，runtime replacement 校验成功后一次 commit exact archive、facts、projection、cursor、checkpoint 和 Ledger；失败 abort | `CORE-003`、`CMP-001` |
| PM Requirement State 不驱动真实流程 | Planner 前后写完整 delta；core question 在 retrieve 前阻断；研究证据回写；最终交付重新读取 durable state | `PM-001`、`PM-004` |
| PM 生产 route 绕过 SemanticReducer | evidence assertion 和 requirement delta 均经 `SemanticReducer` admission 后才写 version/current projection | reducer production-route tests、`CORE-002` |
| PM claim 只要有 URL 就接受 | 主题、否定、数字、时间、单位和方向不一致时 deterministic fail closed | `PM-003` |
| `/query`、cache、首次 execute 或 repair 可绕过 canonical IR | canonical IR 在 provider 前首次持久化且不可变；generator、cache、execute、EXPLAIN、repair 和 agent flow 复用同一 IR/verifier | `SQL-001`、`SQL-003`、`SQL-005` |
| Metric/Join Contract 是 tenant-wide shadow data | contract 按 tenant + datasource + version + validity + lineage 加载；歧义旧数据标为 `legacy_unscoped`，不会静默命中 | `SQL-002`、migration tests |
| repair 失败没有 durable audit | 每个可归属 canonical intent 的 semantic rejection 均写不可变 repair verification；scope/identity 错误不污染后续合法重试 | `SQL-003` |
| Feedback correction 可跨用户或未经 owner 批准学习 | correction 绑定真实本人查询；仅 datasource owner/admin 可批准或撤销；学习、回归和 confidence 均按 scope 隔离 | `SQL-004` |
| Web Agent 的 `AskUserQuestion` 会读 stdin | CLI adapter 保留 terminal-only；Gateway/Web 路径明确拒绝 stdin 工具并使用 durable suspend/resume question protocol | Gateway durable-question tests |
| 一致性检查只有字符串 traceability | dataset 校验负责映射，behavior script 逐 case 运行 Cargo 测试并拒绝零匹配 | 31-case behavior gate |

## 3. 迁移与升级验证

以下升级约束已经自动化：

| 验证 | 结果 |
| --- | --- |
| 历史 `0017_semantic_kernel_core.sql` SHA-384 固定 | 已验证：`58772f...e280f` |
| `0030` 单 datasource 旧 contract 映射 | 保留 active 状态并绑定唯一 datasource |
| `0030` 多 datasource 歧义旧 contract | 标记 `__legacy_unscoped__` / `legacy_unscoped`，禁止静默启用 |
| N-2（0031）升级到当前 | Metric/Join Contract 内容、版本和 lineage 不丢失 |
| N-1（0032）升级到当前 | Metric/Join Contract 内容、版本和 lineage 不丢失 |
| capability 新列默认值 | 旧 token 保留 remaining uses，默认 `capability-policy-v1`，不被误撤销 |
| 重复启动 | SQLx migration ledger 保持 33 条，不重复执行 schema mutation |

对应测试：`sqlite_baseline_tests::historical_semantic_kernel_migration_checksum_is_stable`、`semantic_contract_scope_migration_maps_only_unambiguous_legacy_rows`、`n_minus_one_and_two_snapshots_upgrade_without_semantic_data_loss`。

## 4. 仍需外部实证的项目

以下项目不能用本地单元测试冒充完成，也不阻断 semantic-kernel 控制面合并：

| 项目 | 当前状态 | 完成证据 |
| --- | --- | --- |
| AOS / Codex / DSH 三方同条件质量对比 | `pending_blind_review` | 固定模型、工具、权限、预算和网络；保存 raw trace、成本、延迟与人工盲评 key |
| Memory recall / false-memory | `mechanism_verified_effect_pending` | 隐藏事实集上的 recall、precision、时效和冲突准确率 |
| PM 下一问信息增益 | `mechanism_verified_effect_pending` | 固定需求集上的问题数、遗漏率、可评审率和用户成本 |
| NL2SQL 业务语义准确率 | `mechanism_verified_effect_pending` | 错 denominator/population/timezone/comparison/grain/join/null/order/limit 的真实业务集 |
| Attribution 因果过度声明 | `mechanism_verified_effect_pending` | L0-L3 evidence level 盲评与 causal overclaim rate |
| 真实进程 crash / partial provider / late side effect e2e | `pending_process_e2e` | 启动 server、在事件边界 kill/restart，比较 Ledger、projection、next request hash 和预算 |
| p95 延迟与用户完成率 | `pending_benchmark` | 固定硬件和并发下的端到端基准，不使用单元测试耗时替代 |

在这些结果完成前，README、发布说明和技术文档不得写“已经超过 Codex/DSH”，只能说明 AOS 已具备可重复、可审计、可恢复的验证底座。

## 5. Release Gate

- [x] 规格、矩阵、dataset 的 case ID 和测试符号一致。
- [x] 31 个 semantic-kernel 行为 case 全部真实执行且无零匹配。
- [x] 历史 migration checksum 固定；N-1/N-2 快照升级通过。
- [x] canonical event、projection、预算、child settlement 和 compaction 失败路径具备原子性回归。
- [x] `/query`、cache、execute、repair、EXPLAIN 和 agent flow 受 canonical IR/verifier 控制。
- [x] Memory structured fact 与 searchable projection 原子提交。
- [x] PM durable state、SemanticReducer 和 final delivery gate 接入生产路径。
- [x] 当前提交的 workspace tests、核心 strict clippy、WebUI typecheck/test/i18n/build 全部通过并记录退出码。
- [ ] 真实进程 crash e2e 完成。
- [ ] 三方盲评和效果指标完成。

最后三项中，workspace/CI 门禁是本次提交前必须完成的工程门槛；进程级 e2e 与三方效果评测保持公开 pending，不能伪造完成结论。
