-- Keep the ordinary manifest as a redacted audit projection while retaining
-- the exact provider-visible packet under the same encrypted recovery policy
-- as durable runtime events. The hash binds ciphertext recovery to the
-- model-visible bytes without exposing those bytes to normal SQL readers.
ALTER TABLE context_packet_manifests ADD COLUMN raw_manifest_hash TEXT;
ALTER TABLE context_packet_manifests ADD COLUMN raw_manifest_ciphertext TEXT;

-- Persist the complete selected Prompt Manifest metadata (never prompt text)
-- so model capability, section-source, evaluation, cache, and rollback lineage
-- remain queryable without unpacking the Context Packet.
ALTER TABLE prompt_manifests ADD COLUMN manifest_json TEXT;
