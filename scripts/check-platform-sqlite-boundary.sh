#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT_DIR"

failed=0

# GitHub-hosted runners do not guarantee ripgrep. Keep this repository guard
# self-contained so a missing optional search binary cannot masquerade as a
# database-boundary regression.
search_lines() {
  local pattern="$1"
  shift
  if command -v rg >/dev/null 2>&1; then
    rg -n "$pattern" "$@" --glob '!scripts/check-platform-sqlite-boundary.sh'
  else
    grep -R -n -E --exclude='check-platform-sqlite-boundary.sh' \
      "$pattern" "$@"
  fi
}

search_files() {
  local pattern="$1"
  shift
  if command -v rg >/dev/null 2>&1; then
    rg -l "$pattern" "$@"
  else
    grep -R -l -E "$pattern" "$@"
  fi
}

file_contains() {
  local pattern="$1"
  local file="$2"
  if command -v rg >/dev/null 2>&1; then
    rg -q "$pattern" "$file"
  else
    grep -q -E "$pattern" "$file"
  fi
}

deployment_targets=(
  docker-compose.yml
  Containerfile
  .env.example
  README.md
  USAGE.md
  docs/INSTALL.md
  docs/container.md
  docs/ARCHITECTURE.md
  scripts
  rust/scripts
  .github/workflows
)

deployment_hits="$(
  search_lines 'DATABASE_URL|MYSQL_ROOT_PASSWORD|AOS_RUN_MYSQL|AOS_TEST_DATABASE_URL|mysqladmin|mysqld' \
    "${deployment_targets[@]}" \
    2>/dev/null || true
)"
if [[ -n "$deployment_hits" ]]; then
  echo "AOS deployment/configuration still contains a platform MySQL dependency:" >&2
  echo "$deployment_hits" >&2
  failed=1
fi

mysql_type_hits="$(
  search_files \
    'MySqlPool|MySqlConnection|MySqlRow|MySqlArguments|QueryBuilder<[^>]*MySql|Transaction<[^>]*MySql|sqlx::MySql|sqlx::mysql' \
    rust/crates 2>/dev/null || true
)"
while IFS= read -r source_file; do
  [[ -z "$source_file" ]] && continue
  case "$source_file" in
    rust/crates/nl2sql-core/src/* | \
    rust/crates/nl2sql-domain/src/* | \
    rust/crates/web-server/src/nl2sql/schema_describer.rs | \
    rust/crates/web-server/src/routes/data_sources.rs | \
    rust/crates/web-server/src/routes/nl2sql/*)
      ;;
    *)
      echo "platform source still binds a MySQL SQLx type: $source_file" >&2
      failed=1
      ;;
  esac
done <<< "$mysql_type_hits"

mysql_dialect_hits="$(
  if command -v rg >/dev/null 2>&1; then
    rg -n \
    'ON DUPLICATE KEY|INSERT IGNORE|FOR UPDATE|FROM DUAL|GET_LOCK|RELEASE_LOCK|LAST_INSERT_ID|UTC_TIMESTAMP|NOW\([0-9]*\)|CURRENT_(DATE|TIME|TIMESTAMP)\(\)|INTERVAL[[:space:]]+[A-Za-z0-9_?]+[[:space:]]+(SECOND|MINUTE|HOUR|DAY|WEEK|MONTH|YEAR)|DATE_FORMAT|TIMESTAMPDIFF|TIMESTAMPADD|INFORMATION_SCHEMA| AS SIGNED|LEFT\([A-Za-z_]|RIGHT\([A-Za-z_]|POW\(' \
    rust/crates/web-server/src \
    rust/crates/agent-gateway/src \
    rust/crates/billing/src \
    rust/crates/pm-orchestrator/src \
    --glob '!rust/crates/web-server/src/nl2sql/**' \
    --glob '!rust/crates/web-server/src/routes/data_sources.rs' \
    --glob '!rust/crates/web-server/src/routes/nl2sql/**' \
    2>/dev/null || true
  else
    grep -R -n -E \
      --exclude-dir=nl2sql \
      --exclude='data_sources.rs' \
      'ON DUPLICATE KEY|INSERT IGNORE|FOR UPDATE|FROM DUAL|GET_LOCK|RELEASE_LOCK|LAST_INSERT_ID|UTC_TIMESTAMP|NOW\([0-9]*\)|CURRENT_(DATE|TIME|TIMESTAMP)\(\)|INTERVAL[[:space:]]+[A-Za-z0-9_?]+[[:space:]]+(SECOND|MINUTE|HOUR|DAY|WEEK|MONTH|YEAR)|DATE_FORMAT|TIMESTAMPDIFF|TIMESTAMPADD|INFORMATION_SCHEMA| AS SIGNED|LEFT\([A-Za-z_]|RIGHT\([A-Za-z_]|POW\(' \
      rust/crates/web-server/src \
      rust/crates/agent-gateway/src \
      rust/crates/billing/src \
      rust/crates/pm-orchestrator/src \
      2>/dev/null || true
  fi
)"
if [[ -n "$mysql_dialect_hits" ]]; then
  echo "platform source still contains a MySQL-only SQL construct:" >&2
  echo "$mysql_dialect_hits" >&2
  failed=1
fi

if find rust/crates/web-server -type f -name 'aos-dump.sql' -print -quit | grep -q .; then
  echo "the pre-release MySQL dump must not be shipped" >&2
  failed=1
fi
if [[ -d rust/crates/web-server/migrations ]] \
  && find rust/crates/web-server/migrations -type f -print -quit | grep -q .; then
  echo "legacy platform migration files must not be shipped" >&2
  failed=1
fi

if [[ ! -f rust/crates/web-server/sqlite-migrations/0001_baseline.sql ]]; then
  echo "missing embedded SQLite baseline migration" >&2
  failed=1
fi
if ! file_contains 'SqlitePool' rust/crates/web-server/src/state.rs; then
  echo "AppState no longer exposes the SQLite platform pool" >&2
  failed=1
fi
if ! file_contains '"sqlite"' rust/Cargo.toml || ! file_contains '"mysql"' rust/Cargo.toml; then
  echo "SQLx must retain both SQLite platform storage and MySQL connector features" >&2
  failed=1
fi
if ! file_contains 'MySqlPoolOptions' rust/crates/nl2sql-core/src/datasource_pool.rs; then
  echo "NL2SQL external MySQL pool support was removed" >&2
  failed=1
fi
if ! file_contains 'decode_mysql_cell' rust/crates/nl2sql-core/src/cell_decoder.rs; then
  echo "NL2SQL external MySQL result decoding was removed" >&2
  failed=1
fi
if ! file_contains '"mysql"' rust/crates/web-server/src/routes/data_sources.rs; then
  echo "NL2SQL MySQL datasource API support was removed" >&2
  failed=1
fi
if ! file_contains "value: 'mysql'" webui/src/pages/DataSources.tsx; then
  echo "NL2SQL MySQL datasource UI support was removed" >&2
  failed=1
fi

if (( failed != 0 )); then
  exit 1
fi

echo "platform SQLite boundary passed; external MySQL/TiDB connector support is preserved"
