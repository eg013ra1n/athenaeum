//! Perseus embedded web status page — router, auth, and read endpoints.
//!
//! A tiny [`axum`] server, bound to [`Config::web_bind`](crate::config::Config)
//! (loopback by default), that lets an operator inspect a headless capture node
//! from a browser. This task (9) lands the skeleton:
//!
//! - `GET /` — a placeholder HTML page ([`index_html`]); the interactive
//!   dashboard is Task 10.
//! - `GET /api/status` — capture dirs, live in-flight transfers, the current
//!   retention policy, and coarse package counts ([`StatusDto`]).
//! - `GET /api/sent` — outbound packages, newest first, optionally filtered by
//!   `?state=` ([`SentDto`]).
//! - `GET /api/history` — the transfer audit log, optionally filtered by
//!   `?query=` (filename) and `?direction=` ([`HistoryDto`]).
//! - Bearer-token auth ([`auth_layer`]): when a token is configured every
//!   request must present `Authorization: Bearer <token>`.
//!
//! # Contract for Task 10
//!
//! [`build_router`] and [`WebState`] are the seam Task 10 extends with write
//! handlers (retention edit, package delete). The auth `token` is **snapshotted
//! at spawn** — changing it needs an agent restart, which keeps the middleware
//! trivial (no shared mutable auth state). [`WebState`] already carries the
//! fields Task 10 needs (`config_path`, `config`, `retention_tx`) so the spawn
//! site in [`run`](crate::run) does not change when those handlers land. The
//! retention-run log (`retention_log` / `RetentionRunRecord`) is intentionally
//! **not** here yet — no read DTO needs it — and is left to Task 10.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Query, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use tokio::sync::watch;

use athenaeum_core::sync::store::StandaloneSyncStore;
use athenaeum_core::sync::{
    Direction, HistoryQuery, HistoryRow, OutboundRow, OutboundState, SyncEngineHandle, SyncStore,
};

use crate::config::{Config, RetentionConfig};

/// Default cap for `GET /api/sent` when the caller supplies no `?limit=`.
const DEFAULT_SENT_LIMIT: u32 = 500;
/// Default cap for `GET /api/history` when the caller supplies no `?limit=`.
const DEFAULT_HISTORY_LIMIT: u32 = 500;
/// Row window `GET /api/status` tallies its terminal counts over. The status
/// page is a summary, not a lifetime ledger — confirmed rows accrue forever
/// (retention deletes source files, never outbound rows), so counts over the
/// most recent N packages keep the endpoint bounded in time and memory.
const STATUS_SCAN_LIMIT: u32 = 5000;

/// Shared state for the status-page router. Task 10 extends the router with
/// write handlers over this same struct — hence fields (`config_path`,
/// `config`, `retention_tx`) this read-only task constructs but does not yet
/// read. See the module docs.
pub struct WebState {
    /// The durable sync store — source of the sent/history/counts reads.
    pub store: Arc<StandaloneSyncStore>,
    /// The running engine (its `status_snapshot` is the live in-flight list;
    /// Task 10 uses it to cancel/delete packages).
    pub engine: Arc<SyncEngineHandle>,
    /// Path to `perseus.toml` — Task 10's retention edit writes it via
    /// [`config_edit`](crate::config_edit). Unused by the read endpoints.
    pub config_path: PathBuf,
    /// The live config, behind an async lock so Task 10 can swap in an edited
    /// copy. The read endpoints only ever `read().await` the retention table.
    pub config: tokio::sync::RwLock<Config>,
    /// Retention live-edit channel (task 8). Task 10 pushes an edited
    /// [`RetentionConfig`] here so the running retention loop adopts it without
    /// a restart. Held now (cheap; agent-available at spawn), used by Task 10.
    pub retention_tx: watch::Sender<RetentionConfig>,
    /// Peer node id (hex) → friendly device name, for enriching history rows.
    /// Empty until Task 11 wires the hub device-name cache.
    pub device_names: HashMap<String, String>,
    /// The capture directories this node watches (for the status banner).
    pub capture_dirs: Vec<PathBuf>,
}

