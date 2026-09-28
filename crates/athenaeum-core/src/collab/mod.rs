//! Stage-II collaboration: hub collab client, verified membership snapshots,
//! and the quality gate. Catalog-side only — the exchange layer is slice 4.

pub mod authz;
// The ONE per-frame contributor-state derivation (spec 2026-09-28 §8.1): pure,
// no DB/render dependency, so the headless build carries it — used by both
// the frame set's Project block/column and the project page's own-frames
// table (`api::collab::get_frame_set_project_status`,
// `api::collab_live::surface::list_collab_frames`).
pub mod contributor_state;
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
// The per-request serve check of the collab provider (wave 3, spec §9.3):
// a pure decision plus the oracle trait the provider consumer asks.
pub mod serve;
// The receive-side scheduler (wave 3, spec §7): a pure deterministic core
// plus its seeded simulation. Pure and ungated — the headless build carries it.
pub mod scheduler;
pub mod snapshot;
// On-disk state of the Collaboration root itself (wave 3, spec §9): the
// storage marker (available/read-only/unavailable, another device's disk
// refused, P22). Pure and ungated — no render/solver dependency.
pub mod storage;
