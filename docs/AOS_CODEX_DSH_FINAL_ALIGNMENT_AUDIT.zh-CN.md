# AOS、Codex 与 DeepSeek Harness 核心架构复审

> 审计日期：2026-08-22
> AOS 复审起点：`b13367059c1c2334ff71a563caf18a3cf185aedf`
> OpenAI Codex：`4f39251a010a8bd7d692d25fb33832ff06f1635a`
> DeepSeek Harness（DSH）：`b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`
> 最终 AOS 版本：本报告所在提交

本报告不是按 README 或架构文档做的功能表对照。固定上述提交后，实际复核的核心源码至少包括：

- Codex：`codex-rs/app-server/src/{message_processor,thread_state,transport,connection_cleanup}.rs`，以及 `codex-rs/core/src/session/{turn,step_context,turn_context,turn_suspension}.rs`、`core/src/tools/parallel.rs`、`core/src/context_manager/`、`core/src/{compact,compact_remote,thread_manager}.rs`；
- DSH：`packages/core/agent-loop/src/{agent,tool-calls}.ts`、`packages/core/session/src/{index,surface,request-header,repair}.ts`、`packages/core/agent/src/{dispatch,inbox,invariant}.ts`、`packages/core/tools/src/index.ts`，以及 `packages/session/session-persistence/src/{coordinator,write-behind,preparations}.ts`；
- AOS：`rust/crates/runtime`、`agent-gateway`、`agent-protocol`、`tools`、`web-server` 中的 turn、context、tool、compaction、ledger、session ownership、shutdown 与 Agent Team 生产路径及故障测试。

## 1. 先回答：Codex harness 是否主要在 app-server

结论是：**`app-server` 是 Codex 很重要且成熟的控制面，但不是模型 harness 执行核心的主要所在地。**

`codex-rs/app-server` 主要负责：

- JSON-RPC/transport、连接和能力协商；
- thread/turn 生命周期 API、listener generation fencing、通知顺序；
- bounded ingress/outbound channel；
- interrupt 排序、连接清理、graceful restart drain；
- 把多客户端请求稳定地映射到 Core thread。

真正执行 sampling loop、上下文构造、工具调度、压缩和取消的核心仍在 `codex-rs/core`：

- `core/src/session/turn.rs`
- `core/src/session/step_context.rs`
- `core/src/session/mod.rs`
- `core/src/context_manager/`
- `core/src/tools/parallel.rs`
- `core/src/compact.rs`、`compact_remote.rs`
- `core/src/thread_manager.rs`

持久化和恢复又分布在 rollout、state、thread-store 等 crate。只阅读 `app-server` 会高估控制面、漏掉真正的 step snapshot、context、tool cancellation 与 compaction 实现。

## 2. 三方核心实现方式

### 2.1 Codex harness

Codex 采用“强控制面 + Core thread/turn state machine + request-scoped snapshot”的分层方式。

每个 sampling step 构造一次不可变 `StepContext`。其中同时固定模型能力、reasoning、审批策略、环境快照、MCP binding、工具 router、capability roots 和 `AGENTS.md`。Provider 请求与随后执行的工具都引用同一个 step snapshot，避免“模型看见工具 A，运行时却执行了热更新后的工具 B”。

工具调用是异步任务。`tools/parallel.rs` 为每次 invocation 传递 `CancellationToken`，取消时 abort dispatch task、等待 settle、生成 aborted tool output 并发送 aborted lifecycle 通知。并行与独占工具通过读写 gate 排序。最新版又加入 unfinished root turn suspension：先 flush persistence，再取消 execution，等待 grace settle，超时才 abort，最后 flush/close writer 后才发布 thread stop，体现出“生产者停止、持久化收敛、所有权移交”的严格顺序。

`app-server` 则提供多客户端控制面：有界 transport channel、per-thread listener、listener generation、pending interrupt queue，以及 shutdown drain。这个控制面是三方中最完整的单体 server contract。

