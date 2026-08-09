# AOS WatchDog / AgentOps 完整测试用例

版本：1.0  
日期：2026-07-26  
适用迁移：188-200  
实施计划：`docs/plans/watchdog-agentops-mobile-command-center.md`

## 1. 测试结论边界

当前核心控制平面已完成本地实现和自动化检查，未发现已知阻断 bug。本文用于完成发布前
人工验收、真实 Bot 平台验收、故障注入和容量验收。任何 P0 用例失败都应停止发布。

以下项目不能仅靠本地单元测试判定完成：

- 钉钉、飞书、企业微信、WhatsApp、Slack、Discord、Telegram 和 Webhook 的真实回执。
- 目标部署容量下的 SLO、长稳、数据库故障、worker kill 和网络分区。
- Linux bubblewrap 与生产进程隔离。
- 依赖企业组织目录的 team scope 和团队 SLA。

当前版本提供结构化值守规则 UI，不把“自然语言生成规则草案”作为本轮通过条件。平台原生
卡片未实现时必须可靠降级为纯文本，不能丢消息或伪装发送成功。

## 2. 严重级别和通过规则

| 级别 | 定义 | 发布规则 |
|---|---|---|
| P0 | 越权、数据泄漏、重复副作用、任务丢失、无法控制、错误终态 | 任意失败立即阻断 |
| P1 | 核心流程、恢复、通知、实时性或主要 UX 异常 | 必须修复后重测 |
| P2 | 平台差异、次要兼容、文案和低频降级问题 | 记录负责人和期限 |

通过必须同时满足：页面结果正确、API/数据库事实一致、事件序号单调、无越权数据、无重复
任务/命令/通知，以及浏览器控制台和服务端日志没有对应 ERROR 或 panic。

## 3. 测试环境和账号

### 3.1 环境

- 使用独立验收数据库，禁止在生产库执行故障注入。
- 通过正常 migration 流程应用 188-200，不手工修改表结构。
- 使用 `AOS_WEB_SERVER_FEATURES=full` 构建和启动服务。
- 为测试租户配置：
  - `watchdog_control_plane_v2=on`
  - `watchdog_external_identity=optional`，强制绑定场景另测 `required`
  - `watchdog_notification_outbox=on`
  - `watchdog_mobile_handoff=on`
  - `watchdog_watch_rules=on`
- 至少配置一个可用聊天模型、一个联网能力、一个测试数据源和一个测试 Bot。
- 记录服务端日志、浏览器 Network/Console、Bot 平台消息 ID 和数据库慢查询。

### 3.2 账号矩阵

| 账号 | 租户 | 角色 | 权限 | 用途 |
|---|---|---|---|---|
| A | T1 | developer | `tasks:read,tasks:control` | 主测试用户 |
| B | T1 | viewer | `tasks:read` | 同租户只读/隔离用户 |
| C | T1 | developer | `tasks:read,tasks:control` | 同租户越权测试 |
| D | T1 | admin | `tasks:read,tasks:control,tasks:admin` | 管理员测试 |
| E | T2 | admin | 完整权限 | 跨租户隔离测试 |
| BOT-A | T1 | 外部身份 | 绑定 A | 私聊和移动接管 |
| BOT-C | T1 | 外部身份 | 绑定 C | 外部身份隔离 |
| BOT-X | T1 | 未绑定 | 无 | 未绑定访问测试 |

### 3.3 测试数据

- A、C 各自上传同名但内容不同的 `private-report.txt`。
- A 创建包含敏感测试串的附件，例如 `AKIA_TEST_ONLY`、`sk-test-only`，不得使用真实密钥。
- 准备一个可返回数据的测试 SQL 数据源和一个故意超时的数据源。
- 准备可下载 artifact、图片附件、大文本附件和失败任务。
- 每个任务记录 `taskId`、短 ID、`sessionId`、`turnId`、首响时间和终态时间。

