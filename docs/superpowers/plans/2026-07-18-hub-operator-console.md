# Hub Operator Console Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instance-level operator console (dashboard, all-projects management, accounts/devices, audit) in the hub + portal, gated by an env-allowlist operator role, per `docs/superpowers/specs/2026-07-18-hub-operator-console-design.md` (the spec lives in the athenaeum repo; work happens in the athenaeum-hub repo + one astronet task).

**Architecture:** `HUB_OPERATOR_EMAILS` env → `AppState.operator_emails`; a `require_operator` middleware (layered INSIDE `require_account`) is the single choke point for a new `/api/v1/operator/*` router; `blocked_at` enforcement folds into the existing auth SQL; all operator mutations write `operator_audit` in the same transaction; portal gets an `/operator` page gated by `/me.operator`.

**Tech Stack:** Rust/Axum/sqlx (Postgres), React portal (vite, `apiGet/apiPost` helpers), `#[sqlx::test]` + `tower::ServiceExt::oneshot` test harness, astronet Ansible for env.

## Global Constraints

- Repo: `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`, branch **`operator-console`** off `main` (`4638a5d`). Astronet task in `/Volumes/BigMac/Users/astrobureau/Documents/astronet`.
- Tests: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test` (local `hub-test-pg` docker postgres). Every test task: red → green.
- Wire types `#[serde(rename_all = "camelCase")]`; portal mirrors in `portal/src/types.ts`.
- **S1**: no operator response may contain `endpointAddr` / direct addrs — raw-body substring tests required.
- Every operator mutation inserts an `operator_audit` row **in the same tx**.
- Migration `0012_operator_console.sql`; header comment convention `-- 0012_operator_console — <one line>`.
- Commits as `eg013ra1n` (repo-configured identity), messages `feat(hub): …` / `test(hub): …` / `feat(portal): …`.
- Axum layering rule (used in Tasks 4+): `route_layer` added LAST runs FIRST (outermost). `require_account` must run before `require_operator`, so add `.route_layer(require_operator).route_layer(require_account)` in that source order.

---

### Task 1: Config + AppState plumbing for operator emails

**Files:**
- Modify: `src/config.rs` (struct at 5–15, `from_env` at 40–64)
- Modify: `src/routes/mod.rs` (`AppState` 32–58)
- Modify: `src/lib.rs:57-58` (wiring)

**Interfaces:**
- Produces: `Config.operator_emails: Vec<String>` (lowercased, trimmed, deduped-not-required); `pub fn parse_operator_emails(raw: &str) -> Vec<String>`; `AppState.operator_emails: Arc<Vec<String>>`; builder `with_operator_emails(self, Vec<String>) -> Self`.

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

- [ ] **Step 2: run** `cargo test operator_emails_parse -p athenaeum-hub` → FAIL (fn not found).
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

In `src/routes/mod.rs` add to `AppState`: `pub operator_emails: Arc<Vec<String>>`; in `AppState::new` init `operator_emails: Arc::new(Vec::new())`; add builder below `with_relay_auth_token` (55–58):

```rust
pub fn with_operator_emails(mut self, emails: Vec<String>) -> Self {
    self.operator_emails = Arc::new(emails);
    self
}
```

In `src/lib.rs:57-58` chain `.with_operator_emails(config.operator_emails.clone())`.

- [ ] **Step 4: run** `cargo test -p athenaeum-hub` (whole suite compiles + new test passes).
- [ ] **Step 5: commit** `feat(hub): HUB_OPERATOR_EMAILS config + AppState plumbing`

---

### Task 2: Migration 0012 + schema smoke test

**Files:**
- Create: `migrations/0012_operator_console.sql`
- Create: `tests/operator_schema.rs` (model on `tests/collab_schema.rs` structure)

**Interfaces:**
- Produces: columns `projects.featured` (bool NOT NULL DEFAULT false), `projects.hidden_at` (timestamptz NULL), `accounts.blocked_at` (timestamptz NULL); table `operator_audit(id bigserial PK, actor uuid NOT NULL REFERENCES accounts(id), action text NOT NULL, target_kind text NOT NULL, target_id text NOT NULL, detail jsonb, at timestamptz NOT NULL DEFAULT now())`.

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
    // projects flags exist with defaults
    let ok: (bool, Option<chrono::DateTime<chrono::Utc>>,) = sqlx::query_as(
        "SELECT false AS featured, NULL::timestamptz AS hidden_at")
        .fetch_one(&pool).await.unwrap();
    let _ = ok;
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
- Modify: `src/routes/auth.rs` (`verify_otp`, blocked check after `upsert_account_tx` — around line 396)
- Modify: `src/routes/portal_auth.rs` (`portal_verify`, same — around line 54)
- Create: `tests/operator_blocked.rs`

**Interfaces:**
- Consumes: `accounts.blocked_at` (Task 2).
- Produces: blocked accounts get 401 on every authenticated path and cannot complete OTP sign-in (message `account blocked`).

- [ ] **Step 1: failing tests** `tests/operator_blocked.rs` (uses `common` helpers: `app_with_capture`, `register_device`, `portal_sign_in`, `get`, `send`, `post_cookie`):

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
    let (st, _) = send(&app, get("/api/v1/devices", &token)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "blocked bearer must 401");
    let (st, _) = get_cookie(&app, "/api/v1/me", &session).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "blocked portal session must 401");
}

