//! SQLite-backed embedding store with LRU cache and cosine similarity search.
//!
//! Supports dynamic embedding dimensions (e.g. 1536 for text-embedding-3-small,
//! 2560 for text-embedding-3-large, 3072 for text-embedding-5-large).

use api::embeddings_endpoint;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Neighbour;
use hnsw_rs::hnswio::HnswIo;
use hnsw_rs::prelude::DistCosine;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const CACHE_MAX_ENTRIES: usize = 50_000;

/// Registry of physically isolated per-tenant, per-profile vector stores.
///
/// Each profile owns its SQLite file and ANN sidecars. This makes it impossible
/// for API and local vectors (including same-dimensional incompatible models)
/// to enter the same index.
pub struct EmbeddingStoreRegistry {
    root_dir: PathBuf,
    stores: Mutex<HashMap<String, Arc<EmbeddingStore>>>,
}

impl EmbeddingStoreRegistry {
    pub fn open(root_dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&root_dir)?;
        Ok(Self {
            root_dir,
            stores: Mutex::new(HashMap::new()),
        })
    }

    fn registry_key(tenant_id: &str, profile_id: &str) -> anyhow::Result<String> {
        if tenant_id.trim().is_empty() {
            anyhow::bail!("embedding tenant id must not be empty");
        }
        if !profile_id.starts_with("ep_")
            || profile_id.len() != 67
            || !profile_id[3..].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            anyhow::bail!("invalid embedding profile id");
        }
        Ok(format!("{tenant_id}\0{profile_id}"))
    }

    fn profile_db_path(&self, tenant_id: &str, profile_id: &str) -> PathBuf {
        let tenant_hash = hex::encode(Sha256::digest(tenant_id.as_bytes()));
        self.root_dir
            .join(tenant_hash)
            .join(profile_id)
            .join("embeddings.db")
    }

    pub fn profile_store(
        &self,
        tenant_id: &str,
        profile_id: &str,
        model: &str,
        embed_url: Option<String>,
    ) -> anyhow::Result<Arc<EmbeddingStore>> {
        let key = Self::registry_key(tenant_id, profile_id)?;
        if let Some(store) = self.stores.lock().get(&key).cloned() {
            return Ok(store);
        }

        let mut stores = self.stores.lock();
        if let Some(store) = stores.get(&key).cloned() {
            return Ok(store);
        }
        let store = Arc::new(EmbeddingStore::open(
            &self.profile_db_path(tenant_id, profile_id),
            Some(model),
            embed_url,
        )?);
        store.warm_load_ann_from_disk_at_startup();
        stores.insert(key, Arc::clone(&store));
        Ok(store)
    }

    pub fn persist_ann_snapshots_if_dirty(&self) -> anyhow::Result<usize> {
        let stores: Vec<_> = self.stores.lock().values().cloned().collect();
        let mut persisted = 0;
        for store in stores {
            if store.persist_ann_snapshot_if_dirty()? {
                persisted += 1;
            }
        }
        Ok(persisted)
    }

    pub fn loaded_profile_count(&self) -> usize {
        self.stores.lock().len()
    }

    pub fn len(&self) -> usize {
        self.stores.lock().values().map(|store| store.len()).sum()
    }

    pub fn ann_runtime_health(&self) -> serde_json::Value {
        let stores: Vec<_> = self
            .stores
            .lock()
            .iter()
            .map(|(key, store)| (key.clone(), Arc::clone(store)))
            .collect();
        let profile_health: Vec<_> = stores
            .into_iter()
            .map(|(key, store)| {
                let profile_id = key.rsplit_once('\0').map_or(key.as_str(), |(_, id)| id);
                (
                    profile_id.to_string(),
                    store.ann_runtime_health(),
                    store.len(),
                )
            })
            .collect();
        let loaded_in_memory = profile_health
            .iter()
            .any(|(_, health, _)| health.loaded_in_memory);
        let reasons = profile_health
            .iter()
            .filter_map(|(_, health, _)| health.reason.as_deref())
            .collect::<Vec<_>>();
        let profiles = profile_health
            .iter()
            .map(|(profile_id, health, vectors)| {
                serde_json::json!({
                    "profileId": profile_id,
                    "health": health,
                    "vectors": vectors,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "state": if loaded_in_memory { "loaded" } else { "unavailable" },
            "reason": if loaded_in_memory || reasons.is_empty() { None } else { Some(reasons.join("; ")) },
            "loaded_in_memory": loaded_in_memory,
            "base_points": profile_health.iter().map(|(_, health, _)| health.base_points).sum::<usize>(),
            "overlay_points": profile_health.iter().map(|(_, health, _)| health.overlay_points).sum::<usize>(),
            "stale_points": profile_health.iter().map(|(_, health, _)| health.stale_points).sum::<usize>(),
            "disk_artifacts_present": profile_health.iter().any(|(_, health, _)| health.disk_artifacts_present),
            "snapshot_pending": profile_health.iter().any(|(_, health, _)| health.snapshot_pending),
            "loadedProfiles": profiles.len(),
            "profiles": profiles,
        })
    }

    pub fn delete_datasource(&self, tenant_id: &str, datasource_id: &str) -> anyhow::Result<()> {
        let prefix = format!("{tenant_id}\0");
        let stores: Vec<_> = self
            .stores
            .lock()
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, store)| Arc::clone(store))
            .collect();
        for store in stores {
            store.delete_datasource(datasource_id)?;
        }
        Ok(())
    }
}

pub fn configure_local_embedding_cache_for_data_dir(
    data_dir: &std::path::Path,
) -> anyhow::Result<()> {
    runtime::local_embedding::configure_cache_for_data_dir(data_dir)
}

pub fn configure_local_embedding_cache_dir(cache_dir: PathBuf) -> anyhow::Result<()> {
    runtime::local_embedding::configure_cache_dir(cache_dir)
}

pub fn warm_local_embedding_model() -> anyhow::Result<()> {
    runtime::local_embedding::warm()
}

pub fn shutdown_local_embedding_model() {
    runtime::local_embedding::shutdown();
}

fn embed_with_local_model(texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
    runtime::local_embedding::embed(texts)
}

fn embed_with_local_model_background(texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
    runtime::local_embedding::embed_background(texts)
}

#[derive(Clone)]
struct CacheEntry {
    vector: Arc<Vec<f32>>,
}

struct LruCache {
    entries: std::collections::HashMap<String, CacheEntry>,
    access_order: VecDeque<String>,
}