## 4. 安装、迁移和启动

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-ENV-001 | P0 | 在 migration 187 的空验收库启动迁移 | 188-200 按顺序成功，重复执行不破坏数据 |
| WD-ENV-002 | P0 | 故意不应用 migration 200 后启动 | schema 健康检查明确指出需要 188-200 和缺失字段，worker 不带病运行 |
| WD-ENV-003 | P0 | 完整迁移后启动 full 服务 | 无 panic、无 ColumnDecode、WatchDog worker 正常启动 |
| WD-ENV-004 | P1 | `watchdog_control_plane_v2=off` | 新链路不对用户生效，旧功能仍可用，无双份任务 |
| WD-ENV-005 | P1 | `watchdog_control_plane_v2=shadow` | 可生成影子投影，但用户侧不出现重复结果或通知 |
| WD-ENV-006 | P0 | `watchdog_notification_outbox=on` | durable worker 投递，旧直发链路被抑制 |
| WD-ENV-007 | P0 | `watchdog_notification_outbox=shadow` | 不向外部真实发送，delivery 状态准确，不假报 sent |
| WD-ENV-008 | P1 | 配置非法 feature mode | 配置被拒绝并给出可诊断错误，不静默回退为 on |
| WD-ENV-009 | P1 | 重启服务三次 | 不生成重复根任务、重复订阅、重复通知或重复 retry |
| WD-ENV-010 | P1 | 检查 188-200 索引和唯一约束 | 短 ID、订阅、retry、outbox、identity 和 delivery 约束存在 |

## 5. 权限、菜单和基本导航

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-RBAC-001 | P1 | A 登录 | 显示“任务指挥中心”和顶栏任务按钮 |
| WD-RBAC-002 | P0 | 移除 A 的 `tasks:read` 后重新登录 | 菜单、路由和顶栏入口均不可访问 |
| WD-RBAC-003 | P0 | B 登录 | 可读本人任务，不显示取消、重试、审批等控制按钮 |
| WD-RBAC-004 | P0 | A 登录 | 只对服务端 `allowedActions` 返回的动作显示按钮 |
| WD-RBAC-005 | P0 | 模拟旧 API 不返回 `allowedActions` | UI 不展示控制按钮，不自行推断权限 |
| WD-RBAC-006 | P0 | A 访问 `/agent-ops` | 被拒绝或隐藏，不获得管理员运维能力 |
| WD-RBAC-007 | P1 | D 访问 `/agent-ops` | 可查看 worker、queue、stale、dead letter 等管理信息 |
| WD-RBAC-008 | P1 | 访问旧 `/watchdog` | 跳转到新任务中心或管理员 AgentOps，不出现死链 |
| WD-RBAC-009 | P1 | 旧 `watchdog:read` 用户登录 | 权限兼容映射为 `tasks:read` |
| WD-RBAC-010 | P1 | 中英文切换 | 菜单、状态、按钮、确认框和空状态全部切换，无 missingKey |

## 6. 全任务覆盖和根任务图

以下提示词均由 A 执行。每个用户意图只能创建一个根任务；专业执行器作为子任务或资源，
不能创建第二份用户问题，也不能写第二个最终回答。

