-- Metric and join contracts are production authorities, not tenant-wide
-- shadow records. Rebuild them with datasource and source lineage so equal
-- logical names in different databases cannot collide.

ALTER TABLE nl2sql_metrics ADD COLUMN time_column TEXT;
ALTER TABLE nl2sql_metrics ADD COLUMN timezone TEXT NOT NULL DEFAULT 'UTC';
ALTER TABLE nl2sql_metrics ADD COLUMN population_json TEXT NOT NULL DEFAULT '{"subject":"query_rows","dedup_key":null,"exclude_test_users":false,"exclude_internal_users":false,"valid_record_rule":null}';
ALTER TABLE nl2sql_metrics ADD COLUMN allowed_grains_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE nl2sql_metrics ADD COLUMN invariants_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE nl2sql_metrics ADD COLUMN join_contract_ids_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE nl2sql_join_paths ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE nl2sql_join_paths ADD COLUMN cardinality TEXT;
ALTER TABLE nl2sql_join_paths ADD COLUMN temporal_condition TEXT;
ALTER TABLE nl2sql_join_paths ADD COLUMN nullable INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nl2sql_join_paths ADD COLUMN dedup_strategy TEXT;
ALTER TABLE nl2sql_join_paths ADD COLUMN allowed_grains_json TEXT NOT NULL DEFAULT '[]';

CREATE TABLE metric_contracts_next (
  id TEXT NOT NULL,
  tenant_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  source_metric_id INTEGER,
  version INTEGER NOT NULL,
  status TEXT NOT NULL,
  contract_json TEXT NOT NULL,
  lineage_json TEXT NOT NULL,
  valid_from TEXT NOT NULL,
  valid_until TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(tenant_id, datasource_id, id, version)
);

INSERT INTO metric_contracts_next
  (id, tenant_id, datasource_id, source_metric_id, version, status,
   contract_json, lineage_json, valid_from, valid_until)
SELECT legacy.id,
       legacy.tenant_id,
       CASE
         WHEN (SELECT COUNT(*) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id) = 1
           THEN (SELECT MIN(ds.id) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id)
         ELSE '__legacy_unscoped__'
       END,
       NULL,
       legacy.version,
       CASE
         WHEN (SELECT COUNT(*) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id) = 1
           THEN legacy.status
         ELSE 'legacy_unscoped'
       END,
       legacy.contract_json,
       json_object(
         'source', 'legacy_semantic_contract',
         'scopeResolution', CASE
           WHEN (SELECT COUNT(*) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id) = 1
             THEN 'single_datasource_mapped'
           ELSE 'blocked_ambiguous_datasource'
         END
       ),
       legacy.valid_from,
       legacy.valid_until
FROM metric_contracts legacy;

DROP TABLE metric_contracts;
ALTER TABLE metric_contracts_next RENAME TO metric_contracts;

CREATE INDEX idx_metric_contracts_current
  ON metric_contracts(tenant_id, datasource_id, status, valid_from, valid_until);
CREATE INDEX idx_metric_contracts_source
  ON metric_contracts(tenant_id, datasource_id, source_metric_id, version);

CREATE TABLE join_contracts_next (
  id TEXT NOT NULL,
  tenant_id TEXT NOT NULL,
  datasource_id TEXT NOT NULL,
  source_kind TEXT NOT NULL,
  source_id INTEGER,
  version INTEGER NOT NULL,
  status TEXT NOT NULL,
  contract_json TEXT NOT NULL,
  lineage_json TEXT NOT NULL,
  valid_from TEXT NOT NULL,
  valid_until TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(tenant_id, datasource_id, id, version)
);

INSERT INTO join_contracts_next
  (id, tenant_id, datasource_id, source_kind, source_id, version, status,
   contract_json, lineage_json, valid_from, valid_until)
SELECT legacy.id,
       legacy.tenant_id,
       CASE
         WHEN (SELECT COUNT(*) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id) = 1
           THEN (SELECT MIN(ds.id) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id)
         ELSE '__legacy_unscoped__'
       END,
       'legacy',
       NULL,
       legacy.version,
       CASE
         WHEN (SELECT COUNT(*) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id) = 1
           THEN legacy.status
         ELSE 'legacy_unscoped'
       END,
       legacy.contract_json,
       json_object(
         'source', 'legacy_semantic_contract',
         'scopeResolution', CASE
           WHEN (SELECT COUNT(*) FROM data_sources ds WHERE ds.tenant_id = legacy.tenant_id) = 1
             THEN 'single_datasource_mapped'
           ELSE 'blocked_ambiguous_datasource'
         END
       ),
       CURRENT_TIMESTAMP,
       NULL
FROM join_contracts legacy;

DROP TABLE join_contracts;
ALTER TABLE join_contracts_next RENAME TO join_contracts;

CREATE INDEX idx_join_contracts_current
  ON join_contracts(tenant_id, datasource_id, status, valid_from, valid_until);
CREATE INDEX idx_join_contracts_source
  ON join_contracts(tenant_id, datasource_id, source_kind, source_id, version);
