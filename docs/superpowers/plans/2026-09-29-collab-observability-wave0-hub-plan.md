# Collab observability — wave 0 (hub + portal) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make per-filter goals a validated, editable hub field (with a portal Admin editor) and expose each member's last-seen time to co-members, so the app's project Overview and Members tabs (waves 1–2) have real data.

**Architecture:** A pure `goals` validator module in the hub is called from `create_project` (against the default dictionary) and `update_project` (against the project's latest dictionary, read inside the update transaction); `put_dictionary` prunes goals of dropped canonicals; migration 0027 nulls any stored `goals` that does not fit the shape. `project_page` adds `lastSeenAt` per member from the existing `devices.last_seen_at`, filled only for a viewer who is a current member. The portal gains a `GoalsEditor` component in Admin and a last-seen line in the public Members list.

**Tech Stack:** Rust 1.96, axum 0.8, sqlx 0.8 (Postgres), `#[sqlx::test]` integration tests; portal React 18 + TypeScript + Vite + Vitest + Testing Library.

**Spec:** `docs/superpowers/specs/2026-09-29-collab-project-observability-design.md` §5.6 (read §2 D7/D8 too).

**Repo:** `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub` (NOT the app repo). All paths below are relative to it unless they start with `athenaeum/`.

## Global Constraints

- Goals shape on the wire and in storage: `{ "<canonical filter>": <seconds> }`; every key a canonical name of the project's LATEST filter dictionary (case-sensitive, like `filter_canonical`); every value a finite JSON number with `0 < seconds ≤ 36 000 000` (10 000 h).
- Clearing goals: the client sends `goals: {}`; the hub stores `NULL`. JSON `null` / an absent field keeps the stored value (the existing `COALESCE` semantics — do not change them).
- Refusal copy, verbatim: `goal for "<key>": not in this project's filter dictionary` and `goal for "<key>" must be a positive number of seconds, at most 36000000`; a non-object: `goals must be an object mapping a canonical filter to seconds`. `<key>` is rendered with Rust `{:?}` (double-quoted).
- The existing 4 KB size cap in `validate_project_texts` stays and runs first.
- Last seen = `max(devices.last_seen_at)` over the member's devices with `revoked_at IS NULL`. No new stamping.
- Last seen is filled only when the viewer is a **current member** of that project (owner ruling D8). Anonymous, non-member **and operator-but-not-member** viewers get `null` for every row.
- No new hub endpoint. Next migration number is `0027`.
- Logging: `tracing` only; message = short stable phrase, data in snake_case fields (`project_id`, `canonical`, …). Never swallow an error.
- No third-party project names in code, comments, docs or commit messages.
- Hub tests need Postgres: `docker compose up -d postgres`, then `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test …`. Run one cargo compile at a time on this Mac (memory exhaustion rule); never beside an app `cargo test`.
- Commits as the user (`eg013ra1n` / `vilen.sharifov@gmail.com`), ending with:
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_015RRpMcroNShaEgp8Q33fR3
  ```
- Push nothing. Deploying to the test hub is owner-gated (Task 8).

## Review Focus

1. **A goal for a filter added to the dictionary AFTER creation** — update must validate against the latest dictionary version, not the default one. Pinned in Task 2 (`update_accepts_goal_for_a_filter_added_later`).
2. **A goals-only PATCH from a `project.edit` delegate who is not coordinator** — must succeed (the portal shows them the editor). Pinned in Task 2 (`project_edit_delegate_can_set_goals`).
3. **Integer vs float seconds** (`3600` vs `3600.0`) — both accepted, value stored as sent. Pinned in Task 1 (`accepts_integer_and_float_seconds`).
4. **A viewer who is an operator but not a member** — must NOT see last seen even though the page treats them as privileged for governance flags. Pinned in Task 5 (`operator_non_member_sees_no_last_seen`).
5. **The goals editor after a dictionary change removes a filter** — the editor must not resubmit a goal for a canonical that is gone (the hub would refuse the whole save). Pinned in Task 7 (`drops_goals_for_filters_not_in_the_dictionary`).

---

### Task 0: Branch

**Files:** none.

- [ ] **Step 1: Create the branch from the unmerged contributor-path branch**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub
git status --short            # expect: empty
git branch --show-current     # expect: contributor-path-hub
git switch -c project-observability-hub
```

- [ ] **Step 2: Start Postgres and confirm the suite is green before changing anything**

```bash
docker compose up -d postgres
DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test projects_update --test coverage --test hardening --test dictionary
```
Expected: all PASS. If anything fails here, stop and report — it is pre-existing.

---

### Task 1: Pure goals validator

**Files:**
- Create: `src/goals.rs`
- Modify: `src/lib.rs` (add `pub mod goals;` in alphabetical position, after `pub mod feed;`)

**Interfaces:**
- Produces: `pub const MAX_GOAL_SECONDS: f64 = 36_000_000.0;` and
  `pub fn validate_goals(goals: &serde_json::Value, canonicals: &[String]) -> Result<Option<serde_json::Value>, crate::error::ApiError>` — `Ok(None)` for `{}` (clear), `Ok(Some(same object))` when valid, `Err(ApiError::bad_request(..))` otherwise. Also
  `pub fn prune_goals(goals: &serde_json::Value, keep: &[String]) -> Option<serde_json::Value>` — drops keys not in `keep`; `None` when nothing is left or `goals` is not an object.

- [ ] **Step 1: Write the module with its unit tests first (tests at the bottom), implementation stubbed to `todo!()`**

```rust
//! Per-filter integration goals (spec 2026-09-29 §5.6.1).
//!
//! Stored shape: `NULL` or `{ "<canonical>": seconds }`, every key a
//! canonical of the project's latest filter dictionary (case-sensitive, like
//! `project_frames.filter_canonical`), every value a finite number in
//! `(0, MAX_GOAL_SECONDS]`. An empty object on the wire means "clear" and is
//! stored as `NULL`; JSON `null`/absent keeps the stored value (the update
//! route's `COALESCE`).

use serde_json::Value;

use crate::error::ApiError;

/// 10 000 hours.
pub const MAX_GOAL_SECONDS: f64 = 36_000_000.0;

pub fn validate_goals(goals: &Value, canonicals: &[String]) -> Result<Option<Value>, ApiError> {
    todo!()
}

pub fn prune_goals(goals: &Value, keep: &[String]) -> Option<Value> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dict() -> Vec<String> {
        ["L", "R", "G", "B", "Ha", "OIII", "SII", "None"].iter().map(|s| s.to_string()).collect()
    }

    fn msg(err: ApiError) -> String {
        match err {
            ApiError::BadRequest(m) => m,
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn accepts_integer_and_float_seconds() {
        let v = json!({"Ha": 3600, "OIII": 7200.5});
        assert_eq!(validate_goals(&v, &dict()).unwrap(), Some(v.clone()));
    }

    #[test]
    fn empty_object_clears() {
        assert_eq!(validate_goals(&json!({}), &dict()).unwrap(), None);
    }

    #[test]
    fn refuses_a_non_object() {
        for bad in [json!([1]), json!("x"), json!(5), json!(true)] {
            assert_eq!(
                msg(validate_goals(&bad, &dict()).unwrap_err()),
                "goals must be an object mapping a canonical filter to seconds"
            );
        }
    }

    #[test]
    fn refuses_a_filter_outside_the_dictionary_case_sensitively() {
        assert_eq!(
            msg(validate_goals(&json!({"Hb": 3600}), &dict()).unwrap_err()),
            r#"goal for "Hb": not in this project's filter dictionary"#
        );
        assert_eq!(
            msg(validate_goals(&json!({"ha": 3600}), &dict()).unwrap_err()),
            r#"goal for "ha": not in this project's filter dictionary"#
        );
    }

    #[test]
    fn refuses_zero_negative_too_large_and_non_numbers() {
        for bad in [json!(0), json!(-1), json!(36_000_001), json!("3600"), json!(null), json!({"x": 1})] {
            assert_eq!(
                msg(validate_goals(&json!({"Ha": bad}), &dict()).unwrap_err()),
                r#"goal for "Ha" must be a positive number of seconds, at most 36000000"#
            );
        }
    }

    #[test]
    fn accepts_the_maximum() {
        assert!(validate_goals(&json!({"L": 36_000_000}), &dict()).unwrap().is_some());
    }

    #[test]
    fn prune_drops_gone_keys_and_empties_to_none() {
        let keep: Vec<String> = vec!["Ha".into()];
        assert_eq!(prune_goals(&json!({"Ha": 1, "SII": 2}), &keep), Some(json!({"Ha": 1})));
        assert_eq!(prune_goals(&json!({"SII": 2}), &keep), None);
        assert_eq!(prune_goals(&json!("garbage"), &keep), None);
    }
}
```

Before writing `msg`, open `src/error.rs` and confirm the variant name `ApiError::bad_request` constructs (line ~35). If it is not `ApiError::BadRequest(String)`, change `msg` to match the real variant — the assertion on the message text is the point.

- [ ] **Step 2: Run the tests to see them fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --lib goals::`
Expected: FAIL (panics at `todo!()`).

- [ ] **Step 3: Implement**

```rust
pub fn validate_goals(goals: &Value, canonicals: &[String]) -> Result<Option<Value>, ApiError> {
    let Some(obj) = goals.as_object() else {
        return Err(ApiError::bad_request(
            "goals must be an object mapping a canonical filter to seconds",
        ));
    };
    if obj.is_empty() {
        return Ok(None);
    }
    for (key, value) in obj {
        if !canonicals.iter().any(|c| c == key) {
            return Err(ApiError::bad_request(format!(
                "goal for {key:?}: not in this project's filter dictionary"
            )));
        }
        let ok = value
            .as_f64()
            .is_some_and(|s| s.is_finite() && s > 0.0 && s <= MAX_GOAL_SECONDS);
        if !ok {
            return Err(ApiError::bad_request(format!(
                "goal for {key:?} must be a positive number of seconds, at most 36000000"
            )));
        }
    }
    Ok(Some(goals.clone()))
}

pub fn prune_goals(goals: &Value, keep: &[String]) -> Option<Value> {
    let obj = goals.as_object()?;
    let kept: serde_json::Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| keep.iter().any(|c| c == *k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(Value::Object(kept))
    }
}
```

`json!("3600").as_f64()` is `None` and `json!(null).as_f64()` is `None`, so strings and null fail the number check.

- [ ] **Step 4: Run the tests to see them pass**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --lib goals::`
Expected: 7 PASS.

- [ ] **Step 5: Commit**

```bash
git add src/goals.rs src/lib.rs
git commit -m "feat(hub): goals validator — {canonical: seconds} against the dictionary"
```
(with the two trailer lines from Global Constraints)

---

### Task 2: Validate goals on create and update

**Files:**
- Modify: `src/routes/projects.rs` — `create_project_core` (the `validate_project_texts` call near line 318) and `update_project` (after the `FOR UPDATE` read near line 1305, and the `UPDATE projects SET` bind for `goals`).
- Create: `tests/goals.rs`

**Interfaces:**
- Consumes: `crate::goals::validate_goals`, `crate::routes::dictionary::{default_dictionary, canonical_filters_tx}` (both `pub(crate)`/`pub` already).
- Produces: create and PATCH refuse a malformed `goals` with 400 and the Global Constraints copy; `goals: {}` on PATCH clears the stored value to `null`.

- [ ] **Step 1: Write the failing integration tests**

```rust
//! Spec 2026-09-29 §5.6.1 — goals validation on create and update.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn create_body(goals: serde_json::Value) -> serde_json::Value {
    json!({
        "title": "Goals",
        "target": {"name": "M101", "raDeg": 210.8, "decDeg": 54.35, "radiusDeg": 1.5},
        "coordinatorDisplayName": "C",
        "coordinatorDataRole": "send_receive",
        "goals": goals,
    })
}

#[sqlx::test]
async fn create_accepts_default_dictionary_goals(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    let (status, body) = send(&app, post("/api/v1/projects", &create_body(json!({"Ha": 36000, "None": 600})), Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["goals"]["Ha"], 36000);
}

#[sqlx::test]
async fn create_refuses_unknown_filter_and_bad_values(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    for (goals, expect) in [
        (json!({"Hb": 3600}), r#"goal for "Hb": not in this project's filter dictionary"#),
        (json!({"Ha": 0}), r#"goal for "Ha" must be a positive number of seconds, at most 36000000"#),
        (json!({"Ha": "3600"}), r#"goal for "Ha" must be a positive number of seconds, at most 36000000"#),
        (json!([1, 2]), "goals must be an object mapping a canonical filter to seconds"),
    ] {
        let (status, body) = send(&app, post("/api/v1/projects", &create_body(goals.clone()), Some(&token))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{goals} accepted");
        assert!(String::from_utf8_lossy(&body).contains(expect), "{goals}: {}", String::from_utf8_lossy(&body));
    }
}

#[sqlx::test]
async fn update_sets_and_clears_goals(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    let project = create_project_via(&app, &token, "P", false).await;
    let id = project["id"].as_str().unwrap();

    let (status, body) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"goals": {"L": 7200}}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["goals"], json!({"L": 7200}));

    // An absent/`null` goals leaves them alone.
    let (status, body) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"description": "x"}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(as_json(&body)["goals"], json!({"L": 7200}));

    // `{}` clears to null.
    let (status, body) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"goals": {}}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(as_json(&body)["goals"].is_null());
}

#[sqlx::test]
async fn update_refuses_unknown_filter(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    let project = create_project_via(&app, &token, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let (status, body) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"goals": {"Hb": 60}}), Some(&token))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains(r#"goal for "Hb": not in this project's filter dictionary"#));
}

#[sqlx::test]
async fn update_accepts_goal_for_a_filter_added_later(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    let project = create_project_via(&app, &token, "P", false).await;
    let id = project["id"].as_str().unwrap();

    // Read the current dictionary and append Hb.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&token))).await;
    let mut entries = as_json(&body)["current"]["entries"].as_array().unwrap().clone();
    entries.push(json!({"canonical": "Hb", "aliases": ["h-beta"], "kind": "narrowband"}));
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let (status, body) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"goals": {"Hb": 3600}}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
}

#[sqlx::test]
async fn project_edit_delegate_can_set_goals(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    let (delegate, _) = register_device(&app, &mailer, "d@example.com", 2, "E").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    // `join_and_approve` returns the member's account id; the grant is the
    // same members PATCH `tests/governance.rs` uses.
    let dee = join_and_approve(&app, &coord, &delegate, id, "Dee", "send").await;
    let (status, body) = send(
        &app,
        patch(&format!("/api/v1/projects/{id}/members/{dee}"), &json!({"govCaps": ["project.edit"]}), Some(&coord)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let (status, body) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"goals": {"Ha": 3600}}), Some(&delegate))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test goals`
Expected: `create_refuses_…`, `update_refuses_…` FAIL (200 instead of 400); `update_sets_and_clears_goals` FAILS on the `{}` clear (stored `{}`, not null).

- [ ] **Step 3: Implement — create**

In `create_project_core`, directly after the existing `validate_project_texts(...)?;` call:

```rust
    // Spec 2026-09-29 §5.6.1 — a new project's dictionary is the default one
    // (`seed_default_tx` below), so its canonicals are the only valid keys.
    let goals: Option<Value> = match body.goals.as_ref() {
        None => None,
        Some(g) => {
            let canonicals: Vec<String> = crate::routes::dictionary::default_dictionary()
                .into_iter()
                .map(|e| e.canonical)
                .collect();
            crate::goals::validate_goals(g, &canonicals)?
        }
    };
```
and in the `INSERT INTO projects …` bind list replace `.bind(&body.goals)` with `.bind(&goals)`.

- [ ] **Step 4: Implement — update**

In `update_project`, right after `let current = sqlx::query_as::<_, ProjectRow>(… FOR UPDATE …)…?;`:

```rust
    // Spec 2026-09-29 §5.6.1 — validated against the LATEST dictionary, under
    // the project row lock taken just above. `Some(None)` = clear (`{}`),
    // `Some(Some(v))` = set, `None` = leave as is.
    let goals_update: Option<Option<Value>> = match body.goals.as_ref() {
        None => None,
        Some(g) => {
            let canonicals = crate::routes::dictionary::canonical_filters_tx(&mut tx, id).await?;
            Some(crate::goals::validate_goals(g, &canonicals)?)
        }
    };
    let goals_set = goals_update.is_some();
```

Change the `UPDATE projects SET` statement's goals line from `goals = COALESCE($3, goals), \` to `goals = CASE WHEN $11 THEN $3::jsonb ELSE goals END, \`, replace `.bind(&body.goals)` with `.bind(goals_update.flatten())`, and append `.bind(goals_set)` as the 11th bind after `.bind(&body.default_data_role)`.

If `canonical_filters_tx` takes `&mut PgConnection`, pass `&mut *tx`. Match the existing call site in `routes/frames.rs` (search `canonical_filters_tx(`) exactly.

- [ ] **Step 5: Run the new and the neighbouring suites**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test goals --test projects_update --test coverage --test hardening`
Expected: all PASS (`hardening`'s 4 KB goals case still 400 via the size cap; `coverage`'s `{"L": 3600.0}` is valid).

- [ ] **Step 6: Commit**

```bash
git add src/routes/projects.rs tests/goals.rs
git commit -m "feat(hub): validate goals on create/update; {} clears them"
```

---

### Task 3: Dictionary PUT prunes goals of dropped canonicals

**Files:**
- Modify: `src/routes/dictionary.rs` — `put_dictionary`, after the in-use loop and before the version `INSERT`.
- Test: `tests/goals.rs` (append)

**Interfaces:**
- Consumes: `crate::goals::prune_goals`.

- [ ] **Step 1: Write the failing test**

```rust
#[sqlx::test]
async fn dictionary_put_drops_goals_of_removed_filters(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    let project = create_project_via(&app, &token, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}"), &json!({"goals": {"SII": 3600, "Ha": 7200}}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK);

    // Remove SII (no frames use it).
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&token))).await;
    let entries: Vec<serde_json::Value> = as_json(&body)["current"]["entries"]
        .as_array().unwrap().iter().filter(|e| e["canonical"] != "SII").cloned().collect();
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), Some(&token))).await;
    assert_eq!(as_json(&body)["project"]["goals"], json!({"Ha": 7200}));

    // Removing the last goal's filter leaves null, not {}.
    let entries: Vec<serde_json::Value> = entries.into_iter().filter(|e| e["canonical"] != "Ha").collect();
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), Some(&token))).await;
    assert!(as_json(&body)["project"]["goals"].is_null());
}
```

- [ ] **Step 2: Run to see it fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test goals dictionary_put_drops`
Expected: FAIL — goals still contain `SII`.