| ID | 级别 | 输入/操作 | 预期结果 |
|---|---|---|---|
| WD-COV-001 | P0 | `把“今天开会讨论发布计划”改写得正式一些。` | 正常完成；简单短问答自动归档，不污染默认活跃列表 |
| WD-COV-002 | P0 | `联网查询今天 OpenAI 官方最新公告并给出处。` | 根任务关联联网执行证据，完成后只有一个终态和一个最终回答 |
| WD-COV-003 | P0 | `/深度研究 研究 2026 年企业 AgentOps 的主要架构和风险。` | PM research 为子资源，阶段持续更新，报告和 artifact 可打开 |
| WD-COV-004 | P0 | `查询测试库最近七天收入趋势并给出 SQL。` | NL2SQL 生成、校验、执行作为关联资源，SQL/结果可审计 |
| WD-COV-005 | P0 | `/数据归因 找出最近七天 ROI 变化最大的一天并下钻原因。` | 归因子任务、每轮 SQL、结果和报告归属同一根任务 |
| WD-COV-006 | P0 | `/超级对抗 比较微服务和模块化单体，给出最终裁决。` | 多轮模型事件和胜者信息属于同一根任务，无重复用户消息 |
| WD-COV-007 | P1 | 发起 RD/Code Studio 长任务并运行测试 | runtime、测试、diff、artifact 可从任务详情关联查看 |
| WD-COV-008 | P1 | 发起素材生成任务 | 素材 job 被投影，成功/失败与业务表终态一致 |
| WD-COV-009 | P1 | 发起 Workspace 长执行 | 父任务可观察，执行 artifact 可见，不能泄露物理路径 |
| WD-COV-010 | P0 | 从任务中心创建计划任务并触发 | 有 AgentOps 根任务，但不创建超级助手 session、不占 session 上限 |
| WD-COV-011 | P0 | BOT-A 私聊发送一个普通长任务 | 进入与 WebUI 相同的超级助手父循环，不走降级版独立回答链路 |
| WD-COV-012 | P0 | BOT-A 询问“现在有哪些任务在执行” | 走确定性查询快路，不递归创建新的用户任务 |
| WD-COV-013 | P1 | 一个子任务失败后父 Agent 换方案成功 | 根任务最终 completed，失败 attempt 保留且不覆盖 |
| WD-COV-014 | P0 | 比较业务资源表和 AgentOps 状态 | 终态一致；查询任务列表本身不修改专业资源状态 |

## 7. 任务指挥中心和实时体验

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-UI-001 | P1 | 打开顶栏任务抽屉 | 运行、待处理、失败计数和最近活动准确 |
| WD-UI-002 | P1 | 新建长任务并保持抽屉打开 | 无需手动刷新即可看到状态和阶段变化 |
| WD-UI-003 | P1 | 打开“进行中” | 仅显示 created/queued/claimed/running/retrying/cancelling 等活跃根任务 |
| WD-UI-004 | P1 | 打开“待我处理” | 仅显示 waiting_input/waiting_approval/blocked 等任务 |
| WD-UI-005 | P1 | 关注一个任务后打开“关注中” | 任务出现且订阅状态准确，取消关注后消失 |
| WD-UI-006 | P1 | 打开“历史” | 终态任务按稳定游标加载，无重复和跳项 |
| WD-UI-007 | P1 | 打开计划任务视图 | 保留计划任务入口，不混入超级助手 session |
| WD-UI-008 | P1 | 打开任务详情 | 摘要、状态、来源、阶段、活动、时间和结果准确 |
| WD-UI-009 | P1 | 查看执行图、attempt、资源和命令审计 | 父子关系、失败尝试、命令 actor 和状态完整 |
| WD-UI-010 | P0 | 打开 artifact | 再次鉴权后读取正确内容；无权限返回不存在 |
| WD-UI-011 | P1 | 点击“打开原会话” | 定位到原 session/turn，不复制问题或新建 session |
| WD-UI-012 | P1 | 移动端 375x812 查看抽屉和详情 | 无遮挡、不可见按钮或横向页面溢出，操作可完成 |
| WD-UI-013 | P1 | 实时更新时主动查看较早内容 | 页面不强制抢走用户当前阅读位置 |
| WD-UI-014 | P1 | 快速切换中英文和任务 tab | 不闪现前一用户/前一租户缓存内容，无控制台异常 |

## 8. SSE、刷新和身份缓存

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-SSE-001 | P0 | 任务执行中刷新页面 | 按 event cursor 恢复，状态、文本和事件不重复 |
| WD-SSE-002 | P0 | 断网 20 秒后恢复 | 自动重连并补齐缺失事件，不回退终态 |
| WD-SSE-003 | P0 | 服务端重复发送同一 event ID | reducer 只应用一次 |
| WD-SSE-004 | P1 | 人为把 SSE frame 拆成任意小 chunk | CRLF/LF、多行 data 和跨 chunk frame 均正确解析 |
| WD-SSE-005 | P1 | 注入 comment、非法 JSON 和超大 frame | 警告并跳过坏 frame，后续合法事件继续处理 |
| WD-SSE-006 | P0 | 收到乱序旧 stateVersion | 旧状态不能覆盖较新状态或终态 |
| WD-SSE-007 | P1 | 连续运行超过 500 个事件 | 前端元数据有界，输入和滚动无明显卡顿 |
| WD-SSE-008 | P0 | A 登录后退出并登录 C | React Query 缓存已清空，不闪现 A 的任务标题和结果 |
| WD-SSE-009 | P0 | T1 切换到 T2 | SSE cursor 和列表缓存按 tenant/user 隔离 |
| WD-SSE-010 | P0 | SSE 建连后撤销用户权限 | 连接关闭或拒绝后续数据，不继续推送私人事件 |

