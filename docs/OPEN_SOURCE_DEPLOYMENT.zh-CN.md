# AOS 开源版从零部署与启动手册

本文面向第一次接触 AOS 的用户，覆盖环境准备、构建、启动、首次初始化、模型配置、Skill/MCP 前置条件、打包、备份、升级和清空数据。

## 1. 部署边界

AOS 自身的平台数据使用 SQLite。MySQL、TiDB、PostgreSQL、ClickHouse、Trino 等仅作为 NL2SQL 可以连接的外部业务数据源，不是 AOS 的安装依赖。

当前发布形态支持：

- 一台机器；
- 一个 AOS 服务进程；
- 一个本地磁盘数据目录；
- 同一实例内的多用户和多租户；
- WebUI 与 API 同端口运行；
- macOS 和主流 Linux 原生部署，或 Docker Compose 部署。

不支持多个 AOS 进程同时访问同一个 SQLite 数据目录，也不要把数据目录放到 NFS、SMB、CIFS 或其他共享网络文件系统。

## 2. 目录与数据

原生部署默认使用：

| 路径 | 用途 | 是否要备份 |
| --- | --- | --- |
| `.env` | 本机密钥和运行配置 | 是，必须保密 |
| `.aos-data/aos.db` | AOS 平台主库 | 是 |
| `.aos-data/nl2sql/embedding-profiles/` | 按租户和 Embedding profile 隔离的 NL2SQL 向量与索引 | 建议 |
| `.aos-data/rd/` | 代码检索向量与索引 | 建议 |
| `.aos-data/` 其他内容 | 上传、运行时、遥测等本地状态 | 建议 |
| `.run/aos/` | PID 和日志 | 否 |

停止 AOS 后备份整个 `.aos-data/`，不要只复制 `aos.db` 而遗漏 WAL 或其他运行文件。

## 3. 推荐路径选择

### 路径 A：AOS Offline 预编译包（推荐）

适合绝大多数用户。macOS、Linux 和 Windows x64 包已经包含 WebUI、Rust 服务、固定版本的
多语言本地 Embedding 模型和 ONNX Runtime；首次启动不会下载模型，也不需要 Rust、Cargo
或前端构建链。Skill、代码仓库和 MCP 能力仍需要 Git、`rg`、`npx` 和 `uvx`。

### 路径 B：Docker Compose

适合不想在宿主机安装 Rust、Node 和 Python 工具链的用户。容器镜像包含 WebUI、Rust 服务、`node/npm/npx`、Python、`uv/uvx`、Git 和 `rg`。

### 路径 C：源码一键启动

适合本地开发、调试和生成当前系统平台的离线发布包。环境脚本支持 macOS、Debian/Ubuntu、Fedora/RHEL 和 Windows PowerShell。

## 4. 获取源码后的安全检查

进入仓库根目录：

```bash
cd aos
```

确认仓库中没有别人遗留的数据或密钥：

```bash
find . -maxdepth 3 -type f \
  \( -name 'aos.db*' -o -name '*.sqlite*' -o -name '.env' \) -print
```

正式发布的源码不应包含 `.env`、SQLite 数据库、会话、运行日志或个人 Token。`.env.example` 只能包含空值和示例值。

## 5. 原生环境准备

### 5.1 所需工具

| 工具 | 最低要求 | 用途 |
| --- | --- | --- |
| Rust/Cargo | 1.85+ | 构建 Rust 后端 |
| Node.js | 20.19+ 或 22.12+ | 构建 WebUI、运行 npm MCP |
| npm/npx | 随 Node 安装 | WebUI 依赖、npm MCP |
| Python | 3.9+ | 辅助脚本、Python MCP |
| uv/uvx | 当前稳定版 | 隔离运行 Python MCP |
| Git | 当前稳定版 | Skill 和代码仓库操作 |
| ripgrep (`rg`) | 当前稳定版 | 代码检索与发布门禁 |
| curl/OpenSSL | 当前稳定版 | 健康检查和密钥生成 |
| C 编译器/pkg-config | Linux 需要 | Rust 原生依赖构建 |

AOS 自带的 Python 脚本只使用标准库，没有必须预装的项目级 `pip` 包。某个 MCP Server 的 Python 包由其 `uvx` 命令独立解析和安装。

### 5.2 只检查，不改机器

