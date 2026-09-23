# Collab v3 — Wave 0: portal thresholds mini-cycle — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The portal's quality-threshold editor becomes three dropdowns plus a
unit-labelled number field, the hub refuses rules outside a shared metric
registry, delegates with `thresholds.edit` can reach the editor, Admin gains
the door fields and a member/capability editor, and `0.6` can be typed.

**Architecture:** One metric registry, three copies pinned by tests: the hub
(`src/collab_rules.rs`, used by `validate_rules`), the portal
(`portal/src/metrics.ts`, drives the editor) and the app
(`collab/gate.rs::METRIC_REGISTRY`, the match arms). The portal keeps the
value as a string while editing and parses on blur. A new member-facing
`GET /projects/{id}/members` (gated on `members.manage`) feeds the Members
& roles editor, which writes through the existing `PATCH .../members/{id}`.

**Tech Stack:** Rust (axum, sqlx/Postgres, `#[sqlx::test]`), React 18 + TS +
vitest + testing-library, Tailwind tokens.

**Spec:** `docs/superpowers/specs/2026-09-23-collab-v3-per-frame-model-design.md`
— rulings R20 (§2), registry §6.3, portal §8.3 wave 0. Wave 0 is independent
of every other wave (§14).

## Global Constraints

- Repos: hub `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`
  (branch `collab-v3-wave0` from `main` 9d82fc7), app
  `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum` (main).
  Plans and specs live in the app repo; commits are per repo.
- Hub tests need Postgres: `docker compose up -d postgres` in the hub repo and
  `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`.
- Portal tests: `cd portal && npx vitest run`; typecheck `npx tsc -b`.
- Registry contents in wave 0 are exactly the seven keys the app understands
  today (§6.3 minus `zero_point`, which wave 3 adds on all three sides at
  once): `fwhm_arcsec` (″, lte|gte, number), `eccentricity` (—, lte|gte,
  number 0..1), `stars_detected` (count, gte|lte, integer), `median_snr`,
  `snr_weight`, `frame_snr` (—, lte|gte, number, advanced), `not_trailed`
  (—, reject_if, `true` only).
- Design tokens only (`text-content-faint`, `border-border`, …); no raw
  colours. UI strings English. No third-party product names anywhere.
- Every hub handler keeps `#[tracing::instrument(skip_all)]`; every refusal
  names the rule in its message. Never swallow an error.
