# Bot Capability Contract

Bot capabilities are stable contracts that map external chat messages to real AOS menu capabilities.

## Standard Keys

- `ai_chat`: WebUI AI 对话.
- `super_adversarial`: WebUI 超级对抗.
- `watchdog`: AgentOps 看门狗 / WatchDog.
- `pm_assistant`: 产运助手.
- `rd_agent`: 代码开发 / RD Studio.
- `nl2sql`: 数据探索.
- `aos_router`: unified Bot entrance that routes to enabled capability bindings.
- `generic_ai`: lightweight fallback only.

## Config JSON

```json
{
  "trigger_prefixes": ["wd", "状态"],
  "require_mention": true,
  "fallback_when_no_prefix": false,
  "model": null,
  "models": [],
  "maxRounds": 8,
  "repositoryId": null,
  "agentProfileId": null,
  "workflowId": null,
  "dataSourceId": null,
  "allowExecuteSql": false,
  "watchdogScope": "conversation",
  "allowActions": ["open_task", "cancel_task", "retry_task"],
  "executionMode": "hybrid",
  "syncTimeoutMs": 15000,
  "ackTimeoutMs": 1500
}
```

Private chat counts as direct mention. Group chat requires platform mentions or textual `@` when `require_mention=true`.

## Hybrid Execution Policy

Bot capabilities use a shared execution policy instead of each adapter inventing its own behavior:

- `sync`: expected to answer quickly and complete the AgentOps task after the reply.
- `async`: sends a first acknowledgement with task/run id, keeps the AgentOps task open, monitors the linked resource, and pushes the final result when available.
- `hybrid`: sync for quick answers; adapters can promote long-running work to async when they create a linked resource.
- `clarification`: sync replies for each turn, but keeps the AgentOps task in `waiting_input` while the user must clarify or approve.

Default thresholds:

- quick sync target: under 8 seconds for WatchDog
- normal sync target: under 15 seconds for AI chat / short answers / simple NL2SQL
- async target: RD, Super Adversarial, and PM deep analysis
- clarification target: NL2SQL multi-turn clarification

`executionMode`, `syncTimeoutMs`, and `ackTimeoutMs` can be configured per bound capability. The policy is written to AgentOps events as `execution_policy`, and async first replies are written as `first_ack_sent`.

If a capability is configured as `async` but the adapter does not create a linked observable resource, AOS records `execution_policy_downgraded` and completes the task as a synchronous reply. This avoids pretending an untracked background job exists.

Inbound Bot execution is durable. The webhook/stream handler persists the normalized inbound message in `bot_message_logs`, creates the AgentOps task, attaches `agentTaskId` and `selectedCapability`, and only then marks the log `queue_status='queued'`. A background worker claims queued logs, executes the real capability adapter, and marks the queue item `succeeded` or `dead`.

Stale `claimed` work is recovered after `AOS_BOT_QUEUE_CLAIM_TIMEOUT_SECS` and retried until `max_attempts` is exhausted. This retry is for process crash / worker loss recovery. Business-level adapter failures are not blindly replayed because RD tasks, debate runs, SQL execution, and outbound messages can have real side effects.

`syncTimeoutMs` and `ackTimeoutMs` are policy targets and audit metadata. Adapters that already create durable resources use them for UX semantics; arbitrary synchronous model calls are not cancelled after timeout because that would create ambiguous user-visible behavior.

## Public Contract API

`GET /api/v1/agent-ops/capabilities` returns capability metadata for Bot binding UI, including menu key, execution mode, rollout, Bot support, WatchDog support, and required permissions.

## P0 Adapter Coverage

- `ai_chat`: sync answer through chat model routing.
- `super_adversarial`: creates a real chat adversarial run, links `chat_adversarial_run`, monitors completion, and pushes the final result back to the Bot conversation.
- `watchdog`: queries AgentOps and returns mobile summaries; optional LLM summarization is evidence-only and falls back to deterministic summaries.
- `pm_assistant`: sync short answer; deep mode creates a real PM research task, links `pm_research_task`, monitors completion, and pushes final status/result.
- `rd_agent`: creates a real RD task and links `rd_task`.
- `nl2sql`: runs real query/route/agent execution, supports clarification-style multi-turn in the same Bot conversation, persists query/result rows like WebUI, and links `nl2sql_agent_query`.
- `aos_router`: sync routing decision, then delegates to the selected capability.
- `generic_ai`: sync fallback answer.

## Adapter Status

- RD exposes linked AgentOps status/events in Code Studio, so Bot-created RD work is visible from both WatchDog and the coding workspace.
- WatchDog retry executes real safe retry for `rd_task` and `pm_research_task` by creating a new AgentOps child task and preserving the original audit trail. Replay-sensitive resources such as debate runs and NL2SQL queries return explicit diagnostics instead of being duplicated silently.
