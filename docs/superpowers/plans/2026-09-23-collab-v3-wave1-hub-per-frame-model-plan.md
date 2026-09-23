# Collab v3 — Wave 1: hub per-frame model — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The hub stores a per-frame manifest (file names included), holds
holders per frame and device, versions every project change behind one cheap
poll, and offers dictionary, trust and grid routes — with the package model
(`package_announcements`, `have_reports`, `/announcements`) removed and every
consumer (project page, `/me`, stats, profiles, operator console, digest)
re-based on frames.

**Architecture:** One new migration (`0022_per_frame_model.sql`) that creates
the frame tables and drops the package tables behind a guard. New route modules
`frames.rs`, `holders.rs`, `dictionary.rs`, `versions.rs`; a `project_version`
helper that bumps `projects.version` inside every writing transaction and
invalidates an in-process `VersionCache` on `AppState`. `announcements.rs` is
deleted; its two SQL constants (`HAVE_REPORT_FRESH_SQL`, holder devices) move
to `holders.rs` on the new tables. Old routes answer `409 collab_api_outdated`.

**Tech Stack:** Rust 2021, axum 0.8, sqlx 0.8 / Postgres 16, `#[sqlx::test]`
(154 tests today, one DB per test), React/TS portal with vitest.

**Spec:** `docs/superpowers/specs/2026-09-23-collab-v3-per-frame-model-design.md`
— §3 model, §4 hub (tables 4.1, routes 4.2, load 4.3), §9 moderation, §11
migration, rulings R4, R5, R8, R11–R15, R19.

## Global Constraints

- Hub repo `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`,
  branch `collab-v3-wave1` from `collab-v3-wave0` (wave 0 merged first; it
  provides `src/collab_rules.rs` and `GET /projects/{id}/members`).
- Tests: `docker compose up -d postgres`; `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`. Every task ends green on the whole suite.
- Every handler: `#[tracing::instrument(skip_all)]`, refusals name the rule, `Internal` never swallowed.
- Rule S1 unchanged: cross-account holder info carries ONLY `homeRelayUrl`, never `directAddrs`.
- Freshness constants unchanged: holder fresh = reported within 75 minutes, online = device seen within 5 minutes.
- `membership_version` and the signed snapshot are unchanged in shape; `projects.version` is a separate, superset counter.
- The migration refuses to run on a database that still holds package rows (spec §11): `RAISE EXCEPTION` when `count(*) FROM package_announcements > 0`. The test hub holds 0 published bytes; prod has had publishing blocked since 2026-08-31.
- Wire names camelCase (`#[serde(rename_all = "camelCase")]`), SQL snake_case.
- No third-party product names. Commit as `eg013ra1n <vilen.sharifov@gmail.com>` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session:` trailers. No push, no deploy.

---

## File structure

- Create `migrations/0022_per_frame_model.sql`.
- Create `src/project_version.rs` — `bump_project_version_tx`, `VersionCache`.
- Create `src/routes/frames.rs` — announce, manifest, version, patch, approve, reject, `FrameRow`/`FrameView`, `validate_file_name`.
- Create `src/routes/holders.rs` — `put_holders_self`, `frame_holders`, `HOLDER_FRESH_SQL`, `HOLDER_ONLINE_SQL`, `fresh_holders_sql()`.
- Create `src/routes/dictionary.rs` — `get_dictionary`, `put_dictionary`, `DEFAULT_DICTIONARY`, `validate_dictionary`, `canonical_filters_tx`.
- Create `src/routes/versions.rs` — `GET /me/project-versions`.
- Create `src/routes/compat.rs` — the 409 handler for retired routes.
- Modify `src/routes/mod.rs` — `AppState.versions`, route table.
- Modify `src/routes/projects.rs` — `PROJECT_COLUMNS` (+`version`, `canonical_grid`), `project_page` (coverage instead of packages), `create_project_core` (default dictionary), `UpdateProject` (no change), new `put_grid`.
- Modify `src/routes/members.rs` — `put_trust`; `MemberAdminRow.trusted_publisher`.
- Modify `src/collab_auth.rs` — `Member.trusted_publisher`; `bump_membership_version_tx` also bumps the project version.
- Modify `src/routes/me.rs`, `stats.rs`, `profiles.rs`, `operator.rs`, `scheduler.rs`, `mail.rs`, `join_requests.rs` (candidate card) — frames instead of packages.
- Delete `src/routes/announcements.rs`, `tests/announcements.rs`, `tests/have_reports.rs`, `tests/have_soft_state.rs`.
- Create `tests/frames.rs`, `tests/holders.rs`, `tests/dictionary.rs`, `tests/versions.rs`, `tests/trust_and_grid.rs`, `tests/compat.rs`; fix `tests/{alumni,account_auth,collab_flow,hardening,governance,collab_schema,mail_and_scheduler,me,profiles,stats}.rs`.
- Modify `tests/common/mod.rs` — `announce_frames`, `frame_body`.
- Portal: `types.ts`, `pages/ProjectPage.tsx`, `pages/Join.tsx`, `pages/Profile.tsx`, `components/me/Member.tsx`, `pages/Operator.tsx`, `pages/Admin.tsx` (+ tests).
- `README.md` API section.

---

### Task 1: Migration 0022 and the version helper

**Files:**
- Create: `migrations/0022_per_frame_model.sql`
- Create: `src/project_version.rs`; add `pub mod project_version;` to the crate root.
- Modify: `src/routes/mod.rs:37-89` (`AppState.versions`), `src/collab_auth.rs` (`Member.trusted_publisher`, `bump_membership_version_tx`).
- Test: `tests/collab_schema.rs` (extend), `src/project_version.rs` unit test.

**Interfaces:**
- Produces:
  ```rust
  // src/project_version.rs
  pub async fn bump_project_version_tx(conn: &mut PgConnection, project_id: Uuid) -> Result<i64, sqlx::Error>; // UPDATE … RETURNING version
  #[derive(Clone, Default)] pub struct VersionCache(Arc<RwLock<HashMap<Uuid, i64>>>);
  impl VersionCache { pub fn get(&self, id: Uuid) -> Option<i64>; pub fn put(&self, id: Uuid, v: i64); pub fn invalidate(&self, id: Uuid); }
  ```
  `AppState.versions: VersionCache` (builder default; no constructor change needed beyond the field).
  `collab_auth::Member` gains `pub trusted_publisher: bool` (every `SELECT … FROM project_members` that maps to `Member` — `lock_member_rows`, `require_member` — adds the column).

- [ ] **Step 1: Write the failing schema test**

Append to `tests/collab_schema.rs`:

```rust
#[sqlx::test]
async fn per_frame_tables_exist_and_package_tables_are_gone(pool: PgPool) {
    for table in ["project_frames", "project_frame_versions", "frame_holders", "project_filter_dictionary"] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL").bind(table).fetch_one(&pool).await.unwrap();
        assert!(exists, "{table} missing");
    }
    for table in ["package_announcements", "have_reports"] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL").bind(table).fetch_one(&pool).await.unwrap();
        assert!(!exists, "{table} should be dropped");
    }
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns WHERE table_name = 'projects' AND column_name IN ('version','canonical_grid') ORDER BY 1",
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(cols, vec!["canonical_grid", "version"]);
    let trusted: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'project_members' AND column_name = 'trusted_publisher')",
    ).fetch_one(&pool).await.unwrap();
    assert!(trusted);
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test collab_schema per_frame_tables`
Expected: FAIL (`project_frames missing`).

- [ ] **Step 3: Write the migration**

`migrations/0022_per_frame_model.sql`:

```sql
-- Collab v3 (spec 2026-09-23 §4.1): the frame is the unit. Package tables
-- are dropped, never migrated — refuse on a database that still holds rows.
DO $$
BEGIN
    IF to_regclass('package_announcements') IS NOT NULL
       AND (SELECT count(*) FROM package_announcements) > 0 THEN
        RAISE EXCEPTION 'package_announcements is not empty; collab v3 does not migrate packages (spec §11)';
    END IF;
END $$;

DROP TABLE IF EXISTS have_reports;
DROP TABLE IF EXISTS package_announcements;

ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS version bigint NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS canonical_grid jsonb;

ALTER TABLE project_members
    ADD COLUMN IF NOT EXISTS trusted_publisher boolean NOT NULL DEFAULT false;

CREATE TABLE IF NOT EXISTS project_frames (
    project_id          uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    frame_uuid          uuid        NOT NULL,
    publisher           uuid        NOT NULL REFERENCES accounts (id),
    publisher_device_id uuid        REFERENCES devices (id) ON DELETE SET NULL,
    file_name           text        NOT NULL,
    content_version     integer     NOT NULL DEFAULT 1,
    blake3              text        NOT NULL,
    byte_size           bigint      NOT NULL CHECK (byte_size > 0),
    xxh3                text        NOT NULL,
    filter_raw          text        NOT NULL,
    filter_canonical    text        NOT NULL,
    channel             text        NOT NULL DEFAULT 'mono'
                        CHECK (channel IN ('mono', 'osc', 'osc-r', 'osc-g', 'osc-b')),
    exptime_sec         double precision NOT NULL CHECK (exptime_sec > 0),
    date_obs            timestamptz,
    meta                jsonb       NOT NULL DEFAULT '{}'::jsonb,
    gate_version        integer     NOT NULL,
    accepted            boolean     NOT NULL DEFAULT true,
    accepted_reason     text,
    accepted_by         uuid        REFERENCES accounts (id),
    accepted_at         timestamptz,
    state               text        NOT NULL CHECK (state IN ('pending', 'published', 'rejected')),
    reject_reason       text,
    decided_by          uuid        REFERENCES accounts (id),
    decided_at          timestamptz,
    manifest_version    bigint      NOT NULL,
    created_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, frame_uuid),
    CHECK (length(file_name) BETWEEN 1 AND 255)
);
CREATE INDEX IF NOT EXISTS project_frames_manifest  ON project_frames (project_id, manifest_version);
CREATE INDEX IF NOT EXISTS project_frames_publisher ON project_frames (project_id, publisher);
CREATE INDEX IF NOT EXISTS project_frames_coverage  ON project_frames (project_id, filter_canonical) WHERE state = 'published' AND accepted;

CREATE TABLE IF NOT EXISTS project_frame_versions (
    project_id      uuid        NOT NULL,
    frame_uuid      uuid        NOT NULL,
    content_version integer     NOT NULL,
    blake3          text        NOT NULL,
    byte_size       bigint      NOT NULL,
    xxh3            text        NOT NULL,
    announced_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, frame_uuid, content_version),
    FOREIGN KEY (project_id, frame_uuid) REFERENCES project_frames (project_id, frame_uuid) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS frame_holders (
    project_id      uuid        NOT NULL,
    frame_uuid      uuid        NOT NULL,
    device_id       uuid        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    content_version integer     NOT NULL,
    reported_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, frame_uuid, device_id),
    FOREIGN KEY (project_id, frame_uuid) REFERENCES project_frames (project_id, frame_uuid) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS frame_holders_device   ON frame_holders (device_id);
CREATE INDEX IF NOT EXISTS frame_holders_reported ON frame_holders (project_id, reported_at);

CREATE TABLE IF NOT EXISTS project_filter_dictionary (
    project_id  uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    version     integer     NOT NULL,
    entries     jsonb       NOT NULL,
    created_by  uuid        NOT NULL REFERENCES accounts (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, version)
);
```

`src/project_version.rs`:

```rust
//! `projects.version` — the one counter a device polls (collab v3 §4.2,
//! R19). Every write a device must notice bumps it inside the writing
//! transaction; the in-process cache answers `GET /me/project-versions`
//! without touching the database. Membership changes bump it too (see
//! `collab_auth::bump_membership_version_tx`), so `membership_version` is a
//! subset signal that stays for the signed snapshot.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sqlx::PgConnection;
use uuid::Uuid;

pub async fn bump_project_version_tx(conn: &mut PgConnection, project_id: Uuid) -> Result<i64, sqlx::Error> {
    let (v,): (i64,) =
        sqlx::query_as("UPDATE projects SET version = version + 1 WHERE id = $1 RETURNING version")
            .bind(project_id)
            .fetch_one(conn)
            .await?;
    Ok(v)
}

/// Per-process cache of `projects.version`. Invalidated (not updated) after
/// every committing write, because the commit may lose a race with another
/// bump; the next read repopulates from the row. Wrong only across two hub
/// replicas (spec §12) — the hub runs single-instance.
#[derive(Clone, Default)]
pub struct VersionCache(Arc<RwLock<HashMap<Uuid, i64>>>);