ContextManager 保留原始 response items、规范化 tool call/result、记录 history version，并为 provider 生成模型可见历史。压缩同时支持本地和 remote 路径，和 thread rollout/replay 集成紧密。

### 2.2 DeepSeek Harness

DSH 采用 Cordis composition，把 session、agent-loop、LLM adapter、tools、persistence 和 invariants 拆成可组合 package；它没有 Codex `app-server` 同等级的单体控制面。

DSH 最强的是“可执行不变量”：

- Agent 有显式 `idle / maintenance / running` phase；
- `Session.append` 在接受边界 deep snapshot、deep freeze，禁止 reentrant append；
- `Session.deriveMessages()` 是模型历史唯一 authority，surface append/replace 决定模型可见节点；
- Provider I/O 前 invariant 重新读取 live Session，比较 `messages` 与 `deriveMessages()`，并比较 model、system、temperature、maxTokens、stop、tools 与 folded request header；
- session persistence 有 durable append、sequence/revision fencing、torn-tail repair、write-behind 和显式 flush barrier。

工具调度采用 bounded rolling pool，而不是按一次模型输出无限创建 worker。后续调用在真正启动前重新分类；dispatch 可以并发，但 policy、tool result 和 additional context 严格按模型顺序提交。取消后停止补充新调用、drain 已启动调用，并为未启动调用补齐 synthetic error result，保证 replay 仍是合法的 call/result 序列。

### 2.3 AOS harness

AOS 采用“typed runtime + SQLite canonical ledger + semantic execution kernel + durable product control plane”。核心路径为：

- `runtime::ConversationRuntime` 负责 turn/step、permission、tool lifecycle 和 in-turn compaction；每个 sampling step 在 Provider I/O 前冻结权威工具集合与 lifecycle contract，模型返回后的授权和 durable intent 复用同一快照；
- `RuntimeCancellationToken + ToolInvocationContext` 把 turn、invocation ID、monotonic timeout/deadline 和父子 cooperative cancellation 传到每个工具；
- parallel tool 使用有界 rolling pool：durable start 后才 dispatch，补池前重读 live execution mode，完成可乱序，result/post-hook/session commit 严格按模型顺序；
- `AgentSessionManager` 负责 session ownership、hot reload、per-session exclusion、stream/resume/cancel；
- `SemanticKernelStore` 在 Provider I/O 前提交 context/prompt/tool lineage 和模型可见 exact manifest；
- SQLite Ledger 是恢复 authority，JSONL 仅作兼容导入导出；
- compaction 有 exact archive、replacement provenance、hash/token proof 和事务性 Memory hook；
- Agent Team 有 durable roster、mailbox、task、permit、lease generation fencing、冷恢复和递归取消；
- Super Assistant 的事件流有 durable cursor replay，live SSE 是可丢失 projection，不是恢复 authority。

AOS 的差异化优势不是复制 coding agent，而是把多租户 Memory、业务语义状态、Context/Prompt/Tool/Wire lineage 和 durable Agent Team 放入同一审计闭环。

## 3. 诚实评分

评分只衡量 harness 架构和底层实现，不衡量模型回答效果、UI 或代码量。10 分表示主路径、故障路径、恢复、不变量和负向测试都接近完整。

