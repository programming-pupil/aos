# AOS 语义内核大重构规格

> 状态：Implementation candidate；生产路径已接入，效果门槛仍以第 17 节评测为准
> 日期：2026-08-15
> 目标版本：AOS Next
> 适用对象：架构、Runtime、Memory、PM、NL2SQL、数据归因、评测与前端团队

## 0. 文档结论

AOS 当前不是“能力弱”，而是“工程能力已经很宽，但底层正确性仍主要依赖模型临场发挥，真实领先尚未被证明”。继续增加 Agent、Planner、Reviewer、工作流节点和 Prompt，不会自动形成相对 Codex 或 DeepSeek Harness 的壁垒。

本次重构的核心不是把 Harness 做得更复杂，而是建立四项可持续积累、可验证、可迁移的基础能力：

1. **Unified Agent Protocol**：让 Thread、Turn、Tool、Approval、Artifact、Checkpoint 和 Child Thread 基于同一运行事件账本恢复、取消、分叉和重放。
2. **Semantic State Kernel**：把对话中的事实、约束、决策、需求、指标口径和未决问题变成有版本、有来源、有时间语义的状态，不再把聊天记录或摘要当作唯一事实源。
3. **Evidence-bound Context Engine**：压缩、记忆、检索和 Prompt 注入围绕同一份语义状态与证据账本工作，摘要只用于导航，原文、证据和状态可精确回溯。
4. **Domain Verifiers**：PM 用需求状态和证据支持度验证结果；NL2SQL 用指标、粒度、人群、时间、Join 和结果不变量验证业务语义。执行成功不再等于回答正确。

本轮二次源码审计还确认了七项必须作为上述内核的生产前提，而不是“以后再优化”：真正的工具渐进披露、通用 Artifact/Spill、可守恒资源预算、具备 writer fencing 的 Ledger 持久性与损坏恢复、provider 回放与故障注入、durable capability token、全链路敏感数据投影。这些能力不会直接替代 PM/NL2SQL verifier，但缺少它们，领域优势无法稳定复现，也无法证明安全、成本和恢复体验。

最终判断如下：

- AOS 的精确归档、混合检索、PM 研究编排、NL2SQL 工程面和企业产品面有真实局部优势。
- AOS 当前的 Memory 整体不能宣称强于 Codex。Codex 的自动抽取、全局 consolidation、证据分层和污染治理更成熟。
- AOS 的长期 Memory 产品面大概率强于当前 DeepSeek Harness，但压缩提交协议、边界保护和溯源完整性仍弱于 DSH。
- AOS 当前 PM 和 NL2SQL 能力很强，但大量优势可由 Codex/DSH 接入相同工具、知识库和 Prompt 后复制。真正的壁垒应来自持续演化的领域状态、验证器、反馈学习和专有评测数据，而不是工作流节点数量。
- AOS 不需要在代码研发上正面复制 Codex。AOS 应成为“可持续理解业务、保持长期语义状态、对业务答案负责”的通用工作系统。

---

## 1. 审计范围与证据边界

### 1.1 源码基线

| 系统 | 审计版本 | 时间 | 说明 |
|---|---|---|---|
| AOS | `3f97bb3f01d36b675c3c48a63123f0415fed202d` | 2026-08-13 | 同时审阅了 2026-08-14 工作区中的未提交 PM、NL2SQL、归因和 Super Assistant 改动；本文不修改这些文件 |
| OpenAI Codex | `cbe85e117b1db59cdbe8175c59793c3cf2a4a7b8` | 2026-08-14 | 重点审阅 compaction、rollout reconstruction、memory phase 1/2、tool search/spec plan、budget、rollout trace 和 prompt 注入 |
| DeepSeek Harness | `47f943859bef60e4160492346772ded9b24f765a` | 2026-08-13 | 重点审阅 compaction、output retention/spill、session persistence/repair、LLM replay、subagent inheritance 和 replacement 溯源 |

### 1.2 本文不做的虚假推断

源码能证明“机制存在”，不能证明“效果更好”。因此本文不会根据以下现象宣布领先：

- 模块、Agent、Prompt 或工作流节点更多；
- 单元测试数量更多；
- 能执行 SQL、能生成 PRD、能输出引用；
- 摘要中出现了原文关键词；
- SQL 能解析、EXPLAIN 成功或查询有返回值；
- 报告格式完整、引用数量多或输出更长；
- 在没有同模型、同工具、同预算盲测的情况下比较产品效果。

所有“赶超”结论必须同时满足：同一任务、同一模型级别、同一工具权限、同一数据、同一时间/Token 预算、盲评、可复现。

---

## 2. 产品定位与非目标

### 2.1 AOS 的目标定位

AOS 的核心定位应从“提供很多 Agent 的平台”调整为：

> **AOS 是一个持久化业务语义状态、能发现真实需求、能生成并验证业务答案的 Agent Operating System。**

目标用户不是只需要一次回答的人，而是需要系统在数周、数月甚至跨项目持续理解以下内容的人：

- 组织如何定义业务指标；
- 某项需求为什么提出、为谁解决什么问题；
- 哪些内容是事实、假设、约束、决策或已经失效的旧结论；
- 当前还缺哪些信息，下一问问什么最有价值；
- 一个分析结论由哪些数据和口径支持；
- 后续反馈是否推翻了原来的假设或答案。

### 2.2 明确非目标

- 不以复制 Codex 的代码编辑、仓库理解和终端体验为第一目标。
- 不以支持最多模型、最多 Agent、最多工具作为核心成功指标。
- 不为“架构先进”拆分大量微服务；优先在现有 Rust workspace 中形成清晰内核边界。
- 不把通用 LLM Judge 当作唯一质量裁判。
- 不承诺“零丢失”“因果归因”“企业级正确率”等未经测量的营销结论。

---

## 3. 当前真实差距矩阵

### 3.1 Memory 与压缩

| 能力 | AOS 当前状态 | Codex | DeepSeek Harness | 真实判断 |
|---|---|---|---|---|
| 精确历史归档 | `archive_windows`、`replacement_messages`、exact archive | replacement history + rollout reconstruction | surface replacement + `sourceEventSeqs` | 三者都有强实现；AOS 有可复用底座 |
| 长期语义检索 | embedding + lexical hybrid retrieval | 本地主要为 substring search | 未发现完整长期语义 Memory | AOS 对 DSH 明显领先，对 Codex 有局部检索优势 |
| 自动 Memory 抽取 | 生产自动压缩主要走启发式 `extract_key_info` | 独立模型执行结构化抽取和 secret redaction | 无完整长期 Memory | AOS 落后 Codex |
| 全局 consolidation | `/consolidate` 主要保存调用者摘要，默认仅 checkpoint | 独立 consolidation model、全局锁、增量 diff、分层产物 | 无完整长期 Memory | AOS 明显落后 Codex |
| 时间与冲突 | 缺少事实版本、失效、矛盾和 supersession | 有更成熟的写入、筛选和污染控制，但也不是完整知识图谱 | 不适用 | 三者都有空间，AOS 可在这里建立新优势 |
| 压缩提交安全 | 有前/中途压缩和替换历史 | 有世界状态重注入、缓存和窗口治理 | 边界保护、max-token fail closed、摘要必须更小、span stability、原子 replacement | AOS 应吸收 DSH 的协议级严谨性 |
| 压缩后效果评测 | Zero Loss probe 存在待召回事实泄题风险 | 有成熟工程测试，不等于业务 recall benchmark | 侧重压缩协议测试 | AOS 当前不能宣称零丢失或总体领先 |

直接回答“目前 AOS 压缩、记忆是否已经强于 Codex 和 DSH”：

- **不是整体更强。**
- 对 DSH：AOS 的长期 Memory、精确归档和语义检索更完整；DSH 的压缩事务与边界正确性更强。
- 对 Codex：AOS 的精确归档和混合检索有局部优势；Codex 的自动抽取、consolidation、证据分层、渐进披露和污染治理更成熟。
- AOS 真正可反超的方向不是再加一个 Memory Agent，而是实现“时间感知、矛盾感知、领域状态感知的 Memory + 可测量的回答保真”。

### 3.2 产品需求挖掘

| 现有能力 | 代码事实 | 核心限制 |
|---|---|---|
| 深度研究 | task graph、并行 probe、evidence admission、claim/evidence/URL、conflict graph、review、editor、quality gate | 强研究编排不等于强需求发现 |
| 假设图 | `hypothesisEvidenceGraph` | 节点和核心假设高度模板化，未形成可持续演化的业务假设空间 |
| 需求状态 | 主要存在于 Prompt、session summary 和最终报告 | 没有 stakeholder/job/pain/outcome/constraint/assumption/decision/acceptance criteria 的正式版本模型 |
| 冲突检测 | 从模型生成的 Conflict Matrix 再做字符串解析 | 不是对原始证据执行独立语义蕴含、数值冲突和来源时效验证 |
| 质量评分 | 引用数、域名数、关键词覆盖、结构完整度等代理指标 | 格式和覆盖率不能证明洞察正确或需求值得做 |
| PM Memory | 显式“记住/以后/默认”等触发及简单文本召回 | 不能持续维护决策、假设、反馈和需求版本 |

真实结论：Codex 或 DSH 接入搜索、同一资料和相同 Prompt 后，可以复制当前大部分“深度研究报告”输出。AOS 的 PM 壁垒必须从报告生成迁移到需求状态、问题选择、证据验证、决策记录和上线结果闭环。

### 3.3 NL2SQL 与数据归因

| 现有能力 | 优势 | 核心限制 |
|---|---|---|
| Query Understanding | intent、实体、时间、过滤、聚合、比较 | 两次独立 LLM 调用；confidence 基本固定为 `0.8`，缓存命中变 `1.0`，不是真实校准置信度 |
| Clarification | 有需求门禁和多轮上下文 | 大量依赖关键词、schema substring 和有限 profile；缺少指标版本、grain、population、denominator、timezone 等核心语义 |
| Metric hard constraint | AST 替换 projection 并追加过滤条件 | 是真实优势，但主要覆盖单个明确指标，复杂比率、多个指标和嵌套查询仍不足 |
| SQL 生成 | schema、FK、join path、metric、reference、tool loop、多模型 failover | 大量业务正确性仍依赖模型遵守 Prompt |
| 语义复核 | 当前工作区已增加独立 LLM semantic review | reviewer 与 generator 共享模型知识和输入偏差；没有确定性的语义等价证明，review 超时会继续执行 |
| SQL 安全 | `sqlparser` AST、只读限制、危险函数拦截 | 这是安全正确性，不是业务语义正确性 |
| Schema/Policy | table/column 提取、query policy、row filter | table/column 提取部分仍基于正则；不能验证 join multiplicity、grain 和口径等价 |
| EXPLAIN/执行修复 | Trino/Presto preflight、执行重试、deterministic/model repair、审计轨迹 | 能执行只证明数据库接受 SQL；修复也可能改变业务范围 |
| Result Validator | 空值、范围、基数、负数、重复 ID 等 | 未验证 metric definition、分母、人群、grain、时间窗口、join 重复计数和业务不变量 |
| Golden Cases | 澄清、SQL 安全、执行、关键词、引用文件命中 | 可把“执行成功但口径错误”的 SQL 判为通过 |
| Feedback | thumbs/correction 入库和统计视图 | 未进入训练样本、检索排序、规则候选、回归集或人工批准闭环 |
| 数据归因 | 多步计划、查询、诊断下钻、证据卡、反查、报告审计 | 当前是描述性诊断和贡献拆解，不是因果识别；`mainCauses` 仍由 LLM 从结果中解释 |

真实结论：AOS 的 NL2SQL 工程覆盖面已经很强，但“业务语义正确率”没有被现有评测证明。下一阶段不应继续以执行率为主指标，而应把自然语言先编译为可验证的分析语义 IR，再生成 SQL。

### 3.4 Harness 运行协议、工具与子 Agent

