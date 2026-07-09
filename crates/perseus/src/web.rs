//! Perseus embedded web status page — router, auth, read + write endpoints.
//!
//! A tiny [`axum`] server, bound to [`Config::web_bind`](crate::config::Config)
//! (loopback by default), that lets an operator inspect and lightly manage a
//! headless capture node from a browser. The page ([`index_html`]) renders four
//! sections — status banner, sent packages, transfer history, and the retention
//! panel (policy editor + recent-pass log) — over these endpoints:
//!
//! - `GET /` — the static, data-free HTML/JS page shell. **Auth-exempt** (see
//!   below) so a browser can load it and then prompt for the token.
//! - `GET /api/status` — capture dirs, live in-flight transfers, the current
//!   retention policy, and coarse package counts ([`StatusDto`]).
//! - `GET /api/sent` — outbound packages, newest first, optionally filtered by
//!   `?state=` ([`SentDto`]).
//! - `GET /api/history` — the transfer audit log, optionally filtered by
//!   `?query=` (filename) and `?direction=` ([`HistoryDto`]).
//! - `GET`/`PUT /api/retention/policy` — read the live retention config +
//!   read-only soak gate ([`PolicyDto`]); a whitelisted [`RetentionEdit`] is
//!   applied to `perseus.toml` and adopted live. Live deletion can never be
//!   enabled here (the edit carries no soak field).
//! - `GET /api/retention/log` — the recent retention-pass ring buffer
//!   ([`RetentionRunRecord`], newest-first).
//! - `GET`/`PUT /api/capture-dirs` — the configured vs. running capture
//!   directories + a `restartPending` flag ([`CaptureDirsDto`]); a PUT rewrites
//!   `perseus.toml`'s capture selection and adopts it into the live config, but
//!   the watchers keep their spawn-time dirs until restart (restart-to-apply).
//! - `POST /api/delete` — delete the source capture files of CONFIRMED packages
//!   through the same confirmed-only deleter retention uses ([`DeleteReport`]).
//!
//! # Auth
//!
//! Bearer-token auth ([`auth_layer`]): when a token is configured, every
//! `/api/*` request must present `Authorization: Bearer <token>`. `GET /` is the
//! sole exemption — the static page shell must load without a token so the
//! browser's JS can then supply one on the `/api/*` calls. The `token` is
//! **snapshotted at spawn** — changing it needs an agent restart, which keeps
//! the middleware trivial (no shared mutable auth state).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Query, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use tokio::sync::watch;

use athenaeum_core::package::MANIFEST_FILENAME;
use athenaeum_core::sync::store::StandaloneSyncStore;
use athenaeum_core::sync::{
    Direction, HistoryQuery, HistoryRow, OutboundRow, OutboundState, SyncEngineHandle, SyncStore,
};

use crate::config::{Config, RetentionConfig};
use crate::config_edit::{apply_capture_dirs_edit, apply_retention_edit, RetentionEdit};
use crate::run::{delete_confirmed_packages, DeleteReport};
use crate::seen::SeenStore;

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
    /// Perseus's stat-aware seen store (source-file linkage). The manual-delete
    /// endpoint (`POST /api/delete`) resolves a confirmed package back to its
    /// source capture file through this, via the exact same deleter retention
    /// uses ([`delete_confirmed_packages`](crate::run::delete_confirmed_packages)).
    pub seen: Arc<SeenStore>,
    /// Rolling record (cap 50, newest-first) of the retention loop's recent
    /// passes, surfaced read-only at `GET /api/retention/log`. The retention loop
    /// in [`crate::run`] push-fronts each pass; this task is read-only.
    pub retention_log: Arc<Mutex<VecDeque<RetentionRunRecord>>>,
}

