//! Shared command layer: one handler per command, wrapped by thin Tauri/Axum shims.
//! Handlers do NOT carry #[tracing::instrument] — boundary spans live on the wrappers.

use std::path::{Path, PathBuf};

pub mod calibration;
pub mod files;
pub mod scan_roots;
// Sky-map spatial queries (imaging locations, rectangular selection). Ungated:
// db/models only.
pub mod spatial;
// Calendar month aggregation. Ungated: db/models/coordinates only.
pub mod calendar;
// Frame-set workflows (auto-generate clustering + session detection + the
// collab project-match suggestion). Render-gated ONLY because it calls
// `api::collab::find_matching_projects` and `api::collab` is render-gated;
// nothing here touches the image pipeline. Both transports build core with
// default features, so the gate is invisible to them.
#[cfg(feature = "render")]
pub mod frame_sets;
// Personal-sync commands (Stage I, task A7). Ungated: uses only db/sharing/sync.
pub mod sync;
// Background package preparation (transfer-prepare spec §3): the worker that
// stages + hashes a planned send after its row is already in the list.
pub mod sync_prepare;
// Frame-set send (spec 2026-08-28): the `PayloadEntry` currency `api::sync`'s
// package builder consumes. Ungated: models/package types only.
pub mod frame_set_send;
// Trigger policy for the whole-library content-hash index. Ungated: db +
// duplicates::backfill + the compute queue, plus `api::sync`'s own gate.
pub mod content_index;
// Account commands (Stage II, task B4). Ungated: db/settings/account only.
pub mod account;
// Stage-II collaboration package exchange — holder-side request-to-serve (slice 4,
// task 6): serve-dir reconstruction + authorize + enqueue. UNGATED (no render
// gate) — uses only db/sync/sharing/collab, so it compiles headless.
pub mod collab_exchange;
// These handler modules drive the render-only image pipeline (analysis,
// master integration, light calibration) and are gated with it.
#[cfg(feature = "render")]
pub mod analysis;
// Resolving an OBJECT name needs the bundled DSO catalog, which lives in
// `plate_solve` — gated on the same two features that module is.
#[cfg(all(feature = "render", feature = "solver"))]
pub mod objects;
pub mod compute;
#[cfg(feature = "render")]
pub mod lights;
#[cfg(feature = "render")]
pub mod masters;
// Shared blocking phase of a WBPP export (admission → plan resolution →
// placement), driven by both hosts from inside `spawn_blocking`. Render-gated
// because it calls `GenerationBatch::resolve`, which only exists under
// `render` (`export::file_organizer` itself stays ungated so a plain copy
// export still compiles headless).
#[cfg(feature = "render")]
pub mod export;
// Stage-II collaboration orchestration (slice 3, Task 4): linking, ranked
// suggestions, per-frame gate report, portal deep-link intents. The old
// rationale — an `api::lights` import for `frame_cal_status` — no longer holds:
// the gate's calibration status is a constant while collab publish is deferred
// (spec 2026-08-31 §8a, rework pending). What still forces the gate is
// `publish_collab_frames`'s call into the render-gated
// `api::sync::unique_rel_path`; the `crate::collab` core module and `db::collab`
// stay ungated. Keeping this gated preserves
// `cargo build -p perseus --no-default-features`.
#[cfg(feature = "render")]
pub mod collab;

// Slice-5 capstone: the three-instance collaboration E2E (publish → moderation →
// swarm delivery → project WBPP export) exercised in one process over the
// in-memory loopback transport. Test-only, and additionally render-gated because
// it drives `publish_collab_frames` (render-gated in `api::collab`).
#[cfg(all(test, feature = "render"))]
mod collab_e2e_tests;

// Task 9: the C1 relay-eviction regression canary — one `#[ignore]`d test binding
// two endpoints with the SAME device secret against a real relay (from
// `ATHENAEUM_TEST_RELAY`) and asserting the first observes the eviction. Owner-run
// against test-relay.artfrom.space; skipped (no-op) when the env var is unset.
#[cfg(test)]
mod relay_live_tests;

