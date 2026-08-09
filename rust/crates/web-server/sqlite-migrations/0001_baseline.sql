-- AOS pre-release schema squash. Edit through reviewed SQLite migrations.

-- AOS platform baseline for SQLite; external NL2SQL MySQL/TiDB support is unaffected.

PRAGMA foreign_keys = ON;

CREATE TABLE "agent_context_archives" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "window_id" TEXT NOT NULL,
  "source" TEXT NOT NULL DEFAULT 'compaction',
  "role" TEXT NOT NULL,
  "ordinal" INTEGER NOT NULL DEFAULT '0',
  "content" TEXT NOT NULL,
  "content_hash" TEXT NOT NULL,
  "content_kind" TEXT NOT NULL DEFAULT 'text',
  "char_count" INTEGER NOT NULL DEFAULT '0',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_external_identity_links" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "platform" TEXT NOT NULL,
  "external_user_id" TEXT NOT NULL,
  "channel_id" TEXT DEFAULT NULL,
  "external_conversation_id" TEXT DEFAULT NULL,
  "display_name" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'active',
  "verified_at" TEXT time(3) NOT NULL,
  "revoked_at" TEXT time(3) DEFAULT NULL,
  "last_seen_at" TEXT time(3) DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_external_identity_pairings" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "code_hash" TEXT NOT NULL,
  "platform" TEXT DEFAULT NULL,
  "expires_at" TEXT time(3) NOT NULL,
  "claimed_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_memory_citations" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "turn_id" TEXT DEFAULT NULL,
  "memory_id" TEXT NOT NULL,
  "path" TEXT NOT NULL,
  "line_start" INTEGER DEFAULT NULL,
  "line_end" INTEGER DEFAULT NULL,
  "note" TEXT,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_memory_items" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "scope" TEXT NOT NULL DEFAULT 'global',
  "app" TEXT NOT NULL DEFAULT 'shared',
  "session_id" TEXT DEFAULT NULL,
  "session_key" TEXT NOT NULL DEFAULT '',
  "memory_type" TEXT NOT NULL DEFAULT 'note',
  "content" TEXT NOT NULL,
  "content_hash" TEXT NOT NULL,
  "source_type" TEXT NOT NULL DEFAULT 'manual',
  "confidence" REAL NOT NULL DEFAULT '1.0000',
  "pinned" INTEGER NOT NULL DEFAULT '0',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "stale_at" TEXT time DEFAULT NULL,
  "verified_at" TEXT time DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "embedding_model" TEXT DEFAULT NULL,
  "embedding_dimensions" INTEGER DEFAULT NULL,
  "embedding_json" TEXT,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_memory_summaries" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "scope" TEXT NOT NULL DEFAULT 'global',
  "app" TEXT NOT NULL DEFAULT 'shared',
  "session_id" TEXT DEFAULT NULL,
  "session_key" TEXT NOT NULL DEFAULT '',
  "summary" TEXT NOT NULL,
  "source_type" TEXT NOT NULL DEFAULT 'session_summary',
  "turn_count" INTEGER NOT NULL DEFAULT '0',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_notification_deliveries" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "outbox_id" INTEGER NOT NULL,
  "subscription_id" TEXT DEFAULT NULL,
  "task_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "platform" TEXT NOT NULL,
  "channel_id" TEXT DEFAULT NULL,
  "external_conversation_id" TEXT DEFAULT NULL,
  "idempotency_key" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'queued',
  "payload_json" TEXT NOT NULL,
  "available_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "claimed_by" TEXT DEFAULT NULL,
  "claimed_at" TEXT time(3) DEFAULT NULL,
  "lease_expires_at" TEXT time(3) DEFAULT NULL,
  "attempt_count" INTEGER NOT NULL DEFAULT '0',
  "max_attempts" INTEGER NOT NULL DEFAULT '8',
  "dispatch_started_at" TEXT time(3) DEFAULT NULL,
  "provider_message_id" TEXT DEFAULT NULL,
  "last_error" TEXT,
  "sent_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_agent_delivery_outbox" FOREIGN KEY ("outbox_id") REFERENCES "agent_task_outbox" ("id") ON DELETE CASCADE,
  CONSTRAINT "fk_agent_delivery_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_runtime_artifacts" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "runtime_session_id" TEXT NOT NULL,
  "agent_task_id" TEXT DEFAULT NULL,
  "artifact_type" TEXT NOT NULL,
  "path" TEXT,
  "content_text" TEXT,
  "content_hash" TEXT DEFAULT NULL,
  "size_bytes" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "agent_runtime_artifacts_session_fk" FOREIGN KEY ("runtime_session_id") REFERENCES "agent_runtime_sessions" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_runtime_processes" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "runtime_session_id" TEXT NOT NULL,
  "agent_task_id" TEXT DEFAULT NULL,
  "command" TEXT NOT NULL,
  "cwd" TEXT NOT NULL,
  "env_redacted_json" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'queued',
  "pid" INTEGER DEFAULT NULL,
  "process_group_id" INTEGER DEFAULT NULL,
  "exit_code" INTEGER DEFAULT NULL,
  "stdout_preview" TEXT,
  "stderr_preview" TEXT,
  "stdout_artifact_id" TEXT DEFAULT NULL,
  "stderr_artifact_id" TEXT DEFAULT NULL,
  "started_at" TEXT time(3) DEFAULT NULL,
  "completed_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "agent_runtime_processes_session_fk" FOREIGN KEY ("runtime_session_id") REFERENCES "agent_runtime_sessions" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_runtime_sessions" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "agent_task_id" TEXT DEFAULT NULL,
  "capability_key" TEXT NOT NULL,
  "workspace_root" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'created',
  "isolation_mode" TEXT NOT NULL DEFAULT 'local_process',
  "pid" INTEGER DEFAULT NULL,
  "process_group_id" INTEGER DEFAULT NULL,
  "cancel_requested" INTEGER NOT NULL DEFAULT '0',
  "heartbeat_at" TEXT time(3) DEFAULT NULL,
  "started_at" TEXT time(3) DEFAULT NULL,
  "completed_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_session_compactions" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "window_id" TEXT NOT NULL,
  "previous_window_id" TEXT DEFAULT NULL,
  "trigger" TEXT NOT NULL DEFAULT 'manual',
  "strategy" TEXT NOT NULL DEFAULT 'deterministic_trident',
  "summary_tokens" INTEGER NOT NULL DEFAULT '0',
  "removed_message_count" INTEGER NOT NULL DEFAULT '0',
  "retained_tail_tokens" INTEGER NOT NULL DEFAULT '0',
  "used_memory_refs_json" TEXT DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_sessions" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "name" TEXT DEFAULT NULL,
  "workspace_path" TEXT NOT NULL,
  "model" TEXT NOT NULL DEFAULT 'anthropic/claude-3-5-sonnet-4-20250514',
  "model_pinned" INTEGER NOT NULL DEFAULT '0',
  "state" TEXT NOT NULL DEFAULT 'idle',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "is_pinned" INTEGER NOT NULL DEFAULT '0',
  "is_bookmarked" INTEGER NOT NULL DEFAULT '0',
  "source" TEXT NOT NULL DEFAULT 'agent',
  "provider" TEXT NOT NULL DEFAULT '',
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "completed_at" TEXT time DEFAULT NULL,
  PRIMARY KEY ("id"),
  CONSTRAINT "agent_sessions_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE,
  CONSTRAINT "agent_sessions_ibfk_2" FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_artifacts" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "owner_user_id" TEXT NOT NULL,
  "artifact_type" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "artifact_ref" TEXT NOT NULL,
  "content_hash" TEXT DEFAULT NULL,
  "mime_type" TEXT DEFAULT NULL,
  "size_bytes" INTEGER NOT NULL DEFAULT '0',
  "sensitivity_label" TEXT NOT NULL DEFAULT 'internal',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_agent_task_artifact_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_attempts" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "attempt_no" INTEGER NOT NULL,
  "trigger_type" TEXT NOT NULL,
  "trigger_ref" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'created',
  "worker_id" TEXT DEFAULT NULL,
  "started_at" TEXT time(3) DEFAULT NULL,
  "completed_at" TEXT time(3) DEFAULT NULL,
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_agent_task_attempt_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_command_requests" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "actor_user_id" TEXT NOT NULL,
  "actor_type" TEXT NOT NULL DEFAULT 'user',
  "command_type" TEXT NOT NULL,
  "active_retry_task_id" TEXT GENERATED ALWAYS AS (CASE WHEN "command_type" = 'retry' AND "status" IN ('queued', 'claimed') THEN "task_id" ELSE NULL END) STORED,
  "status" TEXT NOT NULL DEFAULT 'queued',
  "expected_state_version" INTEGER DEFAULT NULL,
  "idempotency_key" TEXT NOT NULL,
  "input_json" TEXT DEFAULT NULL,
  "result_json" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "available_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "claimed_by" TEXT DEFAULT NULL,
  "claimed_at" TEXT time(3) DEFAULT NULL,
  "lease_expires_at" TEXT time(3) DEFAULT NULL,
  "attempt_count" INTEGER NOT NULL DEFAULT '0',
  "max_attempts" INTEGER NOT NULL DEFAULT '3',
  "completed_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_agent_task_command_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_events" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "event_type" TEXT NOT NULL,
  "phase" TEXT DEFAULT NULL,
  "status" TEXT DEFAULT NULL,
  "severity" TEXT NOT NULL DEFAULT 'info',
  "message" TEXT NOT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "agent_task_events_task_fk" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_grants" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "grantee_type" TEXT NOT NULL,
  "grantee_id" TEXT NOT NULL,
  "permission" TEXT NOT NULL DEFAULT 'read',
  "granted_by" TEXT NOT NULL,
  "revoked_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_agent_task_grant_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_outbox" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "event_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "root_task_id" TEXT NOT NULL,
  "event_type" TEXT NOT NULL,
  "state_version" INTEGER NOT NULL,
  "visibility" TEXT NOT NULL DEFAULT 'owner',
  "payload_json" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'pending',
  "available_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "claimed_by" TEXT DEFAULT NULL,
  "claimed_at" TEXT time(3) DEFAULT NULL,
  "lease_expires_at" TEXT time(3) DEFAULT NULL,
  "attempt_count" INTEGER NOT NULL DEFAULT '0',
  "max_attempts" INTEGER NOT NULL DEFAULT '8',
  "last_error" TEXT,
  "published_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_agent_outbox_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_resource_links" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "root_task_id" TEXT NOT NULL,
  "resource_type" TEXT NOT NULL,
  "resource_id" TEXT NOT NULL,
  "relation_type" TEXT NOT NULL DEFAULT 'primary',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_agent_resource_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_task_subscriptions" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT DEFAULT NULL,
  "task_key" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "event_types_json" TEXT NOT NULL,
  "destination_type" TEXT NOT NULL,
  "destination_ref" TEXT DEFAULT NULL,
  "destination_key" TEXT NOT NULL,
  "policy_json" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_agent_subscription_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_tasks" (
  "id" TEXT NOT NULL,
  "short_code" TEXT DEFAULT NULL,
  "tenant_id" TEXT NOT NULL,
  "source" TEXT NOT NULL DEFAULT 'webui',
  "source_ref" TEXT DEFAULT NULL,
  "source_label" TEXT DEFAULT NULL,
  "capability_key" TEXT NOT NULL,
  "agent_id" TEXT DEFAULT NULL,
  "agent_name" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'created',
  "queue_status" TEXT NOT NULL DEFAULT 'none',
  "available_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "claimed_by" TEXT DEFAULT NULL,
  "claimed_at" TEXT time(3) DEFAULT NULL,
  "lease_expires_at" TEXT time(3) DEFAULT NULL,
  "attempt_count" INTEGER NOT NULL DEFAULT '0',
  "max_attempts" INTEGER NOT NULL DEFAULT '3',
  "idempotency_key" TEXT DEFAULT NULL,
  "priority" INTEGER NOT NULL DEFAULT '100',
  "last_error" TEXT,
  "finished_at" TEXT time(3) DEFAULT NULL,
  "dead_reason" TEXT,
  "phase" TEXT NOT NULL DEFAULT 'intake',
  "progress_percent" INTEGER NOT NULL DEFAULT '0',
  "state_version" INTEGER NOT NULL DEFAULT '0',
  "desired_state" TEXT DEFAULT NULL,
  "progress_json" TEXT DEFAULT NULL,
  "title" TEXT NOT NULL,
  "summary" TEXT,
  "owner_user_id" TEXT DEFAULT NULL,
  "initiator_user_id" TEXT DEFAULT NULL,
  "visibility_scope" TEXT NOT NULL DEFAULT 'own',
  "team_id" TEXT DEFAULT NULL,
  "assigned_user_id" TEXT DEFAULT NULL,
  "correlation_id" TEXT DEFAULT NULL,
  "parent_task_id" TEXT DEFAULT NULL,
  "root_task_id" TEXT DEFAULT NULL,
  "origin_session_id" TEXT DEFAULT NULL,
  "origin_turn_id" TEXT DEFAULT NULL,
  "external_platform" TEXT DEFAULT NULL,
  "external_channel_id" TEXT DEFAULT NULL,
  "external_conversation_id" TEXT DEFAULT NULL,
  "external_message_id" TEXT DEFAULT NULL,
  "linked_resource_type" TEXT DEFAULT NULL,
  "linked_resource_id" TEXT DEFAULT NULL,
  "input_json" TEXT DEFAULT NULL,
  "output_json" TEXT DEFAULT NULL,
  "result_summary" TEXT,
  "result_artifact_ref" TEXT DEFAULT NULL,
  "sensitivity_label" TEXT NOT NULL DEFAULT 'internal',
  "archived" INTEGER NOT NULL DEFAULT '0',
  "projection_active" INTEGER GENERATED ALWAYS AS ((case when ((`status` in ('created','queued','claimed','running','waiting_input','waiting_approval','blocked','retrying','cancelling')) and (`linked_resource_type` in ('rd_task','chat_adversarial_run','pm_research_task','nl2sql_agent_query','nl2sql_async_query','nl2sql_attribution_task','super_assistant_turn','pm_material_job','bot_inbound_message')) and (`linked_resource_id` is not null)) then 1 else 0 end)) STORED,
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "last_event" TEXT,
  "last_heartbeat_at" TEXT time(3) DEFAULT NULL,
  "last_progress_at" TEXT time(3) DEFAULT NULL,
  "sla_due_at" TEXT time(3) DEFAULT NULL,
  "budget_json" TEXT DEFAULT NULL,
  "cost_json" TEXT DEFAULT NULL,
  "started_at" TEXT time(3) DEFAULT NULL,
  "completed_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_thread_memory_state" (
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "use_memories" INTEGER NOT NULL DEFAULT '1',
  "generate_memories" INTEGER NOT NULL DEFAULT '1',
  "pollution_state" TEXT NOT NULL DEFAULT 'clean',
  "pollution_reason" TEXT,
  "last_external_context_at" TEXT time DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","user_id","session_id")
);

CREATE TABLE "agent_trace_events" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "event_type" TEXT NOT NULL,
  "phase" TEXT DEFAULT NULL,
  "status" TEXT DEFAULT NULL,
  "severity" TEXT NOT NULL DEFAULT 'info',
  "message" TEXT NOT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "artifact_id" TEXT DEFAULT NULL,
  "runtime_session_id" TEXT DEFAULT NULL,
  "runtime_process_id" TEXT DEFAULT NULL,
  "token_input" INTEGER DEFAULT NULL,
  "token_output" INTEGER DEFAULT NULL,
  "cost_usd" REAL DEFAULT NULL,
  "duration_ms" INTEGER DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "agent_trace_events_task_fk" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_user_presence_leases" (
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "client_id" TEXT NOT NULL,
  "current_path" TEXT DEFAULT NULL,
  "mobile_follow_enabled" INTEGER NOT NULL DEFAULT '0',
  "last_seen_at" TEXT time(3) NOT NULL,
  "expires_at" TEXT time(3) NOT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","user_id","client_id")
);

CREATE TABLE "agent_watch_rule_runs" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "rule_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "outbox_id" INTEGER DEFAULT NULL,
  "matched" INTEGER NOT NULL,
  "action_status" TEXT NOT NULL,
  "reason_code" TEXT DEFAULT NULL,
  "detail_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_agent_watch_rule_run_rule" FOREIGN KEY ("rule_id") REFERENCES "agent_watch_rules" ("id") ON DELETE CASCADE,
  CONSTRAINT "fk_agent_watch_rule_run_task" FOREIGN KEY ("task_id") REFERENCES "agent_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "agent_watch_rules" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "scope_type" TEXT NOT NULL DEFAULT 'own',
  "scope_ref" TEXT DEFAULT NULL,
  "condition_json" TEXT NOT NULL,
  "action_json" TEXT NOT NULL,
  "quiet_hours_json" TEXT DEFAULT NULL,
  "max_actions_per_day" INTEGER NOT NULL DEFAULT '20',
  "requires_confirmation" INTEGER NOT NULL DEFAULT '1',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_by" TEXT NOT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_workspace_entries" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "owner_user_id" TEXT NOT NULL,
  "workspace_id" TEXT NOT NULL,
  "visibility" TEXT NOT NULL DEFAULT 'private',
  "resource_type" TEXT NOT NULL,
  "resource_id" TEXT NOT NULL,
  "virtual_path" TEXT NOT NULL,
  "version" TEXT NOT NULL,
  "content_hash" TEXT DEFAULT NULL,
  "size_bytes" INTEGER NOT NULL DEFAULT '0',
  "mime_type" TEXT DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "source_updated_at" TEXT time(3) DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "is_current" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time(3) DEFAULT NULL,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_workspace_grants" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "workspace_id" TEXT NOT NULL,
  "entry_id" TEXT DEFAULT NULL,
  "resource_id" TEXT NOT NULL,
  "grantee_user_id" TEXT NOT NULL,
  "permission" TEXT NOT NULL DEFAULT 'read',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "revoked_at" TEXT time(3) DEFAULT NULL,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_workspace_mounts" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "workspace_id" TEXT NOT NULL,
  "virtual_root" TEXT NOT NULL,
  "resource_type" TEXT NOT NULL,
  "selector_json" TEXT DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "agent_workspace_usage" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "workspace_id" TEXT NOT NULL,
  "turn_id" TEXT DEFAULT NULL,
  "operation" TEXT NOT NULL,
  "virtual_path" TEXT DEFAULT NULL,
  "resource_id" TEXT DEFAULT NULL,
  "outcome" TEXT NOT NULL,
  "duration_ms" INTEGER DEFAULT NULL,
  "denial_code" TEXT DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "agent_workspaces" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "owner_user_id" TEXT NOT NULL,
  "workspace_type" TEXT NOT NULL DEFAULT 'personal',
  "visibility" TEXT NOT NULL DEFAULT 'private',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "acl_version" INTEGER NOT NULL DEFAULT '1',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "aos_setup_lock" (
  "lock_id" INTEGER NOT NULL,
  PRIMARY KEY ("lock_id")
);

INSERT INTO "aos_setup_lock" ("lock_id") VALUES (1);

CREATE TABLE "api_keys" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "base_url" TEXT DEFAULT NULL,
  "dimensions" INTEGER DEFAULT NULL,
  "model" TEXT DEFAULT NULL,
  "model_type" TEXT NOT NULL DEFAULT 'chat',
  "key_hash" TEXT NOT NULL,
  "encrypted_key" TEXT,
  "key_hint" TEXT NOT NULL,
  "daily_limit" INTEGER DEFAULT NULL,
  "monthly_limit" INTEGER DEFAULT NULL,
  "expires_at" TEXT time DEFAULT NULL,
  "rotation_hint" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "priority" INTEGER NOT NULL DEFAULT '0',
  "is_primary" INTEGER NOT NULL DEFAULT '1',
  "input_price_per_million" REAL DEFAULT NULL,
  "output_price_per_million" REAL DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "scenarios" TEXT DEFAULT NULL,
  "capabilities_json" TEXT DEFAULT NULL,
  "scenarios_list" TEXT GENERATED ALWAYS AS (ifnull(`scenarios`,'["ALL"]')) STORED,
  "audio_generate_path" TEXT DEFAULT NULL,
  "audio_query_path" TEXT DEFAULT NULL,
  PRIMARY KEY ("id"),
  CONSTRAINT "api_keys_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE
);

CREATE TABLE "audit_log" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "action" TEXT NOT NULL,
  "resource" TEXT NOT NULL,
  "resource_id" TEXT DEFAULT NULL,
  "details" TEXT DEFAULT NULL,
  "ip_address" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "audit_log_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE
);

CREATE TABLE "bot_agent_capabilities" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "agent_id" TEXT NOT NULL,
  "capability_key" TEXT NOT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "config_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "bot_agent_channels" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "agent_id" TEXT NOT NULL,
  "platform" TEXT NOT NULL DEFAULT 'generic_webhook',
  "name" TEXT NOT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "inbound_mode" TEXT NOT NULL DEFAULT 'auto',
  "inbound_secret" TEXT DEFAULT NULL,
  "inbound_cursor" TEXT,
  "inbound_status" TEXT NOT NULL DEFAULT 'idle',
  "inbound_error" TEXT,
  "inbound_last_seen_at" TEXT time DEFAULT NULL,
  "inbound_last_message_at" TEXT time DEFAULT NULL,
  "outbound_webhook_url" TEXT DEFAULT NULL,
  "outbound_token" TEXT,
  "outbound_signing_secret" TEXT DEFAULT NULL,
  "signing_secret" TEXT DEFAULT NULL,
  "config_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "bot_agents" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "default_capability" TEXT NOT NULL DEFAULT 'pm_assistant',
  "persona_prompt" TEXT,
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "bot_message_logs" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "agent_id" TEXT DEFAULT NULL,
  "channel_id" TEXT DEFAULT NULL,
  "direction" TEXT NOT NULL,
  "platform" TEXT NOT NULL,
  "external_user_id" TEXT DEFAULT NULL,
  "external_conversation_id" TEXT DEFAULT NULL,
  "message_type" TEXT NOT NULL DEFAULT 'text',
  "content_json" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'received',
  "queue_status" TEXT NOT NULL DEFAULT 'none',
  "available_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "claimed_by" TEXT DEFAULT NULL,
  "claimed_at" TEXT time(3) DEFAULT NULL,
  "attempt_count" INTEGER NOT NULL DEFAULT '0',
  "max_attempts" INTEGER NOT NULL DEFAULT '3',
  "last_error" TEXT,
  "finished_at" TEXT time(3) DEFAULT NULL,
  "error_message" TEXT,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "chat_adversarial_runs" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT DEFAULT NULL,
  "thread_id" TEXT DEFAULT NULL,
  "parent_run_id" TEXT DEFAULT NULL,
  "iteration_no" INTEGER NOT NULL DEFAULT '1',
  "question" TEXT NOT NULL,
  "models_json" TEXT NOT NULL,
  "judge_model" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'queued',
  "current_round" INTEGER NOT NULL DEFAULT '0',
  "max_rounds" INTEGER NOT NULL DEFAULT '5',
  "winner_model" TEXT DEFAULT NULL,
  "winner_reason" TEXT,
  "final_answer" TEXT,
  "error_message" TEXT,
  "trace_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "completed_at" TEXT time(3) DEFAULT NULL,
  PRIMARY KEY ("id"),
  CONSTRAINT "chat_adversarial_runs_tenant_fk" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE,
  CONSTRAINT "chat_adversarial_runs_user_fk" FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE
);

