# Collaboration v2 — Wave 1 (correctness + foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three lies the current collaboration stack tells (collapsed credits of departed members, phantom holders, a swarm download that can never finish behind one useless peer) and lay the governance plane half the v2 portal depends on.

**Architecture:** Hub-side, four additive Postgres migrations (governance flags on `project_members`, a `project_alumni` table, a `join_policy`/`default_data_role` pair on `projects`, a `publisher_device_id` on announcements) plus one new `PUT …/have` endpoint; every `is_coordinator` gate that is about *governance* becomes a capability check, `is_coordinator` stays only for ownership (handover, leave, delete). App-side, the poll loop re-confirms the full held-package set each pass, iroh moves 1.0.3 → 1.2.0 with a reproducible bench, and the swarm fetch's phase 2 gets our own assignment loop over `store.remote()` with a progress deadline, a stall ceiling and range-split hedging — the stock `SplitStrategy::Split` path stays as the fallback behind one flag.

**Tech Stack:** Rust 1.96 · axum 0.8 + sqlx 0.8 (Postgres 16) on the hub · iroh 1.x / iroh-blobs 0.103 / n0-future in `athenaeum-core` · wiremock for hub-client tests · `#[sqlx::test]` per-test databases on the hub.

**Spec:** `docs/superpowers/plans/2026-09-19-collab-v2-backlog.md` (tasks H1–H4, A1, A2, A7; the three invariants) and `docs/superpowers/specs/2026-09-19-swarm-fast-paths-design.md` (D4 §4.1 T1, §4.5 T4, §4.7 T6, §6 parameters, §7 tests). Lifecycle reference: `athenaeum-hub/docs/design/2026-09-19-collab-lifecycle-4-members.md`.

## Global Constraints

- **Invariant 1 — governance flags never enter the signed membership snapshot.** Only `dataRole` and `coordinator` travel. A snapshot of a project where a member holds all six flags is byte-identical to one where they hold none. Every task touching `project_members` keeps `snapshots.rs`'s `SELECT` untouched and re-runs the pin test from Task 1.
- **Invariant 2 — clients apply every signature-verified snapshot whole, comparing content, not version.** No task bumps `membership_version` for a governance-only change; data-role changes, joins, leaves, removals and handovers still bump it.
- **Invariant 3 — anything a peer says (presence, have-reports relayed peer-to-peer, snapshots relayed by a peer) is a hint, never authorization.** Only the hub's own authenticated reply authorizes.
- **Two repos, two branches:** hub work on `athenaeum-hub` branch `collab-v2` (checked out at `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`); app work on athenaeum branch `collab-v2` (checked out in the worktree `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.claude/worktrees/perf-stacking-tier1` — the directory name is historical, the branch is `collab-v2`). Never touch the main athenaeum checkout at `Documents/Projects/athenaeum` (the owner's interactive tree).
- **Hub tests run against the compose Postgres:** `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test` from the hub repo root (`docker compose up -d postgres` if `docker ps` does not list `athenaeum-hub-postgres-1`). `#[sqlx::test]` provisions a fresh database per test and runs every migration, so a migration bug fails every test.
- **App builds use the worktree's own `target/`** (never the owner's `Documents/Projects/athenaeum/target`). Implementers run only scoped checks (`cargo check -p athenaeum-core`, the named test filters); the orchestrator runs the full gates between tasks.
- **Hub migrations are append-only and idempotent** (`ADD COLUMN IF NOT EXISTS`, `CREATE TABLE IF NOT EXISTS`); numbering continues at `0013`. The test hub and prod run the migrations at boot, so a migration must never fail on a database that already has the column.
- **Wire casing:** hub JSON is camelCase (`#[serde(rename_all = "camelCase")]`), Postgres columns snake_case. App-side TS mirrors live in `src/types/models.ts`; none change in this wave (A1/A2/A7 are backend-only).
- **Logging:** `tracing` only, message = short stable phrase, data in snake_case fields (`info!(project_id = %id, count = n, "have set reported")`). No `println!`. Hub handlers keep `#[tracing::instrument(skip_all)]`.
- **Never name third-party projects** (stacking tools, other astro software) in code, comments, docs or commit messages. iroh and Postgres are dependencies, not "third-party projects" in that sense, and may be named.
- **Commits:** as the user (`eg013ra1n` / `vilen.sharifov@gmail.com`), one commit per task, trailers:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01EjvVJVxscAK1QAMYuCRUsa
  ```
- **No push** — the owner pushes.

---

## File map

**Hub (`athenaeum-hub`)**

| File | Responsibility after this wave |
| ---- | ---- |
| `migrations/0013_governance.sql` | `project_members.gov_caps text[]`, backfill coordinators |
| `migrations/0014_alumni.sql` | `project_alumni` table |
| `migrations/0016_join_policy.sql` | `projects.join_policy`, `projects.default_data_role` |
| `migrations/0015_have_soft_state.sql` | `package_announcements.publisher_device_id`, index on `have_reports.reported_at` |
| `src/collab_auth.rs` | `Member.gov_caps`, `GOV_CAPS`, `Member::has_cap`, `require_cap` / `require_cap_tx` |
| `src/routes/members.rs` | `PATCH members/{account}` takes `dataRole` and/or `govCaps`; alumni row on remove/leave; leave takes `{credited}` |
| `src/routes/join_requests.rs` | gates on `members.manage`; open door joins directly; invite door 403 |
| `src/routes/thresholds.rs` | gate on `thresholds.edit` |
| `src/routes/announcements.rs` | approve/reject/list-all gate on `data.moderate`; `PUT /projects/{id}/have`; publisher device stamped; holder freshness + publisher ∪ |
| `src/routes/projects.rs` | `joinPolicy`/`defaultDataRole` in row/view/create/update; page: alumni block, per-member progress keyed by account, fresh holders ∪ publisher, `govCaps` per member |
| `src/routes/me.rs` | `govCaps` on `MyProjectView`; pending count gated on `data.moderate` |
| `src/routes/operator.rs` | new coordinator appointed with all six caps; operator removal writes alumni |
| `src/routes/mod.rs` | registers `PUT /api/v1/projects/{id}/have` |
| `README.md` | API table updated per task |
| `tests/governance.rs`, `tests/alumni.rs`, `tests/have_soft_state.rs`, `tests/join_policy.rs` | new suites |

**App (`athenaeum`, crate `athenaeum-core`)**

| File | Responsibility after this wave |
| ---- | ---- |
| `crates/athenaeum-core/src/collab/hub_client.rs` | `CollabClient::report_have_set(token, project_id, package_ids)` |
| `crates/athenaeum-core/src/api/collab_exchange.rs` | `held_package_ids_for_project`, `report_held_set` called from every auto-sync pass |
| `crates/athenaeum-core/Cargo.toml` | `iroh = "=1.2.0"` (+ whatever `noq` the lockfile resolves) |
| `crates/athenaeum-core/examples/swarm_bench.rs` | reproducible two-node localhost bench writing JSON |
| `crates/athenaeum-core/src/sharing/iroh/assign.rs` | **new** — the assignment loop (deadline, stall ceiling, hedging, per-provider stats) |
| `crates/athenaeum-core/src/sharing/iroh/blobs.rs` | `fetch_collection_multi` phase 2 dispatches on `SwarmFetchMode` |
| `crates/athenaeum-core/src/sharing/iroh/tests.rs` | assignment-loop tests (slow peer, stall, hedge cancel, budget) |

---

## Task 1: H1 — governance capability plane (hub)

**Files:**
- Create: `migrations/0013_governance.sql`
- Modify: `src/collab_auth.rs` (whole file — `Member` gains `gov_caps`; new `require_cap`)
- Modify: `src/routes/members.rs:70-130` (`change_member_role` → `patch_member`), `:262-330` (handover resets caps)
- Modify: `src/routes/join_requests.rs:186-195, 274-285, 326-335` (gates)
- Modify: `src/routes/thresholds.rs:81-88`
- Modify: `src/routes/announcements.rs:260-285` (list), `:351-360`, `:385-400` (approve/reject), `:425-445` (have pending rule)
- Modify: `src/routes/projects.rs:620-635` (`MemberRow`/`MemberPublicView` gain `govCaps`), `:768-790` (update gate)
- Modify: `src/routes/me.rs` (`govCaps`, pending gate)
- Modify: `src/routes/operator.rs:351-365` (appoint coordinator sets caps)
- Modify: `README.md` "API — Collaboration" auth-level legend + rows
- Test: `tests/governance.rs`

**Interfaces:**
- Produces (used by every later hub task):
  ```rust
  // src/collab_auth.rs
  pub const GOV_CAPS: [&str; 6] = ["members.manage", "data.moderate", "project.edit", "thresholds.edit", "invites.manage", "posts.write"];
  pub struct Member { pub account_id: Uuid, pub display_name: String, pub data_role: String, pub is_coordinator: bool, pub gov_caps: Vec<String> }
  impl Member { pub fn has_cap(&self, cap: &str) -> bool }         // coordinator ⇒ true for every cap
  pub async fn require_cap(db: &PgPool, project_id: Uuid, account_id: Uuid, cap: &str) -> Result<Member, ApiError>      // 403 body-less
  pub async fn require_cap_tx(conn: &mut PgConnection, project_id: Uuid, account_id: Uuid, cap: &str) -> Result<Member, ApiError>
  pub fn valid_gov_caps(caps: &[String]) -> bool                      // every entry ∈ GOV_CAPS, no duplicates
  ```
- Wire: `PATCH /api/v1/projects/{id}/members/{account_id}` body `{ "dataRole"?: "send"|"send_receive", "govCaps"?: string[] }`; `MemberPublicView.govCaps: string[]`; `MyProjectView.govCaps: string[]`.

- [ ] **Step 1: Write the migration**

`migrations/0013_governance.sql`:
```sql
-- 0013_governance — hub-only governance flags (never in the signed snapshot).
ALTER TABLE project_members ADD COLUMN IF NOT EXISTS gov_caps text[] NOT NULL DEFAULT '{}';
-- The coordinator owns the project: every flag, always. Backfill existing rows.
UPDATE project_members
   SET gov_caps = ARRAY['members.manage','data.moderate','project.edit','thresholds.edit','invites.manage','posts.write']
 WHERE is_coordinator AND gov_caps = '{}';
