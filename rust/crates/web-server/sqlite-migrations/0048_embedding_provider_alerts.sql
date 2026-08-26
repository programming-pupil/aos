CREATE TABLE embedding_provider_alerts (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  profile_id TEXT NOT NULL,
  scenario TEXT NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'resolved')),
  failure_count INTEGER NOT NULL DEFAULT 1,
  notification_version INTEGER NOT NULL DEFAULT 1,
  first_failed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  last_failed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  last_error TEXT NOT NULL,
  last_notified_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  resolved_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (tenant_id, profile_id, scenario),
  FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE INDEX idx_embedding_provider_alerts_active
  ON embedding_provider_alerts(tenant_id, status, last_failed_at DESC);