## 9. 控制命令和状态收敛

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-CMD-001 | P0 | 对 running 任务点击取消并确认 | 创建持久化 command，任务先 cancelling，执行器确认后 cancelled |
| WD-CMD-002 | P0 | 取消长模型/研究/归因任务 | 后台实际停止或明确显示外部调用不可取消，不能只改 UI |
| WD-CMD-003 | P0 | 页面断开但不点击取消 | 任务继续执行，断线不等于取消 |
| WD-CMD-004 | P0 | 对 failed 且可重试任务点击重试 | 创建新 attempt，原失败证据保留 |
| WD-CMD-005 | P0 | 并发点击两次重试 | 最多一个 queued/claimed retry 或活动 retry attempt |
| WD-CMD-006 | P0 | 对 waiting_input 提交补充信息 | 恢复同一任务和原 turn，不创建平行回答 |
| WD-CMD-007 | P0 | 对 waiting_approval 批准 | 审计 actor，原任务继续执行 |
| WD-CMD-008 | P0 | 对 waiting_approval 拒绝 | 审计 actor 和拒绝结果，任务按策略收敛 |
| WD-CMD-009 | P0 | 使用过期 expectedStateVersion 发命令 | 返回冲突/校验错误，不覆盖新状态 |
| WD-CMD-010 | P0 | 同一 idempotencyKey 重复同一命令 | 返回同一 command，不产生重复副作用 |
| WD-CMD-011 | P0 | 同一 idempotencyKey 用于另一任务 | 返回 conflict，不串任务 |
| WD-CMD-012 | P0 | 对终态任务发送取消/审批 | 服务端拒绝非法状态转换 |
| WD-CMD-013 | P0 | B/C 猜测 A 的 taskId 或短 ID 发命令 | 返回不存在或无权，不泄露任务是否存在 |
| WD-CMD-014 | P1 | Bot 输入“取消第 1 个任务” | 必须解析并确认稳定短 ID，不能直接依赖动态序号执行 |

## 10. Bot 身份、查询和跨端接管

每个已启用平台至少执行一次本节；平台专项差异见第 15 节。

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-BOT-001 | P0 | A 在 WebUI 创建一次性配对码，BOT-A 私聊提交 | 绑定到 A，保存稳定平台身份和私聊会话 |
| WD-BOT-002 | P0 | 重复使用已消费配对码 | 被拒绝，不创建第二个绑定 |
| WD-BOT-003 | P0 | 使用过期、错误平台或篡改配对码 | 被拒绝且有审计，不泄露用户信息 |
| WD-BOT-004 | P0 | 尝试把 BOT-A 再绑定给 C | 被拒绝，不能抢占身份 |
| WD-BOT-005 | P0 | `watchdog_external_identity=required` 时 BOT-X 查询 | 进入受限配对提示，不能读取私人任务 |
| WD-BOT-006 | P0 | BOT-A 询问“正在执行哪些任务” | 仅返回 A 可见根任务、短 ID、真实状态和更新时间 |
| WD-BOT-007 | P0 | BOT-C 查询 A 的短 ID | 返回不存在，不泄露标题、状态、模型或耗时 |
| WD-BOT-008 | P0 | BOT-A 在群聊查询私人任务 | 默认不暴露私人标题/结果，提示转私聊或显式分享 |
| WD-BOT-009 | P1 | BOT-A 询问“#XXXX 为什么慢” | 仅根据结构化阶段、心跳和阻塞证据解释，不编造百分比/ETA |
| WD-BOT-010 | P0 | BOT-A 订阅 WebUI 发起的长任务 | 后续事件路由回当前已验证会话 |
| WD-BOT-011 | P0 | BOT-A 为 waiting_input 任务补充信息 | 恢复原任务、原 session 和原 turn |
| WD-BOT-012 | P0 | BOT-A 批准/拒绝等待审批任务 | 权限和二次确认生效，动作可审计 |
| WD-BOT-013 | P0 | BOT-A 取消任务 | 使用稳定 task/short ID，最终状态与后台一致 |
| WD-BOT-014 | P1 | Bot 上传附件并发起任务 | 附件进入 A 的隔离 Workspace，C 无法搜索或读取 |
| WD-BOT-015 | P1 | Bot 返回长报告 | 先发摘要和操作，长内容以 artifact/分页提供 |
| WD-BOT-016 | P0 | 在 WebUI 打开 Bot 发起任务 | 可见完整 lineage，不出现重复用户消息 |
| WD-BOT-017 | P0 | 撤销 BOT-A 绑定后继续查询/接收通知 | 立即失效，相关订阅禁用或投递失败 |
| WD-BOT-018 | P0 | 平台重复投递同一 external message ID | 只创建一个根任务和一个 acknowledgement |

