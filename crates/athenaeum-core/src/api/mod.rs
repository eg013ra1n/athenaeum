//! Shared command layer: one handler per command, wrapped by thin Tauri/Axum shims.
//! Handlers do NOT carry #[tracing::instrument] — boundary spans live on the wrappers.

use std::path::{Path, PathBuf};

pub mod scan_roots;
pub mod files;
pub mod calibration;
// Personal-sync commands (Stage I, task A7). Ungated: uses only db/sharing/sync.
pub mod sync;
// Full-app capture-node retention (task M4). Ungated: db/settings/sync only.
pub mod retention;
// Account commands (Stage II, task B4). Ungated: db/settings/account only.
pub mod account;
// These handler modules drive the render-only image pipeline (analysis,
// master integration, light calibration) and are gated with it.
#[cfg(feature = "render")]
pub mod analysis;
pub mod compute;
#[cfg(feature = "render")]
pub mod masters;
#[cfg(feature = "render")]
pub mod lights;
// Stage-II collaboration orchestration (slice 3, Task 4): linking, ranked
// suggestions, per-frame gate report, portal deep-link intents. Render-gated
// because it imports `api::lights` internals (`frame_cal_status`); the
// `crate::collab` core module and `db::collab` stay ungated. Keeping this gated
// preserves `cargo build -p perseus --no-default-features`.
#[cfg(feature = "render")]
pub mod collab;

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
            ApiError::NotFound(m) | ApiError::Invalid(m) | ApiError::Conflict(m)
            | ApiError::Forbidden(m) | ApiError::Internal(m) | ApiError::SignedOut(m) => {
                f.write_str(m)
            }
        }
    }
}
impl std::error::Error for ApiError {}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self { ApiError::Internal(e.to_string()) }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self { ApiError::Internal(format!("{e:#}")) }
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
                if roots.iter().any(|r| p.starts_with(r)) {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden(format!(
                        "path {} is outside the allowed roots", p.display()
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
    ctx.db.get().ok_or_else(|| ApiError::Internal("Database not initialized".into()))
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
        assert!(matches!(p.check(Path::new("/etc/passwd")), Err(ApiError::Forbidden(_))));
        assert!(PathPolicy::AllowAll.check(Path::new("/anything")).is_ok());
    }
}
