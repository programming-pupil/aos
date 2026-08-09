# AOS Open-Source Wow Demo

Start the local demo stack:

```bash
./scripts/aos-demo-start.sh
```

This starts one AOS server with `.aos-demo-data/aos.db`; the embedded SQLite baseline is applied automatically.

Then complete setup, open Dashboard, and click one of the demo cards.

## Demo Assets

- Code Studio frontend bug repo: `examples/code-studio/frontend-bug-demo`
- Bot Router smoke config: `examples/bot-router`
- Manifest: `examples/demo_manifest.json`

## What Each Demo Proves

### Fix a frontend bug

Proves Code Studio can inspect files, run a preview, capture an error, create a candidate Diff, run tests, and keep the main repository unchanged until human review.

Setup:

1. Open Code Repos.
2. Register `examples/code-studio/frontend-bug-demo` as a local repository.
3. Sync the repository.
4. Open the demo card; the prompt is prefilled in Code Studio.
5. Start Preview with `npm run dev`, then ask AOS to fix the console error.

### Ask WatchDog

Proves AgentOps tasks are visible through WatchDog with structured state, events, stale/failed explanations, and action affordances.

Clicking the WatchDog demo card seeds demo AgentOps tasks for running/stale/failed states. These are marked with `source=demo` so they are easy to filter and safe to ignore.

### Bot Router unified entrance

Proves users do not need to remember menu names or prefixes. The same Bot can route ordinary chat, PM analysis, code tasks, NL2SQL, WatchDog actions, and Super Adversarial debates.

Smoke assets:

- `examples/bot-router/aos_router_agent.json`
- `examples/bot-router/generic_webhook_channel.json`
- `examples/bot-router/smoke_messages.jsonl`

Runbook: `examples/bot-router/README.md`.

## Release Smoke

```bash
cd rust
cargo check -p web-server --features full

cd ../webui
npm run i18n:check
npm run typecheck
npm run build
```
