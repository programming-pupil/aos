-- A Plan Mode document can span several independently deployed services.
-- repository_id remains the primary/backward-compatible repository reference;
-- the JSON list preserves the complete ordered selection.
ALTER TABLE rd_specs ADD COLUMN repository_ids_json TEXT DEFAULT NULL;

UPDATE rd_specs
SET repository_ids_json = json_array(repository_id)
WHERE repository_id IS NOT NULL
  AND (repository_ids_json IS NULL OR trim(repository_ids_json) = '');