```bash
./scripts/setup-environment.sh --check
```

脚本会自动识别目录布局：源码仓库检查构建链和运行链，解压后的预编译发布包只检查运行链，不要求 Rust、Cargo、C 编译器或 `pkg-config`。所有必需项显示 `[ok]` 才算通过。`AOSD_GITHUB_TOKEN` 是可选项，不会因为为空导致检查失败。

### 5.3 自动安装缺失工具

```bash
./scripts/setup-environment.sh --install
```

该命令只在明确传入 `--install` 时安装软件：

- macOS 使用 Homebrew；如果未安装 Homebrew，会给出明确提示；
- Debian/Ubuntu 使用 apt，Node 版本不足时使用 NodeSource 22；
- Fedora/RHEL 使用 dnf；
- Rust 使用官方 rustup；
- uv 使用 Astral 官方安装器；
- Linux 安装过程可能询问 `sudo` 密码。

安装后重新打开终端，再执行一次 `--check`。

## 6. GitHub Token

4 个默认 Skill 仓库都是公开仓库，不提供 Token 也能工作，但 GitHub 对匿名 API 请求的共享 IP 限额很低。建议创建一个最小权限 Token，只授予读取公开仓库所需权限。

不要把 Token 写进源码、命令示例、截图或提交记录。推荐在生成 `.env` 前临时导出：

```bash
export AOSD_GITHUB_TOKEN=your_github_token
./scripts/generate-env.sh
unset AOSD_GITHUB_TOKEN
```

`generate-env.sh` 会把当前环境中的 Token 写入本地 `.env`，但不会在终端输出 Token。默认模式不会覆盖已存在的 `.env`；`generate-env.sh --repair` 只修复缺失、长度错误或仍为公开占位值的三项服务端密钥，其他业务配置保持不变。`aos-start.sh` 会自动执行这项安全修复，因此从旧版 `.env` 升级时不需要先删除整个文件。

如果 `.env` 已经生成，请手工填写其中的空值：

```dotenv
AOSD_GITHUB_TOKEN=
```

`.env` 已被 `.gitignore` 排除。发布前仍要运行密钥扫描，防止历史提交或其他文件泄漏。

### 6.1 首次启动前的 `.env` 核心配置

先执行生成脚本，不要从 README 复制固定密钥：

```bash
./scripts/generate-env.sh
```

脚本会生成 `JWT_SECRET`、`ENCRYPTION_KEY` 和 `TOKEN_ENCRYPTION_KEY`。这三项一旦开始使用就必须随数据一起保留；升级时更换它们会导致登录令牌失效，或数据库中已加密的 API Key、仓库 Token 无法解密。

随后打开当前目录的 `.env`，按部署场景检查下表。空白的可选项可以保留为空：

