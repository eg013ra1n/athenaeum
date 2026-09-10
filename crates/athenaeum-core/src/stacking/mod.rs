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
//! hashes artifacts key off), `groups` (spec §2: integration groups from
//! the catalog — grouping keys, the group-key string, the frame-set slug —
//! plus, `cfg(test)`, the catalog fixture builder every later task in this
//! plan reuses), `paths` (spec §9.6: working/output folder resolution
//! and validation, the on-disk working layout, free-space and byte-usage
//! probes, a run's byte-footprint estimate, and working-folder cleanup),
//! and `plan` (spec §2/§9.3/§9.4: `StackingPlan` — the groups, gate
//! blockers and stale-stage report a run would face — plus the three
//! per-stage config-hash helpers Tasks 6-7 reuse verbatim). Plan 5a Task 6
//! adds `provenance` (spec §9.1: `RunSummary` — `summary_json` and
//! `runs/run-<id>.json` are the same document) and `run` (the run thread:
//! queue admission, `stacking-progress`/`stacking-complete` events, and
//! stage 1 — calibrate, with artifact reuse; Tasks 7-8 add the rest of the
//! pipeline to the same file). M2 Task 1 adds `ln` (spec §5.2: `LnGrid`,
//! the bicubic B-spline evaluator over its coarse stride grid, and the
//! `.athln` sidecar one frame's per-channel grids round-trip through —
//! the foundation local normalization's later M2 tasks build on).

pub mod config;
pub mod groups;
pub mod integrate;
pub mod ln;
pub mod master_cards;
pub mod measure;
pub mod paths;
pub mod plan;
pub mod provenance;
pub mod psf_signal;
pub mod register;
pub mod robust;
pub mod run;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod weights;
