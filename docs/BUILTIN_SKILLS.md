# Built-In Skills Strategy

AOS ships several built-in skill contracts for core agent behavior. They are
English, repo-owned `SKILL.md` files intended for open-source readability,
reviewability, and future extension.

They do not replace the execution system.

## What Skills Can Own

- Prompt and behavior contracts.
- Routing policy documentation.
- Output schema descriptions.
- Agent tone, evidence policy, and safety reminders.
- Strategy contracts for PM, NL2SQL references, Code Studio, WatchDog, and Super Adversarial.

## What Skills Must Not Own

- Database writes and migrations.
- Runtime process execution.
- Cancel, retry, lease, heartbeat, and queue state machines.
- SQL safety and datasource permission checks.
- Tenant/user authorization.
- AgentOps audit events and WatchDog action execution.
- Scheduler execution and notification delivery.
- Diff validation, path safety, ownership checks, and apply/reject behavior.

Those remain in Rust code.

## Runtime Safety

Built-in skill documents are not selected by the LLM. Each scenario is bound by
code to a specific skill:

- Bot Router -> `aos-router`
- WatchDog intent -> `watchdog`
- Code Studio Code Mode -> `code-studio-code`
- Code Studio Spec Mode -> `code-studio-spec`
- NL2SQL reference binding -> `nl2sql-reference`
- PM Assistant -> `pm-assistant`
- Super Adversarial -> `super-adversarial`

By default, runtime prompts still use hardcoded fallback prompts. This preserves
existing behavior for Chinese command routing, WatchDog actions, Code Studio
schemas, and debate prompts.

Prompt rendering is centralized through `PromptRegistry` in
`web-server/src/routes/builtin_skills.rs`:

- `PromptId` maps each scenario to a built-in skill and section.
- `PromptRegistry::render(...)` returns the legacy prompt by default.
- When `AOS_BUILTIN_SKILL_RUNTIME_PROMPTS=1` is enabled, the registry tries the
  English skill section first and falls back to the legacy prompt if the section
  is missing.
- Execution logic still stays outside the registry.

To explicitly test English built-in skill prompts at runtime:

```bash
AOS_BUILTIN_SKILL_RUNTIME_PROMPTS=1 cargo run -p web-server --features full
```

Use that opt-in only when evaluating prompt behavior. Keep it off for production
until golden tests and product evals pass for your deployment language mix.

## Current Built-In Skills

| Skill | Purpose |
| --- | --- |
| `aos-router` | Bot capability routing policy. |
| `watchdog` | WatchDog natural-language intent parsing contract. |
| `code-studio-code` | Ordinary Code Mode coding agent contract. |
| `code-studio-spec` | Kiro-style Spec Mode contract. |
| `nl2sql-reference` | Data Exploration reference binding policy. |
| `pm-assistant` | Product operations assistant strategy. |
| `super-adversarial` | Multi-model debate strategy. |

## Quality Gates

Built-in skill changes should pass:

```bash
cargo test -p web-server builtin_skills --features full
cargo test -p web-server golden_router_targets_core_menu_capabilities --features full
cargo test -p web-server golden_running_agents_maps_to_active_statuses --features full
cargo test -p web-server plan_prompts_preserve_json_schemas --features full
cargo check -p web-server --features full
```

For behavior-sensitive changes, add or update golden tests before enabling skill
runtime prompts.
