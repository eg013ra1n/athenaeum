# Collab Slice 1 — Hub Project Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The hub side of Stage II collaboration — project/membership/threshold/announcement tables, their HTTP API, hub-signed membership snapshots, and the announcement approval state machine. No UI (portal is slice 2), no app client (slice 3), no exchange (slice 4).

**Architecture:** Extends the existing `athenaeum-hub` Axum + Postgres service (repo `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`). One additive migration; five new route modules following the existing `routes/devices.rs` pattern (device bearer auth via `auth_mw::require_auth` → `Extension<AuthDevice>`, `ApiError` for every failure, camelCase wire); one signing helper module (ed25519, key stored in DB). Spec: athenaeum repo `docs/superpowers/specs/2026-07-06-collaboration-projects-design.md` §2 (approval bullet), §5, §5a, §8, §11, §13-slice-1.

**Tech Stack:** Rust 1.96.1 (rust-toolchain.toml), axum 0.8, sqlx 0.8 (postgres, migrate), ed25519-dalek 2 (new dep), `#[sqlx::test]` integration tests.

## Global Constraints

- **Repo:** all code changes in `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`. Work on a new branch `collab-project-model` cut from `main`; merge to `main` only when the whole slice is green and reviewed.
- **Commit identity:** `eg013ra1n <vilen.sharifov@gmail.com>` — never a Claude author/co-author line.
- **Wire casing:** every request/response body is camelCase (`#[serde(rename_all = "camelCase")]`).
- **Errors:** handlers return `Result<_, ApiError>` (`src/error.rs`). Never swallow: `ApiError` logs on conversion; any extra context via `tracing`. Auth failures are body-less `ApiError::Status(StatusCode::UNAUTHORIZED/FORBIDDEN)`.
- **Tests:** `#[sqlx::test]` (auto-applies `./migrations`, fresh DB per test). Local run: `docker compose up -d postgres` once, then `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked <name>`. CI runs the same against a disposable postgres:16.
- **DB values:** `data_role ∈ {'send','send_receive'}`, announcement `state ∈ {'pending','published','rejected'}`, project `status ∈ {'active','closed'}` — CHECK-constrained, exactly these spellings on the wire too.
- **Membership is account-level; snapshots expand to devices at build time.** `project_members` stores accounts. The signed snapshot expands each member account to its **active, non-revoked, `capability='athenaeum'`** device pubkeys at query time (perseus capture nodes never participate in projects; device adds/revokes flow into the next snapshot automatically with no stored node lists to go stale).
- **Member display names, never emails, cross account boundaries.** `display_name` is captured at create/join time; emails never appear in snapshots, directory, project pages, or member lists.
- **Public (no token) endpoints:** directory, project page, signing pubkey. Everything else requires a device bearer token; project-scoped access is enforced per-handler via `require_member` / `require_coordinator` (403 on failure).

---

### Task 1: Migration 0009 — collab tables + ed25519 dependency

**Files:**
- Create: `migrations/0009_collab_projects.sql`
- Modify: `Cargo.toml` (add `ed25519-dalek`)
- Test: `tests/collab_schema.rs`

**Interfaces:**
- Consumes: existing `accounts`, `devices` tables (0002/0007).
- Produces: tables `projects`, `project_members`, `join_requests`, `project_thresholds`, `package_announcements`, `have_reports`, `project_events`, `hub_keys` — exactly the columns below; every later task binds them verbatim.

- [ ] **Step 1: Write the failing test**

```rust
// tests/collab_schema.rs
//! Migration 0009 shape: the collab tables exist, and the load-bearing
//! constraints (announcement-state CHECK, one-coordinator partial unique)
//! actually reject bad rows.

mod common;

use sqlx::PgPool;
use uuid::Uuid;

async fn seed_account(pool: &PgPool, email: &str) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as("INSERT INTO accounts (email) VALUES ($1) RETURNING id")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("insert account");
    id
}

#[sqlx::test]
async fn project_row_round_trips(pool: PgPool) {
    let account = seed_account(&pool, "a@example.com").await;
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO projects (slug, title, target_name, target_ra_deg, target_dec_deg, \
         target_radius_deg, created_by) VALUES ('m101', 'M 101', 'M101', 210.8, 54.35, 1.5, $1) \
         RETURNING id",
    )
    .bind(account)
    .fetch_one(&pool)
    .await
    .expect("insert project");

    let (version, require_approval, status): (i64, bool, String) = sqlx::query_as(
        "SELECT membership_version, require_approval, status FROM projects WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("read defaults");
    assert_eq!(version, 1);
    assert!(!require_approval);
    assert_eq!(status, "active");
}

#[sqlx::test]
async fn announcement_state_check_rejects_bogus(pool: PgPool) {
    let account = seed_account(&pool, "a@example.com").await;
    let (project,): (Uuid,) = sqlx::query_as(
        "INSERT INTO projects (slug, title, target_name, target_ra_deg, target_dec_deg, \
         target_radius_deg, created_by) VALUES ('p', 'P', 'P', 0, 0, 1, $1) RETURNING id",
    )
    .bind(account)
    .fetch_one(&pool)
    .await
    .unwrap();

    let err = sqlx::query(
        "INSERT INTO package_announcements (project_id, package_id, publisher, root_hash, \
         byte_size, frame_count, state) VALUES ($1, gen_random_uuid(), $2, 'ab', 1, 1, 'bogus')",
    )
    .bind(project)
    .bind(account)
    .execute(&pool)
    .await
    .expect_err("CHECK must reject state='bogus'");
    assert!(err.to_string().contains("check"), "unexpected error: {err}");
}

#[sqlx::test]
async fn one_coordinator_per_project(pool: PgPool) {
    let a = seed_account(&pool, "a@example.com").await;
    let b = seed_account(&pool, "b@example.com").await;
    let (project,): (Uuid,) = sqlx::query_as(
        "INSERT INTO projects (slug, title, target_name, target_ra_deg, target_dec_deg, \
         target_radius_deg, created_by) VALUES ('p', 'P', 'P', 0, 0, 1, $1) RETURNING id",
    )
    .bind(a)
    .fetch_one(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO project_members (project_id, account_id, display_name, data_role, is_coordinator) \
         VALUES ($1, $2, 'A', 'send_receive', true)",
    )
    .bind(project)
    .bind(a)
    .execute(&pool)
    .await
    .expect("first coordinator");

    let err = sqlx::query(
        "INSERT INTO project_members (project_id, account_id, display_name, data_role, is_coordinator) \
         VALUES ($1, $2, 'B', 'send_receive', true)",
    )
    .bind(project)
    .bind(b)
    .execute(&pool)
    .await
    .expect_err("second coordinator must violate one_coordinator_per_project");
    assert!(
        athenaeum_hub::routes::is_unique_violation(&err, "one_coordinator_per_project"),
        "unexpected error: {err}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub && DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test collab_schema`
Expected: FAIL — `relation "projects" does not exist` (migration applies but table missing until Step 3).

- [ ] **Step 3: Write the migration + add the dependency**

```sql
-- migrations/0009_collab_projects.sql
-- Stage II collaboration: projects, membership, join requests, versioned
-- thresholds, package announcements (approval state machine), have-reports,
-- audit events, and the hub signing key for membership snapshots.
-- Spec: athenaeum docs/superpowers/specs/2026-07-06-collaboration-projects-design.md §5/§5a.

CREATE TABLE IF NOT EXISTS projects (
    id                 uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    slug               text        UNIQUE NOT NULL,
    title              text        NOT NULL,
    description        text        NOT NULL DEFAULT '',
    target_name        text        NOT NULL,
    target_ra_deg      double precision NOT NULL,
    target_dec_deg     double precision NOT NULL,
    target_radius_deg  double precision NOT NULL,
    goals              jsonb,
    chat_link          text,
    data_policy_text   text        NOT NULL DEFAULT '',
    require_approval   boolean     NOT NULL DEFAULT false,
    status             text        NOT NULL DEFAULT 'active' CHECK (status IN ('active','closed')),
    -- Bumped on every change that alters the signed membership snapshot
    -- (member add/remove/leave, role change, handover, require_approval flip).
    membership_version bigint      NOT NULL DEFAULT 1,
    created_by         uuid        NOT NULL REFERENCES accounts (id),
    created_at         timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS project_members (
    project_id     uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    account_id     uuid        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- Cross-account display identity; emails never leave the account boundary.
    display_name   text        NOT NULL,
    data_role      text        NOT NULL CHECK (data_role IN ('send','send_receive')),
    is_coordinator boolean     NOT NULL DEFAULT false,
    joined_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, account_id)
);

-- Exactly one coordinator per project, DB-enforced (handover swaps inside a tx).
CREATE UNIQUE INDEX IF NOT EXISTS one_coordinator_per_project
    ON project_members (project_id)
    WHERE is_coordinator;

CREATE TABLE IF NOT EXISTS join_requests (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id   uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    account_id   uuid        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    display_name text        NOT NULL,
    desired_role text        NOT NULL CHECK (desired_role IN ('send','send_receive')),
    message      text        NOT NULL DEFAULT '',
    status       text        NOT NULL DEFAULT 'open' CHECK (status IN ('open','approved','rejected')),
    decided_by   uuid        REFERENCES accounts (id),
    decided_at   timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- At most one open request per account per project.
CREATE UNIQUE INDEX IF NOT EXISTS one_open_join_request
    ON join_requests (project_id, account_id)
    WHERE status = 'open';

CREATE TABLE IF NOT EXISTS project_thresholds (
    id         bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    version    integer     NOT NULL,
    rules      jsonb       NOT NULL,
    created_by uuid        NOT NULL REFERENCES accounts (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, version)
);

CREATE TABLE IF NOT EXISTS package_announcements (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    package_id      uuid        UNIQUE NOT NULL,
    publisher       uuid        NOT NULL REFERENCES accounts (id),
    root_hash       text        NOT NULL,
    byte_size       bigint      NOT NULL,
    frame_count     integer     NOT NULL,
    aggregate_stats jsonb       NOT NULL DEFAULT '{}'::jsonb,
    supersedes      uuid[]      NOT NULL DEFAULT '{}',
    -- State machine: pending → published | rejected (born published when the
    -- project's require_approval is off, or when the coordinator publishes).
    state           text        NOT NULL CHECK (state IN ('pending','published','rejected')),
    reject_reason   text,
    decided_by      uuid        REFERENCES accounts (id),
    decided_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS package_announcements_project
    ON package_announcements (project_id, state);

-- "I hold package X" — device-granular so online-holder counts derive from
-- devices.last_seen_at. Idempotent inserts (PK).
CREATE TABLE IF NOT EXISTS have_reports (
    announcement_id uuid        NOT NULL REFERENCES package_announcements (id) ON DELETE CASCADE,
    device_id       uuid        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    reported_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (announcement_id, device_id)
);

-- Append-only audit trail (handover history, membership changes, decisions).
CREATE TABLE IF NOT EXISTS project_events (
    id         bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    kind       text        NOT NULL,
    actor      uuid        REFERENCES accounts (id),
    subject    uuid        REFERENCES accounts (id),
    detail     jsonb       NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS project_events_project ON project_events (project_id, id);

-- Hub-held signing keys (ed25519 seeds). One row per purpose; 'snapshot' signs
-- membership snapshots. Generated lazily on first use (race-safe upsert).
CREATE TABLE IF NOT EXISTS hub_keys (
    key_id     text        PRIMARY KEY,
    seed       bytea       NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
```

In `Cargo.toml`, after the `base64 = "0.22"` line, add:

```toml
# ed25519 signing for membership snapshots (seed kept in the hub_keys table;
# clients verify with the pubkey from GET /api/v1/collab/pubkey).
ed25519-dalek = "2"
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test collab_schema`
Expected: PASS (3 tests). Note: drop `--locked` on this run — the new dependency must update `Cargo.lock`; use `--locked` again afterwards.

- [ ] **Step 5: Commit**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub
git checkout -b collab-project-model
git add migrations/0009_collab_projects.sql Cargo.toml Cargo.lock tests/collab_schema.rs
git commit -m "feat(collab): migration 0009 — project/membership/threshold/announcement tables + hub_keys"
```

---

### Task 2: `collab_auth` — membership guards, events, version bumps

**Files:**
- Create: `src/collab_auth.rs`
- Modify: `src/lib.rs` (add `pub mod collab_auth;` after `pub mod auth_mw;`)
- Test: `tests/collab_auth.rs`

**Interfaces:**
- Consumes: tables from Task 1; `ApiError` (`src/error.rs`).
- Produces (every later task calls these exact signatures):
  - `pub struct Member { pub account_id: Uuid, pub display_name: String, pub data_role: String, pub is_coordinator: bool }`
  - `pub async fn require_member(db: &PgPool, project_id: Uuid, account_id: Uuid) -> Result<Member, ApiError>` — 403 body-less when not a member.
  - `pub async fn require_coordinator(db: &PgPool, project_id: Uuid, account_id: Uuid) -> Result<Member, ApiError>` — 403 when not the coordinator.
  - `pub async fn record_event(db: &PgPool, project_id: Uuid, kind: &str, actor: Option<Uuid>, subject: Option<Uuid>, detail: serde_json::Value) -> Result<(), sqlx::Error>`
  - `pub async fn bump_membership_version(db: &PgPool, project_id: Uuid) -> Result<(), sqlx::Error>`
  - All four also have `*_tx` siblings taking `&mut sqlx::PgConnection` (used inside transactions): `require_member_tx`, `record_event_tx`, `bump_membership_version_tx`.

- [ ] **Step 1: Write the failing test**

```rust
// tests/collab_auth.rs
//! Membership guards: member vs non-member vs coordinator, event rows,
//! version bumps.

mod common;

