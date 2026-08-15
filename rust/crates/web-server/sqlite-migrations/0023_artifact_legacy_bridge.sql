-- Bridge legacy artifact writers into the durable Artifact/Spill Plane.
--
-- The legacy tables remain readable for compatibility, but new rows must also
-- have a tenant/session-scoped tombstone, source payload and client projection
-- in the unified plane. SQLite cannot hash arbitrary text without an extension,
-- so the stable legacy row id is used as the source identity; the original
-- payload is still retained and can be re-hashed by an export/replay adapter.

INSERT OR IGNORE INTO artifact_objects
  (id, tenant_id, owner_scope, content_hash, media_type, byte_size, locator,
   retention_policy, payload_blob, deleted_at)
SELECT 'legacy:chat:' || id, tenant_id, session_id, id,
       'application/json', length(CAST(payload_json AS TEXT)),
       'artifact://legacy:chat:' || id, 'session',
       CAST(payload_json AS BLOB), NULL
FROM chat_turn_artifacts;

INSERT OR IGNORE INTO artifact_projections
  (artifact_id, projection_kind, policy_version, projection_hash,
   payload_json, omitted_bytes, created_at)
SELECT 'legacy:chat:' || id, 'client', 'legacy-bridge-v1', id,
       json_object('legacyArtifactId', id, 'artifactType', artifact_type,
                   'payload', json(payload_json)), 0, created_at
FROM chat_turn_artifacts;

INSERT OR IGNORE INTO artifact_objects
  (id, tenant_id, owner_scope, content_hash, media_type, byte_size, locator,
   retention_policy, payload_blob, deleted_at)
SELECT 'legacy:runtime:' || id, tenant_id, runtime_session_id, COALESCE(content_hash, id),
       'text/plain; charset=utf-8', size_bytes,
       'artifact://legacy:runtime:' || id, 'session',
       CAST(COALESCE(content_text, '') AS BLOB), NULL
FROM agent_runtime_artifacts;

INSERT OR IGNORE INTO artifact_projections
  (artifact_id, projection_kind, policy_version, projection_hash,
   payload_json, omitted_bytes, created_at)
SELECT 'legacy:runtime:' || id, 'client', 'legacy-bridge-v1', COALESCE(content_hash, id),
       json_object('legacyArtifactId', id, 'artifactType', artifact_type,
                   'path', path, 'preview', COALESCE(content_text, '')),
       MAX(size_bytes - length(CAST(COALESCE(content_text, '') AS TEXT)), 0), created_at
FROM agent_runtime_artifacts;

CREATE TRIGGER IF NOT EXISTS trg_chat_turn_artifacts_to_artifact_plane
AFTER INSERT ON chat_turn_artifacts
BEGIN
  INSERT OR REPLACE INTO artifact_objects
    (id, tenant_id, owner_scope, content_hash, media_type, byte_size, locator,
     retention_policy, payload_blob, deleted_at)
  VALUES ('legacy:chat:' || NEW.id, NEW.tenant_id, NEW.session_id, NEW.id,
          'application/json', length(CAST(NEW.payload_json AS TEXT)),
          'artifact://legacy:chat:' || NEW.id, 'session',
          CAST(NEW.payload_json AS BLOB), NULL);
  INSERT OR REPLACE INTO artifact_projections
    (artifact_id, projection_kind, policy_version, projection_hash,
     payload_json, omitted_bytes, created_at)
  VALUES ('legacy:chat:' || NEW.id, 'client', 'legacy-bridge-v1', NEW.id,
          json_object('legacyArtifactId', NEW.id, 'artifactType', NEW.artifact_type,
                      'payload', json(NEW.payload_json)), 0, NEW.created_at);
END;

CREATE TRIGGER IF NOT EXISTS trg_agent_runtime_artifacts_to_artifact_plane
AFTER INSERT ON agent_runtime_artifacts
BEGIN
  INSERT OR REPLACE INTO artifact_objects
    (id, tenant_id, owner_scope, content_hash, media_type, byte_size, locator,
     retention_policy, payload_blob, deleted_at)
  VALUES ('legacy:runtime:' || NEW.id, NEW.tenant_id, NEW.runtime_session_id,
          COALESCE(NEW.content_hash, NEW.id), 'text/plain; charset=utf-8',
          NEW.size_bytes, 'artifact://legacy:runtime:' || NEW.id, 'session',
          CAST(COALESCE(NEW.content_text, '') AS BLOB), NULL);
  INSERT OR REPLACE INTO artifact_projections
    (artifact_id, projection_kind, policy_version, projection_hash,
     payload_json, omitted_bytes, created_at)
  VALUES ('legacy:runtime:' || NEW.id, 'client', 'legacy-bridge-v1',
          COALESCE(NEW.content_hash, NEW.id),
          json_object('legacyArtifactId', NEW.id, 'artifactType', NEW.artifact_type,
                      'path', NEW.path, 'preview', COALESCE(NEW.content_text, '')),
          MAX(NEW.size_bytes - length(CAST(COALESCE(NEW.content_text, '') AS TEXT)), 0),
          NEW.created_at);
END;