| 核心方向 | 权重 | AOS | Codex | DSH | 判断 |
| --- | ---: | ---: | ---: | ---: | --- |
| 控制面与进程生命周期 | 12% | 8.9 | 9.8 | 8.5 | Codex app-server 最成熟；AOS 已补齐 turn/tool settle barrier；DSH 偏组合式 SDK |
| Agent loop 与 step 一致性 | 15% | 9.5 | 9.7 | 9.7 | AOS 已冻结 step 工具集合/contract；Codex `StepContext` 覆盖环境与 router 更完整，DSH phase/invariant 极强 |
| Context authority 与请求真实性 | 15% | 9.4 | 9.5 | 9.9 | DSH dispatch-time reconstruction 最严格；AOS lineage/proof 更丰富 |
| 压缩与长期连续性 | 13% | 9.4 | 9.7 | 9.2 | AOS exact archive/provenance 强；Codex remote continuation 更成熟 |
| 工具调度、顺序与取消 | 14% | 9.6 | 9.8 | 9.7 | AOS 已实现 async invocation、live reclassification、ordered commit、timeout/cancel drain、synthetic result 与内核级进程组终止 |
| Durability 与 crash recovery | 13% | 9.6 | 9.3 | 9.9 | DSH persistence contract 最纯；AOS SQLite ledger/事务闭环很强 |
| 多 Agent 编排 | 10% | 9.3 | 9.6 | 8.8 | AOS durable team 有独特价值；Codex tool/runtime 结合更紧 |
| 沙箱、扩展与可观测 | 8% | 8.8 | 9.7 | 9.1 | AOS shell/workspace cancellation 已贯通且不再依赖外部 `kill`；非 Linux full sandbox 仍 unavailable |
| **加权总分** | **100%** | **9.4** | **9.6** | **9.4** | **AOS 核心能力已对齐；并列分数不代表相同优势，差异主要是跨平台成熟度、step 覆盖面和生产验证规模** |

排名不是所有方向都按总分排序：

- Codex 在统一控制面、step snapshot、异步工具取消、coding sandbox 和产品化 lifecycle 最强；
- DSH 在 session surface、dispatch-time invariant、bounded scheduler 和 persistence contract 最强；
- AOS 在 durable semantic ledger、proof-carrying compaction、长期 Memory、多租户治理和 durable Agent Team 更有研究价值。

## 4. 本轮发现并已修复的问题

### 4.1 SSE 背压从“伪有界”改为真实有界

旧实现的 bounded channel 满时会为每个事件 `tokio::spawn(sender.send(...))`。这会把 channel 背后的 waiter task 变成无界队列，并可能重排结构事件。

现在不再创建后台 waiter。高频 text/thinking/tool-input/hook delta 在背压下允许丢弃，并为结构事件预留 channel 容量。最终响应和可恢复事件仍以 TurnResult/SQLite Ledger 为 authority。

### 4.2 工具并行增加 harness 级硬上限

旧 `GatewayToolExecutor::execute_batch` 会按模型一次返回的调用数创建 OS thread。现在使用固定大小的 rolling worker pool；`AOS_MAX_PARALLEL_TOOL_CALLS` 默认 8、合法范围 1..32。完成一个调用后 worker 立即领取下一个，不等待整批最慢调用；结果仍按模型顺序返回，worker panic 被转换为显式 tool error。

### 4.3 Provider exact context 反向校验

旧 dispatch 只校验 context manifest ID/hash 的引用关系，没有把实际 wire `system/messages` 与加密 exact manifest 反向比较。

现在 turn-bound dispatch 必须：

1. 解密 scoped-AAD exact context manifest；
2. 重算并校验 ciphertext plaintext hash；
3. 从 exact runtime messages 重建 Provider messages；
4. 比较实际 `system/messages`，任一漂移都在 Provider I/O 前 fail closed。

### 4.4 Web Server 关停等待 Agent turn

旧关停只等待 HTTP server/调度器，不等待 Agent turn，却仍清除 unclean marker。

现在 shutdown signal 先关闭 turn admission；活动 turn 在 grace period 内自然结束，超时后发送 cooperative cancellation，再等待 durable cancellation checkpoint。只有所有 admitted turn 收敛后才清除 unclean marker，否则保留该 marker 供下次启动恢复。

新增配置：

- `AOS_AGENT_TURN_SHUTDOWN_GRACE_SECS`，默认 30；
- `AOS_AGENT_TURN_CANCELLATION_GRACE_SECS`，默认 10。

### 4.5 Batched cancellation 补 durable terminal

旧 batched turn cancel 只回滚内存消息，可能留下 running kernel turn。现在取消后显式提交 `RuntimeTurnTerminalStatus::Cancelled`；即使 turn 已标记 rolled back，kernel 仍得到唯一 cancelled terminal checkpoint。

