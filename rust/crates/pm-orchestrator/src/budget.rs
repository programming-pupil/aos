//! Compatibility re-exports for PM budget domain models.
//!
//! Pure budget DTOs live in `pm-domain` so API/UI crates can use them without
//! pulling in persistence-heavy orchestration internals.

pub use pm_domain::budget::*;
