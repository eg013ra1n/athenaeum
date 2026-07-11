# Sync Phase 1 — Hub (capability + unique names) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evolve the hub from a one-primary-per-account model to explicit multi-node: every device carries a `capability` (`athenaeum` = full peer, `perseus` = send-only), the one-primary limit and the role/pairing endpoints are removed, and node names are unique per account and renamable.

**Architecture:** `athenaeum-hub` is a Rust/Axum HTTP service over Postgres (sqlx runtime queries, all parameterized). Schema changes ride numbered SQL migrations applied at boot and in tests. Behavior is added via the `/api/v1/*` handlers and pinned by `#[sqlx::test]` integration tests that provision a fresh DB per test.

**Tech Stack:** Rust, axum 0.8, sqlx 0.8 (Postgres, runtime `query`/`query_as` + `.bind`), argon2/sha2 (unchanged), `#[sqlx::test]` integration tests.

**Spec:** `docs/superpowers/specs/2026-07-11-sync-model-phase1-design.md` §3, §5. This plan implements the **hub half** only (Plan 1 of 3). Client changes (core/perseus/tauri) are Plan 2.

**Repo:** all paths below are in `athenaeum-hub` (sibling repo `../athenaeum-hub`, on `main`).

## Global Constraints

- Language/stack: Rust + axum 0.8 + sqlx 0.8 Postgres; **every query parameterized** via `$N` + `.bind()` (never `format!` user input into SQL).
- Tests: `#[sqlx::test]` needs a reachable Postgres. Run with `DATABASE_URL=postgres://hub:hub@localhost:5432/hub`. Bring one up once with: `docker run -d --name hub-test-pg -e POSTGRES_USER=hub -e POSTGRES_PASSWORD=hub -e POSTGRES_DB=hub -p 5432:5432 postgres:16`.
- Migrations are append-only numbered files in `migrations/`; last existing is `0006_otp_verify_attempts.sql`. Use `IF NOT EXISTS` / `IF EXISTS` guards (the existing style).
- Capability values are exactly the strings `athenaeum` and `perseus`. Default is `athenaeum`.
- Errors: reuse `ApiError` (`bad_request`, `conflict`, `not_found`, `Status`); the unique-violation helper is `routes::is_unique_violation(&err, "<constraint_name>")`.
- Commits: author is the repo user (`eg013ra1n`); **no Claude co-author/footer** (owner rule).

---

### Task 1: `capability` column + verify sets it + drop the one-primary limit

Replaces the `role`-based model's core: adds `devices.capability`, has `/auth/verify` set it from a new `deviceCapability` field, removes the one-primary DB limit and the role/pairing logic from the verify upsert, and returns `capability` (not `role`/`peerDeviceId`) from `GET /devices`.

**Files:**
- Create: `migrations/0007_capability.sql`
- Modify: `src/routes/auth.rs` (`VerifyRequest`, `verify_otp` upsert)
- Modify: `src/routes/devices.rs` (`DeviceRow`, `DeviceView`, `DEVICE_COLUMNS`, `list_devices`)
- Test: `tests/device_registry.rs`, `tests/common/mod.rs`

**Interfaces:**
- Consumes: `common::{app_with_capture, register_device, post, get, send, as_json, pubkey_b64}`.
- Produces: on-wire `deviceCapability` request field on `POST /api/v1/auth/verify` (optional, default `"athenaeum"`); `capability` field on each `GET /api/v1/devices` row; `DEVICE_COLUMNS` now `"id, name, pubkey, capability, created_at, last_seen_at"`.

- [ ] **Step 1: Write the migration**

Create `migrations/0007_capability.sql`:

```sql
-- Device capability replaces the primary/capture role model (sync Phase 1).
-- 'athenaeum' = a full peer (receives + sends); 'perseus' = send-only.
-- Every device is one capability; there is no longer a per-account primary
-- limit, so multiple 'athenaeum' devices coexist.
ALTER TABLE devices
    ADD COLUMN IF NOT EXISTS capability text NOT NULL DEFAULT 'athenaeum';

-- Backfill: existing rows predate the split and cannot be distinguished as
-- perseus from the schema, so all become 'athenaeum'; a Perseus node re-declares
-- 'perseus' on its next verify. (Pre-stable data — no migration signal needed.)
UPDATE devices SET capability = 'athenaeum' WHERE capability IS NULL;

-- The one-active-primary-per-account limit no longer applies.
DROP INDEX IF EXISTS one_primary_per_account;

-- role / peer_device_id are left in place for one release but are no longer
-- written or read; drop them in a later migration once no client depends on them.
```