impl VersionCache {
    pub fn get(&self, id: Uuid) -> Option<i64> {
        self.0.read().expect("version cache poisoned").get(&id).copied()
    }
    pub fn put(&self, id: Uuid, v: i64) {
        self.0.write().expect("version cache poisoned").insert(id, v);
    }
    pub fn invalidate(&self, id: Uuid) {
        self.0.write().expect("version cache poisoned").remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_put_get_invalidate() {
        let c = VersionCache::default();
        let id = Uuid::new_v4();
        assert_eq!(c.get(id), None);
        c.put(id, 7);
        assert_eq!(c.get(id), Some(7));
        c.invalidate(id);
        assert_eq!(c.get(id), None);
    }
}
```

`src/routes/mod.rs`: add `pub versions: crate::project_version::VersionCache,` to `AppState` and `versions: Default::default()` in `AppState::new`.

`src/collab_auth.rs`:
- `Member` gains `pub trusted_publisher: bool`; add `trusted_publisher` to the column list of every query that maps into `Member` (`lock_member_rows`, `require_member`, `require_coordinator`, `require_cap` — grep `sqlx::query_as::<_, Member>`).
- `bump_membership_version_tx` becomes:

```rust
pub async fn bump_membership_version_tx(conn: &mut PgConnection, project_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE projects SET membership_version = membership_version + 1 WHERE id = $1")
        .bind(project_id)
        .execute(&mut *conn)
        .await?;
    crate::project_version::bump_project_version_tx(conn, project_id).await?;
    Ok(())
}
```

and `bump_membership_version` (pool variant) wraps the same two statements in a transaction. Callers that commit after a membership bump must call `state.versions.invalidate(id)`: `members.rs` (`patch_member`, `remove_member`, `leave_project`, `handover`), `join_requests.rs` (approve/join), `invites.rs` (redeem), `projects.rs` (`update_project` when `require_approval` flips, and `create_project`). Add the line right after each `tx.commit().await?;`.

- [ ] **Step 4: Compile and run the schema test — the crate will not compile until Task 3 deletes `announcements.rs` (it references the dropped tables only at runtime, so it DOES compile; run the schema test now)**

Run: `DATABASE_URL=… cargo test --test collab_schema && cargo test --lib project_version`
Expected: PASS. (Other test binaries that announce packages now fail at runtime — expected until Task 3.)

- [ ] **Step 5: Commit**

```bash
git checkout -b collab-v3-wave1 && git add migrations/0022_per_frame_model.sql src/project_version.rs src/lib.rs src/routes/mod.rs src/collab_auth.rs src/routes/members.rs src/routes/join_requests.rs src/routes/invites.rs src/routes/projects.rs tests/collab_schema.rs
git commit -m "feat(hub): per-frame model tables, projects.version + cache, trusted_publisher; package tables dropped behind a guard"
```

---

### Task 2: Frames module — announce batch and manifest delta

**Files:**
- Create: `src/routes/frames.rs`
- Modify: `src/routes/mod.rs` (`pub mod frames;`, two routes)
- Modify: `tests/common/mod.rs` (helpers)
- Test: `tests/frames.rs`

**Interfaces:**
- Wire (`FrameIn`, request item of `POST /projects/{id}/frames`):
  `{frameUuid, fileName, blake3, byteSize, xxh3, filterRaw, filterCanonical, channel?, exptimeSec, dateObs?, gateVersion, meta?}`; `meta` is any JSON object ≤ 8 KiB (instrume, telescope, xbinning, naxis1/2, pixelScaleArcsec, bayerpat, focalLen, aperture, metrics {fwhmArcsec, eccentricity, starsDetected, medianSnr, snrWeight, frameSnr}, zeroPoint, wcs).
- Response: `{state: "published"|"pending", projectVersion: i64, announced: usize}`.
- `GET /projects/{id}/manifest?since=V&limit=N` → `{projectVersion, rows: FrameView[], hasMore}` where `FrameView` = every column of `project_frames` camelCased plus `publisherDisplayName` (via `PUBLISHER_NAME_SQL`, `FORMER_MEMBER` fallback) and `holderCount`/`freshHolderCount` (Task 4 adds; until then 0).
- Produces for later tasks:
  ```rust
  pub(crate) fn validate_file_name(name: &str) -> Result<(), ApiError>;
  pub(crate) async fn current_thresholds_version(conn: &mut PgConnection, project_id: Uuid) -> Result<i32, sqlx::Error>;
  pub(crate) fn birth_state(require_approval: bool, member: &Member) -> &'static str;
  ```
- Consumes: `dictionary::canonical_filters_tx` (Task 5 — in this task, stub it as "any non-empty string ≤ 16 chars" and replace in Task 5), `project_version::bump_project_version_tx`, `collab_auth::{require_member, record_event_tx}`.

Rules enforced by `announce`:
1. member; project `status = 'active'` else 409 "project is closed"; a device token is required (400 otherwise).
2. 1..=500 items; `frameUuid` unique within the batch (400 "duplicate frameUuid in batch").
3. `fileName`: 1..=255 chars, no `/`, `\`, NUL, not `.` or `..`, no leading/trailing whitespace (400 naming the file).
4. `blake3` 64 lowercase hex; `xxh3` 16 lowercase hex; `byteSize > 0`; `exptimeSec > 0` finite; `channel` ∈ the CHECK list (default `mono`); `meta` an object ≤ 8192 bytes serialized.
5. `gateVersion` must equal the project's current thresholds version (409 "gate version N is stale, current is M").
6. `filterCanonical` must be in the dictionary (400 "filter X is not in the project dictionary").
7. An existing `(project, frameUuid)` → 409 "frame already announced; use /version" (no partial batches: the whole POST is one transaction).
8. `state = birth_state(require_approval, member)` = `published` when `!require_approval || member.trusted_publisher || member.has_cap("data.moderate")`, else `pending`.
9. All rows get the same `manifest_version` = the bumped project version; a `project_frame_versions` row (content_version 1) per frame; one event `frames_announced {count, state}`; cache invalidated after commit.

- [ ] **Step 1: Add test helpers**

Append to `tests/common/mod.rs`:

```rust
/// One valid frame for `POST /projects/{id}/frames`. `n` seeds every unique field.
pub fn frame_body(n: u32) -> Value {
    json!({
        "frameUuid": format!("00000000-0000-4000-8000-{:012x}", n),
        "fileName": format!("c_L_{n:04}.fits"),
        "blake3": format!("{:064x}", n),
        "byteSize": 104_000_000u64,
        "xxh3": format!("{:016x}", n),
        "filterRaw": "Luminance",
        "filterCanonical": "L",
        "channel": "mono",
        "exptimeSec": 300.0,
        "dateObs": "2026-09-01T22:14:00Z",
        "gateVersion": 1,
        "meta": {"instrume": "Cam A", "pixelScaleArcsec": 1.25, "metrics": {"fwhmArcsec": 2.1, "eccentricity": 0.35, "starsDetected": 812}}
    })
}

/// Announce frames `from..to` as `token`; returns the response JSON.
pub async fn announce_frames(app: &axum::Router, token: &str, project_id: &str, from: u32, to: u32) -> (StatusCode, Value) {
    let frames: Vec<Value> = (from..to).map(frame_body).collect();
    let (status, body) = send(app, post(&format!("/api/v1/projects/{project_id}/frames"), &json!({ "frames": frames }), Some(token))).await;
    (status, if body.is_empty() { Value::Null } else { as_json(&body) })
}
```

- [ ] **Step 2: Write the failing tests**

`tests/frames.rs`:

```rust
//! Per-frame announce + manifest (collab v3 §4.2).
mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

async fn setup(pool: PgPool, require_approval: bool) -> (axum::Router, CaptureMailer, String, String, String) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", require_approval).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&app, &coord, &anna, &id, "Anna", "send").await;
    (app, mailer, coord, anna, id)
}

#[sqlx::test]
async fn announce_publishes_and_manifest_returns_rows_with_names(pool: PgPool) {
    let (app, _m, _coord, anna, id) = setup(pool, false).await;
    let (status, body) = announce_frames(&app, &anna, &id, 1, 4).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "published");
    assert_eq!(body["announced"], 3);
    let v1 = body["projectVersion"].as_i64().unwrap();

    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/manifest?since=0"), Some(&anna))).await;
    assert_eq!(status, StatusCode::OK);
    let m = as_json(&body);
    assert_eq!(m["projectVersion"].as_i64().unwrap(), v1);
    let rows = m["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["fileName"], "c_L_0001.fits");
    assert_eq!(rows[0]["publisherDisplayName"], "Anna");
    assert_eq!(rows[0]["filterCanonical"], "L");
    assert_eq!(rows[0]["accepted"], true);
    assert_eq!(rows[0]["meta"]["metrics"]["fwhmArcsec"], 2.1);
    assert_eq!(m["hasMore"], false);

    // Delta: nothing since v1.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/manifest?since={v1}"), Some(&anna))).await;
    assert_eq!(as_json(&body)["rows"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn announce_refuses_bad_names_hashes_duplicates_and_stale_gate(pool: PgPool) {
    let (app, _m, _coord, anna, id) = setup(pool, false).await;
    let cases: Vec<(serde_json::Value, StatusCode, &str)> = vec![
        ({ let mut f = frame_body(1); f["fileName"] = json!("../x.fits"); f }, StatusCode::BAD_REQUEST, "fileName"),
        ({ let mut f = frame_body(1); f["fileName"] = json!("a/b.fits"); f }, StatusCode::BAD_REQUEST, "fileName"),
        ({ let mut f = frame_body(1); f["blake3"] = json!("zz"); f }, StatusCode::BAD_REQUEST, "blake3"),
        ({ let mut f = frame_body(1); f["byteSize"] = json!(0); f }, StatusCode::BAD_REQUEST, "byteSize"),
        ({ let mut f = frame_body(1); f["filterCanonical"] = json!("Purple"); f }, StatusCode::BAD_REQUEST, "not in the project dictionary"),
        ({ let mut f = frame_body(1); f["gateVersion"] = json!(9); f }, StatusCode::CONFLICT, "stale"),
    ];
    for (frame, expected, needle) in cases {
        let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [frame]}), Some(&anna))).await;
        assert_eq!(status, expected);
        assert!(String::from_utf8_lossy(&body).contains(needle), "{}", String::from_utf8_lossy(&body));
    }
    // Duplicate within the batch.
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [frame_body(1), frame_body(1)]}), Some(&anna))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("duplicate frameUuid"));
    // Already announced.
    assert_eq!(announce_frames(&app, &anna, &id, 1, 2).await.0, StatusCode::OK);
    let (status, body) = announce_frames(&app, &anna, &id, 1, 2).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already announced"));
}

#[sqlx::test]
async fn approval_projects_park_untrusted_publishers_pending(pool: PgPool) {
    let (app, _m, coord, anna, id) = setup(pool, true).await;
    let (status, body) = announce_frames(&app, &anna, &id, 1, 3).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "pending");
    // The coordinator (data.moderate by construction) publishes at once.
    let (status, body) = announce_frames(&app, &coord, &id, 10, 11).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "published");
    // A plain member's manifest hides Anna's pending rows; Anna sees her own; the coordinator sees all.
    let (bob, _) = register_device(&app, &_m, "bob@example.com", 3, "PC").await;
    join_and_approve(&app, &coord, &bob, &id, "Bob", "send_receive").await;
    let count = |token: String| {
        let app = app.clone();
        let id = id.clone();
        async move {
            let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/manifest?since=0"), Some(&token))).await;
            as_json(&body)["rows"].as_array().unwrap().len()
        }
    };
    assert_eq!(count(bob).await, 1);
    assert_eq!(count(anna).await, 3);
    assert_eq!(count(coord).await, 3);
}

#[sqlx::test]
async fn outsiders_and_portal_sessions_cannot_announce(pool: PgPool) {
    let (app, mailer, _coord, _anna, id) = setup(pool, false).await;
    let (outsider, _) = register_device(&app, &mailer, "x@example.com", 9, "PC").await;
    assert_eq!(announce_frames(&app, &outsider, &id, 1, 2).await.0, StatusCode::FORBIDDEN);
    let session = portal_sign_in(&app, &mailer, "anna@example.com").await;
    let (status, _) = send(&app, post_cookie(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [frame_body(1)]}), &session)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a device token is required");
}
```

- [ ] **Step 3: Run to see them fail**

Run: `DATABASE_URL=… cargo test --test frames`
Expected: FAIL (404 — routes missing).

- [ ] **Step 4: Implement `src/routes/frames.rs`**

```rust
//! Per-frame announcements and the manifest (collab v3 §3.2, §4.2). A frame
//! is the unit: announce in batches, read back as a delta by project version.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::auth_mw::AuthAccount;
use crate::collab_auth::{record_event_tx, require_member, Member};
use crate::error::ApiError;
use crate::project_version::bump_project_version_tx;
use crate::routes::projects::{FORMER_MEMBER, PUBLISHER_NAME_SQL};
use crate::routes::AppState;

