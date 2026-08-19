-- Durable phase-2 memory learning.  Compaction creates session-scoped
-- canonical facts; this queue conservatively promotes only pinned or
-- independently repeated facts into global memory.
CREATE TABLE memory_learning_jobs (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  app TEXT NOT NULL,
  compaction_transaction_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN
    ('queued', 'leased', 'cooldown', 'completed', 'quarantined', 'failed')),
  attempt INTEGER NOT NULL DEFAULT 0,
  lease_owner TEXT,
  lease_expires_at TEXT,
  next_attempt_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  promoted_count INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  UNIQUE(tenant_id, compaction_transaction_id),
  FOREIGN KEY(compaction_transaction_id) REFERENCES compaction_transactions(id)
);

CREATE INDEX idx_memory_learning_jobs_claim
  ON memory_learning_jobs(status, next_attempt_at, lease_expires_at, created_at);
