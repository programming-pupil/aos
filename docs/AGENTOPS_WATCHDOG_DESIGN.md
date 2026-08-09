# AgentOps / WatchDog Design

AgentOps is the AOS control plane for observing work started from WebUI, Bot, webhook, schedule, or API. WatchDog is both a WebUI console and a bindable Bot capability for mobile inspection.

## Goals

- Bot is an entrypoint, not a reduced implementation.
- Bot binds menu capability contracts: `aos_router`, `ai_chat`, `super_adversarial`, `watchdog`, `pm_assistant`, `rd_agent`, `nl2sql`, `generic_ai`.
- Every task writes `agent_tasks` and `agent_task_events`.
- WatchDog answers from structured task/event data first. It must not invent state.

## Data Model

`agent_tasks` stores one observable unit of work with tenant, source, capability, status, phase, owner, external conversation ids, linked resources, input/output snapshots, last event, heartbeat, and timestamps.

`agent_task_events` stores append-only lifecycle events with type, phase, status, severity, message, metadata, and timestamp.

## Status Machine

`created -> queued -> claimed -> running -> waiting_input -> blocked -> retrying -> completed`

Terminal states: `completed`, `failed`, `cancelled`, `timed_out`, `stale`.

## Phases

`intake`, `capability_matching`, `context_loading`, `planning`, `retrieving`, `model_calling`, `debating`, `judging`, `tool_running`, `executing`, `validating`, `replying`, `finalizing`, `idle`.

## WatchDog Scope

Default Bot scope is the current external conversation. Users can ask for `全部` to expand to tenant scope. WebUI uses tenant scope.

## P0 Behavior

- Bot inbound creates an AgentOps task.
- Bot inbound is executed through the durable `bot_message_logs` queue, not an in-process-only spawn.
- Context loading, capability execution, reply delivery, failures, and completions write events.
- Bot execution uses a shared Hybrid policy: quick work replies synchronously, long-running work sends a first acknowledgement and continues asynchronously, and clarification flows move to `waiting_input`.
- WatchDog WebUI shows summary, capability health, live task board, task inspector, and Ask WatchDog.
- WatchDog Bot answers short mobile-friendly summaries.

## P1/P2 Hardening Status

- P1 includes NL2SQL clarification-style multi-turn UX through Bot conversations.
- P1 safe retry is enabled only for adapters with explicit replay semantics. `rd_task`, `pm_research_task`, `chat_adversarial_run`, and `nl2sql_agent_query` create new AgentOps child tasks while preserving the original task/resource audit trail.
- Replay-sensitive resources must return explicit diagnostics instead of silent duplication unless their adapter implements safe retry.
- Runtime stale recovery is exposed through `POST /api/v1/agent-runtime/sessions/recover`; it marks heartbeat-expired runtime sessions as `stale`, finalizes running processes as `timed_out`, mirrors the state to linked AgentOps tasks, and writes timeline events.
- Runtime contracts are discoverable from `/api/v1/agent-ops/capabilities`. `local_process` is the default runtime; `docker_sandbox` is available as an opt-in runtime via `AOS_AGENT_RUNTIME_ISOLATION_MODE=docker_sandbox`, mounts the task workspace into `/workspace`, and keeps Docker network disabled by default.
- Durable queue inspection is exposed through `GET /api/v1/agent-ops/queue`. Operators can filter by `queueStatus`, `capabilityKey`, `workerId`, `deadOnly`, and `staleOnly`; stale detection uses `leaseTimeoutSecs` and the same lease fields used by queue recovery.
- Code Studio consumes the shared RD workbench aggregation API for linked RD tasks. The workbench response includes the AgentOps task, AgentOps/trace events, runtime session, runtime processes, artifacts, file changes, test runs, latest answer, and suggested actions, so the coding UI and WatchDog inspect the same execution evidence.
