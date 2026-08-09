# 贡献指南

感谢你参与 AOS。AOS 是一个 Web-first Agent OS，包含 Rust 后端、React Web
UI、AgentOps/WatchDog、Bot 网关、研发工作流和数据探索能力。

## 开始之前

- 先阅读 `README.md`、`docs/ARCHITECTURE.md` 以及相关 `docs/` runbook。
- 保持改动聚焦。不要夹带无关重构、格式化噪音和本地环境文件。
- 不要提交 `.env`、运行数据、本地 workspace、API Key、Token、日志或构建产物。
- 优先复用现有架构和 helper API，不要为了局部问题随意新增抽象。

## 开发环境

后端：

```bash
cd rust
AOS_WEB_SERVER_FEATURES=full ./scripts/dev_web_server.sh check
```

前端：

```bash
cd webui
npm install
npm run typecheck
npm run i18n:check
npm run build
```

## 必要检查

小改动跑相关检查；大改动建议跑：

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

## PR 要求

一个合格 PR 应包含：

- 改了什么，为什么改。
- UI/Bot/AgentOps 改动的截图或日志。
- 验证命令和结果。
- 兼容性或迁移说明。
- 已知限制和后续事项。

## 代码风格

- Rust：生产路径不要 panic，返回结构化错误。
- 前端：所有用户可见文案必须走 i18n。
- Bot/AgentOps：长任务必须写任务事件和清晰失败原因。
- Runtime：不要记录明文密钥；大输出应写 artifact 或 preview。

