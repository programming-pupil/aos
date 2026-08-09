# Bot Platform Smoke Tests

This runbook verifies that AOS Bot Gateway can receive inbound messages, route
them through real AOS capabilities, and send outbound replies. Keep the checks
small and repeatable; they are meant for release validation, not full platform
certification.

## Preconditions

- Start `web-server` with the feature profile you want to release.
- Complete WebUI setup and create an administrator user.
- Configure at least one chat API key in System -> API Keys.
- Create one Bot Agent and bind `aos_router` (Super Assistant).
- Optionally bind `rd_agent`, `nl2sql`, and `super_adversarial` when those
  explicit prefixes need dedicated configuration. Task query/control is global
  and does not require a WatchDog capability binding.
- AOS automatically makes `aos_router` the unmatched-message entry. The
  `fallback_when_no_prefix` storage field is not user-configurable.
- For a copyable local setup, use `examples/bot-router/aos_router_agent.json`,
  `examples/bot-router/generic_webhook_channel.json`, and
  `examples/bot-router/smoke_messages.jsonl`.
- Enable structured logs:

```bash
RUST_LOG=web_server=info,tower_http=info,feishu_sdk=info
```

## Universal Smoke Matrix

Run these messages against every supported platform that is enabled for the
release.

| Message | Expected Route | Expected Result |
| --- | --- | --- |
| `今天天气咋样？` | `aos_router` | Super Assistant answer using the configured search path. |
| `印尼出海用户画像` | `aos_router` | Super Assistant answer or durable research acknowledgement. |
| `当前有哪些 agent 在运行？` | global task control | Mobile-friendly task summary before capability routing. |
| `停掉刚才那个研究任务` | global task control | Resolves the relative task and terminates its runtime. |
| `修复登录超时 bug` | `rd_agent` if bound | RD task id and AgentOps task visible in WatchDog. |
| `昨天 GMV 按国家统计` | `nl2sql` if bound | SQL/query answer or clarification prompt. |
| `两个方案辩一辩` | `super_adversarial` if bound | Debate run id and async progress. |

Expected logs:

```text
bot gateway inbound message queued
router.decision
bot.queue.succeeded
bot outbound delivery succeeded
```

For task-control commands, expected logs also include:

```text
watchdog.action
```

## Feishu Local Smoke

Inbound mode: long connection.

1. Create an internal app in Feishu.
2. Enable long-connection event subscription.
3. Subscribe to `im.message.receive_v1`.
4. In AOS Bot channel, set platform to `feishu`, inbound mode to `auto` or
   `stream`, and fill App ID/App Secret.
5. For outbound, configure either:
   - custom bot webhook, or
   - App ID/App Secret plus default chat ID.
6. Start the server and look for:

```text
Stream connected: wss://msg-frontier.feishu.cn
Dispatching event: im.message.receive_v1
Feishu/Lark inbound event queued
```

Private chat is treated as direct mention. Group chat requires mention when
`require_mention=true`.

## Lark Local Smoke

Repeat the Feishu smoke using platform `lark` and the international Lark
developer console. Confirm that logs and outbound calls use
`open.larksuite.com`, not `open.feishu.cn`.

## WeCom Local Smoke

Inbound mode: AI Bot WebSocket.

1. Create a WeCom AI Bot.
2. Configure AOS channel platform `wecom`, inbound mode `auto` or `stream`.
3. Fill Bot ID/Bot Secret.
4. Configure outbound group bot webhook for replies.
5. Send the universal smoke messages in private chat and then in a group.

Expected behavior:

- Private chat can trigger without explicit mention.
- Group chat follows `require_mention`.
- Outbound failures should show a structured validation error, not just a 500.

## Slack Local Smoke

Inbound mode: Socket Mode.

1. Enable Socket Mode in the Slack app.
2. Create an app-level token with `connections:write`.
3. Subscribe to message events.
4. Configure AOS platform `slack`, inbound mode `auto` or `socket`.
5. Fill App-Level Token.
6. Configure Incoming Webhook or Bot Token + Channel ID for outbound.

Expected behavior:

- Direct messages route through Super Assistant.
- Channel messages require mention when configured.
- Task-control commands are handled before capability routing.

## Discord Local Smoke

Inbound mode: Gateway WebSocket.

1. Create a Discord bot.
2. Enable Message Content Intent if raw text is required.
3. Configure AOS platform `discord`, inbound mode `auto` or `socket`.
4. Fill Bot Token.
5. Configure Discord Webhook or Bot Token + Channel ID for outbound.

## Telegram Smoke

Inbound mode: polling.

1. Create a BotFather bot and copy the token.
2. Configure AOS platform `telegram`, inbound mode `auto` or `polling`. Use
   `webhook` only when Telegram can reach the generated public callback URL.
3. Send private messages with the universal smoke matrix.

## DingTalk Smoke

Inbound mode: stream, outbound group bot webhook.

1. Configure stream credentials in AOS.
2. Configure group bot webhook and signing secret for outbound.
3. Verify universal smoke messages.

## WhatsApp Smoke

WhatsApp Cloud API does not provide a first-party local polling/socket inbound
mode. Use one of:

- HTTPS callback with a public URL.
- Local tunnel.
- Generic relay webhook.

Do not mark WhatsApp smoke as passed unless inbound and outbound both work.
The channel must use `webhook`; AOS deliberately exposes no local
polling/socket mode for WhatsApp.

## Generic Webhook Router Smoke

Use this when validating the unified Router locally without a third-party Bot platform.

1. Create the Router Bot Agent from `examples/bot-router/aos_router_agent.json`.
2. Replace `agent_id` in `examples/bot-router/generic_webhook_channel.json`.
3. Create the generic webhook channel.
4. POST each line from `examples/bot-router/smoke_messages.jsonl` to:

```text
/api/v1/bot-agents/webhooks/{channel_id}?secret=aos-router-smoke
```

Expected behavior:

- Each inbound payload creates a `bot_message_logs` item and an AgentOps task.
- Router decisions are visible in task input/events.
- `取消 1`, `详情 1`, and `重试 1` are handled by global task control before
  Super Assistant routing.
- Capability configuration errors are structured and visible in the task trace
  instead of being swallowed as generic Bot failures.

## Transport Contract Checks

Verify the channel form offers only these inbound modes:

| Platform | Modes |
| --- | --- |
| DingTalk, Feishu, Lark, WeCom | `auto`, `stream` |
| Slack, Discord | `auto`, `socket` |
| Telegram | `auto`, `polling`, `webhook` |
| WhatsApp, Generic Webhook | `webhook` |

POSTing to `/webhooks/{channel_id}` for a Stream/Socket/Polling channel must be
rejected. This prevents a public compatibility endpoint from silently bypassing
the configured inbound transport.

For every platform, open the official setup guide from the channel form and
open the Advanced Config help. Confirm that the example contains no secret and
only documents keys consumed by that adapter.

## Release Evidence Template

```text
Date:
Commit:
Feature profile:
Platform:
Channel:
Inbound mode:
Outbound mode:
Messages tested:
Observed AgentOps task ids:
Observed Bot log ids:
Failures:
Notes:
```