### 4.6 ToolExecutor 升级为 async invocation contract

旧 runtime 在 async turn future 内直接调用同步 `execute_outcome/execute_batch`，导致外层 cancel future 无法获得调度。现在 production 主路径是 owned executor clone 上的 `execute_invocation`，请求携带：

- turn ID、invocation ID、iteration；
- monotonic start/timeout/deadline；
- level-triggered 父 turn token 与 invocation token，同时提供 async waiter 和 blocking atomic flag；
- 原有 durable runtime tool contract。

兼容同步工具在 `spawn_blocking` 内执行，但 runtime 不会丢弃 JoinHandle：取消或 timeout 后仍等待 worker settle，避免 terminal checkpoint 早于迟到副作用。单个工具 timeout 只触发其 invocation token，不误取消 sibling/turn；Gateway 把 token 继续传给 shell、isolated workspace 和 MCP。

### 4.7 工具调度改为 durable bounded rolling pool

旧实现先把整批调用全部记为 started，再进入 executor batch。现在 runtime 自己维护滚动池：

1. 单个调用 durable authorize/start 成功后才进入 executor；
2. in-flight 不超过 executor 声明的硬上限；
3. 任一调用完成后立即补充一个调用，不等待整批最慢项；
4. 每次补池前重读 live execution mode；前序 commit 导致的独占模式会形成 barrier；
5. 完成顺序可以不同，但 durable outcome、post-hook、session message 严格按模型顺序提交；
6. cancel 后停止补充，drain 已启动调用，为未启动调用写 synthetic cancelled result；
7. durable authorize/start 失败时先 drain，并只提交模型顺序上的连续已知前缀；ordered commit 自身失败时禁止后续结果越过缺口，不丢弃 worker；
8. 外层 coordinator 在 drain 返回后才提交 turn cancelled checkpoint。

### 4.8 终态区分 Expired、Cancelled 与 OutcomeUnknown

单工具 timeout 且本地完成回收时写 `Expired`；turn/user cancellation 且本地 shell/workspace 能确认子进程停止时写 `Cancelled`。MCP 或其他外部边界在 transport future 被中止后，远端可能已经接受了副作用；这种情况写 `OutcomeUnknown`，不能错误声明成功、失败或已取消。这些状态都进入 canonical ledger 和恢复路径。

### 4.9 本地文件写入增加取消提交边界

旧 `write_file/edit_file` 只在工具总入口检查一次 cancellation，权限检查和内容准备期间到达的取消仍可能继续写盘。现在两者在解析前和第一次文件系统 mutation 前都检查 invocation atomic flag；最后一次检查是明确的线性化点。取消先到则返回 `Interrupted`、保持原文件不变并持久化 `Cancelled`；操作先越过提交边界则等待真实写入结果，不能把已发生的本地副作用误报为取消。

### 4.10 Ordered finalize 失败不再越过缺口

旧并行提交路径会在 `finish_tool` 成功前先取走 slot、推进 `next_to_commit`。如果第 N 个 durable finalize 失败，settle 可能跳过 N 而提交 N+1。现在提交游标只在 finalize 成功后推进；失败时仍 drain 所有已 dispatch worker，但 ledger/model history 只能保留失败位置之前的连续前缀，后续 outcome 留给 canonical recovery，不能跨越因果缺口。

### 4.11 Terminal checkpoint 失败向调用方传播

旧 Gateway 外层 settle 会忽略或只记录 `finish_latest_kernel_turn` 错误，可能在 cancelled/failed terminal 尚未持久化时仍向调用方宣布取消完成。现在流式和批量路径都会把 terminal checkpoint 失败提升为 `GatewayError::Runtime`；context-window rollback 仍执行，但持久化失败不能伪装成成功的所有权收敛。

### 4.12 Shell 进程组终止改为内核级信号

