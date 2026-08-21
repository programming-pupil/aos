-- Generic durable Agent Team control plane. The roster, mailbox, task board,
-- and global permits are authorities; in-memory workers are replaceable leases.

CREATE TABLE IF NOT EXISTS agent_team_members (
  tenant_id TEXT NOT NULL,
  team_id TEXT NOT NULL,
  thread_id TEXT NOT NULL,
  parent_thread_id TEXT,
  name TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'worker',
  depth INTEGER NOT NULL,
  status TEXT NOT NULL,
  context_mode TEXT NOT NULL,
  model TEXT,
  spawn_idempotency_key TEXT NOT NULL,
  detached INTEGER NOT NULL DEFAULT 0,
  wake_requested INTEGER NOT NULL DEFAULT 0,
  lease_fencing INTEGER NOT NULL DEFAULT 0,
  lease_owner TEXT,
  lease_expires_at TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(tenant_id, team_id, thread_id),
  UNIQUE(tenant_id, team_id, name),
  UNIQUE(tenant_id, parent_thread_id, spawn_idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_agent_team_members_status
  ON agent_team_members(tenant_id, team_id, status, wake_requested, updated_at);
CREATE INDEX IF NOT EXISTS idx_agent_team_members_parent
  ON agent_team_members(tenant_id, parent_thread_id, status);

CREATE TABLE IF NOT EXISTS agent_mailbox_items (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  team_id TEXT NOT NULL,
  sender_thread_id TEXT NOT NULL,
  target_thread_id TEXT NOT NULL,
  delivery TEXT NOT NULL,
  content_ciphertext TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  accepted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  consumed_turn_id TEXT,
  consumed_at TEXT,
  observed_turn_id TEXT,
  delivery_attempts INTEGER NOT NULL DEFAULT 0,
  UNIQUE(tenant_id, target_thread_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_agent_mailbox_pending
  ON agent_mailbox_items(tenant_id, target_thread_id, consumed_at, accepted_at);

CREATE TABLE IF NOT EXISTS agent_team_tasks (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  team_id TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  subject TEXT NOT NULL,
  description_ciphertext TEXT NOT NULL,
  description_hash TEXT NOT NULL,
  status TEXT NOT NULL,
  owner_thread_id TEXT,
  blocked_by_json TEXT NOT NULL DEFAULT '[]',
  write_scopes_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_agent_team_tasks_status
  ON agent_team_tasks(tenant_id, team_id, status, updated_at);

CREATE TABLE IF NOT EXISTS agent_concurrency_permits (
  tenant_id TEXT NOT NULL,
  scope TEXT NOT NULL,
  holder_thread_id TEXT NOT NULL,
  lease_fencing INTEGER NOT NULL,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(tenant_id, scope, holder_thread_id)
);

CREATE INDEX IF NOT EXISTS idx_agent_concurrency_permits_expiry
  ON agent_concurrency_permits(tenant_id, scope, expires_at);