pub const MAX_BATCH: usize = 500;
pub const MAX_META_BYTES: usize = 8192;
pub const MANIFEST_PAGE: i64 = 1000;
const CHANNELS: [&str; 5] = ["mono", "osc", "osc-r", "osc-g", "osc-b"];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameIn {
    pub frame_uuid: Uuid,
    pub file_name: String,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
    pub filter_raw: String,
    pub filter_canonical: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    pub exptime_sec: f64,
    pub date_obs: Option<DateTime<Utc>>,
    pub gate_version: i32,
    #[serde(default = "empty_object")]
    pub meta: Value,
}
fn default_channel() -> String { "mono".into() }
fn empty_object() -> Value { Value::Object(Default::default()) }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceBatch {
    pub frames: Vec<FrameIn>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceResponse {
    pub state: &'static str,
    pub project_version: i64,
    pub announced: usize,
}

/// A landing-safe file name: one path segment, no separators, no dot names.
pub(crate) fn validate_file_name(name: &str) -> Result<(), ApiError> {
    let bad = name.is_empty()
        || name.len() > 255
        || name != name.trim()
        || name == "."
        || name == ".."
        || name.chars().any(|c| c == '/' || c == '\\' || c == '\0' || c.is_control());
    if bad {
        return Err(ApiError::bad_request(format!("fileName {name:?} is not a plain file name")));
    }
    Ok(())
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn validate_frame(f: &FrameIn) -> Result<(), ApiError> {
    validate_file_name(&f.file_name)?;
    if !is_lower_hex(&f.blake3, 64) {
        return Err(ApiError::bad_request(format!("{}: blake3 must be 64 lowercase hex chars", f.file_name)));
    }
    if !is_lower_hex(&f.xxh3, 16) {
        return Err(ApiError::bad_request(format!("{}: xxh3 must be 16 lowercase hex chars", f.file_name)));
    }
    if f.byte_size <= 0 {
        return Err(ApiError::bad_request(format!("{}: byteSize must be > 0", f.file_name)));
    }
    if !(f.exptime_sec.is_finite() && f.exptime_sec > 0.0) {
        return Err(ApiError::bad_request(format!("{}: exptimeSec must be > 0", f.file_name)));
    }
    if !CHANNELS.contains(&f.channel.as_str()) {
        return Err(ApiError::bad_request(format!("{}: channel must be one of {}", f.file_name, CHANNELS.join(", "))));
    }
    if f.filter_raw.trim().is_empty() || f.filter_raw.len() > 80 {
        return Err(ApiError::bad_request(format!("{}: filterRaw must be 1..=80 chars", f.file_name)));
    }
    if !f.meta.is_object() || serde_json::to_vec(&f.meta).map(|b| b.len()).unwrap_or(usize::MAX) > MAX_META_BYTES {
        return Err(ApiError::bad_request(format!("{}: meta must be an object of at most {MAX_META_BYTES} bytes", f.file_name)));
    }
    Ok(())
}

pub(crate) fn birth_state(require_approval: bool, member: &Member) -> &'static str {
    if !require_approval || member.trusted_publisher || member.has_cap("data.moderate") {
        "published"
    } else {
        "pending"
    }
}

pub(crate) async fn current_thresholds_version(conn: &mut PgConnection, project_id: Uuid) -> Result<i32, sqlx::Error> {
    let (v,): (i32,) = sqlx::query_as("SELECT COALESCE(MAX(version), 0) FROM project_thresholds WHERE project_id = $1")
        .bind(project_id)
        .fetch_one(conn)
        .await?;
    Ok(v)
}

#[tracing::instrument(skip_all)]
pub async fn announce(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
    Json(body): Json<AnnounceBatch>,
) -> Result<Json<AnnounceResponse>, ApiError> {
    let Some(device_id) = auth.device_id else {
        return Err(ApiError::bad_request("a device token is required to announce frames"));
    };
    let member = require_member(&state.db, id, auth.account_id).await?;
    if body.frames.is_empty() || body.frames.len() > MAX_BATCH {
        return Err(ApiError::bad_request(format!("frames must contain 1..={MAX_BATCH} items")));
    }
    let mut seen = std::collections::HashSet::with_capacity(body.frames.len());
    for f in &body.frames {
        validate_frame(f)?;
        if !seen.insert(f.frame_uuid) {
            return Err(ApiError::bad_request(format!("duplicate frameUuid in batch: {}", f.frame_uuid)));
        }
    }

    let mut tx = state.db.begin().await?;
    let (status, require_approval): (String, bool) =
        sqlx::query_as("SELECT status, require_approval FROM projects WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::not_found("project not found"))?;
    if status != "active" {
        return Err(ApiError::conflict("project is closed"));
    }
    let gate = current_thresholds_version(&mut tx, id).await?;
    let canonical = crate::routes::dictionary::canonical_filters_tx(&mut tx, id).await?;
    for f in &body.frames {
        if f.gate_version != gate {
            return Err(ApiError::conflict(format!("gate version {} is stale, current is {gate}", f.gate_version)));
        }
        if !canonical.iter().any(|c| c == &f.filter_canonical) {
            return Err(ApiError::bad_request(format!("{}: filter {:?} is not in the project dictionary", f.file_name, f.filter_canonical)));
        }
    }
    let uuids: Vec<Uuid> = body.frames.iter().map(|f| f.frame_uuid).collect();
    let existing: Vec<(Uuid,)> = sqlx::query_as("SELECT frame_uuid FROM project_frames WHERE project_id = $1 AND frame_uuid = ANY($2)")
        .bind(id)
        .bind(&uuids)
        .fetch_all(&mut *tx)
        .await?;
    if let Some((dup,)) = existing.first() {
        return Err(ApiError::conflict(format!("frame {dup} already announced; use /version to publish new content")));
    }

    let state_str = birth_state(require_approval, &member);
    let version = bump_project_version_tx(&mut tx, id).await?;
    for f in &body.frames {
        sqlx::query(
            "INSERT INTO project_frames (project_id, frame_uuid, publisher, publisher_device_id, file_name, blake3, byte_size, xxh3, \
                 filter_raw, filter_canonical, channel, exptime_sec, date_obs, meta, gate_version, state, manifest_version) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(id).bind(f.frame_uuid).bind(auth.account_id).bind(device_id).bind(&f.file_name).bind(&f.blake3)
        .bind(f.byte_size).bind(&f.xxh3).bind(f.filter_raw.trim()).bind(&f.filter_canonical).bind(&f.channel)
        .bind(f.exptime_sec).bind(f.date_obs).bind(&f.meta).bind(f.gate_version).bind(state_str).bind(version)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO project_frame_versions (project_id, frame_uuid, content_version, blake3, byte_size, xxh3) VALUES ($1,$2,1,$3,$4,$5)")
            .bind(id).bind(f.frame_uuid).bind(&f.blake3).bind(f.byte_size).bind(&f.xxh3)
            .execute(&mut *tx)
            .await?;
        // The publisher's own device holds the frame from the first moment.
        sqlx::query("INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version) VALUES ($1,$2,$3,1)")
            .bind(id).bind(f.frame_uuid).bind(device_id)
            .execute(&mut *tx)
            .await?;
    }
    record_event_tx(&mut tx, id, "frames_announced", Some(auth.account_id), None,
        serde_json::json!({ "count": body.frames.len(), "state": state_str })).await?;
    tx.commit().await?;
    state.versions.invalidate(id);
    tracing::info!(project_id = %id, count = body.frames.len(), state = state_str, "frames announced");
    Ok(Json(AnnounceResponse { state: state_str, project_version: version, announced: body.frames.len() }))
}

// ---- GET /api/v1/projects/{id}/manifest?since=V&limit=N ----------------------

#[derive(Deserialize)]
pub struct ManifestQuery {
    #[serde(default)]
    pub since: i64,
    pub limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub struct FrameRow {
    pub frame_uuid: Uuid,
    pub publisher: Uuid,
    pub publisher_display_name: Option<String>,
    pub file_name: String,
    pub content_version: i32,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
    pub filter_raw: String,
    pub filter_canonical: String,
    pub channel: String,
    pub exptime_sec: f64,
    pub date_obs: Option<DateTime<Utc>>,
    pub meta: Value,
    pub gate_version: i32,
    pub accepted: bool,
    pub accepted_reason: Option<String>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub manifest_version: i64,
    pub created_at: DateTime<Utc>,
    pub holder_count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameView {
    pub frame_uuid: Uuid,
    pub publisher_account_id: Uuid,
    pub publisher_display_name: String,
    pub own: bool,
    pub file_name: String,
    pub content_version: i32,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
    pub filter_raw: String,
    pub filter_canonical: String,
    pub channel: String,
    pub exptime_sec: f64,
    pub date_obs: Option<DateTime<Utc>>,
    pub meta: Value,
    pub gate_version: i32,
    pub accepted: bool,
    pub accepted_reason: Option<String>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub manifest_version: i64,
    pub created_at: DateTime<Utc>,
    pub holder_count: i64,
}

impl FrameRow {
    pub fn into_view(self, viewer: Uuid) -> FrameView {
        FrameView {
            own: self.publisher == viewer,
            publisher_display_name: self.publisher_display_name.unwrap_or_else(|| FORMER_MEMBER.to_string()),
            frame_uuid: self.frame_uuid, publisher_account_id: self.publisher, file_name: self.file_name,
            content_version: self.content_version, blake3: self.blake3, byte_size: self.byte_size, xxh3: self.xxh3,
            filter_raw: self.filter_raw, filter_canonical: self.filter_canonical, channel: self.channel,
            exptime_sec: self.exptime_sec, date_obs: self.date_obs, meta: self.meta, gate_version: self.gate_version,
            accepted: self.accepted, accepted_reason: self.accepted_reason, state: self.state,
            reject_reason: self.reject_reason, manifest_version: self.manifest_version, created_at: self.created_at,
            holder_count: self.holder_count,
        }
    }
}

/// The columns + joins every frame read shares. `$1` project, `$2` viewer
/// account, `$3` viewer-is-moderator. Visibility: published rows to every
/// member, pending/rejected rows only to their publisher and moderators.
pub(crate) fn frame_select_sql(extra_where: &str, order_limit: &str) -> String {
    format!(
        "SELECT f.frame_uuid, f.publisher, {PUBLISHER_NAME_SQL} AS publisher_display_name, f.file_name, f.content_version, \
                f.blake3, f.byte_size, f.xxh3, f.filter_raw, f.filter_canonical, f.channel, f.exptime_sec, f.date_obs, f.meta, \
                f.gate_version, f.accepted, f.accepted_reason, f.state, f.reject_reason, f.manifest_version, f.created_at, \
                (SELECT count(*) FROM frame_holders h WHERE h.project_id = f.project_id AND h.frame_uuid = f.frame_uuid \
                    AND {fresh}) AS holder_count \
         FROM project_frames f \
         LEFT JOIN project_members pm ON pm.project_id = f.project_id AND pm.account_id = f.publisher \
         LEFT JOIN project_alumni al ON al.project_id = f.project_id AND al.account_id = f.publisher \
         WHERE f.project_id = $1 AND (f.state = 'published' OR f.publisher = $2 OR $3) {extra_where} {order_limit}",
        fresh = crate::routes::holders::HOLDER_FRESH_SQL
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestResponse {
    pub project_version: i64,
    pub rows: Vec<FrameView>,
    pub has_more: bool,
}

#[tracing::instrument(skip_all)]
pub async fn manifest(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
    Query(q): Query<ManifestQuery>,
) -> Result<Json<ManifestResponse>, ApiError> {
    let member = require_member(&state.db, id, auth.account_id).await?;
    let moderator = member.has_cap("data.moderate");
    let limit = q.limit.unwrap_or(MANIFEST_PAGE).clamp(1, MANIFEST_PAGE);
    let (project_version,): (i64,) = sqlx::query_as("SELECT version FROM projects WHERE id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let sql = frame_select_sql("AND f.manifest_version > $4", "ORDER BY f.manifest_version, f.frame_uuid LIMIT $5");
    let rows = sqlx::query_as::<_, FrameRow>(&sql)
        .bind(id).bind(auth.account_id).bind(moderator).bind(q.since).bind(limit + 1)
        .fetch_all(&state.db)
        .await?;
    let has_more = rows.len() as i64 > limit;
    let rows = rows.into_iter().take(limit as usize).map(|r| r.into_view(auth.account_id)).collect();
    Ok(Json(ManifestResponse { project_version, rows, has_more }))
}
```

Until Task 4 exists, create `src/routes/holders.rs` with only the constant so this compiles:

```rust
pub(crate) const HOLDER_FRESH_SQL: &str = "h.reported_at > now() - interval '75 minutes'";
```

and until Task 5, create `src/routes/dictionary.rs` with a stub:

```rust
use sqlx::PgConnection;
use uuid::Uuid;
/// Replaced in Task 5 by the stored dictionary. Wave-1 default vocabulary.
pub(crate) async fn canonical_filters_tx(_conn: &mut PgConnection, _project_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    Ok(["L", "R", "G", "B", "Ha", "OIII", "SII"].iter().map(|s| s.to_string()).collect())
}
```

Routes in `mod.rs` (inside `account_protected`):

```rust
        .route("/api/v1/projects/{id}/frames", post(frames::announce))
        .route("/api/v1/projects/{id}/manifest", get(frames::manifest))
```

and `pub mod frames; pub mod holders; pub mod dictionary;`.

- [ ] **Step 5: Run the tests**

Run: `DATABASE_URL=… cargo test --test frames`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add src/routes/frames.rs src/routes/holders.rs src/routes/dictionary.rs src/routes/mod.rs tests/frames.rs tests/common/mod.rs
git commit -m "feat(hub): per-frame announce (batch, validated, trust-aware birth state) and manifest delta by project version"
```

---

### Task 3: Retire the package model — delete `announcements.rs`, compat 409, re-base consumers

**Files:**
- Delete: `src/routes/announcements.rs`, `tests/announcements.rs`, `tests/have_reports.rs`, `tests/have_soft_state.rs`
- Create: `src/routes/compat.rs`, `tests/compat.rs`
- Modify: `src/routes/mod.rs` (drop `pub mod announcements`, the six routes at l.226-245; add compat routes), `src/routes/projects.rs` (l.18 import, l.640-690 types, l.727 field, l.894-1001 queries, l.1084-1097 mapping), `src/routes/me.rs:48-80`, `src/routes/stats.rs:55-90`, `src/routes/profiles.rs` (l.26, 142, 314-384, 444-541, 716), `src/routes/operator.rs` (l.149-240), `src/scheduler.rs:243-337`, `src/mail.rs:53-201, 414-444`
- Modify tests: `alumni.rs`, `account_auth.rs`, `collab_flow.rs`, `hardening.rs`, `governance.rs`, `mail_and_scheduler.rs`, `me.rs`, `profiles.rs`, `stats.rs` — replace every announce/have call with `announce_frames`/holder PUT (Task 4) and adjust assertions to frames.

**Interfaces:**
- `ProjectPage` loses `packages` and gains `coverage: CoverageView` and `canonical_grid: Option<Value>`, keeps `progress` (now from frames):
  ```rust
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct CoverageFilter { filter: String, kind: String, frames: i64, seconds: f64, publishers: i64, goal_seconds: Option<f64> }
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct CoverageView { frames: i64, bytes: i64, by_filter: Vec<CoverageFilter>, single_holder_frames: i64, well_replicated_frames: i64 /* ≥3 fresh holders */, online_holders: i64 }
  ```
  Published AND accepted frames only. `kind` comes from the dictionary (Task 5; until then `"broadband"` for L/R/G/B else `"narrowband"`). `goal_seconds` from `projects.goals[filter]` when numeric.
- `MyProjectView.pending_announcements` → `pending_frames` (wire `pendingFrames`): `count(*) FROM project_frames WHERE state='pending'` gated the same way.
- `stats.rs`: `hours_pooled = SUM(exptime_sec)/3600`, `bytes_published = SUM(byte_size)` over `state='published' AND accepted` frames of non-hidden projects.
- `profiles.rs`: `packages_accepted` → `frames_accepted` (published frames by this publisher, non-hidden projects); `package_seconds` → `SUM(exptime_sec)`; candidate history `accepted`/`declined` = published/rejected frame counts; `holding` = distinct frames with a fresh holder row on any of the account's devices.
- `operator.rs`: `announcements_by_state` → `frames_by_state` (same shape, keyed by state); `announcement_count` → `frame_count`.
- `scheduler.rs`/`mail.rs`: `pending_announcements` → `pending_frames` and `pending_publishers` (distinct publishers with a pending frame); digest line "N frames from M contributors pending".
- `compat.rs`: `pub async fn retired() -> ApiError` returning `ApiError::Message(StatusCode::CONFLICT, "collab_api_outdated")` for: `POST|GET /projects/{id}/announcements`, `POST /announcements/{id}/approve|reject|have`, `PUT /projects/{id}/have`.

- [ ] **Step 1: Write the compat test and the re-based consumer tests first**

`tests/compat.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn retired_package_routes_answer_409_outdated(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let zero = "00000000-0000-0000-0000-000000000000";
    for req in [
        post(&format!("/api/v1/projects/{id}/announcements"), &json!({}), Some(&coord)),
        get(&format!("/api/v1/projects/{id}/announcements"), Some(&coord)),
        post(&format!("/api/v1/announcements/{zero}/approve"), &json!({}), Some(&coord)),
        post(&format!("/api/v1/announcements/{zero}/reject"), &json!({}), Some(&coord)),
        post(&format!("/api/v1/announcements/{zero}/have"), &json!({}), Some(&coord)),
        put(&format!("/api/v1/projects/{id}/have"), &json!({"packageIds": []}), Some(&coord)),
    ] {
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(as_json(&body)["error"], "collab_api_outdated");
    }
}
```

Re-based assertions to add (one per consumer; put them in the existing test files next to the tests they replace):

`tests/me.rs` — replace the pending-announcement test with:

```rust
#[sqlx::test]
async fn pending_frames_count_is_moderator_only(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    assert_eq!(announce_frames(&app, &anna, id, 1, 4).await.0, StatusCode::OK);
    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&coord))).await;
    assert_eq!(as_json(&body)[0]["pendingFrames"], 3);
    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&anna))).await;
    assert_eq!(as_json(&body)[0]["pendingFrames"], 0);
}
```

`tests/stats.rs` — replace the pooled-hours test with one that announces 3 frames of 300 s (`hoursPooled == 0.3` after rounding to one decimal → assert `0.2 <= h <= 0.3` if the route rounds; read the route) and `bytesPublished == 3 * 104_000_000`.

`tests/profiles.rs` — `packagesAccepted` → `framesAccepted`; `holding` assertions use the holder PUT from Task 4 (write them now against `PUT /projects/{id}/holders/self` with `{"full": true, "add": [{"frameUuid": …, "contentVersion": 1}], "remove": []}`; they turn green after Task 4).

`tests/mail_and_scheduler.rs` — digest expectation text becomes `"3 frames from 1 contributor pending"`.

`tests/collab_flow.rs`, `governance.rs`, `hardening.rs`, `alumni.rs`, `account_auth.rs` — mechanical: `announce_body(n)` → `frame_body(n)`, `/announcements` → `/frames`, approve/reject → Task 6's `/frames/{uuid}/approve|reject` (write against the new paths now), have → Task 4's PUT.

- [ ] **Step 2: Run to see the build break where expected**

Run: `DATABASE_URL=… cargo test --no-run`
Expected: compile errors in the test files referencing removed helpers — the map of what to fix.

- [ ] **Step 3: Delete and rewrite**

1. `git rm src/routes/announcements.rs tests/announcements.rs tests/have_reports.rs tests/have_soft_state.rs`.
2. `src/routes/compat.rs`:

```rust
//! Routes retired by collab v3 (spec §11). An app built for the package model
//! gets one stable answer and shows its "update required" state.
use axum::http::StatusCode;
use crate::error::ApiError;

pub async fn retired() -> Result<(), ApiError> {
    Err(ApiError::Message(StatusCode::CONFLICT, "collab_api_outdated".into()))
}
```

In `mod.rs` replace the six announcement routes with:

```rust
        .route("/api/v1/projects/{id}/announcements", post(compat::retired).get(compat::retired))
        .route("/api/v1/announcements/{id}/approve", post(compat::retired))
        .route("/api/v1/announcements/{id}/reject", post(compat::retired))
        .route("/api/v1/announcements/{id}/have", post(compat::retired))
        .route("/api/v1/projects/{id}/have", axum::routing::put(compat::retired))
```

3. `projects.rs`:
   - Remove the l.18 import; remove `PackageRow`/`PackagePublicView`; keep `MemberProgress`/`ProjectProgress`.
   - `ProjectPage`: replace `packages: Vec<PackagePublicView>` with `coverage: CoverageView` and add `canonical_grid: Option<Value>` (read from `PROJECT_COLUMNS` — add `version` and `canonical_grid` there and to `ProjectRow`/`ProjectView` as `version: i64`, `canonical_grid: Option<Value>`).
   - Coverage query (replaces l.894-917):

```rust
#[derive(sqlx::FromRow)]
struct CoverageRow { filter_canonical: String, frames: i64, seconds: f64, publishers: i64 }
let by_filter = sqlx::query_as::<_, CoverageRow>(
    "SELECT filter_canonical, count(*) AS frames, COALESCE(SUM(exptime_sec), 0)::double precision AS seconds, \
            count(DISTINCT publisher) AS publishers \
     FROM project_frames WHERE project_id = $1 AND state = 'published' AND accepted \
     GROUP BY filter_canonical ORDER BY filter_canonical",
).bind(project.id).fetch_all(&state.db).await?;
let (frames, bytes): (i64, i64) = sqlx::query_as(
    "SELECT count(*), COALESCE(SUM(byte_size), 0)::bigint FROM project_frames \
     WHERE project_id = $1 AND state = 'published' AND accepted",
).bind(project.id).fetch_one(&state.db).await?;
let (single, well, online): (i64, i64, i64) = sqlx::query_as(&format!(
    "WITH h AS (SELECT f.frame_uuid, \
                 count(*) FILTER (WHERE {fresh}) AS fresh, \
                 count(*) FILTER (WHERE {fresh} AND {online}) AS online \
                FROM project_frames f \
                LEFT JOIN frame_holders h ON h.project_id = f.project_id AND h.frame_uuid = f.frame_uuid \
                LEFT JOIN devices d ON d.id = h.device_id AND d.revoked_at IS NULL \
                WHERE f.project_id = $1 AND f.state = 'published' AND f.accepted \
                GROUP BY f.frame_uuid) \
     SELECT count(*) FILTER (WHERE fresh <= 1), count(*) FILTER (WHERE fresh >= 3), COALESCE(MAX(online), 0) FROM h",
    fresh = crate::routes::holders::HOLDER_FRESH_SQL, online = crate::routes::holders::HOLDER_ONLINE_SQL,
)).bind(project.id).fetch_one(&state.db).await?;
```

   `HOLDER_ONLINE_SQL` = `"d.last_seen_at > now() - interval '5 minutes'"` (add to `holders.rs` now). Goal seconds: `project.goals` is `Option<Value>`; `goal_seconds = goals.get(filter).and_then(Value::as_f64)`. Kind: until Task 5, `if ["L","R","G","B"].contains(filter) {"broadband"} else {"narrowband"}` — Task 5 replaces this with the dictionary lookup.
   - Progress query (replaces l.919-1001):

```sql
SELECT f.publisher AS account_id, {PUBLISHER_NAME_SQL} AS display_name, pm.account_id IS NULL AS former,
       count(*) AS frames, COALESCE(SUM(f.exptime_sec), 0)::double precision AS integration_seconds
FROM project_frames f
LEFT JOIN project_members pm ON pm.project_id = f.project_id AND pm.account_id = f.publisher
LEFT JOIN project_alumni al ON al.project_id = f.project_id AND al.account_id = f.publisher
WHERE f.project_id = $1 AND f.state = 'published' AND f.accepted
GROUP BY f.publisher, pm.display_name, al.display_name, al.credited, pm.account_id
```

   `integration_seconds_by_filter` = the coverage `by_filter` folded into the BTreeMap; `total_frames` = `frames`. Keep the existing sort (lowercased name, then id) and the `FORMER_MEMBER` fallback.
4. `me.rs`: rename the field to `pending_frames` and the subquery to `SELECT count(*) FROM project_frames f WHERE f.project_id = p.id AND f.state = 'pending' AND (pm.is_coordinator OR 'data.moderate' = ANY(pm.gov_caps))`.
5. `stats.rs`: `seconds_pooled` = `SELECT COALESCE(SUM(f.exptime_sec), 0) FROM project_frames f JOIN projects p ON p.id = f.project_id WHERE f.state = 'published' AND f.accepted AND p.hidden_at IS NULL`; `bytes_published` = the same with `SUM(f.byte_size)::bigint`. Update the doc comment.
6. `profiles.rs`: replace every `package_announcements` query with the `project_frames` equivalent (`publisher`, `state`, `exptime_sec`, `byte_size`); `holding_count` = `SELECT count(DISTINCT (h.project_id, h.frame_uuid)) FROM frame_holders h JOIN devices d ON d.id = h.device_id WHERE d.account_id = $1 AND d.revoked_at IS NULL AND {HOLDER_FRESH_SQL}`; rename the wire field `packagesAccepted` → `framesAccepted`.
7. `operator.rs`: `frames_by_state` = `SELECT state, count(*) FROM project_frames GROUP BY state`; `frame_count` per project.
8. `scheduler.rs` + `mail.rs`: `pending_frames`/`pending_publishers` = `SELECT project_id, count(*), count(DISTINCT publisher) FROM project_frames WHERE state = 'pending' GROUP BY project_id`; digest text `"{n} frame{s} from {m} contributor{s} pending"`; update the unit tests at `mail.rs:414-444`.
9. `join_requests.rs` candidate card: it calls `profiles::candidate_history` — the field names inside stay (`accepted`, `declined`, `holding`), only the SQL under them changed.

- [ ] **Step 4: Run the whole suite**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -20`
Expected: everything green except tests that target Task 4–6 routes (holders PUT, approve/reject, dictionary, trust, grid), which fail with 404 — list them in the commit message body.

- [ ] **Step 5: Commit**

```bash
git add -A src tests
git commit -m "refactor(hub): package model removed — frames feed the project page (coverage), /me, stats, profiles, operator and the digest; retired routes answer 409 collab_api_outdated"
```

---

### Task 4: Holders — delta report and per-frame holders

**Files:**
- Modify: `src/routes/holders.rs` (full module)
- Modify: `src/routes/mod.rs` (two routes)
- Test: `tests/holders.rs`

**Interfaces:**
- `PUT /projects/{id}/holders/self` body `{full: bool, add: [{frameUuid, contentVersion}], remove: [frameUuid]}` → 204. Device token required. Permission per frame (never a 403 for the whole call): a `send_receive` member or a moderator may hold any `published` frame (a moderator also `pending`); a `send` member may hold only frames it published. Rows outside the permission are skipped with a `warn!` and counted in the log. `full = true` deletes every other holder row of this device in the project after the upsert. `add`/`remove` ≤ 10 000 each. Refreshes `reported_at` on conflict.
- `GET /projects/{id}/frames/{uuid}/holders` → `[{pubkey, displayName, lastSeenAt, relayUrl}]` — fresh holders, non-revoked devices, current members only, relay URL only (S1). Visible to members for published frames; pending frames only to publisher/moderators (404 otherwise — never reveal existence).
- Produces `pub(crate) const HOLDER_FRESH_SQL`, `HOLDER_ONLINE_SQL` (already added), and `pub(crate) fn fresh_holders_of_device_sql()` if useful for profiles.

- [ ] **Step 1: Write the failing tests**

`tests/holders.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn uuid_n(n: u32) -> String { format!("00000000-0000-4000-8000-{n:012x}") }

#[sqlx::test]
async fn holder_delta_add_remove_and_full_resync(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    join_and_approve(&app, &coord, &bob, id, "Bob", "send_receive").await;
    assert_eq!(announce_frames(&app, &anna, id, 1, 4).await.0, StatusCode::OK);

    // Anna's device is a holder from the announce; Bob adds two.
    let holders = |uuid: String, token: String| { let app = app.clone(); let id = id.to_string(); async move {
        let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/frames/{uuid}/holders"), Some(&token))).await;
        assert_eq!(status, StatusCode::OK);
        as_json(&body).as_array().unwrap().len()
    }};
    assert_eq!(holders(uuid_n(1), bob.clone()).await, 1);
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &json!({
        "full": false, "add": [{"frameUuid": uuid_n(1), "contentVersion": 1}, {"frameUuid": uuid_n(2), "contentVersion": 1}], "remove": []
    }), Some(&bob))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(holders(uuid_n(1), anna.clone()).await, 2);
    assert_eq!(holders(uuid_n(3), anna.clone()).await, 1);

    // Remove one.
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &json!({"full": false, "add": [], "remove": [uuid_n(1)]}), Some(&bob))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(holders(uuid_n(1), anna.clone()).await, 1);

    // Full resync with only frame 3 drops frame 2.
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &json!({"full": true, "add": [{"frameUuid": uuid_n(3), "contentVersion": 1}], "remove": []}), Some(&bob))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(holders(uuid_n(2), anna.clone()).await, 1);
    assert_eq!(holders(uuid_n(3), anna.clone()).await, 2);

    // Holder rows carry relay url only, never direct addresses.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/frames/{}/holders", uuid_n(3)), Some(&anna))).await;
    let h = as_json(&body);
    assert!(h[0].get("directAddrs").is_none());
    assert!(h[0].get("relayUrl").is_some());
}

#[sqlx::test]
async fn send_members_hold_only_their_own_frames_and_sessions_are_refused(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    assert_eq!(announce_frames(&app, &coord, id, 1, 2).await.0, StatusCode::OK);
    // Anna (send) claims the coordinator's frame: 204 but no row.
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &json!({"full": false, "add": [{"frameUuid": uuid_n(1), "contentVersion": 1}], "remove": []}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/frames/{}/holders", uuid_n(1)), Some(&coord))).await;
    assert_eq!(as_json(&body).as_array().unwrap().len(), 1);
    // A portal session has no device.
    let session = portal_sign_in(&app, &mailer, "anna@example.com").await;
    let (status, _) = send(&app, common::put_cookie(&format!("/api/v1/projects/{id}/holders/self"), &json!({"full": false, "add": [], "remove": []}), &session)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
```

Add `put_cookie` to `tests/common/mod.rs` mirroring `patch_cookie` with method PUT.

- [ ] **Step 2: Run to see them fail**

Run: `DATABASE_URL=… cargo test --test holders`
Expected: FAIL (404).

- [ ] **Step 3: Implement `src/routes/holders.rs`**

```rust
//! Who holds which frame (collab v3 §3.1 Holder, §4.2). Reported by devices
//! as deltas; fresh within 75 minutes; relay URL is the only address that
//! crosses accounts (rule S1).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth_mw::AuthAccount;
use crate::collab_auth::require_member;
use crate::error::ApiError;
use crate::routes::AppState;

pub(crate) const HOLDER_FRESH_SQL: &str = "h.reported_at > now() - interval '75 minutes'";
pub(crate) const HOLDER_ONLINE_SQL: &str = "d.last_seen_at > now() - interval '5 minutes'";
const MAX_DELTA: usize = 10_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldFrame { pub frame_uuid: Uuid, pub content_version: i32 }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersSelf {
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub add: Vec<HeldFrame>,
    #[serde(default)]
    pub remove: Vec<Uuid>,
}

#[tracing::instrument(skip_all)]
pub async fn put_holders_self(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
    Json(body): Json<HoldersSelf>,
) -> Result<StatusCode, ApiError> {
    let Some(device_id) = auth.device_id else {
        return Err(ApiError::bad_request("a device token is required to report holdings"));
    };
    let member = require_member(&state.db, id, auth.account_id).await?;
    if body.add.len() > MAX_DELTA || body.remove.len() > MAX_DELTA {
        return Err(ApiError::bad_request(format!("add/remove must each contain at most {MAX_DELTA} items")));
    }
    let any_frame = member.data_role == "send_receive" || member.has_cap("data.moderate");
    let moderator = member.has_cap("data.moderate");

    let mut tx = state.db.begin().await?;
    let add_uuids: Vec<Uuid> = body.add.iter().map(|f| f.frame_uuid).collect();
    let add_versions: Vec<i32> = body.add.iter().map(|f| f.content_version).collect();
    // Permission is applied INSIDE the upsert: rows the device may not hold
    // are simply not written.
    let written = sqlx::query(
        "INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version) \
         SELECT f.project_id, f.frame_uuid, $2, a.v \
         FROM unnest($3::uuid[], $4::int[]) AS a(u, v) \
         JOIN project_frames f ON f.project_id = $1 AND f.frame_uuid = a.u \
         WHERE (f.state = 'published' OR (f.state = 'pending' AND $6)) \
           AND ($5 OR f.publisher = $7) \
         ON CONFLICT (project_id, frame_uuid, device_id) \
         DO UPDATE SET reported_at = now(), content_version = EXCLUDED.content_version",
    )
    .bind(id).bind(device_id).bind(&add_uuids).bind(&add_versions).bind(any_frame).bind(moderator).bind(auth.account_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if written as usize != body.add.len() {
        tracing::warn!(project_id = %id, requested = body.add.len(), written, "holder report: some frames skipped (unknown or not holdable by this member)");
    }
    if body.full {
        sqlx::query("DELETE FROM frame_holders WHERE project_id = $1 AND device_id = $2 AND NOT (frame_uuid = ANY($3))")
            .bind(id).bind(device_id).bind(&add_uuids)
            .execute(&mut *tx)
            .await?;
    } else if !body.remove.is_empty() {
        sqlx::query("DELETE FROM frame_holders WHERE project_id = $1 AND device_id = $2 AND frame_uuid = ANY($3)")
            .bind(id).bind(device_id).bind(&body.remove)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    tracing::debug!(project_id = %id, added = written, removed = body.remove.len(), full = body.full, "holders reported");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct HolderView {
    pub pubkey: String,
    pub display_name: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub relay_url: Option<String>,
}

#[tracing::instrument(skip_all)]
pub async fn frame_holders(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path((id, frame_uuid)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<HolderView>>, ApiError> {
    let member = require_member(&state.db, id, auth.account_id).await?;
    let moderator = member.has_cap("data.moderate");
    let visible: Option<(bool,)> = sqlx::query_as(
        "SELECT (state = 'published' OR publisher = $3 OR $4) FROM project_frames WHERE project_id = $1 AND frame_uuid = $2",
    )
    .bind(id).bind(frame_uuid).bind(auth.account_id).bind(moderator)
    .fetch_optional(&state.db)
    .await?;
    if !matches!(visible, Some((true,))) {
        return Err(ApiError::not_found("frame not found"));
    }
    #[derive(sqlx::FromRow)]
    struct Row { pubkey: Vec<u8>, display_name: String, last_seen_at: Option<DateTime<Utc>>, relay_url: Option<String> }
    let rows = sqlx::query_as::<_, Row>(&format!(
        "SELECT d.pubkey, pm.display_name, d.last_seen_at, d.endpoint_addr->>'homeRelayUrl' AS relay_url \
         FROM frame_holders h \
         JOIN devices d ON d.id = h.device_id AND d.revoked_at IS NULL \
         JOIN project_members pm ON pm.project_id = h.project_id AND pm.account_id = d.account_id \
         WHERE h.project_id = $1 AND h.frame_uuid = $2 AND {HOLDER_FRESH_SQL} \
         ORDER BY d.last_seen_at DESC NULLS LAST",
    ))
    .bind(id).bind(frame_uuid)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(|r| HolderView {
        pubkey: crate::security::encode_pubkey(&r.pubkey),
        display_name: r.display_name,
        last_seen_at: r.last_seen_at,
        relay_url: r.relay_url,
    }).collect()))
}
```

(`security::encode_pubkey` is what the old `HolderView` used — keep the same encoding.) Routes:

```rust
        .route("/api/v1/projects/{id}/holders/self", axum::routing::put(holders::put_holders_self))
        .route("/api/v1/projects/{id}/frames/{frame_uuid}/holders", get(holders::frame_holders))
```

- [ ] **Step 4: Run the tests**

Run: `DATABASE_URL=… cargo test --test holders --test profiles --test frames`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes/holders.rs src/routes/mod.rs tests/holders.rs tests/common/mod.rs
git commit -m "feat(hub): per-frame holders — device delta report with per-frame permission, fresh holders with relay url only"
```

---

### Task 5: Filter dictionary

**Files:**
- Modify: `src/routes/dictionary.rs` (replace the stub), `src/routes/mod.rs` (routes), `src/routes/projects.rs` (`create_project_core` seeds v1; coverage `kind` from the dictionary)
- Test: `tests/dictionary.rs`

**Interfaces:**
- Entry: `{canonical: string, aliases: string[], kind: "broadband"|"narrowband"|"luminance"}`.
- `DEFAULT_DICTIONARY`: L (luminance; aliases `lum, luminance, clear, l`), R (broadband; `red, r`), G (`green, g`), B (`blue, b`), Ha (narrowband; `h-alpha, halpha, h_alpha, hα, ha`), OIII (`o3, oiii, o-iii`), SII (`s2, sii, s-ii`).
- `GET /projects/{id}/dictionary` (member) → `{current: {version, entries, createdAt}, history: [...]}`.
- `PUT /projects/{id}/dictionary` (`thresholds.edit`) body `{entries}` → `{version}`; validation: 1..=50 entries; `canonical` matches `^[A-Za-z0-9][A-Za-z0-9_-]{0,15}$`, unique case-insensitively; aliases ≤ 20 per entry, each 1..=40 chars, unique across the whole dictionary case-insensitively; kind ∈ the three. Bumps the project version; event `dictionary_updated {version}`.
- `pub(crate) async fn canonical_filters_tx(conn, project_id) -> Result<Vec<String>, sqlx::Error>` reads the latest version's canonicals (empty vec if none — which `announce` then refuses with "project has no filter dictionary").
- `pub(crate) async fn filter_kinds(db, project_id) -> Result<HashMap<String, String>, sqlx::Error>` for the coverage view.

- [ ] **Step 1: Write the failing tests**

`tests/dictionary.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn new_project_has_the_default_dictionary_and_announce_respects_it(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    let d = as_json(&body);
    assert_eq!(d["current"]["version"], 1);
    let canon: Vec<&str> = d["current"]["entries"].as_array().unwrap().iter().map(|e| e["canonical"].as_str().unwrap()).collect();
    assert_eq!(canon, ["L", "R", "G", "B", "Ha", "OIII", "SII"]);

    // v2 adds Hb; announce with Hb now passes, with Purple still fails.
    let mut entries = d["current"]["entries"].as_array().unwrap().clone();
    entries.push(json!({"canonical": "Hb", "aliases": ["h-beta"], "kind": "narrowband"}));
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["version"], 2);
    let mut f = frame_body(1); f["filterCanonical"] = json!("Hb");
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [f]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test]
async fn dictionary_validation_and_cap(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": [{"canonical": "L", "aliases": [], "kind": "luminance"}]}), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for (entries, needle) in [
        (json!([]), "1..=50"),
        (json!([{"canonical": "L", "aliases": [], "kind": "luminance"}, {"canonical": "l", "aliases": [], "kind": "luminance"}]), "duplicate canonical"),
        (json!([{"canonical": "L", "aliases": ["x"], "kind": "luminance"}, {"canonical": "R", "aliases": ["X"], "kind": "broadband"}]), "duplicate alias"),
        (json!([{"canonical": "bad name", "aliases": [], "kind": "luminance"}]), "canonical"),
        (json!([{"canonical": "L", "aliases": [], "kind": "weird"}]), "kind"),
    ] {
        let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&coord))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&body).contains(needle), "{}", String::from_utf8_lossy(&body));
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `DATABASE_URL=… cargo test --test dictionary`
Expected: FAIL (404 on GET).

- [ ] **Step 3: Implement**

`src/routes/dictionary.rs` (replacing the stub):

```rust
//! The project's canonical filter vocabulary (collab v3 §6.2, R8). Versioned
//! like thresholds; announce refuses a frame whose canonical filter is not in
//! the current version.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::auth_mw::AuthAccount;
use crate::collab_auth::{record_event_tx, require_cap, require_member};
use crate::error::ApiError;
use crate::project_version::bump_project_version_tx;
use crate::routes::AppState;

const KINDS: [&str; 3] = ["broadband", "narrowband", "luminance"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DictEntry {
    pub canonical: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub kind: String,
}

pub fn default_dictionary() -> Vec<DictEntry> {
    let e = |c: &str, a: &[&str], k: &str| DictEntry { canonical: c.into(), aliases: a.iter().map(|s| s.to_string()).collect(), kind: k.into() };
    vec![
        e("L", &["lum", "luminance", "clear", "l"], "luminance"),
        e("R", &["red", "r"], "broadband"),
        e("G", &["green", "g"], "broadband"),
        e("B", &["blue", "b"], "broadband"),
        e("Ha", &["h-alpha", "halpha", "h_alpha", "hα", "ha"], "narrowband"),
        e("OIII", &["o3", "oiii", "o-iii"], "narrowband"),
        e("SII", &["s2", "sii", "s-ii"], "narrowband"),
    ]
}

pub(crate) fn validate_dictionary(entries: &[DictEntry]) -> Result<(), ApiError> {
    if entries.is_empty() || entries.len() > 50 {
        return Err(ApiError::bad_request("entries must contain 1..=50 items"));
    }
    let mut canon = std::collections::HashSet::new();
    let mut aliases = std::collections::HashSet::new();
    for e in entries {
        let ok = !e.canonical.is_empty()
            && e.canonical.len() <= 16
            && e.canonical.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && e.canonical.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !ok {
            return Err(ApiError::bad_request(format!("canonical {:?} must match [A-Za-z0-9][A-Za-z0-9_-]{{0,15}}", e.canonical)));
        }
        if !canon.insert(e.canonical.to_lowercase()) {
            return Err(ApiError::bad_request(format!("duplicate canonical {:?}", e.canonical)));
        }
        if !KINDS.contains(&e.kind.as_str()) {
            return Err(ApiError::bad_request(format!("{}: kind must be one of {}", e.canonical, KINDS.join(", "))));
        }
        if e.aliases.len() > 20 {
            return Err(ApiError::bad_request(format!("{}: at most 20 aliases", e.canonical)));
        }
        for a in &e.aliases {
            if a.trim().is_empty() || a.len() > 40 {
                return Err(ApiError::bad_request(format!("{}: alias {:?} must be 1..=40 chars", e.canonical, a)));
            }
            if !aliases.insert(a.trim().to_lowercase()) {
                return Err(ApiError::bad_request(format!("duplicate alias {:?}", a)));
            }
        }
    }
    Ok(())
}

/// Insert version 1 for a new project (called by `create_project_core`).
pub(crate) async fn seed_default_tx(conn: &mut PgConnection, project_id: Uuid, by: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO project_filter_dictionary (project_id, version, entries, created_by) VALUES ($1, 1, $2, $3)")
        .bind(project_id)
        .bind(serde_json::to_value(default_dictionary()).expect("static json"))
        .bind(by)
        .execute(conn)
        .await?;
    Ok(())
}

async fn latest_entries(conn: &mut PgConnection, project_id: Uuid) -> Result<Vec<DictEntry>, sqlx::Error> {
    let row: Option<(Value,)> = sqlx::query_as("SELECT entries FROM project_filter_dictionary WHERE project_id = $1 ORDER BY version DESC LIMIT 1")
        .bind(project_id)
        .fetch_optional(conn)
        .await?;
    Ok(row.and_then(|(v,)| serde_json::from_value(v).ok()).unwrap_or_default())
}

pub(crate) async fn canonical_filters_tx(conn: &mut PgConnection, project_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    Ok(latest_entries(conn, project_id).await?.into_iter().map(|e| e.canonical).collect())
}

pub(crate) async fn filter_kinds(db: &PgPool, project_id: Uuid) -> Result<HashMap<String, String>, sqlx::Error> {
    let mut conn = db.acquire().await?;
    Ok(latest_entries(&mut conn, project_id).await?.into_iter().map(|e| (e.canonical, e.kind)).collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryView { version: i32, entries: Vec<DictEntry>, created_at: DateTime<Utc> }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryResponse { current: Option<DictionaryView>, history: Vec<DictionaryView> }

#[tracing::instrument(skip_all)]
pub async fn get_dictionary(State(state): State<AppState>, Extension(auth): Extension<AuthAccount>, Path(id): Path<Uuid>) -> Result<Json<DictionaryResponse>, ApiError> {
    require_member(&state.db, id, auth.account_id).await?;
    let rows: Vec<(i32, Value, DateTime<Utc>)> = sqlx::query_as("SELECT version, entries, created_at FROM project_filter_dictionary WHERE project_id = $1 ORDER BY version DESC")
        .bind(id).fetch_all(&state.db).await?;
    let history: Vec<DictionaryView> = rows.into_iter().map(|(version, entries, created_at)| DictionaryView {
        version, entries: serde_json::from_value(entries).unwrap_or_default(), created_at,
    }).collect();
    let current = history.first().map(|h| DictionaryView { version: h.version, entries: h.entries.clone(), created_at: h.created_at });
    Ok(Json(DictionaryResponse { current, history }))
}

#[derive(Deserialize)]
pub struct PutDictionary { entries: Vec<DictEntry> }

#[tracing::instrument(skip_all)]
pub async fn put_dictionary(State(state): State<AppState>, Extension(auth): Extension<AuthAccount>, Path(id): Path<Uuid>, Json(body): Json<PutDictionary>) -> Result<Json<serde_json::Value>, ApiError> {
    require_cap(&state.db, id, auth.account_id, "thresholds.edit").await?;
    let entries: Vec<DictEntry> = body.entries.into_iter().map(|mut e| { e.aliases = e.aliases.into_iter().map(|a| a.trim().to_string()).collect(); e }).collect();
    validate_dictionary(&entries)?;
    let mut tx = state.db.begin().await?;
    let locked: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM projects WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    if locked.is_none() { return Err(ApiError::not_found("project not found")); }
    let (version,): (i32,) = sqlx::query_as("SELECT COALESCE(MAX(version), 0) + 1 FROM project_filter_dictionary WHERE project_id = $1").bind(id).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO project_filter_dictionary (project_id, version, entries, created_by) VALUES ($1, $2, $3, $4)")
        .bind(id).bind(version).bind(serde_json::to_value(&entries).expect("json")).bind(auth.account_id)
        .execute(&mut *tx).await?;
    bump_project_version_tx(&mut tx, id).await?;
    record_event_tx(&mut tx, id, "dictionary_updated", Some(auth.account_id), None, serde_json::json!({ "version": version })).await?;
    tx.commit().await?;
    state.versions.invalidate(id);
    tracing::info!(project_id = %id, version, "dictionary updated");
    Ok(Json(serde_json::json!({ "version": version })))
}
```

In `announce` (Task 2) add before the filter loop: `if canonical.is_empty() { return Err(ApiError::conflict("project has no filter dictionary")); }`.

`projects.rs::create_project_core`: after the coordinator insert, `crate::routes::dictionary::seed_default_tx(tx, project.id, coordinator).await?;`. Coverage `kind`: `let kinds = dictionary::filter_kinds(&state.db, project.id).await?;` and `kind: kinds.get(&r.filter_canonical).cloned().unwrap_or_else(|| "broadband".into())`.

Routes: `.route("/api/v1/projects/{id}/dictionary", get(dictionary::get_dictionary).put(dictionary::put_dictionary))`.

- [ ] **Step 4: Run tests**

Run: `DATABASE_URL=… cargo test --test dictionary --test frames --test projects_api`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes/dictionary.rs src/routes/projects.rs src/routes/mod.rs tests/dictionary.rs
git commit -m "feat(hub): versioned per-project filter dictionary; announce validates the canonical filter against it"
```

---

### Task 6: Frame versions, moderation (approve/reject/trust), exclusion and remap, grid

**Files:**
- Modify: `src/routes/frames.rs` (add `new_version`, `patch_frame`, `approve`, `reject`), `src/routes/members.rs` (`put_trust`, `MemberAdminRow.trusted_publisher`), `src/routes/projects.rs` (`put_grid`), `src/routes/mod.rs`
- Test: `tests/trust_and_grid.rs`, extend `tests/frames.rs`

**Interfaces:**
- `POST /projects/{id}/frames/{uuid}/version` body `{blake3, byteSize, xxh3}` (publisher only; project active): `content_version + 1`, new `project_frame_versions` row, `manifest_version` = bumped project version, the publisher's device becomes the only holder of the new version (`DELETE FROM frame_holders WHERE … frame_uuid = $2` then insert own device); event `frame_version {frameUuid, contentVersion}`. Response `{contentVersion, projectVersion}`.
- `PATCH /projects/{id}/frames/{uuid}` body `{accepted?: bool, acceptedReason?: string (required when accepted=false, 1..=500), filterCanonical?: string}`; cap `data.moderate` for `accepted`, `thresholds.edit` for `filterCanonical` (must be in the dictionary); bumps; events `frame_excluded {frameUuid, reason}` / `frame_restored` / `frame_remapped {from, to}`. 204.
- `POST /projects/{id}/frames/{uuid}/approve` body `{trust: bool}` (`data.moderate`): pending → published; when `trust`, sets `project_members.trusted_publisher = true` for the publisher AND publishes every other pending frame of that publisher in the project; bumps; events `frame_approved` (+ `trust_changed`). Response `{published: n}`.
- `POST /projects/{id}/frames/{uuid}/reject` body `{reason}` (1..=500): pending → rejected; refused 409 "frame is already held by other members" when any fresh holder row belongs to a device of an account other than the publisher or a moderator (R13). Rejected rows stay in the manifest (state `rejected`) so the publisher's app can show the reason.
- `PUT /projects/{id}/members/{account_id}/trust` body `{trusted: bool}` (`members.manage`; 409 when the target is the coordinator): sets the flag; when `true` publishes the target's pending frames; bumps; event `trust_changed {accountId, trusted}`.
- `PUT /projects/{id}/grid` body `{scaleArcsec, widthPx, heightPx, crval1, crval2, rotationDeg}` (`project.edit`): validation `scaleArcsec` in (0, 100], width/height in [64, 65536], crval1 in [0, 360), crval2 in [-90, 90], rotation finite; stored as `projects.canonical_grid`; bumps; event `grid_changed`. 204. Read back through `project_page.canonicalGrid`.
- `MemberAdminRow` (wave 0) gains `trusted_publisher: bool`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/frames.rs`:

```rust
#[sqlx::test]
async fn new_content_version_resets_holders_to_the_publisher(pool: PgPool) {
    let (app, _m, coord, anna, id) = setup(pool, false).await;
    assert_eq!(announce_frames(&app, &anna, &id, 1, 2).await.0, StatusCode::OK);
    let uuid = "00000000-0000-4000-8000-000000000001";
    // Coordinator holds v1.
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &json!({"full": false, "add": [{"frameUuid": uuid, "contentVersion": 1}], "remove": []}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames/{uuid}/version"), &json!({"blake3": format!("{:064x}", 77), "byteSize": 105_000_000, "xxh3": format!("{:016x}", 77)}), Some(&anna))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["contentVersion"], 2);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/frames/{uuid}/holders"), Some(&coord))).await;
    assert_eq!(as_json(&body).as_array().unwrap().len(), 1, "only the publisher holds v2");
    // Only the publisher may version.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/frames/{uuid}/version"), &json!({"blake3": format!("{:064x}", 78), "byteSize": 1, "xxh3": format!("{:016x}", 78)}), Some(&coord))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn exclusion_and_remap_are_manifest_edits(pool: PgPool) {
    let (app, _m, coord, anna, id) = setup(pool, false).await;
    assert_eq!(announce_frames(&app, &anna, &id, 1, 2).await.0, StatusCode::OK);
    let uuid = "00000000-0000-4000-8000-000000000001";
    // Anna cannot exclude; the coordinator can, with a reason.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/frames/{uuid}"), &json!({"accepted": false, "acceptedReason": "clouds"}), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(&app, patch(&format!("/api/v1/projects/{id}/frames/{uuid}"), &json!({"accepted": false}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", String::from_utf8_lossy(&body));
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/frames/{uuid}"), &json!({"accepted": false, "acceptedReason": "clouds"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/manifest?since=0"), Some(&anna))).await;
    let row = &as_json(&body)["rows"][0];
    assert_eq!(row["accepted"], false);
    assert_eq!(row["acceptedReason"], "clouds");
    // Remap to R (in the dictionary) works; to Purple does not.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/frames/{uuid}"), &json!({"filterCanonical": "R"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/frames/{uuid}"), &json!({"filterCanonical": "Purple"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Coverage on the public page reflects exclusion: 0 accepted frames.
    let slug = as_json(&send(&app, get(&format!("/api/v1/projects/{id}"), Some(&coord))).await.1)["project"]["slug"].as_str().unwrap().to_string();
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{slug}"), None)).await;
    assert_eq!(as_json(&body)["coverage"]["frames"], 0);
}

#[sqlx::test]
async fn approve_with_trust_publishes_the_rest_and_reject_is_pre_seed_only(pool: PgPool) {
    let (app, mailer, coord, anna, id) = setup(pool, true).await;
    assert_eq!(announce_frames(&app, &anna, &id, 1, 4).await.0, StatusCode::OK); // pending ×3
    let u = |n: u32| format!("00000000-0000-4000-8000-{n:012x}");
    // Reject frame 3 while only Anna holds it.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/frames/{}/reject", u(3)), &json!({"reason": "trailed"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    // Approve frame 1 with trust → frame 2 publishes too, Anna is trusted.
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames/{}/approve", u(1)), &json!({"trust": true}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["published"], 2);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/members"), Some(&coord))).await;
    let anna_row = as_json(&body).as_array().unwrap().iter().find(|r| r["displayName"] == "Anna").cloned().unwrap();
    assert_eq!(anna_row["trustedPublisher"], true);
    // Anna's next announce is published outright.
    let (_, body) = announce_frames(&app, &anna, &id, 10, 11).await;
    assert_eq!(body["state"], "published");
    // A published frame held by Bob cannot be rejected (it is not pending anyway) — and a pending one held by a processor cannot either.
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "PC").await;
    join_and_approve(&app, &coord, &bob, &id, "Bob", "send_receive").await;
    let (carl, _) = register_device(&app, &mailer, "carl@example.com", 4, "PC2").await;
    join_and_approve(&app, &coord, &carl, &id, "Carl", "send").await;
    assert_eq!(announce_frames(&app, &carl, &id, 20, 21).await.1["state"], "pending");
    // Bob (send_receive, not a moderator) cannot hold a pending frame — so no holder; reject works.
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &json!({"full": false, "add": [{"frameUuid": u(20), "contentVersion": 1}], "remove": []}), Some(&bob))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/frames/{}/reject", u(20)), &json!({"reason": "no"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
}
```

`tests/trust_and_grid.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn trust_route_publishes_pending_and_refuses_the_coordinator(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    let anna_id = join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    assert_eq!(announce_frames(&app, &anna, id, 1, 3).await.1["state"], "pending");
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/members/{anna_id}/trust"), &json!({"trusted": true}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/manifest?since=0"), Some(&anna))).await;
    assert!(as_json(&body)["rows"].as_array().unwrap().iter().all(|r| r["state"] == "published"));
    // Coordinator's own trust is ownership.
    let coord_id = as_json(&send(&app, get(&format!("/api/v1/projects/{id}/members"), Some(&coord))).await.1).as_array().unwrap().iter().find(|r| r["coordinator"] == true).unwrap()["accountId"].as_str().unwrap().to_string();
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/members/{coord_id}/trust"), &json!({"trusted": false}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn grid_is_validated_and_read_back(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let slug = project["slug"].as_str().unwrap();
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/grid"), &json!({"scaleArcsec": 0, "widthPx": 100, "heightPx": 100, "crval1": 1, "crval2": 2, "rotationDeg": 0}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", String::from_utf8_lossy(&body));
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/grid"), &json!({"scaleArcsec": 1.25, "widthPx": 6000, "heightPx": 4000, "crval1": 210.8, "crval2": 54.35, "rotationDeg": 0}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{slug}"), None)).await;
    assert_eq!(as_json(&body)["project"]["canonicalGrid"]["scaleArcsec"], 1.25);
}
```

- [ ] **Step 2: Run to see them fail**

Run: `DATABASE_URL=… cargo test --test frames --test trust_and_grid`
Expected: FAIL (404s).

- [ ] **Step 3: Implement**

Append to `src/routes/frames.rs`:

```rust
// ---- POST /projects/{id}/frames/{uuid}/version ---------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewVersion { pub blake3: String, pub byte_size: i64, pub xxh3: String }

