-- Scope-level Memory consolidation is a durable diff, not an overwrite of the
-- current summary. Relations preserve supersession/conflict history and the
-- cursor makes retries idempotent.
CREATE TABLE IF NOT EXISTS agent_memory_consolidation_cursors (
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  scope TEXT NOT NULL,
  app TEXT NOT NULL,
  session_key TEXT NOT NULL DEFAULT '',
  cursor TEXT NOT NULL DEFAULT '',
  revision INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (tenant_id, user_id, scope, app, session_key)
);

CREATE TABLE IF NOT EXISTS agent_memory_relations (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  from_memory_id TEXT NOT NULL,
  to_memory_id TEXT NOT NULL,
  relation TEXT NOT NULL,
  reason TEXT NOT NULL,
  source_cursor TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (tenant_id, user_id, from_memory_id, to_memory_id, relation)
);

CREATE INDEX IF NOT EXISTS idx_agent_memory_relations_scope
  ON agent_memory_relations(tenant_id, user_id, from_memory_id, to_memory_id);
