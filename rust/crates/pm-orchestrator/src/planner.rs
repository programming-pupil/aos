//! Compatibility re-exports for PM planning domain models.
//!
//! Pure planning DTOs live in `pm-domain` so UI/API crates can use them without
//! depending on persistence-heavy orchestration internals.

pub use pm_domain::planner::*;
