# AOS 开源发布全功能测试手册

本文是从空数据开始的发布验收清单。测试范围以当前 WebUI 导航、路由和 Rust API 为准，覆盖首次启动、26 个可见菜单、辅助页面、权限、SQLite 单实例约束、Skill/MCP、NL2SQL 外部数据源、代码研发、Bot、超级助手和发布包。

## 1. 执行规则

每个用例记录：

| 字段 | 内容 |
| --- | --- |
| 结果 | `PASS` / `FAIL` / `BLOCKED` |
| 时间 | ISO 8601 时间 |
| 执行人 | 姓名或账号 |
| 版本 | commit、tag 或发布包名 |
| 证据 | 截图、响应、日志或任务 ID |
| 缺陷 | issue 链接和严重级别 |

发布条件：

- P0/P1 缺陷为 0；
- 本文所有“发布阻断”用例通过；
- 允许依赖外部平台的用例因无账号标记 `BLOCKED`，但对应表单校验、错误处理和文档必须通过；
- 任何真实密钥不得出现在截图、日志、数据库导出或报告中；
- 失败用例修复后，重跑当前模块、相邻模块和自动化基线。

## 2. 测试环境

准备：

- 一台 macOS、Linux 或 Windows x64 机器；
- 本地磁盘不少于 20 GB；
- 一个只读公开仓库 GitHub Token，可选但推荐；
- 一个真实聊天模型 API Key；
- 一个用于代码测试的公开或临时 Git 仓库；
- 一个外部 MySQL/TiDB/PostgreSQL/ClickHouse/Trino 测试库，用于 NL2SQL；
- 可选的 Search Provider、Bot 平台和 GitLab/Jira/Sentry 测试账号。

真实模型发布基线建议：

```text
Provider: custom
Base URL: https://api.deepseek.com/v1
Model: deepseek-v4-pro
Model type: chat
Scenarios: chat, nl2sql, rd, pm, agent
```

API Key 只在首次向导或“API 密钥”页面输入，不写入本文和源码。

## 3. 自动化基线

### T-AUTO-001 环境门禁（发布阻断）

```bash
./scripts/setup-environment.sh --check
```

- [ ] Rust、Cargo、Node、npm、`npx`、Python、`uv`、`uvx`、Git、`rg`、curl、OpenSSL 均为 `[ok]`。
- [ ] 在解压后的预编译发布包中自动识别 runtime 布局，不要求 Rust、Cargo、C 编译器或 `pkg-config`。
- [ ] 没有真实 GitHub Token 时只显示可选警告，不误报失败。
- [ ] 临时从 `PATH` 隐藏 `uvx` 后，脚本非零退出并准确指出缺失项。

### T-AUTO-002 WebUI（发布阻断）

```bash
cd webui
npm ci
npm run typecheck
npm test
npm run i18n:check
npm run build
```

- [ ] TypeScript 0 错误。
- [ ] Vitest 0 失败。
- [ ] 中英文翻译均 0 missing。
- [ ] `dist/index.html` 和静态资源存在。

### T-AUTO-003 Rust（发布阻断）

```bash
cd rust
cargo fmt --all --check
cargo test --workspace --all-features
```

- [ ] workspace 全部测试通过。
- [ ] `web-server` 的租户 seed 测试验证 25 条脱敏规则、1 个默认预算和 4 个 Skill 仓库。
- [ ] 没有需要 MySQL 才能启动的测试或平台 migration。

### T-AUTO-004 SQLite/NL2SQL 边界（发布阻断）

```bash
./scripts/check-platform-sqlite-boundary.sh
```

- [ ] AOS 平台状态只使用 SQLite。
- [ ] MySQL/PostgreSQL/ClickHouse/Trino 代码只位于 NL2SQL 外部数据源连接边界。
- [ ] NL2SQL 的 MySQL/TiDB 选项仍存在。

### T-AUTO-005 脚本语法

```bash
bash -n install.sh scripts/*.sh
```

- [ ] 所有脚本语法通过。
- [ ] 所有需要直接执行的脚本具有可执行权限。

## 4. 空数据安装、打包和启动

### T-BOOT-001 清空运行数据（发布阻断）

```bash
./scripts/reset-local-data.sh --all --yes
```

- [ ] `.aos-data`、demo/smoke 数据、`.run` 和 `rust/.claw` 被删除。
- [ ] `.env`、源码、文档、依赖和测试 fixture 未被删除。
- [ ] 第二次执行提示没有运行数据，且成功退出。
- [ ] 传入仓库外路径时脚本拒绝执行。

### T-BOOT-002 生成配置

```bash
mv .env .env.saved 2>/dev/null || true
export AOSD_GITHUB_TOKEN=the_test_token
./scripts/generate-env.sh
unset AOSD_GITHUB_TOKEN
```

- [ ] `.env` 权限仅当前用户可读写。
- [ ] 三个加密密钥均非空且互不相同。
- [ ] GitHub Token 被写入但没有在终端回显。
- [ ] 再次执行拒绝覆盖已有 `.env`。
- [ ] 把旧 `.env` 的 `JWT_SECRET` 改成公开占位值并删除另外两项密钥后执行 `./scripts/aos-start.sh`，脚本只修复三项密钥，保留端口、模型、GitHub Token 等其他配置。
- [ ] 测试结束恢复原配置或删除临时配置。

### T-BOOT-003 一键构建

```bash
./install.sh --release
```

- [ ] 使用 `npm ci`，不修改锁文件。
- [ ] 生成 `rust/target/release/web-server`。
- [ ] `web-server --help` 包含 `--data-dir` 和 `--web-dir`。
- [ ] 生成 `webui/dist/index.html`。
- [ ] 安装后的 smoke、前端测试、租户 seed 和 SQLite 边界门禁通过。