## 11. 订阅、通知和投递可靠性

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-NOT-001 | P1 | 关注任务，目标 WebUI | 订阅存在，完成/失败按策略生成站内通知 |
| WD-NOT-002 | P0 | 订阅 Bot/Webhook 但不提供 destinationRef | 服务端拒绝 |
| WD-NOT-003 | P0 | A 使用 C 的 destinationRef | 服务端拒绝，不能跨用户发送 |
| WD-NOT-004 | P0 | 重复创建相同任务/目标订阅 | 更新原订阅，不产生重复有效订阅 |
| WD-NOT-005 | P1 | 删除订阅 | 后续事件不再生成该目标 delivery |
| WD-NOT-006 | P1 | 任务 started/progress 高频更新 | 不逐 token 推送，只生成关键里程碑 |
| WD-NOT-007 | P0 | 任务 completed | 只投递一次，包含短 ID、摘要、真实耗时和深链接 |
| WD-NOT-008 | P0 | 任务 failed/cancelled/waiting_input/waiting_approval | 事件类型、等级和可用操作准确 |
| WD-NOT-009 | P1 | WebUI 在线且开启移动跟随 | presence 策略按配置抑制外部打扰 |
| WD-NOT-010 | P1 | WebUI 离开超过阈值 | opt-in 用户开始接收移动跟随通知 |
| WD-NOT-011 | P1 | 设置静默时段 | 非紧急通知延迟/抑制，原因可审计 |
| WD-NOT-012 | P0 | 平台返回 429/可重试 5xx | 指数退避并最终收敛，不改变任务业务状态 |
| WD-NOT-013 | P1 | 平台永久错误或达到最大尝试 | delivery 进入 failed/dead letter，可在管理面查看 |
| WD-NOT-014 | P0 | worker 在外部 dispatch 前崩溃 | lease 恢复后回到 queued，可安全重试 |
| WD-NOT-015 | P0 | worker 在外部可能接收后、本地落库前崩溃 | delivery 进入 `unknown`，禁止自动重发 |
| WD-NOT-016 | P0 | 普通用户查看 `unknown` delivery | 不显示人工 replay 权限 |
| WD-NOT-017 | P0 | D 人工 replay `unknown` delivery | 明确警告可能重复；确认后才重新排队并写审计 |
| WD-NOT-018 | P0 | 通知正文含测试密钥、token、密码 | DLP 脱敏或阻断，不把原值发出 |
| WD-NOT-019 | P0 | restricted/confidential 任务完成 | 外部只发送元数据和安全深链接，不发送敏感正文 |
| WD-NOT-020 | P0 | durable mode on 时触发旧通知场景 | 用户只收到一份通知，旧直发被禁用 |