/// `GET /api/status` payload: the operator's one-screen picture of the node.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    /// Watched capture directories (as display strings).
    capture_dirs: Vec<String>,
    /// Live non-terminal packages (queued/announced/transferring).
    in_flight: Vec<SentDto>,
    /// The current retention policy + tuning.
    retention: RetentionDto,
    /// Coarse package counts (see [`CountsDto`]).
    counts: CountsDto,
}

/// The retention table, flattened for the status page. `policy` is the same
/// snake_case string the TOML uses.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetentionDto {
    policy: String,
    dry_run: bool,
    soak_opt_in: bool,
    keep_days: u32,
    disk_max_pct: u8,
    interval_secs: u64,
}

/// Coarse package tallies. `queued` is exact (derived from the live in-flight
/// list); `confirmedTotal`/`failedTotal` are over the most recent
/// [`STATUS_SCAN_LIMIT`] packages — a summary, not an exact lifetime total.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CountsDto {
    confirmed_total: u64,
    failed_total: u64,
    queued: u64,
}

/// One outbound package row, for `GET /api/sent` and the status page's
/// in-flight list.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SentDto {
    id: i64,
    package_ref: String,
    state: String,
    attempts: u32,
    created_at: String,
    confirmed_at: Option<String>,
    /// The single safe-to-delete predicate surfaced to the UI: only a
    /// `confirmed` (fully received by the peer) package may be deleted.
    deletable: bool,
}

/// One transfer-history row, for `GET /api/history`.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryDto {
    filename: String,
    object: Option<String>,
    peer_device: String,
    /// Friendly name for `peer_device`, when known (Task 11); else `None`.
    peer_name: Option<String>,
    direction: String,
    bytes: u64,
    started_at: String,
    finished_at: Option<String>,
    /// `finished - started` in seconds when both stamps parse as RFC3339; else
    /// `None` (never a panic).
    duration_secs: Option<f64>,
    outcome: String,
}

/// Build the status-page router. `token` is snapshotted from
/// [`Config::web_token`](crate::config::Config) at spawn time — an auth change
/// needs an agent restart, which keeps [`auth_layer`] free of shared mutable
/// state. Task 10 adds write routes onto this same router + [`WebState`].
pub fn build_router(state: Arc<WebState>, token: Option<String>) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/api/status", get(api_status))
        .route("/api/sent", get(api_sent))
        .route("/api/history", get(api_history))
        .with_state(state)
        .layer(axum::middleware::from_fn(move |req, next| {
            auth_layer(token.clone(), req, next)
        }))
}

/// Bearer-token gate. With `token = None` (the loopback default) every request
/// passes through. With `Some(expected)`, a request must present
/// `Authorization: Bearer <expected>` or it is refused `401` — no logging, so a
/// port scanner cannot flood the log one line per probe (the 401 body tells a
/// legitimate caller exactly what is wrong).
async fn auth_layer(token: Option<String>, req: Request, next: Next) -> Response {
    let Some(expected) = token.as_deref() else {
        return next.run(req).await;
    };
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if presented == Some(expected) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "invalid or missing bearer token").into_response()
    }
}

/// `GET /` — placeholder status page. The interactive dashboard is Task 10.
async fn index_html() -> Html<&'static str> {
    Html(include_str!("web/index.html"))
}

