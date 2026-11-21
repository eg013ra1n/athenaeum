//! Assistant access (MCP) on the web build: the three commands the Settings page calls, the plan window's two, and the `/mcp` service an
//! assistant connects to. The logic is core's (`athenaeum_core::api::mcp`, `mcp::McpHost`); this is the wrapper plus
//! `/mcp`'s own gate. The mirror of `crates/athenaeum-tauri/src/commands/mcp.rs`.
//!
//! `/mcp` is a `nest_service` outside `/api/`, so it is not behind `ATHENAEUM_API_KEY`: the key an assistant presents
//! is the MCP key (`McpHost::check_token`), a different secret, and the API key does not open it. The web host never
//! binds a socket, so turning access off or regenerating the key takes effect per request, here.

use std::sync::Arc;

use athenaeum_core::api::mcp as api;
use athenaeum_core::mcp::{McpHost, McpPlanView, McpStatus, PlanAnswer};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;

use crate::routes::api_err;
use crate::{McpSlot, WebAppState};

/// `/mcp` and the three commands before the host exists or the catalog is open.
const STARTING: &str = "Athenaeum is starting and has not opened its catalog yet.";
const OFF: &str = "Assistant access (MCP) is off in Athenaeum's Settings.";
const WRONG_KEY: &str = "Wrong or missing assistant access key.";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetMcpEnabledReq {
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetMcpPlanReq {
    #[serde(default)]
    plan_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerMcpPlanReq {
    plan_id: String,
    answer: PlanAnswer,
}

fn host(state: &WebAppState) -> Result<&Arc<McpHost>, (StatusCode, String)> {
    state.mcp.get().ok_or_else(|| {
        tracing::warn!("mcp command called before the host was built");
        (StatusCode::SERVICE_UNAVAILABLE, STARTING.to_string())
    })
}

/// POST /api/get_mcp_status
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_mcp_status(State(state): State<WebAppState>) -> Result<Json<McpStatus>, (StatusCode, String)> {
    Ok(Json(api::get_mcp_status(host(&state)?)))
}

/// POST /api/set_mcp_enabled
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_mcp_enabled(
    State(state): State<WebAppState>,
    Json(req): Json<SetMcpEnabledReq>,
) -> Result<Json<McpStatus>, (StatusCode, String)> {
    api::set_mcp_enabled(host(&state)?, req.enabled).map(Json).map_err(api_err)
}

/// POST /api/regenerate_mcp_token
#[tracing::instrument(skip_all, err(Debug))]
pub async fn regenerate_mcp_token(State(state): State<WebAppState>) -> Result<Json<McpStatus>, (StatusCode, String)> {
    api::regenerate_mcp_token(host(&state)?).map(Json).map_err(api_err)
}

/// POST /api/add_mcp_to_claude_desktop: desktop only (it writes the user's own Claude desktop file), so 501 here.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn add_mcp_to_claude_desktop() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, "add_mcp_to_claude_desktop is not available in web mode".to_string()))
}

/// POST /api/get_mcp_plan: the plan by id, or the newest (`planId` absent); `null` when there is none.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_mcp_plan(
    State(state): State<WebAppState>,
    Json(req): Json<GetMcpPlanReq>,
) -> Result<Json<Option<McpPlanView>>, (StatusCode, String)> {
    Ok(Json(api::get_mcp_plan(host(&state)?, req.plan_id)))
}

/// POST /api/answer_mcp_plan: the user's Apply or Cancel from the plan window.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn answer_mcp_plan(
    State(state): State<WebAppState>,
    Json(req): Json<AnswerMcpPlanReq>,
) -> Result<Json<McpPlanView>, (StatusCode, String)> {
    api::answer_mcp_plan(host(&state)?, req.plan_id, req.answer).map(Json).map_err(api_err)
}

/// The Streamable HTTP service at `/mcp`, behind [`require_mcp_access`]. Nothing cancels the service: a cancelled
/// token would leave `/mcp` dead after a re-enable, so turning access off or regenerating the key is the middleware's
/// per-request check (a live session's next request is refused).
pub(crate) fn mcp_router(slot: McpSlot) -> Router {
    let for_sessions = slot.clone();
    let service = StreamableHttpService::new(
        move || {
            for_sessions.get().map(|host| host.server()).ok_or_else(|| std::io::Error::other(STARTING))
        },
        Arc::new(LocalSessionManager::default()),
        // A proxy or container name is the host an assistant uses; the key is the gate, not the host name.
        StreamableHttpServerConfig::default().disable_allowed_hosts(),
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(slot, require_mcp_access))
}

fn refuse(status: StatusCode, reason: &'static str, message: &'static str) -> Response {
    // Deliberately a debug line: a port scanner must not flood the log, and the body already says what is wrong.
    tracing::debug!(reason, "mcp request refused");
    (status, message).into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")
}