- [ ] **Step 3: Implement**

In `put_dictionary`, after the `for gone in current.iter()…` in-use loop:

```rust
    // Spec 2026-09-29 §5.6.1: a goal may only name a canonical that exists —
    // drop the goals of every canonical this version removes, in this tx.
    let keep: Vec<String> = entries.iter().map(|e| e.canonical.clone()).collect();
    let stored: Option<(Option<Value>,)> =
        sqlx::query_as("SELECT goals FROM projects WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((Some(goals),)) = stored {
        let pruned = crate::goals::prune_goals(&goals, &keep);
        if pruned.as_ref() != Some(&goals) {
            tracing::info!(project_id = %id, "goals pruned to the new dictionary");
            sqlx::query("UPDATE projects SET goals = $2 WHERE id = $1")
                .bind(id)
                .bind(&pruned)
                .execute(&mut *tx)
                .await?;
        }
    }
```

- [ ] **Step 4: Run to see it pass, plus the dictionary suite**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test goals --test dictionary`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes/dictionary.rs tests/goals.rs
git commit -m "feat(hub): a dictionary version drops goals of removed canonicals"
```

---

### Task 4: Migration 0027 — normalise stored goals

**Files:**
- Create: `migrations/0027_goals_shape.sql`
- Test: `tests/goals.rs` (append)

