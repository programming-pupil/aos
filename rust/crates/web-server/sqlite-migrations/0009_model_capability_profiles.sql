CREATE TABLE IF NOT EXISTS "model_capability_profiles" (
  "id" TEXT NOT NULL,
  "tenant_id" TEXT NOT NULL,
  "profile_key" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "base_url" TEXT NOT NULL DEFAULT '',
  "protocol" TEXT NOT NULL,
  "model" TEXT NOT NULL,
  "model_type" TEXT NOT NULL DEFAULT 'chat',
  "schema_version" INTEGER NOT NULL DEFAULT 1,
  "registry_version" TEXT NOT NULL,
  "source" TEXT NOT NULL,
  "confidence" TEXT NOT NULL,
  "capabilities_json" TEXT NOT NULL,
  "observations_json" TEXT DEFAULT NULL,
  "detection_status" TEXT NOT NULL DEFAULT 'inferred',
  "last_error" TEXT DEFAULT NULL,
  "detected_at" TEXT time DEFAULT NULL,
  "expires_at" TEXT time DEFAULT NULL,
  "created_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT time NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY ("id"),
  UNIQUE ("tenant_id", "profile_key"),
  CONSTRAINT "model_capability_profiles_tenant_fk"
    FOREIGN KEY ("tenant_id") REFERENCES "tenants" ("id") ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS "idx_model_capability_profiles_lookup"
  ON "model_capability_profiles" ("tenant_id", "provider", "base_url", "model", "model_type");

ALTER TABLE "api_keys" ADD COLUMN "model_profile_id" TEXT DEFAULT NULL;

CREATE INDEX IF NOT EXISTS "idx_api_keys_model_profile"
  ON "api_keys" ("tenant_id", "model_profile_id");
