//! Stage-II collaboration: hub collab client, verified membership snapshots,
//! and the quality gate. Catalog-side only — the exchange layer is slice 4.

pub mod authz;
// Automatic filter-name matching against a project's cached dictionary (P3).
// Pure and ungated — no render/solver dependency — so the headless build
// still carries it.
pub mod filters;
// The manifest `meta` builder (P4/P18): reads `plate_solves` through
// `plate_solve::storage`, which only exists under `render`+`solver`.
#[cfg(all(feature = "render", feature = "solver"))]
pub mod frame_meta;
pub mod gate;
pub mod hub_client;
pub mod snapshot;