## 12. 值守规则和决策收件箱

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-RULE-001 | P1 | 创建“超过 10 分钟无进展通知”规则 | 规则按当前用户保存并显示 |
| WD-RULE-002 | P1 | 创建 task.failed -> notify 规则 | 仅匹配指定事件和 scope |
| WD-RULE-003 | P0 | 创建 retry_once 且 requiresConfirmation=true | 动作进入决策收件箱，不自动产生副作用 |
| WD-RULE-004 | P0 | A 批准规则动作 | 创建一个幂等 command，记录 rule/run/actor |
| WD-RULE-005 | P0 | A 拒绝规则动作 | 不执行命令，拒绝原因/决定可审计 |
| WD-RULE-006 | P0 | 重复批准同一 run | 只执行一次 |
| WD-RULE-007 | P1 | 设置 maxActionsPerDay=1 并触发两次 | 第二次跳过且记录限额原因 |
| WD-RULE-008 | P1 | 设置 quiet hours 后触发 | 按策略跳过或延迟，不能静默丢失 |
| WD-RULE-009 | P0 | C 猜测 A 的 rule/run ID | 返回不存在或无权 |
| WD-RULE-010 | P1 | 删除规则 | 后续事件不再匹配，历史 run 保留审计 |

## 13. 重启恢复、reconciler 和故障注入

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-REC-001 | P0 | running 父任务期间重启服务 | 任务被重新认领并继续，不重复启动子任务 |
| WD-REC-002 | P0 | waiting_input/waiting_approval 时重启 | 等待状态和待处理动作保留 |
| WD-REC-003 | P0 | command queued/claimed 时杀 worker | lease 到期后安全恢复，副作用最多一次 |
| WD-REC-004 | P0 | outbox claimed 时杀 worker | 事件最终被投影，无丢失和重复可见事件 |
| WD-REC-005 | P0 | notification claimed 时杀 worker | 按 dispatch 边界收敛为 queued 或 unknown |
| WD-REC-006 | P0 | 数据库短暂不可用 30 秒 | 服务记录可诊断错误，恢复后 worker 继续收敛 |
| WD-REC-007 | P1 | 外部平台网络分区 | 其他目标不受阻，失败目标独立重试 |
| WD-REC-008 | P1 | 心跳暂停但业务资源仍运行 | 先 suspected/stalled，再复核；不立即误判失败 |
| WD-REC-009 | P0 | 正常长任务超过常规耗时 | 不被系统自动取消，页面持续显示真实阶段和心跳 |
| WD-REC-010 | P1 | stale 任务恢复 | 创建 recovered 事件/新 attempt，旧历史保留 |
| WD-REC-011 | P0 | 已终态任务收到旧事件 | 终态不可回退 |
| WD-REC-012 | P0 | 重放同一 projector event | 状态和通知保持幂等 |
| WD-REC-013 | P0 | SQL/外发等不可盲重放动作失败 | 不自动重复外部副作用，要求明确重试/审批 |
| WD-REC-014 | P1 | 自愈失败 | 用户看到已尝试动作、当前风险和可选下一步，无无限循环 |