#[tracing::instrument(skip_all)]
pub async fn new_version(
    State(state): State<AppState>, Extension(auth): Extension<AuthAccount>,
    Path((id, frame_uuid)): Path<(Uuid, Uuid)>, Json(body): Json<NewVersion>,
) -> Result<Json<Value>, ApiError> {
    let Some(device_id) = auth.device_id else { return Err(ApiError::bad_request("a device token is required")); };
    require_member(&state.db, id, auth.account_id).await?;
    if !is_lower_hex(&body.blake3, 64) { return Err(ApiError::bad_request("blake3 must be 64 lowercase hex chars")); }
    if !is_lower_hex(&body.xxh3, 16) { return Err(ApiError::bad_request("xxh3 must be 16 lowercase hex chars")); }
    if body.byte_size <= 0 { return Err(ApiError::bad_request("byteSize must be > 0")); }
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, i32, String)> = sqlx::query_as(
        "SELECT f.publisher, f.content_version, p.status FROM project_frames f JOIN projects p ON p.id = f.project_id \
         WHERE f.project_id = $1 AND f.frame_uuid = $2 FOR UPDATE OF f",
    ).bind(id).bind(frame_uuid).fetch_optional(&mut *tx).await?;
    let Some((publisher, current, status)) = row else { return Err(ApiError::not_found("frame not found")); };
    if publisher != auth.account_id { return Err(ApiError::Status(axum::http::StatusCode::FORBIDDEN)); }
    if status != "active" { return Err(ApiError::conflict("project is closed")); }
    let next = current + 1;
    let version = bump_project_version_tx(&mut tx, id).await?;
    sqlx::query("UPDATE project_frames SET content_version = $3, blake3 = $4, byte_size = $5, xxh3 = $6, manifest_version = $7 WHERE project_id = $1 AND frame_uuid = $2")
        .bind(id).bind(frame_uuid).bind(next).bind(&body.blake3).bind(body.byte_size).bind(&body.xxh3).bind(version)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO project_frame_versions (project_id, frame_uuid, content_version, blake3, byte_size, xxh3) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(frame_uuid).bind(next).bind(&body.blake3).bind(body.byte_size).bind(&body.xxh3)
        .execute(&mut *tx).await?;
    sqlx::query("DELETE FROM frame_holders WHERE project_id = $1 AND frame_uuid = $2").bind(id).bind(frame_uuid).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version) VALUES ($1,$2,$3,$4)")
        .bind(id).bind(frame_uuid).bind(device_id).bind(next).execute(&mut *tx).await?;
    record_event_tx(&mut tx, id, "frame_version", Some(auth.account_id), None, serde_json::json!({ "frameUuid": frame_uuid, "contentVersion": next })).await?;
    tx.commit().await?;
    state.versions.invalidate(id);
    Ok(Json(serde_json::json!({ "contentVersion": next, "projectVersion": version })))
}