| 能力 | AOS 当前状态 | Codex / DeepSeek Harness 证据 | 真实判断 |
|---|---|---|---|
| 企业控制面 | 多租户、数据源、策略、凭证、审计、预算和业务流程已经形成产品面 | Codex/DSH 更聚焦执行 Harness 和开发者工作流 | AOS 有明确产品面优势；没有同条件体验评测时，不扩张为“所有治理效果更强” |
| 统一运行账本 | Runtime Session、`AgentEvent`、Super Assistant parent turn、PM 和 NL2SQL 各有事件/任务持久化 | DSH 以 append-only `SessionEvent`、surface projection 和 persistence contract 为核心；Codex 以 thread/turn/rollout 和 thread store 统一生命周期 | 附件判断属实：AOS 不是没有协议，而是协议分散，缺唯一运行事实源 |
| 模块化与替换 | 工具、Gateway、Runtime 能力很宽，但 `tools/src/lib.rs`、`runtime_builder.rs` 等聚合文件很大 | DSH 将 session、persistence、compaction、provider、subagent、tool、sandbox 等拆成显式 package contract | DSH 的替换边界更清晰；不应照搬全部拆包数量，应吸收 contract/invariant |
| Prompt | 有 `SystemPromptBuilder`、Profile 和场景化 Prompt | AOS 业务 Prompt 仍分散；built-in registry 默认存在 legacy prompt 路径 | Prompt Manifest 与 model variant 有必要，附件判断属实 |
| 工具生命周期 | AOS 已有审批、取消、durable task 和部分 parent suspend/resume；通用 `AskUserQuestion` 仍有 stdin adapter，领域工具状态各自实现 | Codex/DSH 对 approval、interrupt、steer、subagent/tool lifecycle 有更统一的协议 | “完全没有异步工具状态机”不属实；“尚未统一”属实 |
| Child Thread | AOS 已有 task registry、subtask control、durable parent 和 specialist task | Codex 有 parent/child thread lineage、fork、interrupt、steer 和可见线程；DSH 有 subagent lifecycle、continuation、settlement、多 driver 和 UI | AOS 有部分实现，但统一性、可见性和跨运行时控制仍落后 |
| 工具渐进披露 | 已有 `ToolSearch`、`deferred_tool_specs` 和场景过滤；但普通 Chat 默认 `allowed_tools = None`，仍会暴露全部注册工具 schema，搜索结果也不会自动成为下一次模型调用的临时工具集 | Codex 对 deferred tool metadata 做 BM25 检索，并在后续模型请求中按预算暴露命中工具 | AOS 不是没有工具搜索，而是尚未完成“搜索 -> 策略裁决 -> 临时激活 -> 失效”的闭环；默认全量 schema 会浪费上下文并增加误调用面 |
| 大工具结果保留 | 已有 `compact_tool_results_for_request`，且只压缩模型请求视图；目前主要处理 `read_file`、`grep_search`、`glob_search`，部分工具仍直接字符截断 | DSH output-retention/spill 保存完整结果，模型只看有界 head/tail、准确省略量和 opaque locator | AOS 的请求视图压缩方向正确，不应重写；缺口是跨工具、可恢复、tenant-scoped 的通用 Artifact/Spill Plane |
| 预算治理 | 月度 Token、reasoning、输出、工具族、上下文、sandbox 和子任务预算均已有局部实现 | Codex 有 token/rollout budget；DSH 有 token meter、compaction measurement 和 delegation depth | 三者都没有完整多维预算事务；AOS 当前预算分散，缺 reservation/commit/release、父子守恒和并发防超卖，这是可进一步超越的机会 |
| Ledger 持久性与损坏恢复 | Session JSONL append 和原子重写已存在，但缺统一 batch durability、连续 sequence 校验、writer fencing、torn-tail 修复、中段损坏分类和开放工具的崩溃收尾 | DSH persistence contract 明确连续 seq、per-id write coordination、revision、durable append、尾部修复、中段损坏 fail closed、event upcast 和 synthetic closer | AOS 的“可重放”尚未等于“崩溃后可信恢复”；这是 Unified Agent Protocol 上线前的 P0 正确性缺口 |
| Provider 回放与故障注入 | 有 eval harness、parity raw trace 和测试内 `ScriptedProvider`，但不能从真实 trace 通用导出并完整消费 provider stream 脚本 | DSH `llm-replay` 可从 session 重建模型流并注入 throw/hang/cancel；Codex rollout trace 将原始 payload、事件 spine 和离线语义解释分离 | AOS 不是没有测试，而是缺生产故障可确定性复现的 Harness TCK；没有它就无法证明恢复协议可靠 |
| Approval capability | 已有 `PermissionPolicy`、`PermissionEnforcer`、one-time/expiry/scope/executor binding 的 `ApprovalTokenLedger` | DSH 明确测试 child 只能降权；Codex 官方边界将 sandbox 与 approval 分层 | 设计基础较强，但 token 账本仍偏独立且内存化，未证明所有 Tool/Child/Executor 路径统一强制；shell 字符串启发式不能成为授权边界 |
| 敏感数据投影 | 已有 secret/PII 脱敏、sensitivity class、consumer capability 和 redaction provenance；Session 持久化主要保存脱敏文本 | Codex raw rollout trace 明确可能含敏感原始数据；DSH spill 使用 session 私有文件，但两者也没有完整企业多投影协议 | AOS 有反超基础，但需把 report 局部能力推广到 provider、tool、artifact、trace、client 和 telemetry 全链路，并处理受控 exact raw 的加密与删除 |
| 编码任务效果 | AOS 不是以代码 Agent 为首要定位 | Codex 的代码工具、sandbox、thread 和交互面源码更成熟 | 只能说机制和产品协同更成熟；没有同模型盲测，不能从源码断言任务成功率一定更高 |
| 实证评测 | 有 eval harness 和大量领域测试 | 三者源码测试重点不同 | 暂不能判定总体效果强弱，必须用同模型、同工具、同预算盲测 |

---

## 4. 重构原则

### P-01：聊天记录不是状态

对话消息是证据和事件，不能直接等同于当前事实。当前事实必须由状态 reducer 根据新证据、时间、冲突和用户确认生成。

### P-02：摘要不是事实源

摘要只服务于导航和低成本上下文。事实、决策、需求、指标、附件和 SQL 口径必须有结构化记录和原始证据指针。

### P-03：LLM 提议，内核裁决

LLM 可以抽取候选事实、提出假设、生成 IR 或修复 SQL；确定性代码负责 schema 校验、版本合并、权限、引用完整性、状态转换和发布门禁。

### P-04：每个结论都必须知道“为什么可信”

所有关键输出必须携带：来源、状态版本、验证结果、冲突、置信度组成和未覆盖范围。

### P-05：拒绝比错误更有价值

当关键指标、时间、人群、分母或需求目标不清楚时，系统应提出最有信息增益的一问，或明确降级为假设，不应为了完成工作流而制造确定答案。

### P-06：领域优势来自 verifier，不来自 Prompt 长度

PM 的壁垒是需求状态和证据支持度；NL2SQL 的壁垒是语义 IR 和业务验证器。Prompt 只是实现组件。

### P-07：先形成模块化单体内核，再按负载拆服务

第一阶段保持 Rust workspace 内调用，避免为了“平台化”引入分布式一致性。只有异步 consolidation、批量评测或大规模向量索引在负载证明后再独立部署。

### P-08：运行事实与语义事实分层，但必须互相引用

`Unified Agent Protocol` 记录 Thread、Turn、Tool、Approval、Artifact、Checkpoint 和 Child Thread 的运行事实，回答“系统实际做了什么、如何恢复”；`Semantic State Kernel` 记录事实、约束、决策、需求和指标语义，回答“系统当前相信什么、为什么可信”。两者不能混成一个巨型事件类型，也不能各自形成孤岛：每个语义 delta 必须引用运行事件和证据，每次模型调用必须记录所使用的 semantic snapshot。

---

## 5. 目标架构

```text
AOS Control Plane
Identity / Tenant / Policy / Credential / Datasource / Audit / Budget
       |
       v
Unified Agent Protocol + Append-only Execution Ledger
Thread / Turn / Step / Item / Tool / Approval / Child / Checkpoint
       |
       +--------------------------+
       v                          v
Semantic State Kernel       Recovery / Projection Runtime
       |                          |
       v                          |
Evidence Ledger / Exact Archive  |
       |                          |
       +-------------+------------+
                     v
Context Compiler / Tool Runtime / Memory Runtime
                     |
                     v
Domain Engines + Verifiers
Requirement Discovery / Analytics / General Assistant
                     |
                     v
Native AOS Runtime | Codex Adapter | DSH Adapter | Other Executor
```

### 5.1 建议的代码边界

不建议立即创建十几个新 crate。建议形成以下五个边界：

1. `agent-protocol`（新增）
   - 统一 Thread/Turn/Step/Item、运行状态机、事件 schema、projection contract 和 executor adapter 接口。
   - 不实现具体模型、工具或业务逻辑。

2. `semantic-core`（新增）
   - 纯类型、状态 reducer、版本、证据引用、冲突模型、Context Manifest。
   - 不依赖 HTTP、数据库实现、模型客户端或具体领域 Prompt。

3. `memory-engine`（新增，逐步吸收现有 memory continuity 逻辑）
   - 抽取、规范化、时间/冲突处理、consolidation、检索、Context Packet 编译。
   - 复用现有 exact archive、embedding 和 lexical retrieval。

4. `pm-domain`（演进为 Requirement Discovery Engine）
   - 保留研究能力，但核心输出改为 `RequirementStateDelta`，报告只是状态视图。

5. `nl2sql-core`（演进为 Analytics Semantic Compiler）
   - 新增 `AnalyticIntentIR`、`MetricContract`、binding、semantic verifier 和 calibrated confidence。
   - `web-server` 只做鉴权、传输、任务持久化和 orchestration，不再承载核心语义规则。

现有 `runtime`、`agent-gateway`、`eval-harness`、`pm-orchestrator` 和 `web-server` 保留；逐步删除其中重复的 Memory、Prompt 拼接和领域规则。

新增的 Harness 能力不对应“一项一个 crate”：Tool Capability Router 先落在 `tools` + `runtime` 的工具规划边界，Artifact/Budget/Durability 先作为 `agent-protocol` 类型和现有 Runtime/Storage 的模块，provider replay/fault kit 归入 `eval-harness`。只有独立负载、权限边界或复用需求被数据证明后再拆服务，避免为了追随 DSH package 结构增加部署与一致性成本。

### 5.2 Unified Agent Protocol v1

AOS 当前已经分别拥有 Runtime Session、`AgentEvent`、Super Assistant durable parent turn、PM run event、NL2SQL attribution event 和多种任务表；问题不是完全没有协议，而是这些协议不能作为同一条运行历史重放。v1 使用统一 envelope，领域 payload 允许扩展：

```rust
pub struct AgentEventEnvelope {
    pub event_id: EventId,
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub step_id: Option<StepId>,
    pub item_id: ItemId,
    pub parent_item_id: Option<ItemId>,
    pub sequence: u64,
    pub occurred_at: DateTime<Utc>,
    pub actor: EventActor,
    pub event: AgentEventV1,
    pub idempotency_key: Option<String>,
    pub source_event_ids: Vec<EventId>,
    pub semantic_snapshot_version: Option<u64>,
    pub schema_version: u32,
    pub batch_id: BatchId,
    pub payload_hash: ContentHash,
}

pub enum AgentEventV1 {
    Thread(ThreadEvent),
    Turn(TurnEvent),
    Message(MessageItem),
    Tool(ToolInvocationEvent),
    Approval(ApprovalEvent),
    Artifact(ArtifactEvent),
    Memory(MemoryEvent),
    Checkpoint(CheckpointEvent),
    ChildThread(ChildThreadEvent),
    Domain(DomainEvent),
}
```

核心不变量：

