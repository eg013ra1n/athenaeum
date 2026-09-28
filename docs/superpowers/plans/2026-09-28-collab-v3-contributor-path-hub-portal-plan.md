# Collab v3 contributor path — hub + portal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every project a default dictionary entry for unfiltered frames (`None` / `unfiltered`), refuse a dictionary PUT that mints nothing or orphans published frames, and give the portal an Admin editor for the dictionary.

**Architecture:** Three additive hub changes in `routes/dictionary.rs` (a fourth `kind`, a second seed constant appended by `default_dictionary()`, two early returns in `put_dictionary`) plus one idempotent SQL migration that appends a new dictionary version to every existing project and bumps `projects.version` so apps reload. The portal gains `DictionaryEditor.tsx` beside `ThresholdEditor.tsx` with the same seed-once / re-mount-by-version / Save-disabled-while-unchanged contract, mounted in `Admin.tsx` behind `thresholds.edit`.

**Tech Stack:** Rust (axum, sqlx/Postgres, `#[sqlx::test]`), SQL migration under `migrations/`, portal React + TypeScript + Vitest (`@testing-library/react`, `user-event`), Tailwind tokens via the portal `ui/` primitives.

**Spec:** `docs/superpowers/specs/2026-09-28-collab-v3-contributor-path-design.md` (Part I §4; F4). Repository: `Documents/Projects/athenaeum-hub` (portal under `athenaeum-hub/portal/`; **never** the stale sibling `athenaeum-hub-portal/`). Work on a branch `contributor-path-hub` from local `main` (`f2c1385`).

## Global Constraints

- `DEFAULT_DICTIONARY_JSON` and migration `0022_per_frame_model.sql` stay byte-identical (spec F4; pin `tests/dictionary.rs::migration_backfill_json_matches_default_dictionary`). The new entry is a SECOND constant and a NEW migration `0026`.
- The unfiltered entry, verbatim: `{"canonical":"None","aliases":["none","nofilter","no filter","no-filter","unfiltered"],"kind":"unfiltered"}`.
- Kinds: exactly `broadband`, `narrowband`, `luminance`, `unfiltered`.
- Refusal sentences, verbatim: `entries are identical to the current version` and `canonical "X" is used by N frames — remap them first` (409 both).
- Every route keeps `#[tracing::instrument(skip_all)]`; every refusal is logged before it returns (never-swallow).
- Portal copy: "Filter dictionary", "Save as new version", "No changes to save.", "Add entry", "Remove entry".
- Full hub suite (`DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`, Postgres from `docker-compose.yml` already running on this Mac) and the full portal suite (`npm test` in `portal/`) green before the branch is merged; nothing is pushed or deployed on this plan — the owner's word.
- No third-party project names in code, comments, docs or commit messages.

## Review Focus

1. A project whose CURRENT dictionary already carries alias `none` (custom PUT before this ship) must be skipped by 0026, not violate alias uniqueness — Task 2's test `migration_0026_skips_a_project_whose_aliases_collide`.
2. Removing a canonical that only PENDING (not yet accepted) frames use must still be refused: `project_frames` rows of any `state` count — Task 3's `removing_a_canonical_in_use_is_refused` announces without approval.
3. Renaming `Ha` to `HA` is a removal in the hub's eyes (case-sensitive `filter_canonical`) — Task 3's test asserts the 409 on a case-only rename while frames use it.
4. A PUT whose only difference is alias whitespace (`" red "` vs `"red"`) is identical after trimming and must 409, never mint — Task 3's `identical_entries_are_refused` posts the trimmed-equal body.
5. The portal editor must not let an empty canonical or a duplicate alias reach the hub (the hub would 400 with a sentence the user cannot act on inline) — Task 5's inline-flag tests.

---

### Task 1: The `unfiltered` kind and the `None` entry in `default_dictionary()`

**Files:**
- Modify: `src/routes/dictionary.rs:22-46` (`KINDS`, the constants, `default_dictionary`)
- Test: `tests/dictionary.rs`

**Interfaces:**
- Produces: `pub const UNFILTERED_ENTRY_JSON: &str`; `pub fn unfiltered_entry() -> DictEntry`; `default_dictionary()` now returns 8 entries (`L R G B Ha OIII SII None`).

- [ ] **Step 1: Write the failing tests**

Append to `tests/dictionary.rs`:

```rust
/// Spec F4: the seed constant of wave 1 is untouched (its pin above), and
/// the unfiltered entry is a SECOND constant `default_dictionary()` appends.
#[test]
fn default_dictionary_ends_with_the_unfiltered_entry() {
    use athenaeum_hub::routes::dictionary::{unfiltered_entry, DEFAULT_DICTIONARY_JSON, UNFILTERED_ENTRY_JSON};
    let seven: Vec<DictEntry> = serde_json::from_str(DEFAULT_DICTIONARY_JSON).unwrap();
    assert_eq!(seven.len(), 7, "the wave-1 seed constant must stay seven entries");
    let all = default_dictionary();
    assert_eq!(all.len(), 8);
    assert_eq!(&all[..7], &seven[..]);
    assert_eq!(all[7], unfiltered_entry());
    let e: DictEntry = serde_json::from_str(UNFILTERED_ENTRY_JSON).unwrap();
    assert_eq!(e.canonical, "None");
    assert_eq!(e.kind, "unfiltered");
    assert_eq!(e.aliases, ["none", "nofilter", "no filter", "no-filter", "unfiltered"]);
}

#[sqlx::test]
async fn new_project_has_none_and_announce_accepts_it(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&coord))).await;
    let d = as_json(&body);
    let canon: Vec<&str> = d["current"]["entries"].as_array().unwrap().iter().map(|e| e["canonical"].as_str().unwrap()).collect();
    assert_eq!(canon, ["L", "R", "G", "B", "Ha", "OIII", "SII", "None"]);
    assert_eq!(d["current"]["entries"][7]["kind"], "unfiltered");

    let mut f = frame_body(1);
    f["filterRaw"] = json!("");
    f["filterCanonical"] = json!("None");
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [f]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    // `unfiltered` is a valid kind on PUT; a made-up one is not.
    let mut entries = d["current"]["entries"].as_array().unwrap().clone();
    entries.push(json!({"canonical": "Dual", "aliases": ["dualband"], "kind": "unfiltered"}));
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": [{"canonical": "X", "aliases": [], "kind": "osc"}]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("kind"));
}
```

