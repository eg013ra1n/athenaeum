# Collaboration v2 — Wave 2 (entry and approvals) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A person finds a project on the portal, applies (or walks through an open door or an invite link), gets a human answer — with a question loop, a reason on decline, a follow button — and sees the whole thing on a portal that reads as one product with the desktop app.

**Architecture:** Hub side, five additive tasks on `athenaeum-hub`: the join-request state machine grows `asked`/`declined`/`withdrawn`/`stale` plus a private question thread and per-project blocks (H6); invite links with hashed one-shot tokens (H5); watchers as a table that never touches membership (H7); account profiles with a public handle and a candidate aggregate for the review card (H11); transactional mail templates behind the existing `Mailer` plus an hourly in-process scheduler that marks stale requests and sends the weekly digest (H13); one tiny public stats endpoint for the landing (H0). Portal side, a Tailwind v4 design system with the app's Nord tokens and the real Athenaeum logo (P1), then the pages in the prototype's order: landing (P2), catalog (P3), join funnel + invite landing (P5), the applicant's request page (P6), profile + edit (P9), `/me` in three states (P10) — every existing page restyled onto the same system. The backlog's H9 (`needs` filter, "Needs your filter tonight") is wave 3; the catalog and landing ship without those two pieces and gain them then.

**Tech Stack:** Rust 1.96 · axum 0.8 + sqlx 0.8 (Postgres 16) · lettre (existing `Mailer`) · React 18 + react-router 7 + Tailwind v4 (`@tailwindcss/vite`) + Vite 6 · Vitest + Testing Library (new dev deps) · `#[sqlx::test]` per-test databases on the hub.

**Spec:** `docs/superpowers/plans/2026-09-19-collab-v2-backlog.md` (H5, H6, H7, H11, H13, P1, P2, P3, P5, P6, P9, P10; the three invariants), design source for every page: `athenaeum-hub/docs/design/2026-09-19-portal-v2-prototype.html` (line ranges cited per task), lifecycle: `athenaeum-hub/docs/design/2026-09-19-collab-lifecycle-4-members.md`. Wave 1 is merged (hub `681cd04`, migrations 0013–0016; app `1bafd81d`) — this plan builds on `Member::has_cap`/`require_cap`, `lock_member_rows`, `join_member_tx`, `forget_alumni_tx`, `project_alumni`, `HOLDER_DEVICES_SQL`, `join_policy`/`default_data_role`.

## Global Constraints

