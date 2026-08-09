//! Lightweight source-contract tests for AOS.
//!
//! Keep these tests dependency-free so CI can validate route/schema contracts
//! without linking the full web-server test binary.

#[cfg(test)]
mod tests {
    const AGENT_RUNTIME: &str = include_str!("../../web-server/src/routes/agent_runtime.rs");
    const AGENT_OPS: &str = include_str!("../../web-server/src/routes/agent_ops.rs");
    const AGENT_OPS_TYPES: &str = include_str!("../../web-server/src/routes/agent_ops_types.rs");
    const AGENT_OPS_ADAPTERS: &str =
        include_str!("../../web-server/src/routes/agent_ops_adapters.rs");
    const AGENT_OPS_WATCHDOG_INTENT: &str =
        include_str!("../../web-server/src/routes/agent_ops_watchdog_intent.rs");
    const AGENT_CHAT_ADVERSARIAL: &str =
        include_str!("../../web-server/src/routes/agent/agent_chat_adversarial.rs");
    const AGENT_CHAT_ADVERSARIAL_DOMAIN: &str =
        include_str!("../../web-server/src/routes/agent/agent_chat_adversarial_domain.rs");
    const AGENT_CHAT_ADVERSARIAL_SUPPORT: &str =
        include_str!("../../web-server/src/routes/agent/agent_chat_adversarial_support.rs");
    const BOT_AGENTS: &str = include_str!("../../web-server/src/routes/bot_agents.rs");
    const BOT_AGENTS_ROUTER: &str =
        include_str!("../../web-server/src/routes/bot_agents_router.rs");
    const RD_WORKBENCH: &str = include_str!("../../web-server/src/routes/rd/workbench.rs");
    const RD_CODE_INTEL: &str = include_str!("../../web-server/src/routes/rd/code_intel.rs");
    const RD_PREVIEW_SESSIONS: &str =
        include_str!("../../web-server/src/routes/rd/preview_sessions.rs");
    const RD_TASK_LIFECYCLE: &str =
        include_str!("../../web-server/src/routes/rd/task_lifecycle.rs");
    const RD_COMMANDS: &str = include_str!("../../web-server/src/routes/rd/commands.rs");
    const RD_RUNTIME_EXECUTION: &str =
        include_str!("../../web-server/src/routes/rd/runtime_execution.rs");
    const RD_TASK_EXECUTOR: &str = include_str!("../../web-server/src/routes/rd/task_executor.rs");
    const RD_UTILS: &str = include_str!("../../web-server/src/routes/rd/utils.rs");
    const RD_SPECS: &str = include_str!("../../web-server/src/routes/rd/specs.rs");
    const RD_STUDIO: &str = include_str!("../../../../webui/src/pages/RdStudio.tsx");
    const RD_CODE_EDITOR_PANEL: &str =
        include_str!("../../../../webui/src/pages/rdStudio/CodeEditorPanel.tsx");
    const RD_FILE_PREVIEW: &str =
        include_str!("../../../../webui/src/pages/rdStudio/FilePreview.tsx");
    const RD_PREVIEW_PANEL: &str =
        include_str!("../../../../webui/src/pages/rdStudio/PreviewPanel.tsx");
    const RD_PREVIEW_LOGS_PANEL: &str =
        include_str!("../../../../webui/src/pages/rdStudio/PreviewLogsPanel.tsx");
    const RD_QUICK_OPEN_PALETTE: &str =
        include_str!("../../../../webui/src/pages/rdStudio/QuickOpenPalette.tsx");
    const RD_AGENT_TIMELINE: &str =
        include_str!("../../../../webui/src/pages/rdStudio/AgentTimeline.tsx");
    const RD_PLAN_TASK_BOARD: &str =
        include_str!("../../../../webui/src/pages/rdStudio/TaskItemBoard.tsx");
    const WATCHDOG_PAGE: &str = include_str!("../../../../webui/src/pages/WatchDog.tsx");
    const BOT_AGENTS_PAGE: &str = include_str!("../../../../webui/src/pages/BotAgents.tsx");
    const SUPER_ADVERSARIAL_PAGE: &str =
        include_str!("../../../../webui/src/pages/SuperAdversarial.tsx");
    const SUPER_ADVERSARIAL_CSS: &str =
        include_str!("../../../../webui/src/pages/SuperAdversarial.css");
    const AGENTOPS_WATCHDOG_DESIGN: &str =
        include_str!("../../../../docs/AGENTOPS_WATCHDOG_DESIGN.md");
    const ROUTER_WATCHDOG_GOLDEN: &str =
        include_str!("../../../../docs/evals/router_watchdog_golden.jsonl");
    const ZH_CN: &str = include_str!("../../../../webui/src/locales/zh-CN.json");
    const EN_US: &str = include_str!("../../../../webui/src/locales/en-US.json");

    #[test]
    fn runtime_routes_expose_recovery_endpoint() {
        assert!(AGENT_RUNTIME.contains("/sessions/recover"));
        assert!(AGENT_RUNTIME.contains("recover_stale_runtime_sessions"));
        assert!(AGENT_RUNTIME.contains("runtime.session.stale"));
        assert!(AGENT_RUNTIME.contains("req: Option<Json<RuntimeRecoverRequest>>"));
    }