// ---- PATCH /projects/{id}/frames/{uuid} ----------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FramePatch { pub accepted: Option<bool>, pub accepted_reason: Option<String>, pub filter_canonical: Option<String> }

#[tracing::instrument(skip_all)]
pub async fn patch_frame(
    State(state): State<AppState>, Extension(auth): Extension<AuthAccount>,
    Path((id, frame_uuid)): Path<(Uuid, Uuid)>, Json(body): Json<FramePatch>,
) -> Result<axum::http::StatusCode, ApiError> {
    if body.accepted.is_none() && body.filter_canonical.is_none() {
        return Err(ApiError::bad_request("accepted or filterCanonical is required"));
    }
    let member = require_member(&state.db, id, auth.account_id).await?;
    if body.accepted.is_some() && !member.has_cap("data.moderate") { return Err(ApiError::Status(axum::http::StatusCode::FORBIDDEN)); }
    if body.filter_canonical.is_some() && !member.has_cap("thresholds.edit") { return Err(ApiError::Status(axum::http::StatusCode::FORBIDDEN)); }
    let reason = body.accepted_reason.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if body.accepted == Some(false) && reason.is_none_or(|r| r.len() > 500) {
        return Err(ApiError::bad_request("acceptedReason (1..=500 chars) is required when excluding a frame"));
    }
    let mut tx = state.db.begin().await?;
    let row: Option<(String, bool)> = sqlx::query_as("SELECT filter_canonical, accepted FROM project_frames WHERE project_id = $1 AND frame_uuid = $2 FOR UPDATE")
        .bind(id).bind(frame_uuid).fetch_optional(&mut *tx).await?;
    let Some((old_filter, _old_accepted)) = row else { return Err(ApiError::not_found("frame not found")); };
    if let Some(filter) = &body.filter_canonical {
        let canonical = crate::routes::dictionary::canonical_filters_tx(&mut tx, id).await?;
        if !canonical.iter().any(|c| c == filter) {
            return Err(ApiError::bad_request(format!("filter {filter:?} is not in the project dictionary")));
        }
    }
    let version = bump_project_version_tx(&mut tx, id).await?;
    if let Some(accepted) = body.accepted {
        sqlx::query("UPDATE project_frames SET accepted = $3, accepted_reason = $4, accepted_by = $5, accepted_at = now(), manifest_version = $6 WHERE project_id = $1 AND frame_uuid = $2")
            .bind(id).bind(frame_uuid).bind(accepted).bind(if accepted { None } else { reason }).bind(auth.account_id).bind(version)
            .execute(&mut *tx).await?;
        record_event_tx(&mut tx, id, if accepted { "frame_restored" } else { "frame_excluded" }, Some(auth.account_id), None,
            serde_json::json!({ "frameUuid": frame_uuid, "reason": reason })).await?;
    }
    if let Some(filter) = &body.filter_canonical {
        sqlx::query("UPDATE project_frames SET filter_canonical = $3, manifest_version = $4 WHERE project_id = $1 AND frame_uuid = $2")
            .bind(id).bind(frame_uuid).bind(filter).bind(version).execute(&mut *tx).await?;
        record_event_tx(&mut tx, id, "frame_remapped", Some(auth.account_id), None, serde_json::json!({ "frameUuid": frame_uuid, "from": old_filter, "to": filter })).await?;
    }
    tx.commit().await?;
    state.versions.invalidate(id);
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ---- approve / reject ----------------------------------------------------------

/// Publish every pending frame of `publisher` in the project. Returns the count.
pub(crate) async fn publish_pending_of_tx(conn: &mut PgConnection, project_id: Uuid, publisher: Uuid, decided_by: Uuid, manifest_version: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("UPDATE project_frames SET state = 'published', decided_by = $3, decided_at = now(), manifest_version = $4 \
                    WHERE project_id = $1 AND publisher = $2 AND state = 'pending'")
        .bind(project_id).bind(publisher).bind(decided_by).bind(manifest_version)
        .execute(conn).await?.rows_affected())
}

