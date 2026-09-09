//! Stacking pipeline (spec `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`).
//! Plan 2 lands the measurement stage: robust rejection (`robust`), the
//! PSF-signal estimators (`psf_signal`), per-frame measurement (`measure`)
//! and weighting / selection (`weights`). Orchestration, configuration and
//! persistence follow in later plans. Plan 3 adds `register` (registration
//! v2: detection, quad-seeded RANSAC alignment, distortion, QA, the
//! registered-frame writer). Plan 4 adds `integrate` (the group driver:
//! configuration, the Auto rejection rule, per-frame normalization and
//! weights, weighted banded integration per plane).

pub mod integrate;
pub mod measure;
pub mod psf_signal;
pub mod register;
pub mod robust;
pub mod weights;
