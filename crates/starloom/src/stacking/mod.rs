//! The stacking kernel (the `stack` feature): measuring stars per frame, weighting and selecting frames, registration,
//! local normalization (`ln`), pixel rejection, integration into a master, drizzle, and the configuration and crop
//! helpers around them.
//!
//! The kernel has no catalog, no settings and no orchestration: the caller decides which frames form a group, where
//! files live and when a stage runs, and passes the results in as parameters.
//!
//! Persisted file names keep their historical `ath` prefix so that files already on disk keep reading: `.athf` (a
//! frame's star fits), `.athm` and its `ATHM` magic (a frame's first-pass measurement) and `.athln`
//! (local-normalization grids). They are not renamed.

pub mod common_set;
pub mod config;
pub mod crop;
pub mod drizzle;
pub mod groups;
pub mod integrate;
pub mod ln;
pub mod measure;
pub mod pass1_artifact;
pub mod prefilter;
pub mod psf_signal;
pub mod register;
pub mod rej;
// Robust Chauvenet Rejection lives with the star-measurement module; re-exported so
// `stacking::robust::…` callers are unchanged.
pub use crate::star_measure::robust;
#[cfg(test)]
mod star_measure_identity;
pub mod structure;
pub mod weights;
