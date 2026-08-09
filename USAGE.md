# AOS Usage

This guide covers the current Web-first AOS workflow. The old CLI workflow has been removed from this repository; use the Rust WebServer and Web UI instead.

## Docker Startup

```bash
./scripts/generate-env.sh
docker compose up --build
```

Open `http://localhost:3000` and complete the setup flow. The server creates `/data/aos.db` and applies embedded SQLite migrations before accepting requests.

Useful Docker commands:

```bash
# Stop services but keep data
docker compose down

# Reset local Docker data and recreate SQLite from scratch
docker compose down -v

# View backend logs
docker compose logs -f server
```

## Development Startup

### Backend

```bash
cd rust
./scripts/dev_web_server.sh run
```

Useful backend commands:

```bash
# Fast compile check for the web-server crate
./scripts/dev_web_server.sh check

# Build once
./scripts/dev_web_server.sh build

# Restart without rebuilding when only env/config changed
./scripts/dev_web_server.sh quick

# Stop previous web-server/cargo-run processes
./scripts/dev_web_server.sh stop
```

### Frontend

```bash
cd webui
npm install
npm run dev
```

For a production frontend build:

```bash
cd webui
npm run build
```

## First Run

When no tenant exists, AOS redirects protected pages and API calls to the setup flow. Complete setup to create the first tenant and administrator. Tenant initialization also seeds required built-in data such as default Skills repositories.

## Model And API Key Configuration

Use the Web UI:

1. Open `System -> API Keys`.
2. Add at least one enabled key with model type `chat`. A chat-scoped key is reused by the unified Super Assistant and is a fallback for deep research and other chat-model capabilities.
3. Add specialized embedding, image, audio, or video keys only for features that need them.
4. Set priority when multiple keys are available; AOS deduplicates and tries candidates in deterministic failover order.

Runtime model selection is table-driven through API Key management rather than CLI environment variables.

## Configuration Management

Use `System -> Configuration Management` for runtime defaults that are backed by environment variables or database settings. If an environment variable is not configured, AOS returns and uses the code default.

## Bot Gateway

Use `System -> Bot Gateway` to create bots, bind capabilities, add platform channels, and configure notifications.

Each platform exposes only the fields it can actually use:

- DingTalk: Stream credentials for inbound and group-bot Webhook/signing for outbound.
- Telegram: Bot Token for polling inbound and outbound replies.
- Feishu/Lark: event-subscription Webhook inbound and custom-bot Webhook/signing outbound.
- WeCom: JSON callback inbound and group-bot Webhook outbound.
- Slack: Events API inbound and Incoming Webhook or Bot Token outbound.
- WhatsApp: Cloud API Webhook inbound and Cloud API or relay Webhook outbound.
- Discord: JSON event Webhook inbound and Webhook or Bot Token outbound.
- Generic Webhook: custom relay integration.

## Verification

```bash
# Rust formatting and tests for the shipped feature surface
cd rust
cargo fmt --all --check
cargo test --workspace --all-features

# Verify the platform SQLite / external NL2SQL connector boundary
cd ..
scripts/check-platform-sqlite-boundary.sh

# Frontend typecheck/tests/translations/build
cd webui
npm run build:ci
```
