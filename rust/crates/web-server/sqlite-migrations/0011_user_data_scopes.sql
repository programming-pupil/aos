-- Per-user ownership and broadcast notification state.

CREATE TABLE IF NOT EXISTS notification_receipts (
    notification_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    `read` INTEGER NOT NULL DEFAULT 0,
    read_at TEXT DEFAULT NULL,
    deleted_at TEXT DEFAULT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (notification_id, user_id),
    FOREIGN KEY (notification_id) REFERENCES notifications(id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_notification_receipts_user_unread
ON notification_receipts (tenant_id, user_id, `read`, deleted_at);

-- Preserve the legacy tenant-wide read bit once, then retire it as mutable
-- state. All subsequent employee state lives in notification_receipts.
INSERT OR IGNORE INTO notification_receipts
    (notification_id, tenant_id, user_id, `read`, read_at, updated_at)
SELECT n.id, n.tenant_id, u.id, 1,
       COALESCE(n.created_at, CURRENT_TIMESTAMP), CURRENT_TIMESTAMP
FROM notifications n
INNER JOIN users u ON u.tenant_id = n.tenant_id
WHERE n.user_id IS NULL AND n.`read` = 1;

UPDATE notifications SET `read` = 0
WHERE user_id IS NULL AND `read` <> 0;

CREATE INDEX IF NOT EXISTS idx_pm_material_jobs_owner_updated
ON pm_material_jobs (tenant_id, created_by, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_pm_missions_owner_updated
ON pm_missions (tenant_id, created_by, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_pm_audit_trails_user_created
ON pm_audit_trails (tenant_id, user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_pm_quality_gate_metrics_run_created
ON pm_quality_gate_metrics (tenant_id, run_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_nl2sql_queries_user_analytics
ON nl2sql_queries (tenant_id, user_id, deleted_at, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_nl2sql_clarifications_user_created
ON nl2sql_clarification_messages (tenant_id, user_id, deleted_at, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_token_usage_user_created
ON token_usage (tenant_id, user_id, created_at DESC);
