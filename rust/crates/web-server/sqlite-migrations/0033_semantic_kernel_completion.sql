-- Complete the durable semantic-kernel control plane.  These tables retain
-- canonical state and lineage; legacy rows remain searchable projections only.

CREATE TABLE provider_request_attempts (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT,
  iteration INTEGER,
  request_group_id TEXT NOT NULL,
  context_manifest_key TEXT,
  attempt_index INTEGER NOT NULL,
  parent_attempt_id TEXT,
  provider_kind TEXT NOT NULL,
  model TEXT NOT NULL,
  api_key_id TEXT,
  base_url_hash TEXT,
  search_stage TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  tool_schema_hash TEXT NOT NULL,
  tool_schema_ciphertext TEXT,
  native_search_mode TEXT NOT NULL,
  reasoning_effort TEXT,
  extra_body_hash TEXT,
  max_output_tokens INTEGER NOT NULL,
  stream INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('dispatched', 'completed', 'failed', 'timed_out', 'cancelled')),
  error_class TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  UNIQUE(tenant_id, session_id, request_group_id, attempt_index),
  FOREIGN KEY(parent_attempt_id) REFERENCES provider_request_attempts(id)
);

CREATE INDEX idx_provider_request_attempt_lineage
  ON provider_request_attempts(tenant_id, session_id, request_group_id, attempt_index);

CREATE TABLE compaction_transactions (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  thread_id TEXT NOT NULL,
  trigger TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('prepared', 'committed', 'aborted')),
  source_sequence_start INTEGER NOT NULL,
  source_sequence_end INTEGER NOT NULL,
  source_hash TEXT NOT NULL,
  source_archive_hash TEXT NOT NULL,
  source_archive_ciphertext TEXT NOT NULL,
  replacement_hash TEXT,
  replacement_ciphertext TEXT,
  memory_candidates_ciphertext TEXT,
  consolidation_cursor TEXT,
  checkpoint_id TEXT,
  ledger_sequence INTEGER,
  abort_reason TEXT,
  prepared_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  committed_at TEXT,
  aborted_at TEXT,
  UNIQUE(tenant_id, thread_id, source_hash)
);

CREATE INDEX idx_compaction_transactions_recovery
  ON compaction_transactions(tenant_id, thread_id, status, prepared_at);

CREATE TABLE structured_memory_facts (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  scope TEXT NOT NULL,
  app TEXT NOT NULL,
  session_id TEXT,
  channel TEXT NOT NULL,
  kind TEXT NOT NULL,
  subject_json TEXT NOT NULL,
  predicate TEXT NOT NULL,
  value_json TEXT NOT NULL,
  text TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  evidence_hash TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  valid_until TEXT,
  confidence REAL NOT NULL,
  sensitivity TEXT NOT NULL,
  current INTEGER NOT NULL DEFAULT 1,
  superseded_by TEXT,
  conflict_group TEXT,
  projection_memory_id TEXT,
  candidate_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, user_id, id),
  FOREIGN KEY(superseded_by) REFERENCES structured_memory_facts(id)
);

CREATE INDEX idx_structured_memory_current
  ON structured_memory_facts(tenant_id, user_id, scope, app, session_id, current, observed_at);

ALTER TABLE capability_tokens ADD COLUMN parent_token_id TEXT;
ALTER TABLE capability_tokens ADD COLUMN policy_version TEXT NOT NULL DEFAULT 'capability-policy-v1';
ALTER TABLE capability_tokens ADD COLUMN datasource_scope TEXT;
ALTER TABLE capability_tokens ADD COLUMN derivation_hash TEXT;
ALTER TABLE capability_tokens ADD COLUMN revoked_at TEXT;
ALTER TABLE capability_tokens ADD COLUMN revocation_reason TEXT;

CREATE INDEX idx_capability_parent_lineage
  ON capability_tokens(tenant_id, parent_token_id, revoked_at);

CREATE TABLE durable_user_questions (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  invocation_id TEXT NOT NULL,
  question TEXT NOT NULL,
  options_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL CHECK (status IN ('pending', 'answered', 'expired', 'cancelled')),
  answer TEXT,
  expires_at TEXT,
  answered_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, session_id, turn_id, invocation_id)
);

CREATE TABLE result_invariant_observations (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  analytic_intent_id TEXT NOT NULL,
  query_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  sql_hash TEXT NOT NULL,
  invariant_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pass', 'fail', 'not_observed')),
  observation_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, execution_id, invariant_id)
);

CREATE INDEX idx_result_invariant_query
  ON result_invariant_observations(tenant_id, query_id, sql_hash, created_at);

CREATE TABLE feedback_regression_cases (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  feedback_event_id TEXT NOT NULL,
  analytic_intent_id TEXT NOT NULL,
  original_ir_hash TEXT NOT NULL,
  original_sql_hash TEXT NOT NULL,
  corrected_sql_hash TEXT NOT NULL,
  semantic_diff_json TEXT NOT NULL,
  verification_json TEXT NOT NULL,
  execution_evidence_json TEXT,
  fixture_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('proposed', 'approved', 'revoked', 'verified')),
  approved_by TEXT,
  approved_at TEXT,
  last_verified_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, feedback_event_id),
  FOREIGN KEY(feedback_event_id) REFERENCES feedback_learning_events(id) ON DELETE CASCADE
);

CREATE INDEX idx_feedback_regression_scope
  ON feedback_regression_cases(tenant_id, datasource_id, status, created_at);
