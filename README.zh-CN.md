<p align="center">
  <img src="docs/assets/aos-hero.svg" alt="AOS - Autonomous Operations System" width="100%">
</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./docs/OPEN_SOURCE_DEPLOYMENT.zh-CN.md">部署文档</a> ·
  <a href="./LICENSE">MIT License</a>
</p>

<p align="center">
  <a href="./.github/workflows/rust-ci.yml"><img src="https://img.shields.io/badge/CI-GitHub%20Actions%20configured-2ea44f?logo=githubactions&logoColor=white" alt="CI：已配置 GitHub Actions"></a>
  <img src="https://img.shields.io/badge/npm-v0.1.0-CB3837?logo=npm&logoColor=white" alt="npm 版本 0.1.0">
  <img src="https://img.shields.io/badge/Node.js-%3E%3D20.19%20%7C%7C%20%3E%3D22.12-339933?logo=nodedotjs&logoColor=white" alt="Node.js >=20.19 或 >=22.12">
  <img src="https://img.shields.io/badge/License-MIT-0b8f55?logo=opensourceinitiative&logoColor=white" alt="MIT License">
  <img src="https://img.shields.io/badge/Discord-community%20coming%20soon-5865F2?logo=discord&logoColor=white" alt="Discord 社区入口待配置">
</p>

# AOS

AOS（Autonomous Operations System）是一个 Web-first、多租户的 Agent OS。核心入口是**超级助手**，在同一个可恢复会话中整合通用对话、联网检索、持久记忆、深度研究、数据归因、SQL 知识库、文件、Skill、MCP 和代码仓库工作。

AOS 的当前产品形态是 Rust WebServer 加 WebUI。推荐发布 **AOS Offline**：离线包包含 WebUI、服务端、ONNX Runtime 和固定版本的多语言本地 Embedding 模型，启动时不会偷偷下载模型，也不依赖 MySQL 或 Embedding API 才能完成基础语义检索。

## 3 分钟体验

```bash
./scripts/aos-demo-start.sh
```

打开 Dashboard。首次启动会创建本地 SQLite 平台数据库；真实模型回答需要在初始化后进入 **System -> API Keys**，配置至少一个启用的 `chat` API Key。

演示环境包含四类可点击场景：

- **修复前端 Bug**：进入 Code Studio，检查真实文件，生成候选 Diff，执行测试，确认后应用。
- **诊断 ROI 下跌**：提问“为什么昨天印尼 ROI 下跌 10%”，查看 SQL、证据、根因、置信度和后续动作。
- **创建日报**：生成自动化草案，预览、试运行、确认、立即执行并接收通知。
- **询问 WatchDog**：查看运行中、卡住、失败和取消中的 AgentOps 任务。

演示不需要企业数据库，使用本地 SQLite、内置回退证据和演示提示词；真实任务使用同一套 AgentOps、Trace 和恢复机制。

## 仓库结构

- `rust/`：Rust workspace、WebServer 和核心服务 crate。
- `webui/`：React + Vite Web UI。
- `docs/`：架构、部署、测试手册、SQL 辅助说明和设计记录。
- `docker-compose.yml`：首次部署的 Docker 栈。

## Docker 快速开始

```bash
./scripts/generate-env.sh
docker compose up --build
```

打开 `http://localhost:3000`。新数据库会自动进入 setup 流程，用于创建第一个租户和管理员。

完成 setup 后，进入 **System -> API Keys** 添加一个启用的 `chat` 模型 Key。Super Assistant、深度研究和其他聊天模型回退会按优先级共享使用它，不需要为每个菜单重复录入。

## 不使用 Docker 的快速开始

```bash
./scripts/setup-environment.sh --check
./scripts/setup-environment.sh --install

# 可选：提高 Skill 市场的 GitHub 请求限额
export AOSD_GITHUB_TOKEN=your_token
./scripts/aos-start.sh
unset AOSD_GITHUB_TOKEN
```

打开 `http://localhost:3000`。发布服务会在一个进程中同时提供 WebUI 和 API，平台状态默认存储在 `.aos-data/`。

```bash
./scripts/aos-stop.sh
./scripts/reset-local-data.sh --all
./scripts/aos-package.sh
```

## AOS Offline

离线包命名为 `aos-offline-<version>-<os>-<arch>.tar.gz`。Windows x64 包在 Windows 上使用 `scripts/aos-package-windows.ps1` 生成 ZIP。

内置本地 Embedding profile 不依赖 API；配置 Embedding API 后优先使用 API profile，API 超时、限流或异常时切换到隔离的本地索引。API 索引和本地索引不混用。

升级时请把新包解压到旧安装旁边，再从新包执行：

```bash
cd /path/to/aos-offline-NEW-<os>-<arch>
./scripts/aos-upgrade.sh --target /path/to/aos-offline-OLD-<os>-<arch> --port 3000
```

升级脚本会校验 release manifest，停止旧服务，备份完整 `.aos-data/` 与 `.env`，只替换程序文件；如果新服务 readiness 失败，会自动恢复旧版本和升级前数据。Windows 使用 `aos-upgrade.ps1`。

## 主要能力

