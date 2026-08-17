-- Result-cache reuse is valid only under the exact semantic compiler lineage
-- that released the SQL. Existing rows intentionally retain empty lineage and
-- therefore miss after this migration instead of inheriting release authority.
ALTER TABLE nl2sql_result_cache ADD COLUMN intent_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE nl2sql_result_cache ADD COLUMN schema_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE nl2sql_result_cache ADD COLUMN metric_contracts_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE nl2sql_result_cache ADD COLUMN join_contracts_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE nl2sql_result_cache ADD COLUMN policy_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE nl2sql_result_cache ADD COLUMN compiler_version TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_nl2sql_result_cache_lineage
    ON nl2sql_result_cache(
        tenant_id,
        datasource_id,
        question_hash,
        intent_hash,
        schema_hash,
        metric_contracts_hash,
        join_contracts_hash,
        policy_hash,
        compiler_version
    );