### T-BOOT-004 一键启动与停止（发布阻断）

```bash
./scripts/aos-start.sh --no-build
curl -i http://127.0.0.1:3000/api/v1/setup/check
curl -i http://127.0.0.1:3000/
./scripts/aos-stop.sh
```

- [ ] 启动脚本等待健康检查后才报告 ready。
- [ ] 启动前自动执行环境门禁，缺失 `npx` 或 `uvx` 时不启动并给出安装命令。
- [ ] setup check 返回 200 和 `initialized:false`。
- [ ] `/` 返回 WebUI HTML，不是 404/428。
- [ ] `/setup` 刷新仍返回 WebUI，SPA fallback 有效。
- [ ] 第二次启动不创建第二个进程。
- [ ] 停止后端口释放，SQLite 完成 clean shutdown。

### T-BOOT-005 发布包（发布阻断）

```bash
./scripts/aos-package.sh --skip-build
tar -tzf dist/aos-offline-*.tar.gz
```

- [ ] 包含 `bin/web-server`、`web/`、固定模型、ONNX Runtime、脚本、`.env.example`、文档、AOS 许可证和模型 Apache-2.0 归属文件。
- [ ] 不包含 `.env`、`aos.db`、WAL、会话、日志、`.run`、`.claw`、`node_modules` 或 Rust target。
- [ ] 解压到新临时目录后，`scripts/aos-start.sh` 能从空数据启动。
- [ ] macOS 下从 `/tmp`（实际指向 `/private/tmp`）或其他符号链接路径进入发行包时，启动、停止和重置脚本不会误报数据目录越界。
- [ ] 断网启动不访问 Hugging Face；删除或篡改任一模型文件时启动明确拒绝。
- [ ] 打包阶段真实生成 384 维向量，而不只是检查 ONNX 文件存在。
- [ ] Windows x64 在 Windows runner 执行 `aos-package-windows.ps1`，解压后由 `aos-start.ps1` 从空数据启动；包名为 `aos-offline-<version>-windows-x86_64.zip`。

## 5. 首次初始化与认证

### T-SETUP-001 自动跳转（发布阻断）

1. 从空 `.aos-data` 启动。
2. 打开 `/`、`/dashboard`、`/keys`。

- [ ] 均自动进入 `/setup`。
- [ ] 未初始化时普通业务 API 返回 `428 setup_required`。
- [ ] 初始化页面没有白屏、循环跳转或需要手工刷新。

### T-SETUP-002 表单校验

- [ ] 空组织名不能提交。
- [ ] slug 含大写、空格、下划线或中文时提示错误。
- [ ] 邮箱格式错误时提示错误。
- [ ] 密码少于 8 位不能提交。
- [ ] 两次密码不一致不能提交。

### T-SETUP-003 创建首租户（发布阻断）

1. 输入有效组织和管理员信息。
2. 同时在两个浏览器窗口提交。

- [ ] 只有一个请求成功，另一个返回冲突。
- [ ] 成功后进入第二步“配置模型”，不直接丢到登录页。
- [ ] 当前管理员已经登录。
- [ ] SQLite 中只有 1 个系统租户和 1 个管理员。
- [ ] 有 1 个 `normal` 默认 PM 预算。
- [ ] 有 25 条默认 NL2SQL 脱敏规则。
- [ ] Skill 仓库恰好为 `ComposioHQ/awesome-claude-skills`、`JimLiu/baoyu-skills`、`anthropics/skills`、`cexll/myclaude`。

### T-SETUP-004 API Key 可跳过（发布阻断）

1. 重新清库并完成第一步。
2. 点击“暂时跳过”。

- [ ] 直接进入仪表盘。
- [ ] 没有 Key 的页面仍可浏览。
- [ ] 需要模型的请求返回可理解的配置提示，不发生 500。
- [ ] 稍后可从“API 密钥”添加 Key。

### T-SETUP-005 向导保存 DeepSeek Key（发布阻断）

1. 选择 DeepSeek。
2. 填 `https://api.deepseek.com/v1`、`deepseek-v4-pro` 和真实 Key。
3. 保存。

- [ ] 保存后进入仪表盘。
- [ ] Key 列表仅显示掩码提示，不回显明文。
- [ ] 场景覆盖 chat、nl2sql、rd、pm 和超级助手/代码 Agent；数据库允许把 `agent` 规范化为 `rd`，功能筛选仍兼容 `agent` 别名。
- [ ] 密钥健康检查成功；失败时错误不含完整 Key。
- [ ] 先在无 Key 状态触发一次模型配置错误，再保存 Key，不重启服务即可创建 Agent/超级助手会话并真实调用模型。

### T-AUTH-001 登录会话

- [ ] 未登录直接打开 `/login` 正常显示登录页，不白屏、不循环跳转。
- [ ] 正确账号密码登录成功。
- [ ] 错误密码失败且不泄露用户是否存在。
- [ ] 刷新页面保持登录。
- [ ] 退出后 Token 被清除，受保护页回登录页。
- [ ] 修改密码后旧密码失效，新密码可登录。
- [ ] 伪造/过期 Token 返回 401 并清理前端会话。
- [ ] 已登录用户再次打开 `/login` 自动回到仪表盘。

### T-AUTH-002 邀请注册 `/invite`

1. 管理员在 `/users` 分别为 admin、developer 和 viewer 生成邀请链接。
2. 在未登录的无痕窗口打开每个链接并设置符合规则的密码。