#[derive(Deserialize)]
pub struct ApproveBody { #[serde(default)] pub trust: bool }

#[tracing::instrument(skip_all)]
pub async fn approve(
    State(state): State<AppState>, Extension(auth): Extension<AuthAccount>,
    Path((id, frame_uuid)): Path<(Uuid, Uuid)>, Json(body): Json<ApproveBody>,
) -> Result<Json<Value>, ApiError> {
    crate::collab_auth::require_cap(&state.db, id, auth.account_id, "data.moderate").await?;
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, String)> = sqlx::query_as("SELECT publisher, state FROM project_frames WHERE project_id = $1 AND frame_uuid = $2 FOR UPDATE")
        .bind(id).bind(frame_uuid).fetch_optional(&mut *tx).await?;
    let Some((publisher, st)) = row else { return Err(ApiError::not_found("frame not found")); };
    if st != "pending" { return Err(ApiError::conflict("frame is not pending")); }
    let version = bump_project_version_tx(&mut tx, id).await?;
    let published = if body.trust {
        sqlx::query("UPDATE project_members SET trusted_publisher = true WHERE project_id = $1 AND account_id = $2").bind(id).bind(publisher).execute(&mut *tx).await?;
        record_event_tx(&mut tx, id, "trust_changed", Some(auth.account_id), Some(publisher), serde_json::json!({ "trusted": true })).await?;
        publish_pending_of_tx(&mut tx, id, publisher, auth.account_id, version).await?
    } else {
        sqlx::query("UPDATE project_frames SET state = 'published', decided_by = $3, decided_at = now(), manifest_version = $4 WHERE project_id = $1 AND frame_uuid = $2")
            .bind(id).bind(frame_uuid).bind(auth.account_id).bind(version).execute(&mut *tx).await?.rows_affected()
    };
    record_event_tx(&mut tx, id, "frame_approved", Some(auth.account_id), Some(publisher), serde_json::json!({ "frameUuid": frame_uuid, "trust": body.trust, "published": published })).await?;
    tx.commit().await?;
    state.versions.invalidate(id);
    Ok(Json(serde_json::json!({ "published": published })))
}