- [ ] **Step 2: Write the failing test**

Add to `tests/device_registry.rs`:

```rust
#[sqlx::test]
async fn two_athenaeum_devices_coexist_and_report_capability(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    // Two devices, one account (same email), both default capability.
    register_device(&app, &mailer, "cap@example.com", 1, "mac").await;
    register_device(&app, &mailer, "cap@example.com", 2, "mini").await;

    let (token, _) = register_device(&app, &mailer, "cap@example.com", 3, "third").await;
    let (status, body) = send(&app, get("/api/v1/devices", Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    let arr = as_json(&body);
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 3, "all three athenaeum devices registered (no one-primary block)");
    assert!(arr.iter().all(|d| d["capability"] == "athenaeum"), "default capability is athenaeum: {arr:?}");
    assert!(arr.iter().all(|d| d.get("role").is_none()), "role is no longer on the wire");
}

#[sqlx::test]
async fn verify_honors_device_capability_perseus(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let email = "pcap@example.com";
    let (s, _) = send(&app, post("/api/v1/auth/otp", &json!({ "email": email }), None)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let code = mailer.last_code(email);
    let (status, _) = send(
        &app,
        post(
            "/api/v1/auth/verify",
            &json!({ "email": email, "code": code, "devicePubkey": pubkey_b64(7),
                     "deviceName": "obs", "deviceCapability": "perseus" }),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Sign in a second (athenaeum) device to read the list.
    let (token, _) = register_device(&app, &mailer, email, 8, "viewer").await;
    let (_s, body) = send(&app, get("/api/v1/devices", Some(&token))).await;
    let arr = as_json(&body);
    let obs = arr.as_array().unwrap().iter().find(|d| d["name"] == "obs").unwrap();
    assert_eq!(obs["capability"], "perseus");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test device_registry two_athenaeum_devices_coexist_and_report_capability verify_honors_device_capability_perseus`
Expected: FAIL — `capability` absent from the `GET /devices` payload (and the perseus test's field is ignored), or a compile error on the not-yet-added behavior.

- [ ] **Step 4: Add `deviceCapability` to the verify request + set capability, simplify the upsert**

In `src/routes/auth.rs`, extend `VerifyRequest`:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRequest {
    email: String,
    code: String,
    device_pubkey: String,
    device_name: Option<String>,
    /// 'athenaeum' (full peer, default) or 'perseus' (send-only).
    device_capability: Option<String>,
}
```

Near the top of `verify_otp` (after `let pubkey = ...`), validate + default the capability:

```rust
    let capability = match body.device_capability.as_deref().unwrap_or("athenaeum") {
        c @ ("athenaeum" | "perseus") => c.to_string(),
        other => return Err(ApiError::bad_request(&format!(
            "deviceCapability must be 'athenaeum' or 'perseus', got '{other}'"
        ))),
    };
```

Replace the `INSERT INTO devices ... ON CONFLICT (pubkey) DO UPDATE ...` upsert (the block that set `role`/`peer_device_id` via `CASE`) with a capability-based one that keeps the re-sign-in semantics (token rotation, un-revoke, name COALESCE, same-account guard) minus the role/pairing machinery:

```rust
    let upserted = sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO devices (account_id, pubkey, name, capability, token_hash) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (pubkey) DO UPDATE SET \
             token_hash = EXCLUDED.token_hash, \
             revoked_at = NULL, \
             name = COALESCE(EXCLUDED.name, devices.name), \
             capability = EXCLUDED.capability, \
             last_seen_at = now() \
         WHERE devices.account_id = EXCLUDED.account_id \
         RETURNING id",
    )
    .bind(account_id)
    .bind(&pubkey)
    .bind(&body.device_name)
    .bind(&capability)
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await?;
```

- [ ] **Step 5: Return `capability` (not role/peer) from `GET /devices`**

In `src/routes/devices.rs`: change `DEVICE_COLUMNS`, `DeviceRow`, `DeviceView`, and its `From` impl:

```rust
const DEVICE_COLUMNS: &str = "id, name, pubkey, capability, created_at, last_seen_at";