- Commit as `eg013ra1n <vilen.sharifov@gmail.com>` with the
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session:` trailers. No push.

---

## File structure

Hub (`athenaeum-hub/`):
- Create `src/collab_rules.rs` — the registry (`MetricSpec`, `METRICS`,
  `validate_rules`) — moved out of `routes/projects.rs`.
- Modify `src/lib.rs` (or `main.rs`, wherever `mod` lines live) — `pub mod collab_rules;`.
- Modify `src/routes/projects.rs:68-98` — delete the old `validate_rules`, re-export the new one.
- Modify `src/routes/members.rs` — add `list_members` + `MemberAdminRow`.
- Modify `src/routes/mod.rs:212-215` — register `GET /api/v1/projects/{id}/members`.
- Create `tests/rules_registry.rs`, modify `tests/members_admin.rs`.

Portal (`athenaeum-hub/portal/src/`):
- Create `metrics.ts` — `METRICS`, `MetricKey`, `RuleOp`, `formatRule`.
- Create `metrics.test.ts`.
- Create `components/ThresholdEditor.tsx` (+ `.test.tsx`) — moved out of `pages/Admin.tsx`.
- Create `components/MembersEditor.tsx` (+ `.test.tsx`).
- Modify `pages/Admin.tsx` — use the two components, gate the editor on `thresholds.edit`, add the door fields, use `ROLE_LABEL`.
- Modify `pages/NewProject.tsx:9-13` — defaults typed from the registry.
- Modify `types.ts:313-317` — `ThresholdRule`, `MemberAdminView`.
- Modify `pages/Admin.test.tsx`.

App (`athenaeum/crates/athenaeum-core/src/collab/gate.rs`):
- Add `METRIC_REGISTRY` + a test that the match arms and the registry agree.

---

### Task 1: Hub registry module and strict `validate_rules`

**Files:**
- Create: `athenaeum-hub/src/collab_rules.rs`
- Modify: `athenaeum-hub/src/routes/projects.rs:67-98` (remove the old fn, add `pub(crate) use crate::collab_rules::validate_rules;`)
- Modify: the crate root that lists `pub mod` (find with `grep -n "pub mod routes" src/*.rs`)
- Test: `athenaeum-hub/tests/rules_registry.rs`

**Interfaces:**
- Produces: `pub struct MetricSpec { pub key: &'static str, pub unit: &'static str, pub ops: &'static [&'static str], pub value: ValueKind, pub advanced: bool }`,
  `pub enum ValueKind { Number, Integer, Unit01, BoolTrue }`,
  `pub const METRICS: [MetricSpec; 7]`,
  `pub fn metric(key: &str) -> Option<&'static MetricSpec>`,
  `pub(crate) fn validate_rules(rules: &Value) -> Result<(), ApiError>` (same signature as today, so `thresholds.rs:88` and `projects.rs:335` do not change).

- [ ] **Step 1: Write the failing integration test**

`athenaeum-hub/tests/rules_registry.rs`:

```rust
//! The hub refuses threshold rules outside the shared metric registry
//! (collab v3 spec §6.3) and names the offending rule.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

async fn post_rules(app: &axum::Router, token: &str, id: &str, rules: serde_json::Value) -> (StatusCode, String) {
    let (status, body) = send(app, post(&format!("/api/v1/projects/{id}/thresholds"), &json!({ "rules": rules }), Some(token))).await;
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[sqlx::test]
async fn registry_accepts_every_known_rule_shape(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();

    let (status, body) = post_rules(&app, &coord, id, json!([
        {"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.5},
        {"metricKey": "eccentricity", "op": "lte", "value": 0.6},
        {"metricKey": "stars_detected", "op": "gte", "value": 150},
        {"metricKey": "median_snr", "op": "gte", "value": 5.0},
        {"metricKey": "snr_weight", "op": "gte", "value": 0.5},
        {"metricKey": "frame_snr", "op": "gte", "value": 20},
        {"metricKey": "not_trailed", "op": "reject_if", "value": true},
    ])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[sqlx::test]
async fn registry_refuses_unknown_key_op_and_value_kind(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();

    let cases: Vec<(serde_json::Value, &str)> = vec![
        (json!([{"metricKey": "fwhm", "op": "lte", "value": 3}]), "unknown metric \"fwhm\""),
        (json!([{"metricKey": "fwhm_arcsec", "op": "<=", "value": 3}]), "op \"<=\" is not allowed for fwhm_arcsec"),
        (json!([{"metricKey": "fwhm_arcsec", "op": "lte", "value": true}]), "fwhm_arcsec needs a number"),
        (json!([{"metricKey": "fwhm_arcsec", "op": "lte", "value": -1}]), "fwhm_arcsec must be > 0"),
        (json!([{"metricKey": "eccentricity", "op": "lte", "value": 1.5}]), "eccentricity must be within 0..1"),
        (json!([{"metricKey": "stars_detected", "op": "gte", "value": 12.5}]), "stars_detected needs an integer"),
        (json!([{"metricKey": "not_trailed", "op": "lte", "value": true}]), "op \"lte\" is not allowed for not_trailed"),
        (json!([{"metricKey": "not_trailed", "op": "reject_if", "value": false}]), "not_trailed must be true"),
        (json!([{"metricKey": "fwhm_arcsec", "op": "lte", "value": 3}, {"metricKey": "fwhm_arcsec", "op": "lte", "value": 2}]), "duplicate rule for fwhm_arcsec lte"),
    ];
    for (rules, expected) in cases {
        let (status, body) = post_rules(&app, &coord, id, rules.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rules}");
        assert!(body.contains(expected), "expected {expected:?} in {body} for {rules}");
    }
}

#[sqlx::test]
async fn create_project_runs_the_same_registry(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (status, body) = send(&app, post("/api/v1/projects", &json!({
        "title": "Bad", "description": "x",
        "target": {"name": "M101", "raDeg": 210.8, "decDeg": 54.35, "radiusDeg": 1.5},
        "requireApproval": false, "coordinatorDisplayName": "Coord", "coordinatorDataRole": "send_receive",
        "initialThresholds": [{"metricKey": "nope", "op": "lte", "value": 1}],
    }), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("unknown metric \"nope\""));
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cd athenaeum-hub && DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test rules_registry`
Expected: `registry_refuses_unknown_key_op_and_value_kind` FAILS (today `fwhm` is accepted with 200); `create_project_runs_the_same_registry` FAILS.

- [ ] **Step 3: Write the registry module**

`athenaeum-hub/src/collab_rules.rs`:

```rust
//! The quality-threshold metric registry (collab v3 spec §6.3). Three copies
//! must agree: this file, `portal/src/metrics.ts` and the app's
//! `collab/gate.rs::METRIC_REGISTRY`. Adding a metric means all three plus
//! the gate's match arm, in one change.

use serde_json::Value;

use crate::error::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// Any finite number > 0.
    Number,
    /// A finite integer >= 0.
    Integer,
    /// A finite number within 0..=1.
    Unit01,
    /// The literal `true` (a reject-if flag).
    BoolTrue,
}

#[derive(Debug, Clone, Copy)]
pub struct MetricSpec {
    pub key: &'static str,
    pub unit: &'static str,
    pub ops: &'static [&'static str],
    pub value: ValueKind,
    /// Setup-dependent metrics the portal warns about (SNR family).
    pub advanced: bool,
}

const LTE_GTE: &[&str] = &["lte", "gte"];

pub const METRICS: [MetricSpec; 7] = [
    MetricSpec { key: "fwhm_arcsec", unit: "″", ops: LTE_GTE, value: ValueKind::Number, advanced: false },
    MetricSpec { key: "eccentricity", unit: "", ops: LTE_GTE, value: ValueKind::Unit01, advanced: false },
    MetricSpec { key: "stars_detected", unit: "stars", ops: LTE_GTE, value: ValueKind::Integer, advanced: false },
    MetricSpec { key: "median_snr", unit: "", ops: LTE_GTE, value: ValueKind::Number, advanced: true },
    MetricSpec { key: "snr_weight", unit: "", ops: LTE_GTE, value: ValueKind::Number, advanced: true },
    MetricSpec { key: "frame_snr", unit: "", ops: LTE_GTE, value: ValueKind::Number, advanced: true },
    MetricSpec { key: "not_trailed", unit: "", ops: &["reject_if"], value: ValueKind::BoolTrue, advanced: false },
];

pub fn metric(key: &str) -> Option<&'static MetricSpec> {
    METRICS.iter().find(|m| m.key == key)
}

/// Threshold rules: a non-empty array (≤50) of `{metricKey, op, value}` where
/// every rule is in the registry, its op is allowed for that metric, its
/// value has the metric's kind, and no (metricKey, op) pair repeats. Every
/// refusal names the rule.
pub(crate) fn validate_rules(rules: &Value) -> Result<(), ApiError> {
    let arr = rules
        .as_array()
        .ok_or_else(|| ApiError::bad_request("rules must be an array"))?;
    if arr.is_empty() || arr.len() > 50 {
        return Err(ApiError::bad_request("rules must contain 1..=50 items"));
    }
    let mut seen: Vec<(String, String)> = Vec::with_capacity(arr.len());
    for rule in arr {
        let obj = rule
            .as_object()
            .ok_or_else(|| ApiError::bad_request("each rule must be an object"))?;
        let key = obj
            .get("metricKey")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiError::bad_request("each rule needs metricKey (string)"))?;
        let spec = metric(key)
            .ok_or_else(|| ApiError::bad_request(format!("unknown metric {key:?}")))?;
        let op = obj
            .get("op")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiError::bad_request(format!("rule for {key} needs op (string)")))?;
        if !spec.ops.contains(&op) {
            return Err(ApiError::bad_request(format!(
                "op {op:?} is not allowed for {key} (allowed: {})",
                spec.ops.join(", ")
            )));
        }
        let value = obj
            .get("value")
            .ok_or_else(|| ApiError::bad_request(format!("rule for {key} needs value")))?;
        check_value(spec, value)?;
        let pair = (key.to_string(), op.to_string());
        if seen.contains(&pair) {
            return Err(ApiError::bad_request(format!("duplicate rule for {key} {op}")));
        }
        seen.push(pair);
    }
    Ok(())
}

fn check_value(spec: &MetricSpec, value: &Value) -> Result<(), ApiError> {
    let key = spec.key;
    match spec.value {
        ValueKind::BoolTrue => {
            if value != &Value::Bool(true) {
                return Err(ApiError::bad_request(format!("{key} must be true")));
            }
        }
        ValueKind::Number => {
            let v = value
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| ApiError::bad_request(format!("{key} needs a number")))?;
            if v <= 0.0 {
                return Err(ApiError::bad_request(format!("{key} must be > 0")));
            }
        }
        ValueKind::Unit01 => {
            let v = value
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| ApiError::bad_request(format!("{key} needs a number")))?;
            if !(0.0..=1.0).contains(&v) {
                return Err(ApiError::bad_request(format!("{key} must be within 0..1")));
            }
        }
        ValueKind::Integer => {
            let v = value
                .as_i64()
                .or_else(|| value.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .ok_or_else(|| ApiError::bad_request(format!("{key} needs an integer")))?;
            if v < 0 {
                return Err(ApiError::bad_request(format!("{key} must be >= 0")));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keys_are_unique_and_ops_non_empty() {
        let mut keys: Vec<&str> = METRICS.iter().map(|m| m.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), METRICS.len());
        assert!(METRICS.iter().all(|m| !m.ops.is_empty()));
    }

    #[test]
    fn integer_kind_accepts_whole_floats_only() {
        let spec = metric("stars_detected").unwrap();
        assert!(check_value(spec, &serde_json::json!(150.0)).is_ok());
        assert!(check_value(spec, &serde_json::json!(150.5)).is_err());
        assert!(check_value(spec, &serde_json::json!(-1)).is_err());
    }
}
```

In `src/routes/projects.rs` delete lines 67-98 (the doc comment + old
`validate_rules`) and add near the top, next to the other `use` lines:

```rust
pub(crate) use crate::collab_rules::validate_rules;
```

Add `pub mod collab_rules;` to the crate root next to `pub mod collab_auth;`.

- [ ] **Step 4: Run the tests**

Run: `cd athenaeum-hub && DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test rules_registry --test thresholds --test collab_flow --test projects_api && cargo test --lib collab_rules`
Expected: all PASS. (`thresholds.rs::versions_accumulate` posts `{"metricKey": "x"}` from a *member* and expects 403 — the cap check runs before validation, so it still passes.)

- [ ] **Step 5: Commit (hub)**

```bash
cd athenaeum-hub && git checkout -b collab-v3-wave0 && git add src/collab_rules.rs src/routes/projects.rs src/lib.rs tests/rules_registry.rs
git commit -m "feat(hub): threshold rules validate against the shared metric registry

Unknown metric, disallowed op, wrong value kind and duplicate (metric, op)
are refused with the rule named. Registry is collab v3 spec §6.3."
```

(Adjust `src/lib.rs` to whichever file holds the `mod` lines.)

---

### Task 2: Hub member-facing members listing

**Files:**
- Modify: `athenaeum-hub/src/routes/members.rs` (append after `patch_member`)
- Modify: `athenaeum-hub/src/routes/mod.rs:212-215`
- Test: `athenaeum-hub/tests/members_admin.rs` (append)

**Interfaces:**
- Produces: `GET /api/v1/projects/{id}/members` → `Vec<MemberAdminRow>` with
  `{accountId, displayName, dataRole, coordinator, govCaps: string[], joinedAt, handle: string|null}`;
  403 without `members.manage`.
- Consumes: `require_cap(&state.db, id, auth.account_id, "members.manage")` (`collab_auth.rs`), `PUBLISHER_NAME_SQL` is not needed (members are current, never former).

- [ ] **Step 1: Write the failing test**

Append to `athenaeum-hub/tests/members_admin.rs`:

```rust
#[sqlx::test]
async fn members_listing_is_gated_on_members_manage_and_carries_caps(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let (bob, _) = register_device(&app, &mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send").await;
    join_and_approve(&app, &coord, &bob, id, "Bob", "send_receive").await;

    // A plain member is refused.
    let (status, _) = send(&app, get(&format!("/api/v1/projects/{id}/members"), Some(&anna))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The coordinator sees everyone, with caps.
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/members"), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let rows = as_json(&body);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let anna_row = rows.iter().find(|r| r["displayName"] == "Anna").unwrap();
    assert_eq!(anna_row["dataRole"], "send");
    assert_eq!(anna_row["coordinator"], false);
    assert_eq!(anna_row["govCaps"], json!([]));
    assert!(anna_row["accountId"].as_str().is_some());

    // Grant Anna members.manage; she can now list, and her row shows it.
    let anna_id = anna_row["accountId"].as_str().unwrap();
    let (status, _) = send(&app, patch(&format!("/api/v1/projects/{id}/members/{anna_id}"), &json!({"govCaps": ["members.manage"]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/members"), Some(&anna))).await;
    assert_eq!(status, StatusCode::OK);
    let rows = as_json(&body);
    let anna_row = rows.as_array().unwrap().iter().find(|r| r["displayName"] == "Anna").unwrap();
    assert_eq!(anna_row["govCaps"], json!(["members.manage"]));
}
```

Check the file's existing `use` lines include `serde_json::json`, `StatusCode`, `PgPool`; add any that are missing.

- [ ] **Step 2: Run it to see it fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test members_admin members_listing`
Expected: FAIL with 404/405 on the GET (route missing).

- [ ] **Step 3: Implement the handler and route**

Append to `src/routes/members.rs`:

```rust
// ---- GET /api/v1/projects/{id}/members ----------------------------------------

/// Member-facing roster for the portal's Members & roles editor. Unlike the
/// public project page it carries account ids and governance flags, so it is
/// gated on `members.manage` (the coordinator holds it by construction).
#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MemberAdminRow {
    pub account_id: Uuid,
    pub display_name: String,
    pub data_role: String,
    pub coordinator: bool,
    pub gov_caps: Vec<String>,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    pub handle: Option<String>,
}

#[tracing::instrument(skip_all)]
pub async fn list_members(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<MemberAdminRow>>, ApiError> {
    require_cap(&state.db, id, auth.account_id, "members.manage").await?;
    let rows = sqlx::query_as::<_, MemberAdminRow>(
        "SELECT m.account_id, m.display_name, m.data_role,
                m.is_coordinator AS coordinator, m.gov_caps, m.joined_at, p.handle
         FROM project_members m
         LEFT JOIN account_profiles p ON p.account_id = m.account_id
         WHERE m.project_id = $1
         ORDER BY m.is_coordinator DESC, m.joined_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}
```

If `gov_caps` is stored as `text[]` this maps to `Vec<String>` directly; if
it is `jsonb`, change the field to `sqlx::types::Json<Vec<String>>` and map
it — check `migrations/0013_governance.sql` first. If `account_profiles`
has a different column name for the handle, mirror what `snapshots.rs` or
`profiles.rs` selects.

In `src/routes/mod.rs` replace lines 212-215 with:

```rust
        .route("/api/v1/projects/{id}/members", get(members::list_members))
        .route(
            "/api/v1/projects/{id}/members/{account_id}",
            axum::routing::patch(members::patch_member).delete(members::remove_member),
        )
```

- [ ] **Step 4: Run the tests**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test members_admin --test governance`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes/members.rs src/routes/mod.rs tests/members_admin.rs
git commit -m "feat(hub): member-facing roster with governance flags for the portal's roles editor"
```

---

### Task 3: Portal metric registry and rule formatting

**Files:**
- Create: `athenaeum-hub/portal/src/metrics.ts`
- Create: `athenaeum-hub/portal/src/metrics.test.ts`
- Modify: `athenaeum-hub/portal/src/types.ts:313-317`

**Interfaces:**
- Produces:
  ```ts
  export type MetricKey = 'fwhm_arcsec' | 'eccentricity' | 'stars_detected' | 'median_snr' | 'snr_weight' | 'frame_snr' | 'not_trailed';
  export type RuleOp = 'lte' | 'gte' | 'reject_if';
  export interface MetricSpec { key: MetricKey; label: string; unit: string; ops: RuleOp[]; value: 'number' | 'integer' | 'unit01' | 'boolTrue'; advanced: boolean; help: string; defaultOp: RuleOp; defaultValue: number | true }
  export const METRICS: MetricSpec[];
  export const OP_LABEL: Record<RuleOp, string>;   // lte '≤', gte '≥', reject_if 'reject if'
  export function metricSpec(key: string): MetricSpec | undefined;
  export function parseRuleValue(spec: MetricSpec, raw: string): { ok: true; value: number | true } | { ok: false; error: string };
  export function formatRule(rule: ThresholdRule): string;   // 'FWHM ≤ 3.5″', 'reject if trailed'
  ```
- `types.ts`: `export interface ThresholdRule { metricKey: string; op: string; value: number | boolean }` and `ThresholdView.rules: ThresholdRule[]`.

- [ ] **Step 1: Write the failing tests**

`portal/src/metrics.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { formatRule, METRICS, metricSpec, OP_LABEL, parseRuleValue } from './metrics';

describe('metric registry', () => {
  it('has the seven keys of collab v3 §6.3 (wave 0) with unique keys', () => {
    const keys = METRICS.map((m) => m.key);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys).toEqual(['fwhm_arcsec', 'eccentricity', 'stars_detected', 'median_snr', 'snr_weight', 'frame_snr', 'not_trailed']);
  });

  it('only allows reject_if on not_trailed, and lte/gte elsewhere', () => {
    for (const m of METRICS) {
      if (m.key === 'not_trailed') expect(m.ops).toEqual(['reject_if']);
      else expect(m.ops).toEqual(['lte', 'gte']);
    }
    expect(OP_LABEL.lte).toBe('≤');
  });

  it('parses fractional input and refuses out-of-range values with a sentence', () => {
    const fwhm = metricSpec('fwhm_arcsec')!;
    expect(parseRuleValue(fwhm, '3.5')).toEqual({ ok: true, value: 3.5 });
    expect(parseRuleValue(fwhm, '0.')).toEqual({ ok: false, error: 'Enter a number.' });
    expect(parseRuleValue(fwhm, '-1')).toEqual({ ok: false, error: 'Must be greater than 0.' });
    const ecc = metricSpec('eccentricity')!;
    expect(parseRuleValue(ecc, '0.6')).toEqual({ ok: true, value: 0.6 });
    expect(parseRuleValue(ecc, '1.2')).toEqual({ ok: false, error: 'Must be between 0 and 1.' });
    const stars = metricSpec('stars_detected')!;
    expect(parseRuleValue(stars, '150')).toEqual({ ok: true, value: 150 });
    expect(parseRuleValue(stars, '12.5')).toEqual({ ok: false, error: 'Must be a whole number.' });
    const trailed = metricSpec('not_trailed')!;
    expect(parseRuleValue(trailed, 'anything')).toEqual({ ok: true, value: true });
  });

  it('formats a rule the way the app shows it', () => {
    expect(formatRule({ metricKey: 'fwhm_arcsec', op: 'lte', value: 3.5 })).toBe('FWHM ≤ 3.5″');
    expect(formatRule({ metricKey: 'stars_detected', op: 'gte', value: 150 })).toBe('Stars ≥ 150');
    expect(formatRule({ metricKey: 'not_trailed', op: 'reject_if', value: true })).toBe('Reject trailed frames');
    expect(formatRule({ metricKey: 'made_up', op: 'lte', value: 1 })).toBe('made_up lte 1');
  });
});
```

- [ ] **Step 2: Run to see it fail**

Run: `cd portal && npx vitest run src/metrics.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

`portal/src/types.ts` — replace lines 313-317 with:

```ts
/** One quality-threshold rule as the hub stores it. Keys and ops are
 * validated by the hub against the registry mirrored in `metrics.ts`. */