- [ ] 有效邀请能打开 `/invite`、显示被邀请账号并成功完成注册。
- [ ] 注册后角色、菜单权限和租户与邀请内容一致，不能由浏览器请求篡改。
- [ ] 同一个邀请 Token 只能成功一次；重复使用、伪造、过期和已撤销 Token 均安全失败。
- [ ] 无 Token 或损坏 Token 不白屏、不泄露租户和用户信息，并提供返回登录页的入口。
- [ ] 已登录用户打开其他人的邀请链接时不能覆盖当前账号或跨租户接受邀请。
- [ ] SMTP 未配置时复制邀请链接可用；已配置时邮件中的链接与页面流程一致。

## 6. 全局导航和公共功能

### T-GLOBAL-001 导航与响应式（发布阻断）

- [ ] 桌面侧栏依次显示工作区、智聊、产运、研发、数分、扩展、系统分组。
- [ ] 26 个可见菜单都能打开且路由与高亮一致。
- [ ] 折叠/展开侧栏不遮挡页面。
- [ ] 移动宽度下导航可打开、选择和关闭，无文本重叠。
- [ ] 中英文切换后页面和菜单即时更新。
- [ ] 浏览器前进/后退正确恢复页面。
- [ ] 任意 SPA 路由直接刷新不 404。

### T-GLOBAL-002 命令面板、通知和租户

- [ ] 命令面板可搜索并跳转到有权限的菜单。
- [ ] 通知抽屉可加载、标记已读、全部已读和删除。
- [ ] 有多个租户时可切换，Token/租户头同步更新，缓存数据不串租户。
- [ ] 用户菜单可查看账号、修改密码和退出。

### T-GLOBAL-003 权限

创建 admin、developer、viewer 和自定义菜单权限用户：

- [ ] 无菜单权限时菜单隐藏，直接访问路由显示无权限。
- [ ] read 权限只能查看，write/delete 按钮禁用或隐藏。
- [ ] 后端同步返回 403，不能只靠前端拦截。
- [ ] 租户 A 的资源 ID 在租户 B 下不能读取、更新或删除。

## 7. 工作区与智聊菜单

### T-MENU-001 仪表盘 `/dashboard`

- [ ] 总览、趋势、模型用量、模块用量、任务/AgentOps 和配置统计加载。
- [ ] 时间范围或筛选切换会刷新数据。
- [ ] 创建、编辑、启停、删除用量告警；阈值非法时拒绝。
- [ ] 4 个 Wow Demo 卡片可启动并跳到正确工作区。
- [ ] 无数据为真实空态；接口失败有重试，不展示伪造生产数据。

### T-MENU-002 超级助手 `/super-assistant`（发布阻断）

数据归因的效果、收敛、证据、权限和失败恢复专项验收见 [数据归因完整测试手册](evals/DATA_ATTRIBUTION_COMPLETE_TEST_GUIDE.zh-CN.md)。

基础会话：

- [ ] 新建、重命名、置顶、收藏、搜索和删除会话。
- [ ] 普通问答流式输出，停止生成有效，重新发送不重复消息。
- [ ] Markdown、代码块、表格、引用和超长内容正确渲染。
- [ ] 刷新后历史、当前会话和未完成状态可恢复。
- [ ] 上下文状态显示 Token、压缩、记忆项；手工压缩后仍能回答前文事实。

路由场景：

- [ ] 普通知识问题走 chat，答案不冒充联网。
- [ ] “查询今天的公开事实并附来源”走 Search/模型原生搜索/MCP 降级链，返回可点击 URL。
- [ ] 深度研究问题展示计划、阶段、证据、来源、质量门和最终报告。
- [ ] 业务数据问题在选定数据源后走 NL2SQL，展示 SQL、表/列证据、结果和审计。
- [ ] 归因问题输出假设、证据、置信度和下一步，不伪造查询结果。
- [ ] 代码问题能路由到研发能力或给出明确跳转。
- [ ] 对抗/多角色问题可启动对应任务并在指挥中心追踪。

附件和记忆：

- [ ] 上传 txt、md、csv、pdf 和图片；类型/大小超限正确拒绝。
- [ ] 文本附件内容可被引用，图片能力不可用时明确降级。
- [ ] 同会话追问能引用前文和附件；新会话不错误泄漏私有上下文。
- [ ] 工具调用、证据和最终回答中的秘密字段被脱敏。

异常：

- [ ] 模型 401、429、超时、断流和 5xx 均有可恢复错误。
- [ ] 多 Key 时主 Key 失败可按优先级 failover，并记录实际 provider/model。
- [ ] 取消后不再继续写最终答案；刷新可看到终态。

### T-MENU-003 指挥中心 `/tasks`

- [ ] “我的任务”按状态、来源和搜索筛选，分页/刷新正确。
- [ ] 选中任务显示时间线、执行图、尝试、产物、资源和命令审计。
- [ ] 打开原会话、关注/取消关注正常。
- [ ] 对运行任务执行取消；对失败/取消任务执行重试；需要审批的任务可批准/拒绝。
- [ ] 产物内容可打开，越权或不存在的产物不能读取。
- [ ] Bot 身份配对码可创建、复制、撤销；过期码不可用。
- [ ] Presence 设置、Watch Rule 创建/编辑/删除、待决动作处理正常。
- [ ] 通知投递记录可查看，失败投递可重放且幂等。

## 8. 产运菜单

### T-MENU-004 任务中心 `/operations/tasks`

- [ ] 任务摘要、任务列表、分页和状态筛选正确。
- [ ] 新建、编辑、启停、删除 mission；cron 预览与时区正确。
- [ ] “立即运行”创建真实任务，状态 queued -> running -> terminal。
- [ ] 运行详情显示阶段、耗时、尝试、错误和回复。
- [ ] 运行中任务可取消，失败/取消任务可恢复。
- [ ] 最终回复可预览、下载和生成分享页，超长内容按规则截断。