```

- [ ] **Step 2: Write the failing tests**

`tests/governance.rs`:
```rust
//! Governance plane (backlog H1): capability gates, coordinator backfill, the
//! snapshot invariant, self-grant refusal.
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

async fn account_id_of(pool: &PgPool, email: &str) -> String {
    let (id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM accounts WHERE email = $1")
        .bind(email).fetch_one(pool).await.unwrap();
    id.to_string()
}

async fn snapshot_bytes(app: &axum::Router, project_id: &str, token: &str) -> Vec<u8> {
    let (status, body) = send(app, get(&format!("/api/v1/projects/{project_id}/membership"), Some(token))).await;
    assert_eq!(status, StatusCode::OK);
    let v = as_json(&body);
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, v["payload"].as_str().unwrap()).unwrap()
}

#[sqlx::test]
async fn coordinator_holds_every_cap_and_member_holds_none(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;

    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&coord))).await;
    let caps = as_json(&body)[0]["govCaps"].as_array().unwrap().len();
    assert_eq!(caps, 6, "coordinator backfilled with all six flags");
    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&anna))).await;
    assert_eq!(as_json(&body)[0]["govCaps"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn moderator_can_decide_packages_but_not_thresholds(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (mod_, _) = register_device(&app, &mailer, "mod@example.com", 2, "L").await;
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "P").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    let mod_account = join_and_approve(&app, &coord, &mod_, id, "Mod", "send_receive").await;
    join_and_approve(&app, &coord, &bob, id, "Bob", "send").await;

    // Grant data.moderate to Mod.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{mod_account}"),
        &json!({"govCaps": ["data.moderate"]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Bob announces → pending (approval on, not a moderator).
    let (_, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &json!({
        "packageId": uuid::Uuid::from_bytes([7; 16]).to_string(), "rootHash": "ab".repeat(32),
        "byteSize": 10_i64, "frameCount": 1, "aggregateStats": {}}), Some(&bob))).await;
    let ann = as_json(&body)["id"].as_str().unwrap().to_string();

    // Mod sees the pending row (data.moderate lists everything) and can approve.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&mod_))).await;
    assert_eq!(as_json(&body).as_array().unwrap().len(), 1);
    let (status, _) = send(&app, post(&format!("/api/v1/announcements/{ann}/approve"), &json!({}), Some(&mod_))).await;
    assert_eq!(status, StatusCode::OK);

    // …but cannot publish thresholds.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/thresholds"),
        &json!({"rules": [{"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0}]}), Some(&mod_))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // …and cannot decide join requests.
    let (status, _) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests"), Some(&mod_))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn governance_flags_never_change_the_snapshot(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    let anna_account = join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;

    let before = snapshot_bytes(&app, id, &anna).await;
    let all: Vec<&str> = vec!["members.manage","data.moderate","project.edit","thresholds.edit","invites.manage","posts.write"];
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_account}"),
        &json!({"govCaps": all}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let after = snapshot_bytes(&app, id, &anna).await;
    // `issuedAt` is the one field allowed to differ: strip it before comparing.
    let strip = |b: &[u8]| { let mut v: serde_json::Value = serde_json::from_slice(b).unwrap(); v.as_object_mut().unwrap().remove("issuedAt"); v };
    assert_eq!(strip(&before), strip(&after), "invariant 1: governance flags are invisible on the wire");
    let (version_before, version_after) = (strip(&before)["membershipVersion"].clone(), strip(&after)["membershipVersion"].clone());
    assert_eq!(version_before, version_after, "invariant 2: no version bump for a governance change");
}

#[sqlx::test]
async fn self_grant_and_coordinator_edit_are_refused(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let anna_account = join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    let coord_account = account_id_of(&pool, "coord@example.com").await;

    // Anna (no members.manage) cannot grant herself anything → 403.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_account}"),
        &json!({"govCaps": ["members.manage"]}), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Nobody edits the coordinator's flags — they are ownership → 409.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{coord_account}"),
        &json!({"govCaps": []}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Unknown flag → 400.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_account}"),
        &json!({"govCaps": ["root"]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Delegated members.manage lets Anna manage others but never herself.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_account}"),
        &json!({"govCaps": ["members.manage"]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_account}"),
        &json!({"govCaps": ["members.manage", "data.moderate"]}), Some(&anna))).await;
    assert_eq!(status, StatusCode::CONFLICT, "own flags are never self-edited");
}

#[sqlx::test]
async fn handover_moves_every_flag_to_the_new_coordinator(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let anna_account = join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/handover"),
        &json!({"toAccountId": anna_account}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&anna))).await;
    assert_eq!(as_json(&body)[0]["govCaps"].as_array().unwrap().len(), 6);
    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&coord))).await;
    assert_eq!(as_json(&body)[0]["govCaps"].as_array().unwrap().len(), 0, "the old coordinator is a plain member");
}
```

- [ ] **Step 3: Run the new suite to see it fail**

Run: `cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub && DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test governance`
Expected: compile OK (the tests only use JSON), assertions FAIL (`govCaps` missing → `as_array()` on `Null` panics).

- [ ] **Step 4: Extend `collab_auth.rs`**

```rust
pub const GOV_CAPS: [&str; 6] = [
    "members.manage", "data.moderate", "project.edit",
    "thresholds.edit", "invites.manage", "posts.write",
];

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Member {
    pub account_id: Uuid,
    pub display_name: String,
    pub data_role: String,
    pub is_coordinator: bool,
    pub gov_caps: Vec<String>,
}

impl Member {
    /// The coordinator owns the project and therefore holds every flag.
    pub fn has_cap(&self, cap: &str) -> bool {
        self.is_coordinator || self.gov_caps.iter().any(|c| c == cap)
    }
}

pub fn valid_gov_caps(caps: &[String]) -> bool {
    let mut seen = std::collections::HashSet::new();
    caps.iter().all(|c| GOV_CAPS.contains(&c.as_str()) && seen.insert(c.as_str()))
}

const MEMBER_QUERY: &str = "SELECT account_id, display_name, data_role, is_coordinator, gov_caps \
     FROM project_members WHERE project_id = $1 AND account_id = $2";

/// The caller's membership row if it holds `cap` (or coordinates), else 403.
pub async fn require_cap(db: &PgPool, project_id: Uuid, account_id: Uuid, cap: &str) -> Result<Member, ApiError> {
    let member = require_member(db, project_id, account_id).await?;
    if !member.has_cap(cap) { return Err(forbidden()); }
    Ok(member)
}
pub async fn require_cap_tx(conn: &mut PgConnection, project_id: Uuid, account_id: Uuid, cap: &str) -> Result<Member, ApiError> {
    let member = require_member_tx(conn, project_id, account_id).await?;
    if !member.has_cap(cap) { return Err(forbidden()); }
    Ok(member)
}
```
Keep `require_coordinator` for ownership call sites only (handover, leave-refusal, `update_project`'s `status` change to `closed`, operator paths).

- [ ] **Step 5: Convert the gates**

| Call site | Old | New |
| ---- | ---- | ---- |
| `join_requests::list_join_requests` | `require_coordinator` | `require_cap(.., "members.manage")` |
| `join_requests::approve_join_request` / `reject_join_request` | `require_coordinator` | `require_cap(.., "members.manage")` |
| `members::patch_member` (was `change_member_role`) | `require_coordinator` + `assert_still_coordinator` | `require_cap(.., "members.manage")` on the pool, then `require_cap_tx` inside the tx (same stale-authorization reasoning as `assert_still_coordinator`; keep that helper for handover/remove) |
| `members::remove_member` | `require_coordinator` | `require_cap(.., "members.manage")` + `require_cap_tx` in the tx |
| `thresholds::post_thresholds` | `require_coordinator` | `require_cap(.., "thresholds.edit")` |
| `announcements::approve` / `reject` | `require_coordinator` | `require_cap(.., "data.moderate")` |
| `announcements::list_announcements` `.bind(member.is_coordinator)` | | `.bind(member.has_cap("data.moderate"))` |
| `announcements::report_have` pending rule `member.is_coordinator` | | `member.has_cap("data.moderate")` (the review copy is held by whoever moderates) |
| `announcements::announce` birth state `!member.is_coordinator` | | `!member.has_cap("data.moderate")` (a moderator's own package needs no moderation) |
| `projects::update_project` | `require_coordinator` | `require_cap(.., "project.edit")`; **the `requireApproval = true` guard must look up the COORDINATOR's data role**, not the caller's: `SELECT data_role FROM project_members WHERE project_id=$1 AND is_coordinator` |
| `me::my_projects` pending subquery `AND pm.is_coordinator` | | `AND (pm.is_coordinator OR 'data.moderate' = ANY(pm.gov_caps))`; select `pm.gov_caps` and emit `gov_caps` |

`members::patch_member` body:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberPatch {
    pub data_role: Option<String>,
    pub gov_caps: Option<Vec<String>>,
}
```
Rules, in order: (1) 400 if both absent, if `data_role` invalid, or if `gov_caps` fails `valid_gov_caps`; (2) `require_cap(.., "members.manage")`; (3) inside the tx, lock the target row `FOR UPDATE` — 404 if absent; (4) `gov_caps` present AND target is the coordinator → 409 `"the coordinator holds every flag; hand over to change that"`; `gov_caps` present AND target == caller → 409 `"governance flags cannot be self-edited"`; (5) `data_role` present: the existing coordinator-role/approval guard (`members.rs:105-115`) stays, `UPDATE … data_role`, `bump_membership_version_tx`, event `role_changed`; (6) `gov_caps` present: `UPDATE project_members SET gov_caps = $3`, event `gov_caps_changed` with `{ "from": [...], "to": [...] }`, **no version bump**; (7) 204.

`members::handover`: after the drop-then-raise, `UPDATE project_members SET gov_caps = '{}' WHERE project_id=$1 AND account_id=<old>` and `SET gov_caps = <all six> WHERE account_id=<new>`. Same two statements in `operator::appoint_coordinator` (`operator.rs:360-365`). `join_requests::approve_join_core` inserts with the column default (`'{}'`).

`projects::project_page`: `MemberRow` gains `gov_caps: Vec<String>` (select it), `MemberPublicView` gains `gov_caps: Vec<String>`.

- [ ] **Step 6: Run the whole hub suite**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`
Expected: everything green including `tests/governance.rs` (5 tests). Existing suites that asserted a coordinator-only 403 (e.g. `members_admin.rs`, `join_requests.rs`) still pass because a plain member still has no caps.

- [ ] **Step 7: README**

In `README.md` "API — Collaboration": replace the auth-level legend's **coordinator** line with: `**cap:<flag>** (a member holding that governance flag; the coordinator holds all six: members.manage, data.moderate, project.edit, thresholds.edit, invites.manage, posts.write — flags are hub-only and never enter the signed snapshot)`, update each row's level per the table above, and document `PATCH members/{account_id}` body `{dataRole?, govCaps?}` with the two 409 rules.

- [ ] **Step 8: Commit**

```bash
git add migrations/0013_governance.sql src tests/governance.rs README.md
git commit -m "feat(hub): governance capability plane — gov_caps on members, per-flag gates, coordinator = ownership"
```

---

## Task 2: H2 — former members keep their name and their numbers (hub)

**Files:**
- Create: `migrations/0014_alumni.sql`
- Modify: `src/routes/members.rs:150-185` (`remove_member_core` writes alumni), `:207-260` (`leave_project` takes `{credited}`, writes alumni)
- Modify: `src/routes/join_requests.rs:approve_join_core` (re-join deletes the alumni row)
- Modify: `src/routes/projects.rs:636-760` (progress keyed by account; alumni block; publisher name resolution)
- Modify: `src/routes/announcements.rs:260-285, 335-345` (publisher name resolution)
- Modify: `README.md`
- Test: `tests/alumni.rs`

**Interfaces:**
- Wire: `POST /projects/{id}/leave` body optional `{ "credited": bool }` (default `true`); `DELETE /projects/{id}/members/{account_id}` body optional `{ "credited": bool }`; `ProjectPage.alumni: [{ displayName, dataRole, joinedAt, leftAt, how: "left"|"removed", credited }]`; `ProjectProgress.perMember[]` gains `former: bool`.
- Produces: `pub(crate) const FORMER_MEMBER: &str = "a former member";` in `projects.rs` and the SQL fragment `pub(crate) const PUBLISHER_NAME_SQL: &str` (below) reused by announcements.

- [ ] **Step 1: Migration**

`migrations/0014_alumni.sql`:
```sql
-- 0014_alumni — members who left or were removed keep their identity for
-- credits. A row moves back out when the account re-joins.
CREATE TABLE IF NOT EXISTS project_alumni (
    project_id   uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    account_id   uuid        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    display_name text        NOT NULL,
    data_role    text        NOT NULL,
    joined_at    timestamptz NOT NULL,
    left_at      timestamptz NOT NULL DEFAULT now(),
    how          text        NOT NULL CHECK (how IN ('left','removed')),
    credited     boolean     NOT NULL DEFAULT true,
    PRIMARY KEY (project_id, account_id)
);
```

- [ ] **Step 2: Failing tests**

`tests/alumni.rs`:
```rust
//! Former members (backlog H2): two departed contributors stay two rows,
//! credit survives departure, `credited=false` hides the name, re-join restores.
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn announce(seed: u8, seconds: f64) -> serde_json::Value {
    json!({"packageId": uuid::Uuid::from_bytes([seed; 16]).to_string(), "rootHash": "ab".repeat(32),
           "byteSize": 1000_i64, "frameCount": 3, "aggregateStats": {"integrationSecondsByFilter": {"L": seconds}}})
}

async fn leave(app: &axum::Router, id: &str, token: &str, body: serde_json::Value) {
    let (status, _) = send(app, post(&format!("/api/v1/projects/{id}/leave"), &body, Some(token))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn two_departed_members_stay_two_rows_with_their_hours(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "P").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    join_and_approve(&app, &coord, &bob, id, "Bob", "send").await;
    send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce(1, 3600.0), Some(&anna))).await;
    send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce(2, 1800.0), Some(&bob))).await;

    leave(&app, id, &anna, json!({})).await;
    leave(&app, id, &bob, json!({"credited": false})).await;

    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    let page = as_json(&body);
    let per_member = page["progress"]["perMember"].as_array().unwrap();
    let anna_row = per_member.iter().find(|m| m["displayName"] == "Anna").expect("Anna keeps her name");
    assert_eq!(anna_row["integrationSeconds"], 3600.0);
    assert_eq!(anna_row["former"], true);
    let bob_row = per_member.iter().find(|m| m["displayName"] == "a former member").expect("Bob is anonymised");
    assert_eq!(bob_row["integrationSeconds"], 1800.0, "numbers survive anonymisation");
    assert_eq!(per_member.len(), 2, "two departed members, two rows — never collapsed");

    let alumni = page["alumni"].as_array().unwrap();
    assert_eq!(alumni.len(), 2);
    assert!(alumni.iter().any(|a| a["displayName"] == "Anna" && a["how"] == "left" && a["credited"] == true));
    assert!(alumni.iter().any(|a| a["displayName"] == "a former member" && a["credited"] == false));

    // Packages keep their author too.
    let pkgs = page["packages"].as_array().unwrap();
    assert!(pkgs.iter().any(|p| p["publisherDisplayName"] == "Anna"));
    assert!(pkgs.iter().any(|p| p["publisherDisplayName"] == "a former member"));
}