- 模型看到的任何内容都能由 ledger、Context Manifest 和 artifact/evidence 引用重建。
- 前端、SSE 和报表是 projection，不是第二事实源。
- Resume、Fork、Rollback、Compact 和 Replay 使用同一事件序列和 checkpoint。
- sequence 在 thread 内单调；重复提交由 idempotency key 去重。
- replacement/checkpoint 必须覆盖所有被 shadow 的源事件。
- 事件 schema 支持向前读取和显式迁移；未知领域事件不得破坏基础投影。
- 所有副作用先有 durable intent，再允许 executor 执行，最终结果回写同一 item 生命周期。

Execution Ledger 与 Evidence Ledger 分离存储但双向关联。工具“执行成功”是运行事实；工具返回的某个业务数字是否能成为当前事实，由 Semantic Reducer 决定。

### 5.3 工具与审批状态机

统一工具生命周期：

```text
proposed -> awaiting_authorization -> authorized -> started -> streaming
         -> suspended -> resumed
         -> completed | failed | cancelled | expired | outcome_unknown
```

每个 `ToolContract` 必须声明：

- 输入/输出 schema 和版本；
- side-effect class 与 risk level；
- required capability、tenant policy 和 secret scope；
- idempotency strategy；
- retry policy、timeout、deadline 和 cancellation contract；
- network/filesystem/datasource policy；
- artifact output 和 evidence conversion policy；
- compensation 或人工恢复方式；
- 可否并行、可否在父取消后继续。

`AskUserQuestion`、审批和补充凭证不能继续抽象成同步阻塞函数。它们必须生成 durable request item，Turn 进入 `suspended`，收到授权用户响应后以新事件恢复。CLI 可以提供 stdin adapter，但 Web/Server runtime 不得依赖 stdin。

副作用执行使用 intent/outcome 协议：写入 authorized intent 和唯一 idempotency key 后再调用外部系统。超时且无法确认远端状态时进入 `outcome_unknown`，禁止自动重复执行高风险操作。

### 5.4 Child Thread

AOS 已有 Task/Worker、subtask control 和 durable parent state，不能按“完全没有子 Agent”处理；缺口是不同业务链路尚未统一为可查看、可控制、可重放的 Child Thread。

Child Thread 必须具备：

- 独立上下文、事件账本和 checkpoint；
- `parent_thread_id`、spawn item 和 lineage；
- 继承权限但绝不扩大权限；
- 独立 Token、时间、深度、并发和工具预算；
- follow-up、steer、interrupt、cancel 和 resume；
- 父取消向下传播，除非 contract 明确允许 detached execution；
- settlement 协议区分 completed、failed、cancelled、timed_out 和 partial；
- 返回结构化 `ChildThreadReport`、artifact/evidence refs 和 unresolved，不把完整原始轨迹灌入父上下文；
- UI 可进入子线程查看详情，但父线程只消费经过预算约束的报告。

这部分应吸收 Codex 的 thread lineage、interrupt/steer/fork 和可见子线程体验，以及 DSH 的 subagent lifecycle、continuation、settlement 和多后端 driver 设计。

### 5.5 Executor Adapter

```rust
#[async_trait]
pub trait AgentExecutorAdapter {
    fn capabilities(&self) -> ExecutorCapabilities;
    async fn start(&self, request: ExecutorStartRequest) -> Result<ExecutorHandle>;
    async fn append_input(&self, handle: &ExecutorHandle, input: ExecutorInput) -> Result<()>;
    async fn interrupt(&self, handle: &ExecutorHandle) -> Result<InterruptOutcome>;
    async fn resume(&self, checkpoint: ExecutorCheckpoint) -> Result<ExecutorHandle>;
    async fn stream_events(&self, handle: &ExecutorHandle) -> Result<ExecutorEventStream>;
}
```

Native AOS、Codex、DSH 或其他执行器的原生事件必须映射到 Unified Agent Protocol，同时保留 `native_event_ref` 便于审计。适配器不得假装所有后端能力相同；fork、approval、streaming tool result、memory 和 sandbox 等能力必须通过 capability negotiation 暴露。

短期内适配器是战略选项，不是 P0 生产依赖。AOS 先把控制面、语义状态和领域 verifier 做强，再允许成熟执行器承担局部任务。

### 5.6 Tool Capability Router 与 Artifact/Spill Plane

AOS 已经有 `ToolSearch`、deferred tool metadata 和请求侧工具结果压缩，正确方向应保留；本次重构补齐的是二者之间尚未闭合的协议。

`Tool Capability Router` 的模型可见工具集必须按以下流程生成：

```text
registry candidates
  -> tenant/user/session policy intersection
  -> domain + current-stage relevance
  -> provider/tool-protocol capability check
  -> context budget admission
  -> model-visible active tool set
```

要求：

- 默认只暴露最小核心工具和 `tool_search`，不得在普通 Chat 中无条件注入全部注册工具 schema。
- 高频通用工具和当前领域阶段确定会用的工具可以预激活，避免每个简单任务都多一次搜索轮次；是否 deferred 由线上召回、额外轮次、延迟和成本数据决定，不以“schema 越少越好”为目标。
- `tool_search` 只返回候选能力；命中工具经权限、场景、provider 和预算再次裁决后，才在后续模型请求中临时激活。
- “搜索到”不等于“被授权执行”。运行时在调用前仍必须校验 capability token 和最新策略。
- Context Manifest 记录工具 schema version、激活原因、来源、有效轮次和被拒原因；任务阶段、权限或 provider 变化时重新裁决。
- 工具描述本身有独立字节/Token 预算；检索排序和最终激活必须可离线评测，不能只靠模型自选。

`Artifact/Spill Plane` 统一承接 oversized tool result，并复用现有 `compact_tool_results_for_request` 作为 model-view reducer：

```text
tool output
  -> durable full artifact
  -> typed reducer
  -> model preview / client projection / telemetry projection
```

要求：

- 文本、日志、搜索结果、表格/SQL result、结构化 JSON 和二进制分别使用类型化 reducer，不再依赖通用字符截断。
- preview 携带 artifact hash、opaque locator、准确 omitted bytes/rows、分页信息和恢复工具；UTF-8 边界必须安全。
- 证据型结果必须先成功形成 durable artifact，才允许只向模型暴露截断视图；不可恢复截断不得成为正式证据。
- spill 失败不能把原本成功的工具调用伪装成工具失败；应保留原结果或明确进入资源不足状态，并阻止错误的 evidence admission。
- artifact 必须 tenant/session scoped，明确 owner-only 访问、加密、retention、删除、fork/child 继承和引用计数策略。
- full payload、模型视图、客户端视图和 telemetry 不是同一份内容的无条件复制，各自走敏感数据投影策略。

### 5.7 Resource Budget Ledger

现有月度额度、Token、上下文、工具族、sandbox 和 child 预算不删除，但统一接入可守恒的资源账本：

```text
dimensions = token_input | token_output | usd | wall_time | tool_calls |
             web_queries | datasource_scans | child_slots | artifact_bytes

available -> reserved -> committed
                      -> released
                      -> expired
```

核心不变量：

- 调用模型、执行高成本工具或 spawn child 前必须原子 reserve；完成后按实际使用 commit 差额，失败、取消和超时按 contract release 或结算。
- 父线程分配给 child 的额度必须从父可用额度中扣除；child 不得自行扩容，并发 child 不得超卖同一份余额。
- context compiler 为 final synthesis、domain verifier 和用户可见错误说明保留硬额度，避免前序研究耗尽全部预算后无法验证或收尾。
- retry、provider failover、compaction 和恢复执行都消费同一账本，不能通过切换模型、工具或执行器绕过限制。
- `budget_exhausted` 是带 dimension、reservation、stage 和降级建议的结构化终态/暂停态，不是注入 Prompt 的软提示。
- 每次回答展示可审计的预算估算、实际值和降级路径；质量评测必须同时比较准确率、成本和预算耗尽率。

### 5.8 Ledger Durability、Recovery 与敏感数据投影

`append-only` 只有在持久性和损坏语义明确时才有恢复价值。Execution/Evidence Ledger 必须定义：

- thread 内连续 sequence、batch ID、schema version、payload hash 和明确的 durability level；append API 返回前约定数据是否已 flush、file-sync 和 directory-sync。
- durability 按风险分级：approval、高风险 side-effect intent/outcome 和 checkpoint commit 必须经过硬持久化屏障；高频 model chunk 可 group commit，但 UI/恢复逻辑不得把未过屏障的数据标为 durable。
- 每个 thread 同时只有一个有效逻辑 writer：使用 lease + fencing token，或数据库事务中的 `expected_tail_sequence`/revision CAS；旧 worker、过期 lease 和并发 append 必须被拒绝，不能各自生成看似连续的分叉历史。
- 最终未完整写入的 torn record/frame 可以确定性丢弃；已提交区间的序列断裂、hash 错误或中段损坏必须 quarantine 并 fail closed，禁止“跳过坏行继续”。
- 重启时为未关闭 turn/tool 追加可审计 synthetic closer；已开始但远端结果不可确认的工具进入 `outcome_unknown`，与 `not_started` 严格区分。
- projection、checkpoint 和索引必须可从 ledger 全量重建；缓存或 projection 损坏不能改变事实。
- event upcaster 只做确定性、可测试的兼容转换；未知且非 `ignorable` 的必需事件必须拒绝读取。
- repair 不得静默改写历史；诊断、被丢弃的尾部范围、修复版本和操作者必须追加为审计事件。

同一份运行数据按消费方生成四类受控投影：

```text
raw_encrypted -> model_visible -> client_visible -> telemetry_redacted
```

这不是固定的逐级字符串脱敏链，而是对同一 source hash 独立裁决。默认不持久化明文 secret；只有业务审计确需 exact raw 时，才允许用 tenant key 加密、短 retention 和审计访问保存。所有 projection 必须记录 policy version、source hash 和 redaction provenance。删除和 retention 要覆盖 artifact、Memory、向量索引、provider trace、导出包和备份，不得只删主表。

现有 `ApprovalTokenLedger` 应生产化而不是另造一套审批：token 持久化进入 Execution Ledger，scope 至少绑定 tenant/user/session/tool/resource/action/expiry/max-uses/executor/child。父到子只允许权限交集，令牌不可跨 executor 或 child 转移。shell 命令字符串分类只能用于风险提示，不能证明“只读”；OS sandbox、egress policy 和 datasource ACL 才是强制边界。审批允许某次动作，不等于解除其他隔离。

### 5.9 Harness Conformance、Replay 与故障注入

回放分成三个不同层次，不能用“能重新渲染聊天记录”代替：

1. **State replay**：从 ledger 重建 projection、Context Manifest 和 semantic snapshot。
2. **Provider replay**：从显式导出的 fixture 重放每次 provider stream，不需要 API key，并校验 canonical request hash。
3. **Side-effect simulation**：工具使用固定 fixture/idempotency outcome，默认不得在测试回放中重放真实外部副作用。

测试工具包必须支持：

- 从生产 trace 显式脱敏导出 provider 脚本；生产 raw trace 默认不能直接成为可分享测试 fixture。
- parent/child 使用稳定 script key，不依赖并发下“谁先发起第一次调用”的顺序。
- 模拟 first-chunk 前抛错、partial stream、hang、timeout、disconnect、duplicate/late event、取消、retry 和 durable intent 后进程崩溃。
- 固定随机种子、逻辑时钟、provider/tool fixture 和外部数据版本。
- `assert_consumed` 检查所有预期模型调用、chunk 和工具结果都被消费，额外或缺失调用必须失败。
- raw request/response/tool payload 与最小事件 spine 分离；语义解释由离线 reducer 产生，允许 reducer 升级后重新解释同一原始 trace。

该工具包同时作为 Executor Adapter 和 Unified Agent Protocol 的 TCK：任何 Native/Codex/DSH adapter 都必须通过相同生命周期、权限、预算、取消、恢复和 projection 一致性测试。

---

## 6. Semantic State Kernel

### 6.1 通用语义记录

