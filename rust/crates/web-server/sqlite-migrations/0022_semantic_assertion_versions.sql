-- Preserve every accepted semantic assertion revision. The current
-- `semantic_assertions` row remains a fast projection for existing readers;
-- this append-only history is the replay and deletion/audit source.
CREATE TABLE IF NOT EXISTS semantic_assertion_versions (
  tenant_id TEXT NOT NULL,
  assertion_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  assertion_json TEXT NOT NULL,
  source_event_ids_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (tenant_id, assertion_id, version)
);

CREATE INDEX IF NOT EXISTS idx_semantic_assertion_versions_lookup
  ON semantic_assertion_versions(tenant_id, assertion_id, version DESC);
