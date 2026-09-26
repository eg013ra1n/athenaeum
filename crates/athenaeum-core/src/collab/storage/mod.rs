//! On-disk state of the Collaboration root itself (wave 3, spec §9): the
//! storage marker that tells one designated device's disk apart from
//! another's (`marker`), the fast-path file watcher plus the canary that
//! detects a dead one (`watch`), the authoritative stat sweep (`sweep`), the
//! per-frame local state machine (`states`) and the L4 deletion rules
//! (`deletions`). The engine that drives them lives in
//! `api::collab_live::storage_task`.

pub mod deletions;
pub mod marker;
pub mod states;
pub mod sweep;
pub mod watch;
