# AOS 对齐 Codex 与 DeepSeek Harness 最终架构审计

> 审计日期：2026-08-21
> AOS 本轮复审起点：`747a08f84284065b5b7b2e8aeb53bf27088ac5f5`；修复基线为本报告所在提交
> OpenAI Codex 对比版本：`bd19459358f534ed1cae464ec13d56600aeb45f2`
> DeepSeek Harness（下称 DSH）对比版本：`141eb6fef83422698aef7a981029e843e8161534`
> 审计范围：harness、上下文真实性、压缩、恢复、多 Agent、沙箱、Provider 边界、Memory 与能力声明

## 1. 最终结论

本次复审不是复述上一版文档，而是重新阅读三方当前核心源码并验证生产调用链。复审新发现的 Agent Team lease fencing 失效、quiet mailbox missed wakeup、spawn 幂等碰撞、provider compaction `active` 误报、surface 输入校验不足和状态更新非事务问题均已完成代码修复，不再需要另行输出“待研发实现”的规格文档。

AOS 当前可以准确表述为：

- 核心 harness 架构不变量已经与 Codex、DSH 对齐；
- 普通 Chat、Super Assistant 与 Agent Runtime 的模型可见会话都以 durable append-only Ledger 的 canonical surface 为权威；
- 不产生长期会话的 PM、RD、NL2SQL、搜索、附件摘要、安全扫描和对抗模型调用，使用独立的 durable one-shot dispatch authority，在 Provider I/O 前提交完整 typed request；
- 通用 Agent Team 已具备模型可调用的 `spawn / send / followup / list / wait / interrupt`、独立子会话、持久 mailbox、全局 permit、owner+generation fencing lease、失租取消、递归取消和冷恢复；
- shell 已收敛到统一 `SandboxBackend`：Linux 在 `bwrap + prlimit` 探针通过时提供 full enforcement；不支持的平台或探针失败时 fail-closed，不再回退 host shell；
- `/responses/compact` v1 已使用专用 endpoint、请求/响应类型和持久 attempt lineage；opaque provider item 不会被伪装成文本摘要；
- capability API 区分 `configured / supported / active`；`/responses/compact` endpoint 完成但 opaque output 未应用时，必须报告 `active=false` 并公开 durable fallback reason。

这不等于可以宣称“效果全面超过 Codex 和 DSH”。源码对齐证明架构基础与故障语义，不证明回答质量、任务完成率或成本领先。领先结论仍需同模型、同工具、同权限和同预算的去品牌盲测。

## 2. 核心能力评分

评分只衡量 harness/agent 架构成熟度，不比较产品 UI，也不以代码量计分。10 分表示该方向具备生产主路径、单一 authority、显式不变量、故障恢复、负向测试和可组合接口。分数不是模型效果排名。

| 核心方向 | 权重 | AOS | Codex | DSH | AOS 当前证据 |
| --- | ---: | ---: | ---: | ---: | --- |
| Agent loop 与工具生命周期 | 10% | 9.1 | 9.5 | 9.0 | typed tool intent/result、审批、原子终态、恢复与预算 |
| Context 编译与模型可见真实性 | 12% | 9.4 | 9.3 | 9.7 | surface fold、request hash assertion、Context/Prompt lineage |
| 压缩、大输出与长期连续性 | 12% | 9.1 | 9.5 | 9.2 | exact archive、replacement provenance、native compact v1 与安全回退 |
| Session durability、replay、crash recovery | 12% | 9.2 | 9.0 | 9.7 | durable Ledger、writer fencing、checkpoint、legacy import/export |
| 长期 Memory | 10% | 9.4 | 8.5 | 6.5 | typed fact、证据、敏感度、projection/outbox、冲突与污染治理 |
| 通用模型驱动多 Agent | 14% | 9.3 | 9.5 | 9.0 | durable roster/mailbox/task、六个控制工具、generation fencing 与失租取消 |
| 权限、审批与执行沙箱 | 12% | 8.3 | 9.5 | 9.2 | Linux full enforcement；其他平台 unavailable + fail-closed |
| Provider 抽象与请求 lineage | 7% | 9.2 | 9.0 | 8.5 | 会话 surface 与 one-shot dispatch 分治，完整 request hashes/AAD ciphertext，能力真值 |
| MCP、Skills 与扩展性 | 5% | 8.8 | 9.0 | 9.5 | 工具注册、deferred controls、tenant-scoped MCP/Skill |
| 可观测、评测与故障证据 | 6% | 9.0 | 9.0 | 9.0 | capability truth、迁移升级测试、语义行为门禁与故障测试 |
| **加权总分** | **100%** | **9.1** | **9.2** | **9.0** | **核心架构已对齐；效果领先仍需实证** |

