# AOS 对齐 Codex 与 DeepSeek Harness 最终架构审计

> 审计日期：2026-08-20
> AOS 对比起点：`8dcb86d948ed1e808665a8445effa5708f63a6db`
> AOS 本轮已提交修复：`f036c546941ee9e36d2515144db1978a5d1616c5`、`52887195a7a0148d30d3126b28df70e280899869`
> OpenAI Codex：`bc3545b805de6e91a11b88114fe1673b678633ca`
> DeepSeek Harness（下称 DSH）：`141eb6fef83422698aef7a981029e843e8161534`
> 本文状态：源码审计结论、已修问题记录、P0/P1 实施规格与发布口径

## 1. 执行结论

结论必须如实表述：**AOS 目前没有在所有核心方向完全对齐 Codex 和 DSH，因此不能宣称“最终全面对齐”或“全面赶超”。**

AOS 已经形成有研究价值的独立架构，尤其是 Context Compiler、请求 lineage、typed Memory、多租户治理、原子终态和精确压缩归档。在这些方向，AOS 不是简单模仿，部分治理能力确实比本地 coding harness 更强。

但源码中仍有三个会被资深评审直接识别的 P0 差距：

1. **模型可见上下文还没有统一由一个 append-only canonical event authority 重建。** Super Assistant 的主运行时已进入 durable execution kernel；普通 `/api/v1/chat/message` 和 `/api/v1/chat/stream` 仍直接读取、追加 JSONL，并在部分持久化失败时继续返回成功。PM、RD、NL2SQL 也各有领域日志，尚未通过统一的“事件 → surface fold → provider request”不变量证明每个模型入口的一致性。
2. **缺少通用、模型驱动、可冷恢复的 Agent Team。** AOS 有一次性后台 `Agent`、Task/Worker 工具、`child_thread_edges`、`child_thread_controls` 及 Super Assistant 预定义专家子任务，但没有 Codex/DSH 等价的 `spawn / send / followup / list / wait / interrupt` 统一工具面、独立子上下文、持久 mailbox、树形生命周期、全局并发配额和重启恢复协议。
3. **通用 shell 执行的文件系统沙箱没有真正落地。** `runtime::sandbox` 的 Linux `unshare` 只创建 namespace，并未 bind 一个受限根目录，也未安装 Landlock 规则；非 Linux 或 namespace 不可用时仍可落到 host shell。AOS 的 unified workspace 另有可靠的 `bwrap + prlimit` fail-closed 实现，但它没有成为所有 Agent shell 的统一后端；Agent Runtime 的 Docker 沙箱又是 opt-in，默认仍是 `local_process`。

因此，当前适合的发布结论是：

- `implemented`：核心单 Agent runtime、Context Compiler、精确压缩/归档、typed Memory、审批与 durable kernel 已有实质实现；
- `automated verified`：本文第 8 节列出的定向测试已执行通过；
- `requires implementation`：统一 canonical surface、通用 Agent Team、统一真实沙箱；
- `requires empirical validation`：同模型、同工具、同权限、同预算的 AOS/Codex/DSH 盲测。

## 2. 评分规则与当前分数

评分只评价“harness/agent 底层架构成熟度”，不评价三者不同的产品 UI，也不把代码行数当能力。10 分表示该方向已有清晰的生产主路径、不变量、恢复语义、负向测试和可组合接口；分数不是回答质量的实证排名。

