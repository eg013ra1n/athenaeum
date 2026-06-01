//! Stacking-preparation: frame-to-frame image registration.
//!
//! Detects stars in every LIGHT member of a frame set, precise-solves the
//! reference member to an absolute WCS, then aligns every other member to the
//! reference using the `solvemyastro::register` primitive. Results are stored in
//! the `registration_results` table and never touch `frames` or `plate_solves`.
//!
//! # Public surface
//!
//! * [`db`] — `RegistrationRecord`, `upsert_registration`,
//!   `get_registration_for_frame_set`, `clear_registration_for_frame_set`.
//! * [`reference`] — `select_reference`.
//! * [`service`] — `register_frame_set`, `RegistrationSummary`.

pub mod db;
pub mod reference;
pub mod service;

pub use db::RegistrationRecord;
pub use service::{register_frame_set, RegistrationSummary};
