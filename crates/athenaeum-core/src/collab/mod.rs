//! Stage-II collaboration: hub collab client, verified membership snapshots,
//! and the quality gate. Catalog-side only — the exchange layer is slice 4.

pub mod authz;
// A stateful fake of the hub's collab v3 api for tests (plan P16).
#[cfg(test)]
pub mod fake_hub;
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
// The live exchange (wave 3): retry back-off, the claim digest, and the REST
// wire shapes for holders/versions/presence. Pure and ungated.
pub mod live;
pub mod snapshot;