/// One recorded retention pass, for `GET /api/retention/log`. Built by the
/// retention loop ([`crate::run`]) from the pass's `RetentionOutcome` and
/// push-fronted into [`WebState::retention_log`] (cap 50, newest-first).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionRunRecord {
    /// RFC3339 timestamp of when the pass completed.
    pub at: String,
    /// Whether the pass ran in dry-run mode (deleted nothing).
    pub dry_run: bool,
    /// The policy label in force for the pass (same snake_case string as TOML).
    pub policy: String,
    /// Source files actually removed this pass (empty in dry-run).
    pub deleted: Vec<String>,
    /// Confirmed candidates the policy deemed eligible this pass.
    pub would_delete: Vec<String>,
    /// Pass-level failures / warnings (a failed tick, or disk still over cap).
    pub errors: Vec<String>,
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
    // `GET /` is the static, data-free page shell — it is deliberately EXEMPT
    // from the bearer layer. On any token-protected (non-loopback) deployment a
    // browser navigation must be able to load the page so its JS token prompt
    // can run; gating `/` would 401 that navigation before any JS loads, making
    // the README's documented flow dead on arrival. Every `/api/*` route (which
    // carries actual node data) stays behind [`auth_layer`].
    let api = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/sent", get(api_sent))
        .route("/api/history", get(api_history))
        .route(
            "/api/retention/policy",
            get(api_get_retention_policy).put(api_put_retention_policy),
        )
        .route("/api/retention/log", get(api_retention_log))
        .route(
            "/api/capture-dirs",
            get(api_get_capture_dirs).put(api_put_capture_dirs),
        )
        .route("/api/delete", post(api_delete))
        .route("/api/retry", post(api_retry))
        .layer(axum::middleware::from_fn(move |req, next| {
            auth_layer(token.clone(), req, next)
        }));
    Router::new()
        .route("/", get(index_html))
        .merge(api)
        .with_state(state)
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

/// `GET`/`PUT /api/retention/policy` payload: the writable retention knobs plus
/// the read-only two-key soak gate. `soakOptIn`/`liveDeletionPossible` mirror
/// the config's soak state and are **never** writable here — [`RetentionEdit`]
/// carries no soak field, so live deletion can only ever be enabled by
/// hand-editing `perseus.toml`. That guarantee is the whole point of the
/// separate read-only surface.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PolicyDto {
    policy: String,
    keep_days: u32,
    disk_max_pct: u8,
    interval_secs: u64,
    dry_run: bool,
    /// The operator's typed soak acknowledgement (`i_have_verified_the_soak`,
    /// a `perseus.toml`-only key). Read-only here.
    soak_opt_in: bool,
    /// Whether confirmed sources are actually being deleted right now — the soak
    /// opt-in is set AND the pass is live (not dry-run). Derived, read-only.
    live_deletion_possible: bool,
}

impl PolicyDto {
    fn from_retention(r: &RetentionConfig) -> Self {
        Self {
            policy: crate::config_edit::policy_str(&r.policy).to_string(),
            keep_days: r.keep_days,
            disk_max_pct: r.disk_max_pct,
            interval_secs: r.interval_secs,
            dry_run: r.dry_run,
            soak_opt_in: r.i_have_verified_the_soak,
            live_deletion_possible: r.i_have_verified_the_soak && !r.dry_run,
        }
    }
}

/// `POST /api/delete` request body.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteRequest {
    /// Outbound-row ids to delete. Non-confirmed / unknown ids are rejected
    /// per-id in the [`DeleteReport`] response, never deleted.
    ids: Vec<i64>,
}

/// `GET /api/retention/policy` — current retention config + read-only soak gate.
async fn api_get_retention_policy(State(state): State<Arc<WebState>>) -> Json<PolicyDto> {
    let retention = state.config.read().await.retention.clone();
    Json(PolicyDto::from_retention(&retention))
}