- [ ] **Step 1: Write the failing test**

```rust
/// Migration 0027 nulls every stored `goals` that is not `{canonical: seconds}`
/// against the project's latest dictionary, keeps valid ones, and is
/// idempotent.
#[sqlx::test]
async fn migration_0027_nulls_malformed_goals_and_keeps_valid_ones(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (token, _) = register_device(&app, &mailer, "c@example.com", 1, "D").await;
    let mut ids = Vec::new();
    for t in ["A", "B", "C", "D", "E", "F"] {
        ids.push(create_project_via(&app, &token, t, false).await["id"].as_str().unwrap().to_string());
    }
    let cases = [
        json!({"Ha": 3600}),            // A valid → kept
        json!({"Hb": 3600}),            // B unknown filter → null
        json!({"Ha": "3600"}),          // C string value → null
        json!(["Ha"]),                  // D not an object → null
        json!({}),                      // E empty → null
        json!({"Ha": 0}),               // F zero → null
    ];
    for (id, goals) in ids.iter().zip(cases.iter()) {
        sqlx::query("UPDATE projects SET goals = $2 WHERE id = $1::uuid")
            .bind(id).bind(goals).execute(&pool).await.unwrap();
    }

    let sql = include_str!("../migrations/0027_goals_shape.sql");
    sqlx::raw_sql(sql).execute(&pool).await.unwrap();
    sqlx::raw_sql(sql).execute(&pool).await.unwrap(); // idempotent

    let read = |id: &str| {
        let pool = pool.clone();
        let id = id.to_string();
        async move {
            sqlx::query_scalar::<_, Option<serde_json::Value>>("SELECT goals FROM projects WHERE id = $1::uuid")
                .bind(id).fetch_one(&pool).await.unwrap()
        }
    };
    assert_eq!(read(&ids[0]).await, Some(json!({"Ha": 3600})));
    for id in &ids[1..] {
        assert_eq!(read(id).await, None, "project {id}");
    }
}
```