#[sqlx::test]
async fn blocked_account_cannot_sign_in_and_unblock_restores(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (_t, _d) = register_device(&app, &mailer, "u@x.io", 1, "pc").await;
    block(&pool, "u@x.io").await;
    // re-registration path must refuse at OTP verify
    let attempt = std::panic::AssertUnwindSafe(register_device(&app, &mailer, "u@x.io", 2, "pc2"));
    // register_device asserts 200 internally, so instead drive verify manually if it panics:
    let refused = futures::FutureExt::catch_unwind(attempt).await.is_err();
    assert!(refused, "blocked OTP verify must not issue a token");
    sqlx::query("UPDATE accounts SET blocked_at = NULL WHERE email = 'u@x.io'")
        .execute(&pool).await.unwrap();
    let (_t2, _d2) = register_device(&app, &mailer, "u@x.io", 3, "pc3").await; // works again
}
```

(If `futures` isn't already a dev-dependency, drive the OTP request/verify calls directly with `post`+`send` instead of `register_device` and assert `StatusCode::UNAUTHORIZED` — inspect `register_device` at `tests/common/mod.rs:194-229` for the exact request bodies and replicate its two calls.)

- [ ] **Step 2: run** `cargo test --test operator_blocked` → FAIL.
- [ ] **Step 3: implement.** In `src/auth_mw.rs` change the two `require_account` lookups and the `require_auth` lookup to JOIN accounts:

```sql
-- bearer branch (was: FROM devices WHERE token_hash=$1 AND revoked_at IS NULL)
SELECT d.id, d.account_id, d.role FROM devices d
JOIN accounts a ON a.id = d.account_id
WHERE d.token_hash = $1 AND d.revoked_at IS NULL AND a.blocked_at IS NULL

-- session branch (was: FROM portal_sessions WHERE token_hash=$1 AND expires_at>now())
SELECT s.id, s.account_id FROM portal_sessions s
JOIN accounts a ON a.id = s.account_id
WHERE s.token_hash = $1 AND s.expires_at > now() AND a.blocked_at IS NULL
```

Apply the same JOIN to `require_auth`'s device query (75–82). In `src/routes/auth.rs::verify_otp` right after `upsert_account_tx` returns `account_id` (~396) and in `src/routes/portal_auth.rs::portal_verify` (~54):

```rust
let blocked: Option<Option<chrono::DateTime<chrono::Utc>>> =
    sqlx::query_scalar("SELECT blocked_at FROM accounts WHERE id = $1")
        .bind(account_id).fetch_optional(&mut *tx).await?;
if matches!(blocked, Some(Some(_))) {
    return Err(ApiError::unauthorized("account blocked"));
}
```

(Match the file's actual error-constructor names — `ApiError::unauthorized` per existing usage in `auth_mw.rs`.)

- [ ] **Step 4: run** `cargo test --test operator_blocked` → PASS; then full `cargo test` → no regressions.
- [ ] **Step 5: commit** `feat(hub): blocked_at enforcement on bearer/session/OTP paths`

---

### Task 4: OperatorAuth middleware, `/me.operator`, audit read route

**Files:**
- Create: `src/routes/operator.rs`
- Modify: `src/routes/mod.rs` (module decl + router 92–182)
- Modify: `src/routes/me.rs` (`MeResponse` 12–17, handler 19–32)
- Modify: `tests/common/mod.rs` (new helper)
- Create: `tests/operator_gate.rs`

**Interfaces:**
- Produces: `pub struct OperatorAuth { pub account_id: Uuid, pub email: String }`; middleware `pub async fn require_operator(State<AppState>, Request, Next) -> Response`; `pub async fn audit_tx(conn: &mut PgConnection, actor: Uuid, action: &str, target_kind: &str, target_id: &str, detail: serde_json::Value) -> Result<(), sqlx::Error>`; route `GET /api/v1/operator/audit?limit=` → `Vec<AuditRow{id, actorEmail, action, targetKind, targetId, detail, at}>`; `MeResponse.operator: bool`; test helper `app_with_operators(pool, &["email"]) -> (Router, CaptureMailer)`.

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

    let (st, body) = get_cookie(&app, "/api/v1/me", &op).await;
    assert_eq!(st, StatusCode::OK);
    assert!(String::from_utf8(body).unwrap().contains("\"operator\":true"));
    let (_, body) = get_cookie(&app, "/api/v1/me", &user).await;
    assert!(String::from_utf8(body).unwrap().contains("\"operator\":false"));

    let (st, _) = get_cookie(&app, "/api/v1/operator/audit", &user).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "non-operator must get 403");
    let (st, body) = get_cookie(&app, "/api/v1/operator/audit", &op).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(String::from_utf8(body).unwrap(), "[]");
}

#[sqlx::test]
async fn empty_allowlist_is_inert(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let s = portal_sign_in(&app, &mailer, "anyone@x.io").await;
    let (st, _) = get_cookie(&app, "/api/v1/operator/audit", &s).await;
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
    middleware::Next,
    response::Response,
    Extension, Json,
};
use sqlx::PgConnection;
use uuid::Uuid;

use super::AppState;
use crate::auth_mw::AuthAccount;
use crate::error::ApiError; // match the actual error module path used by sibling routes

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
        .ok_or_else(|| ApiError::unauthorized("sign in required"))?;
    let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(auth.account_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| ApiError::unauthorized("sign in required"))?;
    let lower = email.to_ascii_lowercase();
    if !state.operator_emails.iter().any(|e| e == &lower) {
        return Err(ApiError::forbidden("operator only"));
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

(`AuthAccount` must be `Clone` — add `#[derive(Clone)]` in `auth_mw.rs` if missing. If the error type lives elsewhere than `crate::error::ApiError`, mirror what `src/routes/members.rs` imports.)

In `src/routes/mod.rs`: `pub mod operator;`; in `build_router` add before the final merge chain:

```rust
let operator = Router::new()
    .route("/api/v1/operator/audit", get(operator::list_audit))
    // require_account must run BEFORE require_operator → add operator layer first (inner)
    .route_layer(axum::middleware::from_fn_with_state(state.clone(), operator::require_operator))
    .route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::auth_mw::require_account));
```

and merge: `.merge(operator)` alongside the existing merges (176–181).

In `src/routes/me.rs`: add `pub operator: bool` to `MeResponse`; in the handler compute after the email SELECT:

```rust
let operator = state.operator_emails.iter().any(|e| e == &email.to_ascii_lowercase());
```

In `tests/common/mod.rs` below `app_with_capture` (82–86):