```rust
pub struct SemanticAssertion {
    pub id: AssertionId,
    pub tenant_id: TenantId,
    pub scope: AssertionScope,
    pub subject: EntityRef,
    pub predicate: String,
    pub value: TypedValue,
    pub qualifiers: BTreeMap<String, TypedValue>,
    pub valid_time: Option<TimeInterval>,
    pub observed_at: DateTime<Utc>,
    pub status: AssertionStatus,
    pub confidence: CalibratedScore,
    pub source_refs: Vec<EvidenceRef>,
    pub supersedes: Vec<AssertionId>,
    pub conflicts_with: Vec<AssertionId>,
    pub sensitivity: Sensitivity,
    pub retention: RetentionPolicy,
}

pub enum AssertionStatus {
    Proposed,
    Confirmed,
    Disputed,
    Superseded,
    Expired,
    Rejected,
}
```

设计要求：

- 相同 subject/predicate 可以同时存在多个时间版本。
- 新事实不能直接覆盖旧事实，必须形成 `supersedes` 或 `conflicts_with`。
- 用户明确确认高于模型推断；一方文档高于无来源总结，但不能越过时间有效性。
- “用户喜欢深色主题”和“本项目必须使用深色主题”是不同 predicate 和 scope。
- 任何注入模型的 assertion 都能回溯到消息、文件、工具结果、数据库查询或人工操作。

### 6.2 决策记录

```rust
pub struct DecisionRecord {
    pub id: DecisionId,
    pub scope: AssertionScope,
    pub question: String,
    pub decision: String,
    pub alternatives: Vec<DecisionAlternative>,
    pub rationale: Vec<EvidenceRef>,
    pub constraints: Vec<AssertionId>,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub owner: Option<EntityRef>,
    pub status: DecisionStatus,
    pub valid_time: Option<TimeInterval>,
    pub version: u64,
}
```

决策必须与偏好、事实、需求分离。用户改变决定时，旧决定标记为 superseded，不能从 Memory 中静默消失。

### 6.3 Evidence Ledger

```rust
pub struct EvidenceRef {
    pub evidence_id: EvidenceId,
    pub source_type: EvidenceSourceType,
    pub source_locator: String,
    pub content_hash: String,
    pub event_seq: Option<u64>,
    pub byte_or_line_range: Option<SourceRange>,
    pub collected_at: DateTime<Utc>,
    pub authority: EvidenceAuthority,
}
```

Evidence Ledger 使用 append-only 事件；状态表是其物化视图。吸收 DSH `sourceEventSeqs` 的思想：任何 replacement、summary、state delta 都必须列出覆盖的源事件，且不能引用未来事件或遗漏被替换节点。

### 6.4 状态 reducer

状态 reducer 必须是可重放、幂等、可审计的：

```rust
pub trait SemanticReducer {
    fn apply(
        &self,
        current: &SemanticSnapshot,
        delta: ProposedStateDelta,
        evidence: &EvidenceLedgerView,
    ) -> Result<ReductionOutcome, ReductionError>;
}
```

`ReductionOutcome` 至少包含 accepted、rejected、conflicts、superseded、needs_confirmation 和新 snapshot version。LLM 输出不能直接写入 confirmed 状态。

---

## 7. Memory 2.0

### 7.1 写入流水线

每次重要 turn、工具结果、文件更新、PM 决策或 NL2SQL 口径确认后执行：

1. **Candidate segmentation**：按语义事件而不是固定字符数切分。
2. **Structured extraction**：抽取事实、偏好、约束、决策、实体关系、开放问题和领域状态 delta。
3. **Normalization**：实体消歧、单位、时区、时间范围、枚举和指标名称规范化。
4. **Privacy filter**：secret/PII 分类、脱敏、禁止持久化策略。
5. **Dedup + entailment**：判断重复、蕴含、补充、冲突或替代。
6. **Temporal merge**：根据 `valid_time`、`observed_at` 和来源优先级决定当前版本。
7. **Persist**：原始证据 append-only，语义记录 versioned upsert。
8. **Async consolidation**：空闲时更新主题摘要、实体档案和长期工作状态。

生产路径不得再以 `extract_key_info` 启发式文本作为主要 Memory 写入。启发式只允许作为模型不可用时的低置信候选，且不能自动标记为 confirmed。

### 7.2 双通道抽取

抽取器输出两个独立通道：

- `continuity_state`：当前任务继续执行必需的信息，如目标、已完成步骤、失败原因、工具产物、待办和下一动作。
- `long_term_memory`：跨任务有价值的信息，如稳定偏好、业务定义、长期约束、组织实体、指标口径和已确认决策。

这样可以避免把一次性工具噪音写入长期 Memory，也避免把任务恢复信息误当作用户长期偏好。

### 7.3 Consolidation

采用类似 Codex 两阶段但更强的机制：

- Phase 1：对单个 rollout/turn 做结构化抽取、敏感信息过滤和证据绑定。
- Phase 2：持有 scope 级逻辑锁，读取自上次 cursor 后的 delta，执行全局合并、冲突发现、陈旧淘汰、主题摘要和实体档案更新。
- consolidation 结果必须是 diff，不允许整份 Memory 无条件重写。
- consolidation model 与主回答 model 可独立配置和评测。
- consolidation 失败不能阻塞用户回答；游标只有在事务提交后推进。

### 7.4 检索

检索分四层并行召回，再统一 rerank：

1. lexical/BM25：精确名称、ID、SQL、错误文本；
2. embedding：语义近似；
3. entity/graph：同一实体、指标、项目、决策或需求；
4. temporal/status：当前有效、最近确认、冲突版本和失效记录。

最终排序建议：

```text
score = semantic_relevance
      + lexical_match
      + entity_overlap
      + scope_match
      + evidence_authority
      + recency_when_relevant
      - contradiction_penalty
      - staleness_penalty
      - redundancy_penalty
```

必须返回“冲突包”，而不是只返回最高分事实。例如指标口径存在新旧两个版本时，上下文应同时包含当前版本、旧版本已失效说明和来源。

### 7.5 读取策略

学习 Codex 的渐进披露，但使用 AOS 的语义检索优势：

- 默认只注入小型 `memory_summary` 和与当前任务高度相关的 assertion。
- 详细历史、原始文件和 exact archive 通过 search/read 工具按需读取。
- 每条注入有硬 Token 上限、来源和截断标记。
- 外部共享上下文、第三方 Prompt、工具结果不得自动转成用户 Memory。

### 7.6 删除与修正

- 删除必须同时影响物化状态、向量索引、摘要和后续 consolidation 输入。
- 原始审计事件按企业 retention 处理；用户可删除内容与合规审计保留必须区分。
- 用户修正一个事实后，后续回答不能继续检索旧版本作为当前事实。

---

## 8. Compaction 2.0

### 8.1 压缩输出不是一段摘要

每次压缩必须生成 `CompactionCheckpoint`：

```rust
pub struct CompactionCheckpoint {
    pub checkpoint_id: String,
    pub source_event_seqs: Vec<u64>,
    pub narrative_summary: String,
    pub continuity_state_delta: ProposedStateDelta,
    pub unresolved_questions: Vec<OpenQuestion>,
    pub artifact_refs: Vec<EvidenceRef>,
    pub exact_archive_refs: Vec<ArchiveRef>,
    pub retained_recent_event_seqs: Vec<u64>,
    pub input_tokens_estimated: u64,
    pub output_tokens_estimated: u64,
    pub extractor_version: String,
    pub prompt_version: String,
}
```

### 8.2 必须吸收的 DSH 协议

- 不在 tool call 与 tool result 中间切割。
- max-token 或不完整模型输出默认 fail closed。
- checkpoint 加 framing 后若不比被替换内容小，不提交。
- 选区在摘要完成前后做 span stability 二次检查。
- replacement 原子提交，失败不改变 surface。
- `source_event_seqs` 必须覆盖所有被 shadow 的节点。
- 手动与自动压缩走同一提交协议。

### 8.3 必须保留的 Codex 能力

- 压缩后重新注入 initial/world state、安全策略和工具环境。
- 精确 rollout reconstruction，不从展示层摘要反推历史。
- stable prefix 优先，尽量复用 Prompt/KV cache。
- 每类注入内容都有硬预算，未知上下文窗口使用保守限制。

### 8.4 AOS 应新增的领域压缩

压缩策略由 scope 决定：

- 通用会话：事实、约束、决策、未决问题、附件指针。
- PM：Requirement State delta、假设、证据、冲突、决定、待验证问题。
- NL2SQL：已确认 metric、population、grain、time、filters、datasource、用户修正和执行结果引用。
- 归因：分析问题、对比基线、已执行观察、可用证据、被否定假设、下一轮诊断方向。

不能使用同一段通用摘要 Prompt 覆盖所有领域。

### 8.5 压缩触发

触发依据从“累计 Token 超阈值”升级为 projected context risk：

```text
projected = current_context
          + next_turn_reserve
          + expected_tool_results
          + verifier_reserve
```

当 projected 超过有效窗口的安全比例时提前压缩。保留：稳定 system/tool 前缀、当前用户请求、最近完整交互、未完成 tool transaction、pinned assertions 和当前领域状态。

---

## 9. Context Compiler 与 Prompt 架构

### 9.1 Context Packet

所有 Agent 最终接收同一通用 envelope：

```rust
pub struct ContextPacket {
    pub objective: String,
    pub domain: DomainKind,
    pub current_state: SemanticSnapshotRef,
    pub confirmed_constraints: Vec<AssertionRef>,
    pub unresolved_conflicts: Vec<ConflictBundle>,
    pub relevant_memories: Vec<AssertionRef>,
    pub evidence_index: Vec<EvidenceRef>,
    pub exact_artifacts: Vec<ArtifactExcerpt>,
    pub recent_messages: Vec<MessageRef>,
    pub output_contract: OutputContract,
    pub budget_manifest: ContextBudgetManifest,
}
```

Context Compiler 必须产出 manifest，记录每一块为什么被选中、使用多少 Token、是否截断和来自哪个版本。任何线上错误都能重放当时模型真正看到的 Context Packet。

### 9.2 Prompt 分层

Prompt 固定为四层，禁止继续拼成不可审计的巨型字符串：

1. **Stable system contract**：身份、安全、工具协议、不可违背规则。
2. **Domain contract**：PM/NL2SQL/General 的输出 schema、验证标准和拒答规则。
3. **Task packet**：当前目标、语义状态、冲突、证据和预算。
4. **Recent interaction**：最近消息和必要工具结果。

工具结果、检索文档和用户附件全部标记为 data，不得携带可提升权限的 instruction。Prompt injection 防护必须由结构与权限实现，不能只靠一句“忽略恶意指令”。

### 9.3 Prompt Registry

每个生产 Prompt 必须有：

- `prompt_id`、semantic version、owner；
- section source、priority、scope 和 trust level；
- 输入/输出 JSON Schema；
- 适用模型能力、model variant 和 tool schema version；
- 最大输入/输出预算；
- cache class、stable-prefix hash 和预期缓存边界；
- 对应 eval suite；
- rollout 比例和回滚版本；
- 线上 trace 中的版本记录。

Prompt 变更若未跑对应评测，不得直接成为默认版本。

模型无关只适用于稳定的安全、状态和领域契约，不能强迫所有模型使用最低公分母。允许在同一 `prompt_id` 下维护经过评测的 provider/model variant，包括工具描述、推理强度、结构化输出协议和错误恢复提示；variant 不得改变业务真相或绕过 verifier。必须监控静态前缀稳定性、cache read/creation tokens、首次有效工具调用率和格式修复率。

### 9.4 结构化输出

- 能用 schema 的任务必须用 schema。
- JSON 修复只允许修复语法形状，不能悄悄补造缺失业务字段。
- parser failure、schema failure、semantic failure 必须是不同错误类型。
- 输出缺失关键证据时应进入 `needs_confirmation` 或 `insufficient_evidence`，不能通过默认值伪装成功。

---