#[derive(sqlx::FromRow)]
struct DeviceRow {
    id: Uuid,
    name: Option<String>,
    pubkey: Vec<u8>,
    capability: String,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    id: Uuid,
    name: Option<String>,
    pubkey: String,
    capability: String,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
}

impl From<DeviceRow> for DeviceView {
    fn from(r: DeviceRow) -> Self {
        DeviceView {
            id: r.id,
            name: r.name,
            pubkey: security::encode_pubkey(&r.pubkey),
            capability: r.capability,
            created_at: r.created_at,
            last_seen_at: r.last_seen_at,
        }
    }
}
```

`list_devices`'s query already selects `{DEVICE_COLUMNS}` and is account-scoped — no change to its body.

- [ ] **Step 6: Run the two new tests to verify they pass**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test device_registry two_athenaeum_devices_coexist_and_report_capability verify_honors_device_capability_perseus`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add migrations/0007_capability.sql src/routes/auth.rs src/routes/devices.rs tests/device_registry.rs
git commit -m "feat(hub): device capability replaces primary/capture role; drop one-primary limit"
```

---

### Task 2: Remove the role/pairing endpoint and repair the affected tests

`POST /devices/{id}/role`, the `RoleRequest`, and the revoke-time inbound-peer clear all encode the pairing model that no longer exists (targets are explicit). Remove them and rewrite the tests that asserted role/peer semantics.

**Files:**
- Modify: `src/routes/devices.rs` (delete `set_role`, `RoleRequest`; simplify `revoke_device`)
- Modify: `src/routes/mod.rs` (drop the `/role` route)
- Modify: `tests/device_registry.rs` (delete role/one-primary tests)
- Modify: `tests/auth_flow.rs` (rewrite the two re-verify role/peer tests)

**Interfaces:**
- Consumes: Task 1's schema (no `role`/`peer_device_id` reads).
- Produces: the router no longer serves `POST /api/v1/devices/{id}/role`; `revoke_device` is a plain account-scoped `revoked_at` stamp.

- [ ] **Step 1: Write the failing test (the role route is gone)**

Add to `tests/device_registry.rs`:

```rust
#[sqlx::test]
async fn role_endpoint_is_removed(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (token, dev) = register_device(&app, &mailer, "gone@example.com", 1, "a").await;
    let (status, _) = send(
        &app,
        post(&format!("/api/v1/devices/{dev}/role"), &json!({ "role": "primary" }), Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the role endpoint no longer exists (unmatched route → 404)");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test device_registry role_endpoint_is_removed`
Expected: FAIL — the route still returns 200/4xx from `set_role`, not 404.

- [ ] **Step 3: Delete `set_role` + `RoleRequest`, simplify `revoke_device`, drop the route**

In `src/routes/devices.rs`: delete the entire `set_role` handler and the `RoleRequest` struct. In `revoke_device`, delete the second query (the `UPDATE devices SET peer_device_id = NULL WHERE peer_device_id = $1 ...` inbound-peer clear) and its `cleared` logging field; keep the account-scoped `UPDATE devices SET revoked_at = now() WHERE id = $1 AND account_id = $2` + the `rows_affected() == 0 → 404`. The handler no longer needs a transaction — a single `execute` on `&state.db` is enough:

```rust
#[tracing::instrument(skip_all)]
pub async fn revoke_device(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let result =
        sqlx::query("UPDATE devices SET revoked_at = now() WHERE id = $1 AND account_id = $2")
            .bind(id)
            .bind(auth.account_id)
            .execute(&state.db)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("device not found"));
    }
    tracing::info!(account_id = %auth.account_id, device_id = %id, "device revoked");
    Ok(StatusCode::NO_CONTENT)
}
```

In `src/routes/mod.rs`, remove the line:

```rust
        .route("/api/v1/devices/{id}/role", post(devices::set_role))
```

`is_unique_violation` is now unused by `devices.rs` — remove its `use` there if the compiler warns (it is still defined in `routes/mod.rs` for Task 3).

- [ ] **Step 4: Delete the role/one-primary tests and rewrite the re-verify tests**

In `tests/device_registry.rs`, delete these tests wholesale (they exercised the removed endpoint): `set_role_primary_with_peer_is_400`, `set_role_capture_peer_must_be_existing_nonrevoked_same_account`, and any test that `POST`s to `/role` or asserts `role`/`peerDeviceId`/`one_primary` (search the file for `/role`, `role`, `peerDeviceId`). Update the survivor at line ~32 that asserts `arr[0]["role"] == "capture"` to assert `arr[0]["capability"] == "athenaeum"` instead.

In `tests/auth_flow.rs`, the two re-verify tests must drop the role/peer assertions. Replace `reverify_of_revoked_ex_primary_demotes_role_and_peer` and `plain_reverify_of_nonrevoked_device_preserves_role_and_peer` with a single test of what re-verify now does (rotate token, un-revoke, keep the same device id, no role concept):

```rust
#[sqlx::test]
async fn reverify_rotates_token_and_unrevokes_same_device(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let email = "rv@example.com";
    let (tok1, dev) = register_device(&app, &mailer, email, 1, "laptop").await;

    // Self-revoke, then re-verify the SAME pubkey (seed 1).
    let (s, _) = send(&app, post(&format!("/api/v1/devices/{dev}/revoke"), &json!({}), Some(&tok1))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (tok2, dev2) = register_device(&app, &mailer, email, 1, "laptop").await;

    assert_eq!(dev2, dev, "re-verify keeps the same device id");
    assert_ne!(tok2, tok1, "re-verify rotates the token");
    // The device is active again and readable.
    let (_s, body) = send(&app, get("/api/v1/devices", Some(&tok2))).await;
    let arr = as_json(&body);
    assert!(arr.as_array().unwrap().iter().any(|d| d["id"] == dev.as_str().unwrap_or_default()
        || d["id"] == serde_json::Value::String(dev.clone())), "the re-verified device is listed");
}
```

(If `dev` is a `String` in the harness, compare with `d["id"] == dev`. Adjust the comparison to the harness's id type — `register_device` returns `(String, String)`.)

- [ ] **Step 5: Run the whole hub suite**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`
Expected: PASS (unit + auth_flow + device_registry + health + relay), no references to the removed endpoint remain.

- [ ] **Step 6: Commit**

```bash
git add src/routes/devices.rs src/routes/mod.rs tests/device_registry.rs tests/auth_flow.rs
git commit -m "refactor(hub): remove role/pairing endpoint + revoke peer-clear (explicit-target model)"
```

---

### Task 3: Unique per-account node names + `PATCH /devices/{id}` rename

Node names become unique among an account's active devices (they key the destination list and the receiver's landing-folder prefix). Registration auto-suffixes a colliding default; explicit rename rejects a taken name with 409.

**Files:**
- Create: `migrations/0008_unique_device_name.sql`
- Modify: `src/routes/auth.rs` (free-name helper; use it in verify)
- Modify: `src/routes/devices.rs` (add `rename_device` handler)
- Modify: `src/routes/mod.rs` (add the `PATCH` route)
- Test: `tests/device_registry.rs`

**Interfaces:**
- Consumes: Task 1 schema; `ApiError::conflict`, `routes::is_unique_violation`.
- Produces: `PATCH /api/v1/devices/{id}` accepting `{ "name": string }` → `200` with the updated `DeviceView`, `409` on a name collision, `404` if the id is not the caller's account. Constraint name for the unique index: `devices_account_name_active`.

- [ ] **Step 1: Write the migration**

Create `migrations/0008_unique_device_name.sql`:

```sql
-- Node names are unique among an account's ACTIVE devices (case-insensitive):
-- they key the destination picker and the receiver's landing-folder prefix.
-- Partial (name NOT NULL, not revoked) so nameless/revoked rows don't collide.
CREATE UNIQUE INDEX IF NOT EXISTS devices_account_name_active
    ON devices (account_id, lower(name))
    WHERE name IS NOT NULL AND revoked_at IS NULL;
```

- [ ] **Step 2: Write the failing tests**

Add to `tests/device_registry.rs`:

```rust
#[sqlx::test]
async fn rename_to_free_name_succeeds_taken_name_409(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let email = "rn@example.com";
    let (tok1, dev1) = register_device(&app, &mailer, email, 1, "alpha").await;
    let (_tok2, _dev2) = register_device(&app, &mailer, email, 2, "beta").await;

    // Free name → 200 + reflected in the view.
    let (s, body) = send(&app, patch(&format!("/api/v1/devices/{dev1}"), &json!({ "name": "gamma" }), Some(&tok1))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(as_json(&body)["name"], "gamma");

    // Taken name (case-insensitive) → 409.
    let (s, _) = send(&app, patch(&format!("/api/v1/devices/{dev1}"), &json!({ "name": "BETA" }), Some(&tok1))).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn rename_other_accounts_device_is_404(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (_tok_a, dev_a) = register_device(&app, &mailer, "a@example.com", 1, "a-dev").await;
    let (tok_b, _dev_b) = register_device(&app, &mailer, "b@example.com", 2, "b-dev").await;
    // B tries to rename A's device.
    let (s, _) = send(&app, patch(&format!("/api/v1/devices/{dev_a}"), &json!({ "name": "hijacked" }), Some(&tok_b))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn verify_auto_suffixes_a_colliding_default_name(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let email = "dup@example.com";
    register_device(&app, &mailer, email, 1, "raspberrypi").await;
    // A second device defaulting to the same hostname must not fail verify.
    let (tok2, _dev2) = register_device(&app, &mailer, email, 2, "raspberrypi").await;
    let (_s, body) = send(&app, get("/api/v1/devices", Some(&tok2))).await;
    let names: Vec<String> = as_json(&body).as_array().unwrap().iter()
        .map(|d| d["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"raspberrypi".to_string()));
    assert!(names.iter().any(|n| n.starts_with("raspberrypi-")), "second collides → auto-suffixed: {names:?}");
}
```

Add a `patch` helper to `tests/common/mod.rs`:

```rust
pub fn patch(uri: &str, body: &Value, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("PATCH").uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    builder.body(Body::from(serde_json::to_vec(body).unwrap())).unwrap()
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test device_registry rename_ verify_auto_suffixes_a_colliding_default_name`
Expected: FAIL — no `PATCH` route (404 for the success case), and verify errors instead of auto-suffixing on the duplicate default.

- [ ] **Step 4: Add the free-name helper + use it in verify**

In `src/routes/auth.rs`, add a helper that returns a per-account unique name (the requested name, or `name-2`, `name-3`, … until free among active devices):

```rust
/// A per-account-unique node name derived from `desired`: returns it if free
/// (case-insensitive, among active devices), else `desired-2`, `desired-3`, …
/// Bounded loop; keys the destination list and the receiver folder prefix.
async fn free_device_name(
    db: &sqlx::PgPool,
    account_id: uuid::Uuid,
    desired: &str,
) -> Result<String, ApiError> {
    for n in 0..1000 {
        let candidate = if n == 0 { desired.to_string() } else { format!("{desired}-{}", n + 1) };
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM devices \
             WHERE account_id = $1 AND revoked_at IS NULL AND lower(name) = lower($2))",
        )
        .bind(account_id)
        .bind(&candidate)
        .fetch_one(db)
        .await?;
        if !taken {
            return Ok(candidate);
        }
    }
    Ok(format!("{desired}-{}", uuid::Uuid::new_v4()))
}
```

In `verify_otp`, when a `device_name` is present, resolve it to a free name **before** the upsert (compute against `account_id`, which is available after the account upsert), and bind the resolved name instead of the raw `body.device_name`:

```rust
    let resolved_name = match &body.device_name {
        Some(n) if !n.trim().is_empty() => Some(free_device_name(&state.db, account_id, n.trim()).await?),
        _ => None,
    };
```

Then in the device upsert bind `resolved_name` in place of `&body.device_name`. (Re-sign-in of the SAME device keeps its stored name via the `COALESCE(EXCLUDED.name, devices.name)`; passing `None` when no name is sent is unchanged.)

- [ ] **Step 5: Add the `rename_device` handler + route**

In `src/routes/devices.rs`:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameRequest {
    name: String,
}

/// `PATCH /api/v1/devices/{id}` — rename a device (account-scoped). 409 if the
/// name is already taken by another active device on the account; 404 if the id
/// isn't the caller's. Names key the destination picker and the landing-folder
/// prefix, so they are unique among active devices (case-insensitive).
#[tracing::instrument(skip_all)]
pub async fn rename_device(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    Json(body): Json<RenameRequest>,
) -> Result<Json<DeviceView>, ApiError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    let updated = sqlx::query_as::<_, DeviceRow>(&format!(
        "UPDATE devices SET name = $1 \
         WHERE id = $2 AND account_id = $3 AND revoked_at IS NULL \
         RETURNING {DEVICE_COLUMNS}"
    ))
    .bind(name)
    .bind(id)
    .bind(auth.account_id)
    .fetch_optional(&state.db)
    .await;

    match updated {
        Ok(Some(row)) => {
            tracing::info!(account_id = %auth.account_id, device_id = %id, "device renamed");
            Ok(Json(DeviceView::from(row)))
        }
        Ok(None) => Err(ApiError::not_found("device not found")),
        Err(err) if is_unique_violation(&err, "devices_account_name_active") => {
            Err(ApiError::conflict("a device with that name already exists on this account"))
        }
        Err(err) => Err(ApiError::from(err)),
    }
}
```

Re-add the `use crate::routes::{is_unique_violation, AppState};` import if Task 2 removed it. In `src/routes/mod.rs`, register the route on the protected router (import `patch` from `axum::routing`):

```rust
        .route("/api/v1/devices/{id}", axum::routing::patch(devices::rename_device))
```

- [ ] **Step 6: Run the new tests, then the full suite**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test device_registry rename_ verify_auto_suffixes_a_colliding_default_name`
Expected: PASS.
Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`
Expected: PASS (whole suite).

- [ ] **Step 7: Commit**

```bash
git add migrations/0008_unique_device_name.sql src/routes/auth.rs src/routes/devices.rs src/routes/mod.rs tests/device_registry.rs tests/common/mod.rs
git commit -m "feat(hub): unique per-account device names + PATCH rename; verify auto-suffixes collisions"
```

---

## Self-Review

**Spec coverage (§3, §5 hub half):**
- §3 capability replaces role → Task 1 (column, verify `deviceCapability`, view). ✓
- §3 drop `one_primary_per_account` → Task 1 migration. ✓
- §3 role endpoint removed; `peer_device_id` no longer written/read; revoke peer-clear dropped → Task 2. ✓
- §3 `GET /devices` returns capability; destination-list filter is client-side (Plan 2) over this field → Task 1. ✓
- §5 unique per-account name (409 on collision) → Task 3 (index + rename 409). ✓
- §5 default hostname is a **client** concern (Plan 2); the hub only enforces uniqueness + auto-suffixes a colliding default at verify → Task 3. ✓
- §5 `PATCH /devices/{id}` rename → Task 3. ✓
- §5 receiver-resolved sanitized slug is a **client/core** concern (Plan 2), not the hub. Out of scope here (noted). ✓

**Placeholder scan:** No TBD/TODO; every code step shows the code; the one harness-dependent comparison (device id String vs Value in Task 2 Step 4) is called out with the fix inline.

**Type consistency:** `DEVICE_COLUMNS` = `"id, name, pubkey, capability, created_at, last_seen_at"` is used identically by `DeviceRow`, `list_devices`, and `rename_device`'s `RETURNING`. `capability` is `String` everywhere. Constraint name `devices_account_name_active` matches between the migration and `is_unique_violation`. `free_device_name(db, account_id, desired) -> Result<String, ApiError>` is defined in Task 3 and used only there.

---

## Execution Handoff

Plan complete. Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks (uses `superpowers:subagent-driven-development`).
2. **Inline Execution** — tasks in this session with checkpoints (uses `superpowers:executing-plans`).