export interface ThresholdRule {
  metricKey: string;
  op: string;
  value: number | boolean;
}

export interface ThresholdView {
  version: number;
  rules: ThresholdRule[];
  createdAt: string;
}
```

`portal/src/metrics.ts`:

```ts
import type { ThresholdRule } from './types';

/** The quality-threshold metric registry (collab v3 spec §6.3). Three copies
 * must agree: the hub's `src/collab_rules.rs`, this file, and the app's
 * `collab/gate.rs::METRIC_REGISTRY`. */
export type MetricKey = 'fwhm_arcsec' | 'eccentricity' | 'stars_detected' | 'median_snr' | 'snr_weight' | 'frame_snr' | 'not_trailed';
export type RuleOp = 'lte' | 'gte' | 'reject_if';
export type ValueKind = 'number' | 'integer' | 'unit01' | 'boolTrue';

export interface MetricSpec {
  key: MetricKey;
  label: string;
  unit: string;
  ops: RuleOp[];
  value: ValueKind;
  /** Setup-dependent (SNR family): the editor warns when one is in use. */
  advanced: boolean;
  help: string;
  defaultOp: RuleOp;
  defaultValue: number | true;
}

const LTE_GTE: RuleOp[] = ['lte', 'gte'];

export const METRICS: MetricSpec[] = [
  { key: 'fwhm_arcsec', label: 'FWHM', unit: '″', ops: LTE_GTE, value: 'number', advanced: false, help: 'Median star width, arcseconds — the app converts from pixels with each frame’s own scale.', defaultOp: 'lte', defaultValue: 3.5 },
  { key: 'eccentricity', label: 'Eccentricity', unit: '', ops: LTE_GTE, value: 'unit01', advanced: false, help: '0 = round stars, 1 = lines.', defaultOp: 'lte', defaultValue: 0.6 },
  { key: 'stars_detected', label: 'Stars', unit: '', ops: LTE_GTE, value: 'integer', advanced: false, help: 'Stars the analysis found in the frame.', defaultOp: 'gte', defaultValue: 150 },
  { key: 'median_snr', label: 'Median SNR', unit: '', ops: LTE_GTE, value: 'number', advanced: true, help: 'Per-star signal to noise; depends on exposure and gear.', defaultOp: 'gte', defaultValue: 5 },
  { key: 'snr_weight', label: 'SNR weight', unit: '', ops: LTE_GTE, value: 'number', advanced: true, help: 'The app’s relative weight; depends on exposure and gear.', defaultOp: 'gte', defaultValue: 0.5 },
  { key: 'frame_snr', label: 'Frame SNR', unit: '', ops: LTE_GTE, value: 'number', advanced: true, help: 'Whole-frame signal to noise; depends on exposure and gear.', defaultOp: 'gte', defaultValue: 20 },
  { key: 'not_trailed', label: 'Trailed frames', unit: '', ops: ['reject_if'], value: 'boolTrue', advanced: false, help: 'Reject frames whose stars look like streaks.', defaultOp: 'reject_if', defaultValue: true },
];