CREATE TABLE "chat_adversarial_threads" (
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "thread_id" TEXT NOT NULL,
  "title" TEXT DEFAULT NULL,
  "is_pinned" INTEGER NOT NULL DEFAULT '0',
  "deleted_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","user_id","thread_id"),
  CONSTRAINT "chat_adversarial_threads_tenant_fk" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE,
  CONSTRAINT "chat_adversarial_threads_user_fk" FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE
);

CREATE TABLE "chat_file_workspace_chunks" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "file_id" TEXT NOT NULL,
  "chunk_index" INTEGER NOT NULL,
  "line_start" INTEGER DEFAULT NULL,
  "line_end" INTEGER DEFAULT NULL,
  "sheet_name" TEXT DEFAULT NULL,
  "content" TEXT NOT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "embedding_model" TEXT DEFAULT NULL,
  "embedding_dimensions" INTEGER DEFAULT NULL,
  "embedding_json" TEXT,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "chat_file_workspace_files" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT DEFAULT NULL,
  "file_id" TEXT NOT NULL,
  "filename" TEXT NOT NULL,
  "media_type" TEXT NOT NULL,
  "size_bytes" INTEGER NOT NULL DEFAULT '0',
  "url" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'uploaded',
  "error_message" TEXT,
  "chunk_count" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "chat_memories" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "memory_type" TEXT NOT NULL DEFAULT 'long_term',
  "content" TEXT NOT NULL,
  "source" TEXT NOT NULL DEFAULT 'manual',
  "confidence" REAL NOT NULL DEFAULT '1.0000',
  "pinned" INTEGER NOT NULL DEFAULT '0',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "chat_memory_preferences" (
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "paused" INTEGER NOT NULL DEFAULT '0',
  "paused_at" TEXT time DEFAULT NULL,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","user_id")
);

CREATE TABLE "chat_turn_artifacts" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "artifact_type" TEXT NOT NULL,
  "payload_json" TEXT NOT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "data_sources" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT,
  "db_type" TEXT NOT NULL,
  "visibility" TEXT NOT NULL DEFAULT 'tenant',
  "config" TEXT NOT NULL,
  "schema_info" TEXT DEFAULT NULL,
  "enabled" INTEGER DEFAULT '1',
  "last_tested_at" TEXT time DEFAULT NULL,
  "last_error" TEXT,
  "embedding_status" TEXT NOT NULL DEFAULT 'not_started',
  "embedding_model" TEXT NOT NULL DEFAULT 'text-embedding-3-small',
  "embedding_dimensions" INTEGER NOT NULL DEFAULT '1536',
  "embedding_needs_reindex" INTEGER NOT NULL DEFAULT '0',
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time DEFAULT CURRENT_TIMESTAMP,
  "user_id_key" TEXT GENERATED ALWAYS AS (coalesce(`user_id`,'')) STORED,
  "sensitive_columns" TEXT DEFAULT NULL,
  "deleted_at" TEXT time DEFAULT NULL,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_ds_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "gitlab_projects" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "url" TEXT NOT NULL,
  "branch" TEXT NOT NULL DEFAULT 'main',
  "gitlab_token" TEXT DEFAULT NULL,
  "description" TEXT,
  "clone_path" TEXT DEFAULT NULL,
  "is_cloned" INTEGER NOT NULL DEFAULT '0',
  "last_sync_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "gitlab_projects_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE,
  CONSTRAINT "gitlab_projects_ibfk_2" FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE
);

CREATE TABLE "hook_execution_logs" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "hook_id" TEXT NOT NULL,
  "event_type" TEXT NOT NULL,
  "scenario" TEXT DEFAULT NULL,
  "tool_name" TEXT NOT NULL,
  "input_json" TEXT,
  "output_json" TEXT,
  "exit_code" INTEGER DEFAULT NULL,
  "duration_ms" INTEGER DEFAULT NULL,
  "error_message" TEXT,
  "executed_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "mcp_server_registry" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "transport" TEXT NOT NULL DEFAULT 'stdio',
  "auth_type" TEXT NOT NULL DEFAULT 'none',
  "auth_token" TEXT DEFAULT NULL,
  "extra_headers" TEXT,
  "timeout_ms" INTEGER NOT NULL DEFAULT '60000',
  "oauth_config" TEXT,
  "command" TEXT DEFAULT NULL,
  "args" TEXT,
  "url" TEXT DEFAULT NULL,
  "env" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "connection_status" TEXT NOT NULL DEFAULT 'disconnected',
  "status" TEXT NOT NULL DEFAULT 'unknown',
  "last_error" TEXT,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "mcp_server_registry_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE
);

CREATE TABLE "nl2sql_affected_queries" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "notification_id" INTEGER NOT NULL,
  "query_id" TEXT NOT NULL,
  "question" TEXT,
  "generated_sql" TEXT,
  "impact_level" TEXT NOT NULL DEFAULT 'low',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "nl2sql_affected_queries_ibfk_1" FOREIGN KEY ("notification_id") REFERENCES "nl2sql_schema_change_notifications" ("id") ON DELETE CASCADE
);

CREATE TABLE "nl2sql_agent_query_results" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "query_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "conversation_id" TEXT DEFAULT NULL,
  "columns_json" TEXT NOT NULL,
  "rows_json" TEXT NOT NULL,
  "total_rows" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_nl2sql_agent_result_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_attribution_conversations" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "message_count" INTEGER NOT NULL DEFAULT '0',
  "summary" TEXT,
  "last_question" TEXT,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time(3) DEFAULT NULL,
  PRIMARY KEY ("id")
);

CREATE TABLE "nl2sql_attribution_tasks" (
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "conversation_id" TEXT NOT NULL,
  "parent_task_id" TEXT DEFAULT NULL,
  "question" TEXT NOT NULL,
  "depth" TEXT NOT NULL DEFAULT 'standard',
  "datasource_ids_json" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'queued',
  "cancel_requested" INTEGER NOT NULL DEFAULT '0',
  "summary" TEXT,
  "response_json" TEXT DEFAULT NULL,
  "evidence_cards_json" TEXT DEFAULT NULL,
  "error" TEXT,
  "total_execution_ms" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("task_id")
);

CREATE TABLE "nl2sql_business_domains" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "domain_name" TEXT NOT NULL,
  "domain_description" TEXT,
  "table_count" INTEGER NOT NULL DEFAULT '0',
  "confidence_score" REAL NOT NULL DEFAULT '0',
  "source" TEXT NOT NULL DEFAULT 'auto',
  "domain_routing_mode" TEXT NOT NULL DEFAULT 'assist',
  "created_by" TEXT DEFAULT NULL,
  "reviewed_by" TEXT DEFAULT NULL,
  "reviewed_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'published',
  CONSTRAINT "fk_bd_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_nl2sql_bd_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_clarification_messages" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "conversation_id" TEXT NOT NULL,
  "session_id" TEXT DEFAULT NULL,
  "turn" INTEGER NOT NULL DEFAULT '1',
  "original_question" TEXT NOT NULL,
  "clarification_question" TEXT NOT NULL,
  "user_input" TEXT NOT NULL,
  "confirmed_requirements" TEXT DEFAULT NULL,
  "missing_requirements" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  PRIMARY KEY ("id")
);

CREATE TABLE "nl2sql_column_masking_rules" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT DEFAULT NULL,
  "table_name" TEXT NOT NULL,
  "column_name" TEXT NOT NULL,
  "mask_type" TEXT NOT NULL,
  "pattern" TEXT DEFAULT NULL,
  "constant_value" TEXT DEFAULT NULL,
  "priority" INTEGER NOT NULL DEFAULT '100',
  "role_exception_patterns" TEXT DEFAULT NULL,
  "condition_expression" TEXT,
  "description" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_by" TEXT NOT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL
);

CREATE TABLE "nl2sql_column_stats" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "table_name" TEXT NOT NULL,
  "column_name" TEXT NOT NULL,
  "row_count" INTEGER NOT NULL DEFAULT '0',
  "null_count" INTEGER NOT NULL DEFAULT '0',
  "distinct_count" INTEGER NOT NULL DEFAULT '0',
  "null_pct" REAL NOT NULL DEFAULT '0.00',
  "min_value" TEXT,
  "max_value" TEXT,
  "avg_value" REAL DEFAULT NULL,
  "sample_values" TEXT DEFAULT NULL,
  "last_analyzed" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_cs_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_cs_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_conversations" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "message_count" INTEGER NOT NULL DEFAULT '0',
  "summary" TEXT,
  "summary_version" INTEGER NOT NULL DEFAULT '1',
  "last_question" TEXT,
  "deleted_at" TEXT time DEFAULT NULL,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_nl2sql_conv_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_cross_datasource_relations" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "left_datasource_id" TEXT NOT NULL,
  "left_table" TEXT NOT NULL,
  "left_column" TEXT NOT NULL,
  "right_datasource_id" TEXT NOT NULL,
  "right_table" TEXT NOT NULL,
  "right_column" TEXT NOT NULL,
  "relation_hash" TEXT NOT NULL,
  "semantic_description" TEXT,
  "match_type" TEXT NOT NULL DEFAULT 'foreign_key',
  "confidence" REAL NOT NULL DEFAULT '0.50',
  "verified" INTEGER NOT NULL DEFAULT '0',
  "source" TEXT NOT NULL DEFAULT 'auto',
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_cdr_left_ds" FOREIGN KEY ("left_datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_cdr_right_ds" FOREIGN KEY ("right_datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_cdr_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_cross_domain_clusters" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "cluster_name" TEXT NOT NULL,
  "datasource_ids" TEXT NOT NULL,
  "domain_ids" TEXT NOT NULL,
  "description" TEXT,
  "confidence" REAL NOT NULL DEFAULT '0.50',
  "auto_discovered" INTEGER NOT NULL DEFAULT '0',
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_cdc_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_datasource_semantics" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "ai_description" TEXT,
  "user_description" TEXT,
  "embedding_model" TEXT NOT NULL DEFAULT 'text-embedding-3-small',
  "embedding_version" INTEGER NOT NULL DEFAULT '1',
  "version" INTEGER NOT NULL DEFAULT '1',
  "cached_at" TEXT time DEFAULT CURRENT_TIMESTAMP,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "status" TEXT NOT NULL DEFAULT 'published',
  "created_by" TEXT DEFAULT NULL,
  "reviewed_by" TEXT DEFAULT NULL,
  "reviewed_at" TEXT time DEFAULT NULL,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_dss_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_dss_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_foreign_keys" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "source_table" TEXT NOT NULL,
  "source_column" TEXT NOT NULL,
  "target_table" TEXT NOT NULL,
  "target_column" TEXT NOT NULL,
  "created_by" TEXT DEFAULT NULL,
  "reviewed_by" TEXT DEFAULT NULL,
  "reviewed_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "source_type" TEXT NOT NULL DEFAULT '',
  "target_type" TEXT NOT NULL DEFAULT '',
  "status" TEXT NOT NULL DEFAULT 'published',
  "updated_by" TEXT DEFAULT NULL,
  CONSTRAINT "fk_fk_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_fk_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_join_paths" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "source_column" TEXT NOT NULL DEFAULT '',
  "target_column" TEXT NOT NULL DEFAULT '',
  "join_type" TEXT NOT NULL DEFAULT 'INNER',
  "verified" INTEGER NOT NULL DEFAULT '0',
  "confidence" REAL NOT NULL DEFAULT '1.00',
  "source" TEXT NOT NULL DEFAULT 'auto',
  "datasource_id" TEXT NOT NULL,
  "source_table" TEXT NOT NULL,
  "target_table" TEXT NOT NULL,
  "path_text" TEXT NOT NULL,
  "sql_joins" TEXT NOT NULL,
  "hops" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "notes" TEXT,
  CONSTRAINT "fk_jp_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_jp_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_metric_approvals" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "metric_id" INTEGER NOT NULL,
  "action" TEXT NOT NULL,
  "reviewer_id" TEXT NOT NULL,
  "comment" TEXT,
  "from_status" TEXT DEFAULT NULL,
  "to_status" TEXT NOT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "nl2sql_metric_versions" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "metric_id" INTEGER NOT NULL,
  "version" INTEGER NOT NULL,
  "metric_name" TEXT NOT NULL,
  "expression" TEXT NOT NULL,
  "filter_conditions" TEXT,
  "description" TEXT,
  "additivity" TEXT NOT NULL DEFAULT 'additive',
  "format_spec" TEXT DEFAULT NULL,
  "changed_by" TEXT NOT NULL,
  "change_note" TEXT,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "nl2sql_metrics" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "metric_name" TEXT NOT NULL,
  "metric_aliases" TEXT NOT NULL,
  "expression" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'draft',
  "version" INTEGER NOT NULL DEFAULT '1',
  "owner_id" TEXT DEFAULT NULL,
  "approved_by" TEXT DEFAULT NULL,
  "approved_at" TEXT time DEFAULT NULL,
  "additivity" TEXT NOT NULL DEFAULT 'additive',
  "format_spec" TEXT DEFAULT NULL,
  "filter_conditions" TEXT DEFAULT NULL,
  "description" TEXT,
  "granularity" TEXT NOT NULL DEFAULT 'day',
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_m_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_m_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_queries" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "data_source_id" TEXT DEFAULT NULL,
  "conversation_id" TEXT DEFAULT NULL,
  "question" TEXT NOT NULL,
  "generated_sql" TEXT,
  "executed" INTEGER DEFAULT '0',
  "saved_view_name" TEXT DEFAULT NULL,
  "saved_view_description" TEXT,
  "rows_returned" INTEGER DEFAULT '0',
  "route_confidence" REAL DEFAULT NULL,
  "routing_method" TEXT DEFAULT NULL,
  "semantic_context" TEXT DEFAULT NULL,
  "execution_ms" INTEGER DEFAULT '0',
  "result_confidence" REAL DEFAULT NULL,
  "result_warnings" TEXT DEFAULT NULL,
  "planning_ms" INTEGER DEFAULT NULL,
  "error_message" TEXT,
  "created_at" TEXT time DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "applied_rules_json" TEXT DEFAULT NULL,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_nl2sql_q_ds" FOREIGN KEY ("data_source_id") REFERENCES "data_sources" ("id") ON DELETE SET NULL ON UPDATE CASCADE,
  CONSTRAINT "fk_nl2sql_q_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_nl2sql_q_user" FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_query_feedback" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "conversation_id" TEXT NOT NULL,
  "query_id" INTEGER DEFAULT NULL,
  "generated_sql" TEXT NOT NULL,
  "feedback_type" TEXT NOT NULL,
  "corrected_sql" TEXT,
  "correction_note" TEXT,
  "generation_confidence" REAL DEFAULT NULL,
  "routing_method" TEXT DEFAULT NULL,
  "correction_accepted" INTEGER NOT NULL DEFAULT '0',
  "clarification_question" TEXT,
  "clarification_response" TEXT,
  "datasource_ids" TEXT DEFAULT NULL,
  "created_by" TEXT NOT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "nl2sql_query_policies" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "allowed_tables" TEXT NOT NULL DEFAULT '[]',
  "denied_tables" TEXT NOT NULL DEFAULT '[]',
  "allowed_columns" TEXT NOT NULL DEFAULT '[]',
  "denied_columns" TEXT NOT NULL DEFAULT '[]',
  "row_filter_expr" TEXT,
  "description" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_nl2sql_qp_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_qp_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_query_reference_usages" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "query_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "pack_id" TEXT DEFAULT NULL,
  "pack_name" TEXT DEFAULT NULL,
  "file_id" TEXT DEFAULT NULL,
  "filename" TEXT DEFAULT NULL,
  "chunk_id" TEXT DEFAULT NULL,
  "language" TEXT DEFAULT NULL,
  "start_line" INTEGER DEFAULT NULL,
  "end_line" INTEGER DEFAULT NULL,
  "preview_text" TEXT,
  "reason" TEXT,
  "score" REAL DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "nl2sql_query_understanding_cache" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "question_hash" TEXT NOT NULL,
  "rewritten_question" TEXT,
  "intent" TEXT NOT NULL DEFAULT 'unknown',
  "entities" TEXT DEFAULT NULL,
  "confidence_score" REAL NOT NULL DEFAULT '0',
  "resolved_at" TEXT time DEFAULT NULL,
  "cache_ttl_hours" INTEGER NOT NULL DEFAULT '24',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_quc_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_quc_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_rate_limit_buckets" (
  "tenant_id" TEXT NOT NULL,
  "bucket" TEXT NOT NULL DEFAULT 'llm',
  "tokens" REAL NOT NULL DEFAULT '0',
  "capacity" REAL NOT NULL DEFAULT '60',
  "rate_per_sec" REAL NOT NULL DEFAULT '1',
  "last_refill_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","bucket"),
  CONSTRAINT "fk_rlb_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_reference_chunks" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "pack_id" TEXT NOT NULL,
  "file_id" TEXT NOT NULL,
  "chunk_index" INTEGER NOT NULL,
  "language" TEXT DEFAULT NULL,
  "chunk_type" TEXT NOT NULL DEFAULT 'text',
  "start_line" INTEGER NOT NULL DEFAULT '1',
  "end_line" INTEGER NOT NULL DEFAULT '1',
  "content_text" TEXT NOT NULL,
  "content_hash" TEXT NOT NULL,
  "token_count" INTEGER NOT NULL DEFAULT '0',
  "keywords_text" TEXT,
  "summary_text" TEXT,
  "extracted_tables_json" TEXT DEFAULT NULL,
  "extracted_columns_json" TEXT DEFAULT NULL,
  "extracted_metrics_json" TEXT DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "embedding_model" TEXT DEFAULT NULL,
  "embedding_dimensions" INTEGER DEFAULT NULL,
  "embedding_json" TEXT,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "nl2sql_reference_files" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "pack_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "filename" TEXT NOT NULL,
  "media_type" TEXT DEFAULT NULL,
  "language" TEXT DEFAULT NULL,
  "size_bytes" INTEGER NOT NULL DEFAULT '0',
  "content_hash" TEXT NOT NULL,
  "version_no" INTEGER NOT NULL DEFAULT '1',
  "storage_path" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'indexed',
  "error" TEXT,
  "summary" TEXT,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "nl2sql_reference_packs" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "datasource_bindings_json" TEXT DEFAULT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT,
  "scope" TEXT NOT NULL DEFAULT 'datasource',
  "tags_json" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "verified" INTEGER NOT NULL DEFAULT '0',
  "stale" INTEGER NOT NULL DEFAULT '0',
  "knowledge_kind" TEXT NOT NULL DEFAULT 'sql_knowledge',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "nl2sql_refresh_tasks" (
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "trigger_source" TEXT NOT NULL DEFAULT 'user',
  "status" TEXT NOT NULL DEFAULT 'pending',
  "change_summary" TEXT DEFAULT NULL,
  "auto_action" TEXT NOT NULL DEFAULT 'pending_approval',
  "progress" INTEGER NOT NULL DEFAULT '0',
  "total_tables" INTEGER NOT NULL DEFAULT '0',
  "override_schema" TEXT DEFAULT NULL,
  "processed_tables" INTEGER NOT NULL DEFAULT '0',
  "error_message" TEXT,
  "failed_tables" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "completed_at" TEXT time DEFAULT NULL,
  PRIMARY KEY ("task_id"),
  CONSTRAINT "fk_nl2sql_rt_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_rt_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_result_cache" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "question_hash" TEXT NOT NULL,
  "question" TEXT NOT NULL,
  "generated_sql" TEXT NOT NULL,
  "result_snapshot" TEXT,
  "hit_count" INTEGER NOT NULL DEFAULT '0',
  "expires_at" TEXT time NOT NULL,
  "invalidated_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "query_id" TEXT DEFAULT NULL
);

CREATE TABLE "nl2sql_result_validation_rules" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "table_name" TEXT NOT NULL,
  "column_name" TEXT NOT NULL,
  "rule_type" TEXT NOT NULL,
  "rule_config" TEXT NOT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "severity" TEXT NOT NULL DEFAULT 'warning',
  "description" TEXT DEFAULT '',
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rvr_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_rvr_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_schema_change_notifications" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "task_id" TEXT DEFAULT NULL,
  "change_type" TEXT NOT NULL,
  "details" TEXT NOT NULL,
  "affected_queries_count" INTEGER NOT NULL DEFAULT '0',
  "recommended_action" TEXT NOT NULL DEFAULT 'review_semantics',
  "status" TEXT NOT NULL DEFAULT 'pending',
  "reviewed_by" TEXT DEFAULT NULL,
  "reviewed_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_scn_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_scn_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_synonyms" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "term" TEXT NOT NULL,
  "canonical_table" TEXT NOT NULL,
  "canonical_column" TEXT NOT NULL,
  "term_type" TEXT NOT NULL DEFAULT 'alias',
  "created_by" TEXT DEFAULT NULL,
  "reviewed_by" TEXT DEFAULT NULL,
  "reviewed_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'published',
  CONSTRAINT "fk_syn_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_syn_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_table_desc_semantics" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "table_name" TEXT NOT NULL,
  "ai_description" TEXT,
  "user_description" TEXT,
  "embedding_model" TEXT NOT NULL DEFAULT 'text-embedding-3-small',
  "embedding_version" INTEGER NOT NULL DEFAULT '1',
  "version" INTEGER NOT NULL DEFAULT '1',
  "is_manual" INTEGER NOT NULL DEFAULT '0',
  "cached_at" TEXT time DEFAULT CURRENT_TIMESTAMP,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'published',
  "created_by" TEXT DEFAULT NULL,
  "reviewed_by" TEXT DEFAULT NULL,
  "reviewed_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_nl2sql_tds_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_tds_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_table_domain_mapping" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "table_name" TEXT NOT NULL,
  "domain_id" INTEGER NOT NULL,
  "confidence_score" REAL NOT NULL DEFAULT '0',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_nl2sql_tdm_domain" FOREIGN KEY ("domain_id") REFERENCES "nl2sql_business_domains" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_tdm_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "nl2sql_table_domain_mapping_ibfk_1" FOREIGN KEY ("domain_id") REFERENCES "nl2sql_business_domains" ("id") ON DELETE CASCADE
);