| 配置项 | 是否必需 | 作用和填写规则 |
| --- | --- | --- |
| `JWT_SECRET` | 必需，脚本生成 | JWT 签名密钥。不得使用公开示例值，升级时不得更换。 |
| `ENCRYPTION_KEY` | 必需，脚本生成 | API Key 等敏感配置的 AES-256-GCM 主密钥，必须恰好 32 字节，升级时不得更换。 |
| `TOKEN_ENCRYPTION_KEY` | 必需，脚本生成 | 代码仓库等 Token 的加密密钥，升级时不得更换。 |
| `BASE_URL` | 必需 | 浏览器实际访问 AOS 的公开地址。本机默认 `http://localhost:3000`；反向代理部署应填写 HTTPS 地址。 |
| `AOS_BIND_HOST` / `AOS_WEB_PORT` | 必需 | 默认 `127.0.0.1` / `3000`。只有在防火墙和 TLS 反向代理后才使用 `0.0.0.0`。 |
| `CORS_ALLOWED_ORIGINS` | 按需 | 前后端同源时留空；独立前端域名时填允许的来源，正式环境不要使用 `*`。 |
| `AOS_ALLOW_PUBLIC_REGISTRATION` | 建议明确填写 | 开源自托管默认 `false`，只允许管理员邀请；公开注册才改为 `true`。 |
| `AOSD_GITHUB_TOKEN` | 推荐 | Skill 市场读取公开 GitHub 仓库时避免匿名限流。只需只读权限。 |
| `SMTP_HOST` / `SMTP_PORT` | 发邀请邮件时必需 | SMTP 服务地址和端口，常见 TLS 端口为 587。 |
| `SMTP_USE_TLS` | 发邀请邮件时必需 | 通常填 `true`；必须与邮件服务商要求一致。 |
| `SMTP_USERNAME` / `SMTP_PASSWORD` | 发邀请邮件时必需 | SMTP 账号及应用专用密码/授权码，不一定是邮箱登录密码。 |
| `SMTP_FROM` | 发邀请邮件时必需 | 发件人，例如 `AOS <noreply@example.com>`；域名需被 SMTP 服务允许。 |
| `AOSD_BUILTIN_SEARCH_*` | 可选 | 内置联网搜索默认无需 Key；只有使用镜像、受控网络或调整超时时才修改。 |
| `AOS_SQLITE_*` | 可选 | 已提供适合单实例的默认值。不要把数据目录放在网络文件系统。 |
| `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | 可选 | 仅作为进程级开发回退。正式多租户应在 WebUI 的“API 密钥”中配置。 |

邮件配置完成后重启 AOS，再到团队邀请页面执行“发送 Email 测试”。测试失败不会撤销邀请记录，页面仍会给出可手工复制的邀请地址。常见失败原因是服务商要求应用专用密码、发件人未验证、端口/TLS 不匹配或云服务器封禁出站 25 端口。

## 7. 原生一键构建与启动

### 7.1 最短流程

```bash
./scripts/aos-start.sh
```

首次执行会自动：

1. 如果缺少产物，调用 `install.sh --release`；
2. 检查完整工具链；预编译包只检查运行工具，源码目录同时检查构建工具；
3. 用锁文件执行 `npm ci`；
4. 完成 TypeScript 检查和 WebUI 构建；
5. 构建带 `full` 特性的 Rust `web-server`；
6. 如果 `.env` 不存在，生成三项随机加密密钥；
7. 如果 `.env` 已存在，保留原配置并自动修复缺失或不安全的服务端密钥；
8. 创建 `.aos-data/`；
9. 源码目录首次缺少固定 Embedding 快照时下载到 `.aos-runtime/models/fastembed` 并校验；
   该缓存与 `.aos-data` 业务数据分离，清空数据不会重复下载；AOS Offline 包不会执行下载；
10. 由 Rust 服务同时托管 `/api`、`/ws` 和 WebUI，不需要另开前端进程；
11. 等待 `/api/v1/setup/check` 健康检查通过后返回。

默认地址：

```text
http://localhost:3000
```

日志和 PID：

```text
.run/aos/web-server.log
.run/aos/web-server.pid
```

### 7.2 显式构建

```bash
./install.sh --release
./scripts/aos-start.sh --no-build
```

开发构建：

```bash
./install.sh --debug
```

### 7.3 前台运行

```bash
./scripts/aos-start.sh --foreground
```

按 `Ctrl+C` 触发优雅停止。后台运行时使用：

```bash
./scripts/aos-stop.sh
```

### 7.4 自定义端口和数据目录

```bash
./scripts/aos-start.sh --host 127.0.0.1 --port 3100 --data-dir .aos-data-local
```

数据目录必须位于当前 AOS 目录中，并使用本地磁盘。远程访问时不要直接裸露服务，应在 TLS 反向代理和防火墙后运行。

## 8. Docker Compose 启动

生成环境文件：

```bash
export AOSD_GITHUB_TOKEN=your_github_token
./scripts/generate-env.sh
unset AOSD_GITHUB_TOKEN
```

构建并启动：

```bash
docker compose up --build -d
docker compose ps
docker compose logs -f server
```

打开 `http://localhost:3000`。默认只绑定 `127.0.0.1`。

停止并保留数据：

```bash
docker compose down
```

永久删除 Docker 数据卷：

```bash
docker compose down -v
```

## 9. 首次初始化向导

空数据库第一次打开任意页面时，WebUI 会请求 `GET /api/v1/setup/check`。如果没有租户和用户，会自动跳到 `/setup`，其他业务 API 在初始化完成前返回 `428 setup_required`。

### 第一步：组织和管理员

填写：

- 组织名称；
- 组织标识，只允许小写字母、数字和连字符；
- 管理员邮箱和姓名；
- 至少 8 位的管理员密码。

提交是一个 SQLite 事务。成功后会同时创建：

