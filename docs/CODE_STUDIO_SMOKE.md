# Code Studio Smoke Tests

This smoke checklist verifies that AOS Code Studio works as a real coding workspace, not only as a configuration page.

## Prerequisites

1. Start the backend with the full profile.

```bash
AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh build
AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh run
```

2. Start the WebUI.

```bash
cd webui
npm run dev
```

3. Configure at least one chat-capable RD model API key.
4. Add and sync one repository under Projects.

## Vibe Mode Smoke

1. Open `/agent`.
2. Confirm the mode selector defaults to `Vibe`.
3. Select a repository and model.
4. Ask:

```text
这个项目怎么启动？
```

Expected:

- A real RD task is created.
- The task appears in the thread list.
- Agent timeline starts updating.
- Workbench shows repository file tree.
- Terminal panel shows current runtime command/output when commands run.
- WatchDog can see the linked `rd_agent` task.

## Vibe Diff-First Smoke

1. In Vibe Mode, ask for a tiny safe change.

```text
修复 README 里一个明显的拼写问题，并给出 diff。
```

Expected:

- Agent works in candidate workspace.
- Diff appears in the Diff tab.
- Main repo is not modified until user applies the diff.
- Apply all / selected hunks works.
- Workbench file changes and changed file groups update.

## Test Runner Smoke

1. Select a task with a repository.
2. Enter a test command in Terminal.
3. Click Run Test.

Expected:

- A test run is recorded.
- Terminal panel shows command output preview.
- Test tab shows status, exit code, stdout/stderr.
- WatchDog task detail shows runtime/process evidence.

## Plan Mode Smoke

1. Switch `/agent` to `Plan`.
2. Create a plan with:

```text
为登录失败增加明确错误提示，并保证现有登录测试不回归。
```

Expected:

- A persistent `rd_spec` is created.
- A linked AgentOps task is created with `linked_resource_type=rd_spec`.
- Spec generation writes `rd_spec_events`, `agent_task_events`, and `agent_trace_events`.

3. Approve Spec.
4. Generate and approve Design.
5. Generate and approve Tasks.
6. Click Implement on one task item.

Expected:

- A real `rd_task` is created.
- The task item stores `linkedRdTaskId`.
- UI switches to Vibe Mode and opens the RD task.
- WatchDog can see both the Plan task and implementation task.

## WatchDog Smoke

Ask WatchDog:

```text
当前有哪些 RD Agent 在运行？
```

Expected:

- It lists active or recent `rd_agent` tasks.
- Plan Mode stages appear as linked `rd_spec` tasks.
- Implementation tasks appear as linked `rd_task` tasks.
- Detail view includes trace events and runtime/process evidence when available.

## Bot RD Smoke

1. Bind a Bot capability to `rd_agent`.
2. Send a private message asking for a code task.
3. Open Code Studio.

Expected:

- Bot-created task appears in AgentOps.
- WatchDog can query it.
- Code Studio workbench can open the linked RD task.

## Pass Criteria

- No `undefined` text appears in the UI.
- Long paths, commands, and errors do not overflow the page.
- `npm run i18n:check` reports 0 missing keys.
- `npm run typecheck` passes.
- `cargo check -p web-server --features bot-agents` passes.