export const OP_LABEL: Record<RuleOp, string> = { lte: '≤', gte: '≥', reject_if: 'reject if' };

export function metricSpec(key: string): MetricSpec | undefined {
  return METRICS.find((m) => m.key === key);
}

export type ParsedValue = { ok: true; value: number | true } | { ok: false; error: string };

/** Parse what the user typed for `spec`. The editor calls this on blur, never
 * per keystroke, so `0.` and `3.` survive while typing. */
export function parseRuleValue(spec: MetricSpec, raw: string): ParsedValue {
  if (spec.value === 'boolTrue') return { ok: true, value: true };
  const trimmed = raw.trim();
  if (trimmed === '' || !/^-?\d+(\.\d+)?$/.test(trimmed)) return { ok: false, error: 'Enter a number.' };
  const n = Number(trimmed);
  if (!Number.isFinite(n)) return { ok: false, error: 'Enter a number.' };
  switch (spec.value) {
    case 'number':
      return n > 0 ? { ok: true, value: n } : { ok: false, error: 'Must be greater than 0.' };
    case 'unit01':
      return n >= 0 && n <= 1 ? { ok: true, value: n } : { ok: false, error: 'Must be between 0 and 1.' };
    case 'integer':
      if (!Number.isInteger(n)) return { ok: false, error: 'Must be a whole number.' };
      return n >= 0 ? { ok: true, value: n } : { ok: false, error: 'Must be 0 or more.' };
  }
}

/** Human form of a rule, matching the app's own wording. Unknown rules are
 * shown raw so a stale registry is visible, never hidden. */
export function formatRule(rule: ThresholdRule): string {
  const spec = metricSpec(rule.metricKey);
  if (!spec) return `${rule.metricKey} ${rule.op} ${String(rule.value)}`;
  if (spec.value === 'boolTrue') return 'Reject trailed frames';
  const op = OP_LABEL[rule.op as RuleOp] ?? rule.op;
  return `${spec.label} ${op} ${String(rule.value)}${spec.unit}`;
}
```

- [ ] **Step 4: Run the tests and typecheck**

Run: `cd portal && npx vitest run src/metrics.test.ts && npx tsc -b`
Expected: PASS; `tsc` clean (Admin.tsx still compiles because `Rule` there is structurally identical).

- [ ] **Step 5: Commit**

```bash
git add portal/src/metrics.ts portal/src/metrics.test.ts portal/src/types.ts
git commit -m "feat(portal): threshold metric registry mirrored from the hub, with parse-on-blur values"
```

---

### Task 4: `ThresholdEditor` component with dropdowns

**Files:**
- Create: `athenaeum-hub/portal/src/components/ThresholdEditor.tsx`
- Create: `athenaeum-hub/portal/src/components/ThresholdEditor.test.tsx`
- Modify: `athenaeum-hub/portal/src/pages/Admin.tsx:30-39` (remove `ADVANCED_METRICS`, `Rule`), `:348-402` (remove the inline `ThresholdEditor`), import the component.

**Interfaces:**
- Produces: `export function ThresholdEditor({ rules, version, history, onSave }: { rules: ThresholdRule[]; version: number | null; history: ThresholdView[]; onSave: (rules: ThresholdRule[]) => void })`.
- Consumes: Task 3's `METRICS`, `metricSpec`, `parseRuleValue`, `OP_LABEL`, `formatRule`; `ui` `Select`, `Field`, `Button`, `Note`, `EmptyState`, `Pill`.

Behaviour:
- Each row: metric `Select` (labels from the registry, `aria-label="Metric"`), op `Select` limited to the metric's ops (`aria-label="Condition"`), value `Field` with `inputMode="decimal"`, the unit shown after it, `aria-label="Value"`; for `not_trailed` the value control is replaced by the fixed text "reject".
- Changing the metric resets op and value to the spec defaults.
- The value is kept as a string in row state; on blur it runs `parseRuleValue`; an error shows under the field and disables Save.
- Duplicate (metric, op) pairs show "This rule is already set." and disable Save.
- "Add rule" appends the first registry metric not yet used (or FWHM).
- Save is disabled while the draft is empty (the hub demands 1..=50) with the help text "A project needs at least one rule."
- The advanced warning stays as today.
- Below the editor: "Previous versions" — one line per history entry `v{n} · {createdAt.slice(0,10)} · {rules.map(formatRule).join(', ')}`.

- [ ] **Step 1: Write the failing tests**

`portal/src/components/ThresholdEditor.test.tsx`:

```tsx
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ThresholdEditor } from './ThresholdEditor';
import type { ThresholdRule } from '../types';

afterEach(cleanup);

const RULES: ThresholdRule[] = [
  { metricKey: 'fwhm_arcsec', op: 'lte', value: 3.5 },
  { metricKey: 'not_trailed', op: 'reject_if', value: true },
];

function renderEditor(rules: ThresholdRule[] = RULES) {
  const onSave = vi.fn();
  render(<ThresholdEditor rules={rules} version={2} history={[{ version: 1, createdAt: '2026-01-01T00:00:00Z', rules: [{ metricKey: 'fwhm_arcsec', op: 'lte', value: 4 }] }]} onSave={onSave} />);
  return onSave;
}