| 核心方向 | 权重 | AOS | Codex | DSH | AOS 判断 |
| --- | ---: | ---: | ---: | ---: | --- |
| Agent loop 与工具生命周期 | 10% | 8.5 | 9.5 | 9.0 | AOS 有迭代工具循环、deferred/approval、原子终态和工具 contract |
| Context 编译与模型可见真实性 | 12% | 9.0 | 9.3 | 9.7 | AOS manifest 很强；但尚未覆盖所有产品入口 |
| 压缩、大输出与长期连续性 | 12% | 8.5 | 9.5 | 9.2 | AOS exact archive/provenance 强；标准原生 compact 端点未实现 |
| Session durability、replay、crash recovery | 12% | 7.5 | 9.0 | 9.7 | kernel 路径强；普通 Chat 仍是第二套 authority |
| 长期 Memory | 10% | 9.0 | 8.5 | 6.5 | AOS typed fact、证据、敏感度、projection/outbox 是明确优势 |
| 通用模型驱动多 Agent | 14% | 4.5 | 9.5 | 9.0 | 一次性 Agent/预定义工作流不等价于 durable Agent Team |
| 权限、审批与执行沙箱 | 12% | 5.5 | 9.5 | 9.2 | 审批治理较强；通用 shell 文件隔离未实施且后端分裂 |
| Provider 抽象与请求 lineage | 7% | 9.0 | 9.0 | 8.5 | AOS 的 Context/Prompt/Tool Schema/Provider Attempt 四层绑定很强 |
| MCP、Skills 与扩展性 | 5% | 8.5 | 9.0 | 9.5 | 已具备丰富工具面，但部分工具仍未进入同一控制协议 |
| 可观测、评测与故障证据 | 6% | 8.5 | 9.0 | 9.0 | AOS conformance 与业务语义证据较强，跨入口 TCK 仍不足 |
| **加权总分** | **100%** | **7.6** | **9.2** | **9.0** | **有明显价值，但尚未完成最终核心对齐** |

分数的主要拖累不是功能数量，而是三条架构不变量尚未统一。完成本文 P0 并通过验收后，AOS 的合理目标区间是 8.8–9.2；是否“超过”只能由盲测和第三方审计决定。

## 3. 三方源码证据

### 3.1 Codex 的关键基准

本轮直接检查的核心路径包括：

- `codex-rs/core/src/context_manager/history.rs`、`normalize.rs`：模型历史规范化、工具调用/结果配对及可见 surface；
- `codex-rs/core/src/compact.rs`、`compact_remote.rs`、`compact_remote_v2.rs`、`tasks/compact.rs`：本地、`/responses/compact` 和 v2 remote compaction，覆盖 pre-turn、mid-turn、manual 触发与降级；
- `codex-rs/memories/write/src/phase1.rs`、`phase2.rs`、`storage.rs`、`control.rs`：两阶段 Memory 抽取、整合、lease、heartbeat、重试与污染控制；
- `codex-rs/core/src/tools/handlers/multi_agents_v2/` 和 `core/src/session/multi_agents.rs`：`spawn_agent`、`send_message`、`followup_task`、`list_agents`、`wait`、`interrupt_agent`，以及 session 级 Agent 控制；
- `codex-rs/linux-sandbox/`、`windows-sandbox-rs/` 和 sandbox policy：真实 OS 约束与审批升级，而不是命令名前缀判定。

固定提交链接：`https://github.com/openai/codex/tree/bc3545b805de6e91a11b88114fe1673b678633ca`。

### 3.2 DSH 的关键基准

本轮直接检查的核心路径包括：

- `packages/core/session/src/index.ts`、`surface.ts`、`types.ts`：每个 message-producing event 必须携带 `surfaceOp`；模型历史只能由 Session event log 的 canonical surface fold 得出；
- `packages/core/agent-loop/src/invariant.ts`：运行时反向校验请求 messages 与 session surface 完全相同，阻止 shadow history；
- `packages/session/session-persistence/src/coordinator.ts`、`write-behind.ts` 以及 JSONL/SQLite backend：连续 append-only log、revision、torn tail repair、cold preparation 和进程恢复；
- `packages/compaction/compaction-basic/`、`compaction-tool-result-pruner/`、`compaction/`：compaction 作为 surface replacement event，来源序列必须覆盖被 shadow 的节点；
- `packages/experimental/agent-team/src/` 与 `packages/experimental/tool-agent-team/src/index.ts`：durable roster、mailbox、task board，以及 `spawn_teammate / send_message / followup_task / list_agents / wait_agent / interrupt_agent`；
- `packages/shell/bash-sandbox/`、`packages/sandbox/` 与 `native/landlock-run/`：真实 Landlock 执行、partial/full enforcement 标记和 runner failure fail-closed。

固定提交链接：`https://github.com/deepseek-ai/deepseek-harness/tree/141eb6fef83422698aef7a981029e843e8161534`。

### 3.3 AOS 已经对齐或有优势的部分

