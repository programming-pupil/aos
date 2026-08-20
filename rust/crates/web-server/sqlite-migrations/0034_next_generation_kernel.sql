-- Canonical durable interaction protocol. Legacy approval/question tables are
-- compatibility projections and must not be used to decide resume dispatch.
CREATE TABLE durable_interactions (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  invocation_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN (
    'approval', 'user_question', 'credential_request', 'external_authorization'
  )),
  state TEXT NOT NULL CHECK (state IN (
    'pending', 'responded', 'granted', 'rejected', 'expired', 'cancelled', 'consumed'
  )),
  owner_user_id TEXT NOT NULL,
  allowed_responder_ids_json TEXT NOT NULL DEFAULT '[]',
  capability_requirement TEXT,
  request_schema_hash TEXT NOT NULL,
  choice_schema_hash TEXT,
  display_projection_json TEXT NOT NULL,
  response_projection_json TEXT,
  encrypted_secret_ref TEXT,
  responder_user_id TEXT,
  idempotency_key TEXT NOT NULL,
  expected_turn_revision INTEGER NOT NULL,
  created_event_id TEXT,
  response_event_id TEXT,
  consumed_event_id TEXT,
  response_hash TEXT,
  expires_at TEXT,
  responded_at TEXT,
  consumed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, session_id, idempotency_key),
  UNIQUE(tenant_id, session_id, turn_id, invocation_id)
);

CREATE INDEX idx_durable_interactions_pending
  ON durable_interactions(tenant_id, session_id, state, expires_at);

CREATE TABLE durable_interaction_outbox (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  interaction_id TEXT NOT NULL,
  intent TEXT NOT NULL CHECK (intent IN ('display', 'resume', 'expire', 'cancel')),
  idempotency_key TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'claimed', 'settled')),
  available_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  lease_owner TEXT,
  lease_expires_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  settled_at TEXT,
  UNIQUE(tenant_id, idempotency_key),
  FOREIGN KEY(interaction_id) REFERENCES durable_interactions(id) ON DELETE CASCADE
);

CREATE INDEX idx_durable_interaction_outbox_ready
  ON durable_interaction_outbox(state, available_at, lease_expires_at);

ALTER TABLE agent_turns ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE execution_checkpoints ADD COLUMN checkpoint_ciphertext TEXT;

-- Existing native approvals become canonical interactions during migration.
-- Rows without a complete authenticated scope are intentionally not guessed;
-- they remain unavailable and require the originating operation to be retried.
INSERT INTO durable_interactions (
  id, tenant_id, user_id, session_id, turn_id, invocation_id, kind, state,
  owner_user_id, allowed_responder_ids_json, capability_requirement,
  request_schema_hash, choice_schema_hash, display_projection_json,
  idempotency_key, expected_turn_revision, expires_at, created_at, updated_at
)
SELECT approval.id, approval.tenant_id, approval.user_id, approval.session_id,
       approval.turn_id, approval.invocation_id, 'approval', 'pending',
       approval.user_id, '[]', 'execute', approval.input_hash,
       'legacy-approval-choices-v1',
       json_object(
         'toolName', approval.tool_name,
         'currentMode', approval.current_mode,
         'requiredMode', approval.required_mode,
         'reason', approval.reason
       ),
       'approval:' || approval.invocation_id,
       COALESCE((
         SELECT turn.revision FROM agent_turns AS turn
         WHERE turn.tenant_id = approval.tenant_id
           AND turn.thread_id = approval.session_id
           AND turn.id = approval.turn_id
       ), 0),
       approval.expires_at, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
FROM approval_requests AS approval
WHERE approval.executor_scope = 'native' AND approval.status = 'pending'
  AND approval.user_id IS NOT NULL AND approval.session_id IS NOT NULL
  AND approval.turn_id IS NOT NULL AND approval.invocation_id IS NOT NULL
  AND approval.input_hash IS NOT NULL
ON CONFLICT(id) DO NOTHING;

-- Attempt-specific immutable request lineage. IDs are populated before the
-- provider boundary; hashes alone are diagnostic and cannot substitute them.
ALTER TABLE provider_request_attempts ADD COLUMN context_manifest_id TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN prompt_manifest_id TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN tool_manifest_id TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN wire_manifest_id TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN capability_profile_version TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN retry_reason TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN cache_key_hash TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN cache_status TEXT;