### T-MENU-005 素材工坊 `/operations/materials`

- [ ] 素材摘要、线程、版本和状态筛选正确。
- [ ] 创建文本、图片、音乐任务；模型列表只显示匹配类型的 Key。
- [ ] 文本结果可查看和下载。
- [ ] 图片可预览、下载、带参考图继续生成。
- [ ] 音乐按歌词草稿、编曲计划、生成阶段推进并播放/下载。
- [ ] PPT 线程能展示大纲、页面蓝图、视觉计划和最终 HTML/PPT 资产。
- [ ] 从某版本继续生成产生新迭代，不覆盖历史资产。
- [ ] 分享、导出和删除任务正常，删除确认有效。

### T-MENU-006 治理中心 `/operations/governance`

- [ ] 概览显示运行风险、预算、SLO、质量、Search 和知识覆盖摘要。
- [ ] “运行”页显示运行洞察、停滞/排队任务和耗时。
- [ ] “质量”页显示质量门、失败分类和知识覆盖警告。
- [ ] “联网/Provider”页显示 Search Doctor、Provider 健康和通道状态。
- [ ] “高级诊断”显示路由学习、质量/SLO 明细和模型维度。
- [ ] 切换默认 PM 预算配置成功且只有一个默认项。
- [ ] 无 Search Provider 或服务异常时给出降级路径，不误报健康。

## 9. 研发菜单

### T-MENU-007 代码仓库 `/projects`

- [ ] 添加 HTTPS/SSH 仓库，分支列表加载，非法 URL 拒绝。
- [ ] Token/凭据不回显，不出现在日志和列表响应。
- [ ] 同步仓库后状态、分支和时间更新。
- [ ] 目录树和文件内容可浏览，路径穿越被拒绝。
- [ ] 编辑仓库名称、分支、测试/构建命令后生效。
- [ ] 删除仓库需要确认，不删除仓库目录之外的文件。

### T-MENU-008 代码开发 `/agent`（发布阻断）

- [ ] 选择仓库、模型、Coding Agent、Workflow 和集成。
- [ ] 普通任务与深度模式意图路由正确，创建后立即出现在线程列表。
- [ ] 运行阶段和事件实时更新；刷新后从 API 恢复。
- [ ] Workbench 文件树、文件读取、搜索、引用和上下文证据正确。
- [ ] 候选 Diff 显示文件/行/hunk；可单 hunk 或全部批准应用。
- [ ] 未批准前不修改目标工作区；应用后 Git diff 与页面一致。
- [ ] 运行测试记录命令、退出码、stdout/stderr 和耗时。
- [ ] 失败后自动修复不超过配置次数；取消、重试正常。
- [ ] 回滚只撤销当前任务已应用变更，不破坏用户预先存在的修改。
- [ ] 下载报告、分享、生成 PR draft 和发布集成均有真实结果或明确错误。
- [ ] Runtime artifact、Token 诊断、review/architecture pass 可追踪。

### T-MENU-009 规格驱动 `/rd/specs`

- [ ] 创建规格，选择仓库和模型。
- [ ] 生成/编辑/批准需求规格。
- [ ] 生成/编辑/批准设计。
- [ ] 生成/编辑/批准任务拆分。
- [ ] 从单个任务或全部任务创建研发任务。
- [ ] 事件时间线和最终报告可查看，状态不能越级。

### T-MENU-010 代码任务 `/pipeline`

- [ ] 仓库和状态筛选正确，任务列表与代码开发一致。
- [ ] 详情展示变更、事件、测试和 Token 诊断。
- [ ] Apply、Rollback、Run Test 的权限、确认和终态正确。
- [ ] 同一任务从代码开发和代码任务页看到一致状态。

### T-MENU-011 质量快照 `/rd/quality`

- [ ] 按仓库和 7/30/90 天切换。
- [ ] 成功率、测试通过率、失败/运行任务、待批 Diff、平均耗时正确。
- [ ] 检索缓存、Embedding、摘要、Symbol、Import、依赖图和任务记忆指标正确。
- [ ] Runtime/Planner/Embedding/输入输出/缓存 Token 汇总与明细一致。
- [ ] read/grep/glob 工具次数和重复目标指标可由真实任务变化验证。

### T-MENU-012 研发配置 `/rd/agents`

- [ ] 市场搜索 Agent/Workflow，按类型筛选并安装。
- [ ] Coding Agent 创建、编辑、启停、删除；模型和提示词保存。
- [ ] Workflow 创建、步骤排序、编辑、删除；引用不存在 Agent 时拒绝。
- [ ] 团队规范按仓库绑定，创建、编辑、启停、删除。
- [ ] GitLab/Jira/Sentry/自定义集成创建、测试、编辑、删除；秘密不回显。
- [ ] 安装市场项后出现在对应已安装列表且不重复。

## 10. 数分菜单

### T-MENU-013 数据接入 `/datasources`（发布阻断）

- [ ] 创建 MySQL、TiDB、PostgreSQL、ClickHouse、Trino、Presto 数据源。
- [ ] 连接测试成功/失败信息准确，密码不回显。
- [ ] 可见性 tenant/private 生效，其他用户不能读取 private 连接。
- [ ] 编辑连接不填写新密码时保留原密码；修改类型被拒绝。
- [ ] Schema 发现、单表发现、Trino catalog/schema 发现正常。
- [ ] 可导入 SQL DDL，也可手工新增/编辑/删除表和列。
- [ ] Schema 管理展示表、列、类型、nullable 和描述。
- [ ] 语义索引可编辑数据源/表/列描述，刷新任务有真实状态。
- [ ] 删除连接后相关缓存/语义按约束处理，不影响其他租户。
- [ ] 批量导入/导出不包含明文密码。