use athenaeum_hub::collab_auth::{
    bump_membership_version, record_event, require_coordinator, require_member,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn seed(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let (a,): (Uuid,) =
        sqlx::query_as("INSERT INTO accounts (email) VALUES ('a@example.com') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let (b,): (Uuid,) =
        sqlx::query_as("INSERT INTO accounts (email) VALUES ('b@example.com') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let (p,): (Uuid,) = sqlx::query_as(
        "INSERT INTO projects (slug, title, target_name, target_ra_deg, target_dec_deg, \
         target_radius_deg, created_by) VALUES ('p', 'P', 'P', 0, 0, 1, $1) RETURNING id",
    )
    .bind(a)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project_members (project_id, account_id, display_name, data_role, is_coordinator) \
         VALUES ($1, $2, 'A', 'send_receive', true)",
    )
    .bind(p)
    .bind(a)
    .execute(pool)
    .await
    .unwrap();
    (p, a, b)
}

#[sqlx::test]
async fn member_and_coordinator_guards(pool: PgPool) {
    let (p, coordinator, outsider) = seed(&pool).await;

    let m = require_member(&pool, p, coordinator).await.expect("coordinator is a member");
    assert_eq!(m.display_name, "A");
    assert_eq!(m.data_role, "send_receive");
    assert!(m.is_coordinator);

    assert!(require_member(&pool, p, outsider).await.is_err(), "outsider is 403");
    assert!(require_coordinator(&pool, p, coordinator).await.is_ok());

    // A plain member is not a coordinator.
    sqlx::query(
        "INSERT INTO project_members (project_id, account_id, display_name, data_role) \
         VALUES ($1, $2, 'B', 'send')",
    )
    .bind(p)
    .bind(outsider)
    .execute(&pool)
    .await
    .unwrap();
    assert!(require_coordinator(&pool, p, outsider).await.is_err(), "plain member is 403");
}

#[sqlx::test]
async fn events_and_version_bump(pool: PgPool) {
    let (p, a, b) = seed(&pool).await;

    record_event(&pool, p, "member_joined", Some(a), Some(b), json!({"dataRole": "send"}))
        .await
        .expect("event recorded");
    let (kind, detail): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, detail FROM project_events WHERE project_id = $1")
            .bind(p)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kind, "member_joined");
    assert_eq!(detail["dataRole"], "send");

    bump_membership_version(&pool, p).await.expect("bumped");
    let (v,): (i64,) = sqlx::query_as("SELECT membership_version FROM projects WHERE id = $1")
        .bind(p)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(v, 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test collab_auth`
Expected: FAIL — `could not find collab_auth in athenaeum_hub` (compile error).

- [ ] **Step 3: Write the implementation**

```rust
// src/collab_auth.rs
//! Project-scoped access guards + audit/versioning helpers shared by every
//! collaboration route module.
//!
//! `require_member` / `require_coordinator` are the sole authorization
//! chokepoints for project-scoped endpoints: both fail with a body-less 403
//! (membership is not enumerable through error shapes). `record_event`
//! appends to the `project_events` audit trail; `bump_membership_version`
//! invalidates cached membership snapshots (the snapshot endpoint reads the
//! version it stamps and signs).

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::ApiError;

/// A project membership row, as authorization context for a handler.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Member {
    pub account_id: Uuid,
    pub display_name: String,
    pub data_role: String,
    pub is_coordinator: bool,
}

const MEMBER_QUERY: &str = "SELECT account_id, display_name, data_role, is_coordinator \
     FROM project_members WHERE project_id = $1 AND account_id = $2";

fn forbidden() -> ApiError {
    ApiError::Status(axum::http::StatusCode::FORBIDDEN)
}

/// The caller's membership row, or a body-less 403.
pub async fn require_member(
    db: &PgPool,
    project_id: Uuid,
    account_id: Uuid,
) -> Result<Member, ApiError> {
    sqlx::query_as::<_, Member>(MEMBER_QUERY)
        .bind(project_id)
        .bind(account_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(forbidden)
}

/// Transaction-scoped sibling of [`require_member`].
pub async fn require_member_tx(
    conn: &mut PgConnection,
    project_id: Uuid,
    account_id: Uuid,
) -> Result<Member, ApiError> {
    sqlx::query_as::<_, Member>(MEMBER_QUERY)
        .bind(project_id)
        .bind(account_id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(forbidden)
}

/// The caller's membership row if they are the coordinator, else 403.
pub async fn require_coordinator(
    db: &PgPool,
    project_id: Uuid,
    account_id: Uuid,
) -> Result<Member, ApiError> {
    let member = require_member(db, project_id, account_id).await?;
    if !member.is_coordinator {
        return Err(forbidden());
    }
    Ok(member)
}

const EVENT_INSERT: &str = "INSERT INTO project_events (project_id, kind, actor, subject, detail) \
     VALUES ($1, $2, $3, $4, $5)";

/// Append an audit event. `detail` is a JSON object with camelCase keys.
pub async fn record_event(
    db: &PgPool,
    project_id: Uuid,
    kind: &str,
    actor: Option<Uuid>,
    subject: Option<Uuid>,
    detail: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(EVENT_INSERT)
        .bind(project_id)
        .bind(kind)
        .bind(actor)
        .bind(subject)
        .bind(detail)
        .execute(db)
        .await?;
    Ok(())
}

/// Transaction-scoped sibling of [`record_event`].
pub async fn record_event_tx(
    conn: &mut PgConnection,
    project_id: Uuid,
    kind: &str,
    actor: Option<Uuid>,
    subject: Option<Uuid>,
    detail: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(EVENT_INSERT)
        .bind(project_id)
        .bind(kind)
        .bind(actor)
        .bind(subject)
        .bind(detail)
        .execute(conn)
        .await?;
    Ok(())
}

const BUMP: &str = "UPDATE projects SET membership_version = membership_version + 1 WHERE id = $1";

/// Bump the snapshot version after any change the signed snapshot reflects.
pub async fn bump_membership_version(db: &PgPool, project_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(BUMP).bind(project_id).execute(db).await?;
    Ok(())
}

/// Transaction-scoped sibling of [`bump_membership_version`].
pub async fn bump_membership_version_tx(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(BUMP).bind(project_id).execute(conn).await?;
    Ok(())
}
```

In `src/lib.rs`, after `pub mod auth_mw;` add:

```rust
pub mod collab_auth;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test collab_auth`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/collab_auth.rs src/lib.rs tests/collab_auth.rs
git commit -m "feat(collab): membership guards + audit events + snapshot version bumps"
```

---

### Task 3: Snapshot signing key + `GET /api/v1/collab/pubkey`

**Files:**
- Create: `src/snapshot_sign.rs`
- Create: `src/routes/collab_keys.rs`
- Modify: `src/lib.rs` (add `pub mod snapshot_sign;`)
- Modify: `src/routes/mod.rs` (add `pub mod collab_keys;` + public route)
- Test: `tests/snapshot_sign.rs`

**Interfaces:**
- Consumes: `hub_keys` table (Task 1).
- Produces:
  - `pub async fn signing_key(db: &PgPool) -> anyhow::Result<ed25519_dalek::SigningKey>` — get-or-create the `'snapshot'` seed, race-safe.
  - `pub fn sign_b64(key: &ed25519_dalek::SigningKey, payload: &[u8]) -> String` — base64 (STANDARD) ed25519 signature.
  - `GET /api/v1/collab/pubkey` (public) → `{ "pubkey": "<base64 32-byte verifying key>" }` — stable across calls; Task 8's snapshot responses carry the same key.

- [ ] **Step 1: Write the failing test**

```rust
// tests/snapshot_sign.rs
//! The snapshot signing key is generated once, survives concurrent first use,
//! and the public endpoint exposes a stable verifying key that validates a
//! signature produced by `sign_b64`.

mod common;

use athenaeum_hub::snapshot_sign::{sign_b64, signing_key};
use axum::http::StatusCode;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use common::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sqlx::PgPool;

#[sqlx::test]
async fn key_is_stable_and_signatures_verify(pool: PgPool) {
    let k1 = signing_key(&pool).await.expect("first key");
    let k2 = signing_key(&pool).await.expect("second read");
    assert_eq!(
        k1.verifying_key().to_bytes(),
        k2.verifying_key().to_bytes(),
        "key must be created once and reused"
    );

    let payload = b"membership snapshot bytes";
    let sig_b64 = sign_b64(&k1, payload);
    let sig_bytes: [u8; 64] = BASE64
        .decode(&sig_b64)
        .expect("valid base64")
        .try_into()
        .expect("64-byte signature");
    k1.verifying_key()
        .verify(payload, &Signature::from_bytes(&sig_bytes))
        .expect("signature verifies");
}

#[sqlx::test]
async fn pubkey_endpoint_matches_signing_key(pool: PgPool) {
    let (app, _mailer) = app_with_capture(pool.clone());

    let (status, body) = send(&app, get("/api/v1/collab/pubkey", None)).await;
    assert_eq!(status, StatusCode::OK);
    let v = as_json(&body);
    let wire_key: [u8; 32] = BASE64
        .decode(v["pubkey"].as_str().expect("pubkey field"))
        .expect("valid base64")
        .try_into()
        .expect("32 bytes");

    let key = signing_key(&pool).await.unwrap();
    assert_eq!(wire_key, key.verifying_key().to_bytes());
    let _ = VerifyingKey::from_bytes(&wire_key).expect("valid ed25519 point");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test snapshot_sign`
Expected: FAIL — `could not find snapshot_sign in athenaeum_hub`.

- [ ] **Step 3: Write the implementation**

```rust
// src/snapshot_sign.rs
//! Hub signing key for membership snapshots.
//!
//! The ed25519 seed lives in the `hub_keys` table (key_id = 'snapshot'),
//! generated lazily on first use with a race-safe `INSERT … ON CONFLICT DO
//! NOTHING` + read-back: two concurrent first callers both end up with the
//! single row that won. Clients fetch the verifying key once from
//! `GET /api/v1/collab/pubkey` (TLS-delivered, then pinned) and verify every
//! snapshot offline.

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use sqlx::PgPool;

const KEY_ID: &str = "snapshot";

/// Load (or create on first use) the snapshot signing key.
pub async fn signing_key(db: &PgPool) -> anyhow::Result<SigningKey> {
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);

    // Race-safe get-or-create: the INSERT is a no-op when the row exists, and
    // the SELECT returns whichever seed actually won.
    sqlx::query("INSERT INTO hub_keys (key_id, seed) VALUES ($1, $2) ON CONFLICT (key_id) DO NOTHING")
        .bind(KEY_ID)
        .bind(seed.as_slice())
        .execute(db)
        .await
        .context("insert snapshot signing key")?;

    let (stored,): (Vec<u8>,) = sqlx::query_as("SELECT seed FROM hub_keys WHERE key_id = $1")
        .bind(KEY_ID)
        .fetch_one(db)
        .await
        .context("read snapshot signing key")?;

    let stored: [u8; 32] = stored
        .try_into()
        .map_err(|_| anyhow::anyhow!("hub_keys.seed for '{KEY_ID}' is not 32 bytes"))?;
    Ok(SigningKey::from_bytes(&stored))
}

/// Sign `payload` and return the base64 (STANDARD) encoding of the signature.
pub fn sign_b64(key: &SigningKey, payload: &[u8]) -> String {
    BASE64.encode(key.sign(payload).to_bytes())
}

/// Base64 (STANDARD) of the verifying key — the wire form used by
/// `GET /api/v1/collab/pubkey` and inside snapshot responses.
pub fn pubkey_b64(key: &SigningKey) -> String {
    BASE64.encode(key.verifying_key().to_bytes())
}
```

```rust
// src/routes/collab_keys.rs
//! `GET /api/v1/collab/pubkey` — the hub's snapshot-verifying key (public).

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::error::ApiError;
use crate::routes::AppState;
use crate::snapshot_sign;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PubkeyResponse {
    pubkey: String,
}

/// The ed25519 verifying key clients pin to validate membership snapshots.
#[tracing::instrument(skip_all)]
pub async fn signing_pubkey(State(state): State<AppState>) -> Result<Json<PubkeyResponse>, ApiError> {
    let key = snapshot_sign::signing_key(&state.db).await?;
    Ok(Json(PubkeyResponse {
        pubkey: snapshot_sign::pubkey_b64(&key),
    }))
}
```

In `src/lib.rs`, after `pub mod security;` add:

```rust
pub mod snapshot_sign;
```

In `src/routes/mod.rs`: add `pub mod collab_keys;` after `pub mod auth;`, and in `build_router`'s `public` router add (after the `/api/v1/health` line):

```rust
        .route("/api/v1/collab/pubkey", get(collab_keys::signing_pubkey))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test snapshot_sign`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/snapshot_sign.rs src/routes/collab_keys.rs src/lib.rs src/routes/mod.rs tests/snapshot_sign.rs
git commit -m "feat(collab): DB-held ed25519 snapshot signing key + public pubkey endpoint"
```

---

### Task 4: Project create + public directory + public project page

**Files:**
- Create: `src/routes/projects.rs`
- Modify: `src/routes/mod.rs` (add `pub mod projects;` + three routes)
- Modify: `tests/common/mod.rs` (add `create_project_via` helper)
- Test: `tests/projects_api.rs`

**Interfaces:**
- Consumes: Task 1 tables; Task 2 `record_event_tx`; `AuthDevice` (`account_id`).
- Produces:
  - `POST /api/v1/projects` (authed) — body `{title, description?, target: {name, raDeg, decDeg, radiusDeg}, goals?, chatLink?, dataPolicyText?, requireApproval?, coordinatorDisplayName, coordinatorDataRole, initialThresholds?}` → 200 `ProjectView` `{id, slug, title, description, target{…}, goals, chatLink, dataPolicyText, requireApproval, status, membershipVersion, createdAt}`.
  - `GET /api/v1/projects?q=` (public) — directory items `{id, slug, title, targetName, targetRaDeg, targetDecDeg, targetRadiusDeg, status, requireApproval, memberCount, createdAt}`.
  - `GET /api/v1/projects/{id}` (public; `{id}` is a uuid **or slug**) — `{project: ProjectView, members: [{displayName, dataRole, coordinator, joinedAt}], packages: [{id, packageId, frameCount, byteSize, createdAt, publisherDisplayName}]}` (published packages only; Task 11 enriches).
  - `pub(crate) fn slugify(title: &str) -> String`, `pub(crate) fn validate_rules(&Value) -> Result<(), ApiError>`, `pub(crate) fn valid_role(&str) -> bool`, `pub(crate) const ROLE_SEND / ROLE_SEND_RECEIVE` — later tasks import these from `crate::routes::projects`.
  - Test helper `create_project_via(app, token, title, require_approval) -> serde_json::Value` (the parsed `ProjectView`; coordinator role `send_receive`, one initial threshold rule).
- **Path-template rule:** every project route uses the literal segment name `{id}` (`/api/v1/projects/{id}`, `…/{id}/members/{account_id}`, …). axum/matchit panics on merge if the same position uses different param names — never introduce `{id_or_slug}`/`{project_id}` variants.

- [ ] **Step 1: Write the failing test**

```rust
// tests/projects_api.rs
//! Project creation, the public directory, and the public project page.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn create_then_directory_then_page(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;

    let project = create_project_via(&app, &token, "M 101 Deep Field", false).await;
    assert_eq!(project["slug"], "m-101-deep-field");
    assert_eq!(project["status"], "active");
    assert_eq!(project["membershipVersion"], 1);
    assert_eq!(project["target"]["raDeg"], 210.8);

    // Directory is public (no token) and carries the member count.
    let (status, body) = send(&app, get("/api/v1/projects", None)).await;
    assert_eq!(status, StatusCode::OK);
    let dir = as_json(&body);
    assert_eq!(dir.as_array().unwrap().len(), 1);
    assert_eq!(dir[0]["memberCount"], 1);
    assert_eq!(dir[0]["targetName"], "M101");

    // Page resolves by slug AND by id, publicly.
    for key in [
        project["slug"].as_str().unwrap().to_string(),
        project["id"].as_str().unwrap().to_string(),
    ] {
        let (status, body) = send(&app, get(&format!("/api/v1/projects/{key}"), None)).await;
        assert_eq!(status, StatusCode::OK, "page by {key}");
        let page = as_json(&body);
        assert_eq!(page["project"]["title"], "M 101 Deep Field");
        assert_eq!(page["members"][0]["displayName"], "Coord");
        assert_eq!(page["members"][0]["coordinator"], true);
        assert!(page["packages"].as_array().unwrap().is_empty());
    }

    let (status, _) = send(&app, get("/api/v1/projects/no-such-slug", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn create_requires_token_and_validates(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;

    // No token → 401 (middleware).
    let (status, _) = send(
        &app,
        post("/api/v1/projects", &json!({"title": "X"}), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Manual-approval project demands a send_receive coordinator (§2/§11).
    let (status, body) = send(
        &app,
        post(
            "/api/v1/projects",
            &json!({
                "title": "T",
                "target": {"name": "M101", "raDeg": 210.8, "decDeg": 54.35, "radiusDeg": 1.5},
                "requireApproval": true,
                "coordinatorDisplayName": "C",
                "coordinatorDataRole": "send",
            }),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(as_json(&body)["error"].as_str().unwrap().contains("send_receive"));

    // Bad declination.
    let (status, _) = send(
        &app,
        post(
            "/api/v1/projects",
            &json!({
                "title": "T",
                "target": {"name": "M101", "raDeg": 210.8, "decDeg": 954.0, "radiusDeg": 1.5},
                "coordinatorDisplayName": "C",
                "coordinatorDataRole": "send_receive",
            }),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn slug_collisions_get_numeric_suffixes(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;

    let first = create_project_via(&app, &token, "M 101", false).await;
    let second = create_project_via(&app, &token, "M 101", false).await;
    assert_eq!(first["slug"], "m-101");
    assert_eq!(second["slug"], "m-101-2");
}
```

Append to `tests/common/mod.rs`:

```rust
/// Create a project through the API; returns the parsed `ProjectView`.
/// Coordinator role is send_receive; one initial FWHM threshold rule.
pub async fn create_project_via(
    app: &axum::Router,
    token: &str,
    title: &str,
    require_approval: bool,
) -> Value {
    let (status, body) = send(
        app,
        post(
            "/api/v1/projects",
            &json!({
                "title": title,
                "description": "test project",
                "target": {"name": "M101", "raDeg": 210.8, "decDeg": 54.35, "radiusDeg": 1.5},
                "requireApproval": require_approval,
                "coordinatorDisplayName": "Coord",
                "coordinatorDataRole": "send_receive",
                "initialThresholds": [{"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.5}],
            }),
            Some(token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create project failed: {}",
        String::from_utf8_lossy(&body)
    );
    as_json(&body)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test projects_api`
Expected: FAIL — compile error, `projects` module missing.

- [ ] **Step 3: Write the implementation**

```rust
// src/routes/projects.rs
//! Projects: create (authed), public directory, public project page.
//! Member administration lives in `routes/members.rs`; PATCH lands in Task 5.
//!
//! POST /api/v1/projects        — caller becomes the coordinator.
//! GET  /api/v1/projects?q=     — public directory (title/target search).
//! GET  /api/v1/projects/{id}   — public page; `{id}` is a uuid or a slug.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth_mw::AuthDevice;
use crate::collab_auth::record_event_tx;
use crate::error::ApiError;
use crate::routes::{is_unique_violation, AppState};

pub(crate) const ROLE_SEND: &str = "send";
pub(crate) const ROLE_SEND_RECEIVE: &str = "send_receive";

pub(crate) fn valid_role(role: &str) -> bool {
    role == ROLE_SEND || role == ROLE_SEND_RECEIVE
}

/// Portal slug from a title: ascii-alnum runs joined by '-', lowercased,
/// capped at 60 chars; "project" when nothing survives.
pub(crate) fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c.to_ascii_lowercase());
            if slug.len() >= 60 {
                break;
            }
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
}

/// Threshold rules: a non-empty array (≤50) of `{metricKey, op, value}`
/// objects. Rule *semantics* (metric registry, ops) belong to the app/portal;
/// the hub pins only the shape so garbage can't be stored.
pub(crate) fn validate_rules(rules: &Value) -> Result<(), ApiError> {
    let arr = rules
        .as_array()
        .ok_or_else(|| ApiError::bad_request("rules must be an array"))?;
    if arr.is_empty() || arr.len() > 50 {
        return Err(ApiError::bad_request("rules must contain 1..=50 items"));
    }
    for rule in arr {
        let obj = rule
            .as_object()
            .ok_or_else(|| ApiError::bad_request("each rule must be an object"))?;
        let key_ok = obj
            .get("metricKey")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let op_ok = obj
            .get("op")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let value_ok = obj
            .get("value")
            .is_some_and(|v| v.is_number() || v.is_boolean());
        if !(key_ok && op_ok && value_ok) {
            return Err(ApiError::bad_request(
                "each rule needs metricKey (string), op (string), value (number|bool)",
            ));
        }
    }
    Ok(())
}

// ---- wire types --------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetSpec {
    pub name: String,
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProject {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub target: TargetSpec,
    pub goals: Option<Value>,
    pub chat_link: Option<String>,
    #[serde(default)]
    pub data_policy_text: String,
    #[serde(default)]
    pub require_approval: bool,
    pub coordinator_display_name: String,
    pub coordinator_data_role: String,
    pub initial_thresholds: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetView {
    pub name: String,
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

#[derive(sqlx::FromRow)]
pub(crate) struct ProjectRow {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub target_name: String,
    pub target_ra_deg: f64,
    pub target_dec_deg: f64,
    pub target_radius_deg: f64,
    pub goals: Option<Value>,
    pub chat_link: Option<String>,
    pub data_policy_text: String,
    pub require_approval: bool,
    pub status: String,
    pub membership_version: i64,
    pub created_at: DateTime<Utc>,
}

pub(crate) const PROJECT_COLUMNS: &str = "id, slug, title, description, target_name, target_ra_deg, \
     target_dec_deg, target_radius_deg, goals, chat_link, data_policy_text, require_approval, \
     status, membership_version, created_at";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub target: TargetView,
    pub goals: Option<Value>,
    pub chat_link: Option<String>,
    pub data_policy_text: String,
    pub require_approval: bool,
    pub status: String,
    pub membership_version: i64,
    pub created_at: DateTime<Utc>,
}

impl From<ProjectRow> for ProjectView {
    fn from(r: ProjectRow) -> Self {
        ProjectView {
            id: r.id,
            slug: r.slug,
            title: r.title,
            description: r.description,
            target: TargetView {
                name: r.target_name,
                ra_deg: r.target_ra_deg,
                dec_deg: r.target_dec_deg,
                radius_deg: r.target_radius_deg,
            },
            goals: r.goals,
            chat_link: r.chat_link,
            data_policy_text: r.data_policy_text,
            require_approval: r.require_approval,
            status: r.status,
            membership_version: r.membership_version,
            created_at: r.created_at,
        }
    }
}

// ---- POST /api/v1/projects ---------------------------------------------------

#[tracing::instrument(skip_all)]
pub async fn create_project(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Json(body): Json<CreateProject>,
) -> Result<Json<ProjectView>, ApiError> {
    let title = body.title.trim();
    if title.is_empty() || title.len() > 120 {
        return Err(ApiError::bad_request("title must be 1..=120 characters"));
    }
    let display_name = body.coordinator_display_name.trim();
    if display_name.is_empty() || display_name.len() > 60 {
        return Err(ApiError::bad_request("coordinatorDisplayName must be 1..=60 characters"));
    }
    if !valid_role(&body.coordinator_data_role) {
        return Err(ApiError::bad_request("coordinatorDataRole must be send or send_receive"));
    }
    if body.require_approval && body.coordinator_data_role != ROLE_SEND_RECEIVE {
        return Err(ApiError::bad_request(
            "a manual-approval project requires a send_receive coordinator (review needs the data)",
        ));
    }
    let t = &body.target;
    if t.name.trim().is_empty()
        || !(0.0..360.0).contains(&t.ra_deg)
        || !(-90.0..=90.0).contains(&t.dec_deg)
        || !(t.radius_deg > 0.0 && t.radius_deg <= 30.0)
    {
        return Err(ApiError::bad_request(
            "target needs name, raDeg in [0,360), decDeg in [-90,90], radiusDeg in (0,30]",
        ));
    }
    if let Some(rules) = &body.initial_thresholds {
        validate_rules(rules)?;
    }

    let mut tx = state.db.begin().await?;

    // Slug: title-derived, numeric suffix on collision (bounded retry).
    let base = slugify(title);
    let mut created: Option<ProjectRow> = None;
    for attempt in 0..10 {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{}", attempt + 1)
        };
        let res = sqlx::query_as::<_, ProjectRow>(&format!(
            "INSERT INTO projects (slug, title, description, target_name, target_ra_deg, \
             target_dec_deg, target_radius_deg, goals, chat_link, data_policy_text, \
             require_approval, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             RETURNING {PROJECT_COLUMNS}"
        ))
        .bind(&candidate)
        .bind(title)
        .bind(body.description.trim())
        .bind(t.name.trim())
        .bind(t.ra_deg)
        .bind(t.dec_deg)
        .bind(t.radius_deg)
        .bind(&body.goals)
        .bind(body.chat_link.as_deref().map(str::trim))
        .bind(body.data_policy_text.trim())
        .bind(body.require_approval)
        .bind(auth.account_id)
        .fetch_one(&mut *tx)
        .await;
        match res {
            Ok(row) => {
                created = Some(row);
                break;
            }
            Err(err) if is_unique_violation(&err, "projects_slug_key") => continue,
            Err(err) => return Err(err.into()),
        }
    }
    let Some(project) = created else {
        return Err(ApiError::conflict("could not allocate a unique project slug"));
    };

    sqlx::query(
        "INSERT INTO project_members (project_id, account_id, display_name, data_role, is_coordinator) \
         VALUES ($1, $2, $3, $4, true)",
    )
    .bind(project.id)
    .bind(auth.account_id)
    .bind(display_name)
    .bind(&body.coordinator_data_role)
    .execute(&mut *tx)
    .await?;

    if let Some(rules) = &body.initial_thresholds {
        sqlx::query(
            "INSERT INTO project_thresholds (project_id, version, rules, created_by) \
             VALUES ($1, 1, $2, $3)",
        )
        .bind(project.id)
        .bind(rules)
        .bind(auth.account_id)
        .execute(&mut *tx)
        .await?;
    }

    record_event_tx(
        &mut tx,
        project.id,
        "created",
        Some(auth.account_id),
        None,
        serde_json::json!({ "requireApproval": body.require_approval }),
    )
    .await?;

    tx.commit().await?;
    tracing::info!(project_id = %project.id, slug = %project.slug, "project created");
    Ok(Json(project.into()))
}

// ---- GET /api/v1/projects (public directory) ----------------------------------

#[derive(Deserialize)]
pub struct DirectoryQuery {
    pub q: Option<String>,
}

#[derive(sqlx::FromRow)]
struct DirectoryRow {
    id: Uuid,
    slug: String,
    title: String,
    target_name: String,
    target_ra_deg: f64,
    target_dec_deg: f64,
    target_radius_deg: f64,
    status: String,
    require_approval: bool,
    member_count: i64,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryItem {
    id: Uuid,
    slug: String,
    title: String,
    target_name: String,
    target_ra_deg: f64,
    target_dec_deg: f64,
    target_radius_deg: f64,
    status: String,
    require_approval: bool,
    member_count: i64,
    created_at: DateTime<Utc>,
}

/// Public project directory, newest first, optional title/target search.
#[tracing::instrument(skip_all)]
pub async fn directory(
    State(state): State<AppState>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<Vec<DirectoryItem>>, ApiError> {
    let rows = sqlx::query_as::<_, DirectoryRow>(
        "SELECT p.id, p.slug, p.title, p.target_name, p.target_ra_deg, p.target_dec_deg, \
                p.target_radius_deg, p.status, p.require_approval, p.created_at, \
                count(pm.account_id) AS member_count \
         FROM projects p \
         LEFT JOIN project_members pm ON pm.project_id = p.id \
         WHERE $1::text IS NULL OR p.title ILIKE '%' || $1 || '%' OR p.target_name ILIKE '%' || $1 || '%' \
         GROUP BY p.id \
         ORDER BY p.created_at DESC \
         LIMIT 200",
    )
    .bind(query.q.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        rows.into_iter()
            .map(|r| DirectoryItem {
                id: r.id,
                slug: r.slug,
                title: r.title,
                target_name: r.target_name,
                target_ra_deg: r.target_ra_deg,
                target_dec_deg: r.target_dec_deg,
                target_radius_deg: r.target_radius_deg,
                status: r.status,
                require_approval: r.require_approval,
                member_count: r.member_count,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

// ---- GET /api/v1/projects/{id} (public page; uuid or slug) --------------------

/// Resolve a path segment that may be a project uuid or a slug.
pub(crate) async fn resolve_project(
    db: &sqlx::PgPool,
    id_or_slug: &str,
) -> Result<ProjectRow, ApiError> {
    let row = match id_or_slug.parse::<Uuid>() {
        Ok(id) => {
            sqlx::query_as::<_, ProjectRow>(&format!(
                "SELECT {PROJECT_COLUMNS} FROM projects WHERE id = $1"
            ))
            .bind(id)
            .fetch_optional(db)
            .await?
        }
        Err(_) => {
            sqlx::query_as::<_, ProjectRow>(&format!(
                "SELECT {PROJECT_COLUMNS} FROM projects WHERE slug = $1"
            ))
            .bind(id_or_slug)
            .fetch_optional(db)
            .await?
        }
    };
    row.ok_or_else(|| ApiError::not_found("project not found"))
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    display_name: String,
    data_role: String,
    is_coordinator: bool,
    joined_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberPublicView {
    display_name: String,
    data_role: String,
    coordinator: bool,
    joined_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct PackageRow {
    id: Uuid,
    package_id: Uuid,
    frame_count: i32,
    byte_size: i64,
    created_at: DateTime<Utc>,
    publisher_display_name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagePublicView {
    id: Uuid,
    package_id: Uuid,
    frame_count: i32,
    byte_size: i64,
    created_at: DateTime<Utc>,
    publisher_display_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPage {
    project: ProjectView,
    members: Vec<MemberPublicView>,
    packages: Vec<PackagePublicView>,
}

/// Public project page: project card + member list + published packages.
/// Never exposes emails, pending/rejected announcements, or node ids.
#[tracing::instrument(skip_all)]
pub async fn project_page(
    State(state): State<AppState>,
    Path(id_or_slug): Path<String>,
) -> Result<Json<ProjectPage>, ApiError> {
    let project = resolve_project(&state.db, &id_or_slug).await?;

    let members = sqlx::query_as::<_, MemberRow>(
        "SELECT display_name, data_role, is_coordinator, joined_at \
         FROM project_members WHERE project_id = $1 ORDER BY joined_at",
    )
    .bind(project.id)
    .fetch_all(&state.db)
    .await?;

    let packages = sqlx::query_as::<_, PackageRow>(
        "SELECT a.id, a.package_id, a.frame_count, a.byte_size, a.created_at, \
                pm.display_name AS publisher_display_name \
         FROM package_announcements a \
         LEFT JOIN project_members pm \
           ON pm.project_id = a.project_id AND pm.account_id = a.publisher \
         WHERE a.project_id = $1 AND a.state = 'published' \
         ORDER BY a.created_at DESC",
    )
    .bind(project.id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(ProjectPage {
        project: project.into(),
        members: members
            .into_iter()
            .map(|m| MemberPublicView {
                display_name: m.display_name,
                data_role: m.data_role,
                coordinator: m.is_coordinator,
                joined_at: m.joined_at,
            })
            .collect(),
        packages: packages
            .into_iter()
            .map(|p| PackagePublicView {
                id: p.id,
                package_id: p.package_id,
                frame_count: p.frame_count,
                byte_size: p.byte_size,
                created_at: p.created_at,
                publisher_display_name: p
                    .publisher_display_name
                    .unwrap_or_else(|| "former member".to_string()),
            })
            .collect(),
    }))
}
```

In `src/routes/mod.rs`: add `pub mod projects;` after `pub mod devices;`. In `build_router`, add to the `public` router:

```rust
        .route("/api/v1/projects", get(projects::directory))
        .route("/api/v1/projects/{id}", get(projects::project_page))
```

and to the `protected` router:

```rust
        .route("/api/v1/projects", post(projects::create_project))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test projects_api`
Expected: PASS (3 tests). Also run `cargo test --locked --test device_registry` — the merged routes must not break existing suites.

- [ ] **Step 5: Commit**

```bash
git add src/routes/projects.rs src/routes/mod.rs tests/projects_api.rs tests/common/mod.rs
git commit -m "feat(collab): project create + public directory + public project page"
```

---

### Task 5: `PATCH /api/v1/projects/{id}` — coordinator settings edit

**Files:**
- Modify: `src/routes/projects.rs` (append handler)
- Modify: `src/routes/mod.rs` (add PATCH route)
- Test: `tests/projects_update.rs`

**Interfaces:**
- Consumes: Task 2 guards, Task 4 `ProjectRow`/`ProjectView`/`PROJECT_COLUMNS`/`ROLE_SEND_RECEIVE`.
- Produces: `PATCH /api/v1/projects/{id}` (coordinator) — body with all-optional `{description, goals, chatLink, dataPolicyText, requireApproval, status}` → 200 updated `ProjectView`. Title/target immutable in v1. `requireApproval: true` is refused (409) unless the coordinator holds `send_receive`; flipping it bumps `membership_version` (it is in the snapshot).

- [ ] **Step 1: Write the failing test**

```rust
// tests/projects_update.rs
//! Coordinator project-settings edits + the approval-toggle guard.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn coordinator_edits_fields(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &token, "P", false).await;
    let id = project["id"].as_str().unwrap();

    let (status, body) = send(
        &app,
        patch(
            &format!("/api/v1/projects/{id}"),
            &json!({"description": "new text", "status": "closed", "requireApproval": true}),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v = as_json(&body);
    assert_eq!(v["description"], "new text");
    assert_eq!(v["status"], "closed");
    assert_eq!(v["requireApproval"], true);
    // requireApproval is in the snapshot → version bumped.
    assert_eq!(v["membershipVersion"], 2);

    // Non-coordinator (any other account) → 403.
    let (other, _) = register_device(&app, &mailer, "other@example.com", 2, "Laptop").await;
    let (status, _) = send(
        &app,
        patch(&format!("/api/v1/projects/{id}"), &json!({"description": "x"}), Some(&other)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Bogus status → 400.
    let (status, _) = send(
        &app,
        patch(&format!("/api/v1/projects/{id}"), &json!({"status": "paused"}), Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn approval_toggle_requires_send_receive_coordinator(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;

    // Coordinator with a send-only role (allowed while approval is off).
    let (status, body) = send(
        &app,
        post(
            "/api/v1/projects",
            &json!({
                "title": "SendOnly",
                "target": {"name": "M101", "raDeg": 210.8, "decDeg": 54.35, "radiusDeg": 1.5},
                "coordinatorDisplayName": "C",
                "coordinatorDataRole": "send",
            }),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let id = as_json(&body)["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &app,
        patch(&format!("/api/v1/projects/{id}"), &json!({"requireApproval": true}), Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(as_json(&body)["error"].as_str().unwrap().contains("send_receive"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test projects_update`
Expected: FAIL — 405 (no PATCH route yet) surfaces as assertion failures.

- [ ] **Step 3: Write the implementation**

Append to `src/routes/projects.rs`:

```rust
// ---- PATCH /api/v1/projects/{id} ----------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProject {
    pub description: Option<String>,
    pub goals: Option<Value>,
    pub chat_link: Option<String>,
    pub data_policy_text: Option<String>,
    pub require_approval: Option<bool>,
    pub status: Option<String>,
}

/// Coordinator-only settings edit. Title/target are immutable in v1 (the slug
/// derives from the title; a target change would invalidate every member's
/// gate results). Flipping `require_approval` bumps the membership version —
/// the flag rides inside the signed snapshot.
#[tracing::instrument(skip_all)]
pub async fn update_project(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateProject>,
) -> Result<Json<ProjectView>, ApiError> {
    let member = crate::collab_auth::require_coordinator(&state.db, id, auth.account_id).await?;

    if let Some(status) = &body.status {
        if status != "active" && status != "closed" {
            return Err(ApiError::bad_request("status must be active or closed"));
        }
    }
    if body.require_approval == Some(true) && member.data_role != ROLE_SEND_RECEIVE {
        return Err(ApiError::conflict(
            "enable manual approval requires a send_receive coordinator (review needs the data)",
        ));
    }

    let mut tx = state.db.begin().await?;

    let current = sqlx::query_as::<_, ProjectRow>(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("project not found"))?;

    let approval_flip = body
        .require_approval
        .is_some_and(|v| v != current.require_approval);

    let updated = sqlx::query_as::<_, ProjectRow>(&format!(
        "UPDATE projects SET \
             description = COALESCE($2, description), \
             goals = COALESCE($3, goals), \
             chat_link = COALESCE($4, chat_link), \
             data_policy_text = COALESCE($5, data_policy_text), \
             require_approval = COALESCE($6, require_approval), \
             status = COALESCE($7, status) \
         WHERE id = $1 \
         RETURNING {PROJECT_COLUMNS}"
    ))
    .bind(id)
    .bind(body.description.as_deref().map(str::trim))
    .bind(&body.goals)
    .bind(body.chat_link.as_deref().map(str::trim))
    .bind(body.data_policy_text.as_deref().map(str::trim))
    .bind(body.require_approval)
    .bind(&body.status)
    .fetch_one(&mut *tx)
    .await?;

    if approval_flip {
        crate::collab_auth::bump_membership_version_tx(&mut tx, id).await?;
    }
    crate::collab_auth::record_event_tx(
        &mut tx,
        id,
        "settings_changed",
        Some(auth.account_id),
        None,
        serde_json::json!({ "requireApprovalFlipped": approval_flip }),
    )
    .await?;

    tx.commit().await?;

    // Re-read after the bump so the returned view carries the final version.
    let fresh = sqlx::query_as::<_, ProjectRow>(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(fresh.into()))
}
```

In `src/routes/mod.rs`, add to the `protected` router:

```rust
        .route("/api/v1/projects/{id}", axum::routing::patch(projects::update_project))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test projects_update`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/routes/projects.rs src/routes/mod.rs tests/projects_update.rs
git commit -m "feat(collab): coordinator project-settings edit with approval-toggle guard"
```

---

### Task 6: Join requests — create / list / approve / reject

**Files:**
- Create: `src/routes/join_requests.rs`
- Modify: `src/routes/mod.rs` (add `pub mod join_requests;` + four routes)
- Modify: `tests/common/mod.rs` (add `join_and_approve` helper)
- Test: `tests/join_requests.rs`

**Interfaces:**
- Consumes: Task 2 guards/events/bump (`_tx` variants), Task 4 `valid_role`/`resolve_project` is NOT used here (join is by uuid only).
- Produces:
  - `POST /api/v1/projects/{id}/join-requests` (authed) — `{displayName, desiredRole, message?}` → 200 `{id}`. 409 when already a member or a request is already open; 409 when the project is closed.
  - `GET /api/v1/projects/{id}/join-requests` (coordinator) — open requests `[{id, displayName, desiredRole, message, createdAt}]`.
  - `POST /api/v1/projects/{id}/join-requests/{req_id}/approve` (coordinator) — `{dataRole?}` (default = desired) → 200 `{accountId, dataRole}`; inserts the member, bumps the membership version, `member_joined` event.
  - `POST /api/v1/projects/{id}/join-requests/{req_id}/reject` (coordinator) → 204; `join_rejected` event.
  - Test helper `join_and_approve(app, coordinator_token, member_token, project_id, display_name, data_role) -> String` (the member's accountId).

- [ ] **Step 1: Write the failing test**

```rust
// tests/join_requests.rs
//! Join flow: request → coordinator decision → membership.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn join_flow_end_to_end(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (member, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();

    // Request to join.
    let (status, body) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests"),
            &json!({"displayName": "Anna", "desiredRole": "send_receive", "message": "hi"}),
            Some(&member),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let req_id = as_json(&body)["id"].as_str().unwrap().to_string();

    // A second open request is refused.
    let (status, _) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests"),
            &json!({"displayName": "Anna", "desiredRole": "send"}),
            Some(&member),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Only the coordinator lists requests.
    let (status, _) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests"), Some(&member))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests"), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(as_json(&body)[0]["displayName"], "Anna");

    // Approve with a role override.
    let (status, body) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests/{req_id}/approve"),
            &json!({"dataRole": "send"}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["dataRole"], "send");

    // Deciding twice is a 409.
    let (status, _) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests/{req_id}/approve"),
            &json!({}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // The member is on the public page; version bumped.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    let page = as_json(&body);
    assert_eq!(page["members"].as_array().unwrap().len(), 2);
    assert_eq!(page["project"]["membershipVersion"], 2);

    // A member asking to join again is refused up front.
    let (status, _) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests"),
            &json!({"displayName": "Anna", "desiredRole": "send"}),
            Some(&member),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn reject_leaves_no_membership(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (member, _) = register_device(&app, &mailer, "bob@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();

    let (_, body) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests"),
            &json!({"displayName": "Bob", "desiredRole": "send"}),
            Some(&member),
        ),
    )
    .await;
    let req_id = as_json(&body)["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests/{req_id}/reject"),
            &json!({}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert_eq!(as_json(&body)["members"].as_array().unwrap().len(), 1);

    // After a rejection a fresh request is allowed (only OPEN ones are unique).
    let (status, _) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/join-requests"),
            &json!({"displayName": "Bob", "desiredRole": "send"}),
            Some(&member),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
```

Append to `tests/common/mod.rs`:

```rust
/// Join `project_id` as the account behind `member_token` and approve as the
/// coordinator; returns the new member's accountId.
pub async fn join_and_approve(
    app: &axum::Router,
    coordinator_token: &str,
    member_token: &str,
    project_id: &str,
    display_name: &str,
    data_role: &str,
) -> String {
    let (status, body) = send(
        app,
        post(
            &format!("/api/v1/projects/{project_id}/join-requests"),
            &json!({"displayName": display_name, "desiredRole": data_role}),
            Some(member_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "join request: {}", String::from_utf8_lossy(&body));
    let req_id = as_json(&body)["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        app,
        post(
            &format!("/api/v1/projects/{project_id}/join-requests/{req_id}/approve"),
            &json!({}),
            Some(coordinator_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {}", String::from_utf8_lossy(&body));
    as_json(&body)["accountId"].as_str().unwrap().to_string()
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test join_requests`
Expected: FAIL — compile error (`join_requests` module missing).

- [ ] **Step 3: Write the implementation**

```rust
// src/routes/join_requests.rs
//! Join-request lifecycle (spec §8 "Join"): any signed-in account asks; the
//! coordinator decides with a role assignment; approval inserts the member and
//! bumps the membership version in the same transaction.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth_mw::AuthDevice;
use crate::collab_auth::{
    bump_membership_version_tx, record_event_tx, require_coordinator,
};
use crate::error::ApiError;
use crate::routes::projects::valid_role;
use crate::routes::{is_unique_violation, AppState};

// ---- POST /api/v1/projects/{id}/join-requests ---------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJoinRequest {
    pub display_name: String,
    pub desired_role: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestCreated {
    pub id: Uuid,
}

#[tracing::instrument(skip_all)]
pub async fn create_join_request(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateJoinRequest>,
) -> Result<Json<JoinRequestCreated>, ApiError> {
    let display_name = body.display_name.trim();
    if display_name.is_empty() || display_name.len() > 60 {
        return Err(ApiError::bad_request("displayName must be 1..=60 characters"));
    }
    if !valid_role(&body.desired_role) {
        return Err(ApiError::bad_request("desiredRole must be send or send_receive"));
    }
    if body.message.len() > 500 {
        return Err(ApiError::bad_request("message must be at most 500 characters"));
    }

    let (status,): (String,) = sqlx::query_as("SELECT status FROM projects WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    if status != "active" {
        return Err(ApiError::conflict("project is closed to new members"));
    }

    let already_member: Option<(Uuid,)> = sqlx::query_as(
        "SELECT account_id FROM project_members WHERE project_id = $1 AND account_id = $2",
    )
    .bind(id)
    .bind(auth.account_id)
    .fetch_optional(&state.db)
    .await?;
    if already_member.is_some() {
        return Err(ApiError::conflict("already a member"));
    }

    let res = sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO join_requests (project_id, account_id, display_name, desired_role, message) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(id)
    .bind(auth.account_id)
    .bind(display_name)
    .bind(&body.desired_role)
    .bind(body.message.trim())
    .fetch_one(&state.db)
    .await;

    match res {
        Ok((request_id,)) => {
            tracing::info!(project_id = %id, request_id = %request_id, "join request created");
            Ok(Json(JoinRequestCreated { id: request_id }))
        }
        Err(err) if is_unique_violation(&err, "one_open_join_request") => {
            Err(ApiError::conflict("a join request is already open"))
        }
        Err(err) => Err(err.into()),
    }
}

// ---- GET /api/v1/projects/{id}/join-requests ----------------------------------

#[derive(sqlx::FromRow)]
struct JoinRequestRow {
    id: Uuid,
    display_name: String,
    desired_role: String,
    message: String,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestView {
    id: Uuid,
    display_name: String,
    desired_role: String,
    message: String,
    created_at: DateTime<Utc>,
}

/// Open requests, oldest first (coordinator only).
#[tracing::instrument(skip_all)]
pub async fn list_join_requests(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<JoinRequestView>>, ApiError> {
    require_coordinator(&state.db, id, auth.account_id).await?;

    let rows = sqlx::query_as::<_, JoinRequestRow>(
        "SELECT id, display_name, desired_role, message, created_at \
         FROM join_requests WHERE project_id = $1 AND status = 'open' ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        rows.into_iter()
            .map(|r| JoinRequestView {
                id: r.id,
                display_name: r.display_name,
                desired_role: r.desired_role,
                message: r.message,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

// ---- POST /api/v1/projects/{id}/join-requests/{req_id}/approve ----------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveBody {
    /// Coordinator's role assignment; defaults to the requested role.
    pub data_role: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveResponse {
    pub account_id: Uuid,
    pub data_role: String,
}

#[tracing::instrument(skip_all)]
pub async fn approve_join_request(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path((id, req_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ApproveBody>,
) -> Result<Json<ApproveResponse>, ApiError> {
    require_coordinator(&state.db, id, auth.account_id).await?;
    if let Some(role) = &body.data_role {
        if !valid_role(role) {
            return Err(ApiError::bad_request("dataRole must be send or send_receive"));
        }
    }

    let mut tx = state.db.begin().await?;

    // Consume the OPEN request atomically; 409 when already decided.
    let consumed: Option<(Uuid, String, String)> = sqlx::query_as(
        "UPDATE join_requests \
         SET status = 'approved', decided_by = $3, decided_at = now() \
         WHERE id = $2 AND project_id = $1 AND status = 'open' \
         RETURNING account_id, display_name, desired_role",
    )
    .bind(id)
    .bind(req_id)
    .bind(auth.account_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((account_id, display_name, desired_role)) = consumed else {
        return Err(ApiError::conflict("join request is not open"));
    };

    let data_role = body.data_role.unwrap_or(desired_role);
    let inserted = sqlx::query(
        "INSERT INTO project_members (project_id, account_id, display_name, data_role) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(account_id)
    .bind(&display_name)
    .bind(&data_role)
    .execute(&mut *tx)
    .await;
    if let Err(err) = inserted {
        if is_unique_violation(&err, "project_members_pkey") {
            return Err(ApiError::conflict("already a member"));
        }
        return Err(err.into());
    }

    bump_membership_version_tx(&mut tx, id).await?;
    record_event_tx(
        &mut tx,
        id,
        "member_joined",
        Some(auth.account_id),
        Some(account_id),
        serde_json::json!({ "dataRole": data_role }),
    )
    .await?;
    tx.commit().await?;

    tracing::info!(project_id = %id, account_id = %account_id, data_role = %data_role, "member joined");
    Ok(Json(ApproveResponse { account_id, data_role }))
}

// ---- POST /api/v1/projects/{id}/join-requests/{req_id}/reject -----------------

#[tracing::instrument(skip_all)]
pub async fn reject_join_request(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path((id, req_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    require_coordinator(&state.db, id, auth.account_id).await?;

    let mut tx = state.db.begin().await?;
    let rejected: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE join_requests \
         SET status = 'rejected', decided_by = $3, decided_at = now() \
         WHERE id = $2 AND project_id = $1 AND status = 'open' \
         RETURNING account_id",
    )
    .bind(id)
    .bind(req_id)
    .bind(auth.account_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((account_id,)) = rejected else {
        return Err(ApiError::conflict("join request is not open"));
    };
    record_event_tx(&mut tx, id, "join_rejected", Some(auth.account_id), Some(account_id), serde_json::json!({}))
        .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
```

In `src/routes/mod.rs`: add `pub mod join_requests;`, and to the `protected` router:

```rust
        .route(
            "/api/v1/projects/{id}/join-requests",
            post(join_requests::create_join_request).get(join_requests::list_join_requests),
        )
        .route(
            "/api/v1/projects/{id}/join-requests/{req_id}/approve",
            post(join_requests::approve_join_request),
        )
        .route(
            "/api/v1/projects/{id}/join-requests/{req_id}/reject",
            post(join_requests::reject_join_request),
        )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test join_requests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/routes/join_requests.rs src/routes/mod.rs tests/join_requests.rs tests/common/mod.rs
git commit -m "feat(collab): join-request lifecycle with coordinator role assignment"
```

---

### Task 7: Member administration — role change / remove / leave / handover

**Files:**
- Create: `src/routes/members.rs`
- Modify: `src/routes/mod.rs` (add `pub mod members;` + four routes)
- Test: `tests/members_admin.rs`

**Interfaces:**
- Consumes: Task 2 guards (`require_member`, `require_coordinator`, `_tx` helpers), Task 4 `valid_role`/`ROLE_SEND_RECEIVE`, Task 6 `join_and_approve` test helper.
- Produces:
  - `PATCH /api/v1/projects/{id}/members/{account_id}` (coordinator) — `{dataRole}` → 204. 409 when demoting the coordinator of a `require_approval` project to `send`.
  - `DELETE /api/v1/projects/{id}/members/{account_id}` (coordinator) — 204. 400 when targeting self ("hand over coordination first").
  - `POST /api/v1/projects/{id}/leave` (member) — 204. 409 for the coordinator.
  - `POST /api/v1/projects/{id}/handover` (coordinator) — `{toAccountId}` → 204. Target must be a member (404); in a `require_approval` project the target must hold `send_receive` (409). Swaps `is_coordinator` inside one transaction (the partial unique index holds throughout).
  - All four bump the membership version and record events (`role_changed`, `member_removed`, `member_left`, `handover`).

- [ ] **Step 1: Write the failing test**

```rust
// tests/members_admin.rs
//! Coordinator member administration + the leave/handover rules.

mod common;

use axum::http::StatusCode;
use axum::body::Body;
use axum::http::Request;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn delete(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[sqlx::test]
async fn role_change_remove_and_guards(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    let anna_id = join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;

    // Coordinator changes Anna's role.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/v1/projects/{id}/members/{anna_id}"),
            &json!({"dataRole": "send"}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    let page = as_json(&body);
    let anna_row = page["members"].as_array().unwrap().iter()
        .find(|m| m["displayName"] == "Anna").unwrap();
    assert_eq!(anna_row["dataRole"], "send");
    // create(1) + join(2) + role change(3)
    assert_eq!(page["project"]["membershipVersion"], 3);

    // Anna (not coordinator) cannot administrate.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/v1/projects/{id}/members/{anna_id}"),
            &json!({"dataRole": "send_receive"}),
            Some(&anna),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // In a require_approval project the coordinator cannot demote themself to send.
    let coord_id = {
        let (_, body) = send(&app, get("/api/v1/devices", Some(&coord))).await;
        // account id comes from the DB, not the devices payload — read it directly:
        let (account,): (uuid::Uuid,) =
            sqlx::query_as("SELECT id FROM accounts WHERE email = 'coord@example.com'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let _ = body;
        account.to_string()
    };
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/v1/projects/{id}/members/{coord_id}"),
            &json!({"dataRole": "send"}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Removing self is a 400; removing Anna works.
    let (status, _) = send(&app, delete(&format!("/api/v1/projects/{id}/members/{coord_id}"), &coord)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(&app, delete(&format!("/api/v1/projects/{id}/members/{anna_id}"), &coord)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert_eq!(as_json(&body)["members"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn leave_and_handover(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    let anna_id = join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;
    let _bob_id = join_and_approve(&app, &coord, &bob, id, "Bob", "send").await;

    // The coordinator cannot leave.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/leave"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Handover to a send-only member in an approval project is refused.
    let (bob_account,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM accounts WHERE email = 'bob@example.com'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/handover"),
            &json!({"toAccountId": bob_account.to_string()}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Handover to Anna (send_receive) succeeds; exactly one coordinator remains.
    let (status, _) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/handover"),
            &json!({"toAccountId": anna_id}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    let coordinators: Vec<_> = as_json(&body)["members"].as_array().unwrap().iter()
        .filter(|m| m["coordinator"] == true).cloned().collect();
    assert_eq!(coordinators.len(), 1);
    assert_eq!(coordinators[0]["displayName"], "Anna");

    // The former coordinator can now leave.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/leave"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test members_admin`
Expected: FAIL — compile error (`members` module missing).

- [ ] **Step 3: Write the implementation**

```rust
// src/routes/members.rs
//! Member administration (spec §5a): role change, removal, leave, handover.
//! Every mutation bumps the membership version (the snapshot must change) and
//! records an audit event, all inside one transaction.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth_mw::AuthDevice;
use crate::collab_auth::{
    bump_membership_version_tx, record_event_tx, require_coordinator, require_member,
};
use crate::error::ApiError;
use crate::routes::projects::{valid_role, ROLE_SEND_RECEIVE};
use crate::routes::AppState;

/// The project's `require_approval` flag, or 404.
async fn require_approval_flag(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
) -> Result<bool, ApiError> {
    let row: Option<(bool,)> = sqlx::query_as("SELECT require_approval FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(conn)
        .await?;
    row.map(|(f,)| f).ok_or_else(|| ApiError::not_found("project not found"))
}

// ---- PATCH /api/v1/projects/{id}/members/{account_id} -------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleChange {
    pub data_role: String,
}

#[tracing::instrument(skip_all)]
pub async fn change_member_role(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path((id, account_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RoleChange>,
) -> Result<StatusCode, ApiError> {
    require_coordinator(&state.db, id, auth.account_id).await?;
    if !valid_role(&body.data_role) {
        return Err(ApiError::bad_request("dataRole must be send or send_receive"));
    }

    let mut tx = state.db.begin().await?;

    let target: Option<(String, bool)> = sqlx::query_as(
        "SELECT data_role, is_coordinator FROM project_members \
         WHERE project_id = $1 AND account_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((old_role, target_is_coordinator)) = target else {
        return Err(ApiError::not_found("member not found"));
    };

    // §2/§11: a manual-approval project's coordinator must keep send_receive.
    if target_is_coordinator
        && body.data_role != ROLE_SEND_RECEIVE
        && require_approval_flag(&mut tx, id).await?
    {
        return Err(ApiError::conflict(
            "the coordinator of a manual-approval project must keep send_receive",
        ));
    }

    sqlx::query(
        "UPDATE project_members SET data_role = $3 WHERE project_id = $1 AND account_id = $2",
    )
    .bind(id)
    .bind(account_id)
    .bind(&body.data_role)
    .execute(&mut *tx)
    .await?;

    bump_membership_version_tx(&mut tx, id).await?;
    record_event_tx(
        &mut tx,
        id,
        "role_changed",
        Some(auth.account_id),
        Some(account_id),
        serde_json::json!({ "from": old_role, "to": body.data_role }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- DELETE /api/v1/projects/{id}/members/{account_id} ------------------------

#[tracing::instrument(skip_all)]
pub async fn remove_member(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path((id, account_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    require_coordinator(&state.db, id, auth.account_id).await?;
    if account_id == auth.account_id {
        return Err(ApiError::bad_request("hand over coordination first"));
    }

    let mut tx = state.db.begin().await?;
    let removed = sqlx::query("DELETE FROM project_members WHERE project_id = $1 AND account_id = $2")
        .bind(id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(ApiError::not_found("member not found"));
    }
    bump_membership_version_tx(&mut tx, id).await?;
    record_event_tx(&mut tx, id, "member_removed", Some(auth.account_id), Some(account_id), serde_json::json!({}))
        .await?;
    tx.commit().await?;

    tracing::info!(project_id = %id, account_id = %account_id, "member removed");
    Ok(StatusCode::NO_CONTENT)
}

// ---- POST /api/v1/projects/{id}/leave ------------------------------------------

#[tracing::instrument(skip_all)]
pub async fn leave_project(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let member = require_member(&state.db, id, auth.account_id).await?;
    if member.is_coordinator {
        return Err(ApiError::conflict("the coordinator must hand over before leaving"));
    }

    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM project_members WHERE project_id = $1 AND account_id = $2")
        .bind(id)
        .bind(auth.account_id)
        .execute(&mut *tx)
        .await?;
    bump_membership_version_tx(&mut tx, id).await?;
    record_event_tx(&mut tx, id, "member_left", Some(auth.account_id), Some(auth.account_id), serde_json::json!({}))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- POST /api/v1/projects/{id}/handover ----------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handover {
    pub to_account_id: Uuid,
}

#[tracing::instrument(skip_all)]
pub async fn handover(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    Json(body): Json<Handover>,
) -> Result<StatusCode, ApiError> {
    require_coordinator(&state.db, id, auth.account_id).await?;
    if body.to_account_id == auth.account_id {
        return Err(ApiError::bad_request("already the coordinator"));
    }

    let mut tx = state.db.begin().await?;

    let target: Option<(String,)> = sqlx::query_as(
        "SELECT data_role FROM project_members \
         WHERE project_id = $1 AND account_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(body.to_account_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((target_role,)) = target else {
        return Err(ApiError::not_found("target is not a member"));
    };
    if target_role != ROLE_SEND_RECEIVE && require_approval_flag(&mut tx, id).await? {
        return Err(ApiError::conflict(
            "a manual-approval project requires a send_receive coordinator",
        ));
    }

    // Drop-then-raise inside one tx keeps the partial unique index satisfied.
    sqlx::query(
        "UPDATE project_members SET is_coordinator = false WHERE project_id = $1 AND account_id = $2",
    )
    .bind(id)
    .bind(auth.account_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE project_members SET is_coordinator = true WHERE project_id = $1 AND account_id = $2",
    )
    .bind(id)
    .bind(body.to_account_id)
    .execute(&mut *tx)
    .await?;

    bump_membership_version_tx(&mut tx, id).await?;
    record_event_tx(
        &mut tx,
        id,
        "handover",
        Some(auth.account_id),
        Some(body.to_account_id),
        serde_json::json!({}),
    )
    .await?;
    tx.commit().await?;

    tracing::info!(project_id = %id, from = %auth.account_id, to = %body.to_account_id, "coordinator handover");
    Ok(StatusCode::NO_CONTENT)
}
```

In `src/routes/mod.rs`: add `pub mod members;`, and to the `protected` router:

```rust
        .route(
            "/api/v1/projects/{id}/members/{account_id}",
            axum::routing::patch(members::change_member_role).delete(members::remove_member),
        )
        .route("/api/v1/projects/{id}/leave", post(members::leave_project))
        .route("/api/v1/projects/{id}/handover", post(members::handover))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test members_admin`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/routes/members.rs src/routes/mod.rs tests/members_admin.rs
git commit -m "feat(collab): member role change / removal / leave / coordinator handover"
```

---

### Task 8: Signed membership snapshot — `GET /api/v1/projects/{id}/membership`

**Files:**
- Create: `src/routes/snapshots.rs`
- Modify: `src/routes/mod.rs` (add `pub mod snapshots;` + route)
- Modify: `tests/common/mod.rs` (add `register_device_with_capability`)
- Test: `tests/membership_snapshot.rs`

**Interfaces:**
- Consumes: Task 2 `require_member`, Task 3 `signing_key`/`sign_b64`/`pubkey_b64`.
- Produces: `GET /api/v1/projects/{id}/membership` (member only) → `{payload, signature, pubkey}` where `payload` is base64 of the exact signed JSON bytes. Decoded payload (camelCase): `{schema: 1, projectId, membershipVersion, requireApproval, issuedAt, members: [{accountId, displayName, dataRole, coordinator, nodes: [<base64 32-byte device pubkey>]}]}`. `nodes` = the member account's **active, `capability='athenaeum'`** devices; members sorted by `accountId`, nodes sorted lexicographically — deterministic payloads. This is the slice-4 `PeerAuthorizer` feed; the payload schema is BINDING for the app client.

- [ ] **Step 1: Write the failing test**

Append to `tests/common/mod.rs`:

```rust
/// Like `register_device`, but declaring an explicit capability
/// (`athenaeum` | `perseus`) on /auth/verify.
pub async fn register_device_with_capability(
    app: &axum::Router,
    mailer: &CaptureMailer,
    email: &str,
    pubkey_seed: u8,
    name: &str,
    capability: &str,
) -> (String, String) {
    let (status, _) = send(
        app,
        post("/api/v1/auth/otp", &json!({ "email": email }), None),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let code = mailer.last_code(email);
    let (status, body) = send(
        app,
        post(
            "/api/v1/auth/verify",
            &json!({
                "email": email,
                "code": code,
                "devicePubkey": pubkey_b64(pubkey_seed),
                "deviceName": name,
                "deviceCapability": capability,
            }),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v = as_json(&body);
    (
        v["deviceToken"].as_str().unwrap().to_string(),
        v["deviceId"].as_str().unwrap().to_string(),
    )
}
```

```rust
// tests/membership_snapshot.rs
//! The signed snapshot: signature verifies against /collab/pubkey, nodes are
//! the member's active athenaeum devices, revocation/role changes reflect,
//! non-members get 403.

mod common;

use axum::http::StatusCode;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use common::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use sqlx::PgPool;

async fn fetch_snapshot(app: &axum::Router, project_id: &str, token: &str) -> (Value, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (status, body) = send(app, get(&format!("/api/v1/projects/{project_id}/membership"), Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v = as_json(&body);
    let payload = BASE64.decode(v["payload"].as_str().unwrap()).unwrap();
    let signature = BASE64.decode(v["signature"].as_str().unwrap()).unwrap();
    let pubkey = BASE64.decode(v["pubkey"].as_str().unwrap()).unwrap();
    let parsed: Value = serde_json::from_slice(&payload).unwrap();
    (parsed, payload, signature, pubkey)
}

#[sqlx::test]
async fn snapshot_signs_athenaeum_devices_only(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    // Anna: two athenaeum devices + one perseus capture node.
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let (_, anna_dev2) = register_device(&app, &mailer, "anna@example.com", 3, "Studio").await;
    register_device_with_capability(&app, &mailer, "anna@example.com", 4, "Obs", "perseus").await;

    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;

    let (parsed, payload, signature, pubkey) = fetch_snapshot(&app, id, &anna).await;

    // Signature verifies against the pinned key from /collab/pubkey.
    let (_, key_body) = send(&app, get("/api/v1/collab/pubkey", None)).await;
    assert_eq!(as_json(&key_body)["pubkey"].as_str().unwrap(),
               BASE64.encode(&pubkey), "snapshot pubkey == pinned pubkey");
    let vk = VerifyingKey::from_bytes(&pubkey.clone().try_into().unwrap()).unwrap();
    let sig = Signature::from_bytes(&signature.clone().try_into().unwrap());
    vk.verify(&payload, &sig).expect("signature verifies over the exact payload bytes");

    // Shape: schema, version (create + join = 2), approval flag, member nodes.
    assert_eq!(parsed["schema"], 1);
    assert_eq!(parsed["membershipVersion"], 2);
    assert_eq!(parsed["requireApproval"], true);
    let members = parsed["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    let anna_m = members.iter().find(|m| m["displayName"] == "Anna").unwrap();
    let nodes = anna_m["nodes"].as_array().unwrap();
    // Two athenaeum devices; the perseus node (seed 4) is excluded.
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().any(|n| n == &json!(pubkey_b64(2))));
    assert!(nodes.iter().any(|n| n == &json!(pubkey_b64(3))));
    assert!(!nodes.iter().any(|n| n == &json!(pubkey_b64(4))));

    // Revoke one of Anna's devices → it drops out of the next snapshot.
    let (status, _) = send(
        &app,
        post(&format!("/api/v1/devices/{anna_dev2}/revoke"), &json!({}), Some(&anna)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (parsed, ..) = fetch_snapshot(&app, id, &anna).await;
    let anna_m = parsed["members"].as_array().unwrap().iter()
        .find(|m| m["displayName"] == "Anna").unwrap().clone();
    assert_eq!(anna_m["nodes"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn snapshot_is_member_only(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (outsider, _) = register_device(&app, &mailer, "x@example.com", 2, "PC").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();

    let (status, _) = send(&app, get(&format!("/api/v1/projects/{id}/membership"), Some(&outsider))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&app, get(&format!("/api/v1/projects/{id}/membership"), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test membership_snapshot`
Expected: FAIL — compile error (`snapshots` module missing).

- [ ] **Step 3: Write the implementation**

```rust
// src/routes/snapshots.rs
//! Hub-signed membership snapshots — the cross-account trust anchor (spec
//! §2a). The response carries the EXACT signed bytes base64-encoded, so the
//! client verifies before parsing; nothing is recomputed or re-serialized on
//! the verify path. Deterministic ordering (members by accountId, nodes
//! lexicographic) keeps payloads reproducible.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use crate::auth_mw::AuthDevice;
use crate::collab_auth::require_member;
use crate::error::ApiError;
use crate::routes::AppState;
use crate::snapshot_sign;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotMember {
    account_id: Uuid,
    display_name: String,
    data_role: String,
    coordinator: bool,
    nodes: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotPayload {
    schema: u32,
    project_id: Uuid,
    membership_version: i64,
    require_approval: bool,
    issued_at: chrono::DateTime<Utc>,
    members: Vec<SnapshotMember>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedSnapshot {
    /// base64 of the exact JSON bytes the signature covers.
    payload: String,
    signature: String,
    pubkey: String,
}

#[derive(sqlx::FromRow)]
struct MemberDevicesRow {
    account_id: Uuid,
    display_name: String,
    data_role: String,
    is_coordinator: bool,
    pubkeys: Vec<Vec<u8>>,
}

#[tracing::instrument(skip_all)]
pub async fn membership_snapshot(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<Json<SignedSnapshot>, ApiError> {
    require_member(&state.db, id, auth.account_id).await?;

    let (membership_version, require_approval): (i64, bool) =
        sqlx::query_as("SELECT membership_version, require_approval FROM projects WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found("project not found"))?;

    // One row per member with their active athenaeum device pubkeys. Perseus
    // capture nodes never join project exchange; revoked devices drop out here.
    let rows = sqlx::query_as::<_, MemberDevicesRow>(
        "SELECT pm.account_id, pm.display_name, pm.data_role, pm.is_coordinator, \
                COALESCE(array_agg(d.pubkey ORDER BY d.pubkey) \
                         FILTER (WHERE d.id IS NOT NULL), '{}'::bytea[]) AS pubkeys \
         FROM project_members pm \
         LEFT JOIN devices d \
           ON d.account_id = pm.account_id \
          AND d.revoked_at IS NULL \
          AND d.capability = 'athenaeum' \
         WHERE pm.project_id = $1 \
         GROUP BY pm.account_id, pm.display_name, pm.data_role, pm.is_coordinator \
         ORDER BY pm.account_id",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let payload = SnapshotPayload {
        schema: 1,
        project_id: id,
        membership_version,
        require_approval,
        issued_at: Utc::now(),
        members: rows
            .into_iter()
            .map(|r| SnapshotMember {
                account_id: r.account_id,
                display_name: r.display_name,
                data_role: r.data_role,
                coordinator: r.is_coordinator,
                nodes: r.pubkeys.iter().map(|k| BASE64.encode(k)).collect(),
            })
            .collect(),
    };

    let bytes = serde_json::to_vec(&payload)
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("serialize snapshot")))?;
    let key = snapshot_sign::signing_key(&state.db).await?;

    Ok(Json(SignedSnapshot {
        payload: BASE64.encode(&bytes),
        signature: snapshot_sign::sign_b64(&key, &bytes),
        pubkey: snapshot_sign::pubkey_b64(&key),
    }))
}
```

In `src/routes/mod.rs`: add `pub mod snapshots;`, and to the `protected` router:

```rust
        .route("/api/v1/projects/{id}/membership", get(snapshots::membership_snapshot))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test membership_snapshot`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/routes/snapshots.rs src/routes/mod.rs tests/membership_snapshot.rs tests/common/mod.rs
git commit -m "feat(collab): hub-signed membership snapshots (athenaeum-device expansion, member-only)"
```

---

### Task 9: Versioned thresholds — `GET`/`POST /api/v1/projects/{id}/thresholds`

**Files:**
- Create: `src/routes/thresholds.rs`
- Modify: `src/routes/mod.rs` (add `pub mod thresholds;` + route)
- Test: `tests/thresholds.rs`

**Interfaces:**
- Consumes: Task 2 guards, Task 4 `validate_rules`.
- Produces:
  - `GET /api/v1/projects/{id}/thresholds` (member) → `{current: {version, rules, createdAt} | null, history: [{version, rules, createdAt}, …newest first]}`.
  - `POST /api/v1/projects/{id}/thresholds` (coordinator) — `{rules: [...]}` → 200 `{version}`; versions are strictly increasing per project (allocated under a project-row lock, no gaps assumed); `threshold_updated` event. Prospective-only semantics (spec §4) are the app's concern — the hub just stores versions.

- [ ] **Step 1: Write the failing test**

```rust
// tests/thresholds.rs
//! Versioned threshold rule-sets.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn versions_accumulate(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await; // seeds v1
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;

    // Members read; v1 came from initialThresholds.
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/thresholds"), Some(&anna))).await;
    assert_eq!(status, StatusCode::OK);
    let v = as_json(&body);
    assert_eq!(v["current"]["version"], 1);
    assert_eq!(v["current"]["rules"][0]["metricKey"], "fwhm_arcsec");

    // Coordinator posts v2.
    let (status, body) = send(
        &app,
        post(
            &format!("/api/v1/projects/{id}/thresholds"),
            &json!({"rules": [
                {"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0},
                {"metricKey": "not_trailed", "op": "reject_if", "value": true},
            ]}),
            Some(&coord),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["version"], 2);

    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/thresholds"), Some(&anna))).await;
    let v = as_json(&body);
    assert_eq!(v["current"]["version"], 2);
    assert_eq!(v["history"].as_array().unwrap().len(), 2);
    assert_eq!(v["history"][0]["version"], 2, "history newest first");

    // Members cannot write; outsiders cannot read; bad rules are 400.
    let (status, _) = send(
        &app,
        post(&format!("/api/v1/projects/{id}/thresholds"), &json!({"rules": [{"metricKey": "x", "op": "lte", "value": 1}]}), Some(&anna)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (outsider, _) = register_device(&app, &mailer, "x@example.com", 3, "PC").await;
    let (status, _) = send(&app, get(&format!("/api/v1/projects/{id}/thresholds"), Some(&outsider))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &app,
        post(&format!("/api/v1/projects/{id}/thresholds"), &json!({"rules": []}), Some(&coord)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test thresholds`
Expected: FAIL — compile error (`thresholds` module missing).

- [ ] **Step 3: Write the implementation**

```rust
// src/routes/thresholds.rs
//! Versioned quality-threshold rule-sets (spec §4). Rules are opaque
//! shape-validated jsonb; semantics live in the app's gate. Threshold changes
//! are prospective — old versions are immutable history.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth_mw::AuthDevice;
use crate::collab_auth::{record_event_tx, require_coordinator, require_member};
use crate::error::ApiError;
use crate::routes::projects::validate_rules;
use crate::routes::AppState;

#[derive(sqlx::FromRow)]
struct ThresholdRow {
    version: i32,
    rules: Value,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdView {
    version: i32,
    rules: Value,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdsResponse {
    current: Option<ThresholdView>,
    history: Vec<ThresholdView>,
}

#[tracing::instrument(skip_all)]
pub async fn get_thresholds(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<Json<ThresholdsResponse>, ApiError> {
    require_member(&state.db, id, auth.account_id).await?;

    let rows = sqlx::query_as::<_, ThresholdRow>(
        "SELECT version, rules, created_at FROM project_thresholds \
         WHERE project_id = $1 ORDER BY version DESC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let history: Vec<ThresholdView> = rows
        .into_iter()
        .map(|r| ThresholdView { version: r.version, rules: r.rules, created_at: r.created_at })
        .collect();
    let current = history.first().map(|t| ThresholdView {
        version: t.version,
        rules: t.rules.clone(),
        created_at: t.created_at,
    });
    Ok(Json(ThresholdsResponse { current, history }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewThresholds {
    rules: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewThresholdsResponse {
    version: i32,
}

#[tracing::instrument(skip_all)]
pub async fn post_thresholds(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    Json(body): Json<NewThresholds>,
) -> Result<Json<NewThresholdsResponse>, ApiError> {
    require_coordinator(&state.db, id, auth.account_id).await?;
    validate_rules(&body.rules)?;

    let mut tx = state.db.begin().await?;

    // Serialize version allocation on the project row (two concurrent posts
    // cannot both compute the same MAX+1).
    let locked: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM projects WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    if locked.is_none() {
        return Err(ApiError::not_found("project not found"));
    }

    let (version,): (i32,) = sqlx::query_as(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM project_thresholds WHERE project_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO project_thresholds (project_id, version, rules, created_by) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(version)
    .bind(&body.rules)
    .bind(auth.account_id)
    .execute(&mut *tx)
    .await?;

    record_event_tx(
        &mut tx,
        id,
        "threshold_updated",
        Some(auth.account_id),
        None,
        serde_json::json!({ "version": version }),
    )
    .await?;
    tx.commit().await?;

    tracing::info!(project_id = %id, version, "thresholds updated");
    Ok(Json(NewThresholdsResponse { version }))
}
```

In `src/routes/mod.rs`: add `pub mod thresholds;`, and to the `protected` router:

```rust
        .route(
            "/api/v1/projects/{id}/thresholds",
            get(thresholds::get_thresholds).post(thresholds::post_thresholds),
        )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test thresholds`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add src/routes/thresholds.rs src/routes/mod.rs tests/thresholds.rs
git commit -m "feat(collab): versioned quality-threshold rule-sets"
```

---

### Task 10: Announcements — ingest, approval state machine, visibility

**Files:**
- Create: `src/routes/announcements.rs`
- Modify: `src/routes/mod.rs` (add `pub mod announcements;` + three routes)
- Test: `tests/announcements.rs`

**Interfaces:**
- Consumes: Task 2 guards/events, Task 1 tables.
- Produces:
  - `POST /api/v1/projects/{id}/announcements` (member) — `{packageId, rootHash, byteSize, frameCount, aggregateStats?, supersedes?}` → 200 `{id, state}`. `state` is `pending` when the project has `require_approval` and the publisher is not the coordinator, else `published`. 409 on duplicate `packageId`; 409 when the project is closed.
  - `GET /api/v1/projects/{id}/announcements` (member) — visibility: coordinator sees ALL; a member sees `published` + their own rows in any state. Each row: `{id, packageId, publisherDisplayName, own, rootHash, byteSize, frameCount, aggregateStats, supersedes, state, rejectReason, createdAt, decidedAt}`. (Task 11 adds `holders`.)
  - `POST /api/v1/announcements/{id}/approve` (coordinator of that project) → 200 `{id, state: "published"}`; 409 unless currently `pending`.
  - `POST /api/v1/announcements/{id}/reject` — `{reason}` (1..=500 chars) → 200 `{id, state: "rejected"}`; 409 unless `pending`.
  - `pub(crate) async fn announcement_project(db, id) -> Result<(Uuid, String), ApiError>` — `(project_id, state)` lookup, 404 when missing (Task 11 reuses).

- [ ] **Step 1: Write the failing test**

```rust
// tests/announcements.rs
//! Package announcements: birth state per the approval toggle, the
//! pending→published|rejected state machine, and per-role visibility.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{json, Value};
use sqlx::PgPool;

fn announce_body(package_seed: u8) -> Value {
    json!({
        "packageId": uuid::Uuid::from_bytes([package_seed; 16]).to_string(),
        "rootHash": "ab".repeat(32),
        "byteSize": 104_000_000_i64,
        "frameCount": 12,
        "aggregateStats": {"integrationSecondsByFilter": {"L": 3600}},
    })
}

#[sqlx::test]
async fn approval_state_machine(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&app, &coord, "P", true).await; // approval ON
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;
    join_and_approve(&app, &coord, &bob, id, "Bob", "send_receive").await;

    // Member publish → pending; coordinator publish → published directly.
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce_body(1), Some(&anna))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let pending_id = as_json(&body)["id"].as_str().unwrap().to_string();
    assert_eq!(as_json(&body)["state"], "pending");

    let (_, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce_body(2), Some(&coord))).await;
    assert_eq!(as_json(&body)["state"], "published");

    // Duplicate packageId → 409.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce_body(1), Some(&anna))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Visibility: Bob sees only published; Anna sees published + her pending;
    // the coordinator sees everything; the public page shows published only.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&bob))).await;
    assert_eq!(as_json(&body).as_array().unwrap().len(), 1);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&anna))).await;
    let anna_view = as_json(&body);
    assert_eq!(anna_view.as_array().unwrap().len(), 2);
    assert!(anna_view.as_array().unwrap().iter().any(|a| a["state"] == "pending" && a["own"] == true));
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&coord))).await;
    assert_eq!(as_json(&body).as_array().unwrap().len(), 2);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert_eq!(as_json(&body)["packages"].as_array().unwrap().len(), 1);

    // Only the coordinator decides; approve flips pending → published once.
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{pending_id}/approve"), &json!({}), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(&app, post(&format!("/api/v1/announcements/{pending_id}/approve"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(as_json(&body)["state"], "published");
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{pending_id}/approve"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT, "double-decide is refused");

    // Reject path: a fresh pending from Anna, rejected with a reason.
    let (_, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce_body(3), Some(&anna))).await;
    let reject_id = as_json(&body)["id"].as_str().unwrap().to_string();
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{reject_id}/reject"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reason is mandatory");
    let (status, body) = send(&app, post(&format!("/api/v1/announcements/{reject_id}/reject"), &json!({"reason": "clouds"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(as_json(&body)["state"], "rejected");
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&anna))).await;
    let rejected = as_json(&body).as_array().unwrap().iter()
        .find(|a| a["id"] == reject_id.as_str()).cloned().unwrap();
    assert_eq!(rejected["rejectReason"], "clouds");
}

#[sqlx::test]
async fn auto_project_publishes_directly_and_validates(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await; // approval OFF
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;

    let (_, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce_body(1), Some(&anna))).await;
    assert_eq!(as_json(&body)["state"], "published");

    // Bad root hash → 400; outsider → 403.
    let mut bad = announce_body(2);
    bad["rootHash"] = json!("not-hex");
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &bad, Some(&anna))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (outsider, _) = register_device(&app, &mailer, "x@example.com", 3, "PC").await;
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce_body(4), Some(&outsider))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test announcements`
Expected: FAIL — compile error (`announcements` module missing).

- [ ] **Step 3: Write the implementation**

```rust
// src/routes/announcements.rs
//! Package announcements (spec §2/§5): the tracker's record of "this package
//! exists". Birth state implements the approval toggle; the state machine is
//! enforced by conditional UPDATEs (`WHERE state = 'pending'`) — a lost race
//! surfaces as a 409, never a double transition.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth_mw::AuthDevice;
use crate::collab_auth::{record_event, require_coordinator, require_member};
use crate::error::ApiError;
use crate::routes::{is_unique_violation, AppState};

/// `(project_id, state)` of an announcement, or 404.
pub(crate) async fn announcement_project(
    db: &sqlx::PgPool,
    id: Uuid,
) -> Result<(Uuid, String), ApiError> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT project_id, state FROM package_announcements WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::not_found("announcement not found"))
}

fn valid_root_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---- POST /api/v1/projects/{id}/announcements ----------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Announce {
    pub package_id: Uuid,
    pub root_hash: String,
    pub byte_size: i64,
    pub frame_count: i32,
    #[serde(default)]
    pub aggregate_stats: Value,
    #[serde(default)]
    pub supersedes: Vec<Uuid>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceResponse {
    pub id: Uuid,
    pub state: String,
}

#[tracing::instrument(skip_all)]
pub async fn announce(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    Json(body): Json<Announce>,
) -> Result<Json<AnnounceResponse>, ApiError> {
    let member = require_member(&state.db, id, auth.account_id).await?;

    if !valid_root_hash(&body.root_hash) {
        return Err(ApiError::bad_request("rootHash must be 64 hex characters"));
    }
    if body.byte_size <= 0 || body.frame_count <= 0 {
        return Err(ApiError::bad_request("byteSize and frameCount must be positive"));
    }
    if !(body.aggregate_stats.is_object() || body.aggregate_stats.is_null()) {
        return Err(ApiError::bad_request("aggregateStats must be an object"));
    }
    if body.supersedes.len() > 100 {
        return Err(ApiError::bad_request("supersedes lists at most 100 packages"));
    }

    let (project_status, require_approval): (String, bool) =
        sqlx::query_as("SELECT status, require_approval FROM projects WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    if project_status != "active" {
        return Err(ApiError::conflict("project is closed"));
    }

    // §2: pending unless approval is off or the coordinator publishes.
    let birth_state = if require_approval && !member.is_coordinator {
        "pending"
    } else {
        "published"
    };

    let stats = if body.aggregate_stats.is_null() {
        serde_json::json!({})
    } else {
        body.aggregate_stats.clone()
    };

    let res = sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO package_announcements \
         (project_id, package_id, publisher, root_hash, byte_size, frame_count, \
          aggregate_stats, supersedes, state) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(id)
    .bind(body.package_id)
    .bind(auth.account_id)
    .bind(&body.root_hash)
    .bind(body.byte_size)
    .bind(body.frame_count)
    .bind(&stats)
    .bind(&body.supersedes)
    .bind(birth_state)
    .fetch_one(&state.db)
    .await;

    let (announcement_id,) = match res {
        Ok(row) => row,
        Err(err) if is_unique_violation(&err, "package_announcements_package_id_key") => {
            return Err(ApiError::conflict("package already announced"));
        }
        Err(err) => return Err(err.into()),
    };

    record_event(
        &state.db,
        id,
        "announcement_created",
        Some(auth.account_id),
        None,
        serde_json::json!({ "announcementId": announcement_id, "state": birth_state }),
    )
    .await?;

    tracing::info!(project_id = %id, announcement_id = %announcement_id, state = birth_state, "package announced");
    Ok(Json(AnnounceResponse { id: announcement_id, state: birth_state.to_string() }))
}

// ---- GET /api/v1/projects/{id}/announcements ------------------------------------

#[derive(sqlx::FromRow)]
struct AnnouncementRow {
    id: Uuid,
    package_id: Uuid,
    publisher: Uuid,
    publisher_display_name: Option<String>,
    root_hash: String,
    byte_size: i64,
    frame_count: i32,
    aggregate_stats: Value,
    supersedes: Vec<Uuid>,
    state: String,
    reject_reason: Option<String>,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementView {
    pub id: Uuid,
    pub package_id: Uuid,
    pub publisher_display_name: String,
    pub own: bool,
    pub root_hash: String,
    pub byte_size: i64,
    pub frame_count: i32,
    pub aggregate_stats: Value,
    pub supersedes: Vec<Uuid>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
}

#[tracing::instrument(skip_all)]
pub async fn list_announcements(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AnnouncementView>>, ApiError> {
    let member = require_member(&state.db, id, auth.account_id).await?;

    // Coordinator: everything. Member: published + own rows (their pending /
    // rejected history is theirs to see; nobody else's pending leaks).
    let rows = sqlx::query_as::<_, AnnouncementRow>(
        "SELECT a.id, a.package_id, a.publisher, pm.display_name AS publisher_display_name, \
                a.root_hash, a.byte_size, a.frame_count, a.aggregate_stats, a.supersedes, \
                a.state, a.reject_reason, a.created_at, a.decided_at \
         FROM package_announcements a \
         LEFT JOIN project_members pm \
           ON pm.project_id = a.project_id AND pm.account_id = a.publisher \
         WHERE a.project_id = $1 \
           AND ($2 OR a.state = 'published' OR a.publisher = $3) \
         ORDER BY a.created_at DESC",
    )
    .bind(id)
    .bind(member.is_coordinator)
    .bind(auth.account_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        rows.into_iter()
            .map(|r| AnnouncementView {
                id: r.id,
                package_id: r.package_id,
                own: r.publisher == auth.account_id,
                publisher_display_name: r
                    .publisher_display_name
                    .unwrap_or_else(|| "former member".to_string()),
                root_hash: r.root_hash,
                byte_size: r.byte_size,
                frame_count: r.frame_count,
                aggregate_stats: r.aggregate_stats,
                supersedes: r.supersedes,
                state: r.state,
                reject_reason: r.reject_reason,
                created_at: r.created_at,
                decided_at: r.decided_at,
            })
            .collect(),
    ))
}

// ---- POST /api/v1/announcements/{id}/approve|reject ------------------------------

#[tracing::instrument(skip_all)]
pub async fn approve(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<Json<AnnounceResponse>, ApiError> {
    let (project_id, _) = announcement_project(&state.db, id).await?;
    require_coordinator(&state.db, project_id, auth.account_id).await?;

    let updated = sqlx::query(
        "UPDATE package_announcements \
         SET state = 'published', decided_by = $2, decided_at = now() \
         WHERE id = $1 AND state = 'pending'",
    )
    .bind(id)
    .bind(auth.account_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict("announcement is not pending"));
    }

    record_event(&state.db, project_id, "announcement_approved", Some(auth.account_id), None,
        serde_json::json!({ "announcementId": id })).await?;
    tracing::info!(project_id = %project_id, announcement_id = %id, "announcement approved");
    Ok(Json(AnnounceResponse { id, state: "published".to_string() }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectBody {
    pub reason: Option<String>,
}

#[tracing::instrument(skip_all)]
pub async fn reject(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    Json(body): Json<RejectBody>,
) -> Result<Json<AnnounceResponse>, ApiError> {
    let reason = body.reason.as_deref().map(str::trim).unwrap_or("");
    if reason.is_empty() || reason.len() > 500 {
        return Err(ApiError::bad_request("reason (1..=500 chars) is required"));
    }

    let (project_id, _) = announcement_project(&state.db, id).await?;
    require_coordinator(&state.db, project_id, auth.account_id).await?;

    let updated = sqlx::query(
        "UPDATE package_announcements \
         SET state = 'rejected', reject_reason = $3, decided_by = $2, decided_at = now() \
         WHERE id = $1 AND state = 'pending'",
    )
    .bind(id)
    .bind(auth.account_id)
    .bind(reason)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict("announcement is not pending"));
    }

    record_event(&state.db, project_id, "announcement_rejected", Some(auth.account_id), None,
        serde_json::json!({ "announcementId": id, "reason": reason })).await?;
    tracing::info!(project_id = %project_id, announcement_id = %id, "announcement rejected");
    Ok(Json(AnnounceResponse { id, state: "rejected".to_string() }))
}
```

In `src/routes/mod.rs`: add `pub mod announcements;`, and to the `protected` router:

```rust
        .route(
            "/api/v1/projects/{id}/announcements",
            post(announcements::announce).get(announcements::list_announcements),
        )
        .route("/api/v1/announcements/{id}/approve", post(announcements::approve))
        .route("/api/v1/announcements/{id}/reject", post(announcements::reject))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test announcements`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/routes/announcements.rs src/routes/mod.rs tests/announcements.rs
git commit -m "feat(collab): package announcements with the approval state machine + visibility rules"
```

---

### Task 11: Have-reports + holders on the member announcement list

**Files:**
- Modify: `src/routes/announcements.rs` (add `report_have`, extend the list with holders)
- Modify: `src/routes/mod.rs` (add route)
- Test: `tests/have_reports.rs`

**Interfaces:**
- Consumes: Task 10 `announcement_project`, Task 2 guards; `AuthDevice.device_id` (holder granularity is per device).
- Produces:
  - `POST /api/v1/announcements/{id}/have` (member) → 204, idempotent. Allowed when the caller's `data_role = 'send_receive'` OR the caller is the coordinator; the announcement must be `published`, EXCEPT the coordinator may report `have` on a `pending` one (the §2 review copy — on approve they are instantly a serving holder). Any other state → 409.
  - `AnnouncementView` gains `holders: [{pubkey, displayName, lastSeenAt}]` — devices with a have-report, still active (`revoked_at IS NULL`), whose account is still a member (ex-members drop out of holder lists even though their audit rows remain).

- [ ] **Step 1: Write the failing test**

```rust
// tests/have_reports.rs
//! "I hold package X": role gating, pending-only-for-coordinator, idempotency,
//! and holder lists on the member announcement view.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn announce_body(package_seed: u8) -> serde_json::Value {
    json!({
        "packageId": uuid::Uuid::from_bytes([package_seed; 16]).to_string(),
        "rootHash": "ab".repeat(32),
        "byteSize": 1_000_i64,
        "frameCount": 2,
        "aggregateStats": {"integrationSecondsByFilter": {"L": 600}},
    })
}

#[sqlx::test]
async fn have_reports_and_holder_lists(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;
    join_and_approve(&app, &coord, &bob, id, "Bob", "send").await; // send-only

    // Anna announces → pending. The coordinator may report-have on pending
    // (the push-seeded review copy); Anna (send_receive) may NOT while pending.
    let (_, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce_body(1), Some(&anna))).await;
    let ann = as_json(&body)["id"].as_str().unwrap().to_string();
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/have"), &json!({}), Some(&anna))).await;
    assert_eq!(status, StatusCode::CONFLICT, "pending is holdable only by the coordinator");
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/have"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Approve → Anna can now report; twice is idempotent; Bob (send-only) can't.
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/approve"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..2 {
        let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/have"), &json!({}), Some(&anna))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/have"), &json!({}), Some(&bob))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Holder list: exactly the coordinator's and Anna's devices, with names.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&bob))).await;
    let list = as_json(&body);
    let holders = list[0]["holders"].as_array().unwrap().clone();
    assert_eq!(holders.len(), 2);
    let names: Vec<_> = holders.iter().map(|h| h["displayName"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"Coord".to_string()) && names.contains(&"Anna".to_string()));
    assert!(holders.iter().any(|h| h["pubkey"] == json!(pubkey_b64(2))));

    // An ex-member's holds disappear from the list.
    let (anna_account,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM accounts WHERE email = 'anna@example.com'")
            .fetch_one(&pool).await.unwrap();
    let (status, _) = send(&app, {
        let uri = format!("/api/v1/projects/{id}/members/{anna_account}");
        axum::http::Request::builder().method("DELETE").uri(&uri)
            .header("authorization", format!("Bearer {coord}"))
            .body(axum::body::Body::empty()).unwrap()
    }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&bob))).await;
    assert_eq!(as_json(&body)[0]["holders"].as_array().unwrap().len(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test have_reports`
Expected: FAIL — 404/405 on `/have` (route missing) and missing `holders` field.

- [ ] **Step 3: Write the implementation**

Append to `src/routes/announcements.rs`:

```rust
// ---- POST /api/v1/announcements/{id}/have ----------------------------------------

/// "This device holds package X." Receive-capable members only; pending
/// packages are holdable only by the coordinator (the §2 review copy).
/// Idempotent (PK upsert-ignore).
#[tracing::instrument(skip_all)]
pub async fn report_have(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    let (project_id, ann_state) = announcement_project(&state.db, id).await?;
    let member = require_member(&state.db, project_id, auth.account_id).await?;

    if member.data_role != crate::routes::projects::ROLE_SEND_RECEIVE && !member.is_coordinator {
        return Err(ApiError::Status(axum::http::StatusCode::FORBIDDEN));
    }
    let holdable = ann_state == "published" || (ann_state == "pending" && member.is_coordinator);
    if !holdable {
        return Err(ApiError::conflict("announcement is not in a holdable state"));
    }

    sqlx::query(
        "INSERT INTO have_reports (announcement_id, device_id) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(auth.device_id)
    .execute(&state.db)
    .await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}
```

Extend the list: add to `AnnouncementView` (and its construction) a `holders` field, plus the holder query after the announcement query in `list_announcements`:

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderView {
    pub pubkey: String,
    pub display_name: String,
    pub last_seen_at: Option<DateTime<Utc>>,
}
```

Add `pub holders: Vec<HolderView>,` to `AnnouncementView`. In `list_announcements`, after fetching `rows`, fetch all holders for the project in one query and group in code:

```rust
    #[derive(sqlx::FromRow)]
    struct HolderRow {
        announcement_id: Uuid,
        pubkey: Vec<u8>,
        display_name: String,
        last_seen_at: Option<DateTime<Utc>>,
    }
    // Active devices of current members only — ex-members' audit rows stay in
    // have_reports but stop being offered as sources.
    let holder_rows = sqlx::query_as::<_, HolderRow>(
        "SELECT hr.announcement_id, d.pubkey, pm.display_name, d.last_seen_at \
         FROM have_reports hr \
         JOIN package_announcements a ON a.id = hr.announcement_id AND a.project_id = $1 \
         JOIN devices d ON d.id = hr.device_id AND d.revoked_at IS NULL \
         JOIN project_members pm ON pm.project_id = a.project_id AND pm.account_id = d.account_id",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let mut holders_by_ann: std::collections::HashMap<Uuid, Vec<HolderView>> = std::collections::HashMap::new();
    for h in holder_rows {
        holders_by_ann.entry(h.announcement_id).or_default().push(HolderView {
            pubkey: crate::security::encode_pubkey(&h.pubkey),
            display_name: h.display_name,
            last_seen_at: h.last_seen_at,
        });
    }
```

and in the row-mapping closure set `holders: holders_by_ann.remove(&r.id).unwrap_or_default(),` (change the `map` to a `for` loop or use `.map(|r| { … })` with a `let mut holders_by_ann` captured mutably — a plain `for` loop pushing into a `Vec<AnnouncementView>` is the simplest correct form).

In `src/routes/mod.rs`, add to the `protected` router:

```rust
        .route("/api/v1/announcements/{id}/have", post(announcements::report_have))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test have_reports`
Expected: PASS (1 test). Also re-run `--test announcements` (the view gained a field — its assertions must still hold).

- [ ] **Step 5: Commit**

```bash
git add src/routes/announcements.rs src/routes/mod.rs tests/have_reports.rs
git commit -m "feat(collab): device-granular have-reports + holder lists on announcements"
```

---

### Task 12: Public-page progress + holder counts, full-flow E2E, README, final gates

**Files:**
- Modify: `src/routes/projects.rs` (enrich `project_page` with holder counts + progress)
- Modify: `README.md` (API endpoint table)
- Test: `tests/collab_flow.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `PackagePublicView` gains `holderCount: i64` and `onlineHolderCount: i64` (a holder device is *online* when `last_seen_at > now() - interval '5 minutes'` — the auth middleware stamps it on every authenticated call, throttled to 1/min).
  - `ProjectPage` gains `progress: {totalFrames: i64, integrationSecondsByFilter: {<filter>: f64}, perMember: [{displayName, frames: i64, integrationSeconds: f64}]}` — summed over **published** announcements; per-frame data never reaches the hub (spec §2 BRD amendment), so this is exactly the aggregate the portal may show. The `aggregateStats.integrationSecondsByFilter` object is the app↔hub contract for integration accounting.

- [ ] **Step 1: Write the failing test**

```rust
// tests/collab_flow.rs
//! Slice-1 end-to-end: create (approval on) → join ×2 → thresholds v2 →
//! member announce (pending) → coordinator holds + approves → processor holds
//! → snapshot verifies → public page shows counts + progress → leave bumps.

mod common;

use axum::http::StatusCode;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use common::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use sqlx::PgPool;

#[sqlx::test]
async fn full_project_lifecycle(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let (marek, _) = register_device(&app, &mailer, "marek@example.com", 3, "NAS").await;

    // Create with approval ON; two members join.
    let project = create_project_via(&app, &coord, "M 101 Deep Field", true).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;
    join_and_approve(&app, &coord, &marek, id, "Marek", "send_receive").await;

    // Tighter thresholds v2.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/thresholds"),
        &json!({"rules": [{"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0}]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);

    // Anna publishes 12 frames → pending; coordinator reviews (holds) and approves.
    let announce = json!({
        "packageId": uuid::Uuid::from_bytes([9; 16]).to_string(),
        "rootHash": "cd".repeat(32),
        "byteSize": 1_248_000_000_i64,
        "frameCount": 12,
        "aggregateStats": {"integrationSecondsByFilter": {"L": 7200.0}},
    });
    let (_, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce, Some(&anna))).await;
    let ann = as_json(&body)["id"].as_str().unwrap().to_string();
    assert_eq!(as_json(&body)["state"], "pending");

    // Public page: nothing visible while pending.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert!(as_json(&body)["packages"].as_array().unwrap().is_empty());

    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/have"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/approve"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/have"), &json!({}), Some(&marek))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Public page: published package with 2 holders (both online — they just
    // made authenticated calls), plus progress from the aggregate stats.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    let page = as_json(&body);
    let pkg = &page["packages"][0];
    assert_eq!(pkg["frameCount"], 12);
    assert_eq!(pkg["holderCount"], 2);
    assert_eq!(pkg["onlineHolderCount"], 2);
    assert_eq!(page["progress"]["totalFrames"], 12);
    assert_eq!(page["progress"]["integrationSecondsByFilter"]["L"], 7200.0);
    let per_member = page["progress"]["perMember"].as_array().unwrap();
    let anna_row = per_member.iter().find(|m| m["displayName"] == "Anna").unwrap();
    assert_eq!(anna_row["frames"], 12);

    // The snapshot verifies and reflects the three members.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/membership"), Some(&marek))).await;
    let v: Value = as_json(&body);
    let payload = BASE64.decode(v["payload"].as_str().unwrap()).unwrap();
    let sig: [u8; 64] = BASE64.decode(v["signature"].as_str().unwrap()).unwrap().try_into().unwrap();
    let pk: [u8; 32] = BASE64.decode(v["pubkey"].as_str().unwrap()).unwrap().try_into().unwrap();
    VerifyingKey::from_bytes(&pk).unwrap()
        .verify(&payload, &Signature::from_bytes(&sig))
        .expect("snapshot signature verifies");
    let parsed: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(parsed["members"].as_array().unwrap().len(), 3);
    let version_before = parsed["membershipVersion"].as_i64().unwrap();

    // Anna leaves → the next snapshot is one member shorter, version bumped.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/leave"), &json!({}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/membership"), Some(&marek))).await;
    let payload = BASE64.decode(as_json(&body)["payload"].as_str().unwrap()).unwrap();
    let parsed: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(parsed["members"].as_array().unwrap().len(), 2);
    assert!(parsed["membershipVersion"].as_i64().unwrap() > version_before);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked --test collab_flow`
Expected: FAIL — `holderCount` / `progress` missing from the page payload.

- [ ] **Step 3: Write the implementation**

In `src/routes/projects.rs`, extend the page types and `project_page`:

1. `PackageRow` gains `holder_count: i64, online_holder_count: i64`; `PackagePublicView` gains `holder_count: i64, online_holder_count: i64` (serde camelCase renders `holderCount`/`onlineHolderCount`). Grouping is by `a.id` (the PK), so the other `a.*` columns are functionally dependent and need no extra GROUP BY entries. Replace the packages query with:

```rust
    let packages = sqlx::query_as::<_, PackageRow>(
        "SELECT a.id, a.package_id, a.frame_count, a.byte_size, a.created_at, \
                pm.display_name AS publisher_display_name, \
                count(d.id) AS holder_count, \
                count(d.id) FILTER (WHERE d.last_seen_at > now() - interval '5 minutes') \
                    AS online_holder_count \
         FROM package_announcements a \
         LEFT JOIN project_members pm \
           ON pm.project_id = a.project_id AND pm.account_id = a.publisher \
         LEFT JOIN have_reports hr ON hr.announcement_id = a.id \
         LEFT JOIN devices d \
           ON d.id = hr.device_id AND d.revoked_at IS NULL \
          AND EXISTS (SELECT 1 FROM project_members pm2 \
                      WHERE pm2.project_id = a.project_id AND pm2.account_id = d.account_id) \
         WHERE a.project_id = $1 AND a.state = 'published' \
         GROUP BY a.id, pm.display_name \
         ORDER BY a.created_at DESC",
    )
    .bind(project.id)
    .fetch_all(&state.db)
    .await?;
```

2. Add the progress types + fold (per-frame data never reaches the hub — this sums the announcement-level aggregates):

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberProgress {
    display_name: String,
    frames: i64,
    integration_seconds: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProgress {
    total_frames: i64,
    integration_seconds_by_filter: std::collections::BTreeMap<String, f64>,
    per_member: Vec<MemberProgress>,
}
```

`ProjectPage` gains `progress: ProjectProgress`. In `project_page`, before building the response, fold over published announcements (publisher display name resolved through the already-fetched package rows):

```rust
    #[derive(sqlx::FromRow)]
    struct StatsRow {
        publisher_display_name: Option<String>,
        frame_count: i32,
        aggregate_stats: Value,
    }
    let stats_rows = sqlx::query_as::<_, StatsRow>(
        "SELECT pm.display_name AS publisher_display_name, a.frame_count, a.aggregate_stats \
         FROM package_announcements a \
         LEFT JOIN project_members pm \
           ON pm.project_id = a.project_id AND pm.account_id = a.publisher \
         WHERE a.project_id = $1 AND a.state = 'published'",
    )
    .bind(project.id)
    .fetch_all(&state.db)
    .await?;

    let mut total_frames = 0i64;
    let mut by_filter = std::collections::BTreeMap::<String, f64>::new();
    let mut per_member = std::collections::BTreeMap::<String, (i64, f64)>::new();
    for row in stats_rows {
        total_frames += row.frame_count as i64;
        let mut package_seconds = 0.0;
        if let Some(map) = row
            .aggregate_stats
            .get("integrationSecondsByFilter")
            .and_then(Value::as_object)
        {
            for (filter, seconds) in map {
                let s = seconds.as_f64().unwrap_or(0.0);
                *by_filter.entry(filter.clone()).or_default() += s;
                package_seconds += s;
            }
        }
        let name = row
            .publisher_display_name
            .unwrap_or_else(|| "former member".to_string());
        let entry = per_member.entry(name).or_default();
        entry.0 += row.frame_count as i64;
        entry.1 += package_seconds;
    }
    let progress = ProjectProgress {
        total_frames,
        integration_seconds_by_filter: by_filter,
        per_member: per_member
            .into_iter()
            .map(|(display_name, (frames, integration_seconds))| MemberProgress {
                display_name,
                frames,
                integration_seconds,
            })
            .collect(),
    };
```

and set `progress` in the returned `ProjectPage` (plus map the two new counters into `PackagePublicView`).

3. Append an "API" section to `README.md` listing every collab endpoint one per line (method, path, auth: public / device token / member / coordinator) — mirror the route registrations from `src/routes/mod.rs` verbatim.

- [ ] **Step 4: Run the full suite + build gates**

```bash
DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --locked
cargo build --release --locked --bin athenaeum-hub
```
Expected: every suite green (schema, auth, snapshot_sign, projects_api, projects_update, join_requests, members_admin, membership_snapshot, thresholds, announcements, have_reports, collab_flow + the pre-existing auth_flow/device_registry/health/relay), release build clean, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/routes/projects.rs README.md tests/collab_flow.rs
git commit -m "feat(collab): public-page holder counts + aggregate progress; slice-1 e2e flow"
```

---

## Post-plan checklist (not tasks — release/ops notes)

- **Merge gate:** the branch merges to `main` only after a whole-slice review; a hub deploy after merge ships these endpoints (additive — no existing client calls them, safe with live v0.4.x clients).
- **Migration 0009 is additive** — no existing-table changes, safe on the production DB with live rows.
- **Spec kept honest:** slice 1 deliberately does NOT implement portal session auth (slice 2 adds a cookie-session layer for browser callers; every endpoint here authenticates with the existing device bearer token, and handlers depend only on `AuthDevice.account_id`, so slice 2 can add a second identity extractor without touching handlers).
- Athenaeum-side memory/ledger updates happen in the athenaeum repo session, not in this plan.

## Self-review notes (already applied)

1. **Spec coverage (§5/§5a/§2-approval/§13-slice-1):** tables ✓ (Task 1), permissions matrix enforcement ✓ (guards in Tasks 2/5/6/7; coordinator-`send_receive` rule at create/PATCH/role-change/handover), signed snapshots ✓ (Tasks 3/8), announcement state machine + visibility ✓ (Task 10), have-reports/holders/aggregates ✓ (Tasks 11/12), audit events ✓ (throughout), join flow ✓ (Task 6). NOT in slice 1 (per §13): portal UI, app client, exchange, email notifications for join decisions (spec §8 mentions email nudges — portal/slice-2 territory, the hub only stores decisions here).
2. **Type consistency:** `data_role` strings are the single source (`ROLE_SEND`/`ROLE_SEND_RECEIVE` from `projects.rs`); every path template uses `{id}` (+ `{account_id}`/`{req_id}`); `is_unique_violation` names checked against migration DDL (`projects_slug_key`, `one_open_join_request`, `project_members_pkey`, `one_coordinator_per_project`, `package_announcements_package_id_key`).
3. **Known simplifications (deliberate, documented):** thresholds version allocation locks the project row; snapshot determinism is ordering-only (no canonical-JSON scheme — the signature covers the exact transported bytes, so canonicalization is unnecessary); directory capped at 200 rows (no pagination in v1).