- [ ] **Step 2: Run to see it fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test goals migration_0027`
Expected: FAIL — compile error, the migration file does not exist.

- [ ] **Step 3: Write the migration**

```sql
-- 0027_goals_shape — spec 2026-09-29 §5.6.1. From now on `projects.goals` is
-- NULL or {"<canonical>": seconds}: every key a canonical of the project's
-- LATEST dictionary version, every value a JSON number in (0, 36000000].
-- The write path enforces it (src/goals.rs); this nulls every stored value
-- that does not fit, logging each one as a WARNING. `{}` is nulled too (the
-- write path stores "no goals" as NULL). Idempotent: a replay finds only
-- NULL or valid rows.
--
-- Guarded with CASE, never AND-ed predicates: Postgres does not promise
-- left-to-right evaluation of AND, and jsonb_each / the ::double precision
-- cast raise on the wrong jsonb type (same rule as 0026).
DO $$
DECLARE
    r RECORD;
BEGIN
    FOR r IN
        SELECT p.id
        FROM projects p
        WHERE p.goals IS NOT NULL
          AND CASE
                WHEN jsonb_typeof(p.goals) <> 'object' THEN true
                WHEN p.goals = '{}'::jsonb THEN true
                ELSE EXISTS (
                    SELECT 1
                    FROM jsonb_each(p.goals) g
                    WHERE CASE
                            WHEN jsonb_typeof(g.value) <> 'number' THEN true
                            WHEN (g.value::text)::double precision <= 0 THEN true
                            WHEN (g.value::text)::double precision > 36000000 THEN true
                            ELSE NOT EXISTS (
                                SELECT 1
                                FROM project_filter_dictionary d
                                CROSS JOIN LATERAL jsonb_array_elements(
                                    CASE WHEN jsonb_typeof(d.entries) = 'array'
                                         THEN d.entries ELSE '[]'::jsonb END
                                ) e
                                WHERE d.project_id = p.id
                                  AND d.version = (
                                      SELECT MAX(version) FROM project_filter_dictionary
                                      WHERE project_id = p.id)
                                  AND e ->> 'canonical' = g.key
                            )
                          END
                )
              END
    LOOP
        UPDATE projects SET goals = NULL WHERE id = r.id;
        RAISE WARNING 'goals reset to NULL for project % (not {canonical: seconds})', r.id;
    END LOOP;
END $$;
```

- [ ] **Step 4: Run to see it pass; the full suite now runs 0027 on every `#[sqlx::test]` DB**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test goals --test dictionary --test collab_schema`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add migrations/0027_goals_shape.sql tests/goals.rs
git commit -m "feat(hub): migration 0027 nulls stored goals that are not {canonical: seconds}"
```

---

### Task 5: `lastSeenAt` on the project page, for co-members only

**Files:**
- Modify: `src/routes/projects.rs` — `MemberRow`, `MemberPublicView`, `viewer_is_member_or_operator` (split out `viewer_is_member`), the members query in `project_page`, the `MemberPublicView { … }` mapping.
- Create: `tests/last_seen.rs`

**Interfaces:**
- Produces (wire): every element of `members[]` in `GET /api/v1/projects/{id}` gains `lastSeenAt: string | null` (RFC 3339 UTC). Non-null only for a current-member viewer.

- [ ] **Step 1: Write the failing tests**

```rust
//! Spec 2026-09-29 §5.6.2 — member last seen on the project page, visible to
//! current members only.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn set_last_seen(pool: &PgPool, device_id: &str, iso: &str) {
    sqlx::query("UPDATE devices SET last_seen_at = $2::timestamptz WHERE id = $1::uuid")
        .bind(device_id).bind(iso).execute(pool).await.unwrap();
}