CREATE TABLE "nl2sql_table_routing_features" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "datasource_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "table_name" TEXT NOT NULL,
  "query_count" INTEGER NOT NULL DEFAULT '0',
  "last_query_at" TEXT time DEFAULT NULL,
  "deleted_at" TEXT time DEFAULT NULL,
  "avg_confidence" REAL NOT NULL DEFAULT '0',
  "success_rate" REAL NOT NULL DEFAULT '0',
  CONSTRAINT "fk_trf_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_table_semantics" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "datasource_id" TEXT NOT NULL,
  "table_name" TEXT NOT NULL,
  "column_name" TEXT NOT NULL,
  "semantic_description" TEXT,
  "user_description" TEXT,
  "sample_values" TEXT DEFAULT NULL,
  "column_type" TEXT NOT NULL DEFAULT '',
  "embedding_model" TEXT NOT NULL DEFAULT 'text-embedding-3-small',
  "embedding_version" INTEGER NOT NULL DEFAULT '1',
  "version" INTEGER NOT NULL DEFAULT '1',
  "is_manual" INTEGER NOT NULL DEFAULT '0',
  "cached_at" TEXT time DEFAULT CURRENT_TIMESTAMP,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'published',
  "created_by" TEXT DEFAULT NULL,
  "reviewed_by" TEXT DEFAULT NULL,
  "reviewed_at" TEXT time DEFAULT NULL,
  "is_indexed" INTEGER NOT NULL DEFAULT '0',
  CONSTRAINT "fk_ts_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_ts_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_table_stats" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL DEFAULT '',
  "datasource_id" TEXT NOT NULL,
  "table_name" TEXT NOT NULL,
  "row_count" INTEGER NOT NULL DEFAULT '0',
  "size_bytes" INTEGER NOT NULL DEFAULT '0',
  "owner" TEXT DEFAULT '',
  "domain_id" INTEGER DEFAULT NULL,
  "tags" TEXT DEFAULT NULL,
  "tags_v2" TEXT DEFAULT NULL,
  "last_analyzed" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "deleted_at" TEXT time DEFAULT NULL,
  CONSTRAINT "fk_nl2sql_ts_ds" FOREIGN KEY ("datasource_id") REFERENCES "data_sources" ("id") ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT "fk_tstat_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "nl2sql_time_patterns" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "pattern_regex" TEXT NOT NULL,
  "pattern_display" TEXT NOT NULL DEFAULT '',
  "resolved_type" TEXT NOT NULL,
  "granularity" TEXT NOT NULL DEFAULT 'day',
  "offset_days" INTEGER NOT NULL DEFAULT '0',
  "priority" INTEGER NOT NULL DEFAULT '0',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_tp_tenant" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE "notifications" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "title" TEXT NOT NULL,
  "body" TEXT NOT NULL,
  "level" TEXT NOT NULL DEFAULT 'info',
  "read" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "notifications_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE
);

CREATE TABLE "pm_audit_trails" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "run_id" TEXT DEFAULT NULL,
  "event_type" TEXT NOT NULL,
  "severity" TEXT NOT NULL DEFAULT 'info',
  "message" TEXT NOT NULL,
  "payload_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_budget_profiles" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "profile_key" TEXT NOT NULL,
  "display_name" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "is_default" INTEGER NOT NULL DEFAULT '0',
  "priority" INTEGER NOT NULL DEFAULT '0',
  "pipeline_timeout_secs" INTEGER NOT NULL DEFAULT '900',
  "max_attempts" INTEGER NOT NULL DEFAULT '4',
  "retrieve_max_tool_calls" INTEGER NOT NULL DEFAULT '12',
  "max_calls_per_source" INTEGER NOT NULL DEFAULT '3',
  "source_slot_search_secs" INTEGER NOT NULL DEFAULT '50',
  "source_slot_browser_secs" INTEGER NOT NULL DEFAULT '80',
  "source_slot_api_fetch_secs" INTEGER NOT NULL DEFAULT '65',
  "preflight_model_timeout_secs" INTEGER NOT NULL DEFAULT '30',
  "preflight_probe_timeout_secs" INTEGER NOT NULL DEFAULT '10',
  "preflight_overall_timeout_secs" INTEGER NOT NULL DEFAULT '120',
  "retry_step_budget_secs" INTEGER NOT NULL DEFAULT '45',
  "retry_total_budget_secs" INTEGER NOT NULL DEFAULT '180',
  "constraints_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_claim_verdicts" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "run_id" TEXT NOT NULL,
  "claim_key" TEXT NOT NULL,
  "claim_text" TEXT NOT NULL,
  "verdict" TEXT NOT NULL,
  "confidence" REAL NOT NULL DEFAULT '0.0000',
  "evidence_excerpt" TEXT,
  "url" TEXT DEFAULT NULL,
  "domain" TEXT DEFAULT NULL,
  "reason" TEXT,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_conflict_cases" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "run_id" TEXT NOT NULL,
  "topic_key" TEXT NOT NULL,
  "topic" TEXT NOT NULL,
  "source_a" TEXT DEFAULT NULL,
  "claim_a" TEXT,
  "source_b" TEXT DEFAULT NULL,
  "claim_b" TEXT,
  "verdict" TEXT DEFAULT NULL,
  "confidence" REAL NOT NULL DEFAULT '0.0000',
  "reason" TEXT,
  "support_urls_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_domain_circuit_states" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "domain_key" TEXT NOT NULL,
  "consecutive_failures" INTEGER NOT NULL DEFAULT '0',
  "open_until" TEXT time DEFAULT NULL,
  "last_error_code" TEXT DEFAULT NULL,
  "last_error_message" TEXT DEFAULT NULL,
  "last_success_at" TEXT time DEFAULT NULL,
  "last_failure_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_material_assets" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "job_id" INTEGER NOT NULL,
  "asset_type" TEXT NOT NULL DEFAULT 'text',
  "url" TEXT DEFAULT NULL,
  "content_text" TEXT,
  "meta_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_material_jobs" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "mission_run_id" INTEGER DEFAULT NULL,
  "thread_id" INTEGER DEFAULT NULL,
  "parent_job_id" INTEGER DEFAULT NULL,
  "iteration_no" INTEGER NOT NULL DEFAULT '1',
  "prompt_text" TEXT NOT NULL,
  "model" TEXT DEFAULT NULL,
  "asset_type" TEXT NOT NULL DEFAULT 'text',
  "status" TEXT NOT NULL DEFAULT 'queued',
  "result_count" INTEGER NOT NULL DEFAULT '0',
  "error_message" TEXT,
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_missions" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "mission_name" TEXT NOT NULL,
  "intent" TEXT NOT NULL,
  "country_code" TEXT NOT NULL,
  "schedule_cron" TEXT DEFAULT NULL,
  "lookback_days" INTEGER NOT NULL DEFAULT '7',
  "max_sources" INTEGER NOT NULL DEFAULT '4',
  "max_signals_per_source" INTEGER NOT NULL DEFAULT '5',
  "auto_discovery" INTEGER NOT NULL DEFAULT '1',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_prompt_registry" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "prompt_key" TEXT NOT NULL,
  "prompt_version" TEXT NOT NULL,
  "contract_version" TEXT NOT NULL DEFAULT 'v1',
  "prompt_hash" TEXT NOT NULL,
  "contract_schema_json" TEXT DEFAULT NULL,
  "stage" TEXT DEFAULT NULL,
  "language" TEXT NOT NULL DEFAULT 'auto',
  "is_machine_executable" INTEGER NOT NULL DEFAULT '1',
  "last_run_id" TEXT DEFAULT NULL,
  "run_count" INTEGER NOT NULL DEFAULT '0',
  "validation_error_count" INTEGER NOT NULL DEFAULT '0',
  "metadata_json" TEXT DEFAULT NULL,
  "last_used_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_provider_health" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "provider_key" TEXT NOT NULL,
  "channel" TEXT NOT NULL,
  "run_count" INTEGER NOT NULL DEFAULT '0',
  "success_count" INTEGER NOT NULL DEFAULT '0',
  "failure_count" INTEGER NOT NULL DEFAULT '0',
  "avg_latency_ms" INTEGER DEFAULT NULL,
  "last_error_code" TEXT DEFAULT NULL,
  "last_status" TEXT NOT NULL DEFAULT 'healthy',
  "last_checked_at" TEXT time DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_quality_gate_metrics" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "run_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT DEFAULT NULL,
  "session_id" TEXT DEFAULT NULL,
  "passed" INTEGER NOT NULL DEFAULT '0',
  "quality_score" REAL NOT NULL DEFAULT '0.0000',
  "tool_call_count" INTEGER NOT NULL DEFAULT '0',
  "citation_count" INTEGER NOT NULL DEFAULT '0',
  "domain_count" INTEGER NOT NULL DEFAULT '0',
  "claim_count" INTEGER NOT NULL DEFAULT '0',
  "claim_alignment_ok" INTEGER NOT NULL DEFAULT '0',
  "triad_total_claims" INTEGER NOT NULL DEFAULT '0',
  "triad_aligned_claims" INTEGER NOT NULL DEFAULT '0',
  "triad_coverage" REAL NOT NULL DEFAULT '0.0000',
  "conflict_adjudicated" INTEGER NOT NULL DEFAULT '0',
  "conflict_confidence" REAL NOT NULL DEFAULT '0.0000',
  "missing_json" TEXT DEFAULT NULL,
  "suggestions_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_evidence_graph" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "claim_key" TEXT NOT NULL,
  "claim_text" TEXT NOT NULL,
  "url_hash" TEXT NOT NULL,
  "url" TEXT NOT NULL,
  "domain" TEXT DEFAULT NULL,
  "relation" TEXT NOT NULL DEFAULT 'supports',
  "source_tool" TEXT DEFAULT NULL,
  "source_route" TEXT DEFAULT NULL,
  "evidence_excerpt" TEXT,
  "run_count" INTEGER NOT NULL DEFAULT '0',
  "support_count" INTEGER NOT NULL DEFAULT '0',
  "contradict_count" INTEGER NOT NULL DEFAULT '0',
  "unresolved_count" INTEGER NOT NULL DEFAULT '0',
  "avg_confidence" REAL NOT NULL DEFAULT '0.0000',
  "last_seen_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_route_stats" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "route_key" TEXT NOT NULL,
  "channel" TEXT DEFAULT NULL,
  "variant" TEXT DEFAULT NULL,
  "run_count" INTEGER NOT NULL DEFAULT '0',
  "success_count" INTEGER NOT NULL DEFAULT '0',
  "failure_count" INTEGER NOT NULL DEFAULT '0',
  "success_rate" REAL NOT NULL DEFAULT '0.0000',
  "avg_quality" REAL NOT NULL DEFAULT '0.0000',
  "avg_citation_count" REAL NOT NULL DEFAULT '0.0000',
  "avg_domain_count" REAL NOT NULL DEFAULT '0.0000',
  "avg_tool_call_count" REAL NOT NULL DEFAULT '0.0000',
  "avg_retrieve_duration_ms" REAL NOT NULL DEFAULT '0.0000',
  "avg_cost_usd" REAL NOT NULL DEFAULT '0.000000',
  "score" REAL NOT NULL DEFAULT '0.0000',
  "last_run_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_runs" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "run_id" TEXT NOT NULL,
  "task_id" TEXT DEFAULT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "source" TEXT NOT NULL DEFAULT 'foreground_stream',
  "status" TEXT NOT NULL DEFAULT 'queued',
  "current_stage" TEXT DEFAULT NULL,
  "attempt" INTEGER DEFAULT NULL,
  "budget_profile" TEXT NOT NULL DEFAULT 'normal',
  "pipeline_timeout_secs" INTEGER NOT NULL DEFAULT '900',
  "max_attempts" INTEGER NOT NULL DEFAULT '4',
  "source_slot_search_secs" INTEGER NOT NULL DEFAULT '50',
  "source_slot_browser_secs" INTEGER NOT NULL DEFAULT '80',
  "source_slot_api_fetch_secs" INTEGER NOT NULL DEFAULT '65',
  "retrieve_max_tool_calls" INTEGER NOT NULL DEFAULT '12',
  "max_calls_per_source" INTEGER NOT NULL DEFAULT '3',
  "user_message" TEXT,
  "total_elapsed_ms" INTEGER DEFAULT NULL,
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "final_quality_score" REAL DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "started_at" TEXT time DEFAULT NULL,
  "deadline_at" TEXT time DEFAULT NULL,
  "ended_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_source_slots" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "run_id" TEXT NOT NULL,
  "stage_attempt_id" INTEGER DEFAULT NULL,
  "slot_seq" INTEGER NOT NULL,
  "route_key" TEXT DEFAULT NULL,
  "channel" TEXT DEFAULT NULL,
  "variant" TEXT DEFAULT NULL,
  "source_key" TEXT DEFAULT NULL,
  "source_url" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL,
  "tool_call_count" INTEGER NOT NULL DEFAULT '0',
  "elapsed_ms" INTEGER DEFAULT NULL,
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "detail_json" TEXT DEFAULT NULL,
  "started_at" TEXT time DEFAULT NULL,
  "ended_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_stage_attempts" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "run_id" TEXT NOT NULL,
  "stage" TEXT NOT NULL,
  "attempt_no" INTEGER NOT NULL,
  "status" TEXT NOT NULL,
  "strategy" TEXT DEFAULT NULL,
  "route_key" TEXT DEFAULT NULL,
  "channel" TEXT DEFAULT NULL,
  "variant" TEXT DEFAULT NULL,
  "timeout_secs" INTEGER DEFAULT NULL,
  "budget_secs" INTEGER DEFAULT NULL,
  "elapsed_ms" INTEGER DEFAULT NULL,
  "detail_json" TEXT,
  "repair_scope_json" TEXT,
  "result_json" TEXT,
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "started_at" TEXT time DEFAULT NULL,
  "ended_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_task_events" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "seq" INTEGER NOT NULL,
  "event_type" TEXT NOT NULL DEFAULT 'stage_event',
  "status" TEXT NOT NULL,
  "stage" TEXT DEFAULT NULL,
  "attempt" INTEGER DEFAULT NULL,
  "message" TEXT,
  "elapsed_ms" INTEGER NOT NULL DEFAULT '0',
  "stage_elapsed_ms" INTEGER DEFAULT NULL,
  "detail_json" TEXT DEFAULT NULL,
  "response_json" TEXT DEFAULT NULL,
  "event_hash" TEXT DEFAULT NULL,
  "idempotency_key" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_task_stream_events" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "seq" INTEGER NOT NULL,
  "stage" TEXT NOT NULL,
  "delta" TEXT NOT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_research_tasks" (
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "message" TEXT NOT NULL,
  "input_context_json" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'queued',
  "stage" TEXT DEFAULT NULL,
  "attempt" INTEGER DEFAULT NULL,
  "elapsed_ms" INTEGER NOT NULL DEFAULT '0',
  "stage_elapsed_ms" INTEGER DEFAULT NULL,
  "detail_json" TEXT DEFAULT NULL,
  "response_json" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "worker_error_code" TEXT DEFAULT NULL,
  "cancel_requested" INTEGER NOT NULL DEFAULT '0',
  "lease_owner" TEXT DEFAULT NULL,
  "lease_expires_at" TEXT time DEFAULT NULL,
  "heartbeat_at" TEXT time DEFAULT NULL,
  "lock_version" INTEGER NOT NULL DEFAULT '0',
  "recovery_cursor_seq" INTEGER NOT NULL DEFAULT '0',
  "resume_from_checkpoint" INTEGER NOT NULL DEFAULT '1',
  "event_seq" INTEGER NOT NULL DEFAULT '0',
  "checkpoint_json" TEXT DEFAULT NULL,
  "completed_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("task_id")
);

CREATE TABLE "pm_research_tool_call_ledger" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "run_id" TEXT NOT NULL,
  "stage_attempt_id" INTEGER DEFAULT NULL,
  "source_slot_id" INTEGER DEFAULT NULL,
  "call_seq" INTEGER NOT NULL,
  "tool_name" TEXT NOT NULL,
  "tool_use_id" TEXT DEFAULT NULL,
  "input_preview" TEXT,
  "output_preview" TEXT,
  "input_raw" TEXT,
  "output_raw" TEXT,
  "is_error" INTEGER NOT NULL DEFAULT '0',
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "http_status" INTEGER DEFAULT NULL,
  "latency_ms" INTEGER DEFAULT NULL,
  "route_key" TEXT DEFAULT NULL,
  "channel" TEXT DEFAULT NULL,
  "provider" TEXT DEFAULT NULL,
  "provider_trace" TEXT,
  "url" TEXT DEFAULT NULL,
  "domain" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_retry_governance_states" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "run_id" TEXT NOT NULL,
  "session_id" TEXT DEFAULT NULL,
  "last_attempt" INTEGER NOT NULL DEFAULT '0',
  "next_allowed_at" TEXT time DEFAULT NULL,
  "base_backoff_ms" INTEGER NOT NULL DEFAULT '0',
  "jitter_ms" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_route_bandit_state" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "route_key" TEXT NOT NULL,
  "channel" TEXT DEFAULT NULL,
  "score" REAL NOT NULL DEFAULT '0.000000',
  "exploration_bonus" REAL NOT NULL DEFAULT '0.000000',
  "exploitation_score" REAL NOT NULL DEFAULT '0.000000',
  "last_decision_at" TEXT time DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_route_circuit_states" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "route_key" TEXT NOT NULL,
  "channel" TEXT DEFAULT NULL,
  "consecutive_failures" INTEGER NOT NULL DEFAULT '0',
  "open_until" TEXT time DEFAULT NULL,
  "last_error_code" TEXT DEFAULT NULL,
  "last_error_message" TEXT DEFAULT NULL,
  "last_success_at" TEXT time DEFAULT NULL,
  "last_failure_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_route_learning_features" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "route_key" TEXT NOT NULL,
  "channel" TEXT DEFAULT NULL,
  "total_runs" INTEGER NOT NULL DEFAULT '0',
  "success_runs" INTEGER NOT NULL DEFAULT '0',
  "failed_runs" INTEGER NOT NULL DEFAULT '0',
  "ema_quality" REAL NOT NULL DEFAULT '0.0000',
  "ema_latency_ms" REAL NOT NULL DEFAULT '0.0000',
  "ema_cost_usd" REAL NOT NULL DEFAULT '0.000000',
  "ema_success_rate" REAL NOT NULL DEFAULT '0.0000',
  "policy_weight" REAL NOT NULL DEFAULT '1.0000',
  "policy_state" TEXT NOT NULL DEFAULT 'learn',
  "last_policy_reason" TEXT DEFAULT NULL,
  "last_run_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_search_provider_configs" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "provider_type" TEXT NOT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "priority" INTEGER NOT NULL DEFAULT '100',
  "base_url" TEXT,
  "method" TEXT NOT NULL DEFAULT 'GET',
  "auth_type" TEXT NOT NULL DEFAULT 'api_key',
  "auth_secret_ref" TEXT DEFAULT NULL,
  "auth_secret_ciphertext" TEXT,
  "key_hint" TEXT DEFAULT NULL,
  "headers_json" TEXT DEFAULT NULL,
  "query_template_json" TEXT DEFAULT NULL,
  "response_mapping_json" TEXT DEFAULT NULL,
  "timeout_secs" INTEGER NOT NULL DEFAULT '12',
  "max_results" INTEGER NOT NULL DEFAULT '10',
  "fetch_content_enabled" INTEGER NOT NULL DEFAULT '1',
  "content_extract_mode" TEXT NOT NULL DEFAULT 'auto',
  "domain_allowlist_json" TEXT DEFAULT NULL,
  "domain_blocklist_json" TEXT DEFAULT NULL,
  "rate_limit_json" TEXT DEFAULT NULL,
  "health_status" TEXT NOT NULL DEFAULT 'unknown',
  "last_error" TEXT,
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "pm_session_memories" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "memory_type" TEXT NOT NULL DEFAULT 'project_fact',
  "content" TEXT NOT NULL,
  "source" TEXT NOT NULL DEFAULT 'manual',
  "confidence" REAL NOT NULL DEFAULT '1.0000',
  "pinned" INTEGER NOT NULL DEFAULT '0',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "pm_session_memory_preferences" (
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "paused" INTEGER NOT NULL DEFAULT '0',
  "paused_at" TEXT time DEFAULT NULL,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","user_id","session_id")
);

CREATE TABLE "pm_session_summaries" (
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "summary" TEXT NOT NULL,
  "turn_count" INTEGER NOT NULL DEFAULT '0',
  "source_task_id" TEXT DEFAULT NULL,
  "last_compacted_removed_messages" INTEGER DEFAULT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","user_id","session_id")
);

