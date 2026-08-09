-- Cost provenance and PM child-state reconciliation for pre-release SQLite installs.
ALTER TABLE token_usage ADD COLUMN usage_kind TEXT NOT NULL DEFAULT 'request_delta';
ALTER TABLE token_usage ADD COLUMN pricing_source TEXT NOT NULL DEFAULT 'unknown';

-- Rows created before this migration may contain cumulative session snapshots
-- and prices inferred from an unrelated fallback tier. Preserve them for audit
-- but exclude them from current governance aggregates.
UPDATE token_usage
SET usage_kind = 'legacy_unverified',
    pricing_source = 'legacy_unverified';

UPDATE pm_subtask_runs
SET status = CASE (
        SELECT r.status FROM pm_research_runs r WHERE r.run_id = pm_subtask_runs.run_id LIMIT 1
    )
        WHEN 'cancelled' THEN 'cancelled'
        WHEN 'failed' THEN 'failed'
        ELSE 'skipped'
    END,
    error_code = CASE (
        SELECT r.status FROM pm_research_runs r WHERE r.run_id = pm_subtask_runs.run_id LIMIT 1
    )
        WHEN 'cancelled' THEN 'parent_cancelled'
        WHEN 'failed' THEN 'parent_failed'
        ELSE 'parent_completed_without_execution'
    END,
    error_message = 'Reconciled because the parent PM run is already terminal',
    ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
    updated_at = CURRENT_TIMESTAMP
WHERE status IN ('queued', 'running')
  AND EXISTS (
      SELECT 1 FROM pm_research_runs r
      WHERE r.run_id = pm_subtask_runs.run_id
        AND r.status IN ('completed', 'failed', 'cancelled')
  );

UPDATE pm_subtask_attempts
SET status = CASE (
        SELECT r.status FROM pm_research_runs r WHERE r.run_id = pm_subtask_attempts.run_id LIMIT 1
    )
        WHEN 'cancelled' THEN 'cancelled'
        WHEN 'failed' THEN 'failed'
        ELSE 'skipped'
    END,
    error_code = CASE (
        SELECT r.status FROM pm_research_runs r WHERE r.run_id = pm_subtask_attempts.run_id LIMIT 1
    )
        WHEN 'cancelled' THEN 'parent_cancelled'
        WHEN 'failed' THEN 'parent_failed'
        ELSE 'parent_completed_without_execution'
    END,
    error_message = 'Reconciled because the parent PM run is already terminal',
    ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
    updated_at = CURRENT_TIMESTAMP
WHERE status IN ('queued', 'running')
  AND EXISTS (
      SELECT 1 FROM pm_research_runs r
      WHERE r.run_id = pm_subtask_attempts.run_id
        AND r.status IN ('completed', 'failed', 'cancelled')
  );

CREATE INDEX IF NOT EXISTS idx_token_usage_governance
ON token_usage (tenant_id, usage_kind, created_at);