旧 foreground shell cancellation 通过再启动 PATH 中的外部 `kill` 命令向负 PGID 发信号。不同 Unix runner 的参数解析或启动失败会退化成只杀 shell；后台后代继续持有 stdout/stderr 管道，调用会等待到 `sleep 30` 自然结束。现在 Unix 路径直接调用安全封装的 `killpg(SIGTERM/SIGKILL)`，`ESRCH` 仅表示进程组已经收敛，其他失败显式降级并记录。取消仍先给 250ms graceful window，再强制终止、回收根进程，最后才读取完整管道和返回 settle barrier。

### 4.13 Sampling step 冻结工具 authority

旧 runtime 在 Provider 返回后重新读取 live `is_tool_call_allowed` 和 `tool_contract`。若 registry 在 Provider I/O 期间变化，可能出现模型看见工具 A、执行时却按 B 的 contract 授权。现在 production `ApiClient` 明确声明工具列表是本 step 的完整 authority；runtime 在网络调用前冻结工具集合与每个 lifecycle contract，模型返回后的 availability、permission、durable intent 和 outcome 使用该快照。执行模式仍在每个调用真正 dispatch 前实时读取，因为独占/并行降级属于安全 barrier，不应被旧快照覆盖。

这使 AOS 达到 request/tool-contract 级 step 一致性，但还不能等同于 Codex 完整 `StepContext`：AOS 的 environment snapshot、MCP session binding、approval policy 和 executor router 尚未统一装入一个单独的不可变对象，因此本报告把该项提高到 9.5，而不是营销性地写成 9.7 或 9.8。

## 5. 仍存在的主要差距

本轮没有再发现未实现的同等级 harness 核心模块；但以下成熟度差距必须保留，不能包装成“全面第一”：

1. **Codex 控制面和跨平台 sandbox 更成熟。** AOS 非 Linux 环境仍 fail closed 为 isolation unavailable；Codex 的 app-server 协议兼容、客户端生态和进程执行覆盖更广。
2. **完整 step snapshot 仍以 Codex 为标杆。** AOS 已冻结模型可见工具集合和 contract，也已固定 exact context/prompt lineage；但 environment、MCP binding、approval policy 与 executor router 还没有收束成单一 `StepContext`。继续改造需要跨 runtime/gateway/MCP 的版本化 contract，不能只增加一个同名结构体。
3. **DSH session/persistence 不变量更纯。** DSH 的 deep-freeze append、surface fold 和 folded request-header dispatch invariant 是更小、更容易独立验证的 contract；AOS 提供等价目标和更丰富 lineage，但实现面更大。
4. **远端副作用无法被本地 harness 绝对撤销。** AOS 现在如实记录 `OutcomeUnknown`。要进一步改善，需要 MCP server/业务 API 自身支持 cancellation/idempotency/compensation，而不是在 harness 里虚构保证。
5. **生产验证规模仍需时间积累。** Codex 的真实用户量、平台覆盖和长期故障样本明显更多。架构对齐不等于已获得同等运行历史。

因此本报告可以声明“AOS 的核心 harness 架构已经对齐，并在 durable semantic governance、Memory、proof-carrying compaction 和 Agent Team 上形成超集”，但不能声明“AOS 在所有工程成熟度上全面超过 Codex”。

## 6. 验证证据

本轮新增负向测试覆盖：

