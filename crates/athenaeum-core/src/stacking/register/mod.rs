//! Registration v2 (spec §3): star detection on calibrated frames, a
//! quad-seeded correspondence search, RANSAC on the configured linear model,
//! a σ-weighted refit, optional polynomial distortion, per-frame QA and the
//! optional registered-frame writer. Per-frame services only — grouping,
//! reference selection, fan-out and persistence of a whole set belong to
//! the run orchestration. Coordinates are 0-based pixel centres;
//! `PixelMap::forward` maps subject → reference.
//!
//! The kernel (`align`, `detect`, `local_loop` and the registration types) comes from starloom; this module keeps the
//! three files that read Athenaeum items: `frame` (the catalog row), `wcs_seed` (the stored plate solve) and `writer`.

pub mod frame;
pub mod wcs_seed;
pub mod writer;

pub use starloom::stacking::register::*;
