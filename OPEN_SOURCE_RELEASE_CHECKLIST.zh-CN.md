# 开源发布清单

发布 AOS 到公开仓库前，请按此清单确认。

## 1. 仓库边界

不要为了开源删除本地开发数据。正确做法是确保这些路径被忽略，并且没有被公开仓库跟踪：

- `.env`、`.env.*`，但保留 `.env.example`
- `.aos-data/`、`.aos-data-*/`、`.aos-demo-data/`
- `.aos-upstream-audit/`
- `.aos-alignment-progress.md`
- `.claude/`、`.vite/`、`.vscode/`
- `rust/.aosd-agents/`、`rust/.claude/`、`rust/.claw/`
- `rust/target/`、`rust/target-*/`、`webui/dist/`、`webui/node_modules/`
- 日志、jsonl trace、数据库文件、runtime artifact、本地 workspace

如果文件已经被 git 跟踪，`.gitignore` 不会自动移除它。发布前需要从公开索引中移除。

## 2. 必要检查

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

## 3. 文档

确认这些文件存在，并在合适位置从 `README.md` 链接：

- `CONTRIBUTING.md`
- `CONTRIBUTING.zh-CN.md`
- `SECURITY.md`
- `SECURITY.zh-CN.md`
- `CODE_OF_CONDUCT.md`
- `CODE_OF_CONDUCT.zh-CN.md`
- `OPEN_SOURCE_RELEASE_CHECKLIST.md`
- `OPEN_SOURCE_RELEASE_CHECKLIST.zh-CN.md`
- `LICENSE`
- `NOTICE.md`
- `docs/BOT_PLATFORM_SMOKE.md`
- `docs/ADAPTER_OPENAPI_EXAMPLES.md`
- `docs/evals/router_watchdog_golden.jsonl`

## 4. 安全

- 轮换任何可能出现在本地日志或 `.env` 中的密钥。
- 确认 `.env.example` 只包含占位值。
- 确认 JWT 不会出现在 URL、截图、access log 或已提交的 fixture 中。
- 确认主动内容附件不会同源执行，Office 解压限制会拒绝解压炸弹。
- 审查 `SECURITY.md` 和 runtime 安全说明。
- 不要公开 Bot 平台凭证或数据源连接串。

## 5. Smoke 证据

发布 tag 前建议保存：

- Docker quickstart 结果。
- WebUI setup 截图。
- 至少一个 local-first 平台的 Bot Gateway smoke 结果。
- WatchDog 查询截图或 Bot 回复。
- 如果宣传 RD，则保存 RD task AgentOps trace 截图。

## 6. 效果声明

- 使用同一模型、文件、工具和尽可能一致的时间预算，对 AOS 与明确版本的 Codex 做盲测在线 A/B。
- 分开报告答案正确性、证据可追溯性、SQL 执行/口径正确性、追问记忆、断线恢复、延迟和 token 成本。
- 不得把确定性的 `eval-harness` fixture 当成 AOS 已超过 Codex 的证据；它只验证接线与回归契约。