    #[test]
    fn runtime_command_persists_stdout_stderr_artifacts() {
        assert!(AGENT_RUNTIME.contains("write_runtime_text_artifact"));
        assert!(AGENT_RUNTIME.contains("INSERT INTO agent_runtime_artifacts"));
        assert!(AGENT_RUNTIME.contains("stdout_artifact_id = ?"));
        assert!(AGENT_RUNTIME.contains("stderr_artifact_id = ?"));
        assert!(AGENT_RUNTIME.contains("fs::write(&absolute_path, content)"));
    }

    #[test]
    fn runtime_cancel_marks_pending_processes_cancelled() {
        assert!(AGENT_RUNTIME.contains("RUNTIME_PROCESS_STATUS_CANCELLED"));
        assert!(AGENT_RUNTIME.contains("WHEN status = 'queued' THEN 'cancelled'"));
        assert!(AGENT_RUNTIME.contains("runtime cancellation requested"));
        assert!(AGENT_RUNTIME.contains("status IN ('queued','running','cancelling')"));
        assert!(AGENT_RUNTIME.contains("runtime_command_cancel_was_requested"));
        assert!(AGENT_RUNTIME.contains("runtime command was cancelled"));
        assert!(AGENT_RUNTIME.contains("runtime.command.cancelled"));
        let completed_at_pos = AGENT_RUNTIME
            .find("completed_at = CASE WHEN status = 'queued'")
            .expect("runtime cancel should stamp queued process completion");
        let status_update_pos = AGENT_RUNTIME
            .find("status = CASE\n                WHEN status = 'queued' THEN 'cancelled'")
            .expect("runtime cancel should update queued process status");
        assert!(
            completed_at_pos < status_update_pos,
            "runtime cancel must calculate completed_at from the original process status"
        );
        assert!(RD_COMMANDS.contains("RUNTIME_PROCESS_STATUS_CANCELLED"));
        assert!(RD_COMMANDS.contains("\"cancelled\".to_string()"));
        assert!(RD_RUNTIME_EXECUTION.contains("verify.status == \"cancelled\""));
        assert!(RD_RUNTIME_EXECUTION.contains("candidate_verify_cancelled"));
        assert!(RD_RUNTIME_EXECUTION.contains("停止后续自动修复"));
        assert!(RD_RUNTIME_EXECUTION.contains("return Err(rd_task_cancelled_error())"));
        assert!(RD_TASK_EXECUTOR.contains("rd_error_is_cancelled(&error)"));
        assert!(RD_TASK_EXECUTOR.contains("return Err(error);"));
        assert!(RD_TASK_LIFECYCLE.contains("rd_error_is_cancelled(&error)"));
        assert!(RD_TASK_LIFECYCLE.contains("rd.task.cancelled"));
        assert!(RD_TASK_LIFECYCLE.contains("mark_task_cancelled"));
        assert!(RD_UTILS.contains("RD_TASK_CANCELLED_ERROR"));
        assert!(RD_UTILS.contains("\"errorKind\": \"cancelled\""));
    }

    #[test]
    fn runtime_artifact_detail_route_reads_safe_preview() {
        assert!(AGENT_RUNTIME.contains("/sessions/{id}/artifacts/{artifact_id}"));
        assert!(AGENT_RUNTIME.contains("read_runtime_artifact_content"));
        assert!(AGENT_RUNTIME.contains("ARTIFACT_DETAIL_MAX_BYTES"));
        assert!(AGENT_RUNTIME
            .contains("ensure_workspace_child_safe(Path::new(workspace_root), &absolute_path)"));
    }

    #[test]
    fn agent_ops_selects_cast_json_columns_to_text() {
        assert!(AGENT_OPS_TYPES.contains("CAST(at.input_json AS TEXT) AS input_json"));
        assert!(AGENT_OPS_TYPES.contains("CAST(at.output_json AS TEXT) AS output_json"));
        assert!(AGENT_OPS_TYPES.contains("CAST(metadata_json AS TEXT) AS metadata_json"));
    }

    #[test]
    fn watchdog_stale_queries_include_recovered_stale_tasks() {
        assert!(AGENT_OPS.contains("at.status = 'stale'"));
        assert!(AGENT_OPS.contains("status = 'stale' OR"));
    }