CREATE TABLE "pm_subtask_attempts" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "subtask_run_id" INTEGER NOT NULL,
  "run_id" TEXT NOT NULL,
  "subtask_key" TEXT NOT NULL,
  "attempt_no" INTEGER NOT NULL,
  "attempt_key" TEXT NOT NULL,
  "variant" TEXT DEFAULT NULL,
  "route_key" TEXT DEFAULT NULL,
  "route_channel" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'running',
  "elapsed_ms" INTEGER DEFAULT NULL,
  "citation_count" INTEGER NOT NULL DEFAULT '0',
  "domain_count" INTEGER NOT NULL DEFAULT '0',
  "tool_call_count" INTEGER NOT NULL DEFAULT '0',
  "quality_score" REAL DEFAULT NULL,
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "detail_json" TEXT DEFAULT NULL,
  "started_at" TEXT time DEFAULT NULL,
  "ended_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "pm_subtask_runs" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "run_id" TEXT NOT NULL,
  "task_id" TEXT DEFAULT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "subtask_key" TEXT NOT NULL,
  "subtask_id" TEXT DEFAULT NULL,
  "title" TEXT NOT NULL,
  "goal" TEXT,
  "deliverable" TEXT,
  "required_evidence_type" TEXT DEFAULT NULL,
  "priority" TEXT NOT NULL DEFAULT 'medium',
  "status" TEXT NOT NULL DEFAULT 'queued',
  "probe_candidate_count" INTEGER NOT NULL DEFAULT '0',
  "probe_completed_count" INTEGER NOT NULL DEFAULT '0',
  "citation_count" INTEGER NOT NULL DEFAULT '0',
  "domain_count" INTEGER NOT NULL DEFAULT '0',
  "tool_call_count" INTEGER NOT NULL DEFAULT '0',
  "quality_score" REAL DEFAULT NULL,
  "error_code" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "detail_json" TEXT DEFAULT NULL,
  "started_at" TEXT time DEFAULT NULL,
  "ended_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "rd_agent_market_index" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repository_id" INTEGER DEFAULT NULL,
  "source_type" TEXT NOT NULL DEFAULT 'repository',
  "repo_full_name" TEXT NOT NULL,
  "repo_url" TEXT NOT NULL,
  "branch" TEXT NOT NULL,
  "item_type" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT NOT NULL,
  "tags_json" TEXT NOT NULL,
  "template_json" TEXT NOT NULL,
  "template_path" TEXT NOT NULL,
  "html_url" TEXT DEFAULT NULL,
  "raw_url" TEXT DEFAULT NULL,
  "source_format" TEXT NOT NULL,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rd_agent_market_index_repo" FOREIGN KEY ("repository_id") REFERENCES "rd_agent_market_repositories" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_agent_market_repositories" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repo_full_name" TEXT NOT NULL,
  "repo_url" TEXT NOT NULL,
  "branch" TEXT NOT NULL DEFAULT 'main',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "discovered_count" INTEGER NOT NULL DEFAULT '0',
  "last_scan_at" TEXT time DEFAULT NULL,
  "last_scan_status" TEXT NOT NULL DEFAULT 'idle',
  "last_scan_error" TEXT,
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "rd_agent_profiles" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "role_prompt" TEXT NOT NULL,
  "allowed_tools" TEXT DEFAULT NULL,
  "default_model" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "rd_agent_workflows" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT,
  "definition_json" TEXT NOT NULL,
  "source" TEXT NOT NULL DEFAULT 'aos',
  "source_item_id" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "rd_code_intel_sessions" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "language" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'disconnected',
  "server_command" TEXT DEFAULT NULL,
  "root_path" TEXT DEFAULT NULL,
  "last_error" TEXT,
  "started_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_code_intel_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_file_changes" (
  "id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT DEFAULT NULL,
  "file_path" TEXT NOT NULL,
  "change_type" TEXT NOT NULL DEFAULT 'modify',
  "diff_patch" TEXT NOT NULL,
  "applied" INTEGER NOT NULL DEFAULT '0',
  "applied_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_file_changes_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE SET NULL,
  CONSTRAINT "fk_rd_file_changes_task" FOREIGN KEY ("task_id") REFERENCES "rd_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_integrations" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "config_json" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "rd_patch_ownerships" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "change_id" TEXT NOT NULL,
  "file_path" TEXT NOT NULL,
  "patch_hash" TEXT NOT NULL,
  "applied" INTEGER NOT NULL DEFAULT '0',
  "applied_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_patch_ownership_change" FOREIGN KEY ("change_id") REFERENCES "rd_file_changes" ("id") ON DELETE CASCADE,
  CONSTRAINT "fk_rd_patch_ownership_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE,
  CONSTRAINT "fk_rd_patch_ownership_task" FOREIGN KEY ("task_id") REFERENCES "rd_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_preview_events" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "event_type" TEXT NOT NULL,
  "severity" TEXT NOT NULL DEFAULT 'info',
  "message" TEXT NOT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_preview_events_session" FOREIGN KEY ("session_id") REFERENCES "rd_preview_sessions" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_preview_sessions" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "task_id" TEXT DEFAULT NULL,
  "runtime_session_id" TEXT DEFAULT NULL,
  "process_id" TEXT DEFAULT NULL,
  "command" TEXT NOT NULL,
  "port" INTEGER DEFAULT NULL,
  "path" TEXT NOT NULL DEFAULT '/',
  "url" TEXT DEFAULT NULL,
  "proxied_url" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'starting',
  "last_error" TEXT,
  "logs_preview" TEXT,
  "started_at" TEXT time(3) DEFAULT NULL,
  "stopped_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_preview_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_quality_metrics" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT DEFAULT NULL,
  "task_id" TEXT DEFAULT NULL,
  "metric_name" TEXT NOT NULL,
  "metric_value" REAL NOT NULL DEFAULT '0',
  "detail_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rd_quality_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE SET NULL,
  CONSTRAINT "fk_rd_quality_task" FOREIGN KEY ("task_id") REFERENCES "rd_tasks" ("id") ON DELETE SET NULL
);

CREATE TABLE "rd_repository_context_summaries" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "scope_type" TEXT NOT NULL,
  "scope_key" TEXT NOT NULL,
  "scope_key_hash" TEXT NOT NULL,
  "source_hash" TEXT NOT NULL,
  "summary_text" TEXT NOT NULL,
  "llm_summary_text" TEXT,
  "llm_model" TEXT DEFAULT NULL,
  "llm_updated_at" TEXT time DEFAULT NULL,
  "detail_json" TEXT DEFAULT NULL,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rd_repo_context_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_repository_file_summaries" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "file_path" TEXT NOT NULL,
  "file_path_hash" TEXT NOT NULL,
  "language" TEXT DEFAULT NULL,
  "size_bytes" INTEGER NOT NULL DEFAULT '0',
  "mtime_ms" INTEGER DEFAULT NULL,
  "content_hash" TEXT NOT NULL,
  "git_blob_sha" TEXT DEFAULT NULL,
  "summary_text" TEXT NOT NULL,
  "summary_hash" TEXT DEFAULT NULL,
  "symbols_json" TEXT DEFAULT NULL,
  "imports_json" TEXT DEFAULT NULL,
  "embedding_model" TEXT DEFAULT NULL,
  "embedding_content_hash" TEXT DEFAULT NULL,
  "last_indexed_at" TEXT time DEFAULT NULL,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rd_repo_file_summaries_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_repository_imports" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "file_path" TEXT NOT NULL,
  "language" TEXT DEFAULT NULL,
  "import_path" TEXT NOT NULL,
  "import_kind" TEXT NOT NULL,
  "line_number" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rd_repo_imports_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_repository_indexes" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'idle',
  "file_count" INTEGER NOT NULL DEFAULT '0',
  "symbol_count" INTEGER NOT NULL DEFAULT '0',
  "embedding_model" TEXT DEFAULT NULL,
  "last_indexed_at" TEXT time DEFAULT NULL,
  "detail_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_repository_indexes_project" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_repository_settings" (
  "project_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "default_test_command" TEXT DEFAULT NULL,
  "default_build_command" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("project_id"),
  CONSTRAINT "fk_rd_repo_settings_project" FOREIGN KEY ("project_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_repository_symbols" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "file_path" TEXT NOT NULL,
  "language" TEXT DEFAULT NULL,
  "symbol_name" TEXT NOT NULL,
  "symbol_kind" TEXT NOT NULL,
  "signature" TEXT DEFAULT NULL,
  "line_number" INTEGER NOT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rd_repo_symbols_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_spec_events" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "spec_id" TEXT NOT NULL,
  "event_type" TEXT NOT NULL,
  "stage" TEXT DEFAULT NULL,
  "status" TEXT DEFAULT NULL,
  "message" TEXT NOT NULL,
  "metadata_json" TEXT DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_spec_events_spec" FOREIGN KEY ("spec_id") REFERENCES "rd_specs" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_spec_task_links" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "spec_id" TEXT NOT NULL,
  "task_item_id" TEXT NOT NULL,
  "rd_task_id" TEXT DEFAULT NULL,
  "agent_task_id" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL DEFAULT 'pending',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_spec_task_links_rd_task" FOREIGN KEY ("rd_task_id") REFERENCES "rd_tasks" ("id") ON DELETE SET NULL,
  CONSTRAINT "fk_rd_spec_task_links_spec" FOREIGN KEY ("spec_id") REFERENCES "rd_specs" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_specs" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "repository_id" TEXT DEFAULT NULL,
  "title" TEXT NOT NULL,
  "prompt" TEXT NOT NULL,
  "requirements_md" TEXT,
  "design_md" TEXT,
  "tasks_md" TEXT,
  "acceptance_md" TEXT,
  "status" TEXT NOT NULL DEFAULT 'draft',
  "mode" TEXT NOT NULL DEFAULT 'plan',
  "current_stage" TEXT NOT NULL DEFAULT 'spec',
  "spec_version" INTEGER NOT NULL DEFAULT '1',
  "design_version" INTEGER NOT NULL DEFAULT '0',
  "tasks_version" INTEGER NOT NULL DEFAULT '0',
  "approved_requirements_at" TEXT time DEFAULT NULL,
  "approved_design_at" TEXT time DEFAULT NULL,
  "approved_tasks_at" TEXT time DEFAULT NULL,
  "approved_by" TEXT DEFAULT NULL,
  "stage_status_json" TEXT DEFAULT NULL,
  "task_items_json" TEXT DEFAULT NULL,
  "implementation_summary_json" TEXT DEFAULT NULL,
  "linked_agent_task_id" TEXT DEFAULT NULL,
  "last_error" TEXT,
  "model" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_specs_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE SET NULL
);

CREATE TABLE "rd_steering_rule_repositories" (
  "rule_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("rule_id","repository_id"),
  CONSTRAINT "fk_rd_steering_rule_repos_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE,
  CONSTRAINT "fk_rd_steering_rule_repos_rule" FOREIGN KEY ("rule_id") REFERENCES "rd_steering_rules" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_steering_rules" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT DEFAULT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT,
  "content_md" TEXT NOT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_steering_rules_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_task_events" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "stage" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "message" TEXT,
  "detail_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "fk_rd_task_events_task" FOREIGN KEY ("task_id") REFERENCES "rd_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_task_git_baselines" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "repository_id" TEXT NOT NULL,
  "baseline_policy" TEXT NOT NULL DEFAULT 'current_worktree',
  "head_sha" TEXT DEFAULT NULL,
  "status_short" TEXT,
  "dirty_paths_json" TEXT DEFAULT NULL,
  "tracked_diff_patch" TEXT,
  "untracked_files_json" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_task_git_baselines_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE CASCADE,
  CONSTRAINT "fk_rd_task_git_baselines_task" FOREIGN KEY ("task_id") REFERENCES "rd_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "rd_tasks" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "thread_id" TEXT DEFAULT NULL,
  "parent_task_id" TEXT DEFAULT NULL,
  "iteration_no" INTEGER NOT NULL DEFAULT '1',
  "repository_id" TEXT DEFAULT NULL,
  "spec_id" TEXT DEFAULT NULL,
  "agent_profile_id" TEXT DEFAULT NULL,
  "workflow_id" TEXT DEFAULT NULL,
  "runtime_session_id" TEXT DEFAULT NULL,
  "mode" TEXT NOT NULL DEFAULT 'ask',
  "context_profile" TEXT DEFAULT NULL,
  "context_depth" TEXT DEFAULT NULL,
  "should_deep_scan" INTEGER NOT NULL DEFAULT '0',
  "status" TEXT NOT NULL DEFAULT 'queued',
  "title" TEXT NOT NULL,
  "thread_title" TEXT DEFAULT NULL,
  "prompt" TEXT NOT NULL,
  "model" TEXT DEFAULT NULL,
  "plan_md" TEXT,
  "answer_md" TEXT,
  "review_md" TEXT,
  "pr_title" TEXT DEFAULT NULL,
  "pr_description" TEXT,
  "error_message" TEXT,
  "started_at" TEXT time DEFAULT NULL,
  "completed_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_tasks_agent_profile" FOREIGN KEY ("agent_profile_id") REFERENCES "rd_agent_profiles" ("id") ON DELETE SET NULL,
  CONSTRAINT "fk_rd_tasks_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE SET NULL,
  CONSTRAINT "fk_rd_tasks_spec" FOREIGN KEY ("spec_id") REFERENCES "rd_specs" ("id") ON DELETE SET NULL,
  CONSTRAINT "fk_rd_tasks_workflow" FOREIGN KEY ("workflow_id") REFERENCES "rd_agent_workflows" ("id") ON DELETE SET NULL
);

CREATE TABLE "rd_test_runs" (
  "id" TEXT NOT NULL,
  "task_id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "repository_id" TEXT DEFAULT NULL,
  "command" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "exit_code" INTEGER DEFAULT NULL,
  "stdout_text" TEXT,
  "stderr_text" TEXT,
  "duration_ms" INTEGER DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "fk_rd_test_runs_repo" FOREIGN KEY ("repository_id") REFERENCES "gitlab_projects" ("id") ON DELETE SET NULL,
  CONSTRAINT "fk_rd_test_runs_task" FOREIGN KEY ("task_id") REFERENCES "rd_tasks" ("id") ON DELETE CASCADE
);

CREATE TABLE "skills_market_index" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repository_id" INTEGER DEFAULT NULL,
  "source_type" TEXT NOT NULL DEFAULT 'repository',
  "repo_full_name" TEXT NOT NULL,
  "repo_url" TEXT NOT NULL,
  "branch" TEXT NOT NULL,
  "skill_name" TEXT NOT NULL,
  "skill_path" TEXT NOT NULL,
  "readme_url" TEXT DEFAULT NULL,
  "html_url" TEXT DEFAULT NULL,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "skills_market_repositories" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "repo_full_name" TEXT NOT NULL,
  "repo_url" TEXT NOT NULL,
  "branch" TEXT NOT NULL DEFAULT 'main',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "discovered_count" INTEGER NOT NULL DEFAULT '0',
  "last_scan_at" TEXT time DEFAULT NULL,
  "last_scan_status" TEXT NOT NULL DEFAULT 'idle',
  "last_scan_error" TEXT,
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "skills_registry" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT,
  "source" TEXT NOT NULL DEFAULT 'uploaded',
  "marketplace_origin_json" TEXT DEFAULT NULL,
  "path" TEXT NOT NULL,
  "tags" TEXT DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "version" TEXT NOT NULL DEFAULT '1.0.0',
  "file_size" INTEGER DEFAULT NULL,
  "created_by" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "sql_knowledge_usage_events" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "datasource_id" TEXT DEFAULT NULL,
  "event_type" TEXT NOT NULL,
  "question" TEXT,
  "query_id" TEXT DEFAULT NULL,
  "pack_id" TEXT DEFAULT NULL,
  "pack_name" TEXT DEFAULT NULL,
  "file_id" TEXT DEFAULT NULL,
  "filename" TEXT DEFAULT NULL,
  "chunk_id" TEXT DEFAULT NULL,
  "chunk_type" TEXT DEFAULT NULL,
  "start_line" INTEGER DEFAULT NULL,
  "end_line" INTEGER DEFAULT NULL,
  "score" REAL DEFAULT NULL,
  "reason" TEXT,
  "verified" INTEGER NOT NULL DEFAULT '0',
  "stale" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "super_assistant_subtasks" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "parent_turn_id" TEXT NOT NULL,
  "runtime_turn_id" TEXT DEFAULT NULL,
  "tool_call_id" TEXT NOT NULL,
  "engine" TEXT NOT NULL,
  "external_task_id" TEXT DEFAULT NULL,
  "child_session_id" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL,
  "artifact_ref" TEXT DEFAULT NULL,
  "artifact_refs_json" TEXT DEFAULT NULL,
  "permission_snapshot_json" TEXT NOT NULL,
  "input_json" TEXT DEFAULT NULL,
  "result_json" TEXT DEFAULT NULL,
  "error_message" TEXT,
  "cancel_requested" INTEGER NOT NULL DEFAULT '0',
  "attempt" INTEGER NOT NULL DEFAULT '0',
  "lease_owner" TEXT DEFAULT NULL,
  "lease_expires_at" TEXT time(3) DEFAULT NULL,
  "created_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "completed_at" TEXT time(3) DEFAULT NULL,
  PRIMARY KEY ("id")
);

CREATE TABLE "super_assistant_turn_events" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "turn_id" TEXT NOT NULL,
  "seq" INTEGER NOT NULL,
  "event_type" TEXT NOT NULL,
  "event_data" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "super_assistant_turns" (
  "id" INTEGER PRIMARY KEY AUTOINCREMENT,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "turn_id" TEXT NOT NULL,
  "runtime_turn_id" TEXT DEFAULT NULL,
  "status" TEXT NOT NULL,
  "route_capability" TEXT DEFAULT NULL,
  "execution_mode" TEXT NOT NULL DEFAULT 'unified',
  "app" TEXT DEFAULT NULL,
  "model" TEXT DEFAULT NULL,
  "user_message" TEXT,
  "final_text" TEXT,
  "error" TEXT,
  "started_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "completed_at" TEXT NULL DEFAULT NULL,
  "verification_round" INTEGER NOT NULL DEFAULT '0',
  "completion_checklist_json" TEXT DEFAULT NULL,
  "completion_decision_json" TEXT DEFAULT NULL,
  "pending_tool_calls_json" TEXT DEFAULT NULL,
  "input_context_json" TEXT DEFAULT NULL,
  "permission_snapshot_json" TEXT DEFAULT NULL,
  "next_event_seq" INTEGER NOT NULL DEFAULT '0',
  "cancel_requested" INTEGER NOT NULL DEFAULT '0',
  "cancel_history_state" INTEGER NOT NULL DEFAULT '0',
  "cancel_history_claimed_at" TEXT time(3) DEFAULT NULL,
  "attempt" INTEGER NOT NULL DEFAULT '0',
  "lease_owner" TEXT DEFAULT NULL,
  "lease_expires_at" TEXT time(3) DEFAULT NULL,
  "last_heartbeat_at" TEXT time(3) DEFAULT NULL
);

CREATE TABLE "tenant_agent_features" (
  "tenant_id" TEXT NOT NULL,
  "feature_key" TEXT NOT NULL,
  "mode" TEXT NOT NULL DEFAULT 'on',
  "config_json" TEXT DEFAULT NULL,
  "updated_at" TEXT time(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("tenant_id","feature_key")
);

CREATE TABLE "tenant_hooks" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "event_type" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "description" TEXT DEFAULT NULL,
  "scenarios" TEXT DEFAULT NULL,
  "command" TEXT NOT NULL,
  "language" TEXT NOT NULL DEFAULT 'shell',
  "code" TEXT,
  "timeout_seconds" INTEGER NOT NULL DEFAULT '30',
  "fail_fast" INTEGER NOT NULL DEFAULT '1',
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "priority" INTEGER NOT NULL DEFAULT '0',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id")
);

CREATE TABLE "tenants" (
  "id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "slug" TEXT NOT NULL,
  "plan" TEXT NOT NULL DEFAULT 'free',
  "max_users" INTEGER NOT NULL DEFAULT '5',
  "max_tokens_monthly" INTEGER NOT NULL DEFAULT '100000000',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "user_count" INTEGER NOT NULL DEFAULT '0',
  "is_system" INTEGER NOT NULL DEFAULT '0',
  "api_keys_version" INTEGER NOT NULL DEFAULT '0',
  PRIMARY KEY ("id")
);

CREATE TABLE "token_usage" (
  "id" TEXT NOT NULL,
  "request_id" TEXT DEFAULT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT DEFAULT NULL,
  "session_id" TEXT NOT NULL,
  "model" TEXT NOT NULL,
  "input_tokens" INTEGER NOT NULL DEFAULT '0',
  "output_tokens" INTEGER NOT NULL DEFAULT '0',
  "cache_creation_tokens" INTEGER NOT NULL DEFAULT '0',
  "cache_read_tokens" INTEGER NOT NULL DEFAULT '0',
  "total_tokens" INTEGER NOT NULL DEFAULT '0',
  "estimated_cost_usd" REAL NOT NULL DEFAULT '0.00000000',
  "api_key_id" TEXT DEFAULT NULL,
  "provider" TEXT NOT NULL DEFAULT 'anthropic',
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "token_usage_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE,
  CONSTRAINT "token_usage_ibfk_2" FOREIGN KEY ("api_key_id") REFERENCES "api_keys" ("id") ON DELETE SET NULL
);

CREATE TABLE "usage_alerts" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "alert_type" TEXT NOT NULL,
  "threshold_tokens" INTEGER NOT NULL,
  "threshold_usd" REAL DEFAULT NULL,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "notified_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "created_by" TEXT DEFAULT NULL,
  PRIMARY KEY ("id"),
  CONSTRAINT "usage_alerts_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE
);

CREATE TABLE "user_quotas" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "max_concurrent" INTEGER NOT NULL DEFAULT '3',
  "max_workspaces" INTEGER NOT NULL DEFAULT '10',
  "monthly_tokens_limit" INTEGER DEFAULT NULL,
  "current_tokens" INTEGER NOT NULL DEFAULT '0',
  "reset_at" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  CONSTRAINT "user_quotas_ibfk_1" FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE,
  CONSTRAINT "user_quotas_ibfk_2" FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE
);

CREATE TABLE "users" (
  "id" TEXT NOT NULL,
  "email" TEXT NOT NULL,
  "password_hash" TEXT NOT NULL,
  "name" TEXT NOT NULL DEFAULT '',
  "role" TEXT NOT NULL DEFAULT 'developer',
  "permission_mode" TEXT NOT NULL DEFAULT 'workspace_write',
  "menu_permissions_json" TEXT DEFAULT NULL,
  "tenant_id" TEXT DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "enabled" INTEGER NOT NULL DEFAULT '1',
  "is_active" INTEGER NOT NULL DEFAULT '1',
  "last_login_at" TEXT time DEFAULT NULL,
  "password_changed_at" TEXT time DEFAULT NULL,
  "created_by" TEXT DEFAULT NULL,
  "invite_token" TEXT DEFAULT NULL,
  PRIMARY KEY ("id")
);

