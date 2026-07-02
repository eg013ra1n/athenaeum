//! Opt-in `ATHENAEUM_API_KEY` auth middleware for `/api/*`.
//!
//! The key itself is parsed once in `Config::from_env` (`main.rs`) and
//! threaded into `WebAppState::api_key`. This middleware only needs that one
//! field, so it takes a small standalone [`ApiKeyState`] rather than the
//! full `WebAppState` — that keeps the middleware (and its tests) free of
//! any `ServiceContext`/DB setup.

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use percent_encoding::percent_decode_str;

/// State needed by [`require_api_key`] — just the configured key, if any.
#[derive(Clone)]
pub struct ApiKeyState {
    pub api_key: Option<String>,
}

/// Enforces the opt-in API-key contract on `/api/*`.
///
/// - `api_key` unset (the default) => every request passes through
///   unchanged; this is bit-for-bit today's fully-open behavior.
/// - `api_key` set => every request whose path starts with `/api/` must
///   present the key via `X-API-Key: <key>`, `Authorization: Bearer <key>`,
///   or — ONLY for `/api/events` — the query parameter `?api_key=<key>`.
///   The query-param path exists solely because browsers' `EventSource`
///   cannot set custom headers; it is deliberately not accepted on any
///   other endpoint since keys embedded in URLs leak into server/proxy
///   access logs.
/// - Non-`/api/` paths (the SPA static fallback: `index.html`, JS/CSS
///   bundles) are always exempt so the app shell can load before the user
///   has had a chance to supply a key. That `/api/` prefix check happens
///   here, inside the middleware, rather than relying on router
///   layer-ordering against the `.fallback_service` static handler in
///   `build_router` — ordering is easy to get subtly wrong across future
///   refactors, and getting it wrong here would fail OPEN silently.
pub async fn require_api_key(State(state): State<ApiKeyState>, req: Request, next: Next) -> Response {
    let Some(key) = state.api_key.as_deref() else {
        return next.run(req).await;
    };

    let path = req.uri().path().to_string();
    if !path.starts_with("/api/") {
        return next.run(req).await;
    }

    if request_has_valid_key(&req, &path, key) {
        return next.run(req).await;
    }

    // Deliberate, scoped exception to the project's never-swallow-errors
    // rule: logging every rejected request would let an internet-facing
    // port scanner flood the log with one line per probe. The 401 body
    // below tells a legitimate caller exactly what's wrong, and nothing
    // else in the codebase depends on auth rejections being logged.
    (StatusCode::UNAUTHORIZED, "invalid or missing API key").into_response()
}

fn request_has_valid_key(req: &Request, path: &str, key: &str) -> bool {
    if let Some(header_key) = req.headers().get("X-API-Key").and_then(|v| v.to_str().ok()) {
        if header_key == key {
            return true;
        }
    }

    if let Some(auth) = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(bearer) = auth.strip_prefix("Bearer ") {
            if bearer == key {
                return true;
            }
        }
    }

    // Query-param auth is intentionally restricted to /api/events (SSE) —
    // see the doc comment on require_api_key for why.
    if path == "/api/events" {
        if let Some(query) = req.uri().query() {
            for pair in query.split('&') {
                if let Some(raw_value) = pair.strip_prefix("api_key=") {
                    // The frontend builds this URL with `encodeURIComponent`
                    // (src/api/http.ts), which percent-encodes everything
                    // outside its unreserved set — including `+` (as `%2B`)
                    // and space (as `%20`). Decode as plain percent-encoding
                    // here, NOT form-urlencoding: form-urlencoding treats a
                    // literal `+` in the query string as a space, which
                    // would corrupt any raw `+` byte a differently-encoded
                    // client sent. `percent_decode_str` never does that
                    // substitution, matching what `encodeURIComponent`
                    // actually produces.
                    if let Ok(decoded) = percent_decode_str(raw_value).decode_utf8() {
                        if decoded == key {
                            return true;
                        }
                    }
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request as HttpRequest,
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use tower::ServiceExt; // oneshot

    async fn dummy_ok() -> &'static str {
        "ok"
    }

    /// A minimal router mirroring the shape of the real one for auth
    /// purposes: an `/api/test` handler, the real `/api/events` path, and a
    /// non-api fallback route — with `require_api_key` layered the same way
    /// `build_router` layers it (before any `.with_state` on the real
    /// router; here there's no router-level state at all to keep the test
    /// setup minimal).
    fn build_test_router(api_key: Option<&str>) -> Router {
        let state = ApiKeyState { api_key: api_key.map(str::to_string) };
        Router::new()
            .route("/api/test", get(dummy_ok))
            .route("/api/events", get(dummy_ok))
            .route("/index.html", get(dummy_ok))
            .layer(from_fn_with_state(state, require_api_key))
    }

    async fn body_text(resp: Response) -> String {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn get_uri(uri: &str) -> HttpRequest<Body> {
        HttpRequest::builder().uri(uri).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn key_set_no_credentials_is_401_with_expected_body() {
        let app = build_test_router(Some("secret"));
        let resp = app.oneshot(get_uri("/api/test")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_text(resp).await, "invalid or missing API key");
    }

    #[tokio::test]
    async fn key_set_x_api_key_header_correct_ok_wrong_401() {
        let app = build_test_router(Some("secret"));
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/test")
                    .header("X-API-Key", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/test")
                    .header("X-API-Key", "wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn key_set_authorization_bearer_correct_ok() {
        let app = build_test_router(Some("secret"));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/test")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn query_param_api_key_ok_only_on_events_path() {
        let app = build_test_router(Some("secret"));
        let resp = app.clone().oneshot(get_uri("/api/events?api_key=secret")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Same query param on a non-events /api/ endpoint must NOT authenticate.
        let resp = app.oneshot(get_uri("/api/test?api_key=secret")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn query_param_api_key_percent_decodes_special_characters() {
        // `key` contains characters `encodeURIComponent` (src/api/http.ts,
        // the only place this frontend builds this URL) always
        // percent-encodes: `+` -> %2B, `/` -> %2F, `=` -> %3D, space ->
        // %20. The query string below is the literal output of
        // `encodeURIComponent("a+b/c=d e")`. A form-urlencoded decoder
        // would turn `%2B` back into a literal `+` but then ALSO treat any
        // raw `+` in the query as a space — decoding this value with that
        // scheme would silently produce the wrong string. This test pins
        // percent-decoding (not form-urlencoding) as the correct choice.
        let key = "a+b/c=d e";
        let app = build_test_router(Some(key));
        let resp = app.oneshot(get_uri("/api/events?api_key=a%2Bb%2Fc%3Dd%20e")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn key_set_non_api_path_is_exempt_without_credentials() {
        let app = build_test_router(Some("secret"));
        let resp = app.oneshot(get_uri("/index.html")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn key_unset_everything_open_without_credentials() {
        let app = build_test_router(None);
        let resp = app.clone().oneshot(get_uri("/api/test")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app.oneshot(get_uri("/index.html")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
