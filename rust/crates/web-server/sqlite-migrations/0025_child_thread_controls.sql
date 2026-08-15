-- Durable child control requests.  A request is committed before any live
-- executor signal is sent; a worker can therefore recover pending controls
-- after a process restart without guessing from UI state.
CREATE TABLE IF NOT EXISTS child_thread_controls (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    parent_thread_id TEXT NOT NULL,
    child_thread_id TEXT NOT NULL,
    action TEXT NOT NULL,
    detail TEXT,
    detail_hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    result_json TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    applied_at TEXT,
    UNIQUE (tenant_id, child_thread_id, action, detail_hash)
);

CREATE INDEX IF NOT EXISTS idx_child_thread_controls_pending
    ON child_thread_controls(tenant_id, child_thread_id, status, created_at);
