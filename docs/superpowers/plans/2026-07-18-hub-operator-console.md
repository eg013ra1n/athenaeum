# Hub Operator Console Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instance-level operator console (dashboard, all-projects management, accounts/devices, audit) in the hub + portal, gated by an env-allowlist operator role, per `docs/superpowers/specs/2026-07-18-hub-operator-console-design.md` (the spec lives in the athenaeum repo; work happens in the athenaeum-hub repo + one astronet task).

**Architecture:** `HUB_OPERATOR_EMAILS` env → `AppState.operator_emails`; a `require_operator` middleware (layered INSIDE `require_account`) is the single choke point for a new `/api/v1/operator/*` router; `blocked_at` enforcement folds into the existing auth SQL; all operator mutations write `operator_audit` in the same transaction; portal gets an `/operator` page gated by `/me.operator`.

**Tech Stack:** Rust/Axum/sqlx (Postgres), React portal (vite, `apiGet/apiPost/apiPatch/apiDelete` helpers — all four exist in `portal/src/api.ts:35-38`), `#[sqlx::test]` + `tower::ServiceExt::oneshot` test harness, astronet Ansible for env.

> **Plan verified against code 2026-07-18** (adversarial fact-check of every snippet; corrections folded in). Line numbers are anchors from that pass — re-locate by identifier if drifted.

## Global Constraints

- Repo: `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`, branch **`operator-console`** off `main` (`4638a5d`). Astronet task in `/Volumes/BigMac/Users/astrobureau/Documents/astronet`.
- Tests: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test` (local `hub-test-pg` docker postgres). Every test task: red → green.
- Wire types `#[serde(rename_all = "camelCase")]`; portal mirrors in `portal/src/types.ts`.
- **S1**: no operator response may contain `endpointAddr` / direct addrs — raw-body substring tests required.
- Every operator mutation inserts an `operator_audit` row **in the same tx**.
- Migration `0012_operator_console.sql`; header comment convention `-- 0012_operator_console — <one line>`.
- Commits as `eg013ra1n` (repo-configured identity), messages `feat(hub): …` / `test(hub): …` / `feat(portal): …`.
- Axum layering rule (used in Tasks 4+): `route_layer` added LAST runs FIRST (outermost). `require_account` must run before `require_operator`, so add `.route_layer(require_operator).route_layer(require_account)` in that source order. `middleware` is already imported from axum in `routes/mod.rs:19`.
- **Error type**: `crate::error::ApiError` (`From<sqlx::Error>` exists, error.rs:89-93). Constructors that EXIST: `bad_request`, `conflict`, `not_found`. **There is NO `unauthorized`/`forbidden` constructor** — the codebase convention (error.rs:18-20, deliberate no-enumeration) is body-less `ApiError::Status(StatusCode::UNAUTHORIZED)` / `ApiError::Status(StatusCode::FORBIDDEN)` (see auth_mw.rs:72,134,163). Use exactly that form everywhere below.
- **Test harness call conventions** (`tests/common/mod.rs`): `post(uri, &serde_json::Value, Option<&str> /*bearer*/) -> Request` (132), `get(uri, Option<&str>) -> Request` (145), `patch(uri, &Value, Option<&str>)` (153) — request BUILDERS; send with `send(&app, req).await -> (StatusCode, Vec<u8>)` (179). Cookie builders: `post_cookie(uri, &Value, &session) -> Request` (332), `get_cookie(uri, &session) -> Request` (344) — also builders, wrap in `send`. Unauthenticated GET = `get(uri, None)`. `register_device(app, mailer, email, pubkey_seed: u8, name) -> (String /*token*/, String /*deviceId*/)` (194-229; asserts OTP→204, verify→200). `portal_sign_in(app, mailer, email) -> String` (306). `create_project_via(app, token, title, require_approval: bool) -> serde_json::Value` (273-303) — returns FULL ProjectView JSON; extract `["id"].as_str()`. `join_and_approve(app, coordinator_token, member_token, project_id: &str, display_name: &str, data_role: &str) -> String /*accountId*/` (355-386) — coordinator token FIRST. There is no delete helper and no patch/delete cookie builders — Task 7 adds `patch_cookie`, Task 9 adds `delete_cookie` (mirror `post_cookie` 332-341).

---

### Task 1: Config + AppState plumbing for operator emails

**Files:**
- Modify: `src/config.rs` (struct at 5–15, `from_env` at 40–64, hand-written `Debug` at 17–29)
- Modify: `src/routes/mod.rs` (`AppState` 32–58)
- Modify: `src/lib.rs:57-58` (wiring)

**Interfaces:**
- Produces: `Config.operator_emails: Vec<String>` (lowercased, trimmed); `pub fn parse_operator_emails(raw: &str) -> Vec<String>`; `AppState.operator_emails: Arc<Vec<String>>`; builder `with_operator_emails(self, Vec<String>) -> Self`.

- [ ] **Step 1: failing unit test** — in `src/config.rs` tests mod (create `#[cfg(test)] mod tests` if absent):

```rust
#[test]
fn operator_emails_parse_trim_lowercase_filter_empty() {
    assert_eq!(
        parse_operator_emails(" Vilen.Sharifov@Gmail.com ,, second@x.io ,"),
        vec!["vilen.sharifov@gmail.com".to_string(), "second@x.io".to_string()]
    );
    assert!(parse_operator_emails("").is_empty());
    assert!(parse_operator_emails("  ,  ").is_empty());
}
```

- [ ] **Step 2: run** `cargo test operator_emails_parse` → FAIL (fn not found).
- [ ] **Step 3: implement** in `src/config.rs`:

```rust
pub fn parse_operator_emails(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}
```

Add field `pub operator_emails: Vec<String>` to `Config`; in `from_env` (mirror the `relay_auth_token` idiom at 54–57):

```rust
operator_emails: std::env::var("HUB_OPERATOR_EMAILS")
    .ok()
    .map(|v| parse_operator_emails(&v))
    .unwrap_or_default(),
```

Add the field to the hand-written `Debug` impl (config.rs:17-29) as a count (`.field("operator_emails", &self.operator_emails.len())`) — do not print addresses.

In `src/routes/mod.rs` add to `AppState`: `pub operator_emails: Arc<Vec<String>>`; in `AppState::new` init `operator_emails: Arc::new(Vec::new())`; add builder below `with_relay_auth_token` (55–58):

```rust
pub fn with_operator_emails(mut self, emails: Vec<String>) -> Self {
    self.operator_emails = Arc::new(emails);
    self
}
```

In `src/lib.rs:57-58` chain `.with_operator_emails(config.operator_emails.clone())`.

- [ ] **Step 4: run** `cargo test` (whole suite compiles + new test passes).
- [ ] **Step 5: commit** `feat(hub): HUB_OPERATOR_EMAILS config + AppState plumbing`

---

### Task 2: Migration 0012 + schema smoke test

**Files:**
- Create: `migrations/0012_operator_console.sql`
- Create: `tests/operator_schema.rs`

**Interfaces:**
- Produces: columns `projects.featured` (bool NOT NULL DEFAULT false), `projects.hidden_at` (timestamptz NULL), `accounts.blocked_at` (timestamptz NULL); table `operator_audit`.

- [ ] **Step 1: failing test** `tests/operator_schema.rs`:

```rust
mod common;
use sqlx::PgPool;

#[sqlx::test]
async fn migration_0012_columns_and_audit_table(pool: PgPool) {
    let account: (uuid::Uuid,) =
        sqlx::query_as("INSERT INTO accounts (email) VALUES ('op@x.io') RETURNING id")
            .fetch_one(&pool).await.unwrap();
    sqlx::query("UPDATE accounts SET blocked_at = now() WHERE id = $1")
        .bind(account.0).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO operator_audit (actor, action, target_kind, target_id, detail)
         VALUES ($1, 'account.block', 'account', $1::text, '{\"note\":\"t\"}')",
    ).bind(account.0).execute(&pool).await.unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM operator_audit")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    sqlx::query("SELECT featured, hidden_at FROM projects LIMIT 0")
        .execute(&pool).await.unwrap();
}
```

- [ ] **Step 2: run** `cargo test --test operator_schema` → FAIL (column/table missing).
- [ ] **Step 3: write** `migrations/0012_operator_console.sql`:

```sql
-- 0012_operator_console — operator flags (featured/hidden, account block) + insert-only audit.
ALTER TABLE projects ADD COLUMN IF NOT EXISTS featured  boolean     NOT NULL DEFAULT false;
ALTER TABLE projects ADD COLUMN IF NOT EXISTS hidden_at timestamptz;
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS blocked_at timestamptz;

CREATE TABLE IF NOT EXISTS operator_audit (
    id          bigserial   PRIMARY KEY,
    actor       uuid        NOT NULL REFERENCES accounts (id),
    action      text        NOT NULL,
    target_kind text        NOT NULL,
    target_id   text        NOT NULL,
    detail      jsonb,
    at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS operator_audit_at_idx ON operator_audit (at DESC);
```

- [ ] **Step 4: run** `cargo test --test operator_schema` → PASS.
- [ ] **Step 5: commit** `feat(hub): migration 0012 — featured/hidden/blocked columns + operator_audit`