```rust
#[allow(dead_code)]
pub fn app_with_operators(pool: sqlx::PgPool, emails: &[&str]) -> (axum::Router, CaptureMailer) {
    let mailer = CaptureMailer::default();
    let state = crate::_hub_state_helper(pool, mailer.clone(), emails); // see note
    (athenaeum_hub::routes::build_router(state), mailer)
}
```

Note: replicate the exact body of `app_with_capture` and chain `.with_operator_emails(emails.iter().map(|s| s.to_string()).collect())` on the `AppState` — no separate helper fn needed if inlined.

- [ ] **Step 4: run** `cargo test --test operator_gate` → PASS; `cargo test` → green.
- [ ] **Step 5: commit** `feat(hub): operator gate (env allowlist), /me.operator, audit table read`

---

### Task 5: overview endpoint (dashboard data)

**Files:**
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs` (add route)
- Create: `tests/operator_overview.rs`

**Interfaces:**
- Produces: `GET /api/v1/operator/overview` → `Overview{accounts, devicesActive, projectsActive, projectsHidden, projectsClosed, members, announcementsByState: {state: count}, joinRequestsOpen, events: [{at, kind, projectSlug, actorEmail, detail}]}` (events = project_events joined to projects+accounts, newest-first, limit 50).

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
    let project = create_project_via(&app, &coord_token, "M31 Mosaic").await; // see common helper 273–303 for exact signature
    let _ = project;
    let (st, body) = get_cookie(&app, "/api/v1/operator/overview", &op).await;
    assert_eq!(st, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["accounts"].as_i64().unwrap() >= 2);
    assert_eq!(v["projectsActive"].as_i64().unwrap(), 1);
    assert!(v["events"].as_array().unwrap().iter().any(|e| e["kind"] == "project_created"
        || e["kind"].as_str().unwrap_or("").contains("created")),
        "project creation must appear in events: {v}");
}
```

