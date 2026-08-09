# AOS Code Studio

AOS Code Studio 是 AOS 的代码开发工作台。它同时提供类似 Cursor/Codex 的聊天式开发体验，以及类似 Kiro 的结构化研发流程，并且所有代码修改都必须经过 Diff-first 审批。

## 两种模式

### Vibe Mode

Vibe Mode 是 `/agent` 的默认入口。

它面向高频研发工作：

- 询问代码库问题。
- 解释错误和日志。
- 用自然语言派发代码修改任务。
- 在 Agent timeline 中看到读取文件、搜索上下文、运行命令、生成 Diff 的全过程。
- 先审查 Diff，再应用到主仓库。
- 在同一个工作台运行测试并查看终端输出。
- 在同一 thread 中追问，复用上一轮 Diff、测试和回答摘要。

### Plan Mode

Plan Mode 面向复杂需求、多人协作和企业审计，流程是：

```text
Requirement -> Spec -> Design -> Tasks -> Implementation -> Verify -> Final Report
```

Spec、Design、Tasks 都是真实持久化的后端产物，存储在 `rd_specs` 中。用户必须确认每个关键阶段，才能进入下一阶段。

Implementation 会为已确认的 task item 创建真实 `rd_tasks`。每个任务仍然走同一套 RD runtime、candidate workspace、AgentOps task、trace event 和 Diff-first 审批链路。

## Diff-first 安全策略

Agent 可以在 candidate workspace 内写文件，但不能静默修改主仓库。

标准链路是：

```text
Agent 在候选工作区执行
-> 收集 unified diff
-> 校验路径和 ownership
-> 写入 rd_file_changes
-> 用户审查 Diff
-> 用户应用全部/单文件/选中 hunks
```

这样 Code Studio 既适合企业审计，也适合开源演示。

## Runtime 与 WatchDog（看门狗）集成

每个真实 RD task 都会进入 AgentOps，并能被 WatchDog 观测。Workbench API 会返回：

- RD task 详情
- AgentOps task
- runtime session
- runtime processes
- runtime artifacts
- trace events
- RD task events
- file changes
- test runs
- 关联的 Plan Mode spec/task item
- suggested next actions

因此 WebUI、Bot 创建的 RD 任务、WatchDog 查询看到的是同一套执行证据。

## Hybrid Code Intelligence

Code Studio 使用混合代码智能：

- 优先使用按仓库维度复用的持久 LSP session，提供定义跳转、引用、hover、文档符号、工作区符号和诊断。
- Language server 未安装或未就绪时，回退到仓库 symbol index。
- LSP 和 symbol index 都未命中时，使用 `rg` 做 symbol token 搜索。

P0 默认语言服务器命令：

```text
TypeScript/JavaScript: typescript-language-server --stdio
Rust: rust-analyzer
Python: pyright-langserver --stdio
Go: gopls
Java: jdtls
C/C++: clangd
```

`POST /code-intel/restart` 会真实停止该仓库的内存 language-server 进程，并重置数据库状态，下一次查询会启动全新的 session。如果本机没有对应 language server，编辑器不会报 500 或阻断使用，而是显示 degraded 状态并继续提供 symbol/rg fallback。所有文件路径都会在 repository root 下做 safe-join 校验，禁止路径逃逸。

## Preview Debug

Preview Debug 通过 Agent Runtime 在隔离的预览工作区启动前端 dev server，不会静默修改主仓库。

预览链路是：

```text
启动预览
-> 创建 runtime session
-> 准备候选预览工作区
-> 执行 dev command
-> 打开后端代理 iframe
-> 自动采集 console/network 错误
-> 写入事件和截图 artifact
-> 可把证据交给 RD Agent 修复
```

iframe 通过受控 AOS proxy 访问 `127.0.0.1:{port}`。HTML、JavaScript、CSS 响应会重写绝对资源路径，保证资源继续走 proxy。HTML 页面会注入一段轻量采集脚本，把 console error、unhandled rejection、失败的 `fetch` 和失败的 XHR 通过 `postMessage` 发送给父页面；父页面再用正常鉴权 API 写入 preview event。

截图是 artifact-backed。服务端会通过当前 runtime session 执行 `npx --yes playwright screenshot` 并把 PNG 写入 runtime artifact。如果 Node/Playwright 不可用，系统会写入结构化 `screenshot.failed` 事件，而不是假装截图成功。

WatchDog 可以回答 Code Studio 相关问题，例如：

- 现在哪个 dev server 在跑？
- 预览页面有什么 console error？
- LSP 为什么不能跳转？
- 这个 RD task 卡在 runtime、测试、Diff 审批还是预览？

当新的 RD task 提到预览、浏览器、console、network 或 dev server 问题时，后端会自动把同仓库/同任务最近的 Preview Debug 证据注入到 Agent prompt。Agent 仍必须读取真实文件后再修改代码。

## 后端接口

Plan Mode APIs:

```http
GET  /api/v1/rd/specs
POST /api/v1/rd/specs
GET  /api/v1/rd/specs/:id
PATCH /api/v1/rd/specs/:id
GET  /api/v1/rd/specs/:id/events
POST /api/v1/rd/specs/:id/generate-spec
POST /api/v1/rd/specs/:id/approve-spec
POST /api/v1/rd/specs/:id/generate-design
POST /api/v1/rd/specs/:id/approve-design
POST /api/v1/rd/specs/:id/generate-tasks
POST /api/v1/rd/specs/:id/approve-tasks
POST /api/v1/rd/specs/:id/implement-task
POST /api/v1/rd/specs/:id/implement-all
POST /api/v1/rd/specs/:id/final-report
```

Workbench API:

```http
GET /api/v1/rd/tasks/:id/workbench
```

Code Intelligence APIs:

```http
GET  /api/v1/rd/repositories/:id/code-intel/status
POST /api/v1/rd/repositories/:id/code-intel/query
POST /api/v1/rd/repositories/:id/code-intel/restart
```

Preview Debug APIs:

```http
POST /api/v1/rd/repositories/:id/preview-sessions
GET  /api/v1/rd/preview-sessions/:session_id
GET  /api/v1/rd/preview-sessions/:session_id/proxy/*
POST /api/v1/rd/preview-sessions/:session_id/stop
GET  /api/v1/rd/preview-sessions/:session_id/logs
POST /api/v1/rd/preview-sessions/:session_id/screenshot
POST /api/v1/rd/preview-sessions/:session_id/console-event
```

## 开源扩展点

推荐扩展方式：

- 新增 Code Studio prompt 放到 `rust/crates/web-server/src/routes/rd/prompts.rs`。
- 优先通过 RD Agent profile 或 workflow 扩展行为，不要把逻辑硬编码进 route。
- 新 runtime 能力通过 Agent Runtime 层扩展。
- UI 新模块放到 `webui/src/pages/rdStudio/`。
- 所有用户可见文案同时补齐 `zh-CN.json` 和 `en-US.json`。

## 质量检查

发布前至少运行：

```bash
cd rust
cargo fmt --all --check
cargo check -p web-server --features bot-agents
cargo test -p aos-contract-tests

cd ../webui
npm run i18n:check
npm run typecheck
npm run code-studio:check
```
