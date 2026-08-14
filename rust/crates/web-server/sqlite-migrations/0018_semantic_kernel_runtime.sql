-- Production semantic-kernel runtime projections.  0017 is intentionally
-- preserved as the additive shadow schema; this migration makes the event
-- ledger and PM delivery projection durable and replayable.
ALTER TABLE agent_event_ledger ADD COLUMN writer_fencing INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS agent_writer_leases (
    tenant_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    writer_id TEXT NOT NULL,
    fencing INTEGER NOT NULL,
    lease_expires_at TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (tenant_id, thread_id)
);

CREATE TABLE IF NOT EXISTS pm_research_task_stage_state (
    task_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    stage TEXT NOT NULL,
    status TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 1,
    detail_json TEXT,
    last_event_seq INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (task_id, stage)
);

CREATE TABLE IF NOT EXISTS pm_final_delivery_artifacts (
    task_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    schema_version TEXT NOT NULL DEFAULT 'pm-final-delivery-v1',
    task_status TEXT NOT NULL,
    quality_status TEXT NOT NULL,
    delivery_status TEXT NOT NULL DEFAULT 'persisted',
    response_json TEXT,
    stages_json TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_pm_stage_state_session
    ON pm_research_task_stage_state(tenant_id, user_id, session_id, updated_at);
CREATE INDEX IF NOT EXISTS idx_pm_delivery_session
    ON pm_final_delivery_artifacts(tenant_id, user_id, session_id, updated_at);

CREATE TABLE IF NOT EXISTS prompt_manifests (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    turn_id TEXT,
    run_id TEXT NOT NULL,
    prompt_id TEXT NOT NULL,
    version TEXT NOT NULL,
    variant TEXT NOT NULL,
    model TEXT NOT NULL,
    stable_prefix_hash TEXT NOT NULL,
    task_packet_hash TEXT NOT NULL,
    tool_schema_hash TEXT NOT NULL,
    context_manifest_id TEXT NOT NULL,
    input_budget INTEGER NOT NULL,
    output_budget INTEGER NOT NULL,
    trust_policy_version TEXT NOT NULL,
    eval_suite TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_prompt_manifests_lookup
    ON prompt_manifests(tenant_id, prompt_id, version);
CREATE INDEX IF NOT EXISTS idx_prompt_manifests_run
    ON prompt_manifests(tenant_id, run_id, created_at);
