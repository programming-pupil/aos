# 安全政策

## 支持范围

安全报告可以覆盖：

- 登录认证、权限绕过、租户隔离问题。
- Secret 存储、API Key 加密、Token 泄露、日志脱敏。
- Agent runtime 命令执行、workspace 隔离、路径逃逸、进程取消。
- Bot 网关入站校验、重放/去重、出站发送、平台凭证处理。
- NL2SQL 数据源访问、SQL 安全、脱敏和执行权限。

## 如何报告漏洞

请不要用公开 issue 报告疑似安全漏洞。

可使用：

- GitHub 仓库 **Security → Report a vulnerability** 私密报告入口。
- 仅当仓库尚未启用私密漏洞报告时，再通过非公开渠道联系维护者。

请包含：

- 受影响版本或 commit。
- 复现步骤。
- 影响范围和组件。
- 已脱敏的日志/截图。
- 问题是否已经公开。

项目公开维护邮箱建立后，我们目标是在 72 小时内确认收到报告。

## Secret 处理

- 不要提交 `.env`、API Key、Bot Token、数据库 dump、runtime artifact、本地
  workspace 或包含凭证的日志。
- `.env.example` 只能使用占位值。
- AOS 会加密表内 API Key，但一旦泄露，运维方仍必须立即轮换。
- 反向代理和访问日志应对 WebSocket 请求关闭 query string 记录或做脱敏；
  浏览器 WebSocket 鉴权可能通过 query string 携带短期凭证。

## Runtime 安全说明

默认 local-process runtime 面向本地开发和可信运维环境。它可以在任务 workspace
中执行命令。生产环境建议：

- 使用最小权限 OS 用户。
- workspace 保持在 `AOS_DATA_DIR` 下。
- 审查命令 allowlist 和 timeout。
- 可用时优先使用 sandbox runtime profile。
- 不要把敏感宿主机目录挂载到 Agent workspace。
