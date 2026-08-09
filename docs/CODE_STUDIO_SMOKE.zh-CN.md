# Code Studio 冒烟测试

这份清单用于验证 AOS Code Studio 是真实代码开发工作台，而不只是配置页。

## 前置条件

1. 用 full profile 启动后端。

```bash
AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh build
AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh run
```

2. 启动 WebUI。

```bash
cd webui
npm run dev
```

3. 至少配置一个可用于 RD 的聊天模型 API Key。
4. 在 Projects 中添加并同步一个仓库。

## Vibe Mode 冒烟

1. 打开 `/agent`。
2. 确认模式选择器默认是 `Vibe`。
3. 选择仓库和模型。
4. 提问：

```text
这个项目怎么启动？
```

期望：

- 创建真实 RD task。
- 任务出现在 thread 列表。
- Agent timeline 开始更新。
- Workbench 显示仓库文件树。
- 运行命令时 Terminal 面板显示当前命令和输出。
- WatchDog 能看到关联的 `rd_agent` 任务。

## Vibe Diff-first 冒烟

1. 在 Vibe Mode 中要求一个很小的安全修改。

```text
修复 README 里一个明显的拼写问题，并给出 diff。
```

期望：

- Agent 在 candidate workspace 中执行。
- Diff 出现在 Diff 页签。
- 用户点击应用前，主仓库不会被修改。
- Apply all / selected hunks 可用。
- Workbench file changes 和 changed file groups 会更新。

## 测试命令冒烟

1. 选择一个带仓库的任务。
2. 在 Terminal 中输入测试命令。
3. 点击 Run Test。

期望：

- 生成 test run 记录。
- Terminal 面板显示命令输出预览。
- Test 页签显示状态、退出码、stdout/stderr。
- WatchDog 详情能看到 runtime/process 证据。

## Plan Mode 冒烟

1. 在 `/agent` 切换到 `Plan`。
2. 创建计划：

```text
为登录失败增加明确错误提示，并保证现有登录测试不回归。
```

期望：

- 创建持久化 `rd_spec`。
- 创建关联的 AgentOps task，`linked_resource_type=rd_spec`。
- Spec 生成会写入 `rd_spec_events`、`agent_task_events` 和 `agent_trace_events`。

3. 确认 Spec。
4. 生成并确认 Design。
5. 生成并确认 Tasks。
6. 点击某个 task item 的 Implement。

期望：

- 创建真实 `rd_task`。
- task item 写入 `linkedRdTaskId`。
- UI 切回 Vibe Mode 并打开 RD task。
- WatchDog 能同时看到 Plan task 和 implementation task。

## WatchDog 冒烟

向 WatchDog 提问：

```text
当前有哪些 RD Agent 在运行？
```

期望：

- 列出活跃或最近的 `rd_agent` 任务。
- Plan Mode 阶段作为 linked `rd_spec` task 出现。
- Implementation 任务作为 linked `rd_task` task 出现。
- 详情里能看到 trace events，存在 runtime 时能看到 process 证据。

## Bot RD 冒烟

1. 给 Bot 绑定 `rd_agent` 能力。
2. 私聊发送一个代码开发任务。
3. 打开 Code Studio。

期望：

- Bot 创建的任务进入 AgentOps。
- WatchDog 可以查询它。
- Code Studio workbench 可以打开关联 RD task。

## 通过标准

- UI 不出现 `undefined` 文案。
- 长路径、长命令、长错误不会撑破页面。
- `npm run i18n:check` 缺失 key 为 0。
- `npm run typecheck` 通过。
- `cargo check -p web-server --features bot-agents` 通过。