/// `PUT /api/retention/policy` — apply a whitelisted [`RetentionEdit`] to
/// `perseus.toml` (comment-preserving, re-validated, atomic), then adopt it live.
///
/// [`apply_retention_edit`] does the file write on an in-memory copy and only
/// swaps it in after re-validation, so a rejected edit (notably an attempt to
/// enable live deletion without the `perseus.toml`-only soak opt-in) leaves the
/// file byte-identical and returns `422`. On success the new config is adopted
/// both in the live web state and on the retention watch channel — the running
/// retention loop picks it up on its next pass, no restart.
async fn api_put_retention_policy(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<RetentionEdit>,
) -> Result<Json<PolicyDto>, (StatusCode, String)> {
    let new_cfg = apply_retention_edit(&state.config_path, &edit).map_err(|e| {
        // Validation failure (e.g. the soak gate) or any file error: the file is
        // left untouched by construction. Surface the actionable text as 422.
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web retention edit rejected");
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;
    // The file was already rewritten by `apply_retention_edit`. Adopt the new
    // config in the live web state, and push it onto the retention watch channel
    // so the running loop re-borrows it next pass. A send with no receiver (an
    // agent started without `watch`) is a harmless no-op — discard the SendError.
    let retention = new_cfg.retention.clone();
    *state.config.write().await = new_cfg;
    let _ = state.retention_tx.send(retention.clone());
    Ok(Json(PolicyDto::from_retention(&retention)))
}

/// `GET /api/retention/log` — the retention-run ring buffer, newest-first.
async fn api_retention_log(State(state): State<Arc<WebState>>) -> Json<Vec<RetentionRunRecord>> {
    let log = state
        .retention_log
        .lock()
        .expect("retention_log mutex poisoned");
    Json(log.iter().cloned().collect())
}

/// `GET`/`PUT /api/capture-dirs` payload. `configured` is the directory list in
/// the live config (`perseus.toml`, freshly rewritten by a PUT); `runtime` is
/// the list the watchers were actually spawned over. They diverge exactly when
/// an edit has been saved but Perseus has not restarted yet — `restartPending`
/// is that difference, ordered-list compared, so the page can show an honest
/// "restart to apply" banner that survives reloads and clears itself once a
/// restarted agent reports `runtime == configured` again.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureDirsDto {
    configured: Vec<String>,
    runtime: Vec<String>,
    restart_pending: bool,
}

/// `PUT /api/capture-dirs` request body: the new capture-directory selection.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureDirsEdit {
    dirs: Vec<String>,
}

/// Build the current [`CaptureDirsDto`] from live state: `configured` from the
/// (possibly just-edited) config, `runtime` from the spawn-time snapshot, and
/// `restartPending` = the two differ as ordered lists.
async fn capture_dirs_dto(state: &WebState) -> CaptureDirsDto {
    let configured: Vec<String> = state
        .config
        .read()
        .await
        .capture_dirs_resolved()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let runtime: Vec<String> = state
        .capture_dirs
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let restart_pending = configured != runtime;
    CaptureDirsDto {
        configured,
        runtime,
        restart_pending,
    }
}

/// `GET /api/capture-dirs` — the configured vs. running capture directories and
/// whether a restart is pending. Read-only; never touches disk.
async fn api_get_capture_dirs(State(state): State<Arc<WebState>>) -> Json<CaptureDirsDto> {
    Json(capture_dirs_dto(&state).await)
}

/// `PUT /api/capture-dirs` — rewrite `perseus.toml`'s capture-directory
/// selection to the array form (comment-preserving, re-validated, atomic), then
/// adopt it into the live config. Restart-to-apply: the running watchers keep
/// their spawn-time directories, so [`WebState::capture_dirs`] is intentionally
/// **not** touched — that gap is what makes the returned `restartPending` true.
///
/// [`apply_capture_dirs_edit`] writes on an in-memory copy and only swaps it in
/// after re-validation, so a rejected edit (an empty list, or a directory that
/// does not exist on the box) leaves the file byte-identical and returns `422`.
async fn api_put_capture_dirs(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<CaptureDirsEdit>,
) -> Result<Json<CaptureDirsDto>, (StatusCode, String)> {
    let new_cfg = apply_capture_dirs_edit(&state.config_path, &edit.dirs).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web capture-dirs edit rejected");
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;
    // The file was already rewritten. Adopt the new config in the live web state
    // so `configured` reflects the edit; the runtime snapshot stays as-is, which
    // is exactly what surfaces the restart-pending state.
    *state.config.write().await = new_cfg;
    Ok(Json(capture_dirs_dto(&state).await))
}

/// `POST /api/delete` — delete the source capture files of the given CONFIRMED
/// packages. Verifies each id is `confirmed` before touching disk; non-confirmed
/// / unknown ids come back in `rejected` with a reason. Shares the same
/// confirmed-only deleter as retention (audit-before-delete, TOCTOU guard).
async fn api_delete(
    State(state): State<Arc<WebState>>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<DeleteReport>, (StatusCode, String)> {
    let report = delete_confirmed_packages(&state.store, &state.seen, &req.ids).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web manual delete failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    Ok(Json(report))
}

/// `POST /api/retry` request body.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryRequest {
    /// Outbound-row ids to re-enqueue. Only `failed` rows with intact package
    /// data are retried; everything else comes back per-id in `rejected`.
    ids: Vec<i64>,
}