- 第一个系统租户；
- 第一个管理员，默认 `danger_full_access`；
- `normal` PM 默认预算；
- 25 条 NL2SQL 默认敏感列脱敏规则；
- 以下 4 个 Skill 市场仓库：

| 仓库 | 默认分支 |
| --- | --- |
| `ComposioHQ/awesome-claude-skills` | `master` |
| `JimLiu/baoyu-skills` | `main` |
| `anthropics/skills` | `main` |
| `cexll/myclaude` | `master` |

并发提交只允许一个请求成功；重复初始化返回冲突，不会重复创建数据。

### 第二步：模型 API Key

向导支持 DeepSeek、OpenAI、Anthropic 和其他 OpenAI 兼容服务。至少填写配置名称、模型 ID 和 API Key；DeepSeek/自定义服务还要填写 API 地址。

保存的聊天 Key 默认覆盖 `chat`、`nl2sql`、`rd`、`pm` 和超级助手/代码 Agent。后端把历史 `agent` 标签规范化存储为 `rd`，读取时仍接受 `agent` 别名，因此不会丢失代码 Agent 能力。API Key 在数据库中加密保存，列表接口只返回尾部提示，不返回明文。

该步骤可以选择“暂时跳过，直接进入 AOS”。跳过后系统仍可进入和浏览，但需要模型的功能会明确提示缺少可用模型。稍后在“系统 -> API 密钥”中添加即可。

模型列表发现、服务商推理参数映射、真实能力验证和 unsupported parameter 降级规则见
[模型能力档案与推理参数](./MODEL_CAPABILITY_PROFILES.zh-CN.md)。

## 10. 首次进入后的检查

按顺序检查：

1. “系统 -> API 密钥”：测试聊天 Key 健康状态；
2. “扩展 -> Skill 市场”：确认 4 个仓库存在，逐一执行扫描；
3. “扩展 -> MCP 服务”：确认 `npx --version` 和 `uvx --version` 可用；
4. “系统 -> 配置管理”：检查公开地址、SQLite 和运行预算；
5. “超级助手”：发送一个简单问题验证真实模型调用；
6. 需要 NL2SQL 时，再到“数分 -> 数据接入”创建外部业务库连接。

### NL2SQL 的 API + 本地 Embedding

AOS Offline 内置 `paraphrase-multilingual-MiniLM-L12-v2` 的固定量化 ONNX 快照，输出 384 维
向量，模型文件约 224 MiB，连同 tokenizer 在磁盘约 257 MiB。服务启动时只校验文件，
首次真正使用本地语义召回时才创建 ONNX session，因此不会因为把模型打进包而在每次
启动时完整加载模型。发布包运行过程中不会访问 Hugging Face 或后台下载模型。

不配置 Embedding API 时，NL2SQL 仍使用本地模型完成 schema、表/列和 SQL 知识语义召回。
配置 `embedding` 类型 API Key 后，AOS 使用以下策略增强效果：

- 每个租户、每个数据源分别维护 API 与 local 绑定；profile 记录 provider、Base URL、
  模型、维度、模型版本和向量签名；
- 各 profile 使用独立 SQLite 文件：
  `.aos-data/nl2sql/embedding-profiles/<tenant-hash>/<profile-id>/embeddings.db`；
- API 查询向量只查询 API 索引，本地查询向量只查询本地索引，维度相同也绝不混用；
- API profile 已建好且健康时优先 API；超时、429 或服务错误触发熔断并切到本地，冷却后
  健康请求成功会自动恢复 API；
- schema 或 SQL 知识变化时双写；API 临时失败时本地结果立即可用，API 补建任务后台重试；
- 修改 provider、Base URL、模型或维度会建立 shadow profile，完成后原子切换；建立期间
  继续使用旧 API profile 或 local profile；
- 只更换 API Key 密文或 Key 记录 ID，且 provider、地址、模型、维度未变时不重建；
- 本地模型升级会创建新 profile，不覆盖旧向量。

Embedding API 必须填写服务真实返回的维度。返回数量不完整、维度不符、响应畸形、超时、
限流和 5xx 都会被视为该 API profile 失败，不会把异常向量写入索引。

### 零配置联网搜索与可选增强

