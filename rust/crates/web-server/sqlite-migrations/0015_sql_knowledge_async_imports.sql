CREATE TABLE IF NOT EXISTS nl2sql_reference_import_tasks (
  id TEXT NOT NULL PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  pack_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'running', 'completed', 'partial', 'failed')),
  total_files INTEGER NOT NULL DEFAULT 0,
  processed_files INTEGER NOT NULL DEFAULT 0,
  failed_files INTEGER NOT NULL DEFAULT 0,
  current_filename TEXT DEFAULT NULL,
  error_message TEXT DEFAULT NULL,
  failure_details_json TEXT DEFAULT NULL,
  manifest_json TEXT NOT NULL,
  staging_dir TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  started_at TEXT DEFAULT NULL,
  completed_at TEXT DEFAULT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (pack_id) REFERENCES nl2sql_reference_packs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_nl2sql_reference_import_tasks_claim
  ON nl2sql_reference_import_tasks (status, created_at);

CREATE INDEX IF NOT EXISTS idx_nl2sql_reference_import_tasks_space
  ON nl2sql_reference_import_tasks (tenant_id, pack_id, created_at DESC);