## 14. 用户隔离和安全

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-SEC-001 | P0 | C 列出任务 | 看不到 A 的私人任务标题、短 ID、状态和更新时间 |
| WD-SEC-002 | P0 | C 直接 GET A 的 taskId/short ID | 返回不存在，不区分“存在但无权” |
| WD-SEC-003 | P0 | C 请求 A 的 events/resources/attempts/commands | 全部零泄漏 |
| WD-SEC-004 | P0 | C 请求 A 的 artifact 内容 | 返回不存在，不能通过 URL 猜测读取 |
| WD-SEC-005 | P0 | C 订阅、取消、重试、审批 A 的任务 | 全部拒绝且无副作用 |
| WD-SEC-006 | P0 | C 连接 SSE 并使用 A 的 after cursor | 只收到 C 的授权事件 |
| WD-SEC-007 | P0 | E 使用 T2 token 猜测 T1 taskId | 跨租户零泄漏 |
| WD-SEC-008 | P0 | A 分享任务给 C 后再撤销 | 授权期间按 grant 可见；撤销后列表、SSE、artifact、命令立即失效 |
| WD-SEC-009 | P0 | 请求 team scope 且未配置组织目录 | fail closed，不退化为 tenant scope |
| WD-SEC-010 | P0 | 群聊中多人查询 | 以发送者身份鉴权，不以群 ID 或 Bot 创建者身份授权 |
| WD-SEC-011 | P0 | D 使用 tenant scope | 仅管理员可扩大范围，文本“全部/all”不能提升权限 |
| WD-SEC-012 | P0 | 构造超长/恶意短 ID、task ID、cursor | 返回受控 4xx，无 panic、SQL 注入或高成本扫描 |
| WD-SEC-013 | P0 | 通知内容加入 prompt injection | 只能作为数据处理，不能改变授权、目标或通知策略 |
| WD-SEC-014 | P0 | 注销、停用用户或解绑外部身份 | 订阅、SSE、命令和历史深链接立即失效 |
| WD-SEC-015 | P0 | 检查日志和拒绝事件 | 不记录 token、真实密钥、其他用户正文或物理路径 |
| WD-SEC-016 | P0 | A/C 上传同名文件并分别执行任务 | 结果只引用各自文件，Hash、路径、embedding 和 artifact 不串用户 |

## 15. 真实平台测试矩阵

每个平台都执行签名验证、重复消息、私聊、群聊、mention、ack、状态查询、订阅、取消、
等待输入、完成通知、失败降级和解绑。没有原生卡片时，纯文本路径必须完整可操作。

| ID | 平台 | 专项检查 | 通过标准 |
|---|---|---|---|
| WD-PLAT-001 | 钉钉 | 签名、conversation、机器人限流、卡片/文本 | 原会话准确、无重复、回执真实 |
| WD-PLAT-002 | 飞书/Lark | challenge、签名、open_id、群 mention | 发送者身份准确，不使用 Bot 创建者 |
| WD-PLAT-003 | 企业微信 | signature、私聊/群聊、主动消息限制 | 限制可见且有文本降级 |
| WD-PLAT-004 | WhatsApp | webhook 验签、24h 窗口、模板消息 | 窗口外不假报成功，delivery 状态准确 |
| WD-PLAT-005 | Slack | signing secret、retry header、thread | 重复回调幂等，回复原 thread |
| WD-PLAT-006 | Discord | interaction 验签、超时 ack、follow-up | 首响及时，长任务用 follow-up |
| WD-PLAT-007 | Telegram | update_id 幂等、private/group、inline/fallback | 重复 update 不重复任务 |
| WD-PLAT-008 | Webhook | HMAC、时间戳、nonce、schemaVersion | replay 被拒绝，delivery ID 稳定 |

## 16. 性能、容量和长稳

所有结果至少记录 p50/p95/p99、错误率、数据库连接等待、慢 SQL、CPU、内存和队列积压。

| ID | 级别 | 场景 | 通过标准 |
|---|---|---|---|
| WD-PERF-001 | P1 | 单用户 1000 个历史任务加载 summary/list | summary 和列表 p95 < 1 秒 |
| WD-PERF-002 | P1 | 1000 个活跃任务并发更新 | WebUI/Bot 状态新鲜度 p95 < 2 秒 |
| WD-PERF-003 | P1 | 100 并发 Bot 入站 | acknowledgement p95 < 1.5 秒，无重复任务 |
| WD-PERF-004 | P1 | 100 并发确定性状态查询 | p95 < 1 秒，不调用 LLM |
| WD-PERF-005 | P1 | 复杂“为什么慢”解释 | 规则结果立即可用，模型总结 p95 < 8 秒或降级 |
| WD-PERF-006 | P1 | 1000 个关键通知事件 | 首次投递 p95 < 5 秒，队列可收敛 |
| WD-PERF-007 | P1 | 100 并发取消 | 可取消任务确认收敛 p95 < 5 秒 |
| WD-PERF-008 | P1 | 单 session 长时间产生 5000 事件 | UI 输入无明显卡顿，浏览器内存保持有界 |
| WD-PERF-009 | P1 | 百万历史事件的列表和 cursor 翻页 | 无全表 JSON 排序、无 out-of-sort-memory |
| WD-PERF-010 | P1 | 24 小时长稳运行 | 无连接池持续增长、lease 泄漏、重复投递或僵尸 running |

