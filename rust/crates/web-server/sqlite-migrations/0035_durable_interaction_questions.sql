-- Durable user-question lifecycle. Rebuild the 0033 preview table because its
-- CHECK constraint did not include the exactly-once consumed state.
ALTER TABLE durable_user_questions RENAME TO durable_user_questions_legacy;

CREATE TABLE durable_user_questions (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  invocation_id TEXT NOT NULL,
  question TEXT NOT NULL,
  options_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL CHECK (
    status IN ('pending', 'answered', 'expired', 'cancelled', 'consumed')
  ),
  answer TEXT,
  answer_hash TEXT,
  expires_at TEXT,
  answered_at TEXT,
  consumed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, session_id, turn_id, invocation_id)
);

INSERT INTO durable_user_questions (
  id, tenant_id, user_id, session_id, turn_id, invocation_id, question,
  options_json, status, answer, expires_at, answered_at, created_at, updated_at
)
SELECT
  id, tenant_id, user_id, session_id, turn_id, invocation_id, question,
  options_json, status, answer, expires_at, answered_at, created_at, created_at
FROM durable_user_questions_legacy;

DROP TABLE durable_user_questions_legacy;

CREATE INDEX idx_durable_questions_owner_pending
  ON durable_user_questions(tenant_id, user_id, session_id, status, created_at);

CREATE INDEX idx_durable_questions_resume
  ON durable_user_questions(tenant_id, session_id, turn_id, invocation_id, status);
