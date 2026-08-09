//! Built-in Agent/Workflow marketplace for AOS Code Studio.

use super::*;

pub(super) async fn search_agent_market(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<RdAgentMarketQuery>,
) -> Result<Json<RdAgentMarketSearchResponse>, AppError> {
    let installed_profiles =
        installed_agent_profile_source_items(&state.db, &claims.tenant_id).await?;
    let installed_workflows = installed_workflow_source_items(&state.db, &claims.tenant_id).await?;
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let item_type = query
        .item_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());

    let mut items = builtin_rd_agent_market_templates()
        .into_iter()
        .filter(|template| {
            item_type
                .as_deref()
                .map_or(true, |expected| template.item_type == expected)
        })
        .filter(|template| {
            q.as_deref().map_or(true, |needle| {
                template.name.to_ascii_lowercase().contains(needle)
                    || template.description.to_ascii_lowercase().contains(needle)
                    || template
                        .tags
                        .iter()
                        .any(|tag| tag.to_ascii_lowercase().contains(needle))
            })
        })
        .map(|template| {
            let install_target_id = if template.item_type == "workflow" {
                installed_workflows.get(template.id).cloned()
            } else {
                installed_profiles.get(template.id).cloned()
            };
            template.to_item(install_target_id)
        })
        .collect::<Vec<_>>();

    items.sort_by(|a, b| {
        let type_rank = |item_type: &str| if item_type == "workflow" { 0 } else { 1 };
        a.installed
            .cmp(&b.installed)
            .then_with(|| type_rank(&a.item_type).cmp(&type_rank(&b.item_type)))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(Json(RdAgentMarketSearchResponse {
        total: items.len(),
        items,
    }))
}

pub(super) async fn install_agent_market_item(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RdAgentMarketInstallRequest>,
) -> Result<Json<RdAgentMarketInstallResponse>, AppError> {
    let template = builtin_rd_agent_market_templates()
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| AppError::NotFound("rd agent market item not found".to_string()))?;

    if template.item_type == "workflow" {
        let workflow = install_builtin_rd_workflow(
            &state.db,
            &claims.tenant_id,
            &template,
            req.enabled.unwrap_or(true),
        )
        .await?;
        let item = template.to_item(Some(workflow.id.clone()));
        return Ok(Json(RdAgentMarketInstallResponse {
            item,
            agent_profile: None,
            workflow: Some(workflow),
        }));
    }

    let profile = install_builtin_rd_agent_profile(
        &state.db,
        &claims.tenant_id,
        &template,
        req.default_model.as_deref(),
        req.enabled.unwrap_or(true),
    )
    .await?;
    let item = template.to_item(Some(profile.id.clone()));
    Ok(Json(RdAgentMarketInstallResponse {
        item,
        agent_profile: Some(profile),
        workflow: None,
    }))
}

#[derive(Debug, Clone)]
struct RdAgentMarketTemplate {
    id: &'static str,
    item_type: &'static str,
    name: &'static str,
    description: &'static str,
    tags: Vec<&'static str>,
    role_prompt: Option<&'static str>,
    allowed_tools: Option<Value>,
    workflow_definition: Option<Value>,
}

impl RdAgentMarketTemplate {
    fn to_item(&self, install_target_id: Option<String>) -> RdAgentMarketItemDto {
        RdAgentMarketItemDto {
            id: self.id.to_string(),
            item_type: self.item_type.to_string(),
            name: self.name.to_string(),
            description: self.description.to_string(),
            tags: self.tags.iter().map(|tag| (*tag).to_string()).collect(),
            source: "aos".to_string(),
            installed: install_target_id.is_some(),
            install_target_id,
        }
    }
}