## 17. 兼容性和可用性

| ID | 级别 | 操作 | 预期结果 |
|---|---|---|---|
| WD-COMP-001 | P1 | Chrome、Edge、Safari 最新版 | 任务抽屉、SSE、详情和命令可用 |
| WD-COMP-002 | P1 | API 先升级、前端旧版本 | 未知字段被忽略，旧前端不崩溃 |
| WD-COMP-003 | P0 | 前端先升级、API 旧版本 | 缺少 `allowedActions` 时不展示控制动作 |
| WD-COMP-004 | P1 | SSE 暂时不可用 | 页面明确显示降级/重连，不清空已有任务 |
| WD-COMP-005 | P1 | 键盘操作主要按钮和确认框 | 焦点可见、Tab 顺序合理、Enter/Escape 正常 |
| WD-COMP-006 | P1 | 200% 浏览器缩放和窄屏 | 文本、按钮、短 ID 不重叠或截断关键操作 |
| WD-COMP-007 | P1 | 任务标题/活动包含中英文、emoji、Markdown | 安全显示，不执行 HTML/script，不破坏布局 |
| WD-COMP-008 | P1 | 401、403、404、409、500 响应 | 用户收到可理解提示，缓存和操作状态正确回滚 |

## 18. 自动化回归命令

### Rust

```bash
cd rust
cargo fmt --all -- --check
cargo check -p web-server --features full
cargo test -p web-server --features full task_control_worker::tests
cargo test -p web-server --features full task_control::tests
cargo test -p web-server --features full agent_ops::tests
```

平台数据库用例默认使用测试内创建的临时 SQLite 数据库，不需要外部数据库服务或跳过开关：

```bash
cd rust
cargo test -p web-server --features full sqlite_ -- --nocapture
cargo test -p web-server --features full workspace_isolation -- --nocapture
```

NL2SQL 对外部 MySQL/TiDB 数据源的 connector 合同测试仍需保留；它们不属于 AOS 平台数据库。

### WebUI

```bash
cd webui
npm test -- --run
npm run typecheck
npm run i18n:check
npm run build
```

浏览器 E2E 在补齐 Playwright 配置后至少覆盖：刷新恢复、A/B 隔离、任务取消、等待输入、
通知 unknown replay、移动端布局、身份切换缓存清理和 SSE 断线续传。

## 19. 发布验收标准

- 所有 P0 和 P1 用例通过，P2 有明确记录和期限。
- 支持的任务类型根任务覆盖率为 100%。
- A/B 用户、跨租户、Bot 身份、事件、artifact、订阅和通知泄漏为 0。
- 重复平台消息、命令、任务、通知和外部副作用为 0。
- 服务重启和 worker 崩溃后，任务、命令、outbox 和 delivery 全部收敛。
- 正常长任务不被误取消，未知进度不伪造百分比或 ETA。
- 实际容量达到第 16 节 SLO，数据库无持续连接池堵塞和高成本列表查询。
- 真实平台状态与 AOS delivery 状态一致，`unknown` 不自动重发。
- migration 200 已通过正常发布流程应用并验证。

## 20. 测试记录模板

| 字段 | 内容 |
|---|---|
| Test Run ID |  |
| 版本/构建号 |  |
| migration 版本 |  |
| 测试环境 |  |
| 执行人 |  |
| 开始/结束时间 |  |
| P0 通过/失败 |  |
| P1 通过/失败 |  |
| P2 通过/失败 |  |
| 未执行及原因 |  |
| 日志/trace/artifact |  |
| 发布结论 | Go / No-Go |

单项失败记录：

```text
Case ID:
环境与账号:
taskId / shortId / sessionId / turnId:
复现步骤:
实际结果:
预期结果:
服务端日志时间段:
浏览器 Network/Console:
Bot platform message/delivery ID:
是否可稳定复现:
安全或重复副作用影响:
```
