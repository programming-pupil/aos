-- Migration 0037 introduced iteration-scoped provider lineage after context
-- manifests already existed. Recover the iteration encoded in the immutable
-- manifest payload so retries of those requests satisfy the lineage trigger.
UPDATE context_packet_manifests
SET iteration = CAST(json_extract(manifest_json, '$.iteration') AS INTEGER)
WHERE iteration IS NULL
  AND json_valid(manifest_json)
  AND json_extract(manifest_json, '$.iteration') IS NOT NULL;

UPDATE prompt_manifests
SET iteration = (
  SELECT context.iteration
  FROM context_packet_manifests AS context
  WHERE context.id = prompt_manifests.context_manifest_id
)
WHERE iteration IS NULL
  AND context_manifest_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM context_packet_manifests AS context
    WHERE context.id = prompt_manifests.context_manifest_id
      AND context.iteration IS NOT NULL
  );
