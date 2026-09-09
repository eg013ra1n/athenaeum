//! Registration storage: the `registration_results` table and the
//! user-chosen reference frame.
//!
//! The plate-solve-era registration service (detect stars → precise-solve
//! the reference member → align every other member) was retired 2026-09-09 —
//! superseded by the M1 stacking pipeline's own registration stage
//! (`crate::stacking::register`), which writes to the same
//! `registration_results` table via [`db::upsert_registration`]. This module
//! now holds only the storage the stacking run and the Analysis tab's "Set as
//! reference" star still use.
//!
//! # Public surface
//!
//! * [`db`] — `upsert_registration`, `get_registration_for_frame_set`,
//!   `clear_registration_for_frame_set`, `FrameSetReference`,
//!   `set_frame_set_reference`, `get_frame_set_reference` (the row struct
//!   itself is reached directly from `db` — no command returns it to the
//!   frontend any more, so it is not re-exported here).

pub mod db;

pub use db::FrameSetReference;
pub use db::{clear_frame_set_reference, get_frame_set_reference, set_frame_set_reference};