#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    Invalid(String),
    Conflict(String),
    Forbidden(String),
    Internal(String),
    /// No local account session (no device token, or the hub revoked it). Maps
    /// to HTTP 401 at the web boundary; the frontend re-shows the sign-in flow.
    SignedOut(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NotFound(m)
            | ApiError::Invalid(m)
            | ApiError::Conflict(m)
            | ApiError::Forbidden(m)
            | ApiError::Internal(m)
            | ApiError::SignedOut(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for ApiError {}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(format!("{e:#}"))
    }
}

/// Transport-specific path sandboxing policy for api handlers.
#[derive(Debug, Clone)]
pub enum PathPolicy {
    AllowAll,
    AllowedRoots(Vec<PathBuf>),
}

impl PathPolicy {
    /// Lexical, component-wise containment check (`Path::starts_with`).
    ///
    /// SECURITY PRECONDITION: the caller must canonicalize BOTH `p` and the
    /// allowed roots (resolving `..` and symlinks) before calling — this
    /// method does no resolution, so `/root/../../etc` would lexically pass.
    /// See athenaeum-web `add_scan_root` for the canonicalization pattern.
    pub fn check(&self, p: &Path) -> Result<(), ApiError> {
        match self {
            PathPolicy::AllowAll => Ok(()),
            PathPolicy::AllowedRoots(roots) => {
                // Windows canonicalize() yields \\?\-verbatim paths. Candidate
                // and roots are canonicalized at DIFFERENT sites, so one side
                // can be verbatim while the other is not — and
                // Prefix::VerbatimDisk never component-matches Prefix::Disk,
                // turning the sandbox into deny-all. Fold both sides first.
                let candidate = crate::api::scan_roots::normalize_path(p);
                if roots
                    .iter()
                    .any(|r| candidate.starts_with(crate::api::scan_roots::normalize_path(r)))
                {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden(format!(
                        "path {} is outside the allowed roots",
                        p.display()
                    )))
                }
            }
        }
    }
}

/// Fetch the shared `Database` handle out of `ServiceContext`, mirroring the
/// `state.ctx.db.get().ok_or("Database not initialized")?` expression used
/// throughout today's Tauri commands (e.g.
/// `crates/athenaeum-tauri/src/commands/scan_roots.rs:94`). `ServiceContext.db`
/// is a `OnceLock<Database>`, so `.get()` returns `Option<&Database>` — same
/// shape here, just mapped onto `ApiError::Internal` instead of a bare string.
///
/// NOTE for async handlers: do not hold the returned `&Database` (or its
/// `.conn()` guard) across an `.await` point — acquire, use, drop before
/// awaiting, or the future may fail Send/borrow checks.
pub fn db(ctx: &crate::services::ServiceContext) -> Result<&crate::db::Database, ApiError> {
    ctx.db
        .get()
        .ok_or_else(|| ApiError::Internal("Database not initialized".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn display_shows_message() {
        assert_eq!(ApiError::NotFound("frame 7".into()).to_string(), "frame 7");
    }

    #[test]
    fn path_policy_allows_and_forbids() {
        let p = PathPolicy::AllowedRoots(vec!["/data/astro".into()]);
        assert!(p.check(Path::new("/data/astro/lights")).is_ok());
        assert!(matches!(
            p.check(Path::new("/etc/passwd")),
            Err(ApiError::Forbidden(_))
        ));
        assert!(PathPolicy::AllowAll.check(Path::new("/anything")).is_ok());
    }

    #[test]
    fn path_policy_allows_inside_and_refuses_sibling() {
        let policy = PathPolicy::AllowedRoots(vec![PathBuf::from("/data/M31")]);
        assert!(policy.check(Path::new("/data/M31/x.fits")).is_ok());
        assert!(
            policy.check(Path::new("/data/M31_Ha/x.fits")).is_err(),
            "component-wise, not string prefix"
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_policy_matches_across_verbatim_and_plain_spellings() {
        let policy = PathPolicy::AllowedRoots(vec![PathBuf::from(r"\\?\C:\data")]);
        assert!(policy.check(Path::new(r"C:\data\x.fits")).is_ok());
        let policy2 = PathPolicy::AllowedRoots(vec![PathBuf::from(r"C:\data")]);
        assert!(policy2.check(Path::new(r"\\?\C:\data\x.fits")).is_ok());
    }
}