---

### Task 3: blocked-account enforcement (bearer, session, OTP)

**Files:**
- Modify: `src/auth_mw.rs` (`require_auth` 67–97, `require_account` 119–184)
- Modify: `src/routes/auth.rs` (`verify_otp` — blocked check right after `let account_id = upsert_account_tx(&mut tx, &email).await?;` at line 396)
- Modify: `src/routes/portal_auth.rs` (`portal_verify` — after `let account_id = auth::upsert_account_tx(&mut tx, &email).await?;` at line 54)
- Create: `tests/operator_blocked.rs`

**Interfaces:**
- Consumes: `accounts.blocked_at` (Task 2).
- Produces: blocked accounts get body-less 401 on every authenticated path and cannot complete OTP verify.

- [ ] **Step 1: failing tests** `tests/operator_blocked.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

async fn block(pool: &PgPool, email: &str) {
    sqlx::query("UPDATE accounts SET blocked_at = now() WHERE email = $1")
        .bind(email).execute(pool).await.unwrap();
}

#[sqlx::test]
async fn blocked_bearer_and_session_get_401(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _dev) = register_device(&app, &mailer, "u@x.io", 1, "pc").await;
    let session = portal_sign_in(&app, &mailer, "u@x.io").await;
    block(&pool, "u@x.io").await;
    let (st, _) = send(&app, get("/api/v1/devices", Some(&token))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "blocked bearer must 401");
    let (st, _) = send(&app, get_cookie("/api/v1/me", &session)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "blocked portal session must 401");
}

#[sqlx::test]
async fn blocked_account_cannot_pass_otp_verify_and_unblock_restores(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (_t, _d) = register_device(&app, &mailer, "u@x.io", 1, "pc").await;
    block(&pool, "u@x.io").await;
    // Drive the OTP request+verify pair MANUALLY, mirroring register_device's
    // internals VERBATIM (tests/common/mod.rs:194-229: exact endpoint paths,
    // JSON field names, and how the code is read out of CaptureMailer).
    // Expectations: the OTP request still succeeds (204 — delivery is not the
    // gate), the VERIFY call returns 401 (blocked), and no token is issued.
    // <copy the two request builders from register_device here, changing only
    //  the final assert to StatusCode::UNAUTHORIZED>
    sqlx::query("UPDATE accounts SET blocked_at = NULL WHERE email = 'u@x.io'")
        .execute(&pool).await.unwrap();
    let (_t2, _d2) = register_device(&app, &mailer, "u@x.io", 3, "pc3").await; // works again
}
```

(Do NOT use `futures::FutureExt::catch_unwind` — `futures` is not a dependency. The manual request pair is the whole point of this test's second half.)

- [ ] **Step 2: run** `cargo test --test operator_blocked` → FAIL.
- [ ] **Step 3: implement.** In `src/auth_mw.rs` change the three lookups to JOIN accounts:

```sql
-- require_account bearer branch (127-128) and require_auth device query (76-78), was:
--   SELECT id, account_id, role FROM devices WHERE token_hash = $1 AND revoked_at IS NULL
SELECT d.id, d.account_id, d.role FROM devices d
JOIN accounts a ON a.id = d.account_id
WHERE d.token_hash = $1 AND d.revoked_at IS NULL AND a.blocked_at IS NULL

-- require_account session branch (148-149), was:
--   SELECT id, account_id FROM portal_sessions WHERE token_hash = $1 AND expires_at > now()
SELECT s.id, s.account_id FROM portal_sessions s
JOIN accounts a ON a.id = s.account_id
WHERE s.token_hash = $1 AND s.expires_at > now() AND a.blocked_at IS NULL
```

In `src/routes/auth.rs` after line 396 and in `src/routes/portal_auth.rs` after line 54 (identical block; `tx` is in scope in both):

```rust
let blocked: Option<chrono::DateTime<chrono::Utc>> =
    sqlx::query_scalar("SELECT blocked_at FROM accounts WHERE id = $1")
        .bind(account_id).fetch_one(&mut *tx).await?;
if blocked.is_some() {
    return Err(ApiError::Status(axum::http::StatusCode::UNAUTHORIZED));
}
```

(Body-less 401 per the error.rs:18-20 no-enumeration convention; `unauthorized(...)` constructors do not exist.)

- [ ] **Step 4: run** `cargo test --test operator_blocked` → PASS; full `cargo test` → no regressions.
- [ ] **Step 5: commit** `feat(hub): blocked_at enforcement on bearer/session/OTP paths`

---

### Task 4: OperatorAuth middleware, `/me.operator`, audit read route

**Files:**
- Create: `src/routes/operator.rs`
- Modify: `src/routes/mod.rs` (module decl + router 92–182)
- Modify: `src/routes/me.rs` (`MeResponse` 12–17, handler 19–32 — it already receives `State<AppState>`)
- Modify: `tests/common/mod.rs` (new helper)
- Create: `tests/operator_gate.rs`

**Interfaces:**
- Produces: `pub struct OperatorAuth { pub account_id: Uuid, pub email: String }` (`AuthAccount` already derives Clone — auth_mw.rs:103); middleware `require_operator`; `audit_tx(conn, actor, action, target_kind, target_id, detail)`; `GET /api/v1/operator/audit?limit=` → `Vec<AuditRow{id, actorEmail, action, targetKind, targetId, detail, at}>`; `MeResponse.operator: bool`; test helper `app_with_operators(pool, &["email"]) -> (Router, CaptureMailer)`.

- [ ] **Step 1: failing tests** `tests/operator_gate.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn operator_gate_and_me_flag(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let user = portal_sign_in(&app, &mailer, "user@x.io").await;

    let (st, body) = send(&app, get_cookie("/api/v1/me", &op)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(String::from_utf8(body).unwrap().contains("\"operator\":true"));
    let (_, body) = send(&app, get_cookie("/api/v1/me", &user)).await;
    assert!(String::from_utf8(body).unwrap().contains("\"operator\":false"));

    let (st, _) = send(&app, get_cookie("/api/v1/operator/audit", &user)).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "non-operator must get 403");
    let (st, body) = send(&app, get_cookie("/api/v1/operator/audit", &op)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(String::from_utf8(body).unwrap(), "[]");
}

#[sqlx::test]
async fn empty_allowlist_is_inert(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let s = portal_sign_in(&app, &mailer, "anyone@x.io").await;
    let (st, _) = send(&app, get_cookie("/api/v1/operator/audit", &s)).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: run** `cargo test --test operator_gate` → FAIL.
- [ ] **Step 3: implement** `src/routes/operator.rs`:

```rust
//! Instance-operator console routes. Single choke point: `require_operator`
//! middleware (env-allowlist via AppState.operator_emails). Spec:
//! athenaeum docs/superpowers/specs/2026-07-18-hub-operator-console-design.md

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Extension, Json,
};
use sqlx::PgConnection;
use uuid::Uuid;

use super::AppState;
use crate::auth_mw::AuthAccount;
use crate::error::ApiError;

#[derive(Clone, Debug)]
pub struct OperatorAuth {
    pub account_id: Uuid,
    pub email: String,
}

pub async fn require_operator(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let auth = req
        .extensions()
        .get::<AuthAccount>()
        .cloned()
        .ok_or(ApiError::Status(StatusCode::UNAUTHORIZED))?;
    let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(auth.account_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| ApiError::Status(StatusCode::UNAUTHORIZED))?;
    let lower = email.to_ascii_lowercase();
    if !state.operator_emails.iter().any(|e| e == &lower) {
        return Err(ApiError::Status(StatusCode::FORBIDDEN));
    }
    req.extensions_mut().insert(OperatorAuth { account_id: auth.account_id, email: lower });
    Ok(next.run(req).await)
}

pub async fn audit_tx(
    conn: &mut PgConnection,
    actor: Uuid,
    action: &str,
    target_kind: &str,
    target_id: &str,
    detail: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO operator_audit (actor, action, target_kind, target_id, detail)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(actor).bind(action).bind(target_kind).bind(target_id).bind(detail)
    .execute(conn).await?;
    Ok(())
}

#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AuditRow {
    pub id: i64,
    pub actor_email: String,
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    pub detail: Option<serde_json::Value>,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Deserialize)]
pub struct AuditQuery { pub limit: Option<i64> }

pub async fn list_audit(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<AuditQuery>,
) -> Result<Json<Vec<AuditRow>>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT oa.id, a.email AS actor_email, oa.action, oa.target_kind, oa.target_id, oa.detail, oa.at
         FROM operator_audit oa JOIN accounts a ON a.id = oa.actor
         ORDER BY oa.at DESC, oa.id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.db).await?;
    Ok(Json(rows))
}
```

In `src/routes/mod.rs`: `pub mod operator;`; in `build_router` before the final merge chain:

```rust
let operator = Router::new()
    .route("/api/v1/operator/audit", get(operator::list_audit))
    // require_account must run BEFORE require_operator → operator layer added first (inner)
    .route_layer(middleware::from_fn_with_state(state.clone(), operator::require_operator))
    .route_layer(middleware::from_fn_with_state(state.clone(), auth_mw::require_account));