## 10. Requirement Discovery Engine

### 10.1 核心状态模型

```rust
pub struct RequirementState {
    pub id: RequirementId,
    pub version: u64,
    pub problem_frame: Option<ProblemFrame>,
    pub stakeholders: Vec<Stakeholder>,
    pub jobs: Vec<JobToBeDone>,
    pub pains: Vec<Pain>,
    pub desired_outcomes: Vec<Outcome>,
    pub constraints: Vec<RequirementConstraint>,
    pub assumptions: Vec<Assumption>,
    pub scope: ScopeDefinition,
    pub decisions: Vec<DecisionRef>,
    pub open_questions: Vec<OpenQuestion>,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub evidence_links: Vec<ClaimEvidenceLink>,
    pub experiments: Vec<ValidationExperiment>,
    pub readiness: RequirementReadiness,
}
```

状态必须跨轮持久化。每次用户回答、研究结果、反馈或产品决定只产生 delta，不重新从聊天记录生成整份 PRD。

### 10.2 需求发现状态机

```text
Frame problem
  -> identify stakeholders/jobs/pains
  -> expose assumptions and conflicts
  -> choose highest-value question
  -> collect user or external evidence
  -> update state
  -> validate outcomes and constraints
  -> converge decisions
  -> generate PRD/spec view
  -> ingest delivery/result feedback
```

研究不是第一步的默认答案。只有开放问题确实需要外部事实时才启动 research task。

### 10.3 下一问选择

问题优先级不能由固定模板决定，使用 Expected Value of Information：

```text
question_value = decision_impact
               * uncertainty_reduction
               * answerability
               / user_effort
```

高价值问题示例：不回答就会改变目标用户、核心指标、范围或方案选择。低价值问题示例：只改善文档措辞但不影响决策。

### 10.4 假设与证据

```rust
pub struct Assumption {
    pub statement: String,
    pub type_: AssumptionType,
    pub importance: f32,
    pub uncertainty: f32,
    pub status: AssumptionStatus,
    pub supporting_evidence: Vec<EvidenceRef>,
    pub counter_evidence: Vec<EvidenceRef>,
    pub falsification_test: Option<ValidationExperiment>,
}
```

Evidence Engine 必须独立验证：

- claim 与证据是否语义蕴含，而不是 claim 行是否带 URL；
- 数值、时间、样本范围和单位是否一致；
- 不同来源是否真正冲突；
- 来源是否一手、过期、转述或循环引用；
- 反证是否被搜索和记录。

LLM NLI 只能提供候选分数；关键 claim 应结合规则、数值解析和人工抽检。

### 10.5 PRD 生成门槛

只有以下条件满足才生成“可评审 PRD”：

- problem frame 已确认；
- primary stakeholder 和 job 已确认；
- desired outcome 有可测量信号；
- P0 constraints 和 scope 冲突已处理；
- 高影响假设有验证计划或明确风险接受；
- acceptance criteria 可测试；
- 未决问题不会改变核心方案。

否则输出应是 Requirement Brief，并明确“已知、假设、缺口、下一问”，不能用完整 PRD 格式掩盖需求尚未收敛。

### 10.6 真实学习闭环

上线后的结果必须回写 Requirement State：

- 哪个 outcome 达成；
- 哪个假设被验证或推翻；
- 用户采用/拒绝了哪些建议；
- 哪些 acceptance criteria 发生误判；
- 哪类澄清问题最能减少返工。

这些反馈用于 question policy、检索 rerank 和评测样本，不直接无审计地改写 Prompt。

---

## 11. Analytics Semantic Compiler（NL2SQL 2.0）

### 11.1 先生成语义 IR，再生成 SQL

```rust
pub struct AnalyticIntentIR {
    pub objective: AnalyticObjective,
    pub metrics: Vec<MetricRef>,
    pub dimensions: Vec<DimensionRef>,
    pub grain: Grain,
    pub population: PopulationDefinition,
    pub filters: Vec<SemanticFilter>,
    pub time: TimeSemantics,
    pub comparison: Option<ComparisonSpec>,
    pub denominator: Option<DenominatorSpec>,
    pub ordering: Vec<OrderSpec>,
    pub limit: Option<u64>,
    pub null_policy: NullPolicy,
    pub data_quality_policy: DataQualityPolicy,
    pub security_scope: SecurityScopeRef,
    pub unresolved: Vec<SemanticAmbiguity>,
}
```

`TimeSemantics` 至少包含 event time、processing time、timezone、business calendar、inclusive/exclusive boundary、as-of/version semantics。

`PopulationDefinition` 至少包含主体、cohort、去重键、内部/测试用户、退款/撤销状态和有效记录规则。

### 11.2 Metric Contract

```rust
pub struct MetricContract {
    pub id: MetricId,
    pub version: u64,
    pub names: Vec<String>,
    pub expression: MetricExpressionIR,
    pub denominator: Option<MetricExpressionIR>,
    pub population: PopulationDefinition,
    pub default_grain: Grain,
    pub allowed_grains: Vec<Grain>,
    pub time_column: ColumnRef,
    pub timezone: String,
    pub mandatory_filters: Vec<SemanticFilter>,
    pub join_contracts: Vec<JoinContractRef>,
    pub invariants: Vec<ResultInvariant>,
    pub valid_time: TimeInterval,
    pub owner: Option<EntityRef>,
    pub evidence_refs: Vec<EvidenceRef>,
}
```

指标是 versioned contract，不是 Prompt 里的一段自由文本。用户问“收入”时，系统必须解析到明确版本，或说明存在多个口径并澄清。

### 11.3 Join Contract

每条可用 Join 必须记录：

- left/right keys；
- cardinality：1:1、1:N、N:1、N:N；
- temporal/as-of 条件；
- nullable 行为；
- 去重策略；
- 允许的 metric/grain；
- fanout 风险和验证查询。

禁止仅因为两个列名相似就生成 Join。N:N 默认阻断，除非存在显式 bridge 和聚合策略。

### 11.4 编译流水线

1. NL -> `AnalyticIntentIR`；
2. entity/metric/schema binding；
3. ambiguity scoring；
4. 需要时生成一条最高价值澄清问题；
5. logical plan；
6. physical binding 和 join plan；
7. 优先用确定性 compiler 生成 SQL；
8. 无法覆盖的复杂表达允许模型生成 candidate SQL；
9. semantic verifier；
10. EXPLAIN/执行；
11. result invariant 和 counter-query 验证；
12. 生成带口径和证据的答案。

LLM 不再直接从自然语言跨越到最终 SQL；至少要留下可验证 IR。

### 11.5 Semantic Verifier

验证器必须输出分层结果：

```rust
pub struct SemanticVerification {
    pub safety: CheckResult,
    pub schema_binding: CheckResult,
    pub metric_equivalence: CheckResult,
    pub population_equivalence: CheckResult,
    pub grain_consistency: CheckResult,
    pub time_consistency: CheckResult,
    pub join_cardinality: CheckResult,
    pub filter_completeness: CheckResult,
    pub policy_compliance: CheckResult,
    pub result_invariants: Vec<CheckResult>,
    pub executable: Option<CheckResult>,
    pub confidence: CalibratedScore,
    pub release_decision: QueryReleaseDecision,
}
```

必须新增的确定性/半确定性检查：

- metric expression canonicalization 和等价比较；
- group-by 是否匹配要求 grain；
- denominator 与 population 是否匹配；
- 时间列、时区和边界是否匹配；
- mandatory filters 是否存在；
- Join 是否造成 fanout；
- `COUNT(*)`、`COUNT(id)`、`COUNT(DISTINCT id)` 是否符合主体；
- 比率能否由独立分子/分母查询复算；
- 总计与分组求和是否一致；
- 时间窗口拆分后是否可重组；
- 关键指标是否违反非负、上下界、单调性或守恒关系。

### 11.6 Confidence 必须校准

禁止继续使用固定 `0.8` 或缓存命中 `1.0`。置信度由可观察信号构成：

- metric/entity binding margin；
- ambiguity count；
- schema coverage；
- verified reference support；
- deterministic check pass rate；
- execution/result invariant；
- 历史同类 case 的真实正确率；
- model disagreement。

用 held-out 数据计算 Brier Score、ECE 和 selective accuracy。缓存只表示复用，不提高语义置信度。

### 11.7 Feedback 闭环

`thumbs_up/down/correction` 不能只入库统计：

- correction 解析为 IR diff、SQL diff 和原因标签；
- 人工确认后成为 datasource/domain scoped exemplar；
- 自动加入 regression case；
- 影响 reference rerank，但有最小样本和反作弊限制；
- 多次出现的 correction 才生成 metric/join contract 候选；
- 未经 owner 批准不得自动改写认证指标。

---

## 12. 数据归因的产品边界

### 12.1 四级证据等级

| 等级 | 允许的结论 | 方法 |
|---|---|---|
| L0 描述 | “发生了什么” | 趋势、分组、异常、数据质量 |
| L1 分解 | “哪些组成项贡献了变化” | 分子/分母、mix shift、贡献度分解 |
| L2 准实验 | “在假设成立时，某因素可能造成多大影响” | DiD、matching、synthetic control、interrupted time series 等 |
| L3 随机实验 | “实验估计的因果效应” | A/B、CUPED/ANCOVA、显著性与敏感性分析 |

当前 AOS 主要处于 L0-L1。UI、API 和报告字段必须标明 evidence level。没有 identification strategy 时禁止使用“导致”“因果效应”“证明原因”等强表述。

### 12.2 Causal Analysis Contract

进入 L2/L3 必须明确：treatment、outcome、unit、pre/post window、control、confounders、interference 假设、missingness、estimator、uncertainty interval 和 robustness checks。

统计方法应使用成熟库或独立分析服务，不在 Prompt 中手算。模型负责提出识别方案和解释结果，不负责凭文本生成显著性结论。

---

## 13. 体验设计要求

### 13.1 Memory 体验

- 用户能看到系统“当前相信什么”、来源和生效时间。
- 新旧事实冲突时提示确认，不静默选择。
- 用户修正后立即影响后续回答，并可查看旧版本为何失效。
- 默认不展示内部摘要工程细节，但所有引用可展开到原文。

### 13.2 PM 体验

- 主视图围绕 Requirement State，而不是长篇聊天和一次性报告。
- 始终区分：已确认、假设、待验证、已决策、已否决。
- 每轮只问最影响决策的问题，不进行问卷式盘问。
- 研究进度应对应“哪个开放问题正在被验证”，而不是泛化的 Agent 阶段。
- PRD 是状态的可发布视图，用户修改 PRD 必须回写状态 delta。

### 13.3 NL2SQL 体验

回答前或答案旁明确展示：指标版本、时间范围、粒度、人群和关键过滤。

系统只有三种诚实状态：

- `verified`：语义和结果验证通过；
- `provisional`：可执行，但存在明确未验证假设；
- `needs_clarification`：关键歧义会改变答案。

不要用一个模糊百分比替代这些状态。置信度用于排序和自动化门禁，用户侧优先展示具体原因。

### 13.4 Harness 与恢复体验

- 常见任务不应因为工具渐进披露被迫多走一轮搜索；只有长尾能力发现才承担额外 round trip，前端无需展示内部 schema 调度细节。
- 大工具结果默认展示可读 preview、总行数/省略量和“继续读取/下载”入口；用户不应只看到“输出过长已截断”且无法找回原文。
- 页面断线、服务重启或 provider failover 后，用户看到的是可解释的 resumed/interrupted/outcome unknown，而不是重复执行、进度倒退或假成功。
- 预算不足应在执行前或可恢复边界明确提示已完成内容、未完成内容和可选降级；不能耗尽后只返回通用错误。
- approval 明确展示动作、资源、影响范围和有效期；批准一次动作不产生“此后默认全放行”的惊讶权限扩张。
- 内部 trace、Memory 和 artifact 删除遵守同一数据策略；用户删除后不能在另一个索引或导出包中再次出现。

---

