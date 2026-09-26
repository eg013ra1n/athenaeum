//! On-disk state of the Collaboration root itself (wave 3, spec §9): the
//! storage marker that tells one designated device's disk apart from
//! another's (`marker`), the fast-path file watcher plus the canary that
//! detects a dead one (`watch`), the authoritative stat sweep (`sweep`),
//! plus the deletion module a later task in this wave adds here.

pub mod marker;
pub mod sweep;
pub mod watch;