### T-MENU-014 数据探索 `/nl2sql`（发布阻断）

准备至少包含订单、用户、国家、日期、金额和敏感列的外部测试库。

- [ ] 选择数据源、加载 schema 和语义状态。
- [ ] 简单筛选、聚合、分组、排序、Top N、同比/环比问题生成正确方言 SQL。
- [ ] MySQL/TiDB、PostgreSQL、ClickHouse、Trino 至少各做一次 SQL 生成；已具备环境的类型做真实执行。
- [ ] 澄清问题能继续原会话，时间/指标歧义不会静默猜测。
- [ ] 仅生成 SQL 和允许执行两种模式权限正确。
- [ ] 结果分页、列、空值、数值、时间和大结果截断正确。
- [ ] 结果缓存命中和过期正确，取消异步查询停止后续执行。
- [ ] 点赞/点踩/文本反馈保存。
- [ ] 保存视图、编辑、运行和删除视图。
- [ ] 会话列表、恢复和删除正常。
- [ ] Agent 模式能分解、路由和合并；多数据源关系不足时拒绝错误拼接。
- [ ] 默认脱敏规则遮盖 password/token/api_key/card 等列，原值不进入模型和响应。
- [ ] 查询策略拒绝无权限表/列和危险 SQL；AOS 不执行写 SQL。

### T-NL2SQL-EMBED-001 API + local 双 profile（发布阻断）

1. 不配置 Embedding API，刷新一个含中文和英文描述的数据源并执行语义问题。
2. 新增健康的 `embedding` Key，填写模型实际维度，等待 profile 状态变为 ready。
3. 依次模拟 API 超时、429、503、畸形 JSON、返回数量不足和维度错误。
4. 恢复 API，等待熔断冷却后再次查询。

- [ ] 无 Embedding API 时使用内置 local profile，语义索引和检索可用，不显示“不可用”。
- [ ] 每个租户、数据源都有 API/local 两条绑定，向量文件位于各自 profile 目录。
- [ ] API 正常且索引 ready 时查询只命中 API profile；API 与 local 向量从不进入同一文件或同一 ANN。
- [ ] API 异常时结果降级到 local，不返回 500；连续失败开启熔断，恢复健康请求后自动切回 API。
- [ ] schema 和 SQL 知识新增/修改同时写入两套 profile；API 失败时创建可观察的补建任务并重试。
- [ ] 修改 provider、Base URL、模型或维度会创建新 profile；shadow 索引完成前旧 profile/local 可用，完成后原子切换。
- [ ] 只更换 API Key、且 provider/地址/模型/维度不变时 profile ID 与索引文件不变，不触发重建。
- [ ] 本地模型版本变化会创建新 local profile，旧向量不被覆盖。
- [ ] 租户 A 无法读取租户 B 的 profile 状态、向量或补建任务。

### T-MENU-015 知识文库 `/nl2sql/sql-knowledge`

- [ ] 创建、编辑、启停、删除知识空间。
- [ ] 绑定数据源和访问角色。
- [ ] 文件/文件夹上传支持 SQL、Markdown、CSV 等允许类型。
- [ ] 文件预览、读取、编辑、保存和删除。
- [ ] 文本/语义检索返回真实片段和来源。
- [ ] 未配置 Embedding API 时明确显示本地语义召回可用；配置 API 后显示增强与 profile 状态。
- [ ] 超限、重复、非法路径和恶意文件名被安全处理。

### T-MENU-016 高级配置 `/nl2sql/management`

完整说明、运行时作用和逐项测试步骤见 [NL2SQL 高级配置说明与完整测试手册](evals/NL2SQL_ADVANCED_CONFIGURATION_TEST_GUIDE.zh-CN.md)。逐一测试 10 个页签：

- [ ] 业务域：自动发现、创建、编辑、表映射、软引导/强过滤和删除。
- [ ] 同义词：创建、编辑、批量 CSV 导入/导出、删除、路由命中。
- [ ] 指标：表达式、别名、过滤条件、默认粒度、提交审核、批准/驳回和删除。
- [ ] 时间模式：正则校验、优先级、解析预览、启停、删除。
- [ ] 校验规则：列规则、条件、严重级别、启停、命中结果。
- [ ] 跨数据源关系：左右数据源/表/键、匹配类型、编辑、删除和查询使用。
- [ ] 跨域集群：名称、描述、成员数据源、规划提示、编辑和删除。
- [ ] 查询权限：按用户和数据源验证允许/拒绝表列、行过滤和绕过阻断。
- [ ] 关系建模：外键/Join Path 创建、编辑、关联 SQL、验证和删除。
- [ ] 脱敏规则：租户默认规则存在，自定义规则作用域、优先级、启停和效果验证。

### T-MENU-017 质量分析 `/nl2sql/analytics`

- [ ] Overview、趋势、路由、规则命中、语义覆盖、数据源健康和慢查询加载。
- [ ] 时间范围与数据源筛选同步作用于图表和表格。
- [ ] 路由置信度、方法分布、Top 表与实际测试查询一致。
- [ ] 语义覆盖在刷新描述/Embedding 后变化。
- [ ] 慢查询只显示当前租户，空数据和接口错误状态正确。

### T-AUX-001 Schema 变更 `/nl2sql/schema-changes`

- [ ] 数据源刷新后新增/删除/修改列产生变更记录。
- [ ] 查看详情、受影响查询和语义影响。
- [ ] 批准/拒绝后状态正确且不能重复决定。

## 11. 扩展菜单

### T-MENU-018 Hook 管理 `/hooks`

