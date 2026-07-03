# Phase 1 Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Catalog identity (catalog_meta + per-row uuid/updated_at via triggers), generated TS types with a drift-failing test, a shared core command layer piloted on 4 modules, and a FITS-4.0-compliant writer with a typed keyword vocabulary.

**Architecture:** All new logic lands in `athenaeum-core`: schema work extends `db/schema.rs::init_db()` using the existing guarded-ALTER pattern; ts-rs generation is a registry-driven harness + `ts_contract` diff test; the command layer is plain handler fns in `core::api::*` wrapped by 3–5-line Tauri/Axum shims; the FITS writer is a self-contained `fits_writer` module (card grammar → serializer → vocabulary) round-tripped through the two existing readers.

**Tech Stack:** Rust 2021 (rusqlite, uuid v4, chrono, ts-rs 10), Tauri v2 + Axum, React/TS 5.8 (tsc gate). Spec: `docs/superpowers/specs/2026-07-04-phase1-foundation-design.md`.

## Global Constraints

- All work on branch **`0.2.4`** (create from `main`); `main` stays releasable. Straight to stable — no beta tag.
- Gates for every task: `cargo build --workspace` + `cargo test --workspace` + `npx tsc --noEmit` (frontend tasks).
- **No renames of existing serde fields, no serde attribute changes.** New fields are additive only (`uuid`, `updated_at`).
- Migration mechanism stays guarded-ALTER via `pragma_table_info` — do NOT introduce `PRAGMA user_version`.
- Logging contract: every Tauri command keeps `#[tracing::instrument(skip_all, err)]`, every web route keeps `#[tracing::instrument(skip_all, err(Debug))]`; no `#[instrument]` on core `api::` handlers (no double spans).
- FITS keywords ≤8 chars `[A-Z0-9-_]`; custom namespace is **`ATH_`** (not `ATHM_`).
- Commit style: conventional commits (`feat(db): …`), no AI attribution trailers.
- The 7 identity tables: `files`, `frames`, `frames_set`, `sessions`, `calibration_set`, `tags`, `export_templates`.

---

### Task 0: Branch setup

**Files:** none (git only)

- [ ] **Step 1: Create the version branch**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
git checkout main && git pull
git checkout -b 0.2.4
```

- [ ] **Step 2: Verify clean baseline**

Run: `cargo build --workspace && cargo test --workspace --quiet`
Expected: green (same as main).

---

### Task 1: `catalog_meta` table + accessor

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (inside `init_db`, insert new block just before the `#[cfg(test)]` module boundary at ~line 1244, i.e. at the end of the function body)
- Modify: `crates/athenaeum-core/src/models.rs` (append struct)
- Modify: `crates/athenaeum-core/src/db/operations.rs` (append accessor)
- Test: `crates/athenaeum-core/src/db/schema.rs` (new `#[cfg(test)] mod identity_schema_tests`)

**Interfaces:**
- Produces: `models::CatalogMeta { catalog_uuid: String, schema_version: i64, created_at: String }`; `db::operations::get_catalog_meta(conn: &Connection) -> rusqlite::Result<CatalogMeta>`; table `catalog_meta` seeded exactly once.

- [ ] **Step 1: Write the failing test** (new module at the bottom of `schema.rs`, alongside `archive_schema_tests`)

```rust
#[cfg(test)]
mod identity_schema_tests {
    use super::*;
    use rusqlite::Connection;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn catalog_meta_seeded_once_and_stable() {
        let conn = mem_db();
        let (uuid1, ver): (String, i64) = conn
            .query_row("SELECT catalog_uuid, schema_version FROM catalog_meta WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(ver, 1);
        assert_eq!(uuid1.len(), 36, "catalog_uuid must be a hyphenated UUID");
        // re-running init_db must NOT regenerate the uuid
        init_db(&conn).unwrap();
        let uuid2: String = conn
            .query_row("SELECT catalog_uuid FROM catalog_meta WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uuid1, uuid2);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM catalog_meta", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core identity_schema_tests -- --nocapture`
Expected: FAIL — `no such table: catalog_meta`.

- [ ] **Step 3: Implement in `init_db`** (append at end of function body, before final `Ok(())`)

```rust
    // ---- Collaboration Stage 1: catalog identity (Phase 1) ----
    conn.execute(
        "CREATE TABLE IF NOT EXISTS catalog_meta (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            catalog_uuid TEXT NOT NULL,
            schema_version INTEGER NOT NULL,
            created_at TEXT NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO catalog_meta (id, catalog_uuid, schema_version, created_at)
         VALUES (1, ?1, 1, ?2)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
```

