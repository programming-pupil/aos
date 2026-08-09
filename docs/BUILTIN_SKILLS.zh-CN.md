# 内置 Skills 策略

AOS 内置了一组核心 Agent 行为契约，形式是英文 `SKILL.md` 文件。它们主要用于开源可读性、审查和后续扩展。

它们不会替代执行系统。

## Skill 可以负责什么

- Prompt 和行为契约。
- 路由策略说明。
- 输出 JSON schema 描述。
- Agent 语气、证据策略和安全提醒。
- PM、NL2SQL 参考文件、Code Studio、WatchDog、超级对抗等策略契约。

## Skill 不能负责什么

- 数据库写入和迁移。
- Runtime 进程执行。
- 取消、重试、lease、heartbeat、队列状态机。
- SQL 安全和数据源权限校验。
- 租户/用户鉴权。
- AgentOps 审计事件和 WatchDog 动作执行。
- 调度器执行和通知发送。
- Diff 校验、路径安全、ownership 检查、apply/reject 行为。

这些必须继续留在 Rust 代码里。

## 运行时安全

内置 skill 不是让 LLM 自己选择。每个场景都由代码固定绑定：

- Bot Router -> `aos-router`
- WatchDog 意图解析 -> `watchdog`
- Code Studio 普通代码模式 -> `code-studio-code`
- Code Studio Spec 模式 -> `code-studio-spec`
- NL2SQL 参考文件绑定 -> `nl2sql-reference`
- PM Assistant -> `pm-assistant`
- Super Adversarial -> `super-adversarial`

默认情况下，运行时 prompt 仍使用原来的 hardcoded fallback。这样可以保持中文命令路由、WatchDog 动作、Code Studio schema、超级对抗 prompt 的现有效果。

Prompt 渲染统一收口到 `web-server/src/routes/builtin_skills.rs` 里的 `PromptRegistry`：

- `PromptId` 把每个场景映射到固定的内置 skill 和 section。
- `PromptRegistry::render(...)` 默认返回 legacy prompt。
- 设置 `AOS_BUILTIN_SKILL_RUNTIME_PROMPTS=1` 后，Registry 会优先尝试英文 skill section；缺失时回退 legacy prompt。
- 执行逻辑仍然不进入 Registry。

如果要显式测试英文内置 skill prompt：

```bash
AOS_BUILTIN_SKILL_RUNTIME_PROMPTS=1 cargo run -p web-server --features full
```

这个开关建议只用于评测，不建议在没有 golden tests 和业务 eval 兜底前直接打开生产。

## 当前内置 Skills

| Skill | 用途 |
| --- | --- |
| `aos-router` | Bot 能力路由策略。 |
| `watchdog` | WatchDog 自然语言意图解析契约。 |
| `code-studio-code` | 普通 Code Mode 编码 Agent 契约。 |
| `code-studio-spec` | Kiro 风格 Spec Mode 契约。 |
| `nl2sql-reference` | 数据探索参考文件绑定策略。 |
| `pm-assistant` | 产运助手策略。 |
| `super-adversarial` | 多模型对抗策略。 |

## 质量门禁

修改内置 skill 后应至少通过：

```bash
cargo test -p web-server builtin_skills --features full
cargo test -p web-server golden_router_targets_core_menu_capabilities --features full
cargo test -p web-server golden_running_agents_maps_to_active_statuses --features full
cargo test -p web-server plan_prompts_preserve_json_schemas --features full
cargo check -p web-server --features full
```

如果要影响运行时行为，必须先补对应 golden tests 或 eval，再打开 runtime skill prompt。