CREATE TABLE tool_manifests (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT,
  context_manifest_id TEXT NOT NULL,
  prompt_manifest_id TEXT,
  provider_kind TEXT NOT NULL,
  model TEXT NOT NULL,
  canonical_schema_hash TEXT NOT NULL,
  schema_ciphertext TEXT NOT NULL,
  permission_policy_version TEXT NOT NULL,
  tool_search_revision TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE wire_attempt_manifests (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT,
  attempt_id TEXT NOT NULL UNIQUE,
  context_manifest_id TEXT NOT NULL,
  prompt_manifest_id TEXT,
  tool_manifest_id TEXT NOT NULL,
  provider_kind TEXT NOT NULL,
  model TEXT NOT NULL,
  endpoint_hash TEXT,
  capability_profile_version TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  wire_tool_schema_hash TEXT NOT NULL,
  parent_attempt_id TEXT,
  retry_reason TEXT,
  cache_key_hash TEXT,
  cache_status TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY(tool_manifest_id) REFERENCES tool_manifests(id),
  FOREIGN KEY(attempt_id) REFERENCES provider_request_attempts(id)
);

CREATE TABLE provider_attempt_artifacts (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  attempt_id TEXT NOT NULL UNIQUE,
  terminal_status TEXT NOT NULL,
  stream_event_count INTEGER NOT NULL,
  payload_hash TEXT NOT NULL,
  payload_ciphertext TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY(attempt_id) REFERENCES provider_request_attempts(id) ON DELETE CASCADE
);

-- Exact compaction provenance and nested replacement lineage.
ALTER TABLE compaction_transactions ADD COLUMN source_message_ids_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE compaction_transactions ADD COLUMN parent_compaction_ids_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE compaction_transactions ADD COLUMN source_token_count INTEGER;
ALTER TABLE compaction_transactions ADD COLUMN replacement_token_count INTEGER;
ALTER TABLE compaction_transactions ADD COLUMN proof_result_json TEXT;
ALTER TABLE compaction_transactions ADD COLUMN baseline_manifest_id TEXT;
ALTER TABLE compaction_transactions ADD COLUMN source_event_sequences_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE compaction_transactions ADD COLUMN expected_ledger_tail_sequence INTEGER NOT NULL DEFAULT 0;
ALTER TABLE compaction_transactions ADD COLUMN expected_turn_id TEXT;
ALTER TABLE compaction_transactions ADD COLUMN expected_turn_revision INTEGER;
ALTER TABLE compaction_transactions ADD COLUMN prepared_replacement_hash TEXT;
ALTER TABLE compaction_transactions ADD COLUMN replacement_artifact_id TEXT;

-- Structured facts are canonical; lifecycle replaces the writable `current`
-- flag for all new commands. The old column remains a reducer projection.
ALTER TABLE structured_memory_facts ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'candidate'
  CHECK (lifecycle IN ('candidate','quarantined','confirmed','superseded','forgotten','rejected'));
ALTER TABLE structured_memory_facts ADD COLUMN valid_from TEXT;
ALTER TABLE structured_memory_facts ADD COLUMN recorded_at TEXT;
ALTER TABLE structured_memory_facts ADD COLUMN authority_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE structured_memory_facts ADD COLUMN evidence_refs_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE structured_memory_facts ADD COLUMN source_event_ids_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE structured_memory_facts ADD COLUMN pollution_lineage_json TEXT NOT NULL DEFAULT '[]';

CREATE TABLE memory_fact_events (
  event_id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  fact_id TEXT NOT NULL,
  stream_revision INTEGER NOT NULL,
  global_sequence INTEGER NOT NULL UNIQUE,
  schema_version INTEGER NOT NULL DEFAULT 1,
  actor_json TEXT NOT NULL,
  causation_event_id TEXT,
  correlation_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN (
    'candidate_created','quarantined','confirmed','superseded','forgotten','rejected','erased'
  )),
  lifecycle TEXT NOT NULL,
  source_event_ids_json TEXT NOT NULL DEFAULT '[]',
  payload_hash TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, idempotency_key),
  UNIQUE(tenant_id, fact_id, stream_revision)
);

