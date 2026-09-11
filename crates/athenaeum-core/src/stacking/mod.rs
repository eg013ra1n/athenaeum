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
//! the foundation local normalization's later M2 tasks build on). M3 Task 1
//! adds `drizzle` (spec §7, rulings R-M3-1..R-M3-3: the reference ↔
//! output-grid coordinate map, drop corners, forward mapping onto the
//! scaled output grid, exact convex-quad ∩ unit-pixel clipping, and the
//! 16×16 tabulated-kernel micro-drop table for `circle`/`gaussian`). M3
//! Task 2 adds `rej` (spec §6.2, ruling R-M3-8: the on-disk `.rej`
//! per-frame rejection-bitmap format `RejBitmapSet`/`RejBitmap`, and
//! `RejPlaneSink` — the `integration::source::RejectionBitSink`
//! implementation the engine's band loop now writes to when a sink is
//! supplied). M3 Task 3 adds `drizzle`'s own stage driver
//! (`drizzle_group`): per plane, per included frame, reads the calibrated
//! plane once and deposits it onto the scaled output grid through the
//! frame's own `PixelMap` — a whole-branch final fix wave item made this
//! tolerate a frame whose native geometry differs from the run's
//! reference (only `channels` has to match), mirroring the tolerance the
//! integration engine's own `RegisteredSource` has always had per frame.
//!
//! M4a Task 2's fix rounds add `prefilter`: math reference §5.1's optional
//! 3×3 median on the image the seed DETECTION runs on, a
//! sharpness-dependent suppression no single threshold can express. It
//! ships off — see that task's report for the two grids that left it
//! there.

pub mod config;
pub mod drizzle;
pub mod groups;
pub mod integrate;
pub mod ln;
pub mod master_cards;
pub mod measure;
pub mod paths;
pub mod plan;
pub mod prefilter;
pub mod provenance;
pub mod psf_signal;
pub mod register;
pub mod rej;
pub mod robust;
pub mod run;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod weights;