#[derive(Deserialize)]
pub struct RejectBody { pub reason: String }

#[tracing::instrument(skip_all)]
pub async fn reject(
    State(state): State<AppState>, Extension(auth): Extension<AuthAccount>,
    Path((id, frame_uuid)): Path<(Uuid, Uuid)>, Json(body): Json<RejectBody>,
) -> Result<Json<Value>, ApiError> {
    crate::collab_auth::require_cap(&state.db, id, auth.account_id, "data.moderate").await?;
    let reason = body.reason.trim();
    if reason.is_empty() || reason.len() > 500 { return Err(ApiError::bad_request("reason must be 1..=500 chars")); }
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, String)> = sqlx::query_as("SELECT publisher, state FROM project_frames WHERE project_id = $1 AND frame_uuid = $2 FOR UPDATE")
        .bind(id).bind(frame_uuid).fetch_optional(&mut *tx).await?;
    let Some((publisher, st)) = row else { return Err(ApiError::not_found("frame not found")); };
    if st != "pending" { return Err(ApiError::conflict("frame is not pending")); }
    // R13: seeded elsewhere → exclusion is the only tool.
    let (foreign_holders,): (i64,) = sqlx::query_as(&format!(
        "SELECT count(*) FROM frame_holders h JOIN devices d ON d.id = h.device_id \
         LEFT JOIN project_members pm ON pm.project_id = h.project_id AND pm.account_id = d.account_id \
         WHERE h.project_id = $1 AND h.frame_uuid = $2 AND {HOLDER_FRESH} AND d.account_id <> $3 \
           AND NOT (pm.is_coordinator OR 'data.moderate' = ANY(pm.gov_caps))",
        HOLDER_FRESH = crate::routes::holders::HOLDER_FRESH_SQL)).bind(id).bind(frame_uuid).bind(publisher).fetch_one(&mut *tx).await?;
    if foreign_holders > 0 { return Err(ApiError::conflict("frame is already held by other members; exclude it instead")); }
    let version = bump_project_version_tx(&mut tx, id).await?;
    sqlx::query("UPDATE project_frames SET state = 'rejected', reject_reason = $3, decided_by = $4, decided_at = now(), manifest_version = $5 WHERE project_id = $1 AND frame_uuid = $2")
        .bind(id).bind(frame_uuid).bind(reason).bind(auth.account_id).bind(version).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM frame_holders WHERE project_id = $1 AND frame_uuid = $2").bind(id).bind(frame_uuid).execute(&mut *tx).await?;
    record_event_tx(&mut tx, id, "frame_rejected", Some(auth.account_id), Some(publisher), serde_json::json!({ "frameUuid": frame_uuid, "reason": reason })).await?;
    tx.commit().await?;
    state.versions.invalidate(id);
    Ok(Json(serde_json::json!({ "state": "rejected" })))
}
```

`members.rs` — `put_trust`:

```rust
#[derive(Deserialize)]
pub struct TrustBody { pub trusted: bool }

#[tracing::instrument(skip_all)]
pub async fn put_trust(
    State(state): State<AppState>, Extension(auth): Extension<AuthAccount>,
    Path((id, account_id)): Path<(Uuid, Uuid)>, Json(body): Json<TrustBody>,
) -> Result<StatusCode, ApiError> {
    require_cap(&state.db, id, auth.account_id, "members.manage").await?;
    let mut tx = state.db.begin().await?;
    let rows = lock_member_rows(&mut tx, id, &[account_id]).await?;
    let Some(target) = rows.into_iter().find(|m| m.account_id == account_id) else { return Err(ApiError::not_found("member not found")); };
    if target.is_coordinator { return Err(ApiError::conflict("the coordinator's trust is ownership, not a flag")); }
    sqlx::query("UPDATE project_members SET trusted_publisher = $3 WHERE project_id = $1 AND account_id = $2").bind(id).bind(account_id).bind(body.trusted).execute(&mut *tx).await?;
    let version = crate::project_version::bump_project_version_tx(&mut tx, id).await?;
    if body.trusted {
        crate::routes::frames::publish_pending_of_tx(&mut tx, id, account_id, auth.account_id, version).await?;
    }
    record_event_tx(&mut tx, id, "trust_changed", Some(auth.account_id), Some(account_id), serde_json::json!({ "trusted": body.trusted })).await?;
    tx.commit().await?;
    state.versions.invalidate(id);
    Ok(StatusCode::NO_CONTENT)
}
```

and add `trusted_publisher` to `MemberAdminRow` + its SELECT.

`projects.rs` — `put_grid`:

```rust
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalGrid { pub scale_arcsec: f64, pub width_px: i64, pub height_px: i64, pub crval1: f64, pub crval2: f64, pub rotation_deg: f64 }