| 能力 | 状态 | AOS 源码证据 | 结论 |
| --- | --- | --- | --- |
| 迭代工具循环与 durable terminal | `implemented` | [`conversation.rs`](../rust/crates/runtime/src/conversation.rs)、[`execution_kernel.rs`](../rust/crates/runtime/src/execution_kernel.rs)、[`semantic_kernel_store.rs`](../rust/crates/web-server/src/semantic_kernel_store.rs) | tool intent/result、审批、checkpoint、terminal 和预算有清晰边界 |
| Context Compiler 与请求 lineage | `implemented` | [`semantic-core/context.rs`](../rust/crates/semantic-core/src/context.rs)、`RuntimeContextManifestInput`、`prompt_manifests`、`provider_request_attempts` | AOS 的四层不可变绑定是可研究的差异化设计 |
| 精确压缩归档 | `implemented` | [`compact.rs`](../rust/crates/runtime/src/compact.rs)、`RuntimeCompactionHook`、`compaction_transactions` | exact window、source event coverage、parent compaction、60% shrink guard 较强 |
| typed Memory | `implemented` | [`memory-engine`](../rust/crates/memory-engine)、`structured_memory_facts`、memory governance worker | canonical fact 与 projection 分离，带证据、敏感度、supersession 和污染治理 |
| durable 用户提问/审批 | `implemented` | `durable_interactions`、`approval_requests`、runtime special control handling | 多租户授权、过期、重启和批量原子响应强于简单 stdin seam |
| 多租户业务语义内核 | `implemented-supported-scope` | PM、NL2SQL、evidence ledger、semantic verification | 属于 AOS 的产品级优势，但不能替代通用 harness 不变量 |

## 4. 不能继续沿用的旧结论

[`AOS_FINAL_OPEN_SOURCE_READINESS_AUDIT.zh-CN.md`](./AOS_FINAL_OPEN_SOURCE_READINESS_AUDIT.zh-CN.md) 中“八项 P0 全部核销”只覆盖当时列出的八项，不能推导为“与最新 Codex/DSH 的全部核心能力已对齐”。本轮新增发现不否定那些实现，但会改变总的发布结论。

尤其不能使用以下等价替换：

- 有 `child_thread_edges` 表 ≠ 已有通用多 Agent；
- 有 `Agent` 工具 ≠ 已有可 follow-up、wait、interrupt、cold-resume 的 Agent Team；
- 有 `Task/Worker/Team` 名称 ≠ 它们已经共享一个模型驱动 mailbox/lifecycle authority；
- 有 `filesystemMode=workspace-only` 配置 ≠ 已执行文件系统隔离；
- 普通模型请求携带 compaction `extra_body` ≠ 调用了标准 `/responses/compact`；
- Super Assistant 经过 durable kernel ≠ 所有 LLM 产品入口都经过同一 canonical ledger。

## 5. 本轮已直接修复的问题

### 5.1 pre-model 持久化失败未 checkpoint

提交 `f036c546941ee9e36d2515144db1978a5d1616c5` 修复 [`conversation.rs`](../rust/crates/runtime/src/conversation.rs)：用户消息 JSONL 写入失败发生在统一模型循环错误出口之前时，Session 与 durable kernel 现在都会得到唯一 `Failed` terminal/checkpoint；若 failure checkpoint 本身失败，错误会组合返回。

新增/加强测试：

- `user_message_persistence_failure_checkpoints_failed_kernel_turn`；
- `run_turn_propagates_api_errors`；
- cancellation checkpoint 回归。

以下 5.2—5.4 由提交 `52887195a7a0148d30d3126b28df70e280899869` 修复。

### 5.2 provider/model compaction summary 被静默丢弃

[`super_assistant.rs`](../rust/crates/web-server/src/routes/super_assistant.rs) 的 `RuntimeCompactionHook` 原先接收到 pre-turn 已选择的 provider/model summary，却无条件换回确定性摘要，导致 telemetry 显示 `provider_native_used/model_summary_compaction`，实际 continuation 并没有使用该 summary。

现在：

- 选中的 summary 会真正进入 committed replacement；
- deterministic pass 仍独立证明 exact source window，不把模型摘要当证据 authority；
- secret admission 失败时回退到安全摘要；敏感 pinned 内容不重新注入；
- exact archive、source coverage、memory evidence span、事务提交和 60% shrink guard 保持不变。

### 5.3 workspace path 与 shell 权限绕过

[`permission_enforcer.rs`](../rust/crates/runtime/src/permission_enforcer.rs) 原先使用字符串前缀判断 workspace，无法正确处理 `..` 和 symlink escape；read-only allowlist 还包含 Python、Node、Ruby、Cargo、Rustc、Git、`tee`、`xargs` 等可执行或可写程序。