CREATE UNIQUE INDEX "idx__agent_context_archives__uk_agent_context_archive_content" ON "agent_context_archives" ("tenant_id","user_id","session_id","window_id","ordinal","content_hash");

CREATE INDEX "idx__agent_context_archives__idx_agent_context_archives_session" ON "agent_context_archives" ("tenant_id","user_id","session_id","created_at");

CREATE INDEX "idx__agent_context_archives__idx_agent_context_archives_window" ON "agent_context_archives" ("tenant_id","user_id","session_id","window_id","ordinal");

CREATE INDEX "idx__agent_context_archives__idx_agent_context_archives_kind" ON "agent_context_archives" ("tenant_id","user_id","content_kind","created_at");

CREATE INDEX "idx_context_archive_keyset" ON "agent_context_archives" ("tenant_id","user_id","session_id","id");

CREATE INDEX "idx__agent_context_archives__ft_agent_context_archive_content" ON "agent_context_archives" ("content");

CREATE UNIQUE INDEX "idx__agent_external_identity_links__uk_agent_external_identity" ON "agent_external_identity_links" ("tenant_id","platform","external_user_id");

CREATE INDEX "idx__agent_external_identity_links__idx_agent_identity_user" ON "agent_external_identity_links" ("tenant_id","user_id","status","updated_at" DESC);

CREATE INDEX "idx__agent_external_identity_links__idx_agent_identity_mobile_destination" ON "agent_external_identity_links" ("tenant_id","user_id","status","last_seen_at" DESC,"channel_id");

CREATE UNIQUE INDEX "idx__agent_external_identity_pairings__uk_agent_pairing_code_hash" ON "agent_external_identity_pairings" ("code_hash");

CREATE INDEX "idx__agent_external_identity_pairings__idx_agent_pairing_expiry" ON "agent_external_identity_pairings" ("tenant_id","user_id","expires_at");

CREATE INDEX "idx__agent_memory_citations__idx_agent_memory_citations_session" ON "agent_memory_citations" ("tenant_id","user_id","session_id","created_at");

CREATE INDEX "idx__agent_memory_citations__idx_agent_memory_citations_memory" ON "agent_memory_citations" ("tenant_id","user_id","memory_id","created_at");

CREATE UNIQUE INDEX "idx__agent_memory_items__uk_agent_memory_content_v2" ON "agent_memory_items" ("tenant_id","user_id","scope","app","session_key","memory_type","content_hash");

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_lookup" ON "agent_memory_items" ("tenant_id","user_id","enabled","pinned","updated_at");

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_scope_app" ON "agent_memory_items" ("tenant_id","user_id","scope","app","session_id","enabled");

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_type" ON "agent_memory_items" ("tenant_id","user_id","memory_type","enabled");

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_embedding" ON "agent_memory_items" ("tenant_id","user_id","app","embedding_model");

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_user_active_recent" ON "agent_memory_items" ("tenant_id","user_id","enabled","pinned","updated_at");

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_app_session_active" ON "agent_memory_items" ("tenant_id","user_id","app","session_id","enabled","updated_at");

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_session_active_recent" ON "agent_memory_items" ("tenant_id","user_id","enabled","session_id","pinned" DESC,"updated_at" DESC,"id" DESC);

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_runtime_recall" ON "agent_memory_items" ("tenant_id","user_id","enabled","session_id","app","pinned" DESC,"updated_at" DESC,"id" DESC);

CREATE INDEX "idx__agent_memory_items__idx_agent_memory_source_page" ON "agent_memory_items" ("tenant_id","user_id","enabled","source_type","pinned" DESC,"updated_at" DESC,"id" DESC);

CREATE INDEX "idx__agent_memory_items__ft_agent_memory_content" ON "agent_memory_items" ("content");

CREATE UNIQUE INDEX "idx__agent_memory_summaries__uk_agent_memory_summary_v2" ON "agent_memory_summaries" ("tenant_id","user_id","scope","app","session_key");

CREATE INDEX "idx__agent_memory_summaries__idx_agent_memory_summary_lookup" ON "agent_memory_summaries" ("tenant_id","user_id","app","updated_at");

CREATE UNIQUE INDEX "idx__agent_notification_deliveries__uk_agent_delivery_idempotency" ON "agent_notification_deliveries" ("tenant_id","idempotency_key");

CREATE INDEX "idx__agent_notification_deliveries__idx_agent_delivery_task" ON "agent_notification_deliveries" ("tenant_id","task_id","created_at" DESC);

CREATE INDEX "idx__agent_notification_deliveries__fk_agent_delivery_outbox" ON "agent_notification_deliveries" ("outbox_id");

CREATE INDEX "idx__agent_notification_deliveries__fk_agent_delivery_task" ON "agent_notification_deliveries" ("task_id");

CREATE INDEX "idx__agent_notification_deliveries__idx_agent_delivery_ready_v2" ON "agent_notification_deliveries" ("status","available_at","created_at","id");

CREATE INDEX "idx__agent_notification_deliveries__idx_agent_delivery_lease_v2" ON "agent_notification_deliveries" ("status","lease_expires_at","dispatch_started_at");

CREATE INDEX "idx__agent_runtime_artifacts__idx_agent_runtime_artifacts_session" ON "agent_runtime_artifacts" ("tenant_id","runtime_session_id","created_at" DESC);

CREATE INDEX "idx__agent_runtime_artifacts__idx_agent_runtime_artifacts_task" ON "agent_runtime_artifacts" ("tenant_id","agent_task_id","created_at" DESC);

CREATE INDEX "idx__agent_runtime_artifacts__agent_runtime_artifacts_session_fk" ON "agent_runtime_artifacts" ("runtime_session_id");

CREATE INDEX "idx__agent_runtime_processes__idx_agent_runtime_processes_session" ON "agent_runtime_processes" ("tenant_id","runtime_session_id","created_at" DESC);

CREATE INDEX "idx__agent_runtime_processes__idx_agent_runtime_processes_task" ON "agent_runtime_processes" ("tenant_id","agent_task_id","created_at" DESC);

CREATE INDEX "idx__agent_runtime_processes__agent_runtime_processes_session_fk" ON "agent_runtime_processes" ("runtime_session_id");

CREATE INDEX "idx__agent_runtime_sessions__idx_agent_runtime_sessions_tenant_status" ON "agent_runtime_sessions" ("tenant_id","status","updated_at" DESC);

CREATE INDEX "idx__agent_runtime_sessions__idx_agent_runtime_sessions_task" ON "agent_runtime_sessions" ("tenant_id","agent_task_id","updated_at" DESC);

CREATE INDEX "idx__agent_runtime_sessions__idx_agent_runtime_sessions_heartbeat" ON "agent_runtime_sessions" ("tenant_id","status","heartbeat_at");

CREATE INDEX "idx__agent_session_compactions__idx_agent_session_compactions_session" ON "agent_session_compactions" ("tenant_id","user_id","session_id","created_at");

CREATE INDEX "idx__agent_sessions__idx_agent_sessions_tenant_user" ON "agent_sessions" ("tenant_id","user_id");

CREATE INDEX "idx__agent_sessions__idx_agent_sessions_session" ON "agent_sessions" ("session_id");

CREATE INDEX "idx__agent_sessions__idx_agent_sessions_state" ON "agent_sessions" ("state");

CREATE INDEX "idx__agent_sessions__user_id" ON "agent_sessions" ("user_id");

CREATE UNIQUE INDEX "idx__agent_task_artifacts__uk_agent_task_artifact_ref" ON "agent_task_artifacts" ("tenant_id","task_id","artifact_ref");

CREATE INDEX "idx__agent_task_artifacts__idx_agent_task_artifact_owner" ON "agent_task_artifacts" ("tenant_id","owner_user_id","created_at" DESC);

CREATE INDEX "idx__agent_task_artifacts__fk_agent_task_artifact_task" ON "agent_task_artifacts" ("task_id");

CREATE UNIQUE INDEX "idx__agent_task_attempts__uk_agent_task_attempt_no" ON "agent_task_attempts" ("tenant_id","task_id","attempt_no");

CREATE INDEX "idx__agent_task_attempts__idx_agent_task_attempt_status" ON "agent_task_attempts" ("tenant_id","status","updated_at" DESC);

CREATE INDEX "idx__agent_task_attempts__fk_agent_task_attempt_task" ON "agent_task_attempts" ("task_id");

CREATE UNIQUE INDEX "idx__agent_task_command_requests__uk_agent_task_command_idempotency" ON "agent_task_command_requests" ("tenant_id","idempotency_key");

CREATE UNIQUE INDEX "idx__agent_task_command_requests__uk_agent_task_active_retry" ON "agent_task_command_requests" ("tenant_id","active_retry_task_id");

CREATE INDEX "idx__agent_task_command_requests__idx_agent_task_command_task" ON "agent_task_command_requests" ("tenant_id","task_id","created_at" DESC);

CREATE INDEX "idx__agent_task_command_requests__fk_agent_task_command_task" ON "agent_task_command_requests" ("task_id");

CREATE INDEX "idx__agent_task_command_requests__idx_agent_task_command_ready_v2" ON "agent_task_command_requests" ("status","available_at","created_at","id");

CREATE INDEX "idx__agent_task_command_requests__idx_agent_task_command_lease_v2" ON "agent_task_command_requests" ("status","lease_expires_at");

CREATE INDEX "idx__agent_task_events__idx_agent_task_events_task_time" ON "agent_task_events" ("tenant_id","task_id","created_at" DESC);

CREATE INDEX "idx__agent_task_events__idx_agent_task_events_type_time" ON "agent_task_events" ("tenant_id","event_type","created_at" DESC);

CREATE INDEX "idx__agent_task_events__agent_task_events_task_fk" ON "agent_task_events" ("task_id");

CREATE UNIQUE INDEX "idx__agent_task_grants__uk_agent_task_grant" ON "agent_task_grants" ("tenant_id","task_id","grantee_type","grantee_id","permission");

CREATE INDEX "idx__agent_task_grants__idx_agent_task_grantee" ON "agent_task_grants" ("tenant_id","grantee_type","grantee_id","revoked_at","created_at" DESC);

CREATE INDEX "idx__agent_task_grants__fk_agent_task_grant_task" ON "agent_task_grants" ("task_id");

CREATE UNIQUE INDEX "idx__agent_task_outbox__uk_agent_outbox_event" ON "agent_task_outbox" ("tenant_id","event_id");

CREATE UNIQUE INDEX "idx__agent_task_outbox__uk_agent_outbox_task_version_event" ON "agent_task_outbox" ("tenant_id","task_id","state_version","event_type");

CREATE INDEX "idx__agent_task_outbox__idx_agent_outbox_stream" ON "agent_task_outbox" ("tenant_id","id");

CREATE INDEX "idx__agent_task_outbox__idx_agent_outbox_task" ON "agent_task_outbox" ("tenant_id","task_id","id");

CREATE INDEX "idx__agent_task_outbox__fk_agent_outbox_task" ON "agent_task_outbox" ("task_id");

CREATE INDEX "idx__agent_task_outbox__idx_agent_outbox_root_stream" ON "agent_task_outbox" ("tenant_id","root_task_id","id");

CREATE INDEX "idx__agent_task_outbox__idx_agent_outbox_ready_v2" ON "agent_task_outbox" ("status","available_at","id");

CREATE INDEX "idx__agent_task_outbox__idx_agent_outbox_lease_v2" ON "agent_task_outbox" ("status","lease_expires_at");

CREATE INDEX "idx__agent_task_outbox__idx_agent_outbox_tenant_cursor_v2" ON "agent_task_outbox" ("tenant_id","id");

CREATE UNIQUE INDEX "idx__agent_task_resource_links__uk_agent_task_resource" ON "agent_task_resource_links" ("tenant_id","task_id","resource_type","resource_id","relation_type");

CREATE INDEX "idx__agent_task_resource_links__idx_agent_resource_lookup" ON "agent_task_resource_links" ("tenant_id","resource_type","resource_id","created_at" DESC);

CREATE INDEX "idx__agent_task_resource_links__idx_agent_resource_root" ON "agent_task_resource_links" ("tenant_id","root_task_id","created_at");

CREATE INDEX "idx__agent_task_resource_links__fk_agent_resource_task" ON "agent_task_resource_links" ("task_id");

CREATE UNIQUE INDEX "idx__agent_task_subscriptions__uk_agent_subscription_destination" ON "agent_task_subscriptions" ("tenant_id","task_key","user_id","destination_type","destination_key");

CREATE INDEX "idx__agent_task_subscriptions__idx_agent_subscription_match" ON "agent_task_subscriptions" ("tenant_id","task_id","user_id","enabled","updated_at" DESC);

CREATE INDEX "idx__agent_task_subscriptions__fk_agent_subscription_task" ON "agent_task_subscriptions" ("task_id");

CREATE UNIQUE INDEX "idx__agent_tasks__idx_agent_tasks_idempotency" ON "agent_tasks" ("tenant_id","idempotency_key");