#[sqlx::test]
async fn removal_writes_alumni_and_rejoin_restores(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let anna_account = join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce(1, 600.0), Some(&anna))).await;

    let req = axum::http::Request::builder().method("DELETE")
        .uri(format!("/api/v1/projects/{id}/members/{anna_account}"))
        .header("authorization", format!("Bearer {coord}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"credited": true}"#)).unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    let alumni = as_json(&body)["alumni"].as_array().unwrap().clone();
    assert_eq!(alumni.len(), 1);
    assert_eq!(alumni[0]["how"], "removed");

    // Re-join: alumni row gone, member row back, history intact.
    join_and_approve(&app, &coord, &anna, id, "Anna again", "send").await;
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    let page = as_json(&body);
    assert!(page["alumni"].as_array().unwrap().is_empty());
    let row = page["progress"]["perMember"].iter().find(|m| m["displayName"] == "Anna again").unwrap();
    assert_eq!(row["former"], false);
    assert_eq!(row["integrationSeconds"], 600.0);
}
```

- [ ] **Step 3: Run to see it fail** — `cargo test --test alumni` → FAIL on `alumni` missing / `perMember.len()`.

- [ ] **Step 4: Implement**

`members.rs`:
```rust
/// Copy a member row into `project_alumni` (upsert — a stale row from an
/// earlier departure is overwritten) then delete it from `project_members`.
/// Runs inside the caller's tx.
pub(crate) async fn retire_member_tx(
    tx: &mut sqlx::PgConnection, project_id: Uuid, account_id: Uuid, how: &str, credited: bool,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO project_alumni (project_id, account_id, display_name, data_role, joined_at, how, credited) \
         SELECT project_id, account_id, display_name, data_role, joined_at, $3, $4 \
           FROM project_members WHERE project_id = $1 AND account_id = $2 \
         ON CONFLICT (project_id, account_id) DO UPDATE \
           SET display_name = EXCLUDED.display_name, data_role = EXCLUDED.data_role, \
               joined_at = EXCLUDED.joined_at, left_at = now(), how = EXCLUDED.how, credited = EXCLUDED.credited",
    ).bind(project_id).bind(account_id).bind(how).bind(credited).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM project_members WHERE project_id = $1 AND account_id = $2")
        .bind(project_id).bind(account_id).execute(&mut *tx).await?;
    Ok(())
}
```
`remove_member_core` gains a `credited: bool` parameter and calls `retire_member_tx(.., "removed", credited)` in place of its `DELETE`; `remove_member` extracts `Option<Json<LeaveBody>>` (axum 0.8 `Option<Json<T>>` is `None` when no JSON body is sent) and passes `body.map(|b| b.credited).unwrap_or(true)`; the operator route passes `true`. `leave_project` extracts the same `Option<Json<LeaveBody>>` and calls `retire_member_tx(.., "left", credited)`.
```rust
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LeaveBody { #[serde(default = "default_true")] pub credited: bool }
fn default_true() -> bool { true }
```
`join_requests::approve_join_core`: before the member `INSERT`, `DELETE FROM project_alumni WHERE project_id = $1 AND account_id = $2`.

`projects.rs`: one SQL fragment used everywhere a publisher is named:
```rust
pub(crate) const FORMER_MEMBER: &str = "a former member";
/// `pm` = current member join, `al` = alumni join; both LEFT JOINed on (project_id, account_id).
pub(crate) const PUBLISHER_NAME_SQL: &str =
    "COALESCE(pm.display_name, CASE WHEN al.credited THEN al.display_name END)";
```
Every query that today has `LEFT JOIN project_members pm ON …` for a publisher adds `LEFT JOIN project_alumni al ON al.project_id = a.project_id AND al.account_id = a.publisher` and selects `{PUBLISHER_NAME_SQL} AS publisher_display_name` (still `Option<String>`, `None` → `FORMER_MEMBER`). Sites: `project_page` packages query, `project_page` stats query, `list_announcements`. The stats query also selects `a.publisher` and `(pm.account_id IS NULL) AS former`; `per_member` becomes `BTreeMap<Uuid, (String /*name*/, bool /*former*/, i64, f64)>` keyed by publisher; `MemberProgress` gains `former: bool`.

`project_page` adds:
```rust
#[derive(sqlx::FromRow)] struct AlumniRow { display_name: String, data_role: String, joined_at: DateTime<Utc>, left_at: DateTime<Utc>, how: String, credited: bool }
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct AlumniPublicView { display_name: String, data_role: String, joined_at: DateTime<Utc>, left_at: DateTime<Utc>, how: String, credited: bool }
```
query `SELECT display_name, data_role, joined_at, left_at, how, credited FROM project_alumni WHERE project_id = $1 ORDER BY left_at DESC`; view maps `credited == false` → `display_name = FORMER_MEMBER`. `ProjectPage` gains `alumni: Vec<AlumniPublicView>`.

- [ ] **Step 5: Run the full hub suite** — expected green; `have_reports.rs`' "ex-member's holds disappear" still holds (holder queries join `project_members`, not alumni).

- [ ] **Step 6: README** — document `alumni` on the page, `former` on `perMember`, the `{credited}` body on leave/remove, and the rule "a former member's name survives departure; `credited=false` replaces it with `a former member` and touches no number".

- [ ] **Step 7: Commit** — `git commit -m "feat(hub): project_alumni — departed members keep identity and credit; progress keyed by account"`

---

## Task 3: H3 — have-reports as soft state, publisher counts as a holder (hub)

**Files:**
- Create: `migrations/0015_have_soft_state.sql` (execution order = filename order: Task 3 is 0015, Task 4 is 0016 — sqlx refuses a database that applied a later version before an earlier one)
- Modify: `src/routes/announcements.rs` (announce stamps device; `report_have_set`; holder list freshness ∪ publisher), `src/routes/projects.rs:636-660` (holder counts), `src/routes/mod.rs` (route)
- Modify: `README.md`
- Test: `tests/have_soft_state.rs`

**Interfaces:**
- Wire: `PUT /api/v1/projects/{id}/have` body `{ "packageIds": [uuid, …] }` → 204. Device token required (400 otherwise, like POST). Same role rule as POST (`send_receive` or `data.moderate`). Semantics: the full set of this project's packages this device holds now; unknown/foreign package ids are ignored with a `warn!(count)`; pending packages are kept only for `data.moderate` holders (others' rows for pending packages are dropped from the set).
- Produces: `pub(crate) const HAVE_REPORT_FRESH_SQL: &str = "hr.reported_at > now() - interval '75 minutes'";` in `announcements.rs`, and `pub(crate) const HOLDER_ONLINE_SQL: &str = "d.last_seen_at > now() - interval '5 minutes'"` (both used by `projects.rs`).

- [ ] **Step 1: Migration**

`migrations/0015_have_soft_state.sql`:
```sql
-- 0015_have_soft_state — have-reports expire unless re-confirmed; the
-- announcing device is a holder by construction.
ALTER TABLE package_announcements ADD COLUMN IF NOT EXISTS publisher_device_id uuid REFERENCES devices (id);
CREATE INDEX IF NOT EXISTS have_reports_reported_at ON have_reports (reported_at);
```

- [ ] **Step 2: Failing tests**

`tests/have_soft_state.rs`:
```rust
//! Have-reports as soft state (D4 T1 / backlog H3).
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn pkg(seed: u8) -> String { uuid::Uuid::from_bytes([seed; 16]).to_string() }
fn announce(seed: u8) -> serde_json::Value {
    json!({"packageId": pkg(seed), "rootHash": "ab".repeat(32), "byteSize": 10_i64, "frameCount": 1, "aggregateStats": {}})
}
async fn holder_count(app: &axum::Router, id: &str, package_id: &str) -> i64 {
    let (_, body) = send(app, get(&format!("/api/v1/projects/{id}"), None)).await;
    as_json(&body)["packages"].as_array().unwrap().iter()
        .find(|p| p["packageId"] == package_id).unwrap()["holderCount"].as_i64().unwrap()
}

#[sqlx::test]
async fn full_set_report_adds_refreshes_and_removes(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, anna_dev) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;
    send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce(1), Some(&coord))).await;
    send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce(2), Some(&coord))).await;

    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/have"), &json!({"packageIds": [pkg(1), pkg(2)]}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(holder_count(&app, id, &pkg(1)).await, 2, "coordinator's publishing device + Anna");
    assert_eq!(holder_count(&app, id, &pkg(2)).await, 2);

    // Age Anna's row past the window, then re-confirm only package 1.
    sqlx::query("UPDATE have_reports SET reported_at = now() - interval '2 hours' WHERE device_id = $1")
        .bind(uuid::Uuid::parse_str(&anna_dev).unwrap()).execute(&pool).await.unwrap();
    assert_eq!(holder_count(&app, id, &pkg(1)).await, 1, "a stale report does not count");
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/have"), &json!({"packageIds": [pkg(1)]}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(holder_count(&app, id, &pkg(1)).await, 2, "re-confirmed → fresh again");
    assert_eq!(holder_count(&app, id, &pkg(2)).await, 1, "absent from the set → row removed");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM have_reports WHERE device_id = $1")
        .bind(uuid::Uuid::parse_str(&anna_dev).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(rows, 1);

    // Unknown ids are ignored, not an error; a portal session (no device) is 400.
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/have"), &json!({"packageIds": [pkg(9)]}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let session = portal_sign_in(&app, &mailer, "anna@example.com").await;
    let req = axum::http::Request::builder().method("PUT").uri(format!("/api/v1/projects/{id}/have"))
        .header("content-type", "application/json").header("cookie", format!("athenaeum_portal_session={session}"))
        .header("x-portal-csrf", "1").body(axum::body::Body::from(r#"{"packageIds": []}"#)).unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn contributor_publisher_is_a_holder_of_its_own_package(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (kostya, _) = register_device(&app, &mailer, "k@example.com", 2, "Mini").await;
    let project = create_project_via(&app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &kostya, id, "Kostya", "send").await;
    let (_, body) = send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce(1), Some(&kostya))).await;
    let ann = as_json(&body)["id"].as_str().unwrap().to_string();
    send(&app, post(&format!("/api/v1/announcements/{ann}/approve"), &json!({}), Some(&coord))).await;

    assert_eq!(holder_count(&app, id, &pkg(1)).await, 1, "the publishing device counts before any replica exists");
    // …and it is offered as a source in the member view, relay url and all.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/announcements"), Some(&coord))).await;
    let holders = as_json(&body)[0]["holders"].as_array().unwrap().clone();
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0]["pubkey"], json!(pubkey_b64(2)));
    assert_eq!(holders[0]["displayName"], "Kostya");
    // A send-only publisher still cannot PUT (nothing to hold but its own).
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/have"), &json!({"packageIds": [pkg(1)]}), Some(&kostya))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn publisher_stops_counting_after_leaving_or_revocation(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, anna_dev) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    send(&app, post(&format!("/api/v1/projects/{id}/announcements"), &announce(1), Some(&anna))).await;
    assert_eq!(holder_count(&app, id, &pkg(1)).await, 1);
    let (status, _) = send(&app, post(&format!("/api/v1/devices/{anna_dev}/revoke"), &json!({}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(holder_count(&app, id, &pkg(1)).await, 0, "a revoked publishing device is no holder");
}
```
(Note: `POST /devices/{id}/revoke` with the device's own token — check `tests/device_registry.rs` for the exact status it returns and adjust the assertion; the point is the count drops to 0.)

- [ ] **Step 3: Run to see it fail** — `cargo test --test have_soft_state` → 404/405 on the PUT, `holderCount` 1 vs 2.

- [ ] **Step 4: Implement**

`announcements::announce`: stamp `publisher_device_id = auth.device_id` (`Option<Uuid>`, `None` for a portal session — only devices publish in practice) in the `INSERT`.

`announcements::report_have_set`:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HaveSet { pub package_ids: Vec<Uuid> }

#[tracing::instrument(skip_all)]
pub async fn report_have_set(
    State(state): State<AppState>, Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>, Json(body): Json<HaveSet>,
) -> Result<axum::http::StatusCode, ApiError> {
    let Some(device_id) = auth.device_id else {
        return Err(ApiError::bad_request("a device token is required to report held packages"));
    };
    if body.package_ids.len() > 10_000 { return Err(ApiError::bad_request("packageIds must have at most 10000 entries")); }
    let member = require_member(&state.db, id, auth.account_id).await?;
    let moderates = member.has_cap("data.moderate");
    if member.data_role != crate::routes::projects::ROLE_SEND_RECEIVE && !moderates {
        return Err(ApiError::Status(axum::http::StatusCode::FORBIDDEN));
    }
    let mut tx = state.db.begin().await?;
    // Resolve to announcement ids of THIS project in a holdable state.
    let holdable: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM package_announcements \
         WHERE project_id = $1 AND package_id = ANY($2) \
           AND (state = 'published' OR (state = 'pending' AND $3))",
    ).bind(id).bind(&body.package_ids).bind(moderates).fetch_all(&mut *tx).await?;
    let ignored = body.package_ids.len().saturating_sub(holdable.len());
    if ignored > 0 { tracing::warn!(project_id = %id, device_id = %device_id, count = ignored, "have set: unknown or unholdable package ids ignored"); }
    sqlx::query(
        "INSERT INTO have_reports (announcement_id, device_id) SELECT unnest($1::uuid[]), $2 \
         ON CONFLICT (announcement_id, device_id) DO UPDATE SET reported_at = now()",
    ).bind(&holdable).bind(device_id).execute(&mut *tx).await?;
    sqlx::query(
        "DELETE FROM have_reports hr USING package_announcements a \
         WHERE hr.announcement_id = a.id AND a.project_id = $1 AND hr.device_id = $2 \
           AND NOT (hr.announcement_id = ANY($3))",
    ).bind(id).bind(device_id).bind(&holdable).execute(&mut *tx).await?;
    tx.commit().await?;
    tracing::info!(project_id = %id, device_id = %device_id, count = holdable.len(), "have set reported");
    Ok(axum::http::StatusCode::NO_CONTENT)
}
```
`report_have` (POST, unchanged semantics) switches to `ON CONFLICT (announcement_id, device_id) DO UPDATE SET reported_at = now()` so an ingest refreshes too.

Holder SQL — the union of fresh reports and the publishing device — in ONE shape used by both readers. `announcements.rs`:
```rust
pub(crate) const HAVE_REPORT_FRESH_SQL: &str = "hr.reported_at > now() - interval '75 minutes'";
pub(crate) const HOLDER_ONLINE_SQL: &str = "d.last_seen_at > now() - interval '5 minutes'";
/// Rows `(announcement_id, device_id)` of every current holder: fresh
/// have-reports ∪ the announcing device. Both legs require an unrevoked device
/// whose account is still a member. Bind $1 = project id.
pub(crate) const HOLDER_DEVICES_SQL: &str =
    "SELECT hr.announcement_id, hr.device_id FROM have_reports hr \
       JOIN package_announcements a ON a.id = hr.announcement_id AND a.project_id = $1 \
      WHERE hr.reported_at > now() - interval '75 minutes' \
     UNION \
     SELECT a.id, a.publisher_device_id FROM package_announcements a \
      WHERE a.project_id = $1 AND a.publisher_device_id IS NOT NULL";
```
`list_announcements` holder query becomes `FROM ({HOLDER_DEVICES_SQL}) h JOIN package_announcements a ON a.id = h.announcement_id JOIN devices d ON d.id = h.device_id AND d.revoked_at IS NULL JOIN project_members pm ON pm.project_id = a.project_id AND pm.account_id = d.account_id`. `projects::project_page` packages query replaces `LEFT JOIN have_reports hr ON hr.announcement_id = a.id LEFT JOIN devices d ON d.id = hr.device_id …` with `LEFT JOIN ({HOLDER_DEVICES_SQL}) h ON h.announcement_id = a.id LEFT JOIN devices d ON d.id = h.device_id AND d.revoked_at IS NULL AND EXISTS (…member…)` — `count(DISTINCT d.id)` and `count(DISTINCT d.id) FILTER (WHERE {HOLDER_ONLINE_SQL})`.

`routes/mod.rs`: `.route("/api/v1/projects/{id}/have", axum::routing::put(announcements::report_have_set))` in `account_protected`.

- [ ] **Step 5: Full hub suite green.** `have_reports.rs` must keep passing: its "ex-member's holds disappear" case relies on the member join, which the union keeps.

- [ ] **Step 6: README** — add the PUT row, the 75-minute freshness rule ("three 20-minute polls plus jitter"), and "the announcing device is always a holder of its own package while it is an unrevoked device of a current member".

- [ ] **Step 7: Commit** — `git commit -m "feat(hub): have-reports are soft state — PUT full set, 75-minute freshness, publisher device counts as holder"`

---

## Task 4: H4 — the project's door (hub)

**Files:**
- Create: `migrations/0016_join_policy.sql`
- Modify: `src/routes/projects.rs` (`ProjectRow`/`PROJECT_COLUMNS`/`ProjectView`/`DirectoryRow`/`DirectoryItem`/`CreateProject`/`UpdateProject`/`create_project_core`/`update_project`)
- Modify: `src/routes/join_requests.rs:38-140` (`create_join_request`)
- Modify: `README.md`
- Test: `tests/join_policy.rs`

**Interfaces:**
- Wire: `projects.joinPolicy: "open"|"request"|"invite"` (default `request`), `projects.defaultDataRole: "send"|"send_receive"` (default `send`) on `ProjectView` and `DirectoryItem`; accepted on create and on `PATCH /projects/{id}` (both fields require `invites.manage`; the other fields keep `project.edit`). `POST /projects/{id}/join-requests` returns `{ "id": uuid|null, "joined": bool, "dataRole": string|null }`: on `open` the caller is a member at once (`id: null, joined: true`), on `invite` → 403 `{"error": "this project is invite-only"}`.
- Constants: `pub(crate) const JOIN_POLICIES: [&str; 3] = ["open", "request", "invite"];` in `projects.rs`.

- [ ] **Step 1: Migration**

`migrations/0016_join_policy.sql`:
```sql
-- 0016_join_policy — who may come in, and which data role a newcomer gets.
ALTER TABLE projects ADD COLUMN IF NOT EXISTS join_policy text NOT NULL DEFAULT 'request';
ALTER TABLE projects ADD COLUMN IF NOT EXISTS default_data_role text NOT NULL DEFAULT 'send';
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'projects_join_policy_check') THEN
    ALTER TABLE projects ADD CONSTRAINT projects_join_policy_check CHECK (join_policy IN ('open','request','invite'));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'projects_default_data_role_check') THEN
    ALTER TABLE projects ADD CONSTRAINT projects_default_data_role_check CHECK (default_data_role IN ('send','send_receive'));
  END IF;
END $$;
```

- [ ] **Step 2: Failing tests**

`tests/join_policy.rs`:
```rust
//! The door (backlog H4): open joins at once, invite refuses, switching the
//! policy touches nobody already in.
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn open_door_joins_immediately_with_the_default_role(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    assert_eq!(project["joinPolicy"], "request");
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}"),
        &json!({"joinPolicy": "open", "defaultDataRole": "send_receive"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests"),
        &json!({"displayName": "Anna", "desiredRole": "send"}), Some(&anna))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v = as_json(&body);
    assert_eq!(v["joined"], true);
    assert!(v["id"].is_null());
    assert_eq!(v["dataRole"], "send_receive", "the door's default role wins over the request");
    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&anna))).await;
    assert_eq!(as_json(&body)[0]["dataRole"], "send_receive");
    // Snapshot version bumped (a membership change).
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert_eq!(as_json(&body)["project"]["membershipVersion"], 2);
}

#[sqlx::test]
async fn invite_door_refuses_requests_but_shows_the_page(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"joinPolicy": "invite"}), Some(&coord))).await;
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests"),
        &json!({"displayName": "Anna", "desiredRole": "send"}), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(as_json(&body)["error"], "this project is invite-only");
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(as_json(&body)["project"]["joinPolicy"], "invite");
    let (_, body) = send(&app, get("/api/v1/projects", None)).await;
    assert_eq!(as_json(&body)[0]["joinPolicy"], "invite");
}

#[sqlx::test]
async fn door_requires_invites_manage_and_keeps_existing_members(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let anna_account = join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    // project.edit alone is not enough for the door.
    send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_account}"), &json!({"govCaps": ["project.edit"]}), Some(&coord))).await;
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"joinPolicy": "open"}), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"description": "x"}), Some(&anna))).await;
    assert_eq!(status, StatusCode::OK);
    // Bad values → 400.
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"joinPolicy": "sesame"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Flipping the door does not touch Anna.
    send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"joinPolicy": "invite"}), Some(&coord))).await;
    let (_, body) = send(&app, get("/api/v1/me/projects", Some(&anna))).await;
    assert_eq!(as_json(&body).as_array().unwrap().len(), 1);
}
```

- [ ] **Step 3: Run to see it fail** — `cargo test --test join_policy`.

- [ ] **Step 4: Implement**

`projects.rs`: add `join_policy: String`, `default_data_role: String` to `ProjectRow`, `PROJECT_COLUMNS`, `ProjectView` (`join_policy`, `default_data_role`), `DirectoryRow`/`DirectoryItem` (+ the directory `SELECT`), `CreateProject` (`#[serde(default = "default_join_policy")] pub join_policy: String`, `#[serde(default = "default_data_role")] pub default_data_role: String`), validate both in `create_project_core` (400 `"joinPolicy must be open, request or invite"` / `"defaultDataRole must be send or send_receive"`), bind them in the `INSERT`. `UpdateProject` gains `pub join_policy: Option<String>, pub default_data_role: Option<String>`; `update_project`: validate values; if either is `Some`, `require_cap(.., "invites.manage")` in addition to `project.edit` (a caller with `invites.manage` but not `project.edit` may ONLY send those two fields — implement as: `let door_only = body.description.is_none() && body.goals.is_none() && body.chat_link.is_none() && body.data_policy_text.is_none() && body.require_approval.is_none() && body.status.is_none(); if !door_only { require_cap(project.edit) } if door_fields { require_cap(invites.manage) }`); `UPDATE … join_policy = COALESCE($9, join_policy), default_data_role = COALESCE($10, default_data_role)`; event `door_changed` with `{from, to}` when the policy actually changes.

`join_requests::create_join_request`: after the closed/hidden check, read `join_policy, default_data_role` in the same `SELECT`; `invite` → `Err(ApiError::Message(StatusCode::FORBIDDEN, "this project is invite-only".into()))`; `open` → in one tx: `DELETE FROM project_alumni …` (Task 2's re-join rule), `INSERT INTO project_members (project_id, account_id, display_name, data_role) VALUES ($1,$2,$3,$4)` with `default_data_role` (409 `already a member` on the PK violation), `bump_membership_version_tx`, `record_event_tx(.., "member_joined", Some(account), Some(account), json!({"dataRole": role, "via": "open_door"}))`, commit, return `JoinRequestCreated { id: None, joined: true, data_role: Some(role) }` and skip the coordinator nudge; `request` → today's path, returning `JoinRequestCreated { id: Some(id), joined: false, data_role: None }`.
```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestCreated { pub id: Option<Uuid>, pub joined: bool, pub data_role: Option<String> }
```
`common::join_and_approve` still reads `["id"]` — unchanged for `request` projects.

- [ ] **Step 5: Full hub suite green.**

- [ ] **Step 6: README** — `joinPolicy`/`defaultDataRole` on the project, the three door behaviours, the response shape of the join endpoint.

- [ ] **Step 7: Commit** — `git commit -m "feat(hub): join policy — open door joins at once, invite-only refuses, default data role"`

---

## Task 5: A1 — the app re-confirms its full held set every pass (app)

**Files:**
- Modify: `crates/athenaeum-core/src/collab/hub_client.rs:378-400` (add `report_have_set`), tests at the bottom of the file
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs:1005-1100` (extract the coverage predicate), `:2466-2620` (call it per project in the pass)
- Test: `hub_client.rs` `#[cfg(test)]` (wiremock), `collab_exchange.rs` tests module (the pass seam)

**Interfaces:**
- Produces:
  ```rust
  // collab/hub_client.rs
  impl CollabClient { pub async fn report_have_set(&self, token: &str, project_id: &str, package_ids: &[String]) -> Result<(), AccountClientError> }  // PUT /projects/{id}/have {packageIds}
  // api/collab_exchange.rs
  pub(crate) fn held_package_ids_for_project(conn: &rusqlite::Connection, project_id: &str) -> anyhow::Result<Vec<String>>   // local_status == "complete" && manifest fully local
  pub async fn report_held_set(ctx: &ServiceContext, project_id: &str) -> Result<usize, ApiError>  // Ok(count) ; signed out ⇒ Ok(0)
  ```
- Consumes: hub Task 3's `PUT /api/v1/projects/{id}/have`.

- [ ] **Step 1: Failing client test** (append to the `tests` module in `hub_client.rs`, next to `report_have_204_returns_ok`):

```rust
#[tokio::test]
async fn report_have_set_puts_full_package_list() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/proj-1/have"))
        .and(header("authorization", "Bearer tok"))
        .and(body_json(serde_json::json!({"packageIds": ["p1", "p2"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server).await;
    let client = CollabClient::new(server.uri()).unwrap();
    client.report_have_set("tok", "proj-1", &["p1".to_string(), "p2".to_string()]).await.unwrap();
}

#[tokio::test]
async fn report_have_set_403_is_forbidden_not_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("PUT")).and(path("/api/v1/projects/proj-1/have"))
        .respond_with(ResponseTemplate::new(403)).mount(&server).await;
    let client = CollabClient::new(server.uri()).unwrap();
    let err = client.report_have_set("tok", "proj-1", &[]).await.unwrap_err();
    assert!(!matches!(err, AccountClientError::Unauthorized), "403 = role, not a dead token: {err:?}");
}
```
(Check how `report_have_400_non_device_maps_to_network_not_unauthorized` names the non-401 variant and mirror it.)

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::hub_client::tests::report_have_set` → FAIL (no method).

- [ ] **Step 3: Implement the client method** — copy `report_have`'s shape: `.put(self.url(&format!("/projects/{project_id}/have"))).bearer_auth(token).json(&serde_json::json!({"packageIds": package_ids}))`, 204/200 → Ok, 401 → `Unauthorized`, else `unexpected(s, resp)`.

- [ ] **Step 4: Failing pass test** — in `collab_exchange.rs`'s test module, next to `poll_*` tests, find the existing `run_auto_sync_pass` test harness (the tests that inject a recording `download` closure) and add a test that: seeds two packages for one `send_receive` project, one with `local_status = "complete"` and a fully-local manifest, one `downloading`; mounts a wiremock `PUT /api/v1/projects/<id>/have` expecting exactly `{"packageIds": ["<complete id>"]}` `.expect(1)`; sets the project's `auto_replicate = false` (the report must run even when replication is off); runs `run_auto_sync_pass(ctx, None, false, download)`; asserts the mock was hit and `download` was NOT called. Use the same fixture helpers those poll tests use for the ctx/hub credentials (`hub_credentials` reads the account row — the existing poll tests show how it is seeded).

- [ ] **Step 5: Implement**
  - Extract from `report_have_after_ingest` the predicate into `fn package_fully_held(conn, row: &PackageRow) -> anyhow::Result<bool>` (complete status + `manifest_fully_local`), reuse it there.
  - `held_package_ids_for_project`: `list_packages` filtered by `package_fully_held`.
  - `report_held_set`: credentials → `None` ⇒ `Ok(0)`; ids → `CollabClient::report_have_set`; `Err(Unauthorized)` propagates; any other error is returned as `ApiError` (the caller logs and continues). `info!(project_id, count, "held set reported")`.
  - In `run_auto_sync_pass`, after the `role_allows` check and BEFORE the `auto_on` gate: `if role_allows { if let Err(e) = report_held_set(ctx, &project.project_id).await { warn!(project_id, error = %e, "held-set report failed; continuing") } }`. Keep the existing `debug!("project skipped")` for `!auto_on` after it.

- [ ] **Step 6: Run** `cargo test -p athenaeum-core --lib collab_exchange` and `--lib collab::hub_client` → green. `cargo check -p athenaeum-core --no-default-features` (Perseus surface) → green.

- [ ] **Step 7: Commit** — `git commit -m "collab: every auto-sync pass re-confirms the full held package set (PUT /projects/{id}/have)"`

---

## Task 6: A7 — iroh 1.0.3 → 1.2.0 with a reproducible bench (app)

**Files:**
- Create: `crates/athenaeum-core/examples/swarm_bench.rs`
- Modify: `crates/athenaeum-core/Cargo.toml:75-85`, `Cargo.lock`, whatever `sharing/iroh/*.rs` and `crates/perseus/src/run.rs` no longer compile
- Test: the whole existing `sharing::iroh::tests` + `wire_golden_tests` modules are the regression net

**Interfaces:** none new. `examples/swarm_bench.rs` CLI: `cargo run -p athenaeum-core --release --example swarm_bench -- --label before --files 12 --size-mb 32 --out target/swarm-bench` → writes `<out>/<label>.json` `{ "label", "iroh", "files", "size_mb", "single_mb_s", "multi_two_providers_mb_s", "elapsed_single_ms", "elapsed_multi_ms" }` and prints the same line.

- [ ] **Step 1: Write the bench FIRST on the current iroh** (so "before" is measured with the same code): model it on `multi_fetch_uses_both_providers` in `sharing/iroh/tests.rs:2200-2280` — three `SharedIrohNode::bind(dir, RelayMode::Disabled)` nodes, `add_peer_ticket` both ways, `build_many_file_package`-style payload (copy that helper into the example — examples cannot see `#[cfg(test)]` code), `serve` on A (single run) then on A and B (multi run), `fetch_collection_multi` from C, wall-clock both, MB/s = total bytes / elapsed. Gate subscribers behind `RUST_LOG` like the other probes. Print with `println!` (examples are exempt from the zero-print rule).
- [ ] **Step 2: Run it on the current tree** — `cargo run -p athenaeum-core --release --example swarm_bench -- --label before` → JSON written; note the numbers in the commit message.
- [ ] **Step 3: Bump** — `iroh = "=1.2.0"` in `crates/athenaeum-core/Cargo.toml`; `cargo update -p iroh -p noq -p noq-proto -p noq-udp -p iroh-relay -p iroh-base -p iroh-dns` (add whichever siblings cargo names); `cargo check -p athenaeum-core --all-targets` and `cargo check -p perseus`. Fix API drift where it appears — expected candidates: `Endpoint::builder(presets::…)` argument shape, `Connection::paths()`/`Path` accessors in `describe_conn_path` (`sharing/iroh/mod.rs`), `Endpoint::metrics().socket` counter names in `telemetry.rs`, relay-mode/`RelayMap` constructors in `node.rs` and `perseus/src/run.rs`. Do not silence anything with `#[allow]`; if a metric was renamed, rename our field's source, not its meaning.
- [ ] **Step 4: Regression net** — `cargo test -p athenaeum-core --lib sharing::` (all of `sharing::iroh::tests`, `sharing::tests`, `sharing::wire_golden_tests`) → green; `cargo test -p perseus` → green.
- [ ] **Step 5: Bench after** — same command with `--label after`. Both JSON files are committed under `docs/superpowers/research/2026-09-20-iroh-1.2-bench/` (copy them there) together with a 10-line `README.md` stating machine, command, and the two numbers. If `multi_two_providers_mb_s` dropped by more than 10 %, STOP and report — do not merge a regression.
- [ ] **Step 6: Commit** — `git commit -m "deps: iroh 1.2.0 (noq 1.3) + swarm_bench example; before/after numbers in docs/superpowers/research"`

---

## Task 7: A2a — our own assignment loop: deadline and stall ceiling (app)

**Files:**
- Create: `crates/athenaeum-core/src/sharing/iroh/assign.rs`
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (declare `pub(crate) mod assign;`), `crates/athenaeum-core/src/sharing/iroh/blobs.rs:1044-1200` (phase 2 dispatch), `sharing/iroh/node.rs:2128-2170` (pass the mode)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs` (new tests after `multi_fetch_survives_a_provider_dying_mid_transfer`)

**Interfaces:**
- Produces:
  ```rust
  // sharing/iroh/assign.rs
  pub(crate) const MAX_IN_FLIGHT: usize = 32;                // same as the stock fan-out
  pub(crate) const STALL_HARD_LIMIT: Duration = Duration::from_secs(20);   // D4 §6: no growth in bytes_read for this long ⇒ provider failure
  pub(crate) const EVICT_AFTER_FAILURES: u32 = 3;            // D4 §6
  pub(crate) const BACKOFF_BASE: Duration = Duration::from_millis(500);    // ×2, max 6 attempts (D4 §6)

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub(crate) enum SwarmFetchMode { Stock, Assigned }         // Stock = iroh-blobs SplitStrategy::Split (the fallback)

  /// Per-provider transfer facts, the ground truth the stock downloader throws away.
  #[derive(Debug, Clone, Default)]
  pub(crate) struct ProviderStats { pub bytes: u64, pub children: u32, pub failures: u32, pub elapsed: Duration }

  #[derive(Debug, Clone, Default)]
  pub(crate) struct AssignmentReport { pub per_provider: HashMap<EndpointId, ProviderStats>, pub stalls: u32, pub hedges: u32, pub hedge_bytes: u64 }

  pub(crate) struct AssignmentOptions { pub stall_hard_limit: Duration, pub hedging: bool /* Task 8 */, pub telemetry: ProviderTelemetrySink }

  pub(crate) async fn fetch_children_assigned(
      store: &Store, endpoint: &Endpoint, providers: Vec<EndpointId>,
      children: Vec<(u64 /*index*/, Hash)>, opts: AssignmentOptions,
  ) -> anyhow::Result<AssignmentReport>
  ```
- Consumes: `store.remote()` (`Remote::local_for_request`, `LocalInfo::{is_complete, missing, local_bytes}`, `Remote::execute_get(conn, GetRequest) -> GetProgress` whose `.stream()` yields `GetProgressItem::{Progress(u64), Done(Stats), Error(GetError)}`), `iroh_util::connection_pool::{ConnectionPool, Options}` (`ConnectionPool::new(endpoint, ALPN, Options::default())`, `get_or_connect(id).await -> Result<Connection>`), `iroh_blobs::protocol::{GetRequest, ChunkRanges}` (per child: `GetRequest::builder().child(index, ChunkRanges::all()).build(root)` is what the stock split builds; use the same, and `local.missing()` after `local_for_request`).

- [ ] **Step 1: Read the stock loop once** — `~/.cargo/registry/src/*/iroh-blobs-0.103.0/src/api/downloader.rs:496-560` (`execute_get`) and `:620-700` (`handle_download_split_impl`). Ours replaces exactly that per-child loop; phase 1 (root + meta) and the per-file observers in `fetch_collection_multi` stay as they are.

- [ ] **Step 2: Failing tests** (in `tests.rs`, reuse `bind_disabled`, `build_many_file_package`, `assert_package_landed`, `sent_bytes`, `SERVED_PAYLOAD_FLOOR`):

```rust
/// A provider that ACCEPTS and then trickles must not pin the fetch: the
/// assignment loop's stall ceiling reassigns its children and the fetch
/// finishes on the healthy provider inside a bounded time.
#[tokio::test]
async fn assigned_fetch_reassigns_a_trickling_provider() {
    // Same three-node setup as multi_fetch_uses_both_providers. Provider B is
    // throttled to the pacer floor (see upload_pacer_limits_real_transfer_wall_clock
    // for the setter) so its children make progress but far too slowly;
    // provider A is unthrottled.
    // FILES = 8, 256 KiB each; opts.stall_hard_limit = Duration::from_millis(1500)
    // (the test override — production keeps STALL_HARD_LIMIT).
    // Assert: fetch completes within 30 s; assert_package_landed; the report's
    // per_provider[A].bytes > 6 × 256 KiB (A served the bulk) and
    // per_provider[B].failures >= 1 (B was stalled out at least once).
}

/// Every provider dead (connection refused) ⇒ the loop exhausts its backoff
/// ladder and returns Err inside a bounded time instead of spinning.
#[tokio::test]
async fn assigned_fetch_fails_fast_when_every_provider_is_dead() {
    // Bind A, serve, take A's node id, then a.shutdown().await BEFORE the fetch.
    // Assert: Err within 6 attempts × (500 ms · 2^k) ≈ 32 s of wall clock; the
    // in-flight tag is still present (partial bytes are kept, same as stock).
}

/// The mode flag: Stock and Assigned both land the identical package, and the
/// Assigned report accounts every byte the providers' socket counters saw.
#[tokio::test]
async fn assigned_fetch_report_matches_provider_send_counters() {
    // Two healthy providers, FILES = 14. Bracket sent_bytes for A and B; run
    // Assigned. Assert per_provider[X].bytes ≤ sent delta of X (framing overhead)
    // and per_provider[X].bytes ≥ 0.9 × sent delta of X for both, and
    // sum(children) == FILES.
}
```
Write the bodies out in full when implementing — the comments above are the specification of each assertion, not placeholders to leave behind.

- [ ] **Step 3: Run** `cargo test -p athenaeum-core --lib sharing::iroh::tests::assigned_` → FAIL (no such function).

- [ ] **Step 4: Implement `assign.rs`**

Structure (≈ 250 lines):
```rust
struct ProviderState { failures: u32, next_try: Option<Instant>, inflight: u32, stats: ProviderStats }

pub(crate) async fn fetch_children_assigned(store, endpoint, providers, children, opts) -> Result<AssignmentReport> {
    anyhow::ensure!(!providers.is_empty(), "assignment loop needs at least one provider");
    let pool = ConnectionPool::new(endpoint.clone(), iroh_blobs::ALPN, Options::default());
    let remote = store.remote();
    let states: Arc<Mutex<HashMap<EndpointId, ProviderState>>> = …;
    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT));
    let mut set = tokio::task::JoinSet::new();
    for (index, hash) in children {
        let permit = sem.clone().acquire_owned().await?;
        set.spawn(run_child(pool.clone(), remote.clone(), states.clone(), providers.clone(), index, hash, opts.clone(), permit));
    }
    while let Some(res) = set.join_next().await { res??; }   // first child error aborts the rest (JoinSet drop aborts)
    Ok(report_from(states))
}

async fn run_child(…) -> Result<()> {
    let request = GetRequest::builder().child(index, ChunkRanges::all()).build(root);  // root = the hash-seq root; pass it in
    let mut attempt = 0u32;
    loop {
        let local = remote.local_for_request(request.clone()).await?;
        if local.is_complete() { return Ok(()); }
        let Some(provider) = pick_provider(&states, &providers) else {
            // every provider in backoff: wait for the earliest next_try, or fail after the ladder
            attempt += 1;
            if attempt > 6 { anyhow::bail!("child {index}: every provider exhausted"); }
            tokio::time::sleep(earliest_wait(&states)).await; continue;
        };
        opts.telemetry(ProviderEvent::Trying(*provider.as_bytes()));
        mark_inflight(&states, provider, +1);
        let outcome = transfer_once(&pool, &remote, provider, local.missing(), local.local_bytes(), opts.stall_hard_limit).await;
        mark_inflight(&states, provider, -1);
        match outcome {
            Ok(stats) => { record_success(&states, provider, stats); return Ok(()); }
            Err(TransferFault::Stalled { bytes }) | Err(TransferFault::Failed { bytes, .. }) => {
                record_failure(&states, provider, bytes);   // failures += 1; ≥ EVICT_AFTER_FAILURES ⇒ next_try = now + BACKOFF_BASE · 2^(failures−EVICT_AFTER_FAILURES), capped at 6 rungs
                opts.telemetry(ProviderEvent::Failed(*provider.as_bytes()));
                // loop: local_for_request recomputes the missing range ⇒ byte-level resume on the next provider
            }
        }
    }
}

/// One `execute_get` with a progress watchdog. Dropping `progress` mid-stream is the cancellation: the
/// underlying QUIC stream is reset when the future is dropped.
async fn transfer_once(pool, remote, provider, request: GetRequest, local_bytes: u64, stall: Duration) -> Result<Stats, TransferFault> {
    let conn = pool.get_or_connect(provider).await.map_err(|e| TransferFault::Failed { bytes: 0, error: e.into() })?;
    let progress = remote.execute_get(conn, request);
    let mut stream = std::pin::pin!(progress.stream());
    let mut last_bytes = 0u64; let mut last_growth = Instant::now();
    loop {
        match tokio::time::timeout(stall.saturating_sub(last_growth.elapsed()).max(Duration::from_millis(50)), stream.next()).await {
            Ok(Some(GetProgressItem::Progress(b))) => { if b > last_bytes { last_bytes = b; last_growth = Instant::now(); } }
            Ok(Some(GetProgressItem::Done(stats))) => return Ok(stats),
            Ok(Some(GetProgressItem::Error(e))) => return Err(TransferFault::Failed { bytes: last_bytes, error: e.into() }),
            Ok(None) => return Err(TransferFault::Failed { bytes: last_bytes, error: anyhow!("get stream ended without Done") }),
            Err(_elapsed) => if last_growth.elapsed() >= stall { return Err(TransferFault::Stalled { bytes: last_bytes }); }
        }
    }
}
```
`pick_provider` for A2: among providers whose `next_try` is `None` or past, the one with the smallest `inflight` (ties → fewest failures, then list order). This is the placeholder A4's `RankedProviders` replaces; keep it in one function so the swap is one edit. `report_from` folds `stats` per provider plus `stalls` (count of `Stalled`).

`blobs.rs` phase 2:
```rust
match mode {
    SwarmFetchMode::Stock => { /* today's download_with_opts(... SplitStrategy::Split) block, verbatim */ }
    SwarmFetchMode::Assigned => {
        let children: Vec<(u64, Hash)> = collection.iter().enumerate().map(|(i, (_, h))| (i as u64 + 1, *h)).collect(); // child 0 is the meta blob already fetched in phase 1 — verify the index convention against `Collection::load` before trusting `+1`
        let report = assign::fetch_children_assigned(store, endpoint, providers, root_hash, children, AssignmentOptions { stall_hard_limit: STALL_HARD_LIMIT, hedging: false, telemetry }).await?;
        tracing::info!(root_hash = %root_hash, providers = report.per_provider.len(), stalls = report.stalls, "assigned fetch complete");
    }
}
```
`fetch_collection_multi` gains a `mode: SwarmFetchMode` parameter; `role_fetch_multi` passes `SwarmFetchMode::Assigned` by default — with an env escape hatch `ATHENAEUM_SWARM_FETCH=stock` read once (`std::sync::OnceLock`) so the stock path stays reachable without a rebuild (D4 §9 "switching between them is one flag"). The batch `FetchEvent::Batch` progress in Assigned mode comes from summing the per-file observers' latest `bytes_done` (the observers already exist; keep a `HashMap<String, u64>` behind a mutex the observer tasks update, and emit `Batch` every `FETCH_PROGRESS_MIN_INTERVAL` from a small ticker task) — never from the get streams (D4 T7: `store.observe()` is the progress truth).

- [ ] **Step 5: Run** the three new tests + the two existing `multi_fetch_*` tests (they now run in Assigned mode) + `fetch_rejects_traversal_entry_names`, `in_flight_tag_protects_partial_collection_across_gc`, `successful_fetch_clears_in_flight_tag` → green. Then the full `sharing::` filter.

- [ ] **Step 6: Commit** — `git commit -m "swarm: own assignment loop over store.remote() — stall ceiling, backoff ladder, per-provider stats (SwarmFetchMode::Assigned, stock path kept)"`

---

## Task 8: A2b — hedging with a token bucket and a proven cancel path (app)

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/assign.rs` (hedge decision, range split, bucket), `blobs.rs` (`hedging: true`)
- Test: `sharing/iroh/tests.rs`