- [ ] 创建各支持生命周期事件的 Hook，校验名称、事件和脚本/配置。
- [ ] Validate 返回语法/策略结果。
- [ ] Dry Run 展示输入输出、退出码、耗时和脱敏日志。
- [ ] 编辑、启停和删除正常。
- [ ] 真实工具/任务触发后日志出现且租户隔离。
- [ ] 超时、非零退出和恶意命令按权限模式处理，不拖死主任务。

### T-MENU-019 Skill 市场 `/skills`（发布阻断）

- [ ] 首租户自动出现 4 个默认仓库且无重复。
- [ ] 逐个扫描仓库，状态 idle -> scanning -> success/error，发现数更新。
- [ ] GitHub Token 有/无两种情况下错误和限流提示准确，Token 不泄漏。
- [ ] 市场搜索、来源/状态筛选、README 预览和安装。
- [ ] 已安装 Skill 列表、详情、命令清单、启停、元数据编辑和删除。
- [ ] README 查看/编辑/保存，Markdown 安全渲染。
- [ ] ZIP 预览先显示告警，再确认上传；路径穿越和符号链接攻击被拒绝。
- [ ] 添加、扫描、打开和删除自定义市场仓库。

### T-MENU-020 Search 扩展 `/search-providers`

- [ ] 在不配置任何搜索 API Key、Search 扩展和搜索 MCP 的全新环境中，页面明确显示 AOS 内置联网搜索已启用。
- [ ] 分别用中文和英文提问实时问题；返回非空、真实 URL、多域名结果，最终回答保留可点击引用。
- [ ] 创建支持的 Search Provider，填写 Base URL、方法、鉴权、参数和优先级。
- [ ] 编辑、启停、删除和健康测试。
- [ ] Search Doctor 显示内置搜索、配置扩展、模型原生搜索、MCP、本地/RAG 和实际生效顺序。
- [ ] 在超级助手真实联网问题中优先使用健康 Provider。
- [ ] 配置一个必然失败的扩展：该扩展自身“测试”必须失败，不能被内置搜索误报为成功。
- [ ] 真实提问时扩展失败后按 AOS 内置搜索 -> 模型原生 -> MCP -> 本地/RAG 降级，且答案披露来源不足。
- [ ] 断网时内置搜索可诊断失败，但后续模型原生、MCP、本地/RAG 链仍继续，不让会话卡死。

### T-MENU-021 MCP 服务 `/mcp`

- [ ] 新增 stdio `npx` MCP 并测试连接。
- [ ] 新增 stdio `uvx` MCP 并测试连接。
- [ ] 新增支持的 HTTP/SSE MCP 并测试连接。
- [ ] 参数、工作目录和环境变量保存；秘密不回显。
- [ ] 启停、编辑、删除和定期健康状态更新。
- [ ] 工具、资源、Prompts 三个列表能读取真实 capability。
- [ ] 超时、进程退出、无效 JSON-RPC 和不存在命令有可理解错误。
- [ ] 超级助手调用一个 MCP 工具，输入/输出可审计并受权限约束。

## 12. 系统菜单

### T-MENU-022 工作区域 `/workspace`

- [ ] 文件/助手附件模式切换。
- [ ] 新建目录、新建文本文件、上传、刷新和加载更多。
- [ ] 面包屑、进入目录和返回上级。
- [ ] 打开编辑器、保存、重命名、下载和删除。
- [ ] 助手附件下载和删除。
- [ ] `../`、绝对路径、符号链接逃逸、同名冲突和超限文件被拒绝。
- [ ] 用户之间的个人工作区隔离。

### T-MENU-023 API 密钥 `/keys`（发布阻断）

- [ ] 创建 chat、embedding、image、video、audio Key。
- [ ] Anthropic、OpenAI、自定义兼容服务 Base URL 和模型保存正确。
- [ ] 场景、多 Key 优先级、primary、日/月限额、价格和过期时间。
- [ ] Reasoning、max completion token、原生搜索、上下文/输出上限能力保存。
- [ ] 编辑时不输入新 Key 保留原值；输入新 Key 后掩码变化。
- [ ] 启停、健康测试、用量统计和删除。
- [ ] 列表/详情/错误/日志/审计均不包含完整 Key。
- [ ] Key 失败、过期、超限时路由到下一个候选。

### T-MENU-024 配置管理 `/config/management`

- [ ] 产运、数分、研发三个页签加载代码默认、环境值和数据库值来源。
- [ ] 布尔、数字、枚举、字符串类型校验正确。
- [ ] 编辑运行配置后刷新仍保留；需要重启的字段明确提示。
- [ ] PM 预算 profile 创建/编辑/激活，默认项唯一。
- [ ] 敏感配置不返回明文。
- [ ] 非管理员不能修改。

### T-MENU-025 Bot 网关 `/bot-agents`

平台配置、身份绑定、会话复用、任务控制和主动通知的端到端专项验收见 [Bot 网关完整测试手册](evals/BOT_GATEWAY_COMPLETE_TEST_GUIDE.zh-CN.md)。