## 14. 数据存储与迁移

### 14.1 新增核心表建议

- `agent_threads`
- `agent_turns`
- `agent_event_ledger`
- `agent_projection_cursors`
- `ledger_repair_events`
- `tool_invocations`
- `approval_requests`
- `capability_tokens`
- `child_thread_edges`
- `execution_checkpoints`
- `artifact_objects` / `artifact_projections`
- `resource_budget_accounts` / `resource_budget_entries`
- `semantic_assertions`
- `semantic_assertion_edges`
- `semantic_snapshots`
- `evidence_ledger`
- `decision_records`
- `compaction_checkpoints`
- `context_packet_manifests`
- `requirement_states`
- `requirement_state_events`
- `metric_contracts`
- `join_contracts`
- `analytic_intent_ir`
- `semantic_verifications`
- `feedback_learning_events`
- `eval_runs` / `eval_case_results`

所有表必须 tenant scoped；所有 versioned 记录使用 append/update-current 双层设计，不能只保留最终值。

### 14.2 迁移策略

禁止 big-bang 切换：

1. 现有 `memory/items`、exact archive、PM session、NL2SQL query 和 feedback 表保持可读。
2. 新内核先 shadow write，不影响生产回答。
3. 对比旧/new Context Packet、Requirement State 和 Analytic IR。
4. 通过离线 gate 后，对 5% tenant 开启 shadow read，但仍使用旧结果回答。
5. 逐步启用新 Memory read、PM state 和 NL2SQL verifier。
6. 每个领域都有独立 feature flag 和快速回滚。
7. 稳定两个版本后再删除重复表和旧 Prompt 路径。

### 14.3 现有数据回填

- exact archive 只建立 EvidenceRef 和索引，不批量让模型把所有历史自动确认成事实。
- 已有显式 Memory 以 `Proposed` 或 `ConfirmedByUser` 迁入。
- PM 历史报告只提取候选 Requirement State，并要求用户/owner 在活跃项目中确认。
- NL2SQL metric 定义迁入 Metric Contract draft；经过 golden case 和 owner 审批后成为 certified。

---

## 15. 可观测性与可重放

每次回答必须能关联：

- thread/turn/step/item id 和 execution ledger cursor；
- tool/approval lifecycle、idempotency key 和最终 outcome；
- child-thread lineage、预算、settlement 和返回报告；
- executor adapter、native event reference 和 capability snapshot；
- provider request canonical hash、stream attempt 和 retry/failover lineage；
- semantic snapshot version；
- context packet manifest；
- model-visible tool set、工具激活原因和 schema hash；
- resource reservation/commit/release 和最终成本；
- prompt/model/tool version；
- Memory 检索候选和最终选择；
- compaction checkpoint；
- Requirement State / Analytic IR；
- verifier 各项结果；
- 最终 release decision；
- full artifact 与 model/client/telemetry projection 的 hash、policy version 和 redaction provenance；
- 用户反馈和后续 correction。

Trace 必须分成两层：最小、append-only 的事件 spine 记录顺序和引用；原始 provider/tool payload 使用受权限和 retention 控制的 artifact ref。`model-visible`、`runtime-visible` 和 `telemetry-visible` 必须可区分，不能因为调试方便把 raw payload 默认送入日志平台。

必须提供内部 replay 工具：给定 trace ID，在固定模型快照或 recorded provider stream 下重建输入、状态和验证结果，并校验 request hash、parent/child script key 和 fixture 完整消费。没有 replay，就无法判断质量下降来自模型、检索、压缩、Prompt、状态 reducer 还是数据变化；没有故障注入，就不能证明 suspend/resume、幂等、预算和 crash recovery 在真实异常下成立。

---

## 16. 评测体系

### 16.1 通用实验规则

- 同模型、同工具、同权限、同输入、同预算；
- 至少包含 Codex、DSH-compatible baseline、AOS old、AOS new；
- 领域 owner 盲评，隐藏系统名称；
- 记录所有失败，不允许静默排除；
- 报告 p50/p95 latency、Token、工具调用数和成本；
- 除总分外报告失败类型，避免平均分掩盖关键错误；
- 任何“超过 Codex/DSH”声明必须给置信区间和 case 清单。

### 16.2 Memory/Compaction 数据集

至少覆盖：

- 早期事实延迟追问；
- 未显式说“记住”的隐含约束；
- 用户后续修正和旧事实失效；
- 多时间版本；
- 矛盾来源；
- 多次连续压缩；
- 长 SQL、长表格、附件和精确原文回取；
- 跨会话、跨项目隔离；
- prompt injection 和 Memory poisoning；
- 不存在事实的诱导追问。

探针不得在问题中包含待召回事实全文。评分同时检查：是否检索正确证据、最终回答是否正确、是否引用过期事实、是否虚构记忆。

### 16.3 PM 数据集

每个 case 包含真实访谈/背景、隐藏关键信息、冲突 stakeholder、错误先验和最终产品结果。评分：

- 问题框架正确率；
- 关键 stakeholder/job/pain/outcome 漏失率；
- 下一问的信息增益；
- 假设识别和可证伪性；
- claim-evidence support precision；
- 冲突识别；
- 无依据结论率；
- PRD acceptance criteria 可测试性；
- 人工修改量、收敛轮次和返工率。

仅评最终报告文风没有意义。

### 16.4 NL2SQL 数据集

每个 case 必须提供 canonical `AnalyticIntentIR`、Metric Contract、允许的 SQL 变体和期望结果。覆盖：

- 同名多指标；
- 比率及分子分母；
- cohort/population；
- timezone/calendar；
- refund/test/internal user；
- SCD/as-of join；
- 1:N/N:N fanout；
- 多数据源；
- schema drift；
- 空数据和异常数据；
- 必须澄清与不应澄清；
- SQL 可执行但业务错误的对抗样本。

核心指标：

- IR exact/semantic match；
- business semantic accuracy；
- denotation accuracy；
- certified metric compliance；
- join fanout error rate；
- clarification precision/recall；
- unsafe query rate；
- execution rate；
- confidence ECE/Brier；
- selective accuracy at coverage levels。

执行率必须是次级指标。

### 16.5 归因数据集

- 合成数据中预设真实 driver 和 effect size；
- mix shift、Simpson's paradox、seasonality、missing data；
- 只有相关性、没有因果关系的负样本；
- A/B、DiD 和 interrupted time series；
- 结果采样导致的错误推断。

评分包含 causal overclaim rate。L0/L1 场景声称因果必须判为严重失败。

### 16.6 Harness 正确性与效率数据集

使用 recorded provider stream、固定工具 fixture 和 crash-point matrix，至少覆盖：

- ledger 每个 batch/record 边界前后的崩溃、torn tail、中段 checksum 错误和未知 schema event；
- tool durable intent 前后崩溃、远端成功但本地未记账、duplicate/late outcome 和 restart retry；
- parent/child 并发 reserve、取消传播、权限降级、越权尝试和 settlement 丢失；
- provider first-chunk error、partial stream、hang、disconnect、rate limit、retry 和 failover；
- 工具搜索命中但无授权、任务阶段切换、provider 不支持工具协议和工具 schema 版本变化；
- oversized 文本、UTF-8、表格/SQL result、日志、JSON 和二进制的 spill、分页、恢复与删除；
- raw/model/client/telemetry 四类投影的 secret/PII 泄漏检查。

核心指标包括 crash recovery correctness、duplicate side-effect rate、projection determinism、budget oversell rate、artifact recoverability、tool-schema tokens、first valid tool selection、replay determinism 和 sensitive-data leak rate。Harness 测试不替代 PM/NL2SQL 效果盲测，但它决定效果结果是否可信、可复现。

---

## 17. 发布门槛

以下是首个 production cutover 的最低门槛，不是营销目标：

### 17.1 Unified Agent Protocol

- thread 内事件 sequence、幂等和 schema migration contract 测试 100% 通过；
- 双 worker、lease 过期、进程暂停后恢复等竞态下，stale writer append/commit 必须 100% 被 fencing 拒绝；
- torn tail 可自动识别并只移除未提交尾部；已提交中段损坏、sequence gap、payload hash 错误和未知必需事件必须 fail closed；
- 所有 crash-point fixture 恢复后，开放 turn/tool 都有确定性 closer，`not_started` 与 `outcome_unknown` 不得混淆；
- ledger/projection 全量重建结果一致，repair 行为本身可审计；
- 任意已提交 turn 可从 ledger + checkpoint 重建同一 model-visible context manifest；
- 前端断线、服务重启、worker 重试不得重复高风险副作用；
- suspended approval/user-question 可跨进程恢复；
- tool outcome 为 unknown 时不得被展示为 completed；
- parent cancellation 在约定时间内传播到所有非 detached child；
- child settlement、artifact 和 evidence reference 不得丢失；
- legacy runtime projection 与新 ledger 的 terminal status 一致率 >= 99.9%。

### 17.2 Memory/Compaction

- hard no-leak recall >= 95%；
- false-memory rate <= 1%；
- temporal/supersession 正确率 >= 95%；
- evidence citation precision >= 98%；
- 多次压缩后的 task completion 不低于未压缩 baseline 2 个百分点以上；
- secret/tenant leakage = 0；
- checkpoint 原子性、边界和 source coverage 测试 100% 通过；
- 任何被截断的正式证据都有可读取 artifact locator，recoverability = 100%；artifact hash、omitted bytes/rows 和分页边界测试 100% 通过。

达到 99% 前不得使用“零丢失”。

### 17.3 PM

- critical requirement omission <= 5%；
- unsupported key claim <= 2%；
- claim-evidence support precision >= 95%；
- clarification/question usefulness 人工评分显著高于旧版；
- 相同预算下，盲评总体胜率相对旧版 >= 60%；
- 相对 Codex/DSH baseline 只有在统计显著时才允许公开“领先”。

### 17.4 NL2SQL

- certified metric compliance >= 98%；
- business semantic accuracy >= 90% 的首批认证领域，通用领域单独报告；
- join fanout 严重错误 <= 0.5%；
- unsafe SQL = 0；
- 必须澄清 case recall >= 90%，不应澄清 case precision >= 90%；
- high-confidence bucket accuracy >= 97%；
- correction 后同 case 回归通过率 >= 99%。

### 17.5 归因

- L0/L1 causal overclaim rate = 0；
- 关键原因必须有有效 evidence step；
- sampled/diagnostic 结果不得作为正式业务答案；
- L2/L3 必须输出 estimator、uncertainty 和 identification assumptions。

### 17.6 Harness、安全与预算

- 默认通用 Chat 的 model-visible 工具 schema Token 相对当前全量注入 baseline 至少下降 60%，同时目标任务工具召回率 >= 98%；
- 常见任务预激活集的 task completion 不得低于全量工具 baseline 1 个百分点以上；长尾 tool search 的额外模型轮次、p95 延迟和成本必须单独报告；
- 搜索到但未授权的工具执行成功数 = 0；child capability 必须是父 capability 与 child policy 的交集，权限扩大测试 100% 拒绝；
- approval token 单次使用、过期、resource/action/executor/child binding 和重启恢复测试 100% 通过；批准动作仍受 sandbox、egress 和 datasource ACL 约束；
- 父子并发预算 oversell = 0；所有 provider retry/failover、tool 和 artifact 开销都有 reservation settlement；final/verifier reserve 不得被前序阶段挪用；
- recorded provider replay 在固定 seed/time/fixture 下 projection 和 terminal state 一致率 = 100%，所有 fixture 必须 `assert_consumed`；
- duplicate high-risk side effect = 0；可模拟的 crash/timeout/late-result case 不得把 unknown 显示为成功；
- raw/model/client/telemetry projection 跨租户泄漏和明文 secret 落日志 = 0；删除测试覆盖 artifact、Memory、索引、trace 和导出包。

---

## 18. 失败指标与停止条件

以下现象出现时必须停止扩量并回滚：