CREATE INDEX idx_memory_fact_events_replay
  ON memory_fact_events(tenant_id, user_id, global_sequence);

-- Preserve the authority of facts that were already canonical before this
-- migration. Their former `current` projection determines only the lifecycle;
-- no historical timestamp or authority stronger than the source can be
-- invented. Re-encode candidate_json so the Memory 3.0 reducer can rebuild its
-- projection without depending on the old Rust struct layout.
UPDATE structured_memory_facts
SET lifecycle = CASE WHEN current = 1 THEN 'confirmed' ELSE 'superseded' END,
    valid_from = COALESCE(valid_from, observed_at),
    recorded_at = COALESCE(recorded_at, created_at, observed_at),
    authority_json = json_array('migration:structured-canonical'),
    evidence_refs_json = json_array(evidence_id),
    source_event_ids_json = json_array('migration:structured:' || id),
    pollution_lineage_json = '[]';

UPDATE structured_memory_facts
SET candidate_json = json_object(
      'fact_id', id,
      'projection_id', COALESCE(projection_memory_id, 'migration-projection:' || id),
      'tenant_id', tenant_id,
      'user_id', user_id,
      'scope', scope,
      'app', app,
      'session_id', session_id,
      'channel', channel,
      'kind', kind,
      'subject', json(subject_json),
      'predicate', predicate,
      'value', json(value_json),
      'text', text,
      'evidence_id', evidence_id,
      'evidence_hash', evidence_hash,
      'valid_from', valid_from,
      'valid_until', valid_until,
      'confidence', confidence,
      'sensitivity', sensitivity,
      'lifecycle', lifecycle,
      'authority', json(authority_json),
      'source_event_ids', json(source_event_ids_json),
      'pollution_lineage', json(pollution_lineage_json),
      'memory_type', COALESCE((
        SELECT item.memory_type FROM agent_memory_items AS item
        WHERE item.id = structured_memory_facts.projection_memory_id
          AND item.tenant_id = structured_memory_facts.tenant_id
          AND item.user_id = structured_memory_facts.user_id
      ), kind),
      'source_type', COALESCE((
        SELECT item.source_type FROM agent_memory_items AS item
        WHERE item.id = structured_memory_facts.projection_memory_id
          AND item.tenant_id = structured_memory_facts.tenant_id
          AND item.user_id = structured_memory_facts.user_id
      ), 'migration'),
      'pinned', COALESCE((
        SELECT CASE WHEN item.pinned != 0 THEN json('true') ELSE json('false') END
        FROM agent_memory_items AS item
        WHERE item.id = structured_memory_facts.projection_memory_id
          AND item.tenant_id = structured_memory_facts.tenant_id
          AND item.user_id = structured_memory_facts.user_id
      ), json('false')),
      'metadata', json(COALESCE((
        SELECT item.metadata_json FROM agent_memory_items AS item
        WHERE item.id = structured_memory_facts.projection_memory_id
          AND item.tenant_id = structured_memory_facts.tenant_id
          AND item.user_id = structured_memory_facts.user_id
      ), '{}')),
      'stale_at', (SELECT item.stale_at FROM agent_memory_items AS item
        WHERE item.id = structured_memory_facts.projection_memory_id
          AND item.tenant_id = structured_memory_facts.tenant_id
          AND item.user_id = structured_memory_facts.user_id),
      'verified_at', (SELECT item.verified_at FROM agent_memory_items AS item
        WHERE item.id = structured_memory_facts.projection_memory_id
          AND item.tenant_id = structured_memory_facts.tenant_id
          AND item.user_id = structured_memory_facts.user_id),
      'embedding_model', NULL,
      'embedding_dimensions', NULL,
      'embedding_json', NULL
    );