AOS 相比首次审计前的 7.6 分，主要提升来自 canonical surface、Durable Agent Team 和统一沙箱三个 P0 的关闭。本次把多 Agent 从 8.8 调整为 9.3，是因为 lease generation 已真正贯穿 claim、renew、result delivery、mailbox ack、terminal commit 和失租取消，而不是只在表中递增。没有给到 10 分的原因是跨平台 full sandbox 尚未提供，以及 opaque `/responses/compact` continuation 仍选择安全回退而非直接应用。

## 3. 三方源码基准

### 3.1 Codex

本轮采用的关键基准包括：

- `codex-rs/core/src/context_manager/history.rs`、`normalize.rs`：模型历史规范化和工具调用/结果配对；
- `codex-rs/core/src/compact.rs`、`compact_remote.rs`、`compact_remote_v2.rs`：本地及 remote compaction；
- `codex-rs/memories/write/`：Memory 抽取、整合、lease、heartbeat 和污染控制；
- `codex-rs/core/src/tools/handlers/multi_agents_v2/`、`core/src/session/multi_agents.rs`：通用 Agent 控制面；
- `codex-rs/core/src/agents_md.rs`、`agents_md_manager.rs`、`session/mod.rs`：active project trust 进入项目指令加载和 cache key；
- `codex-rs/linux-sandbox/`、`windows-sandbox-rs/`：真实 OS enforcement。

固定版本：<https://github.com/openai/codex/tree/bd19459358f534ed1cae464ec13d56600aeb45f2>。

### 3.2 DSH

本轮采用的关键基准包括：

- `packages/core/session/src/surface.ts`：message-producing event 的 `surfaceOp` 与 canonical fold；
- `packages/core/agent-loop/src/invariant.ts`：Provider request 与 session surface 反向一致性校验；
- `packages/session/session-persistence/`：append-only log、revision、torn-tail repair 与冷恢复；
- `packages/compaction/`：surface replacement 与来源覆盖；
- `packages/experimental/agent-team/`、`packages/experimental/tool-agent-team/`：roster、mailbox、task board 与模型工具面；
- `packages/shell/bash-sandbox/`、`packages/sandbox/`、`native/landlock-run/`：runner capability 与 fail-closed enforcement。

固定版本：<https://github.com/deepseek-ai/deepseek-harness/tree/141eb6fef83422698aef7a981029e843e8161534>。

## 4. 已关闭问题与实现证据

### 4.1 P0：统一 canonical Session Surface

新增 [`surface.rs`](../rust/crates/agent-protocol/src/surface.rs)，定义 provider-neutral typed surface：

- `SurfaceMessage` 保留 role、text、thinking、image/document、tool call/result；
- `SurfaceOperation::Append/Replace` 绑定事件序列和来源；
- fold 校验 Ledger 序列连续、message id 唯一、replacement 当前且连续；
- event-local 校验 role/block 合法性、tool invocation/name 非空、image/document source 仅允许 `url/base64`；
- Provider dispatch 前校验 tool call/result 严格配对、surface hash 与 request messages hash 相同；
- crash repair 通过显式 error tool result 收敛未完成调用，不能静默删掉半边历史。

[`semantic_kernel_store.rs`](../rust/crates/web-server/src/semantic_kernel_store.rs) 已将该 contract 接入生产 Ledger：