fn builtin_rd_agent_market_templates() -> Vec<RdAgentMarketTemplate> {
    vec![
        RdAgentMarketTemplate {
            id: "rust-review-agent",
            item_type: "agent",
            name: "Rust Review Agent",
            description: "面向 Rust 项目的代码审查 Agent，重点检查 ownership、错误处理、并发安全、测试缺口和可维护性。",
            tags: vec!["rust", "review", "quality"],
            role_prompt: Some("你是资深 Rust Review Agent。审查时优先输出 findings，按严重级别排序，必须包含文件路径、行号线索、风险、建议修复方式和缺失测试。不要为了反驳而反驳，正确的实现要明确认可。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "react-frontend-fix-agent",
            item_type: "agent",
            name: "React Frontend Fix Agent",
            description: "面向 React/TypeScript 前端修复，关注组件状态、i18n、可访问性、构建错误和视觉一致性。",
            tags: vec!["react", "typescript", "frontend", "i18n"],
            role_prompt: Some("你是 React/TypeScript 前端修复 Agent。先定位组件、状态流、API 类型和 i18n key，再给出最小可审查 Diff。必须考虑桌面/移动端、空状态、loading/error 状态和构建类型错误。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "security-review-agent",
            item_type: "agent",
            name: "Security Review Agent",
            description: "安全审查 Agent，关注鉴权、越权、敏感信息、命令执行、路径穿越、SSRF 和注入风险。",
            tags: vec!["security", "review", "audit"],
            role_prompt: Some("你是安全审查 Agent。优先检查鉴权/租户隔离、权限边界、敏感信息泄漏、命令执行、路径穿越、SSRF、SQL/Prompt 注入和供应链风险。输出 findings-first，包含严重级别、证据、影响和修复建议。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "test-repair-agent",
            item_type: "agent",
            name: "Test Repair Agent",
            description: "测试失败修复 Agent，聚焦失败日志、真实文件、候选 Diff 和最小修复。",
            tags: vec!["test", "repair", "ci"],
            role_prompt: Some("你是测试失败修复 Agent。必须先读取失败日志、最近 Diff 和相关真实文件，再定位根因。只生成最小修复 Diff，不要掩盖测试，不要删除有效断言。输出包含复现原因、修复点和验证命令。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "architecture-agent",
            item_type: "agent",
            name: "Architecture Agent",
            description: "架构理解与方案设计 Agent，适合复杂需求开始前梳理边界、依赖、风险、验证路径和拆分计划。",
            tags: vec!["architecture", "planning", "design"],
            role_prompt: Some("你是 Architecture Agent。先读取仓库结构、关键配置、入口文件和相关模块，输出面向实现的计划：目标、相关文件、数据流/调用链、风险、需要保护的兼容性、推荐验证命令。不要直接写代码；如果信息不足，列出最小必要补充文件。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "coding-agent",
            item_type: "agent",
            name: "Coding Agent",
            description: "通用实现 Agent，强调真实仓库上下文、最小 Diff、可回滚改动、测试闭环和清晰总结。",
            tags: vec!["coding", "implementation", "diff"],
            role_prompt: Some("你是 Coding Agent。必须基于真实仓库上下文修改代码，先给计划，再生成最小可审查 Diff。不要做无关重构，不要静默覆盖文件，不要删除有效测试。输出包含变更摘要、涉及文件、验证方式和残留风险；遇到测试失败要读取错误、相关文件和候选 Diff 后再修复。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "review-agent",
            item_type: "agent",
            name: "Review Agent",
            description: "通用代码审查 Agent，按严重级别输出文件/行级 findings、回归风险和缺失测试。",
            tags: vec!["review", "quality", "risk"],
            role_prompt: Some("你是 Review Agent。采用 findings-first 风格：只列真实问题，不为了凑数量输出泛泛建议。每个 finding 必须包含严重级别、文件路径、行号线索、问题证据、影响、建议修复和应补测试。最后简短说明总体风险和已验证内容。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "test-agent",
            item_type: "agent",
            name: "Test Agent",
            description: "测试策略与验证 Agent，负责选择验证命令、分析 stdout/stderr、判断失败是否由本次改动引入。",
            tags: vec!["test", "validation", "ci"],
            role_prompt: Some("你是 Test Agent。根据项目语言、变更文件和已有脚本选择最小充分验证命令。分析测试输出时区分环境问题、历史问题和本次回归；不要建议跳过有效测试。输出验证命令、结果解释、失败根因和下一步修复建议。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "pr-agent",
            item_type: "agent",
            name: "PR Agent",
            description: "PR 产物 Agent，生成可直接用于代码评审的标题、描述、测试结果、风险说明和回滚建议。",
            tags: vec!["pr", "documentation", "release"],
            role_prompt: Some("你是 PR Agent。基于需求、计划、Diff 和测试结果生成 PR 标题与描述。内容必须包含背景、主要改动、验证结果、兼容性影响、风险与回滚方式。不要夸大，不要隐藏未运行测试或失败项。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "java-spring-agent",
            item_type: "agent",
            name: "Java Spring Agent",
            description: "Java/Spring Boot 后端 Agent，关注分层边界、事务、异常、并发、配置、测试和接口兼容。",
            tags: vec!["java", "spring", "backend"],
            role_prompt: Some("你是 Java/Spring Boot Agent。优先检查 Controller/Service/Repository 分层、事务边界、异常映射、参数校验、序列化兼容、配置加载、并发安全和测试覆盖。生成 Diff 时保持接口兼容，避免大范围重构；输出建议验证命令，如 Maven/Gradle 单测或指定模块测试。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "python-agent",
            item_type: "agent",
            name: "Python Service Agent",
            description: "Python/FastAPI/Django 服务端 Agent，关注类型、异步、依赖、异常、测试和数据处理边界。",
            tags: vec!["python", "fastapi", "django", "backend"],
            role_prompt: Some("你是 Python Service Agent。先识别项目框架和测试工具，关注类型注解、异步/同步边界、异常处理、依赖注入、数据库访问、数据校验和测试隔离。修改要最小化，避免隐式全局状态和破坏兼容；输出 pytest/ruff/mypy 等建议验证命令。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "sql-migration-review-agent",
            item_type: "agent",
            name: "SQL Migration Review Agent",
            description: "数据库迁移审查 Agent，检查 DDL/DML 安全、锁表风险、回滚方案、索引和兼容性。",
            tags: vec!["sql", "migration", "database", "review"],
            role_prompt: Some("你是 SQL Migration Review Agent。重点检查迁移是否幂等、是否可能锁大表、是否破坏历史数据、是否需要回滚脚本、索引是否合理、字段默认值/空值/字符集是否安全、应用代码是否兼容新旧 schema。输出 findings-first 和安全上线建议。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "devops-ci-agent",
            item_type: "agent",
            name: "DevOps CI Agent",
            description: "CI/CD 与部署 Agent，关注构建脚本、缓存、环境变量、容器镜像、安全密钥和发布失败定位。",
            tags: vec!["devops", "ci", "docker", "release"],
            role_prompt: Some("你是 DevOps CI Agent。优先读取 package/cargo/maven/gradle、Dockerfile、compose、CI YAML 和部署脚本。检查构建缓存、环境变量、密钥泄漏、镜像层、权限、超时和失败日志。输出最小修复 Diff、验证命令和发布风险。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "performance-agent",
            item_type: "agent",
            name: "Performance Agent",
            description: "性能优化 Agent，定位慢查询、重复 IO、过度渲染、N+1、缓存缺失和大上下文/token 消耗。",
            tags: vec!["performance", "optimization", "scalability"],
            role_prompt: Some("你是 Performance Agent。先定位热点路径、循环、IO、数据库查询、缓存、并发和前端渲染边界。优先提出低风险优化和可度量验证方式，不用牺牲正确性换速度。输出瓶颈假设、证据、最小 Diff、指标验证和回滚策略。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "accessibility-ux-agent",
            item_type: "agent",
            name: "Accessibility UX Agent",
            description: "前端可用性与可访问性 Agent，关注键盘操作、语义、空态、错误态、响应式和可读性。",
            tags: vec!["frontend", "accessibility", "ux"],
            role_prompt: Some("你是 Accessibility UX Agent。检查交互是否支持键盘和屏幕阅读器，按钮/表单/弹窗是否有清晰语义、焦点、loading/error/empty 状态，移动端是否可用。修改时遵循现有设计系统，不引入突兀视觉风格。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "legacy-refactor-agent",
            item_type: "agent",
            name: "Legacy Refactor Agent",
            description: "遗留代码渐进式重构 Agent，强调行为保持、分步迁移、测试护栏和低风险回滚。",
            tags: vec!["refactor", "legacy", "maintainability"],
            role_prompt: Some("你是 Legacy Refactor Agent。重构前必须说明当前行为、调用方、风险和测试护栏。只做可审查的小步改动，避免大爆炸重写；优先提取函数、收敛重复、补类型/测试和保留兼容层。输出行为不变性说明和回滚路径。"),
            allowed_tools: Some(json!(["read_file", "grep_search", "glob_search", "rd_validate_diff"])),
            workflow_definition: None,
        },
        RdAgentMarketTemplate {
            id: "spec-to-pr-workflow",
            item_type: "workflow",
            name: "Spec to PR Workflow",
            description: "从需求/规格到实现、测试、Review 和 PR 描述的多 Agent 工作流模板。",
            tags: vec!["spec", "pr", "multi-agent"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "architecture", "agent": "Architecture Agent", "mode": "ask", "goal": "理解仓库结构、相关文件、风险和验证命令"},
                    {"id": "implementation", "agent": "Coding Agent", "mode": "modify", "goal": "生成可审查 Diff，不直接写主仓库"},
                    {"id": "test", "agent": "Test Agent", "mode": "explain", "goal": "运行或建议测试命令，分析失败"},
                    {"id": "review", "agent": "Review Agent", "mode": "review", "goal": "输出 findings-first 审查和风险"},
                    {"id": "pr_draft", "agent": "PR Agent", "mode": "ask", "goal": "生成 PR 标题、描述、测试结果和风险说明"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "failed-test-repair-workflow",
            item_type: "workflow",
            name: "Failed Test Repair Workflow",
            description: "针对失败测试的定位、修复、复测和 Review 工作流模板。",
            tags: vec!["test", "repair", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "failure_analysis", "agent": "Test Repair Agent", "mode": "explain", "goal": "读取测试日志和相关文件，定位失败根因"},
                    {"id": "patch", "agent": "Coding Agent", "mode": "modify", "goal": "生成最小修复 Diff"},
                    {"id": "rerun", "agent": "Test Agent", "mode": "explain", "goal": "复测并解释残余失败"},
                    {"id": "review", "agent": "Review Agent", "mode": "review", "goal": "确认没有掩盖测试或引入回归"}
                ],
                "maxRepairRounds": 3
            })),
        },
        RdAgentMarketTemplate {
            id: "frontend-i18n-workflow",
            item_type: "workflow",
            name: "Frontend i18n Workflow",
            description: "前端国际化巡检工作流，覆盖缺失 key、硬编码文案、英文模式弹窗按钮和构建验证。",
            tags: vec!["frontend", "i18n", "react"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "scan", "agent": "React Frontend Fix Agent", "mode": "ask", "goal": "扫描页面硬编码文案和缺失 i18n key"},
                    {"id": "patch", "agent": "React Frontend Fix Agent", "mode": "modify", "goal": "补齐中英文 key 并修复按钮/弹窗文案"},
                    {"id": "build", "agent": "Test Agent", "mode": "explain", "goal": "运行或建议 webui build 验证"},
                    {"id": "review", "agent": "Review Agent", "mode": "review", "goal": "检查翻译一致性和 UI 回归"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "security-hardening-workflow",
            item_type: "workflow",
            name: "Security Hardening Workflow",
            description: "从攻击面梳理、安全审查、最小修复到回归验证的安全加固工作流。",
            tags: vec!["security", "hardening", "review"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "threat_model", "agent": "Architecture Agent", "mode": "ask", "goal": "识别入口、权限边界、外部输入、敏感数据和高风险文件"},
                    {"id": "security_review", "agent": "Security Review Agent", "mode": "review", "goal": "输出真实安全 findings，包含证据和影响"},
                    {"id": "patch", "agent": "Coding Agent", "mode": "modify", "goal": "生成最小安全修复 Diff，保持兼容"},
                    {"id": "regression", "agent": "Test Agent", "mode": "explain", "goal": "给出安全回归验证和必要测试"},
                    {"id": "pr", "agent": "PR Agent", "mode": "ask", "goal": "生成安全修复 PR 描述和风险说明"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "java-spring-change-workflow",
            item_type: "workflow",
            name: "Java Spring Change Workflow",
            description: "Java/Spring 服务端需求变更工作流，覆盖架构理解、实现、单测和 Review。",
            tags: vec!["java", "spring", "backend", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "analysis", "agent": "Architecture Agent", "mode": "ask", "goal": "理解 Controller/Service/Repository、配置和接口兼容风险"},
                    {"id": "implementation", "agent": "Java Spring Agent", "mode": "modify", "goal": "生成最小后端变更 Diff"},
                    {"id": "test", "agent": "Test Agent", "mode": "explain", "goal": "建议 Maven/Gradle/模块级测试并分析结果"},
                    {"id": "review", "agent": "Review Agent", "mode": "review", "goal": "检查事务、异常、参数校验、兼容性和缺失测试"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "frontend-feature-workflow",
            item_type: "workflow",
            name: "Frontend Feature Workflow",
            description: "React/TypeScript 功能开发工作流，覆盖状态、接口、i18n、可访问性、构建验证。",
            tags: vec!["frontend", "react", "typescript", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "ux_analysis", "agent": "Accessibility UX Agent", "mode": "ask", "goal": "梳理页面状态、交互、空态、错误态和响应式风险"},
                    {"id": "implementation", "agent": "React Frontend Fix Agent", "mode": "modify", "goal": "实现功能并补齐 i18n/类型/状态处理"},
                    {"id": "build", "agent": "Test Agent", "mode": "explain", "goal": "建议 TypeScript/build/lint 验证"},
                    {"id": "review", "agent": "Review Agent", "mode": "review", "goal": "检查 UI 回归、可访问性和缺失测试"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "database-migration-workflow",
            item_type: "workflow",
            name: "Database Migration Workflow",
            description: "数据库迁移安全工作流，覆盖 schema 风险、应用兼容、回滚和上线验证。",
            tags: vec!["database", "sql", "migration", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "impact", "agent": "Architecture Agent", "mode": "ask", "goal": "定位迁移脚本、数据访问代码和兼容边界"},
                    {"id": "migration_review", "agent": "SQL Migration Review Agent", "mode": "review", "goal": "检查锁表、幂等、默认值、索引、回滚和数据风险"},
                    {"id": "patch", "agent": "Coding Agent", "mode": "modify", "goal": "修复迁移或应用兼容问题"},
                    {"id": "validation", "agent": "Test Agent", "mode": "explain", "goal": "给出迁移验证、回滚演练和应用测试建议"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "performance-optimization-workflow",
            item_type: "workflow",
            name: "Performance Optimization Workflow",
            description: "性能问题定位与低风险优化工作流，强调证据、指标、最小改动和回滚。",
            tags: vec!["performance", "optimization", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "baseline", "agent": "Performance Agent", "mode": "ask", "goal": "建立瓶颈假设、热点路径和指标验证方式"},
                    {"id": "patch", "agent": "Performance Agent", "mode": "modify", "goal": "生成低风险性能优化 Diff"},
                    {"id": "validation", "agent": "Test Agent", "mode": "explain", "goal": "说明性能验证、正确性测试和可能的副作用"},
                    {"id": "review", "agent": "Review Agent", "mode": "review", "goal": "检查优化是否牺牲正确性或可维护性"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "ci-failure-recovery-workflow",
            item_type: "workflow",
            name: "CI Failure Recovery Workflow",
            description: "CI/CD 失败恢复工作流，覆盖日志定位、构建脚本修复、复测和发布风险。",
            tags: vec!["ci", "devops", "release", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "failure_analysis", "agent": "DevOps CI Agent", "mode": "explain", "goal": "读取 CI 配置、构建脚本和失败日志，定位失败根因"},
                    {"id": "patch", "agent": "DevOps CI Agent", "mode": "modify", "goal": "生成最小 CI/构建修复 Diff"},
                    {"id": "rerun", "agent": "Test Agent", "mode": "explain", "goal": "建议复测命令并解释残余失败"},
                    {"id": "release_risk", "agent": "PR Agent", "mode": "ask", "goal": "总结发布风险、验证证据和回滚方式"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "legacy-refactor-workflow",
            item_type: "workflow",
            name: "Legacy Refactor Workflow",
            description: "遗留模块渐进式重构工作流，先建测试护栏，再小步重构和 Review。",
            tags: vec!["legacy", "refactor", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "behavior_map", "agent": "Architecture Agent", "mode": "ask", "goal": "梳理当前行为、调用方、隐式约束和高风险路径"},
                    {"id": "guardrail", "agent": "Test Agent", "mode": "explain", "goal": "建议重构前测试护栏和验证命令"},
                    {"id": "refactor", "agent": "Legacy Refactor Agent", "mode": "modify", "goal": "生成行为保持的小步重构 Diff"},
                    {"id": "review", "agent": "Review Agent", "mode": "review", "goal": "确认没有行为变化、接口破坏和测试缺口"}
                ]
            })),
        },
        RdAgentMarketTemplate {
            id: "incident-hotfix-workflow",
            item_type: "workflow",
            name: "Incident Hotfix Workflow",
            description: "线上故障热修工作流，强调快速定位、最小修复、验证、风险说明和回滚。",
            tags: vec!["incident", "hotfix", "release", "workflow"],
            role_prompt: None,
            allowed_tools: None,
            workflow_definition: Some(json!({
                "version": 1,
                "stages": [
                    {"id": "triage", "agent": "Architecture Agent", "mode": "ask", "goal": "快速定位故障路径、影响范围、可疑变更和回滚可能性"},
                    {"id": "hotfix", "agent": "Coding Agent", "mode": "modify", "goal": "生成最小热修 Diff，不做无关优化"},
                    {"id": "verification", "agent": "Test Agent", "mode": "explain", "goal": "给出快速验证、回归测试和监控观察点"},
                    {"id": "pr", "agent": "PR Agent", "mode": "ask", "goal": "生成热修说明、风险、回滚和后续补偿任务"}
                ]
            })),
        },
    ]
}

async fn installed_agent_profile_source_items(
    db: &SqlitePool,
    tenant_id: &str,
) -> Result<HashMap<String, String>, AppError> {
    let rows = sqlx::query(
        "SELECT id, source_item_id FROM rd_agent_profiles WHERE tenant_id = ? AND source = 'aos' AND source_item_id IS NOT NULL",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            row.get::<Option<String>, _>("source_item_id")
                .map(|source_item_id| (source_item_id, row.get::<String, _>("id")))
        })
        .collect())
}

async fn installed_workflow_source_items(
    db: &SqlitePool,
    tenant_id: &str,
) -> Result<HashMap<String, String>, AppError> {
    let rows = sqlx::query("SELECT id, source_item_id FROM rd_agent_workflows WHERE tenant_id = ?")
        .bind(tenant_id)
        .fetch_all(db)
        .await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            row.get::<Option<String>, _>("source_item_id")
                .map(|source_item_id| (source_item_id, row.get::<String, _>("id")))
        })
        .collect())
}

async fn install_builtin_rd_agent_profile(
    db: &SqlitePool,
    tenant_id: &str,
    template: &RdAgentMarketTemplate,
    default_model: Option<&str>,
    enabled: bool,
) -> Result<RdAgentProfileDto, AppError> {
    if let Some(existing_id) = installed_agent_profile_source_items(db, tenant_id)
        .await?
        .get(template.id)
        .cloned()
    {
        return get_agent_profile_row(db, tenant_id, &existing_id).await;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let result = sqlx::query("INSERT INTO rd_agent_profiles (id, tenant_id, name, role_prompt, allowed_tools, default_model, enabled, source, source_item_id) VALUES (?, ?, ?, ?, ?, ?, ?, 'aos', ?) ON CONFLICT DO NOTHING")
        .bind(&id)
        .bind(tenant_id)
        .bind(template.name)
        .bind(template.role_prompt.unwrap_or(template.description))
        .bind(&template.allowed_tools)
        .bind(normalize_optional(default_model))
        .bind(enabled)
        .bind(template.id)
        .execute(db)
        .await?;
    if result.rows_affected() == 0 {
        let existing_id = installed_agent_profile_source_items(db, tenant_id)
            .await?
            .get(template.id)
            .cloned()
            .ok_or_else(|| {
                AppError::Internal(
                    "agent market install conflicted without an installed item".to_string(),
                )
            })?;
        return get_agent_profile_row(db, tenant_id, &existing_id).await;
    }
    get_agent_profile_row(db, tenant_id, &id).await
}

async fn install_builtin_rd_workflow(
    db: &SqlitePool,
    tenant_id: &str,
    template: &RdAgentMarketTemplate,
    enabled: bool,
) -> Result<RdAgentWorkflowDto, AppError> {
    if let Some(existing_id) = installed_workflow_source_items(db, tenant_id)
        .await?
        .get(template.id)
        .cloned()
    {
        return get_agent_workflow_row(db, tenant_id, &existing_id).await;
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO rd_agent_workflows (id, tenant_id, name, description, definition_json, source, source_item_id, enabled) VALUES (?, ?, ?, ?, ?, 'aos', ?, ?)")
        .bind(&id)
        .bind(tenant_id)
        .bind(template.name)
        .bind(template.description)
        .bind(template.workflow_definition.clone().unwrap_or_else(|| json!({})))
        .bind(template.id)
        .bind(enabled)
        .execute(db)
        .await?;
    get_agent_workflow_row(db, tenant_id, &id).await
}

#[cfg(test)]
mod market_tests {
    use super::*;

    #[tokio::test]
    async fn market_agent_identity_does_not_claim_or_delete_a_manual_namesake() {
        let db = crate::test_sqlite_pool().await;
        let tenant_id = uuid::Uuid::new_v4().to_string();
        let manual_id = uuid::Uuid::new_v4().to_string();
        let template = builtin_rd_agent_market_templates()
            .into_iter()
            .find(|item| item.item_type == "agent")
            .expect("built-in agent template");

        sqlx::query("INSERT INTO rd_agent_profiles (id, tenant_id, name, role_prompt, enabled) VALUES (?, ?, ?, 'manual profile', 1)")
            .bind(&manual_id)
            .bind(&tenant_id)
            .bind(template.name)
            .execute(&db)
            .await
            .expect("insert manual namesake");
        assert!(installed_agent_profile_source_items(&db, &tenant_id)
            .await
            .expect("load installed market agents")
            .is_empty());

        let installed = install_builtin_rd_agent_profile(&db, &tenant_id, &template, None, true)
            .await
            .expect("install market agent");
        assert_ne!(installed.id, manual_id);
        assert_eq!(
            installed_agent_profile_source_items(&db, &tenant_id)
                .await
                .expect("reload installed market agents")
                .get(template.id),
            Some(&installed.id)
        );

        sqlx::query("DELETE FROM rd_agent_profiles WHERE tenant_id = ? AND id = ?")
            .bind(&tenant_id)
            .bind(&installed.id)
            .execute(&db)
            .await
            .expect("uninstall market agent");
        let manual_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM rd_agent_profiles WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&manual_id)
        .fetch_one(&db)
        .await
        .expect("count manual namesake");
        assert_eq!(manual_count, 1);
        assert!(installed_agent_profile_source_items(&db, &tenant_id)
            .await
            .expect("reload market agents after uninstall")
            .is_empty());
        db.close().await;
    }
}