[`tools/src/lib.rs`](../rust/crates/tools/src/lib.rs) 的动态 bash/PowerShell 分类又只看第一个命令，`cat file; <write command>` 会继承第一个命令的低权限。

现在：

- workspace 判断使用 path component 与最近存在祖先的 canonicalization；
- missing child 经 symlink 逃逸会被拒绝；
- shell composition、重定向、变量/命令展开、glob、解释器和 build 工具统一要求 `DangerFullAccess`；
- 只有单条、不可编程、workspace 内的读取命令得到 `ReadOnly`；
- PowerShell 不再接受前缀伪装或 compound command。

这仍不是 shell sandbox 的替代品，只是关闭权限分类绕过。

### 5.4 沙箱状态误报

[`sandbox.rs`](../rust/crates/runtime/src/sandbox.rs) 不再把设置 `HOME/TMPDIR` 和环境变量报告为 `filesystem_active=true`。现在明确暴露 `filesystem_supported=false`、`filesystem_active=false`、`active=false` 及 fallback reason，并删除会让子进程误以为 mount policy 已生效的环境标记。

真实文件隔离仍属于 P0-03，不能因本次“状态如实化”而视为完成。

## 6. 必须实施的 P0

### P0-01：统一 canonical Session Event Surface

#### 目标

任何进入模型的 messages、system/context sections、tool results 和 compaction replacement，必须能从同一个 tenant/thread 的 append-only ledger 完整重建。JSONL、领域表、缓存和 projection 只能是导出或读模型，不能成为第二个写 authority。

#### 复用现有结构

优先扩展而不是另起一套：

- `agent_threads`、`agent_turns`；
- `agent_event_ledger` 与 `agent_writer_leases`；
- `execution_checkpoints`、`context_packet_manifests`、`prompt_manifests`；
- `agent_projection_cursors`；
- `compaction_transactions/checkpoints`。

`agent-protocol::EventLedger` 当前主要是内存 contract/TCK；生产 authority 是 `semantic_kernel_store::RuntimeExecutionKernel`。两者应共享 backend-neutral contract suite，但不能把内存类型本身宣传成生产持久化。

#### 必须新增的 event/surface 规则

1. 所有 message-producing event 必须携带：
   - `surface_op = append | replace(start_seq, end_seq)`；
   - `source_event_seqs`；
   - `message_id`、role、typed content；
   - schema version 与 payload hash。
2. compaction replacement 必须引用并完整覆盖被替换的当前 surface nodes；不得引用未来或非当前节点。
3. tool call 与 tool result 必须按 invocation id 成对；缺失结果在 crash repair 时追加显式 interrupted result，不能静默删除。
4. provider dispatch 前同时计算：
   - `surface_hash = hash(fold(agent_event_ledger))`；
   - `request_messages_hash`；
   - 二者不相等则 fail-closed，不发送请求。
5. projection cache 必须带 `last_sequence + state_version + surface_hash`；版本不符、越过 ledger 尾部或 hash 不符时丢弃并重建。

#### 产品入口迁移

按以下顺序切换：

1. 普通 Chat：停止以 `.jsonl` 作为恢复 authority；路由创建 `RuntimeExecutionKernel`，JSONL 降为 best-effort export。
2. Super Assistant：保留当前 kernel 主路径，补上 surface fold dispatch assertion。
3. PM/RD/NL2SQL：领域事件表继续存在，但每次模型调用都必须绑定一个 canonical thread/turn/event range；领域状态成为 ledger projection 或显式 source reference。
4. Bot/异步 worker：复用同一 thread lease、idempotency 和 terminal protocol。

#### 迁移策略

- `shadow`：旧入口正常服务，同时生成 canonical events 和 surface hash，不用于回答；记录旧 request 与新 fold 的 diff。
- `dual-read`：新 surface 为主，旧 JSONL/领域历史只用于检测差异，禁止拼接进 provider request。
- `on`：只读 ledger；JSONL 仅导出。连续 7 天无 mismatch 后移除 dual-write。
- legacy import 必须生成明确 `legacy_import` batch、稳定 idempotency key 和 source hash；不能伪造成原生实时事件。

#### 验收