Also update the existing assertion in `new_project_has_the_default_dictionary_and_announce_respects_it` from `["L", "R", "G", "B", "Ha", "OIII", "SII"]` to `["L", "R", "G", "B", "Ha", "OIII", "SII", "None"]`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test dictionary`
Expected: compile error — `unfiltered_entry`/`UNFILTERED_ENTRY_JSON` do not exist.

- [ ] **Step 3: Implement**

In `src/routes/dictionary.rs` replace the `KINDS` line and add below `DEFAULT_DICTIONARY_JSON`:

```rust
const KINDS: [&str; 4] = ["broadband", "narrowband", "luminance", "unfiltered"];

/// Spec 2026-09-28 (contributor path) F4: the default entry for frames shot
/// without a filter — mono without a wheel, OSC without glass. A SECOND
/// constant on purpose: `DEFAULT_DICTIONARY_JSON` is pinned byte-equal to
/// migration 0022 and must never change; this one is pinned the same way to
/// migration 0026 (`tests/dictionary.rs::migration_0026_literal_matches_unfiltered_entry`).
pub const UNFILTERED_ENTRY_JSON: &str = r#"{"canonical":"None","aliases":["none","nofilter","no filter","no-filter","unfiltered"],"kind":"unfiltered"}"#;

pub fn unfiltered_entry() -> DictEntry {
    serde_json::from_str(UNFILTERED_ENTRY_JSON).expect("UNFILTERED_ENTRY_JSON is static JSON")
}
```

and change `default_dictionary()`:

```rust
/// Seed vocabulary for a new project's version 1: the wave-1 seven followed
/// by the unfiltered entry (F4). Projects that predate 0026 get the eighth
/// entry as a new version from that migration.
pub fn default_dictionary() -> Vec<DictEntry> {
    let mut v: Vec<DictEntry> =
        serde_json::from_str(DEFAULT_DICTIONARY_JSON).expect("DEFAULT_DICTIONARY_JSON is static JSON");
    v.push(unfiltered_entry());
    v
}
```

Update the doc comment of `validate_dictionary` to say `kind` is one of the four. **Do not** touch `DEFAULT_DICTIONARY_JSON`.

- [ ] **Step 4: Fix the pin that now sees eight entries**

`migration_backfill_json_matches_default_dictionary` compares the 0022 literal to `default_dictionary()`, which is now eight long. Change its assertion target to the seven:

```rust
    let seven: Vec<DictEntry> = serde_json::from_str(athenaeum_hub::routes::dictionary::DEFAULT_DICTIONARY_JSON).unwrap();
    assert_eq!(
        from_migration,
        seven,
        "migration 0022's backfill literal has drifted from dictionary::DEFAULT_DICTIONARY_JSON"
    );
```

- [ ] **Step 5: Run the dictionary tests**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test dictionary`
Expected: all pass. Then `cargo test --test frames --test feed_publish` — the announce tests still pass (they use `L`).

- [ ] **Step 6: Commit**

```bash
git add src/routes/dictionary.rs tests/dictionary.rs
git commit -m "feat(hub): dictionary kind \`unfiltered\` and the \`None\` entry in every new project's seed (F4)"
```

---

### Task 2: Migration 0026 — the `None` entry for every existing project

**Files:**
- Create: `migrations/0026_unfiltered_dictionary_entry.sql`
- Test: `tests/dictionary.rs`

**Interfaces:**
- Consumes: `UNFILTERED_ENTRY_JSON` (Task 1) — the SQL embeds the same literal.
- Produces: for every project whose latest dictionary lacks the entry and has no colliding names, a new dictionary version and `projects.version + 1`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/dictionary.rs`:

```rust
/// Migration 0026 embeds `UNFILTERED_ENTRY_JSON` as a literal; this pin
/// keeps the two from drifting, exactly as the 0022 pin does.
#[test]
fn migration_0026_literal_matches_unfiltered_entry() {
    use athenaeum_hub::routes::dictionary::unfiltered_entry;
    let migration = include_str!("../migrations/0026_unfiltered_dictionary_entry.sql");
    let needle = "'{\"canonical\":\"None\"";
    let start = migration.find(needle).expect("unfiltered literal not found in migration 0026") + 1;
    let end = migration[start..].find("}'::jsonb").expect("closing `}'::jsonb` not found") + start + 1;
    let from_migration: DictEntry = serde_json::from_str(&migration[start..end]).expect("valid JSON");
    assert_eq!(from_migration, unfiltered_entry());
}

const SEVEN: &str = athenaeum_hub::routes::dictionary::DEFAULT_DICTIONARY_JSON;

