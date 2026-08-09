# AOS Router Bot Smoke

This directory contains copyable payloads for testing the unified AOS Bot entrypoint with the local `generic_webhook` adapter.

## Files

- `aos_router_agent.json` creates a Bot Agent whose default capability is `aos_router`.
- `generic_webhook_channel.json` creates a local generic webhook channel for that agent.
- `smoke_messages.jsonl` contains routing messages and expected capability targets.

## Setup

1. Start AOS with the `bot-agents` or `full` feature profile.
2. Create an admin user and an API token.
3. Create the Bot Agent:

```bash
curl -sS "$AOS_BASE_URL/api/v1/bot-agents" \
  -H "Authorization: Bearer $AOS_TOKEN" \
  -H "Content-Type: application/json" \
  --data @examples/bot-router/aos_router_agent.json
```

4. Copy the returned `id` into `generic_webhook_channel.json` as `agent_id`, then create the channel:

```bash
curl -sS "$AOS_BASE_URL/api/v1/bot-agents/channels" \
  -H "Authorization: Bearer $AOS_TOKEN" \
  -H "Content-Type: application/json" \
  --data @examples/bot-router/generic_webhook_channel.json
```

5. Send smoke messages through the created channel id:

```bash
while read -r payload; do
  curl -sS "$AOS_BASE_URL/api/v1/bot-agents/webhooks/$CHANNEL_ID?secret=aos-router-smoke" \
    -H "Content-Type: application/json" \
    --data "$payload"
  echo
done < examples/bot-router/smoke_messages.jsonl
```

6. Open Bot Gateway message logs and WatchDog. Each inbound message should create a queued Bot log and an AgentOps task with the selected capability recorded in task input/events.

## Expected Routing

The expected target for each message is recorded in `expectedCapability`. WatchDog action commands such as `取消 1` must route to `watchdog` before any LLM fallback.

Some capabilities need extra project/data-source/model configuration to produce a full business result. The smoke is still valid when those capabilities reply with a structured validation message or an async acknowledgement, as long as the Router target is correct and the failure is visible in AgentOps/WatchDog.