fn member<'a>(page: &'a Value, name: &str) -> &'a Value {
    page["members"].as_array().unwrap().iter().find(|m| m["displayName"] == name).expect("member row")
}

/// Coordinator "Coord" (device 1) + member "Anna" with two devices (2, 3).
async fn setup(pool: &PgPool) -> (axum::Router, CaptureMailer, String, String, String, String, String) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "c@example.com", 1, "Desk").await;
    let (anna, anna_dev1) = register_device(&app, &mailer, "anna@example.com", 2, "Anna-1").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&app, &coord, &anna, &id, "Anna", "send").await;
    let (_, anna_dev2) = register_device(&app, &mailer, "anna@example.com", 3, "Anna-2").await;
    (app, mailer, coord, anna, id, anna_dev1, anna_dev2)
}

#[sqlx::test]
async fn member_sees_co_members_max_last_seen(pool: PgPool) {
    let (app, _m, coord, _anna, id, d1, d2) = setup(&pool).await;
    set_last_seen(&pool, &d1, "2026-09-20T10:00:00Z").await;
    set_last_seen(&pool, &d2, "2026-09-27T08:30:00Z").await;
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}"), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    let page = as_json(&body);
    assert_eq!(member(&page, "Anna")["lastSeenAt"], "2026-09-27T08:30:00Z");
}

#[sqlx::test]
async fn revoked_device_is_ignored(pool: PgPool) {
    let (app, _m, coord, _anna, id, d1, d2) = setup(&pool).await;
    set_last_seen(&pool, &d1, "2026-09-20T10:00:00Z").await;
    set_last_seen(&pool, &d2, "2026-09-27T08:30:00Z").await;
    sqlx::query("UPDATE devices SET revoked_at = now() WHERE id = $1::uuid").bind(&d2).execute(&pool).await.unwrap();
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), Some(&coord))).await;
    assert_eq!(member(&as_json(&body), "Anna")["lastSeenAt"], "2026-09-20T10:00:00Z");
}

#[sqlx::test]
async fn anonymous_and_non_member_see_no_last_seen(pool: PgPool) {
    let (app, mailer, _coord, _anna, id, d1, _d2) = setup(&pool).await;
    set_last_seen(&pool, &d1, "2026-09-20T10:00:00Z").await;
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert!(member(&as_json(&body), "Anna")["lastSeenAt"].is_null());
    let (stranger, _) = register_device(&app, &mailer, "s@example.com", 9, "S").await;
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), Some(&stranger))).await;
    assert!(member(&as_json(&body), "Anna")["lastSeenAt"].is_null());
}

#[sqlx::test]
async fn operator_non_member_sees_no_last_seen(pool: PgPool) {
    let (app, mailer) = app_with_operators(pool.clone(), &["op@example.com"]);
    let (coord, coord_dev) = register_device(&app, &mailer, "c@example.com", 1, "Desk").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    set_last_seen(&pool, &coord_dev, "2026-09-20T10:00:00Z").await;
    let (op, _) = register_device(&app, &mailer, "op@example.com", 7, "Op").await;
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), Some(&op))).await;
    assert!(member(&as_json(&body), "Coord")["lastSeenAt"].is_null());
}

#[sqlx::test]
async fn never_seen_is_null_for_members_too(pool: PgPool) {
    let (app, _m, coord, _anna, id, d1, d2) = setup(&pool).await;
    for d in [&d1, &d2] {
        sqlx::query("UPDATE devices SET last_seen_at = NULL WHERE id = $1::uuid").bind(d).execute(&pool).await.unwrap();
    }
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}"), Some(&coord))).await;
    assert!(member(&as_json(&body), "Anna")["lastSeenAt"].is_null());
    assert_eq!(json!(null), member(&as_json(&body), "Anna")["lastSeenAt"]);
}
```

Notes for the implementer: every authenticated request stamps the CALLER's device (`auth_mw::stamp_device_last_seen`), so the tests set Anna's times after Anna's last call and read as the coordinator. The coordinator's display name from `create_project_via` is `"Coord"`. Check `register_device`'s second return value is the device id (it is `v["deviceId"]`). If `devices.last_seen_at` serialises with fractional seconds, compare with `chrono::DateTime::parse_from_rfc3339` instead of the string — keep the assertion on the instant.

- [ ] **Step 2: Run to see them fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test last_seen`
Expected: FAIL — `lastSeenAt` is missing (`Value::Null` for all, so the two positive tests fail).

- [ ] **Step 3: Implement**

