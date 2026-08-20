use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};
use thiserror::Error;

use crate::MemoryEngine;

fn process_fault_point(name: &str) {
    #[cfg(debug_assertions)]
    if std::env::var("AOS_INTERNAL_PROCESS_TCK").as_deref() == Ok("1")
        && std::env::var("AOS_PROCESS_FAULT_POINT").as_deref() == Ok(name)
    {
        eprintln!("AOS_PROCESS_FAULT\t{name}\tpid={}", std::process::id());
        std::process::abort();
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactLifecycle {
    Candidate,
    Quarantined,
    Confirmed,
    Superseded,
    Forgotten,
    Rejected,
}

impl FactLifecycle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Quarantined => "quarantined",
            Self::Confirmed => "confirmed",
            Self::Superseded => "superseded",
            Self::Forgotten => "forgotten",
            Self::Rejected => "rejected",
        }
    }

    #[must_use]
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Candidate => "candidate_created",
            Self::Quarantined => "quarantined",
            Self::Confirmed => "confirmed",
            Self::Superseded => "superseded",
            Self::Forgotten => "forgotten",
            Self::Rejected => "rejected",
        }
    }

    #[must_use]
    pub const fn can_transition_to(self, target: Self) -> bool {
        match self {
            Self::Candidate => matches!(
                target,
                Self::Quarantined | Self::Confirmed | Self::Rejected | Self::Forgotten
            ),
            Self::Quarantined => matches!(
                target,
                Self::Candidate | Self::Confirmed | Self::Rejected | Self::Forgotten
            ),
            Self::Confirmed => matches!(target, Self::Superseded | Self::Forgotten),
            Self::Superseded | Self::Forgotten | Self::Rejected => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryFactDraft {
    pub fact_id: String,
    pub projection_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub scope: String,
    pub app: String,
    pub session_id: Option<String>,
    pub channel: String,
    pub kind: String,
    pub subject: serde_json::Value,
    pub predicate: String,
    pub value: serde_json::Value,
    pub text: String,
    pub evidence_id: String,
    pub evidence_hash: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub confidence: f64,
    pub sensitivity: String,
    pub lifecycle: FactLifecycle,
    pub authority: Vec<String>,
    pub source_event_ids: Vec<String>,
    pub pollution_lineage: Vec<String>,
    pub memory_type: String,
    pub source_type: String,
    pub pinned: bool,
    pub metadata: serde_json::Value,
    pub stale_at: Option<String>,
    pub verified_at: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_dimensions: Option<i32>,
    pub embedding_json: Option<String>,
}

#[derive(Debug, Error)]
pub enum MemoryRepositoryError {
    #[error("memory repository database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("memory repository rejected a candidate: {0}")]
    Admission(String),
    #[error("memory repository scope mismatch: {0}")]
    Scope(String),
    #[error("memory repository lifecycle conflict: {0}")]
    Lifecycle(String),
}

/// Backend-neutral durable Memory contract. Adapters must execute every
/// operation atomically and preserve the lifecycle/evidence rules enforced by
/// the production `SQLite` implementation. The trait intentionally exposes
/// domain commands rather than SQL or projection tables, so a future store
/// can run the same contract suite without importing `SQLite` helpers.
#[async_trait]
pub trait MemoryRepository: Send {
    async fn upsert(&mut self, draft: MemoryFactDraft) -> Result<(), MemoryRepositoryError>;

    async fn transition(
        &mut self,
        tenant_id: &str,
        user_id: &str,
        fact_id: &str,
        target: FactLifecycle,
        source_event_ids: &[String],
        independent_confirmation: bool,
    ) -> Result<(), MemoryRepositoryError>;

    async fn forget(
        &mut self,
        tenant_id: &str,
        user_id: &str,
        projection_id: &str,
        source_event_ids: &[String],
    ) -> Result<(), MemoryRepositoryError>;

    async fn rebuild_projection(
        &mut self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<String, MemoryRepositoryError>;
}

pub struct SqliteMemoryRepository;

/// Owned `SQLite` adapter for the public [`MemoryRepository`] contract. The
/// legacy transaction helpers remain private implementation details of this
/// adapter and are never called by application routes directly.
#[derive(Clone)]
pub struct SqliteMemoryRepositoryAdapter {
    db: sqlx::SqlitePool,
}

impl SqliteMemoryRepositoryAdapter {
    #[must_use]
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self { db }
    }
}

/// Transaction-scoped Memory command surface used when a larger kernel
/// transaction must commit Memory state together with another canonical
/// transition. Application crates never call the `SQLite` helpers directly.
pub struct SqliteMemoryTransaction;

#[derive(Debug, Clone, Copy)]
pub struct MemoryEmbeddingUpdate<'a> {
    pub tenant_id: &'a str,
    pub user_id: &'a str,
    pub fact_id: &'a str,
    pub projection_id: &'a str,
    pub source_hash: &'a str,
    pub embedding_model: &'a str,
    pub embedding: &'a [f32],
}

impl SqliteMemoryTransaction {
    pub async fn update_embedding_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        update: &MemoryEmbeddingUpdate<'_>,
    ) -> Result<bool, MemoryRepositoryError> {
        SqliteMemoryRepository::update_embedding_in_transaction(tx, update).await
    }

    pub async fn upsert_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        draft: &MemoryFactDraft,
    ) -> Result<(), MemoryRepositoryError> {
        crate::behavior_trace("MEM-004");
        SqliteMemoryRepository::upsert_in_transaction(tx, draft).await
    }

    pub async fn forget_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
        projection_id: &str,
        source_event_ids: &[String],
    ) -> Result<(), MemoryRepositoryError> {
        SqliteMemoryRepository::forget_in_transaction(
            tx,
            tenant_id,
            user_id,
            projection_id,
            source_event_ids,
        )
        .await
    }

    pub async fn transition_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
        fact_id: &str,
        target: FactLifecycle,
        source_event_ids: &[String],
        independent_confirmation: bool,
    ) -> Result<(), MemoryRepositoryError> {
        SqliteMemoryRepository::transition_in_transaction(
            tx,
            tenant_id,
            user_id,
            fact_id,
            target,
            source_event_ids,
            independent_confirmation,
        )
        .await
    }

    pub async fn rebuild_projection_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<String, MemoryRepositoryError> {
        SqliteMemoryRepository::rebuild_projection_in_transaction(tx, tenant_id, user_id).await
    }

    pub async fn supersede_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
        from_projection_id: &str,
        to_projection_id: &str,
        source_event_ids: &[String],
    ) -> Result<(), MemoryRepositoryError> {
        SqliteMemoryRepository::supersede_in_transaction(
            tx,
            tenant_id,
            user_id,
            from_projection_id,
            to_projection_id,
            source_event_ids,
        )
        .await
    }

    pub async fn erase_session_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        session_id: &str,
    ) -> Result<(), MemoryRepositoryError> {
        SqliteMemoryRepository::erase_session_in_transaction(tx, tenant_id, session_id).await
    }
}

