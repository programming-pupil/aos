-- Durable asynchronous lifecycle for stdio MCP cold starts.

ALTER TABLE mcp_server_registry ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE mcp_server_registry ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mcp_server_registry ADD COLUMN last_attempt_at TEXT DEFAULT NULL;
ALTER TABLE mcp_server_registry ADD COLUMN lease_expires_at TEXT DEFAULT NULL;
ALTER TABLE mcp_server_registry ADD COLUMN tools_json TEXT DEFAULT NULL;

CREATE INDEX IF NOT EXISTS idx_mcp_server_lifecycle
ON mcp_server_registry (enabled, transport, status, lease_expires_at, updated_at);
