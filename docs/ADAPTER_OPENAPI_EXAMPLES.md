# Adapter And OpenAPI Examples

This document is the public extension starting point for AOS adapters. It is
intentionally small: every adapter should expose a contract, config schema,
permission requirements, trace mapping, and failure modes before it becomes a
first-class built-in.

## Capability Contract Endpoint

```http
GET /api/v1/agent-ops/capabilities
```

Example response item:

```json
{
  "key": "watchdog",
  "displayName": "WatchDog",
  "menuKey": "watchdog",
  "executionMode": "sync",
  "supportsBot": true,
  "supportsWatchDog": true,
  "supportsContext": false,
  "supportsAsync": false,
  "requiredConfig": [],
  "optionalConfig": ["watchdogScope", "allowActions"],
  "requiredPermissions": ["watchdog:read"],
  "actions": ["detail", "cancel", "retry"]
}
```

Bot configuration screens should load this endpoint instead of hard-coding
capability lists.

## Built-in Adapter Families

- `CapabilityAdapter`: AI Chat, PM Assistant, RD Agent, NL2SQL, Super
  Adversarial, WatchDog, AOS Router.
- `BotPlatformAdapter`: Feishu/Lark, WeCom, Slack, Discord, DingTalk,
  Telegram, WhatsApp, Generic Webhook.
- `TaskRuntimeAdapter`: Local Process, Docker Sandbox.
- `WatchDogActionAdapter`: detail, cancel, retry, show logs, open runtime.

## Minimal Capability Adapter Shape

```rust
pub trait CapabilityAdapter {
    fn key(&self) -> &'static str;
    fn contract(&self) -> CapabilityContract;

    async fn start(
        &self,
        ctx: CapabilityContext,
        input: CapabilityInput,
    ) -> Result<CapabilityStartResult>;

    async fn cancel(
        &self,
        ctx: CapabilityContext,
        task: LinkedTaskRef,
    ) -> Result<CancelResult>;

    async fn retry(
        &self,
        ctx: CapabilityContext,
        task: LinkedTaskRef,
    ) -> Result<RetryResult>;

    async fn status(
        &self,
        ctx: CapabilityContext,
        task: LinkedTaskRef,
    ) -> Result<CapabilityStatus>;
}
```

Minimal contract:

```rust
CapabilityContract {
    key: "my_capability",
    display_name: "My Capability",
    menu_key: "extensions",
    execution_mode: "hybrid",
    supports_bot: true,
    supports_watchdog: true,
    supports_context: true,
    supports_async: true,
    required_config: vec!["model"],
    optional_config: vec!["triggerPrefixes"],
    required_permissions: vec!["bot_agents:use"],
    actions: vec!["start", "cancel", "retry", "detail"],
}
```

## Minimal Bot Platform Adapter Shape

```rust
pub trait BotPlatformAdapter {
    fn platform(&self) -> &'static str;

    fn normalize_inbound(&self, raw: Value) -> Result<NormalizedInboundMessage>;

    async fn send_message(
        &self,
        ctx: BotSendContext,
        msg: BotOutboundMessage,
    ) -> Result<BotSendResult>;

    fn verify_signature(
        &self,
        headers: HeaderMap,
        body: &[u8],
        secret: Option<&str>,
    ) -> Result<()>;
}
```

Every platform adapter must document:

- inbound modes: webhook, polling, socket, stream
- outbound modes: webhook, bot token, OpenAPI, relay
- mention semantics for private chat and group chat
- required credentials
- local development story
- retry and deduplication behavior

## Trace Event Mapping

Adapters should emit these common event types:

```text
bot.inbound
task.queued
task.claimed
router.decision
context.loaded
model.request
model.response
tool.started
tool.completed
runtime.command.started
runtime.command.completed
bot.outbound
task.completed
task.failed
watchdog.action
```

Large payloads should be stored as artifacts. Trace events should keep previews
and metadata only; never store raw secrets.

## Validation Error Format

Adapters should return structured validation errors:

```json
{
  "error": "validation error: missing model",
  "status": 400,
  "code": "CAPABILITY_CONFIG_MISSING",
  "details": {
    "capability": "my_capability",
    "missing": ["model"]
  }
}
```

## Example Failure Modes

- Missing credential: return validation error and write `task.failed`.
- Platform outbound failure: write `bot.outbound.failed` with redacted metadata.
- Capability unavailable under current feature profile: return validation error
  explaining which feature/profile is required.
- Cancel unsupported: return explicit unsupported-action result; do not silently
  mark a task cancelled.