- shutdown 后拒绝新 turn，活动 guard drop 后 idle barrier 被唤醒；
- 工具峰值并发不超过配置，结果顺序不变，panic 不越过 harness；
- SSE 小容量满载时 delta 降级且结构事件保留，不产生后台 waiter；
- exact context 的 system 或 messages 任一漂移都 fail closed；
- 使用生产式 scoped encryption 的完整 Provider attempt/retry/fallback lineage；
- parallel dispatch 可以乱序完成，但 tool result 始终按模型顺序提交；
- 前序结果改变 live execution mode 后，未启动调用降级为独占 barrier；
- cancel 后滚动池停止补充，queued invocation 从未进入 executor；
- 已启动 blocking invocation 观察同一 token 并在 turn terminal 前完成 drain；
- durable authorization 失败时已启动调用仍 drain 并提交 outcome；
- durable ordered finalize 失败时已启动调用全部 drain，但后续结果不会越过失败 slot 提交；
- invocation timeout 只取消该工具并持久化 `Expired`；
- cancelled external transport 在无法排除远端副作用时持久化 `OutcomeUnknown`；
- cancelled `write_file/edit_file` 在提交边界前保持原文件不变，并映射为本地 `Cancelled`；
- foreground shell cancellation 通过内核级 PGID 信号在限定时间内终止并回收进程组；
- Provider 返回后 live client 即使发生变化，本 step 仍只能使用 Provider 前冻结的权威工具集合和 lifecycle contract。

最终门禁以本报告所在提交的 CI/本地执行结果为准：

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --all-features` | 通过 |
| `cargo clippy --workspace --all-features` | 通过；仓库既有 pedantic warnings，无 error |
| `cargo test --workspace --all-features` | 通过；全部非 ignored 测试 0 失败，联网/配额类测试按声明 ignored |
| `scripts/check-semantic-kernel-behavior.sh` | 通过；40 个 production trace cases |
| `git diff --check` | 通过 |

## 7. 最终结论

AOS 的 context、compaction、durability、Memory 和 Agent Team 已经具备开源研究价值，不是只有宏观框架。它在 semantic durability 方向有 Codex/DSH 没有的独特设计；本轮又补齐了此前唯一明确的核心短板：统一 async/cancellable tool invocation、live-reclassified durable rolling scheduler、ordered commit 和 settle-before-terminal。

诚实结论是：当前综合 harness 评分约 **AOS 9.4、Codex 9.6、DSH 9.4**。AOS 的核心架构已经对齐，且在 durable semantic ledger、长期 Memory、compaction proof 和多 Agent 治理上超过两者的公开实现；Codex 仍以完整 `StepContext`、控制面、跨平台执行和生产成熟度领先，DSH 仍以 session/persistence contract 的纯度领先。同为 9.4 不表示 AOS 与 DSH 的长短板相同；回答效果是否领先仍必须用同模型、同权限、同工具和同预算盲测证明。

## 8. 达到 9.7+ 的真实验收线

以下是仍值得继续的工程，不计入当前分数，也不应仅通过增加同名类型宣称完成：

1. **Agent loop / StepContext**：把 model capability、reasoning、environment/world state、approval policy、MCP binding generation、tool router 与 instruction lineage 收束成版本化不可变 step authority；Provider wire 与 tool dispatch 都必须反向校验该 authority。把当前隐式 loop 分支改为可测试的 `preparing -> sampling -> committing -> executing -> compacting/suspended/completed` transition matrix，并覆盖每个阶段 crash/cancel/retry。
2. **上下文压缩与 Session 连续性**：保留 AOS exact archive/proof/Memory 主路径；为支持的 Provider 增加可验证 remote continuation adapter，但 fallback 必须仍能从 canonical archive 本地重建。进一步把模型可见历史抽成更小的 append/replace surface fold contract，并增加 torn-tail、重复 compaction、remote/local 切换的差分测试。
3. **多 Agent 编排**：让 `agent_team_tasks.blocked_by_json` 从存储字段升级为有环检测、dependency completion、失败传播和原子 ready transition 的真实 DAG；对 `write_scopes_json` 增加冲突 lease；为 parent fan-in 增加有预算的 artifact merge、部分成功和 deadlock detection。只有生产 worker claim/renew/recovery 全部消费这些约束，才能计入评分。
4. **成熟度证据**：增加 Linux/macOS/Windows 分层故障矩阵、MCP server restart/upgrade、Provider fallback、SQLite torn-write/ENOSPC、长会话多次压缩和百任务 Agent Team soak；没有这些持续证据，架构设计分可以接近 9.7，生产成熟度仍不能与 Codex 等同。