- 普通 Chat 不再以 JSONL 作为恢复权威；JSONL 仅用于 legacy import 和兼容导出；
- client full history 只能是 canonical prefix + 一个新 user message，shadow history fail-closed；
- request ID 的 delta retry 和 full-history retry 都幂等，不重复追加或重复终态；
- assistant、tool result、context manifest、compaction replacement 都作为 surface event 提交；
- Provider 调用失败或响应无法提交时，durable terminal 明确记录失败，不返回伪成功。

对 one-shot 推理没有强行伪造“会话”。新增 [`governed_provider.rs`](../rust/crates/web-server/src/governed_provider.rs) 与迁移 [`0043_model_dispatch_surfaces.sql`](../rust/crates/web-server/sqlite-migrations/0043_model_dispatch_surfaces.sql)：

- Provider I/O 前持久化完整 provider-bound request；
- 记录 request、messages、system、tool schema hash；
- 原文使用 tenant/row AAD 加密，外部查询只暴露脱敏 projection；
- attempt index、success/failure/cancellation、worker restart 都有终态；
- 存储失败时不发送模型请求；wrapper 不再暴露通用 `Deref` 绕过治理边界。

### 4.2 P0：Durable Agent Team

新增 [`agent_team.rs`](../rust/crates/web-server/src/agent_team.rs) 与迁移 [`0041_durable_agent_team.sql`](../rust/crates/web-server/sqlite-migrations/0041_durable_agent_team.sql)，并接入 Super Assistant parent/child runtime。

模型可见工具与运行时执行面一致：

- `spawn_agent`：fresh/fork 独立 session；fork 只复制父会话已完成 immutable prefix；
- `send_message`：持久 quiet delivery，不唤醒 idle/completed agent；
- `followup_task`：持久 delivery，并将可恢复 member 置为 queued；
- `list_agents`：tenant/team scoped roster；
- `wait_agent`：Notify + durable recheck，避免 missed wakeup 和 busy poll；无 active peer 时立即返回；
- `interrupt_agent`：记录 child control、取消 live turn、保留未消费 mailbox。

控制平面的关键不变量：

- spawn、child edge、mailbox、task、permit 和 Ledger control event 在一个事务内提交，事务完成后才启动 worker；
- 所有 optional lease 写入口均 fail-closed：`Some` 必须通过 member+permit fencing，`None` 只允许 root coordinator；child 不能通过内部 API 降级为无租约调用；
- `(tenant, team, name)` 与 mailbox idempotency key 唯一；同 key 不得改变内容或 quiet/followup 语义；
- spawn 幂等重试在返回既有 child 前重新验证 parent owner、name、task hash、context mode 与 model；同 key 不同 payload fail-closed；
- 全树共享 team permit，深度和并发有硬上限，queued worker 不会绕过全局配额；
- root coordinator 不是合法的 interrupt target，child 无法通过控制面取消主协调器；
- mailbox at-least-once delivery，delivery ID 固定，result delivery 和 consume acknowledgement 幂等；
- quiet mailbox 在 `Notify` arm 后会同时复读 roster 和 mailbox，关闭 `notify_waiters` 不保留 permit 导致的 missed wakeup；
- `WorkerLease(owner, fencing, team)` 同时绑定 member 与 global permit；mailbox consume、renew、result delivery、ack 和 terminal 都在写事务内校验 generation 与未过期 permit；
- 旧 generation 无法提交完成/失败、投递结果或消费 mailbox；heartbeat 失租/续租异常会立即取消当前模型 turn；
- member、permit 和 task terminal 更新在同一 SQLite write transaction 内提交；expired permit 不会被旧 worker 覆盖；
- lease heartbeat、进程重启 reclaim、未 ack fenced requeue、递归取消和 tenant/owner 隔离均已实现；
- child 使用独立 canonical Session、预算和工具生命周期；父子权限不通过控制消息升级。

### 4.3 P0：统一真实 SandboxBackend

