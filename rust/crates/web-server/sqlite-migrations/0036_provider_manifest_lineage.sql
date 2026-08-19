CREATE TABLE tool_schema_manifests (
  id TEXT PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT,
  iteration INTEGER,
  schema_hash TEXT NOT NULL,
  schema_ciphertext TEXT NOT NULL,
  tool_count INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(tenant_id, session_id, turn_id, iteration, schema_hash)
);

ALTER TABLE context_packet_manifests ADD COLUMN iteration INTEGER;
CREATE UNIQUE INDEX idx_context_manifest_attempt
  ON context_packet_manifests(tenant_id, thread_id, turn_id, iteration);

ALTER TABLE prompt_manifests ADD COLUMN iteration INTEGER;
CREATE UNIQUE INDEX idx_prompt_manifest_attempt
  ON prompt_manifests(tenant_id, thread_id, turn_id, iteration);

ALTER TABLE provider_request_attempts ADD COLUMN context_manifest_id TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN prompt_manifest_id TEXT;
ALTER TABLE provider_request_attempts ADD COLUMN tool_manifest_id TEXT;

CREATE INDEX idx_provider_attempt_manifests
  ON provider_request_attempts(
    tenant_id, session_id, context_manifest_id, prompt_manifest_id, tool_manifest_id
  );

CREATE TRIGGER provider_attempt_manifest_lineage_insert
BEFORE INSERT ON provider_request_attempts
BEGIN
  SELECT CASE
    WHEN NEW.tool_manifest_id IS NULL OR NOT EXISTS (
      SELECT 1 FROM tool_schema_manifests
      WHERE id = NEW.tool_manifest_id AND tenant_id = NEW.tenant_id
        AND session_id = NEW.session_id
        AND turn_id IS NEW.turn_id AND iteration IS NEW.iteration
        AND schema_hash = NEW.tool_schema_hash
    ) THEN RAISE(ABORT, 'provider attempt tool manifest is missing')
    WHEN (NEW.context_manifest_id IS NULL) <> (NEW.prompt_manifest_id IS NULL)
      THEN RAISE(ABORT, 'provider attempt context/prompt lineage is incomplete')
    WHEN NEW.context_manifest_id IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM context_packet_manifests
      WHERE id = NEW.context_manifest_id AND tenant_id = NEW.tenant_id
        AND thread_id = NEW.session_id
        AND turn_id IS NEW.turn_id AND iteration IS NEW.iteration
    ) THEN RAISE(ABORT, 'provider attempt context manifest is missing')
    WHEN NEW.prompt_manifest_id IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM prompt_manifests
      WHERE id = NEW.prompt_manifest_id AND tenant_id = NEW.tenant_id
        AND thread_id = NEW.session_id
        AND turn_id IS NEW.turn_id AND iteration IS NEW.iteration
        AND model = NEW.model
        AND tool_schema_hash = NEW.tool_schema_hash
        AND context_manifest_id IS NEW.context_manifest_id
    ) THEN RAISE(ABORT, 'provider attempt prompt manifest is missing')
  END;
END;

CREATE TRIGGER provider_attempt_manifest_lineage_immutable
BEFORE UPDATE OF context_manifest_id, prompt_manifest_id, tool_manifest_id
ON provider_request_attempts
WHEN OLD.context_manifest_id IS NOT NEW.context_manifest_id
  OR OLD.prompt_manifest_id IS NOT NEW.prompt_manifest_id
  OR OLD.tool_manifest_id IS NOT NEW.tool_manifest_id
BEGIN
  SELECT RAISE(ABORT, 'provider attempt manifest lineage is immutable');
END;