- 在 user append、assistant delta、tool intent、tool result、compaction prepare/commit、terminal/checkpoint 的每个 await 边界注入 kill -9，重启后 surface hash 与无故障运行一致；
- 普通 Chat 持久化失败不得返回成功，不得出现 user/assistant 单边落盘；
- 同一 request id 重试不重复追加 message、tool side effect 或 terminal；
- 随机 event 序列 property test 保证 fold 确定性、replace coverage 与 tool pairing；
- CI 中逐产品入口断言 provider request 的 messages 只能来自 surface fold。

### P0-02：通用 Durable Agent Team

#### 目标

提供模型可直接调用、可树形嵌套、独立上下文、持久通信、可中断/续跑、全局资源受控的 Agent runtime。Super Assistant 专家编排、PM 工作流和一次性后台 Agent 可以成为它的调用者或 adapter，但不能继续充当其替代品。

#### 模型工具面

首版必须同时提供并作为 runtime special control tools 执行：

- `spawn_agent(task, name?, context=fresh|fork, model?, budget?)`；
- `send_message(target, message)`：durable quiet delivery，不唤醒 idle agent；
- `followup_task(target, message)`：durable delivery，必要时启动下一 turn；
- `list_agents(path_prefix?)`；
- `wait_agent(timeout_ms)`：等待调用后发生的 mailbox/status/task 变化，不忙轮询、不自动唤醒；
- `interrupt_agent(target)`：中断当前 turn，保留 mailbox；
- 可选 `team_task_create/list/claim/complete`，必须带 revision CAS 和依赖图。

这些工具不能走普通 `ToolExecutor` 后再补记控制状态；控制 event、mailbox item、child edge、预算预留和 lease 必须先在一个事务中提交，随后才启动 live worker。

#### 数据模型

复用 `child_thread_edges`、`child_thread_controls`、`agent_threads/turns/event_ledger`，并新增：

- `agent_team_members(tenant_id, team_id, thread_id, parent_thread_id, name, role, depth, status, context_mode, model, lease_fencing, created_at, updated_at)`；
- `agent_mailbox_items(id, tenant_id, team_id, sender_thread_id, target_thread_id, delivery, content_ciphertext, idempotency_key, accepted_at, consumed_turn_id, consumed_at)`；
- `agent_team_tasks(id, tenant_id, team_id, revision, subject, description, status, owner_thread_id, blocked_by_json, write_scopes_json, created_at, updated_at)`；
- `agent_concurrency_permits(tenant_id, scope, holder_thread_id, lease_fencing, expires_at)`。

唯一约束至少包含 `(tenant_id, team_id, name)`、`(tenant_id, target_thread_id, idempotency_key)`。mailbox 使用 at-least-once delivery + exactly-once consume command；业务 side effect 仍由工具 idempotency contract 保证。

#### 不变量

1. 每个 child 都有独立 thread、Session surface、turn lease、permission context 和 budget account。
2. `fork` 只能复制父线程已完成 turn 的 immutable prefix；`fresh` 不继承父历史，只接收任务 envelope。
3. 树深由 tenant policy `max_depth` 限制；全树同时运行数由全局 semaphore/DB permit 限制，不能每个父节点各自放大并发。
4. child 权限不得高于父权限；审批 token 必须显式声明 `child_scope`，默认不可继承。
5. parent cancel 默认向未 detached child 传播；detached 必须由明确 policy 允许。
6. agent 完成后仍可收到 `followup_task` 并创建新 turn；进程重启后 supervisor 从 ledger/mailbox/lease 恢复，而不是依赖内存 thread handle。
7. shared workspace 需要 write-scope advisory、stale-version guard 和最终 diff review；它不是文件锁。

#### 验收

- 三层 Agent 树、fresh/fork、同名冲突、并发配额、预算耗尽和权限不升级测试；
- 在 spawn 事务后/worker 启动前、mailbox accept 后/consume 前、child terminal 前后 kill -9，恢复后无丢信、无重复 agent、无重复 side effect；
- `send_message` 不唤醒 idle，`followup_task` 唤醒，`wait_agent` 在无 active peer 时立即返回 no-progress；
- interrupt 保留未消费 mailbox；parent cancel 传播符合 detached policy；
- 模型实际可见 tools/schema 与 runtime 可执行控制面完全一致。

### P0-03：统一真实执行沙箱

#### 目标