describe('ThresholdEditor', () => {
  it('renders each rule as metric/condition dropdowns and a unit-labelled value', () => {
    renderEditor();
    const metrics = screen.getAllByRole('combobox', { name: 'Metric' });
    expect(metrics).toHaveLength(2);
    expect(metrics[0]).toHaveValue('fwhm_arcsec');
    expect(screen.getAllByRole('combobox', { name: 'Condition' })[0]).toHaveValue('lte');
    expect(screen.getByRole('textbox', { name: 'Value' })).toHaveValue('3.5');
    expect(screen.getByText('″')).toBeInTheDocument();
    // The trailed rule has no value box, just the fixed word.
    expect(screen.getAllByRole('textbox', { name: 'Value' })).toHaveLength(1);
    expect(screen.getByText('reject')).toBeInTheDocument();
    // The condition dropdown offers only the metric's own ops.
    const conditions = screen.getAllByRole('combobox', { name: 'Condition' });
    expect(within(conditions[0]).getAllByRole('option').map((o) => o.textContent)).toEqual(['≤', '≥']);
    expect(within(conditions[1]).getAllByRole('option').map((o) => o.textContent)).toEqual(['reject if']);
  });

  it('lets 0.6 be typed and saves the parsed number', async () => {
    const user = userEvent.setup();
    const onSave = renderEditor([{ metricKey: 'eccentricity', op: 'lte', value: 0.5 }]);
    const value = screen.getByRole('textbox', { name: 'Value' });
    await user.clear(value);
    await user.type(value, '0.6');
    expect(value).toHaveValue('0.6');
    await user.tab();
    await user.click(screen.getByRole('button', { name: 'Save as new version' }));
    expect(onSave).toHaveBeenCalledWith([{ metricKey: 'eccentricity', op: 'lte', value: 0.6 }]);
  });

  it('shows the parse error on blur and disables Save', async () => {
    const user = userEvent.setup();
    const onSave = renderEditor([{ metricKey: 'eccentricity', op: 'lte', value: 0.5 }]);
    const value = screen.getByRole('textbox', { name: 'Value' });
    await user.clear(value);
    await user.type(value, '1.2');
    await user.tab();
    expect(screen.getByText('Must be between 0 and 1.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save as new version' })).toBeDisabled();
    expect(onSave).not.toHaveBeenCalled();
  });

  it('changing the metric resets condition and value to the registry defaults', async () => {
    const user = userEvent.setup();
    renderEditor([{ metricKey: 'fwhm_arcsec', op: 'gte', value: 9 }]);
    await user.selectOptions(screen.getByRole('combobox', { name: 'Metric' }), 'stars_detected');
    expect(screen.getByRole('combobox', { name: 'Condition' })).toHaveValue('gte');
    expect(screen.getByRole('textbox', { name: 'Value' })).toHaveValue('150');
  });

  it('refuses a duplicate (metric, condition) pair', async () => {
    const user = userEvent.setup();
    renderEditor([{ metricKey: 'fwhm_arcsec', op: 'lte', value: 3 }]);
    await user.click(screen.getByRole('button', { name: 'Add rule' }));
    await user.selectOptions(screen.getAllByRole('combobox', { name: 'Metric' })[1], 'fwhm_arcsec');
    expect(screen.getByText('This rule is already set.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save as new version' })).toBeDisabled();
  });

  it('disables Save on an empty draft and explains why, and lists previous versions', async () => {
    const user = userEvent.setup();
    renderEditor([{ metricKey: 'fwhm_arcsec', op: 'lte', value: 3 }]);
    await user.click(screen.getByRole('button', { name: 'Remove rule' }));
    expect(screen.getByRole('button', { name: 'Save as new version' })).toBeDisabled();
    expect(screen.getByText('A project needs at least one rule.')).toBeInTheDocument();
    expect(screen.getByText(/v1 · 2026-01-01 · FWHM ≤ 4″/)).toBeInTheDocument();
  });

  it('warns when an SNR-family metric is in use', async () => {
    const user = userEvent.setup();
    renderEditor([{ metricKey: 'fwhm_arcsec', op: 'lte', value: 3 }]);
    await user.selectOptions(screen.getByRole('combobox', { name: 'Metric' }), 'median_snr');
    expect(screen.getByText(/not comparable across members/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run to see it fail**

Run: `cd portal && npx vitest run src/components/ThresholdEditor.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement the component**

`portal/src/components/ThresholdEditor.tsx`:

```tsx
import { useEffect, useMemo, useState } from 'react';
import { formatRule, METRICS, metricSpec, OP_LABEL, parseRuleValue, type MetricSpec, type RuleOp } from '../metrics';
import type { ThresholdRule, ThresholdView } from '../types';
import { Button, EmptyState, Field, Note, Select } from '../ui';

interface DraftRule {
  metricKey: string;
  op: string;
  /** What the user typed — parsed on blur, never per keystroke. */
  raw: string;
  error: string | null;
}

function toDraft(rule: ThresholdRule): DraftRule {
  return { metricKey: rule.metricKey, op: rule.op, raw: rule.value === true ? 'true' : String(rule.value), error: null };
}

function fromDraft(d: DraftRule, spec: MetricSpec): ThresholdRule {
  const parsed = parseRuleValue(spec, d.raw);
  return { metricKey: d.metricKey, op: d.op, value: parsed.ok ? parsed.value : 0 };
}

function defaultDraft(spec: MetricSpec): DraftRule {
  return { metricKey: spec.key, op: spec.defaultOp, raw: spec.defaultValue === true ? 'true' : String(spec.defaultValue), error: null };
}

export function ThresholdEditor({
  rules,
  version,
  history,
  onSave,
}: {
  rules: ThresholdRule[];
  version: number | null;
  history: ThresholdView[];
  onSave: (rules: ThresholdRule[]) => void;
}) {
  const [draft, setDraft] = useState<DraftRule[]>(rules.map(toDraft));
  useEffect(() => setDraft(rules.map(toDraft)), [rules]);

  const duplicates = useMemo(() => {
    const seen = new Set<string>();
    return draft.map((d) => {
      const key = `${d.metricKey}|${d.op}`;
      const dup = seen.has(key);
      seen.add(key);
      return dup;
    });
  }, [draft]);
  const hasAdvanced = draft.some((d) => metricSpec(d.metricKey)?.advanced);
  const hasError = draft.some((d) => d.error != null) || duplicates.some(Boolean);
  const empty = draft.length === 0;

  const update = (i: number, patch: Partial<DraftRule>) => setDraft(draft.map((d, j) => (j === i ? { ...d, ...patch } : d)));

  const changeMetric = (i: number, key: string) => {
    const spec = metricSpec(key);
    if (!spec) return;
    setDraft(draft.map((d, j) => (j === i ? defaultDraft(spec) : d)));
  };

  const blurValue = (i: number) => {
    const d = draft[i];
    const spec = metricSpec(d.metricKey);
    if (!spec) return;
    const parsed = parseRuleValue(spec, d.raw);
    update(i, { error: parsed.ok ? null : parsed.error, raw: parsed.ok ? String(parsed.value) : d.raw });
  };

  const addRule = () => {
    const used = new Set(draft.map((d) => d.metricKey));
    const spec = METRICS.find((m) => !used.has(m.key)) ?? METRICS[0];
    setDraft([...draft, defaultDraft(spec)]);
  };

  const save = () => {
    const out: ThresholdRule[] = [];
    for (const d of draft) {
      const spec = metricSpec(d.metricKey);
      if (!spec) return;
      const parsed = parseRuleValue(spec, d.raw);
      if (!parsed.ok) return;
      out.push(fromDraft(d, spec));
    }
    onSave(out);
  };

  return (
    <section className="flex flex-col gap-2">
      <h2 className="text-[1.25rem]">
        Quality thresholds {version != null && <span className="text-[0.86rem] font-normal text-content-faint">(v{version})</span>}
      </h2>
      {empty && <EmptyState>No rules — every frame that reaches the project is accepted.</EmptyState>}
      {draft.map((d, i) => {
        const spec = metricSpec(d.metricKey);
        return (
          <div key={i} className="flex flex-col gap-1">
            <div className="flex flex-wrap items-start gap-2">
              <Select value={d.metricKey} onChange={(e) => changeMetric(i, e.target.value)} aria-label="Metric" className="w-44">
                {METRICS.map((m) => (
                  <option key={m.key} value={m.key}>
                    {m.label}
                  </option>
                ))}
              </Select>
              <Select value={d.op} onChange={(e) => update(i, { op: e.target.value as RuleOp })} aria-label="Condition" className="w-28">
                {(spec?.ops ?? []).map((op) => (
                  <option key={op} value={op}>
                    {OP_LABEL[op]}
                  </option>
                ))}
              </Select>
              {spec?.value === 'boolTrue' ? (
                <span className="py-[7px] text-[0.88rem] text-content-faint">reject</span>
              ) : (
                <div className="flex items-center gap-1">
                  <Field
                    value={d.raw}
                    inputMode="decimal"
                    aria-label="Value"
                    error={d.error}
                    onChange={(e) => update(i, { raw: e.target.value, error: null })}
                    onBlur={() => blurValue(i)}
                    className="w-24"
                  />
                  {spec?.unit && <span className="text-[0.88rem] text-content-faint">{spec.unit}</span>}
                </div>
              )}
              <Button variant="ghost" size="sm" aria-label="Remove rule" onClick={() => setDraft(draft.filter((_, j) => j !== i))}>
                Remove
              </Button>
            </div>
            {spec?.help && <p className="text-[0.78rem] text-content-faint">{spec.help}</p>}
            {duplicates[i] && <p className="text-[0.78rem] text-error-ink">This rule is already set.</p>}
          </div>
        );
      })}
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="ghost" size="sm" onClick={addRule}>
          Add rule
        </Button>
        <Button size="sm" onClick={save} disabled={empty || hasError}>
          Save as new version
        </Button>
        {empty && <span className="text-[0.78rem] text-content-faint">A project needs at least one rule.</span>}
      </div>
      {hasAdvanced && (
        <Note tone="warn">
          SNR-family metrics depend on exposure and gear — values are not comparable across members’ setups. Prefer FWHM/eccentricity/star
          counts.
        </Note>
      )}
      <p className="text-[0.78rem] text-content-faint">Changes are prospective: already-published frames stay published.</p>
      {history.length > 0 && (
        <div className="flex flex-col gap-1">
          <h3 className="text-[0.86rem] font-medium">Previous versions</h3>
          {history.map((h) => (
            <p key={h.version} className="text-[0.78rem] text-content-faint">
              v{h.version} · {h.createdAt.slice(0, 10)} · {h.rules.map(formatRule).join(', ')}
            </p>
          ))}
        </div>
      )}
    </section>
  );
}
```

If `Field` does not forward `onBlur`/`inputMode` (it spreads `...rest` onto
the `<input>`, so it does), nothing else is needed. If `Button` has no
`aria-label` passthrough, add `...rest` spreading to it in `ui/Button.tsx`.

- [ ] **Step 4: Run the tests**

Run: `cd portal && npx vitest run src/components/ThresholdEditor.test.tsx`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add portal/src/components/ThresholdEditor.tsx portal/src/components/ThresholdEditor.test.tsx
git commit -m "feat(portal): threshold editor with metric/condition dropdowns, units, parse-on-blur and version history"
```

---

### Task 5: Wire the editor into Admin, gate on `thresholds.edit`, fix role labels

**Files:**
- Modify: `athenaeum-hub/portal/src/pages/Admin.tsx` — lines 30-39 (delete `ADVANCED_METRICS` and `Rule`), 55-56 (`rules` state typed `ThresholdRule[]`, add `history` state), 75-79 (load thresholds when `canEditThresholds`), 165-176 (render), 268 (`Pill`), 272-275 (approve `Select`), 348-402 (delete the inline editor).
- Test: `athenaeum-hub/portal/src/pages/Admin.test.tsx`

**Interfaces:**
- Consumes: Task 4's `ThresholdEditor`; `ROLE_LABEL`, `ROLE_OPTIONS` from `components/RolePicker`.
- Produces: `function canEditThresholds(m: MyProject): boolean` (module-local).

- [ ] **Step 1: Update the tests**

In `Admin.test.tsx` change the thresholds mock (line 82) to return history too:

```ts
  if (url === '/api/v1/projects/p1/thresholds') return { current: null, history: [] } as { current: ThresholdView | null; history: ThresholdView[] };
```

Replace the test at lines 161-173 with:

```tsx
  it('gives a members.manage delegate the queue and neither settings nor thresholds', async () => {
    membership = membershipWith({ coordinator: false, govCaps: ['members.manage'] });
    renderAdmin();

    await waitFor(() => expect(screen.getByText('Anna')).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Approve' })).toBeInTheDocument();
    expect(screen.getByText('Only the coordinator can change settings.')).toBeInTheDocument();
    expect(screen.queryByText(/Quality thresholds/)).toBeNull();
    expect(apiGet).not.toHaveBeenCalledWith('/api/v1/projects/p1/thresholds');
  });

  it('gives a thresholds.edit delegate the editor and nothing else', async () => {
    membership = membershipWith({ coordinator: false, govCaps: ['members.manage', 'thresholds.edit'] });
    renderAdmin();

    await waitFor(() => expect(screen.getByText(/Quality thresholds/)).toBeInTheDocument());
    expect(apiGet).toHaveBeenCalledWith('/api/v1/projects/p1/thresholds');
    expect(screen.queryByRole('button', { name: 'Save settings' })).toBeNull();
  });

  it('shows the requested role with its product name, and offers roles by name', async () => {
    request = requestWith({ desiredRole: 'send_receive' });
    renderAdmin();
    await waitFor(() => expect(screen.getByText('Anna')).toBeInTheDocument());
    expect(screen.getByText('wants Processor')).toBeInTheDocument();
    const roles = screen.getByRole('combobox', { name: 'Data role to grant' });
    expect(within(roles).getAllByRole('option').map((o) => o.textContent)).toEqual(['Contributor', 'Processor']);
  });
```

Add `within` to the testing-library import.

- [ ] **Step 2: Run to see the new tests fail**

Run: `cd portal && npx vitest run src/pages/Admin.test.tsx`
Expected: the two new tests FAIL (delegate gets no editor; pill reads `wants send_receive`).

- [ ] **Step 3: Edit Admin.tsx**

Imports: add `import { ThresholdEditor } from '../components/ThresholdEditor';` and change the `types` import to include `ThresholdRule` (drop the local `Rule`). Delete lines 30-39.

State (replace lines 55-56):

```ts
  const [rules, setRules] = useState<ThresholdRule[]>([]);
  const [thresholdVersion, setThresholdVersion] = useState<number | null>(null);
  const [thresholdHistory, setThresholdHistory] = useState<ThresholdView[]>([]);
```

Capability helper next to `canManageInvites`:

```ts
function canEditThresholds(m: MyProject): boolean {
  return m.coordinator || m.govCaps.includes('thresholds.edit');
}
```

Loading (replace lines 75-79):

```ts
    if (m != null && canEditThresholds(m)) {
      const t = await apiGet<{ current: ThresholdView | null; history: ThresholdView[] }>(`/api/v1/projects/${p.project.id}/thresholds`);
      setRules(t.current?.rules ?? []);
      setThresholdVersion(t.current?.version ?? null);
      setThresholdHistory(t.history ?? []);
    }
```

Render (replace lines 165-176):

```tsx
      {canEditThresholds(membership) && (
        <ThresholdEditor
          rules={rules}
          version={thresholdVersion}
          history={thresholdHistory}
          onSave={(next) => void act(() => apiPost(`/api/v1/projects/${projectId}/thresholds`, { rules: next }), 'Thresholds saved.')}
        />
      )}
      {coordinator ? (
        <SettingsEditor page={page} onPatch={(body, ok) => void act(() => apiPatch(`/api/v1/projects/${projectId}`, body), ok)} />
      ) : (
        <Note>Only the coordinator can change settings.</Note>
      )}
```

Line 268: `<Pill>wants {ROLE_LABEL[request.desiredRole]}</Pill>`.

Lines 272-275:

```tsx
          <Select value={role} onChange={(e) => setRole(e.target.value)} aria-label="Data role to grant" className="w-auto">
            {ROLE_OPTIONS.map((option) => (
              <option key={option.role} value={option.role}>
                {option.label}
              </option>
            ))}
          </Select>
```

Delete the inline `ThresholdEditor` function (old lines 348-402).

- [ ] **Step 4: Run the page tests and typecheck**

Run: `cd portal && npx vitest run src/pages/Admin.test.tsx && npx tsc -b`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add portal/src/pages/Admin.tsx portal/src/pages/Admin.test.tsx
git commit -m "feat(portal): Admin uses the dropdown threshold editor, opens it to thresholds.edit delegates, names roles"
```

---

### Task 6: Door fields in Admin settings

**Files:**
- Modify: `athenaeum-hub/portal/src/pages/Admin.tsx` — `SettingsEditor` (old lines 404-448) and its call site.
- Test: `athenaeum-hub/portal/src/pages/Admin.test.tsx`

**Interfaces:**
- Consumes: `page.door?.joinPolicy`, `page.door?.defaultDataRole` (`DoorView`, `types.ts`), hub `PATCH /projects/{id}` accepting `joinPolicy` and `defaultDataRole` (gated on `invites.manage`; the coordinator holds it).
- `SettingsEditor` gains prop `canManageDoor: boolean`.

- [ ] **Step 1: Write the failing test**

Add to `Admin.test.tsx` (the `PAGE` const gains `door: { joinPolicy: 'request', defaultDataRole: 'send', medianDecisionDays: null, openRequests: 0, watcherCount: 0, watching: false, applied: null }`):

```tsx
  it('lets the coordinator change the door from settings', async () => {
    const user = userEvent.setup();
    renderAdmin();
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save settings' })).toBeInTheDocument());
    await user.selectOptions(screen.getByRole('combobox', { name: 'Who may join' }), 'open');
    await user.selectOptions(screen.getByRole('combobox', { name: 'What a new member may do' }), 'send_receive');
    await user.click(screen.getByRole('button', { name: 'Save door' }));
    await waitFor(() => expect(apiPatch).toHaveBeenCalledWith('/api/v1/projects/p1', { joinPolicy: 'open', defaultDataRole: 'send_receive' }));
  });
```

- [ ] **Step 2: Run to see it fail**

Run: `cd portal && npx vitest run src/pages/Admin.test.tsx -t "change the door"`
Expected: FAIL — no combobox "Who may join".

- [ ] **Step 3: Implement**

Inside `SettingsEditor`, after the settings `Card` and before the status buttons, add a second card. New props: `canManageDoor: boolean`. State:

```tsx
  const [joinPolicy, setJoinPolicy] = useState<JoinPolicy>(page.door?.joinPolicy ?? 'request');
  const [defaultDataRole, setDefaultDataRole] = useState<DataRole>(page.door?.defaultDataRole ?? 'send');
```

Markup:

```tsx
      {canManageDoor && (
        <Card>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              onPatch({ joinPolicy, defaultDataRole }, 'Door saved.');
            }}
            className="flex flex-col gap-3"
          >
            <h3 className="text-[0.95rem] font-medium">The door</h3>
            <Select label="Who may join" value={joinPolicy} onChange={(e) => setJoinPolicy(e.target.value as JoinPolicy)} help="Never affects anyone already in.">
              <option value="open">Open — anyone signed in joins at once</option>
              <option value="request">By request — I review each one</option>
              <option value="invite">Invite only — nobody can ask</option>
            </Select>
            <Select label="What a new member may do" value={defaultDataRole} onChange={(e) => setDefaultDataRole(e.target.value as DataRole)} help="The role an open-door join lands on, and the default offered when you approve a request.">
              {ROLE_OPTIONS.map((option) => (
                <option key={option.role} value={option.role}>
                  {option.label}
                </option>
              ))}
            </Select>
            <Button type="submit" size="sm" className="self-start">
              Save door
            </Button>
          </form>
        </Card>
      )}
```

Import `JoinPolicy` from `../types`. Call site: `<SettingsEditor page={page} canManageDoor={canManageInvites(membership)} onPatch={…} />`. Because a `project.edit`-less coordinator is impossible (coordinator holds every cap), the door card sits inside the coordinator branch; for an `invites.manage` delegate who is not coordinator the door card renders alone: change the `coordinator ? … : …` branch to

```tsx
      {coordinator ? (
        <SettingsEditor page={page} canManageDoor onPatch={…} />
      ) : canManageInvites(membership) ? (
        <DoorOnly page={page} onPatch={…} />
      ) : (
        <Note>Only the coordinator can change settings.</Note>
      )}
```

where `DoorOnly` is the door card extracted into its own small component (`function DoorCard({ page, onPatch })`) used by both branches. Keep the "Only the coordinator can change settings." note text unchanged so the existing delegate test still passes.

- [ ] **Step 4: Run tests and typecheck**

Run: `cd portal && npx vitest run src/pages/Admin.test.tsx && npx tsc -b`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add portal/src/pages/Admin.tsx portal/src/pages/Admin.test.tsx
git commit -m "feat(portal): door (join policy, default role) editable from Admin"
```

---

### Task 7: Members & roles editor with capability checkboxes

**Files:**
- Create: `athenaeum-hub/portal/src/components/MembersEditor.tsx`
- Create: `athenaeum-hub/portal/src/components/MembersEditor.test.tsx`
- Modify: `athenaeum-hub/portal/src/types.ts` (add `MemberAdminView`, `GOV_CAPS`)
- Modify: `athenaeum-hub/portal/src/pages/Admin.tsx` (load `/members` when `canManageMembers`, render the editor between Join requests and Invite links)
- Modify: `athenaeum-hub/portal/src/pages/Admin.test.tsx` (mock `/api/v1/projects/p1/members` → `[]` by default)

**Interfaces:**
- `types.ts`:
  ```ts
  export type GovCap = 'members.manage' | 'data.moderate' | 'project.edit' | 'thresholds.edit' | 'invites.manage' | 'posts.write';
  export const GOV_CAPS: { cap: GovCap; label: string; help: string }[];
  export interface MemberAdminView { accountId: string; displayName: string; dataRole: DataRole; coordinator: boolean; govCaps: string[]; joinedAt: string; handle: string | null }
  ```
- `MembersEditor({ members, canEdit, onPatch }: { members: MemberAdminView[]; canEdit: boolean; onPatch: (accountId: string, body: { dataRole?: DataRole; govCaps?: string[] }, ok: string) => void })`.
- Consumes: hub `GET /projects/{id}/members` (Task 2), `PATCH /projects/{id}/members/{accountId}` (existing: 409 on self-edit and on editing the coordinator's caps).

`GOV_CAPS` labels: members.manage "Manage members and requests", data.moderate "Moderate contributions", project.edit "Edit project settings", thresholds.edit "Edit quality thresholds", invites.manage "Manage the door and invites", posts.write "Write updates".

- [ ] **Step 1: Write the failing tests**

`portal/src/components/MembersEditor.test.tsx`:

```tsx
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MembersEditor } from './MembersEditor';
import type { MemberAdminView } from '../types';

afterEach(cleanup);

const MEMBERS: MemberAdminView[] = [
  { accountId: 'c', displayName: 'Coord', dataRole: 'send_receive', coordinator: true, govCaps: [], joinedAt: '2026-01-01T00:00:00Z', handle: null },
  { accountId: 'a', displayName: 'Anna', dataRole: 'send', coordinator: false, govCaps: ['members.manage'], joinedAt: '2026-01-02T00:00:00Z', handle: 'anna' },
];

describe('MembersEditor', () => {
  it('lists members with role selects and capability checkboxes; the coordinator row is read-only', () => {
    render(<MembersEditor members={MEMBERS} canEdit onPatch={vi.fn()} />);
    const anna = screen.getByRole('row', { name: /Anna/ });
    expect(within(anna).getByRole('combobox', { name: 'Data role' })).toHaveValue('send');
    expect(within(anna).getByRole('checkbox', { name: 'Manage members and requests' })).toBeChecked();
    expect(within(anna).getByRole('checkbox', { name: 'Edit quality thresholds' })).not.toBeChecked();
    const coord = screen.getByRole('row', { name: /Coord/ });
    expect(within(coord).queryByRole('combobox')).toBeNull();
    expect(within(coord).getByText('Coordinator — holds every capability')).toBeInTheDocument();
  });

  it('patches the role and the full capability set on change', async () => {
    const user = userEvent.setup();
    const onPatch = vi.fn();
    render(<MembersEditor members={MEMBERS} canEdit onPatch={onPatch} />);
    const anna = screen.getByRole('row', { name: /Anna/ });
    await user.selectOptions(within(anna).getByRole('combobox', { name: 'Data role' }), 'send_receive');
    expect(onPatch).toHaveBeenCalledWith('a', { dataRole: 'send_receive' }, 'Role updated.');
    await user.click(within(anna).getByRole('checkbox', { name: 'Edit quality thresholds' }));
    expect(onPatch).toHaveBeenCalledWith('a', { govCaps: ['members.manage', 'thresholds.edit'] }, 'Capabilities updated.');
  });

  it('renders read-only when the viewer cannot edit', () => {
    render(<MembersEditor members={MEMBERS} canEdit={false} onPatch={vi.fn()} />);
    expect(screen.getByRole('checkbox', { name: 'Manage members and requests' })).toBeDisabled();
  });
});
```

- [ ] **Step 2: Run to see it fail**

Run: `cd portal && npx vitest run src/components/MembersEditor.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

`types.ts` additions (after `MemberPublicView`):

```ts
export type GovCap = 'members.manage' | 'data.moderate' | 'project.edit' | 'thresholds.edit' | 'invites.manage' | 'posts.write';

/** Mirrors the hub's `GOV_CAPS` (`src/collab_auth.rs`); order is display order. */
export const GOV_CAPS: { cap: GovCap; label: string; help: string }[] = [
  { cap: 'members.manage', label: 'Manage members and requests', help: 'Approve, decline, remove; grant the capabilities they hold themselves.' },
  { cap: 'data.moderate', label: 'Moderate contributions', help: 'Approve first publications, exclude frames.' },
  { cap: 'project.edit', label: 'Edit project settings', help: 'Description, goals, chat link, house rules.' },
  { cap: 'thresholds.edit', label: 'Edit quality thresholds', help: 'Publish a new rule version.' },
  { cap: 'invites.manage', label: 'Manage the door and invites', help: 'Join policy, default role, invite links.' },
  { cap: 'posts.write', label: 'Write updates', help: 'Post to the project feed.' },
];

/** `GET /projects/{id}/members` — the roster with account ids and flags. */
export interface MemberAdminView {
  accountId: string;
  displayName: string;
  dataRole: DataRole;
  coordinator: boolean;
  govCaps: string[];
  joinedAt: string;
  handle: string | null;
}
```

`components/MembersEditor.tsx`:

```tsx
import { ROLE_OPTIONS } from './RolePicker';
import { GOV_CAPS, type DataRole, type MemberAdminView } from '../types';
import { Card, Select } from '../ui';

const TABLE = 'w-full border-collapse text-left text-[0.86rem]';
const TH = 'px-[10px] pb-[7px] pt-0 text-left text-[0.72rem] font-semibold uppercase tracking-[0.08em] text-content-faint';
const TD = 'border-t border-border px-[10px] py-[9px] align-top';

export function MembersEditor({
  members,
  canEdit,
  onPatch,
}: {
  members: MemberAdminView[];
  canEdit: boolean;
  onPatch: (accountId: string, body: { dataRole?: DataRole; govCaps?: string[] }, ok: string) => void;
}) {
  const toggleCap = (m: MemberAdminView, cap: string, on: boolean) => {
    const next = on ? [...m.govCaps, cap] : m.govCaps.filter((c) => c !== cap);
    // Keep the hub's order so two edits never differ only by ordering.
    const ordered = GOV_CAPS.map((g) => g.cap).filter((c) => next.includes(c));
    onPatch(m.accountId, { govCaps: ordered }, 'Capabilities updated.');
  };

  return (
    <section className="flex flex-col gap-2">
      <h2 className="text-[1.25rem]">Members &amp; roles</h2>
      <Card className="overflow-x-auto">
        <table className={TABLE}>
          <thead>
            <tr>
              <th className={TH}>Member</th>
              <th className={TH}>Data role</th>
              <th className={TH}>Capabilities</th>
            </tr>
          </thead>
          <tbody>
            {members.map((m) => (
              <tr key={m.accountId} aria-label={m.displayName}>
                <td className={TD}>
                  <span className="font-medium">{m.displayName}</span>
                  {m.handle && <span className="ml-1 text-content-faint">@{m.handle}</span>}
                </td>
                <td className={TD}>
                  {m.coordinator ? (
                    <span className="text-content-faint">Coordinator — holds every capability</span>
                  ) : (
                    <Select
                      value={m.dataRole}
                      disabled={!canEdit}
                      aria-label="Data role"
                      className="w-auto"
                      onChange={(e) => onPatch(m.accountId, { dataRole: e.target.value as DataRole }, 'Role updated.')}
                    >
                      {ROLE_OPTIONS.map((o) => (
                        <option key={o.role} value={o.role}>
                          {o.label}
                        </option>
                      ))}
                    </Select>
                  )}
                </td>
                <td className={TD}>
                  {!m.coordinator && (
                    <div className="flex flex-col gap-1">
                      {GOV_CAPS.map((g) => (
                        <label key={g.cap} className="flex items-center gap-2 text-[0.82rem]" title={g.help}>
                          <input
                            type="checkbox"
                            className="accent-accent"
                            checked={m.govCaps.includes(g.cap)}
                            disabled={!canEdit}
                            aria-label={g.label}
                            onChange={(e) => toggleCap(m, g.cap, e.target.checked)}
                          />
                          {g.label}
                        </label>
                      ))}
                    </div>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </Card>
      <p className="text-[0.78rem] text-content-faint">A delegate can grant only the capabilities they hold themselves; the coordinator’s own flags change by handover.</p>
    </section>
  );
}
```

`Admin.tsx`: state `const [members, setMembers] = useState<MemberAdminView[]>([]);`; in `reload` under `canManageMembers(m)` add `setMembers(await apiGet<MemberAdminView[]>(`/api/v1/projects/${p.project.id}/members`));`; render after the Join requests section:

```tsx
      <MembersEditor
        members={members}
        canEdit={canManageMembers(membership)}
        onPatch={(accountId, body, ok) => void act(() => apiPatch(`/api/v1/projects/${projectId}/members/${accountId}`, body), ok)}
      />
```

`Admin.test.tsx`: add `if (url === '/api/v1/projects/p1/members') return [];` to the `apiGet` mock.

- [ ] **Step 4: Run all portal tests and typecheck**

Run: `cd portal && npx vitest run && npx tsc -b`
Expected: PASS (all suites), clean.

- [ ] **Step 5: Commit**

```bash
git add portal/src/components/MembersEditor.tsx portal/src/components/MembersEditor.test.tsx portal/src/types.ts portal/src/pages/Admin.tsx portal/src/pages/Admin.test.tsx
git commit -m "feat(portal): members & roles editor — data role select and capability checkboxes per member"
```

---

### Task 8: NewProject defaults from the registry

**Files:**
- Modify: `athenaeum-hub/portal/src/pages/NewProject.tsx:8-13` and the static thresholds `Note` near line 167.

- [ ] **Step 1: Write the failing test**

Append to `portal/src/pages/NewProject.test.tsx` (or create `describe('NewProject thresholds')` in it) — look at the file's existing render helper and reuse it:

```tsx
  it('shows the initial thresholds it will create, in words', async () => {
    renderNewProject();
    expect(await screen.findByText(/FWHM ≤ 3.5″, Eccentricity ≤ 0.6, Reject trailed frames/)).toBeInTheDocument();
  });
```

- [ ] **Step 2: Run to see it fail**

Run: `cd portal && npx vitest run src/pages/NewProject.test.tsx -t "initial thresholds"`
Expected: FAIL (text absent).

- [ ] **Step 3: Implement**

Replace lines 8-13:

```ts
import { formatRule, metricSpec } from '../metrics';
import type { ThresholdRule } from '../types';

/** Spec §8 template defaults for the initial quality thresholds — values
 * come from the registry so the form and the editor never disagree. */
const DEFAULT_THRESHOLDS: ThresholdRule[] = ['fwhm_arcsec', 'eccentricity', 'not_trailed'].map((key) => {
  const spec = metricSpec(key)!;
  return { metricKey: spec.key, op: spec.defaultOp, value: spec.defaultValue };
});
```

Replace the static `Note` about thresholds (near line 167) with:

```tsx
        <Note>Starts with these quality rules: {DEFAULT_THRESHOLDS.map(formatRule).join(', ')}. Change them any time under Admin.</Note>
```

- [ ] **Step 4: Run tests and typecheck**

Run: `cd portal && npx vitest run src/pages/NewProject.test.tsx && npx tsc -b`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add portal/src/pages/NewProject.tsx portal/src/pages/NewProject.test.tsx
git commit -m "feat(portal): new-project thresholds come from the registry and are shown in words"
```

---

### Task 9: App-side registry pin

**Files:**
- Modify: `athenaeum/crates/athenaeum-core/src/collab/gate.rs` (add the constant near `ThresholdRuleView`, extend the tests module)

**Interfaces:**
- Produces: `pub const METRIC_REGISTRY: &[(&str, &[&str])]` — `(metric_key, allowed ops)`, the third copy of §6.3.

- [ ] **Step 1: Write the failing test**

Append inside `mod tests` of `gate.rs`:

```rust
    /// The registry constant and the match arms in `evaluate_frame` are two
    /// statements of the same fact; this test makes them one. Every registry
    /// metric with a satisfiable value must produce a failure when the rule is
    /// violated, and a key outside the registry must be skipped.
    #[test]
    fn registry_matches_the_evaluator() {
        assert_eq!(
            METRIC_REGISTRY.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["fwhm_arcsec", "eccentricity", "stars_detected", "median_snr", "snr_weight", "frame_snr", "not_trailed"]
        );
        let mut a = analysis(1.2, 0.4, 400, true);
        a.median_snr = 1.0;
        a.snr_weight = 1.0;
        a.frame_snr = 1.0;
        for (key, ops) in METRIC_REGISTRY {
            for op in *ops {
                let value = if *key == "not_trailed" { serde_json::json!(true) } else if *op == "lte" { serde_json::json!(0.0001) } else { serde_json::json!(1_000_000) };
                let rule: ThresholdRuleView = serde_json::from_value(serde_json::json!({"metricKey": key, "op": op, "value": value})).unwrap();
                let row = evaluate_frame(&input(Some(a.clone())), &target(), &[rule]);
                assert!(!row.publishable, "{key} {op} must be enforceable, failures: {:?}", row.failures);
            }
        }
    }
```

If `FrameAnalysis` is not `Clone`, build `a` inside the loop instead.

- [ ] **Step 2: Run to see it fail**

Run: `cd athenaeum && cargo test -p athenaeum-core --lib collab::gate::tests::registry_matches_the_evaluator`
Expected: FAIL to compile — `METRIC_REGISTRY` undefined.

- [ ] **Step 3: Add the constant**

After `ThresholdRuleView` in `gate.rs`:

```rust
/// The threshold metric registry (collab v3 spec §6.3) — the app's copy. The
/// hub's `src/collab_rules.rs` and the portal's `metrics.ts` are the other two;
/// the test `registry_matches_the_evaluator` pins this one to the match arms
/// below. `lte`/`gte` compare a number; `reject_if` takes the literal `true`.
pub const METRIC_REGISTRY: &[(&str, &[&str])] = &[
    ("fwhm_arcsec", &["lte", "gte"]),
    ("eccentricity", &["lte", "gte"]),
    ("stars_detected", &["lte", "gte"]),
    ("median_snr", &["lte", "gte"]),
    ("snr_weight", &["lte", "gte"]),
    ("frame_snr", &["lte", "gte"]),
    ("not_trailed", &["reject_if"]),
];
```

- [ ] **Step 4: Run the whole core suite (owner rule: all targets before any push; here it is the gate module plus a compile of everything)**

Run: `cd athenaeum && cargo test -p athenaeum-core --lib collab::gate && cargo check -p athenaeum-core --all-targets`
Expected: PASS, clean.

- [ ] **Step 5: Commit (app repo)**

```bash
cd athenaeum && git add crates/athenaeum-core/src/collab/gate.rs
git commit -m "feat(collab): metric registry constant pinned to the gate's evaluator (collab v3 §6.3)"
```

---

### Task 10: Whole-branch verification and docs

**Files:**
- Modify: `athenaeum/docs/superpowers/open-items.md` — add a smoke line under the wave-0 heading.
- Modify: `athenaeum-hub/README.md` — API section: `validate_rules` registry, `GET /projects/{id}/members`.

- [ ] **Step 1: Run everything**

```bash
cd athenaeum-hub && DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test 2>&1 | tail -5
cd portal && npx vitest run 2>&1 | tail -5 && npx tsc -b && npm run build 2>&1 | tail -3
cd ../../athenaeum && cargo test -p athenaeum-core 2>&1 | tail -5
```

Expected: all green, 0 warnings in the hub build, the portal `dist/` rebuilt (the release binary embeds it).

- [ ] **Step 2: Browser click-through on a local hub** (`cargo run` in the hub with a fresh DB, `npm run dev` in `portal`): create a project, open Admin, add a rule, type `0.6` into eccentricity, save, see v2 in history; grant a member `thresholds.edit`, sign in as them, see the editor and no settings; change the door; check the console has zero errors. Record the result in `open-items.md` under a new heading "Collab v3 wave 0 (2026-09-23)" as either "verified" or a smoke owed.

- [ ] **Step 3: Docs**

`athenaeum-hub/README.md` API section: replace the sentence about opaque rule shape with "Rules are validated against the metric registry in `src/collab_rules.rs` (unknown metric/op/value kind → 400 naming the rule)"; add the `GET /api/v1/projects/{id}/members` row (members.manage).

- [ ] **Step 4: Commit both repos**

```bash
cd athenaeum-hub && git add README.md && git commit -m "docs(hub): registry-validated rules; member-facing roster route"
cd ../athenaeum && git add docs/superpowers/open-items.md && git commit -m "docs: collab v3 wave 0 smoke ledger"
```

No push, no deploy — the owner's word (test-hub first, `hub_artifact_ref=collab-v3-wave0`).

---

## Self-review

- **Spec coverage (R20, §6.3, §8.3 wave 0):** dropdowns + units + parse-on-blur (Task 4), hub registry validation naming the rule (Task 1), editor open to `thresholds.edit` (Task 5), door fields in Admin (Task 6), delegate capability checkboxes (Tasks 2, 7), `ROLE_LABEL` everywhere (Task 5), NewProject from the registry (Task 8), app-side pin (Task 9). `zero_point` deliberately absent until wave 3 (Global Constraints).
- **Placeholders:** none; every step has code and a command.
- **Type consistency:** `ThresholdRule` (Task 3) is used by Tasks 4, 5, 8; `MemberAdminView`/`GOV_CAPS` (Task 7) match Task 2's wire (`accountId, displayName, dataRole, coordinator, govCaps, joinedAt, handle`); `parseRuleValue` returns `{ok, value|error}` in Tasks 3 and 4; `onPatch(accountId, body, ok)` in Task 7 matches Admin's `act`.
