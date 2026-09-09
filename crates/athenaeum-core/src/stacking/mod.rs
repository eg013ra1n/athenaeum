//! Stacking pipeline (spec `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`).
//! Plan 2 lands the measurement stage: robust rejection (`robust`), the
//! PSF-signal estimators (`psf_signal`), per-frame measurement (`measure`)
//! and weighting / selection (`weights`). Orchestration, configuration and
//! persistence follow in later plans. Plan 3 adds `register` (registration
//! v2: detection, quad-seeded RANSAC alignment, distortion, QA, the
//! registered-frame writer). Plan 4 adds `integrate` (the group driver:
//! configuration, the Auto rejection rule, per-frame normalization and
//! weights, weighted banded integration per plane) and `master_cards` (the
//! master-light header, §9.5 file naming, and the master/rejection-map
//! writers). Plan 5a (M1 orchestration) adds `config` (spec §9.2/§9.3: the
//! whole `StackingConfig` tree, its built-in presets, whole-config
//! precedence over a stored set/global override, and the per-stage config
//! hashes artifacts key off) and `groups` (spec §2: integration groups from
//! the catalog — grouping keys, the group-key string, the frame-set slug —
//! plus, `cfg(test)`, the catalog fixture builder every later task in this
//! plan reuses).

pub mod config;
pub mod groups;
pub mod integrate;
pub mod master_cards;
pub mod measure;
pub mod psf_signal;
pub mod register;
pub mod robust;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod weights;
