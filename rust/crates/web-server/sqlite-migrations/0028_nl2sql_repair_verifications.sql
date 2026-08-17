-- Immutable semantic decisions for SQL repair candidates. These rows are
-- separate from the canonical intent and its initial release decision so a
-- repair can never rewrite what the user originally asked for.
CREATE TABLE IF NOT EXISTS nl2sql_repair_verifications (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    analytic_intent_id TEXT NOT NULL,
    sql_hash TEXT NOT NULL,
    verification_json TEXT NOT NULL,
    release_decision TEXT NOT NULL,
    calibrated_score REAL NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(tenant_id, analytic_intent_id, sql_hash)
);

CREATE INDEX IF NOT EXISTS idx_nl2sql_repair_verifications_intent
    ON nl2sql_repair_verifications(tenant_id, analytic_intent_id, created_at);