所有 Agent shell，包括 foreground/background、Bash/PowerShell、一次性 Agent、RD runtime、Super Assistant workspace execution，必须通过同一个 `SandboxBackend` contract。权限分类用于决定策略与审批，不能替代 OS enforcement。

#### 后端 contract

```text
probe(policy) -> EnforcementCapability { full | partial | unavailable }
confine(argv, cwd, env, policy, limits, cancellation) -> ProcessHandle
inspect(handle) -> EffectivePolicy + resource usage + denial reason
terminate(handle) -> terminal receipt
```

支持顺序：

- Linux：优先复用现有 `agent-gateway/workspace_sandbox.rs` 的 `bwrap + prlimit`；或 Landlock + network namespace。必须执行 host escape、symlink、network 和 resource probe；
- macOS：Seatbelt profile 或等价受支持机制；未实现前 `workspace-only` 必须 fail-closed；
- Windows：restricted token/AppContainer、Job Object、ACL staging root；未实现前 fail-closed；
- Docker：固定 digest image、`--network none` 默认、drop capabilities、no-new-privileges、只读 rootfs、明确 writable workspace、pids/memory/cpu limits；启动 probe 失败不得回退 local process。

#### 策略

- `read-only`：workspace 与必要系统路径只读，网络默认拒绝；
- `workspace-write`：只有 canonical workspace root 和显式 temp/artifact 目录可写；
- `danger-full-access`：仍受进程/时间/输出限制，是否开放网络由独立 egress policy 决定；
- sandbox 不可用且请求的模式不是 `off/danger-full-access-approved` 时，命令不运行；
- `supported/active/enforcement` 必须来自 probe 和实际 process receipt，不得从配置值推断。

#### 验收

- 读取 `/etc/passwd`、`$HOME/.ssh`、workspace symlink escape、`..`、proc fd escape、mount trick 均失败；
- workspace-write 只能写 workspace，read-only 不能写；
- network deny 下 DNS、TCP、Unix socket 越界均失败；
- background process、timeout、cancel、child process tree 都被回收；
- sandbox runner 自身失败时 command 未运行，result 明确 `runner_failed`；
- Linux/macOS/Windows 各有真实 runner CI，不能只跑 mock。

## 7. P1 与声明边界

### P1-01：真正的 provider-native compaction adapter

当前 [`runtime_builder.rs`](../rust/crates/agent-gateway/src/runtime_builder.rs) 中 `compact_session_with_provider_native` 本质是一次普通 `MessageRequest`，用 summary prompt 和可选 `extra_body` 让模型生成文本摘要。它不是 Codex/OpenAI 标准 `/responses/compact` 的等价实现。

需要：

- provider capability 明确区分 `model_summary`、`responses_compact_v1`、`responses_compact_v2`；
- v1/v2 使用专用 endpoint/request/response 类型，不复用普通 chat completion；
- 保存 provider-normalized items、retained items、request hash、attempt lineage 和 fallback reason；
- endpoint 不支持或 metadata 无法安全恢复时，回退到 AOS deterministic/model summary，并如实记录 strategy；
- 对外字段在实现前将 `provider_native_used` 更名或限定为 `provider_compaction_bridge_used`，避免误导。

官方语义参考：

- `https://developers.openai.com/api/docs/guides/compaction`；
- `https://developers.openai.com/api/docs/guides/responses-multi-agent`。

### P1-02：跨入口 backend-neutral TCK

将以下 contract 做成所有 storage/route adapter 必跑的测试套件：

- writer fencing、sequence continuity、idempotency collision；
- terminal + checkpoint atomicity；
- surface fold/request equality；
- deferred approval/question cold resume；
- compaction replace coverage；
- tool side-effect lost-ack retry；
- Agent mailbox/lifecycle；
- tenant/owner isolation。

### P1-03：统一能力注册与宣传口径

`/capabilities` 只能报告经过实际 probe 且当前调用链可用的能力。配置存在、表存在、feature 编译成功、测试 fake 可用，都不能单独把能力标为 active。

## 8. 本轮自动验证

已通过：