1. Split the membership check (keep the old function's behaviour for its callers):

```rust
/// `true` iff `account_id` is a current member of `project_id`. Spec
/// 2026-09-29 D8: member last-seen is shown to exactly these viewers — an
/// operator who is not a member does not count here.
async fn viewer_is_member(
    state: &AppState,
    project_id: Uuid,
    account_id: Option<Uuid>,
) -> Result<bool, ApiError> {
    let Some(account_id) = account_id else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM project_members WHERE project_id=$1 AND account_id=$2)",
    )
    .bind(project_id)
    .bind(account_id)
    .fetch_one(&state.db)
    .await?)
}
```
and make `viewer_is_member_or_operator` call it for its first half (`if viewer_is_member(state, project_id, Some(account_id)).await? { return Ok(true); }`), leaving the operator-email half unchanged.

2. In `project_page`, beside `let privileged = …`: `let viewer_member = viewer_is_member(&state, project.id, viewer_account).await?;`

3. `MemberRow` gains `last_seen_at: Option<DateTime<Utc>>,`; the members query becomes:

```rust
        "SELECT pm.display_name, pm.data_role, pm.is_coordinator, pm.joined_at, pm.gov_caps, \
                ap.handle, \
                (SELECT max(d.last_seen_at) FROM devices d \
                  WHERE d.account_id = pm.account_id AND d.revoked_at IS NULL) AS last_seen_at \
         FROM project_members pm \
         LEFT JOIN account_profiles ap ON ap.account_id = pm.account_id \
         WHERE pm.project_id = $1 ORDER BY pm.joined_at",
```

4. `MemberPublicView` gains (with a doc line) `last_seen_at: Option<DateTime<Utc>>,` — "latest activity of any non-revoked device of this member; `null` unless the viewer is a current member (spec 2026-09-29 D8)". The mapping sets `last_seen_at: if viewer_member { m.last_seen_at } else { None },`.

- [ ] **Step 4: Run to see them pass, and the page's other suites**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test last_seen --test projects_api --test profiles --test alumni --test coverage`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes/projects.rs tests/last_seen.rs
git commit -m "feat(hub): member lastSeenAt on the project page, for co-members only"
```

---

### Task 6: Portal — last seen in the Members list

**Files:**
- Create: `portal/src/lastSeen.ts`, `portal/src/lastSeen.test.ts`
- Modify: `portal/src/types.ts` (`MemberPublicView`), `portal/src/pages/ProjectPage.tsx` (members `<li>`), `portal/src/pages/ProjectPage.test.tsx` (fixture + one test). Also add `lastSeenAt: null` to every `MemberPublicView` literal the type checker then flags (search `joinedAt:` in `portal/src`).

**Interfaces:**
- Produces: `export function lastSeenLabel(iso: string, now: number): string` — `"active now"` under 2 min, `"N min ago"` under 60 min, `"N h ago"` under 24 h, else `"N day(s) ago"`; clock skew into the future reads `"active now"`.

- [ ] **Step 1: Write the failing unit tests**

```ts
import { describe, expect, it } from 'vitest';
import { lastSeenLabel } from './lastSeen';

const NOW = Date.parse('2026-09-29T12:00:00Z');
const ago = (ms: number) => new Date(NOW - ms).toISOString();

describe('lastSeenLabel', () => {
  it('reads active now under two minutes, and for a future instant', () => {
    expect(lastSeenLabel(ago(60_000), NOW)).toBe('active now');
    expect(lastSeenLabel(ago(-30_000), NOW)).toBe('active now');
  });
  it('counts minutes, hours, then days', () => {
    expect(lastSeenLabel(ago(5 * 60_000), NOW)).toBe('5 min ago');
    expect(lastSeenLabel(ago(3 * 3_600_000), NOW)).toBe('3 h ago');
    expect(lastSeenLabel(ago(26 * 3_600_000), NOW)).toBe('1 day ago');
    expect(lastSeenLabel(ago(3 * 86_400_000), NOW)).toBe('3 days ago');
  });
});
```

- [ ] **Step 2: Run to see them fail**

Run: `cd portal && npx vitest run src/lastSeen.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `portal/src/lastSeen.ts`**

```ts
/** Member last-seen, as the Members list reads it (spec 2026-09-29 §5.6.2).
 * The hub stamps a device at most once a minute while it is active, so
 * anything under two minutes is "active now". */
export function lastSeenLabel(iso: string, now: number): string {
  const ms = now - Date.parse(iso);
  if (!Number.isFinite(ms) || ms < 2 * 60_000) return 'active now';
  const min = Math.floor(ms / 60_000);
  if (min < 60) return `${min} min ago`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h} h ago`;
  const d = Math.floor(h / 24);
  return `${d} day${d === 1 ? '' : 's'} ago`;
}
```

An unparsable `iso` (`NaN`) reads "active now" — acceptable only because the hub always sends RFC 3339; keep the `Number.isFinite` guard and add `console.warn('[portal] unparsable lastSeenAt', iso)` inside that branch when `Number.isNaN(ms)`.

- [ ] **Step 4: Wire the type and the list**

`types.ts`, in `MemberPublicView` after `handle`:

```ts
  /** Latest activity of any of this member's devices (RFC 3339). Present
   * only when the viewer is a current member of the project; `null` for
   * everyone else and for a member never seen. */
  lastSeenAt: string | null;
```

`ProjectPage.tsx`, inside the members `<li>`, after the contribution `<span>`:

```tsx
                  {m.lastSeenAt && (
                    <span className="text-content-faint" title={m.lastSeenAt.replace('T', ' ').slice(0, 19)}>
                      · {lastSeenLabel(m.lastSeenAt, Date.now())}
                    </span>
                  )}
```
with `import { lastSeenLabel } from '../lastSeen';`. The title is `YYYY-MM-DD HH:MM:SS` (UTC), the project's date format.

- [ ] **Step 5: Add a page test**

In `ProjectPage.test.tsx`, the page fixture's `members` (around line 38) become:

```ts
    members: [
      { displayName: 'Anna', dataRole: 'send_receive', coordinator: true, joinedAt: '2026-01-01T00:00:00Z', handle: 'anna', lastSeenAt: new Date(Date.now() - 3 * 86_400_000).toISOString() },
      { displayName: 'Bora', dataRole: 'send', coordinator: false, joinedAt: '2026-01-02T00:00:00Z', handle: null, lastSeenAt: null },
    ],
```
(the other `members:` literal near line 229 gets `lastSeenAt: null` on each row), and a new block after the handle-link `describe`:

```ts
describe('ProjectPage member last seen', () => {
  it('shows it when the hub sends it and nothing when it is null', async () => {
    renderProjectPage();
    expect(await screen.findByText('· 3 days ago')).toBeInTheDocument();
    expect(screen.getByText('Bora').closest('li')).not.toHaveTextContent(/ago|active now/);
  });
});
```

- [ ] **Step 6: Run the portal suite and the type check**

Run: `cd portal && npx vitest run && npx tsc --noEmit`
Expected: all PASS, no type errors.

- [ ] **Step 7: Commit**

```bash
git add portal/src/lastSeen.ts portal/src/lastSeen.test.ts portal/src/types.ts portal/src/pages/ProjectPage.tsx portal/src/pages/ProjectPage.test.tsx
git commit -m "feat(portal): member last seen in the project's Members list"
```
(plus any other file Step 4's type fix touched)

---

### Task 7: Portal — goals editor in Admin

**Files:**
- Create: `portal/src/components/GoalsEditor.tsx`, `portal/src/components/GoalsEditor.test.tsx`
- Modify: `portal/src/pages/Admin.tsx` (load the dictionary for `project.edit` holders too; render the editor), `portal/src/pages/Admin.test.tsx` (one wiring test)

**Interfaces:**
- Produces: `export function GoalsEditor(props: { entries: DictionaryEntry[]; goals: Record<string, number> | null; onSave: (goals: Record<string, number>) => void }): JSX.Element`. `onSave` receives seconds (integers), `{}` when every field is empty.

- [ ] **Step 1: Write the failing component tests**

```tsx
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { GoalsEditor } from './GoalsEditor';
import type { DictionaryEntry } from '../types';

