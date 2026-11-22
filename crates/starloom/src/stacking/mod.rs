//! The stacking kernel: measure, register, rejection, drizzle, ln, weights, config and crop (`stack`).
//!
//! Copied from Athenaeum core's `stacking` module; core keeps the orchestration, the catalog and the
//! persistence around it (`run`, `plan`, `paths`, `db` rows, master cards) and switches to this copy.

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
