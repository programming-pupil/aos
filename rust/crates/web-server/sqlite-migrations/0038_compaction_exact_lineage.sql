ALTER TABLE compaction_transactions ADD COLUMN source_unit_hashes_json TEXT NOT NULL DEFAULT '[]';
