# WatchDog Runbook

WatchDog is the durable task control plane for AOS. Super Assistant and the
domain executors do the work; WatchDog records task state, projects progress,
accepts authorized commands, and delivers notifications.

## Required Schema

Apply migrations `188` through `194` before starting the server. Startup fails
closed when the control-plane schema is incomplete. Do not edit already applied
migrations.

## WebUI

- `/tasks` is the user task command center. It defaults to the signed-in user's
  own, assigned, or explicitly shared tasks.
- The global task drawer uses resumable SSE and shows active or waiting tasks.
- `/agent-ops` is the administrator operations view for tenant-wide recovery,
  stale leases, traces, and failed notification deliveries.
- `/watchdog` is a compatibility route. New links should use `/tasks` or
  `/agent-ops` directly.

Administrator role does not silently widen the user task APIs. Tenant-wide
visibility is available only through an explicit admin scope or AgentOps route.
Team scope fails closed until an organization directory is configured.

## Bot Identity And Commands

1. In `/tasks`, open Watch Settings and generate a one-time pairing code.
2. Send `bind CODE` or `绑定 CODE` to the Bot in a private conversation.
3. After pairing, task status requests use the bound AOS user and default to
   that user's authorized task set.

Use stable task references for writes:

```text
现在有哪些任务在执行？
查看 #A1B2C3D4E5 的进度
取消 #A1B2C3D4E5
重试 #A1B2C3D4E5
```

Words such as `all` or `全部` never elevate task scope. Bot write commands also
require `tasks:control`; authorization is checked again by the command worker.
Dynamic list positions such as “取消第一个” are not accepted as write targets.

## Notifications

The notification outbox defaults to shadow mode:

```text
WATCHDOG_NOTIFICATION_OUTBOX_MODE=shadow
```

Use `on` only after platform identity, destination, DLP, and duplicate-delivery
smoke tests pass. Failed deliveries are visible in AgentOps and can be replayed
by an administrator. Replays are audited and remain subject to current identity
binding checks.

Mobile follow is opt-in. A WebUI-originated long task is sent to the user's most
recent active Bot binding only when mobile follow is enabled and all WebUI
presence leases have expired. Bot-originated tasks continue to reply to their
original conversation.

Simple completed Super Assistant turns under 60 seconds without subtasks are
archived and do not create automatic completion notifications.

## Watch Rules

Watch rules support notification, one retry, approval, and escalation drafts.
Actions with side effects always enter the Decision Inbox. Approval performs a
fresh permission and task-version check, then queues the durable command or
delivery. Duplicate decisions are rejected and every outcome is audited.

## Recovery

- Command, outbox, and delivery workers reclaim expired leases after restart.
- Projectors scan active resources independently of user reads.
- A missed heartbeat first emits `task.stalled`. Only a later resource recheck
  may transition the task to `stale`.
- Cancellation records `desired_state=cancelled` before dispatching to the real
  executor. A failed dispatch is visible in the task audit trail.
- Retry creates a new attempt or child execution and preserves prior evidence.

Generic pause/resume, supplemental input, and non-RD approvals fail explicitly
when the linked executor has no safe adapter. They never pretend to have resumed
the task.

## Diagnostics

Check these in order:

1. `/agent-ops` worker health, dead queues, stale leases, and failed deliveries.
2. `agent_tasks`, `agent_task_outbox`, `agent_task_command_requests`, and
   `agent_notification_deliveries` for the stable task ID.
3. The linked resource and attempt timeline in `/tasks`.
4. Bot channel signature validation, durable inbound logs, identity binding,
   and destination configuration.

Recommended production checks include A/B user isolation, permission revocation,
worker termination during claim, platform 429/5xx behavior, DLP samples, and one
real private/group conversation smoke test for every enabled Bot provider.