-- Projection-only rows have never passed canonical admission. Preserve them
-- as Candidate facts for review/migration, but never infer Confirmed from the
-- old projection's enabled flag.
INSERT INTO structured_memory_facts (
  id, tenant_id, user_id, scope, app, session_id, channel, kind,
  subject_json, predicate, value_json, text, evidence_id, evidence_hash,
  observed_at, valid_from, recorded_at, confidence, sensitivity, lifecycle,
  current, conflict_group, projection_memory_id, candidate_json,
  authority_json, evidence_refs_json, source_event_ids_json,
  pollution_lineage_json, created_at, updated_at
)
SELECT 'migration:projection:' || item.id,
       item.tenant_id, item.user_id, item.scope, item.app, item.session_id,
       'continuity', item.memory_type,
       json_object('kind', 'legacy_projection', 'id', item.id),
       'legacy_projection.' || item.memory_type,
       json_object('text', item.content),
       item.content,
       'migration:projection:' || item.id,
       item.content_hash,
       item.created_at, item.created_at, CURRENT_TIMESTAMP,
       item.confidence, 'internal', 'candidate', 0,
       item.content_hash, item.id,
       json_object(
         'fact_id', 'migration:projection:' || item.id,
         'projection_id', item.id,
         'tenant_id', item.tenant_id,
         'user_id', item.user_id,
         'scope', item.scope,
         'app', item.app,
         'session_id', item.session_id,
         'channel', 'continuity',
         'kind', item.memory_type,
         'subject', json_object('kind', 'legacy_projection', 'id', item.id),
         'predicate', 'legacy_projection.' || item.memory_type,
         'value', json_object('text', item.content),
         'text', item.content,
         'evidence_id', 'migration:projection:' || item.id,
         'evidence_hash', item.content_hash,
         'valid_from', item.created_at,
         'valid_until', NULL,
         'confidence', item.confidence,
         'sensitivity', 'internal',
         'lifecycle', 'candidate',
         'authority', json_array(),
         'source_event_ids', json_array('migration:projection:' || item.id),
         'pollution_lineage', json_array(),
         'memory_type', item.memory_type,
         'source_type', item.source_type,
         'pinned', CASE WHEN item.pinned != 0 THEN json('true') ELSE json('false') END,
         'metadata', json(COALESCE(item.metadata_json, '{}')),
         'stale_at', item.stale_at,
         'verified_at', item.verified_at,
         'embedding_model', NULL,
         'embedding_dimensions', NULL,
         'embedding_json', NULL
       ),
       '[]', json_array('migration:projection:' || item.id),
       json_array('migration:projection:' || item.id), '[]',
       item.created_at, item.updated_at
FROM agent_memory_items AS item
WHERE NOT EXISTS (
  SELECT 1 FROM structured_memory_facts AS fact
  WHERE fact.tenant_id = item.tenant_id
    AND fact.user_id = item.user_id
    AND fact.projection_memory_id = item.id
);

INSERT INTO memory_fact_events (
  event_id, tenant_id, user_id, fact_id, stream_revision, global_sequence,
  schema_version, actor_json, causation_event_id, correlation_id, operation,
  lifecycle, source_event_ids_json, payload_hash, idempotency_key, created_at
)
SELECT 'migration:memory-event:' || id,
       tenant_id, user_id, id, 1,
       ROW_NUMBER() OVER (ORDER BY tenant_id, user_id, id),
       1, '{"kind":"migration","id":"0034"}', NULL,
       'memory-fact:' || id,
       CASE lifecycle
         WHEN 'confirmed' THEN 'confirmed'
         WHEN 'superseded' THEN 'superseded'
         ELSE 'candidate_created'
       END,
       lifecycle, source_event_ids_json, evidence_hash,
       'migration:0034:' || id, CURRENT_TIMESTAMP
FROM structured_memory_facts
ORDER BY tenant_id, user_id, id;

-- Embeddings are projections of an exact fact version. Old vectors lack that
-- lineage, so they are invalidated and confirmed facts are queued for a fresh
-- local-first rebuild by the Memory governance worker.
CREATE TABLE memory_embedding_rebuild_outbox (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  fact_id TEXT NOT NULL,
  projection_memory_id TEXT NOT NULL,
  source_hash TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','claimed','processed','poisoned')),
  attempts INTEGER NOT NULL DEFAULT 0,
  available_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  lease_owner TEXT,
  lease_expires_at TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  processed_at TEXT,
  UNIQUE(tenant_id, fact_id, source_hash)
);

UPDATE agent_memory_items
SET embedding_model = NULL, embedding_dimensions = NULL, embedding_json = NULL;