AOS Offline 自带运行时 `WebSearch`，全新安装不需要配置 Brave、Tavily 等搜索 API Key。
内置通用搜索会实时访问 DuckDuckGo、Brave、Bing、Wikipedia、Hacker News Algolia 和
Stack Exchange；天气问题还会使用 Open-Meteo 的地理编码、当前天气和 7 天结构化预报。
因此运行机器仍需具备互联网出口；“离线包”表示依赖和本地 Embedding 已随包提供，不表示
联网功能在断网时仍能访问网页。Open-Meteo 数据按 CC BY 4.0 使用，回答会保留来源链接。
开启联网搜索时，查询词会发送到可达的公共搜索来源；包含内部机密的问题应关闭联网搜索，
或配置受控的 SearXNG/Generic HTTP/Search MCP。

“扩展 -> Search 扩展”中的 Brave API、Tavily、Serper、Exa、SearXNG 和 Generic HTTP，
以及搜索 MCP，都是可选增强。健康扩展优先运行，失败或证据不足时自动进入 AOS 内置搜索，
随后才尝试模型原生搜索、MCP 和本地/RAG。无需为了让普通联网问答可用而配置这些增强项。

受控网络可以通过 `.env` 中的 `AOSD_BUILTIN_SEARCH_*_URL` 将公共来源指向兼容镜像；
不要把需要鉴权的内部地址写进发布包。

## 11. MCP 说明

`npx` 和 `uvx` 是 MCP 启动器，不代表每个 MCP 都已经安装完成：

- npm MCP 通常在第一次运行时由 `npx` 下载；
- Python MCP 通常由 `uvx` 创建隔离环境并下载；
- 个别 MCP 还可能需要 Docker、Java、浏览器或平台账号；
- MCP 的命令、参数和环境变量在“扩展 -> MCP 服务”中独立配置；
- 添加后先执行“测试连接”，再启用并检查工具、资源和 Prompt 列表；
- 服务进程必须能在其 `PATH` 中找到 `npx`/`uvx`。安装工具后应重启 AOS。

## 12. 生成发布包

```bash
./scripts/aos-package.sh
```

输出示例：

```text
dist/aos-offline-0.1.0-darwin-x86_64.tar.gz
```

发布包与构建机器的操作系统和 CPU 架构一致，包含：

- `bin/web-server`；
- `web/` 构建产物；
- 固定版本本地 Embedding 模型与对应平台 ONNX Runtime；
- `.env.example`；
- 启动、停止、环境检查、密钥生成和数据重置脚本；
- 无损升级和失败自动回滚脚本；
- `RELEASE-MANIFEST.sha256` 发布文件完整性清单；
- 安装和完整测试文档；
- License 和 Notice。

打包脚本会复用 `.aos-runtime/models/fastembed` 中已经校验的固定快照，缺少时才下载；随后
复制到发布包，执行一次真实本地向量推理，并逐个验证五个模型文件的 SHA-256。归档会拒绝
包含 `.env`、SQLite 数据、`.run` 或 `.claw`。

解压后直接执行：

```bash
tar -xzf aos-offline-0.1.0-<os>-<arch>.tar.gz
cd aos-offline-0.1.0-<os>-<arch>
./scripts/setup-environment.sh --check
./scripts/aos-start.sh
```

发布包已经包含构建产物，不需要 Rust、Cargo 或 TypeScript；但使用 Skill、代码仓库和 MCP 时仍需要 Git、`rg`、Node/`npx`、Python/`uvx`。

Windows x64 必须在 Windows 上构建：

```powershell
.\scripts\aos-package-windows.ps1
Expand-Archive .\dist\aos-offline-0.1.0-windows-x86_64.zip .\release
cd .\release\aos-offline-0.1.0-windows-x86_64
.\scripts\setup-environment.ps1
.\scripts\aos-start.ps1
```

需要自动安装缺失的 Git、`rg`、Node/`npx`、Python/`uvx` 时执行
`.\scripts\setup-environment.ps1 -Install`。Windows 包已经包含 `.exe`、模型和
`onnxruntime.dll`，启动时不会下载它们。

## 13. 备份、恢复和升级

### 备份

```bash
./scripts/aos-stop.sh
tar -czf aos-backup-$(date +%Y%m%d-%H%M%S).tar.gz .aos-data .env
```

把备份存放到仓库目录之外，并按密钥文件保护。

### 恢复