- **Invariant 1** — nothing this wave adds enters the signed membership snapshot: `src/routes/snapshots.rs`'s SELECT stays untouched; watchers, invites, profiles, blocks and request threads are hub-only. Task 3 pins "a watcher is not in the snapshot".
- **Invariant 2** — `membership_version` bumps only on membership changes (an invite redemption IS a join → bump via `join_member_tx`; follow/unfollow, profile edits, request messages never bump).
- **Invariant 3** — a device's own reports never authorize; unchanged.
- **Minimal exposure on public surfaces** (Task 1 of wave 1): the public project page and the public profile expose no e-mail, no account id, no node id, no direct address. Handles are public by design; a profile is public only once its owner has set a handle.
- **Repos & branches:** hub + portal work on `athenaeum-hub` branch `collab-v2` at `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub` (HEAD 681cd04 == local main). No app-repo code changes in this wave (the wire stays additive: new fields only, nothing renamed or removed; the desktop client decodes `MyProjectWire`, `ProjectPageWire`, `AnnouncementWire` without `deny_unknown_fields`).
- **Hub tests:** `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test` from the hub root (`docker compose up -d postgres` if `athenaeum-hub-postgres-1` is not running). Migrations are append-only and idempotent, numbering continues at `0017`. The one exception to "never edit an applied migration" does not apply here — every wave-2 migration is new.
- **Portal:** `cd portal && npm ci && npm run check && npm test && npm run build` are the gates; `portal/dist` is what the hub binary embeds. The SPA calls only `/api/v1/*` with the existing `request()` helper (cookie session + `x-portal-csrf: 1` on writes). No new npm runtime dependency beyond `react`/`react-dom`/`react-router-dom` without a stated reason in the task; dev deps for Vitest are allowed.
- **Wire casing:** camelCase JSON (`#[serde(rename_all = "camelCase")]`), snake_case columns. Error shapes: body-less 403 for membership/authorization refusals, `{"error": msg}` for 400/404/409/410 with fixed strings, nothing that enumerates membership.
- **Logging:** `tracing` only; message = short stable phrase, data in snake_case fields (`info!(project_id = %id, request_id = %rid, "join request asked")`). New field names (`request_id`, `invite_id`, `handle`, `watchers`) are added to the dictionary in the app repo's `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` by the controller at wave end (the hub has no copy of the spec).
- **Mail is best-effort:** every notification runs in a `tokio::spawn`ed task off the request path, logs `warn!` on failure, and never fails the triggering request (the pattern `create_join_request` already uses). Tests capture mail through `tests/common::CaptureMailer`.
- **Copy and layout come from the prototype** (English, verbatim where the prototype has text); UI strings stay English. No third-party astro software named anywhere.
- **Never a spinner-only page:** every page renders a not-found / signed-out / empty state (the prototype's `.empty` block) rather than "Loading…" forever.
- **Commits:** as the repo's configured user, one commit per task (+ fix-round commits), trailers:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01EjvVJVxscAK1QAMYuCRUsa
  ```
  No push.

---

## File map

**Hub (`athenaeum-hub`)**

| File | Responsibility after this wave |
| ---- | ---- |
| `migrations/0017_join_request_lifecycle.sql` | status set widened, `decline_reason`/`offer_follow`/`last_activity_at`, `join_request_messages`, `project_blocks`, the open-uniqueness index re-created over `open`+`asked` |
| `migrations/0018_invites.sql` | `project_invites` |
| `migrations/0019_watchers.sql` | `project_watchers` |
| `migrations/0020_profiles.sql` | `account_profiles` |
| `migrations/0021_hub_meta.sql` | `hub_meta` KV (scheduler bookkeeping) |
| `src/routes/join_requests.rs` | ask / reply / withdraw / decline-with-reason / detail / my-requests; managers' list includes `asked` |
| `src/routes/invites.rs` (new) | create / list / revoke / public lookup / redeem |
| `src/routes/watchers.rs` (new) | watch / unwatch / my-watching |
| `src/routes/profiles.rs` (new) | my profile get/put, public profile, candidate aggregate |
| `src/routes/stats.rs` (new) | `GET /api/v1/stats` |
| `src/routes/projects.rs` | page gains `door` block (`medianDecisionDays`, `openRequests`, `watcherCount`, `watching`) and `handle` on members; directory gains `watcherCount` |
| `src/mail.rs` (new) | the five templates as pure functions + `send_*` helpers over `Mailer` |
| `src/scheduler.rs` (new) | hourly tick: stale marking, weekly digest; `hub_meta` bookkeeping |
| `src/config.rs` | `HUB_PUBLIC_URL` |
| `src/routes/mod.rs`, `src/lib.rs` | routes registered, scheduler spawned |
| `README.md` | every new route/field |
| `tests/{join_lifecycle,invites,watchers,profiles,mail_and_scheduler,stats}.rs` | new suites |

**Portal (`athenaeum-hub/portal`)**

| File | Responsibility |
| ---- | ---- |
| `src/styles.css` | Tailwind v4 `@theme` Nord tokens, light-theme override, base typography |
| `public/logo.svg` | the Athenaeum logo (copied from the docs site) |
| `src/ui/*.tsx` | the design-system primitives (Button, Card, Pill, Field, Select, Textarea, Meter, FilterTag, Tabs, Note, Avatar, Steps, KeyValue, EmptyState, PageHeader, Switch, Starfield) |
| `src/theme.ts` | theme state (system → stored preference), `useTheme` |
| `src/Layout.tsx` | header (logo, nav, theme toggle, sign-in/avatar), footer |
| `src/api.ts`, `src/types.ts`, `src/useMe.ts` | typed helpers for every new endpoint |
| `src/pages/{Landing,Directory,ProjectPage,Join,Invite,Application,Profile,EditProfile,Me,SignIn,NewProject,Admin,Operator}.tsx` | pages (Admin/Operator restyled only — their rebuild is wave 3 P7/P11) |
| `src/App.tsx` | routes: `/`, `/projects`, `/p/:slug`, `/p/:slug/join`, `/p/:slug/application`, `/i/:token`, `/u/:handle`, `/me`, `/me/profile`, `/new`, `/p/:slug/admin`, `/operator`, `/signin` |
| `vitest.config.ts`, `src/**/*.test.tsx` | Vitest + Testing Library |

---

## Task 1: H6 — join-request lifecycle: question loop, withdraw, decline with reason, blocks (hub)

**Files:**
- Create: `migrations/0017_join_request_lifecycle.sql`
- Modify: `src/routes/join_requests.rs` (whole module), `src/routes/mod.rs`, `src/routes/operator.rs` (`decide_join` passes a reason), `README.md`
- Test: `tests/join_lifecycle.rs`

**Interfaces (Produces):**
- States: `open | asked | approved | declined | withdrawn | stale`. `asked` = a manager asked, waiting on the applicant; `stale` = `asked` with no activity for 30 days (Task 6's scheduler sets it; this task exposes the pure transition `mark_stale_requests(conn, now) -> Result<u64>`).
- Routes (all `account_protected`):
  - `GET /api/v1/projects/{id}/join-requests` (`members.manage`) → `[JoinRequestView]` for `open` **and** `asked`, oldest first; `JoinRequestView` gains `status`, `accountId` (managers see it — they need it for the candidate call), `lastActivityAt`, `thread: [MessageView]`.
  - `GET /api/v1/projects/{id}/join-requests/{req}` (the applicant OR `members.manage`) → `JoinRequestDetail { id, projectId, projectSlug, projectTitle, status, desiredRole, message, createdAt, decidedAt, declineReason, offerFollow, thread }` — the applicant never sees `accountId`s of others; `MessageView { id, authorDisplayName, mine: bool, body, createdAt }`.
  - `POST …/join-requests/{req}/ask {question}` (`members.manage`; 1..=1000 chars) → 204; only from `open` (409 otherwise); inserts a message, `status='asked'`, `last_activity_at=now()`; event `join_asked`.
  - `POST …/join-requests/{req}/reply {body}` (the applicant; 1..=1000) → 204; only from `asked` (409); message, `status='open'`, `last_activity_at`; event `join_replied`.
  - `POST …/join-requests/{req}/withdraw` (the applicant) → 204; from `open|asked` (409); event `join_withdrawn`.
  - `POST …/join-requests/{req}/reject {reason, offerFollow?, block?}` (`members.manage`; reason 1..=500) → 204; from `open|asked`; `status='declined'`, `decline_reason`, `offer_follow` (default true); `block=true` inserts `project_blocks (project_id, account_id, blocked_by, reason)`; event `join_declined {reason, blocked}`. (Route path unchanged so the operator console keeps working; the operator's `decide_join` reject passes `reason: "declined by the instance operator"`.)
  - `GET /api/v1/me/join-requests` → `[JoinRequestDetail]` for the caller, newest first, all states.
- `create_join_request`: a `project_blocks` row for the caller → body-less 403; the open-uniqueness index now covers `open|asked` (409 `"a join request is already open"` when either exists).
- `pub(crate) async fn mark_stale_requests(conn: &mut PgConnection, now: DateTime<Utc>) -> Result<u64, sqlx::Error>` — `UPDATE join_requests SET status='stale' WHERE status='asked' AND last_activity_at < $1 - interval '30 days' RETURNING id`, records `join_stale` events. Task 6 calls it.
- Mail hooks (Task 5 wires the bodies; this task calls `crate::mail::*` stubs? No — to keep this task independent, it keeps today's inline nudge for "received" and emits NO mail for ask/reply/decision; Task 5 replaces the inline nudge and adds the rest).

- [ ] **Step 1: Migration**

`migrations/0017_join_request_lifecycle.sql`:
```sql
-- 0017_join_request_lifecycle — the question loop, withdrawal, decline reasons, blocks.
ALTER TABLE join_requests DROP CONSTRAINT IF EXISTS join_requests_status_check;
UPDATE join_requests SET status = 'declined' WHERE status = 'rejected';
ALTER TABLE join_requests ADD CONSTRAINT join_requests_status_check
    CHECK (status IN ('open','asked','approved','declined','withdrawn','stale'));
ALTER TABLE join_requests ADD COLUMN IF NOT EXISTS decline_reason    text;
ALTER TABLE join_requests ADD COLUMN IF NOT EXISTS offer_follow      boolean     NOT NULL DEFAULT true;
ALTER TABLE join_requests ADD COLUMN IF NOT EXISTS last_activity_at  timestamptz NOT NULL DEFAULT now();
-- One live request per account per project, where live = open OR asked.
DROP INDEX IF EXISTS one_open_join_request;
CREATE UNIQUE INDEX IF NOT EXISTS one_live_join_request
    ON join_requests (project_id, account_id)
    WHERE status IN ('open','asked');

CREATE TABLE IF NOT EXISTS join_request_messages (
    id                bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_id        uuid        NOT NULL REFERENCES join_requests (id) ON DELETE CASCADE,
    author_account_id uuid        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    body              text        NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS join_request_messages_request ON join_request_messages (request_id, id);

CREATE TABLE IF NOT EXISTS project_blocks (
    project_id  uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    account_id  uuid        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    blocked_by  uuid        REFERENCES accounts (id),
    reason      text        NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, account_id)
);
```

- [ ] **Step 2: Failing tests** — `tests/join_lifecycle.rs`:

```rust
//! Join-request lifecycle (backlog H6): ask/reply parks and un-parks, withdraw,
//! decline carries a reason and can block, stale after 30 days, the applicant's
//! own view never leaks other account ids.
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

async fn open_request(app: &axum::Router, id: &str, token: &str, name: &str) -> String {
    let (status, body) = send(app, post(&format!("/api/v1/projects/{id}/join-requests"),
        &json!({"displayName": name, "desiredRole": "send", "message": "hi"}), Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    as_json(&body)["id"].as_str().unwrap().to_string()
}

#[sqlx::test]
async fn ask_parks_reply_unparks_and_the_count_follows(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let req = open_request(&app, id, &anna, "Anna").await;

    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req}/ask"),
        &json!({"question": "Mono rig or OSC?"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests"), Some(&coord))).await;
    let list = as_json(&body);
    assert_eq!(list[0]["status"], "asked");
    assert_eq!(list[0]["thread"].as_array().unwrap().len(), 1);
    // Asking twice is a 409 — the ball is with the applicant.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req}/ask"),
        &json!({"question": "again?"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // The applicant sees the thread with `mine` flags and no account ids.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests/{req}"), Some(&anna))).await;
    let detail = as_json(&body);
    assert_eq!(detail["status"], "asked");
    assert_eq!(detail["thread"][0]["mine"], false);
    assert_eq!(detail["thread"][0]["authorDisplayName"], "Coord");
    assert!(detail.to_string().find("accountId").is_none(), "applicant view carries no account ids");

    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req}/reply"),
        &json!({"body": "Mono, 3 nm."}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests"), Some(&coord))).await;
    assert_eq!(as_json(&body)[0]["status"], "open");
    assert_eq!(as_json(&body)[0]["thread"].as_array().unwrap().len(), 2);
    // A stranger can neither read nor reply.
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "P").await;
    let (status, _) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests/{req}"), Some(&bob))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req}/reply"), &json!({"body": "x"}), Some(&bob))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn decline_carries_reason_and_block_refuses_a_new_request(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let req = open_request(&app, id, &anna, "Anna").await;
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req}/reject"),
        &json!({"reason": "Narrowband only, 3 nm or tighter.", "offerFollow": true, "block": true}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, get("/api/v1/me/join-requests", Some(&anna))).await;
    let mine = as_json(&body);
    assert_eq!(mine[0]["status"], "declined");
    assert_eq!(mine[0]["declineReason"], "Narrowband only, 3 nm or tighter.");
    assert_eq!(mine[0]["offerFollow"], true);
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests"),
        &json!({"displayName": "Anna", "desiredRole": "send"}), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "blocked accounts get a body-less 403");
    // A reason is mandatory.
    let (carl, _) = register_device(&app, &mailer, "carl@example.com", 3, "P").await;
    let req2 = open_request(&app, id, &carl, "Carl").await;
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req2}/reject"), &json!({}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn withdraw_frees_the_slot_and_stale_is_a_transition(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "D").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "L").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let req = open_request(&app, id, &anna, "Anna").await;
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req}/withdraw"), &json!({}), Some(&anna))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req}/withdraw"), &json!({}), Some(&anna))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // The slot is free again.
    let req2 = open_request(&app, id, &anna, "Anna").await;
    send(&app, post(&format!("/api/v1/projects/{id}/join-requests/{req2}/ask"), &json!({"question": "?"}), Some(&coord))).await;
    sqlx::query("UPDATE join_requests SET last_activity_at = now() - interval '31 days' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&req2).unwrap()).execute(&pool).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let n = athenaeum_hub::routes::join_requests::mark_stale_requests(&mut conn, chrono::Utc::now()).await.unwrap();
    assert_eq!(n, 1);
    let (_, body) = send(&app, get("/api/v1/me/join-requests", Some(&anna))).await;
    assert_eq!(as_json(&body)[0]["status"], "stale");
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/join-requests"), Some(&coord))).await;
    assert!(as_json(&body).as_array().unwrap().is_empty(), "stale leaves the managers' queue");
}
```

- [ ] **Step 3: Run** `cargo test --test join_lifecycle` → FAIL (404s, missing fields).

- [ ] **Step 4: Implement** — module shape:
  - `fn load_request(conn, project_id, req_id) -> Option<Row{id, account_id, status, …}>`; `fn require_applicant_or_manager(db, project_id, req_row, caller) -> Result<Role{Applicant|Manager}>` — applicant = `row.account_id == caller`; manager = `require_cap(.., "members.manage")` succeeds; else body-less 403 (a stranger must not learn whether the request exists).
  - transitions as `UPDATE … WHERE id=$1 AND project_id=$2 AND status = ANY($3) RETURNING account_id`, 409 `"join request is not open"` / `"…not awaiting a reply"` / `"…already decided"` on zero rows; message inserts in the same tx; `last_activity_at = now()` on every transition.
  - `list_open_join_requests` → `WHERE status IN ('open','asked')`, thread loaded in one query `WHERE request_id = ANY($1) ORDER BY id` and grouped; `authorDisplayName` = the author's `project_members.display_name` if a member, else the request's `display_name` (the applicant), else `"a former member"`.
  - `create_join_request`: `SELECT 1 FROM project_blocks WHERE project_id=$1 AND account_id=$2` → 403 before the door dispatch; the unique-violation arm now names `one_live_join_request`.
  - `mark_stale_requests` as specified, `pub` on the module so the test and Task 6 can call it (`pub mod join_requests` is already public via `routes`).
  - Operator `decide_join` (reject arm): passes `reason: "declined by the instance operator"`, no block.
- [ ] **Step 5: Full suite** — `join_requests.rs`, `join_policy.rs`, `operator_*` keep passing (the operator reject now writes a reason).
- [ ] **Step 6: README** — the state table (verbatim from the prototype's "States a request can be in", lines 1376–1384), the six routes, the block rule.
- [ ] **Step 7: Commit** — `feat(hub): join-request lifecycle — question thread, withdraw, decline with reason, per-project blocks, stale transition`

---

## Task 2: H5 — invite links (hub)

**Files:**
- Create: `migrations/0018_invites.sql`, `src/routes/invites.rs`
- Modify: `src/routes/mod.rs`, `src/security.rs` (`generate_invite_token`), `src/config.rs` (`HUB_PUBLIC_URL`), `README.md`
- Test: `tests/invites.rs`

**Interfaces (Produces):**
- `config.public_url: Option<String>` from `HUB_PUBLIC_URL` (trimmed, no trailing slash; `AppState.public_url: Arc<Option<String>>` via `with_public_url`). Used here for the invite URL and by Task 5 for mail links.
- `security::generate_invite_token() -> String` — 24 CSPRNG bytes, base64url without padding (32 chars); stored as `hash_token(token)`.
- Routes:
  - `POST /api/v1/projects/{id}/invites {grantsDataRole, maxUses?, expiresInDays?}` (`invites.manage`) → `InviteCreated { id, token, url, grantsDataRole, maxUses, expiresAt }` — `token` appears here ONCE. `maxUses` 1..=1000 default 25; `expiresInDays` 1..=365 default 14 (`null` = never); `url` = `{public_url or ""}/i/{token}`.
  - `GET /api/v1/projects/{id}/invites` (`invites.manage`) → `[InviteView { id, grantsDataRole, maxUses, usedCount, expiresAt, revokedAt, createdAt, createdByDisplayName }]` (no tokens).
  - `POST /api/v1/projects/{id}/invites/{invite}/revoke` (`invites.manage`) → 204 (idempotent).
  - `GET /api/v1/invites/{token}` (public) → `InvitePublic { project: { slug, title, targetName }, grantsDataRole, state: "valid"|"revoked"|"expired"|"exhausted" }`; unknown token → 404.
  - `POST /api/v1/invites/{token}/redeem {displayName}` (account; 1..=60) → `{ projectId, projectSlug, dataRole }` 200; unknown 404; revoked/expired 410 `{"error": "invite link revoked"|"invite link expired"}`; exhausted 409 `"invite link exhausted"`; already a member 409 `"already a member"` (no second row, `used_count` untouched); blocked 403. One transaction: `UPDATE project_invites SET used_count = used_count + 1 WHERE id=$1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now()) AND used_count < max_uses RETURNING …` (zero rows ⇒ re-read to pick the right error), then `join_member_tx(.., grants_data_role, actor = redeemer, json!({"dataRole", "via": "invite", "inviteId"}))`. A blocked account: 403 before the UPDATE.

- [ ] **Step 1: Migration**
```sql
-- 0018_invites — invite links carry the role they grant and skip the queue.
CREATE TABLE IF NOT EXISTS project_invites (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id       uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    token_hash       text        UNIQUE NOT NULL,
    grants_data_role text        NOT NULL CHECK (grants_data_role IN ('send','send_receive')),
    max_uses         integer     NOT NULL CHECK (max_uses BETWEEN 1 AND 1000),
    used_count       integer     NOT NULL DEFAULT 0,
    expires_at       timestamptz,
    revoked_at       timestamptz,
    created_by       uuid        NOT NULL REFERENCES accounts (id),
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS project_invites_project ON project_invites (project_id, created_at DESC);
```
- [ ] **Step 2: Failing tests** — `tests/invites.rs`: (a) `create_redeem_and_the_snapshot_reflects_the_join`: coordinator creates (`grantsDataRole: send_receive, maxUses: 2`), the response carries a 32-char token and a `url` ending in `/i/<token>`; Anna redeems with a device token → 200 `dataRole == send_receive`, `membershipVersion` bumped by 1, her device is in the snapshot's nodes; a second redeem by Anna → 409 `already a member`, `usedCount` stays 1 in the list; the list never contains the token; (b) `exhausted_expired_and_revoked_give_distinct_errors`: three links — `maxUses: 1` used by Bob then Carl → 409 exhausted; `expiresInDays: 1` aged via SQL → 410 expired; revoked → 410 revoked; the public lookup returns matching `state`s and a random token → 404; (c) `invites_manage_alone_can_run_the_door`: an `invites.manage`-only delegate creates and revokes; a `project.edit`-only member gets 403 on create; (d) `blocked_account_cannot_redeem`: a `project_blocks` row → 403.
- [ ] **Step 3: Run** → FAIL. **Step 4: Implement** per the interfaces (pool check `require_cap` for the three managed routes; redeem is account-level and validates `displayName` like `create_join_request`). **Step 5: Full suite.** **Step 6: README** (routes + the "a link carries the role it grants" paragraph, prototype lines 1139–1150). **Step 7: Commit** — `feat(hub): invite links — hashed one-shot tokens, per-link role, uses and expiry, distinct refusals`

---

## Task 3: H7 + H0 — watchers, the door block on the page, public stats (hub)

**Files:**
- Create: `migrations/0019_watchers.sql`, `src/routes/watchers.rs`, `src/routes/stats.rs`
- Modify: `src/routes/projects.rs` (page `door` block, `handle` placeholder `None` until Task 4, directory `watcherCount`), `src/routes/mod.rs`, `README.md`
- Test: `tests/watchers.rs`, `tests/stats.rs`

**Interfaces (Produces):**
- `project_watchers (project_id, account_id, created_at, PK(project_id, account_id))`.
- `POST /api/v1/projects/{id}/watch` / `DELETE /api/v1/projects/{id}/watch` (account) → 204, idempotent; hidden project → 404; a member may watch too (harmless).
- `GET /api/v1/me/watching` → `[{ id, slug, title, status, joinPolicy, watcherCount, latestPost: null }]` (`latestPost` is a wave-3 H8 field, emitted as `null` now so the type is stable).
- `ProjectPage` gains `door: { joinPolicy, defaultDataRole, medianDecisionDays: number|null, openRequests, watcherCount, watching: bool, applied: {requestId, status}|null }` — `medianDecisionDays` = median over requests decided (`approved|declined`) in the last 90 days of `(decided_at − created_at)` in days, one decimal, `null` when fewer than 3; `openRequests` = count of `open|asked`; `watching`/`applied` computed for the viewer via `optional_account` (anonymous ⇒ `false`/`null`).
- `DirectoryItem` gains `watcherCount`.
- `GET /api/v1/stats` (public) → `{ activeProjects, imagers, hoursPooled, bytesPublished }` — `imagers` = distinct accounts in `project_members` of non-hidden projects; `hoursPooled` = Σ published `integrationSecondsByFilter` / 3600 (one decimal); `bytesPublished` = Σ `byte_size` of published announcements. Cheap enough to compute per request (four queries); no cache.
- Test pin for invariant 1: a watcher who is not a member is absent from the membership snapshot and from `members`.

- [ ] **Step 1: Migration** — `CREATE TABLE IF NOT EXISTS project_watchers (project_id uuid NOT NULL REFERENCES projects (id) ON DELETE CASCADE, account_id uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE, created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (project_id, account_id)); CREATE INDEX IF NOT EXISTS project_watchers_account ON project_watchers (account_id);`
- [ ] **Step 2: Failing tests** — `tests/watchers.rs`: (a) `follow_counts_and_never_enters_the_snapshot`: Anna (portal session, no device) follows → page `door.watcherCount == 1`, `watching == true` for her session and `false` anonymously; `/me/watching` lists the project; the coordinator's snapshot has ONE member; unfollow → 0; double follow is 204; (b) `door_block_reports_median_and_open_count`: three requests decided at controlled `created_at`/`decided_at` (SQL) → `medianDecisionDays` matches; two open + one asked → `openRequests == 3`; an applicant's own page shows `applied.status`. `tests/stats.rs`: one project, two members, one published announcement with `integrationSecondsByFilter {"L": 7200}` and `byteSize 10_000` → `{activeProjects: 1, imagers: 2, hoursPooled: 2.0, bytesPublished: 10000}`; a hidden project counts nowhere.
- [ ] **Step 3–7:** implement, full suite, README (`watch` routes, the `door` block, `/stats`), commit — `feat(hub): watchers, the project page's door block, public stats`

---

## Task 4: H11 — profiles and the candidate card (hub)

**Files:**
- Create: `migrations/0020_profiles.sql`, `src/routes/profiles.rs`
- Modify: `src/routes/projects.rs` (`MemberPublicView.handle`, alumni `handle` when credited), `src/routes/join_requests.rs` (candidate route), `src/routes/mod.rs`, `README.md`
- Test: `tests/profiles.rs`

**Interfaces (Produces):**
- `account_profiles (account_id PK → accounts, handle text UNIQUE, display_name text NOT NULL, location text NOT NULL DEFAULT '', sky text NOT NULL DEFAULT '', bio text NOT NULL DEFAULT '', gear text NOT NULL DEFAULT '', links jsonb NOT NULL DEFAULT '[]', updated_at timestamptz NOT NULL DEFAULT now())`. `sky ∈ {"", "bortle_1_3", "bortle_4_5", "bortle_6_7", "bortle_8_9", "remote"}` (the prototype's five options, lines 692–694); `handle` matches `^[a-z0-9][a-z0-9-]{2,29}$`, lowercase, unique (409 `"handle already taken"`); `links` = up to 5 `{label, url}` with `https?://` URLs (same validator as `chat_link`), `bio` ≤ 1000, `gear` ≤ 1000, `location` ≤ 80, `displayName` 1..=60.
- `GET /api/v1/me/profile` → `Profile { handle: string|null, displayName, location, sky, bio, gear, links, accountSince }` — a never-saved profile returns `handle: null, displayName: ""` and empty fields (200, not 404).
- `PUT /api/v1/me/profile` (full replace, all fields; `handle` optional) → 200 `Profile`.
- `GET /api/v1/profiles/{handle}` (public) → `PublicProfile { handle, displayName, location, sky, bio, gear, links, accountSince, stats: { projects, hoursContributed, framesPublished, packagesAccepted }, projects: [{ slug, title, dataRole, coordinator, joinedAt, hours, frames }] }` — only NON-hidden projects, `hours`/`frames` from that account's published announcements per project, `packagesAccepted` = published announcements by the account across non-hidden projects. 404 for an unknown handle.
- `GET /api/v1/projects/{id}/join-requests/{req}/candidate` (`members.manage`) → `Candidate { profile: PublicProfile-without-stats|null (null when no profile saved), history: { projects: [{ slug, title, hours, frames }], accepted, declined, holding, lastSeenAt, activeDevices, accountSince } }` — `accepted/declined` = the account's announcements by state across all projects; `holding` = distinct announcements with a FRESH have-report (`HAVE_REPORT_FRESH_SQL`) from any unrevoked device of the account; `lastSeenAt` = max `devices.last_seen_at`; `activeDevices` = unrevoked `athenaeum` devices. (Ruling: "how stably online" is `lastSeenAt` + `activeDevices` — the hub keeps no presence history.)
- `MemberPublicView.handle: Option<String>` (LEFT JOIN `account_profiles`); `AlumniPublicView.handle` when `credited`.

- [ ] **Step 1: Migration** (as above). **Step 2: Failing tests** — `tests/profiles.rs`: (a) `profile_round_trip_and_handle_rules`: GET on a fresh account is empty/200; PUT with `handle: "Anna-Ivanova"` → 400 (uppercase); `"anna-ivanova"` → 200; a second account claiming it → 409; public GET by handle → 200 with the fields, no `email`, no `accountId`; unknown handle → 404; (b) `public_profile_aggregates_only_non_hidden_projects`: Anna publishes 3600 s in project A and 1800 s in hidden project B (operator hides it) → `stats.hoursContributed == 1.0`, `projects.len() == 1`; the member list on A's page carries `handle: "anna-ivanova"`; (c) `candidate_card_reports_history_and_zeros_for_a_newcomer`: the coordinator reads the candidate for a request by Anna (one accepted, one declined announcement elsewhere, a fresh have-report) → `accepted 1, declined 1, holding 1, activeDevices 1`; for a brand-new account → all zeros and `profile: null`; a non-manager → 403.
- [ ] **Step 3–7:** implement, full suite, README, commit — `feat(hub): account profiles with public handles, the public profile page data, the candidate card aggregate`

---

## Task 5: H13 — mail templates and hooks (hub)

**Files:**
- Create: `src/mail.rs`
- Modify: `src/routes/join_requests.rs` (replace the inline nudge; hook ask/reply/approve/decline), `src/routes/invites.rs`? (no mail), `src/lib.rs` (`pub mod mail`), `README.md`
- Test: `tests/mail_and_scheduler.rs` (mail half; Task 6 adds the scheduler half to the same file)

**Interfaces (Produces):**
- `pub struct Links<'a> { public_url: Option<&'a str> }` with `fn to(&self, path: &str) -> String` (absolute when `public_url` is set, else the bare path).
- Pure template fns (each `-> (subject: String, body: String)`, unit-tested for exact text): `join_received { applicant, project_title, desired_role, note, manage_url }`, `join_decided { project_title, approved: bool, data_role: Option, reason: Option, offer_follow: bool, project_url, app_hint: bool }`, `question_asked { project_title, question, request_url }`, `question_answered { applicant, project_title, reply, manage_url }`, `weekly_digest { items: Vec<DigestItem { project_title, waiting: usize, oldest_days: f64, pending_announcements: usize, manage_url }> }`.
- `pub async fn notify(mailer: Arc<dyn Mailer>, to: Vec<String>, subject: String, body: String)` — spawns one task, one `send_notification` per recipient, `warn!` per failure with `error`, never returns an error.
- Hooks: `create_join_request` (request door only) → `join_received` to every `members.manage` holder (replaces the inline block); `approve_join_request` → `join_decided(approved)` to the applicant; `reject` → `join_decided(declined, reason, offer_follow)`; `ask` → `question_asked` to the applicant; `reply` → `question_answered` to managers; an invite redemption and an open-door join send nothing (the person is already in). Subjects are stable strings: `"[Athenaeum] New join request: {title}"`, `"[Athenaeum] You're in: {title}"`, `"[Athenaeum] Not this time: {title}"`, `"[Athenaeum] A question about your request: {title}"`, `"[Athenaeum] {applicant} answered: {title}"`, `"[Athenaeum] Weekly: what is waiting on you"`.

- [ ] **Step 1: Failing tests** — unit tests in `src/mail.rs` (exact subject/body for each template, both absolute and relative links) + integration in `tests/mail_and_scheduler.rs`: `ask_reply_and_decision_each_send_exactly_one_mail_to_the_right_side` — capture mailer; ask → the applicant's address gets the question subject; reply → the coordinator's address gets the answered subject; decline with reason → the applicant gets "Not this time" with the reason in the body; approve → "You're in" naming the role; the counts are exact (no duplicate to the coordinator on their own action). Use `mailer.last_notification()` / `last_notification_body()` and a small helper that lists all captured notifications (add `pub fn notifications(&self) -> Vec<(String,String,String)>` to `CaptureMailer`).
- [ ] **Step 2–5:** implement, full suite, README ("Mail" section: the six subjects and when each fires), commit — `feat(hub): mail — join received/decided, question asked/answered, digest template; links from HUB_PUBLIC_URL`

---

## Task 6: H13b — the hourly scheduler: stale requests and the weekly digest (hub)

**Files:**
- Create: `migrations/0021_hub_meta.sql`, `src/scheduler.rs`
- Modify: `src/lib.rs` (`pub mod scheduler`, spawn in `run()` after the router is built, before `axum::serve`), `README.md`
- Test: `tests/mail_and_scheduler.rs` (scheduler half), unit tests in `src/scheduler.rs`

**Interfaces (Produces):**
- `hub_meta (key text PRIMARY KEY, value text NOT NULL, updated_at timestamptz NOT NULL DEFAULT now())`.
- `pub async fn tick(db: &PgPool, mailer: Arc<dyn Mailer>, public_url: Option<&str>, now: DateTime<Utc>) -> Result<TickReport { stale_marked: u64, digests_sent: usize }>` — (1) `join_requests::mark_stale_requests(now)`; (2) if `now` is Monday and `now.hour() >= 8` (UTC) and `hub_meta['last_digest_week'] != iso_week(now)`: build per-manager digests — for every `members.manage` holder, the projects where `open|asked` requests older than `max(median_decision_days + 3, 3)` days exist or `pending` announcements exist; one mail per manager via `mail::weekly_digest`; then `hub_meta['last_digest_week'] = iso_week` (written AFTER the sends, so a crash mid-send re-sends next tick rather than silently skipping a week — documented).
- `pub fn spawn(db, mailer, public_url) -> JoinHandle<()>` — `tokio::time::interval(3600 s)` loop calling `tick(now = Utc::now())`, `error!` on failure, never exits. `lib.rs::run` spawns it with the same pool/mailer.
- Tests: unit — the digest gating (`should_send_digest(now, last_week) -> bool`) over Sunday 23:00 / Monday 07:59 / Monday 08:00 / Monday 20:00 same-week; integration — two projects, one with a 10-day-old open request, one clean; `tick(now = a Monday 09:00)` → the coordinator of the first gets one digest naming the project and "1 waiting"; the second coordinator gets nothing; calling `tick` again the same day sends nothing; a `stale` transition counted in `stale_marked`.

- [ ] **Steps:** migration → tests (RED) → implement → full suite → README ("Scheduler" section) → commit `feat(hub): hourly scheduler — stale join requests, Monday weekly digest, hub_meta bookkeeping`.

---

## Task 7: P1 — the design system, the logo, the shell (portal)

**Files:**
- Create: `portal/public/logo.svg` (copy of `/Volumes/BigMac/Users/astrobureau/Documents/Projects/artfrom-space/src/assets/logo.svg`), `portal/src/theme.ts`, `portal/src/ui/{Button,Card,Pill,Field,Select,Textarea,Meter,FilterTag,Tabs,Note,Avatar,Steps,KeyValue,EmptyState,PageHeader,Switch,Starfield}.tsx`, `portal/src/ui/index.ts`, `portal/vitest.config.ts`, `portal/src/test/setup.ts`, `portal/src/ui/ui.test.tsx`, `portal/src/theme.test.ts`
- Modify: `portal/src/styles.css`, `portal/src/Layout.tsx`, `portal/index.html` (`<link rel="icon" href="/logo.svg">`, `<meta name="color-scheme" content="dark light">`), `portal/package.json` (scripts `test`, dev deps `vitest`, `jsdom`, `@testing-library/react`, `@testing-library/jest-dom`, `@testing-library/user-event`), every existing page (`SignIn`, `Directory`, `ProjectPage`, `NewProject`, `Admin`, `Operator`) — restyled onto the primitives, behaviour unchanged (`NewProject` additionally gains the door select + default role select + the house-rules textarea prefilled with the template below — H4's fields)

**Design tokens** (`styles.css`, Tailwind v4):
```css
@import "tailwindcss";
@theme {
  --font-antiqua: "Book Antiqua", Palatino, "Palatino Linotype", "URW Palladio L", Georgia, serif;
  --color-surface: #2e3440; --color-surface-elevated: #3b4252; --color-surface-hover: #434c5e;
  --color-content: #eceff4; --color-content-secondary: #e5e9f0; --color-content-muted: #d8dee9; --color-content-faint: #9aa5b8;
  --color-border: #4c566a;
  --color-accent: #88c0d0; --color-accent-hover: #81a1c1; --color-accent-muted: #5e81ac; --color-accent-ink: #20262f;
  --color-success: #a3be8c; --color-warning: #ebcb8b; --color-error: #bf616a; --color-info: #81a1c1; --color-orange: #d08770; --color-purple: #b48ead;
  --radius-card: 10px;
}
@layer base {
  :root[data-theme="light"] {
    --color-surface: #f7f8fa; --color-surface-elevated: #ffffff; --color-surface-hover: #eceff4;
    --color-content: #2e3440; --color-content-secondary: #4c566a; --color-content-muted: #4c566a; --color-content-faint: #6b7689;
    --color-border: #d3dae5; --color-accent: #4a7f94; --color-accent-ink: #ffffff;
  }
  body { @apply bg-surface text-content antialiased; font-size: 15px; line-height: 1.55; }
  h1, h2, h3, h4 { @apply font-antiqua font-semibold; letter-spacing: .01em; }
}
```
The names mirror the app's `tailwind.config.js` (surface / content / border / accent / success / warning / error / info / orange / purple / `font-antiqua`), so a class reads the same in both codebases. `theme.ts`: initial theme = `localStorage['athenaeum.portal.theme']` else `prefers-color-scheme`; `useTheme()` returns `{theme, toggle}` and writes `document.documentElement.dataset.theme`.

**Primitives** — one file each, props typed, classes copied from the prototype's CSS (lines 60–158): `Button {variant: 'primary'|'ghost'|'danger', size: 'sm'|'md'|'lg', asLink?: to}`, `Card {flat?, link?: to}`, `Pill {tone: 'default'|'accent'|'green'|'yellow'|'red'|'purple', dot?}`, `Field`/`Select`/`Textarea` (label + help + error), `Meter {value, max, color?}`, `FilterTag {filter}` (colour map `F` from line 235), `Tabs {items, value, onChange}`, `Note {tone: 'info'|'warn'|'bad'}`, `Avatar {name, size}` (initials + the `hsl(hash % 360 42% 68%)` colour, lines 384–386), `Steps {names, current}`, `KeyValue {rows}`, `EmptyState {children}`, `PageHeader {title, eyebrow?, actions?}`, `Switch {on, onChange, locked?}`, `Starfield {seed, target?}` (the SVG generator, lines 399–411, deterministic LCG).

**Shell** (`Layout.tsx`, prototype lines 184–195 + 423–433): sticky header — `<img src="/logo.svg" alt="Athenaeum" class="h-7 w-7">` + "Athenaeum Projects" in `font-antiqua` (the owner's requirement: the real logo top-left), nav `Home` `/`, `Projects` `/projects`, `My projects`/`My account` `/me` when signed in (label per `/me`'s state — Task 12 supplies `useMeSummary`; until then "My projects"), `Operator` when `me.operator`; right side: theme toggle (sun/moon glyph, no icon library), then avatar+first-name link to `/me/profile` or `Sign in` + `Get the app` (→ `https://artfrom.space/`); footer verbatim from line 192–195 (Directory / Flows link removed — the prototype's `/flows` page is documentation, not a product route). `max-w` 1080 px, 20 px gutters, phone-safe (the prototype's `@media (max-width: 820px)` rules).

**Vitest**: `vitest.config.ts` (jsdom, `setupFiles: src/test/setup.ts` importing `@testing-library/jest-dom/vitest`), `npm test` = `vitest run`. Tests: `Pill` renders the tone class; `Meter` clamps to 100 %; `Avatar` initials for "Vilen (Astro Bureau)" = "VA"; `Starfield` is deterministic for a seed (same markup twice); `useTheme` reads the stored preference and toggles `data-theme`.

**House-rules template** (Ruling — the backlog's open question 4; the prototype's Squid rules, line 242, adapted to the general case): `"Frames you publish stay published — leaving does not withdraw them, and nobody's exit can invalidate a stack in progress. What you received while a member is yours to keep. Any published result must credit every contributor by name, past members included. Nobody resells the pooled data."` — `NewProject` prefills `dataPolicyText` with it.

- [ ] **Steps:** deps + config → tokens → primitives (with tests) → shell → restyle each existing page (screenshots not required; `npm run check`, `npm test`, `npm run build` green; the hub's `cargo build --release` embeds the new dist) → commit `feat(portal): design system — Nord tokens, the Athenaeum logo, primitives, shell, existing pages restyled`.

---

## Task 8: P2 + P3 — landing and catalog (portal)

**Files:** Create `portal/src/pages/Landing.tsx`, `portal/src/pages/Directory.tsx` (rewrite), `portal/src/components/ProjectCard.tsx`; modify `App.tsx` (`/` → Landing, `/projects` → Directory), `types.ts` (`Stats`, `DirectoryItem.watcherCount/joinPolicy`), tests `Landing.test.tsx`, `Directory.test.tsx`.

**Landing** (prototype lines 436–480): hero with `Starfield`, the `N projects collecting photons right now` pill from `/api/v1/stats`, the h1/paragraph/buttons verbatim (`Find a project` → `/projects`, `How joining works` → `/projects` for now — the flows page is not a product route; Ruling), the four stat tiles (`Active projects`, `Imagers`, `Hours pooled`, `Moved peer-to-peer` = `bytesPublished` in TB with one decimal), the "Getting in takes four steps" cards + the note (verbatim), "Featured" = `DirectoryItem.featured` cards. **"Needs your filter tonight" is NOT built in this wave** (needs H9's per-filter shortfall — wave 3); leave a `{/* H9: needs-your-filter section */}` marker.

**Catalog** (lines 494–523 + `projectCard` 482–491): search box, status select (`Any status` / `Collecting` / `Complete`), door select (`Any door` / `Open join` / `By request` / `Invite only`), all client-side over the directory list; the `Needs` chip row is NOT built (H9) — marker only; card = title + `doorPill` (green `open join`, purple `invite only`, default `by request`, `complete` when closed) + `target · members · N following`; no progress meter yet (goals are wave 3). Empty state verbatim. `Publish a project` button → `/new`.

Tests: Landing renders the four tiles from a mocked `/api/v1/stats`; Directory filters by door and status (three fixture items; the selects narrow the list), search matches target name.

- [ ] Commit `feat(portal): landing and catalog — stats tiles, featured, door/status filters`.

---

## Task 9: P5 — the join funnel and the invite landing (portal)

**Files:** Create `portal/src/pages/Join.tsx`, `portal/src/pages/Invite.tsx`, `portal/src/components/{SignInInline,RolePicker,HouseRules}.tsx`; modify `App.tsx` (`/p/:slug/join`, `/i/:token`), `ProjectPage.tsx` (the CTA block from lines 548–562: Manage / Open in app / Review pending / See your request / closed / Follow (invite door) / Join-or-Request + Follow; "Open in app" → `athenaeum://` is out of scope — it links to `https://artfrom.space/` with title "Open the project in the Athenaeum app"), `types.ts`, tests `Join.test.tsx`, `Invite.test.tsx`.

**Funnel** (lines 661–764), four steps with the `Steps` rail: (1) Sign in — inline OTP (`SignInInline` reuses the two-stage form; skipped when `me`); (2) About you — `GET /me/profile` prefilled, saved with `PUT /me/profile` on Continue (`displayName` required; handle optional here); (3) What you want — `RolePicker` (the two `cap-opt` cards with blurbs/costs verbatim; the processor cost line shows `Σ byteSize` of published packages in GB) + the note textarea + the "short X h of F" warning is H9 (wave 3) — show instead `This project has {N} published packages · {GB} GB` ; (4) House rules — `dataPolicyText` in a card, the checkbox sentence verbatim (gate version from `thresholds` is not on the page; show "the quality gate" without a version), the note per door; submit → `POST /projects/{id}/join-requests {displayName (from profile), desiredRole, message}` → done screen: `joined: true` → the "Welcome" card (lines 740–747, the three in-app steps verbatim, "Download Athenaeum" → `https://artfrom.space/`); else the "Request sent" card (lines 749–760) with `medianDecisionDays`/`openRequests` from `door`, `See your request` → `/p/:slug/application`, `Follow the project meanwhile` → `POST watch`. Errors: 403 with `invite-only` → route to the project page with the invite note; blocked (body-less 403) → "This project is not accepting a request from this account."; 409 already open → link to the application page.

**Invite landing** `/i/:token` (no prototype page — Ruling on its shape): `GET /api/v1/invites/{token}` → card "You've been invited to {title} as a {Role}"; `state != valid` → the matching sentence (revoked / expired / exhausted) + link to the project page; steps: sign in (inline) → display name (profile step 2 reused) → house rules → `POST /invites/{token}/redeem {displayName}` → the same Welcome card; 409 already a member → "You are already a member" + link.

Tests (Testing Library, mocked `fetch`): signed-in user starts at step 2; the submit button is disabled until the checkbox; an open-door response renders "you are a member"; an invite in state `expired` renders the expired sentence and no form.

- [ ] Commit `feat(portal): join funnel — sign-in, profile, role with cost, house rules; invite landing`.

---

## Task 10: P6 — the applicant's request page (portal)

**Files:** Create `portal/src/pages/Application.tsx`; modify `App.tsx` (`/p/:slug/application`), `types.ts` (`JoinRequestDetail`, `MessageView`), tests `Application.test.tsx`.

Page (lines 767–797): resolves the caller's request for this project from `GET /me/join-requests` (the newest for that `projectSlug`); header pill by status (`open` → "waiting on a human", `asked` → "question asked", `declined` → "declined", `withdrawn`, `stale` → "no answer for 30 days", `approved` → "approved — open the app"); the key/value card (Sent · Asked for · Coordinator (from the page's members) · Usually answers in); the conversation card (thread bubbles `them`/`us`, reply box shown only in `asked`); actions: Withdraw (`open|asked`, confirm), Follow the project (`POST watch`, toggles to Following); `declined` shows the reason in a `Note` + the follow offer when `offerFollow`; the closing note verbatim ("Nothing about your frames has been shared…"). No request → the empty state with a link to `/p/:slug/join`.

Tests: `asked` renders the reply box and the status pill; `declined` renders the reason and hides the box; reply POSTs to `/reply` and flips the pill to "answered — waiting".

- [ ] Commit `feat(portal): the applicant's request page — status, thread, reply, withdraw, follow`.

---

## Task 11: P9 — public profile and profile editing (portal)

**Files:** Create `portal/src/pages/Profile.tsx`, `portal/src/pages/EditProfile.tsx`; modify `App.tsx` (`/u/:handle`, `/me/profile`), `ProjectPage.tsx` (member names link to `/u/:handle` when present), `types.ts`, tests.

Public profile (lines 1301–1326): avatar + name + `location · sky · on Athenaeum since`, bio, link pills, the note verbatim, the four stat tiles (`Projects`, `Hours contributed`, `Frames published`, `Packages accepted`), Gear card, Projects table (`Project`, `Role`, `Contribution`, `Since`; coordinator pill). `Edit profile` when `me` owns it; `Invite to a project` is wave 3 (omit). Edit page (`/me/profile`): the "About you" form (lines 687–698) + handle field with the rule text, live availability error from the 409, links editor (up to 5), Save → `PUT`; a profile without a handle shows the note "Choose a handle to make your profile public".

Tests: the edit form rejects an invalid handle client-side before PUT; the public page renders stats from a mocked profile; `sky` codes map to the five labels.

- [ ] Commit `feat(portal): public profile and profile editing with handles`.

---

## Task 12: P10 — `/me` in three states (portal)

**Files:** Create `portal/src/pages/Me.tsx`, `portal/src/useMeSummary.ts`; modify `Layout.tsx` (nav label), `App.tsx` (`/me`), tests `Me.test.tsx`.

`useMeSummary()` loads `/me`, `/me/projects`, `/me/join-requests`, `/me/watching` once and derives `state: 'fresh' | 'applicant' | 'member'`: `member` when any membership; else `applicant` when any request in `open|asked`; else `fresh`. Page (lines 1238–1298): **fresh** — "Signed in, and that is all so far" card with the two feed items (Browse → `/projects`, Edit → `/me/profile`) + "The app is separate" card; **applicant** — one card per live request (title, status pill, "Sent N days ago as Role", Answer/See buttons) + "While you wait" card; **member** — the four tiles (`Projects`, `Hours contributed`, `Frames published` from the public profile stats when a handle exists, else from a `/me/profile`-less fallback of `—`; `Held for others` is app-side data → omit the tile in this wave, Ruling), "Needs you" (open+asked requests per project where `govCaps` contains `members.manage` or coordinator, `pendingAnnouncements` per project; both from existing endpoints), "Member of" cards (meter omitted until H9), "Following" cards from `/me/watching`. Nav label: `My account` for fresh/applicant, `My projects` for member.

Tests: the three states render their headline cards from mocked responses; the "Needs you" list counts only projects with `members.manage`.

- [ ] Commit `feat(portal): /me in three states — fresh, applicant, member`.

---

## Wave-2 acceptance (orchestrator, after Task 12)

- Hub: full suite green, `cargo build --release` with the fresh `portal/dist` embedded; every route in the README; the six migrations apply on a DB at 0016.
- Portal: `npm run check`, `npm test`, `npm run build`; a manual click-through on the served bundle (hub `cargo run` + the SPA at `http://127.0.0.1:8080/`) of: landing → catalog → project → request-to-join funnel → application page (ask/reply via a second session) → decline with reason → follow; open-door join; invite link redeem; profile edit → public profile; `/me` in all three states; light/dark toggle; the logo top-left.
- Merge `collab-v2` → local hub `main`; report; then wave 3.