CREATE UNIQUE INDEX "idx__agent_tasks__uk_agent_tasks_tenant_short_code" ON "agent_tasks" ("tenant_id","short_code");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_tenant_status" ON "agent_tasks" ("tenant_id","status","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_tenant_capability" ON "agent_tasks" ("tenant_id","capability_key","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_external_scope" ON "agent_tasks" ("tenant_id","external_platform","external_channel_id","external_conversation_id","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_owner" ON "agent_tasks" ("tenant_id","owner_user_id","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_linked" ON "agent_tasks" ("tenant_id","linked_resource_type","linked_resource_id");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_correlation" ON "agent_tasks" ("tenant_id","correlation_id");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_queue_claim" ON "agent_tasks" ("tenant_id","queue_status","available_at","priority","created_at");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_queue_lease" ON "agent_tasks" ("tenant_id","queue_status","lease_expires_at","updated_at");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_root" ON "agent_tasks" ("tenant_id","root_task_id","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_origin_turn" ON "agent_tasks" ("tenant_id","initiator_user_id","origin_turn_id");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_owner_active" ON "agent_tasks" ("tenant_id","owner_user_id","archived","status","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_team_active" ON "agent_tasks" ("tenant_id","team_id","archived","status","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_assignee" ON "agent_tasks" ("tenant_id","assigned_user_id","archived","status","updated_at" DESC);

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_sla" ON "agent_tasks" ("tenant_id","status","sla_due_at");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_stale_scan" ON "agent_tasks" ("status","last_heartbeat_at","id");

CREATE INDEX "idx__agent_tasks__idx_agent_tasks_projection_queue" ON "agent_tasks" ("projection_active","last_heartbeat_at","id");

CREATE INDEX "idx__agent_thread_memory_state__idx_agent_thread_memory_state_user" ON "agent_thread_memory_state" ("tenant_id","user_id","updated_at");

CREATE INDEX "idx__agent_trace_events__idx_agent_trace_task_time" ON "agent_trace_events" ("tenant_id","task_id","created_at" DESC);

CREATE INDEX "idx__agent_trace_events__idx_agent_trace_type_time" ON "agent_trace_events" ("tenant_id","event_type","created_at" DESC);

CREATE INDEX "idx__agent_trace_events__idx_agent_trace_runtime" ON "agent_trace_events" ("tenant_id","runtime_session_id","runtime_process_id","created_at" DESC);

CREATE INDEX "idx__agent_trace_events__agent_trace_events_task_fk" ON "agent_trace_events" ("task_id");

CREATE INDEX "idx__agent_user_presence_leases__idx_agent_presence_expiry" ON "agent_user_presence_leases" ("tenant_id","user_id","expires_at");

CREATE UNIQUE INDEX "idx__agent_watch_rule_runs__uk_agent_watch_rule_event" ON "agent_watch_rule_runs" ("tenant_id","rule_id","task_id","outbox_id");

CREATE INDEX "idx__agent_watch_rule_runs__idx_agent_watch_rule_audit" ON "agent_watch_rule_runs" ("tenant_id","rule_id","created_at" DESC);

CREATE INDEX "idx__agent_watch_rule_runs__fk_agent_watch_rule_run_rule" ON "agent_watch_rule_runs" ("rule_id");

CREATE INDEX "idx__agent_watch_rule_runs__fk_agent_watch_rule_run_task" ON "agent_watch_rule_runs" ("task_id");

CREATE INDEX "idx__agent_watch_rules__idx_agent_watch_rule_match" ON "agent_watch_rules" ("tenant_id","user_id","enabled","updated_at" DESC);

CREATE UNIQUE INDEX "idx__agent_workspace_entries__uk_workspace_path_version" ON "agent_workspace_entries" ("tenant_id","workspace_id","virtual_path","version");

CREATE INDEX "idx__agent_workspace_entries__idx_workspace_entry_owner" ON "agent_workspace_entries" ("tenant_id","owner_user_id","workspace_id","enabled");

CREATE INDEX "idx__agent_workspace_entries__idx_workspace_entry_path" ON "agent_workspace_entries" ("tenant_id","workspace_id","enabled","is_current","virtual_path");

CREATE INDEX "idx__agent_workspace_entries__idx_workspace_entry_resource" ON "agent_workspace_entries" ("tenant_id","workspace_id","resource_type","resource_id","enabled");

CREATE INDEX "idx_workspace_shared_keyset" ON "agent_workspace_entries" ("tenant_id","visibility","enabled","is_current","virtual_path","resource_id");

CREATE INDEX "idx__agent_workspace_entries__idx_workspace_owner_visibility_id" ON "agent_workspace_entries" ("tenant_id","owner_user_id","visibility","id");

CREATE UNIQUE INDEX "idx__agent_workspace_grants__uk_workspace_entry_grant" ON "agent_workspace_grants" ("tenant_id","workspace_id","entry_id","grantee_user_id");

CREATE INDEX "idx__agent_workspace_grants__idx_workspace_grant_grantee" ON "agent_workspace_grants" ("tenant_id","grantee_user_id","enabled","workspace_id");

CREATE INDEX "idx__agent_workspace_grants__idx_workspace_grant_entry" ON "agent_workspace_grants" ("tenant_id","workspace_id","entry_id","enabled","revoked_at");

CREATE INDEX "idx__agent_workspace_grants__idx_workspace_grantee_id" ON "agent_workspace_grants" ("tenant_id","grantee_user_id","id");

CREATE UNIQUE INDEX "idx__agent_workspace_mounts__uk_workspace_mount" ON "agent_workspace_mounts" ("tenant_id","workspace_id","virtual_root");

CREATE INDEX "idx__agent_workspace_usage__idx_workspace_usage_actor" ON "agent_workspace_usage" ("tenant_id","user_id","workspace_id","created_at");

CREATE INDEX "idx__agent_workspaces__idx_agent_workspaces_owner" ON "agent_workspaces" ("tenant_id","owner_user_id","enabled");

CREATE INDEX "idx__api_keys__idx_api_keys_tenant" ON "api_keys" ("tenant_id");

CREATE INDEX "idx__api_keys__idx_api_keys_tenant_type" ON "api_keys" ("tenant_id","model_type");

CREATE INDEX "idx__api_keys__idx_api_keys_scenarios" ON "api_keys" ("scenarios_list");

CREATE INDEX "idx__audit_log__idx_audit_log_tenant_created" ON "audit_log" ("tenant_id","created_at");

CREATE UNIQUE INDEX "idx__bot_agent_capabilities__uk_bot_agent_capability" ON "bot_agent_capabilities" ("tenant_id","agent_id","capability_key");

CREATE INDEX "idx__bot_agent_capabilities__idx_bot_agent_capabilities_agent" ON "bot_agent_capabilities" ("tenant_id","agent_id");

CREATE INDEX "idx__bot_agent_channels__idx_bot_agent_channels_agent" ON "bot_agent_channels" ("tenant_id","agent_id");

CREATE INDEX "idx__bot_agent_channels__idx_bot_agent_channels_platform" ON "bot_agent_channels" ("tenant_id","platform","enabled");

CREATE INDEX "idx__bot_agent_channels__idx_bot_agent_channels_inbound_runtime" ON "bot_agent_channels" ("enabled","platform","inbound_mode","updated_at");

CREATE UNIQUE INDEX "idx__bot_agents__uk_bot_agents_tenant_name" ON "bot_agents" ("tenant_id","name");

CREATE INDEX "idx__bot_agents__idx_bot_agents_tenant_enabled" ON "bot_agents" ("tenant_id","enabled");

CREATE INDEX "idx__bot_message_logs__idx_bot_message_logs_tenant_time" ON "bot_message_logs" ("tenant_id","created_at" DESC);

CREATE INDEX "idx__bot_message_logs__idx_bot_message_logs_channel_time" ON "bot_message_logs" ("tenant_id","channel_id","created_at" DESC);

CREATE INDEX "idx__bot_message_logs__idx_bot_message_logs_external" ON "bot_message_logs" ("tenant_id","platform","external_conversation_id");

CREATE INDEX "idx__bot_message_logs__idx_bot_message_queue_claim" ON "bot_message_logs" ("direction","queue_status","available_at","created_at");

CREATE INDEX "idx__bot_message_logs__idx_bot_message_queue_stale_v2" ON "bot_message_logs" ("direction","queue_status","claimed_at","attempt_count");

CREATE INDEX "idx__bot_message_logs__idx_bot_message_queue_tenant_ready_v2" ON "bot_message_logs" ("tenant_id","direction","queue_status","available_at","created_at","id");

CREATE INDEX "idx__chat_adversarial_runs__idx_chat_adv_tenant_user_created" ON "chat_adversarial_runs" ("tenant_id","user_id","created_at");

CREATE INDEX "idx__chat_adversarial_runs__idx_chat_adv_status" ON "chat_adversarial_runs" ("tenant_id","status","updated_at");

CREATE INDEX "idx__chat_adversarial_runs__chat_adversarial_runs_user_fk" ON "chat_adversarial_runs" ("user_id");

CREATE INDEX "idx__chat_adversarial_runs__idx_chat_adv_thread" ON "chat_adversarial_runs" ("tenant_id","user_id","thread_id","iteration_no");

CREATE INDEX "idx__chat_adversarial_runs__idx_chat_adv_parent" ON "chat_adversarial_runs" ("tenant_id","user_id","parent_run_id");

CREATE INDEX "idx__chat_adversarial_runs__idx_chat_adv_session_status" ON "chat_adversarial_runs" ("tenant_id","user_id","session_id","status","updated_at");

CREATE INDEX "idx__chat_adversarial_threads__idx_chat_adv_threads_user_list" ON "chat_adversarial_threads" ("tenant_id","user_id","deleted_at","is_pinned","updated_at");

CREATE INDEX "idx__chat_adversarial_threads__chat_adversarial_threads_user_fk" ON "chat_adversarial_threads" ("user_id");

CREATE UNIQUE INDEX "idx__chat_file_workspace_chunks__uk_chat_file_workspace_chunk" ON "chat_file_workspace_chunks" ("tenant_id","user_id","file_id","chunk_index");

CREATE INDEX "idx__chat_file_workspace_chunks__idx_chat_file_workspace_embedding" ON "chat_file_workspace_chunks" ("tenant_id","user_id","embedding_model");

CREATE INDEX "idx__chat_file_workspace_chunks__ft_chat_file_workspace_content" ON "chat_file_workspace_chunks" ("content");

CREATE UNIQUE INDEX "idx__chat_file_workspace_files__uk_chat_file_workspace_file" ON "chat_file_workspace_files" ("tenant_id","user_id","file_id");

CREATE INDEX "idx__chat_file_workspace_files__idx_chat_file_workspace_user_status" ON "chat_file_workspace_files" ("tenant_id","user_id","status","updated_at");

CREATE INDEX "idx__chat_file_workspace_files__idx_chat_file_workspace_session" ON "chat_file_workspace_files" ("tenant_id","user_id","session_id","updated_at");

CREATE INDEX "idx_chat_workspace_keyset" ON "chat_file_workspace_files" ("tenant_id","user_id","session_id","status","file_id");

CREATE INDEX "idx__chat_memories__idx_chat_memories_user_enabled" ON "chat_memories" ("tenant_id","user_id","enabled","pinned","updated_at");

CREATE INDEX "idx__chat_memories__idx_chat_memories_type" ON "chat_memories" ("tenant_id","user_id","memory_type","enabled");

CREATE INDEX "idx__chat_turn_artifacts__idx_chat_turn_artifacts_session" ON "chat_turn_artifacts" ("tenant_id","user_id","session_id","created_at");

CREATE INDEX "idx__chat_turn_artifacts__idx_chat_turn_artifacts_type" ON "chat_turn_artifacts" ("tenant_id","user_id","artifact_type","created_at");

CREATE INDEX "idx_chat_artifact_keyset" ON "chat_turn_artifacts" ("tenant_id","user_id","session_id","id");

CREATE UNIQUE INDEX "idx__data_sources__uk_tenant_user_name" ON "data_sources" ("tenant_id","user_id_key","name");

CREATE INDEX "idx__data_sources__idx_ds_tenant" ON "data_sources" ("tenant_id");

CREATE INDEX "idx__data_sources__idx_ds_user" ON "data_sources" ("user_id");

CREATE INDEX "idx__data_sources__idx_ds_deleted" ON "data_sources" ("deleted_at");

CREATE INDEX "idx__gitlab_projects__idx_gitlab_projects_tenant_user" ON "gitlab_projects" ("tenant_id","user_id");

CREATE INDEX "idx__gitlab_projects__user_id" ON "gitlab_projects" ("user_id");

CREATE INDEX "idx__hook_execution_logs__idx_hook_logs_tenant" ON "hook_execution_logs" ("tenant_id");

CREATE INDEX "idx__hook_execution_logs__idx_hook_logs_hook" ON "hook_execution_logs" ("hook_id");

CREATE INDEX "idx__hook_execution_logs__idx_hook_logs_executed" ON "hook_execution_logs" ("tenant_id","hook_id","executed_at" DESC);

CREATE INDEX "idx__hook_execution_logs__idx_hook_logs_scenario" ON "hook_execution_logs" ("tenant_id","scenario","executed_at" DESC);

CREATE UNIQUE INDEX "idx__mcp_server_registry__uk_mcp_tenant_name" ON "mcp_server_registry" ("tenant_id","name");

CREATE INDEX "idx__mcp_server_registry__idx_mcp_registry_tenant" ON "mcp_server_registry" ("tenant_id");

CREATE INDEX "idx__mcp_server_registry__idx_mcp_tenant_enabled" ON "mcp_server_registry" ("tenant_id","enabled");

CREATE INDEX "idx__mcp_server_registry__idx_mcp_name" ON "mcp_server_registry" ("name");

CREATE INDEX "idx__nl2sql_affected_queries__idx_notification" ON "nl2sql_affected_queries" ("notification_id");

CREATE UNIQUE INDEX "idx__nl2sql_agent_query_results__uk_nl2sql_agent_result_query" ON "nl2sql_agent_query_results" ("query_id");

CREATE INDEX "idx__nl2sql_agent_query_results__idx_nl2sql_agent_result_tenant" ON "nl2sql_agent_query_results" ("tenant_id");

CREATE INDEX "idx__nl2sql_agent_query_results__idx_nl2sql_agent_result_tenant_user" ON "nl2sql_agent_query_results" ("tenant_id","user_id");

CREATE INDEX "idx__nl2sql_agent_query_results__idx_nl2sql_agent_result_conversation" ON "nl2sql_agent_query_results" ("conversation_id");

CREATE INDEX "idx__nl2sql_attribution_conversations__idx_nl2sql_attr_conv_user" ON "nl2sql_attribution_conversations" ("tenant_id","user_id","deleted_at","updated_at" DESC);

CREATE INDEX "idx__nl2sql_attribution_tasks__idx_nl2sql_attr_tasks_conv" ON "nl2sql_attribution_tasks" ("tenant_id","conversation_id","created_at" DESC);

CREATE INDEX "idx__nl2sql_attribution_tasks__idx_nl2sql_attr_tasks_user" ON "nl2sql_attribution_tasks" ("tenant_id","user_id","created_at" DESC);

CREATE INDEX "idx__nl2sql_attribution_tasks__idx_nl2sql_attr_tasks_parent" ON "nl2sql_attribution_tasks" ("tenant_id","parent_task_id");

CREATE INDEX "idx__nl2sql_attribution_tasks__idx_nl2sql_attr_tasks_conv_user_created" ON "nl2sql_attribution_tasks" ("tenant_id","user_id","conversation_id","created_at","task_id");

CREATE INDEX "idx__nl2sql_attribution_tasks__idx_nl2sql_attr_tasks_active" ON "nl2sql_attribution_tasks" ("tenant_id","user_id","status","cancel_requested","updated_at");

CREATE UNIQUE INDEX "idx__nl2sql_business_domains__uk_ds_domain" ON "nl2sql_business_domains" ("datasource_id","domain_name");

CREATE INDEX "idx__nl2sql_business_domains__idx_tenant" ON "nl2sql_business_domains" ("tenant_id");

CREATE INDEX "idx__nl2sql_business_domains__idx_datasource" ON "nl2sql_business_domains" ("datasource_id");

CREATE INDEX "idx__nl2sql_business_domains__idx_source" ON "nl2sql_business_domains" ("datasource_id","source");

CREATE INDEX "idx__nl2sql_business_domains__idx_deleted" ON "nl2sql_business_domains" ("deleted_at");

CREATE INDEX "idx__nl2sql_business_domains__idx_status" ON "nl2sql_business_domains" ("status");

CREATE INDEX "idx__nl2sql_business_domains__idx_datasource_status" ON "nl2sql_business_domains" ("datasource_id","status");

CREATE INDEX "idx__nl2sql_clarification_messages__idx_nl2sql_clarify_tenant_conv_created" ON "nl2sql_clarification_messages" ("tenant_id","conversation_id","created_at");

CREATE INDEX "idx__nl2sql_clarification_messages__idx_nl2sql_clarify_conv_created" ON "nl2sql_clarification_messages" ("conversation_id","created_at");

CREATE INDEX "idx__nl2sql_clarification_messages__idx_nl2sql_clarify_session" ON "nl2sql_clarification_messages" ("session_id");

CREATE INDEX "idx__nl2sql_clarification_messages__idx_nl2sql_clarify_deleted" ON "nl2sql_clarification_messages" ("deleted_at");

CREATE INDEX "idx__nl2sql_column_masking_rules__idx_tenant" ON "nl2sql_column_masking_rules" ("tenant_id");

CREATE INDEX "idx__nl2sql_column_masking_rules__idx_datasource" ON "nl2sql_column_masking_rules" ("datasource_id");

CREATE INDEX "idx__nl2sql_column_masking_rules__idx_table_pattern" ON "nl2sql_column_masking_rules" ("table_name");

CREATE INDEX "idx__nl2sql_column_masking_rules__idx_column_pattern" ON "nl2sql_column_masking_rules" ("column_name");

CREATE INDEX "idx__nl2sql_column_masking_rules__idx_priority" ON "nl2sql_column_masking_rules" ("priority");

CREATE INDEX "idx__nl2sql_column_masking_rules__idx_enabled" ON "nl2sql_column_masking_rules" ("enabled");

CREATE UNIQUE INDEX "idx__nl2sql_column_stats__uk_ds_table_column" ON "nl2sql_column_stats" ("datasource_id","table_name","column_name");

CREATE INDEX "idx__nl2sql_column_stats__idx_datasource" ON "nl2sql_column_stats" ("datasource_id");

CREATE INDEX "idx__nl2sql_column_stats__idx_tenant" ON "nl2sql_column_stats" ("tenant_id");

CREATE INDEX "idx__nl2sql_column_stats__idx_tenant_ds" ON "nl2sql_column_stats" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_column_stats__idx_deleted" ON "nl2sql_column_stats" ("deleted_at");

CREATE INDEX "idx__nl2sql_conversations__idx_tenant_user" ON "nl2sql_conversations" ("tenant_id","user_id");

CREATE INDEX "idx__nl2sql_conversations__idx_updated" ON "nl2sql_conversations" ("updated_at");

CREATE INDEX "idx__nl2sql_conversations__idx_nl2sql_conv_deleted" ON "nl2sql_conversations" ("deleted_at");

CREATE UNIQUE INDEX "idx__nl2sql_cross_datasource_relations__uk_relation" ON "nl2sql_cross_datasource_relations" ("tenant_id","relation_hash");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__idx_tenant" ON "nl2sql_cross_datasource_relations" ("tenant_id");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__idx_left_ds" ON "nl2sql_cross_datasource_relations" ("tenant_id","left_datasource_id");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__idx_right_ds" ON "nl2sql_cross_datasource_relations" ("tenant_id","right_datasource_id");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__idx_cds_verified" ON "nl2sql_cross_datasource_relations" ("verified");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__idx_cds_confidence" ON "nl2sql_cross_datasource_relations" ("confidence");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__idx_deleted" ON "nl2sql_cross_datasource_relations" ("deleted_at");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__fk_cdr_left_ds" ON "nl2sql_cross_datasource_relations" ("left_datasource_id");

CREATE INDEX "idx__nl2sql_cross_datasource_relations__fk_cdr_right_ds" ON "nl2sql_cross_datasource_relations" ("right_datasource_id");

CREATE UNIQUE INDEX "idx__nl2sql_cross_domain_clusters__uk_tenant_cluster" ON "nl2sql_cross_domain_clusters" ("tenant_id","cluster_name");

CREATE INDEX "idx__nl2sql_cross_domain_clusters__idx_tenant" ON "nl2sql_cross_domain_clusters" ("tenant_id");

CREATE INDEX "idx__nl2sql_cross_domain_clusters__idx_nl2sql_cdc_deleted" ON "nl2sql_cross_domain_clusters" ("deleted_at");

CREATE UNIQUE INDEX "idx__nl2sql_datasource_semantics__uk_ds" ON "nl2sql_datasource_semantics" ("datasource_id");

CREATE INDEX "idx__nl2sql_datasource_semantics__idx_tenant" ON "nl2sql_datasource_semantics" ("tenant_id");

CREATE INDEX "idx__nl2sql_datasource_semantics__idx_status" ON "nl2sql_datasource_semantics" ("status");

CREATE INDEX "idx__nl2sql_datasource_semantics__idx_datasource_status" ON "nl2sql_datasource_semantics" ("datasource_id","status");

CREATE INDEX "idx__nl2sql_datasource_semantics__idx_dss_deleted" ON "nl2sql_datasource_semantics" ("datasource_id","deleted_at");

CREATE UNIQUE INDEX "idx__nl2sql_foreign_keys__uk_ds_src_tgt" ON "nl2sql_foreign_keys" ("datasource_id","source_table","source_column","target_table","target_column");

CREATE INDEX "idx__nl2sql_foreign_keys__idx_tenant_ds" ON "nl2sql_foreign_keys" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_foreign_keys__idx_nl2sql_fk_deleted" ON "nl2sql_foreign_keys" ("deleted_at");

CREATE INDEX "idx__nl2sql_foreign_keys__idx_status" ON "nl2sql_foreign_keys" ("status");

CREATE INDEX "idx__nl2sql_foreign_keys__idx_datasource_status" ON "nl2sql_foreign_keys" ("datasource_id","status");

CREATE UNIQUE INDEX "idx__nl2sql_join_paths__uk_ds_src_tgt" ON "nl2sql_join_paths" ("datasource_id","source_table","target_table","hops");

CREATE INDEX "idx__nl2sql_join_paths__idx_datasource" ON "nl2sql_join_paths" ("datasource_id");

CREATE INDEX "idx__nl2sql_join_paths__idx_source" ON "nl2sql_join_paths" ("datasource_id","source_table");

CREATE INDEX "idx__nl2sql_join_paths__idx_target" ON "nl2sql_join_paths" ("datasource_id","target_table");

CREATE INDEX "idx__nl2sql_join_paths__idx_tenant_id" ON "nl2sql_join_paths" ("tenant_id");

CREATE INDEX "idx__nl2sql_join_paths__idx_verified" ON "nl2sql_join_paths" ("verified");

CREATE INDEX "idx__nl2sql_join_paths__idx_tenant_ds_verified" ON "nl2sql_join_paths" ("tenant_id","datasource_id","verified");

CREATE INDEX "idx__nl2sql_join_paths__idx_jp_tenant_id" ON "nl2sql_join_paths" ("tenant_id");

CREATE INDEX "idx__nl2sql_join_paths__idx_jp_verified" ON "nl2sql_join_paths" ("verified");

CREATE INDEX "idx__nl2sql_join_paths__idx_jp_tenant_ds" ON "nl2sql_join_paths" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_join_paths__idx_deleted" ON "nl2sql_join_paths" ("deleted_at");

CREATE INDEX "idx__nl2sql_metric_approvals__idx_metric_id" ON "nl2sql_metric_approvals" ("metric_id");

CREATE INDEX "idx__nl2sql_metric_approvals__idx_reviewer" ON "nl2sql_metric_approvals" ("reviewer_id");

CREATE INDEX "idx__nl2sql_metric_approvals__idx_created_at" ON "nl2sql_metric_approvals" ("created_at");

CREATE UNIQUE INDEX "idx__nl2sql_metric_versions__uk_metric_version" ON "nl2sql_metric_versions" ("metric_id","version");

CREATE INDEX "idx__nl2sql_metric_versions__idx_metric_id" ON "nl2sql_metric_versions" ("metric_id");

CREATE INDEX "idx__nl2sql_metric_versions__idx_created_at" ON "nl2sql_metric_versions" ("created_at");

CREATE UNIQUE INDEX "idx__nl2sql_metrics__uk_tenant_ds_metric" ON "nl2sql_metrics" ("tenant_id","datasource_id","metric_name");

CREATE INDEX "idx__nl2sql_metrics__idx_tenant" ON "nl2sql_metrics" ("tenant_id");

CREATE INDEX "idx__nl2sql_metrics__idx_datasource" ON "nl2sql_metrics" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_metrics__idx_nl2sql_metrics_deleted" ON "nl2sql_metrics" ("deleted_at");

CREATE INDEX "idx__nl2sql_metrics__fk_m_ds" ON "nl2sql_metrics" ("datasource_id");

CREATE INDEX "idx__nl2sql_queries__idx_nl2sql_tenant" ON "nl2sql_queries" ("tenant_id");

CREATE INDEX "idx__nl2sql_queries__idx_nl2sql_user" ON "nl2sql_queries" ("user_id");

CREATE INDEX "idx__nl2sql_queries__idx_nl2sql_ds" ON "nl2sql_queries" ("data_source_id");

CREATE INDEX "idx__nl2sql_queries__idx_nl2sql_created" ON "nl2sql_queries" ("created_at");

CREATE INDEX "idx__nl2sql_queries__idx_nl2sql_conversation" ON "nl2sql_queries" ("conversation_id");

CREATE INDEX "idx__nl2sql_queries__idx_planning_ms" ON "nl2sql_queries" ("planning_ms");

CREATE INDEX "idx__nl2sql_queries__idx_deleted" ON "nl2sql_queries" ("deleted_at");

CREATE INDEX "idx__nl2sql_queries__idx_nl2sql_analytics_range" ON "nl2sql_queries" ("tenant_id","deleted_at","created_at","execution_ms");

CREATE UNIQUE INDEX "idx__nl2sql_query_feedback__uk_query_user" ON "nl2sql_query_feedback" ("query_id","created_by");

CREATE INDEX "idx__nl2sql_query_feedback__idx_tenant" ON "nl2sql_query_feedback" ("tenant_id");

CREATE INDEX "idx__nl2sql_query_feedback__idx_conversation" ON "nl2sql_query_feedback" ("conversation_id");

CREATE INDEX "idx__nl2sql_query_feedback__idx_feedback_type" ON "nl2sql_query_feedback" ("feedback_type");

CREATE INDEX "idx__nl2sql_query_feedback__idx_created_at" ON "nl2sql_query_feedback" ("created_at");

CREATE INDEX "idx__nl2sql_query_feedback__idx_correction_accepted" ON "nl2sql_query_feedback" ("correction_accepted");

CREATE INDEX "idx__nl2sql_query_feedback__idx_generation_confidence" ON "nl2sql_query_feedback" ("generation_confidence");

CREATE UNIQUE INDEX "idx__nl2sql_query_policies__uk_tenant_ds_user" ON "nl2sql_query_policies" ("tenant_id","datasource_id","user_id");

CREATE INDEX "idx__nl2sql_query_policies__idx_tenant" ON "nl2sql_query_policies" ("tenant_id");

CREATE INDEX "idx__nl2sql_query_policies__idx_ds" ON "nl2sql_query_policies" ("datasource_id");

CREATE INDEX "idx__nl2sql_query_policies__idx_tenant_user" ON "nl2sql_query_policies" ("tenant_id","user_id");

CREATE INDEX "idx__nl2sql_query_policies__idx_ds_user" ON "nl2sql_query_policies" ("datasource_id","user_id");

CREATE INDEX "idx__nl2sql_query_reference_usages__idx_nl2sql_ref_usage_query" ON "nl2sql_query_reference_usages" ("tenant_id","query_id");

CREATE INDEX "idx__nl2sql_query_reference_usages__idx_nl2sql_ref_usage_ds" ON "nl2sql_query_reference_usages" ("tenant_id","datasource_id","created_at" DESC);

CREATE UNIQUE INDEX "idx__nl2sql_query_understanding_cache__uk_hash_ds" ON "nl2sql_query_understanding_cache" ("question_hash","datasource_id");

CREATE INDEX "idx__nl2sql_query_understanding_cache__idx_tenant" ON "nl2sql_query_understanding_cache" ("tenant_id");

CREATE INDEX "idx__nl2sql_query_understanding_cache__idx_datasource" ON "nl2sql_query_understanding_cache" ("datasource_id");

CREATE INDEX "idx__nl2sql_query_understanding_cache__idx_resolved" ON "nl2sql_query_understanding_cache" ("resolved_at");

CREATE UNIQUE INDEX "idx__nl2sql_reference_chunks__uk_nl2sql_ref_chunk_order" ON "nl2sql_reference_chunks" ("tenant_id","file_id","chunk_index");

CREATE INDEX "idx__nl2sql_reference_chunks__idx_nl2sql_ref_chunks_lookup" ON "nl2sql_reference_chunks" ("tenant_id","datasource_id","pack_id","file_id","chunk_index");

CREATE INDEX "idx__nl2sql_reference_chunks__idx_nl2sql_ref_chunks_embedding" ON "nl2sql_reference_chunks" ("tenant_id","datasource_id","embedding_model");

CREATE INDEX "idx__nl2sql_reference_chunks__ft_nl2sql_ref_chunks" ON "nl2sql_reference_chunks" ("content_text","keywords_text");

CREATE UNIQUE INDEX "idx__nl2sql_reference_files__uk_nl2sql_ref_file_hash" ON "nl2sql_reference_files" ("tenant_id","pack_id","content_hash");

CREATE INDEX "idx__nl2sql_reference_files__idx_nl2sql_ref_files_pack" ON "nl2sql_reference_files" ("tenant_id","pack_id","updated_at" DESC);

CREATE INDEX "idx__nl2sql_reference_files__idx_nl2sql_ref_files_ds" ON "nl2sql_reference_files" ("tenant_id","datasource_id","updated_at" DESC);

CREATE INDEX "idx__nl2sql_reference_packs__idx_nl2sql_ref_packs_ds" ON "nl2sql_reference_packs" ("tenant_id","datasource_id","enabled","updated_at" DESC);

CREATE INDEX "idx__nl2sql_reference_packs__idx_nl2sql_ref_packs_user" ON "nl2sql_reference_packs" ("tenant_id","user_id","updated_at" DESC);

CREATE INDEX "idx__nl2sql_reference_packs__idx_nl2sql_ref_packs_scope_enabled" ON "nl2sql_reference_packs" ("tenant_id","scope","enabled","updated_at" DESC);

CREATE INDEX "idx__nl2sql_refresh_tasks__idx_tenant" ON "nl2sql_refresh_tasks" ("tenant_id");

CREATE INDEX "idx__nl2sql_refresh_tasks__idx_datasource" ON "nl2sql_refresh_tasks" ("datasource_id");

CREATE INDEX "idx__nl2sql_refresh_tasks__idx_status" ON "nl2sql_refresh_tasks" ("status");

CREATE INDEX "idx__nl2sql_refresh_tasks__idx_trigger" ON "nl2sql_refresh_tasks" ("datasource_id","trigger_source","created_at");

CREATE INDEX "idx__nl2sql_refresh_tasks__idx_deleted" ON "nl2sql_refresh_tasks" ("deleted_at");

CREATE UNIQUE INDEX "idx__nl2sql_result_cache__uk_cache_key" ON "nl2sql_result_cache" ("tenant_id","datasource_id","question_hash");

CREATE INDEX "idx__nl2sql_result_cache__idx_expires" ON "nl2sql_result_cache" ("expires_at");

CREATE INDEX "idx__nl2sql_result_cache__idx_datasource" ON "nl2sql_result_cache" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_result_cache__idx_cache_query_id" ON "nl2sql_result_cache" ("tenant_id","query_id");

CREATE UNIQUE INDEX "idx__nl2sql_result_validation_rules__uk_ds_table_col_rule" ON "nl2sql_result_validation_rules" ("datasource_id","table_name","column_name","rule_type");

CREATE INDEX "idx__nl2sql_result_validation_rules__idx_tenant" ON "nl2sql_result_validation_rules" ("tenant_id");

CREATE INDEX "idx__nl2sql_result_validation_rules__idx_datasource" ON "nl2sql_result_validation_rules" ("datasource_id");

CREATE INDEX "idx__nl2sql_result_validation_rules__idx_enabled" ON "nl2sql_result_validation_rules" ("datasource_id","enabled");

CREATE INDEX "idx__nl2sql_schema_change_notifications__idx_tenant_ds" ON "nl2sql_schema_change_notifications" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_schema_change_notifications__idx_status" ON "nl2sql_schema_change_notifications" ("status");

CREATE INDEX "idx__nl2sql_schema_change_notifications__idx_created" ON "nl2sql_schema_change_notifications" ("created_at" DESC);

CREATE INDEX "idx__nl2sql_schema_change_notifications__idx_datasource_status" ON "nl2sql_schema_change_notifications" ("datasource_id","status");

CREATE UNIQUE INDEX "idx__nl2sql_synonyms__uk_term" ON "nl2sql_synonyms" ("tenant_id","datasource_id","term");

CREATE INDEX "idx__nl2sql_synonyms__idx_tenant" ON "nl2sql_synonyms" ("tenant_id");

CREATE INDEX "idx__nl2sql_synonyms__idx_datasource" ON "nl2sql_synonyms" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_synonyms__idx_deleted" ON "nl2sql_synonyms" ("deleted_at");

CREATE INDEX "idx__nl2sql_synonyms__fk_syn_ds" ON "nl2sql_synonyms" ("datasource_id");

CREATE INDEX "idx__nl2sql_synonyms__idx_status" ON "nl2sql_synonyms" ("status");

CREATE INDEX "idx__nl2sql_synonyms__idx_datasource_status" ON "nl2sql_synonyms" ("datasource_id","status");

CREATE UNIQUE INDEX "idx__nl2sql_table_desc_semantics__uk_ds_table" ON "nl2sql_table_desc_semantics" ("datasource_id","table_name");

CREATE INDEX "idx__nl2sql_table_desc_semantics__idx_tenant" ON "nl2sql_table_desc_semantics" ("tenant_id");

CREATE INDEX "idx__nl2sql_table_desc_semantics__idx_manual" ON "nl2sql_table_desc_semantics" ("datasource_id","is_manual");

CREATE INDEX "idx__nl2sql_table_desc_semantics__idx_deleted" ON "nl2sql_table_desc_semantics" ("deleted_at");

CREATE INDEX "idx__nl2sql_table_desc_semantics__idx_status" ON "nl2sql_table_desc_semantics" ("status");

CREATE INDEX "idx__nl2sql_table_desc_semantics__idx_datasource_status" ON "nl2sql_table_desc_semantics" ("datasource_id","status");

CREATE UNIQUE INDEX "idx__nl2sql_table_domain_mapping__uk_ds_table_domain" ON "nl2sql_table_domain_mapping" ("datasource_id","table_name","domain_id");

CREATE INDEX "idx__nl2sql_table_domain_mapping__idx_domain" ON "nl2sql_table_domain_mapping" ("domain_id");

CREATE INDEX "idx__nl2sql_table_domain_mapping__idx_table" ON "nl2sql_table_domain_mapping" ("datasource_id","table_name");

CREATE INDEX "idx__nl2sql_table_domain_mapping__idx_nl2sql_tdm_deleted" ON "nl2sql_table_domain_mapping" ("deleted_at");

CREATE UNIQUE INDEX "idx__nl2sql_table_routing_features__uk_ds_table" ON "nl2sql_table_routing_features" ("datasource_id","table_name");

CREATE INDEX "idx__nl2sql_table_routing_features__idx_query_count" ON "nl2sql_table_routing_features" ("query_count" DESC);

CREATE INDEX "idx__nl2sql_table_routing_features__idx_last_query" ON "nl2sql_table_routing_features" ("last_query_at" DESC);

CREATE INDEX "idx__nl2sql_table_routing_features__idx_tenant" ON "nl2sql_table_routing_features" ("tenant_id");

CREATE INDEX "idx__nl2sql_table_routing_features__idx_tenant_ds" ON "nl2sql_table_routing_features" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_table_routing_features__idx_deleted" ON "nl2sql_table_routing_features" ("deleted_at");

CREATE UNIQUE INDEX "idx__nl2sql_table_semantics__uk_ds_table_column" ON "nl2sql_table_semantics" ("datasource_id","table_name","column_name");

CREATE INDEX "idx__nl2sql_table_semantics__idx_tenant" ON "nl2sql_table_semantics" ("tenant_id");

CREATE INDEX "idx__nl2sql_table_semantics__idx_datasource" ON "nl2sql_table_semantics" ("datasource_id");

CREATE INDEX "idx__nl2sql_table_semantics__idx_manual" ON "nl2sql_table_semantics" ("datasource_id","is_manual");

CREATE INDEX "idx__nl2sql_table_semantics__idx_deleted" ON "nl2sql_table_semantics" ("deleted_at");

CREATE INDEX "idx__nl2sql_table_semantics__idx_status" ON "nl2sql_table_semantics" ("status");

CREATE INDEX "idx__nl2sql_table_semantics__idx_datasource_status" ON "nl2sql_table_semantics" ("datasource_id","status");

CREATE INDEX "idx__nl2sql_table_semantics__idx_is_indexed" ON "nl2sql_table_semantics" ("datasource_id","is_indexed");

CREATE UNIQUE INDEX "idx__nl2sql_table_stats__uk_ds_table" ON "nl2sql_table_stats" ("datasource_id","table_name");

CREATE INDEX "idx__nl2sql_table_stats__idx_datasource" ON "nl2sql_table_stats" ("datasource_id");

CREATE INDEX "idx__nl2sql_table_stats__idx_domain_id" ON "nl2sql_table_stats" ("domain_id");

CREATE INDEX "idx__nl2sql_table_stats__idx_tenant" ON "nl2sql_table_stats" ("tenant_id");

CREATE INDEX "idx__nl2sql_table_stats__idx_tenant_ds" ON "nl2sql_table_stats" ("tenant_id","datasource_id");

CREATE INDEX "idx__nl2sql_table_stats__idx_deleted" ON "nl2sql_table_stats" ("deleted_at");

CREATE UNIQUE INDEX "idx__nl2sql_time_patterns__uk_tenant_pattern" ON "nl2sql_time_patterns" ("tenant_id","pattern_regex");

CREATE INDEX "idx__nl2sql_time_patterns__idx_tenant" ON "nl2sql_time_patterns" ("tenant_id");

CREATE INDEX "idx__nl2sql_time_patterns__idx_enabled" ON "nl2sql_time_patterns" ("tenant_id","enabled");

CREATE INDEX "idx__notifications__idx_notifications_user" ON "notifications" ("user_id","read");

CREATE INDEX "idx__notifications__tenant_id" ON "notifications" ("tenant_id");

CREATE INDEX "idx__pm_audit_trails__idx_pm_audit_tenant_created" ON "pm_audit_trails" ("tenant_id","created_at");

CREATE INDEX "idx__pm_audit_trails__idx_pm_audit_run_created" ON "pm_audit_trails" ("run_id","created_at");

CREATE INDEX "idx__pm_audit_trails__idx_pm_audit_event_created" ON "pm_audit_trails" ("event_type","created_at");

CREATE UNIQUE INDEX "idx__pm_budget_profiles__uk_pm_budget_profiles_tenant_profile" ON "pm_budget_profiles" ("tenant_id","profile_key");

CREATE INDEX "idx__pm_budget_profiles__idx_pm_budget_profiles_tenant_enabled" ON "pm_budget_profiles" ("tenant_id","enabled","priority","updated_at");

CREATE UNIQUE INDEX "idx__pm_claim_verdicts__uk_pm_claim_verdict_tenant_run_claim" ON "pm_claim_verdicts" ("tenant_id","run_id","claim_key");

CREATE INDEX "idx__pm_claim_verdicts__idx_pm_claim_verdict_tenant_verdict" ON "pm_claim_verdicts" ("tenant_id","verdict","updated_at");

CREATE INDEX "idx__pm_claim_verdicts__idx_pm_claim_verdict_tenant_domain" ON "pm_claim_verdicts" ("tenant_id","domain","updated_at");

CREATE INDEX "idx__pm_claim_verdicts__idx_pm_claim_verdict_tenant_run_updated" ON "pm_claim_verdicts" ("tenant_id","run_id","updated_at","created_at");

CREATE UNIQUE INDEX "idx__pm_conflict_cases__uk_pm_conflict_case_tenant_run_topic" ON "pm_conflict_cases" ("tenant_id","run_id","topic_key");

CREATE INDEX "idx__pm_conflict_cases__idx_pm_conflict_case_tenant_conf" ON "pm_conflict_cases" ("tenant_id","confidence","updated_at");

CREATE INDEX "idx__pm_conflict_cases__idx_pm_conflict_case_tenant_run_updated" ON "pm_conflict_cases" ("tenant_id","run_id","updated_at","created_at");

CREATE UNIQUE INDEX "idx__pm_domain_circuit_states__uk_pm_domain_circuit_tenant_domain" ON "pm_domain_circuit_states" ("tenant_id","domain_key");

CREATE INDEX "idx__pm_domain_circuit_states__idx_pm_domain_circuit_open_until" ON "pm_domain_circuit_states" ("tenant_id","open_until");

CREATE INDEX "idx__pm_domain_circuit_states__idx_pm_domain_circuit_updated" ON "pm_domain_circuit_states" ("tenant_id","updated_at");

CREATE INDEX "idx__pm_material_assets__idx_pm_material_asset_tenant_job" ON "pm_material_assets" ("tenant_id","job_id","id");

CREATE INDEX "idx__pm_material_jobs__idx_pm_material_job_tenant_status" ON "pm_material_jobs" ("tenant_id","status","created_at");

CREATE INDEX "idx__pm_material_jobs__idx_pm_material_job_tenant_run" ON "pm_material_jobs" ("tenant_id","mission_run_id","created_at");

CREATE INDEX "idx__pm_material_jobs__idx_pm_material_job_tenant_thread" ON "pm_material_jobs" ("tenant_id","thread_id","id");

CREATE INDEX "idx__pm_material_jobs__idx_pm_material_job_tenant_parent" ON "pm_material_jobs" ("tenant_id","parent_job_id","id");

CREATE INDEX "idx__pm_missions__idx_pm_mission_tenant_country_enabled" ON "pm_missions" ("tenant_id","country_code","enabled");

CREATE INDEX "idx__pm_missions__idx_pm_mission_tenant_updated" ON "pm_missions" ("tenant_id","updated_at");

CREATE UNIQUE INDEX "idx__pm_prompt_registry__uk_pm_prompt_registry_tenant_key_ver" ON "pm_prompt_registry" ("tenant_id","prompt_key","prompt_version");

CREATE INDEX "idx__pm_prompt_registry__idx_pm_prompt_registry_tenant_stage" ON "pm_prompt_registry" ("tenant_id","stage","updated_at");

CREATE INDEX "idx__pm_prompt_registry__idx_pm_prompt_registry_contract" ON "pm_prompt_registry" ("tenant_id","contract_version","stage","updated_at");

CREATE INDEX "idx__pm_prompt_registry__idx_pm_prompt_registry_tenant_run_updated" ON "pm_prompt_registry" ("tenant_id","last_run_id","updated_at");

CREATE UNIQUE INDEX "idx__pm_provider_health__uk_pm_provider_health_tenant_provider_channel" ON "pm_provider_health" ("tenant_id","provider_key","channel");

CREATE INDEX "idx__pm_provider_health__idx_pm_provider_health_tenant_status" ON "pm_provider_health" ("tenant_id","last_status","updated_at");

CREATE INDEX "idx__pm_provider_health__idx_pm_provider_health_channel_status" ON "pm_provider_health" ("channel","last_status","updated_at");

CREATE UNIQUE INDEX "idx__pm_quality_gate_metrics__uk_pm_quality_gate_run" ON "pm_quality_gate_metrics" ("run_id");

CREATE INDEX "idx__pm_quality_gate_metrics__idx_pm_quality_gate_tenant_time" ON "pm_quality_gate_metrics" ("tenant_id","created_at");

CREATE INDEX "idx__pm_quality_gate_metrics__idx_pm_quality_gate_passed" ON "pm_quality_gate_metrics" ("tenant_id","passed","created_at");

CREATE UNIQUE INDEX "idx__pm_research_evidence_graph__uk_pm_evidence_graph_tenant_claim_url_rel" ON "pm_research_evidence_graph" ("tenant_id","claim_key","url_hash","relation");

CREATE INDEX "idx__pm_research_evidence_graph__idx_pm_evidence_graph_tenant_claim" ON "pm_research_evidence_graph" ("tenant_id","claim_key","updated_at");

CREATE INDEX "idx__pm_research_evidence_graph__idx_pm_evidence_graph_tenant_domain" ON "pm_research_evidence_graph" ("tenant_id","domain","updated_at");

CREATE INDEX "idx__pm_research_evidence_graph__idx_pm_evidence_graph_tenant_relation" ON "pm_research_evidence_graph" ("tenant_id","relation","updated_at");

CREATE UNIQUE INDEX "idx__pm_research_route_stats__uk_pm_research_route_stats_tenant_route" ON "pm_research_route_stats" ("tenant_id","route_key");

CREATE INDEX "idx__pm_research_route_stats__idx_pm_research_route_stats_tenant_score" ON "pm_research_route_stats" ("tenant_id","score","updated_at");

CREATE INDEX "idx__pm_research_route_stats__idx_pm_research_route_stats_tenant_channel" ON "pm_research_route_stats" ("tenant_id","channel","updated_at");

CREATE UNIQUE INDEX "idx__pm_research_runs__uk_pm_research_runs_run_id" ON "pm_research_runs" ("run_id");

CREATE INDEX "idx__pm_research_runs__idx_pm_research_runs_tenant_status_updated" ON "pm_research_runs" ("tenant_id","status","updated_at");

CREATE INDEX "idx__pm_research_runs__idx_pm_research_runs_tenant_user_updated" ON "pm_research_runs" ("tenant_id","user_id","updated_at");

CREATE INDEX "idx__pm_research_runs__idx_pm_research_runs_tenant_session_updated" ON "pm_research_runs" ("tenant_id","session_id","updated_at");

CREATE INDEX "idx__pm_research_runs__idx_pm_research_runs_tenant_task_id" ON "pm_research_runs" ("tenant_id","task_id");

CREATE UNIQUE INDEX "idx__pm_research_source_slots__uk_pm_source_slot_run_seq" ON "pm_research_source_slots" ("run_id","slot_seq");

CREATE INDEX "idx__pm_research_source_slots__idx_pm_source_slot_run_stage" ON "pm_research_source_slots" ("run_id","stage_attempt_id","created_at");

CREATE INDEX "idx__pm_research_source_slots__idx_pm_source_slot_status" ON "pm_research_source_slots" ("status","updated_at");

CREATE UNIQUE INDEX "idx__pm_research_stage_attempts__uk_pm_stage_attempt_run_stage_attempt" ON "pm_research_stage_attempts" ("run_id","stage","attempt_no");

CREATE INDEX "idx__pm_research_stage_attempts__idx_pm_stage_attempt_run_stage" ON "pm_research_stage_attempts" ("run_id","stage","created_at");

CREATE INDEX "idx__pm_research_stage_attempts__idx_pm_stage_attempt_status" ON "pm_research_stage_attempts" ("status","updated_at");

CREATE UNIQUE INDEX "idx__pm_research_task_events__uk_pm_research_task_events_task_seq" ON "pm_research_task_events" ("task_id","seq");

CREATE INDEX "idx__pm_research_task_events__idx_pm_research_task_events_tenant_task_id" ON "pm_research_task_events" ("tenant_id","task_id","id");

CREATE INDEX "idx__pm_research_task_events__idx_pm_research_task_events_tenant_user_id" ON "pm_research_task_events" ("tenant_id","user_id","id");

CREATE INDEX "idx__pm_research_task_events__idx_pm_task_events_task_seq_id" ON "pm_research_task_events" ("task_id","seq","id");

CREATE INDEX "idx__pm_research_task_events__idx_pm_task_events_task_tenant_user_id" ON "pm_research_task_events" ("task_id","tenant_id","user_id","id");

CREATE INDEX "idx__pm_research_task_events__idx_pm_task_events_tenant_user_task_id" ON "pm_research_task_events" ("tenant_id","user_id","task_id","id");

CREATE INDEX "idx__pm_research_task_events__idx_pm_task_events_tenant_task_seq_id" ON "pm_research_task_events" ("tenant_id","task_id","seq","id");

CREATE INDEX "idx__pm_research_task_events__idx_pm_task_events_created_id" ON "pm_research_task_events" ("created_at","id");

CREATE UNIQUE INDEX "idx__pm_research_task_stream_events__uk_pm_task_stream_events_task_seq" ON "pm_research_task_stream_events" ("task_id","seq");

CREATE INDEX "idx__pm_research_task_stream_events__idx_pm_task_stream_events_tenant_user_task_id" ON "pm_research_task_stream_events" ("tenant_id","user_id","task_id","id");

CREATE INDEX "idx__pm_research_task_stream_events__idx_pm_stream_events_created_id" ON "pm_research_task_stream_events" ("created_at","id");

CREATE INDEX "idx__pm_research_tasks__idx_pm_research_tasks_tenant_user_updated" ON "pm_research_tasks" ("tenant_id","user_id","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_research_tasks_tenant_status_updated" ON "pm_research_tasks" ("tenant_id","status","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_research_tasks_tenant_session_updated" ON "pm_research_tasks" ("tenant_id","session_id","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_tenant_status_heartbeat" ON "pm_research_tasks" ("tenant_id","status","heartbeat_at","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_tenant_lease_expire" ON "pm_research_tasks" ("tenant_id","lease_expires_at","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_runtime_claim" ON "pm_research_tasks" ("status","cancel_requested","completed_at","lease_expires_at","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_runtime_session_status" ON "pm_research_tasks" ("session_id","status","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_tenant_user_session_updated" ON "pm_research_tasks" ("tenant_id","user_id","session_id","updated_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_tenant_user_session_updated_task" ON "pm_research_tasks" ("tenant_id","user_id","session_id","updated_at","task_id");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_tenant_user_session_created_updated" ON "pm_research_tasks" ("tenant_id","user_id","session_id","created_at","updated_at","task_id");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_claim_scan_v2" ON "pm_research_tasks" ("completed_at","cancel_requested","updated_at","task_id","status","lease_expires_at");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_claim_scan_v3" ON "pm_research_tasks" ("completed_at","cancel_requested","status","lease_expires_at","updated_at","task_id");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_active_session_v2" ON "pm_research_tasks" ("tenant_id","user_id","session_id","cancel_requested","status","updated_at","created_at","task_id");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_tenant_claim_scan_v4" ON "pm_research_tasks" ("completed_at","cancel_requested","status","lease_expires_at","tenant_id","updated_at","task_id");

CREATE INDEX "idx__pm_research_tasks__idx_pm_tasks_tenant_active_lease_v2" ON "pm_research_tasks" ("tenant_id","completed_at","cancel_requested","status","lease_expires_at");

CREATE UNIQUE INDEX "idx__pm_research_tool_call_ledger__uk_pm_tool_ledger_run_seq" ON "pm_research_tool_call_ledger" ("run_id","call_seq");

CREATE INDEX "idx__pm_research_tool_call_ledger__idx_pm_tool_ledger_run" ON "pm_research_tool_call_ledger" ("run_id","created_at");

CREATE INDEX "idx__pm_research_tool_call_ledger__idx_pm_tool_ledger_tool" ON "pm_research_tool_call_ledger" ("tool_name","created_at");

CREATE INDEX "idx__pm_research_tool_call_ledger__idx_pm_tool_ledger_error" ON "pm_research_tool_call_ledger" ("is_error","created_at");

CREATE INDEX "idx__pm_research_tool_call_ledger__idx_pm_tool_ledger_provider" ON "pm_research_tool_call_ledger" ("provider","created_at");

CREATE INDEX "idx__pm_research_tool_call_ledger__idx_pm_tool_ledger_created_id" ON "pm_research_tool_call_ledger" ("created_at","id");

CREATE UNIQUE INDEX "idx__pm_retry_governance_states__uk_pm_retry_governance_tenant_run" ON "pm_retry_governance_states" ("tenant_id","run_id");

CREATE INDEX "idx__pm_retry_governance_states__idx_pm_retry_governance_next_allowed" ON "pm_retry_governance_states" ("tenant_id","next_allowed_at");

CREATE INDEX "idx__pm_retry_governance_states__idx_pm_retry_governance_session" ON "pm_retry_governance_states" ("tenant_id","session_id","updated_at");

CREATE UNIQUE INDEX "idx__pm_route_bandit_state__uk_pm_bandit_tenant_route_channel" ON "pm_route_bandit_state" ("tenant_id","route_key","channel");

CREATE INDEX "idx__pm_route_bandit_state__idx_pm_bandit_tenant_score" ON "pm_route_bandit_state" ("tenant_id","score","updated_at");

CREATE UNIQUE INDEX "idx__pm_route_circuit_states__uk_pm_route_circuit_tenant_route" ON "pm_route_circuit_states" ("tenant_id","route_key");

CREATE INDEX "idx__pm_route_circuit_states__idx_pm_route_circuit_open_until" ON "pm_route_circuit_states" ("tenant_id","open_until");

CREATE INDEX "idx__pm_route_circuit_states__idx_pm_route_circuit_channel" ON "pm_route_circuit_states" ("tenant_id","channel","updated_at");

CREATE UNIQUE INDEX "idx__pm_route_learning_features__uk_pm_route_learning_tenant_route_channel" ON "pm_route_learning_features" ("tenant_id","route_key","channel");

CREATE INDEX "idx__pm_route_learning_features__idx_pm_route_learning_tenant_score" ON "pm_route_learning_features" ("tenant_id","ema_success_rate","ema_quality","updated_at");

CREATE UNIQUE INDEX "idx__pm_search_provider_configs__uk_pm_search_provider_tenant_name" ON "pm_search_provider_configs" ("tenant_id","name");

CREATE INDEX "idx__pm_search_provider_configs__idx_pm_search_provider_tenant_enabled_priority" ON "pm_search_provider_configs" ("tenant_id","enabled","priority");

CREATE INDEX "idx__pm_search_provider_configs__idx_pm_search_provider_tenant_type" ON "pm_search_provider_configs" ("tenant_id","provider_type","enabled");

CREATE INDEX "idx__pm_session_memories__idx_pm_session_memories_session_enabled" ON "pm_session_memories" ("tenant_id","user_id","session_id","enabled","pinned","updated_at");

CREATE INDEX "idx__pm_session_memories__idx_pm_session_memories_type" ON "pm_session_memories" ("tenant_id","user_id","session_id","memory_type","enabled");

CREATE INDEX "idx__pm_session_summaries__idx_pm_session_summaries_updated" ON "pm_session_summaries" ("tenant_id","user_id","updated_at");

CREATE UNIQUE INDEX "idx__pm_subtask_attempts__uk_pm_subtask_attempts_key" ON "pm_subtask_attempts" ("subtask_run_id","attempt_key");

CREATE INDEX "idx__pm_subtask_attempts__idx_pm_subtask_attempts_run_subtask" ON "pm_subtask_attempts" ("run_id","subtask_key","attempt_no");

CREATE INDEX "idx__pm_subtask_attempts__idx_pm_subtask_attempts_route_status" ON "pm_subtask_attempts" ("route_key","status","updated_at");

CREATE INDEX "idx__pm_subtask_attempts__idx_pm_subtask_attempts_status" ON "pm_subtask_attempts" ("status","updated_at");

CREATE INDEX "idx__pm_subtask_attempts__idx_pm_subtask_attempts_run_subtask_attempt_id" ON "pm_subtask_attempts" ("run_id","subtask_run_id","attempt_no","id");

CREATE UNIQUE INDEX "idx__pm_subtask_runs__uk_pm_subtask_runs_run_key" ON "pm_subtask_runs" ("run_id","subtask_key");

CREATE INDEX "idx__pm_subtask_runs__idx_pm_subtask_runs_run_status" ON "pm_subtask_runs" ("run_id","status","updated_at");

CREATE INDEX "idx__pm_subtask_runs__idx_pm_subtask_runs_tenant_status" ON "pm_subtask_runs" ("tenant_id","status","updated_at");

CREATE INDEX "idx__pm_subtask_runs__idx_pm_subtask_runs_tenant_task" ON "pm_subtask_runs" ("tenant_id","task_id","updated_at");

CREATE INDEX "idx__pm_subtask_runs__idx_pm_subtask_runs_run_id" ON "pm_subtask_runs" ("run_id","id");

CREATE UNIQUE INDEX "idx__rd_agent_market_index__uk_rd_agent_market_index" ON "rd_agent_market_index" ("tenant_id","repo_full_name","branch","template_path","item_type");

CREATE INDEX "idx__rd_agent_market_index__idx_rd_agent_market_index_name" ON "rd_agent_market_index" ("tenant_id","name");

CREATE INDEX "idx__rd_agent_market_index__idx_rd_agent_market_index_repo" ON "rd_agent_market_index" ("tenant_id","repo_full_name","branch");

CREATE INDEX "idx__rd_agent_market_index__idx_rd_agent_market_index_type" ON "rd_agent_market_index" ("tenant_id","item_type","updated_at");

CREATE INDEX "idx__rd_agent_market_index__fk_rd_agent_market_index_repo" ON "rd_agent_market_index" ("repository_id");

CREATE UNIQUE INDEX "idx__rd_agent_market_repositories__uk_rd_agent_market_repo" ON "rd_agent_market_repositories" ("tenant_id","repo_full_name","branch");

CREATE INDEX "idx__rd_agent_market_repositories__idx_rd_agent_market_repo_tenant_enabled" ON "rd_agent_market_repositories" ("tenant_id","enabled");

CREATE INDEX "idx__rd_agent_market_repositories__idx_rd_agent_market_repo_updated" ON "rd_agent_market_repositories" ("tenant_id","updated_at");

CREATE UNIQUE INDEX "idx__rd_agent_profiles__uk_rd_agent_profiles_name" ON "rd_agent_profiles" ("tenant_id","name");

CREATE UNIQUE INDEX "idx__rd_agent_workflows__uk_rd_agent_workflows_name" ON "rd_agent_workflows" ("tenant_id","name");

CREATE INDEX "idx__rd_agent_workflows__idx_rd_agent_workflows_source" ON "rd_agent_workflows" ("tenant_id","source","source_item_id");

CREATE INDEX "idx__rd_agent_workflows__idx_rd_agent_workflows_enabled" ON "rd_agent_workflows" ("tenant_id","enabled","updated_at");

CREATE UNIQUE INDEX "idx__rd_code_intel_sessions__uk_rd_code_intel_repo_lang" ON "rd_code_intel_sessions" ("tenant_id","user_id","repository_id","language");

CREATE INDEX "idx__rd_code_intel_sessions__idx_rd_code_intel_repo" ON "rd_code_intel_sessions" ("tenant_id","repository_id","updated_at" DESC);

CREATE INDEX "idx__rd_code_intel_sessions__fk_rd_code_intel_repo" ON "rd_code_intel_sessions" ("repository_id");

CREATE INDEX "idx__rd_file_changes__idx_rd_file_changes_task" ON "rd_file_changes" ("task_id");

CREATE INDEX "idx__rd_file_changes__idx_rd_file_changes_repo" ON "rd_file_changes" ("repository_id","applied");

CREATE INDEX "idx__rd_integrations__idx_rd_integrations_tenant" ON "rd_integrations" ("tenant_id","provider","enabled");

CREATE UNIQUE INDEX "idx__rd_patch_ownerships__uniq_rd_patch_ownership_change_file" ON "rd_patch_ownerships" ("tenant_id","change_id","file_path");

CREATE INDEX "idx__rd_patch_ownerships__idx_rd_patch_ownership_task" ON "rd_patch_ownerships" ("tenant_id","task_id");

CREATE INDEX "idx__rd_patch_ownerships__idx_rd_patch_ownership_repo_file" ON "rd_patch_ownerships" ("tenant_id","repository_id","file_path","applied");

CREATE INDEX "idx__rd_patch_ownerships__fk_rd_patch_ownership_task" ON "rd_patch_ownerships" ("task_id");

CREATE INDEX "idx__rd_patch_ownerships__fk_rd_patch_ownership_change" ON "rd_patch_ownerships" ("change_id");

CREATE INDEX "idx__rd_patch_ownerships__fk_rd_patch_ownership_repo" ON "rd_patch_ownerships" ("repository_id");

CREATE INDEX "idx__rd_preview_events__idx_rd_preview_events_session" ON "rd_preview_events" ("tenant_id","session_id","created_at" DESC);

CREATE INDEX "idx__rd_preview_events__fk_rd_preview_events_session" ON "rd_preview_events" ("session_id");

CREATE INDEX "idx__rd_preview_sessions__idx_rd_preview_repo" ON "rd_preview_sessions" ("tenant_id","repository_id","updated_at" DESC);

CREATE INDEX "idx__rd_preview_sessions__idx_rd_preview_task" ON "rd_preview_sessions" ("tenant_id","task_id","updated_at" DESC);

CREATE INDEX "idx__rd_preview_sessions__idx_rd_preview_status" ON "rd_preview_sessions" ("tenant_id","status","updated_at" DESC);

CREATE INDEX "idx__rd_preview_sessions__fk_rd_preview_repo" ON "rd_preview_sessions" ("repository_id");

CREATE INDEX "idx__rd_quality_metrics__idx_rd_quality_tenant_metric" ON "rd_quality_metrics" ("tenant_id","metric_name","created_at");

CREATE INDEX "idx__rd_quality_metrics__idx_rd_quality_task" ON "rd_quality_metrics" ("task_id");

CREATE INDEX "idx__rd_quality_metrics__fk_rd_quality_repo" ON "rd_quality_metrics" ("repository_id");

CREATE UNIQUE INDEX "idx__rd_repository_context_summaries__uk_rd_repo_context_scope" ON "rd_repository_context_summaries" ("tenant_id","repository_id","scope_type","scope_key_hash");

CREATE INDEX "idx__rd_repository_context_summaries__idx_rd_repo_context_repo" ON "rd_repository_context_summaries" ("tenant_id","repository_id","scope_type");

CREATE INDEX "idx__rd_repository_context_summaries__fk_rd_repo_context_repo" ON "rd_repository_context_summaries" ("repository_id");

CREATE UNIQUE INDEX "idx__rd_repository_file_summaries__uk_rd_repo_file_summaries_path" ON "rd_repository_file_summaries" ("tenant_id","repository_id","file_path_hash");

CREATE INDEX "idx__rd_repository_file_summaries__idx_rd_repo_file_summaries_repo" ON "rd_repository_file_summaries" ("tenant_id","repository_id");

CREATE INDEX "idx__rd_repository_file_summaries__idx_rd_repo_file_summaries_path" ON "rd_repository_file_summaries" ("repository_id","file_path");

CREATE INDEX "idx__rd_repository_imports__idx_rd_repo_imports_lookup" ON "rd_repository_imports" ("tenant_id","repository_id","import_path");

CREATE INDEX "idx__rd_repository_imports__idx_rd_repo_imports_file" ON "rd_repository_imports" ("repository_id","file_path");

CREATE UNIQUE INDEX "idx__rd_repository_indexes__uk_rd_repository_indexes_repo" ON "rd_repository_indexes" ("repository_id");

CREATE INDEX "idx__rd_repository_indexes__idx_rd_repository_indexes_tenant" ON "rd_repository_indexes" ("tenant_id","status");

CREATE INDEX "idx__rd_repository_settings__idx_rd_repo_settings_tenant_user" ON "rd_repository_settings" ("tenant_id","user_id");

CREATE INDEX "idx__rd_repository_symbols__idx_rd_repo_symbols_lookup" ON "rd_repository_symbols" ("tenant_id","repository_id","symbol_name");

CREATE INDEX "idx__rd_repository_symbols__idx_rd_repo_symbols_file" ON "rd_repository_symbols" ("repository_id","file_path");

CREATE INDEX "idx__rd_spec_events__idx_rd_spec_events_spec_time" ON "rd_spec_events" ("tenant_id","spec_id","created_at" DESC);

CREATE INDEX "idx__rd_spec_events__fk_rd_spec_events_spec" ON "rd_spec_events" ("spec_id");

CREATE UNIQUE INDEX "idx__rd_spec_task_links__idx_rd_spec_task_links_item" ON "rd_spec_task_links" ("tenant_id","spec_id","task_item_id");

CREATE INDEX "idx__rd_spec_task_links__idx_rd_spec_task_links_rd_task" ON "rd_spec_task_links" ("tenant_id","rd_task_id");

CREATE INDEX "idx__rd_spec_task_links__fk_rd_spec_task_links_spec" ON "rd_spec_task_links" ("spec_id");

CREATE INDEX "idx__rd_spec_task_links__fk_rd_spec_task_links_rd_task" ON "rd_spec_task_links" ("rd_task_id");

CREATE INDEX "idx__rd_specs__idx_rd_specs_tenant_user" ON "rd_specs" ("tenant_id","user_id","created_at");

CREATE INDEX "idx__rd_specs__idx_rd_specs_repo" ON "rd_specs" ("repository_id");

CREATE INDEX "idx__rd_steering_rule_repositories__idx_rd_steering_rule_repos_tenant_repo" ON "rd_steering_rule_repositories" ("tenant_id","repository_id");

CREATE INDEX "idx__rd_steering_rule_repositories__idx_rd_steering_rule_repos_repo" ON "rd_steering_rule_repositories" ("repository_id");

CREATE INDEX "idx__rd_steering_rules__idx_rd_steering_rules_repo" ON "rd_steering_rules" ("repository_id","enabled");

CREATE INDEX "idx__rd_task_events__idx_rd_task_events_task" ON "rd_task_events" ("task_id","id");

CREATE INDEX "idx__rd_task_events__idx_rd_task_events_tenant" ON "rd_task_events" ("tenant_id","created_at");

CREATE INDEX "idx__rd_task_events__idx_rd_task_events_tenant_task_id" ON "rd_task_events" ("tenant_id","task_id","id");

CREATE INDEX "idx__rd_task_events__idx_rd_task_events_tenant_task_stage_status_id" ON "rd_task_events" ("tenant_id","task_id","stage","status","id");

CREATE UNIQUE INDEX "idx__rd_task_git_baselines__uniq_rd_task_git_baselines_task_repo" ON "rd_task_git_baselines" ("tenant_id","task_id","repository_id");

CREATE INDEX "idx__rd_task_git_baselines__idx_rd_task_git_baselines_repo" ON "rd_task_git_baselines" ("tenant_id","repository_id","created_at");

CREATE INDEX "idx__rd_task_git_baselines__fk_rd_task_git_baselines_task" ON "rd_task_git_baselines" ("task_id");

CREATE INDEX "idx__rd_task_git_baselines__fk_rd_task_git_baselines_repo" ON "rd_task_git_baselines" ("repository_id");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_tenant_user" ON "rd_tasks" ("tenant_id","user_id","created_at");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_repo" ON "rd_tasks" ("repository_id","status");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_spec" ON "rd_tasks" ("spec_id");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_agent_profile" ON "rd_tasks" ("agent_profile_id");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_thread" ON "rd_tasks" ("tenant_id","user_id","thread_id","iteration_no");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_parent" ON "rd_tasks" ("tenant_id","user_id","parent_task_id");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_runtime_session" ON "rd_tasks" ("tenant_id","user_id","runtime_session_id");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_workflow" ON "rd_tasks" ("workflow_id");

CREATE INDEX "idx__rd_tasks__idx_rd_tasks_context_profile" ON "rd_tasks" ("tenant_id","user_id","context_profile","created_at");

CREATE INDEX "idx__rd_test_runs__idx_rd_test_runs_task" ON "rd_test_runs" ("task_id","created_at");

CREATE INDEX "idx__rd_test_runs__idx_rd_test_runs_repo" ON "rd_test_runs" ("repository_id","status");

CREATE UNIQUE INDEX "idx__skills_market_index__uk_skill_market_index" ON "skills_market_index" ("tenant_id","repo_full_name","branch","skill_path");

CREATE INDEX "idx__skills_market_index__idx_skill_market_index_tenant_name" ON "skills_market_index" ("tenant_id","skill_name");

CREATE INDEX "idx__skills_market_index__idx_skill_market_index_repo" ON "skills_market_index" ("tenant_id","repo_full_name","branch");

CREATE UNIQUE INDEX "idx__skills_market_repositories__uk_skill_market_repo" ON "skills_market_repositories" ("tenant_id","repo_full_name","branch");

CREATE INDEX "idx__skills_market_repositories__idx_skill_market_repo_tenant_enabled" ON "skills_market_repositories" ("tenant_id","enabled");

CREATE INDEX "idx__skills_market_repositories__idx_skill_market_repo_tenant_updated" ON "skills_market_repositories" ("tenant_id","updated_at");

CREATE UNIQUE INDEX "idx__skills_registry__uk_tenant_name" ON "skills_registry" ("tenant_id","name");

CREATE INDEX "idx__skills_registry__idx_tenant_enabled" ON "skills_registry" ("tenant_id","enabled");

CREATE INDEX "idx__skills_registry__idx_source" ON "skills_registry" ("tenant_id","source");

CREATE INDEX "idx__sql_knowledge_usage_events__idx_sql_knowledge_usage_tenant_created" ON "sql_knowledge_usage_events" ("tenant_id","created_at" DESC);

CREATE INDEX "idx__sql_knowledge_usage_events__idx_sql_knowledge_usage_query" ON "sql_knowledge_usage_events" ("tenant_id","query_id");

CREATE INDEX "idx__sql_knowledge_usage_events__idx_sql_knowledge_usage_ds" ON "sql_knowledge_usage_events" ("tenant_id","datasource_id","created_at" DESC);

CREATE INDEX "idx__sql_knowledge_usage_events__idx_sql_knowledge_usage_chunk" ON "sql_knowledge_usage_events" ("tenant_id","chunk_id","created_at" DESC);

CREATE UNIQUE INDEX "idx__super_assistant_subtasks__uk_super_assistant_subtask_call" ON "super_assistant_subtasks" ("tenant_id","user_id","parent_turn_id","tool_call_id");

CREATE INDEX "idx__super_assistant_subtasks__idx_super_assistant_subtask_parent" ON "super_assistant_subtasks" ("tenant_id","user_id","parent_turn_id","status","updated_at");

CREATE INDEX "idx__super_assistant_subtasks__idx_super_assistant_subtask_claim" ON "super_assistant_subtasks" ("status","cancel_requested","lease_expires_at","updated_at");

CREATE UNIQUE INDEX "idx__super_assistant_turn_events__uk_super_assistant_turn_event" ON "super_assistant_turn_events" ("tenant_id","user_id","session_id","turn_id","seq");

CREATE UNIQUE INDEX "idx__super_assistant_turns__uk_super_assistant_turn" ON "super_assistant_turns" ("tenant_id","user_id","session_id","turn_id");

CREATE INDEX "idx__super_assistant_turns__idx_super_assistant_active" ON "super_assistant_turns" ("tenant_id","user_id","session_id","status","updated_at");

CREATE INDEX "idx__super_assistant_turns__idx_super_assistant_turn_claim" ON "super_assistant_turns" ("status","cancel_requested","lease_expires_at","updated_at");

CREATE INDEX "idx__tenant_agent_features__idx_tenant_agent_features_mode" ON "tenant_agent_features" ("feature_key","mode");

CREATE INDEX "idx__tenant_hooks__idx_tenant_event" ON "tenant_hooks" ("tenant_id","event_type");

CREATE INDEX "idx__tenant_hooks__idx_tenant_enabled" ON "tenant_hooks" ("tenant_id","enabled");

CREATE INDEX "idx__tenant_hooks__idx_tenant_priority" ON "tenant_hooks" ("tenant_id","event_type","priority","created_at");

CREATE UNIQUE INDEX "idx__tenants__slug" ON "tenants" ("slug");

CREATE INDEX "idx__tenants__idx_tenants_slug" ON "tenants" ("slug");

CREATE INDEX "idx__token_usage__idx_token_usage_tenant_created" ON "token_usage" ("tenant_id","created_at");

CREATE INDEX "idx__token_usage__idx_token_usage_session" ON "token_usage" ("session_id");

CREATE INDEX "idx__token_usage__idx_token_usage_model" ON "token_usage" ("model");

CREATE INDEX "idx__token_usage__api_key_id" ON "token_usage" ("api_key_id");

CREATE INDEX "idx__usage_alerts__idx_usage_alerts_tenant" ON "usage_alerts" ("tenant_id");

CREATE INDEX "idx__usage_alerts__idx_usage_alerts_tenant_enabled" ON "usage_alerts" ("tenant_id","enabled");

CREATE UNIQUE INDEX "idx__user_quotas__uk_user_quotas" ON "user_quotas" ("tenant_id","user_id");

CREATE INDEX "idx__user_quotas__user_id" ON "user_quotas" ("user_id");

CREATE UNIQUE INDEX "idx__users__email" ON "users" ("email");

CREATE UNIQUE INDEX "idx__users__uk_users_invite_token" ON "users" ("invite_token");

CREATE INDEX "idx__users__idx_users_email" ON "users" ("email");

CREATE INDEX "idx__users__idx_users_tenant" ON "users" ("tenant_id");

CREATE VIEW "nl2sql_feedback_stats" AS
SELECT
  CAST(json_each.value AS TEXT) AS ds_id,
  COUNT(*) AS total_feedback,
  SUM(CASE WHEN feedback_type = 'thumbs_up' THEN 1 ELSE 0 END) AS thumbs_up_count,
  SUM(CASE WHEN feedback_type = 'thumbs_down' THEN 1 ELSE 0 END) AS thumbs_down_count,
  SUM(CASE WHEN feedback_type = 'correction' THEN 1 ELSE 0 END) AS correction_count,
  SUM(CASE WHEN correction_accepted = 1 THEN 1 ELSE 0 END) AS corrections_accepted,
  AVG(generation_confidence) AS avg_confidence,
  CASE
    WHEN SUM(CASE WHEN feedback_type IN ('thumbs_up', 'thumbs_down') THEN 1 ELSE 0 END) = 0
      THEN NULL
    ELSE 100.0 * SUM(CASE WHEN feedback_type = 'thumbs_up' THEN 1 ELSE 0 END)
      / SUM(CASE WHEN feedback_type IN ('thumbs_up', 'thumbs_down') THEN 1 ELSE 0 END)
  END AS satisfaction_rate
FROM nl2sql_query_feedback, json_each(nl2sql_query_feedback.datasource_ids)
WHERE json_valid(nl2sql_query_feedback.datasource_ids)
GROUP BY CAST(json_each.value AS TEXT);
