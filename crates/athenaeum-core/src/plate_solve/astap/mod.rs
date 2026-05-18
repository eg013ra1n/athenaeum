//! ASTAP-port plate solver (per-trial gnomonic-consistent quad matching).
//!
//! A faithful Rust port of ASTAP's command-line blind solver: an autoFOV
//! ladder × square-spiral sky search where, at each (FOV, sky-cell) trial,
//! the catalog cone is gnomonically projected onto the *same flat plane as
//! the image* and quads are built with the *same function on both sides*
//! (the verified fix for the wide-field/dense failures of the legacy
//! prebuilt-great-circle-index path).
//!
//! Lands behind `PlateSolveConfig.solver_backend == "astap"`; the legacy
//! path is byte-identical while the flag is `"legacy"` (the default). The
//! `index: &QuadIndex` parameter is accepted for API compatibility and
//! ignored here (per-trial quads make a prebuilt index unnecessary).
//!
//! Submodules (filled in by subsequent tasks):
//!  - `fov_ladder`    — ASTAP autoFOV rung generator
//!  - `sky_search`    — ASTAP square-spiral sky-cell generator
//!  - `catalog_source`— pluggable catalog-quad source (Tycho-2 now, Gaia later)
//!  - `trial`         — per-cell project→quad→match→fit→verify
//!  - `refine`        — second full-image solve → WCS

pub mod catalog_source;
pub mod fov_ladder;
pub mod sky_search;

use std::sync::Arc;

use anyhow::Result;

use astroimage::platesolving::SolveHints;

use crate::catalog::CatalogEngine;
use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::quad_index::QuadIndex;
use crate::plate_solve::service::SolveResult;

/// Entry point for the ASTAP-port backend. Same signature as
/// [`crate::plate_solve::service::solve_frame_with_hints`] so the dispatch
/// in `solve_frame_with_hints` is a one-line delegation.
pub fn solve(
    _frame: &Frame,
    _file_path: &str,
    _hints: &SolveHints,
    _catalog: &CatalogEngine,
    _index: &QuadIndex,
    _config: &PlateSolveConfig,
    _thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> Result<SolveResult> {
    anyhow::bail!(
        "astap solver backend not yet implemented (set solver_backend = \"legacy\")"
    )
}
