# Capability Matrix

| Capability | Menu | Execution | Bot | WatchDog | Rollout |
| --- | --- | --- | --- | --- | --- |
| `ai_chat` | AI 对话 | sync | supported | basic | P0 |
| `super_adversarial` | 超级对抗 | async | create run + final push | status/round/failure/final | P0 |
| `watchdog` | 看门狗 / WatchDog | sync | supported | self + optional LLM summary | P0 |
| `pm_assistant` | 产运助手 | sync + async deep | short answer + PM research task | status/stage/final push | P0 |
| `rd_agent` | 代码开发 | async | create RD task + safe retry | status/resource/events + Code Studio AgentOps bridge | P2 |
| `nl2sql` | 数据探索 | sync + clarification | real execution + persisted results + clarification multi-turn | result/status/waiting_input resource | P1 |
| `aos_router` | Unified Bot entrance | sync routing + delegated execution | supported | full | P0 |
| `generic_ai` | Bot fallback | sync | supported | basic | P0 |

`aos_router` is the recommended default Bot capability. `generic_ai` is only a lightweight fallback. New Bot agents should prefer `aos_router` with explicit `ai_chat`, `watchdog`, and domain capability bindings.

Current status:

- WatchDog safe retry is implemented for `rd_task` and `pm_research_task`; retry creates a new AgentOps child task and preserves the original task/resource audit trail. Unsupported replay-sensitive resources return explicit diagnostics.
- Code Studio consumes the shared AgentOps task/events for linked RD tasks.