**Interfaces:**
- Produces in `assign.rs`:
  ```rust
  pub(crate) const HEDGE_BUDGET_RATIO: f64 = 0.05;     // ≈ 5 % extra bytes (gRPC hedging)
  pub(crate) const HEDGE_EXPECTED_MULTIPLIER: f64 = 2.0;  // hedge at max(p95 completions, 2 × expected)
  pub(crate) struct HedgeBudget { tokens: f64, cap: f64 }  // earn HEDGE_BUDGET_RATIO × bytes on every completed child; hedge only while tokens > cap / 2 and tokens ≥ the hedged range's bytes; cap = HEDGE_BUDGET_RATIO × total collection bytes
  pub(crate) struct CompletionWindow(VecDeque<Duration>)   // last 32 completions; fn p95(&self) -> Option<Duration>
  ```
- Hedge mechanics (avoids two writers on the same chunks): when a child's transfer has run longer than `max(p95, 2 × expected)` where `expected = missing_bytes / ewma_goodput(provider)` (goodput = bytes / elapsed of that provider's completed transfers; unmeasured provider ⇒ the median of measured ones, else no hedge), and the budget allows, and another eligible provider exists: recompute `local_for_request`, take the missing `ChunkRanges`, split them at the midpoint by chunk count into `front`/`back`, and start a second `transfer_once` for `back` on the other provider. The primary keeps its request. When EITHER finishes: recompute `local`; if complete → drop the other (cancel) and return; if the hedge finished first and the primary is still below the midpoint → drop the primary (cancel), reassign `local.missing()` to the hedge's provider; if the primary finished first → drop the hedge. Charge the bucket with the `back` range's bytes at hedge start; refund what the loser had NOT yet transferred when it is cancelled.