#[tracing::instrument(skip_all)]
pub async fn put_grid(State(state): State<AppState>, Extension(auth): Extension<AuthAccount>, Path(id): Path<Uuid>, Json(g): Json<CanonicalGrid>) -> Result<StatusCode, ApiError> {
    crate::collab_auth::require_cap(&state.db, id, auth.account_id, "project.edit").await?;
    let ok = g.scale_arcsec.is_finite() && g.scale_arcsec > 0.0 && g.scale_arcsec <= 100.0
        && (64..=65536).contains(&g.width_px) && (64..=65536).contains(&g.height_px)
        && (0.0..360.0).contains(&g.crval1) && (-90.0..=90.0).contains(&g.crval2) && g.rotation_deg.is_finite();
    if !ok {
        return Err(ApiError::bad_request("grid needs scaleArcsec in (0,100], widthPx/heightPx in [64,65536], crval1 in [0,360), crval2 in [-90,90], finite rotationDeg"));
    }
    let mut tx = state.db.begin().await?;
    let n = sqlx::query("UPDATE projects SET canonical_grid = $2 WHERE id = $1").bind(id).bind(serde_json::to_value(&g).expect("json")).execute(&mut *tx).await?.rows_affected();
    if n == 0 { return Err(ApiError::not_found("project not found")); }
    crate::project_version::bump_project_version_tx(&mut tx, id).await?;
    crate::collab_auth::record_event_tx(&mut tx, id, "grid_changed", Some(auth.account_id), None, serde_json::to_value(&g).expect("json")).await?;
    tx.commit().await?;
    state.versions.invalidate(id);
    Ok(StatusCode::NO_CONTENT)
}
```

Routes:

```rust
        .route("/api/v1/projects/{id}/frames/{frame_uuid}", axum::routing::patch(frames::patch_frame))
        .route("/api/v1/projects/{id}/frames/{frame_uuid}/version", post(frames::new_version))
        .route("/api/v1/projects/{id}/frames/{frame_uuid}/approve", post(frames::approve))
        .route("/api/v1/projects/{id}/frames/{frame_uuid}/reject", post(frames::reject))
        .route("/api/v1/projects/{id}/members/{account_id}/trust", axum::routing::put(members::put_trust))
        .route("/api/v1/projects/{id}/grid", axum::routing::put(projects::put_grid))
```

- [ ] **Step 4: Run the tests**

Run: `DATABASE_URL=… cargo test --test frames --test trust_and_grid --test governance --test collab_flow`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes/frames.rs src/routes/members.rs src/routes/projects.rs src/routes/mod.rs tests/frames.rs tests/trust_and_grid.rs
git commit -m "feat(hub): frame content versions, exclusion/remap as manifest edits, approve-with-trust, pre-seed reject, publisher trust, canonical grid"
```

---

### Task 7: `GET /me/project-versions` with the in-process cache

**Files:**
- Create: `src/routes/versions.rs`; register in `mod.rs` (`account_protected`).
- Test: `tests/versions.rs`

**Interfaces:**
- `GET /api/v1/me/project-versions` → `[{projectId, version}]` for every project the caller is a member of. Cache hit per project; miss → one `SELECT id, version FROM projects WHERE id = ANY($1)` for the missing ids, results put into the cache. The membership list itself is one indexed query on `project_members` (account_id) — acceptable at 8 req/s; if profiling shows otherwise, cache the membership per account with the same invalidation on membership bumps.

- [ ] **Step 1: Write the failing test**

`tests/versions.rs`:

```rust
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn versions_move_on_every_device_visible_change(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let anna_id = join_and_approve(&app, &coord, &anna, &id, "Anna", "send").await;
    let read = |token: String| { let app = app.clone(); let id = id.clone(); async move {
        let (status, body) = send(&app, get("/api/v1/me/project-versions", Some(&token))).await;
        assert_eq!(status, StatusCode::OK);
        as_json(&body).as_array().unwrap().iter().find(|r| r["projectId"] == id).unwrap()["version"].as_i64().unwrap()
    }};
    let v0 = read(anna.clone()).await;
    assert_eq!(read(anna.clone()).await, v0, "stable between changes");
    // announce, thresholds, dictionary, trust, grid, membership each bump.
    announce_frames(&app, &anna, &id, 1, 2).await;
    let v1 = read(anna.clone()).await; assert!(v1 > v0);
    send(&app, post(&format!("/api/v1/projects/{id}/thresholds"), &json!({"rules": [{"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0}]}), Some(&coord))).await;
    let v2 = read(anna.clone()).await; assert!(v2 > v1);
    send(&app, put(&format!("/api/v1/projects/{id}/members/{anna_id}/trust"), &json!({"trusted": true}), Some(&coord))).await;
    let v3 = read(anna.clone()).await; assert!(v3 > v2);
    send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_id}"), &json!({"dataRole": "send_receive"}), Some(&coord))).await;
    let v4 = read(anna.clone()).await; assert!(v4 > v3, "membership change bumps the project version too");
    // An outsider sees nothing.
    let (x, _) = register_device(&app, &mailer, "x@example.com", 9, "PC").await;
    let (_, body) = send(&app, get("/api/v1/me/project-versions", Some(&x))).await;
    assert_eq!(as_json(&body).as_array().unwrap().len(), 0);
}
```

Also make `post_thresholds` (existing) bump the project version + invalidate (it records an event today; add the two lines).

- [ ] **Step 2: Run to see it fail** — `cargo test --test versions` → 404.

- [ ] **Step 3: Implement `src/routes/versions.rs`**

```rust
//! The one request a device polls every 15 s (collab v3 R19, §4.3): served
//! from `VersionCache`, no DB hit for a warm project.
use axum::extract::State;
use axum::{Extension, Json};
use serde::Serialize;
use uuid::Uuid;

use crate::auth_mw::AuthAccount;
use crate::error::ApiError;
use crate::routes::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectVersion { pub project_id: Uuid, pub version: i64 }

#[tracing::instrument(skip_all, level = "debug")]
pub async fn my_project_versions(State(state): State<AppState>, Extension(auth): Extension<AuthAccount>) -> Result<Json<Vec<ProjectVersion>>, ApiError> {
    let ids: Vec<(Uuid,)> = sqlx::query_as("SELECT project_id FROM project_members WHERE account_id = $1")
        .bind(auth.account_id).fetch_all(&state.db).await?;
    let mut out = Vec::with_capacity(ids.len());
    let mut missing = Vec::new();
    for (id,) in ids {
        match state.versions.get(id) {
            Some(v) => out.push(ProjectVersion { project_id: id, version: v }),
            None => missing.push(id),
        }
    }
    if !missing.is_empty() {
        let rows: Vec<(Uuid, i64)> = sqlx::query_as("SELECT id, version FROM projects WHERE id = ANY($1)").bind(&missing).fetch_all(&state.db).await?;
        for (id, v) in rows {
            state.versions.put(id, v);
            out.push(ProjectVersion { project_id: id, version: v });
        }
    }
    out.sort_by_key(|p| p.project_id);
    Ok(Json(out))
}
```

Route: `.route("/api/v1/me/project-versions", get(versions::my_project_versions))`.

- [ ] **Step 4: Run** — `cargo test --test versions --test thresholds` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes/versions.rs src/routes/mod.rs src/routes/thresholds.rs tests/versions.rs
git commit -m "feat(hub): GET /me/project-versions from the in-process cache; every device-visible write bumps projects.version"
```

---

### Task 8: Portal adapts to the frame-based project page

**Files:**
- Modify: `portal/src/types.ts` (drop `PackagePublicView`; `ProjectPageData.packages` → `coverage: CoverageView`; `ProjectView.canonicalGrid`, `version`; `MyProject.pendingAnnouncements` → `pendingFrames`; `PublicProfileStats.packagesAccepted` → `framesAccepted`; `OperatorOverview.announcementsByState` → `framesByState`; `OperatorProjectRow.announcementCount` → `frameCount`; `MemberAdminView.trustedPublisher`)
- Modify: `pages/ProjectPage.tsx:74-80, 126, 148, 193-209, 264-265`, `pages/Join.tsx:74-77, 245`, `pages/Profile.tsx:114`, `components/me/Member.tsx:29-48`, `pages/Operator.tsx:154-171`, `pages/Admin.tsx:148-153`, `components/MembersEditor.tsx` (trust switch), and their tests.

**Interfaces:**
```ts
export interface CoverageFilter { filter: string; kind: 'broadband' | 'narrowband' | 'luminance'; frames: number; seconds: number; publishers: number; goalSeconds: number | null }
export interface CoverageView { frames: number; bytes: number; byFilter: CoverageFilter[]; singleHolderFrames: number; wellReplicatedFrames: number; onlineHolders: number }
```

- [ ] **Step 1: Update the tests first** — in `ProjectPage.test.tsx`, `Join.test.tsx`, `Me.test.tsx`, `Profile.test.tsx`, `Admin.test.tsx`, `Operator` tests: replace `packages: []` fixtures with `coverage: { frames: 0, bytes: 0, byFilter: [], singleHolderFrames: 0, wellReplicatedFrames: 0, onlineHolders: 0 }`, `pendingAnnouncements` → `pendingFrames`, `packagesAccepted` → `framesAccepted`. Add one assertion to `ProjectPage.test.tsx`:

```tsx
  it('shows coverage per filter with goal shortfall and redundancy', async () => {
    page.coverage = { frames: 40, bytes: 4e9, byFilter: [{ filter: 'Ha', kind: 'narrowband', frames: 40, seconds: 12000, publishers: 3, goalSeconds: 36000 }], singleHolderFrames: 2, wellReplicatedFrames: 30, onlineHolders: 4 };
    renderPage();
    expect(await screen.findByText('Ha')).toBeInTheDocument();
    expect(screen.getByText(/3\.3 h of 10\.0 h/)).toBeInTheDocument();
    expect(screen.getByText(/2 frames with a single holder/)).toBeInTheDocument();
  });
```

(Use the file's existing `page`/`renderPage` helpers; read the file first.)

- [ ] **Step 2: Run** — `npx vitest run` → the changed suites FAIL on types/text.

- [ ] **Step 3: Implement**

`ProjectPage.tsx`: replace the "Packages" section (l.193-209) with a "Coverage" card: a row per `byFilter` entry — `FilterTag` for the filter, `Meter` with `meterPercent(seconds, goalSeconds)` when a goal exists, text `${hoursFromSeconds(seconds)} h of ${hoursFromSeconds(goalSeconds)} h` or `${hours} h` without a goal, `${publishers} contributors`; below: `${frames} frames · ${(bytes/1e9).toFixed(1)} GB · ${wellReplicatedFrames} well replicated · ${singleHolderFrames} frames with a single holder · ${onlineHolders} holders online`. `progress` (l.79-80, 126, 148) is unchanged in shape. The "Review N pending" button reads `membership.pendingFrames`.

`Join.tsx`: `packages.length`/summed `byteSize` → `coverage.frames` / `coverage.bytes`.
`Profile.tsx:114`: "Frames accepted".
`Member.tsx`: `pendingFrames`.
`Operator.tsx`: column "Frames", dashboard `framesByState`.
`Admin.tsx:148-153`: "{n} frame{s} awaiting your approval — review them in the Athenaeum app (Projects → {title} → Summary)".
`MembersEditor.tsx`: a `Switch` "Trusted publisher" per non-coordinator row calling `apiPut(…/members/{id}/trust, {trusted})` through a new `onTrust(accountId, trusted)` prop; Admin wires it with `act`.

- [ ] **Step 4: Run** — `npx vitest run && npx tsc -b && npm run build` → PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add portal/src
git commit -m "feat(portal): project page on frames — coverage per filter with goals and redundancy; pending frames; trusted-publisher switch"
```

---

### Task 9: README, whole-suite gate, deploy notes

**Files:**
- Modify: `README.md` (route table l.112-117; §§ at l.345-547 on announcements/supersedes/have → frames/manifest/holders/version; add the migration guard note)
- Modify (app repo): `docs/superpowers/open-items.md` — wave-1 entries: test-hub deploy owed (`hub_artifact_ref=collab-v3-wave1`), post-deploy check `SELECT count(*) FROM project_frames`, and "the desktop app cannot talk to a wave-1 hub until app wave 2 — expected 409 collab_api_outdated".

- [ ] **Step 1: Run the whole hub suite and the portal**

```bash
DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test 2>&1 | tail -8
cargo build --release 2>&1 | grep -c warning   # expect 0
cd portal && npx vitest run 2>&1 | tail -3
```

Expected: all green; 0 warnings.

- [ ] **Step 2: Migration replay check** — on a scratch DB restore the 0001–0021 state (run the hub once against an empty DB at the wave-0 commit, or `sqlx migrate run` up to 0021), insert one row into `package_announcements`, then run the wave-1 binary: it must refuse with the §11 message; delete the row, run again: success. Record the transcript in the commit message body.

- [ ] **Step 3: Docs and commit**

```bash
git add README.md && git commit -m "docs(hub): collab v3 API — frames, manifest, holders, dictionary, trust, grid, project versions; migration guard"
cd ../athenaeum && git add docs/superpowers/open-items.md && git commit -m "docs: collab v3 wave 1 — deploy and app-compat notes"
```

No push, no deploy without the owner's word.

---

## Self-review

- **Spec coverage:** §4.1 tables (Task 1), `projects.version` + cache (Tasks 1, 7), `trusted_publisher` (1, 6), `canonical_grid` (1, 6), drop guard (1); §4.2 routes: project-versions (7), manifest delta (2), announce batch (2), version (6), PATCH accepted/remap (6), approve/reject (6), holders self delta + per-frame holders (4), dictionary (5), trust (6), grid (6), thresholds registry (wave 0); coverage aggregate (3, 5); §9 birth state and trust (2, 6); R13 reject pre-seed (6); R4 file names validated (2); S1 relay-only (4); §11 compat 409 (3); portal (8); README (9). `zero_point` in the registry stays for wave 3.
- **Placeholders:** none. Every route has a test in the task that introduces it.
- **Type consistency:** `birth_state(require_approval, &Member)` (2) used by 6 through `publish_pending_of_tx`; `HOLDER_FRESH_SQL` defined in 2's stub, filled in 4, used by 2, 3, 6; `canonical_filters_tx` stubbed in 2, real in 5, used by 2 and 6; `FrameView.holder_count` (2) reads `frame_holders` (1); portal `CoverageView` (8) mirrors Rust `CoverageView` (3) field for field; `MemberAdminRow.trusted_publisher` (6) ↔ `MemberAdminView.trustedPublisher` (8).