新增 [`sandbox_backend.rs`](../rust/crates/runtime/src/sandbox_backend.rs)，并让 runtime bash、Agent Runtime local process 和 workspace execution 共享同一 launcher contract。

Linux full enforcement 包含：

- 启动前必须通过真实 `bwrap + prlimit` probe；
- mount namespace 中仅只读映射必要系统目录，workspace 按策略只读或可写；
- `--unshare-all`、`--die-with-parent`、`--new-session`、清空环境；
- CPU、address-space、file-size、process-count 限制；
- cwd 和 writable mount 必须 canonicalize 且位于 workspace；
- timeout/cancel 回收 runner/process tree，输出有硬上限；
- runner 不存在、probe 失败或平台不支持时返回 `unavailable`，命令不执行。

模型可见 bash schema 不再接受 sandbox override、unmanaged background 等逃逸字段。显式、已授权的 `danger-full-access/off` 仍可使用 host execution；这属于单独的权限模式，不是 sandbox fallback。

### 4.4 P1：真正的 `/responses/compact` adapter

[`openai_compat.rs`](../rust/crates/api/src/providers/openai_compat.rs) 新增专用 `/responses/compact` request/response/normalized item 类型；[`runtime_builder.rs`](../rust/crates/agent-gateway/src/runtime_builder.rs) 新增显式协议选择；迁移 [`0042_provider_compaction_lineage.sql`](../rust/crates/web-server/sqlite-migrations/0042_provider_compaction_lineage.sql) 保存 attempt lineage。

当前语义：

- `responses_v1/v1` 等配置别名与 runtime parser 使用同一 canonical protocol；
- 只有当前仍显式配置 `responses_compact_v1`、所选 Provider adapter 支持、attempt completed 且 `output_applied=true` 时才标记 active；历史成功 attempt 不会污染已关闭或已切换协议的当前能力状态；
- `model_summary` 继续如实标记为 fallback，不冒充 native compact；
- v2 目前仅能被识别为 configured，不能标记 supported/active；
- normalized output、retained items、hash、AAD ciphertext、parent attempt、timeout/failure 和 `output_applied` 均可审计；
- 仅明确的 plaintext `compaction_summary` 能进入现有 summary continuation；opaque item 原样保存并安全回退，避免数据语义伪造。

### 4.5 P1：能力真值与跨入口治理

[`chat_capabilities.rs`](../rust/crates/web-server/src/routes/chat_capabilities.rs) 现在分别报告：

- canonical surface；
- sandbox；
- Agent Team；
- provider compaction。

每项均区分 `configured / supported / active / enforcement / unavailableReason`。数据库表存在、feature 编译成功或配置声明，不会单独把能力标记为 active。

PM、RD、NL2SQL、搜索、Skill 安全扫描、图片摘要和对抗流程使用 governed provider wrapper；会话型 runtime 继续使用 canonical Ledger + request lineage。两类 authority 按是否存在可延续 session 分治，避免为“一致”而制造第二套 shadow history。

Codex 最新版本新增了 active-project trust gate。AOS 的应用边界不同：生产 session workspace 只能来自 tenant/user 专属 workspace、用户显式绑定并由 `GitLabManager` 同步的 owned repository，或由这些 workspace 派生的 hidden internal worktree；RD instruction loader 在 `repository_root` 处再次校验 tenant/user ownership。AOS 不接受客户端传入任意 host cwd 后自动把其中 `AGENTS.md/CLAUDE.md` 提升为指令，因此不需要复制 Codex 的本地 CLI trust UI；若未来开放任意本地 cwd，必须先增加等价 trust gate。

## 5. 关键负向与恢复场景

本轮新增或加强的测试覆盖：