- [ ] **Step 1: Prove the store tolerates two disjoint-range writers on one blob** — a test `two_disjoint_range_gets_on_one_blob_verify` that serves one 4 MiB blob from A and B, runs `remote.execute_get` for the front half from A and the back half from B concurrently (build the two `GetRequest`s with `ChunkRanges` from `local.missing()` split at the midpoint), awaits both, then asserts `local_for_request(all).is_complete()` and the exported bytes hash-match. If this test cannot be made to pass, STOP: hedging must then take the "cancel and reassign whole missing range" form instead (deadline-only), and the plan is amended before proceeding.

- [ ] **Step 2: Failing hedge tests**
```rust
/// A provider at 1 KB/s (pacer floor is 100 KB/s — set the child size so the
/// expected time on the healthy provider is ≪ the slow one's) triggers a hedge
/// before the stall ceiling, and the fetch finishes early.
#[tokio::test] async fn hedge_fires_before_the_stall_ceiling_on_a_slow_provider() { /* FILES = 4 × 2 MiB; stall_hard_limit = 60 s; assert elapsed < 20 s, report.hedges >= 1 */ }
/// The loser is REALLY cancelled: the slow provider's socket send delta after
/// the hedge won stays under one child's size (no whole-frame duplicate).
#[tokio::test] async fn hedge_cancels_the_loser_and_bounds_duplicate_bytes() { /* assert sent_bytes(slow) − before < 1 child + SERVED_PAYLOAD_FLOOR; report.hedge_bytes ≤ 0.6 × total */ }
/// The bucket: with every child slow (both providers throttled equally) no
/// hedge is allowed once tokens drop below half — hedges ≤ 1 on 8 children.
#[tokio::test] async fn hedge_budget_stops_a_storm() { /* assert report.hedges <= 1 */ }
/// The 20 s ceiling still fires with the budget exhausted (independent rules).
#[tokio::test] async fn stall_ceiling_is_independent_of_the_hedge_budget() { /* provider B accepts then sends nothing: use a serve dir with the child blob deleted from B's store after import — see in_flight_tag test for store surgery; assert per_provider[B].failures ≥ 1 with hedging on and budget 0 */ }
```

