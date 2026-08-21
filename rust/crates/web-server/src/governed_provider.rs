//! Durable provider boundary for bounded, product-specific model inference.
//!
//! Session-producing runtimes use the canonical `agent_event_ledger` surface.
//! One-shot classifiers and domain helpers have no conversational surface to
//! fold, so their complete typed request is the canonical surface. This
//! adapter commits and encrypts that surface before provider I/O and records a
//! terminal attempt afterward. A storage failure is fail-closed.

use api::{ApiError, MessageRequest, MessageResponse, MessageStream, StreamEvent, Usage};
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, SqlitePool, Transaction};

#[derive(Debug, Clone)]
pub struct GovernedProviderClient {
    inner: api::ProviderClient,
    db: SqlitePool,
    tenant_id: String,
    owner_user_id: String,
    authority: String,
}

#[derive(Debug)]
struct DispatchAttempt {
    id: String,
    tenant_id: String,
    db: SqlitePool,
    terminal: bool,
}

#[derive(Debug)]
pub struct GovernedMessageStream {
    inner: MessageStream,
    attempt: DispatchAttempt,
    response_hasher: Sha256,
    terminal: bool,
}

impl GovernedProviderClient {
    #[must_use]
    pub(crate) fn new(
        inner: api::ProviderClient,
        db: SqlitePool,
        tenant_id: impl Into<String>,
        owner_user_id: impl Into<String>,
        authority: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            db,
            tenant_id: tenant_id.into(),
            owner_user_id: owner_user_id.into(),
            authority: authority.into(),
        }
    }

    pub(crate) async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let request = request
            .protect_sensitive_content(runtime::configured_data_protection_mode())
            .0;
        let mut attempt = self
            .begin_dispatch(&request)
            .await
            .map_err(governance_error)?;
        match self.inner.send_message(&request).await {
            Ok(response) => {
                let response_hash = hash_serializable(&response).map_err(governance_error)?;
                finish_dispatch(&mut attempt, "succeeded", Some(&response_hash), None)
                    .await
                    .map_err(governance_error)?;
                Ok(response)
            }
            Err(error) => {
                let projection = protected_error(&error.to_string());
                finish_dispatch(&mut attempt, "failed", None, Some(&projection))
                    .await
                    .map_err(governance_error)?;
                Err(error)
            }
        }
    }

    pub(crate) async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<GovernedMessageStream, ApiError> {
        let request = request
            .protect_sensitive_content(runtime::configured_data_protection_mode())
            .0;
        let mut attempt = self
            .begin_dispatch(&request)
            .await
            .map_err(governance_error)?;
        match self.inner.stream_message(&request).await {
            Ok(inner) => Ok(GovernedMessageStream {
                inner,
                attempt,
                response_hasher: Sha256::new(),
                terminal: false,
            }),
            Err(error) => {
                let projection = protected_error(&error.to_string());
                finish_dispatch(&mut attempt, "failed", None, Some(&projection))
                    .await
                    .map_err(governance_error)?;
                Err(error)
            }
        }
    }

    pub(crate) async fn send_responses_web_search_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let request = request
            .protect_sensitive_content(runtime::configured_data_protection_mode())
            .0;
        let mut attempt = self
            .begin_dispatch(&request)
            .await
            .map_err(governance_error)?;
        match self.inner.send_responses_web_search_message(&request).await {
            Ok(response) => {
                let response_hash = hash_serializable(&response).map_err(governance_error)?;
                finish_dispatch(&mut attempt, "succeeded", Some(&response_hash), None)
                    .await
                    .map_err(governance_error)?;
                Ok(response)
            }
            Err(error) => {
                let projection = protected_error(&error.to_string());
                finish_dispatch(&mut attempt, "failed", None, Some(&projection))
                    .await
                    .map_err(governance_error)?;
                Err(error)
            }
        }
    }

    pub(crate) async fn stream_responses_web_search_message(
        &self,
        request: &MessageRequest,
    ) -> Result<GovernedMessageStream, ApiError> {
        let request = request
            .protect_sensitive_content(runtime::configured_data_protection_mode())
            .0;
        let mut attempt = self
            .begin_dispatch(&request)
            .await
            .map_err(governance_error)?;
        match self
            .inner
            .stream_responses_web_search_message(&request)
            .await
        {
            Ok(inner) => Ok(GovernedMessageStream {
                inner,
                attempt,
                response_hasher: Sha256::new(),
                terminal: false,
            }),
            Err(error) => {
                let projection = protected_error(&error.to_string());
                finish_dispatch(&mut attempt, "failed", None, Some(&projection))
                    .await
                    .map_err(governance_error)?;
                Err(error)
            }
        }
    }

    #[must_use]
    pub(crate) fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    async fn begin_dispatch(&self, request: &MessageRequest) -> anyhow::Result<DispatchAttempt> {
        let request_json = serde_json::to_string(request)?;
        let request_hash = sha256(request_json.as_bytes());
        let messages_hash = hash_serializable(&request.messages)?;
        let system_hash = sha256(request.system.as_deref().unwrap_or_default().as_bytes());
        let tool_schema_hash = hash_serializable(&request.tools)?;
        let request_group_id = sha256(
            format!(
                "v1\0{}\0{}\0{}\0{}",
                self.tenant_id, self.owner_user_id, self.authority, request_hash
            )
            .as_bytes(),
        );
        let id = uuid::Uuid::new_v4().to_string();
        let ciphertext = agent_gateway::crypto::encrypt_scoped(
            &request_json,
            &agent_gateway::crypto::scoped_aad("model_dispatch.request", &self.tenant_id, &id),
        )?;
        let projection_value = runtime::protect_sensitive_json(
            &serde_json::to_value(request)?,
            runtime::configured_data_protection_mode(),
        )
        .0;
        let projection = serde_json::to_string(&projection_value)?;
        let mut tx = self.db.begin().await?;
        acquire_write_lock(&mut tx).await?;
        sqlx::query(
            "UPDATE model_dispatch_surfaces
             SET status = 'failed', error_projection = 'worker_restarted',
                 completed_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND request_group_id = ? AND status = 'dispatched'
               AND created_at < datetime('now', '-15 minutes')",
        )
        .bind(&self.tenant_id)
        .bind(&request_group_id)
        .execute(&mut *tx)
        .await?;
        let attempt_index = sqlx::query_scalar::<Sqlite, i64>(
            "SELECT COALESCE(MAX(attempt_index), 0) + 1
             FROM model_dispatch_surfaces
             WHERE tenant_id = ? AND request_group_id = ?",
        )
        .bind(&self.tenant_id)
        .bind(&request_group_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO model_dispatch_surfaces
                (id, tenant_id, owner_user_id, authority, request_group_id,
                 attempt_index, provider_kind, model, request_hash,
                 messages_hash, system_hash, tool_schema_hash,
                 request_projection_json, request_ciphertext, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'dispatched')",
        )
        .bind(&id)
        .bind(&self.tenant_id)
        .bind(&self.owner_user_id)
        .bind(&self.authority)
        .bind(&request_group_id)
        .bind(attempt_index)
        .bind(format!("{:?}", self.inner.provider_kind()).to_ascii_lowercase())
        .bind(&request.model)
        .bind(&request_hash)
        .bind(&messages_hash)
        .bind(&system_hash)
        .bind(&tool_schema_hash)
        .bind(projection)
        .bind(ciphertext)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(DispatchAttempt {
            id,
            tenant_id: self.tenant_id.clone(),
            db: self.db.clone(),
            terminal: false,
        })
    }
}