INSERT INTO memory_embedding_rebuild_outbox (
  id, tenant_id, user_id, fact_id, projection_memory_id, source_hash
)
SELECT 'memory-embedding-rebuild:' || fact.id,
       fact.tenant_id, fact.user_id, fact.id, fact.projection_memory_id,
       fact.evidence_hash
FROM structured_memory_facts AS fact
WHERE fact.lifecycle = 'confirmed' AND fact.projection_memory_id IS NOT NULL;

CREATE TABLE memory_consolidation_leases (
  tenant_id TEXT PRIMARY KEY,
  lease_owner TEXT NOT NULL,
  fencing_token INTEGER NOT NULL,
  cursor_sequence INTEGER NOT NULL DEFAULT 0,
  lease_expires_at TEXT NOT NULL,
  cooldown_until TEXT,
  poison_batch_hash TEXT,
  last_error_class TEXT,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE memory_extraction_outbox (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  source_sequence_start INTEGER NOT NULL,
  source_sequence_end INTEGER NOT NULL,
  source_window_hash TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
    'pending','claimed','processed','poisoned'
  )),
  attempts INTEGER NOT NULL DEFAULT 0,
  available_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  lease_owner TEXT,
  lease_expires_at TEXT,
  last_error_class TEXT,
  candidate_count INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  processed_at TEXT,
  UNIQUE(tenant_id, session_id, turn_id, source_window_hash)
);

CREATE INDEX idx_memory_extraction_outbox_ready
  ON memory_extraction_outbox(status, available_at, lease_expires_at, tenant_id);

CREATE TABLE memory_consolidation_batches (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  lease_owner TEXT NOT NULL,
  fencing_token INTEGER NOT NULL,
  source_cursor_start INTEGER NOT NULL,
  source_cursor_end INTEGER NOT NULL,
  source_batch_hash TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('claimed','committed','poisoned')),
  candidate_count INTEGER NOT NULL DEFAULT 0,
  promoted_count INTEGER NOT NULL DEFAULT 0,
  quarantined_count INTEGER NOT NULL DEFAULT 0,
  conflict_count INTEGER NOT NULL DEFAULT 0,
  error_class TEXT,
  lease_expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  committed_at TEXT,
  UNIQUE(tenant_id, source_cursor_start, source_cursor_end, source_batch_hash)
);

CREATE TABLE memory_projection_state (
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  reducer_version TEXT NOT NULL,
  last_global_sequence INTEGER NOT NULL DEFAULT 0,
  projection_hash TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(tenant_id, user_id)
);

CREATE TABLE ciphertext_store_registry (
  store_id TEXT PRIMARY KEY,
  key_namespace TEXT NOT NULL,
  codec_version INTEGER NOT NULL,
  scanner_id TEXT NOT NULL,
  rewriter_id TEXT NOT NULL,
  retention_policy TEXT NOT NULL,
  registered_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE ciphertext_rotation_cursors (
  store_id TEXT NOT NULL,
  retiring_key_id TEXT NOT NULL,
  cursor TEXT,
  reference_count INTEGER NOT NULL DEFAULT 0,
  sampled_decrypt_ok INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(store_id, retiring_key_id),
  FOREIGN KEY(store_id) REFERENCES ciphertext_store_registry(store_id)
);

CREATE TABLE key_retirement_certificates (
  key_id TEXT PRIMARY KEY,
  registry_snapshot_hash TEXT NOT NULL,
  registered_store_count INTEGER NOT NULL,
  zero_reference_store_count INTEGER NOT NULL,
  sampled_decrypt_ok INTEGER NOT NULL,
  backup_policy_confirmed INTEGER NOT NULL,
  issued_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE pm_question_outcomes (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  question_id TEXT NOT NULL,
  domain_bucket TEXT NOT NULL,
  raw_prior REAL NOT NULL,
  calibrated_prior REAL NOT NULL,
  raw_posterior REAL,
  calibrated_posterior REAL,
  answered INTEGER NOT NULL DEFAULT 0,
  decision_changed INTEGER NOT NULL DEFAULT 0,
  risk_reduced REAL,
  rework_reduced REAL,
  user_effort_ms INTEGER,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, run_id, question_id)
);