/// `GET /api/status`
async fn api_status(
    State(state): State<Arc<WebState>>,
) -> Result<Json<StatusDto>, (StatusCode, String)> {
    let retention = state.config.read().await.retention.clone();

    // Live, complete in-flight picture (non-terminal rows, no cap).
    let in_flight_rows = state.engine.status_snapshot().map_err(|e| {
        tracing::error!(error = %e, "web status: read in-flight failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let queued = in_flight_rows
        .iter()
        .filter(|r| r.state == OutboundState::Queued)
        .count() as u64;
    let in_flight = in_flight_rows.iter().map(to_sent_dto).collect();

    // Terminal counts over a bounded recent window (see STATUS_SCAN_LIMIT).
    let recent = state.store.all_outbound(STATUS_SCAN_LIMIT).map_err(|e| {
        tracing::error!(error = %e, "web status: read outbound window failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let confirmed_total = recent
        .iter()
        .filter(|r| r.state == OutboundState::Confirmed)
        .count() as u64;
    let failed_total = recent
        .iter()
        .filter(|r| r.state == OutboundState::Failed)
        .count() as u64;

    Ok(Json(StatusDto {
        capture_dirs: state
            .capture_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        in_flight,
        retention: RetentionDto {
            policy: crate::config_edit::policy_str(&retention.policy).to_string(),
            dry_run: retention.dry_run,
            soak_opt_in: retention.i_have_verified_the_soak,
            keep_days: retention.keep_days,
            disk_max_pct: retention.disk_max_pct,
            interval_secs: retention.interval_secs,
        },
        counts: CountsDto {
            confirmed_total,
            failed_total,
            queued,
        },
    }))
}

/// Query string for `GET /api/sent`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SentQuery {
    /// Restrict to one state (`queued`/`announced`/`transferring`/`confirmed`/
    /// `failed`). Absent → every state.
    state: Option<String>,
    /// Row cap; defaults to [`DEFAULT_SENT_LIMIT`].
    limit: Option<u32>,
}

/// `GET /api/sent`
async fn api_sent(
    State(state): State<Arc<WebState>>,
    Query(q): Query<SentQuery>,
) -> Result<Json<Vec<SentDto>>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(DEFAULT_SENT_LIMIT);
    let rows = state.store.all_outbound(limit).map_err(|e| {
        tracing::error!(error = %e, "web sent: read outbound failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let filter = q.state.as_deref().filter(|s| !s.is_empty());
    let dtos: Vec<SentDto> = rows
        .iter()
        .filter(|r| filter.is_none_or(|s| r.state.as_str() == s))
        .map(to_sent_dto)
        .collect();
    Ok(Json(dtos))
}

/// Query string for `GET /api/history`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryQ {
    /// Exact filename to filter on. `search_history` AND-combines its filters,
    /// so a single free-text box maps to filename (the most useful axis), not
    /// filename-and-object (which would require both columns to equal it).
    query: Option<String>,
    /// Restrict to one direction (`sent`/`received`). Invalid → `400`.
    direction: Option<String>,
    /// Row cap; defaults to [`DEFAULT_HISTORY_LIMIT`].
    limit: Option<u32>,
}

/// `GET /api/history`
async fn api_history(
    State(state): State<Arc<WebState>>,
    Query(q): Query<HistoryQ>,
) -> Result<Json<Vec<HistoryDto>>, (StatusCode, String)> {
    let direction = match q.direction.as_deref().filter(|d| !d.is_empty()) {
        None => None,
        Some(d) => Some(Direction::from_db(d).map_err(|e| {
            tracing::error!(error = %e, direction = d, "web history: bad direction filter");
            (StatusCode::BAD_REQUEST, e.to_string())
        })?),
    };
    let hq = HistoryQuery {
        filename: q.query.filter(|s| !s.is_empty()),
        object: None,
        direction,
        peer: None,
        limit: q.limit.unwrap_or(DEFAULT_HISTORY_LIMIT),
    };
    let rows = state.store.search_history(hq).map_err(|e| {
        tracing::error!(error = %e, "web history: search failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let dtos = rows
        .iter()
        .map(|r| to_history_dto(r, &state.device_names))
        .collect();
    Ok(Json(dtos))
}

/// Map an [`OutboundRow`] to its wire DTO. `deletable` is the single
/// safe-to-delete predicate: only `confirmed` packages.
fn to_sent_dto(r: &OutboundRow) -> SentDto {
    SentDto {
        id: r.id,
        package_ref: r.package_ref.clone(),
        state: r.state.as_str().to_string(),
        attempts: r.attempts,
        created_at: r.created_at.clone(),
        confirmed_at: r.confirmed_at.clone(),
        deletable: r.state == OutboundState::Confirmed,
    }
}

/// Map a [`HistoryRow`] to its wire DTO, resolving a friendly peer name (when
/// known) and computing the transfer duration.
fn to_history_dto(r: &HistoryRow, device_names: &HashMap<String, String>) -> HistoryDto {
    HistoryDto {
        filename: r.filename.clone(),
        object: r.object.clone(),
        peer_device: r.peer_device.clone(),
        peer_name: device_names.get(&r.peer_device).cloned(),
        direction: r.direction.as_str().to_string(),
        bytes: r.bytes,
        started_at: r.started_at.clone(),
        finished_at: r.finished_at.clone(),
        duration_secs: duration_secs(&r.started_at, r.finished_at.as_deref()),
        outcome: r.outcome.clone(),
    }
}

/// `finished - started` in seconds, or `None` when the row is unfinished or
/// either stamp fails to parse as RFC3339. Never panics.
fn duration_secs(started: &str, finished: Option<&str>) -> Option<f64> {
    let finished = finished?;
    let s = chrono::DateTime::parse_from_rfc3339(started).ok()?;
    let f = chrono::DateTime::parse_from_rfc3339(finished).ok()?;
    Some((f - s).num_milliseconds() as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use athenaeum_core::sharing::loopback::LoopbackNetwork;
    use athenaeum_core::sharing::SharingTransport;
    use athenaeum_core::sync::SyncEngine; // Direction/HistoryRow come via `super::*`
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt; // for `oneshot`

    const PEER: [u8; 32] = [5u8; 32];

    /// A `WebState` over a temp store seeded with one confirmed + one
    /// transferring outbound row and two history rows (one sent, one received).
    /// The engine is spawned over an EMPTY store so its crash-resume finds
    /// nothing to re-drive; the rows are seeded directly afterwards, invisible
    /// to the idle worker (it never re-polls the DB). Returns the tempdir guard
    /// so the DB file outlives the test.
    async fn test_state() -> (Arc<WebState>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());

        // Spawn the engine on the empty store (nothing to resume), then seed.
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let engine = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            transport,
            PEER,
        ));

        let confirmed = store.enqueue("pkg-confirmed", PEER).unwrap();
        store.confirm(confirmed, &[]).unwrap();
        let transferring = store.enqueue("pkg-transferring", PEER).unwrap();
        store.set_state(transferring, OutboundState::Transferring).unwrap();

        store
            .append_history(HistoryRow {
                frame_uuid: "uuid-1".into(),
                filename: "frame-0001.fits".into(),
                object: Some("M42".into()),
                peer_device: "aa".repeat(32),
                direction: Direction::Sent,
                bytes: 1000,
                started_at: "2026-07-08T10:00:00.000Z".into(),
                finished_at: Some("2026-07-08T10:00:02.500Z".into()),
                outcome: "sent".into(),
            })
            .unwrap();
        store
            .append_history(HistoryRow {
                frame_uuid: "uuid-2".into(),
                filename: "other-0002.fits".into(),
                object: Some("M31".into()),
                peer_device: "bb".repeat(32),
                direction: Direction::Received,
                bytes: 2000,
                started_at: "2026-07-08T11:00:00.000Z".into(),
                finished_at: None, // unfinished → durationSecs is None
                outcome: "ingested".into(),
            })
            .unwrap();

        let config = Config::from_toml_str(&sample_toml(tmp.path())).unwrap();
        let state = Arc::new(WebState {
            store,
            engine,
            config_path: tmp.path().join("perseus.toml"),
            config: tokio::sync::RwLock::new(config.clone()),
            retention_tx: watch::channel(config.retention.clone()).0,
            device_names: HashMap::new(),
            capture_dirs: config.capture_dirs_resolved(),
        });
        (state, tmp)
    }

    fn sample_toml(dir: &std::path::Path) -> String {
        // `dir` exists (tempdir), so the capture-dir existence check passes.
        format!(
            "capture_dir = \"{}\"\ndata_dir = \"{}\"\npairing_ticket = \"t\"\nmode = \"auto\"\n[retention]\npolicy = \"keep_days\"\ndry_run = true\nkeep_days = 21\n",
            dir.display(),
            dir.display()
        )
    }

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn status_endpoint_shape() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);
        let res = app
            .oneshot(HttpRequest::builder().uri("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert!(v["captureDirs"].is_array());
        assert_eq!(v["retention"]["policy"], "keep_days");
        assert_eq!(v["retention"]["keepDays"], 21);
        assert!(v["inFlight"].is_array());
        // One transferring row is in flight; the confirmed one is not.
        assert_eq!(v["inFlight"].as_array().unwrap().len(), 1);
        assert_eq!(v["inFlight"][0]["state"], "transferring");
        assert_eq!(v["inFlight"][0]["deletable"], false);
        assert_eq!(v["counts"]["confirmedTotal"], 1);
        assert_eq!(v["counts"]["queued"], 0);
    }

    #[tokio::test]
    async fn bearer_required_when_token_set() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, Some("s3cret".to_string()));
        let unauth = app
            .clone()
            .oneshot(HttpRequest::builder().uri("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let wrong = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .header("authorization", "Bearer nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let auth = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .header("authorization", "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(auth.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn no_token_allows_unauthenticated() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);
        let res = app
            .oneshot(HttpRequest::builder().uri("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "loopback (no token) needs no auth");
    }

    #[tokio::test]
    async fn sent_lists_all_states_and_filters_by_state() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);

        // Unfiltered → both rows, with their state strings + deletable flag.
        let res = app
            .clone()
            .oneshot(HttpRequest::builder().uri("/api/sent").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let states: Vec<&str> = rows.iter().map(|r| r["state"].as_str().unwrap()).collect();
        assert!(states.contains(&"confirmed"));
        assert!(states.contains(&"transferring"));
        // deletable is exactly `state == confirmed`.
        for r in rows {
            let expect = r["state"] == "confirmed";
            assert_eq!(r["deletable"], expect);
        }

        // Filtered → only the confirmed one.
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/sent?state=confirmed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "confirmed");
        assert_eq!(rows[0]["deletable"], true);
    }

    #[tokio::test]
    async fn history_filters_and_computes_duration_and_peer_name() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);

        // Unfiltered → both rows.
        let res = app
            .clone()
            .oneshot(HttpRequest::builder().uri("/api/history").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = body_json(res).await;
        assert_eq!(v.as_array().unwrap().len(), 2);
        // The finished 'sent' row carries a 2.5s duration; peerName is None
        // (no device-name cache until Task 11).
        let sent = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["direction"] == "sent")
            .unwrap();
        assert_eq!(sent["filename"], "frame-0001.fits");
        assert_eq!(sent["durationSecs"], 2.5);
        assert!(sent["peerName"].is_null());
        // The unfinished 'received' row has no duration.
        let recv = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["direction"] == "received")
            .unwrap();
        assert!(recv["durationSecs"].is_null());

        // Exact-filename filter.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/history?query=frame-0001.fits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["filename"], "frame-0001.fits");

        // Direction filter.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/history?direction=received")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["direction"], "received");

        // Bad direction → 400.
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/history?direction=sideways")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn index_is_served() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);
        let res = app
            .oneshot(HttpRequest::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