- canonical Chat 拒绝 shadow history、不同 payload 的 request-ID collision 和 terminal redispatch；
- delta/full-history retry 不重复 user event；
- context replacement、tool pairing、interrupted tool repair 和 nested compaction provenance；
- compaction prepare/commit 的 archive hash、stream revision、turn revision、baseline、token proof；
- compaction commit 任一后段写失败时 Memory、cursor、checkpoint、Ledger 全事务回滚；
- Agent Team 同名冲突、幂等 spawn、quiet/followup 差异、missed wakeup、tenant isolation；
- mailbox lost-ack retry、stale generation consume/ack/terminal rejection、child 无租约降级拒绝、expired permit fencing/requeue、lease recovery、递归取消和 interrupt 后消息保留；
- unsupported sandbox 不执行、launcher path/cwd 不越界、model bash 不接受危险控制字段；
- one-shot dispatch request 加密、attempt 递增、terminal 与 restart recovery；
- N-1/N-2 SQLite snapshot 可升级且 semantic data 不丢失。

## 6. 发布边界

以下是能力边界，不是本轮遗留的 silent bug：

1. **当前开发机为 macOS。** 本机 sandbox 状态必须是 `unavailable` 且 fail-closed。只有在安装 `bwrap + prlimit` 并通过真实 probe 的 Linux runner 上，才能对外声明 `full`。不得把 macOS 单元测试当成 Linux escape-test 证据。
2. **macOS/Windows 未提供 full runner。** AOS 在这些平台的安全承诺是“不执行受保护命令”，不是“已经有原生隔离”。
3. **opaque compact continuation 不直接应用。** AOS 保存并审计 opaque items，但在拥有经过验证的 continuation contract 前回退到 deterministic/model summary。这比伪装成文本摘要更安全，但功能上不等同于 Codex 的全部 remote-v2 continuation。
4. **9.1 是架构评分，不是效果评分。** 对外如需使用“领先/赶超”，必须公开盲测原始 cases、失败样本、任务完成率、恢复成功率、越权率、p50/p95、token/cost 和置信区间。

## 7. 最终门禁

本轮提交前的最终执行结果如下：

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --all-features` | 通过 |
| semantic-kernel strict clippy（all features/targets，deny warnings） | 通过 |
| `cargo clippy --workspace --all-features` | 通过（correctness/suspicious deny） |
| `cargo test --workspace --all-features` | 通过（exit 0；web-server 1179 passed、0 failed、1 ignored；进程故障集成测试 2 passed） |
| `scripts/check-semantic-kernel-behavior.sh` | 通过（40 cases，均产生 production trace） |
| `git diff --check` | 通过 |

定向回归已经通过：canonical surface、governed provider、Agent Team、sandbox、bash、workspace sandbox、native compaction、Chat、Agent Runtime、Super Assistant parent、SQLite baseline/upgrade，以及本轮全量测试暴露后修复的 5 个 compaction/并发预算回归。

## 8. 对外发布口径

可以准确声明：

- AOS 已具备 durable canonical session surface、proof-carrying compaction、typed evidence-backed Memory、Durable Agent Team、统一 fail-closed sandbox backend 和持久 provider request lineage；
- AOS 在多租户 Memory 治理、业务语义内核和审计证据上形成了区别于本地 coding agent 的研究价值；
- AOS 的核心 harness 架构不变量已对齐 Codex 与 DSH，当前源码架构评分约 9.1/10。

不可以声明：

- “AOS 的回答效果已经全面超过 Codex/DSH”；
- “macOS/Windows 已有 full sandbox”；
- “所有 Provider 都支持 `/responses/compact`”或“opaque continuation 已直接恢复”；
- “配置了某能力就代表运行时实际 active”。

## 9. 最终判断

AOS 已经不是“宏观框架”或功能堆叠。会话 authority、模型请求真实性、压缩 provenance、多 Agent 生命周期、OS enforcement 和 Provider lineage 现在形成了可验证的底层闭环。

在本审计范围内，没有仍需研发补写的 P0/P1 架构缺口；因此不再附加实施规格 Markdown。后续工作应从“补齐架构”转向两类实证：Linux full-sandbox CI，以及同模型/同工具/同预算的三方盲测。只有这两类证据完成后，才适合讨论“全面赶超”。
