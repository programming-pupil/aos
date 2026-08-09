//! In-memory usage aggregator with periodic flush to the local SQLite database.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::sqlite::SqlitePool;
use tokio::sync::Mutex;
use tracing::{error, info};

/// A single token usage record parsed from telemetry.
#[derive(Debug, Clone)]
pub struct TokenUsageRecord {
    pub tenant_id: Option<String>,
    pub user_id: Option<String>,
    pub session_id: String,
    pub request_id: Option<String>,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_read_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
    pub provider: String,
    pub created_at: DateTime<Utc>,
}

/// Aggregates token usage in memory and flushes to the database.
pub struct UsageAggregator {
    pool: SqlitePool,
    buffer: Mutex<Vec<TokenUsageRecord>>,
    /// Key: (`session_id`, model) → cumulative counts
    cache: Mutex<HashMap<(String, String), TokenUsageRecord>>,
}

impl UsageAggregator {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            buffer: Mutex::new(Vec::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Add a usage record to the buffer.
    pub async fn add(&self, record: TokenUsageRecord) {
        let mut buf = self.buffer.lock().await;
        buf.push(record);
    }

    /// Flush all buffered records to the database.
    pub async fn flush(&self) -> anyhow::Result<()> {
        let records: Vec<TokenUsageRecord> = {
            let mut buf = self.buffer.lock().await;
            std::mem::take(&mut *buf)
        };

        if records.is_empty() {
            return Ok(());
        }

        info!(
            count = records.len(),
            "flushing token usage records to database"
        );

        for record in &records {
            let id = uuid::Uuid::new_v4().to_string();
            let result = sqlx::query(
                "
                INSERT INTO token_usage
                    (id, tenant_id, user_id, session_id, request_id, model, input_tokens, output_tokens,
                     cache_creation_tokens, cache_read_tokens, total_tokens, estimated_cost_usd, provider, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )
            .bind(&id)
            .bind(record.tenant_id.as_deref().unwrap_or("default"))
            .bind(&record.user_id)
            .bind(&record.session_id)
            .bind(&record.request_id)
            .bind(&record.model)
            .bind(i64::from(record.input_tokens))
            .bind(i64::from(record.output_tokens))
            .bind(i64::from(record.cache_creation_tokens))
            .bind(i64::from(record.cache_read_tokens))
            .bind(i64::from(record.total_tokens))
            .bind(record.estimated_cost_usd)
            .bind(&record.provider)
            .bind(record.created_at)
            .execute(&self.pool)
            .await;

            if let Err(e) = result {
                error!(
                    session_id = %record.session_id,
                    "failed to insert token usage: {e}",
                );
            }
        }

        Ok(())
    }

    /// Get a cumulative summary for a given (`session_id`, model) pair.
    pub async fn get_cumulative(&self, session_id: &str, model: &str) -> Option<TokenUsageRecord> {
        let cache = self.cache.lock().await;
        cache
            .get(&(session_id.to_string(), model.to_string()))
            .cloned()
    }
}

/// Model pricing data for cost estimation.
#[expect(dead_code)]
pub fn pricing_for_model(model: &str) -> ModelPricing {
    let model = model.to_ascii_lowercase();
    if model.contains("opus") {
        ModelPricing {
            input: 15.0,
            output: 75.0,
            cache_creation: 18.75,
            cache_read: 1.5,
        }
    } else if model.contains("sonnet") {
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_creation: 3.75,
            cache_read: 0.3,
        }
    } else if model.contains("haiku") {
        ModelPricing {
            input: 0.8,
            output: 4.0,
            cache_creation: 1.0,
            cache_read: 0.08,
        }
    } else {
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_creation: 3.75,
            cache_read: 0.3,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_creation: f64,
    pub cache_read: f64,
}

impl ModelPricing {
    #[expect(dead_code)]
    pub fn estimate_cost(&self, record: &TokenUsageRecord) -> f64 {
        let input_cost = (f64::from(record.input_tokens) / 1_000_000.0) * self.input;
        let output_cost = (f64::from(record.output_tokens) / 1_000_000.0) * self.output;
        let cache_creation_cost =
            (f64::from(record.cache_creation_tokens) / 1_000_000.0) * self.cache_creation;
        let cache_read_cost = (f64::from(record.cache_read_tokens) / 1_000_000.0) * self.cache_read;
        input_cost + output_cost + cache_creation_cost + cache_read_cost
    }
}
