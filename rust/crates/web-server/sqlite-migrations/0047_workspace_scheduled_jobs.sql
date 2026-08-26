-- Durable, tenant-isolated workspace automation. The database row is the
-- authority; workers use fenced leases so restarts and multiple server
-- instances cannot execute the same occurrence concurrently.
CREATE TABLE IF NOT EXISTS workspace_scheduled_jobs (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT,
  name TEXT NOT NULL,
  script_path TEXT,
  command TEXT NOT NULL,
  cwd TEXT NOT NULL,
  cron_expression TEXT NOT NULL,
  timezone TEXT NOT NULL,
  timeout_seconds INTEGER NOT NULL DEFAULT 120,
  enabled INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL DEFAULT 'scheduled',
  next_run_at INTEGER,
  last_started_at INTEGER,
  last_finished_at INTEGER,
  last_exit_code INTEGER,
  last_stdout TEXT,
  last_stderr TEXT,
  run_count INTEGER NOT NULL DEFAULT 0,
  lease_owner TEXT,
  lease_fencing INTEGER NOT NULL DEFAULT 0,
  lease_expires_at INTEGER,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK(enabled IN (0, 1)),
  CHECK(timeout_seconds BETWEEN 1 AND 600),
  CHECK(status IN ('scheduled', 'running', 'succeeded', 'failed', 'timed_out', 'cancelled'))
);

CREATE INDEX IF NOT EXISTS idx_workspace_scheduled_jobs_due
  ON workspace_scheduled_jobs(enabled, next_run_at, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_workspace_scheduled_jobs_owner
  ON workspace_scheduled_jobs(tenant_id, user_id, updated_at);