- [ ] Agent 创建、编辑、启停、删除和详情。
- [ ] 新建 Agent 的能力下拉仅显示超级助手、超级对抗、数据探索和代码开发；默认绑定超级助手。
- [ ] 页面不显示 fallback/兜底开关；未命中前缀时自动进入超级助手，未绑定超级助手时进入第一个能力。
- [ ] 前缀和“是否要求 @”生效；私聊可直接触发，群聊按配置要求 mention。
- [ ] 完成身份绑定后，在 IM 中验证“停掉刚才那个研究任务”“今天失败的有哪些”“研发任务卡在哪里”；这些指令无需绑定 WatchDog。
- [ ] Channel 创建、编辑、启停、测试和删除。
- [ ] 至少测试 Generic Webhook 本地收发；有账号时测试目标平台。
- [ ] 平台字段完整显示 Generic Webhook、DingTalk、Feishu、Lark、WeCom、Slack、Discord、Telegram、WhatsApp；Feishu 和 Lark 不混用 API 域名。
- [ ] 入站模式严格匹配实现：DingTalk/Feishu/Lark/WeCom 为 auto+stream，Slack/Discord 为 auto+socket，Telegram 为 auto+polling+webhook，WhatsApp/Generic 仅 webhook。
- [ ] 每个平台都显示双语“官方配置手册”入口（Generic 除外），链接打开对应官方文档。
- [ ] 高级配置的 `?` 弹窗按当前平台显示实际支持的 JSON key、无凭证示例和说明；Token/Secret 只填专用密码字段。
- [ ] 自动通知逐项验证任务完成、失败、等待输入、等待审批、卡住、取消完成；卡住默认阈值为 10 分钟。
- [ ] 未完成身份绑定或没有已验证私聊时不会误投递通知；勾选事件不影响 IM 中主动查询、追问和取消任务。
- [ ] Stream/Socket/Polling 通道直接 POST Webhook URL 必须被拒绝；Webhook 通道正常接收。
- [ ] 入站幂等，重复 event/message ID 不重复创建任务或回复。
- [ ] 出站日志、通知事件筛选、失败原因和重试可见。
- [ ] Secret/Webhook/Token 不回显、不进入日志。
- [ ] Feishu/Lark 同时配置 App Secret 和自定义机器人 Webhook 时，Webhook 只使用独立的机器人加签 Secret，不误用 App Secret。
- [ ] WhatsApp 页面明确说明官方入站依赖公网 Webhook；不得宣传为本地长连接。

### T-MENU-026 团队管理 `/users`

- [ ] 邀请 admin/developer/viewer，生成邀请链接。
- [ ] SMTP 未配置时链接可复制；已配置时发送状态准确。
- [ ] 接受邀请设置密码，Token 只能使用一次且过期后失败。
- [ ] 编辑姓名、角色、状态、权限模式和菜单权限。
- [ ] 发送重置邮件/生成重置链接，状态准确。
- [ ] 停用用户后立即不能登录，不能删除/停用最后一个系统管理员。

### T-AUX-002 租户管理 `/tenants`

- [ ] 管理员可打开直接路由，创建、编辑套餐/名称/slug、查看用量和删除租户。
- [ ] 新租户同样 seed 默认预算、25 条脱敏规则和 4 个 Skill 仓库。
- [ ] slug 唯一；系统租户和当前租户受删除保护。
- [ ] 租户切换后用户、Key、Skill、任务、数据源和文件不串数据。

## 13. AgentOps / WatchDog 辅助页

### T-AUX-003 WatchDog `/agent-ops`

- [ ] Summary、Agent、Queue、running/stale/failed/cancelling 列表更新。
- [ ] 任务抽屉显示 trace、runtime process、artifact、资源链接和投递记录。
- [ ] Cancel/Retry 根据任务状态启用，操作后 WebUI 和指挥中心一致。
- [ ] Recover Queue 和 Recover Runtime 对真实异常任务生效且幂等。
- [ ] 打开原资源和导出产物正确。
- [ ] Ask WatchDog 能回答当前任务状态，引用真实 task/trace，不编造。
- [ ] 失败投递 Replay 不重复发送已成功记录。

### T-AUX-004 超级对抗 `/adversarial`

- [ ] 创建多角色对抗任务，选择模型和参数。
- [ ] 流式阶段、各角色输出、审计和最终综合可见。
- [ ] 取消、恢复、历史列表、线程编辑和删除正常。
- [ ] 任务同步出现在 AgentOps/指挥中心。

### T-AUX-005 分享页 `/preview/share`

- [ ] 未登录可打开有效分享 payload。
- [ ] UTF-8 中文、Markdown、图片/音频链接正确。
- [ ] 损坏、超限或恶意 payload 安全失败，不执行脚本。

## 14. Rust API 全面检查

以下命名空间必须至少完成鉴权、正常响应、非法参数、资源不存在、无权限、跨租户和并发测试。菜单章节中的 UI 操作应同时在浏览器 Network 或 API 日志中记录对应请求。

| 命名空间 | 必测能力 |
| --- | --- |
| `/api/v1/setup` | 状态、首租户事务、并发冲突 |
| `/api/v1/auth` | 登录、当前用户、退出、改密、注册策略、接受邀请 |
| `/api/v1/users` | 列表、详情、邀请、更新、停用、重置 |
| `/api/v1/notifications` | 列表、单个/全部已读、删除 |
| `/api/v1/dashboard` | overview、趋势、模型/模块用量、告警 CRUD |
| `/api/v1/mcp` | CRUD、测试、启停、stats、tools/resources/prompts |
| `/api/v1/memory` | 记忆列表、偏好、压缩和引用隔离 |
| `/api/v1/workspace` | 文件/目录/上传 CRUD、下载、路径安全 |
| `/api/v1/skills` | registry、ZIP、README、commands、市场仓库/扫描/安装 |
| `/api/v1/hooks` | CRUD、validate、dry-run、logs |
| `/api/v1/apikeys` | CRUD、健康、stats、加密/掩码/failover |
| `/api/v1/sessions` | 会话列表、详情、更新、删除和消息恢复 |
| `/api/v1/tenants` | CRUD、usage、tenant seed 和隔离 |
| `/api/v1/chat` | 消息、流、能力、文件、记忆和错误恢复 |
| `/api/v1/uploads` | 类型/大小、下载鉴权和文件名安全 |
| `/api/v1/config` | overview、management、env、PM budget |
| `/api/v1/demo` | 场景列表、启动、feature readiness |
| `/api/v1/agent-ops` | summary/tasks/trace/queue/agent/recover/ask/artifact |
| `/api/v1/agent-runtime` | session/process/artifact 生命周期和权限 |
| `/api/v1/tasks` | list/detail/events/attempts/resources/artifacts/commands/watch/identity |
| `/api/v1/bot-identities` | 配对、解析、撤销和过期 |
| `/ws` | 鉴权、订阅、重连、重复事件和断开清理 |
| `/api/v1/bot-agents` | Agent/Channel CRUD、测试、logs、入站/出站 |
| `/api/v1/super-assistant` | 统一消息、路由、阶段、取消和最终结果 |
| `/api/v1/agent` | 会话、对抗、PM、上下文和工具能力 |
| `/api/v1/pm` | chat/search/report/quality/mission/material 全生命周期 |
| `/api/v1/projects` | 项目 CRUD、sync 和路径边界 |
| `/api/v1/rd` | 仓库、任务、Diff、测试、规格、Agent/Workflow/规则/集成 |
| `/api/v1/data-sources` | CRUD、连接、discover、DDL、手工 schema、导入导出 |
| `/api/v1/nl2sql` | 理解、澄清、路由、生成、执行、Agent、语义、知识、治理、分析 |