/// `POST /api/retry` response: the ids re-enqueued (old→new mapping) and the
/// ids rejected (each with a human reason).
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct RetryReport {
    /// One entry per package re-enqueued: the original (still-`failed`) row id
    /// and the brand-new queued row id the receiver will drive.
    retried: Vec<RetryPair>,
    /// Ids not retried, each with the reason (unknown, not failed, or the
    /// package data is gone).
    rejected: Vec<RetryRejection>,
}

/// One re-enqueued package: the original failed row id and its new queued row.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryPair {
    old_id: i64,
    new_id: i64,
}

/// One rejected id from `POST /api/retry`, with a reason for the UI to surface.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryRejection {
    id: i64,
    reason: String,
}

/// True iff `dir` holds `manifest.ndjson` AND at least one non-manifest regular
/// file — i.e. there is real payload left to re-serve. A confirmed-then-cleaned
/// dir is manifest-only (task 1) and a vanished dir fails `read_dir`; both
/// return `false` so the retry handler can reject them honestly as "package
/// data missing" rather than enqueueing an empty package.
fn package_has_payload(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut has_manifest = false;
    let mut has_payload = false;
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        if entry.file_name() == std::ffi::OsStr::new(MANIFEST_FILENAME) {
            has_manifest = true;
        } else {
            has_payload = true;
        }
    }
    has_manifest && has_payload
}

