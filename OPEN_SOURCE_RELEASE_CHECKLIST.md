# Open Source Release Checklist

Use this checklist before publishing AOS to a public repository.

## 1. Repository Boundary

Do not delete local development data just to publish. Instead, ensure these
paths are ignored and are not tracked in the public repository:

- `.env`, `.env.*` except `.env.example`
- `.aos-data/`, `.aos-data-*/`, `.aos-demo-data/`
- `.aos-upstream-audit/`
- `.aos-alignment-progress.md`
- `.claude/`, `.vite/`, `.vscode/`
- `rust/.aosd-agents/`, `rust/.claude/`, `rust/.claw/`
- `rust/target/`, `rust/target-*/`, `webui/dist/`, `webui/node_modules/`
- logs, jsonl traces, database files, runtime artifacts, local workspaces

If a file was already tracked, `.gitignore` is not enough. Remove it from the
public index before publishing.

## 2. Required Checks

```bash
cd rust
cargo fmt --all --check
cargo test --workspace --all-features
cargo build -p web-server --features full
cargo run -p eval-harness

cd ../webui
npm run typecheck
npm run test
npm run i18n:check
npm audit --omit=dev
npm run build

cd ..
scripts/check-platform-sqlite-boundary.sh
bash -n scripts/*.sh rust/scripts/*.sh
release_env="$(mktemp -d)/.env"
./scripts/generate-env.sh "$release_env"
docker compose --env-file "$release_env" config --quiet
```

## 3. Documentation

Confirm these files exist and are linked from `README.md` when appropriate:

- `CONTRIBUTING.md`
- `SECURITY.md`
- `CODE_OF_CONDUCT.md`
- `LICENSE`
- `NOTICE.md`
- `docs/BOT_PLATFORM_SMOKE.md`
- `docs/ADAPTER_OPENAPI_EXAMPLES.md`
- `docs/evals/router_watchdog_golden.jsonl`

## 4. Security

- Rotate any key that may have appeared in local logs or `.env`.
- Verify `.env.example` contains placeholders only.
- Confirm JWTs do not appear in URLs, screenshots, access logs, or checked-in fixtures.
- Confirm uploaded active content is served as an attachment and Office extraction limits reject decompression bombs.
- Review `SECURITY.md` and runtime safety notes.
- Do not publish private Bot platform credentials or data-source connection
  strings.

## 5. Smoke Evidence

Before a tagged release, capture:

- Docker quickstart result.
- WebUI setup screenshot.
- Bot Gateway smoke for at least one local-first platform.
- WatchDog query screenshot or Bot reply.
- R&D task AgentOps trace screenshot if RD is advertised.

## 6. Quality Claims

- Run blinded online A/B cases through AOS and the declared Codex surface with the same model, files, tools, and time budget where possible.
- Report answer correctness, evidence grounding, SQL execution/semantic correctness, follow-up memory, disconnect recovery, latency, and token cost separately.
- Do not describe the deterministic `eval-harness` fixture as proof that AOS outperforms Codex; it is a wiring/regression gate only.