- Memory recall 提升但 false-memory 同时明显增加；
- consolidation 将用户确认事实降级或跨 scope 污染；
- Context Packet 更长但正确率不升、p95 明显恶化；
- PM 报告引用更多但关键 claim support precision 下降；
- NL2SQL execution rate 上升但 semantic accuracy 下降；
- verifier 被频繁 bypass 或超时后默认放行；
- confidence 与实际正确率负相关；
- 数据归因使用“原因”措辞但没有对应证据等级；
- 人工 correction 未进入回归集；
- 新旧路径无法通过 trace 重放解释差异；
- 工具渐进披露降低了 schema Token，但目标工具召回率或首个有效调用率明显下降；
- spill preview 无法恢复完整证据，或 artifact 删除后索引/Memory 仍保留不可解释结论；
- child/provider 并发时出现预算超卖、权限扩大或父取消后未授权继续运行；
- ledger repair 跳过已提交损坏、把 `outcome_unknown` 当成功，或 replay fixture 未完整消费仍判通过；
- raw provider/tool payload、secret 或跨租户 locator 进入无权客户端或 telemetry。

---

## 19. 实施阶段

### Phase 0：冻结事实与建立评测基线

交付：

- 固定 AOS/Codex/DSH 审计版本和统一运行配置；
- 建立 Memory、PM、NL2SQL、Attribution 四套不可变 case ID；
- 跑 AOS current baseline，记录质量、延迟和成本；
- 修正现有 Zero Loss 泄题 probe；
- Golden Case 新增 canonical IR 和 expected result，不再只检查关键词；
- 建立 Harness recorded-provider replay、side-effect fixture 和 crash-point/fault matrix；
- 记录当前普通 Chat 工具 schema Token、目标工具召回、artifact recoverability、预算超卖和恢复正确性 baseline。

退出条件：任何后续重构都能用同一评测比较。

### Phase 1：Unified Agent Protocol、Semantic Core 与双账本

交付：

- `agent-protocol` 的 Thread/Turn/Item/Event v1 和 schema evolution 规则；
- append-only Execution Ledger、projection cursor 和 Context Manifest 关联；
- writer fencing、ledger batch durability、payload hash、torn-tail repair、中段损坏 quarantine、event upcaster 和 synthetic closer；
- `semantic-core` 类型、reducer、version、conflict、EvidenceRef；
- Evidence Ledger 和 snapshot 表；
- raw event spine、payload artifact ref、敏感数据多投影和 trace/context manifest；
- exact archive 适配器；
- shadow write。

退出条件：可从运行事件重建相同 projection/context manifest，并从证据事件重放相同 semantic snapshot。

### Phase 2：工具、Child Thread 与 Memory/Compaction 2.0

交付：

- durable tool/approval/user-question 状态机；
- side-effect、idempotency、retry、deadline 和 cancellation contract；
- 现有 ApprovalTokenLedger 持久化并统一接入 Tool/Child/Executor；
- Tool Capability Router 和按需临时激活；
- 通用 Artifact/Spill Plane 及类型化 reducer；
- Resource Budget Ledger、原子 reservation 和父子守恒；
- 统一 Child Thread lineage、能力交集、预算、控制和 settlement；
- 双通道结构化抽取；
- temporal/conflict-aware merge；
- Phase 2 consolidation；
- DSH 风格原子 checkpoint；
- Context Compiler 和渐进披露；
- shadow read 对比。

退出条件：通过 §17.1、§17.2 和 §17.6，且成本/延迟在预算内。

### Phase 3：Requirement Discovery Engine

交付：

- Requirement State/Event；
- 信息增益问询；
- claim-evidence verifier；
- Requirement Brief/PRD readiness gate；
- 历史 PM 会话适配和 UI state view。

退出条件：PM 盲评达到 §17.3，研究编排作为状态验证工具而非主状态。

### Phase 4：Analytics Semantic Compiler

交付：

- Analytic Intent IR；
- Metric/Join Contract；
- binder、compiler、semantic verifier；
- calibrated confidence；
- feedback -> approved exemplar -> regression 流水线；
- 现有 SQL generation 作为 fallback backend。

退出条件：首批认证领域达到 §17.4。

### Phase 5：归因等级与反馈学习

交付：

- evidence level；
- L0/L1 语言约束；
- L2/L3 analysis contract；
- 结果/用户反馈进入状态与评测；
- 对外可复现 benchmark report。

退出条件：达到 §17.5，且诊断性贡献结论不会被展示成因果结论。

### Phase 6：Executor Adapter 与模型专属优化

交付：

- Native AOS executor capability contract；
- Codex/DSH adapter 原型和事件映射一致性测试；
- capability negotiation 和不支持能力的显式降级；
- 主力模型的 Prompt/Tool Schema variant；
- stable-prefix/cache、首轮工具选择和恢复成功率评测。

退出条件：接入外部执行器不会绕过 AOS 权限、语义状态、审计和 verifier，且至少一个真实工作流证明适配收益高于维护成本。

---

## 20. P0 开发任务清单

| ID | 任务 | Owner 建议 | 验收标准 |
|---|---|---|---|
| PROTO-001 | 定义 Unified Agent Protocol v1 | Runtime/Gateway | Thread/Turn/Item/Event schema、状态机和迁移规则完整 |
| PROTO-002 | Execution Ledger 与 projection | Runtime/DB | 断线/重启可重放，前端不保存第二事实源 |
| PROTO-003 | Ledger durability 与 recovery | Runtime/DB | writer fencing、batch durability、torn tail、corruption quarantine、upcaster、synthetic closer 通过 crash matrix |
| TOOL-001 | durable tool/approval lifecycle | Tools/Gateway | suspend/resume/cancel/idempotency/outcome_unknown 通过故障测试 |
| TOOL-002 | Tool Capability Router | Tools/Runtime | 默认最小工具集，search 后策略裁决/临时激活/失效，schema Token 与召回达到 §17.6 |
| ART-001 | Artifact/Spill Plane | Tools/Storage | typed preview、hash/locator/paging、完整恢复、tenant retention/delete 通过 |
| BUDGET-001 | Resource Budget Ledger | Runtime/Billing | reserve/commit/release、父子守恒、并发无超卖、verifier reserve |
| CHILD-001 | 统一 Child Thread | Runtime/Gateway | lineage、预算、steer/interrupt、top-down cancel、settlement |
| SEC-001 | Durable capability/approval token | Security/Runtime | 所有 Tool/Child/Executor 强制接线，权限只降不升，sandbox/approval 分层 |
| SEC-002 | Sensitive Data Projection Contract | Security/Storage | raw/model/client/telemetry 独立裁决，provenance 与全链路删除 |
| PROMPT-001 | Prompt Manifest 与 model variant | Runtime/Eval | trust/cache/tool-schema/hash/评测 lineage 可追踪 |
| CORE-001 | 定义 Assertion/Decision/Evidence/Snapshot | Runtime | 无 LLM 依赖；serde/schema/test 完整 |
| CORE-002 | 实现幂等 reducer 与冲突/supersession | Runtime | 事件乱序、重复、版本冲突属性测试通过 |
| CORE-003 | Evidence Ledger 与 source coverage | Runtime/DB | replacement 不得遗漏源事件 |
| MEM-001 | 双通道 extractor | Memory | 结构化 schema、secret filter、离线 eval |
| MEM-002 | temporal consolidation | Memory | 新旧事实、冲突和失效 case 通过 |
| CMP-001 | 原子 CompactionCheckpoint | Runtime | 边界、smaller-than-source、fail-closed |
| CTX-001 | Context Compiler + manifest | Runtime | 每个注入块可解释、可重放、有预算 |
| PM-001 | RequirementState/Event | PM | 跨轮 delta，不从摘要重建当前状态 |
| PM-002 | next-question policy | PM/Eval | 信息增益盲评优于固定模板 |
| PM-003 | claim-evidence verifier | PM | URL 存在不等于支持，数值冲突可检出 |
| SQL-001 | AnalyticIntentIR | NL2SQL | canonical schema 覆盖核心语义字段 |
| SQL-002 | Metric/Join Contract | NL2SQL/Data | owner/version/valid-time/lineage 完整 |
| SQL-003 | semantic verifier | NL2SQL | metric/grain/population/time/join 检查 |
| SQL-004 | calibrated confidence | NL2SQL/Eval | 移除固定 0.8/1.0，报告 ECE/Brier |
| EVAL-001 | no-leak Memory benchmark | Eval | 问题不包含答案，评最终回答和证据 |
| EVAL-002 | PM/NL2SQL blind benchmark | Eval/Domain | 同模型同工具同预算，可复现 |
| EVAL-003 | Provider replay + fault-injection TCK | Eval/Runtime | request hash、稳定 parent/child script、fault matrix、`assert_consumed` |

`Executor Adapter` 不进入 P0 生产范围。Phase 1 只冻结 adapter SPI 和 capability schema；Native/Codex/DSH 的真实接入按 Phase 6 收益评测决定，避免统一协议尚未稳定就同时维护多个后端。

---

## 21. 应删除、合并或降级的现有模式

- 将启发式 `extract_key_info` 降级为 fallback，不作为主要 Memory 核心。
- 合并 `/chat/memories`、`/memory/items`、PM memory 的重叠事实源；保留领域视图，不保留三套真相。
- session summary 不再承担 PM Requirement State。
- 固定 `hypothesisEvidenceGraph` 模板改为真实 Assumption State 的视图。
- Conflict Matrix 文本解析改为独立 evidence conflict 检测。
- Golden Case 的关键词命中只保留为诊断信号，不作为 semantic pass。
- feedback stats 保留，但 correction 必须进入审核和回归闭环。
- SQL semantic LLM reviewer 保留为 ensemble signal，不作为最终业务正确性证明。
- EXPLAIN/执行成功从“质量通过”降级为“物理可执行通过”。
- Attribution 的 `mainCauses` 在 L0/L1 改名或明确为“主要贡献方向/候选解释”。

---

## 22. 竞争策略

AOS 不应以“我也能调用工具、也能写 SQL、也能研究”与 Codex/DSH 比较，因为这些能力会迅速商品化。应建立以下难复制资产：

1. **长期业务语义图谱**：不仅记住文本，还理解事实的时间、冲突、scope 和决策影响。
2. **需求状态数据**：真实项目中问题如何收敛、什么问题最有价值、哪些假设最终被推翻。
3. **认证指标与 Join Contract**：企业私有业务语义成为可验证资产，而不是散落在 Prompt 和 SQL 文件中。
4. **纠错数据飞轮**：每次人工修正变成 IR diff、规则候选和回归 case。
5. **领域 verifier**：即使底层模型相同，AOS 能发现通用 Agent 看不出的需求缺口和业务口径错误。
6. **诚实的 evidence level**：能区分事实、假设、贡献分解和因果，而不是生成更肯定的文字。

当这些资产成立后，Codex/DSH 可以成为 AOS 的底层执行器或工具，而不是 AOS 必须在所有方向击败的对手。

---

## 23. 最终验收定义

本次重构完成，不以“新架构上线”或“所有模块迁移”为准，而以以下结果为准：

- 压缩后回答保真有无泄题数据证明；
- Memory 能正确处理时间、冲突、修正和删除；
- PM 在用户给出模糊需求后，能用更少、更高价值的问题形成可验证 Requirement State；
- PRD 中关键结论能回溯到用户确认、事实或外部证据；
- NL2SQL 先产生可审计 IR，SQL 的指标、粒度、人群、时间和 Join 可被验证；
- 执行成功但业务错误的 SQL 能被稳定拦截；
- 用户 correction 能在批准后防止同类错误复发；
- 数据归因不会把相关性包装成因果；
- ledger 损坏、进程崩溃、断流和 late result 下能确定性恢复或 fail closed，不制造完成状态；
- 工具按需披露在显著降低上下文成本的同时不牺牲目标能力召回；大结果可从 artifact 精确恢复；
- parent/child/provider 并发下预算不超卖、权限不扩大，审批不绕过 sandbox/egress/datasource ACL；
- 同一真实故障可由脱敏 provider fixture 和 side-effect simulation 离线复现，敏感 raw 不泄漏到模型、客户端或 telemetry；
- 在同模型、同工具、同预算盲测中，AOS 在目标领域稳定胜过旧版，并对 Codex/DSH baseline 形成可复现优势。

