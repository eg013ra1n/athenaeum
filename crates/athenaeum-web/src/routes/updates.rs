// Update route handlers — mirrors `athenaeum-tauri/src/commands/updates.rs`
// one-for-one. The check and the notes are core; install/restart are
// desktop-only and answer 501 here (spec §2.4): a Docker install is updated
// by pulling the image.

use athenaeum_core::updates::{self, HostInfo, HostKind, UpdateCheck, WhatsNew};
use axum::{extract::State, http::StatusCode, Json};

use crate::WebAppState;

const HOST: HostInfo<'static> = HostInfo {
    kind: HostKind::Web,
    git_hash: env!("ATHENAEUM_GIT_HASH"),
    installer: None,
};

pub const NOT_ON_WEB: &str = "updates are installed by pulling the image";

/// `POST /api/check_for_updates`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn check_for_updates(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<UpdateCheck>, (StatusCode, String)> {
    updates::check(&state.ctx, &HOST)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
}

/// `POST /api/get_whats_new`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_whats_new(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Option<WhatsNew>>, (StatusCode, String)> {
    updates::whats_new(&state.ctx)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `POST /api/get_release_notes`
#[tracing::instrument(skip_all)]
pub async fn get_release_notes(Json(_): Json<serde_json::Value>) -> Json<WhatsNew> {
    Json(updates::release_notes())
}

/// `POST /api/install_update` — 501 on web.
#[tracing::instrument(skip_all)]
pub async fn install_update(Json(_): Json<serde_json::Value>) -> (StatusCode, String) {
    tracing::warn!("install_update called on the web host");
    (StatusCode::NOT_IMPLEMENTED, NOT_ON_WEB.to_string())
}

/// `POST /api/restart_app` — 501 on web.
#[tracing::instrument(skip_all)]
pub async fn restart_app(Json(_): Json<serde_json::Value>) -> (StatusCode, String) {
    tracing::warn!("restart_app called on the web host");
    (StatusCode::NOT_IMPLEMENTED, NOT_ON_WEB.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn install_and_restart_are_not_implemented_on_web() {
        let (s, body) = install_update(Json(serde_json::json!({}))).await;
        assert_eq!(s, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(body, NOT_ON_WEB);
        let (s, _) = restart_app(Json(serde_json::json!({}))).await;
        assert_eq!(s, StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn release_notes_are_the_embedded_notes() {
        let Json(n) = get_release_notes(Json(serde_json::json!({}))).await;
        assert_eq!(n.version, env!("CARGO_PKG_VERSION"));
        assert!(n.notes.starts_with('*'));
    }

    #[test]
    fn web_host_is_never_installable() {
        assert_eq!(HOST.kind, HostKind::Web);
        assert!(HOST.installer.is_none());
    }
}
