-- Backfill canonical structured facts before active reads require canonical ownership.
INSERT INTO structured_memory_facts (
  id, tenant_id, user_id, scope, app, session_id, channel, kind,
  subject_json, predicate, value_json, text, evidence_id, evidence_hash,
  observed_at, valid_until, confidence, sensitivity, current, superseded_by,
  conflict_group, projection_memory_id, candidate_json, created_at, updated_at
)
SELECT
  'structured-memory-backfill:' || memory.id,
  memory.tenant_id, memory.user_id, memory.scope, memory.app, memory.session_id,
  CASE WHEN memory.source_type = 'compaction' THEN 'continuity_state' ELSE 'long_term_memory' END,
  memory.memory_type,
  json_object('memoryType', memory.memory_type, 'sessionId', memory.session_id, 'sourceType', memory.source_type),
  memory.memory_type, json_quote(memory.content), memory.content,
  'memory:' || memory.id, memory.content_hash, memory.created_at, memory.stale_at,
  memory.confidence, 'internal', CASE WHEN memory.enabled = 1 THEN 1 ELSE 0 END,
  NULL, memory.content_hash, memory.id,
  json_object(
    'projectionMemoryId', memory.id, 'content', memory.content,
    'sourceType', memory.source_type, 'pinned', memory.pinned,
    'confidence', memory.confidence, 'migration', '0035_memory_canonical_backfill'
  ),
  memory.created_at, memory.updated_at
FROM agent_memory_items AS memory
WHERE NOT EXISTS (
  SELECT 1 FROM structured_memory_facts AS fact
  WHERE fact.tenant_id = memory.tenant_id
    AND fact.user_id = memory.user_id
    AND fact.projection_memory_id = memory.id
);

CREATE INDEX idx_structured_memory_projection_current
  ON structured_memory_facts(tenant_id, user_id, projection_memory_id, current);
