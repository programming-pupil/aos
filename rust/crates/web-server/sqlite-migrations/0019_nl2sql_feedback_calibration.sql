-- Feedback learning and confidence calibration are additive so installations
-- that already applied the semantic-kernel base migrations upgrade safely.
CREATE TABLE IF NOT EXISTS nl2sql_confidence_observations (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  analytic_intent_id TEXT NOT NULL,
  predicted_score REAL NOT NULL,
  actual_correct INTEGER,
  feedback_id INTEGER,
  created_at TEXT NOT NULL,
  labeled_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_nl2sql_confidence_scope
  ON nl2sql_confidence_observations(
    tenant_id,
    datasource_id,
    actual_correct,
    created_at
  );
