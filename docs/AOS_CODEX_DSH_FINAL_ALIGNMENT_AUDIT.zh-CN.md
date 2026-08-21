# AOS、Codex 与 DeepSeek Harness 核心架构复审

> 审计日期：2026-08-22
> AOS 复审起点：`0b1ddbbde1b0dafd15389cf3f05ab3f40e74568f`
> OpenAI Codex：`00a7b888b23715989db19b74f6cb623ca46be620`
> DeepSeek Harness（DSH）：`b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`
> 最终 AOS 版本：本报告所在提交

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

工具调用是异步任务。`tools/parallel.rs` 为每次 invocation 传递 `CancellationToken`，取消时 abort dispatch task、等待 settle、生成 aborted tool output 并发送 aborted lifecycle 通知。并行与独占工具通过读写 gate 排序。

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

- `runtime::ConversationRuntime` 负责 turn/step、permission、tool lifecycle 和 in-turn compaction；
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
| 控制面与进程生命周期 | 12% | 8.7 | 9.8 | 8.5 | Codex app-server 最成熟；AOS 本轮补齐 turn drain；DSH 偏组合式 SDK |
| Agent loop 与 step 一致性 | 15% | 9.0 | 9.7 | 9.7 | Codex `StepContext` 最完整；DSH phase/invariant 极强 |
| Context authority 与请求真实性 | 15% | 9.4 | 9.5 | 9.9 | DSH dispatch-time reconstruction 最严格；AOS lineage/proof 更丰富 |
| 压缩与长期连续性 | 13% | 9.4 | 9.7 | 9.2 | AOS exact archive/provenance 强；Codex remote continuation 更成熟 |
| 工具调度、顺序与取消 | 14% | 8.4 | 9.8 | 9.7 | AOS 已有 bounded order-preserving batch，但 blocking tool 取消仍弱 |
| Durability 与 crash recovery | 13% | 9.5 | 9.2 | 9.9 | DSH persistence contract 最纯；AOS SQLite ledger/事务闭环很强 |
| 多 Agent 编排 | 10% | 9.3 | 9.6 | 8.8 | AOS durable team 有独特价值；Codex tool/runtime 结合更紧 |
| 沙箱、扩展与可观测 | 8% | 8.5 | 9.7 | 9.1 | AOS 非 Linux full sandbox 仍 unavailable；Codex 跨平台更完整 |
| **加权总分** | **100%** | **9.0** | **9.6** | **9.4** | **AOS 已是高质量 harness，但尚不能诚实宣称全面超过二者** |

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

## 5. 仍存在的主要差距

### P1：运行中的 blocking tool 不能统一可靠中止

这是本轮结束后唯一明确的 harness 核心差距，也是 AOS 工具调度只给 8.4 的原因。

`runtime::ToolExecutor` 的 production 接口仍以同步 `execute/execute_batch` 为核心。外层 `tokio::select!` 可以取消 Provider future，但当 runtime thread 正阻塞在 MCP、同步 HTTP、shell 或其他 blocking tool 内时，取消分支不能及时获得调度。AOS 的 sandbox backend 本身支持 cooperative cancellation，但该 token 尚未贯穿统一 ToolExecutor invocation。

不能用“把 blocking 调用扔进 `spawn_blocking` 后丢弃 JoinHandle”冒充修复：worker 和副作用仍可能在 turn 已记录 cancelled 后继续运行。

后续正确改造必须作为一个完整 contract 落地：

1. 把 ToolExecutor 主路径改为 async invocation，输入包含 `invocation_id / cancellation token / deadline / contract`；
2. shell/sandbox 取消必须 kill 并 wait process tree；MCP/HTTP 使用同一 cancellation signal；文件写在 commit 前再次检查取消；
3. 调度器改为 rolling pool，启动前重读 execution mode，停止后不再补充 worker；
4. dispatch 可并发，但 durable call/result、post-hook、additional context 必须按模型顺序提交；
5. 未启动调用补 synthetic aborted result，已启动调用必须 settle 后才能提交 turn cancelled；
6. shutdown barrier 同时等待 active turns 和 active tool invocations，超时必须保留 unclean marker；
7. 增加进程树回收、MCP abort、取消时写入竞争、ordered result、restart repair 的故障注入测试。

这不是当前补丁可以安全局部修改的细节，而是公共执行接口迁移。未完成前，对外不能声明 AOS 的运行中工具取消已达到 Codex/DSH 水平。

## 6. 验证证据

本轮新增负向测试覆盖：

- shutdown 后拒绝新 turn，活动 guard drop 后 idle barrier 被唤醒；
- 工具峰值并发不超过配置，结果顺序不变，panic 不越过 harness；
- SSE 小容量满载时 delta 降级且结构事件保留，不产生后台 waiter；
- exact context 的 system 或 messages 任一漂移都 fail closed；
- 使用生产式 scoped encryption 的完整 Provider attempt/retry/fallback lineage。

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

AOS 的 context、compaction、durability、Memory 和 Agent Team 已经具备开源研究价值，不是只有宏观框架。它在 semantic durability 方向有 Codex/DSH 没有的独特设计。

但诚实结论不是“全面赶超”：当前综合 harness 评分约 **AOS 9.0、Codex 9.6、DSH 9.4**。AOS 与两者的主要核心差距已经收敛到统一 async/cancellable tool execution contract，以及非 Linux full sandbox。完成前者并通过故障注入门禁后，AOS 才有依据把总分提高到约 9.3-9.4；回答效果是否领先仍必须用同模型、同权限、同工具和同预算盲测证明。