1. 停止 AOS；
2. 保存当前 `.aos-data` 和 `.env`；
3. 解压同一份备份；
4. 确认文件所有者和权限；
5. 启动 AOS；
6. 登录并检查任务、Key、Skill 和数据源。

### 升级

预编译包不要清空数据，也不要把旧 `.env` 换成新包里的 `.env.example`。把同一操作系统和 CPU 架构的新包解压到旧目录旁边，然后从新包执行：

```bash
cd /path/to/aos-offline-NEW-<os>-<arch>
./scripts/aos-upgrade.sh \
  --target /path/to/aos-offline-OLD-<os>-<arch> \
  --port 3000
```

升级完成后仍从原来的旧安装目录启动和使用；该目录中的程序已替换为新版本。脚本会自动：

1. 验证新包 `RELEASE-MANIFEST.sha256`；
2. 停止旧进程，确保 SQLite WAL 不再变化；
3. 备份完整 `.aos-data` 和 `.env` 到旧目录的 `.aos-backups/upgrade-<UTC时间>/`；
4. 只替换程序、WebUI、模型、运行库、脚本和文档；
5. 原样保留租户、用户、API Key、Bot、Skill、MCP、会话、任务、上传文件、工作区和向量索引；
6. 启动新版本，由服务在监听端口前执行嵌入式 SQLite migration；
7. 等待健康检查；失败时自动恢复旧程序和升级前数据快照，并重新启动旧版本。

Windows PowerShell：

```powershell
cd C:\path\to\aos-offline-NEW-windows-x86_64
.\scripts\aos-upgrade.ps1 `
  -Target C:\path\to\aos-offline-OLD-windows-x86_64 `
  -Port 3000
```

源码部署应先停止服务并备份 `.aos-data`、`.env`，再执行 `git pull`、测试和重新构建。不要在未停止 SQLite 写入时只复制 `aos.db`。

## 14. 清空全部本地运行数据

预发布环境可用：

```bash
./scripts/reset-local-data.sh --all
```

脚本会列出精确目标并要求输入 `RESET`。CI 或明确知道后果时：

```bash
./scripts/reset-local-data.sh --all --yes
```

会删除当前 AOS 目录中的默认数据库、demo/smoke 数据、`.run` 和 Rust `.claw` 运行状态。不会删除源码、`.env`、构建缓存、依赖或文档测试 fixture。下一次启动会重新进入首次初始化向导。

## 15. 常见故障

### 页面打不开

```bash
cat .run/aos/web-server.log
curl -i http://127.0.0.1:3000/api/v1/setup/check
```

确认端口未被占用，`webui/dist/index.html` 存在，且日志中出现监听地址。

### 启动提示另一个实例占用数据目录

AOS 使用 `.aos-data/aos.lock` 阻止同库多进程。先用 `aos-stop.sh` 停止旧进程。不要手工删除锁后并行启动两个实例。

### Skill 扫描超时或 403

检查 `.env` 中的 `AOSD_GITHUB_TOKEN` 是否非空、Token 是否有效、网络是否能访问 GitHub。不要在日志或截图中显示 Token。

### MCP 报 `npx` 或 `uvx` 不存在

```bash
npx --version
uvx --version
./scripts/setup-environment.sh --check
./scripts/aos-stop.sh
./scripts/aos-start.sh
```

工具安装后必须重启 AOS，后台进程才能继承新的 `PATH`。

### API Key 可保存但模型调用失败

检查服务商、Base URL、模型 ID、余额、地区网络和 Key 权限。在“API 密钥”中执行健康测试。OpenAI 兼容地址应指向 API 根路径或完整 `chat/completions` 端点，AOS 会规范化末尾路径。

### SQLite busy

确认只有一个 AOS 进程，数据目录是本地磁盘。默认 `AOS_SQLITE_BUSY_TIMEOUT_MS=10000`、最大连接数为 4；不要通过盲目提高并发掩盖长事务。

## 16. 开源发布前命令

```bash
./scripts/setup-environment.sh --check
./install.sh --release
./scripts/aos-package.sh --skip-build
./scripts/reset-local-data.sh --all --yes
```

随后确认源码目录不存在 `.env`、真实 Token、SQLite 文件、用户会话或运行日志，再按 [完整测试手册](./OPEN_SOURCE_TEST_GUIDE.zh-CN.md) 从空目录执行一次验收。