- `cargo fmt --all -- --check`；
- `cargo test -p runtime user_message_persistence_failure_checkpoints_failed_kernel_turn`；
- `cargo test -p runtime run_turn_propagates_api_errors`；
- `cargo test -p runtime outer_cancellation_checkpoints_a_cancelled_session_not_a_running_turn`；
- `cargo test -p web-server dual_channel_extraction_tests`：9 passed，包含真实 compaction transaction/rollback 定向测试；
- `cargo test -p runtime permission_enforcer::tests`：23 passed；
- `cargo test -p runtime sandbox::tests`：6 passed；
- `cargo test -p tools shell_permission_classifier_rejects_composition_and_programmable_commands`；
- `cargo test -p tools powershell_permission_classifier_rejects_compound_commands`；
- `cargo test -p runtime`：单元测试 564 passed、1 ignored，集成测试 12 passed，parity 测试 6 passed；
- `cargo test -p tools`：126 passed、1 ignored；
- `cargo check --workspace --all-features`；
- `scripts/check-semantic-kernel-behavior.sh`：40/40 cases passed，且每个用例均命中唯一 production trace；
- `git diff --check`。

上述证明本轮修改路径通过，不代表 P0-01/02/03 已实现，也不替代干净 Linux/Windows/macOS CI 的全量测试。

建议合并前在干净 runner 再执行：

```bash
cd rust
cargo check --workspace --all-features
cargo test --workspace --all-features
cd ..
scripts/check-semantic-kernel-behavior.sh
git diff --check
```

## 9. 研发实施顺序与关闭标准

| 顺序 | 工作包 | 关闭条件 |
| ---: | --- | --- |
| 1 | P0-01 canonical surface | 所有模型入口 request/surface hash 一致；普通 Chat 不再以 JSONL 恢复；kill/restart TCK 全绿 |
| 2 | P0-03 unified sandbox | 通用 shell 不再直接 host fallback；三 OS 至少有明确 full/unsupported fail-closed；真实 escape tests 全绿 |
| 3 | P0-02 Agent Team | 六个核心工具、mailbox、树/配额/预算/权限继承、cold recovery 全部通过故障测试 |
| 4 | P1-01 native compact | 专用 endpoint adapter、lineage、fallback 与 capability 口径完成 |
| 5 | 三方盲测 | 固定模型/工具/权限/预算，发布原始 cases、失败和 95% CI |

任何工作包只有同时具备以下内容才算 `implemented`：

```text
production entry
  + one canonical authority
  + explicit invariants
  + atomic transaction or recovery protocol
  + authorization / idempotency / resource limits
  + migration and rollback
  + negative and process-fault tests
  + model-visible capability evidence
```

只有 trait、表、DTO、feature flag、文档、mock 或 happy-path unit test，状态只能是 `scaffolded`，不能关闭 P0。

## 10. 可以和不可以对外说什么

在 P0 完成前，可以准确声明：

- AOS 具备 durable multi-tenant execution kernel、proof-carrying exact-window compaction、typed evidence-backed Memory、四层 provider request lineage 和业务语义治理；
- AOS 在 Memory 数据治理、多租户控制面、NL2SQL/PM 语义层形成了区别于本地 coding agent 的研究价值；
- 当前通用 Agent Team、全入口 canonical surface 和跨平台 shell isolation 正在按公开 RFC 补齐。

不可以声明：

- “已全面对齐/超越 Codex 和 DeepSeek Harness”；
- “所有入口零丢失恢复”；
- “provider-native compaction 已等价支持 `/responses/compact`”；
- “workspace-only 沙箱已在通用 Agent shell 生效”；
- “已有表和专家子任务，所以已具备通用多 Agent”。

完成 P0 以后仍不能只凭源码宣称“效果全面超过”。最终领先声明必须有同一模型、同一仓库快照、同一工具与权限、同一 token/时间预算的去品牌盲测，并公开正确率、任务完成率、恢复成功率、越权率、p50/p95、token/cost、失败样本和置信区间。

## 11. 最终判断

AOS 不是一个“只有宏观框架”的项目；其 durable kernel、Context/Prompt lineage、Memory 和业务语义治理已经足以让外部工程师认真研究。当前 7.6/10 的状态也高于一般 agent demo。

但若目标是让熟悉 Codex/DSH 源码的评审认可“核心架构已经最终对齐”，本文三个 P0 不能省略。最重要的下一步不是继续增加产品功能，而是把所有模型输入统一到 canonical event surface、把现有零散编排收敛为 durable Agent Team、把所有 shell 收敛到真实且 fail-closed 的 sandbox backend。完成这些后，AOS 才具备可信地讨论“对齐甚至赶超”的基础。