impl LruCache {
    fn new() -> Self {
        Self {
            entries: std::collections::HashMap::with_capacity(CACHE_MAX_ENTRIES / 2),
            access_order: VecDeque::with_capacity(CACHE_MAX_ENTRIES / 2),
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<Vec<f32>>> {
        if let Some(entry) = self.entries.get(key) {
            self.access_order.retain(|k| k != key);
            self.access_order.push_back(key.to_owned());
            Some(Arc::clone(&entry.vector))
        } else {
            None
        }
    }

    fn insert(&mut self, key: String, vector: Arc<Vec<f32>>) {
        if self.entries.len() >= CACHE_MAX_ENTRIES {
            if let Some(lru_key) = self.access_order.pop_front() {
                self.entries.remove(&lru_key);
            }
        }
        self.access_order.push_back(key.clone());
        self.entries.insert(key, CacheEntry { vector });
    }

    fn invalidate(&mut self, key: &str) {
        self.entries.remove(key);
        self.access_order.retain(|k| k != key);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.access_order.clear();
    }
}

/// SQLite-backed embedding store with LRU in-memory cache.
pub struct EmbeddingStore {
    conn: Mutex<Connection>,
    cache: Mutex<LruCache>,
    /// The embedding model name (e.g. "text-embedding-3-small").
    model: std::sync::OnceLock<String>,
    /// The embedding base URL (e.g. "https://api.openai.com").
    embed_url: std::sync::OnceLock<Option<String>>,
    /// ANN runtime loaded at startup from disk.
    ///
    /// Important contract:
    /// - Query paths must never load or rebuild ANN.
    /// - ANN is startup-warm-loaded once.
    /// - Runtime updates are applied incrementally via overlay/tombstones.
    ann_runtime: Mutex<Option<AnnRuntime>>,
    ann_status: Mutex<AnnStatus>,
    /// True when column embedding changes need to be snapshotted to ANN disk artifacts.
    ann_dirty: AtomicBool,
    /// Monotonic revision of ANN-affecting mutations (col upsert/delete/clear).
    ann_revision: AtomicU64,
    /// Prevents concurrent snapshot attempts.
    ann_snapshot_in_progress: AtomicBool,
    /// Path prefix for the persisted ANN index files (hnsw.graph, hnsw.data).
    ann_index_path: PathBuf,
}

/// Metadata for the ANN index: maps hnsw_rs numeric keys back to column identifiers.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct AnnMetadata {
    /// Maps a numeric ANN key to (datasource_id, table_name, column_name).
    keys: std::collections::HashMap<usize, (String, String, String)>,
    /// Dimension of vectors stored in the ANN index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ColumnKey {
    datasource_id: String,
    table_name: String,
    column_name: String,
}

impl ColumnKey {
    fn new(datasource_id: &str, table_name: &str, column_name: &str) -> Self {
        Self {
            datasource_id: datasource_id.to_string(),
            table_name: table_name.to_string(),
            column_name: column_name.to_string(),
        }
    }
}

struct AnnRuntime {
    index: Arc<Mutex<hnsw_rs::hnsw::Hnsw<'static, f32, DistCosine>>>,
    meta: AnnMetadata,
    base_keys: HashSet<ColumnKey>,
    stale_keys: HashSet<ColumnKey>,
    overlay: HashMap<ColumnKey, Arc<Vec<f32>>>,
    dimensions: usize,
}

#[derive(Clone, Debug)]
enum AnnStatus {
    Loaded,
    Unavailable(String),
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AnnRuntimeHealth {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub loaded_in_memory: bool,
    pub base_points: usize,
    pub overlay_points: usize,
    pub stale_points: usize,
    pub disk_artifacts_present: bool,
    pub snapshot_pending: bool,
}

impl EmbeddingStore {
    pub fn open(
        db_path: &PathBuf,
        model: Option<&str>,
        embed_url: Option<String>,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;

        // Keep startup safe but not needlessly slow. Full `integrity_check`
        // scans more aggressively; `quick_check` is enough for normal startup.
        // Set NL2SQL_EMBEDDING_DB_CHECK=full for deep diagnostics or off to
        // skip the check in trusted/local workflows.
        let db_check = std::env::var("NL2SQL_EMBEDDING_DB_CHECK")
            .unwrap_or_else(|_| "quick".to_string())
            .to_ascii_lowercase();
        if db_check != "off" && db_check != "none" && db_check != "false" && db_check != "0" {
            let pragma = if db_check == "full" || db_check == "integrity" {
                "PRAGMA integrity_check"
            } else {
                "PRAGMA quick_check"
            };
            let mut stmt = conn.prepare(pragma)?;
            let rows: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .filter_map(std::result::Result::ok)
                .collect();
            let integrity_ok = rows.len() == 1 && rows[0].eq_ignore_ascii_case("ok");
            if !integrity_ok {
                return Err(anyhow::anyhow!(
                    "embedding store database check failed ({pragma}): {}. \
                     Consider deleting the database file and restarting the service to rebuild embeddings.",
                    rows.join("; ")
                ));
            }
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS embeddings (
                datasource_id TEXT NOT NULL,
                table_name TEXT NOT NULL,
                column_name TEXT NOT NULL,
                embed_type TEXT NOT NULL DEFAULT 'col',
                model TEXT NOT NULL DEFAULT 'text-embedding-3-small',
                vector BLOB NOT NULL,
                dimensions INTEGER NOT NULL DEFAULT 1536,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (datasource_id, table_name, column_name, embed_type)
            );
            CREATE INDEX IF NOT EXISTS idx_ds ON embeddings(datasource_id);
            ",
        )?;

        // Rebuild legacy schemas that predate `embed_type`.
        //
        // SQLite can't ALTER a PRIMARY KEY in place, so a table created
        // before this column existed will have PK `(ds, table, column)`.
        // Our upsert relies on `ON CONFLICT(ds, table, column, embed_type)`
        // which requires the 4-column PK, so we detect the legacy shape
        // and copy the data into a fresh table.
        let has_embed_type_in_pk = {
            let mut stmt = conn.prepare("PRAGMA table_info(embeddings)")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i32>(5)?)))?;
            let mut found = false;
            for row in rows {
                let (name, pk) = row?;
                if name == "embed_type" && pk > 0 {
                    found = true;
                    break;
                }
            }
            found
        };

        if !has_embed_type_in_pk {
            // Rebuild the table with the correct 4-column PK. Wrap in a
            // transaction so the rebuild is atomic; a crash mid-way leaves
            // the original table intact.
            conn.execute_batch(
                "BEGIN;
                CREATE TABLE embeddings_new (
                    datasource_id TEXT NOT NULL,
                    table_name TEXT NOT NULL,
                    column_name TEXT NOT NULL,
                    embed_type TEXT NOT NULL DEFAULT 'col',
                    model TEXT NOT NULL DEFAULT 'text-embedding-3-small',
                    vector BLOB NOT NULL,
                    dimensions INTEGER NOT NULL DEFAULT 1536,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (datasource_id, table_name, column_name, embed_type)
                );
                INSERT INTO embeddings_new (datasource_id, table_name, column_name, embed_type, model, vector, dimensions, updated_at)
                SELECT datasource_id, table_name, column_name,
                       COALESCE(NULLIF(embed_type, ''), 'col') AS embed_type,
                       model, vector, dimensions, updated_at
                FROM embeddings;
                DROP TABLE embeddings;
                ALTER TABLE embeddings_new RENAME TO embeddings;
                CREATE INDEX IF NOT EXISTS idx_ds ON embeddings(datasource_id);
                COMMIT;",
            )?;
        }

