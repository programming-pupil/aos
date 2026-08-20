CREATE TABLE ciphertext_rotation_registry (
  table_name TEXT NOT NULL,
  column_name TEXT NOT NULL,
  codec TEXT NOT NULL,
  key_namespace TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY(table_name, column_name)
);

INSERT INTO ciphertext_rotation_registry(table_name, column_name, codec, key_namespace) VALUES
  ('api_keys', 'encrypted_key', 'aosenc-v1', 'platform'),
  ('bot_channels', 'auth_secret_ciphertext', 'aosenc-v1', 'platform'),
  ('agent_event_ledger', 'raw_payload_ciphertext', 'aosenc-v1', 'platform'),
  ('context_packet_manifests', 'raw_manifest_ciphertext', 'aosenc-v1', 'platform'),
  ('provider_request_attempts', 'tool_schema_ciphertext', 'aosenc-v1', 'platform'),
  ('tool_schema_manifests', 'schema_ciphertext', 'aosenc-v1', 'platform'),
  ('compaction_transactions', 'source_archive_ciphertext', 'aosenc-v1', 'platform'),
  ('compaction_transactions', 'replacement_ciphertext', 'aosenc-v1', 'platform'),
  ('compaction_transactions', 'memory_candidates_ciphertext', 'aosenc-v1', 'platform'),
  ('gitlab_projects', 'gitlab_token', 'aosenc-v1+legacy-git-token', 'platform'),
  ('data_sources', 'config', 'json-envelope+aosenc-v1', 'platform'),
  ('durable_user_questions', 'answer', 'aosenc-v1+legacy-plaintext', 'platform');

CREATE TABLE ciphertext_rotation_jobs (
  id TEXT PRIMARY KEY,
  active_key_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'failed')),
  rotated_count INTEGER NOT NULL DEFAULT 0,
  remaining_old_key_references INTEGER,
  last_error TEXT,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  heartbeat_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT
);
