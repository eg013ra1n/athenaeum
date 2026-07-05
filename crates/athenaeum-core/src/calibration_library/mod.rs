//! Calibration Library: fixed v1 master-file naming (`paths`) and
//! consolidated master FITS header construction (`headers`) for Phase 2
//! master calibration. `register` (Task 11) builds runtime plumbing on top
//! of these: registering a just-written master file's DB rows, provenance,
//! relink, and supersede.

pub mod headers;
pub mod light_cal;
pub mod light_headers;
pub mod paths;
pub mod register;