        let model_str = model.unwrap_or("text-embedding-3-small");
        let index_path = db_path.with_extension("ann");
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Mutex::new(LruCache::new()),
            model: std::sync::OnceLock::from(model_str.to_owned()),
            embed_url: std::sync::OnceLock::from(embed_url),
            ann_runtime: Mutex::new(None),
            ann_status: Mutex::new(AnnStatus::Unavailable(
                "ANN warm-load has not run yet".to_string(),
            )),
            ann_dirty: AtomicBool::new(false),
            ann_revision: AtomicU64::new(0),
            ann_snapshot_in_progress: AtomicBool::new(false),
            ann_index_path: index_path,
        })
    }

    fn mark_ann_dirty(&self) {
        self.ann_revision.fetch_add(1, Ordering::AcqRel);
        self.ann_dirty.store(true, Ordering::Release);
    }

    fn set_ann_status_loaded(&self) {
        *self.ann_status.lock() = AnnStatus::Loaded;
    }

    fn set_ann_status_unavailable(&self, reason: impl Into<String>) {
        *self.ann_status.lock() = AnnStatus::Unavailable(reason.into());
    }

    /// Warm-load ANN at startup from disk only.
    ///
    /// This method never rebuilds ANN in-process. If loading fails, ANN stays
    /// unavailable and query path falls back to brute-force.
    pub fn warm_load_ann_from_disk_at_startup(&self) {
        let enabled = std::env::var("NL2SQL_USE_ANN_INDEX")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(true);
        if !enabled {
            *self.ann_runtime.lock() = None;
            self.set_ann_status_unavailable("ANN disabled by NL2SQL_USE_ANN_INDEX=false");
            self.ann_dirty.store(false, Ordering::Release);
            return;
        }

        if !self.ann_files_exist() {
            *self.ann_runtime.lock() = None;
            if self.col_len() > 0 {
                self.set_ann_status_unavailable(
                    "ANN artifacts are missing; background snapshot build scheduled",
                );
                self.mark_ann_dirty();
            } else {
                self.set_ann_status_unavailable("no column vectors available for ANN");
                self.ann_dirty.store(false, Ordering::Release);
            }
            return;
        }

        let expected_dims = match self.stored_dimensions() {
            Some(d) if d > 0 => d as usize,
            _ => {
                *self.ann_runtime.lock() = None;
                self.set_ann_status_unavailable(
                    "no embeddings found in SQLite, cannot validate ANN dimensions",
                );
                self.ann_dirty.store(false, Ordering::Release);
                return;
            }
        };

        match self.load_ann_from_disk(expected_dims) {
            Ok((index, meta)) => {
                let index = Arc::new(Mutex::new(index));
                let base_keys: HashSet<ColumnKey> = meta
                    .keys
                    .values()
                    .map(|(ds, t, c)| ColumnKey::new(ds, t, c))
                    .collect();
                *self.ann_runtime.lock() = Some(AnnRuntime {
                    index,
                    meta,
                    base_keys,
                    stale_keys: HashSet::new(),
                    overlay: HashMap::new(),
                    dimensions: expected_dims,
                });
                self.set_ann_status_loaded();
                self.ann_dirty.store(false, Ordering::Release);
                tracing::info!(
                    points = self
                        .ann_runtime
                        .lock()
                        .as_ref()
                        .map(|rt| rt.index.lock().get_nb_point())
                        .unwrap_or(0),
                    dims = expected_dims,
                    "ANN warm-loaded from disk at startup"
                );
            }
            Err(e) => {
                *self.ann_runtime.lock() = None;
                self.set_ann_status_unavailable(format!(
                    "failed to load ANN from disk at startup: {}",
                    e
                ));
                self.ann_dirty.store(false, Ordering::Release);
                tracing::warn!(error = %e, "ANN startup warm-load failed; ANN remains unavailable");
            }
        }
    }

    pub fn ann_runtime_health(&self) -> AnnRuntimeHealth {
        let status = self.ann_status.lock().clone();
        let runtime = self.ann_runtime.lock();
        let (loaded_in_memory, base_points, overlay_points, stale_points) =
            if let Some(rt) = runtime.as_ref() {
                (
                    true,
                    rt.index.lock().get_nb_point(),
                    rt.overlay.len(),
                    rt.stale_keys.len(),
                )
            } else {
                (false, 0, 0, 0)
            };
        let (state, reason) = match status {
            AnnStatus::Loaded => ("loaded".to_string(), None),
            AnnStatus::Unavailable(reason) => ("unavailable".to_string(), Some(reason)),
        };
        AnnRuntimeHealth {
            state,
            reason,
            loaded_in_memory,
            base_points,
            overlay_points,
            stale_points,
            disk_artifacts_present: self.ann_files_exist(),
            snapshot_pending: self.ann_dirty.load(Ordering::Acquire),
        }
    }

    /// Persist an ANN snapshot to disk when there are pending mutations.
    ///
    /// This method is intended to be called by a background worker. It never runs
    /// in query path.
    pub fn persist_ann_snapshot_if_dirty(&self) -> anyhow::Result<bool> {
        if !self.ann_dirty.load(Ordering::Acquire) {
            return Ok(false);
        }
        if self
            .ann_snapshot_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(false);
        }

        let result = (|| -> anyhow::Result<bool> {
            let start_rev = self.ann_revision.load(Ordering::Acquire);
            let dimensions = match self.stored_dimensions() {
                Some(d) if d > 0 => d as usize,
                _ => {
                    self.remove_ann_sidecar_files();
                    let meta_path = ann_meta_path(&self.ann_index_path);
                    if meta_path.exists() {
                        let _ = std::fs::remove_file(&meta_path);
                    }
                    *self.ann_runtime.lock() = None;
                    self.set_ann_status_unavailable("no vectors available to snapshot");
                    self.ann_dirty.store(false, Ordering::Release);
                    return Ok(true);
                }
            };

            let (index, meta) = self.build_ann_index(dimensions)?;
            let index = Arc::new(Mutex::new(index));
            self.save_ann_to_disk(&index, &meta)?;

            let mut new_runtime = AnnRuntime {
                index,
                base_keys: meta
                    .keys
                    .values()
                    .map(|(ds, t, c)| ColumnKey::new(ds, t, c))
                    .collect(),
                meta,
                stale_keys: HashSet::new(),
                overlay: HashMap::new(),
                dimensions,
            };

            // Preserve updates that happened while snapshot building was in flight.
            if let Some(current_runtime) = self.ann_runtime.lock().as_ref() {
                for (k, v) in &current_runtime.overlay {
                    if v.len() == new_runtime.dimensions {
                        new_runtime.overlay.insert(k.clone(), Arc::clone(v));
                    }
                }
                for k in &current_runtime.stale_keys {
                    if new_runtime.base_keys.contains(k) {
                        new_runtime.stale_keys.insert(k.clone());
                    }
                }
                for k in new_runtime.overlay.keys() {
                    if new_runtime.base_keys.contains(k) {
                        new_runtime.stale_keys.insert(k.clone());
                    }
                }
            }

            *self.ann_runtime.lock() = Some(new_runtime);
            self.set_ann_status_loaded();

            let end_rev = self.ann_revision.load(Ordering::Acquire);
            if start_rev == end_rev {
                self.ann_dirty.store(false, Ordering::Release);
            } else {
                // Keep dirty=true so next background cycle snapshots the newer revision.
                self.ann_dirty.store(true, Ordering::Release);
            }
            Ok(true)
        })();

        self.ann_snapshot_in_progress
            .store(false, Ordering::Release);
        result
    }

    /// Upsert a single embedding. Stores the vector regardless of dimension.
    pub fn upsert(
        &self,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
        vector: &[f32],
        model: &str,
    ) -> anyhow::Result<()> {
        self.upsert_typed(datasource_id, table_name, column_name, "col", vector, model)
    }

    /// Upsert an embedding with a specific embed type ('col', 'table', 'datasource').
    ///
    /// For column embeddings this updates ANN runtime incrementally in memory.
    /// It never triggers ANN rebuild/load in query path.
    pub fn upsert_typed(
        &self,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
        embed_type: &str,
        vector: &[f32],
        model: &str,
    ) -> anyhow::Result<()> {
        let dims = vector.len();
        let bytes = f32_slice_to_le_bytes(vector);
        let updated_at = current_unix_secs();

        self.conn.lock().execute(
            "INSERT INTO embeddings (datasource_id, table_name, column_name, embed_type, model, vector, dimensions, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(datasource_id, table_name, column_name, embed_type)
             DO UPDATE SET model = excluded.model, vector = excluded.vector,
                           dimensions = excluded.dimensions, updated_at = excluded.updated_at",
            params![datasource_id, table_name, column_name, embed_type, model, bytes, dims as i32, updated_at],
        )?;

        let key = cache_key(datasource_id, table_name, column_name, embed_type);
        self.cache.lock().insert(key, Arc::new(vector.to_vec()));

        if embed_type == "col" {
            self.mark_ann_dirty();
            self.apply_ann_overlay_upsert(datasource_id, table_name, column_name, vector);
        }

        Ok(())
    }

    fn apply_ann_overlay_upsert(
        &self,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
        vector: &[f32],
    ) {
        let key = ColumnKey::new(datasource_id, table_name, column_name);
        let mut runtime = self.ann_runtime.lock();
        let Some(current_dim) = runtime.as_ref().map(|rt| rt.dimensions) else {
            return;
        };

        if current_dim != vector.len() {
            *runtime = None;
            self.set_ann_status_unavailable(format!(
                "ANN runtime disabled after dimension mismatch: ann_dim={} upsert_dim={}",
                current_dim,
                vector.len()
            ));
            return;
        }
        let Some(rt) = runtime.as_mut() else {
            return;
        };
        if rt.base_keys.contains(&key) {
            rt.stale_keys.insert(key.clone());
        }
        rt.overlay.insert(key, Arc::new(vector.to_vec()));
    }

    /// Retrieve a single embedding. Returns `None` if not found.
    pub fn get(
        &self,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
    ) -> anyhow::Result<Option<Arc<Vec<f32>>>> {
        self.get_typed(datasource_id, table_name, column_name, "col")
    }

    /// Retrieve an embedding by type. Returns `None` if not found.
    pub fn get_typed(
        &self,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
        embed_type: &str,
    ) -> anyhow::Result<Option<Arc<Vec<f32>>>> {
        let key = cache_key(datasource_id, table_name, column_name, embed_type);

        if let Some(v) = self.cache.lock().get(&key) {
            return Ok(Some(v));
        }

        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT vector, dimensions, updated_at FROM embeddings
             WHERE datasource_id = ? AND table_name = ? AND column_name = ? AND embed_type = ?",
        )?;
        let mut rows = stmt.query(params![datasource_id, table_name, column_name, embed_type])?;

        if let Some(row) = rows.next()? {
            let blob: Vec<u8> = row.get(0)?;
            let dims: i32 = row.get(1)?;
            let expected_len = (dims as usize) * 4;
            if blob.len() == expected_len {
                let vector = le_bytes_to_vec(&blob, dims as usize);
                let key_owned = cache_key(datasource_id, table_name, column_name, embed_type);
                self.cache
                    .lock()
                    .insert(key_owned, Arc::new(vector.clone()));
                return Ok(Some(Arc::new(vector)));
            }
        }
        Ok(None)
    }

    /// Invalidate all in-memory cached vectors and the ANN index.
    /// Call this when the embedding model or dimensions change to prevent
    /// stale-vector poisoning in search results.
    pub fn invalidate_all(&self) {
        self.cache.lock().clear();
        *self.ann_runtime.lock() = None;
        self.set_ann_status_unavailable("ANN runtime invalidated in memory");
        tracing::info!("embedding store: invalidated all in-memory cache and ANN runtime");
    }

    /// Delete all embeddings and reset the store to a clean state.
    /// Used when the embedding model changes globally.
    pub fn clear_all(&self) -> anyhow::Result<()> {
        self.invalidate_all();
        self.conn.lock().execute("DELETE FROM embeddings", [])?;
        self.remove_ann_sidecar_files();
        let meta_path = self.ann_index_path.with_extension("ann.meta");
        if meta_path.exists() {
            std::fs::remove_file(&meta_path).ok();
        }
        self.ann_dirty.store(false, Ordering::Release);
        tracing::info!("embedding store: cleared all embeddings");
        Ok(())
    }

    /// Invalidate the in-memory cache and ANN index, then delete all embeddings for a datasource.
    /// Used during schema refresh to keep the local embedding store consistent with metadata tables.
    pub fn delete_datasource(&self, datasource_id: &str) -> anyhow::Result<()> {
        {
            let mut cache = self.cache.lock();
            let prefix = format!("{datasource_id}\x00");
            let keys: Vec<_> = cache
                .entries
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            for key in keys {
                cache.invalidate(&key);
            }
        }
        self.conn.lock().execute(
            "DELETE FROM embeddings WHERE datasource_id = ?",
            params![datasource_id],
        )?;
        self.mark_ann_dirty();
        // Incrementally hide removed datasource points from loaded ANN runtime.
        {
            let mut runtime = self.ann_runtime.lock();
            if let Some(rt) = runtime.as_mut() {
                let remove_ds = datasource_id.to_string();
                rt.overlay.retain(|k, _| k.datasource_id != remove_ds);
                rt.stale_keys.extend(
                    rt.base_keys
                        .iter()
                        .filter(|k| k.datasource_id == remove_ds)
                        .cloned(),
                );
            }
        }
        Ok(())
    }

    /// Atomically replace every schema embedding for one datasource after a
    /// complete shadow index has already been generated and validated.
    pub fn replace_datasource_embeddings(
        &self,
        datasource_id: &str,
        rows: &[(String, String, String, Vec<f32>, String)],
    ) -> anyhow::Result<()> {
        {
            let mut conn = self.conn.lock();
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM embeddings WHERE datasource_id = ?",
                params![datasource_id],
            )?;
            {
                let mut statement = tx.prepare(
                    "INSERT INTO embeddings \
                     (datasource_id, table_name, column_name, embed_type, model, vector, dimensions, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?;
                let updated_at = current_unix_secs();
                for (table_name, column_name, embed_type, vector, model) in rows {
                    statement.execute(params![
                        datasource_id,
                        table_name,
                        column_name,
                        embed_type,
                        model,
                        f32_slice_to_le_bytes(vector),
                        i32::try_from(vector.len()).unwrap_or(i32::MAX),
                        updated_at,
                    ])?;
                }
            }
            tx.commit()?;
        }

        let prefix = format!("{datasource_id}\x00");
        {
            let mut cache = self.cache.lock();
            let stale_keys: Vec<_> = cache
                .entries
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .cloned()
                .collect();
            for key in stale_keys {
                cache.invalidate(&key);
            }
            for (table_name, column_name, embed_type, vector, _) in rows {
                cache.insert(
                    cache_key(datasource_id, table_name, column_name, embed_type),
                    Arc::new(vector.clone()),
                );
            }
        }
        self.mark_ann_dirty();
        *self.ann_runtime.lock() = None;
        self.set_ann_status_unavailable("ANN snapshot pending after atomic profile replacement");
        Ok(())
    }

    /// Returns the total number of embedding rows stored across all datasources.
    pub fn len(&self) -> usize {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .unwrap_or(0);
        usize::try_from(count).unwrap_or(0)
    }

    /// Returns the number of column embeddings (`embed_type='col'`) across all datasources.
    pub fn col_len(&self) -> usize {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM embeddings WHERE embed_type = 'col'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        usize::try_from(count).unwrap_or(0)
    }

    /// Returns the number of column embeddings (`embed_type='col'`) for the given datasources.
    pub fn col_len_for_datasources(&self, datasource_ids: &[String]) -> usize {
        if datasource_ids.is_empty() {
            return 0;
        }
        let placeholders = datasource_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM embeddings \
             WHERE embed_type = 'col' AND datasource_id IN ({})",
            placeholders
        );
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let params = rusqlite::params_from_iter(datasource_ids.iter().map(|s| s.as_str()));
        let count: i64 = stmt.query_row(params, |r| r.get(0)).unwrap_or(0);
        usize::try_from(count).unwrap_or(0)
    }

    /// Returns true if the store contains no embeddings.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the stored embedding dimension from the most recent embedding row,
    /// or `None` if no embeddings exist yet.
    pub fn stored_dimensions(&self) -> Option<u32> {
        let conn = self.conn.lock();
        conn.query_row("SELECT dimensions FROM embeddings LIMIT 1", [], |r| {
            r.get::<_, i32>(0).map(|d| d as u32)
        })
        .ok()
    }

    /// Returns the embedding model name.
    pub fn embed_model(&self) -> String {
        self.model
            .get()
            .cloned()
            .unwrap_or_else(|| "text-embedding-3-small".to_owned())
    }

    /// Returns the embedding base URL.
    pub fn embed_url(&self) -> Option<String> {
        self.embed_url.get().cloned().flatten()
    }

    /// Load all embeddings for a datasource from cache + SQLite.
    pub fn get_for_datasource(
        &self,
        datasource_id: &str,
    ) -> anyhow::Result<Vec<(String, String, Arc<Vec<f32>>)>> {
        let mut results = Vec::new();
        let prefix = format!("{datasource_id}\x00");

        {
            let cache = self.cache.lock();
            for (key, entry) in cache.entries.iter() {
                if key.starts_with(&prefix) {
                    let parts: Vec<_> = key.split('\x00').collect();
                    if parts.len() >= 4 {
                        results.push((
                            parts[1].to_owned(),
                            parts[2].to_owned(),
                            Arc::clone(&entry.vector),
                        ));
                    }
                }
            }
        }

        {
            let conn = self.conn.lock();
            let mut stmt = conn.prepare(
                "SELECT table_name, column_name, vector, dimensions FROM embeddings
                 WHERE datasource_id = ? AND embed_type = 'col'",
            )?;
            let mut rows = stmt.query(params![datasource_id])?;
            while let Some(row) = rows.next()? {
                let table_name: String = row.get(0)?;
                let column_name: String = row.get(1)?;
                let blob: Vec<u8> = row.get(2)?;
                let dims: i32 = row.get(3)?;
                let expected_len = (dims as usize) * 4;
                if blob.len() == expected_len {
                    let vector = le_bytes_to_vec(&blob, dims as usize);
                    let key = cache_key(datasource_id, &table_name, &column_name, "col");
                    self.cache.lock().insert(key, Arc::new(vector.clone()));
                    results.push((table_name, column_name, Arc::new(vector)));
                }
            }
        }

        Ok(results)
    }

    /// Return the `(table, column, embed_type)` tuples that have an embedding
    /// stored for the given datasource. Used by the semantics API to flag
    /// rows as "indexed" without re-running the embedding query.
    pub fn indexed_keys(
        &self,
        datasource_id: &str,
    ) -> anyhow::Result<std::collections::HashSet<(String, String, String)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT table_name, column_name, embed_type \
             FROM embeddings WHERE datasource_id = ?",
        )?;
        let mut rows = stmt.query(params![datasource_id])?;
        let mut out = std::collections::HashSet::new();
        while let Some(row) = rows.next()? {
            out.insert((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ));
        }
        Ok(out)
    }

    /// Search across ALL datasources for the Top-K most relevant tables.
    ///
    /// Strategy:
    /// 1. Load all column embeddings from SQLite (cache-first, then DB).
    /// 2. Compute cosine similarity for each column against `question_vec`.
    /// 3. Per-table: keep the best column's similarity score.
    /// 4. Sort globally across ALL datasources, return Top-K.
    ///
    /// NOTE: This is O(n_columns) full scan. For very large schemas (>50K columns)
    /// this should be replaced with ANN index (see P2-1). The `use_ann_index` flag
    /// gates this behavior.
    /// Returns one `GlobalTableMatch` per table (deduplicated by datasource+table).
    /// Synchronous — caller must provide the pre-computed question embedding.
    pub fn global_table_search(
        &self,
        question_vec: &[f32],
        top_k: usize,
        use_ann_index: bool,
        allowed_datasources: Option<&std::collections::HashSet<String>>,
    ) -> anyhow::Result<Vec<GlobalTableMatch>> {
        // If ANN is enabled, delegate to in-memory runtime only.
        // Query path never loads/rebuilds ANN.
        if use_ann_index {
            if let Some(ann_result) =
                self.global_table_search_ann(question_vec, top_k, allowed_datasources)?
            {
                return Ok(ann_result);
            }
            // Fall through to brute-force when ANN runtime is unavailable.
        }

        // O(n) scan: load all column embeddings from SQLite.
        // We scan the full table rather than per-datasource to avoid
        // N round-trips for N datasources.
        // Optionally filter to only allowed datasources (from embedding pre-filter).
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT datasource_id, table_name, column_name, vector, dimensions \
             FROM embeddings WHERE embed_type = 'col'",
        )?;
        let mut rows = stmt.query([])?;

        let mut table_best: std::collections::HashMap<(String, String), (f32, String)> =
            std::collections::HashMap::new();
        let mut table_embed_sims: std::collections::HashMap<(String, String), f32> =
            std::collections::HashMap::new();
        let mut datasource_embed_sims: std::collections::HashMap<String, f32> =
            std::collections::HashMap::new();
        let mut total_col_vectors_scanned: usize = 0;
        let mut skipped_by_datasource_prefilter: usize = 0;
        let mut skipped_by_vector_shape: usize = 0;
        let mut skipped_by_dimension_mismatch: usize = 0;

        while let Some(row) = rows.next()? {
            let datasource_id: String = row.get(0)?;
            total_col_vectors_scanned += 1;
            // Skip rows from datasources that failed the embedding pre-filter.
            if let Some(allowed) = allowed_datasources {
                if !allowed.contains(&datasource_id) {
                    skipped_by_datasource_prefilter += 1;
                    continue;
                }
            }
            let table_name: String = row.get(1)?;
            let column_name: String = row.get(2)?;
            let blob: Vec<u8> = row.get(3)?;
            let dims: i32 = row.get(4)?;
            let expected_len = (dims as usize) * 4;

            if blob.len() != expected_len {
                skipped_by_vector_shape += 1;
                continue;
            }
            if (dims as usize) != question_vec.len() {
                skipped_by_dimension_mismatch += 1;
                continue;
            }
            let col_vec = le_bytes_to_vec(&blob, dims as usize);
            let sim = cosine_similarity(question_vec, &col_vec);

            let entry = table_best
                .entry((datasource_id.clone(), table_name.clone()))
                .or_insert_with(|| (sim, column_name.clone()));

            if sim > entry.0 {
                *entry = (sim, column_name);
            }
        }

        // Load table-level embeddings and compute similarity per table.
        let mut total_table_vectors_scanned: usize = 0;
        let mut skipped_table_by_datasource_prefilter: usize = 0;
        let mut skipped_table_by_vector_shape: usize = 0;
        let mut skipped_table_by_dimension_mismatch: usize = 0;
        let mut table_stmt = conn.prepare(
            "SELECT datasource_id, table_name, vector, dimensions \
             FROM embeddings WHERE embed_type = 'table'",
        )?;
        let mut table_rows = table_stmt.query([])?;
        while let Some(row) = table_rows.next()? {
            let datasource_id: String = row.get(0)?;
            total_table_vectors_scanned += 1;
            if let Some(allowed) = allowed_datasources {
                if !allowed.contains(&datasource_id) {
                    skipped_table_by_datasource_prefilter += 1;
                    continue;
                }
            }
            let table_name: String = row.get(1)?;
            let blob: Vec<u8> = row.get(2)?;
            let dims: i32 = row.get(3)?;
            let expected_len = (dims as usize) * 4;
            if blob.len() != expected_len {
                skipped_table_by_vector_shape += 1;
                continue;
            }
            if (dims as usize) != question_vec.len() {
                skipped_table_by_dimension_mismatch += 1;
                continue;
            }
            let table_vec = le_bytes_to_vec(&blob, dims as usize);
            let sim = cosine_similarity(question_vec, &table_vec);
            table_embed_sims.insert((datasource_id, table_name), sim);
        }

        // Load datasource-level embeddings and compute similarity.
        let mut total_datasource_vectors_scanned: usize = 0;
        let mut skipped_datasource_by_datasource_prefilter: usize = 0;
        let mut skipped_datasource_by_vector_shape: usize = 0;
        let mut skipped_datasource_by_dimension_mismatch: usize = 0;
        let mut ds_stmt = conn.prepare(
            "SELECT datasource_id, vector, dimensions \
             FROM embeddings WHERE embed_type = 'datasource'",
        )?;
        let mut ds_rows = ds_stmt.query([])?;
        while let Some(row) = ds_rows.next()? {
            let datasource_id: String = row.get(0)?;
            total_datasource_vectors_scanned += 1;
            if let Some(allowed) = allowed_datasources {
                if !allowed.contains(&datasource_id) {
                    skipped_datasource_by_datasource_prefilter += 1;
                    continue;
                }
            }
            let blob: Vec<u8> = row.get(1)?;
            let dims: i32 = row.get(2)?;
            let expected_len = (dims as usize) * 4;
            if blob.len() != expected_len {
                skipped_datasource_by_vector_shape += 1;
                continue;
            }
            if (dims as usize) != question_vec.len() {
                skipped_datasource_by_dimension_mismatch += 1;
                continue;
            }
            let ds_vec = le_bytes_to_vec(&blob, dims as usize);
            let sim = cosine_similarity(question_vec, &ds_vec);
            datasource_embed_sims.insert(datasource_id, sim);
        }

        // Sort globally by best column similarity, then filter by minimum threshold.
        let threshold = crate::nl2sql::min_table_sim_threshold();
        let mut sorted: Vec<_> = table_best.into_iter().collect();
        sorted.sort_by(|a, b| {
            b.1 .0
                .partial_cmp(&a.1 .0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Filter out tables whose best-column similarity falls below the minimum threshold.
        // This prevents low-relevance tables from being presented to the LLM reranker.
        let tables_before_threshold = sorted.len();
        let top: Vec<GlobalTableMatch> = sorted
            .into_iter()
            .filter_map(|((datasource_id, table_name), (col_sim, best_column))| {
                let table_sim = table_embed_sims
                    .get(&(datasource_id.clone(), table_name.clone()))
                    .copied()
                    .unwrap_or(col_sim);
                let ds_sim = datasource_embed_sims
                    .get(&datasource_id)
                    .copied()
                    .unwrap_or(-1.0);
                // Keep compatibility with previous threshold semantics by checking
                // the primary (column) similarity threshold first.
                if col_sim < threshold {
                    return None;
                }
                // Blend in table/datasource similarities so tables with strong table-level
                // semantics can rank higher even when column names are weakly lexical.
                let fused = (ds_sim + 1.0) * 0.125_f32
                    + (table_sim + 1.0) * 0.175_f32
                    + (col_sim + 1.0) * 0.200_f32;
                Some(GlobalTableMatch {
                    datasource_id,
                    table_name,
                    best_column,
                    column_sim: col_sim,
                    table_sim,
                    candidate_score: fused.clamp(0.0, 1.0),
                    datasource_desc: String::new(),
                    column_description: String::new(),
                })
            })
            .collect::<Vec<_>>();
        let mut top = top;
        top.sort_by(|a, b| {
            b.candidate_score
                .partial_cmp(&a.candidate_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        top.truncate(top_k);

        if top.is_empty() {
            tracing::warn!(
                threshold,
                top_k,
                question_dim = question_vec.len(),
                total_col_vectors_scanned,
                skipped_by_datasource_prefilter,
                skipped_by_vector_shape,
                skipped_by_dimension_mismatch,
                total_table_vectors_scanned,
                skipped_table_by_datasource_prefilter,
                skipped_table_by_vector_shape,
                skipped_table_by_dimension_mismatch,
                total_datasource_vectors_scanned,
                skipped_datasource_by_datasource_prefilter,
                skipped_datasource_by_vector_shape,
                skipped_datasource_by_dimension_mismatch,
                tables_before_threshold,
                allowed_datasource_count = allowed_datasources.map(|s| s.len()),
                "global_table_search returned no candidate tables"
            );
        } else {
            tracing::debug!(
                top_count = top.len(),
                top_candidate_score = top.first().map(|item| item.candidate_score),
                "global_table_search produced candidates"
            );
        }

        Ok(top)
    }

    /// Async wrapper for RRFS: embeds the question then searches.
    /// Embeds once and delegates to the sync global_table_search.
    pub async fn global_table_search_top_k(
        &self,
        question: &str,
        embed_api_key: Option<String>,
        use_ann_index: bool,
        allowed_datasources: Option<&std::collections::HashSet<String>>,
    ) -> anyhow::Result<Vec<GlobalTableMatch>> {
        let model = EmbeddingModel::new(&self.embed_model(), self.embed_url(), embed_api_key);
        let vecs = model.embed_batch(&[question.to_owned()]).await?;
        let question_vec = vecs.into_iter().next().unwrap_or_default();
        self.global_table_search(
            &question_vec,
            crate::nl2sql::top_k_tables_for_llm(),
            use_ann_index,
            allowed_datasources,
        )
    }

    /// Search using ANN runtime that was warm-loaded at startup.
    ///
    /// This method never loads or rebuilds ANN in the request path.
    fn global_table_search_ann(
        &self,
        question_vec: &[f32],
        top_k: usize,
        allowed_datasources: Option<&std::collections::HashSet<String>>,
    ) -> anyhow::Result<Option<Vec<GlobalTableMatch>>> {
        let runtime_guard = self.ann_runtime.lock();
        let Some(rt) = runtime_guard.as_ref() else {
            return Ok(None);
        };
        if rt.dimensions != question_vec.len() {
            tracing::warn!(
                ann_dim = rt.dimensions,
                query_dim = question_vec.len(),
                "ANN runtime dimension mismatch, falling back to brute-force"
            );
            return Ok(None);
        }

        let opt_results = self.search_ann_index(
            &rt.index,
            &rt.meta,
            &rt.stale_keys,
            &rt.overlay,
            question_vec,
            top_k,
        )?;
        if let Some(mut results) = opt_results {
            // Filter ANN results by allowed datasources (from embedding pre-filter).
            if let Some(allowed) = allowed_datasources {
                results.retain(|m| allowed.contains(&m.datasource_id));
            }
            Ok(Some(results))
        } else {
            Ok(None)
        }
    }

    /// Delete vectors from SQLite whose stored dimensions do not match `expected_dims`.
    /// Called when the embedding model changes to prevent stale vectors from polluting
    /// ANN index search results and cosine similarity scores.
    ///
    /// Returns the number of stale vectors removed.
    pub fn cleanup_stale_vectors(&self, expected_dims: usize) -> anyhow::Result<usize> {
        let conn = self.conn.lock();
        let expected_bytes = expected_dims * 4;
        // Delete rows where blob size doesn't match expected dimensions (stale from old model)
        // or where the explicit dimensions column differs.
        let stale = conn.execute(
            "DELETE FROM embeddings \
             WHERE dimensions != ?1 OR LENGTH(vector) != ?2",
            rusqlite::params![expected_dims as i32, expected_bytes],
        )?;
        if stale > 0 {
            self.mark_ann_dirty();
        }
        tracing::info!(
            expected_dims = expected_dims,
            stale_vectors = stale,
            "cleanup_stale_vectors",
        );
        Ok(stale)
    }

    /// Build the ANN index from all column embeddings currently in SQLite.
    fn build_ann_index(
        &self,
        dimensions: usize,
    ) -> anyhow::Result<(hnsw_rs::hnsw::Hnsw<'static, f32, DistCosine>, AnnMetadata)> {
        // Count rows first so we can pre-allocate.
        let count: i64 = self.conn.lock()
            .query_row(
                "SELECT COUNT(*) FROM embeddings WHERE embed_type = 'col' AND dimensions = ? AND LENGTH(vector) = ?",
                rusqlite::params![dimensions as i32, dimensions * 4],
                |r| r.get(0),
            )
            .unwrap_or(0);

        // If no vectors exist, return an empty index rather than building.
        if count == 0 {
            let empty = hnsw_rs::hnsw::Hnsw::new(16, 1, 16, 200, DistCosine {});
            return Ok((
                empty,
                AnnMetadata {
                    keys: std::collections::HashMap::new(),
                    dimensions: Some(dimensions),
                },
            ));
        }

        // HNSW parameters: max_nb_connection=16, ef_construction=200, max_layer=16.
        // These are tuned for moderate-size embedding tables (typically <100K columns).
        let hnsw = hnsw_rs::hnsw::Hnsw::new(
            16,             // max_nb_connection
            count as usize, // max_elements (pre-allocation hint)
            16,             // max_layer
            200,            // ef_construction
            DistCosine {},  // distance function
        );

        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT datasource_id, table_name, column_name, vector, dimensions \
             FROM embeddings WHERE embed_type = 'col'",
        )?;
        let mut rows = stmt.query([])?;

        let mut meta_keys: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        let mut count = 0usize;

        while let Some(row) = rows.next()? {
            let datasource_id: String = row.get(0)?;
            let table_name: String = row.get(1)?;
            let column_name: String = row.get(2)?;
            let blob: Vec<u8> = row.get(3)?;
            let dims: i32 = row.get(4)?;

            if (dims as usize) != dimensions || blob.len() != dimensions * 4 {
                continue;
            }

            let vec = le_bytes_to_vec(&blob, dimensions);
            let key = count;

            hnsw.insert((&vec, key));

            meta_keys.insert(key, (datasource_id, table_name, column_name));
            count += 1;
        }

        tracing::info!(
            vectors = count,
            "built ANN index from {} column vectors",
            count
        );
        Ok((
            hnsw,
            AnnMetadata {
                keys: meta_keys,
                dimensions: Some(dimensions),
            },
        ))
    }

    /// Perform ANN search and aggregate results per table.
    fn search_ann_index(
        &self,
        index: &Arc<Mutex<hnsw_rs::hnsw::Hnsw<'static, f32, DistCosine>>>,
        meta: &AnnMetadata,
        stale_keys: &HashSet<ColumnKey>,
        overlay: &HashMap<ColumnKey, Arc<Vec<f32>>>,
        question_vec: &[f32],
        top_k: usize,
    ) -> anyhow::Result<Option<Vec<GlobalTableMatch>>> {
        // Enable searching mode (required after bulk insertion).
        index.lock().set_searching_mode(true);

        // ef_arg controls search breadth; higher = better recall, slower search.
        // Use a wider candidate pool to survive stale-key filtering.
        let candidate_k = (top_k * 8).max(64);
        let ef_arg = (candidate_k * 2).max(32);

        let neighbours: Vec<Neighbour> = index.lock().search(question_vec, candidate_k, ef_arg);
        let neighbours_count = neighbours.len();

        let mut table_best: std::collections::HashMap<(String, String), (f32, String)> =
            std::collections::HashMap::new();

        for neighbour in neighbours {
            let key = neighbour.d_id;
            let distance = neighbour.distance;

            let (datasource_id, table_name, column_name) = match meta.keys.get(&key) {
                Some(v) => v,
                None => continue,
            };
            let col_key = ColumnKey::new(datasource_id, table_name, column_name);
            if stale_keys.contains(&col_key) {
                continue;
            }

            // DistCosine returns [0, 2]: 0 = identical, 2 = opposite.
            // Convert to similarity score in [-1, 1]: sim = 1 - distance.
            let sim = (1.0_f32 - distance).clamp(-1.0, 1.0);

            let entry = table_best
                .entry((datasource_id.clone(), table_name.clone()))
                .or_insert_with(|| (sim, column_name.clone()));

            if sim > entry.0 {
                *entry = (sim, column_name.clone());
            }
        }

        // Merge overlay vectors that were updated after startup warm-load.
        for (col_key, vec) in overlay {
            if vec.len() != question_vec.len() {
                continue;
            }
            let sim = cosine_similarity(question_vec, vec);
            let entry = table_best
                .entry((col_key.datasource_id.clone(), col_key.table_name.clone()))
                .or_insert_with(|| (sim, col_key.column_name.clone()));
            if sim > entry.0 {
                *entry = (sim, col_key.column_name.clone());
            }
        }

        let threshold = crate::nl2sql::min_table_sim_threshold();
        let mut sorted: Vec<_> = table_best.into_iter().collect();
        sorted.sort_by(|a, b| {
            b.1 .0
                .partial_cmp(&a.1 .0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let tables_before_threshold = sorted.len();
        let matches: Vec<GlobalTableMatch> = sorted
            .into_iter()
            .filter(|(_, (sim, _))| *sim >= threshold)
            .take(top_k)
            .map(
                |((datasource_id, table_name), (col_sim, best_column))| GlobalTableMatch {
                    datasource_id,
                    table_name,
                    best_column,
                    column_sim: col_sim,
                    table_sim: col_sim,
                    candidate_score: col_sim,
                    datasource_desc: String::new(),
                    column_description: String::new(),
                },
            )
            .collect();

        if matches.is_empty() {
            tracing::info!(
                threshold,
                top_k,
                candidate_k,
                ef_arg,
                neighbours_count,
                question_dim = question_vec.len(),
                ann_meta_keys = meta.keys.len(),
                overlay_points = overlay.len(),
                stale_points = stale_keys.len(),
                tables_before_threshold,
                "ANN global_table_search returned no candidate tables"
            );
        }

        Ok(Some(matches))
    }

    /// Load column descriptions for a set of datasources from nl2sql_table_semantics.
    /// Returns a map from (table_name, column_name) -> combined description (AI + user).
    pub async fn get_column_descriptions_for_datasources(
        &self,
        db: &sqlx::SqlitePool,
        datasource_ids: &[String],
    ) -> std::collections::HashMap<(String, String), String> {
        if datasource_ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let placeholders: Vec<String> = datasource_ids.iter().map(|_| "?".to_string()).collect();
        let query = format!(
            "SELECT table_name, column_name, COALESCE(semantic_description, '') \
             FROM nl2sql_table_semantics WHERE datasource_id IN ({}) AND deleted_at IS NULL",
            placeholders.join(", ")
        );
        let mut q = sqlx::query_as::<_, (String, String, String)>(&query);
        for ds_id in datasource_ids {
            q = q.bind(ds_id);
        }
        let rows = q.fetch_all(db).await;
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "get_column_descriptions_for_datasources: query failed: {}",
                    e
                );
                return std::collections::HashMap::new();
            }
        };
        let mut map = std::collections::HashMap::new();
        for (table, col, desc) in rows {
            if !desc.is_empty() {
                map.insert((table, col), desc);
            }
        }
        map
    }
}

/// Path for the ANN metadata sidecar file.
fn ann_meta_path(ann_path: &std::path::Path) -> std::path::PathBuf {
    ann_path.with_extension("ann.meta")
}

impl EmbeddingStore {
    /// Persist the HNSW index and metadata to disk.
    fn save_ann_to_disk(
        &self,
        hnsw: &Arc<Mutex<hnsw_rs::hnsw::Hnsw<'static, f32, DistCosine>>>,
        meta: &AnnMetadata,
    ) -> anyhow::Result<()> {
        let dir = self
            .ann_index_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let basename = self
            .ann_index_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("embeddings");

        let hnsw = hnsw.lock();
        hnsw.file_dump(dir, basename)
            .map_err(|e| anyhow::anyhow!("hnsw_rs file_dump failed: {}", e))?;

        let meta_path = ann_meta_path(&self.ann_index_path);
        let file = std::fs::File::create(&meta_path)?;
        let writer = std::io::BufWriter::new(file);
        serde_json::to_writer(writer, meta)
            .map_err(|e| anyhow::anyhow!("failed to serialize ANN metadata: {}", e))?;

        tracing::info!(path = %self.ann_index_path.display(), "persisted ANN index to disk");
        Ok(())
    }

    /// Load the HNSW index and metadata from disk.
    ///
    fn load_ann_from_disk(
        &self,
        dimensions: usize,
    ) -> anyhow::Result<(hnsw_rs::hnsw::Hnsw<'static, f32, DistCosine>, AnnMetadata)> {
        let dir = self
            .ann_index_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let basename = self
            .ann_index_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("embeddings");
        let meta_path = ann_meta_path(&self.ann_index_path);

        let meta_file = std::fs::File::open(&meta_path)?;
        let meta_reader = std::io::BufReader::new(meta_file);
        let meta: AnnMetadata = serde_json::from_reader(meta_reader)
            .map_err(|e| anyhow::anyhow!("failed to parse ANN metadata: {}", e))?;

        let leaked_reloader: &'static mut HnswIo = Box::leak(Box::new(HnswIo::new(dir, basename)));
        let hnsw: hnsw_rs::hnsw::Hnsw<'static, f32, DistCosine> = leaked_reloader
            .load_hnsw::<f32, DistCosine>()
            .map_err(|e| anyhow::anyhow!("failed to load ANN hnsw from disk: {}", e))?;

        if let Some(loaded_dims) = meta.dimensions {
            if hnsw.get_nb_point() > 0 && loaded_dims != dimensions {
                return Err(anyhow::anyhow!(
                    "ANN dimensions mismatch: loaded={} expected={}",
                    loaded_dims,
                    dimensions
                ));
            }
        }

        Ok((hnsw, meta))
    }

    fn ann_files_exist(&self) -> bool {
        let dir = self
            .ann_index_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let basename = self
            .ann_index_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("embeddings");
        let graph_path = dir.join(format!("{}.hnsw.graph", basename));
        let data_path = dir.join(format!("{}.hnsw.data", basename));
        let meta_path = ann_meta_path(&self.ann_index_path);
        graph_path.exists() && data_path.exists() && meta_path.exists()
    }

    /// Remove ANN index sidecar files.
    fn remove_ann_sidecar_files(&self) {
        // hnsw_rs creates {basename}.hnsw.graph and {basename}.hnsw.data.
        let dir = self
            .ann_index_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let basename = self
            .ann_index_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("embeddings");
        for ext in &["graph", "data"] {
            let path = dir.join(format!("{}.hnsw.{}", basename, ext));
            if path.exists() {
                std::fs::remove_file(&path).ok();
            }
        }
        // Also remove the legacy single-file index if it exists.
        if self.ann_index_path.exists() {
            std::fs::remove_file(&self.ann_index_path).ok();
        }
    }
}

/// Result of a global table search across all datasources.
#[derive(Debug, Clone)]
pub struct GlobalTableMatch {
    pub datasource_id: String,
    pub table_name: String,
    /// The best-matching column within this table.
    pub best_column: String,
    /// Raw cosine similarity of the best column [-1, 1].
    pub column_sim: f32,
    /// Table-level similarity (from table embedding when available, else best column sim).
    pub table_sim: f32,
    /// Blended score used for coarse candidate ranking.
    pub candidate_score: f32,
    /// Datasource description (populated by the routing layer from the DB).
    pub datasource_desc: String,
    /// Column description (populated from nl2sql_table_semantics by the route handler).
    pub column_description: String,
}

#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    // Mismatched dimensions almost always mean the two vectors came from
    // different embedding models (e.g. text-embedding-3-small @ 1536 vs
    // text-embedding-3-large @ 3072). Silently truncating to the shorter
    // length would return a plausible-looking but meaningless score;
    // returning 0.0 forces the caller to filter these rows out.
    if a.len() != b.len() {
        tracing::warn!(
            a_dim = a.len(),
            b_dim = b.len(),
            "cosine_similarity: dimension mismatch; likely a stale vector from a previous embedding model — returning 0.0"
        );
        return 0.0;
    }
    let mut dot = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
    }
    dot.clamp(-1.0, 1.0)
}

fn f32_slice_to_le_bytes(slice: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(slice.len() * 4);
    for &v in slice {
        bytes.extend_from_slice(&v.to_le_bytes());
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

fn cache_key(datasource_id: &str, table_name: &str, column_name: &str, embed_type: &str) -> String {
    format!("{datasource_id}\x00{table_name}\x00{column_name}\x00{embed_type}")
}

fn current_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── Embedding model ─────────────────────────────────────────────────────────

/// Generate text embeddings using the built-in ONNX model or an optional
/// tenant-scoped OpenAI-compatible API endpoint.
pub struct EmbeddingModel {
    pub model: String,
    embed_url: Option<String>,
    api_key: Option<String>,
    dimensions: Option<usize>,
    request_timeout: Duration,
}

impl EmbeddingModel {
    pub fn new(model: &str, embed_url: Option<String>, api_key: Option<String>) -> Self {
        Self::new_with_dimensions(model, embed_url, api_key, None)
    }

    pub fn new_with_dimensions(
        model: &str,
        embed_url: Option<String>,
        api_key: Option<String>,
        dimensions: Option<usize>,
    ) -> Self {
        Self {
            model: model.to_owned(),
            embed_url,
            api_key,
            dimensions,
            request_timeout: Duration::from_secs(30),
        }
    }

    #[cfg(test)]
    fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let (vectors, _) = self.embed_batch_with_usage(texts).await?;
        Ok(vectors)
    }

    pub async fn embed_batch_with_usage(
        &self,
        texts: &[String],
    ) -> anyhow::Result<(Vec<Vec<f32>>, Option<api::Usage>)> {
        self.embed_batch_with_usage_priority(texts, false).await
    }

    pub async fn embed_batch_with_usage_background(
        &self,
        texts: &[String],
    ) -> anyhow::Result<(Vec<Vec<f32>>, Option<api::Usage>)> {
        self.embed_batch_with_usage_priority(texts, true).await
    }

    async fn embed_batch_with_usage_priority(
        &self,
        texts: &[String],
        background: bool,
    ) -> anyhow::Result<(Vec<Vec<f32>>, Option<api::Usage>)> {
        if texts.is_empty() {
            return Ok((Vec::new(), None));
        }

        let remote_api_key = self
            .api_key
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                super::tenant_embedding_env_fallback_enabled()
                    .then(|| std::env::var("OPENAI_API_KEY").ok())
                    .flatten()
            })
            .or_else(|| {
                super::tenant_embedding_env_fallback_enabled()
                    .then(|| std::env::var("ANTHROPIC_API_KEY").ok())
                    .flatten()
            });
        if self.model == super::LOCAL_EMBEDDING_MODEL || remote_api_key.is_none() {
            let owned_texts = texts.to_vec();
            let vectors = tokio::task::spawn_blocking(move || {
                if background {
                    embed_with_local_model_background(owned_texts)
                } else {
                    embed_with_local_model(owned_texts)
                }
            })
            .await
            .map_err(|error| anyhow::anyhow!("local embedding worker failed: {error}"))??;
            validate_embedding_output(&vectors, texts.len(), self.dimensions, "local")?;
            return Ok((vectors, None));
        }

        let mut protected_texts = Vec::with_capacity(texts.len());
        let mut protection = runtime::DataProtectionReport::default();
        for text in texts {
            let protected =
                runtime::protect_sensitive_text(text, runtime::configured_data_protection_mode());
            protected_texts.push(protected.value);
            protection.merge(&protected.report);
        }
        if protection.redacted {
            tracing::warn!(
                model = %self.model,
                finding_count = protection.finding_count,
                categories = ?protection.categories,
                "embedding outbound data protection redacted sensitive values"
            );
        }
        let mut body = serde_json::json!({ "model": self.model, "input": protected_texts });
        if let Some(dimensions) = self.dimensions {
            body["dimensions"] = serde_json::json!(dimensions);
        }
        let base = self
            .embed_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com".to_owned());

        let api_key = remote_api_key.expect("remote embedding key checked above");

        let url = embeddings_endpoint(&base);

        tracing::debug!(
            model = %self.model,
            url = %url,
            texts_count = texts.len(),
            "embedding request"
        );

        let client = reqwest::Client::builder()
            .timeout(self.request_timeout)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client for embedding: {}", e))?;

        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("embedding API request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("failed to read embedding response body: {}", e))?;
        if !status.is_success() {
            anyhow::bail!("embedding API error {}: {text}", status);
        }

        #[derive(Debug, Deserialize)]
        struct EmbeddingApiItem {
            embedding: Vec<f32>,
        }
        #[derive(Debug, Deserialize)]
        struct EmbeddingApiUsage {
            #[serde(default)]
            prompt_tokens: Option<u64>,
            #[serde(default)]
            input_tokens: Option<u64>,
            #[serde(default)]
            total_tokens: Option<u64>,
        }
        #[derive(Debug, Deserialize)]
        struct EmbeddingApiResponse {
            data: Vec<EmbeddingApiItem>,
            #[serde(default)]
            usage: Option<EmbeddingApiUsage>,
        }

        let parsed: EmbeddingApiResponse = serde_json::from_str(&text)?;

        let mut results = Vec::with_capacity(parsed.data.len());
        for item in parsed.data {
            let vec = item.embedding;
            tracing::debug!(
                model = %self.model,
                dims = vec.len(),
                "embedding response"
            );
            results.push(vec);
        }
        validate_embedding_output(&results, texts.len(), self.dimensions, "API")?;

        let usage = parsed.usage.and_then(|u| {
            let input_u64 = u.input_tokens.or(u.prompt_tokens).or(u.total_tokens)?;
            let total_u64 = u.total_tokens.unwrap_or(input_u64);
            let input_tokens = u32::try_from(input_u64).unwrap_or(u32::MAX);
            let total_tokens = u32::try_from(total_u64).unwrap_or(u32::MAX);
            Some(api::Usage {
                input_tokens,
                output_tokens: total_tokens.saturating_sub(input_tokens),
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            })
        });

        Ok((results, usage))
    }
}

fn validate_embedding_output(
    vectors: &[Vec<f32>],
    expected_count: usize,
    expected_dimensions: Option<usize>,
    label: &str,
) -> anyhow::Result<()> {
    if vectors.len() != expected_count {
        anyhow::bail!(
            "{label} embedding returned {} vectors for {expected_count} inputs",
            vectors.len()
        );
    }
    if let Some(expected_dimensions) = expected_dimensions {
        if let Some(vector) = vectors
            .iter()
            .find(|vector| vector.len() != expected_dimensions)
        {
            anyhow::bail!(
                "{label} embedding returned {} dimensions; expected {expected_dimensions}",
                vector.len()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{EmbeddingModel, EmbeddingStore, EmbeddingStoreRegistry};
    use api::embeddings_endpoint;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn mock_embedding_server(
        status: u16,
        response_body: &str,
        response_delay: Duration,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock embedding server");
        let address = listener.local_addr().expect("read mock address");
        let response_body = response_body.to_string();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept mock request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.expect("read mock request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let Some(headers_end) = request.windows(4).position(|item| item == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let _ = request_tx.send(String::from_utf8_lossy(&request).into_owned());
            tokio::time::sleep(response_delay).await;
            let reason = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (format!("http://{address}/v1"), request_rx)
    }

    #[test]
    fn embeddings_endpoint_handles_all_variants() {
        // Full endpoint (no append needed)
        assert_eq!(
            embeddings_endpoint("https://api.openai.com/v1/embeddings"),
            "https://api.openai.com/v1/embeddings"
        );
        // /v1 base → append /embeddings
        assert_eq!(
            embeddings_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/embeddings"
        );
        // Bare base → append /embeddings
        assert_eq!(
            embeddings_endpoint("https://open.bigmodel.cn/api/paas"),
            "https://open.bigmodel.cn/api/paas/embeddings"
        );
        // /v4 base → append /embeddings
        assert_eq!(
            embeddings_endpoint("https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn/api/paas/v4/embeddings"
        );
        // Already ends with /embeddings
        assert_eq!(
            embeddings_endpoint("https://example.com/v1/embeddings"),
            "https://example.com/v1/embeddings"
        );
    }

    #[tokio::test]
    async fn remote_embedding_sends_dimensions_and_accepts_valid_batch() {
        let (base_url, request_rx) = mock_embedding_server(
            200,
            r#"{"data":[{"embedding":[1.0,0.0,0.5]}],"usage":{"prompt_tokens":7,"total_tokens":7}}"#,
            Duration::ZERO,
        )
        .await;
        let model = EmbeddingModel::new_with_dimensions(
            "embedding-test",
            Some(base_url),
            Some("test-secret".to_string()),
            Some(3),
        );
        let (vectors, usage) = model
            .embed_batch_with_usage(&["hello".to_string()])
            .await
            .expect("valid embedding response");
        assert_eq!(vectors, vec![vec![1.0, 0.0, 0.5]]);
        assert_eq!(usage.expect("usage").input_tokens, 7);
        let request = request_rx.await.expect("captured embedding request");
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-secret"));
        assert!(request.contains(r#""dimensions":3"#));
    }

    #[tokio::test]
    async fn remote_embedding_rejects_http_and_malformed_responses() {
        for (status, body, expected) in [
            (429, r#"{"error":"rate limited"}"#, "429"),
            (503, r#"{"error":"unavailable"}"#, "503"),
            (200, "not-json", "expected"),
        ] {
            let (base_url, _) = mock_embedding_server(status, body, Duration::ZERO).await;
            let model = EmbeddingModel::new_with_dimensions(
                "embedding-test",
                Some(base_url),
                Some("test-secret".to_string()),
                Some(3),
            );
            let error = model
                .embed_batch(&["hello".to_string()])
                .await
                .expect_err("invalid response must fail")
                .to_string();
            assert!(
                error.to_ascii_lowercase().contains(expected),
                "unexpected error for status {status}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn remote_embedding_rejects_incomplete_and_wrong_dimension_batches() {
        let cases = [
            (
                r#"{"data":[{"embedding":[1.0,0.0,0.5]}]}"#,
                vec!["one".to_string(), "two".to_string()],
                "1 vectors for 2 inputs",
            ),
            (
                r#"{"data":[{"embedding":[1.0,0.0]}]}"#,
                vec!["one".to_string()],
                "2 dimensions; expected 3",
            ),
        ];
        for (body, inputs, expected) in cases {
            let (base_url, _) = mock_embedding_server(200, body, Duration::ZERO).await;
            let model = EmbeddingModel::new_with_dimensions(
                "embedding-test",
                Some(base_url),
                Some("test-secret".to_string()),
                Some(3),
            );
            let error = model
                .embed_batch(&inputs)
                .await
                .expect_err("invalid vector shape must fail")
                .to_string();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[tokio::test]
    async fn remote_embedding_timeout_is_reported() {
        let (base_url, _) = mock_embedding_server(
            200,
            r#"{"data":[{"embedding":[1.0,0.0,0.5]}]}"#,
            Duration::from_millis(200),
        )
        .await;
        let model = EmbeddingModel::new_with_dimensions(
            "embedding-test",
            Some(base_url),
            Some("test-secret".to_string()),
            Some(3),
        )
        .with_request_timeout(Duration::from_millis(25));
        let error = model
            .embed_batch(&["hello".to_string()])
            .await
            .expect_err("slow response must time out")
            .to_string();
        assert!(error.contains("embedding API request failed"), "{error}");
    }

    #[test]
    fn ann_search_preserves_metadata_across_cache_hits() {
        let base =
            std::env::temp_dir().join(format!("aos-embedding-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).expect("create temp dir");
        let db_path = PathBuf::from(base.join("embeddings.db"));
        let store = EmbeddingStore::open(&db_path, Some("embedding-3"), None).expect("open store");
        let store = Arc::new(store);

        // Use 4-d vectors to keep the test tiny and deterministic.
        // Query vector is closest to business_order.id and should remain resolvable
        // even after ANN index cache is reused.
        let vec_a = vec![1.0_f32, 0.0, 0.0, 0.0];
        let vec_b = vec![0.0_f32, 1.0, 0.0, 0.0];
        store
            .upsert_typed(
                "ds_test",
                "business_order",
                "id",
                "col",
                &vec_a,
                "embedding-3",
            )
            .expect("upsert vec_a");
        store
            .upsert_typed(
                "ds_test",
                "business_partner",
                "id",
                "col",
                &vec_b,
                "embedding-3",
            )
            .expect("upsert vec_b");

        let query = vec![1.0_f32, 0.0, 0.0, 0.0];

        // First ANN search builds index + metadata.
        let r1 = store
            .global_table_search(&query, 5, true, None)
            .expect("first search");
        assert!(
            r1.iter().any(|m| m.table_name == "business_order"),
            "first ANN search should include business_order"
        );

        // Second ANN search reuses cached ANN index; metadata must still be present.
        let r2 = store
            .global_table_search(&query, 5, true, None)
            .expect("second search");
        assert!(
            r2.iter().any(|m| m.table_name == "business_order"),
            "second ANN search should include business_order (metadata must not be dropped)"
        );
    }

    #[test]
    fn registry_physically_isolates_tenants_and_profiles() {
        let root = std::env::temp_dir().join(format!("aos-profile-test-{}", uuid::Uuid::new_v4()));
        let registry = EmbeddingStoreRegistry::open(root.clone()).expect("open registry");
        let profile_a = format!("ep_{}", "a".repeat(64));
        let profile_b = format!("ep_{}", "b".repeat(64));
        let store_a = registry
            .profile_store("tenant-a", &profile_a, "model-a", None)
            .expect("open profile a");
        let store_b = registry
            .profile_store("tenant-a", &profile_b, "model-b", None)
            .expect("open profile b");
        store_a
            .upsert_typed("ds", "orders", "id", "col", &[1.0, 0.0], "model-a")
            .expect("write profile a");

        assert!(store_a
            .get_typed("ds", "orders", "id", "col")
            .expect("read profile a")
            .is_some());
        assert!(store_b
            .get_typed("ds", "orders", "id", "col")
            .expect("read profile b")
            .is_none());
        assert_ne!(
            registry.profile_db_path("tenant-a", &profile_a),
            registry.profile_db_path("tenant-a", &profile_b)
        );
        drop(store_a);
        drop(store_b);
        drop(registry);
        std::fs::remove_dir_all(root).expect("remove profile test directory");
    }

    #[test]
    fn opening_legacy_vectors_without_ann_sidecars_schedules_and_builds_snapshot() {
        let root =
            std::env::temp_dir().join(format!("aos-ann-upgrade-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create ANN upgrade test directory");
        let db_path = root.join("embeddings.db");
        {
            let store = EmbeddingStore::open(&db_path, Some("model"), None).expect("open store");
            store
                .upsert_typed("ds", "orders", "id", "col", &[1.0, 0.0], "model")
                .expect("insert legacy vector");
        }

        let reopened = EmbeddingStore::open(&db_path, Some("model"), None).expect("reopen store");
        reopened.warm_load_ann_from_disk_at_startup();
        let pending = reopened.ann_runtime_health();
        assert!(pending.snapshot_pending);
        assert!(!pending.loaded_in_memory);

        assert!(reopened
            .persist_ann_snapshot_if_dirty()
            .expect("persist missing ANN snapshot"));
        let loaded = reopened.ann_runtime_health();
        assert!(loaded.loaded_in_memory);
        assert!(loaded.disk_artifacts_present);
        assert!(!loaded.snapshot_pending);
        drop(reopened);
        std::fs::remove_dir_all(root).expect("remove ANN upgrade test directory");
    }

    #[test]
    fn atomic_datasource_replacement_removes_stale_vectors() {
        let root = std::env::temp_dir().join(format!("aos-replace-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create replace test directory");
        let store = EmbeddingStore::open(&root.join("embeddings.db"), Some("model"), None)
            .expect("open store");
        store
            .upsert_typed("ds", "stale", "id", "col", &[1.0, 0.0], "model")
            .expect("insert stale vector");
        store
            .replace_datasource_embeddings(
                "ds",
                &[(
                    "current".to_string(),
                    "id".to_string(),
                    "col".to_string(),
                    vec![0.0, 1.0],
                    "model".to_string(),
                )],
            )
            .expect("replace datasource vectors");

        let keys = store.indexed_keys("ds").expect("read replaced keys");
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&("current".to_string(), "id".to_string(), "col".to_string())));
        drop(store);
        std::fs::remove_dir_all(root).expect("remove replace test directory");
    }
}
