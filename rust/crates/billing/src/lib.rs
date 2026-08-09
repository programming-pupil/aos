//! Billing consumer — reads telemetry JSONL files and writes token usage to the database.
//!
//! This crate is completely decoupled from the core CLI.
//! It runs as a background worker that records usage events produced by AOS services.

mod consumer;
mod usage;

pub use consumer::{TelemetryConsumer, TelemetryConsumerConfig};
pub use usage::{TokenUsageRecord, UsageAggregator};