- [ ] **Step 3: Implement** the hedge branch inside `run_child` (a `tokio::select!` over the primary's `transfer_once` and an optional hedge future armed by a `tokio::time::sleep_until(hedge_at)` guard), `HedgeBudget`, `CompletionWindow`, EWMA goodput per provider (`α = 0.25`, warm-up `min(1/n, α)` — D4 §6, tunable), and the `report.hedges` / `hedge_bytes` accounting. Turn `hedging: true` in `blobs.rs`.

- [ ] **Step 4: Run** the four hedge tests + Task 7's three + the full `sharing::` filter → green.

- [ ] **Step 5: Log line** — on every hedge: `debug!(child = index, provider = %hex, hedge_provider = %hex, missing_bytes, expected_ms, p95_ms, "swarm hedge armed")`; on cancel: `debug!(child, loser = %hex, refunded_bytes, "swarm hedge loser cancelled")`. Add `hedge_provider`, `refunded_bytes`, `missing_bytes` to the logging spec dictionary (`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, "Unified event schema") in the same commit.

- [ ] **Step 6: Commit** — `git commit -m "swarm: hedged assignments — p95/2×expected trigger, midpoint range split, token bucket, cancel path pinned by socket counters"`

---

## Wave-1 acceptance (orchestrator, after Task 8)

- Hub: `DATABASE_URL=… cargo test` all green; `cargo build --release` (portal untouched this wave, `portal/dist` may be stale — that is fine for the test build).
- App (worktree): `cargo build --workspace --all-targets`, `cargo test -p athenaeum-core` (all targets, incl. `tests/ts_contract.rs`), `cargo test -p perseus`, `cargo check -p athenaeum-core --no-default-features`, `npm run build:web` unaffected (no TS change) — still run `npx tsc --noEmit`.
- Merge `collab-v2` → local `main` in both repos (hub: also carries `operator-console`; that is intended — the test hub already soaks it).
- Report to the owner: the before/after bench numbers, the D4 §7 T4 assertions that passed, and what the test-hub deploy would need (`hub_artifact_ref=collab-v2`).