### T-API-001 通用协议（发布阻断）

- [ ] 无 Token 返回 401；无权限返回 403；不存在返回 404；冲突返回 409；未初始化返回 428。
- [ ] JSON 错误结构稳定，5xx 不包含 SQL、栈、磁盘路径或秘密。
- [ ] 分页边界、空字符串、超长字符串、非法枚举、负数和未知字段处理一致。
- [ ] POST 重试或幂等键不会重复创建关键任务/投递。
- [ ] SSE/WS 断线重连不丢终态、不重复最终答案。

### T-API-002 SQLite 并发（发布阻断）

- [ ] 20 个并发只读请求稳定。
- [ ] 多用户同时创建任务、消息、通知和反馈不会出现 `database is locked`。
- [ ] 同资源并发修改按事务/冲突规则收敛，不产生半写数据。
- [ ] 第二个进程使用同一数据目录时明确拒绝启动。
- [ ] SIGTERM 后 WAL checkpoint 和 clean marker 正常。
- [ ] 模拟 SIGKILL 后重启执行 quick check，数据可恢复或明确拒绝损坏库。

## 15. 安全与开源卫生

### T-SEC-001 密钥扫描（发布阻断）

```bash
rg -n --hidden \
  -g '!webui/node_modules/**' -g '!webui/dist/**' \
  -g '!rust/target*/**' -g '!dist/**' \
  '(^|[^A-Za-z0-9])(sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_]{20,}|BEGIN .*PRIVATE KEY|DATABASE_URL=)' .
```

- [ ] 没有真实模型 Key、GitHub Token、私钥或已废弃平台 `DATABASE_URL`。
- [ ] 保留命中仅允许是明显不可用的测试 fixture，并且同一测试断言秘密已被脱敏；其他命中全部阻断发布。
- [ ] `.env.example` 只有空占位。
- [ ] 日志和截图检查无秘密。

### T-SEC-002 Web 安全

- [ ] Markdown/分享/Skill README 中脚本、事件属性和危险 URL 被净化。
- [ ] 上传文件不能路径穿越或覆盖配置/数据库。
- [ ] CORS 默认只允许 `BASE_URL`，不配置时不等于 `*`。
- [ ] 公开注册默认关闭。
- [ ] 密码哈希、JWT、API Key 加密密钥缺失或格式错误时拒绝不安全启动。
- [ ] 数据删除、代码应用、任务执行等危险操作有权限和确认。

## 16. 恢复、备份和升级

### T-OPS-001 备份恢复（发布阻断）

1. 创建用户、Key、Skill、任务和文件。
2. 停止并备份整个 `.aos-data` 与 `.env`。
3. 清空数据。
4. 恢复并启动。

- [ ] 所有对象可读，Key 仍可解密和调用。
- [ ] 没有 WAL 丢失或 quick check 错误。

### T-OPS-002 异常重启

- [ ] 运行中任务在进程被杀后按能力恢复或标记失败，不永久卡 running。
- [ ] queued/cancelling、NL2SQL refresh、PM mission、投递 outbox 状态一致。
- [ ] 重启后定时任务只在 misfire grace 内补跑，不重复大规模执行。

### T-OPS-003 升级

- [ ] 旧版本数据副本启动新版本时 migration 一次成功。
- [ ] 重启不会重复 seed 默认仓库/规则/预算。
- [ ] 新版本失败时保留原备份并能回退，不用手工编辑 SQLite。

## 17. 最终发布验收表

| 门禁 | 结果 | 证据 |
| --- | --- | --- |
| 环境脚本在空白机通过 |  |  |
| 源码一键构建通过 |  |  |
| 发布包生成且无数据/密钥 |  |  |
| 解压发布包从空数据启动 |  |  |
| 首租户与默认 seed 正确 |  |  |
| API Key 保存和跳过两条路径 |  |  |
| 26 个可见菜单全部通过 |  |  |
| 辅助页面全部通过 |  |  |
| Rust workspace 全量测试通过 |  |  |
| WebUI 全量测试通过 |  |  |
| DeepSeek 真实场景通过 |  |  |
| NL2SQL 外部数据源回归通过 |  |  |
| SQLite 并发/单实例/崩溃恢复通过 |  |  |
| 备份恢复和升级通过 |  |  |
| 密钥、许可证、Notice 和文档审计通过 |  |  |

只有全部发布阻断项为 `PASS`，并且没有未解释的功能降级，才能标记当前版本为开源可发布。