(Adjust the expected event `kind` to whatever `record_event_tx` writes on creation — check `create_project` in `src/routes/projects.rs:234-384`; if creation records no event, assert on any event kind after performing a `join_and_approve` instead.)

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
        "SELECT state, count(*) FROM announcements GROUP BY state").fetch_all(db).await.unwrap_or_default();
    let join_requests_open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM join_requests WHERE status = 'open'").fetch_one(db).await?;
    let events = sqlx::query_as::<_, EventRow>(
        "SELECT e.at, e.kind, p.slug AS project_slug, a.email AS actor_email, e.detail
         FROM project_events e
         LEFT JOIN projects p ON p.id = e.project_id
         LEFT JOIN accounts a ON a.id = e.actor
         ORDER BY e.at DESC LIMIT 50",
    ).fetch_all(db).await?;
    Ok(Json(Overview {
        accounts, devices_active, projects_active, projects_hidden, projects_closed,
        members, announcements_by_state: ann.into_iter().collect(), join_requests_open, events,
    }))
}
```

(Verify the announcements table/column names against `src/routes/announcements.rs` — if the state column is named differently (e.g. `status`), match it; if `project_events` columns differ (check `EVENT_INSERT` at `src/collab_auth.rs:73-74` and migration 0009), match them.) Register `.route("/api/v1/operator/overview", get(operator::overview))` in the operator router.

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
    let _p = create_project_via(&app, &t, "M31 Mosaic").await;
    let (st, body) = get_cookie(&app, "/api/v1/operator/projects", &op).await;
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
                (SELECT count(*) FROM announcements an WHERE an.project_id = p.id) AS announcement_count,
                (SELECT max(e.at) FROM project_events e WHERE e.project_id = p.id) AS last_event_at
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

### Task 7: operator PATCH (featured/hidden/status) + hidden semantics enforcement

**Files:**
- Modify: `src/routes/operator.rs` (PATCH handler)
- Modify: `src/routes/projects.rs` (`directory` WHERE at ~436 + ORDER BY; `project_page` 567–694 hidden gate)
- Modify: `src/auth_mw.rs` (new `optional_account` helper)
- Modify: `src/routes/join_requests.rs` (`create_join_request` 55–62: also refuse hidden)
- Create: `tests/operator_hidden.rs`

**Interfaces:**
- Consumes: `audit_tx` (Task 4).
- Produces: `PATCH /api/v1/operator/projects/{id}` body `{featured?: bool, hidden?: bool, status?: "active"|"closed", note?: string}`; `pub async fn optional_account(state: &AppState, headers: &HeaderMap) -> Option<Uuid>` in `auth_mw.rs` (bearer-then-cookie, read-only, no CSRF — GET use only).

- [ ] **Step 1: failing tests** `tests/operator_hidden.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn hidden_project_semantics(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (coord, _) = register_device(&app, &mailer, "coord@x.io", 1, "pc").await;
    let (member, _) = register_device(&app, &mailer, "member@x.io", 2, "pc2").await;
    let (outsider, _) = register_device(&app, &mailer, "out@x.io", 3, "pc3").await;
    let project = create_project_via(&app, &coord, "M31 Mosaic").await;
    join_and_approve(&app, &member, &coord, &project).await; // exact signature: tests/common/mod.rs:355-386

    // hide it
    let (st, _) = post_cookie(&app,
        &format!("/api/v1/operator/projects/{project}"),
        r#"{"hidden":true,"note":"spam check"}"#, &op).await; // use PATCH variant helper — see step 3 note
    assert_eq!(st, StatusCode::OK);

    // 1) absent from public directory
    let (_, body) = send(&app, get_public("/api/v1/projects")).await;
    assert!(!String::from_utf8(body).unwrap().contains("M31"), "hidden project must not be in directory");
    // 2) page 404 for anonymous and for outsider
    let (st, _) = send(&app, get_public(&format!("/api/v1/projects/{project}"))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = send(&app, get(&format!("/api/v1/projects/{project}"), &outsider)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // 3) page 200 for member and operator
    let (st, _) = send(&app, get(&format!("/api/v1/projects/{project}"), &member)).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = get_cookie(&app, &format!("/api/v1/projects/{project}"), &op).await;
    assert_eq!(st, StatusCode::OK);
    // 4) join refused while hidden
    let (st, _) = send(&app, post(&format!("/api/v1/projects/{project}/join-requests"),
        r#"{"displayName":"X","desiredRole":"send"}"#, &outsider)).await;
    assert_eq!(st, StatusCode::CONFLICT);
    // 5) audit row written
    let (_, body) = get_cookie(&app, "/api/v1/operator/audit", &op).await;
    let raw = String::from_utf8(body).unwrap();
    assert!(raw.contains("project.update") && raw.contains("spam check"));
}
```

(`get_public` = request without auth header — add a tiny local builder if `common` lacks one; `post_cookie` does POST — for PATCH add `patch_cookie` to `tests/common/mod.rs` mirroring `post_cookie` at 332–341 with method PATCH.)

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
) -> Result<axum::http::StatusCode, ApiError> {
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
    Ok(axum::http::StatusCode::OK)
}
```

`projects.rs` `directory` (~436): AND-in `p.hidden_at IS NULL`; change ORDER BY to `p.featured DESC, p.created_at DESC`.

`auth_mw.rs` new helper (read-only resolution — bearer branch then cookie branch, both WITHOUT the CSRF check, both WITH the blocked JOIN):

```rust
pub async fn optional_account(state: &AppState, headers: &HeaderMap) -> Option<Uuid> {
    if let Some(token) = bearer_token(headers) {
        if let Ok(Some(id)) = sqlx::query_scalar::<_, Uuid>(
            "SELECT d.account_id FROM devices d JOIN accounts a ON a.id = d.account_id
             WHERE d.token_hash = $1 AND d.revoked_at IS NULL AND a.blocked_at IS NULL")
            .bind(hash_token(&token)).fetch_optional(&state.db).await
        { return Some(id); }
    }
    if let Some(cookie) = session_cookie_value(headers) {
        if let Ok(Some(id)) = sqlx::query_scalar::<_, Uuid>(
            "SELECT s.account_id FROM portal_sessions s JOIN accounts a ON a.id = s.account_id
             WHERE s.token_hash = $1 AND s.expires_at > now() AND a.blocked_at IS NULL")
            .bind(hash_token(&cookie)).fetch_optional(&state.db).await
        { return Some(id); }
    }
    None
}
```

(Reuse the file's actual token-hash helper name — check how `require_account` hashes before SELECT.)

`projects.rs` `project_page` (567–694): add `headers: axum::http::HeaderMap` parameter; after `resolve_project`:

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

(`ProjectRow`/`PROJECT_COLUMNS` at 187–189 must gain `hidden_at`; add the column there and to the struct.)

`join_requests.rs` `create_join_request` (55–62): extend the status check to `SELECT status, hidden_at` and refuse with the existing conflict message when `hidden_at.is_some()`.

Register `.route("/api/v1/operator/projects/{id}", patch(operator::patch_project))`.

- [ ] **Step 4: run** `cargo test --test operator_hidden` → PASS; full `cargo test` → green (existing directory/page tests must still pass).
- [ ] **Step 5: commit** `feat(hub): operator project patch + hidden/featured semantics (directory, page gate, join refusal)`

---

### Task 8: coordinator appointment (by email, any account)

**Files:**
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_coordinator.rs`

**Interfaces:**
- Consumes: `bump_membership_version_tx`, `record_event_tx` (`src/collab_auth.rs:97-131`), `audit_tx`.
- Produces: `POST /api/v1/operator/projects/{id}/coordinator` body `{email: string}` → 200; non-member target gets a `send_receive` membership created (display_name = email).

- [ ] **Step 1: failing test**:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn appoints_non_member_as_coordinator(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (coord, _) = register_device(&app, &mailer, "old@x.io", 1, "pc").await;
    let (_new, _) = register_device(&app, &mailer, "new@x.io", 2, "pc2").await;
    let project = create_project_via(&app, &coord, "M31 Mosaic").await;

    let before: i64 = sqlx::query_scalar("SELECT membership_version FROM projects WHERE id = $1::uuid")
        .bind(&project).fetch_one(&pool).await.unwrap();
    let (st, _) = post_cookie(&app, &format!("/api/v1/operator/projects/{project}/coordinator"),
        r#"{"email":"new@x.io"}"#, &op).await;
    assert_eq!(st, StatusCode::OK);
    let after: i64 = sqlx::query_scalar("SELECT membership_version FROM projects WHERE id = $1::uuid")
        .bind(&project).fetch_one(&pool).await.unwrap();
    assert!(after > before, "membership_version must bump");
    let coords: Vec<(String,)> = sqlx::query_as(
        "SELECT a.email FROM project_members m JOIN accounts a ON a.id=m.account_id
         WHERE m.project_id=$1::uuid AND m.is_coordinator").bind(&project)
        .fetch_all(&pool).await.unwrap();
    assert_eq!(coords, vec![("new@x.io".to_string(),)]);
    let (st, _) = post_cookie(&app, &format!("/api/v1/operator/projects/{project}/coordinator"),
        r#"{"email":"ghost@x.io"}"#, &op).await;
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
) -> Result<axum::http::StatusCode, ApiError> {
    let email = body.email.trim().to_ascii_lowercase();
    let target: Option<Uuid> = sqlx::query_scalar("SELECT id FROM accounts WHERE lower(email) = $1")
        .bind(&email).fetch_optional(&state.db).await?;
    let target = target.ok_or_else(|| ApiError::not_found("no account with that email"))?;
    let mut tx = state.db.begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id=$1)")
        .bind(id).fetch_one(&mut *tx).await?;
    if !exists { return Err(ApiError::not_found("project not found")); }
    // membership upsert (deadlock-free takeover: target need not be a member)
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
        Some(op.account_id), Some(target), serde_json::json!({"by":"operator"})).await?;
    audit_tx(&mut tx, op.account_id, "project.coordinator", "project", &id.to_string(),
        serde_json::json!({"email": email})).await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::OK)
}
```

(Match `bump_membership_version_tx`/`record_event_tx` exact param types from `src/collab_auth.rs:97-131` — they take `&mut PgConnection`; pass `&mut *tx`. If `project_members` PK constraint name differs from `(project_id, account_id)` composite, mirror the ON CONFLICT target used by `approve_join_request` at `join_requests.rs:234-243`.) Register the route.

- [ ] **Step 4: run** → PASS; full suite green (snapshot tests confirm lazy re-sign covers the flip).
- [ ] **Step 5: commit** `feat(hub): operator coordinator appointment — any account, deadlock-free takeover`

---

### Task 9: on-behalf join decisions + member removal

**Files:**
- Modify: `src/routes/join_requests.rs` (factor cores from 201–297)
- Modify: `src/routes/members.rs` (factor core from 139–187)
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_members.rs`

**Interfaces:**
- Produces: `pub(crate) async fn approve_join_core(tx: &mut PgConnection, project_id: Uuid, req_id: Uuid, decided_by: Uuid, data_role_override: Option<String>) -> Result<ApproveResponse, ApiError>`; `pub(crate) async fn reject_join_core(tx, project_id, req_id, decided_by) -> Result<(), ApiError>`; `pub(crate) async fn remove_member_core(tx, project_id, target: Uuid) -> Result<(), ApiError>` (keeps the "cannot remove the coordinator — hand over first" 409 guard); operator routes `POST /api/v1/operator/projects/{id}/join-requests/{rid}/decide` body `{action: "approve"|"reject", dataRole?: string}` and `DELETE /api/v1/operator/projects/{id}/members/{account_id}`.

- [ ] **Step 1: failing tests** `tests/operator_members.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn operator_decides_join_and_removes_member(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (coord, _) = register_device(&app, &mailer, "coord@x.io", 1, "pc").await;
    let (joiner, _) = register_device(&app, &mailer, "j@x.io", 2, "pc2").await;
    let project = create_project_via(&app, &coord, "M31 Mosaic").await;
    // joiner files a request (mirror join_and_approve's request half; see common 355–386)
    let (st, body) = send(&app, post(&format!("/api/v1/projects/{project}/join-requests"),
        r#"{"displayName":"J","desiredRole":"send"}"#, &joiner)).await;
    assert_eq!(st, StatusCode::OK);
    let rid = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"].as_str().unwrap().to_string();

    let (st, _) = post_cookie(&app,
        &format!("/api/v1/operator/projects/{project}/join-requests/{rid}/decide"),
        r#"{"action":"approve"}"#, &op).await;
    assert_eq!(st, StatusCode::OK);
    let member_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM project_members WHERE project_id=$1::uuid").bind(&project)
        .fetch_one(&pool).await.unwrap();
    assert_eq!(member_count, 2);

    // removal: member ok, coordinator refused
    let joiner_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='j@x.io'")
        .fetch_one(&pool).await.unwrap();
    let coord_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='coord@x.io'")
        .fetch_one(&pool).await.unwrap();
    let (st, _) = delete_cookie(&app,
        &format!("/api/v1/operator/projects/{project}/members/{coord_id}"), &op).await;
    assert_eq!(st, StatusCode::CONFLICT, "coordinator removal must be refused");
    let (st, _) = delete_cookie(&app,
        &format!("/api/v1/operator/projects/{project}/members/{joiner_id}"), &op).await;
    assert_eq!(st, StatusCode::OK);
}
```

(Add `delete_cookie` to `tests/common/mod.rs` mirroring `post_cookie` 332–341 with method DELETE, no body.)

- [ ] **Step 2: run** → FAIL. **Step 3: implement.** In `join_requests.rs`, move the tx body of `approve_join_request` (218–260: atomic consume UPDATE → member INSERT with 409-on-pkey → `bump_membership_version_tx` → `record_event_tx("member_joined", …)`) into `approve_join_core` with `decided_by` parameterized; the existing handler becomes `require_coordinator` + tx + core + commit. Same split for reject (270–297) into `reject_join_core`. In `members.rs`, move the tx body of `remove_member` (152–181: `FOR UPDATE` re-read refusing coordinator target → DELETE → bump → event) into `remove_member_core(tx, project_id, target)`; existing handler keeps its `require_coordinator` + self-removal guard + `assert_still_coordinator` and calls the core. Operator handlers in `operator.rs`:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinDecision { pub action: String, pub data_role: Option<String> }

pub async fn decide_join(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path((id, rid)): axum::extract::Path<(Uuid, Uuid)>,
    Json(body): Json<JoinDecision>,
) -> Result<axum::http::StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;
    match body.action.as_str() {
        "approve" => { crate::routes::join_requests::approve_join_core(&mut tx, id, rid, op.account_id, body.data_role).await?; }
        "reject"  => { crate::routes::join_requests::reject_join_core(&mut tx, id, rid, op.account_id).await?; }
        _ => return Err(ApiError::bad_request("action must be approve or reject")),
    }
    audit_tx(&mut tx, op.account_id, "join_request.decide", "join_request", &rid.to_string(),
        serde_json::json!({"projectId": id, "action": body.action})).await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::OK)
}

pub async fn remove_member(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path((id, account_id)): axum::extract::Path<(Uuid, Uuid)>,
) -> Result<axum::http::StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;
    crate::routes::members::remove_member_core(&mut tx, id, account_id).await?;
    audit_tx(&mut tx, op.account_id, "member.remove", "member",
        &format!("{id}/{account_id}"), serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::OK)
}
```

Register both routes (`post(...)` and `delete(...)`).

- [ ] **Step 4: run** → PASS; full suite green (existing coordinator-path tests pin the refactor).
- [ ] **Step 5: commit** `feat(hub): operator join decisions + member removal via factored coordinator cores`

---

### Task 10: operator project create + delete

**Files:**
- Modify: `src/routes/projects.rs` (factor creation core from `create_project` 234–384)
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_create_delete.rs`

**Interfaces:**
- Produces: `pub(crate) async fn create_project_core(tx: &mut PgConnection, coordinator: Uuid, coordinator_display_name: &str, body: &CreateProject) -> Result<ProjectView, ApiError>` (project INSERT + coordinator membership INSERT from 348–357, slug derivation as-is; rate-limit stays OUTSIDE the core in the user handler); operator routes `POST /api/v1/operator/projects` body = `CreateProject` fields + `coordinatorEmail: string`, `DELETE /api/v1/operator/projects/{id}`.

- [ ] **Step 1: failing tests**:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn operator_creates_and_deletes_project(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (_c, _) = register_device(&app, &mailer, "astro@x.io", 1, "pc").await;

    // mirror the CreateProject JSON that create_project_via (common 273–303) sends, plus coordinatorEmail
    let (st, body) = post_cookie(&app, "/api/v1/operator/projects",
        r#"{"title":"Official M31","targetName":"M31","coordinatorEmail":"astro@x.io","coordinatorDataRole":"send_receive"}"#,
        &op).await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let project = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"].as_str().unwrap().to_string();
    let coord_email: String = sqlx::query_scalar(
        "SELECT a.email FROM project_members m JOIN accounts a ON a.id=m.account_id
         WHERE m.project_id=$1::uuid AND m.is_coordinator").bind(&project)
        .fetch_one(&pool).await.unwrap();
    assert_eq!(coord_email, "astro@x.io");

    let (st, _) = post_cookie(&app, "/api/v1/operator/projects",
        r#"{"title":"X","targetName":"X","coordinatorEmail":"ghost@x.io"}"#, &op).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _) = delete_cookie(&app, &format!("/api/v1/operator/projects/{project}"), &op).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, get_public(&format!("/api/v1/projects/{project}"))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, body) = get_cookie(&app, "/api/v1/operator/audit", &op).await;
    let raw = String::from_utf8(body).unwrap();
    assert!(raw.contains("project.delete") && raw.contains("Official M31"),
        "delete audit must snapshot the title: {raw}");
}
```

(Adjust the create JSON to the REAL required `CreateProject` fields — read `projects.rs:141-157` and `create_project_via` in common; include every NOT NULL field it sends.)

- [ ] **Step 2: run** → FAIL. **Step 3: implement.** Factor `create_project_core` out of `create_project` (the INSERT block + membership INSERT 348–357, parameterizing `auth.account_id` → `coordinator` and the display name); user handler = rate limit (276–286) + core with `auth.account_id`. Operator handlers:

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
    let view = crate::routes::projects::create_project_core(&mut tx, coordinator, &email, &body.base).await?;
    audit_tx(&mut tx, op.account_id, "project.create", "project", &view.id.to_string(),
        serde_json::json!({"coordinatorEmail": email})).await?;
    tx.commit().await?;
    Ok(Json(view))
}

pub async fn delete_project(
    State(state): State<AppState>,
    Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
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
    Ok(axum::http::StatusCode::OK)
}
```

(`CreateProject` and `ProjectView` need `pub` visibility from `projects.rs` — make them `pub` if `pub(crate)`; `#[serde(flatten)]` requires `CreateProject: Deserialize` — it already is.) Register routes.

- [ ] **Step 4: run** → PASS; full suite green (user create_project path re-pinned by existing tests).
- [ ] **Step 5: commit** `feat(hub): operator project create (assigned coordinator) + hard delete with audit snapshot`

---

### Task 11: accounts & devices endpoints (+ S1 test)

**Files:**
- Modify: `src/routes/operator.rs`, `src/routes/mod.rs`
- Create: `tests/operator_accounts.rs`

**Interfaces:**
- Produces: `GET /api/v1/operator/accounts?search=` → `Vec<OperatorAccountRow{id, email, createdAt, lastSeenAt, blockedAt, deviceCount, projectCount}>`; `GET /api/v1/operator/accounts/{id}/devices` → `Vec<OperatorDeviceRow{id, name, capability, createdAt, lastSeenAt, revokedAt}>` (**NO endpoint_addr**); `POST /api/v1/operator/accounts/{id}/block` body `{note?: string}` / `POST .../unblock`; `POST /api/v1/operator/devices/{id}/revoke`.

- [ ] **Step 1: failing tests** `tests/operator_accounts.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn accounts_block_guards_and_s1(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@x.io", "op2@x.io"]);
    let op = portal_sign_in(&app, &mailer, "op@x.io").await;
    let (user_token, _) = register_device(&app, &mailer, "user@x.io", 1, "pc").await;
    let _ = portal_sign_in(&app, &mailer, "op2@x.io").await; // materialize fellow-operator account

    // search
    let (st, body) = get_cookie(&app, "/api/v1/operator/accounts?search=user", &op).await;
    assert_eq!(st, StatusCode::OK);
    let raw = String::from_utf8(body).unwrap();
    assert!(raw.contains("user@x.io") && raw.contains("\"deviceCount\":1"));
    // S1: no endpoint address material in operator responses
    assert!(!raw.contains("endpointAddr") && !raw.contains("directAddrs"), "S1: {raw}");

    let user_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='user@x.io'")
        .fetch_one(&pool).await.unwrap();
    let op_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='op@x.io'")
        .fetch_one(&pool).await.unwrap();
    let op2_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email='op2@x.io'")
        .fetch_one(&pool).await.unwrap();

    // guards: self and fellow operator
    let (st, _) = post_cookie(&app, &format!("/api/v1/operator/accounts/{op_id}/block"), "{}", &op).await;
    assert_eq!(st, StatusCode::CONFLICT, "cannot block self");
    let (st, _) = post_cookie(&app, &format!("/api/v1/operator/accounts/{op2_id}/block"), "{}", &op).await;
    assert_eq!(st, StatusCode::CONFLICT, "cannot block a fellow operator");

    // block user → their bearer dies; devices listing shows them; S1 holds there too
    let (st, _) = post_cookie(&app, &format!("/api/v1/operator/accounts/{user_id}/block"),
        r#"{"note":"abuse"}"#, &op).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, get("/api/v1/devices", &user_token)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, body) = get_cookie(&app, &format!("/api/v1/operator/accounts/{user_id}/devices"), &op).await;
    assert_eq!(st, StatusCode::OK);
    let raw = String::from_utf8(body).unwrap();
    assert!(!raw.contains("endpointAddr"), "S1 devices: {raw}");
    // revoke the device via operator route
    let dev_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM devices WHERE account_id=$1")
        .bind(user_id).fetch_one(&pool).await.unwrap();
    let (st, _) = post_cookie(&app, &format!("/api/v1/operator/devices/{dev_id}/revoke"), "{}", &op).await;
    assert_eq!(st, StatusCode::OK);
    // unblock restores sign-in ability
    let (st, _) = post_cookie(&app, &format!("/api/v1/operator/accounts/{user_id}/unblock"), "{}", &op).await;
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
        "SELECT a.id, a.email, a.created_at, a.last_seen_at, a.blocked_at,
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
    pub name: String,
    pub capability: Option<String>,
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
) -> Result<axum::http::StatusCode, ApiError> {
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
    Ok(axum::http::StatusCode::OK)
}

pub async fn block_account(State(state): State<AppState>, Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>, Json(b): Json<BlockBody>)
    -> Result<axum::http::StatusCode, ApiError> { set_blocked(&state, &op, id, true, b.note).await }

pub async fn unblock_account(State(state): State<AppState>, Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>, Json(b): Json<BlockBody>)
    -> Result<axum::http::StatusCode, ApiError> { set_blocked(&state, &op, id, false, b.note).await }

pub async fn revoke_device(
    State(state): State<AppState>, Extension(op): Extension<OperatorAuth>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;
    let n = sqlx::query("UPDATE devices SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(id).execute(&mut *tx).await?.rows_affected();
    if n == 0 { return Err(ApiError::not_found("device not found or already revoked")); }
    audit_tx(&mut tx, op.account_id, "device.revoke", "device", &id.to_string(),
        serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::OK)
}
```

(Match `capability` column type against `DeviceRow` in `devices.rs:29-41` — if it's `String` not `Option<String>`, mirror it.) Register the five routes.

- [ ] **Step 4: run** → PASS; full suite green.
- [ ] **Step 5: commit** `feat(hub): operator accounts/devices — search, block guards, revoke, S1-clean views`

---

### Task 12: portal — `/me.operator`, nav, page scaffold + Dashboard + Audit tabs

**Files:**
- Modify: `portal/src/useMe.ts` (Me at 4–7), `portal/src/App.tsx` (routes 13–18), `portal/src/Layout.tsx` (nav), `portal/src/types.ts`
- Create: `portal/src/pages/Operator.tsx`

**Interfaces:**
- Consumes: Tasks 4–5 wire shapes.
- Produces: `Me.operator: boolean`; route `/operator`; `types.ts` additions `OperatorOverview`, `OperatorEvent`, `OperatorAuditRow` mirroring Tasks 4–5 camelCase JSON; `Operator.tsx` exports default page with tab state `'dashboard' | 'projects' | 'accounts' | 'audit'` (projects/accounts tabs render "coming in the next commit" placeholders REPLACED in Tasks 13–14 — acceptable intra-plan since both land in this plan).

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
import { useCallback, useEffect, useState } from 'react';
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

- [ ] **Step 3: routing + nav.** `App.tsx`: add `<Route path="/operator" element={<Operator />} />` inside the Layout route; import. `Layout.tsx`: in the nav, render an `Operator` link when `me?.operator` (Layout already consumes `useMe` — if not, mirror how it shows sign-in state).
- [ ] **Step 4: verify** `cd portal && npm ci && npm run build` → tsc + vite succeed.
- [ ] **Step 5: commit** `feat(portal): operator page scaffold — dashboard + audit tabs, nav gated by /me.operator`

---

### Task 13: portal — Projects tab (actions)

**Files:**
- Modify: `portal/src/pages/Operator.tsx` (replace `ProjectsTab`), `portal/src/types.ts`

**Interfaces:**
- Consumes: Tasks 6–10 wire shapes (`OperatorProjectRow`, PATCH body, coordinator `{email}`, decide `{action, dataRole?}`, DELETE routes).
- Produces: full Projects tab: table, feature/hide/status toggles, appoint-coordinator by email, expandable row with members (remove) + open join requests (approve/reject), delete with typed-slug confirm.

- [ ] **Step 1: types.** Append to `types.ts`:

```ts
export interface OperatorProjectRow {
  id: string; slug: string; title: string; status: string; featured: boolean;
  hiddenAt: string | null; createdAt: string; coordinatorEmail: string | null;
  memberCount: number; announcementCount: number; lastEventAt: string | null;
}
```

- [ ] **Step 2: implement `ProjectsTab`** (replace the placeholder; uses `apiGet/apiPatch/apiPost/apiDelete`; reuse `MemberPublicView`/`JoinRequestView` from types.ts for the expanded row — fetch via the existing `/api/v1/projects/{slug}` page + `/api/v1/projects/{id}/join-requests` coordinator listing IF that listing is operator-accessible; if it is coordinator-gated, list open join requests from `overview.events` is NOT acceptable — instead add nothing and rely on the decide-by-id flow ONLY when the hub exposes them. **Resolution locked here:** reuse `GET /api/v1/operator/projects` for rows, and for the expanded row call `apiGet<ProjectPage>('/api/v1/projects/' + slug)` (operator passes the hidden gate per Task 7) which contains `members`; open join requests come from a small addition — extend Task 6's `list_projects` response? NO — keep v1 simple: the expanded row shows members with remove buttons; join-request decisions happen via the project's own Admin page which the operator can open (link `/p/{slug}/admin` — the coordinator gate there refuses non-coordinators, so instead the decide action lives HERE with a request-id input? Unacceptable UX.) **Final resolution:** add `GET /api/v1/operator/projects/{id}/join-requests` to Task 9's backend (same `Vec<JoinRequestView>` shape the coordinator listing uses — factor its query) and consume it here.**

```tsx
function ProjectsTab() {
  const [rows, setRows] = useState<OperatorProjectRow[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [msg, setMsg] = useState('');
  const reload = useCallback(() => {
    apiGet<OperatorProjectRow[]>('/api/v1/operator/projects').then(setRows)
      .catch((e) => console.error('[operator] projects failed:', e));
  }, []);
  useEffect(reload, [reload]);
  const act = (fn: () => Promise<unknown>, ok: string) =>
    fn().then(() => { setMsg(ok); reload(); }).catch((e) => setMsg(String(e)));
  return (
    <section className="space-y-2">
      <h2 className="font-semibold">Projects</h2>
      {msg && <p className="text-sm text-neutral-500">{msg}</p>}
      <table className="w-full text-left text-sm">
        <thead><tr className="text-neutral-500">
          <th>Project</th><th>Coordinator</th><th>Members</th><th>Status</th><th>Actions</th></tr></thead>
        <tbody>{rows.map((p) => (
          <ProjectRow key={p.id} p={p} open={open === p.id}
            onToggle={() => setOpen(open === p.id ? null : p.id)} act={act} />
        ))}</tbody>
      </table>
    </section>
  );
}
```

`ProjectRow` renders: title+slug (+`featured`/`hidden` badges), coordinator email, member/announcement counts, status; action buttons — `Feature`/`Unfeature` → `apiPatch('/api/v1/operator/projects/'+p.id, {featured: !p.featured})`; `Hide`/`Unhide` → `{hidden: !p.hiddenAt}`; `Close`/`Reopen` → `{status: p.status === 'active' ? 'closed' : 'active'}`; `Coordinator…` → `window.prompt('New coordinator email')` → `apiPost(.../coordinator, {email})`; `Delete…` → `window.prompt('Type the slug to confirm')` must equal `p.slug` → `apiDelete('/api/v1/operator/projects/'+p.id)`. Expanded row: fetch `apiGet('/api/v1/projects/'+p.slug)` for members (each with `Remove` → `apiDelete('/api/v1/operator/projects/'+p.id+'/members/'+m.accountId)`) and `apiGet('/api/v1/operator/projects/'+p.id+'/join-requests')` for open requests (Approve/Reject → `apiPost(.../join-requests/{rid}/decide, {action})`). Write the full JSX inline following the Admin.tsx field/button class constants.

- [ ] **Step 3 (backend addendum from resolution above):** add to `src/routes/operator.rs` `GET /api/v1/operator/projects/{id}/join-requests` returning the same rows as the coordinator listing (find its handler/query in `join_requests.rs` — the open-requests SELECT — and factor or copy the query without `require_coordinator`), register the route, and extend `tests/operator_members.rs` with a listing assertion (operator sees the open request before deciding). Red → green.
- [ ] **Step 4: verify** `cd portal && npm run build` → clean; `cargo test` → green.
- [ ] **Step 5: commit** `feat(portal): operator projects tab — flags, coordinator, joins, member removal, typed-slug delete`

---

### Task 14: portal — Accounts tab

**Files:**
- Modify: `portal/src/pages/Operator.tsx` (replace `AccountsTab`), `portal/src/types.ts`

**Interfaces:**
- Consumes: Task 11 wire shapes.

- [ ] **Step 1: types**:

```ts
export interface OperatorAccountRow {
  id: string; email: string; createdAt: string; lastSeenAt: string | null;
  blockedAt: string | null; deviceCount: number; projectCount: number;
}
export interface OperatorDeviceRow {
  id: string; name: string; capability: string | null; createdAt: string;
  lastSeenAt: string | null; revokedAt: string | null;
}
```

- [ ] **Step 2: implement `AccountsTab`**: search input (`field` class constant) driving `apiGet('/api/v1/operator/accounts?search='+encodeURIComponent(q))`; account rows: email, created/last-seen, device/project counts, `Blocked` badge when `blockedAt`; actions: `Block…` (prompt for note → `apiPost(.../block, {note})`) / `Unblock` → `apiPost(.../unblock, {})`; expandable device list via `apiGet(.../accounts/{id}/devices)` with `Revoke` buttons (`window.confirm`) → `apiPost('/api/v1/operator/devices/'+d.id+'/revoke', {})`. Full JSX in the Admin.tsx style, reload after each action, error → status line.
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
- [ ] **Step 3: final gates.** `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test` (full, expect all green incl. 12 new test files), `cd portal && npm run build`, then push branch: `git push -u origin operator-console` → hub CI green (test + build jobs).
- [ ] **Step 4: test-hub deploy (owner-confirmed 1Password prompts):**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/astronet
ansible-playbook deploy_athenaeum_hub.yml -e hub_target=athenaeum_hub_test -e hub_artifact_ref=operator-console
```

Verify: `curl https://test-hub.artfrom.space/api/v1/health`; portal sign-in as the owner → `/operator` renders; a non-operator account gets no nav link and 403 on the API. Commit astronet changes: `git commit -am "hub: HUB_OPERATOR_EMAILS for operator console"`.
- [ ] **Step 5: commit + wrap.** Final hub commit if README pending; update the athenaeum-repo memory ledger note (operator console CODE COMPLETE on `operator-console`, test-hub soaked, prod deploy = owner procedure per hub-deploy discipline).

---

## Self-review notes (done at authoring)

- **Spec coverage**: identity/choke point (T4), migration (T2), blocked enforcement incl. OTP (T3), overview (T5), listing (T6), patch+hidden/featured/closed semantics (T7), coordinator any-account (T8), joins+member removal on behalf (T9, + operator join-request listing added in T13 step 3), create/delete (T10), accounts/devices/S1 (T11), portal (T12–14), env/deploy/test-hub-first (T15). Evolution section requires no code. Non-goals honored (no frame moderation, no roles UI, no account deletion).
- **Line numbers** cited from the 2026-07-18 code brief — treat as anchors, re-locate by identifier if drifted.
- **Known judgment calls locked in**: operator join-request listing endpoint (T13 step 3) added rather than reusing the coordinator-gated listing; `optional_account` skips CSRF (GET-only use); block guard covers self + allowlist.