在这些结果出现前，AOS 可以说“具备更完整的业务工作流和企业治理能力”，但不能说 Memory、需求挖掘或 NL2SQL 已经全面赶超 Codex/DeepSeek Harness。

## 23.1 本次实现自检（2026-08-15）

本节是本次代码交付前的逐项核对，不以“表已创建”或“类型已定义”作为完成依据：只有存在生产调用路径和回归测试才标记为“已接通”。第 17 节中的准确率、召回率、ECE、Brier、延迟和竞品胜率属于需要固定数据集/模型/预算后才能证明的实证门槛，不能由离线单元测试虚报为已达标。

| 规格能力 | 当前实现状态 | 代码/测试证据 |
| --- | --- | --- |
| Unified Agent Protocol、事件 hash、sequence、幂等、writer fencing | 已接通 | `agent-protocol` 类型与回放测试；SQLite `agent_event_ledger`、`agent_writer_leases`、中段损坏 fail-closed 测试 |
| Turn/Tool durable intent、outcome_unknown、取消/异常终结 | 已接通 | `runtime::AgentExecutionKernel` 已挂入 `ConversationRuntime`；SQLite runtime-kernel 测试验证 intent 先于 side effect、重启 synthetic closer |
| Context Manifest 与敏感投影 | 已接通 | 每次模型迭代在 provider 前由 `semantic-core::ContextCompiler` 生成四层 `ContextPacket`，记录 block/snapshot/hash/预算/截断；结构化保护、hash、PM prompt manifest 测试 |
| Tool Capability Router 渐进披露 | 已接通 | 普通 Chat 默认核心工具 + `ToolSearch`；命中候选按 registry/permission 再激活；runtime 工具路由测试 |
| Artifact/Spill 与精确恢复 | 已接通 | oversized tool output 先持久化完整受保护 payload，再按 text/log/search/table/JSON/binary 类型 reducer 生成 model/client/telemetry 投影；只有显式 `source` projection 可 owner 分页恢复，测试覆盖 UTF-8、行/字节计数、租户隔离和泄漏 |
| 多维资源预算 | 已接通 | `agent-protocol::BudgetLedger`；SQLite 工具调用、web query、datasource scan 和 artifact bytes 记账，父子守恒由纯内核测试覆盖 |
| Child Thread lineage / settlement / top-down cancel | 已接通（能力协商） | Super Assistant specialist subtask 在执行前写 `child_thread_edges` 和统一 Agent Ledger；完成、失败、取消只结算一次；父任务取消向下传播，tenant lineage 回归测试覆盖。native AOS executor 实际支持 `cancel`，并在重启后恢复 pending cancel；`follow_up`/`steer`/`interrupt`/`resume` 会持久化后按 executor capability 明确拒绝，不伪装成已执行 |
| Durable capability 与权限只降不升 | 已接通 | Tool intent 在 side effect 前持久化一次性 capability token 并消费，resource scope 仅存 hash；child 使用既有 permission snapshot，`CapabilityScope::intersection` 阻止扩权；权限模式比较使用显式语义，回归测试覆盖 `Prompt` 不能因 enum 顺序而自动满足危险权限 |
| 通用 Web Approval suspend/resolve/resume | 已接通（Server/Gateway/WebUI） | `DurableDeferPrompter` 在生产 Gateway 路径中写入 durable request 后挂起；SSE 发送脱敏 `approval_paused`，恢复要求租户/用户/session/turn/invocation owner scope、一次性决策、过期检查和当前策略重检；SQLite owner/expiry/单次 dispatch 与重启回归测试通过；WebUI 刷新后从持久化审批列表恢复卡片，并用 reload-safe handlers 继续批准或拒绝，`approvalResume.test.ts` 覆盖实时与刷新两条路径 |
| Prompt Manifest 与 model/tool lineage | 已接通 | Runtime 每次 provider 调用前持久化 model、active tool set、system prompt hash、message hash 和预算；PM 额外写版本化 `prompt_manifests` |
| Semantic Assertion/Decision/Evidence/Reducer | 已接通 | `semantic-core` 可重放 reducer；证据缺失、冲突、supersession、context budget 测试 |
| Memory 双通道与压缩边界 | 已接通 | compaction 生产 hook 使用租户 chat model 做语义抽取，确定性通道仅作 fallback；`memory-engine` 统一 secret admission、continuity/long-term channel 与 source evidence provenance；先写 key info/checkpoint 后替换，exact archive 保留；完整 continuation framing 后的替换内容不小于 source window 时 fail closed，测试覆盖增长门禁；scope cursor 与 `supersedes/conflicts_with` diff 事务已接入 |
| PM Requirement State / next question | 已接通 | `pm-domain::requirement_state` 增量 delta、确认门禁、信息价值排序；SQLite requirement state/event 持久化和幂等测试 |
| PM claim-evidence semantic admission | 已接通 | claim 对齐不再只接受 URL：证据 excerpt 必须通过数字、日期/时间单位、金额/百分比/人群单位和方向一致性检查；不一致进入 evidence gap，回归测试覆盖冲突数值/单位 |
| NL2SQL NL -> IR -> binding -> verifier -> SQL audit | 已接通 | 查询前持久化 `AnalyticIntentIR`；metric contract alias/version binding、join contract loading、语义 verifier release decision 和 audit persistence |
| NL2SQL feedback learning / calibrated confidence | 已接通 | safe correction 写入 `feedback_learning_events` 和稳定 regression case；仅 approved 且同 tenant/datasource 的 correction 进入检索；feedback 回填 confidence observation，API 报告 ECE/Brier；执行成功后回写 `executionPassed`、行列数和耗时，且按 tenant 隔离回归覆盖 |
| Attribution evidence level / causal guard | 已接通 | 报告输出 `L0_descriptive`/`L1_decomposition`、主因强制 evidence step、服务端加入不可证明因果 caveat，WebUI 展示证据等级 |
| Provider replay / fault TCK | 已接通（离线） | `eval-harness::replay` canonical request hash、stable script key、`assert_consumed` 和故障帧测试 |
| No-leak Memory probe | 已接通（机制） | Zero Loss follow-up 不再包含待召回事全文，expected fact 只用于评分；最终效果数值仍必须由固定数据集实测 |
| 跨竞品质量、准确率、召回率和效果领先 | 待实证 | 必须按第 17 节固定 case、同模型/工具/预算盲评后才可宣布，代码不伪造该结论；本次代码已提供可复现 fixture/指标记录入口 |

本次交付不再把 legacy JSONL、PM stage 表或 NL2SQL 旧 SQL 行当作第二个事实源：它们保留为兼容 projection；新的运行事件、语义 IR、证据等级和最终交付 artifact 是可恢复路径的事实来源。任何 provider/数据库故障都进入结构化降级或 `outcome_unknown`，不得伪造 completed。

---

## 24. 主要源码证据索引

### AOS

- `rust/crates/runtime/src/compact.rs`
- `rust/crates/runtime/src/conversation.rs`
- `rust/crates/runtime/src/session.rs`
- `rust/crates/runtime/src/data_protection.rs`
- `rust/crates/runtime/src/report_schema.rs`
- `rust/crates/runtime/src/approval_tokens.rs`
- `rust/crates/runtime/src/permission_enforcer.rs`
- `rust/crates/runtime/src/tenant_executor.rs`
- `rust/crates/runtime/src/prompt.rs`
- `rust/crates/runtime/src/task_registry.rs`
- `rust/crates/tools/src/lib.rs`
- `rust/crates/eval-harness/src/parity.rs`
- `rust/crates/runtime/tests/parity_e2e.rs`
- `rust/crates/agent-gateway/src/events.rs`
- `rust/crates/web-server/src/routes/memory_continuity.rs`
- `rust/crates/agent-gateway/src/runtime_builder.rs`
- `rust/crates/web-server/src/routes/super_assistant_parent.rs`
- `rust/crates/pm-domain/src/prompts.rs`
- `rust/crates/pm-domain/src/task_graph.rs`
- `rust/crates/pm-domain/src/deep_research_loop.rs`
- `rust/crates/pm-report/src/lib.rs`
- `rust/crates/web-server/src/routes/agent/agent_pm_orch_quality.rs`
- `rust/crates/web-server/src/routes/agent/agent_pm_alignment.rs`
- `rust/crates/web-server/src/routes/agent/agent_pm_memory.rs`
- `rust/crates/nl2sql-core/src/requirements.rs`
- `rust/crates/nl2sql-core/src/query_understanding.rs`
- `rust/crates/nl2sql-core/src/result_validator.rs`
- `rust/crates/web-server/src/routes/nl2sql/mod.rs`
- `rust/crates/web-server/src/routes/nl2sql/prompts.rs`
- `rust/crates/web-server/src/routes/nl2sql/queries.rs`
- `rust/crates/web-server/src/routes/nl2sql/agent_executor.rs`
- `rust/crates/web-server/src/routes/nl2sql/golden_cases.rs`
- `rust/crates/web-server/src/routes/nl2sql/feedback.rs`
- `rust/crates/web-server/src/routes/nl2sql/attribution.rs`

### OpenAI Codex

- 官方行为边界：[Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md)（用于校准 sandbox 与 approval 的分层；源码判断仍以本节锁定 commit 为准）
- `codex-rs/core/src/compact.rs`
- `codex-rs/core/src/session/rollout_reconstruction.rs`
- `codex-rs/core/src/thread_manager.rs`
- `codex-rs/core/src/tools/handlers/tool_search_spec.rs`
- `codex-rs/core/src/tools/spec_plan.rs`
- `codex-rs/tools/src/tool_search.rs`
- `codex-rs/core/src/session/token_budget.rs`
- `codex-rs/core/src/rollout_budget.rs`
- `codex-rs/rollout-trace/README.md`
- `codex-rs/rollout-trace/src/writer.rs`
- `codex-rs/rollout-trace/src/payload.rs`
- `codex-rs/state/migrations/0021_thread_spawn_edges.sql`
- `codex-rs/app-server/tests/suite/v2/turn_interrupt.rs`
- `codex-rs/app-server/tests/suite/v2/turn_steer.rs`
- `codex-rs/app-server/tests/suite/v2/thread_fork.rs`
- `codex-rs/memories/write/src/phase1.rs`
- `codex-rs/memories/write/src/phase2.rs`
- `codex-rs/memories/write/templates/memories/*.md`
- `codex-rs/ext/memories/src/prompts.rs`
- `codex-rs/ext/memories/src/local/search.rs`

### DeepSeek Harness

- `packages/compaction/compaction-basic/src/summarizer.ts`
- `packages/compaction/compaction-basic/src/region.ts`
- `packages/compaction/compaction-tool-result-pruner/src/index.ts`
- `packages/core/session/src/surface.ts`
- `packages/core/session/src/types.ts`
- `packages/session/session-persistence/src/coordinator.ts`
- `packages/session/session-persistence/README.md`
- `packages/session/session-persistence-jsonl/src/format.ts`
- `packages/core/session/src/repair.ts`
- `packages/util/output-retention/src/index.ts`
- `packages/spill/spill/src/index.ts`
- `packages/spill/spill-local/src/index.ts`
- `packages/spill/spill-policy/src/index.ts`
- `packages/test-support/llm-replay/src/index.ts`
- `packages/test-support/agent-loop-testkit/src/index.ts`
- `packages/subagent/subagent/src/lifecycle.ts`
- `packages/subagent/subagent/src/continuation.ts`
- `packages/subagent/subagent/src/run-settlement.ts`
- `packages/subagent/tool-subagent-control/src/index.ts`
- `packages/subagent/subagent-in-process-driver/tests/inheritance.spec.ts`
