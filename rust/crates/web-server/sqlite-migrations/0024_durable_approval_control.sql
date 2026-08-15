-- Bind web approvals to the exact authenticated runtime invocation. Legacy
-- rows remain readable but cannot be resolved through the durable v2 path.
ALTER TABLE approval_requests ADD COLUMN user_id TEXT;
ALTER TABLE approval_requests ADD COLUMN session_id TEXT;
ALTER TABLE approval_requests ADD COLUMN turn_id TEXT;
ALTER TABLE approval_requests ADD COLUMN invocation_id TEXT;
ALTER TABLE approval_requests ADD COLUMN input_hash TEXT;
ALTER TABLE approval_requests ADD COLUMN current_mode TEXT;
ALTER TABLE approval_requests ADD COLUMN required_mode TEXT;
ALTER TABLE approval_requests ADD COLUMN reason TEXT;
ALTER TABLE approval_requests ADD COLUMN executor_scope TEXT NOT NULL DEFAULT 'native';
ALTER TABLE approval_requests ADD COLUMN resolved_at TEXT;
ALTER TABLE approval_requests ADD COLUMN resolution_reason TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_approval_invocation_scope
    ON approval_requests(tenant_id, user_id, session_id, turn_id, invocation_id);
CREATE INDEX IF NOT EXISTS idx_approval_pending_owner
    ON approval_requests(tenant_id, user_id, session_id, status, expires_at);
