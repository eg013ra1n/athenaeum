//! On-disk state of the Collaboration root itself (wave 3, spec §9): the
//! storage marker that tells one designated device's disk apart from
//! another's (`marker`), plus the watcher/sweep/deletion modules later tasks
//! in this wave add here.

pub mod marker;
