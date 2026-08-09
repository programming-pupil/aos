ALTER TABLE rd_repository_settings ADD COLUMN auto_sync_enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE rd_repository_settings ADD COLUMN auto_sync_interval_minutes INTEGER NOT NULL DEFAULT 60;
ALTER TABLE rd_repository_settings ADD COLUMN last_auto_sync_at TEXT DEFAULT NULL;
ALTER TABLE rd_repository_settings ADD COLUMN last_sync_error TEXT DEFAULT NULL;

CREATE INDEX idx_rd_repo_settings_auto_sync_due
ON rd_repository_settings (auto_sync_enabled, last_auto_sync_at);