```

merged via `.merge(operator)` in the final chain (176–181).

In `src/routes/me.rs`: add `operator: bool` to `MeResponse`; compute after the existing email SELECT (24–27):

```rust
let operator = state.operator_emails.iter().any(|e| e == &email.to_ascii_lowercase());
```

In `tests/common/mod.rs` below `app_with_capture` (82–86) — replicate its body exactly, chaining the builder:

```rust
#[allow(dead_code)]
pub fn app_with_operators(pool: sqlx::PgPool, emails: &[&str]) -> (axum::Router, CaptureMailer) {
    let mailer = CaptureMailer::default();
    let state = athenaeum_hub::routes::AppState::new(pool, std::sync::Arc::new(mailer.clone()))
        .with_operator_emails(emails.iter().map(|s| s.to_string()).collect());
    (athenaeum_hub::routes::build_router(state), mailer)
}
```

(Mirror the exact AppState construction expression from `app_with_capture` — if it differs from the above, copy it and only append `.with_operator_emails(...)`.)

- [ ] **Step 4: run** `cargo test --test operator_gate` → PASS; `cargo test` → green.
- [ ] **Step 5: commit** `feat(hub): operator gate (env allowlist), /me.operator, audit table read`

---

### Task 5: overview endpoint (dashboard data)

**Files:**
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs` (add route)
- Create: `tests/operator_overview.rs`

**Interfaces:**
- Produces: `GET /api/v1/operator/overview` → `Overview{accounts, devicesActive, projectsActive, projectsHidden, projectsClosed, members, announcementsByState, joinRequestsOpen, events: [{at, kind, projectSlug, actorEmail, detail}]}`.
- Table facts: announcements table is **`package_announcements`** (state column `state`, CHECK pending/published/rejected); events table `project_events` timestamp column is **`created_at`** (no `at` column); project creation records event kind **`"created"`** (projects.rs:374).

- [ ] **Step 1: failing test** `tests/operator_overview.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn overview_counts_and_events(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (coord_token, _d) = register_device(&app, &mailer, "coord@x.io", 1, "pc").await;
    let project = create_project_via(&app, &coord_token, "M31 Mosaic", false).await;
    let _project_id = project["id"].as_str().unwrap();
    let (st, body) = send(&app, get_cookie("/api/v1/operator/overview", &op)).await;
    assert_eq!(st, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["accounts"].as_i64().unwrap() >= 2);
    assert_eq!(v["projectsActive"].as_i64().unwrap(), 1);
    assert!(v["events"].as_array().unwrap().iter().any(|e| e["kind"] == "created"),
        "project creation event must appear: {v}");
}
```

- [ ] **Step 2: run** → FAIL (404).
- [ ] **Step 3: implement** in `operator.rs`:

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub accounts: i64,
    pub devices_active: i64,
    pub projects_active: i64,
    pub projects_hidden: i64,
    pub projects_closed: i64,
    pub members: i64,
    pub announcements_by_state: std::collections::BTreeMap<String, i64>,
    pub join_requests_open: i64,
    pub events: Vec<EventRow>,
}

#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct EventRow {
    pub at: chrono::DateTime<chrono::Utc>,
    pub kind: String,
    pub project_slug: Option<String>,
    pub actor_email: Option<String>,
    pub detail: Option<serde_json::Value>,
}

pub async fn overview(State(state): State<AppState>) -> Result<Json<Overview>, ApiError> {
    let db = &state.db;
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts").fetch_one(db).await?;
    let devices_active: i64 =
        sqlx::query_scalar("SELECT count(*) FROM devices WHERE revoked_at IS NULL").fetch_one(db).await?;
    let projects_active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM projects WHERE status = 'active' AND hidden_at IS NULL").fetch_one(db).await?;
    let projects_hidden: i64 =
        sqlx::query_scalar("SELECT count(*) FROM projects WHERE hidden_at IS NOT NULL").fetch_one(db).await?;
    let projects_closed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM projects WHERE status = 'closed'").fetch_one(db).await?;
    let members: i64 = sqlx::query_scalar("SELECT count(*) FROM project_members").fetch_one(db).await?;
    let ann: Vec<(String, i64)> = sqlx::query_as(
        "SELECT state, count(*) FROM package_announcements GROUP BY state")
        .fetch_all(db).await.unwrap_or_default();
    let join_requests_open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM join_requests WHERE status = 'open'").fetch_one(db).await?;
    let events = sqlx::query_as::<_, EventRow>(
        "SELECT e.created_at AS at, e.kind, p.slug AS project_slug, a.email AS actor_email, e.detail
         FROM project_events e
         LEFT JOIN projects p ON p.id = e.project_id
         LEFT JOIN accounts a ON a.id = e.actor
         ORDER BY e.created_at DESC LIMIT 50",
    ).fetch_all(db).await?;
    Ok(Json(Overview {
        accounts, devices_active, projects_active, projects_hidden, projects_closed,
        members, announcements_by_state: ann.into_iter().collect(), join_requests_open, events,
    }))
}
```

Register `.route("/api/v1/operator/overview", get(operator::overview))` in the operator router.

- [ ] **Step 4: run** → PASS; `cargo test` → green.
- [ ] **Step 5: commit** `feat(hub): operator overview — counts + recent events`

---

### Task 6: operator projects listing

**Files:**
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_projects_list.rs`

**Interfaces:**
- Produces: `GET /api/v1/operator/projects` → `Vec<OperatorProjectRow{id, slug, title, status, featured, hiddenAt, createdAt, coordinatorEmail, memberCount, announcementCount, lastEventAt}>`.