- **超级助手**：统一会话、工具调用、联网搜索、记忆、文件、Skill、MCP 和任务恢复。
- **深度研究**：多源证据、冲突处理、阶段进度、质量门和超时保留正文。
- **数据分析**：数据源接入、NL2SQL、SQL 知识库、语义召回、数据归因和下钻。
- **研发方案**：先生成可审查的核心设计，确认后逐步生成代码研发方案和 Task，可修改、下载并交给外部 Code Agent。
- **AgentOps / WatchDog**：任务状态、恢复、取消、审计、值守和移动端查询。
- **扩展**：Skill 市场、手动上传 Skill、MCP Server 和 Hook。
- **Bot 网关**：将超级助手和 WatchDog 接入飞书、钉钉、企业微信、Slack、Discord、Telegram、WhatsApp 和通用 Webhook。

## Bot 网关

已实现的平台适配器包括：

- 钉钉：Stream 入站和机器人 Webhook/签名出站。
- 飞书/Lark：本地长连接事件入站，自定义机器人 Webhook 或 OpenAPI 出站。
- 企业微信：本地 AI Bot WebSocket 入站，群机器人 Webhook 出站。
- Slack：Socket Mode 入站，Incoming Webhook 或 Bot Token 出站。
- Discord：Gateway WebSocket 入站，Webhook 或 Bot Token 出站。
- Telegram：Polling 入站，`sendMessage` 出站。
- WhatsApp：Cloud API Webhook 入站，需要公网 Webhook 或 relay，不提供官方本地轮询模式。
- Generic Webhook：自定义中转适配器。

本地优先配置说明：

- 飞书/Lark：创建企业内部应用，开启长连接事件订阅和消息接收权限，再在 AOS 创建 channel 并填写 App ID/App Secret。
- 钉钉：配置 Stream 或机器人 Webhook 与签名参数。
- Slack：开启 Socket Mode，创建具有 `connections:write` 的 App-Level Token。
- Discord：创建 Bot，并在需要读取原始消息时开启 Message Content Intent。
- WhatsApp：准备公网回调地址或 relay；本地开发需要 tunnel。

## AgentOps / WatchDog

AgentOps 是 AOS Agent 任务的控制平面。Super Assistant、Bot、研发方案、深度研究、超级对抗和 NL2SQL 可以写入共享的任务时间线与事件。

WatchDog 可用于：

- 查询运行中、等待中、失败、卡住和取消中的任务；
- 通过自然语言询问任务进度、耗时和失败原因；
- 从 WebUI 或 Bot 取消任务、恢复任务和接收完成/失败通知；
- 查看任务 Trace、工具调用和恢复记录。

Bot 移动端建议使用统一入口 `aos_router`，再按需绑定 `watchdog`、`pm_assistant`、`nl2sql` 等能力。

相关文档：

- [AgentOps / WatchDog 设计](./docs/AGENTOPS_WATCHDOG_DESIGN.md)
- [Bot 能力契约](./docs/BOT_CAPABILITY_CONTRACT.md)
- [WatchDog 运行手册](./docs/WATCHDOG_RUNBOOK.md)
- [Bot 网关企业配置](./docs/BOT_GATEWAY_ENTERPRISE.md)
- [Bot 平台冒烟测试](./docs/BOT_PLATFORM_SMOKE.md)

## 开发命令

```bash
# Rust 后端
cd rust
./scripts/dev_web_server.sh check
cargo test --workspace --all-features

# WebUI
cd ../webui
npm install
npm run dev
npm run typecheck
npm run build:ci
npm run lint
```

完整测试手册：

- [开源部署手册](./docs/OPEN_SOURCE_DEPLOYMENT.zh-CN.md)
- [完整功能测试手册](./docs/OPEN_SOURCE_TEST_GUIDE.zh-CN.md)
- [研发方案设计手册](./docs/AOS_ENGINEERING_DESIGN_CENTER.zh-CN.md)
- [Bot 网关完整测试手册](./docs/evals/BOT_GATEWAY_COMPLETE_TEST_GUIDE.zh-CN.md)
- [数据归因完整测试手册](./docs/evals/DATA_ATTRIBUTION_COMPLETE_TEST_GUIDE.zh-CN.md)
- [NL2SQL 高级配置测试手册](./docs/evals/NL2SQL_ADVANCED_CONFIGURATION_TEST_GUIDE.zh-CN.md)

## 文档与社区

- [使用说明](./USAGE.md)
- [安装说明](./docs/INSTALL.md)
- [架构说明](./docs/ARCHITECTURE.md)
- [贡献指南](./CONTRIBUTING.zh-CN.md)
- [安全政策](./SECURITY.zh-CN.md)
- [社区行为准则](./CODE_OF_CONDUCT.zh-CN.md)
- [开源发布清单](./OPEN_SOURCE_RELEASE_CHECKLIST.zh-CN.md)

Discord badge 已预留，但当前仓库没有配置公开 Discord 邀请地址；在发布组织的 Discord 建立后，应将 badge 链接替换为真实邀请链接，不使用虚假在线人数或无效 URL。

## 许可证与声明

AOS 使用 MIT License 发布。包括项目初始源码来源在内的第三方版权和许可声明统一保存在 [`NOTICE.md`](./NOTICE.md) 与 [`LICENSE`](./LICENSE) 中。

仓库中的评估 fixture 用于验证 wiring 和回归契约，不等同于 AOS 一定优于其他 Agent 产品的经验结论。正式发布前，应使用相同的盲测案例评估 grounding、上下文恢复、恢复能力、延迟和成本。
