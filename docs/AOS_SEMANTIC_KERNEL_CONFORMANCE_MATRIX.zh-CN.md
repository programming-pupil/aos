# AOS 语义内核一致性矩阵

> 规格：`docs/AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md`
> 数据集：`eval/datasets/semantic-kernel-conformance.json`
> 判定原则：生产符号存在、触发路径可达、行为断言通过，三者缺一不可。

状态说明：

- `automated_behavior_verified`：生产调用路径和自动化行为测试均已接通。
- `capability_negotiated`：统一协议已接通；executor 支持的动作必须真实执行，不支持的动作必须持久化并明确拒绝。
- `mechanism_verified_effect_pending`：机制与测量链路已验证，准确率/召回率/延迟等数值仍需固定环境实测。
- `pending_blind_review`：真实 adapter、trace、盲评配对已具备，但没有完成同模型/工具/预算的人工盲评，不声明竞品领先。

| ID | 行为触发与关键断言 | 状态 |
| --- | --- | --- |
| PROTO-001 | 事件 hash 重算，篡改 payload 后必须不一致 | automated_behavior_verified |
| PROTO-002 | 写入 stage/final projection 后 reload，必须从持久化事实恢复 | automated_behavior_verified |
| PROTO-003 | stale writer、torn tail、中段损坏分别触发 fencing、尾部修复、fail closed | automated_behavior_verified |
| TOOL-001 | 审批前持久化并 suspend，恢复后只允许一次 dispatch | automated_behavior_verified |
| TOOL-002 | ToolSearch 命中后仅激活授权工具，阻断或 turn 结束后失效 | automated_behavior_verified |
| ART-001 | text/log/search/table/JSON/binary 大结果生成 typed preview，完整 payload 可分页恢复 | automated_behavior_verified |
| BUDGET-001 | general 池耗尽后 final 池仍可执行；父子 commit/release 守恒且不超卖 | automated_behavior_verified |
| CHILD-001 | lineage/control/settlement 持久化且只结算一次；native cancel 生效，其余能力按 executor 明确拒绝 | capability_negotiated |
| SEC-001 | child capability 只能取交集，过期、扩权、重复使用全部 fail closed | automated_behavior_verified |
| SEC-002 | raw/model/client/telemetry 独立投影，source hash 保留且 secret 不泄漏 | automated_behavior_verified |
| PROMPT-001 | provider 调用前记录 prompt/tool/context hash、预算和 snapshot lineage，不落原始 prompt | automated_behavior_verified |
| CORE-001 | Assertion/Decision/Evidence/Snapshot 可确定性校验，不依赖 LLM | automated_behavior_verified |
| CORE-002 | 重复、乱序、冲突、supersession 重放保持幂等且不覆盖旧版本 | automated_behavior_verified |
| CORE-003 | checkpoint/replacement source coverage 完整且敏感字段受保护 | automated_behavior_verified |
| MEM-001 | continuity/long-term 双通道独立写入，secret admission 被拒绝 | automated_behavior_verified |
| MEM-002 | 新旧事实 consolidation 保留 conflict/supersession，当前检索不静默返回过期事实 | automated_behavior_verified |
| CMP-001 | replacement 不小于源窗口或边界不稳定时 fail closed，原文 archive 保留 | automated_behavior_verified |
| CTX-001 | 每个 Context Block 记录 source/hash/layer/token/truncation 并可重放 | automated_behavior_verified |
| PM-001 | 多轮需求只应用 delta/version，状态不从聊天摘要重建 | automated_behavior_verified |
| PM-002 | 下一问按信息价值排序而非固定问卷 | mechanism_verified_effect_pending |
| PM-003 | URL 存在但数字、单位、方向冲突时拒绝 evidence admission | automated_behavior_verified |
| SQL-001 | NL 先编译 canonical IR，再进入 SQL 生成与审计 | automated_behavior_verified |
| SQL-002 | Metric/Join Contract 只读取当前 tenant 的有效版本并保留 lineage | automated_behavior_verified |
| SQL-003 | 可执行但 grain/fanout/metric 口径错误的 SQL 在执行前被拒绝 | automated_behavior_verified |
| SQL-004 | confidence 使用同 scope 标注并输出可复现 ECE/Brier，不使用固定常数 | mechanism_verified_effect_pending |
| EVAL-001 | probe 不包含目标事实，压缩后按隐藏答案和真实证据评分 | mechanism_verified_effect_pending |
| EVAL-002 | 180-case manifest、真实 AOS/Codex adapter、raw trace、盲评隐藏键可复现 | pending_blind_review |
| EVAL-003 | provider partial/hang/timeout/late/crash fixture 校验 request hash 与 assert_consumed | automated_behavior_verified |

## 删除与保留

会话删除测试额外覆盖没有 Artifact 的场景：session-scoped Memory、relation、citation、summary、archive、Context Manifest、checkpoint、semantic snapshot、prompt manifest、PM delivery 和 trace 必须删除；global Memory 与 compliance 保留记录不得误删。

## 仍需外部实证的发布门槛

以下数值不能由本地单元测试替代：Memory recall/false-memory、PM omission/support precision、NL2SQL semantic accuracy/ECE/Brier、Attribution causal overclaim、工具 schema token 降幅、p95 延迟、AOS/Codex/DSH 同条件盲评胜率。运行结果必须保留 case 清单、模型、工具、权限、预算、trace 和人工盲评状态；在完成前统一标记为 `pending_blind_review`，禁止写成“已领先”。