impl GovernedMessageStream {
    #[must_use]
    pub(crate) fn request_id(&self) -> Option<&str> {
        self.inner.request_id()
    }

    pub(crate) async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        match self.inner.next_event().await {
            Ok(Some(event)) => {
                let encoded = serde_json::to_vec(&event).map_err(governance_error)?;
                self.response_hasher.update(encoded);
                Ok(Some(event))
            }
            Ok(None) => {
                if !self.terminal {
                    let response_hash = hex::encode(self.response_hasher.clone().finalize());
                    finish_dispatch(&mut self.attempt, "succeeded", Some(&response_hash), None)
                        .await
                        .map_err(governance_error)?;
                    self.terminal = true;
                }
                Ok(None)
            }
            Err(error) => {
                if !self.terminal {
                    let projection = protected_error(&error.to_string());
                    finish_dispatch(&mut self.attempt, "failed", None, Some(&projection))
                        .await
                        .map_err(governance_error)?;
                    self.terminal = true;
                }
                Err(error)
            }
        }
    }

    pub(crate) fn usage_summary(&mut self) -> Option<Usage> {
        self.inner.usage_summary()
    }

    #[must_use]
    pub(crate) fn provider_metadata(&self) -> Option<serde_json::Value> {
        self.inner.provider_metadata()
    }
}

pub(crate) async fn recover_incomplete_dispatches(db: &SqlitePool) -> anyhow::Result<u64> {
    Ok(sqlx::query(
        "UPDATE model_dispatch_surfaces
         SET status = 'failed', error_projection = 'worker_restarted',
             completed_at = CURRENT_TIMESTAMP
         WHERE status = 'dispatched'",
    )
    .execute(db)
    .await?
    .rows_affected())
}