/// Re-run the migration's own SQL against a project whose current version
/// is the wave-1 seven (a pre-0026 project): v2 appears with `None` last,
/// `projects.version` moves by one, and a replay changes nothing.
#[sqlx::test]
async fn migration_0026_appends_none_once_and_bumps_the_project_version(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = Uuid::parse_str(project["id"].as_str().unwrap()).unwrap();
    // Make it look pre-0026: version 1 holds the seven only.
    sqlx::query("UPDATE project_filter_dictionary SET entries = $2::jsonb WHERE project_id = $1 AND version = 1")
        .bind(id).bind(SEVEN).execute(&pool).await.unwrap();
    let (before,): (i64,) = sqlx::query_as("SELECT version FROM projects WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();

    let sql = include_str!("../migrations/0026_unfiltered_dictionary_entry.sql");
    sqlx::raw_sql(sql).execute(&pool).await.unwrap();

    let (versions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM project_filter_dictionary WHERE project_id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(versions, 2);
    let (entries,): (serde_json::Value,) = sqlx::query_as("SELECT entries FROM project_filter_dictionary WHERE project_id = $1 AND version = 2").bind(id).fetch_one(&pool).await.unwrap();
    let entries: Vec<DictEntry> = serde_json::from_value(entries).unwrap();
    assert_eq!(entries.len(), 8);
    assert_eq!(entries[7], athenaeum_hub::routes::dictionary::unfiltered_entry());
    let (after,): (i64,) = sqlx::query_as("SELECT version FROM projects WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(after, before + 1);

    // Replay: nothing changes.
    sqlx::raw_sql(sql).execute(&pool).await.unwrap();
    let (versions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM project_filter_dictionary WHERE project_id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(versions, 2);
    let (again,): (i64,) = sqlx::query_as("SELECT version FROM projects WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(again, after);

    // The route serves v2 as current.
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&coord))).await;
    assert_eq!(as_json(&body)["current"]["version"], 2);
}

/// A project whose dictionary already uses one of the new aliases (a custom
/// PUT before this ship) is skipped — the migration must never violate the
/// alias-uniqueness rule; the coordinator adds the entry by hand.
#[sqlx::test]
async fn migration_0026_skips_a_project_whose_aliases_collide(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = Uuid::parse_str(project["id"].as_str().unwrap()).unwrap();
    let mut seven: Vec<DictEntry> = serde_json::from_str(SEVEN).unwrap();
    seven[0].aliases.push("NONE".into()); // case-insensitive collision with the new entry's alias
    sqlx::query("UPDATE project_filter_dictionary SET entries = $2 WHERE project_id = $1 AND version = 1")
        .bind(id).bind(serde_json::to_value(&seven).unwrap()).execute(&pool).await.unwrap();
    let (before,): (i64,) = sqlx::query_as("SELECT version FROM projects WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();

    sqlx::raw_sql(include_str!("../migrations/0026_unfiltered_dictionary_entry.sql")).execute(&pool).await.unwrap();

    let (versions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM project_filter_dictionary WHERE project_id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(versions, 1, "a colliding project is left alone");
    let (after,): (i64,) = sqlx::query_as("SELECT version FROM projects WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(after, before);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test dictionary migration_0026`
Expected: compile error — `include_str!` finds no such file.

- [ ] **Step 3: Write the migration**

Create `migrations/0026_unfiltered_dictionary_entry.sql`:

```sql
-- 0026_unfiltered_dictionary_entry — collab v3 contributor path (spec
-- 2026-09-28, F4): every project's dictionary gains the default entry for
-- frames shot without a filter, canonical `None`, kind `unfiltered`, so a
-- publisher has something to map an empty FILTER to. Migration 0022 and
-- `dictionary::DEFAULT_DICTIONARY_JSON` stay byte-equal (their pin); the
-- literal below is `dictionary::UNFILTERED_ENTRY_JSON` verbatim, pinned by
-- `tests/dictionary.rs::migration_0026_literal_matches_unfiltered_entry`.
--
-- For each project whose LATEST version has no `unfiltered` entry, no
-- canonical `none` (case-insensitive) and none of the five aliases anywhere
-- (case-insensitive), insert version MAX+1 = latest entries || the entry,
-- authored by the project's creator, and bump `projects.version` so every
-- app's next hello reloads the project (a migration cannot publish a feed
-- event). A project that fails the guard is skipped: its coordinator adds
-- the entry in the portal editor. Idempotent: a replay finds the kind
-- present and changes nothing.

WITH latest AS (
    SELECT d.project_id, d.version, d.entries
    FROM project_filter_dictionary d
    JOIN (
        SELECT project_id, MAX(version) AS version
        FROM project_filter_dictionary
        GROUP BY project_id
    ) m ON m.project_id = d.project_id AND m.version = d.version
),
eligible AS (
    SELECT l.project_id, l.version, l.entries
    FROM latest l
    WHERE NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(l.entries) e
        WHERE e->>'kind' = 'unfiltered'
           OR lower(e->>'canonical') = 'none'
    )
    AND NOT EXISTS (
        SELECT 1
        FROM jsonb_array_elements(l.entries) e,
             jsonb_array_elements_text(COALESCE(e->'aliases', '[]'::jsonb)) a
        WHERE lower(trim(a)) IN ('none', 'nofilter', 'no filter', 'no-filter', 'unfiltered')
    )
),
inserted AS (
    INSERT INTO project_filter_dictionary (project_id, version, entries, created_by)
    SELECT e.project_id,
           e.version + 1,
           e.entries || '{"canonical":"None","aliases":["none","nofilter","no filter","no-filter","unfiltered"],"kind":"unfiltered"}'::jsonb,
           p.created_by
    FROM eligible e
    JOIN projects p ON p.id = e.project_id
    RETURNING project_id
)
UPDATE projects
SET version = version + 1
WHERE id IN (SELECT project_id FROM inserted);
```

Note `jsonb || jsonb` on an array and an object appends the object as one element — that is the behaviour the tests pin (eight entries, `None` last).

- [ ] **Step 4: Run the dictionary tests**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test dictionary`
Expected: all pass, including the two 0026 tests and the pin. If `sqlx::raw_sql` is not available in the pinned sqlx version, use `sqlx::query(sql).execute(&pool)` — the file is a single statement.

- [ ] **Step 5: Commit**

```bash
git add migrations/0026_unfiltered_dictionary_entry.sql tests/dictionary.rs
git commit -m "feat(hub): migration 0026 — the \`None\`/\`unfiltered\` entry as a new dictionary version for every existing project (F4)"
```

---

### Task 3: `PUT /dictionary` refuses an identical body and an in-use removal

**Files:**
- Modify: `src/routes/dictionary.rs:240-300` (`put_dictionary`)
- Test: `tests/dictionary.rs`

**Interfaces:**
- Produces: 409 `entries are identical to the current version`; 409 `canonical "X" is used by N frames — remap them first`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/dictionary.rs`:

```rust
/// Same line as `thresholds::post_thresholds` (hub f2c1385): an unchanged
/// body mints nothing. Alias whitespace is trimmed before the comparison,
/// so `" red "` equals `"red"`.
#[sqlx::test]
async fn identical_entries_are_refused(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&coord))).await;
    let mut entries = as_json(&body)["current"]["entries"].clone();
    // Whitespace around an alias is not a change.
    entries[1]["aliases"][0] = json!(" red ");
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&body).contains("identical"));
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&coord))).await;
    assert_eq!(as_json(&body)["current"]["version"], 1, "nothing minted");
    assert_eq!(as_json(&body)["history"].as_array().unwrap().len(), 1);
}

/// A canonical that published frames carry (any state — a pending frame
/// counts) cannot be removed; a case-only rename IS a removal; an unused
/// canonical may go.
#[sqlx::test]
async fn removing_a_canonical_in_use_is_refused(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", true).await; // require approval: frames stay pending
    let id = project["id"].as_str().unwrap();
    let mut f = frame_body(1);
    f["filterCanonical"] = json!("Ha");
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [f]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&coord))).await;
    let entries = as_json(&body)["current"]["entries"].as_array().unwrap().clone();

    // Drop Ha → refused, naming the count.
    let without_ha: Vec<Value> = entries.iter().filter(|e| e["canonical"] != "Ha").cloned().collect();
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": without_ha}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&body));
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(text.contains("canonical \\\"Ha\\\" is used by 1 frame") || text.contains("canonical \"Ha\" is used by 1 frame"), "{text}");

    // Rename Ha → HA is a removal too.
    let renamed: Vec<Value> = entries.iter().map(|e| if e["canonical"] == "Ha" { let mut e = e.clone(); e["canonical"] = json!("HA"); e } else { e.clone() }).collect();
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": renamed}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Dropping SII (unused) is fine and mints v2.
    let without_sii: Vec<Value> = entries.iter().filter(|e| e["canonical"] != "SII").cloned().collect();
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": without_sii}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["version"], 2);
}
```

Add `use serde_json::Value;` to the test file's imports if it is not there.

- [ ] **Step 2: Run them to verify they fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test dictionary identical_entries removing_a_canonical`
Expected: both fail — the first gets 200 and version 2, the second gets 200.

- [ ] **Step 3: Implement the two refusals**

In `put_dictionary`, after the `FOR UPDATE` lock and before `SELECT COALESCE(MAX(version)…`, insert:

```rust
    // Spec 2026-09-28 §4.2, same line as `thresholds::post_thresholds`: an
    // unchanged body mints nothing. jsonb equality after alias trimming —
    // numeric-free here, key-order free, array-order sensitive.
    let same_as_current: Option<(bool,)> = sqlx::query_as(
        "SELECT entries = $2 FROM project_filter_dictionary WHERE project_id = $1 \
         ORDER BY version DESC LIMIT 1",
    )
    .bind(id)
    .bind(serde_json::to_value(&entries).expect("json"))
    .fetch_optional(&mut *tx)
    .await?;
    if matches!(same_as_current, Some((true,))) {
        tracing::info!(project_id = %id, "dictionary unchanged; no version minted");
        return Err(ApiError::conflict("entries are identical to the current version"));
    }

    // §4.2: a canonical that published frames carry (any state) cannot
    // disappear — the coverage view and every stacking group key on it.
    // Case-sensitive, like `filter_canonical` itself: a case-only rename is
    // a removal plus an addition.
    let current = latest_entries(&mut tx, id).await?;
    let kept: std::collections::HashSet<&str> = entries.iter().map(|e| e.canonical.as_str()).collect();
    for gone in current.iter().filter(|e| !kept.contains(e.canonical.as_str())) {
        let (in_use,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM project_frames WHERE project_id = $1 AND filter_canonical = $2",
        )
        .bind(id)
        .bind(&gone.canonical)
        .fetch_one(&mut *tx)
        .await?;
        if in_use > 0 {
            tracing::warn!(project_id = %id, canonical = %gone.canonical, count = in_use, "dictionary PUT refused: canonical in use");
            return Err(ApiError::conflict(format!(
                "canonical {:?} is used by {in_use} frame{} — remap them first",
                gone.canonical,
                if in_use == 1 { "" } else { "s" }
            )));
        }
    }
```

`latest_entries` takes `&mut PgConnection`; pass `&mut *tx` if the compiler asks. Update the route's doc comment and the README line 114 (`PUT … publish a new dictionary version …`) to mention both refusals: append ` — 409 when the body equals the current version or drops a canonical that published frames still carry`.

- [ ] **Step 4: Run the whole dictionary test file, then frames**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test dictionary --test frames`
Expected: all pass. `new_project_has_the_default_dictionary_and_announce_respects_it` still mints v2 because it ADDS `Hb`.

- [ ] **Step 5: Commit**

```bash
git add src/routes/dictionary.rs tests/dictionary.rs README.md
git commit -m "fix(hub): PUT /dictionary refuses an unchanged body and the removal of a canonical published frames carry (§4.2)"
```

---

### Task 4: Portal types and the Admin fetch of the dictionary

**Files:**
- Modify: `portal/src/types.ts:101-108` (`CoverageFilter.kind`), and add `DictionaryEntry`/`DictionaryView` after `ThresholdView`
- Modify: `portal/src/pages/Admin.tsx:70-95` (state + fetch), `portal/src/pages/Admin.tsx:189-196` (mount, Task 5 fills the component)
- Test: `portal/src/pages/Admin.test.tsx`

**Interfaces:**
- Produces: `DictionaryEntry { canonical: string; aliases: string[]; kind: DictionaryKind }`, `DictionaryKind = 'broadband' | 'narrowband' | 'luminance' | 'unfiltered'`, `DictionaryView { version: number; entries: DictionaryEntry[]; createdAt: string }`; Admin state `dictEntries`, `dictVersion`, `dictHistory`; a `<DictionaryEditor>` mount point (Task 5 provides the component; this task mounts a placeholder heading so the capability tests pass first).

- [ ] **Step 1: Write the failing Admin tests**

In `portal/src/pages/Admin.test.tsx`, extend the `apiGet` mock with

```ts
  if (url === '/api/v1/projects/p1/dictionary') return { current: null, history: [] } as { current: DictionaryView | null; history: DictionaryView[] };
```

(import `DictionaryView` from `../types`), then add two cases next to the thresholds ones:

```ts
  it('asks for the dictionary only when thresholds.edit is held, and shows the editor', async () => {
    membership = membershipWith({ coordinator: false, govCaps: ['thresholds.edit'] });
    renderAdmin();
    await waitFor(() => expect(screen.getByText(/Filter dictionary/)).toBeInTheDocument());
    expect(apiGet).toHaveBeenCalledWith('/api/v1/projects/p1/dictionary');
  });

  it('never asks a members.manage delegate for the dictionary', async () => {
    membership = membershipWith({ coordinator: false, govCaps: ['members.manage'] });
    renderAdmin();
    await waitFor(() => expect(screen.getByText(/Join requests/)).toBeInTheDocument());
    expect(screen.queryByText(/Filter dictionary/)).toBeNull();
    expect(apiGet).not.toHaveBeenCalledWith('/api/v1/projects/p1/dictionary');
  });
```

(If the queue heading in the existing tests is not literally "Join requests", use whatever text the existing `members.manage` test waits for.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cd portal && npx vitest run src/pages/Admin.test.tsx`
Expected: the first fails — no "Filter dictionary" text; the second passes vacuously (keep it, it pins the negative).

- [ ] **Step 3: Types**

In `portal/src/types.ts` change `CoverageFilter.kind` to `DictionaryKind` and add after `ThresholdView`:

```ts
export type DictionaryKind = 'broadband' | 'narrowband' | 'luminance' | 'unfiltered';

/** One canonical filter name of a project's dictionary, as the hub stores it
 * (`routes/dictionary.rs::DictEntry`). Aliases are matched case-insensitively
 * by the app; `canonical` is exact and case-sensitive on the wire. */
export interface DictionaryEntry {
  canonical: string;
  aliases: string[];
  kind: DictionaryKind;
}

export interface DictionaryView {
  version: number;
  entries: DictionaryEntry[];
  createdAt: string;
}
```

- [ ] **Step 4: Admin state, fetch and mount**

In `Admin.tsx` add state after the threshold state:

```ts
  const [dictEntries, setDictEntries] = useState<DictionaryEntry[]>([]);
  const [dictVersion, setDictVersion] = useState<number | null>(null);
  const [dictHistory, setDictHistory] = useState<DictionaryView[]>([]);
```

In `reload`, inside the existing `if (m != null && canEditThresholds(m)) { … }` block after the thresholds fetch:

```ts
      const d = await apiGet<{ current: DictionaryView | null; history: DictionaryView[] }>(`/api/v1/projects/${p.project.id}/dictionary`);
      setDictEntries(d.current?.entries ?? []);
      setDictVersion(d.current?.version ?? null);
      setDictHistory(d.history ?? []);
```

Right after the `<ThresholdEditor … />` mount add:

```tsx
      {canEditThresholds(membership) && (
        <DictionaryEditor
          key={`dict-${dictVersion ?? 'none'}`}
          entries={dictEntries}
          version={dictVersion}
          history={dictHistory}
          onSave={(next) => void act(() => apiPut(`/api/v1/projects/${projectId}/dictionary`, { entries: next }), 'Dictionary saved.')}
        />
      )}
```

Import `DictionaryEntry`, `DictionaryView` from `../types` and `DictionaryEditor` from `../components/DictionaryEditor`. Create a minimal `portal/src/components/DictionaryEditor.tsx` for this task so the page compiles:

```tsx
import type { DictionaryEntry, DictionaryView } from '../types';

export function DictionaryEditor({
  version,
}: {
  entries: DictionaryEntry[];
  version: number | null;
  history: DictionaryView[];
  onSave: (entries: DictionaryEntry[]) => void;
}) {
  return (
    <section className="flex flex-col gap-2">
      <h2 className="text-[1.25rem]">
        Filter dictionary {version != null && <span className="text-[0.86rem] font-normal text-content-faint">(v{version})</span>}
      </h2>
    </section>
  );
}
```

- [ ] **Step 5: Run the Admin tests and the type check**

Run: `cd portal && npx vitest run src/pages/Admin.test.tsx && npx tsc --noEmit`
Expected: all Admin cases pass; no type errors (the `CoverageFilter.kind` widening compiles everywhere it is read).

- [ ] **Step 6: Commit**

```bash
git add portal/src/types.ts portal/src/pages/Admin.tsx portal/src/pages/Admin.test.tsx portal/src/components/DictionaryEditor.tsx
git commit -m "feat(portal): dictionary types, the Admin fetch behind thresholds.edit and the editor mount"
```

---

### Task 5: `DictionaryEditor` — rows, inline validation, Save gating, history

**Files:**
- Modify: `portal/src/components/DictionaryEditor.tsx` (replace the placeholder)
- Create: `portal/src/components/DictionaryEditor.test.tsx`

**Interfaces:**
- Consumes: `DictionaryEntry`, `DictionaryKind`, `DictionaryView` (Task 4); `Button`, `EmptyState`, `Field`, `Select`, `Note` from `../ui`.
- Produces: `onSave(entries)` with trimmed aliases, empties dropped, only when the draft differs from `entries` and has no error.

- [ ] **Step 1: Write the failing tests**

Create `portal/src/components/DictionaryEditor.test.tsx`:

```tsx
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DictionaryEditor } from './DictionaryEditor';
import type { DictionaryEntry, DictionaryView } from '../types';

afterEach(cleanup);

const ENTRIES: DictionaryEntry[] = [
  { canonical: 'L', aliases: ['lum', 'luminance'], kind: 'luminance' },
  { canonical: 'Ha', aliases: ['h-alpha'], kind: 'narrowband' },
];
const HISTORY: DictionaryView[] = [
  { version: 1, createdAt: '2026-09-01T00:00:00Z', entries: [{ canonical: 'L', aliases: [], kind: 'luminance' }] },
];

function renderEditor(entries: DictionaryEntry[] = ENTRIES) {
  const onSave = vi.fn();
  render(<DictionaryEditor entries={entries} version={2} history={HISTORY} onSave={onSave} />);
  return onSave;
}

describe('DictionaryEditor', () => {
  it('renders one row per entry: canonical, kind select, comma-joined aliases', () => {
    renderEditor();
    const canonicals = screen.getAllByRole('textbox', { name: 'Canonical' });
    expect(canonicals).toHaveLength(2);
    expect(canonicals[0]).toHaveValue('L');
    expect(screen.getAllByRole('combobox', { name: 'Kind' })[1]).toHaveValue('narrowband');
    expect(screen.getAllByRole('textbox', { name: 'Aliases' })[0]).toHaveValue('lum, luminance');
    expect(screen.getByText('v1 · 2026-09-01 · L')).toBeInTheDocument();
  });

  it('disables Save while the draft equals the stored entries, and enables it on a real change', async () => {
    const user = userEvent.setup();
    const onSave = renderEditor();
    const save = screen.getByRole('button', { name: 'Save as new version' });
    expect(save).toBeDisabled();
    expect(screen.getByText('No changes to save.')).toBeInTheDocument();
    // Whitespace in the alias box is not a change.
    const aliases = screen.getAllByRole('textbox', { name: 'Aliases' })[0];
    await user.clear(aliases);
    await user.type(aliases, ' lum , luminance ');
    expect(save).toBeDisabled();
    await user.type(aliases, ', clear');
    expect(save).not.toBeDisabled();
    await user.click(save);
    expect(onSave).toHaveBeenCalledWith([
      { canonical: 'L', aliases: ['lum', 'luminance', 'clear'], kind: 'luminance' },
      { canonical: 'Ha', aliases: ['h-alpha'], kind: 'narrowband' },
    ]);
  });

  it('adds and removes entries, and a new row with an empty canonical blocks Save', async () => {
    const user = userEvent.setup();
    const onSave = renderEditor();
    await user.click(screen.getByRole('button', { name: 'Add entry' }));
    expect(screen.getAllByRole('textbox', { name: 'Canonical' })).toHaveLength(3);
    expect(screen.getByRole('button', { name: 'Save as new version' })).toBeDisabled();
    expect(screen.getByText('Canonical must be 1–16 letters, digits, _ or -.')).toBeInTheDocument();
    await user.type(screen.getAllByRole('textbox', { name: 'Canonical' })[2], 'None');
    await user.selectOptions(screen.getAllByRole('combobox', { name: 'Kind' })[2], 'unfiltered');
    expect(screen.getByRole('button', { name: 'Save as new version' })).not.toBeDisabled();
    await user.click(screen.getAllByRole('button', { name: 'Remove entry' })[1]);
    await user.click(screen.getByRole('button', { name: 'Save as new version' }));
    expect(onSave).toHaveBeenCalledWith([
      { canonical: 'L', aliases: ['lum', 'luminance'], kind: 'luminance' },
      { canonical: 'None', aliases: [], kind: 'unfiltered' },
    ]);
  });

  it('flags a duplicate canonical (case-insensitive) and a duplicate alias across entries', async () => {
    const user = userEvent.setup();
    renderEditor();
    await user.clear(screen.getAllByRole('textbox', { name: 'Canonical' })[1]);
    await user.type(screen.getAllByRole('textbox', { name: 'Canonical' })[1], 'l');
    expect(screen.getByText('This canonical is already in the dictionary.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save as new version' })).toBeDisabled();
    await user.clear(screen.getAllByRole('textbox', { name: 'Canonical' })[1]);
    await user.type(screen.getAllByRole('textbox', { name: 'Canonical' })[1], 'Ha');
    await user.type(screen.getAllByRole('textbox', { name: 'Aliases' })[1], ', LUM');
    expect(screen.getByText('Alias "LUM" is already used by L.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save as new version' })).toBeDisabled();
  });

  it('refuses an empty dictionary', async () => {
    const user = userEvent.setup();
    renderEditor([{ canonical: 'L', aliases: [], kind: 'luminance' }]);
    await user.click(screen.getByRole('button', { name: 'Remove entry' }));
    expect(screen.getByText('A project needs at least one filter.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save as new version' })).toBeDisabled();
  });
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd portal && npx vitest run src/components/DictionaryEditor.test.tsx`
Expected: every case fails against the placeholder (no textboxes).

- [ ] **Step 3: Implement the editor**

Replace `portal/src/components/DictionaryEditor.tsx`:

```tsx
import { useMemo, useState } from 'react';
import type { DictionaryEntry, DictionaryKind, DictionaryView } from '../types';
import { Button, EmptyState, Field, Select } from '../ui';

/** The hub's own bounds (`routes/dictionary.rs::validate_dictionary`),
 * mirrored so the form refuses inline what the route would refuse with a
 * sentence the user cannot act on. */
const CANONICAL_RE = /^[A-Za-z0-9][A-Za-z0-9_-]{0,15}$/;
const MAX_ENTRIES = 50;
const MAX_ALIASES = 20;
const MAX_ALIAS_LEN = 40;

export const KIND_LABEL: Record<DictionaryKind, string> = {
  broadband: 'Broadband',
  narrowband: 'Narrowband',
  luminance: 'Luminance',
  unfiltered: 'Unfiltered',
};
const KINDS = Object.keys(KIND_LABEL) as DictionaryKind[];

interface DraftEntry {
  canonical: string;
  kind: DictionaryKind;
  /** What the user typed — split on commas, trimmed, empties dropped, on save and on compare. */
  aliasesRaw: string;
}

const splitAliases = (raw: string): string[] => raw.split(',').map((a) => a.trim()).filter((a) => a.length > 0);
const toDraft = (e: DictionaryEntry): DraftEntry => ({ canonical: e.canonical, kind: e.kind, aliasesRaw: e.aliases.join(', ') });
const fromDraft = (d: DraftEntry): DictionaryEntry => ({ canonical: d.canonical.trim(), aliases: splitAliases(d.aliasesRaw), kind: d.kind });
const formatEntry = (e: DictionaryEntry) => (e.aliases.length ? `${e.canonical} (${e.aliases.join(', ')})` : e.canonical);

export function DictionaryEditor({
  entries,
  version,
  history,
  onSave,
}: {
  entries: DictionaryEntry[];
  version: number | null;
  history: DictionaryView[];
  onSave: (entries: DictionaryEntry[]) => void;
}) {
  // Seeded once — Admin re-mounts this component by `key` on the stored
  // version, so a reload that changes nothing here never wipes an edit.
  const [draft, setDraft] = useState<DraftEntry[]>(entries.map(toDraft));

  const errors = useMemo(() => {
    const seenCanon = new Map<string, number>();
    const aliasOwner = new Map<string, string>();
    return draft.map((d, i) => {
      const canonical = d.canonical.trim();
      const errs: string[] = [];
      if (!CANONICAL_RE.test(canonical)) errs.push('Canonical must be 1–16 letters, digits, _ or -.');
      const key = canonical.toLowerCase();
      if (key && seenCanon.has(key)) errs.push('This canonical is already in the dictionary.');
      if (key) seenCanon.set(key, i);
      const aliases = splitAliases(d.aliasesRaw);
      if (aliases.length > MAX_ALIASES) errs.push(`At most ${MAX_ALIASES} aliases.`);
      for (const a of aliases) {
        if (a.length > MAX_ALIAS_LEN) errs.push(`Alias "${a}" is longer than ${MAX_ALIAS_LEN} characters.`);
        const owner = aliasOwner.get(a.toLowerCase());
        if (owner !== undefined && owner !== canonical) errs.push(`Alias "${a}" is already used by ${owner}.`);
        else if (owner === canonical) errs.push(`Alias "${a}" is listed twice.`);
        else aliasOwner.set(a.toLowerCase(), canonical || '(this entry)');
      }
      return errs;
    });
  }, [draft]);

  const empty = draft.length === 0;
  const tooMany = draft.length > MAX_ENTRIES;
  const hasError = errors.some((e) => e.length > 0) || empty || tooMany;
  const unchanged = useMemo(
    () => JSON.stringify(entries.map((e) => [e.canonical, e.kind, e.aliases])) === JSON.stringify(draft.map(fromDraft).map((e) => [e.canonical, e.kind, e.aliases])),
    [draft, entries],
  );

  const update = (i: number, patch: Partial<DraftEntry>) => setDraft(draft.map((d, j) => (j === i ? { ...d, ...patch } : d)));

  const save = () => {
    if (hasError) {
      console.error('[portal] dictionary editor: cannot save with errors', errors);
      return;
    }
    onSave(draft.map(fromDraft));
  };

  return (
    <section className="flex flex-col gap-2">
      <h2 className="text-[1.25rem]">
        Filter dictionary {version != null && <span className="text-[0.86rem] font-normal text-content-faint">(v{version})</span>}
      </h2>
      <p className="text-[0.78rem] text-content-faint">
        The canonical filter names publishers map their frames to. Aliases are matched case-insensitively; a canonical is exact.
      </p>
      {empty && <EmptyState>No filters.</EmptyState>}
      {draft.map((d, i) => (
        <div key={i} className="flex flex-col gap-1">
          <div className="flex flex-wrap items-start gap-2">
            <Field value={d.canonical} aria-label="Canonical" onChange={(e) => update(i, { canonical: e.target.value })} className="w-32" />
            <Select value={d.kind} onChange={(e) => update(i, { kind: e.target.value as DictionaryKind })} aria-label="Kind" className="w-36">
              {KINDS.map((k) => (
                <option key={k} value={k}>
                  {KIND_LABEL[k]}
                </option>
              ))}
            </Select>
            <Field value={d.aliasesRaw} aria-label="Aliases" placeholder="aliases, comma-separated" onChange={(e) => update(i, { aliasesRaw: e.target.value })} className="w-72" />
            <Button variant="ghost" size="sm" aria-label="Remove entry" onClick={() => setDraft(draft.filter((_, j) => j !== i))}>
              Remove
            </Button>
          </div>
          {errors[i].map((err) => (
            <p key={err} className="text-[0.78rem] text-error-ink">
              {err}
            </p>
          ))}
        </div>
      ))}
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="ghost" size="sm" onClick={() => setDraft([...draft, { canonical: '', kind: 'broadband', aliasesRaw: '' }])} disabled={tooMany}>
          Add entry
        </Button>
        <Button size="sm" onClick={save} disabled={hasError || unchanged}>
          Save as new version
        </Button>
        {empty && <span className="text-[0.78rem] text-content-faint">A project needs at least one filter.</span>}
        {tooMany && <span className="text-[0.78rem] text-content-faint">At most {MAX_ENTRIES} filters.</span>}
        {!empty && !tooMany && unchanged && <span className="text-[0.78rem] text-content-faint">No changes to save.</span>}
      </div>
      <p className="text-[0.78rem] text-content-faint">
        A canonical that published frames carry cannot be removed — remap those frames first. Publishers see the new version at their next
        publish.
      </p>
      {history.filter((h) => h.version !== version).length > 0 && (
        <div className="flex flex-col gap-1">
          <h3 className="text-[0.86rem] font-medium">Previous versions</h3>
          {history
            .filter((h) => h.version !== version)
            .map((h) => (
              <p key={h.version} className="text-[0.78rem] text-content-faint">
                v{h.version} · {h.createdAt.slice(0, 10)} · {h.entries.map(formatEntry).join(', ')}
              </p>
            ))}
        </div>
      )}
    </section>
  );
}
```

If `Field` does not accept `placeholder`, drop that prop (check `portal/src/ui/Field.tsx`). If `Field` renders no `aria-label` passthrough, add it the way `ThresholdEditor`'s `Value` box gets its accessible name.

- [ ] **Step 4: Run the editor tests, the Admin tests, the whole portal suite**

Run: `cd portal && npx vitest run src/components/DictionaryEditor.test.tsx src/pages/Admin.test.tsx && npm test && npx tsc --noEmit`
Expected: all green (the suite was 216 + the new cases).

- [ ] **Step 5: Commit**

```bash
git add portal/src/components/DictionaryEditor.tsx portal/src/components/DictionaryEditor.test.tsx
git commit -m "feat(portal): Admin filter-dictionary editor — rows, inline hub-rule validation, Save only on a real change (§4.3)"
```

---

### Task 6: Full suites, README, ledger

**Files:**
- Modify: `README.md` (route table lines 113–114: mention `unfiltered` and the two 409s; the dictionary paragraph around line 609 if it lists the seven canonicals)
- Modify (app repo): `docs/superpowers/open-items.md` — a line under the contributor-path entry: hub branch `contributor-path-hub` done, suites green, test-hub deploy owed.

- [ ] **Step 1: Run the full hub suite**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test 2>&1 | tail -30`
Expected: every binary `test result: ok`; exit 0. This is the gate before any merge (owner lesson 2026-09-18/20); do not run it beside a core `cargo test` of the app (memory: one Rust compile at a time on this Mac).

- [ ] **Step 2: Run the full portal suite**

Run: `cd portal && npm test && npx tsc --noEmit && npm run build`
Expected: green; the build succeeds.

- [ ] **Step 3: README**

Update the two route lines:

```
GET    /api/v1/projects/{id}/dictionary   (member)  the project's canonical filter vocabulary — {current, history}, versioned like thresholds; kinds broadband · narrowband · luminance · unfiltered; every project carries `None` (unfiltered) since 0026
PUT    /api/v1/projects/{id}/dictionary   (cap:thresholds.edit) publish a new dictionary version {entries:[{canonical,aliases,kind}]} — 409 when the body equals the current version or drops a canonical published frames carry; announce and remap refuse any filterCanonical not in it
```

- [ ] **Step 4: Commit and record**

```bash
git add README.md
git commit -m "docs(hub): README — dictionary kinds, the None entry and the two PUT refusals"
```

Then in the app repo append to the ledger entry "Collab v3 wave 3 — owner smoke (2026-09-28)" under the spec line: `Hub+portal plan DONE on branch \`contributor-path-hub\` (<sha>): full hub suite + portal suite green; OWED: merge on the owner's word, test-hub deploy, the portal click-through of acceptance step 1.` Commit that in the app repo.

Stop here: merging to hub `main`, pushing and deploying happen only on the owner's word (memory: deploy discipline).

---

### Task 7: Announce accepts an empty `filterRaw` (executed right after Task 1)

Added 2026-09-28 during execution (controller ruling, ledger). Task 1's
implementer found that `routes/frames.rs::validate` refuses a blank
`filterRaw` (`1..=80 chars`), so a frame without a `FILTER` header could
never be announced even once mapped to `None`. The app's own mirror of that
rule (`api::collab::hub_frame_rule_problem`) and its fake hub have the same
check — the app plan carries the mirror change (its Task 6).

**Files:**
- Modify: `src/routes/frames.rs:179-184` (the `filterRaw` rule)
- Modify: `tests/dictionary.rs` (`new_project_has_none_and_announce_accepts_it` announces `filterRaw: ""` again, as the plan's Task 1 wrote it)
- Modify: `README.md` (the announce rule line, if it states `1..=80`)
- Test: `tests/frames.rs`

**Interfaces:**
- Produces: `filterRaw` valid when `0..=80` chars after trimming; stored trimmed (`""` for a header-less frame). The refusal sentence becomes `filterRaw must be at most 80 chars`.

- [ ] **Step 1: Write the failing tests**

In `tests/frames.rs` add, next to the existing validation test:

```rust
/// Contributor path (spec 2026-09-28 F1): a frame without a FILTER header
/// announces with an empty `filterRaw` and a canonical the publisher mapped.
#[sqlx::test]
async fn announce_accepts_an_empty_filter_raw(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let mut f = frame_body(1);
    f["filterRaw"] = json!("   ");
    f["filterCanonical"] = json!("None");
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [f]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let (stored,): (String,) = sqlx::query_as("SELECT filter_raw FROM project_frames WHERE project_id = $1::uuid AND frame_uuid = $2::uuid")
        .bind(id).bind(frame_uuid(1)).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, "", "stored trimmed");
    // Too long is still refused.
    let mut g = frame_body(2);
    g["filterRaw"] = json!("x".repeat(81));
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames"), &json!({"frames": [g]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("at most 80"));
}
```

If `frame_uuid` is a `String` and the column is `uuid`, bind `Uuid::parse_str(&frame_uuid(1)).unwrap()` instead of the cast; match how `tests/frames.rs` already reads `project_frames`.

In `tests/dictionary.rs::new_project_has_none_and_announce_accepts_it` change `f["filterRaw"] = json!("None");` back to `f["filterRaw"] = json!("");` and delete the NOTE comment Task 1 left.

- [ ] **Step 2: Run to verify they fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test frames announce_accepts_an_empty_filter_raw --test dictionary new_project_has_none`
Expected: both 400 `filterRaw must be 1..=80 chars`.

- [ ] **Step 3: Implement**

In `src/routes/frames.rs` replace the rule:

```rust
    // Contributor path (spec 2026-09-28 F1): a frame without a FILTER header
    // announces with an empty filterRaw; the canonical carries the meaning.
    if f.filter_raw.trim().len() > 80 {
        return Err(ApiError::bad_request(format!(
            "{}: filterRaw must be at most 80 chars",
            f.file_name
        )));
    }
```

The INSERT already binds `f.filter_raw.trim()`. Update the README's announce rule text if it mentions `1..=80` for `filterRaw`.

- [ ] **Step 4: Run the tests**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test frames --test dictionary`
Expected: all pass; any existing test that asserted the old refusal on an EMPTY filterRaw is updated to assert the new behaviour (an empty one is accepted) — keep the over-length assertion.

- [ ] **Step 5: Commit**

```bash
git add src/routes/frames.rs tests/frames.rs tests/dictionary.rs README.md
git commit -m "fix(hub): announce accepts an empty filterRaw — a header-less frame announces under its mapped canonical (F1)"
```
