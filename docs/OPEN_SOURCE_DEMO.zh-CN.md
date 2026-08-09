# AOS 开源 Wow Demo

启动本地 demo：

```bash
./scripts/aos-demo-start.sh
```

默认启动一个 AOS 服务并使用 `.aos-demo-data/aos.db`，内嵌的 SQLite baseline 会自动执行。

然后完成初始化，打开仪表盘，点击 demo 卡片之一。

## Demo 资产

- Code Studio 前端 Bug 示例仓库：`examples/code-studio/frontend-bug-demo`
- Bot Router smoke 配置：`examples/bot-router`
- Manifest：`examples/demo_manifest.json`

## 每个 Demo 证明什么

### 修复前端 Bug

证明 Code Studio 能读取文件、启动预览、采集错误、生成 candidate Diff、运行测试，并且主仓库不会在人工审查前被静默修改。

准备步骤：

1. 打开代码仓库。
2. 把 `examples/code-studio/frontend-bug-demo` 注册为本地仓库。
3. 同步仓库。
4. 点击 demo 卡片，Code Studio 会预填 prompt。
5. 用 `npm run dev` 启动 Preview，然后让 AOS 修复 console error。

### 询问看门狗

证明 AgentOps 任务能被 WatchDog 看到，并展示结构化状态、事件、stale/failed 解释和动作入口。

点击 WatchDog demo 卡片会自动 seed running/stale/failed 状态的 demo AgentOps 任务。这些任务标记为 `source=demo`，方便过滤，也不会伪装成生产任务。

### Bot Router 统一入口

证明用户不需要记菜单名或前缀。同一个 Bot 可以把普通聊天、产运分析、代码任务、NL2SQL、WatchDog 动作和超级对抗自动分发到正确能力。

Smoke 资产：

- `examples/bot-router/aos_router_agent.json`
- `examples/bot-router/generic_webhook_channel.json`
- `examples/bot-router/smoke_messages.jsonl`

运行说明：`examples/bot-router/README.md`。

## 发布 Smoke

```bash
cd rust
cargo check -p web-server --features full

cd ../webui
npm run i18n:check
npm run typecheck
npm run build
```