/// `POST /api/retry` — re-enqueue failed packages. For each id: look up the
/// outbound row, require `state == failed`, require the package dir to still
/// hold its manifest + payload, then `enqueue_package` it — the sanctioned retry
/// model. Re-enqueueing the same package dir mints a NEW durable row (the
/// receiver dedups by frame uuid); the original `failed` row is left untouched.
/// Unknown / non-failed / data-missing ids are rejected per-id, never enqueued.
async fn api_retry(
    State(state): State<Arc<WebState>>,
    Json(req): Json<RetryRequest>,
) -> Result<Json<RetryReport>, (StatusCode, String)> {
    let mut report = RetryReport::default();
    for &id in &req.ids {
        // A genuine store read failure is a 500 (the request failed), not a
        // per-id reject — mirror the delete handler's error philosophy.
        let row = match state.store.get_outbound(id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                report.rejected.push(RetryRejection {
                    id,
                    reason: "unknown package".to_string(),
                });
                continue;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(id, error = %msg, "web retry: outbound lookup failed");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, msg));
            }
        };
        // State check first: only a terminal `failed` row is retryable. (A
        // confirmed id is manifest-only after task-1 cleanup, but it never
        // reaches the payload gate — it is "not failed" here.)
        if row.state != OutboundState::Failed {
            report.rejected.push(RetryRejection {
                id,
                reason: "not failed".to_string(),
            });
            continue;
        }
        let dir = Path::new(&row.package_ref);
        if !package_has_payload(dir) {
            report.rejected.push(RetryRejection {
                id,
                reason: "package data missing".to_string(),
            });
            continue;
        }
        match state.engine.enqueue_package(dir).await {
            Ok(new_id) => {
                tracing::info!(old_id = id, new_id, "failed package re-enqueued via web");
                report.retried.push(RetryPair {
                    old_id: id,
                    new_id,
                });
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(old_id = id, error = %msg, "web retry: re-enqueue failed");
                report.rejected.push(RetryRejection {
                    id,
                    reason: format!("re-enqueue failed: {msg}"),
                });
            }
        }
    }
    Ok(Json(report))
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
        // Perseus's seen store shares the same db file under WAL (production wiring).
        let seen = Arc::new(crate::seen::SeenStore::open(tmp.path().join("sync.db")).unwrap());

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

        // Materialize the config on disk too, so the PUT-policy handler (which
        // rewrites `config_path` via `apply_retention_edit`) has a real file.
        let toml_str = sample_toml(tmp.path());
        let config_path = tmp.path().join("perseus.toml");
        std::fs::write(&config_path, &toml_str).unwrap();
        let config = Config::from_toml_str(&toml_str).unwrap();
        let state = Arc::new(WebState {
            store,
            engine,
            config_path,
            config: tokio::sync::RwLock::new(config.clone()),
            retention_tx: watch::channel(config.retention.clone()).0,
            device_names: HashMap::new(),
            capture_dirs: config.capture_dirs_resolved(),
            seen,
            retention_log: Arc::new(Mutex::new(VecDeque::new())),
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

        // `GET /` (the static page shell) is EXEMPT even when a token is set: it
        // must load without a token so the browser can run its JS token prompt.
        let root = app
            .clone()
            .oneshot(HttpRequest::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            root.status(),
            StatusCode::OK,
            "GET / must load without a token even when one is configured"
        );

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

    // ── Task 10: manual delete, retention policy GET/PUT, retention log ───────

    /// Manual delete is the same confirmed()-only chokepoint retention uses: a
    /// confirmed package's source is removed (with a `deleted_manual` audit row),
    /// while a non-confirmed id is rejected with a reason and never touched.
    #[tokio::test]
    async fn delete_rejects_non_confirmed() {
        let (state, tmp) = test_state().await;

        // Register a real on-disk source file for the seeded confirmed package.
        let source = tmp.path().join("light-A.fits");
        std::fs::write(&source, b"confirmed-source-bytes").unwrap();
        let meta = std::fs::metadata(&source).unwrap();
        let mtime = crate::seen::mtime_millis(meta.modified().ok());
        state
            .seen
            .mark_enqueued(&source, meta.len(), mtime, "pkg-confirmed")
            .unwrap();

        // Resolve the two seeded outbound ids by state.
        let rows = state.store.all_outbound(100).unwrap();
        let a = rows.iter().find(|r| r.state == OutboundState::Confirmed).unwrap().id;
        let b = rows.iter().find(|r| r.state == OutboundState::Transferring).unwrap().id;

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let body = serde_json::json!({ "ids": [a, b] });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/delete")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;

        let deleted = v["deleted"].as_array().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].as_i64().unwrap(), a, "only the confirmed package is deleted");

        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), b);
        assert_eq!(rejected[0]["reason"], "not confirmed");

        assert!(!source.exists(), "the confirmed package's source is removed from disk");

        let hist = store
            .search_history(HistoryQuery {
                filename: None,
                object: None,
                direction: None,
                peer: None,
                limit: 1000,
            })
            .unwrap();
        assert_eq!(
            hist.iter().filter(|h| h.outcome == "deleted_manual").count(),
            1,
            "exactly one deleted_manual audit row for the confirmed package"
        );
    }

    /// GET returns the live policy + read-only soak indicators; a valid PUT is
    /// adopted (watch receiver + file rewritten); a `dry_run = false` PUT is
    /// refused 422 and leaves the file byte-identical (the web can never enable
    /// live deletion — `RetentionEdit` has no soak field).
    #[tokio::test]
    async fn retention_policy_roundtrip() {
        let (state, _tmp) = test_state().await;
        // A live receiver so the PUT's watch send is both observable and succeeds.
        let mut rx = state.retention_tx.subscribe();
        let config_path = state.config_path.clone();
        let app = build_router(state, None);

        // GET — current values + read-only soak gate.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/retention/policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["policy"], "keep_days");
        assert_eq!(v["keepDays"], 21);
        assert_eq!(v["dryRun"], true);
        assert_eq!(v["soakOptIn"], false, "soak opt-in is read-only and off");
        assert_eq!(v["liveDeletionPossible"], false);

        // PUT a valid edit.
        let edit = crate::config_edit::RetentionEdit {
            policy: crate::config::RetentionPolicy::KeepDays,
            keep_days: 14,
            disk_max_pct: 90,
            interval_secs: 1800,
            dry_run: true,
        };
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/retention/policy")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&edit).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "a valid edit is accepted");

        assert_eq!(
            rx.borrow_and_update().keep_days,
            14,
            "the running retention loop's watch receiver adopts the edit"
        );
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(text.contains("keep_days = 14"), "the config file was rewritten: {text}");

        // PUT dry_run = false with no soak opt-in in the file → 422, file untouched.
        let before = std::fs::read_to_string(&config_path).unwrap();
        let bad = crate::config_edit::RetentionEdit {
            policy: crate::config::RetentionPolicy::KeepDays,
            keep_days: 14,
            disk_max_pct: 90,
            interval_secs: 1800,
            dry_run: false,
        };
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/retention/policy")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&bad).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "the web can never enable live deletion"
        );
        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after, before, "a rejected edit leaves the file byte-identical");
    }

    /// The retention-run log endpoint serializes the ring buffer newest-first.
    #[tokio::test]
    async fn retention_log_returns_ring_buffer() {
        let (state, _tmp) = test_state().await;
        {
            let mut log = state.retention_log.lock().unwrap();
            // Push oldest first; each push_front puts the newest at the head.
            log.push_front(RetentionRunRecord {
                at: "2026-07-08T10:00:00.000Z".into(),
                dry_run: true,
                policy: "keep_days".into(),
                deleted: vec![],
                would_delete: vec!["/cap/a.fits".into()],
                errors: vec![],
            });
            log.push_front(RetentionRunRecord {
                at: "2026-07-08T11:00:00.000Z".into(),
                dry_run: true,
                policy: "keep_days".into(),
                deleted: vec!["/cap/b.fits".into()],
                would_delete: vec![],
                errors: vec![],
            });
        }
        let app = build_router(state, None);
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/retention/log")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["at"], "2026-07-08T11:00:00.000Z", "newest first");
        assert_eq!(rows[0]["deleted"][0], "/cap/b.fits");
        assert_eq!(rows[1]["wouldDelete"][0], "/cap/a.fits");
    }

    // ── Task 3 (S1.5.1): capture-dirs editor (restart-to-apply) ───────────────

    async fn get_capture_dirs(app: &Router) -> serde_json::Value {
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/capture-dirs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        body_json(res).await
    }

    async fn put_capture_dirs(app: &Router, dirs: &[String]) -> Response {
        let body = serde_json::json!({ "dirs": dirs });
        app.clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/capture-dirs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// GET reports `configured`/`runtime`/`restartPending`. A valid PUT rewrites
    /// `perseus.toml` to the array form and adopts it into the live config, so
    /// `configured` reflects the edit while `runtime` stays the spawn-time
    /// snapshot — making `restartPending` true. The pending flag is server-
    /// derived, so a subsequent GET still reports it (survives reloads).
    #[tokio::test]
    async fn capture_dirs_get_and_put_roundtrip() {
        let (state, _tmp) = test_state().await;
        let config_path = state.config_path.clone();
        // The spawn-time runtime snapshot — never mutated by a web edit.
        let runtime_snapshot: Vec<String> = state
            .capture_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let app = build_router(state, None);

        // GET before any edit: configured == runtime, nothing pending.
        let v = get_capture_dirs(&app).await;
        assert!(v["configured"].is_array());
        assert!(v["runtime"].is_array());
        assert_eq!(v["restartPending"], false, "no edit yet → not pending");
        assert_eq!(v["configured"], v["runtime"], "configured mirrors the runtime snapshot");

        // PUT a new, existing directory.
        let newdir = tempfile::tempdir().unwrap();
        let new = newdir.path().display().to_string();
        let res = put_capture_dirs(&app, &[new.clone()]).await;
        assert_eq!(res.status(), StatusCode::OK, "a valid edit is accepted");
        let v = body_json(res).await;
        assert_eq!(
            v["restartPending"], true,
            "config changed but runtime is still the spawn snapshot → pending"
        );
        assert_eq!(v["configured"].as_array().unwrap().len(), 1);
        assert_eq!(v["configured"][0], new, "the live config adopts the edit");

        // The file was rewritten to the array form with the singular key removed.
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(text.contains("capture_dirs"), "array form written: {text}");
        assert!(!text.contains("capture_dir ="), "singular key removed: {text}");

        // A later GET still reports pending (server-derived), and `runtime` is
        // unchanged from the spawn snapshot.
        let v = get_capture_dirs(&app).await;
        assert_eq!(v["restartPending"], true, "pending survives across requests");
        let runtime_now: Vec<String> = v["runtime"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        assert_eq!(runtime_now, runtime_snapshot, "runtime stays the spawn-time snapshot");
    }

    /// A PUT naming a directory that does not exist is refused `422` and leaves
    /// the config file byte-identical (edit-on-copy, write-after-validate).
    #[tokio::test]
    async fn capture_dirs_put_nonexistent_rejected_byte_identical() {
        let (state, tmp) = test_state().await;
        let config_path = state.config_path.clone();
        let before = std::fs::read_to_string(&config_path).unwrap();
        let app = build_router(state, None);

        let missing = tmp.path().join("does-not-exist");
        let res = put_capture_dirs(&app, &[missing.display().to_string()]).await;
        assert_eq!(
            res.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a non-existent directory is rejected"
        );
        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after, before, "a rejected edit leaves the file byte-identical");
    }

    // ── Task 2 (S1.5.1): retry failed packages ───────────────────────────────

    /// Build a real package dir under `base`: a `manifest.ndjson` plus, unless
    /// `manifest_only`, one payload file — the shape task 1 preserves for a
    /// non-confirmed package (confirmed dirs are cleaned to manifest-only).
    fn make_package_dir(base: &std::path::Path, name: &str, manifest_only: bool) -> PathBuf {
        let pkg = base.join(name);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("manifest.ndjson"), b"{}\n").unwrap();
        if !manifest_only {
            std::fs::write(pkg.join("frame-0001.fits"), b"payload-bytes").unwrap();
        }
        pkg
    }

    async fn post_retry(app: Router, ids: &[i64]) -> serde_json::Value {
        let body = serde_json::json!({ "ids": ids });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/retry")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        body_json(res).await
    }

    /// A failed package whose dir still holds its manifest + payload (task 1
    /// keeps non-confirmed payloads) re-enqueues: a brand-new outbound row is
    /// created for the same package dir, the response maps old→new id, and the
    /// original failed row is left untouched (the sanctioned retry model — the
    /// old row stays failed, the new row lives its own lifecycle).
    #[tokio::test]
    async fn retry_reenqueues_failed_with_intact_payload() {
        let (state, tmp) = test_state().await;
        let pkg = make_package_dir(tmp.path(), "pkg-failed-intact", false);
        let old_id = state.store.enqueue(&pkg.to_string_lossy(), PEER).unwrap();
        state.store.set_state(old_id, OutboundState::Failed).unwrap();

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let v = post_retry(app, &[old_id]).await;

        let retried = v["retried"].as_array().unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0]["oldId"].as_i64().unwrap(), old_id);
        let new_id = retried[0]["newId"].as_i64().unwrap();
        assert_ne!(new_id, old_id, "a brand-new row id, not the old one");
        assert!(v["rejected"].as_array().unwrap().is_empty());

        // A real new row exists for the same package dir…
        let new_row = store.get_outbound(new_id).unwrap().expect("new row exists");
        assert_eq!(new_row.package_ref, pkg.to_string_lossy());
        // …and the original failed row is untouched.
        assert_eq!(
            store.get_outbound(old_id).unwrap().unwrap().state,
            OutboundState::Failed,
            "the old failed row is left as-is"
        );
    }

    /// A non-failed id (here: the seeded `transferring` package) is rejected
    /// "not failed" and never re-enqueued — no new row is created.
    #[tokio::test]
    async fn retry_rejects_non_failed() {
        let (state, _tmp) = test_state().await;
        let before = state.store.all_outbound(100).unwrap();
        let transferring = before
            .iter()
            .find(|r| r.state == OutboundState::Transferring)
            .unwrap()
            .id;

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let v = post_retry(app, &[transferring]).await;

        assert!(v["retried"].as_array().unwrap().is_empty());
        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), transferring);
        assert_eq!(rejected[0]["reason"], "not failed");
        assert_eq!(
            store.all_outbound(100).unwrap().len(),
            before.len(),
            "no new row created for a rejected retry"
        );
    }

    /// A failed package whose dir was cleaned to manifest-only (the task-1
    /// confirmed-then-cleaned shape) has nothing left to re-send: rejected
    /// "package data missing", honestly — no new row.
    #[tokio::test]
    async fn retry_rejects_missing_payload() {
        let (state, tmp) = test_state().await;
        let pkg = make_package_dir(tmp.path(), "pkg-manifest-only", true);
        let id = state.store.enqueue(&pkg.to_string_lossy(), PEER).unwrap();
        state.store.set_state(id, OutboundState::Failed).unwrap();

        let store = Arc::clone(&state.store);
        let before = store.all_outbound(100).unwrap().len();
        let app = build_router(state, None);
        let v = post_retry(app, &[id]).await;

        assert!(v["retried"].as_array().unwrap().is_empty());
        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), id);
        assert_eq!(rejected[0]["reason"], "package data missing");
        assert_eq!(
            store.all_outbound(100).unwrap().len(),
            before,
            "no new row created for a data-missing reject"
        );
    }
}
