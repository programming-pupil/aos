-- Keep the complete, tenant-scoped (already protected) artifact payload so a
-- bounded model projection is always recoverable. Existing projection rows
-- remain readable; new writes populate both columns during the migration.
ALTER TABLE artifact_objects ADD COLUMN payload_blob BLOB;
