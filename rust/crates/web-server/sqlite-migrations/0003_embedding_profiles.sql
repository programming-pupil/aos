-- Isolate NL2SQL vector spaces by tenant and embedding profile. A profile is
-- immutable: changing provider, endpoint, model, dimensions, model version, or
-- vector-space signature creates a new profile and leaves the old index intact
-- until the replacement is ready.

CREATE TABLE IF NOT EXISTS nl2sql_embedding_profiles (
  id TEXT NOT NULL PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  profile_kind TEXT NOT NULL CHECK (profile_kind IN ('api', 'local')),
  provider TEXT NOT NULL,
  base_url TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL,
  dimensions INTEGER NOT NULL,
  model_version TEXT NOT NULL,
  vector_signature TEXT NOT NULL,
  configured_via TEXT NOT NULL,
  health_status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (health_status IN ('unknown', 'healthy', 'degraded', 'unavailable')),
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  circuit_open_until TEXT DEFAULT NULL,
  last_success_at TEXT DEFAULT NULL,
  last_failure_at TEXT DEFAULT NULL,
  last_error TEXT DEFAULT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    tenant_id,
    profile_kind,
    provider,
    base_url,
    model,
    dimensions,
    model_version,
    vector_signature
  ),
  FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_nl2sql_embedding_profiles_tenant_kind
  ON nl2sql_embedding_profiles (tenant_id, profile_kind, updated_at DESC);

CREATE TABLE IF NOT EXISTS nl2sql_datasource_embedding_profiles (
  tenant_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  profile_kind TEXT NOT NULL CHECK (profile_kind IN ('api', 'local')),
  active_profile_id TEXT DEFAULT NULL,
  desired_profile_id TEXT DEFAULT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'building', 'ready', 'degraded', 'failed', 'disabled')),
  indexed_items INTEGER NOT NULL DEFAULT 0,
  total_items INTEGER NOT NULL DEFAULT 0,
  last_error TEXT DEFAULT NULL,
  activated_at TEXT DEFAULT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (tenant_id, datasource_id, profile_kind),
  FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
  FOREIGN KEY (datasource_id) REFERENCES data_sources(id) ON DELETE CASCADE,
  FOREIGN KEY (active_profile_id) REFERENCES nl2sql_embedding_profiles(id) ON DELETE SET NULL,
  FOREIGN KEY (desired_profile_id) REFERENCES nl2sql_embedding_profiles(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_nl2sql_datasource_embedding_profile_active
  ON nl2sql_datasource_embedding_profiles (tenant_id, profile_kind, active_profile_id, status);

CREATE TABLE IF NOT EXISTS nl2sql_embedding_reindex_jobs (
  id TEXT NOT NULL PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  profile_kind TEXT NOT NULL CHECK (profile_kind IN ('api', 'local')),
  profile_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  last_error TEXT DEFAULT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  started_at TEXT DEFAULT NULL,
  completed_at TEXT DEFAULT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
  FOREIGN KEY (datasource_id) REFERENCES data_sources(id) ON DELETE CASCADE,
  FOREIGN KEY (profile_id) REFERENCES nl2sql_embedding_profiles(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_nl2sql_embedding_reindex_job_inflight
  ON nl2sql_embedding_reindex_jobs (tenant_id, datasource_id, profile_kind, profile_id)
  WHERE status IN ('pending', 'running');

CREATE INDEX IF NOT EXISTS idx_nl2sql_embedding_reindex_job_ready
  ON nl2sql_embedding_reindex_jobs (status, next_attempt_at, created_at);

CREATE TABLE IF NOT EXISTS nl2sql_reference_chunk_embeddings (
  tenant_id TEXT NOT NULL,
  chunk_id TEXT NOT NULL,
  profile_id TEXT NOT NULL,
  model TEXT NOT NULL,
  dimensions INTEGER NOT NULL,
  embedding_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (tenant_id, chunk_id, profile_id),
  FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
  FOREIGN KEY (chunk_id) REFERENCES nl2sql_reference_chunks(id) ON DELETE CASCADE,
  FOREIGN KEY (profile_id) REFERENCES nl2sql_embedding_profiles(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_nl2sql_reference_chunk_embedding_profile
  ON nl2sql_reference_chunk_embeddings (tenant_id, profile_id, chunk_id);
