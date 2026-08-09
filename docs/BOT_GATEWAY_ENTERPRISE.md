# Enterprise Bot Gateway

The Bot Gateway connects Feishu/Lark, WeCom, DingTalk, Slack, Discord, Telegram, WhatsApp-compatible webhooks, and generic webhooks to AOS capabilities.

## Enterprise Principles

- Platform adapters normalize inbound payloads.
- Capability adapters execute real AOS menu contracts.
- AgentOps records every task and event.
- WatchDog is available from WebUI and mobile Bot.
- Private chat is treated as direct mention. Group chat honors mention requirements.

## Execution Chain

```text
platform inbound
-> normalize payload
-> select capability
-> create agent_task
-> persist durable queue item
-> queue worker claims item
-> load Bot conversation context
-> execute capability adapter
-> link real resource
-> send outbound reply
-> complete or fail agent_task
```

## Durable Queue

`bot_message_logs` is the durable queue carrier for inbound execution. Queue metadata includes `queue_status`, `available_at`, `claimed_by`, `claimed_at`, `attempt_count`, `max_attempts`, `last_error`, and `finished_at`.

Queue states:

- `none`: log-only record, usually outbound.
- `queued`: ready for a worker.
- `claimed`: owned by one worker.
- `succeeded`: adapter completed or intentionally ignored the message.
- `dead`: unrecoverable execution/load failure or exhausted stale-claim attempts.

The worker does not hold a database transaction while a capability runs. It claims with a short conditional update, executes outside the transaction, and finalizes with a second update. Server restart recovery requeues stale claims; adapter-level failures are not automatically replayed unless that adapter exposes explicit safe retry semantics through WatchDog.

Operational knobs:

- `AOS_BOT_QUEUE_BATCH_SIZE`, default `5`, range `1..50`.
- `AOS_BOT_QUEUE_POLL_MS`, default `1000`, range `100..30000`.
- `AOS_BOT_QUEUE_CLAIM_TIMEOUT_SECS`, default `600`, range `30..86400`.

## Adapter Contracts

`GET /api/v1/agent-ops/capabilities` returns capability contracts plus adapter metadata:

- `items`: menu capability contracts.
- `botPlatforms`: built-in Bot platform contracts.
- `runtimes`: runtime adapter contracts.
- `watchdogActions`: WatchDog action contracts.

The response is intentionally public to the WebUI and extension authors so new adapters do not require hard-coded frontend lists.

Built-in runtime contracts:

- `local_process`: default. Uses per-task local workspaces and process-group cancellation.
- `docker_sandbox`: optional. Runs commands through `docker run` with the task workspace mounted at `/workspace`; disabled by default so local development does not require Docker.

Runtime knobs:

- `AOS_AGENT_RUNTIME_ISOLATION_MODE=local_process|docker_sandbox`, default `local_process`.
- `AOS_AGENT_RUNTIME_DOCKER_IMAGE`, default `ubuntu:24.04`.
- `AOS_AGENT_RUNTIME_DOCKER_NETWORK`, default `none`.

## Extending a Capability

1. Register a capability contract through the Capability Registry.
2. Add the capability execution adapter in the Bot Gateway.
3. Record AgentOps phase/events and standard trace events.
4. Link the real resource with `linked_resource_type` and `linked_resource_id`.
5. Add tests for matching, execution, failure, retry, and WatchDog visibility.

## Extending a Platform

1. Register a Bot platform contract.
2. Normalize inbound text, user, conversation, and message ids.
3. Implement outbound delivery.
4. Preserve local-development mode where the platform supports long connection, gateway, socket, or polling.
5. Log platform ids without leaking secrets.