#[async_trait]
impl MemoryRepository for SqliteMemoryRepositoryAdapter {
    async fn upsert(&mut self, draft: MemoryFactDraft) -> Result<(), MemoryRepositoryError> {
        let mut tx = self.db.begin().await?;
        SqliteMemoryRepository::upsert_in_transaction(&mut tx, &draft).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn transition(
        &mut self,
        tenant_id: &str,
        user_id: &str,
        fact_id: &str,
        target: FactLifecycle,
        source_event_ids: &[String],
        independent_confirmation: bool,
    ) -> Result<(), MemoryRepositoryError> {
        let mut tx = self.db.begin().await?;
        SqliteMemoryRepository::transition_in_transaction(
            &mut tx,
            tenant_id,
            user_id,
            fact_id,
            target,
            source_event_ids,
            independent_confirmation,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn forget(
        &mut self,
        tenant_id: &str,
        user_id: &str,
        projection_id: &str,
        source_event_ids: &[String],
    ) -> Result<(), MemoryRepositoryError> {
        let mut tx = self.db.begin().await?;
        SqliteMemoryRepository::forget_in_transaction(
            &mut tx,
            tenant_id,
            user_id,
            projection_id,
            source_event_ids,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn rebuild_projection(
        &mut self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<String, MemoryRepositoryError> {
        let mut tx = self.db.begin().await?;
        let hash =
            SqliteMemoryRepository::rebuild_projection_in_transaction(&mut tx, tenant_id, user_id)
                .await?;
        tx.commit().await?;
        Ok(hash)
    }
}

impl SqliteMemoryRepository {
    /// Update the searchable embedding projection only while it is still
    /// owned by the same confirmed canonical fact version. Keeping this CAS
    /// inside the Repository prevents an asynchronous embedding worker from
    /// reviving stale or superseded Memory state.
    pub(crate) async fn update_embedding_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        update: &MemoryEmbeddingUpdate<'_>,
    ) -> Result<bool, MemoryRepositoryError> {
        let embedding_json = serde_json::to_string(update.embedding)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        let dimensions = i64::try_from(update.embedding.len()).unwrap_or(i64::MAX);
        let updated = sqlx::query(
            "UPDATE agent_memory_items
             SET embedding_model = ?, embedding_dimensions = ?, embedding_json = ?,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND tenant_id = ? AND user_id = ?
               AND EXISTS (
                 SELECT 1 FROM structured_memory_facts AS fact
                 WHERE fact.id = ? AND fact.tenant_id = agent_memory_items.tenant_id
                   AND fact.user_id = agent_memory_items.user_id
                   AND fact.projection_memory_id = agent_memory_items.id
                   AND fact.evidence_hash = ? AND fact.lifecycle = 'confirmed'
               )",
        )
        .bind(update.embedding_model)
        .bind(dimensions)
        .bind(embedding_json)
        .bind(update.projection_id)
        .bind(update.tenant_id)
        .bind(update.user_id)
        .bind(update.fact_id)
        .bind(update.source_hash)
        .execute(&mut **tx)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    // The insert, immutable-ID check, lifecycle event and search projection are
    // intentionally linear so they remain auditable as one atomic transition.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn upsert_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        draft: &MemoryFactDraft,
    ) -> Result<(), MemoryRepositoryError> {
        MemoryEngine::admit_text(&draft.text)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        if draft.fact_id.is_empty()
            || draft.projection_id.is_empty()
            || draft.evidence_id.is_empty()
            || draft.tenant_id.is_empty()
            || draft.user_id.is_empty()
        {
            return Err(MemoryRepositoryError::Scope(
                "fact, projection, evidence, tenant and user are required".into(),
            ));
        }
        if !draft.pollution_lineage.is_empty() && draft.lifecycle == FactLifecycle::Confirmed {
            return Err(MemoryRepositoryError::Admission(
                "polluted evidence cannot enter Confirmed memory".into(),
            ));
        }
        let subject_json = draft.subject.to_string();
        let value_json = draft.value.to_string();
        let authority_json = serde_json::to_string(&draft.authority)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        let evidence_refs_json = serde_json::to_string(&[draft.evidence_id.as_str()])
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        let source_event_ids_json = serde_json::to_string(&draft.source_event_ids)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        let pollution_lineage_json = serde_json::to_string(&draft.pollution_lineage)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        let candidate_json = serde_json::to_string(draft)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        let conflict_group = stable_hash(&format!(
            "{}\0{}\0{}\0{}",
            draft.tenant_id, draft.user_id, subject_json, draft.predicate
        ));
        sqlx::query::<Sqlite>(
            "INSERT INTO structured_memory_facts
                (id, tenant_id, user_id, scope, app, session_id, channel, kind,
                 subject_json, predicate, value_json, text, evidence_id, evidence_hash,
                 observed_at, valid_from, valid_until, recorded_at, confidence,
                 sensitivity, lifecycle, current, conflict_group, projection_memory_id,
                 candidate_json, authority_json, evidence_refs_json,
                 source_event_ids_json, pollution_lineage_json, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP,
                     ?, ?, CURRENT_TIMESTAMP, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                     CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(&draft.fact_id)
        .bind(&draft.tenant_id)
        .bind(&draft.user_id)
        .bind(&draft.scope)
        .bind(&draft.app)
        .bind(&draft.session_id)
        .bind(&draft.channel)
        .bind(&draft.kind)
        .bind(&subject_json)
        .bind(&draft.predicate)
        .bind(&value_json)
        .bind(&draft.text)
        .bind(&draft.evidence_id)
        .bind(&draft.evidence_hash)
        .bind(&draft.valid_from)
        .bind(&draft.valid_until)
        .bind(draft.confidence.clamp(0.0, 1.0))
        .bind(&draft.sensitivity)
        .bind(draft.lifecycle.as_str())
        .bind(i64::from(draft.lifecycle == FactLifecycle::Confirmed))
        .bind(&conflict_group)
        .bind(&draft.projection_id)
        .bind(&candidate_json)
        .bind(&authority_json)
        .bind(&evidence_refs_json)
        .bind(&source_event_ids_json)
        .bind(&pollution_lineage_json)
        .execute(&mut **tx)
        .await?;
        let stored = sqlx::query_as::<Sqlite, (String, String, String, String)>(
            "SELECT tenant_id, user_id, evidence_hash, candidate_json
             FROM structured_memory_facts WHERE id = ?",
        )
        .bind(&draft.fact_id)
        .fetch_one(&mut **tx)
        .await?;
        if stored.0 != draft.tenant_id
            || stored.1 != draft.user_id
            || stored.2 != draft.evidence_hash
            || stable_hash(&stored.3) != stable_hash(&candidate_json)
        {
            return Err(MemoryRepositoryError::Scope(
                "immutable fact ID was reused with different evidence or scope".into(),
            ));
        }
        let idempotency_key = format!("{}:{}", draft.lifecycle.operation(), draft.fact_id);
        append_fact_event_in_transaction(
            tx,
            &FactEventAppend {
                tenant_id: &draft.tenant_id,
                user_id: &draft.user_id,
                fact_id: &draft.fact_id,
                operation: draft.lifecycle.operation(),
                lifecycle: draft.lifecycle,
                source_event_ids: &draft.source_event_ids,
                payload_hash: &stable_hash(&candidate_json),
                idempotency_key: &idempotency_key,
            },
        )
        .await?;
        if draft.lifecycle == FactLifecycle::Confirmed {
            let old = sqlx::query_scalar::<Sqlite, String>(
                "SELECT projection_memory_id FROM structured_memory_facts
                 WHERE tenant_id = ? AND user_id = ? AND scope = ? AND app = ?
                   AND (session_id = ? OR (session_id IS NULL AND ? IS NULL))
                   AND subject_json = ? AND predicate = ? AND lifecycle = 'confirmed'
                   AND id <> ? AND projection_memory_id IS NOT NULL",
            )
            .bind(&draft.tenant_id)
            .bind(&draft.user_id)
            .bind(&draft.scope)
            .bind(&draft.app)
            .bind(&draft.session_id)
            .bind(&draft.session_id)
            .bind(&subject_json)
            .bind(&draft.predicate)
            .bind(&draft.fact_id)
            .fetch_all(&mut **tx)
            .await?;
            for projection_id in old {
                Self::supersede_in_transaction(
                    tx,
                    &draft.tenant_id,
                    &draft.user_id,
                    &projection_id,
                    &draft.projection_id,
                    &draft.source_event_ids,
                )
                .await?;
            }
        }
        let enabled = i64::from(draft.lifecycle == FactLifecycle::Confirmed);
        sqlx::query::<Sqlite>(
            "INSERT INTO agent_memory_items
                (id, tenant_id, user_id, scope, app, session_id, session_key,
                 memory_type, content, content_hash, source_type, confidence,
                 pinned, enabled, stale_at, verified_at, metadata_json,
                 embedding_model, embedding_dimensions, embedding_json,
                 created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, json(?), ?, ?, ?,
                     CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
                 content = excluded.content, content_hash = excluded.content_hash,
                 confidence = excluded.confidence, pinned = excluded.pinned,
                 enabled = excluded.enabled, metadata_json = excluded.metadata_json,
                 stale_at = COALESCE(excluded.stale_at, agent_memory_items.stale_at),
                 verified_at = COALESCE(excluded.verified_at, agent_memory_items.verified_at),
                 embedding_model = COALESCE(excluded.embedding_model, agent_memory_items.embedding_model),
                 embedding_dimensions = COALESCE(excluded.embedding_dimensions, agent_memory_items.embedding_dimensions),
                 embedding_json = COALESCE(excluded.embedding_json, agent_memory_items.embedding_json),
                 updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&draft.projection_id)
        .bind(&draft.tenant_id)
        .bind(&draft.user_id)
        .bind(&draft.scope)
        .bind(&draft.app)
        .bind(&draft.session_id)
        .bind(draft.session_id.as_deref().unwrap_or_default())
        .bind(&draft.memory_type)
        .bind(&draft.text)
        .bind(stable_hash(&draft.text))
        .bind(&draft.source_type)
        .bind(draft.confidence.clamp(0.0, 1.0))
        .bind(i64::from(draft.pinned))
        .bind(enabled)
        .bind(&draft.stale_at)
        .bind(&draft.verified_at)
        .bind(draft.metadata.to_string())
        .bind(&draft.embedding_model)
        .bind(draft.embedding_dimensions)
        .bind(&draft.embedding_json)
        .execute(&mut **tx)
        .await?;
        process_fault_point("memory.repository.before_return");
        Ok(())
    }

    pub(crate) async fn forget_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
        projection_id: &str,
        source_event_ids: &[String],
    ) -> Result<(), MemoryRepositoryError> {
        let fact_id = sqlx::query_scalar::<Sqlite, String>(
            "SELECT id FROM structured_memory_facts
             WHERE tenant_id = ? AND user_id = ? AND projection_memory_id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(projection_id)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query::<Sqlite>(
            "UPDATE structured_memory_facts
             SET lifecycle = 'forgotten', current = 0,
                 valid_until = COALESCE(valid_until, CURRENT_TIMESTAMP),
                 candidate_json = json_set(
                     json_set(candidate_json, '$.lifecycle', 'forgotten'),
                     '$.valid_until', COALESCE(valid_until, CURRENT_TIMESTAMP)
                 ),
                 updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&fact_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query::<Sqlite>(
            "UPDATE agent_memory_items SET enabled = 0, stale_at = CURRENT_TIMESTAMP,
                    updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(projection_id)
        .execute(&mut **tx)
        .await?;
        let key = format!("forgotten:{fact_id}");
        append_fact_event_in_transaction(
            tx,
            &FactEventAppend {
                tenant_id,
                user_id,
                fact_id: &fact_id,
                operation: "forgotten",
                lifecycle: FactLifecycle::Forgotten,
                source_event_ids,
                payload_hash: &stable_hash(&format!("{fact_id}:forgotten")),
                idempotency_key: &key,
            },
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn transition_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
        fact_id: &str,
        target: FactLifecycle,
        source_event_ids: &[String],
        independent_confirmation: bool,
    ) -> Result<(), MemoryRepositoryError> {
        let row = sqlx::query_as::<Sqlite, (String, Option<String>, String, String)>(
            "SELECT lifecycle, projection_memory_id, pollution_lineage_json, candidate_json
             FROM structured_memory_facts
             WHERE id = ? AND tenant_id = ? AND user_id = ?",
        )
        .bind(fact_id)
        .bind(tenant_id)
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await?;
        let current = lifecycle_from_str(&row.0)?;
        if current == target {
            return Ok(());
        }
        if !current.can_transition_to(target) {
            return Err(MemoryRepositoryError::Lifecycle(format!(
                "illegal transition {} -> {} for {fact_id}",
                current.as_str(),
                target.as_str()
            )));
        }
        let pollution_lineage = serde_json::from_str::<Vec<String>>(&row.2)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        if target == FactLifecycle::Confirmed
            && !pollution_lineage.is_empty()
            && !independent_confirmation
        {
            return Err(MemoryRepositoryError::Admission(
                "quarantined evidence requires an independent authority before promotion".into(),
            ));
        }
        let changed = sqlx::query::<Sqlite>(
            "UPDATE structured_memory_facts
             SET lifecycle = ?, current = ?, updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND tenant_id = ? AND user_id = ? AND lifecycle = ?",
        )
        .bind(target.as_str())
        .bind(i64::from(target == FactLifecycle::Confirmed))
        .bind(fact_id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(current.as_str())
        .execute(&mut **tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(MemoryRepositoryError::Lifecycle(
                "stale lifecycle writer was fenced".into(),
            ));
        }
        if let Some(projection_id) = row.1.as_deref() {
            sqlx::query::<Sqlite>(
                "UPDATE agent_memory_items
                 SET enabled = ?, stale_at = CASE WHEN ? = 1 THEN NULL ELSE CURRENT_TIMESTAMP END,
                     verified_at = CASE WHEN ? = 1 THEN CURRENT_TIMESTAMP ELSE verified_at END,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ? AND tenant_id = ? AND user_id = ?",
            )
            .bind(i64::from(target == FactLifecycle::Confirmed))
            .bind(i64::from(target == FactLifecycle::Confirmed))
            .bind(i64::from(target == FactLifecycle::Confirmed))
            .bind(projection_id)
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut **tx)
            .await?;
        }
        let source_event_ids_json = serde_json::to_string(source_event_ids)
            .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
        let payload_hash = stable_hash(&format!(
            "{fact_id}:{}:{source_event_ids_json}:{independent_confirmation}",
            target.as_str()
        ));
        let idempotency_key = format!("{}:{fact_id}:{payload_hash}", target.operation());
        append_fact_event_in_transaction(
            tx,
            &FactEventAppend {
                tenant_id,
                user_id,
                fact_id,
                operation: target.operation(),
                lifecycle: target,
                source_event_ids,
                payload_hash: &payload_hash,
                idempotency_key: &idempotency_key,
            },
        )
        .await?;
        Ok(())
    }

    /// Rebuild the compatibility search projection exclusively from canonical
    /// structured facts. No projection row is allowed to create or mutate a
    /// fact in the opposite direction.
    pub(crate) async fn rebuild_projection_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<String, MemoryRepositoryError> {
        let rows = sqlx::query_as::<Sqlite, (String, String, String)>(
            "SELECT candidate_json, lifecycle, projection_memory_id
             FROM structured_memory_facts
             WHERE tenant_id = ? AND user_id = ? AND projection_memory_id IS NOT NULL
             ORDER BY id",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_all(&mut **tx)
        .await?;
        for (_, _, projection_id) in &rows {
            sqlx::query::<Sqlite>(
                "DELETE FROM agent_memory_items
                 WHERE id = ? AND tenant_id = ? AND user_id = ?",
            )
            .bind(projection_id)
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut **tx)
            .await?;
        }
        let mut hash_rows = Vec::with_capacity(rows.len());
        for (candidate_json, lifecycle, projection_id) in rows {
            let draft = serde_json::from_str::<MemoryFactDraft>(&candidate_json)
                .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
            if draft.tenant_id != tenant_id
                || draft.user_id != user_id
                || draft.projection_id != projection_id
            {
                return Err(MemoryRepositoryError::Scope(
                    "canonical fact candidate projection scope is inconsistent".into(),
                ));
            }
            let lifecycle = lifecycle_from_str(&lifecycle)?;
            write_projection(tx, &draft, lifecycle == FactLifecycle::Confirmed).await?;
            hash_rows.push(format!(
                "{}:{}:{}:{}",
                draft.fact_id,
                draft.projection_id,
                lifecycle.as_str(),
                stable_hash(&candidate_json)
            ));
        }
        let projection_hash = stable_hash(&hash_rows.join("\n"));
        let last_global_sequence = sqlx::query_scalar::<Sqlite, i64>(
            "SELECT COALESCE(MAX(global_sequence), 0) FROM memory_fact_events
             WHERE tenant_id = ? AND user_id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query::<Sqlite>(
            "INSERT INTO memory_projection_state
                (tenant_id, user_id, reducer_version, last_global_sequence, projection_hash)
             VALUES (?, ?, 'memory-reducer-v3', ?, ?)
             ON CONFLICT(tenant_id, user_id) DO UPDATE SET
                 reducer_version = excluded.reducer_version,
                 last_global_sequence = excluded.last_global_sequence,
                 projection_hash = excluded.projection_hash,
                 updated_at = CURRENT_TIMESTAMP",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(last_global_sequence)
        .bind(&projection_hash)
        .execute(&mut **tx)
        .await?;
        Ok(projection_hash)
    }

    pub(crate) async fn supersede_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        user_id: &str,
        from_projection_id: &str,
        to_projection_id: &str,
        source_event_ids: &[String],
    ) -> Result<(), MemoryRepositoryError> {
        let from_fact = sqlx::query_scalar::<Sqlite, String>(
            "SELECT id FROM structured_memory_facts
             WHERE tenant_id = ? AND user_id = ? AND projection_memory_id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(from_projection_id)
        .fetch_one(&mut **tx)
        .await?;
        let to_fact = sqlx::query_scalar::<Sqlite, String>(
            "SELECT id FROM structured_memory_facts
             WHERE tenant_id = ? AND user_id = ? AND projection_memory_id = ?
               AND lifecycle = 'confirmed'",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(to_projection_id)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query::<Sqlite>(
            "UPDATE structured_memory_facts
             SET lifecycle = 'superseded', current = 0, superseded_by = ?,
                 updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND id = ?
               AND lifecycle = 'confirmed'",
        )
        .bind(&to_fact)
        .bind(tenant_id)
        .bind(user_id)
        .bind(&from_fact)
        .execute(&mut **tx)
        .await?;
        sqlx::query::<Sqlite>(
            "UPDATE agent_memory_items SET enabled = 0, stale_at = CURRENT_TIMESTAMP,
                    updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(from_projection_id)
        .execute(&mut **tx)
        .await?;
        let key = format!("superseded:{from_fact}:{to_fact}");
        append_fact_event_in_transaction(
            tx,
            &FactEventAppend {
                tenant_id,
                user_id,
                fact_id: &from_fact,
                operation: "superseded",
                lifecycle: FactLifecycle::Superseded,
                source_event_ids,
                payload_hash: &stable_hash(&key),
                idempotency_key: &key,
            },
        )
        .await?;
        Ok(())
    }

    /// Compliance erasure removes ciphertext-bearing fact/projection rows but
    /// retains a content-free canonical erasure event for audit and replay.
    pub(crate) async fn erase_session_in_transaction(
        tx: &mut Transaction<'_, Sqlite>,
        tenant_id: &str,
        session_id: &str,
    ) -> Result<(), MemoryRepositoryError> {
        let rows = sqlx::query_as::<Sqlite, (String, String)>(
            "SELECT id, user_id FROM structured_memory_facts
             WHERE tenant_id = ? AND session_id = ?",
        )
        .bind(tenant_id)
        .bind(session_id)
        .fetch_all(&mut **tx)
        .await?;
        for (fact_id, user_id) in &rows {
            let key = format!("erased:{fact_id}");
            append_fact_event_in_transaction(
                tx,
                &FactEventAppend {
                    tenant_id,
                    user_id,
                    fact_id,
                    operation: "erased",
                    lifecycle: FactLifecycle::Forgotten,
                    source_event_ids: &[],
                    payload_hash: &stable_hash(&key),
                    idempotency_key: &key,
                },
            )
            .await?;
        }
        sqlx::query::<Sqlite>(
            "DELETE FROM agent_memory_items WHERE tenant_id = ? AND session_id = ?",
        )
        .bind(tenant_id)
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query::<Sqlite>(
            "DELETE FROM structured_memory_facts WHERE tenant_id = ? AND session_id = ?",
        )
        .bind(tenant_id)
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

struct FactEventAppend<'a> {
    tenant_id: &'a str,
    user_id: &'a str,
    fact_id: &'a str,
    operation: &'a str,
    lifecycle: FactLifecycle,
    source_event_ids: &'a [String],
    payload_hash: &'a str,
    idempotency_key: &'a str,
}

async fn append_fact_event_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    event: &FactEventAppend<'_>,
) -> Result<(), MemoryRepositoryError> {
    let existing = sqlx::query_as::<Sqlite, (String, String, String)>(
        "SELECT fact_id, lifecycle, payload_hash FROM memory_fact_events
         WHERE tenant_id = ? AND idempotency_key = ?",
    )
    .bind(event.tenant_id)
    .bind(event.idempotency_key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(existing) = existing {
        if existing.0 == event.fact_id
            && existing.1 == event.lifecycle.as_str()
            && existing.2 == event.payload_hash
        {
            return Ok(());
        }
        return Err(MemoryRepositoryError::Lifecycle(
            "memory event idempotency key was reused with different semantics".into(),
        ));
    }
    let stream_revision = sqlx::query_scalar::<Sqlite, i64>(
        "SELECT COALESCE(MAX(stream_revision), 0) + 1
         FROM memory_fact_events WHERE tenant_id = ? AND fact_id = ?",
    )
    .bind(event.tenant_id)
    .bind(event.fact_id)
    .fetch_one(&mut **tx)
    .await?;
    let global_sequence = sqlx::query_scalar::<Sqlite, i64>(
        "SELECT COALESCE(MAX(global_sequence), 0) + 1 FROM memory_fact_events",
    )
    .fetch_one(&mut **tx)
    .await?;
    let source_event_ids_json = serde_json::to_string(event.source_event_ids)
        .map_err(|error| MemoryRepositoryError::Admission(error.to_string()))?;
    let causation_event_id = event.source_event_ids.first().map(String::as_str);
    let correlation_id = format!("memory-fact:{}", event.fact_id);
    sqlx::query::<Sqlite>(
        "INSERT INTO memory_fact_events
            (event_id, tenant_id, user_id, fact_id, stream_revision,
             global_sequence, schema_version, actor_json, causation_event_id,
             correlation_id, operation, lifecycle, source_event_ids_json,
             payload_hash, idempotency_key)
         VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(event.tenant_id)
    .bind(event.user_id)
    .bind(event.fact_id)
    .bind(stream_revision)
    .bind(global_sequence)
    .bind(r#"{"kind":"worker","id":"memory-engine-v3"}"#)
    .bind(causation_event_id)
    .bind(correlation_id)
    .bind(event.operation)
    .bind(event.lifecycle.as_str())
    .bind(source_event_ids_json)
    .bind(event.payload_hash)
    .bind(event.idempotency_key)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn lifecycle_from_str(value: &str) -> Result<FactLifecycle, MemoryRepositoryError> {
    match value {
        "candidate" => Ok(FactLifecycle::Candidate),
        "quarantined" => Ok(FactLifecycle::Quarantined),
        "confirmed" => Ok(FactLifecycle::Confirmed),
        "superseded" => Ok(FactLifecycle::Superseded),
        "forgotten" => Ok(FactLifecycle::Forgotten),
        "rejected" => Ok(FactLifecycle::Rejected),
        other => Err(MemoryRepositoryError::Lifecycle(format!(
            "unknown lifecycle {other}"
        ))),
    }
}

async fn write_projection(
    tx: &mut Transaction<'_, Sqlite>,
    draft: &MemoryFactDraft,
    enabled: bool,
) -> Result<(), MemoryRepositoryError> {
    sqlx::query::<Sqlite>(
        "INSERT INTO agent_memory_items
            (id, tenant_id, user_id, scope, app, session_id, session_key,
             memory_type, content, content_hash, source_type, confidence,
             pinned, enabled, stale_at, verified_at, metadata_json,
             embedding_model, embedding_dimensions, embedding_json,
             created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, json(?), ?, ?, ?,
                 CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .bind(&draft.projection_id)
    .bind(&draft.tenant_id)
    .bind(&draft.user_id)
    .bind(&draft.scope)
    .bind(&draft.app)
    .bind(&draft.session_id)
    .bind(draft.session_id.as_deref().unwrap_or_default())
    .bind(&draft.memory_type)
    .bind(&draft.text)
    .bind(stable_hash(&draft.text))
    .bind(&draft.source_type)
    .bind(draft.confidence.clamp(0.0, 1.0))
    .bind(i64::from(draft.pinned))
    .bind(i64::from(enabled))
    .bind(&draft.stale_at)
    .bind(&draft.verified_at)
    .bind(draft.metadata.to_string())
    .bind(&draft.embedding_model)
    .bind(draft.embedding_dimensions)
    .bind(&draft.embedding_json)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn stable_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::path::Path;

    #[tokio::test]
    async fn embedding_projection_update_is_fenced_by_canonical_fact_version_and_scope() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open Memory Repository fixture");
        sqlx::query(
            "CREATE TABLE agent_memory_items (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT NOT NULL,
                embedding_model TEXT, embedding_dimensions INTEGER, embedding_json TEXT,
                updated_at TEXT
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE structured_memory_facts (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT NOT NULL,
                projection_memory_id TEXT, evidence_hash TEXT NOT NULL, lifecycle TEXT NOT NULL
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_memory_items (id, tenant_id, user_id)
             VALUES ('projection-1', 'tenant-1', 'user-1')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO structured_memory_facts
                (id, tenant_id, user_id, projection_memory_id, evidence_hash, lifecycle)
             VALUES ('fact-1', 'tenant-1', 'user-1', 'projection-1', 'source-v1', 'confirmed')",
        )
        .execute(&db)
        .await
        .unwrap();

        let mut tx = db.begin().await.unwrap();
        assert!(SqliteMemoryRepository::update_embedding_in_transaction(
            &mut tx,
            &MemoryEmbeddingUpdate {
                tenant_id: "tenant-1",
                user_id: "user-1",
                fact_id: "fact-1",
                projection_id: "projection-1",
                source_hash: "source-v1",
                embedding_model: "local-model-v1",
                embedding: &[0.1, 0.2, 0.3],
            },
        )
        .await
        .unwrap());
        tx.commit().await.unwrap();

        sqlx::query("UPDATE structured_memory_facts SET evidence_hash = 'source-v2'")
            .execute(&db)
            .await
            .unwrap();
        let mut stale_tx = db.begin().await.unwrap();
        assert!(!SqliteMemoryRepository::update_embedding_in_transaction(
            &mut stale_tx,
            &MemoryEmbeddingUpdate {
                tenant_id: "tenant-1",
                user_id: "user-1",
                fact_id: "fact-1",
                projection_id: "projection-1",
                source_hash: "source-v1",
                embedding_model: "stale-remote-model",
                embedding: &[9.0],
            },
        )
        .await
        .unwrap());
        stale_tx.commit().await.unwrap();

        let stored = sqlx::query_as::<_, (String, i64, String)>(
            "SELECT embedding_model, embedding_dimensions, embedding_json
             FROM agent_memory_items WHERE id = 'projection-1'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(stored.0, "local-model-v1");
        assert_eq!(stored.1, 3);
        assert_eq!(
            serde_json::from_str::<Vec<f32>>(&stored.2).unwrap(),
            vec![0.1, 0.2, 0.3]
        );
    }

    #[test]
    fn production_memory_table_writes_are_owned_by_memory_repository() {
        let crates_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates directory");
        let forbidden = Regex::new(
            r"(?i)\b(?:INSERT\s+INTO|UPDATE|DELETE\s+FROM)\s+(?:agent_memory_items|structured_memory_facts)\b",
        )
        .unwrap();
        let repository_bypass = Regex::new(r"\bSqliteMemoryRepository\s*::").unwrap();
        let mut violations = Vec::new();
        visit_rust_sources(crates_root, &mut |path, source| {
            if path == Path::new(env!("CARGO_MANIFEST_DIR")).join("src/repository.rs") {
                return;
            }
            let production = remove_cfg_test_modules(source);
            for found in forbidden.find_iter(&production) {
                let line = production[..found.start()]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1;
                violations.push(format!("{}:{line}", path.display()));
            }
            for found in repository_bypass.find_iter(&production) {
                let line = production[..found.start()]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1;
                violations.push(format!("{}:{line}", path.display()));
            }
        });
        assert!(
            violations.is_empty(),
            "Memory canonical/projection tables have production writers outside MemoryRepository: {}",
            violations.join(", ")
        );
    }

    fn visit_rust_sources(root: &Path, visitor: &mut impl FnMut(&Path, &str)) {
        for entry in std::fs::read_dir(root).expect("read crates directory") {
            let entry = entry.expect("read source entry");
            let path = entry.path();
            if path.is_dir() {
                visit_rust_sources(&path, visitor);
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                let source = std::fs::read_to_string(&path).expect("read Rust source");
                visitor(&path, &source);
            }
        }
    }

    fn remove_cfg_test_modules(source: &str) -> String {
        let declaration =
            Regex::new(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{")
                .unwrap();
        let mut bytes = source.as_bytes().to_vec();
        for found in declaration.find_iter(source) {
            let opening = source[found.start()..found.end()]
                .rfind('{')
                .map(|offset| found.start() + offset)
                .expect("test module opening brace");
            let closing = matching_rust_brace(source, opening).expect("balanced test module");
            bytes[found.start()..=closing].fill(b' ');
        }
        String::from_utf8(bytes).expect("Rust source remains UTF-8")
    }

    fn matching_rust_brace(source: &str, opening: usize) -> Option<usize> {
        let bytes = source.as_bytes();
        let mut index = opening;
        let mut depth = 0_i64;
        let mut block_comment_depth = 0_i64;
        let mut quote = None;
        let mut raw_hashes = None;
        while index < bytes.len() {
            let current = bytes[index];
            let next = bytes.get(index + 1).copied();
            if block_comment_depth > 0 {
                if current == b'/' && next == Some(b'*') {
                    block_comment_depth += 1;
                    index += 2;
                    continue;
                }
                if current == b'*' && next == Some(b'/') {
                    block_comment_depth -= 1;
                    index += 2;
                    continue;
                }
                index += 1;
                continue;
            }
            if let Some(hash_count) = raw_hashes {
                if current == b'"'
                    && bytes
                        .get(index + 1..index + 1 + hash_count)
                        .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
                {
                    raw_hashes = None;
                    index += hash_count + 1;
                } else {
                    index += 1;
                }
                continue;
            }
            if let Some(delimiter) = quote {
                if current == b'\\' {
                    index += 2;
                    continue;
                }
                if current == delimiter {
                    quote = None;
                }
                index += 1;
                continue;
            }
            if current == b'/' && next == Some(b'/') {
                index = source[index..]
                    .find('\n')
                    .map_or(bytes.len(), |offset| index + offset + 1);
                continue;
            }
            if current == b'/' && next == Some(b'*') {
                block_comment_depth = 1;
                index += 2;
                continue;
            }
            if current == b'r' {
                let mut cursor = index + 1;
                while bytes.get(cursor) == Some(&b'#') {
                    cursor += 1;
                }
                if bytes.get(cursor) == Some(&b'"') {
                    raw_hashes = Some(cursor - index - 1);
                    index = cursor + 1;
                    continue;
                }
            }
            if current == b'\''
                && next.is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && bytes.get(index + 2) != Some(&b'\'')
            {
                index += 1;
                continue;
            }
            if current == b'"' || current == b'\'' {
                quote = Some(current);
                index += 1;
                continue;
            }
            if current == b'{' {
                depth += 1;
            } else if current == b'}' {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            index += 1;
        }
        None
    }
}
