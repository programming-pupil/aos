-- Dedicated `/responses/compact` attempt lineage. Remote compaction returns
-- provider-normalized (often opaque) items and therefore cannot share the
-- ordinary chat-stream artifact schema without misrepresenting its protocol.

CREATE TABLE provider_compaction_attempts (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  trigger TEXT NOT NULL,
  protocol TEXT NOT NULL CHECK (protocol IN ('responses_compact_v1', 'responses_compact_v2')),
  provider_kind TEXT NOT NULL,
  model TEXT NOT NULL,
  endpoint_hash TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  attempt_index INTEGER NOT NULL,
  parent_attempt_id TEXT,
  status TEXT NOT NULL CHECK (status IN ('dispatched', 'completed', 'failed', 'timed_out')),
  normalized_output_hash TEXT,
  normalized_output_ciphertext TEXT,
  retained_items_hash TEXT,
  retained_items_ciphertext TEXT,
  output_applied INTEGER NOT NULL DEFAULT 0,
  fallback_reason TEXT,
  error_class TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  UNIQUE(tenant_id, session_id, trigger, attempt_index),
  FOREIGN KEY(parent_attempt_id) REFERENCES provider_compaction_attempts(id)
);

CREATE INDEX idx_provider_compaction_attempt_lineage
  ON provider_compaction_attempts(tenant_id, session_id, trigger, attempt_index);