- [ ] **Step 1: failing test**:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn lists_all_projects_with_aggregates(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (t, _) = register_device(&app, &mailer, "coord@x.io", 1, "pc").await;
    let _p = create_project_via(&app, &t, "M31 Mosaic", false).await;
    let (st, body) = send(&app, get_cookie("/api/v1/operator/projects", &op)).await;
    assert_eq!(st, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let row = &v.as_array().unwrap()[0];
    assert_eq!(row["coordinatorEmail"], "coord@x.io");
    assert_eq!(row["memberCount"], 1);
    assert_eq!(row["featured"], false);
    assert!(row["hiddenAt"].is_null());
}
```

- [ ] **Step 2: run** → FAIL. **Step 3: implement**:

```rust
#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OperatorProjectRow {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub featured: bool,
    pub hidden_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub coordinator_email: Option<String>,
    pub member_count: i64,
    pub announcement_count: i64,
    pub last_event_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<OperatorProjectRow>>, ApiError> {
    let rows = sqlx::query_as::<_, OperatorProjectRow>(
        "SELECT p.id, p.slug, p.title, p.status, p.featured, p.hidden_at, p.created_at,
                ca.email AS coordinator_email,
                (SELECT count(*) FROM project_members m WHERE m.project_id = p.id) AS member_count,
                (SELECT count(*) FROM package_announcements an WHERE an.project_id = p.id) AS announcement_count,
                (SELECT max(e.created_at) FROM project_events e WHERE e.project_id = p.id) AS last_event_at
         FROM projects p
         LEFT JOIN project_members cm ON cm.project_id = p.id AND cm.is_coordinator
         LEFT JOIN accounts ca ON ca.id = cm.account_id
         ORDER BY p.created_at DESC",
    ).fetch_all(&state.db).await?;
    Ok(Json(rows))
}
```

Register `.route("/api/v1/operator/projects", get(operator::list_projects))`.

- [ ] **Step 4: run** → PASS. **Step 5: commit** `feat(hub): operator all-projects listing with aggregates`

---

### Task 7: operator PATCH (featured/hidden/status) + hidden/featured semantics enforcement

**Files:**
- Modify: `src/routes/operator.rs` (PATCH handler)
- Modify: `src/routes/projects.rs` (`directory` 426–462: WHERE at 436, ORDER BY, `featured` in SELECT + `DirectoryRow` 393–406 + `DirectoryItem` 408–422; `PROJECT_COLUMNS` 187–189 + `ProjectRow` 168–185 gain `hidden_at`; `project_page` 567–694 hidden gate)
- Modify: `src/auth_mw.rs` (new `optional_account` helper)
- Modify: `src/routes/join_requests.rs` (`create_join_request` status check 55–62: also refuse hidden)
- Modify: `tests/common/mod.rs` (add `patch_cookie` builder, mirror `post_cookie` 332–341 with method PATCH)
- Create: `tests/operator_hidden.rs`

**Interfaces:**
- Consumes: `audit_tx` (Task 4).
- Produces: `PATCH /api/v1/operator/projects/{id}` body `{featured?, hidden?, status?, note?}`; `pub async fn optional_account(state: &AppState, headers: &HeaderMap) -> Option<Uuid>`; `DirectoryItem.featured: bool` on the public directory wire.

- [ ] **Step 1: failing tests** `tests/operator_hidden.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn hidden_project_semantics(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (coord, _) = register_device(&app, &mailer, "coord@x.io", 1, "pc").await;
    let (member, _) = register_device(&app, &mailer, "member@x.io", 2, "pc2").await;
    let (outsider, _) = register_device(&app, &mailer, "out@x.io", 3, "pc3").await;
    let project = create_project_via(&app, &coord, "M31 Mosaic", false).await;
    let pid = project["id"].as_str().unwrap().to_string();
    join_and_approve(&app, &coord, &member, &pid, "M", "send").await;

    let (st, _) = send(&app, patch_cookie(&format!("/api/v1/operator/projects/{pid}"),
        &json!({"hidden": true, "note": "spam check"}), &op)).await;
    assert_eq!(st, StatusCode::OK);

    // 1) absent from public directory
    let (_, body) = send(&app, get("/api/v1/projects", None)).await;
    assert!(!String::from_utf8(body).unwrap().contains("M31"), "hidden project must not be listed");
    // 2) page 404 for anonymous and for outsider
    let (st, _) = send(&app, get(&format!("/api/v1/projects/{pid}"), None)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = send(&app, get(&format!("/api/v1/projects/{pid}"), Some(&outsider))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // 3) page 200 for member and operator
    let (st, _) = send(&app, get(&format!("/api/v1/projects/{pid}"), Some(&member))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, get_cookie(&format!("/api/v1/projects/{pid}"), &op)).await;
    assert_eq!(st, StatusCode::OK);
    // 4) join refused while hidden
    let (st, _) = send(&app, post(&format!("/api/v1/projects/{pid}/join-requests"),
        &json!({"displayName": "X", "desiredRole": "send"}), Some(&outsider))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    // 5) audit row written
    let (_, body) = send(&app, get_cookie("/api/v1/operator/audit", &op)).await;
    let raw = String::from_utf8(body).unwrap();
    assert!(raw.contains("project.update") && raw.contains("spam check"));
}

#[sqlx::test]
async fn featured_sorts_first_in_directory(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (t, _) = register_device(&app, &mailer, "c@x.io", 1, "pc").await;
    let p1 = create_project_via(&app, &t, "First", false).await;
    let _p2 = create_project_via(&app, &t, "Second", false).await;
    let (st, _) = send(&app, patch_cookie(
        &format!("/api/v1/operator/projects/{}", p1["id"].as_str().unwrap()),
        &json!({"featured": true}), &op)).await;
    assert_eq!(st, StatusCode::OK);
    let (_, body) = send(&app, get("/api/v1/projects", None)).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["title"], "First", "featured project must sort first");
    assert_eq!(v[0]["featured"], true, "directory must carry the featured flag");
}
```

- [ ] **Step 2: run** → FAIL. **Step 3: implement.**

`operator.rs`:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorPatchProject {
    pub featured: Option<bool>,
    pub hidden: Option<bool>,
    pub status: Option<String>,
    pub note: Option<String>,
}

pub async fn patch_project(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(body): Json<OperatorPatchProject>,
) -> Result<StatusCode, ApiError> {
    if let Some(s) = &body.status {
        if s != "active" && s != "closed" {
            return Err(ApiError::bad_request("status must be active or closed"));
        }
    }
    let mut tx = state.db.begin().await?;
    let found = sqlx::query(
        "UPDATE projects SET
            featured  = COALESCE($2, featured),
            hidden_at = CASE WHEN $3::bool IS NULL THEN hidden_at
                             WHEN $3 THEN COALESCE(hidden_at, now()) ELSE NULL END,
            status    = COALESCE($4, status)
         WHERE id = $1",
    )
    .bind(id).bind(body.featured).bind(body.hidden).bind(&body.status)
    .execute(&mut *tx).await?;
    if found.rows_affected() == 0 {
        return Err(ApiError::not_found("project not found"));
    }
    audit_tx(&mut tx, op.account_id, "project.update", "project", &id.to_string(),
        serde_json::json!({"featured": body.featured, "hidden": body.hidden,
                           "status": body.status, "note": body.note})).await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}
```

`projects.rs` `directory` (426–462): the WHERE is an OR chain — **parenthesize before AND-ing** and add featured to SELECT + ORDER BY:

```sql
WHERE ($1::text IS NULL OR p.title ILIKE '%' || $1 || '%' OR p.target_name ILIKE '%' || $1 || '%')
  AND p.hidden_at IS NULL
GROUP BY p.id ORDER BY p.featured DESC, p.created_at DESC LIMIT 200
```

Add `p.featured` to the directory SELECT list, `featured: bool` to `DirectoryRow` (393–406) and `DirectoryItem` (408–422) with the mapping line in between. Add `hidden_at` to `PROJECT_COLUMNS` (187–189) and `pub(crate) hidden_at: Option<chrono::DateTime<chrono::Utc>>` to `ProjectRow` (168–185).

`auth_mw.rs` new helper (read-only, GET use only — no CSRF; note the qualified helper paths):

```rust
pub async fn optional_account(state: &AppState, headers: &axum::http::HeaderMap) -> Option<Uuid> {
    if let Some(token) = bearer_token(headers) {
        if let Ok(Some(id)) = sqlx::query_scalar::<_, Uuid>(
            "SELECT d.account_id FROM devices d JOIN accounts a ON a.id = d.account_id
             WHERE d.token_hash = $1 AND d.revoked_at IS NULL AND a.blocked_at IS NULL")
            .bind(crate::security::hash_token(&token)).fetch_optional(&state.db).await
        { return Some(id); }
    }
    if let Some(cookie) = crate::routes::portal_auth::session_cookie_value(headers) {
        if let Ok(Some(id)) = sqlx::query_scalar::<_, Uuid>(
            "SELECT s.account_id FROM portal_sessions s JOIN accounts a ON a.id = s.account_id
             WHERE s.token_hash = $1 AND s.expires_at > now() AND a.blocked_at IS NULL")
            .bind(crate::security::hash_token(&cookie)).fetch_optional(&state.db).await
        { return Some(id); }
    }
    None
}
```

(`session_cookie_value` is `pub(crate)` in `portal_auth.rs:23` and takes headers — mirror how auth_mw.rs:144 calls it. `security::hash_token` per auth_mw.rs:73.)

`projects.rs` `project_page` (567–570): add `headers: axum::http::HeaderMap` parameter; after `resolve_project`:

```rust
if project.hidden_at.is_some() {
    let viewer = crate::auth_mw::optional_account(&state, &headers).await;
    let allowed = match viewer {
        Some(account_id) => {
            let member: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM project_members WHERE project_id=$1 AND account_id=$2)")
                .bind(project.id).bind(account_id).fetch_one(&state.db).await?;
            let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id=$1")
                .bind(account_id).fetch_one(&state.db).await?;
            member || state.operator_emails.iter().any(|e| e == &email.to_ascii_lowercase())
        }
        None => false,
    };
    if !allowed { return Err(ApiError::not_found("project not found")); }
}
```

`join_requests.rs` `create_join_request` (55–62): extend the SELECT to `SELECT status, hidden_at FROM projects …` and refuse when `hidden_at` is set with the SAME existing message `"project is closed to new members"` (join_requests.rs:61).

`tests/common/mod.rs`: add `patch_cookie(uri: &str, body: &serde_json::Value, session_token: &str) -> Request<Body>` mirroring `post_cookie` (332–341) with `Method::PATCH`.

Register `.route("/api/v1/operator/projects/{id}", patch(operator::patch_project))`.

- [ ] **Step 4: run** `cargo test --test operator_hidden` → PASS; full `cargo test` → green (existing directory/page tests must still pass).
- [ ] **Step 5: commit** `feat(hub): operator project patch + hidden/featured semantics (directory, page gate, join refusal)`

---

### Task 8: coordinator appointment (by email, any account)

**Files:**
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_coordinator.rs`

**Interfaces:**
- Consumes: `bump_membership_version_tx(conn, project_id)` and `record_event_tx(conn, project_id, kind, actor, subject, detail)` from `crate::collab_auth` (97–131); `audit_tx`.
- Produces: `POST /api/v1/operator/projects/{id}/coordinator` body `{email}` → 200; non-member target gets a `send_receive` membership (display_name = email). Drop-then-raise flip order per the `one_coordinator_per_project` partial unique index (0009:40-42), same as `handover` (members.rs:279-293).

- [ ] **Step 1: failing test**:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn appoints_non_member_as_coordinator(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (coord, _) = register_device(&app, &mailer, "old@x.io", 1, "pc").await;
    let (_new, _) = register_device(&app, &mailer, "new@x.io", 2, "pc2").await;
    let project = create_project_via(&app, &coord, "M31 Mosaic", false).await;
    let pid = project["id"].as_str().unwrap().to_string();

    let before: i64 = sqlx::query_scalar("SELECT membership_version FROM projects WHERE id = $1::uuid")
        .bind(&pid).fetch_one(&pool).await.unwrap();
    let (st, _) = send(&app, post_cookie(&format!("/api/v1/operator/projects/{pid}/coordinator"),
        &json!({"email": "new@x.io"}), &op)).await;
    assert_eq!(st, StatusCode::OK);
    let after: i64 = sqlx::query_scalar("SELECT membership_version FROM projects WHERE id = $1::uuid")
        .bind(&pid).fetch_one(&pool).await.unwrap();
    assert!(after > before, "membership_version must bump");
    let coords: Vec<(String,)> = sqlx::query_as(
        "SELECT a.email FROM project_members m JOIN accounts a ON a.id=m.account_id
         WHERE m.project_id=$1::uuid AND m.is_coordinator").bind(&pid)
        .fetch_all(&pool).await.unwrap();
    assert_eq!(coords, vec![("new@x.io".to_string(),)]);
    let (st, _) = send(&app, post_cookie(&format!("/api/v1/operator/projects/{pid}/coordinator"),
        &json!({"email": "ghost@x.io"}), &op)).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "unknown account email → 404");
}
```

- [ ] **Step 2: run** → FAIL. **Step 3: implement**:

```rust
#[derive(serde::Deserialize)]
pub struct AppointCoordinator { pub email: String }

pub async fn appoint_coordinator(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(body): Json<AppointCoordinator>,
) -> Result<StatusCode, ApiError> {
    let email = body.email.trim().to_ascii_lowercase();
    let target: Option<Uuid> = sqlx::query_scalar("SELECT id FROM accounts WHERE lower(email) = $1")
        .bind(&email).fetch_optional(&state.db).await?;
    let target = target.ok_or_else(|| ApiError::not_found("no account with that email"))?;
    let mut tx = state.db.begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id=$1)")
        .bind(id).fetch_one(&mut *tx).await?;
    if !exists { return Err(ApiError::not_found("project not found")); }
    sqlx::query(
        "INSERT INTO project_members (project_id, account_id, display_name, data_role)
         VALUES ($1, $2, $3, 'send_receive')
         ON CONFLICT (project_id, account_id) DO NOTHING")
        .bind(id).bind(target).bind(&email).execute(&mut *tx).await?;
    // drop-then-raise (one_coordinator_per_project partial unique index)
    sqlx::query("UPDATE project_members SET is_coordinator = false WHERE project_id=$1 AND is_coordinator")
        .bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE project_members SET is_coordinator = true WHERE project_id=$1 AND account_id=$2")
        .bind(id).bind(target).execute(&mut *tx).await?;
    crate::collab_auth::bump_membership_version_tx(&mut tx, id).await?;
    crate::collab_auth::record_event_tx(&mut tx, id, "coordinator_changed",
        Some(op.account_id), Some(target), serde_json::json!({"by": "operator"})).await?;
    audit_tx(&mut tx, op.account_id, "project.coordinator", "project", &id.to_string(),
        serde_json::json!({"email": email})).await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}
```

(`bump_membership_version_tx`/`record_event_tx` take `&mut PgConnection` — pass `&mut *tx` if the compiler asks.) Register the route.

- [ ] **Step 4: run** → PASS; full suite green (snapshot signing is lazy — the next `membership_snapshot` fetch re-signs; no extra call needed, matching how `handover` works).
- [ ] **Step 5: commit** `feat(hub): operator coordinator appointment — any account, deadlock-free takeover`

---

### Task 9: on-behalf join decisions, member removal, operator listings (joins + members)

**Files:**
- Modify: `src/routes/join_requests.rs` (factor cores from 201–297; factor the open-requests listing query from `list_join_requests` 156–183 — `JoinRequestView` fields are private, so the operator listing must be produced INSIDE this module as a `pub(crate)` fn)
- Modify: `src/routes/members.rs` (factor core from 139–187)
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Modify: `tests/common/mod.rs` (add `delete_cookie(uri, &session) -> Request` mirroring `post_cookie` 332–341 with `Method::DELETE`, no body)
- Create: `tests/operator_members.rs`

**Interfaces:**
- Produces:
  - `pub(crate) async fn approve_join_core(tx: &mut sqlx::PgConnection, project_id: Uuid, req_id: Uuid, decided_by: Uuid, data_role_override: Option<String>) -> Result<ApproveResponse, ApiError>` (the 218–260 block: atomic consume UPDATE → member INSERT with 409 on `project_members_pkey` → bump → `record_event_tx("member_joined", …)`)
  - `pub(crate) async fn reject_join_core(tx, project_id, req_id, decided_by) -> Result<(), ApiError>` (from 270–297)
  - `pub(crate) async fn list_open_join_requests(db: &sqlx::PgPool, project_id: Uuid) -> Result<Vec<JoinRequestView>, ApiError>` (factored from 156–183, WITHOUT the coordinator gate)
  - `pub(crate) async fn remove_member_core(tx, project_id, target: Uuid) -> Result<(), ApiError>` (from 152–181, keeps the "cannot remove the coordinator — hand over first" 409 guard)
  - Operator routes: `POST /api/v1/operator/projects/{id}/join-requests/{rid}/decide` body `{action, dataRole?}`; `GET /api/v1/operator/projects/{id}/join-requests` → `Vec<JoinRequestView>`; `DELETE /api/v1/operator/projects/{id}/members/{account_id}`; `GET /api/v1/operator/projects/{id}/members` → `Vec<OperatorMemberRow{accountId, email, displayName, dataRole, coordinator, joinedAt}>` (the PUBLIC page deliberately never exposes accountId/emails — the portal remove-flow needs this operator-scoped listing).

- [ ] **Step 1: failing tests** `tests/operator_members.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn operator_lists_decides_joins_and_removes_members(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (coord, _) = register_device(&app, &mailer, "coord@x.io", 1, "pc").await;
    let (joiner, _) = register_device(&app, &mailer, "j@x.io", 2, "pc2").await;
    let project = create_project_via(&app, &coord, "M31 Mosaic", false).await;
    let pid = project["id"].as_str().unwrap().to_string();
    let (st, body) = send(&app, post(&format!("/api/v1/projects/{pid}/join-requests"),
        &json!({"displayName": "J", "desiredRole": "send"}), Some(&joiner))).await;
    assert_eq!(st, StatusCode::OK);
    let rid = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_str().unwrap().to_string();

    // operator sees the open request
    let (st, body) = send(&app, get_cookie(&format!("/api/v1/operator/projects/{pid}/join-requests"), &op)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(String::from_utf8(body).unwrap().contains(&rid));

    let (st, _) = send(&app, post_cookie(
        &format!("/api/v1/operator/projects/{pid}/join-requests/{rid}/decide"),
        &json!({"action": "approve"}), &op)).await;
    assert_eq!(st, StatusCode::OK);

    // operator members listing carries accountId + email
    let (st, body) = send(&app, get_cookie(&format!("/api/v1/operator/projects/{pid}/members"), &op)).await;
    assert_eq!(st, StatusCode::OK);
    let members: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(members.as_array().unwrap().len(), 2);
    let joiner_row = members.as_array().unwrap().iter()
        .find(|m| m["email"] == "j@x.io").expect("joiner listed");
    let joiner_id = joiner_row["accountId"].as_str().unwrap().to_string();
    let coord_id = members.as_array().unwrap().iter()
        .find(|m| m["email"] == "coord@x.io").unwrap()["accountId"].as_str().unwrap().to_string();

    let (st, _) = send(&app, delete_cookie(
        &format!("/api/v1/operator/projects/{pid}/members/{coord_id}"), &op)).await;
    assert_eq!(st, StatusCode::CONFLICT, "coordinator removal must be refused");
    let (st, _) = send(&app, delete_cookie(
        &format!("/api/v1/operator/projects/{pid}/members/{joiner_id}"), &op)).await;
    assert_eq!(st, StatusCode::OK);
}
```

- [ ] **Step 2: run** → FAIL. **Step 3: implement.** Factor the cores as specified in Interfaces (existing handlers keep their `require_coordinator`/self-guards and call the cores; existing tests re-pin them). Operator handlers in `operator.rs`:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinDecision { pub action: String, pub data_role: Option<String> }

pub async fn decide_join(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path((id, rid)): axum::extract::Path<(Uuid, Uuid)>,
    Json(body): Json<JoinDecision>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;
    match body.action.as_str() {
        "approve" => { crate::routes::join_requests::approve_join_core(&mut tx, id, rid, op.account_id, body.data_role).await?; }
        "reject"  => { crate::routes::join_requests::reject_join_core(&mut tx, id, rid, op.account_id).await?; }
        _ => return Err(ApiError::bad_request("action must be approve or reject")),
    }
    audit_tx(&mut tx, op.account_id, "join_request.decide", "join_request", &rid.to_string(),
        serde_json::json!({"projectId": id, "action": body.action})).await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}

pub async fn list_join_requests(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Vec<crate::routes::join_requests::JoinRequestView>>, ApiError> {
    Ok(Json(crate::routes::join_requests::list_open_join_requests(&state.db, id).await?))
}

#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OperatorMemberRow {
    pub account_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub data_role: String,
    pub coordinator: bool,
    pub joined_at: chrono::DateTime<chrono::Utc>,
}

pub async fn list_members(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Vec<OperatorMemberRow>>, ApiError> {
    let rows = sqlx::query_as::<_, OperatorMemberRow>(
        "SELECT m.account_id, a.email, m.display_name, m.data_role,
                m.is_coordinator AS coordinator, m.joined_at
         FROM project_members m JOIN accounts a ON a.id = m.account_id
         WHERE m.project_id = $1 ORDER BY m.joined_at",
    ).bind(id).fetch_all(&state.db).await?;
    Ok(Json(rows))
}

pub async fn remove_member(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path((id, account_id)): axum::extract::Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;
    crate::routes::members::remove_member_core(&mut tx, id, account_id).await?;
    audit_tx(&mut tx, op.account_id, "member.remove", "member",
        &format!("{id}/{account_id}"), serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}
```

(`JoinRequestView` needs `pub(crate)` visibility on the TYPE — keep its fields private; the operator handler only re-serializes it.) Register the four routes.

- [ ] **Step 4: run** → PASS; full suite green.
- [ ] **Step 5: commit** `feat(hub): operator join listings/decisions + member listing/removal via factored cores`

---

### Task 10: operator project create + delete

**Files:**
- Modify: `src/routes/projects.rs` (factor creation core from `create_project` 234–384; `CreateProject` at 141–157 and `ProjectView` at 191 are already `pub`)
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_create_delete.rs`

**Interfaces:**
- Produces: `pub(crate) async fn create_project_core(tx: &mut sqlx::PgConnection, coordinator: Uuid, body: &CreateProject) -> Result<ProjectView, ApiError>` (project INSERT with slugify+suffix retry 295–343 + coordinator membership INSERT 348–357, coordinator account parameterized; the 5/24h rate limit 276–286 stays in the USER handler only); operator routes `POST /api/v1/operator/projects` (body = `CreateProject` fields + `coordinatorEmail`), `DELETE /api/v1/operator/projects/{id}`.
- **`CreateProject` required fields (verified)**: `title`, nested `target: {name, raDeg, decDeg, radiusDeg}` (ALL four required — there is NO top-level `targetName`), `coordinatorDisplayName`, `coordinatorDataRole`. Mirror `create_project_via`'s JSON body (common/mod.rs:283–291) when writing test payloads.

- [ ] **Step 1: failing tests**:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn create_body(coordinator_email: &str) -> serde_json::Value {
    json!({
        "title": "Official M31",
        "target": {"name": "M31", "raDeg": 10.68, "decDeg": 41.27, "radiusDeg": 1.0},
        "coordinatorDisplayName": "Astro",
        "coordinatorDataRole": "send_receive",
        "coordinatorEmail": coordinator_email
    })
}

#[sqlx::test]
async fn operator_creates_and_deletes_project(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (_c, _) = register_device(&app, &mailer, "astro@x.io", 1, "pc").await;

    let (st, body) = send(&app, post_cookie("/api/v1/operator/projects",
        &create_body("astro@x.io"), &op)).await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let pid = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_str().unwrap().to_string();
    let coord_email: String = sqlx::query_scalar(
        "SELECT a.email FROM project_members m JOIN accounts a ON a.id=m.account_id
         WHERE m.project_id=$1::uuid AND m.is_coordinator").bind(&pid)
        .fetch_one(&pool).await.unwrap();
    assert_eq!(coord_email, "astro@x.io");

    let (st, _) = send(&app, post_cookie("/api/v1/operator/projects",
        &create_body("ghost@x.io"), &op)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _) = send(&app, delete_cookie(&format!("/api/v1/operator/projects/{pid}"), &op)).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, get(&format!("/api/v1/projects/{pid}"), None)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, body) = send(&app, get_cookie("/api/v1/operator/audit", &op)).await;
    let raw = String::from_utf8(body).unwrap();
    assert!(raw.contains("project.delete") && raw.contains("Official M31"),
        "delete audit must snapshot the title: {raw}");
}
```

- [ ] **Step 2: run** → FAIL. **Step 3: implement.** Factor `create_project_core` (coordinator account + `body.coordinator_display_name` parameterized where 348–357 binds `auth.account_id`); user handler = rate limit + core with `auth.account_id`. Operator handlers:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorCreateProject {
    #[serde(flatten)]
    pub base: crate::routes::projects::CreateProject,
    pub coordinator_email: String,
}

pub async fn create_project(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    Json(body): Json<OperatorCreateProject>,
) -> Result<Json<crate::routes::projects::ProjectView>, ApiError> {
    let email = body.coordinator_email.trim().to_ascii_lowercase();
    let coordinator: Option<Uuid> = sqlx::query_scalar("SELECT id FROM accounts WHERE lower(email)=$1")
        .bind(&email).fetch_optional(&state.db).await?;
    let coordinator = coordinator.ok_or_else(|| ApiError::not_found("no account with that email"))?;
    let mut tx = state.db.begin().await?;
    let view = crate::routes::projects::create_project_core(&mut tx, coordinator, &body.base).await?;
    audit_tx(&mut tx, op.account_id, "project.create", "project", &view_id_string(&view),
        serde_json::json!({"coordinatorEmail": email})).await?;
    tx.commit().await?;
    Ok(Json(view))
}
// view_id_string: ProjectView's id field — if private, add a `pub(crate) fn id(&self) -> Uuid`
// accessor in projects.rs or make the field pub(crate); pick whichever matches the file's style.

pub async fn delete_project(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;
    let snap: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT p.slug, p.title,
                (SELECT count(*) FROM project_members m WHERE m.project_id = p.id)
         FROM projects p WHERE p.id = $1").bind(id).fetch_optional(&mut *tx).await?;
    let (slug, title, member_count) = snap.ok_or_else(|| ApiError::not_found("project not found"))?;
    audit_tx(&mut tx, op.account_id, "project.delete", "project", &id.to_string(),
        serde_json::json!({"slug": slug, "title": title, "memberCount": member_count})).await?;
    sqlx::query("DELETE FROM projects WHERE id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}
```

Register both routes.

- [ ] **Step 4: run** → PASS; full suite green (existing user-create tests re-pin the factored core).
- [ ] **Step 5: commit** `feat(hub): operator project create (assigned coordinator) + hard delete with audit snapshot`

---

### Task 11: accounts & devices endpoints (+ S1 test)

**Files:**
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_accounts.rs`

**Interfaces:**
- Produces: `GET /api/v1/operator/accounts?search=` → `Vec<OperatorAccountRow{id, email, createdAt, lastSeenAt, blockedAt, deviceCount, projectCount}>` — **`accounts` has NO `last_seen_at` column**: derive it as `max(devices.last_seen_at)`; `GET /api/v1/operator/accounts/{id}/devices` → `Vec<OperatorDeviceRow{id, name, capability, createdAt, lastSeenAt, revokedAt}>` — **NO endpoint_addr** (S1); `name` is nullable (`Option<String>`), `capability` is NOT NULL (`String`); `POST /api/v1/operator/accounts/{id}/block|unblock` body `{note?}`; `POST /api/v1/operator/devices/{id}/revoke`.

- [ ] **Step 1: failing tests** `tests/operator_accounts.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn accounts_block_guards_and_s1(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io", "op2@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (user_token, _) = register_device(&app, &mailer, "user@x.io", 1, "pc").await;
    let _ = portal_sign_in(&app, &mailer, "op2@x.io").await; // materialize fellow-operator account

    let (st, body) = send(&app, get_cookie("/api/v1/operator/accounts?search=user", &op)).await;
    assert_eq!(st, StatusCode::OK);
    let raw = String::from_utf8(body).unwrap();
    assert!(raw.contains("user@x.io") && raw.contains("\"deviceCount\":1"));
    assert!(!raw.contains("endpointAddr") && !raw.contains("directAddrs"), "S1: {raw}");

    let user_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='user@x.io'")
        .fetch_one(&pool).await.unwrap();
    let op_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='op@x.io'")
        .fetch_one(&pool).await.unwrap();
    let op2_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='op2@x.io'")
        .fetch_one(&pool).await.unwrap();

    let (st, _) = send(&app, post_cookie(&format!("/api/v1/operator/accounts/{op_id}/block"),
        &json!({}), &op)).await;
    assert_eq!(st, StatusCode::CONFLICT, "cannot block self");
    let (st, _) = send(&app, post_cookie(&format!("/api/v1/operator/accounts/{op2_id}/block"),
        &json!({}), &op)).await;
    assert_eq!(st, StatusCode::CONFLICT, "cannot block a fellow operator");

    let (st, _) = send(&app, post_cookie(&format!("/api/v1/operator/accounts/{user_id}/block"),
        &json!({"note": "abuse"}), &op)).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, get("/api/v1/devices", Some(&user_token))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, body) = send(&app, get_cookie(&format!("/api/v1/operator/accounts/{user_id}/devices"), &op)).await;
    assert_eq!(st, StatusCode::OK);
    let raw = String::from_utf8(body).unwrap();
    assert!(!raw.contains("endpointAddr"), "S1 devices: {raw}");

    let dev_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM devices WHERE account_id=$1")
        .bind(user_id).fetch_one(&pool).await.unwrap();
    let (st, _) = send(&app, post_cookie(&format!("/api/v1/operator/devices/{dev_id}/revoke"),
        &json!({}), &op)).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, post_cookie(&format!("/api/v1/operator/accounts/{user_id}/unblock"),
        &json!({}), &op)).await;
    assert_eq!(st, StatusCode::OK);
}
```

- [ ] **Step 2: run** → FAIL. **Step 3: implement** in `operator.rs`:

```rust
#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OperatorAccountRow {
    pub id: Uuid,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub blocked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub device_count: i64,
    pub project_count: i64,
}

#[derive(serde::Deserialize)]
pub struct AccountsQuery { pub search: Option<String> }

pub async fn list_accounts(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<AccountsQuery>,
) -> Result<Json<Vec<OperatorAccountRow>>, ApiError> {
    let rows = sqlx::query_as::<_, OperatorAccountRow>(
        "SELECT a.id, a.email, a.created_at,
                (SELECT max(d.last_seen_at) FROM devices d WHERE d.account_id = a.id) AS last_seen_at,
                a.blocked_at,
                (SELECT count(*) FROM devices d WHERE d.account_id=a.id AND d.revoked_at IS NULL) AS device_count,
                (SELECT count(*) FROM project_members m WHERE m.account_id=a.id) AS project_count
         FROM accounts a
         WHERE $1::text IS NULL OR a.email ILIKE '%' || $1 || '%'
         ORDER BY a.created_at DESC LIMIT 200",
    ).bind(q.search).fetch_all(&state.db).await?;
    Ok(Json(rows))
}

#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OperatorDeviceRow {
    pub id: Uuid,
    pub name: Option<String>,          // nullable in schema (0001)
    pub capability: String,            // NOT NULL DEFAULT 'athenaeum' (0007)
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list_account_devices(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Vec<OperatorDeviceRow>>, ApiError> {
    // Deliberately NOT DEVICE_COLUMNS: endpoint_addr must never leave operator responses (S1).
    let rows = sqlx::query_as::<_, OperatorDeviceRow>(
        "SELECT id, name, capability, created_at, last_seen_at, revoked_at
         FROM devices WHERE account_id = $1 ORDER BY created_at DESC",
    ).bind(id).fetch_all(&state.db).await?;
    Ok(Json(rows))
}

#[derive(serde::Deserialize)]
pub struct BlockBody { pub note: Option<String> }

async fn set_blocked(
    state: &AppState, op: &OperatorAuth, target: Uuid, blocked: bool, note: Option<String>,
) -> Result<StatusCode, ApiError> {
    let email: Option<String> = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(target).fetch_optional(&state.db).await?;
    let email = email.ok_or_else(|| ApiError::not_found("account not found"))?;
    let lower = email.to_ascii_lowercase();
    if blocked && (target == op.account_id || state.operator_emails.iter().any(|e| e == &lower)) {
        return Err(ApiError::conflict("cannot block an operator account"));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query(if blocked { "UPDATE accounts SET blocked_at = now() WHERE id = $1" }
                else { "UPDATE accounts SET blocked_at = NULL WHERE id = $1" })
        .bind(target).execute(&mut *tx).await?;
    audit_tx(&mut tx, op.account_id, if blocked { "account.block" } else { "account.unblock" },
        "account", &target.to_string(), serde_json::json!({"email": lower, "note": note})).await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}

pub async fn block_account(State(state): State<AppState>, Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>, Json(b): Json<BlockBody>)
    -> Result<StatusCode, ApiError> { set_blocked(&state, &op, id, true, b.note).await }

pub async fn unblock_account(State(state): State<AppState>, Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>, Json(b): Json<BlockBody>)
    -> Result<StatusCode, ApiError> { set_blocked(&state, &op, id, false, b.note).await }

pub async fn revoke_device(
    State(state): State<AppState>, Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;
    let n = sqlx::query("UPDATE devices SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(id).execute(&mut *tx).await?.rows_affected();
    if n == 0 { return Err(ApiError::not_found("device not found or already revoked")); }
    audit_tx(&mut tx, op.account_id, "device.revoke", "device", &id.to_string(),
        serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}
```

Register the five routes.

- [ ] **Step 4: run** → PASS; full suite green.
- [ ] **Step 5: commit** `feat(hub): operator accounts/devices — search, block guards, revoke, S1-clean views`

---

### Task 12: portal — `/me.operator`, nav, page scaffold + Dashboard + Audit tabs

**Files:**
- Modify: `portal/src/useMe.ts` (Me at 4–7), `portal/src/App.tsx` (routes 13–19), `portal/src/Layout.tsx` (nav; it already calls `useMe` at line 6), `portal/src/types.ts`
- Create: `portal/src/pages/Operator.tsx`

**Interfaces:**
- Consumes: Tasks 4–5 wire shapes.
- Produces: `Me.operator: boolean`; route `/operator`; types `OperatorOverview`, `OperatorEvent`, `OperatorAuditRow`; `Operator.tsx` default export with tab state `'dashboard' | 'projects' | 'accounts' | 'audit'` (Projects/Accounts tabs are placeholder components REPLACED by Tasks 13–14 of this same plan).

- [ ] **Step 1: types + hook.** `useMe.ts`: add `operator: boolean` to `Me`. `types.ts` append:

```ts
export interface OperatorEvent { at: string; kind: string; projectSlug: string | null; actorEmail: string | null; detail: unknown }
export interface OperatorOverview {
  accounts: number; devicesActive: number; projectsActive: number; projectsHidden: number;
  projectsClosed: number; members: number; announcementsByState: Record<string, number>;
  joinRequestsOpen: number; events: OperatorEvent[];
}
export interface OperatorAuditRow { id: number; actorEmail: string; action: string; targetKind: string; targetId: string; detail: unknown; at: string }
```

- [ ] **Step 2: page.** `portal/src/pages/Operator.tsx` (follow Admin.tsx patterns — `space-y-8` root, `section space-y-2`, `h2 font-semibold`, status line `text-sm text-neutral-500`):

```tsx
import { useEffect, useState } from 'react';
import { apiGet } from '../api';
import { useMe } from '../useMe';
import type { OperatorAuditRow, OperatorOverview } from '../types';

type Tab = 'dashboard' | 'projects' | 'accounts' | 'audit';

export default function Operator() {
  const { me, loading } = useMe();
  const [tab, setTab] = useState<Tab>('dashboard');
  if (loading) return <p className="text-sm text-neutral-500">Loading…</p>;
  if (!me) return <p className="text-sm text-neutral-500">Sign in first.</p>;
  if (!me.operator) return <p className="text-sm text-neutral-500">Operator access only.</p>;
  return (
    <div className="space-y-8">
      <h1 className="text-xl font-semibold">Operator console</h1>
      <nav className="flex gap-4 text-sm">
        {(['dashboard', 'projects', 'accounts', 'audit'] as Tab[]).map((t) => (
          <button key={t} onClick={() => setTab(t)}
            className={t === tab ? 'font-semibold underline' : 'underline'}>{t}</button>
        ))}
      </nav>
      {tab === 'dashboard' && <Dashboard />}
      {tab === 'projects' && <ProjectsTab />}
      {tab === 'accounts' && <AccountsTab />}
      {tab === 'audit' && <AuditTab />}
    </div>
  );
}

function Dashboard() {
  const [o, setO] = useState<OperatorOverview | null>(null);
  useEffect(() => { apiGet<OperatorOverview>('/api/v1/operator/overview').then(setO)
    .catch((e) => console.error('[operator] overview failed:', e)); }, []);
  if (!o) return <p className="text-sm text-neutral-500">Loading…</p>;
  const tiles: [string, number][] = [
    ['Accounts', o.accounts], ['Active devices', o.devicesActive],
    ['Active projects', o.projectsActive], ['Hidden', o.projectsHidden],
    ['Closed', o.projectsClosed], ['Members', o.members], ['Open joins', o.joinRequestsOpen],
  ];
  return (
    <div className="space-y-6">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {tiles.map(([label, n]) => (
          <div key={label} className="rounded border border-neutral-300 px-3 py-2 dark:border-neutral-700">
            <div className="text-2xl font-semibold">{n}</div>
            <div className="text-sm text-neutral-500">{label}</div>
          </div>
        ))}
      </div>
      <section className="space-y-2">
        <h2 className="font-semibold">Recent events</h2>
        <ul className="space-y-1 text-sm">
          {o.events.map((e, i) => (
            <li key={i}>
              <span className="text-neutral-500">{e.at.slice(0, 16).replace('T', ' ')}</span>{' '}
              {e.kind} {e.projectSlug && <span className="text-neutral-500">· {e.projectSlug}</span>}
              {e.actorEmail && <span className="text-neutral-500"> · {e.actorEmail}</span>}
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}

function AuditTab() {
  const [rows, setRows] = useState<OperatorAuditRow[]>([]);
  useEffect(() => { apiGet<OperatorAuditRow[]>('/api/v1/operator/audit?limit=200').then(setRows)
    .catch((e) => console.error('[operator] audit failed:', e)); }, []);
  return (
    <section className="space-y-2">
      <h2 className="font-semibold">Audit</h2>
      <table className="w-full text-left text-sm">
        <thead><tr className="text-neutral-500"><th>When</th><th>Actor</th><th>Action</th><th>Target</th></tr></thead>
        <tbody>{rows.map((r) => (
          <tr key={r.id}><td>{r.at.slice(0, 16).replace('T', ' ')}</td><td>{r.actorEmail}</td>
            <td>{r.action}</td><td>{r.targetKind} {r.targetId.slice(0, 8)}</td></tr>
        ))}</tbody>
      </table>
    </section>
  );
}

function ProjectsTab() { return <p className="text-sm text-neutral-500">Projects — next commit.</p>; }
function AccountsTab() { return <p className="text-sm text-neutral-500">Accounts — next commit.</p>; }
```

- [ ] **Step 3: routing + nav.** `App.tsx`: add `<Route path="/operator" element={<Operator />} />` inside the Layout route (13–19); import the page. `Layout.tsx`: next to the existing "New project" link, render `<Link to="/operator">Operator</Link>` when `me?.operator`.
- [ ] **Step 4: verify** `cd portal && npm ci && npm run build` → tsc + vite succeed.
- [ ] **Step 5: commit** `feat(portal): operator page scaffold — dashboard + audit tabs, nav gated by /me.operator`

---

### Task 13: portal — Projects tab (actions) + Directory featured badge

**Files:**
- Modify: `portal/src/pages/Operator.tsx` (replace `ProjectsTab`), `portal/src/types.ts`, `portal/src/pages/Directory.tsx` (featured badge)

**Interfaces:**
- Consumes: Tasks 6–10 wire shapes; the page type in types.ts is **`ProjectPageData`** (not `ProjectPage`); member identity comes from the OPERATOR members endpoint (Task 9) — the public `MemberPublicView` deliberately has NO `accountId`.
- Produces: full Projects tab + `DirectoryItem.featured` badge.

- [ ] **Step 1: types.** Append to `types.ts` (and add `featured: boolean` to the existing `DirectoryItem`):

```ts
export interface OperatorProjectRow {
  id: string; slug: string; title: string; status: string; featured: boolean;
  hiddenAt: string | null; createdAt: string; coordinatorEmail: string | null;
  memberCount: number; announcementCount: number; lastEventAt: string | null;
}
export interface OperatorMemberRow {
  accountId: string; email: string; displayName: string; dataRole: string;
  coordinator: boolean; joinedAt: string;
}
```

- [ ] **Step 2: implement `ProjectsTab`** (replace the placeholder). Data: rows from `apiGet<OperatorProjectRow[]>('/api/v1/operator/projects')`; expanded row loads `apiGet<OperatorMemberRow[]>('/api/v1/operator/projects/'+p.id+'/members')` and `apiGet<JoinRequestView[]>('/api/v1/operator/projects/'+p.id+'/join-requests')`. Actions (each → reload + status line, Admin.tsx `act` pattern):
  - Feature/Unfeature → `apiPatch('/api/v1/operator/projects/'+p.id, {featured: !p.featured})`
  - Hide/Unhide → `{hidden: !p.hiddenAt}`; Close/Reopen → `{status: p.status === 'active' ? 'closed' : 'active'}`
  - Coordinator… → `window.prompt('New coordinator email')` → `apiPost('.../coordinator', {email})`
  - Join requests: Approve/Reject → `apiPost('.../join-requests/'+r.id+'/decide', {action: 'approve'|'reject'})`
  - Member remove → `window.confirm` → `apiDelete('.../members/'+m.accountId)` (skip the button when `m.coordinator`)
  - Delete… → `window.prompt('Type the slug to confirm')` === `p.slug` → `apiDelete('/api/v1/operator/projects/'+p.id)`

Write the full JSX with the Admin.tsx class constants (`field`, primary button, `underline` text buttons); table columns: Project (title + slug + `featured`/`hidden` badges), Coordinator, Members, Announcements, Status, Actions.

- [ ] **Step 3: Directory badge.** In `Directory.tsx`, render a `★ featured` badge (small `text-amber-500` span) on items where `item.featured` — the API already sorts them first (Task 7).
- [ ] **Step 4: verify** `cd portal && npm run build` → clean.
- [ ] **Step 5: commit** `feat(portal): operator projects tab — flags, coordinator, joins, member removal, typed-slug delete; directory featured badge`

---

### Task 14: portal — Accounts tab

**Files:**
- Modify: `portal/src/pages/Operator.tsx` (replace `AccountsTab`), `portal/src/types.ts`

**Interfaces:**
- Consumes: Task 11 wire shapes (note `name: string | null`, `capability: string`).

- [ ] **Step 1: types**:

```ts
export interface OperatorAccountRow {
  id: string; email: string; createdAt: string; lastSeenAt: string | null;
  blockedAt: string | null; deviceCount: number; projectCount: number;
}
export interface OperatorDeviceRow {
  id: string; name: string | null; capability: string; createdAt: string;
  lastSeenAt: string | null; revokedAt: string | null;
}
```

- [ ] **Step 2: implement `AccountsTab`**: search input (`field` class) driving `apiGet('/api/v1/operator/accounts?search='+encodeURIComponent(q))`; rows: email, created/last-seen, device/project counts, `Blocked` badge when `blockedAt`; actions: `Block…` (prompt note → `apiPost('.../block', {note})`) / `Unblock` (`apiPost('.../unblock', {})`); expandable device list via `apiGet('.../accounts/'+a.id+'/devices')` showing `d.name ?? '(unnamed)'`, capability, revoked state, with `Revoke` (`window.confirm` → `apiPost('/api/v1/operator/devices/'+d.id+'/revoke', {})`). Full JSX in the Admin.tsx style; reload after each action; errors to the status line.
- [ ] **Step 3: verify** `cd portal && npm run build` → clean.
- [ ] **Step 4: commit** `feat(portal): operator accounts tab — search, block/unblock, device revoke`

---

### Task 15: astronet env, docs, final gates, test-hub deploy prep

**Files:**
- Modify: `/Volumes/BigMac/Users/astrobureau/Documents/astronet/templates/hub.env.j2`
- Modify: `/Volumes/BigMac/Users/astrobureau/Documents/astronet/inventory.yml` (both hub groups)
- Modify: `/Volumes/BigMac/Users/astrobureau/Documents/astronet/CLAUDE.md` (hub vars doc ~line 576–581)
- Modify: `athenaeum-hub/README.md` (operator section)

- [ ] **Step 1: env template.** In `hub.env.j2` (after the mailer block):

```jinja
{% if hub_operator_emails | default('') %}
HUB_OPERATOR_EMAILS={{ hub_operator_emails }}
{% endif %}
```

In `inventory.yml`, add to BOTH `athenaeum_hub` and `athenaeum_hub_test` group vars: `hub_operator_emails: "vilen.sharifov@gmail.com"`. In astronet `CLAUDE.md` hub-vars line, append `hub_operator_emails → HUB_OPERATOR_EMAILS (comma-separated operator allowlist)`.
- [ ] **Step 2: hub README** — add an "Operator console" section: env var, `/operator` portal path, audit table, blocked-account semantics (3–6 lines).
- [ ] **Step 3: final gates.** `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test` (full, all green), `cd portal && npm run build`, then push branch: `git push -u origin operator-console` → hub CI green (`test` + `build` jobs).
- [ ] **Step 4: test-hub deploy (owner confirms 1Password prompts):**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/astronet
ansible-playbook deploy_athenaeum_hub.yml -e hub_target=athenaeum_hub_test -e hub_artifact_ref=operator-console
```

Verify: `curl https://test-hub.artfrom.space/api/v1/health`; portal sign-in as the owner → `/operator` renders all four tabs; a non-operator account gets no nav link and 403 on `/api/v1/operator/audit`. Commit astronet changes: `git commit -am "hub: HUB_OPERATOR_EMAILS for operator console"`.
- [ ] **Step 5: commit + wrap.** Final hub commit if README pending; update the athenaeum-repo memory ledger (operator console CODE COMPLETE on `operator-console`, test-hub soaked, prod deploy = owner procedure per hub-deploy discipline).

---

## Self-review + verification notes

- **Spec coverage**: identity/choke point (T4), migration (T2), blocked enforcement incl. OTP (T3), overview (T5), listing (T6), patch + hidden/featured/closed semantics + directory badge data (T7), coordinator any-account (T8), joins/members on behalf + operator listings (T9), create/delete (T10), accounts/devices/S1 (T11), portal (T12–14), env/deploy/test-hub-first (T15). Evolution section requires no code. Non-goals honored.
- **2026-07-18 adversarial fact-check corrections folded in**: `ApiError::Status(...)` form (no unauthorized/forbidden constructors); test-helper builder signatures + `send()` wrapping; `create_project_via` 4-arg/Value return; `join_and_approve` 6-arg coordinator-first; no `futures` dep (manual OTP drive); `security::hash_token` + `portal_auth::session_cookie_value` qualification; `package_announcements`; `project_events.created_at`; derived account `last_seen_at`; device `name` nullable / `capability` NOT NULL; nested `target` in CreateProject; parenthesized directory WHERE; `ProjectPageData` type name; operator members endpoint (public page hides accountId); `DirectoryItem.featured` added end-to-end; event kind `"created"`.
