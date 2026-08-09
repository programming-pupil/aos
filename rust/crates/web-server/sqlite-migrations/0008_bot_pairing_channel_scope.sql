ALTER TABLE agent_external_identity_pairings
ADD COLUMN channel_id TEXT DEFAULT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_pairing_channel_expiry
ON agent_external_identity_pairings (tenant_id, channel_id, expires_at);
