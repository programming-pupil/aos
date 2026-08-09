//! Compatibility re-exports for NL2SQL merge strategies.
//!
//! Pure merge DTOs and algorithms live in `nl2sql-domain` so they can be checked
//! and reused without pulling in SQL engines, network clients, or runtime deps.

pub use nl2sql_domain::merge_strategy::*;
