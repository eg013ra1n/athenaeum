//! File operations (Move, Delete) feature.
//!
//! Mirrors `crates/athenaeum-core/src/archive/` in structure: `models` for data
//! types, `db` for CRUD, `planner` for plan construction, `executor` for the
//! per-file work, plus `rollback` and `resume` (added in Phase 2 for Delete).
//!
//! The shared scaffolding (step-log writes, cancellation, progress emission)
//! is exposed via `services::operation_runner` so the same primitives back
//! both archive and file ops.

pub mod db;
pub mod executor;
pub mod models;
pub mod planner;
pub mod reconcile;
