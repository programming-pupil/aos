//! Telemetry JSONL consumer — tails telemetry files and persists usage data.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sqlx::sqlite::SqlitePool;
use tokio::io::AsyncBufReadExt;
use tokio::sync::watch;
use tokio::time::interval;
use tracing::{debug, error, info};

use crate::usage::{TokenUsageRecord, UsageAggregator};

/// Configuration for the telemetry consumer.
#[derive(Debug, Clone)]
pub struct TelemetryConsumerConfig {
    /// Telemetry JSONL file path.
    pub telemetry_path: PathBuf,
    /// Database pool.
    pub pool: SqlitePool,
    /// How often to flush aggregated data to DB.
    pub flush_interval: Duration,
    /// Tenant ID to associate usage with.
    pub tenant_id: Option<String>,
    /// User ID to associate usage with.
    pub user_id: Option<String>,
}

impl TelemetryConsumerConfig {
    #[must_use]
    pub fn new(telemetry_path: PathBuf, pool: SqlitePool) -> Self {
        Self {
            telemetry_path,
            pool,
            flush_interval: Duration::from_secs(30),
            tenant_id: Some("default".to_string()),
            user_id: None,
        }
    }
}

/// Tails a telemetry JSONL file, parses events, aggregates usage, and flushes to DB.
pub struct TelemetryConsumer {
    config: TelemetryConsumerConfig,
    aggregator: Arc<UsageAggregator>,
}

impl TelemetryConsumer {
    #[must_use]
    pub fn new(config: TelemetryConsumerConfig) -> Self {
        let aggregator = UsageAggregator::new(config.pool.clone());
        Self {
            config,
            aggregator: Arc::new(aggregator),
        }
    }

    /// Start the consumer. Runs until `shutdown` is signaled.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        info!(
            path = %self.config.telemetry_path.display(),
            "starting telemetry consumer",
        );

        // Wait for file to exist, then open it
        loop {
            match tokio::fs::File::open(&self.config.telemetry_path).await {
                Ok(f) => {
                    let mut reader = tokio::io::BufReader::new(f);
                    tokio::io::AsyncSeekExt::seek(&mut reader, std::io::SeekFrom::End(0)).await?;
                    let mut lines = reader.lines();
                    let mut flush_interval = interval(self.config.flush_interval);

                    info!(path = %self.config.telemetry_path.display(), "tailing telemetry file");

                    loop {
                        tokio::select! {
                            _ = shutdown.changed() => {
                                if *shutdown.borrow() {
                                    info!("shutdown signal received; flushing remaining data...");
                                    self.aggregator.flush().await?;
                                    info!("telemetry consumer stopped gracefully");
                                    return Ok(());
                                }
                            }
                            line = lines.next_line() => {
                                match line {
                                    Ok(Some(line)) => {
                                        if let Err(e) = self.process_line(&line).await {
                                            error!(line = %line, "failed to process telemetry line: {e}");
                                        }
                                    }
                                    Ok(None) => {
                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                    }
                                    Err(e) => {
                                        error!("error reading telemetry file: {e}");
                                        tokio::time::sleep(Duration::from_secs(1)).await;
                                    }
                                }
                            }
                            _ = flush_interval.tick() => {
                                if let Err(e) = self.aggregator.flush().await {
                                    error!("failed to flush usage data: {e}");
                                }
                            }
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    debug!(
                        path = %self.config.telemetry_path.display(),
                        "telemetry file not found; waiting for CLI to create it...",
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    async fn process_line(&self, line: &str) -> anyhow::Result<()> {
        let event: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("failed to parse telemetry line: {e}"))?;

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "analytics" => {
                let namespace = event
                    .get("namespace")
                    .and_then(|v: &serde_json::Value| v.as_str())
                    .unwrap_or("");
                let action = event
                    .get("action")
                    .and_then(|v: &serde_json::Value| v.as_str())
                    .unwrap_or("");

                if namespace == "api" && action == "message_usage" {
                    let record = self.parse_usage_event(&event);
                    self.aggregator.add(record).await;
                }
            }
            "http_request_succeeded" => {
                // Log HTTP-level metrics
                let session_id = event
                    .get("session_id")
                    .and_then(|v: &serde_json::Value| v.as_str())
                    .unwrap_or("unknown");
                let status = event
                    .get("status")
                    .and_then(|v: &serde_json::Value| v.as_u64())
                    .unwrap_or(0);
                info!(
                    session_id = %session_id,
                    status = %status,
                    "HTTP request completed",
                );
            }
            _ => {}
        }

        Ok(())
    }

    fn parse_usage_event(&self, event: &serde_json::Value) -> TokenUsageRecord {
        let props = event.get("properties").and_then(|v| v.as_object());

        let session_id = props
            .and_then(|p| p.get("session_id"))
            .and_then(|v: &serde_json::Value| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let request_id = props
            .and_then(|p| p.get("request_id"))
            .and_then(|v: &serde_json::Value| v.as_str())
            .map(String::from);

        // Read the four granular token fields that are now emitted by api/anthropic.rs
        let input_tokens = u32::try_from(
            props
                .and_then(|p| p.get("input_tokens"))
                .and_then(|v: &serde_json::Value| v.as_u64())
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX);

        let output_tokens = u32::try_from(
            props
                .and_then(|p| p.get("output_tokens"))
                .and_then(|v: &serde_json::Value| v.as_u64())
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX);

        let cache_creation_tokens = u32::try_from(
            props
                .and_then(|p| p.get("cache_creation_input_tokens"))
                .and_then(|v: &serde_json::Value| v.as_u64())
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX);

        let cache_read_tokens = u32::try_from(
            props
                .and_then(|p| p.get("cache_read_input_tokens"))
                .and_then(|v: &serde_json::Value| v.as_u64())
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX);

        let total_tokens = input_tokens
            .saturating_add(output_tokens)
            .saturating_add(cache_creation_tokens)
            .saturating_add(cache_read_tokens);

        // Parse estimated cost from the string property (e.g. "$1.2340")
        let estimated_cost_usd = props
            .and_then(|p| p.get("estimated_cost_usd"))
            .and_then(|v: &serde_json::Value| v.as_str())
            .and_then(|s| s.strip_prefix('$'))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let model = props
            .and_then(|p| p.get("model"))
            .and_then(|v: &serde_json::Value| v.as_str())
            .map_or_else(|| "unknown".to_string(), String::from);

        TokenUsageRecord {
            tenant_id: self.config.tenant_id.clone(),
            user_id: self.config.user_id.clone(),
            session_id,
            request_id,
            model,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            total_tokens,
            estimated_cost_usd,
            provider: "anthropic".to_string(),
            created_at: chrono::Utc::now(),
        }
    }
}