If `uuid`/`chrono` are not already `use`d in schema.rs, reference them fully-qualified as shown (both are existing athenaeum-core dependencies; `Cargo.toml:20` has `uuid = { version = "1", features = ["v4"] }`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p athenaeum-core identity_schema_tests`
Expected: PASS.

- [ ] **Step 5: Add the model + accessor**

Append to `crates/athenaeum-core/src/models.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogMeta {
    pub catalog_uuid: String,
    pub schema_version: i64,
    pub created_at: String,
}
```

Append to `crates/athenaeum-core/src/db/operations.rs` (above the `#[cfg(test)]` modules that start at ~line 3600):

```rust
pub fn get_catalog_meta(conn: &Connection) -> rusqlite::Result<crate::models::CatalogMeta> {
    conn.query_row(
        "SELECT catalog_uuid, schema_version, created_at FROM catalog_meta WHERE id = 1",
        [],
        |r| {
            Ok(crate::models::CatalogMeta {
                catalog_uuid: r.get(0)?,
                schema_version: r.get(1)?,
                created_at: r.get(2)?,
            })
        },
    )
}
```

- [ ] **Step 6: Full gate + commit**

```bash
cargo build --workspace && cargo test -p athenaeum-core
git add crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/models.rs crates/athenaeum-core/src/db/operations.rs
git commit -m "feat(db): catalog_meta table with catalog_uuid identity (collab Stage 1)"
```

---

### Task 2: `uuid`/`updated_at` columns + backfill + unique indexes

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (same identity block from Task 1)
- Test: extend `identity_schema_tests`

**Interfaces:**
- Produces: const `UUID_TABLES: [&str; 7]`; fn `column_exists(conn, table, col) -> rusqlite::Result<bool>`; fn `backfill_identity(conn) -> rusqlite::Result<()>`; columns `uuid TEXT` + `updated_at TEXT` and unique index `idx_<table>_uuid` on all 7 tables. Tasks 3–4 rely on these exact names.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn identity_columns_and_indexes_exist_on_all_seven_tables() {
        let conn = mem_db();
        for t in super::UUID_TABLES {
            for col in ["uuid", "updated_at"] {
                let n: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                        rusqlite::params![t, col],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(n, 1, "{t}.{col} missing");
            }
            let idx: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = ?1",
                    rusqlite::params![format!("idx_{t}_uuid")],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(idx, 1, "unique index on {t}.uuid missing");
        }
    }

    #[test]
    fn legacy_rows_get_backfilled() {
        // Simulate a legacy catalog: rows inserted while triggers/columns are absent is
        // impossible after init_db, so emulate by clearing uuid on an inserted row and
        // re-running init_db (backfill must repair NULL uuids idempotently).
        let conn = mem_db();
        conn.execute(
            "INSERT INTO tags (name, color) VALUES ('legacy', NULL)", [],
        ).unwrap();
        conn.execute("UPDATE tags SET uuid = NULL, updated_at = NULL", []).unwrap();
        init_db(&conn).unwrap();
        let (u, ts): (Option<String>, Option<String>) = conn
            .query_row("SELECT uuid, updated_at FROM tags WHERE name='legacy'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let u = u.expect("uuid backfilled");
        assert_eq!(u.len(), 36);
        assert!(ts.is_some(), "updated_at backfilled");
    }
```

Note: `UPDATE tags SET uuid = NULL` will be bounced back by the Task-3 `tags_touch` trigger only for `updated_at`; uuid stays NULL until backfill — exactly what we're testing. When Task 3 lands, the `tags_identity` trigger only fires on INSERT, so this test stays valid.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core identity_schema_tests`
Expected: FAIL — `UUID_TABLES` not found / columns missing.

- [ ] **Step 3: Implement** (extend the identity block in `init_db`; helpers go above `init_db` in the same file)

```rust
pub const UUID_TABLES: [&str; 7] = [
    "files", "frames", "frames_set", "sessions",
    "calibration_set", "tags", "export_templates",
];

fn column_exists(conn: &Connection, table: &str, col: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        rusqlite::params![table, col],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn backfill_identity(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for t in UUID_TABLES {
        let ids: Vec<i64> = {
            let mut st = tx.prepare(&format!("SELECT id FROM {t} WHERE uuid IS NULL"))?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        if !ids.is_empty() {
            let mut up = tx.prepare(&format!("UPDATE {t} SET uuid = ?1 WHERE id = ?2"))?;
            for id in &ids {
                up.execute(rusqlite::params![uuid::Uuid::new_v4().to_string(), id])?;
            }
        }
        // best-available timestamp source per table; NULL-safe
        let src = match t {
            "files" | "sessions" => "COALESCE(created_at, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            _ => "strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        };
        tx.execute(
            &format!("UPDATE {t} SET updated_at = {src} WHERE updated_at IS NULL"),
            [],
        )?;
    }
    tx.commit()
}
```

In the identity block of `init_db` (after the `catalog_meta` seed):

```rust
    for t in UUID_TABLES {
        if !column_exists(conn, t, "uuid")? {
            conn.execute(&format!("ALTER TABLE {t} ADD COLUMN uuid TEXT"), [])?;
        }
        if !column_exists(conn, t, "updated_at")? {
            conn.execute(&format!("ALTER TABLE {t} ADD COLUMN updated_at TEXT"), [])?;
        }
    }
    backfill_identity(conn)?;
    for t in UUID_TABLES {
        conn.execute(
            &format!("CREATE UNIQUE INDEX IF NOT EXISTS idx_{t}_uuid ON {t}(uuid)"),
            [],
        )?;
    }
```

Do NOT touch the original `CREATE TABLE` statements — fresh and legacy DBs take the identical ALTER path (single code path, per spec §1).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core identity_schema_tests`
Expected: PASS (2 new tests + Task 1 test).

- [ ] **Step 5: Full gate + commit**

```bash
cargo build --workspace && cargo test --workspace --quiet
git add crates/athenaeum-core/src/db/schema.rs
git commit -m "feat(db): uuid/updated_at columns, backfill and unique indexes on the 7 entity tables"
```

---

### Task 3: identity + touch triggers

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (identity block)
- Test: extend `identity_schema_tests`

**Interfaces:**
- Produces: triggers `<table>_identity` (AFTER INSERT) and `<table>_touch` (AFTER UPDATE) on all 7 tables. INSERT without uuid ⇒ v4 uuid + updated_at appear; UPDATE ⇒ updated_at bumps unless explicitly changed in the same statement.

- [ ] **Step 1: Write the failing tests**

```rust
    fn assert_v4_shape(u: &str) {
        assert_eq!(u.len(), 36);
        let b: Vec<char> = u.chars().collect();
        for i in [8, 13, 18, 23] { assert_eq!(b[i], '-', "dash at {i} in {u}"); }
        assert_eq!(b[14], '4', "version nibble in {u}");
        assert!("89ab".contains(b[19]), "variant nibble in {u}");
    }

    #[test]
    fn insert_trigger_fills_uuid_and_updated_at() {
        let conn = mem_db();
        conn.execute("INSERT INTO tags (name, color) VALUES ('t1', NULL)", []).unwrap();
        let (u, ts): (String, String) = conn
            .query_row("SELECT uuid, updated_at FROM tags WHERE name='t1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_v4_shape(&u);
        assert!(ts.ends_with('Z') && ts.contains('T'));
    }

    #[test]
    fn update_trigger_bumps_updated_at_but_respects_explicit_set() {
        let conn = mem_db();
        conn.execute("INSERT INTO tags (name, color) VALUES ('t2', NULL)", []).unwrap();
        let ts0: String = conn.query_row("SELECT updated_at FROM tags WHERE name='t2'", [], |r| r.get(0)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        conn.execute("UPDATE tags SET color = 'red' WHERE name='t2'", []).unwrap();
        let ts1: String = conn.query_row("SELECT updated_at FROM tags WHERE name='t2'", [], |r| r.get(0)).unwrap();
        assert_ne!(ts0, ts1, "touch trigger must bump updated_at");
        // explicit set wins (future sync import path)
        conn.execute("UPDATE tags SET color='blue', updated_at='2020-01-01T00:00:00.000Z' WHERE name='t2'", []).unwrap();
        let ts2: String = conn.query_row("SELECT updated_at FROM tags WHERE name='t2'", [], |r| r.get(0)).unwrap();
        assert_eq!(ts2, "2020-01-01T00:00:00.000Z");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core identity_schema_tests`
Expected: FAIL — uuid NULL after plain INSERT.

- [ ] **Step 3: Implement** (in the identity block, after index creation)

```rust
    const UUID_V4_SQL: &str = "lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || \
        substr(hex(randomblob(2)),2) || '-' || substr('89ab', abs(random()) % 4 + 1, 1) || \
        substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))";
    for t in UUID_TABLES {
        conn.execute(
            &format!(
                "CREATE TRIGGER IF NOT EXISTS {t}_identity AFTER INSERT ON {t}
                 FOR EACH ROW WHEN NEW.uuid IS NULL
                 BEGIN
                     UPDATE {t} SET uuid = {UUID_V4_SQL},
                         updated_at = COALESCE(NEW.updated_at, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     WHERE id = NEW.id;
                 END"
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "CREATE TRIGGER IF NOT EXISTS {t}_touch AFTER UPDATE ON {t}
                 FOR EACH ROW WHEN NEW.updated_at IS OLD.updated_at
                 BEGIN
                     UPDATE {t} SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE id = NEW.id;
                 END"
            ),
            [],
        )?;
    }
```

`IS` (not `=`) in the touch guard is NULL-safe. SQLite `recursive_triggers` is OFF by default, so the self-UPDATE inside each trigger does not re-fire it. IMPORTANT ordering: triggers are created AFTER `backfill_identity` in the block so the touch trigger cannot interfere with backfill's own UPDATEs (it would only bump `updated_at` on rows whose uuid is being set — harmless, but the explicit order keeps semantics obvious).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core identity_schema_tests`
Expected: PASS (all identity tests).

- [ ] **Step 5: Full workspace gate + commit**

```bash
cargo build --workspace && cargo test --workspace --quiet
git add crates/athenaeum-core/src/db/schema.rs
git commit -m "feat(db): AFTER INSERT identity and AFTER UPDATE touch triggers for uuid/updated_at"
```

---

### Task 4: expose `uuid`/`updated_at` on the Rust row models

**Files:**
- Modify: `crates/athenaeum-core/src/models.rs` — `File` (line 6), `Frame` (line 55), `FramesSet` (line 234), `Session` (line 273)
- Modify: whatever structs map the `calibration_set`, `tags`, `export_templates` rows (discovery in Step 1)
- Modify: `crates/athenaeum-core/src/db/operations.rs` (+ any other row-mapper site the compiler flags)

**Interfaces:**
- Produces: `pub uuid: Option<String>` + `pub updated_at: Option<String>` on the row structs for all 7 entities (`Option` because structs are also constructed in non-DB contexts, e.g. scanner pre-insert; DB-mapped instances always carry values post-Task 2/3).

- [ ] **Step 1: Discover the three non-obvious row structs**

```bash
grep -rn "pub struct Tag\b\|pub struct ExportTemplate\|struct CalibrationSet \b" crates/ --include="*.rs"
grep -rn "FROM tags\|FROM export_templates" crates/athenaeum-core/src/db/*.rs | head -20
```

Expected: locate the structs used by the tag/template/calibration-set SELECT mappers (they may live in `db/operations.rs` or `models.rs` under different names, e.g. `CalibrationSetWithFrameCount` at models.rs:609 is the calibration list model). Record file:line for each; if an entity genuinely has no Rust row struct (mapped ad-hoc to tuples/json), note it and skip — do NOT invent new API surface (YAGNI); the DB columns still exist for later phases.

- [ ] **Step 2: Add the fields**

For each row struct found (shown here for `File`; repeat identically for `Frame`, `FramesSet`, `Session`, and the discovered three):

```rust
pub struct File {
    // ... existing fields unchanged ...
    pub uuid: Option<String>,
    pub updated_at: Option<String>,
}
```

- [ ] **Step 3: Follow the compiler to every construction site**

Run: `cargo build --workspace 2>&1 | grep -E "^error" | head -50`

For every `missing fields uuid, updated_at` error:
- **DB row mappers** (`Ok(File { ... })` inside `query_map`/`query_row`): append `uuid, updated_at` to the END of the SQL column list and map with the next two indexes: `uuid: row.get(N)?, updated_at: row.get(N + 1)?`. Appending at the end keeps all existing index-based `row.get(i)` calls valid. (`SELECT *` mappers need no SQL change — ALTER-added columns come last in table order.)
- **Non-DB construction sites** (scanner building a `File` before insert, test fixtures): set `uuid: None, updated_at: None` — the Task-3 INSERT trigger fills them.

- [ ] **Step 4: Run full test suite**

Run: `cargo test --workspace --quiet`
Expected: PASS — behavior is unchanged; only additive fields.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit -m "feat(core): expose uuid/updated_at on entity row models"
```

---

### Task 5: ts-rs dependency + TS derives on all 6 type-file sources

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml`
- Modify: `crates/athenaeum-core/src/models.rs`, `src/archive/models.rs`, `src/export/models.rs`, `src/file_op/models.rs`, `src/db/analysis.rs`, `src/db/calibration_links.rs`, plus the config structs backing `calibration-config.ts` / `plate-solve.ts` / `analysis-config.ts` (discovery below)

**Interfaces:**
- Produces: every Rust type mirrored in the 6 hand-written TS files implements `ts_rs::TS`. Task 6's registry references them by exact type name.

- [ ] **Step 1: Add the dependency**

In `crates/athenaeum-core/Cargo.toml` `[dependencies]`:

```toml
ts-rs = { version = "10", features = ["chrono-impl"] }
```

(`serde-compat` is a default feature — rename_all attributes are honored automatically. If any derived struct has a `serde_json::Value` field the build will say `TS is not implemented` — then add `"serde-json-impl"` to the feature list.)

- [ ] **Step 2: Build the type inventory**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
grep -n "^export \(interface\|type\|enum\|const enum\)" src/types/models.ts src/types/archive.ts src/types/export.ts src/types/calibration-config.ts src/types/plate-solve.ts src/types/analysis-config.ts > /tmp/ts-inventory.txt
wc -l /tmp/ts-inventory.txt
```

For each exported name, locate the Rust source: `grep -rn "pub struct <Name>\b\|pub enum <Name>\b" crates/athenaeum-core/src/`. Save the mapping — it becomes the Task 6 registry. Names with NO Rust source (hand-written TS-only helpers/unions) go to `helpers.ts` in Task 7, not the registry.

- [ ] **Step 3: Add derives**

For every mapped Rust type, extend the derive list (keep existing derives untouched):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct Frame { /* unchanged */ }
```

Do NOT add `#[ts(export)]` — export is driven by our harness, not ts-rs's own machinery.

- [ ] **Step 4: Build gate + commit**

```bash
cargo build --workspace
git add -A crates/athenaeum-core src/types 2>/dev/null; git add crates/athenaeum-core
git commit -m "feat(types): derive ts_rs::TS on all frontend-mirrored model types"
```

---

### Task 6: generation harness + `ts_contract` diff test

**Files:**
- Create: `crates/athenaeum-core/src/ts_export.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod ts_export;`)
- Test: `crates/athenaeum-core/tests/ts_contract.rs`

**Interfaces:**
- Consumes: `TS` impls from Task 5.
- Produces: `ts_export::generated_files() -> Vec<(&'static str, String)>` (rel path under `src/types/`, full file content); test `ts_contract` (diff mode) with env-var write mode `TS_RS_WRITE=1`.

- [ ] **Step 1: Write the harness**

```rust
//! Assembles the 6 frontend type files from the Rust model types.
//! Diffed against disk by tests/ts_contract.rs; regenerate with:
//!   TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

use ts_rs::TS;

const HEADER: &str = "// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.\n\
                      // Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract\n\n";

macro_rules! decls {
    ($($t:ty),* $(,)?) => {{
        let mut out = String::new();
        $( out.push_str(&format!("export {}\n\n", <$t as TS>::decl())); )*
        out
    }};
}

pub fn generated_files() -> Vec<(&'static str, String)> {
    vec![
        ("models.ts", format!("{HEADER}{}", decls![
            // paste the full ordered registry from Task 5 Step 2, e.g.:
            crate::models::File,
            crate::models::Frame,
            crate::models::FramesSet,
            crate::models::Session,
            crate::models::CatalogMeta,
            // ... every remaining models.ts-mapped type, in the original file order ...
        ])),
        ("archive.ts", format!(
            "{HEADER}import type {{ Frame }} from './models';\n\n{}",  // copy the ACTUAL import lines from the current hand-written file
            decls![ /* archive/models.rs types in original order */ ]
        )),
        ("export.ts", format!("{HEADER}{}", decls![ /* export/models.rs types */ ])),
        ("calibration-config.ts", format!("{HEADER}{}", decls![ /* calibration config types */ ])),
        ("plate-solve.ts", format!("{HEADER}{}", decls![ /* plate-solve types */ ])),
        ("analysis-config.ts", format!("{HEADER}{}", decls![ /* analysis config types */ ])),
    ]
}
```

The registry contents are mechanical: one line per mapped type from the Task 5 inventory, in the order they appear in today's hand-written files. Cross-file references need import preambles — copy each existing file's current `import` lines verbatim into its preamble string.

- [ ] **Step 2: Write the diff test**

```rust
// crates/athenaeum-core/tests/ts_contract.rs
use std::path::Path;

#[test]
fn ts_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/types");
    let write = std::env::var("TS_RS_WRITE").is_ok();
    let mut stale: Vec<String> = Vec::new();
    for (rel, content) in athenaeum_core::ts_export::generated_files() {
        let path = root.join(rel);
        if write {
            std::fs::write(&path, &content).unwrap();
            continue;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        if on_disk != content {
            stale.push(rel.to_string());
        }
    }
    assert!(
        stale.is_empty(),
        "stale generated TS files: {stale:?}\nRegenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract"
    );
}
```

- [ ] **Step 3: Run in diff mode — must fail against hand-written files**

Run: `cargo test -p athenaeum-core --test ts_contract`
Expected: FAIL listing all 6 files (hand-written ≠ generated).

- [ ] **Step 4: Generate**

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract && cargo test -p athenaeum-core --test ts_contract`
Expected: write pass, then diff-mode PASS.

- [ ] **Step 5: Commit** (frontend will be red until Task 7 — commit only the Rust side + generated files together with Task 7; no commit yet)

---

### Task 7: `helpers.ts`, frontend adjustment, rename canary

**Files:**
- Create: `src/types/helpers.ts`
- Modify: the 6 generated files' consumers across `src/` (compiler-driven)
- Modify: `src/types/models.ts` etc. — now generated (from Task 6)

**Interfaces:**
- Produces: `helpers.ts` carrying every hand-written function/const from the old type files (e.g. `isMasterType` from old models.ts:35) plus `as const` companion objects for former TS enums.

- [ ] **Step 1: Extract hand-written helpers**

From `git show HEAD:src/types/models.ts` (and the other 5), copy every function/const/TS-only type that had no Rust source into `src/types/helpers.ts`. For each former `enum` referenced as a VALUE in frontend code, add a companion:

```ts
import type { ImageType } from './models';

export const ImageTypeValues = {
  Light: 'Light', Dark: 'Dark', Flat: 'Flat', Bias: 'Bias', DarkFlat: 'DarkFlat',
  MasterLight: 'MasterLight', MasterDark: 'MasterDark', MasterFlat: 'MasterFlat',
  MasterBias: 'MasterBias', MasterDarkFlat: 'MasterDarkFlat',
} as const satisfies Record<string, ImageType>;
```

(Copy the exact variant strings from the generated union — they come from serde's serialization of `models.rs:96` `ImageType`, so verify against the generated `models.ts` content, not from memory.)

- [ ] **Step 2: Fix consumers compiler-driven**

Run: `npx tsc --noEmit 2>&1 | head -40`
Fix in order: imports of removed helpers → point at `./helpers`; enum-value usages `ImageType.Light` → `ImageTypeValues.Light` (type positions keep importing `ImageType` from `./models`). Repeat until clean.

- [ ] **Step 3: Full gates**

```bash
npx tsc --noEmit && cargo test -p athenaeum-core --test ts_contract
```
Expected: both PASS.

- [ ] **Step 4: Rename canary — prove the contract test catches drift**

```bash
# temporarily rename a field
sed -i '' 's/pub rotation: /pub rotation_canary: /' crates/athenaeum-core/src/models.rs
cargo test -p athenaeum-core --test ts_contract; echo "exit: $?"   # Expected: FAIL (exit 101)
git checkout crates/athenaeum-core/src/models.rs                   # revert
cargo test -p athenaeum-core --test ts_contract                    # Expected: PASS
```

- [ ] **Step 5: Commit everything from Tasks 6+7**

```bash
git add crates/athenaeum-core/src/ts_export.rs crates/athenaeum-core/src/lib.rs crates/athenaeum-core/tests/ts_contract.rs src/types src/
git commit -m "feat(types): ts-rs generated type files + ts_contract diff test + helpers.ts (Stage 4)"
```

---

### Task 8: `core::api` skeleton — `ApiError` + `PathPolicy`

**Files:**
- Create: `crates/athenaeum-core/src/api/mod.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod api;`)
- Modify: `crates/athenaeum-web/src/routes/mod.rs` (add `pub(crate) fn api_err`)
- Test: unit tests in `api/mod.rs`

**Interfaces:**
- Produces (used by every wrapper in Tasks 9–12):
  - `api::ApiError::{NotFound, Invalid, Conflict, Forbidden, Internal}(String)` implementing `Display` + `From<rusqlite::Error>` + `From<anyhow::Error>`
  - `api::PathPolicy::{AllowAll, AllowedRoots(Vec<PathBuf>)}` with `check(&self, &Path) -> Result<(), ApiError>`
  - `api::db(ctx: &ServiceContext) -> Result<..., ApiError>` (same return type as today's `ctx.db.get()` — mirror the existing expression)
  - web-side `api_err(e: ApiError) -> (StatusCode, String)` mapping NotFound→404, Invalid→400, Conflict→409, Forbidden→403, Internal→500

- [ ] **Step 1: Write the failing unit tests** (bottom of new `api/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn display_shows_message() {
        assert_eq!(ApiError::NotFound("frame 7".into()).to_string(), "frame 7");
    }

    #[test]
    fn path_policy_allows_and_forbids() {
        let p = PathPolicy::AllowedRoots(vec!["/data/astro".into()]);
        assert!(p.check(Path::new("/data/astro/lights")).is_ok());
        assert!(matches!(p.check(Path::new("/etc/passwd")), Err(ApiError::Forbidden(_))));
        assert!(PathPolicy::AllowAll.check(Path::new("/anything")).is_ok());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core api::` → FAIL (module missing).

- [ ] **Step 3: Implement `api/mod.rs`**

```rust
//! Shared command layer: one handler per command, wrapped by thin Tauri/Axum shims.
//! Handlers do NOT carry #[tracing::instrument] — boundary spans live on the wrappers.

use std::path::{Path, PathBuf};

pub mod scan_roots;      // Task 9
pub mod files;           // Task 10
pub mod calibration;     // Task 11
pub mod analysis;        // Task 12

#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    Invalid(String),
    Conflict(String),
    Forbidden(String),
    Internal(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NotFound(m) | ApiError::Invalid(m) | ApiError::Conflict(m)
            | ApiError::Forbidden(m) | ApiError::Internal(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for ApiError {}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self { ApiError::Internal(e.to_string()) }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self { ApiError::Internal(format!("{e:#}")) }
}

#[derive(Debug, Clone)]
pub enum PathPolicy {
    AllowAll,
    AllowedRoots(Vec<PathBuf>),
}

impl PathPolicy {
    pub fn check(&self, p: &Path) -> Result<(), ApiError> {
        match self {
            PathPolicy::AllowAll => Ok(()),
            PathPolicy::AllowedRoots(roots) => {
                if roots.iter().any(|r| p.starts_with(r)) {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden(format!(
                        "path {} is outside the allowed roots", p.display()
                    )))
                }
            }
        }
    }
}
```

Add a `db` accessor mirroring the exact expression used in `crates/athenaeum-tauri/src/commands/scan_roots.rs:94` (`state.ctx.db.get().ok_or(...)`) — copy its return type:

```rust
pub fn db(ctx: &crate::services::ServiceContext) -> Result<DB_HANDLE_TYPE, ApiError> {
    ctx.db.get().ok_or_else(|| ApiError::Internal("Database not initialized".into()))
}
```

(`DB_HANDLE_TYPE` = whatever `ctx.db.get()` returns today — read it from `crates/athenaeum-core/src/services/mod.rs:52-85`; do not guess.)

- [ ] **Step 4: Web-side error mapper** — in `crates/athenaeum-web/src/routes/mod.rs`:

```rust
pub(crate) fn api_err(e: athenaeum_core::api::ApiError) -> (axum::http::StatusCode, String) {
    use athenaeum_core::api::ApiError as E;
    use axum::http::StatusCode as S;
    let code = match &e {
        E::NotFound(_) => S::NOT_FOUND,
        E::Invalid(_) => S::BAD_REQUEST,
        E::Conflict(_) => S::CONFLICT,
        E::Forbidden(_) => S::FORBIDDEN,
        E::Internal(_) => S::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string())
}
```

Note: `impl From<ApiError> for (StatusCode, String)` is impossible (orphan rule — both types foreign to the tuple impl), hence the helper fn.

- [ ] **Step 5: Comment out the four `pub mod` lines** (their files don't exist yet; re-enable one per task), run `cargo test -p athenaeum-core api::` → PASS, `cargo build --workspace` → green.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/api crates/athenaeum-core/src/lib.rs crates/athenaeum-web/src/routes/mod.rs
git commit -m "feat(api): shared command-layer skeleton (ApiError, PathPolicy, web status mapping)"
```

---

### Task 9: pilot conversion — `scan_roots`

**Files:**
- Create: `crates/athenaeum-core/src/api/scan_roots.rs`
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs`
- Modify: `crates/athenaeum-web/src/routes/scan_roots.rs`

**Interfaces:**
- Consumes: `ApiError`, `PathPolicy`, `api::db` (Task 8); `ProgressEmitter` (`crates/athenaeum-core/src/events.rs:7`).
- Produces: one `api::scan_roots::<name>` handler per scan_roots command, e.g. `pub fn get_scan_roots(ctx: &ServiceContext) -> Result<Vec<ScanRoot>, ApiError>` and `pub fn add_scan_root(ctx: &ServiceContext, path: String, policy: &PathPolicy) -> Result<ScanRoot, ApiError>`. Wrappers on both sides shrink to extraction + mapping only.

**Conversion recipe (applies verbatim to Tasks 10–12):**
1. For each command in the module, move the DESKTOP body (`commands/<mod>.rs`) into `api::<mod>::<fn>` — the desktop body is authoritative; diff it against the web body first (`git diff --no-index` on the two files helps) and port any web-only logic as either (a) `PathPolicy` checks where the web did `allowed_paths` validation, or (b) status-specific errors: web `BAD_REQUEST` → `ApiError::Invalid`, `CONFLICT` → `Conflict`, `FORBIDDEN` → `Forbidden`, everything else `Internal`.
2. Handler signature: `ctx: &ServiceContext` first; typed args next; `policy: &PathPolicy` if the command takes user paths; `emitter: &dyn ProgressEmitter` last if the body emits progress. `async fn` only if the body awaits.
3. Replace `ok_or("Database not initialized")?` with `api::db(ctx)?`; replace `.map_err(|e| e.to_string())` with `?` (ApiError has the From impls).
4. Wrappers keep their `#[tracing::instrument]` attribute EXACTLY as-is.

- [ ] **Step 1: Write the two exemplar handlers** in `api/scan_roots.rs`:

```rust
use crate::api::{db, ApiError, PathPolicy};
use crate::models::ScanRoot;
use crate::services::ServiceContext;

pub fn get_scan_roots(ctx: &ServiceContext) -> Result<Vec<ScanRoot>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_scan_roots(&conn)?)
}

pub fn add_scan_root(ctx: &ServiceContext, path: String, policy: &PathPolicy) -> Result<ScanRoot, ApiError> {
    let p = std::path::Path::new(&path);
    policy.check(p)?;
    // >>> moved body of commands/scan_roots.rs:15-88 (validation, overlap checks, upsert) <<<
    // overlap/duplicate cases return ApiError::Conflict(msg) — matching the web route's
    // current CONFLICT mapping at routes/scan_roots.rs:81-160.
    todo!("move desktop body here in Step 3")
}
```

(Adjust `ScanRoot`'s import path to wherever it actually lives — `grep -n "pub struct ScanRoot" crates/athenaeum-core/src/`.)

- [ ] **Step 2: Convert the wrappers.** Tauri (`commands/scan_roots.rs`):

```rust
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_scan_roots(state: State<'_, AppState>) -> Result<Vec<ScanRoot>, String> {
    athenaeum_core::api::scan_roots::get_scan_roots(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn add_scan_root(path: String, state: State<'_, AppState>) -> Result<ScanRoot, String> {
    athenaeum_core::api::scan_roots::add_scan_root(&state.ctx, path, &athenaeum_core::api::PathPolicy::AllowAll)
        .map_err(|e| e.to_string())
}
```

Web (`routes/scan_roots.rs`):

```rust
#[tracing::instrument(skip_all, err(Debug))]
pub async fn add_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<AddScanRootArgs>,           // existing extractor struct stays
) -> Result<Json<ScanRoot>, (StatusCode, String)> {
    let policy = athenaeum_core::api::PathPolicy::AllowedRoots(state.allowed_paths.clone());
    athenaeum_core::api::scan_roots::add_scan_root(&state.ctx, args.path, &policy)
        .map(Json)
        .map_err(crate::routes::api_err)
}
```

- [ ] **Step 3: Move the remaining scan_roots command bodies** (enumerate: `grep -n "pub async fn" crates/athenaeum-tauri/src/commands/scan_roots.rs`) through the same recipe, one commit-sized chunk. `run_registered_scan`-style calls already take `&dyn ProgressEmitter` — pass the transport emitter from each wrapper exactly as the current code constructs it.

- [ ] **Step 4: Gates**

```bash
cargo build --workspace && cargo test --workspace --quiet && npx tsc --noEmit
```
Expected: green; web `{config}`-wrapper regression tests and scan tests unchanged.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit -m "refactor(api): scan_roots commands single-sourced in core::api (pilot 1/4)"
```

---

### Task 10: pilot conversion — `files`

**Files:**
- Create: `crates/athenaeum-core/src/api/files.rs`
- Modify: `crates/athenaeum-tauri/src/commands/files.rs`, `crates/athenaeum-web/src/routes/files.rs`

Apply the Task 9 recipe to every command pair in the module (`grep -n "pub async fn" crates/athenaeum-tauri/src/commands/files.rs` for the list; file-op enqueue sites keep calling `operation_queue.enqueue(...)` from the handler). Re-enable `pub mod files;` in `api/mod.rs`.

- [ ] **Step 1: Enumerate commands + diff desktop vs web bodies**
- [ ] **Step 2: Move bodies into `api::files`, one handler per command, recipe rules 1–4**
- [ ] **Step 3: Shrink both wrappers per command**
- [ ] **Step 4: Gates:** `cargo build --workspace && cargo test --workspace --quiet`
- [ ] **Step 5: Commit:** `git commit -m "refactor(api): files commands single-sourced in core::api (pilot 2/4)"`

---

### Task 11: pilot conversion — `calibration`

**Files:**
- Create: `crates/athenaeum-core/src/api/calibration.rs`
- Modify: `crates/athenaeum-tauri/src/commands/calibration.rs`, `crates/athenaeum-web/src/routes/calibration.rs`

Same recipe. This is the highest-churn module (39 commits desktop vs 11 web) — expect the web side to be BEHIND the desktop side; the desktop body is authoritative, and any web-side divergence you find is a drift bug: note each one in the commit message. `set_*_config` routes keep their web-side `{config}` extractor structs (the `8999e33e` regression tests must stay green).

- [ ] **Step 1: Enumerate + diff** (`grep -n "pub async fn" crates/athenaeum-tauri/src/commands/calibration.rs`)
- [ ] **Step 2: Move bodies into `api::calibration`**
- [ ] **Step 3: Shrink wrappers**
- [ ] **Step 4: Gates** incl. `cargo test -p athenaeum-web` (config-wrapper tests)
- [ ] **Step 5: Commit:** `git commit -m "refactor(api): calibration commands single-sourced in core::api (pilot 3/4)"`

---

### Task 12: pilot conversion — `analysis` (ProgressEmitter unification) + checklist doc

**Files:**
- Create: `crates/athenaeum-core/src/api/analysis.rs`
- Modify: `crates/athenaeum-tauri/src/commands/analysis.rs` (body at :83-294), `crates/athenaeum-web/src/routes/analysis.rs` (body at :155-375, delete the "mirrors" comment at :1)
- Modify: `CLAUDE.md` (add-a-command checklist)

**Interfaces:**
- Consumes: `ProgressEmitter` (`events.rs:7`), `TauriProgressEmitter` (`tauri/src/tauri_events.rs:5`), `SseProgressEmitter` (`web/src/events.rs:12`).
- Produces: `api::analysis::analyze_frame_set(ctx, <args as today>, thread_budget: usize, emitter: &dyn ProgressEmitter) -> Result<_, ApiError>` — single implementation of the worker-pool loop.

- [ ] **Step 1: Diff the two bodies** — `diff <(sed -n '83,294p' crates/athenaeum-tauri/src/commands/analysis.rs) <(sed -n '155,375p' crates/athenaeum-web/src/routes/analysis.rs)`. Expected: only the emit calls differ (`app_handle.emit("analysis-progress", …)` vs `event_tx.send(SseEvent{…})`).
- [ ] **Step 2: Move the body into `api::analysis::analyze_frame_set`**, replacing both emit styles with `crate::events::emit_event(emitter, "analysis-progress", &payload)` — event names `analysis-progress` / `analysis-complete` MUST stay byte-identical (frontend listens on them).
- [ ] **Step 3: Shrink both wrappers; desktop passes `TauriProgressEmitter(app_handle)`, web passes `SseProgressEmitter{tx: state.event_tx.clone()}` — matching how `run_registered_scan` is already invoked on each side (see `commands/scan_roots.rs:574` / `routes/scan_roots.rs:212` for the live examples).**
- [ ] **Step 4: Merge the copy-pasted helper** `recreate_calibration_sets_for_root` (`commands/scan_roots.rs:486` + `routes/scan_roots.rs:552`) into `api::scan_roots` and point both callers at it.
- [ ] **Step 5: Convert the remaining analysis commands per the Task 9 recipe.**
- [ ] **Step 6: Update `CLAUDE.md` add-a-command checklist** — append: "New commands: implement in `athenaeum-core/src/api/<module>.rs` (handler takes `&ServiceContext`, typed args, `&PathPolicy` for user paths, `&dyn ProgressEmitter` for progress), then add the two 3–5-line wrappers; register in `invoke_handler![]` (tauri/src/lib.rs:207) and `build_router` (web/src/routes/mod.rs:36); add new model types to `ts_export.rs` registry."
- [ ] **Step 7: Gates + commit**

```bash
cargo build --workspace && cargo test --workspace --quiet && npx tsc --noEmit
git add -A crates/ CLAUDE.md
git commit -m "refactor(api): analysis single-sourced with ProgressEmitter; command checklist updated (pilot 4/4)"
```

---

### Task 13: `fits_writer` — card model + grammar (`card.rs`)

**Files:**
- Create: `crates/athenaeum-core/src/fits_writer/mod.rs`, `crates/athenaeum-core/src/fits_writer/card.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod fits_writer;`)
- Test: `#[cfg(test)]` in `card.rs`

**Interfaces:**
- Produces (consumed by Tasks 14–15):
  - `CardValue::{Logical(bool), Integer(i64), Real(f64), Str(String)}`
  - `Card::new(keyword: &str, value: CardValue) -> Result<Card, FitsWriteError>`; `Card::with_comment(self, &str) -> Card`; `Card::comment_cards(&str) -> Vec<Card>`; `Card::history_cards(&str) -> Vec<Card>`
  - `format_card(&Card) -> Result<Vec<[u8; 80]>, FitsWriteError>` (CONTINUE chains yield >1 record)
  - `FitsWriteError::{InvalidKeyword, ReservedKeyword, NonAsciiString, CommentTooLong, ValueTooLong, NonFiniteReal, DataSizeMismatch, BadChannels, Io}`
  - consts `CARD_SIZE: usize = 80`, `BLOCK_SIZE: usize = 2880`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn s(records: &[[u8; 80]], i: usize) -> String {
        String::from_utf8(records[i].to_vec()).unwrap()
    }

    #[test]
    fn logical_fixed_format_t_in_col_30() {
        let c = Card::new("SIMPLE2", CardValue::Logical(true)).unwrap();
        let r = format_card(&c).unwrap();
        let line = s(&r, 0);
        assert_eq!(&line[0..8], "SIMPLE2 ");
        assert_eq!(&line[8..10], "= ");
        assert_eq!(line.as_bytes()[29], b'T', "logical value in column 30");
    }

    #[test]
    fn integer_right_justified_to_col_30() {
        let c = Card::new("GAIN", CardValue::Integer(100)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert_eq!(&line[10..30], "                 100");
    }

    #[test]
    fn real_always_has_decimal_point() {
        let c = Card::new("EXPTIME", CardValue::Real(300.0)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert!(line[10..30].contains("300.0"), "got {line:?}");
    }

    #[test]
    fn string_quotes_doubled_and_closing_quote_at_or_after_col_20() {
        let c = Card::new("OBJECT", CardValue::Str("O'Neill".into())).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert!(line.contains("'O''Neill "), "quote doubling + min-8 padding: {line:?}");
    }

    #[test]
    fn long_string_emits_continue_chain() {
        let long = "x".repeat(100);
        let c = Card::new("ATH_SRC", CardValue::Str(long.clone())).unwrap();
        let r = format_card(&c).unwrap();
        assert!(r.len() >= 2);
        let first = s(&r, 0);
        let second = s(&r, 1);
        assert!(first.trim_end().ends_with("&'"), "continuation marker: {first:?}");
        assert!(second.starts_with("CONTINUE  "), "CONTINUE card, no value indicator: {second:?}");
    }

    #[test]
    fn keyword_validation() {
        assert!(Card::new("TOOLONGKEY", CardValue::Integer(1)).is_err()); // 10 chars
        assert!(Card::new("BAD KEY", CardValue::Integer(1)).is_err());    // space
        assert!(Card::new("gain", CardValue::Integer(1)).is_ok());        // lowercase normalized
        assert!(matches!(
            Card::new("NAXIS1", CardValue::Integer(1)),
            Err(FitsWriteError::ReservedKeyword(_))
        ));
    }

    #[test]
    fn non_ascii_rejected() {
        assert!(matches!(
            format_card(&Card::new("OBJECT", CardValue::Str("Туманность".into())).unwrap()),
            Err(FitsWriteError::NonAsciiString(_))
        ));
    }

    #[test]
    fn comment_must_fit() {
        let c = Card::new("GAIN", CardValue::Integer(100)).unwrap()
            .with_comment(&"c".repeat(100));
        assert!(matches!(format_card(&c), Err(FitsWriteError::CommentTooLong(_))));
    }

    #[test]
    fn comment_and_history_cards_split_at_72() {
        let cards = Card::comment_cards(&"y".repeat(100));
        assert_eq!(cards.len(), 2);
        let r = format_card(&cards[0]).unwrap();
        assert!(s(&r, 0).starts_with("COMMENT "));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core fits_writer` → FAIL (module missing).

- [ ] **Step 3: Implement `card.rs`**

```rust
//! FITS 4.0 header cards: grammar validation + 80-byte record serialization.
//! Fixed-format values (FITS 4.0 §4.2); long strings via the CONTINUE convention (§4.2.1.2).

pub const CARD_SIZE: usize = 80;
pub const BLOCK_SIZE: usize = 2880;
const MAX_STR_CONTENT: usize = 68; // printable chars inside the quotes of one card

#[derive(Debug)]
pub enum FitsWriteError {
    InvalidKeyword(String),
    ReservedKeyword(String),
    NonAsciiString(String),
    CommentTooLong(String),
    ValueTooLong(String),
    NonFiniteReal(String),
    DataSizeMismatch { expected: usize, got: usize },
    BadChannels(usize),
    Io(std::io::Error),
}

impl std::fmt::Display for FitsWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKeyword(k) => write!(f, "invalid FITS keyword: {k}"),
            Self::ReservedKeyword(k) => write!(f, "structural keyword not allowed in user cards: {k}"),
            Self::NonAsciiString(k) => write!(f, "non-printable-ASCII string value for {k}"),
            Self::CommentTooLong(k) => write!(f, "comment does not fit the card for {k}"),
            Self::ValueTooLong(k) => write!(f, "value does not fit fixed format for {k}"),
            Self::NonFiniteReal(k) => write!(f, "non-finite real value for {k}"),
            Self::DataSizeMismatch { expected, got } => write!(f, "data length {got}, expected {expected}"),
            Self::BadChannels(c) => write!(f, "channels must be 1 or 3, got {c}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}
impl std::error::Error for FitsWriteError {}
impl From<std::io::Error> for FitsWriteError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CardValue {
    Logical(bool),
    Integer(i64),
    Real(f64),
    Str(String),
}

#[derive(Debug, Clone)]
pub struct Card {
    pub keyword: String,
    pub value: Option<CardValue>, // None => COMMENT/HISTORY-style text card
    pub comment: Option<String>,
    pub(crate) text: Option<String>, // COMMENT/HISTORY payload
}

const RESERVED: [&str; 6] = ["SIMPLE", "BITPIX", "END", "BZERO", "BSCALE", "CONTINUE"];

fn validate_keyword(kw: &str) -> Result<String, FitsWriteError> {
    let up = kw.to_ascii_uppercase();
    if up.is_empty() || up.len() > 8
        || !up.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(FitsWriteError::InvalidKeyword(kw.to_string()));
    }
    if RESERVED.contains(&up.as_str()) || (up.starts_with("NAXIS")) {
        return Err(FitsWriteError::ReservedKeyword(up));
    }
    Ok(up)
}

impl Card {
    pub fn new(keyword: &str, value: CardValue) -> Result<Card, FitsWriteError> {
        Ok(Card { keyword: validate_keyword(keyword)?, value: Some(value), comment: None, text: None })
    }

    pub fn with_comment(mut self, comment: &str) -> Card {
        self.comment = Some(comment.to_string());
        self
    }

    fn text_cards(kind: &str, text: &str) -> Vec<Card> {
        text.as_bytes()
            .chunks(72)
            .map(|c| Card {
                keyword: kind.to_string(),
                value: None,
                comment: None,
                text: Some(String::from_utf8_lossy(c).into_owned()),
            })
            .collect()
    }
    pub fn comment_cards(text: &str) -> Vec<Card> { Self::text_cards("COMMENT", text) }
    pub fn history_cards(text: &str) -> Vec<Card> { Self::text_cards("HISTORY", text) }

    /// Internal constructor for writer-owned structural cards (bypasses RESERVED).
    pub(crate) fn structural(keyword: &str, value: CardValue, comment: &str) -> Card {
        Card { keyword: keyword.to_string(), value: Some(value), comment: Some(comment.to_string()), text: None }
    }
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

fn fmt_real(kw: &str, v: f64) -> Result<String, FitsWriteError> {
    if !v.is_finite() {
        return Err(FitsWriteError::NonFiniteReal(kw.to_string()));
    }
    let mut s = format!("{v}");
    if !s.contains('.') { s.push_str(".0"); }
    if s.len() > 20 {
        s = format!("{v:.10E}");
        if !s.contains('.') {
            let e = s.find('E').unwrap();
            s.insert_str(e, ".0");
        }
    }
    if s.len() > 20 {
        return Err(FitsWriteError::ValueTooLong(kw.to_string()));
    }
    Ok(s)
}

fn pack(line: &str) -> [u8; 80] {
    let mut rec = [b' '; 80];
    rec[..line.len()].copy_from_slice(line.as_bytes());
    rec
}

pub fn format_card(card: &Card) -> Result<Vec<[u8; 80]>, FitsWriteError> {
    // COMMENT / HISTORY text cards
    if let Some(text) = &card.text {
        if !is_printable_ascii(text) {
            return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
        }
        return Ok(vec![pack(&format!("{:<8}{}", card.keyword, text))]);
    }

    let value = card.value.as_ref().expect("value card");
    let kw8 = format!("{:<8}", card.keyword);

    // Strings get their own path (CONTINUE support)
    if let CardValue::Str(s) = value {
        if !is_printable_ascii(s) {
            return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
        }
        let escaped = s.replace('\'', "''");
        if escaped.len() <= MAX_STR_CONTENT {
            // fixed format: opening quote col 11, closing quote at/after col 20 => pad to >= 8
            let mut line = format!("{kw8}= '{:<8}'", escaped);
            if let Some(c) = &card.comment {
                let candidate = format!("{line} / {c}");
                if candidate.len() > CARD_SIZE {
                    return Err(FitsWriteError::CommentTooLong(card.keyword.clone()));
                }
                line = candidate;
            }
            return Ok(vec![pack(&line)]);
        }
        // CONTINUE chain: each card carries <= 67 content chars + '&' except the last
        let mut records = Vec::new();
        let chars: Vec<char> = escaped.chars().collect();
        let mut idx = 0;
        let mut first = true;
        while idx < chars.len() {
            let take = (chars.len() - idx).min(MAX_STR_CONTENT - 1);
            let chunk: String = chars[idx..idx + take].iter().collect();
            idx += take;
            let cont = idx < chars.len();
            let payload = if cont { format!("{chunk}&") } else { chunk };
            let line = if first {
                first = false;
                format!("{kw8}= '{payload}'")
            } else {
                format!("CONTINUE  '{payload}'")
            };
            if !cont {
                if let Some(c) = &card.comment {
                    let candidate = format!("{line} / {c}");
                    if candidate.len() > CARD_SIZE {
                        return Err(FitsWriteError::CommentTooLong(card.keyword.clone()));
                    }
                    records.push(pack(&candidate));
                    return Ok(records);
                }
            }
            records.push(pack(&line));
        }
        return Ok(records);
    }

    // fixed-format non-string values right-justified to column 30
    let vstr = match value {
        CardValue::Logical(b) => format!("{:>20}", if *b { "T" } else { "F" }),
        CardValue::Integer(i) => format!("{:>20}", i),
        CardValue::Real(r) => format!("{:>20}", fmt_real(&card.keyword, *r)?),
        CardValue::Str(_) => unreachable!(),
    };
    let mut line = format!("{kw8}= {vstr}");
    if let Some(c) = &card.comment {
        if !is_printable_ascii(c) {
            return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
        }
        let candidate = format!("{line} / {c}");
        if candidate.len() > CARD_SIZE {
            return Err(FitsWriteError::CommentTooLong(card.keyword.clone()));
        }
        line = candidate;
    }
    Ok(vec![pack(&line)])
}
```

`mod.rs`:

```rust
//! Standards-compliant FITS writer (FITS 4.0): BITPIX=-32 primary HDU, typed keyword vocabulary.
pub mod card;
pub mod writer;    // Task 14
pub mod keywords;  // Task 15
pub use card::{Card, CardValue, FitsWriteError};
pub use writer::{write_fits_f32, write_fits_f32_to};
```

(Comment out the `writer`/`keywords` lines until their tasks land.)

- [ ] **Step 4: Run tests** — `cargo test -p athenaeum-core fits_writer` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/fits_writer crates/athenaeum-core/src/lib.rs
git commit -m "feat(fits): FITS 4.0 card model — grammar validation, fixed format, CONTINUE chains"
```

---

### Task 14: `fits_writer` — file serialization + round-trips (`writer.rs`)

**Files:**
- Create: `crates/athenaeum-core/src/fits_writer/writer.rs`
- Test: `crates/athenaeum-core/tests/fits_roundtrip.rs`

**Interfaces:**
- Consumes: `Card`, `CardValue`, `format_card`, `FitsWriteError` (Task 13); readers: `crate::fits_parser` (header), rustafits `ImageConverter::read_raw` (data).
- Produces: `write_fits_f32(path: &Path, width: usize, height: usize, channels: usize, data: &[f32], cards: &[Card]) -> Result<(), FitsWriteError>` and `write_fits_f32_to<W: Write>(w, ...)`. Data layout: plane-major for channels=3 (all R, then G, then B) — matches FITS NAXIS3 semantics.

- [ ] **Step 1: Write the failing round-trip tests**

```rust
// crates/athenaeum-core/tests/fits_roundtrip.rs
use athenaeum_core::fits_writer::{write_fits_f32, Card, CardValue};
// Header reader: FitsHeader::from_path(path) -> Result<FitsHeader>
//   (crates/athenaeum-core/src/fits_parser/fits_header_reader.rs:20; getters :135-149)
// Data reader: astroimage::{ImageConverter, PixelData} — ImageConverter::read_raw(path)
//   (import pattern as in crates/athenaeum-core/src/flat_analysis.rs:37)

fn sample_cards() -> Vec<Card> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Master Dark".into())).unwrap(),
        Card::new("EXPTIME", CardValue::Real(300.0)).unwrap().with_comment("[s] exposure"),
        Card::new("GAIN", CardValue::Integer(100)).unwrap(),
        Card::new("CCD-TEMP", CardValue::Real(-10.5)).unwrap().with_comment("[degC]"),
        Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
        Card::new("ATH_SRC", CardValue::Str("u".repeat(80))).unwrap(), // forces CONTINUE
    ];
    cards.extend(Card::history_cards("integrated by athenaeum test"));
    cards
}

#[test]
fn header_roundtrip_through_fits_parser() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rt.fits");
    let data: Vec<f32> = (0..12).map(|i| i as f32 / 3.0).collect();
    write_fits_f32(&path, 4, 3, 1, &data, &sample_cards()).unwrap();

    let header = athenaeum_core::fits_parser::FitsHeader::from_path(&path).unwrap();
    // (if FitsHeader is not re-exported at fits_parser root, use the fits_header_reader path
    //  or add a `pub use` — check crates/athenaeum-core/src/fits_parser/mod.rs imports)
    assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Dark"));
    assert_eq!(header.get_f64("EXPTIME"), Some(300.0));
    assert_eq!(header.get_i32("GAIN"), Some(100));
    assert_eq!(header.get_f64("CCD-TEMP"), Some(-10.5));
    assert_eq!(header.get_str("ATH_SRC").as_deref(), Some("u".repeat(80).as_str()),
        "CONTINUE chain must reassemble");
}

#[test]
fn data_roundtrip_through_rustafits_bit_exact_incl_nan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rt2.fits");
    let mut data: Vec<f32> = (0..64).map(|i| (i as f32).sin()).collect();
    data[7] = f32::NAN;
    write_fits_f32(&path, 8, 8, 1, &data, &[
        Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(), // suppress reader flip
    ]).unwrap();

    let converter = astroimage::ImageConverter::new(); // check ctor signature at rustafits/src/converter.rs:65
    let (_meta, pixels) = converter.read_raw(&path).unwrap();
    let read = match pixels {
        astroimage::PixelData::Float32(v) => v,
        other => panic!("expected Float32, got {other:?}"),
    };
    assert_eq!(read.len(), data.len());
    for (a, b) in read.iter().zip(&data) {
        assert_eq!(a.to_bits(), b.to_bits(), "bit-exact incl. NaN");
    }
}

#[test]
fn rgb_dims_and_size_validation() {
    let dir = tempfile::tempdir().unwrap();
    let ok = write_fits_f32(&dir.path().join("rgb.fits"), 2, 2, 3, &[0.0f32; 12], &[]);
    assert!(ok.is_ok());
    let bad = write_fits_f32(&dir.path().join("bad.fits"), 2, 2, 1, &[0.0f32; 3], &[]);
    assert!(bad.is_err(), "data size mismatch must fail");
}
```

(`tempfile` is already a dev-dependency.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core --test fits_roundtrip` → FAIL (writer missing).

- [ ] **Step 3: Implement `writer.rs`**

```rust
use std::io::Write;
use std::path::Path;

use super::card::{format_card, Card, CardValue, FitsWriteError, BLOCK_SIZE, CARD_SIZE};

pub fn write_fits_f32(
    path: &Path, width: usize, height: usize, channels: usize,
    data: &[f32], cards: &[Card],
) -> Result<(), FitsWriteError> {
    let f = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::new(f);
    write_fits_f32_to(&mut w, width, height, channels, data, cards)?;
    w.flush()?;
    Ok(())
}

pub fn write_fits_f32_to<W: Write>(
    mut w: W, width: usize, height: usize, channels: usize,
    data: &[f32], cards: &[Card],
) -> Result<(), FitsWriteError> {
    if channels != 1 && channels != 3 {
        return Err(FitsWriteError::BadChannels(channels));
    }
    let expected = width * height * channels;
    if data.len() != expected {
        return Err(FitsWriteError::DataSizeMismatch { expected, got: data.len() });
    }

    let mut records: Vec<[u8; CARD_SIZE]> = Vec::new();
    let push = |records: &mut Vec<[u8; CARD_SIZE]>, c: Card| -> Result<(), FitsWriteError> {
        records.extend(format_card(&c)?);
        Ok(())
    };
    push(&mut records, Card::structural("SIMPLE", CardValue::Logical(true), "conforms to FITS standard"))?;
    push(&mut records, Card::structural("BITPIX", CardValue::Integer(-32), "IEEE single precision floating point"))?;
    let naxis: i64 = if channels == 3 { 3 } else { 2 };
    push(&mut records, Card::structural("NAXIS", CardValue::Integer(naxis), "number of data axes"))?;
    push(&mut records, Card::structural("NAXIS1", CardValue::Integer(width as i64), "width"))?;
    push(&mut records, Card::structural("NAXIS2", CardValue::Integer(height as i64), "height"))?;
    if channels == 3 {
        push(&mut records, Card::structural("NAXIS3", CardValue::Integer(3), "color planes"))?;
    }
    for c in cards {
        records.extend(format_card(c)?);
    }
    // END card
    let mut end = [b' '; CARD_SIZE];
    end[..3].copy_from_slice(b"END");
    records.push(end);

    for r in &records {
        w.write_all(r)?;
    }
    // pad header to 2880 with ASCII spaces
    let header_bytes = records.len() * CARD_SIZE;
    let pad = (BLOCK_SIZE - header_bytes % BLOCK_SIZE) % BLOCK_SIZE;
    w.write_all(&vec![b' '; pad])?;

    // data: big-endian f32, plane-major
    let mut buf = Vec::with_capacity(8192 * 4);
    for v in data {
        buf.extend_from_slice(&v.to_be_bytes());
        if buf.len() >= 8192 * 4 {
            w.write_all(&buf)?;
            buf.clear();
        }
    }
    w.write_all(&buf)?;
    let data_bytes = data.len() * 4;
    let dpad = (BLOCK_SIZE - data_bytes % BLOCK_SIZE) % BLOCK_SIZE;
    w.write_all(&vec![0u8; dpad])?;
    Ok(())
}
```

`Card::structural` bypasses the RESERVED check (it is `pub(crate)`, defined in Task 13). Fix the noted import line.

- [ ] **Step 4: Fill in the two reader call sites in the test** (from the discovery greps) and run:

Run: `cargo test -p athenaeum-core --test fits_roundtrip`
Expected: PASS ×3.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/fits_writer crates/athenaeum-core/tests/fits_roundtrip.rs
git commit -m "feat(fits): BITPIX=-32 primary-HDU writer, round-tripped through both existing readers"
```

---

### Task 15: `fits_writer` — keyword vocabulary (`keywords.rs`)

**Files:**
- Create: `crates/athenaeum-core/src/fits_writer/keywords.rs`
- Test: `#[cfg(test)]` in `keywords.rs`

**Interfaces:**
- Consumes: `Card`, `CardValue` (Task 13); `crate::models::ImageType` (models.rs:96) for round-trip assertions.
- Produces:
  - `FrameKind::{Light, Dark, Bias, Flat, DarkFlat, MasterLight, MasterDark, MasterBias, MasterFlat, MasterDarkFlat}` with `imagetyp(&self) -> &'static str`
  - `Bayer::{Rggb, Bggr, Gbrg, Grbg}`
  - `HeaderBuilder` (all methods `self -> Self`, `build(self) -> Result<Vec<Card>, FitsWriteError>`): `new(FrameKind)`, `swcreate(app_version)`, `exptime(secs)`, `date_obs(DateTime<Utc>)`, `ccd_temp(c)`, `set_temp(c)`, `gain(i64)`, `offset(i64)`, `egain(f64)`, `binning(x, y)`, `pixel_size(x_um, y_um)`, `bayer(Bayer, xoff, yoff)`, `radec(ra_deg, dec_deg)`, `instrume(&str)`, `telescop(&str)`, `focallen(mm)`, `filter(&str)`, `object(&str)`, `roworder_top_down()`, `calstat(&str)`, `pedestal(i64)`, `ath_src(&str)`, `ath_n(u32)`, `ath_rej(&str)`, `ath_ver(&str)`, `ath_hsh(&str)`, `ath_temp_span(min_c, max_c)`, `custom(Card)`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ImageType;

    #[test]
    fn every_canonical_imagetyp_roundtrips_through_our_parser() {
        let all = [
            (FrameKind::Light, ImageType::Light),
            (FrameKind::Dark, ImageType::Dark),
            (FrameKind::Bias, ImageType::Bias),
            (FrameKind::Flat, ImageType::Flat),
            (FrameKind::DarkFlat, ImageType::DarkFlat),
            (FrameKind::MasterLight, ImageType::MasterLight),
            (FrameKind::MasterDark, ImageType::MasterDark),
            (FrameKind::MasterBias, ImageType::MasterBias),
            (FrameKind::MasterFlat, ImageType::MasterFlat),
            (FrameKind::MasterDarkFlat, ImageType::MasterDarkFlat),
        ];
        for (kind, expected) in all {
            let parsed = ImageType::from_str(kind.imagetyp());
            assert_eq!(parsed, Some(expected), "IMAGETYP {:?}", kind.imagetyp());
        }
        // ImageType::from_str(s: &str) -> Option<Self> — verified at models.rs:111.
    }

    #[test]
    fn master_values_contain_master_substring_for_wbpp() {
        for k in [FrameKind::MasterLight, FrameKind::MasterDark, FrameKind::MasterBias,
                  FrameKind::MasterFlat, FrameKind::MasterDarkFlat] {
            assert!(k.imagetyp().to_lowercase().contains("master"));
        }
    }

    #[test]
    fn sexagesimal_reparse_within_arcsec() {
        // M31: RA 10.684708°, DEC +41.269065°
        let (ra_s, dec_s) = (ra_to_sexagesimal(10.684708), dec_to_sexagesimal(41.269065));
        assert_eq!(ra_s.split(' ').count(), 3, "{ra_s}");
        assert!(dec_s.starts_with('+'), "{dec_s}");
        // reparse and compare
        let parts: Vec<f64> = ra_s.split(' ').map(|p| p.parse().unwrap()).collect();
        let ra_back = (parts[0] + parts[1] / 60.0 + parts[2] / 3600.0) * 15.0;
        assert!((ra_back - 10.684708).abs() < 0.001 / 3600.0 * 15.0, "{ra_s} -> {ra_back}");
        let dparts: Vec<f64> = dec_s[1..].split(' ').map(|p| p.parse().unwrap()).collect();
        let dec_back = dparts[0] + dparts[1] / 60.0 + dparts[2] / 3600.0;
        assert!((dec_back - 41.269065).abs() < 0.01 / 3600.0, "{dec_s} -> {dec_back}");
    }

    #[test]
    fn negative_dec_sign() {
        assert!(dec_to_sexagesimal(-16.716).starts_with('-'));
    }

    #[test]
    fn ath_keywords_are_all_within_8_chars() {
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .ath_src("abc").ath_n(30).ath_rej("sigma3.0/2.5").ath_ver("0.2.4")
            .ath_hsh("deadbeef").ath_temp_span(-10.6, -9.8)
            .build().unwrap();
        for c in &cards {
            assert!(c.keyword.len() <= 8, "{}", c.keyword);
        }
        assert!(cards.iter().any(|c| c.keyword == "ATH_TMIN"));
    }

    #[test]
    fn builder_emits_units_in_comments() {
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .ccd_temp(-10.0).exptime(300.0).build().unwrap();
        let ccd = cards.iter().find(|c| c.keyword == "CCD-TEMP").unwrap();
        assert!(ccd.comment.as_deref().unwrap_or("").contains("[degC]"));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core keywords` → FAIL.

- [ ] **Step 3: Implement `keywords.rs`**

```rust
//! Typed FITS keyword vocabulary — canonical, standards-based header values.
//! Sources: SBFITSEXT 1.0 (IMAGETYP/EXPTIME/CCD-TEMP/…), NINA conventions
//! (GAIN/OFFSET/BAYERPAT/ROWORDER), WBPP master detection ("master" substring),
//! FITS 4.0 (dates, unit-bracket comments). Custom namespace: ATH_* (<= 8 chars).

use chrono::{DateTime, Utc};

use super::card::{Card, CardValue, FitsWriteError};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameKind {
    Light, Dark, Bias, Flat, DarkFlat,
    MasterLight, MasterDark, MasterBias, MasterFlat, MasterDarkFlat,
}

impl FrameKind {
    pub fn imagetyp(&self) -> &'static str {
        match self {
            FrameKind::Light => "Light Frame",
            FrameKind::Dark => "Dark Frame",
            FrameKind::Bias => "Bias Frame",
            FrameKind::Flat => "Flat Field",
            FrameKind::DarkFlat => "Dark Flat",
            FrameKind::MasterLight => "Master Light",
            FrameKind::MasterDark => "Master Dark",
            FrameKind::MasterBias => "Master Bias",
            FrameKind::MasterFlat => "Master Flat",
            FrameKind::MasterDarkFlat => "Master Dark Flat",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Bayer { Rggb, Bggr, Gbrg, Grbg }
impl Bayer {
    pub fn as_str(&self) -> &'static str {
        match self { Bayer::Rggb => "RGGB", Bayer::Bggr => "BGGR", Bayer::Gbrg => "GBRG", Bayer::Grbg => "GRBG" }
    }
}

pub fn ra_to_sexagesimal(ra_deg: f64) -> String {
    let total_h = ra_deg.rem_euclid(360.0) / 15.0;
    let h = total_h.floor();
    let total_m = (total_h - h) * 60.0;
    let m = total_m.floor();
    let s = (total_m - m) * 60.0;
    format!("{:02} {:02} {:06.3}", h as u32, m as u32, s)
}

pub fn dec_to_sexagesimal(dec_deg: f64) -> String {
    let sign = if dec_deg < 0.0 { '-' } else { '+' };
    let a = dec_deg.abs();
    let d = a.floor();
    let total_m = (a - d) * 60.0;
    let m = total_m.floor();
    let s = (total_m - m) * 60.0;
    format!("{sign}{:02} {:02} {:05.2}", d as u32, m as u32, s)
}

pub struct HeaderBuilder {
    cards: Vec<Card>,
    err: Option<FitsWriteError>,
}

impl HeaderBuilder {
    pub fn new(kind: FrameKind) -> Self {
        let mut b = HeaderBuilder { cards: Vec::new(), err: None };
        b.push_str("IMAGETYP", kind.imagetyp(), "type of image");
        b
    }

    fn push(&mut self, kw: &str, v: CardValue, comment: &str) {
        if self.err.is_some() { return; }
        match Card::new(kw, v) {
            Ok(c) => self.cards.push(if comment.is_empty() { c } else { c.with_comment(comment) }),
            Err(e) => self.err = Some(e),
        }
    }
    fn push_str(&mut self, kw: &str, v: &str, comment: &str) {
        self.push(kw, CardValue::Str(v.to_string()), comment);
    }

    pub fn swcreate(mut self, app_version: &str) -> Self {
        self.push_str("SWCREATE", &format!("Athenaeum {app_version}"), "software that created this file"); self
    }
    pub fn exptime(mut self, secs: f64) -> Self {
        self.push("EXPTIME", CardValue::Real(secs), "[s] exposure duration"); self
    }
    pub fn date_obs(mut self, t: DateTime<Utc>) -> Self {
        self.push_str("DATE-OBS", &t.format("%Y-%m-%dT%H:%M:%S%.3f").to_string(), "UTC observation start/midpoint"); self
    }
    pub fn ccd_temp(mut self, c: f64) -> Self {
        self.push("CCD-TEMP", CardValue::Real(c), "[degC] sensor temperature"); self
    }
    pub fn set_temp(mut self, c: f64) -> Self {
        self.push("SET-TEMP", CardValue::Real(c), "[degC] cooling setpoint"); self
    }
    pub fn gain(mut self, g: i64) -> Self { self.push("GAIN", CardValue::Integer(g), "camera gain setting"); self }
    pub fn offset(mut self, o: i64) -> Self { self.push("OFFSET", CardValue::Integer(o), "camera offset setting"); self }
    pub fn egain(mut self, e: f64) -> Self { self.push("EGAIN", CardValue::Real(e), "[e-/ADU] electronic gain"); self }
    pub fn binning(mut self, x: i64, y: i64) -> Self {
        self.push("XBINNING", CardValue::Integer(x), "binning factor X");
        self.push("YBINNING", CardValue::Integer(y), "binning factor Y"); self
    }
    pub fn pixel_size(mut self, x_um: f64, y_um: f64) -> Self {
        self.push("XPIXSZ", CardValue::Real(x_um), "[um] pixel width after binning");
        self.push("YPIXSZ", CardValue::Real(y_um), "[um] pixel height after binning"); self
    }
    pub fn bayer(mut self, b: Bayer, xoff: i64, yoff: i64) -> Self {
        self.push_str("BAYERPAT", b.as_str(), "Bayer color pattern");
        self.push("XBAYROFF", CardValue::Integer(xoff), "Bayer X offset");
        self.push("YBAYROFF", CardValue::Integer(yoff), "Bayer Y offset"); self
    }
    pub fn radec(mut self, ra_deg: f64, dec_deg: f64) -> Self {
        self.push("RA", CardValue::Real(ra_deg), "[deg] right ascension");
        self.push("DEC", CardValue::Real(dec_deg), "[deg] declination");
        self.push_str("OBJCTRA", &ra_to_sexagesimal(ra_deg), "RA of image center, HH MM SS.SSS");
        self.push_str("OBJCTDEC", &dec_to_sexagesimal(dec_deg), "DEC of image center, +DD MM SS.SS"); self
    }
    pub fn instrume(mut self, v: &str) -> Self { self.push_str("INSTRUME", v, "camera"); self }
    pub fn telescop(mut self, v: &str) -> Self { self.push_str("TELESCOP", v, "telescope"); self }
    pub fn focallen(mut self, mm: f64) -> Self { self.push("FOCALLEN", CardValue::Real(mm), "[mm] focal length"); self }
    pub fn filter(mut self, v: &str) -> Self { self.push_str("FILTER", v, "filter name"); self }
    pub fn object(mut self, v: &str) -> Self { self.push_str("OBJECT", v, "target name"); self }
    pub fn roworder_top_down(mut self) -> Self { self.push_str("ROWORDER", "TOP-DOWN", "image row order"); self }
    pub fn calstat(mut self, flags: &str) -> Self { self.push_str("CALSTAT", flags, "calibration state (B/D/F)"); self }
    pub fn pedestal(mut self, p: i64) -> Self { self.push("PEDESTAL", CardValue::Integer(p), "add to ADU for zero base"); self }

    pub fn ath_src(mut self, uuid: &str) -> Self { self.push_str("ATH_SRC", uuid, "source calibration_set uuid"); self }
    pub fn ath_n(mut self, n: u32) -> Self { self.push("ATH_N", CardValue::Integer(n as i64), "number of integrated frames"); self }
    pub fn ath_rej(mut self, v: &str) -> Self { self.push_str("ATH_REJ", v, "rejection algorithm"); self }
    pub fn ath_ver(mut self, v: &str) -> Self { self.push_str("ATH_VER", v, "athenaeum version"); self }
    pub fn ath_hsh(mut self, v: &str) -> Self { self.push_str("ATH_HSH", v, "xxh3 of member hash list"); self }
    pub fn ath_temp_span(mut self, min_c: f64, max_c: f64) -> Self {
        self.push("ATH_TMIN", CardValue::Real(min_c), "[degC] min member CCD-TEMP");
        self.push("ATH_TMAX", CardValue::Real(max_c), "[degC] max member CCD-TEMP"); self
    }

    pub fn custom(mut self, c: Card) -> Self { self.cards.push(c); self }

    pub fn build(self) -> Result<Vec<Card>, FitsWriteError> {
        match self.err { Some(e) => Err(e), None => Ok(self.cards) }
    }
}
```

Check `ImageType::from_str`'s real signature at `models.rs:111` before finalizing the test (it may be `Option<ImageType>` or implement `FromStr`) and adjust the assertion accordingly.

- [ ] **Step 4: Run tests** — `cargo test -p athenaeum-core keywords fits_writer` → PASS. Uncomment `pub mod keywords;` in `fits_writer/mod.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/fits_writer
git commit -m "feat(fits): typed keyword vocabulary (SBFITSEXT/NINA/WBPP canonical values, ATH_* namespace)"
```

---

### Task 16: scanner `EXPOSURE` fallback

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/mod.rs:254`
- Test: extend the inline test module at `fits_parser/mod.rs:815`

- [ ] **Step 1: Write the failing test** (build a header through the existing test helper pattern used at `fits_header_reader.rs:251-370` — a synthetic 2880-byte block with `EXPOSURE` but no `EXPTIME` — then assert `build_frame_from_header` (make it `pub(crate)` if it isn't) yields `exptime == Some(120.0)`).

```rust
    #[test]
    fn exptime_falls_back_to_exposure() {
        // reuse the write_card / block-building helper from fits_header_reader tests
        // header contains: EXPOSURE = 120.0, no EXPTIME
        // assert: parsed frame exptime == Some(120.0)
    }
```

(Write the real body against the existing helpers — copy `write_card` from `fits_header_reader.rs` tests if it is test-local.)

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement** at `mod.rs:254`:

```rust
    let exptime = header.get_f64("EXPTIME").or_else(|| header.get_f64("EXPOSURE"));
```

- [ ] **Step 4: Run** `cargo test -p athenaeum-core fits_parser` → PASS.

- [ ] **Step 5: Commit:** `git commit -m "fix(scanner): EXPOSURE fallback for EXPTIME (parity with stored-header path)"`

---

### Task 17: final gates + as-built docs

**Files:**
- Modify: `docs/superpowers/plans/2026-07-02-roadmap.md` (tick Phase 1 checkboxes)
- Create: `scripts/verify_fits_astropy.py` (optional dev tool)

- [ ] **Step 1: Full gates**

```bash
cargo build --workspace && cargo test --workspace && npx tsc --noEmit
```
Expected: all green.

- [ ] **Step 2: Optional astropy cross-check script**

```python
#!/usr/bin/env python3
"""Dev-only: validate an athenaeum-written FITS against astropy (reference impl).
Usage: python3 scripts/verify_fits_astropy.py <file.fits>"""
import sys
from astropy.io import fits
with fits.open(sys.argv[1]) as hdul:
    hdul.verify('exception')
    print("OK:", repr(hdul[0].header))
```

- [ ] **Step 3: Tick the five Phase 1 checkboxes in the roadmap** (catalog_meta/uuid, updated_at discipline, ts-rs, shared layer, FITS writer) with a one-line status note pointing at this plan.

- [ ] **Step 4: Commit**

```bash
git add -A docs/ scripts/
git commit -m "docs(roadmap): Phase 1 foundation complete; astropy verification dev script"
```

Release (owner runs per standard workflow, not part of this plan): EN release notes → version bump ×5 → ff-merge `0.2.4` to `main` → tag `v0.2.4`.
