-- Canonical, durable authority for product-specific one-shot model calls.
-- Conversational runtimes use agent_event_ledger surface folds; classifiers,
-- schema description, PM/RD helpers, and other bounded inference calls store
-- the complete typed request here before any provider I/O.
CREATE TABLE IF NOT EXISTS model_dispatch_lock (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    revision INTEGER NOT NULL DEFAULT 0
);

INSERT OR IGNORE INTO model_dispatch_lock (id, revision) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS model_dispatch_surfaces (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    authority TEXT NOT NULL,
    request_group_id TEXT NOT NULL,
    attempt_index INTEGER NOT NULL CHECK (attempt_index > 0),
    provider_kind TEXT NOT NULL,
    model TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    messages_hash TEXT NOT NULL,
    system_hash TEXT NOT NULL,
    tool_schema_hash TEXT NOT NULL,
    request_projection_json TEXT NOT NULL,
    request_ciphertext TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN ('dispatched', 'succeeded', 'failed')
    ),
    response_hash TEXT,
    error_projection TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TEXT,
    UNIQUE (tenant_id, request_group_id, attempt_index)
);

CREATE INDEX IF NOT EXISTS idx_model_dispatch_scope
    ON model_dispatch_surfaces (tenant_id, owner_user_id, authority, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_model_dispatch_recovery
    ON model_dispatch_surfaces (status, created_at);
