ALTER TABLE feedback_learning_events ADD COLUMN approved_by TEXT;
ALTER TABLE feedback_learning_events ADD COLUMN approved_at TEXT;

CREATE TABLE feedback_learning_approval_events (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  feedback_event_id TEXT NOT NULL,
  decision TEXT NOT NULL CHECK (decision IN ('approved', 'revoked')),
  decided_by TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (feedback_event_id) REFERENCES feedback_learning_events(id) ON DELETE CASCADE
);

CREATE INDEX idx_feedback_learning_approval_events_lookup
  ON feedback_learning_approval_events(tenant_id, feedback_event_id, created_at);