async fn acquire_write_lock(tx: &mut Transaction<'_, Sqlite>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE model_dispatch_lock SET revision = revision + 1 WHERE id = 1")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn finish_dispatch(
    attempt: &mut DispatchAttempt,
    status: &str,
    response_hash: Option<&str>,
    error_projection: Option<&str>,
) -> anyhow::Result<()> {
    let result = sqlx::query(
        "UPDATE model_dispatch_surfaces
         SET status = ?, response_hash = ?, error_projection = ?,
             completed_at = CURRENT_TIMESTAMP
         WHERE id = ? AND tenant_id = ? AND status = 'dispatched'",
    )
    .bind(status)
    .bind(response_hash)
    .bind(error_projection)
    .bind(&attempt.id)
    .bind(&attempt.tenant_id)
    .execute(&attempt.db)
    .await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "model dispatch terminal transition lost its durable attempt"
    );
    attempt.terminal = true;
    Ok(())
}

impl Drop for DispatchAttempt {
    fn drop(&mut self) {
        if self.terminal {
            return;
        }
        let db = self.db.clone();
        let id = self.id.clone();
        let tenant_id = self.tenant_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = sqlx::query(
                    "UPDATE model_dispatch_surfaces
                     SET status = 'failed', error_projection = 'dispatch_cancelled',
                         completed_at = CURRENT_TIMESTAMP
                     WHERE id = ? AND tenant_id = ? AND status = 'dispatched'",
                )
                .bind(id)
                .bind(tenant_id)
                .execute(&db)
                .await
                {
                    tracing::error!(%error, "failed to terminate a cancelled model dispatch");
                }
            });
        }
    }
}

fn protected_error(error: &str) -> String {
    runtime::protect_sensitive_text(error, runtime::configured_data_protection_mode()).value
}

fn hash_serializable(value: &impl serde::Serialize) -> anyhow::Result<String> {
    Ok(sha256(&serde_json::to_vec(value)?))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn governance_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::Io(std::io::Error::other(format!(
        "durable model dispatch governance failed: {error}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row as _;

    fn request(text: &str) -> MessageRequest {
        MessageRequest {
            model: "test-model".into(),
            max_tokens: 32,
            messages: vec![api::InputMessage::user_text(text)],
            ..MessageRequest::default()
        }
    }

    #[tokio::test]
    async fn one_shot_dispatch_surface_is_encrypted_versioned_and_terminal() {
        let db = crate::test_sqlite_pool().await;
        let client = GovernedProviderClient::new(
            api::ProviderClient::OpenAi(api::OpenAiCompatClient::new(
                "unused",
                api::OpenAiCompatConfig::openai(),
            )),
            db.clone(),
            "tenant",
            "owner",
            "nl2sql-test",
        );
        let mut first = client.begin_dispatch(&request("hello")).await.unwrap();
        finish_dispatch(&mut first, "succeeded", Some("response-hash"), None)
            .await
            .unwrap();
        let mut second = client.begin_dispatch(&request("hello")).await.unwrap();
        finish_dispatch(&mut second, "failed", None, Some("provider_error"))
            .await
            .unwrap();
        let pending = client.begin_dispatch(&request("pending")).await.unwrap();
        assert_eq!(recover_incomplete_dispatches(&db).await.unwrap(), 1);

        let rows = sqlx::query(
            "SELECT attempt_index, status, request_ciphertext, request_hash,
                    messages_hash, system_hash, tool_schema_hash
             FROM model_dispatch_surfaces
             ORDER BY CASE
                 WHEN response_hash = 'response-hash' THEN 0
                 WHEN error_projection = 'provider_error' THEN 1
                 ELSE 2 END",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get::<i64, _>("attempt_index"), 1);
        assert_eq!(rows[1].get::<i64, _>("attempt_index"), 2);
        assert_eq!(rows[0].get::<String, _>("status"), "succeeded");
        assert_eq!(rows[1].get::<String, _>("status"), "failed");
        assert_eq!(rows[2].get::<String, _>("status"), "failed");
        assert!(rows[0]
            .get::<String, _>("request_ciphertext")
            .starts_with("aosenc:v2:"));
        for column in [
            "request_hash",
            "messages_hash",
            "system_hash",
            "tool_schema_hash",
        ] {
            assert_eq!(rows[0].get::<String, _>(column).len(), 64);
        }
        drop(pending);
    }
}
