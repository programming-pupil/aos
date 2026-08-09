# Contributing

Thank you for helping improve AOS. AOS is a Web-first Agent OS with Rust backend
services, a React Web UI, AgentOps/WatchDog, Bot Gateway integrations, R&D
workflows, and data exploration.

## Before You Start

- Read `README.md`, `docs/ARCHITECTURE.md`, and the relevant runbook under
  `docs/`.
- Keep changes focused. Avoid unrelated refactors, formatting churn, and local
  environment files.
- Never commit `.env`, runtime data, local workspaces, API keys, tokens, logs, or
  generated build artifacts.
- Prefer existing architecture and helper APIs over adding new abstractions.

## Development Setup

Backend:

```bash
cd rust
AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh check
```

Web UI:

```bash
cd webui
npm install
npm run typecheck
npm run i18n:check
npm run build
```

## Required Checks

Run the smallest relevant checks for your change. For broad changes, run:

```bash
cd rust
cargo fmt --all --check
cargo check -p web-server --features bot-agents
cargo check -p web-server --features full
cargo test -p aos-contract-tests

cd ../webui
npm run typecheck
npm run i18n:check
```

## Pull Request Expectations

A good PR includes:

- What changed and why.
- Screenshots or logs for UI/Bot/AgentOps changes.
- Verification commands and results.
- Compatibility or migration notes.
- Known limitations and follow-up work.

## Code Style

- Rust: return structured errors instead of panicking in production paths.
- Frontend: all user-facing text must use i18n.
- Bot/AgentOps: every long-running task should write task events and meaningful
  failure reasons.
- Runtime: never log raw secrets; store large outputs as artifacts or previews.

