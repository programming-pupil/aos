//! SQLite-backed semantic index for AOS Code Studio.
//!
//! The RD embedding store is intentionally local and best-effort. It is used to
//! rank repository/task context candidates; it never replaces reading real files
//! before producing code changes.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Context;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
const MAX_SEARCH_ROWS_PER_REPOSITORY: usize = 20_000;
const MIN_SEMANTIC_SCORE: f32 = 0.18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RdEmbeddingChunkType {
    ContextSummary,
    FileSummary,
    Symbol,
    Import,
    Task,
}

impl RdEmbeddingChunkType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ContextSummary => "context_summary",
            Self::FileSummary => "file_summary",
            Self::Symbol => "symbol",
            Self::Import => "import",
            Self::Task => "task",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "context_summary" => Self::ContextSummary,
            "symbol" => Self::Symbol,
            "import" => Self::Import,
            "task" => Self::Task,
            _ => Self::FileSummary,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RdEmbeddingChunkUpsert {
    pub(crate) chunk_id: String,
    pub(crate) chunk_type: RdEmbeddingChunkType,
    pub(crate) file_path: Option<String>,
    pub(crate) symbol_name: Option<String>,
    pub(crate) line_number: Option<u64>,
    pub(crate) content_hash: String,
    pub(crate) text: String,
    pub(crate) metadata_json: serde_json::Value,
    pub(crate) vector: Vec<f32>,
    pub(crate) task_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RdEmbeddingSearchHit {
    pub(crate) chunk_id: String,
    pub(crate) chunk_type: RdEmbeddingChunkType,
    pub(crate) file_path: Option<String>,
    pub(crate) symbol_name: Option<String>,
    pub(crate) line_number: Option<u64>,
    pub(crate) score: f32,
    pub(crate) text: String,
    pub(crate) metadata_json: serde_json::Value,
    pub(crate) task_id: Option<String>,
}

/// Local SQLite vector store for RD repository and task chunks.
pub struct RdEmbeddingStore {
    conn: Mutex<Connection>,
}

impl RdEmbeddingStore {
    pub(crate) fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create RD embedding dir {}", parent.display())
            })?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open RD embedding store {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS rd_embedding_chunks (
                tenant_id TEXT NOT NULL,
                repository_id TEXT NOT NULL,
                model TEXT NOT NULL,
                chunk_id TEXT NOT NULL,
                chunk_type TEXT NOT NULL,
                file_path TEXT,
                symbol_name TEXT,
                line_number INTEGER,
                content_hash TEXT NOT NULL,
                text TEXT NOT NULL,
                metadata_json TEXT,
                vector BLOB NOT NULL,
                dimensions INTEGER NOT NULL,
                task_id TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (tenant_id, repository_id, model, chunk_id)
            );
            CREATE INDEX IF NOT EXISTS idx_rd_embedding_chunks_repo_model
                ON rd_embedding_chunks(tenant_id, repository_id, model, chunk_type);
            CREATE INDEX IF NOT EXISTS idx_rd_embedding_chunks_task
                ON rd_embedding_chunks(tenant_id, task_id);
            CREATE INDEX IF NOT EXISTS idx_rd_embedding_chunks_content
                ON rd_embedding_chunks(tenant_id, repository_id, model, content_hash);
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn repository_chunk_hashes(
        &self,
        tenant_id: &str,
        repository_id: &str,
        model: &str,
    ) -> anyhow::Result<HashMap<String, String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT chunk_id, content_hash FROM rd_embedding_chunks
             WHERE tenant_id = ?1 AND repository_id = ?2 AND model = ?3
               AND chunk_type IN ('context_summary', 'file_summary', 'symbol', 'import')",
        )?;
        let rows = stmt.query_map(params![tenant_id, repository_id, model], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut hashes = HashMap::new();
        for row in rows {
            let (chunk_id, content_hash) = row?;
            hashes.insert(chunk_id, content_hash);
        }
        Ok(hashes)
    }

    pub(crate) fn prune_repository_index(
        &self,
        tenant_id: &str,
        repository_id: &str,
        model: &str,
        keep_chunk_ids: &HashSet<String>,
    ) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let existing = {
            let mut stmt = tx.prepare(
                "SELECT chunk_id FROM rd_embedding_chunks
                 WHERE tenant_id = ?1 AND repository_id = ?2 AND model = ?3
                   AND chunk_type IN ('context_summary', 'file_summary', 'symbol', 'import')",
            )?;
            let rows = stmt.query_map(params![tenant_id, repository_id, model], |row| {
                row.get::<_, String>(0)
            })?;
            let mut existing = Vec::new();
            for row in rows {
                existing.push(row?);
            }
            existing
        };
        let mut deleted = 0usize;
        {
            let mut delete_stmt = tx.prepare(
                "DELETE FROM rd_embedding_chunks
                 WHERE tenant_id = ?1 AND repository_id = ?2 AND model = ?3 AND chunk_id = ?4",
            )?;
            for chunk_id in existing {
                if !keep_chunk_ids.contains(&chunk_id) {
                    deleted = deleted.saturating_add(delete_stmt.execute(params![
                        tenant_id,
                        repository_id,
                        model,
                        chunk_id
                    ])?);
                }
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    pub(crate) fn upsert_chunks(
        &self,
        tenant_id: &str,
        repository_id: &str,
        model: &str,
        chunks: &[RdEmbeddingChunkUpsert],
    ) -> anyhow::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO rd_embedding_chunks
                    (tenant_id, repository_id, model, chunk_id, chunk_type, file_path, symbol_name,
                     line_number, content_hash, text, metadata_json, vector, dimensions, task_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(tenant_id, repository_id, model, chunk_id) DO UPDATE SET
                    chunk_type = excluded.chunk_type,
                    file_path = excluded.file_path,
                    symbol_name = excluded.symbol_name,
                    line_number = excluded.line_number,
                    content_hash = excluded.content_hash,
                    text = excluded.text,
                    metadata_json = excluded.metadata_json,
                    vector = excluded.vector,
                    dimensions = excluded.dimensions,
                    task_id = excluded.task_id,
                    updated_at = excluded.updated_at",
            )?;
            let now = current_unix_secs();
            for chunk in chunks {
                let mut vector = chunk.vector.clone();
                normalize_vector(&mut vector);
                let dimensions = i64::try_from(vector.len()).unwrap_or(i64::MAX);
                if dimensions <= 0 {
                    continue;
                }
                let line_number = chunk
                    .line_number
                    .and_then(|value| i64::try_from(value).ok());
                let metadata_json = serde_json::to_string(&chunk.metadata_json)
                    .unwrap_or_else(|_| "{}".to_string());
                stmt.execute(params![
                    tenant_id,
                    repository_id,
                    model,
                    chunk.chunk_id.as_str(),
                    chunk.chunk_type.as_str(),
                    chunk.file_path.as_deref(),
                    chunk.symbol_name.as_deref(),
                    line_number,
                    chunk.content_hash.as_str(),
                    chunk.text.as_str(),
                    metadata_json,
                    f32_slice_to_le_bytes(&vector),
                    dimensions,
                    chunk.task_id.as_deref(),
                    now,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn search_repository(
        &self,
        tenant_id: &str,
        repository_id: &str,
        model: &str,
        query_vector: &[f32],
        limit: usize,
    ) -> anyhow::Result<Vec<RdEmbeddingSearchHit>> {
        if query_vector.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut query = query_vector.to_vec();
        normalize_vector(&mut query);

        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT chunk_id, chunk_type, file_path, symbol_name, line_number, text,
                    metadata_json, vector, dimensions, task_id
             FROM rd_embedding_chunks
             WHERE tenant_id = ?1 AND repository_id = ?2 AND model = ?3
             ORDER BY updated_at DESC
             LIMIT ?4",
        )?;
        let max_rows = i64::try_from(MAX_SEARCH_ROWS_PER_REPOSITORY).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![tenant_id, repository_id, model, max_rows], |row| {
            let chunk_type_raw: String = row.get(1)?;
            let metadata_raw: Option<String> = row.get(6)?;
            let vector_blob: Vec<u8> = row.get(7)?;
            let dimensions_i64: i64 = row.get(8)?;
            Ok((
                row.get::<_, String>(0)?,
                RdEmbeddingChunkType::from_str(&chunk_type_raw),
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
                metadata_raw,
                vector_blob,
                usize::try_from(dimensions_i64.max(0)).unwrap_or(0),
                row.get::<_, Option<String>>(9)?,
            ))
        })?;

        let mut hits = Vec::new();
        for row in rows {
            let (
                chunk_id,
                chunk_type,
                file_path,
                symbol_name,
                line_number,
                text,
                metadata_raw,
                vector_blob,
                dimensions,
                task_id,
            ) = row?;
            if dimensions == 0 || dimensions != query.len() || vector_blob.len() != dimensions * 4 {
                continue;
            }
            let vector = le_bytes_to_vec(&vector_blob, dimensions);
            let score = dot_product(&query, &vector);
            if score < MIN_SEMANTIC_SCORE {
                continue;
            }
            let metadata_json = metadata_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            hits.push(RdEmbeddingSearchHit {
                chunk_id,
                chunk_type,
                file_path,
                symbol_name,
                line_number: line_number.and_then(|value| u64::try_from(value).ok()),
                score,
                text,
                metadata_json,
                task_id,
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| score_type_rank(a.chunk_type).cmp(&score_type_rank(b.chunk_type)))
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    pub(crate) fn repository_chunk_count(
        &self,
        tenant_id: &str,
        repository_id: &str,
        model: &str,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rd_embedding_chunks
             WHERE tenant_id = ?1 AND repository_id = ?2 AND model = ?3
               AND chunk_type IN ('context_summary', 'file_summary', 'symbol', 'import')",
            params![tenant_id, repository_id, model],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count.max(0)).unwrap_or(0))
    }

    pub(crate) fn chunk_counts_by_type(
        &self,
        tenant_id: &str,
        repository_id: Option<&str>,
        model: Option<&str>,
    ) -> anyhow::Result<HashMap<String, usize>> {
        let conn = self.conn.lock();
        let mut sql = String::from(
            "SELECT chunk_type, COUNT(*) FROM rd_embedding_chunks WHERE tenant_id = ?1",
        );
        if repository_id.is_some() {
            sql.push_str(" AND repository_id = ?2");
        }
        if model.is_some() {
            sql.push_str(if repository_id.is_some() {
                " AND model = ?3"
            } else {
                " AND model = ?2"
            });
        }
        sql.push_str(" GROUP BY chunk_type");

        let mut args = Vec::new();
        args.push(tenant_id.to_string());
        if let Some(repository_id) = repository_id {
            args.push(repository_id.to_string());
        }
        if let Some(model) = model {
            args.push(model.to_string());
        }

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = HashMap::new();
        for row in rows {
            let (chunk_type, count) = row?;
            counts.insert(chunk_type, usize::try_from(count.max(0)).unwrap_or(0));
        }
        Ok(counts)
    }
}

pub(crate) fn repository_chunk_id(
    chunk_type: RdEmbeddingChunkType,
    file_path: &str,
    discriminator: &str,
) -> String {
    format!(
        "{}:{}",
        chunk_type.as_str(),
        hash_text(&format!("{file_path}\n{discriminator}"))
    )
}

pub(crate) fn task_chunk_id(task_id: &str) -> String {
    format!("task:{}", hash_text(task_id))
}

pub(crate) fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

fn score_type_rank(chunk_type: RdEmbeddingChunkType) -> usize {
    match chunk_type {
        RdEmbeddingChunkType::ContextSummary => 0,
        RdEmbeddingChunkType::FileSummary => 1,
        RdEmbeddingChunkType::Symbol => 2,
        RdEmbeddingChunkType::Task => 3,
        RdEmbeddingChunkType::Import => 4,
    }
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return;
    }
    for value in vector.iter_mut() {
        *value /= norm;
    }
}

fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter()
        .zip(b.iter())
        .map(|(left, right)| left * right)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

fn f32_slice_to_le_bytes(slice: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(slice.len() * 4);
    for value in slice {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn le_bytes_to_vec(bytes: &[u8], dims: usize) -> Vec<f32> {
    let mut vec = Vec::with_capacity(dims);
    for i in 0..dims {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes[i * 4..(i + 1) * 4]);
        vec.push(f32::from_le_bytes(buf));
    }
    vec
}

fn current_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_search_returns_best_match() {
        let dir =
            std::env::temp_dir().join(format!("aos-rd-embedding-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let store = RdEmbeddingStore::open(&dir.join("rd.db")).expect("open store");
        store
            .upsert_chunks(
                "tenant",
                "repo",
                "embed-model",
                &[
                    RdEmbeddingChunkUpsert {
                        chunk_id: "a".to_string(),
                        chunk_type: RdEmbeddingChunkType::FileSummary,
                        file_path: Some("src/auth.rs".to_string()),
                        symbol_name: None,
                        line_number: None,
                        content_hash: "h1".to_string(),
                        text: "auth login token".to_string(),
                        metadata_json: serde_json::json!({}),
                        vector: vec![1.0, 0.0],
                        task_id: None,
                    },
                    RdEmbeddingChunkUpsert {
                        chunk_id: "b".to_string(),
                        chunk_type: RdEmbeddingChunkType::FileSummary,
                        file_path: Some("src/pay.rs".to_string()),
                        symbol_name: None,
                        line_number: None,
                        content_hash: "h2".to_string(),
                        text: "payment callback".to_string(),
                        metadata_json: serde_json::json!({}),
                        vector: vec![0.0, 1.0],
                        task_id: None,
                    },
                ],
            )
            .expect("upsert");
        let hits = store
            .search_repository("tenant", "repo", "embed-model", &[0.9, 0.1], 2)
            .expect("search");
        assert_eq!(hits[0].file_path.as_deref(), Some("src/auth.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