    #[test]
    fn agent_task_creation_uses_durable_idempotency_keys() {
        assert!(AGENT_OPS_TYPES.contains("pub idempotency_key: Option<String>"));
        assert!(AGENT_OPS_TYPES.contains("pub struct CreateAgentTaskOutcome"));
        assert!(AGENT_OPS.contains("pub async fn create_task_with_outcome"));
        assert!(AGENT_OPS.contains("existing_task_for_idempotency_key"));
        assert!(AGENT_OPS.contains("record_idempotency_reuse"));
        assert!(AGENT_OPS.contains("db.is_unique_violation()"));
        assert!(AGENT_OPS.contains(
            "SELECT id FROM agent_tasks WHERE tenant_id = ? AND idempotency_key = ? LIMIT 1"
        ));
        assert!(AGENT_OPS.contains("task.idempotency_reused"));
        assert!(AGENT_OPS.contains("max_attempts, idempotency_key, priority"));
        assert!(BOT_AGENTS.contains("let external_message_id ="));
        assert!(BOT_AGENTS.contains("fn bot_message_idempotency_key("));
        assert!(BOT_AGENTS.contains("format!(\"bot-{scope}:{}\", hex::encode(hasher.finalize()))"));
        assert!(BOT_AGENTS
            .contains("bot_message_idempotency_key(\"task\", &platform, &channel_id, message_id)"));
        assert!(BOT_AGENTS.contains("unwrap_or_else(|| format!(\"bot:{inbound_log_id}\"))"));
        assert!(BOT_AGENTS.contains("create_task_with_outcome"));
        assert!(BOT_AGENTS.contains("duplicate_delivery = agent_task_outcome.reused"));
        assert!(BOT_AGENTS.contains("status = 'duplicate'"));
        assert!(BOT_AGENTS.contains("queue_status = 'succeeded'"));
        assert!(BOT_AGENTS.contains("\"duplicateDelivery\""));
        assert!(BOT_AGENTS.contains("last_error = NULL"));
        assert!(BOT_AGENTS.contains("error_message = NULL"));
        assert!(!BOT_AGENTS
            .contains("last_error = 'duplicate inbound delivery reused existing agent task'"));
        assert!(BOT_AGENTS_PAGE.contains("botAgents.statuses.${key}"));
        assert!(BOT_AGENTS_PAGE.contains("botAgents.queueStatuses.${key}"));
        assert!(RD_TASK_LIFECYCLE.contains("idempotency_key: Some(format!(\"rd_task:{id}\"))"));
        assert!(AGENT_OPS.contains("idempotency_key: audit"));
    }