/// `/mcp`'s gate: off is 403, a host without its catalog is 503, a missing or wrong `Authorization: Bearer` key is
/// 401. The API key (`X-API-Key`) is not this key.
async fn require_mcp_access(State(slot): State<McpSlot>, req: Request, next: Next) -> Response {
    let Some(host) = slot.get() else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "no_host", STARTING);
    };
    if !host.is_serving() {
        return refuse(StatusCode::FORBIDDEN, "off", OFF);
    }
    if host.source().is_err() {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "no_catalog", STARTING);
    }
    match bearer(req.headers()) {
        Some(presented) if host.check_token(presented) => next.run(req).await,
        _ => refuse(StatusCode::UNAUTHORIZED, "key", WRONG_KEY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::tests::{test_state, test_state_with_catalog};
    use crate::routes::build_router;
    use athenaeum_core::mcp::NEEDS_API_KEY;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use axum::response::Response;
    use axum::Router;
    use tower::ServiceExt; // oneshot

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;

    async fn api(app: &Router, command: &str, body: &str, api_key: Option<&str>) -> (StatusCode, String) {
        let mut req = Request::builder().method("POST").uri(format!("/api/{command}")).header("content-type", "application/json");
        if let Some(key) = api_key {
            req = req.header("X-API-Key", key);
        }
        let resp = app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let status = resp.status();
        (status, text(resp).await)
    }

    async fn text(resp: Response) -> String {
        String::from_utf8(to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
    }

    async fn mcp(app: &Router, bearer: Option<&str>, api_key: Option<&str>) -> (StatusCode, String) {
        let mut req = Request::builder()
            .method("POST")
            .uri("/mcp")
            // A real HTTP/1.1 client always sends it; rmcp refuses a request with neither Host nor an authority.
            .header("host", "athenaeum.example:3000")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(token) = bearer {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(key) = api_key {
            req = req.header("X-API-Key", key);
        }
        let resp = app.clone().oneshot(req.body(Body::from(INITIALIZE)).unwrap()).await.unwrap();
        let status = resp.status();
        // An accepted `initialize` answers with an event stream that stays open; its status is all a test needs.
        if status == StatusCode::OK {
            return (status, String::new());
        }
        (status, text(resp).await)
    }

    fn token_of(body: &str) -> String {
        let status: serde_json::Value = serde_json::from_str(body).unwrap();
        status["token"].as_str().expect("the web status carries the key").to_string()
    }

    /// Router over a state with an open catalog; the caller closes everything in order (`finish`).
    struct Fixture {
        app: Router,
        state: crate::WebAppState,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(api_key: Option<&str>) -> Self {
            let (state, dir) = test_state_with_catalog(api_key);
            Self { app: build_router(state.clone(), None), state, dir }
        }

        /// Handles first, then the temp dir (Windows cannot delete an open database).
        fn finish(self) {
            let Fixture { app, state, dir } = self;
            if let Some(host) = state.mcp.get() {
                host.shutdown();
            }
            drop(app);
            drop(state);
            drop(dir);
        }
    }

    #[tokio::test]
    async fn enable_command_refuses_without_api_key() {
        let fx = Fixture::new(None);
        let (status, body) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":true}"#, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, NEEDS_API_KEY);
        let (status, body) = api(&fx.app, "get_mcp_status", "{}", None).await;
        assert_eq!(status, StatusCode::OK);
        let status_json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status_json["state"], "off");
        assert_eq!(status_json["enabled"], false);
        assert_eq!(status_json["blockedReason"], NEEDS_API_KEY);
        assert_eq!(mcp(&fx.app, Some("anything"), None).await.0, StatusCode::FORBIDDEN);
        fx.finish();
    }

    #[tokio::test]
    async fn mcp_requires_its_token() {
        let fx = Fixture::new(Some("k"));
        let (status, body) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":true}"#, Some("k")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let token = token_of(&body);
        let (status, body) = mcp(&fx.app, None, None).await;
        assert_eq!((status, body.as_str()), (StatusCode::UNAUTHORIZED, "Wrong or missing assistant access key."));
        assert_eq!(mcp(&fx.app, Some("not-the-key"), None).await.0, StatusCode::UNAUTHORIZED);
        let (status, body) = mcp(&fx.app, Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        fx.finish();
    }

    #[tokio::test]
    async fn mcp_off_answers_403_even_with_token() {
        let fx = Fixture::new(Some("k"));
        // Never turned on: off.
        let (status, body) = mcp(&fx.app, None, None).await;
        assert_eq!((status, body.as_str()), (StatusCode::FORBIDDEN, "Assistant access (MCP) is off in Athenaeum's Settings."));
        let (_, body) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":true}"#, Some("k")).await;
        let token = token_of(&body);
        assert_eq!(mcp(&fx.app, Some(&token), None).await.0, StatusCode::OK);
        let (status, _) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":false}"#, Some("k")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(mcp(&fx.app, Some(&token), None).await.0, StatusCode::FORBIDDEN);
        fx.finish();
    }

    #[tokio::test]
    async fn api_key_does_not_open_mcp() {
        let fx = Fixture::new(Some("k"));
        let (status, _) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":true}"#, Some("k")).await;
        assert_eq!(status, StatusCode::OK);
        // The API key as the header the API accepts, and as a bearer: neither is the assistant key.
        assert_eq!(mcp(&fx.app, None, Some("k")).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(mcp(&fx.app, Some("k"), Some("k")).await.0, StatusCode::UNAUTHORIZED);
        fx.finish();
    }

    #[tokio::test]
    async fn off_then_on_serves_again() {
        let fx = Fixture::new(Some("k"));
        let (_, body) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":true}"#, Some("k")).await;
        let token = token_of(&body);
        api(&fx.app, "set_mcp_enabled", r#"{"enabled":false}"#, Some("k")).await;
        let (status, body) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":true}"#, Some("k")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(token_of(&body), token, "the key is kept across off and on");
        assert_eq!(mcp(&fx.app, Some(&token), None).await.0, StatusCode::OK);
        fx.finish();
    }

    #[tokio::test]
    async fn regenerated_key_refuses_the_old_one() {
        let fx = Fixture::new(Some("k"));
        let (_, body) = api(&fx.app, "set_mcp_enabled", r#"{"enabled":true}"#, Some("k")).await;
        let old = token_of(&body);
        assert_eq!(mcp(&fx.app, Some(&old), None).await.0, StatusCode::OK);
        let (status, body) = api(&fx.app, "regenerate_mcp_token", "{}", Some("k")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let new = token_of(&body);
        assert_ne!(old, new);
        assert_eq!(mcp(&fx.app, Some(&old), None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(mcp(&fx.app, Some(&new), None).await.0, StatusCode::OK);
        fx.finish();
    }

    /// Every `/api` command of assistant access sits behind `ATHENAEUM_API_KEY`: the key is the router's own state
    /// (`test_state`), so no process variable is set and no parallel test can see one.
    #[tokio::test]
    async fn every_mcp_api_route_needs_the_api_key() {
        let fx = Fixture::new(Some("k"));
        let routes = [
            ("get_mcp_status", "{}"),
            ("set_mcp_enabled", r#"{"enabled":false}"#),
            ("regenerate_mcp_token", "{}"),
            ("add_mcp_to_claude_desktop", "{}"),
            ("get_mcp_plan", "{}"),
            ("answer_mcp_plan", r#"{"planId":"plan-nope","answer":{"action":"cancel"}}"#),
        ];
        for (command, body) in routes {
            let (status, text) = api(&fx.app, command, body, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{command} without the key: {text}");
            let (status, text) = api(&fx.app, command, body, Some("wrong")).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{command} with a wrong key: {text}");
            let (status, text) = api(&fx.app, command, body, Some("k")).await;
            assert_ne!(status, StatusCode::UNAUTHORIZED, "{command} with the key: {text}");
        }
        fx.finish();
    }

    #[tokio::test]
    async fn add_to_claude_desktop_is_501_on_the_web() {
        let fx = Fixture::new(Some("k"));
        let (status, body) = api(&fx.app, "add_mcp_to_claude_desktop", "{}", Some("k")).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
        assert_eq!(body, "add_mcp_to_claude_desktop is not available in web mode");
        fx.finish();
    }

    #[tokio::test]
    async fn the_plan_commands_answer_over_the_router() {
        let fx = Fixture::new(Some("k"));
        // No plan yet: the newest plan is null, not an error.
        let (status, body) = api(&fx.app, "get_mcp_plan", "{}", Some("k")).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "null"));
        let (status, body) = api(&fx.app, "get_mcp_plan", r#"{"planId":"plan-nope"}"#, Some("k")).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "null"));
        // A plan the page opens: read it, cancel it, read it again.
        let host = fx.state.mcp.get().expect("the host");
        let (id, _) = host.plans.begin(host, athenaeum_core::mcp::PlanKind::BlackHole, "Move duplicates");
        let (status, body) = api(&fx.app, "get_mcp_plan", "{}", Some("k")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let plan: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!((plan["planId"].as_str(), plan["state"]["kind"].as_str()), (Some(id.as_str()), Some("preparing")));
        let cancel = format!(r#"{{"planId":"{id}","answer":{{"action":"cancel"}}}}"#);
        let (status, body) = api(&fx.app, "answer_mcp_plan", &cancel, Some("k")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let plan: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(plan["state"]["kind"], "cancelled");
        // A plan that is decided cannot be cancelled again (400); one that is gone is a 404.
        let (status, _) = api(&fx.app, "answer_mcp_plan", &cancel, Some("k")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let gone = r#"{"planId":"plan-nope","answer":{"action":"cancel"}}"#;
        let (status, _) = api(&fx.app, "answer_mcp_plan", gone, Some("k")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        fx.finish();
    }

    #[tokio::test]
    async fn mcp_before_the_catalog_answers_503() {
        // `test_state` has no catalog and no host: the server is up but has not opened anything.
        let app = build_router(test_state(Some("k")), None);
        let (status, body) = mcp(&app, Some("anything"), None).await;
        assert_eq!((status, body.as_str()), (StatusCode::SERVICE_UNAVAILABLE, "Athenaeum is starting and has not opened its catalog yet."));
        let (status, _) = api(&app, "get_mcp_status", "{}", Some("k")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