const ENTRIES: DictionaryEntry[] = [
  { canonical: 'Ha', aliases: [], kind: 'narrowband' },
  { canonical: 'OIII', aliases: [], kind: 'narrowband' },
  { canonical: 'L', aliases: [], kind: 'luminance' },
];

afterEach(cleanup);

describe('GoalsEditor', () => {
  it('renders one hours field per dictionary filter, seeded from seconds', () => {
    render(<GoalsEditor entries={ENTRIES} goals={{ Ha: 216000 }} onSave={vi.fn()} />);
    expect(screen.getByLabelText('Ha goal, hours')).toHaveValue('60');
    expect(screen.getByLabelText('OIII goal, hours')).toHaveValue('');
  });

  it('saves hours as whole seconds and omits empty fields', async () => {
    const onSave = vi.fn();
    render(<GoalsEditor entries={ENTRIES} goals={null} onSave={onSave} />);
    await userEvent.type(screen.getByLabelText('OIII goal, hours'), '12.5');
    await userEvent.tab();
    await userEvent.click(screen.getByRole('button', { name: 'Save goals' }));
    expect(onSave).toHaveBeenCalledWith({ OIII: 45000 });
  });

  it('clearing every field saves {}', async () => {
    const onSave = vi.fn();
    render(<GoalsEditor entries={ENTRIES} goals={{ Ha: 3600 }} onSave={onSave} />);
    await userEvent.clear(screen.getByLabelText('Ha goal, hours'));
    await userEvent.tab();
    await userEvent.click(screen.getByRole('button', { name: 'Save goals' }));
    expect(onSave).toHaveBeenCalledWith({});
  });

  it('shows an error on blur for a bad value and blocks saving', async () => {
    render(<GoalsEditor entries={ENTRIES} goals={null} onSave={vi.fn()} />);
    await userEvent.type(screen.getByLabelText('Ha goal, hours'), '-3');
    await userEvent.tab();
    expect(screen.getByText('Enter hours between 0 and 10 000.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save goals' })).toBeDisabled();
  });

  it('drops_goals_for_filters_not_in_the_dictionary', async () => {
    const onSave = vi.fn();
    render(<GoalsEditor entries={ENTRIES} goals={{ Ha: 3600, SII: 7200 }} onSave={onSave} />);
    await userEvent.type(screen.getByLabelText('L goal, hours'), '1');
    await userEvent.tab();
    await userEvent.click(screen.getByRole('button', { name: 'Save goals' }));
    expect(onSave).toHaveBeenCalledWith({ Ha: 3600, L: 3600 });
  });

  it('says there is nothing to save when unchanged', () => {
    render(<GoalsEditor entries={ENTRIES} goals={{ Ha: 3600 }} onSave={vi.fn()} />);
    expect(screen.getByRole('button', { name: 'Save goals' })).toBeDisabled();
    expect(screen.getByText('No changes to save.')).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run to see them fail**

Run: `cd portal && npx vitest run src/components/GoalsEditor.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `portal/src/components/GoalsEditor.tsx`**

```tsx
import { useMemo, useState } from 'react';
import type { DictionaryEntry } from '../types';
import { Button, Field } from '../ui';

/** Hub cap (src/goals.rs MAX_GOAL_SECONDS): 10 000 h. */
const MAX_HOURS = 10_000;
const BAD = 'Enter hours between 0 and 10 000.';

interface Row {
  canonical: string;
  kind: string;
  /** What the user typed, in hours — parsed on blur, never per keystroke. */
  raw: string;
  error: string | null;
}

function hoursText(seconds: number | undefined): string {
  if (seconds == null) return '';
  const h = seconds / 3600;
  return Number.isInteger(h) ? String(h) : String(Math.round(h * 100) / 100);
}

function parseHours(raw: string): { ok: true; seconds: number | null } | { ok: false } {
  const t = raw.trim().replace(',', '.');
  if (t === '') return { ok: true, seconds: null };
  const h = Number(t);
  if (!Number.isFinite(h) || h <= 0 || h > MAX_HOURS) return { ok: false };
  return { ok: true, seconds: Math.round(h * 3600) };
}

/** Per-filter integration goals (spec 2026-09-29 §5.6.1): one row per
 * canonical of the CURRENT dictionary. A stored goal for a filter that is no
 * longer in the dictionary is not shown and not resubmitted (the hub would
 * refuse the whole save). Seeded once — Admin re-mounts it (a `key` on the
 * project version) after a save. */
export function GoalsEditor({
  entries,
  goals,
  onSave,
}: {
  entries: DictionaryEntry[];
  goals: Record<string, number> | null;
  onSave: (goals: Record<string, number>) => void;
}) {
  const [rows, setRows] = useState<Row[]>(() =>
    entries.map((e) => ({ canonical: e.canonical, kind: e.kind, raw: hoursText(goals?.[e.canonical]), error: null })),
  );

  const next = useMemo(() => {
    const out: Record<string, number> = {};
    for (const r of rows) {
      const p = parseHours(r.raw);
      if (p.ok && p.seconds != null) out[r.canonical] = p.seconds;
    }
    return out;
  }, [rows]);

  const stored = useMemo(() => {
    const out: Record<string, number> = {};
    for (const e of entries) {
      const v = goals?.[e.canonical];
      if (typeof v === 'number') out[e.canonical] = v;
    }
    return out;
  }, [entries, goals]);

  const hasError = rows.some((r) => !parseHours(r.raw).ok);
  const unchanged = JSON.stringify(next) === JSON.stringify(stored) && Object.keys(goals ?? {}).length === Object.keys(stored).length;

  const blur = (i: number) =>
    setRows((rs) => rs.map((r, j) => (j === i ? { ...r, error: parseHours(r.raw).ok ? null : BAD } : r)));

  return (
    <section className="flex max-w-[640px] flex-col gap-2">
      <h2 className="text-[1.25rem]">Goals</h2>
      <p className="text-[0.78rem] text-content-faint">
        Integration to collect per filter, in hours. Empty means no goal. Members see progress toward these in the app.
      </p>
      {rows.map((r, i) => (
        <div key={r.canonical} className="flex flex-wrap items-start gap-2">
          <span className="w-20 py-[7px] text-[0.88rem] font-medium">{r.canonical}</span>
          <span className="w-24 py-[7px] text-[0.78rem] text-content-faint">{r.kind}</span>
          <Field
            value={r.raw}
            inputMode="decimal"
            aria-label={`${r.canonical} goal, hours`}
            error={r.error}
            onChange={(e) => {
              const raw = e.target.value;
              setRows((rs) => rs.map((x, j) => (j === i ? { ...x, raw, error: null } : x)));
            }}
            onBlur={() => blur(i)}
            className="w-24"
          />
          <span className="py-[7px] text-[0.88rem] text-content-faint">h</span>
        </div>
      ))}
      <div className="flex flex-wrap items-center gap-3">
        <Button size="sm" onClick={() => onSave(next)} disabled={hasError || unchanged}>
          Save goals
        </Button>
        {!hasError && unchanged && <span className="text-[0.78rem] text-content-faint">No changes to save.</span>}
      </div>
    </section>
  );
}
```

The `unchanged` check also counts stored goals for filters no longer in the dictionary: if `goals` holds such a key, the button stays enabled so the owner can save the pruned set. (Task 3 prunes on the hub side too; this covers goals stored before this wave.)

- [ ] **Step 4: Wire into Admin**

In `Admin.tsx`:

```tsx
function canEditProject(m: MyProject): boolean {
  return m.coordinator || m.govCaps.includes('project.edit');
}
```
In `reload`, change the dictionary fetch condition so it also runs for `canEditProject(m)` (fetch the dictionary once when `canEditThresholds(m) || canEditProject(m)`; keep the thresholds fetch under `canEditThresholds` only). Render, just above the `coordinator ? <SettingsEditor …>` block:

```tsx
      {canEditProject(membership) && dictVersion != null && (
        <GoalsEditor
          key={`goals-${page.project.version}-${dictVersion}`}
          entries={dictEntries}
          goals={page.project.goals}
          onSave={(goals) => void act(() => apiPatch(`/api/v1/projects/${projectId}`, { goals }), 'Goals saved.')}
        />
      )}
```
and `import { GoalsEditor } from '../components/GoalsEditor';`.

- [ ] **Step 5: Add an Admin wiring test**

Append to `Admin.test.tsx`:

```tsx
describe('Admin — goals editor (project.edit delegate)', () => {
  beforeEach(() => {
    membership = membershipWith({ coordinator: false, govCaps: ['project.edit'] });
    dictionary = {
      current: {
        version: 1,
        entries: [
          { canonical: 'L', aliases: ['lum'], kind: 'luminance' },
          { canonical: 'Ha', aliases: ['h-alpha'], kind: 'narrowband' },
        ],
        createdAt: '2026-01-01T00:00:00Z',
      },
      history: [],
    };
  });

  it('loads the dictionary for a project.edit holder and patches goals in seconds', async () => {
    const user = userEvent.setup();
    renderAdmin();
    const ha = await screen.findByLabelText('Ha goal, hours');
    expect(apiGet).toHaveBeenCalledWith('/api/v1/projects/p1/dictionary');
    expect(apiGet).not.toHaveBeenCalledWith('/api/v1/projects/p1/thresholds');
    await user.type(ha, '10');
    await user.tab();
    await user.click(screen.getByRole('button', { name: 'Save goals' }));
    await waitFor(() => expect(apiPatch).toHaveBeenCalledWith('/api/v1/projects/p1', { goals: { Ha: 36000 } }));
    expect(await screen.findByText('Goals saved.')).toBeInTheDocument();
  });

  it('shows no goals editor to a members.manage-only delegate', async () => {
    membership = membershipWith({ coordinator: false, govCaps: ['members.manage'] });
    renderAdmin();
    await waitFor(() => expect(screen.getByText('Join requests')).toBeInTheDocument());
    expect(screen.queryByLabelText('Ha goal, hours')).toBeNull();
  });
});
```

The existing test "asks for the dictionary only when thresholds.edit is held" (around line 193) must still pass: a holder of neither `thresholds.edit` nor `project.edit` still never fetches it. If its name now reads wrong, rename it to "…when thresholds.edit or project.edit is held" without changing its assertions.

- [ ] **Step 6: Run the portal suite, type check and build**

Run: `cd portal && npx vitest run && npx tsc --noEmit && npm run build`
Expected: all PASS; build succeeds.

- [ ] **Step 7: Commit**

```bash
git add portal/src/components/GoalsEditor.tsx portal/src/components/GoalsEditor.test.tsx portal/src/pages/Admin.tsx portal/src/pages/Admin.test.tsx
git commit -m "feat(portal): goals editor in Admin for project.edit holders"
```

---

### Task 8: Docs, full gates, and the owner-gated deploy

**Files:**
- Modify: `README.md` (hub) — the `PATCH /api/v1/projects/{id}` and `POST /api/v1/projects` rows (search `goals`), the project-page (`GET /api/v1/projects/{id}`) members description, and the migrations list (add `0027_goals_shape`).
- Modify (app repo): `athenaeum/docs/superpowers/specs/2026-09-29-collab-project-observability-design.md` §5.6.1/§5.6.3 — replace "`null` resets" with "`{}` clears (stored as NULL); `null`/absent keeps the stored value".

- [ ] **Step 1: README edits**

Replace the goals wording in the create/update rows with: "`goals`: `{ "<canonical>": seconds }` — every key in the project's current filter dictionary, `0 < seconds ≤ 36000000`; `{}` clears; refused with 400 otherwise. A dictionary version that drops a canonical drops its goal." Add to the project-page members description: "`lastSeenAt` — latest activity of the member's non-revoked devices; only for a viewer who is a current member, else `null`." Add the migration line: "`0027_goals_shape` — nulls stored `goals` that are not `{canonical: seconds}`."

- [ ] **Step 2: Full hub suite**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`
Expected: all PASS. The load check test (`tests/load_check.rs`) — run it only if it is part of the default suite and nothing else is compiling.

- [ ] **Step 3: Full portal suite + build**

Run: `cd portal && npx vitest run && npx tsc --noEmit && npm run build`
Expected: all PASS.

- [ ] **Step 4: Commit docs (hub repo, then app repo)**

```bash
git add README.md && git commit -m "docs(hub): goals shape, member lastSeenAt, migration 0027"
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
git add docs/superpowers/specs/2026-09-29-collab-project-observability-design.md
git commit -m "docs(collab): observability spec — {} clears goals, null keeps them"
```

- [ ] **Step 5: STOP — ask the owner before deploying**

Report the branch, the commit list and the green gates, and ask whether to deploy `project-observability-hub` to the test hub now (the test-hub deploy follows the hub deploy discipline; prod is a separate explicit procedure and is not part of this plan). Do not deploy, push or merge without that answer.
