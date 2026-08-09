//! Telemetry consumer management.
//!
//! Runs in a background task inside the web-server tokio runtime,
//! watching `.aos/telemetry.jsonl` files for usage events and flushing
//! them into the local SQLite database. This decouples the CLI (which only writes JSONL)
//! from the database entirely.

use billing::{TelemetryConsumer, TelemetryConsumerConfig};
use std::path::PathBuf;

/// Start the telemetry consumer as a detached background task.
/// It tails `.aos/telemetry.jsonl` and flushes usage records into SQLite.
/// Runs until the tokio runtime shuts down.
#[allow(clippy::needless_pass_by_value)]
pub fn start_telemetry_consumer(data_dir: PathBuf, pool: sqlx::SqlitePool) {
    let telemetry_path = data_dir.join(".aos").join("telemetry.jsonl");

    let config = TelemetryConsumerConfig::new(telemetry_path, pool);

    tokio::spawn(async move {
        let consumer = TelemetryConsumer::new(config);

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        if let Err(e) = consumer.run(shutdown_rx).await {
            tracing::error!(error = %e, "telemetry consumer terminated with error");
        }
    });

    tracing::info!(
        path = %data_dir.join(".aos/telemetry.jsonl").display(),
        "telemetry consumer started (background)"
    );
}
