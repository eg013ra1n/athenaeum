//! External processing evidence and user-confirmed exposure identity.
//! Files remain independent catalog entries. A group records available versions,
//! not the existence of an original raw file. Integrations have multiple/unknown
//! inputs and are never members of a single-exposure group.
mod assessment;
pub use assessment::ProcessingAssessment;
mod catalog;
mod classify;
mod review;
pub use catalog::{
    effective_ids, exposure_seconds, get_effective_frame_ids, init_schema, refresh_file,
};
pub use classify::{classify, Classification};
pub use review::{confirm_link, get_review, set_stage, unlink_version};
pub use review::{VersionRecord, VersionReview, VersionSuggestion};

#[cfg(test)]
mod tests;