    #[test]
    fn agent_ops_exposes_durable_queue_inspection_api() {
        assert!(AGENT_OPS.contains(".route(\"/queue\", routing_get(queue_items))"));
        assert!(AGENT_OPS_TYPES.contains("pub struct QueueListQuery"));
        assert!(AGENT_OPS_TYPES.contains("pub queue_status: Option<String>"));
        assert!(AGENT_OPS_TYPES.contains("pub dead_only: Option<bool>"));
        assert!(AGENT_OPS_TYPES.contains("pub stale_only: Option<bool>"));
        assert!(AGENT_OPS.contains("at.queue_status = 'dead'"));
        assert!(AGENT_OPS.contains("at.queue_status IN ('claimed','running','cancelling')"));
        assert!(AGENT_OPS.contains("lease_timeout_secs.unwrap_or(600).clamp(30, 86_400)"));
        assert!(AGENT_OPS.contains("ORDER BY at.priority ASC, at.available_at ASC"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("enum WatchDogQueueIntent"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("fn detect_queue_intent"));
        assert!(AGENT_OPS.contains("fn build_watchdog_queue_answer"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("question.contains(\"死信\")"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("question.contains(\"租约\")"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("question.contains(\"队列\")"));
        assert!(WATCHDOG_PAGE.contains("agentOpsApi.queue({ deadOnly: true"));
        assert!(WATCHDOG_PAGE.contains("agentOpsApi.queue({ staleOnly: true"));
        assert!(WATCHDOG_PAGE.contains("watchdog.queue.health"));
        assert!(WATCHDOG_PAGE.contains("watchdog.queue.deadTasks"));
        assert!(WATCHDOG_PAGE.contains("watchdog.queue.staleLeases"));
    }

    #[test]
    fn linked_resource_actions_cover_builtin_async_capabilities() {
        for resource_type in [
            "rd_task",
            "pm_research_task",
            "chat_adversarial_run",
            "nl2sql_agent_query",
        ] {
            assert!(
                AGENT_OPS.contains(&format!("\"{resource_type}\"")),
                "missing linked resource handling for {resource_type}"
            );
        }
        assert!(AGENT_OPS.contains("retry_chat_adversarial_run"));
        assert!(AGENT_OPS.contains("retry_nl2sql_agent_query"));
        assert!(AGENT_OPS.contains(
            "status IN ('queued','claimed','running','waiting_input','retrying','cancelling')"
        ));
        assert!(AGENT_OPS.contains("\"cancelling\" => (STATUS_CANCELLING, 95, false)"));
        let stale_retry_message = ["not auto", "-replayed yet"].concat();
        assert!(!AGENT_OPS.contains(&stale_retry_message));
    }

    #[test]
    fn super_adversarial_is_agentops_observable_and_cancel_safe() {
        assert!(AGENT_CHAT_ADVERSARIAL_SUPPORT.contains("create_chat_adversarial_agent_task"));
        assert!(AGENT_CHAT_ADVERSARIAL_SUPPORT.contains("link_task_resource"));
        assert!(AGENT_CHAT_ADVERSARIAL_SUPPORT.contains("request_chat_adversarial_cancel"));
        assert!(AGENT_CHAT_ADVERSARIAL_SUPPORT.contains("finish_chat_adversarial_cancelled"));
        assert!(AGENT_CHAT_ADVERSARIAL_SUPPORT.contains("mark_task_cancelling"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("create_agent_task: bool"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("ensure_chat_adversarial_not_cancelled"));
        assert!(AGENT_CHAT_ADVERSARIAL
            .contains("status NOT IN ('completed','failed','cancelled','cancelling')"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("AND status = 'running'"));
        assert!(AGENT_CHAT_ADVERSARIAL_SUPPORT
            .contains("AND status IN ('queued','running','cancelling')"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("stream_chat_adversarial_run_events"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("provider.stream_message(&request)"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("chat_adversarial_cancel_requested"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("event_prefix: \"model\".to_string()"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("event_prefix: \"final\".to_string()"));
        assert!(AGENT_CHAT_ADVERSARIAL.contains("format!(\"{}_delta\", context.event_prefix)"));
        assert!(AGENT_CHAT_ADVERSARIAL_DOMAIN.contains("pub(super) struct AdversarialDebateMemory"));
        assert!(AGENT_CHAT_ADVERSARIAL_DOMAIN.contains("parse_final_decision"));
        assert!(AGENT_OPS.contains("request_chat_adversarial_cancel_from_agent_ops"));
    }

    #[test]
    fn super_adversarial_ui_is_split_and_has_runtime_actions() {
        assert!(SUPER_ADVERSARIAL_PAGE.contains("ThreadSidebar"));
        assert!(SUPER_ADVERSARIAL_PAGE.contains("RunHeader"));
        assert!(SUPER_ADVERSARIAL_PAGE.contains("DebateTimeline"));
        assert!(SUPER_ADVERSARIAL_PAGE.contains("AdversarialComposer"));
        assert!(SUPER_ADVERSARIAL_PAGE.contains("cancelChatAdversarialRun"));
        assert!(SUPER_ADVERSARIAL_PAGE.contains("streamChatAdversarialRunEvents"));
        assert!(SUPER_ADVERSARIAL_PAGE.contains("liveAdversarialMessages"));
        assert!(!SUPER_ADVERSARIAL_PAGE.contains("agentOpsApi.retryTask"));
        assert!(!SUPER_ADVERSARIAL_PAGE.contains("/watchdog?task="));
        assert!(SUPER_ADVERSARIAL_CSS.contains("@media (max-width: 900px)"));
        assert!(SUPER_ADVERSARIAL_CSS.contains("overflow-wrap: anywhere"));
        assert!(!SUPER_ADVERSARIAL_CSS.contains("radial-gradient"));
    }

    #[test]
    fn capability_contracts_cover_builtin_menu_capabilities() {
        for capability in ["aos_router", "super_adversarial", "rd_agent", "nl2sql"] {
            assert!(
                AGENT_OPS_ADAPTERS.contains(&format!("key: \"{capability}\"")),
                "missing capability contract for {capability}"
            );
        }
        for legacy_capability in ["ai_chat", "generic_ai", "pm_assistant", "watchdog"] {
            assert!(
                !AGENT_OPS_ADAPTERS.contains(&format!("key: \"{legacy_capability}\",")),
                "legacy capability should be consolidated into aos_router: {legacy_capability}"
            );
        }
        assert!(AGENT_OPS.contains("agent_ops_adapters::capability_contracts()"));
        assert!(!AGENT_OPS.contains("pub trait CapabilityAdapter"));
        assert!(!AGENT_OPS.contains("struct WatchDogIntent"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("pub struct WatchDogIntent"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("parse_watchdog_intent_with_llm"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("active_watchdog_statuses"));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("当前有哪些 agent 在运行？"));
    }

    #[test]
    fn adapter_contracts_cover_builtin_platforms_runtimes_and_actions() {
        for adapter_trait in [
            "pub trait CapabilityAdapter",
            "pub trait BotPlatformAdapter",
            "pub trait TaskRuntimeAdapter",
            "pub trait WatchDogActionAdapter",
            "pub struct CapabilityRegistry",
            "pub struct BotPlatformRegistry",
            "pub struct RuntimeRegistry",
            "pub struct WatchDogActionRegistry",
            "pub fn capability_contracts()",
            "pub fn bot_platform_contracts()",
            "pub fn runtime_contracts()",
            "pub fn watchdog_action_contracts()",
        ] {
            assert!(
                AGENT_OPS_ADAPTERS.contains(adapter_trait),
                "missing adapter SDK surface: {adapter_trait}"
            );
        }
        for platform in [
            "feishu",
            "wecom",
            "slack",
            "discord",
            "dingtalk",
            "telegram",
            "whatsapp",
            "generic_webhook",
        ] {
            assert!(
                AGENT_OPS_ADAPTERS.contains(&format!("key: \"{platform}\"")),
                "missing bot platform contract for {platform}"
            );
        }
        assert!(AGENT_OPS_ADAPTERS.contains("key: \"local_process\""));
        assert!(AGENT_OPS_ADAPTERS.contains("key: \"docker_sandbox\""));
        assert!(AGENT_OPS_ADAPTERS.contains("default_enabled: false"));
        assert!(AGENT_OPS_ADAPTERS.contains("supports_process_group_cancel: true"));
        assert!(AGENT_OPS.contains("agent_ops_adapters::bot_platform_contracts()"));
        assert!(AGENT_OPS.contains("agent_ops_adapters::runtime_contracts()"));
        assert!(AGENT_OPS.contains("agent_ops_adapters::watchdog_action_contracts()"));
        assert!(!AGENT_OPS.contains("trait WatchDogActionAdapter"));
        assert!(!AGENT_OPS.contains("struct WatchDogActionRegistry"));
        for action in ["detail_task", "cancel_task", "retry_task"] {
            assert!(
                AGENT_OPS_ADAPTERS.contains(&format!("key: \"{action}\"")),
                "missing watchdog action contract for {action}"
            );
        }
    }

    #[test]
    fn agent_runtime_supports_optional_docker_sandbox_without_changing_default() {
        assert!(AGENT_RUNTIME.contains("AOS_AGENT_RUNTIME_ISOLATION_MODE"));
        assert!(AGENT_RUNTIME.contains("AOS_AGENT_RUNTIME_DOCKER_IMAGE"));
        assert!(AGENT_RUNTIME.contains("AOS_AGENT_RUNTIME_DOCKER_NETWORK"));
        assert!(AGENT_RUNTIME.contains("RUNTIME_ISOLATION_LOCAL_PROCESS"));
        assert!(AGENT_RUNTIME.contains("RUNTIME_ISOLATION_DOCKER_SANDBOX"));
        assert!(AGENT_RUNTIME.contains("runtime_isolation_mode(input.isolation_mode.as_deref())"));
        assert!(AGENT_RUNTIME.contains("docker_launch_plan"));
        assert!(AGENT_RUNTIME.contains("\"dockerContainerName\""));
        assert!(AGENT_RUNTIME.contains("kill_docker_container_best_effort"));
        assert!(AGENT_RUNTIME.contains(".arg(\"kill\")"));
        assert!(AGENT_RUNTIME.contains("runtime_process_container_name"));
        assert!(RD_TASK_LIFECYCLE.contains("isolation_mode: None"));
    }

    #[test]
    fn rd_workbench_includes_runtime_artifacts_with_unsigned_size() {
        assert!(RD_WORKBENCH.contains("runtime_artifacts"));
        assert!(RD_WORKBENCH.contains("load_runtime_artifacts"));
        assert!(RD_WORKBENCH.contains("FROM agent_runtime_artifacts"));
        assert!(RD_WORKBENCH.contains("row.try_get::<u64, _>(\"size_bytes\")"));
    }

    #[test]
    fn rd_workbench_suggested_actions_stay_actionable() {
        assert!(RD_WORKBENCH.contains("suggested_actions"));
        assert!(RD_WORKBENCH.contains("\"cancel\""));
        assert!(RD_WORKBENCH.contains("\"retry\""));
        assert!(RD_WORKBENCH.contains("\"review_diff\""));
        assert!(RD_WORKBENCH.contains("\"fix_tests\""));
        assert!(RD_WORKBENCH.contains("rd_file_change_is_applyable"));
    }

    #[test]
    fn rd_code_intel_uses_safe_hybrid_fallbacks() {
        assert!(RD_CODE_INTEL.contains("code_intel_status"));
        assert!(RD_CODE_INTEL.contains("code_intel_query"));
        assert!(RD_CODE_INTEL.contains("safe_join(&root, path)"));
        assert!(RD_CODE_INTEL.contains("try_lsp_code_intel"));
        assert!(RD_CODE_INTEL.contains("LSP_MANAGER"));
        assert!(RD_CODE_INTEL.contains("LspSessionManager"));
        assert!(RD_CODE_INTEL.contains("run_lsp_query_with_session"));
        assert!(RD_CODE_INTEL.contains("restart_lsp_sessions_for_repository"));
        assert!(RD_CODE_INTEL.contains("shutdown_lsp_session"));
        assert!(RD_CODE_INTEL.contains("run_lsp_query"));
        assert!(RD_CODE_INTEL.contains("textDocument/definition"));
        assert!(RD_CODE_INTEL.contains("textDocument/references"));
        assert!(RD_CODE_INTEL.contains("textDocument/hover"));
        assert!(RD_CODE_INTEL.contains("Content-Length"));
        assert!(RD_CODE_INTEL.contains("source: \"lsp\""));
        assert!(RD_CODE_INTEL.contains("query_symbol_index"));
        assert!(RD_CODE_INTEL.contains("run_rg_repository_search"));
        assert!(RD_CODE_INTEL.contains("\"symbol_index\".to_string()"));
        assert!(RD_CODE_INTEL.contains("\"rg\".to_string()"));
        assert!(RD_CODE_INTEL.contains("returned symbol index fallback"));
        assert!(RD_CODE_INTEL.contains("used rg fallback"));
        assert!(RD_CODE_INTEL.contains("code_intel_command_is_installed"));
        assert!(RD_CODE_INTEL.contains("row.get::<u64, _>(\"line_number\")"));
    }

    #[test]
    fn rd_preview_sessions_are_runtime_backed_and_non_blocking() {
        assert!(RD_PREVIEW_SESSIONS.contains("create_preview_session"));
        assert!(RD_PREVIEW_SESSIONS.contains("create_runtime_session"));
        assert!(RD_PREVIEW_SESSIONS.contains("spawn_preview_runtime_command"));
        assert!(RD_PREVIEW_SESSIONS.contains("tokio::spawn(async move"));
        assert!(RD_PREVIEW_SESSIONS.contains("run_runtime_command"));
        assert!(RD_PREVIEW_SESSIONS.contains("request_cancel_runtime_session"));
        assert!(RD_PREVIEW_SESSIONS.contains("preview_proxy"));
        assert!(RD_PREVIEW_SESSIONS.contains("inject_preview_capture_script"));
        assert!(RD_PREVIEW_SESSIONS.contains("window.parent.postMessage"));
        assert!(RD_PREVIEW_SESSIONS.contains("screenshot.captured"));
        assert!(RD_PREVIEW_SESSIONS.contains("RuntimeArtifactWriteInput"));
        assert!(RD_PREVIEW_SESSIONS.contains("choose_preview_port"));
        assert!(RD_PREVIEW_SESSIONS.contains("CAST(metadata_json AS TEXT) AS metadata_json"));
        assert!(RD_PREVIEW_SESSIONS.contains("try_get::<Option<u64>, _>(\"port\")"));
        assert!(RD_PREVIEW_SESSIONS.contains("filter_entry"));
        assert!(RD_PREVIEW_SESSIONS.contains("should_skip_path(relative)"));
        assert!(
            RD_PREVIEW_SESSIONS
                .find("spawn_preview_runtime_command")
                .expect("preview should spawn runtime command")
                < RD_PREVIEW_SESSIONS
                    .find("get_preview_session_inner")
                    .expect("preview should return session after spawn"),
            "preview create should return the session after spawning the background runtime command"
        );
    }

    #[test]
    fn rd_agent_prompt_includes_preview_debug_evidence_when_relevant() {
        assert!(RD_TASK_EXECUTOR.contains("build_preview_debug_evidence_section"));
        assert!(RD_TASK_EXECUTOR.contains("rd_prompt_mentions_preview_debug"));
        assert!(RD_TASK_EXECUTOR.contains("rd_preview_sessions"));
        assert!(RD_TASK_EXECUTOR.contains("rd_preview_events"));
        assert!(RD_TASK_EXECUTOR.contains("Preview Debug 证据"));
        assert!(RD_TASK_EXECUTOR.contains("已注入 Preview Debug 证据到研发 Agent prompt"));
        assert!(RD_TASK_EXECUTOR.contains("CAST(pe.metadata_json AS TEXT) AS metadata_json"));
    }

    #[test]
    fn rd_background_execution_heartbeats_agent_task_lease() {
        assert!(AGENT_OPS.contains("acquire_agent_task_execution_lease"));
        assert!(AGENT_OPS.contains("queue.execution_lease_acquired"));
        assert!(RD_TASK_LIFECYCLE.contains("tokio::sync::watch::channel(false)"));
        assert!(
            RD_TASK_LIFECYCLE.contains("tokio::time::interval(std::time::Duration::from_secs(60))")
        );
        assert!(RD_TASK_LIFECYCLE.contains("heartbeat_agent_task_queue"));
        assert!(RD_TASK_LIFECYCLE.contains("acquire_agent_task_execution_lease"));
        assert!(RD_TASK_LIFECYCLE.contains("queue.execution_lease_skipped"));
        assert!(RD_TASK_LIFECYCLE.contains("heartbeat_stop_tx.send(true)"));
        assert!(RD_TASK_LIFECYCLE.contains("heartbeat_handle.await"));
    }

    #[test]
    fn bot_gateway_ui_guards_recent_regressions() {
        assert!(BOT_AGENTS_PAGE.contains("Number.isFinite(row.attempt_count)"));
        assert!(BOT_AGENTS_PAGE.contains("Number.isFinite(row.max_attempts)"));
        assert!(BOT_AGENTS_PAGE.contains("overflowWrap: 'anywhere'"));
        assert!(BOT_AGENTS_PAGE.contains("scroll={{ x: 'max-content' }}"));
        assert!(!BOT_AGENTS_PAGE.contains("`${row.attempt_count}/${row.max_attempts}`"));
    }

    #[test]
    fn bot_queue_finalization_writes_agentops_events() {
        assert!(BOT_AGENTS.contains("bot.queue.succeeded"));
        assert!(BOT_AGENTS.contains("bot.queue.ignored"));
        assert!(BOT_AGENTS.contains("bot.queue.dead"));
        assert!(BOT_AGENTS.contains("outcome.agent_event()"));
        assert!(BOT_AGENTS.contains("load_bot_queue_task_ref(&state, &log_id)"));
        assert!(BOT_AGENTS.contains("agent_ops::add_event"));
    }

    #[test]
    fn bot_capability_advanced_form_is_capability_scoped() {
        for guard in [
            "capabilityValue === 'aos_router'",
            "capabilityValue === 'super_adversarial'",
            "capabilityValue === 'rd_agent'",
            "capabilityValue === 'nl2sql'",
        ] {
            assert!(
                BOT_AGENTS_PAGE.contains(guard),
                "missing capability-scoped advanced config guard: {guard}"
            );
        }
        assert!(BOT_AGENTS_PAGE.contains("name={[field.name, 'dataSourceId']}"));
        assert!(BOT_AGENTS_PAGE.contains("name={[field.name, 'repositoryId']}"));
        assert!(BOT_AGENTS_PAGE.contains("selectCapabilityForAdvanced"));
        assert!(BOT_AGENTS_PAGE.contains("name={[field.name, 'confidenceThreshold']}"));
        assert!(BOT_AGENTS_PAGE.contains(
            "['ai_chat', 'generic_ai', 'pm_assistant', 'watchdog'].includes(key) ? 'aos_router'"
        ));
        assert!(BOT_AGENTS.contains("\"aos_router\""));
        assert!(!BOT_AGENTS.contains("struct AosRouterIntent"));
        assert!(BOT_AGENTS_ROUTER.contains("pub struct AosRouterIntent"));
        assert!(BOT_AGENTS_ROUTER.contains("parse_router_intent_with_llm"));
        assert!(BOT_AGENTS_ROUTER.contains("golden_router_targets_core_menu_capabilities"));
        assert!(BOT_AGENTS.contains("router.decision"));
    }

    #[test]
    fn router_and_watchdog_golden_smoke_cases_are_pinned() {
        for sample in [
            "今天天气咋样？",
            "印尼出海用户画像",
            "修复登录超时 bug",
            "昨天 GMV 按国家统计",
            "两个方案辩一辩",
            "当前有哪些 agent 在运行？",
            "取消 1",
        ] {
            assert!(
                BOT_AGENTS_ROUTER.contains(sample),
                "missing AOS Router golden smoke sample: {sample}"
            );
        }
        for target in [
            "\"ai_chat\"",
            "\"pm_assistant\"",
            "\"rd_agent\"",
            "\"nl2sql\"",
            "\"super_adversarial\"",
            "\"watchdog\"",
        ] {
            assert!(
                BOT_AGENTS_ROUTER.contains(target),
                "missing AOS Router golden target: {target}"
            );
        }
        for sample in [
            "当前有哪些 agent 在运行？",
            "取消 1",
            "死信任务",
            "产运助手有几个在工作？",
        ] {
            assert!(
                AGENT_OPS_WATCHDOG_INTENT.contains(sample),
                "missing WatchDog intent golden smoke sample: {sample}"
            );
        }
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("\"queued\","));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("\"claimed\","));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("\"running\","));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("\"waiting_input\","));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("\"retrying\","));
        assert!(AGENT_OPS_WATCHDOG_INTENT.contains("\"cancelling\","));
        for eval_id in [
            "router-pm-user-profile",
            "router-rd-bugfix",
            "router-nl2sql-gmv",
            "router-watchdog-cancel",
            "watchdog-active-status",
            "watchdog-action-cancel",
            "watchdog-queue-dead",
        ] {
            assert!(
                ROUTER_WATCHDOG_GOLDEN.contains(eval_id),
                "missing router/watchdog eval case: {eval_id}"
            );
        }
    }

    #[test]
    fn watchdog_page_exposes_runtime_trace_and_safe_layout() {
        assert!(WATCHDOG_PAGE.contains("agentOpsApi.taskTrace"));
        assert!(WATCHDOG_PAGE.contains("agentOpsApi.runtimeProcesses"));
        assert!(WATCHDOG_PAGE.contains("agentOpsApi.runtimeArtifacts"));
        assert!(WATCHDOG_PAGE.contains("agentOpsApi.runtimeArtifact"));
        assert!(WATCHDOG_PAGE.contains("scroll={{ x: 'max-content' }}"));
        assert!(WATCHDOG_PAGE.contains("overflowWrap: 'anywhere'"));
        assert!(WATCHDOG_PAGE.contains("contentTruncated"));
    }

    #[test]
    fn rd_studio_workbench_exposes_trace_runtime_artifacts_and_actions() {
        assert!(RD_STUDIO.contains("AgentTimeline"));
        assert!(RD_STUDIO.contains("PreviewPanel"));
        assert!(RD_STUDIO.contains("PreviewLogsPanel"));
        assert!(RD_STUDIO.contains("QuickOpenPalette"));
        assert!(RD_STUDIO.contains("handleReferences"));
        assert!(RD_STUDIO.contains("handlePreviewFixWithAgent"));
        assert!(RD_AGENT_TIMELINE.contains("workbench?.traceEvents"));
        assert!(RD_AGENT_TIMELINE.contains("workbench?.runtimeArtifacts"));
        assert!(RD_STUDIO.contains("setSelectedRuntimeArtifact"));
        assert!(RD_STUDIO.contains("selectedAgentOpsTask ?? workbench?.agentTask ?? null"));
        assert!(RD_STUDIO.contains("workbench?.fileChanges ?? []"));
        assert!(RD_STUDIO.contains("workbench?.testRuns ?? []"));
        assert!(RD_STUDIO.contains("workbench?.latestAnswer?.trim()"));
        assert!(RD_STUDIO.contains("resultAnswerFromWorkbench"));
        assert!(RD_STUDIO.contains("workbenchFallbackAnswer"));
        assert!(RD_STUDIO.contains("changes: workbenchAwareChanges"));
        assert!(RD_STUDIO.contains("tests: workbenchAwareTests"));
        assert!(RD_AGENT_TIMELINE.contains("type AgentOpsBridgeTask"));
        assert!(RD_AGENT_TIMELINE.contains("rd.agentRuntimeProcesses"));
        assert!(RD_AGENT_TIMELINE.contains("process.stderrPreview || process.stdoutPreview"));
        assert!(RD_AGENT_TIMELINE.contains("process.exitCode"));
        assert!(RD_AGENT_TIMELINE.contains("rd.agentSuggestedActions"));
        assert!(RD_AGENT_TIMELINE.contains("onReviewDiff"));
        assert!(RD_AGENT_TIMELINE.contains("onShowTests"));
    }

    #[test]
    fn rd_studio_code_intel_and_preview_ui_are_interactive() {
        assert!(RD_FILE_PREVIEW.contains("CodeEditorPanel"));
        assert!(RD_CODE_EDITOR_PANEL.contains("@monaco-editor/react"));
        assert!(RD_CODE_EDITOR_PANEL.contains("onMouseDown"));
        assert!(RD_CODE_EDITOR_PANEL.contains("event.event?.metaKey || event.event?.ctrlKey"));
        assert!(RD_CODE_EDITOR_PANEL.contains("codeIntelQuery"));
        assert!(RD_CODE_EDITOR_PANEL.contains("'definition'"));
        assert!(RD_CODE_EDITOR_PANEL.contains("'references'"));
        assert!(RD_CODE_EDITOR_PANEL.contains("DefinitionCandidates"));
        assert!(RD_PREVIEW_PANEL.contains("createPreviewSession"));
        assert!(RD_PREVIEW_PANEL.contains("stopPreviewSession"));
        assert!(RD_PREVIEW_PANEL.contains("recordPreviewConsoleEvent"));
        assert!(RD_PREVIEW_PANEL.contains("aos-preview-event"));
        assert!(RD_PREVIEW_PANEL.contains("authorizePreviewSession"));
        assert!(RD_PREVIEW_PANEL.contains("setAuthorizedPreviewUrl(authorization.url)"));
        assert!(RD_PREVIEW_PANEL.contains("previewEvidencePrompt"));
        assert!(RD_PREVIEW_PANEL.contains("iframe"));
        assert!(RD_PREVIEW_PANEL.contains("onFixWithAgent"));
        assert!(RD_PREVIEW_LOGS_PANEL.contains("previewSessionLogs"));
        assert!(RD_QUICK_OPEN_PALETTE.contains("repositoryFileSuggestions"));
        assert!(RD_QUICK_OPEN_PALETTE.contains("repositorySymbols"));
        assert!(RD_QUICK_OPEN_PALETTE.contains("onOpen(item.filePath"));
    }

    #[test]
    fn rd_plan_implement_all_matches_frontend_array_contract() {
        assert!(RD_SPECS.contains(") -> Result<Json<Vec<RdTaskDto>>, AppError>"));
        assert!(RD_SPECS.contains("created.push(task);"));
        assert!(RD_SPECS.contains("Ok(Json(created))"));
        assert!(!RD_SPECS.contains("Ok(Json(json!({ \"created\": created })))"));
    }

    #[test]
    fn rd_plan_final_report_uses_real_rd_task_evidence() {
        assert!(RD_SPECS.contains("refresh_plan_task_statuses("));
        assert!(RD_SPECS.contains("build_implementation_summary("));
        assert!(RD_SPECS.contains("FROM rd_spec_task_links"));
        assert!(RD_SPECS.contains("FROM rd_file_changes"));
        assert!(RD_SPECS.contains("FROM rd_test_runs"));
        assert!(RD_SPECS.contains("FROM rd_task_events"));
        assert!(RD_SPECS.contains("\"evidence\": implementation_evidence"));
        assert!(RD_PLAN_TASK_BOARD.contains("cancelled: 'default'"));
    }

    #[test]
    fn agentops_design_docs_match_code_studio_workbench_status() {
        assert!(AGENTOPS_WATCHDOG_DESIGN
            .contains("Code Studio consumes the shared RD workbench aggregation API"));
        assert!(!AGENTOPS_WATCHDOG_DESIGN
            .contains("P2 makes Code Studio consume the same AgentOps timeline"));
    }

    #[test]
    fn unsigned_bigint_hotspots_use_unsigned_rust_types() {
        assert!(BOT_AGENTS.contains("seq: u64"));
        assert!(BOT_AGENTS.contains("after_seq: u64"));
        assert!(BOT_AGENTS.contains("row.get::<u64, _>(\"seq\")"));
        assert!(BOT_AGENTS.contains("CAST(response_json AS TEXT) AS response_json"));
        assert!(RD_WORKBENCH.contains("row.try_get::<u64, _>(\"size_bytes\")"));
        assert!(!BOT_AGENTS.contains("row.get::<i64, _>(\"seq\")"));
        assert!(!RD_WORKBENCH.contains("row.get::<i64, _>(\"size_bytes\")"));
    }

    #[test]
    fn new_watchdog_bot_and_rd_i18n_keys_exist_in_zh_and_en() {
        for key in [
            "\"watchdog\"",
            "\"capabilities\"",
            "\"runtime\"",
            "\"artifactTruncated\"",
            "\"noArtifactContent\"",
            "\"queueStatuses\"",
            "\"duplicate\"",
            "\"selectCapabilityForAdvanced\"",
            "\"capabilityAdvanced\"",
            "\"agentSuggestedActions\"",
            "\"agentRuntimeArtifacts\"",
            "\"agentRuntimeProcesses\"",
            "\"agentRuntimeArtifactTruncated\"",
            "\"workbenchFallbackAnswer\"",
            "\"deadTasks\"",
            "\"staleLeases\"",
            "\"noDeadTasks\"",
            "\"noStaleLeases\"",
            "\"codeIntel\"",
            "\"goToDefinition\"",
            "\"findReferences\"",
            "\"references\"",
            "\"previewDebug\"",
            "\"previewStart\"",
            "\"previewStop\"",
            "\"previewLogsEmpty\"",
            "\"quickOpenFiles\"",
            "\"quickOpenSymbols\"",
        ] {
            assert!(ZH_CN.contains(key), "missing zh-CN i18n key {key}");
            assert!(EN_US.contains(key), "missing en-US i18n key {key}");
        }
    }
}
