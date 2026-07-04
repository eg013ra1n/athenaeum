# Phase 2 Plan A — Master Calibration Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In-app master calibration frame creation: integration engine, Calibration Library root, atomic master registration with provenance + relink, global compute queue (analysis migrated), archive-of-originals, and the Equipment/Coverage UI.

**Architecture:** A new `integration/` engine reads calibration frames band-by-band (never N full frames in RAM), combines them per §9 recipes, and writes BITPIX=-32 masters via the existing `fits_writer` into a designated `calibration_library` scan root. Registration is direct: the just-written file is parsed with the SAME `fits_parser` + inserted with the SAME `db::insert_file`/`insert_frame` helpers the scanner uses, then the SAME `create_master_sets_from_frames` creates the 1:1 master set — equivalence with scanner ingestion by construction. One DB transaction adds `master_provenance`, repoints every `calibration_set_to_frames` link from the raw set to the master, and marks the raw set superseded. Heavy CPU work is admitted through a new FIFO `ComputeQueue` (default 1 concurrent job) that analysis also goes through. Archiving originals reuses the existing archive planner/executor with a calibration-set subject.

**Tech Stack:** Rust (rusqlite, rayon, serde, ts-rs v10), Tauri 2 + Axum (thin wrappers over `core::api`), React 18 + TypeScript + Tailwind, existing `fits_writer`/`fits_parser`/`astroimage` crates.

**Spec:** `docs/superpowers/specs/2026-07-04-phase2-calibration-library-design.md` (approved). Research: `docs/superpowers/research/2026-07-04-calibration-math-research.md`.

## Global Constraints

- Repo: `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum`. Work on branch **`0.2.5`** (created from `main` in Task 1).
- **Raw master darks**: darks/darkflats/bias are combined RAW (no bias pre-subtraction). No dark scaling/optimization anywhere in this plan.
- **Recipes (spec §9)**: bias/dark/darkflat → average, no normalization, no weighting; rejection = Winsorized sigma-clip 3σ/3σ for N ≥ 15, plain median below. Flat → per-frame pre-calibration (darkflat → exposure-matched dark → bias → synthetic constant → none+warning), per-frame multiplicative normalization to the frame's **central-third mean**, rejection = percentile clip (low 0.2 / high 0.02) for N < 15, Winsorized 3σ/3σ for N ≥ 15.
- **f32 output keeps the input ADU scale** (no rescale, no clipping — negatives pass through).
- Master flats stamp `ATH_FNRM` (central-third mean of the final master) as a `Real` card.
- Library layout (fixed v1): `<LibraryRoot>/<INSTRUME sanitized>/<MasterType>/master_<type>[_<filter>]_<exptime>s_<temp>C_g<gain>_bin<binning>_<YYYY-MM-DD>.fits`, collision suffix `_2`, `_3`…
- Exactly ONE scan root may have `kind='calibration_library'` (code-enforced).
- Archive layout: `<archive_root>/Calibration_Archive/<INSTRUME sanitized>/<date_start YYYY-MM-DD>/<zip>`; only superseded sets may be archived this way.
- Compute queue: FIFO, `compute.max_concurrent` setting (default **1**), analysis migrates to it, `analysis-progress`/`analysis-complete` event names and snake_case payloads UNCHANGED.
- All new commands: core handler in `crates/athenaeum-core/src/api/` + thin Tauri wrapper + thin Axum wrapper + ts-rs registry entry (Phase 1 convention, CLAUDE.md add-a-command checklist).
- New event payloads use snake_case field names on the wire (match analysis events precedent).
- Commit messages: conventional commits, NO AI attribution trailers.
- After every task: `cargo test -p athenaeum-core` green, `cargo clippy -p athenaeum-core --no-deps -- -D warnings` clean for touched code. Frontend tasks: `npx tsc --noEmit` green.
- ts files are generated: after Rust model changes run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and commit the regenerated `src/types/*.ts` together with the Rust change.

## File Structure (locked decomposition)

```text
crates/athenaeum-core/src/
  fits_writer/writer.rs            MODIFY  hardening (Task 2)
  fits_writer/card.rs              MODIFY  format_card re-validation (Task 2)
  db/schema.rs                     MODIFY  migrations (Task 3)
  models.rs                        MODIFY  ScanRoot.kind, CalibrationSetDetail.superseded_by_set_id (Tasks 3, 8)
  services/compute_queue.rs        CREATE  ComputeQueue (Task 4)
  services/mod.rs                  MODIFY  compute_queue field (Task 4)
  api/compute.rs                   CREATE  get_compute_queue / cancel_compute_job / set_compute_max_concurrent (Task 5)
  api/analysis.rs                  MODIFY  queue admission (Task 5)
  integration/mod.rs               CREATE  module root + shared types (Task 6)
  integration/banded.rs            CREATE  BandSource: banded FITS reads + scratch fallback (Task 6)
  integration/combine.rs           CREATE  combiners (Task 7)
  integration/engine.rs            CREATE  recipes: bias-like + flat (Task 8)
  api/scan_roots.rs                MODIFY  kind param + single-library enforcement (Task 9)
  db/operations.rs                 MODIFY  scan_roots kind in SELECT/INSERT (Task 9)
  calibration_library/mod.rs       CREATE  module root (Task 10)
  calibration_library/paths.rs     CREATE  master file naming (Task 10)
  calibration_library/headers.rs   CREATE  header consolidation (Task 10)
  calibration_library/register.rs  CREATE  registration + provenance + relink (Task 11)
  db/master_provenance.rs          CREATE  provenance CRUD (Task 11)
  calibration/scan_integration.rs  MODIFY  pub create_master_sets_from_frames (Task 11)
  calibration/configurable_matcher.rs MODIFY superseded exclusion (Task 11)
  api/masters.rs                   CREATE  start/cancel/preview/batch master builds + provenance queries (Task 12, 13)
  archive/path_layout.rs           MODIFY  calibration zip naming (Task 14)
  archive/planner.rs               MODIFY  build_calibration_set_plan (Task 14)
  archive/db.rs                    MODIFY  insert_operation with calibration_set_id (Task 14)
  archive/executor.rs              MODIFY  finalize branch for calibration subject (Task 14)
  archive/models.rs                MODIFY  ArchivePlan.calibration_set_id (Task 14)
  ts_export.rs                     MODIFY  registry additions (Tasks 5, 12)
crates/athenaeum-tauri/src/
  commands/compute.rs              CREATE  wrappers (Task 5)
  commands/masters.rs              CREATE  wrappers (Tasks 12–14)
  lib.rs                           MODIFY  register commands, init queue notifier (Tasks 4, 5, 12)
crates/athenaeum-web/src/
  routes/compute.rs                CREATE  wrappers (Task 5)
  routes/masters.rs                CREATE  wrappers (Tasks 12–14)
  routes/mod.rs + main.rs          MODIFY  register routes, init queue notifier (Tasks 4, 5, 12)
src/
  types/helpers.ts                 MODIFY  master build event interfaces (Task 15)
  hooks/useMasterBuilds.ts         CREATE  build queue hook (Task 15)
  contexts/MasterBuildContext.tsx  CREATE  provider (Task 15)
  components/ComputeQueueIndicator.tsx CREATE sidebar global queue (Task 15)
  components/Layout.tsx            MODIFY  provider + indicator (Task 15)
  components/calibration/CreateMasterDialog.tsx CREATE shared dialog (Task 16)
  components/CalibrationSetTable.tsx MODIFY Create Master action + badges + provenance (Task 16)
  components/calibration/CalibrationTableView.tsx MODIFY Coverage buttons + batch (Task 17)
  pages/Settings.tsx               MODIFY  Calibration Library root section (Task 9)
docs/superpowers/plans/2026-07-02-roadmap.md MODIFY checkboxes (Task 18)
CLAUDE.md                          MODIFY  Phase 2 notes (Task 18)
```

Dependency order: 1 → 2 → 3 → 4 → 5, then 6 → 7 → 8 (engine chain), 9 → 10 → 11 → 12 → 13 → 14 (library chain, needs 3+8), then 15 → 16 → 17 (UI, needs 12–14), 18 last. Tasks 6–8 can proceed in parallel with 9.

---

### Task 1: Branch setup

**Files:** none (git only)

- [ ] **Step 1: Create the version branch**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
git checkout main && git pull
git checkout -b 0.2.5
```

- [ ] **Step 2: Verify clean baseline**

Run: `cargo test -p athenaeum-core 2>&1 | tail -5`
Expected: `test result: ok` (all existing tests pass). Known pre-existing failure exception: `fast_detect_matches_analyze_on_real_data` in rustafits (documented in roadmap Phase-1 follow-ups) — ignore if it fails, it is not ours.

No commit (nothing changed).

---

### Task 2: FITS writer hardening (spec §11 hard gate)

**Files:**
- Modify: `crates/athenaeum-core/src/fits_writer/writer.rs`
- Modify: `crates/athenaeum-core/src/fits_writer/card.rs`

**Interfaces:**
- Consumes: existing `write_fits_f32`, `format_card`, `Card` (fields are `pub`).
- Produces: same public API, hardened. New `FitsWriteError::BadDimensions(String)` variant. Later tasks call `write_fits_f32` for every master/fixture write.

- [ ] **Step 1: Write the failing tests** (append to `writer.rs` — create a `#[cfg(test)] mod tests` if the file has none; check first: `grep -n "mod tests" crates/athenaeum-core/src/fits_writer/writer.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::card::{Card, CardValue};

    #[test]
    fn zero_dimensions_rejected() {
        let r = write_fits_f32_to(std::io::sink(), 0, 10, 1, &[], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadDimensions(_))), "{r:?}");
        let r = write_fits_f32_to(std::io::sink(), 10, 0, 1, &[], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadDimensions(_))), "{r:?}");
    }

    #[test]
    fn dimension_overflow_rejected_not_panicking() {
        // usize::MAX * 3 would overflow the expected-length multiply
        let r = write_fits_f32_to(std::io::sink(), usize::MAX, 2, 1, &[], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadDimensions(_))), "{r:?}");
    }

    #[test]
    fn concurrent_same_target_writers_do_not_collide_on_tmp() {
        // Two threads writing the same path: both must succeed (last rename
        // wins) — with a fixed ".fits.tmp" suffix one thread unlinks the
        // other's tmp and rename fails with NotFound.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.fits");
        let mk = |v: f32| {
            let path = path.clone();
            std::thread::spawn(move || {
                let data = vec![v; 64 * 64];
                for _ in 0..20 {
                    write_fits_f32(&path, 64, 64, 1, &data, &[]).unwrap();
                }
            })
        };
        let (a, b) = (mk(1.0), mk(2.0));
        a.join().unwrap();
        b.join().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn bypassed_card_constructor_still_validated_at_format_time() {
        // Card fields are pub — a caller can build an invalid keyword directly.
        let evil = Card { keyword: "BAD KEY!".into(), value: Some(CardValue::Integer(1)), comment: None, text: None };
        let r = crate::fits_writer::card::format_card(&evil);
        assert!(r.is_err(), "format_card must re-validate keywords: {r:?}");
        let reserved = Card { keyword: "NAXIS1".into(), value: Some(CardValue::Integer(1)), comment: None, text: None };
        assert!(crate::fits_writer::card::format_card(&reserved).is_err());
    }

    #[test]
    fn text_card_with_no_value_is_error_not_panic() {
        // value: None + text: None used to hit `expect("value card")`.
        let broken = Card { keyword: "GAIN".into(), value: None, comment: None, text: None };
        assert!(crate::fits_writer::card::format_card(&broken).is_err());
    }
}
```

Add `tempfile` to dev-dependencies if absent: `grep -n "tempfile" crates/athenaeum-core/Cargo.toml` — it is already a dependency of the archive tests; if missing add `tempfile = "3"` under `[dev-dependencies]`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core fits_writer::writer::tests -- --nocapture`
Expected: FAIL — `BadDimensions` variant doesn't exist (compile error), then after stubbing the variant: overflow test panics on multiply, bypass test returns Ok, text-card test panics.

- [ ] **Step 3: Implement the hardening**

In `card.rs`:

1. Add error variant after `BadChannels(usize)`:

```rust
    BadDimensions(String),
```

and its Display arm:

```rust
            Self::BadDimensions(m) => write!(f, "bad image dimensions: {m}"),
```

2. Make `validate_keyword` reusable at format time — it already exists; call it at the top of `format_card` (structural cards bypass by keyword equality):

```rust
pub fn format_card(card: &Card) -> Result<Vec<[u8; 80]>, FitsWriteError> {
    // Re-validate: Card fields are pub, so constructor-only validation is
    // bypassable. Structural keywords are writer-owned and arrive here via
    // Card::structural — allow exactly those through the reserved check.
    const STRUCTURAL_OK: [&str; 4] = ["SIMPLE", "BITPIX", "NAXIS", "END"];
    let is_structural = STRUCTURAL_OK.contains(&card.keyword.as_str())
        || (card.keyword.starts_with("NAXIS")
            && card.keyword.len() <= 8
            && card.keyword[5..].bytes().all(|b| b.is_ascii_digit()));
    let is_text_kind = card.keyword == "COMMENT" || card.keyword == "HISTORY";
    if !is_structural && !is_text_kind {
        validate_keyword(&card.keyword)?;
    }
    // COMMENT / HISTORY text cards
    if let Some(text) = &card.text {
        ...unchanged...
    }
    let Some(value) = card.value.as_ref() else {
        return Err(FitsWriteError::InvalidKeyword(format!(
            "{}: card has neither value nor text", card.keyword
        )));
    };
    ...rest unchanged, using `value`...
```

(Replace the `let value = card.value.as_ref().expect("value card");` line with the `let Some(value) = … else` form above. Keep everything else in the function byte-identical.)

In `writer.rs`:

3. `validate` gains zero/overflow checks:

```rust
fn validate(width: usize, height: usize, channels: usize, data_len: usize) -> Result<(), FitsWriteError> {
    if channels != 1 && channels != 3 {
        return Err(FitsWriteError::BadChannels(channels));
    }
    if width == 0 || height == 0 {
        return Err(FitsWriteError::BadDimensions(format!("{width}x{height}")));
    }
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels))
        .ok_or_else(|| FitsWriteError::BadDimensions(format!("{width}x{height}x{channels} overflows")))?;
    if data_len != expected {
        return Err(FitsWriteError::DataSizeMismatch { expected, got: data_len });
    }
    Ok(())
}
```

4. Unique temp suffix + durability in `write_fits_f32`:

```rust
    let tmp = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        path.with_extension(format!("fits.tmp.{}.{}", std::process::id(), seq))
    };
    let write_result = (|| -> Result<(), FitsWriteError> {
        let f = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(f);
        write_fits_f32_to(&mut w, width, height, channels, data, cards)?;
        w.flush()?;
        // Power-loss durability: data must be on disk before the rename
        // makes the file visible under its final name.
        w.get_ref().sync_all()?;
        Ok(())
    })();
```

(The unique suffix also fixes the `a.fit`/`a.fits` sibling collision — both used to map to `a.fits.tmp`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core fits_writer -- --nocapture`
Expected: PASS, including all pre-existing fits_writer tests (round-trips, CONTINUE chains).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/fits_writer/
git commit -m "fix(fits_writer): unique tmp suffix + fsync, checked dims, format-time card re-validation"
```

---

### Task 3: Schema migrations — kind, superseded, provenance, archive subject

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs`
- Modify: `crates/athenaeum-core/src/models.rs` (ScanRoot + CalibrationSetDetail fields)
- Modify: `crates/athenaeum-core/src/db/operations.rs:259-312` (get_scan_roots / upsert_scan_root)

**Interfaces:**
- Produces:
  - `scan_roots.kind TEXT NOT NULL DEFAULT 'normal'` — values `'normal' | 'calibration_library'`; `ScanRoot` struct gains `pub kind: String`.
  - `calibration_set.superseded_by_set_id INTEGER` (nullable) — `CalibrationSetDetail` gains `pub superseded_by_set_id: Option<i64>`.
  - Table `master_provenance(master_set_id PK, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)`.
  - `archive_operations` rebuilt: `frames_set_id` becomes NULLABLE, new nullable `calibration_set_id` column (SQLite cannot drop NOT NULL via ALTER — 12-step rebuild).
- Consumed by: Tasks 9, 11, 12, 14.

- [ ] **Step 1: Write the failing schema tests** (append to the existing `#[cfg(test)]` module at the bottom of `schema.rs` — it already contains archive schema tests; follow their in-memory-Connection style)

```rust
    #[test]
    fn scan_roots_kind_column_and_default() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/a')", []).unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM scan_roots WHERE path='/data/a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "normal");
    }

    #[test]
    fn scan_roots_kind_migrates_existing_table() {
        let conn = Connection::open_in_memory().unwrap();
        // Simulate a legacy scan_roots without kind
        conn.execute(
            "CREATE TABLE scan_roots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                enabled INTEGER NOT NULL DEFAULT 1,
                find_duplicates INTEGER NOT NULL DEFAULT 1,
                unique_camera INTEGER NOT NULL DEFAULT 0,
                last_scan TEXT
            )",
            [],
        ).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/old')", []).unwrap();
        init_db(&conn).unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM scan_roots WHERE path='/old'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "normal");
    }

    #[test]
    fn calibration_set_superseded_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')", [],
        ).unwrap();
        let v: Option<i64> = conn
            .query_row("SELECT superseded_by_set_id FROM calibration_set WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn master_provenance_table_exists() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library) VALUES ('MasterDark','2026-01-01',1)", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO master_provenance (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (1, NULL, '{}', '[]', 'abc', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM master_provenance", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn archive_operations_accepts_calibration_subject() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // NULL frames_set_id + calibration_set_id must be insertable
        conn.execute(
            "INSERT INTO archive_operations
             (frames_set_id, calibration_set_id, archive_root_path, compression, status, started_at)
             VALUES (NULL, 42, '/arch', 'store', 'planning', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        let (fs, cs): (Option<i64>, Option<i64>) = conn.query_row(
            "SELECT frames_set_id, calibration_set_id FROM archive_operations WHERE id=1",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!((fs, cs), (None, Some(42)));
    }

    #[test]
    fn archive_operations_rebuild_preserves_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO frames_set (name) VALUES ('M31')", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO archive_operations
             (frames_set_id, archive_root_path, compression, status, started_at)
             VALUES (1, '/arch', 'store', 'completed', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        // Re-running init_db (idempotent) must keep the row intact
        init_db(&conn).unwrap();
        let fs: Option<i64> = conn
            .query_row("SELECT frames_set_id FROM archive_operations WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fs, Some(1));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core schema -- --nocapture`
Expected: FAIL — `no such column: kind`, `no such column: superseded_by_set_id`, `no such table: master_provenance`, `NOT NULL constraint failed: archive_operations.frames_set_id`.

- [ ] **Step 3: Implement migrations in `init_db`**

Insert after the archive `files` migrations block (after `schema.rs:1282`, before the `catalog_meta` section at `:1284`):

```rust
    // ---- Phase 2: calibration library ----

    // scan_roots.kind: 'normal' | 'calibration_library'
    if !column_exists(conn, "scan_roots", "kind")? {
        conn.execute(
            "ALTER TABLE scan_roots ADD COLUMN kind TEXT NOT NULL DEFAULT 'normal'",
            [],
        )?;
    }

    // calibration_set.superseded_by_set_id: set once a master replaced this raw set
    if !column_exists(conn, "calibration_set", "superseded_by_set_id")? {
        conn.execute(
            "ALTER TABLE calibration_set ADD COLUMN superseded_by_set_id INTEGER REFERENCES calibration_set(id)",
            [],
        )?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_calibration_set_superseded ON calibration_set(superseded_by_set_id)",
        [],
    )?;

    // master_provenance: row EXISTS = master built by Athenaeum
    conn.execute(
        "CREATE TABLE IF NOT EXISTS master_provenance (
            master_set_id      INTEGER PRIMARY KEY REFERENCES calibration_set(id) ON DELETE CASCADE,
            source_set_id      INTEGER REFERENCES calibration_set(id),
            recipe_json        TEXT NOT NULL,
            member_frame_uuids TEXT NOT NULL,
            member_hash        TEXT NOT NULL,
            created_at         TEXT NOT NULL
        )",
        [],
    )?;

    // archive_operations: frames_set_id nullable + calibration_set_id.
    // SQLite can't drop NOT NULL via ALTER — rebuild once, detected by the
    // absence of the calibration_set_id column.
    if !column_exists(conn, "archive_operations", "calibration_set_id")? {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE archive_operations_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                frames_set_id INTEGER,
                calibration_set_id INTEGER,
                archive_root_path TEXT NOT NULL,
                flats_disposition TEXT,
                darks_disposition TEXT,
                bias_disposition TEXT,
                darkflats_disposition TEXT,
                compression TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                error_message TEXT,
                FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE,
                FOREIGN KEY (calibration_set_id) REFERENCES calibration_set(id) ON DELETE CASCADE
             );
             INSERT INTO archive_operations_new
                (id, frames_set_id, archive_root_path, flats_disposition, darks_disposition,
                 bias_disposition, darkflats_disposition, compression, status, started_at,
                 finished_at, error_message)
                SELECT id, frames_set_id, archive_root_path, flats_disposition, darks_disposition,
                       bias_disposition, darkflats_disposition, compression, status, started_at,
                       finished_at, error_message
                FROM archive_operations;
             DROP TABLE archive_operations;
             ALTER TABLE archive_operations_new RENAME TO archive_operations;
             COMMIT;",
        )?;
        // Recreate the two indexes the rebuild dropped
        conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_status ON archive_operations(status)", [])?;
        conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_frames_set ON archive_operations(frames_set_id)", [])?;
        conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_calibration_set ON archive_operations(calibration_set_id)", [])?;
    }
```

IMPORTANT: also update the CREATE TABLE at `schema.rs:391-408` (fresh DBs) to the new shape — `frames_set_id INTEGER` (no NOT NULL), add `calibration_set_id INTEGER` + its FK, so fresh databases skip the rebuild (the guard column exists from birth). Add the `idx_archive_ops_calibration_set` index next to the existing archive indexes (`schema.rs:629-636`).

- [ ] **Step 4: Update the Rust models + scan-root queries**

`models.rs:152-161` — add field to `ScanRoot` (last position, additive):

```rust
pub struct ScanRoot {
    pub id: Option<i64>,
    pub path: String,
    pub enabled: bool,
    pub find_duplicates: bool,
    pub unique_camera: bool,
    pub last_scan: Option<DateTime<Utc>>,
    pub last_scan_errors: Option<Vec<String>>,
    pub monitor_enabled: bool,
    /// 'normal' | 'calibration_library'
    pub kind: String,
}
```

`models.rs:342-369` — add to `CalibrationSetDetail` after `updated_at`:

```rust
    /// Set when this raw set has been superseded by a built master.
    pub superseded_by_set_id: Option<i64>,
```

`db/operations.rs:259-286` (`get_scan_roots`) — add `kind` to the SELECT list and mapper:

```rust
        "SELECT id, path, enabled, find_duplicates, unique_camera, last_scan, last_scan_errors, monitor_enabled, kind FROM scan_roots ORDER BY path"
```
```rust
            monitor_enabled: row.get::<_, i32>(7)? == 1,
            kind: row.get(8)?,
```

Fix every other `ScanRoot { … }` construction site — find them: `grep -rn "ScanRoot {" crates/ | grep -v test`. Known: `api/scan_roots.rs:160-169` (add `kind: "normal".into()` for now; Task 9 threads the real value). Compile errors will surface the full list — fix each with `kind: "normal".into()` unless it reads from the DB.

`CalibrationSetDetail { … }` construction sites — find them: `grep -rn "CalibrationSetDetail {" crates/athenaeum-core/src/ | grep -v test`. For each, extend the underlying SELECT with `superseded_by_set_id` and map it; where the query doesn't read from `calibration_set` directly, use `None`. (Sites live in `calibration/mod.rs` — the dark/master library builders — and `db/equipment.rs`.)

- [ ] **Step 5: Regenerate ts types**

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`
Expected: rewrites `src/types/models.ts` with `kind: string` on `ScanRoot` and `superseded_by_set_id: number | null` on `CalibrationSetDetail`. Then `npx tsc --noEmit` — expected PASS (fields are additive).

- [ ] **Step 6: Run all tests**

Run: `cargo test -p athenaeum-core`
Expected: PASS including the 6 new schema tests and all pre-existing archive tests (the rebuild preserves behavior).

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/models.rs \
        crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/api/scan_roots.rs \
        crates/athenaeum-core/src/calibration/mod.rs crates/athenaeum-core/src/db/equipment.rs \
        src/types/models.ts
git commit -m "feat(db): scan_roots.kind, calibration_set.superseded_by_set_id, master_provenance, archive calibration subject"
```

---

### Task 4: ComputeQueue core

**Files:**
- Create: `crates/athenaeum-core/src/services/compute_queue.rs`
- Modify: `crates/athenaeum-core/src/services/mod.rs`
- Modify: `crates/athenaeum-tauri/src/lib.rs` (context construction + notifier)
- Modify: `crates/athenaeum-web/src/main.rs` (context construction + notifier)

**Interfaces:**
- Produces (consumed by Tasks 5 and 12):

```rust
pub enum ComputeJobKind { Analysis, MasterBuild, LightCalibration }   // serde snake_case
pub enum ComputeJobState { Queued, Running }                          // serde snake_case
pub struct ComputeQueueEntry {                                        // serde camelCase, ts_rs::TS
    pub job_id: i64, pub kind: ComputeJobKind, pub label: String,
    pub state: ComputeJobState, pub queued_at: String,
}
pub struct QueueCancelled;                                            // error: cancelled while queued

impl ComputeQueue {
    pub fn new() -> Self;
    pub fn set_max_concurrent(&self, n: usize);                       // clamps to >= 1
    pub fn max_concurrent(&self) -> usize;
    /// Register the transport notifier called with a fresh snapshot on every
    /// queue transition (enqueue/admit/finish/cancel). One per process.
    pub fn set_notifier(&self, f: Box<dyn Fn(Vec<ComputeQueueEntry>) + Send + Sync>);
    /// FIFO admission. Blocks the calling thread until a slot is free AND all
    /// earlier tickets have been admitted. Returns Err(QueueCancelled) if
    /// `cancel_flag` becomes true while waiting. The permit frees the slot on
    /// Drop. Returns (permit, job_id).
    pub fn acquire(&self, kind: ComputeJobKind, label: &str, cancel_flag: Arc<AtomicBool>)
        -> Result<(ComputePermit, i64), QueueCancelled>;
    pub fn snapshot(&self) -> Vec<ComputeQueueEntry>;
    /// Sets the job's cancel flag (queued or running). Returns false if unknown id.
    pub fn cancel(&self, job_id: i64) -> bool;
}
```

Design notes locked in: jobs run on the CALLER's thread (analysis already runs inside each wrapper's `spawn_blocking`; master builds spawn a `std::thread` in Task 12) — the queue is an admission controller + registry, it never owns closures, so no `Send` bounds on job bodies and no second DB-connection dance.

- [ ] **Step 1: Write the failing tests** (bottom of the new `compute_queue.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn flag() -> Arc<AtomicBool> { Arc::new(AtomicBool::new(false)) }

    #[test]
    fn fifo_one_at_a_time() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(1);
        let running = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let mut handles = Vec::new();
        for i in 0..5 {
            let (q, running, max_seen, order) = (q.clone(), running.clone(), max_seen.clone(), order.clone());
            handles.push(std::thread::spawn(move || {
                // stagger enqueue so ticket order is deterministic
                std::thread::sleep(Duration::from_millis(i as u64 * 30));
                let (_permit, _id) = q.acquire(ComputeJobKind::Analysis, &format!("job{i}"), flag()).unwrap();
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                order.lock().unwrap().push(i);
                std::thread::sleep(Duration::from_millis(50));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles { h.join().unwrap(); }
        assert_eq!(max_seen.load(Ordering::SeqCst), 1, "only one job may run");
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3, 4], "FIFO admission");
    }

    #[test]
    fn cancel_while_queued_returns_err_and_frees_ticket() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(1);
        let hold = flag();
        let q2 = q.clone();
        let hold2 = hold.clone();
        let first = std::thread::spawn(move || {
            let (_p, _id) = q2.acquire(ComputeJobKind::Analysis, "long", hold2).unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        std::thread::sleep(Duration::from_millis(50));
        // Second job queues behind first; cancel it while it waits.
        let cancelled = flag();
        let q3 = q.clone();
        let c2 = cancelled.clone();
        let second = std::thread::spawn(move || q3.acquire(ComputeJobKind::MasterBuild, "victim", c2));
        std::thread::sleep(Duration::from_millis(50));
        let snap = q.snapshot();
        let victim = snap.iter().find(|e| e.label == "victim").expect("queued entry visible");
        assert!(matches!(victim.state, ComputeJobState::Queued));
        assert!(q.cancel(victim.job_id));
        let res = second.join().unwrap();
        assert!(res.is_err(), "cancelled-in-queue must not be admitted");
        first.join().unwrap();
        assert!(q.snapshot().is_empty(), "registry drained");
    }

    #[test]
    fn concurrency_two_admits_two() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(2);
        let running = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let (q, running, max_seen) = (q.clone(), running.clone(), max_seen.clone());
            handles.push(std::thread::spawn(move || {
                let (_p, _id) = q.acquire(ComputeJobKind::Analysis, "j", flag()).unwrap();
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(60));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles { h.join().unwrap(); }
        assert_eq!(max_seen.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn notifier_fires_on_transitions() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        q.set_notifier(Box::new(move |_snap| { c2.fetch_add(1, Ordering::SeqCst); }));
        let (p, _id) = q.acquire(ComputeJobKind::Analysis, "n", flag()).unwrap();
        drop(p);
        // enqueue -> running -> finished: at least 2 notifications
        assert!(calls.load(Ordering::SeqCst) >= 2);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core compute_queue`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement `services/compute_queue.rs`**

```rust
//! Global FIFO admission queue for heavy CPU jobs (analysis, master builds,
//! light calibration). One heavy job at a time by default
//! (`compute.max_concurrent`), so an analysis started in another tab queues
//! behind a running master build instead of fighting it for the rayon pool.
//!
//! The queue is an admission controller, not a job runner: `acquire()` blocks
//! the calling thread (each caller already sits on its own
//! `spawn_blocking`/`std::thread`) until a slot frees AND every
//! earlier-enqueued ticket has been admitted. The returned permit frees the
//! slot on Drop. Cancellation of a QUEUED job flips the same cancel flag the
//! running job would poll; the waiting `acquire` sees it and returns
//! `Err(QueueCancelled)` without ever running.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ComputeJobKind {
    Analysis,
    MasterBuild,
    LightCalibration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ComputeJobState {
    Queued,
    Running,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputeQueueEntry {
    pub job_id: i64,
    pub kind: ComputeJobKind,
    pub label: String,
    pub state: ComputeJobState,
    pub queued_at: String,
}

/// Job was cancelled while still waiting in the queue.
#[derive(Debug)]
pub struct QueueCancelled;

struct JobSlot {
    entry: ComputeQueueEntry,
    cancel_flag: Arc<AtomicBool>,
}

struct Inner {
    /// Tickets in FIFO order. Front = next to admit. Running jobs are NOT in
    /// this deque — they live only in `registry`.
    waiting: Mutex<VecDeque<i64>>,
    registry: Mutex<Vec<JobSlot>>,
    running_count: AtomicUsize,
    max_concurrent: AtomicUsize,
    next_id: AtomicI64,
    cv: Condvar,
    /// Guards cv waits; the actual state lives in waiting/registry/counters.
    gate: Mutex<()>,
    notifier: Mutex<Option<Box<dyn Fn(Vec<ComputeQueueEntry>) + Send + Sync>>>,
}

#[derive(Clone)]
pub struct ComputeQueue {
    inner: Arc<Inner>,
}

impl ComputeQueue {
    pub fn new() -> Self {
        ComputeQueue {
            inner: Arc::new(Inner {
                waiting: Mutex::new(VecDeque::new()),
                registry: Mutex::new(Vec::new()),
                running_count: AtomicUsize::new(0),
                max_concurrent: AtomicUsize::new(1),
                next_id: AtomicI64::new(1),
                cv: Condvar::new(),
                gate: Mutex::new(()),
                notifier: Mutex::new(None),
            }),
        }
    }

    pub fn set_max_concurrent(&self, n: usize) {
        self.inner.max_concurrent.store(n.max(1), Ordering::SeqCst);
        self.inner.cv.notify_all();
    }

    pub fn max_concurrent(&self) -> usize {
        self.inner.max_concurrent.load(Ordering::SeqCst)
    }

    pub fn set_notifier(&self, f: Box<dyn Fn(Vec<ComputeQueueEntry>) + Send + Sync>) {
        *self.inner.notifier.lock().unwrap() = Some(f);
    }

    fn notify(&self) {
        let snap = self.snapshot();
        if let Some(f) = self.inner.notifier.lock().unwrap().as_ref() {
            f(snap);
        }
    }

    pub fn snapshot(&self) -> Vec<ComputeQueueEntry> {
        self.inner.registry.lock().unwrap().iter().map(|s| s.entry.clone()).collect()
    }

    pub fn cancel(&self, job_id: i64) -> bool {
        let registry = self.inner.registry.lock().unwrap();
        match registry.iter().find(|s| s.entry.job_id == job_id) {
            Some(slot) => {
                slot.cancel_flag.store(true, Ordering::SeqCst);
                drop(registry);
                self.inner.cv.notify_all();
                true
            }
            None => false,
        }
    }

    pub fn acquire(
        &self,
        kind: ComputeJobKind,
        label: &str,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<(ComputePermit, i64), QueueCancelled> {
        let job_id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut waiting = self.inner.waiting.lock().unwrap();
            let mut registry = self.inner.registry.lock().unwrap();
            waiting.push_back(job_id);
            registry.push(JobSlot {
                entry: ComputeQueueEntry {
                    job_id,
                    kind,
                    label: label.to_string(),
                    state: ComputeJobState::Queued,
                    queued_at: chrono::Utc::now().to_rfc3339(),
                },
                cancel_flag: cancel_flag.clone(),
            });
        }
        self.notify();

        // Wait until: cancelled, or (front of queue AND slot free).
        let mut gate = self.inner.gate.lock().unwrap();
        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                let mut waiting = self.inner.waiting.lock().unwrap();
                waiting.retain(|&id| id != job_id);
                let mut registry = self.inner.registry.lock().unwrap();
                registry.retain(|s| s.entry.job_id != job_id);
                drop(registry);
                drop(waiting);
                drop(gate);
                self.inner.cv.notify_all();
                self.notify();
                return Err(QueueCancelled);
            }
            let is_front = {
                let waiting = self.inner.waiting.lock().unwrap();
                waiting.front() == Some(&job_id)
            };
            let free = self.inner.running_count.load(Ordering::SeqCst)
                < self.inner.max_concurrent.load(Ordering::SeqCst);
            if is_front && free {
                break;
            }
            let (g, _timeout) = self
                .inner
                .cv
                .wait_timeout(gate, std::time::Duration::from_millis(200))
                .unwrap();
            gate = g;
        }

        // Admit: pop ticket, bump running, flip registry state.
        {
            let mut waiting = self.inner.waiting.lock().unwrap();
            waiting.pop_front();
        }
        self.inner.running_count.fetch_add(1, Ordering::SeqCst);
        {
            let mut registry = self.inner.registry.lock().unwrap();
            if let Some(slot) = registry.iter_mut().find(|s| s.entry.job_id == job_id) {
                slot.entry.state = ComputeJobState::Running;
            }
        }
        drop(gate);
        self.inner.cv.notify_all();
        self.notify();

        Ok((ComputePermit { queue: self.clone(), job_id }, job_id))
    }
}

impl Default for ComputeQueue {
    fn default() -> Self { Self::new() }
}

/// RAII slot: releasing (Drop) frees the concurrency slot, removes the
/// registry entry, wakes waiters, and notifies the transport.
pub struct ComputePermit {
    queue: ComputeQueue,
    job_id: i64,
}

impl Drop for ComputePermit {
    fn drop(&mut self) {
        self.queue.inner.running_count.fetch_sub(1, Ordering::SeqCst);
        {
            let mut registry = self.queue.inner.registry.lock().unwrap();
            registry.retain(|s| s.entry.job_id != self.job_id);
        }
        self.queue.inner.cv.notify_all();
        self.queue.notify();
    }
}
```

(The 200 ms `wait_timeout` makes the cancelled-while-queued path robust without a dedicated wake-token per ticket; every transition also `notify_all`s so the common path never waits the full timeout.)

- [ ] **Step 4: Wire into `ServiceContext`**

`services/mod.rs`: add `pub mod compute_queue;` next to `pub mod operation_queue;` and the field at the end of `ServiceContext` (after `operation_queue`):

```rust
    /// Global FIFO admission queue for heavy CPU jobs (analysis, master
    /// builds). See compute_queue module docs.
    pub compute_queue: compute_queue::ComputeQueue,
```

Construction sites (find with `grep -rn "operation_queue: " crates/athenaeum-tauri/src/lib.rs crates/athenaeum-web/src/main.rs`): add

```rust
        compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
```

In both backends, immediately after the context is built and the settings DB is available, initialize `max_concurrent` from settings (key added in Task 5's settings step; until then leave the default 1 — this line lands in Task 5).

- [ ] **Step 5: Run tests**

Run: `cargo test -p athenaeum-core compute_queue && cargo build -p athenaeum-tauri -p athenaeum-web`
Expected: 4 tests PASS; both backends compile.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/services/ crates/athenaeum-tauri/src/lib.rs crates/athenaeum-web/src/main.rs
git commit -m "feat(core): global ComputeQueue — FIFO admission for heavy CPU jobs"
```

---

### Task 5: Analysis onto the queue + inspection API + notifier wiring

**Files:**
- Modify: `crates/athenaeum-core/src/api/analysis.rs:151-232` (admission)
- Create: `crates/athenaeum-core/src/api/compute.rs`
- Modify: `crates/athenaeum-core/src/api/mod.rs` (`pub mod compute;`)
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (key + default)
- Create: `crates/athenaeum-tauri/src/commands/compute.rs`; register in `crates/athenaeum-tauri/src/lib.rs`
- Create: `crates/athenaeum-web/src/routes/compute.rs`; register in `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `crates/athenaeum-core/src/ts_export.rs` (registry)

**Interfaces:**
- Produces:
  - `api::compute::get_compute_queue(ctx) -> Vec<ComputeQueueEntry>`
  - `api::compute::cancel_compute_job(ctx, job_id: i64) -> Result<(), ApiError>` (NotFound if unknown)
  - `api::compute::set_compute_max_concurrent(ctx, n: usize) -> Result<(), ApiError>` (persists setting + applies live)
  - New event `compute-queue-changed` with payload `{ "entries": [ComputeQueueEntry…] }` (camelCase inside entries — matches the ts type), emitted via each transport's notifier.
  - Settings key `keys::COMPUTE_MAX_CONCURRENT = "compute.max_concurrent"`, default `"1"`.
- Consumes: Task 4's `ComputeQueue`.
- Analysis behavior contract (regression-pinned): event names/payloads unchanged; second concurrent analysis of the SAME set still returns Conflict; analyses of DIFFERENT sets now serialize instead of running concurrently.

- [ ] **Step 1: Write the failing core test** (append to `api/analysis.rs` tests or create `#[cfg(test)]` there; a full ServiceContext is heavy to fake — instead put the admission test in `compute_queue.rs`-style form inside `api/compute.rs` tests, and pin analysis behavior via the queue itself)

In `api/compute.rs` (new file), bottom:

```rust
#[cfg(test)]
mod tests {
    // The api handlers are thin; what needs pinning is the settings key
    // default and the NotFound classification.
    #[test]
    fn default_max_concurrent_is_one() {
        assert_eq!(crate::settings::defaults::COMPUTE_MAX_CONCURRENT, "1");
    }
}
```

- [ ] **Step 2: Settings key**

`settings/mod.rs` — in the `defaults` module (near `ARCHIVE_COMPRESSION` at `:42`):

```rust
    pub const COMPUTE_MAX_CONCURRENT: &str = "1";
```

In the `keys` module (near `:76`):

```rust
    pub const COMPUTE_MAX_CONCURRENT: &str = "compute.max_concurrent";
```

- [ ] **Step 3: Implement `api/compute.rs`**

```rust
//! Compute-queue inspection/control handlers (Tauri + web wrappers are thin).

use crate::api::{db, ApiError};
use crate::services::compute_queue::ComputeQueueEntry;
use crate::services::ServiceContext;
use crate::settings::keys;

pub fn get_compute_queue(ctx: &ServiceContext) -> Vec<ComputeQueueEntry> {
    ctx.compute_queue.snapshot()
}

pub fn cancel_compute_job(ctx: &ServiceContext, job_id: i64) -> Result<(), ApiError> {
    if ctx.compute_queue.cancel(job_id) {
        Ok(())
    } else {
        Err(ApiError::NotFound(format!("no compute job with id {job_id}")))
    }
}

pub fn set_compute_max_concurrent(ctx: &ServiceContext, n: usize) -> Result<(), ApiError> {
    if n == 0 || n > 8 {
        return Err(ApiError::Invalid("compute.max_concurrent must be 1..=8".into()));
    }
    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings
        .persist_setting(&conn, keys::COMPUTE_MAX_CONCURRENT, &n.to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    ctx.compute_queue.set_max_concurrent(n);
    Ok(())
}
```

Add `pub mod compute;` to `api/mod.rs` (after `pub mod analysis;`).

- [ ] **Step 4: Queue admission in `analyze_frame_set`**

In `api/analysis.rs`, immediately after the `active_analyses.insert(...)` block (`:174-179`) and BEFORE the config/frame-list loading (`:182`), insert:

```rust
    // Global compute-queue admission: heavy jobs run one-at-a-time (default)
    // across analysis/master-build/light-calibration. Blocks this (already
    // spawn_blocking) thread until admitted; a cancel while queued surfaces
    // as a normal cancelled result, mirroring a cancel during the run.
    let queue_permit = match ctx.compute_queue.acquire(
        crate::services::compute_queue::ComputeJobKind::Analysis,
        &format!("Analysis: frame set {frame_set_id}"),
        cancel_flag.clone(),
    ) {
        Ok((permit, _job_id)) => permit,
        Err(_cancelled) => {
            let mut analyses = ctx.active_analyses.lock().unwrap();
            analyses.remove(&frame_set_id);
            drop(analyses);
            emit_event(emitter, "analysis-complete", &AnalysisCompleteEvent {
                frame_set_id,
                analyzed: 0,
                skipped: 0,
                failed: 0,
                errors: Vec::new(),
                cancelled: true,
            });
            return Ok(AnalyzeFrameSetResult {
                analyzed: 0, skipped: 0, failed: 0, errors: Vec::new(), cancelled: true,
            });
        }
    };
```

And just before the final `emit_event(emitter, "analysis-complete", …)` (`:347`), add:

```rust
    drop(queue_permit);
```

(Explicit drop before the completion event so the next queued job is admitted the moment CPU work ends, not after event serialization.)

- [ ] **Step 5: Transport wrappers + notifier**

`crates/athenaeum-tauri/src/commands/compute.rs` (mirror an existing thin command file, e.g. the shape of `commands/analysis.rs` config commands):

```rust
use tauri::State;
use crate::AppState;
use athenaeum_core::api;
use athenaeum_core::services::compute_queue::ComputeQueueEntry;

#[tauri::command]
#[tracing::instrument(skip_all)]
pub async fn get_compute_queue(state: State<'_, AppState>) -> Result<Vec<ComputeQueueEntry>, String> {
    Ok(api::compute::get_compute_queue(&state.ctx))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_compute_job(state: State<'_, AppState>, jobId: i64) -> Result<(), String> {
    api::compute::cancel_compute_job(&state.ctx, jobId).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_compute_max_concurrent(state: State<'_, AppState>, n: usize) -> Result<(), String> {
    api::compute::set_compute_max_concurrent(&state.ctx, n).map_err(|e| e.to_string())
}
```

NOTE: match the parameter-naming convention of neighboring commands — check `grep -n "frameSetId\|frame_set_id" crates/athenaeum-tauri/src/commands/analysis.rs` and use the same style (Tauri v2 camelCases args from JS; existing commands take snake_case Rust params that Tauri maps — follow what `cancel_analysis` does, e.g. `frame_set_id: i64`, and name ours `job_id: i64`).

Register the three commands in the `tauri::generate_handler![…]` list in `crates/athenaeum-tauri/src/lib.rs` and add `pub mod compute;` to `commands/mod.rs`.

Notifier (Tauri) — in `lib.rs` `setup` closure where the `AppHandle` is available (same place other startup wiring lives):

```rust
        let emitter_handle = app.handle().clone();
        ctx.compute_queue.set_notifier(Box::new(move |entries| {
            use tauri::Emitter;
            let _ = emitter_handle.emit("compute-queue-changed", serde_json::json!({ "entries": entries }));
        }));
        // Apply persisted max_concurrent
        if let Some(db) = ctx.db.get() {
            let conn = db.conn();
            if let Ok(v) = ctx.settings.get_setting_or(&conn,
                athenaeum_core::settings::keys::COMPUTE_MAX_CONCURRENT,
                athenaeum_core::settings::defaults::COMPUTE_MAX_CONCURRENT)
            {
                if let Ok(n) = v.parse::<usize>() { ctx.compute_queue.set_max_concurrent(n); }
            }
        }
```

(Adapt the settings-read call to the actual `SettingsManager` getter — check `grep -n "pub fn get_setting" crates/athenaeum-core/src/settings/mod.rs` and use the existing method; if only `get_setting(conn, key) -> Option<String>` exists, use `.unwrap_or_else(|| defaults::COMPUTE_MAX_CONCURRENT.into())`.)

Web mirror `crates/athenaeum-web/src/routes/compute.rs`: three Axum handlers with the `impl From<ApiError>` mapping used across routes; notifier in `main.rs` after the SSE broadcast channel exists:

```rust
    let sse_emitter = SseProgressEmitter { tx: sse_tx.clone() };
    ctx.compute_queue.set_notifier(Box::new(move |entries| {
        athenaeum_core::events::emit_event(&sse_emitter, "compute-queue-changed",
            &serde_json::json!({ "entries": entries }));
    }));
```

Routes: `GET /api/compute/queue`, `POST /api/compute/cancel/{job_id}`, `POST /api/compute/max-concurrent` — register in `routes/mod.rs` following the archive routes' registration pattern.

- [ ] **Step 6: ts registry + regenerate**

`ts_export.rs` models.ts block — append after the logging entries (`:128`):

```rust
            crate::services::compute_queue::ComputeJobKind,
            crate::services::compute_queue::ComputeJobState,
            crate::services::compute_queue::ComputeQueueEntry,
```

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` then `npx tsc --noEmit`.

- [ ] **Step 7: Full test pass + manual smoke**

Run: `cargo test -p athenaeum-core && cargo build -p athenaeum-tauri -p athenaeum-web`
Expected: PASS.

Manual smoke (dev app): start two analyses on two different frame sets from the Objects page → the second must sit queued (no CPU) until the first completes; `analysis-progress` events still drive the existing sidebar indicator; cancel from the indicator still works.

- [ ] **Step 8: Commit**

```bash
git add crates/ src/types/models.ts
git commit -m "feat(compute): analysis admitted through global compute queue; queue inspection API + events"
```

---

### Task 6: Integration module — banded reader + scratch fallback

**Files:**
- Create: `crates/athenaeum-core/src/integration/mod.rs`
- Create: `crates/athenaeum-core/src/integration/banded.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod integration;` — check the module list with `grep -n "pub mod" crates/athenaeum-core/src/lib.rs`)

**Interfaces:**
- Produces (consumed by Tasks 7–8):

```rust
// integration/mod.rs
pub mod banded;
pub mod combine;   // Task 7
pub mod engine;    // Task 8

#[derive(Debug)]
pub enum IntegrationError {
    Io(std::io::Error),
    BadInput(String),      // dim mismatch, unsupported layout, empty set
    Decode(String),        // read_raw / header parse failure
    Cancelled,
}
// impl Display + Error + From<std::io::Error>
```

```rust
// integration/banded.rs
pub struct BandSource { /* private */ }
impl BandSource {
    /// Opens every path. Plain single-HDU uncompressed FITS (BITPIX 16/-32/32/-64/8,
    /// NAXIS==2) get a direct seek-read plan; anything else (XISF, RGB FITS,
    /// nonstandard) is decoded once via astroimage::ImageConverter::read_raw and
    /// spilled to a raw little-endian f32 scratch file in `scratch_dir`.
    /// Errors if frames disagree on (width, height).
    pub fn open(paths: &[std::path::PathBuf], scratch_dir: &std::path::Path)
        -> Result<BandSource, IntegrationError>;
    pub fn width(&self) -> usize;
    pub fn height(&self) -> usize;
    pub fn frame_count(&self) -> usize;
    /// Reads rows [y0, y0+rows) of every frame into out[i] (len = rows*width),
    /// BZERO/BSCALE applied, native f32, NO stretch, CFA untouched.
    pub fn read_band(&mut self, y0: usize, rows: usize, out: &mut [Vec<f32>])
        -> Result<(), IntegrationError>;
}
/// Band height so that (frame_count+2) * band_rows * width * 4 bytes stays
/// under budget_bytes (default caller passes 256 MiB), min 16 rows.
pub fn band_rows_for_budget(width: usize, frame_count: usize, budget_bytes: usize) -> usize;
```

FITS direct-path facts (for the implementer): a conforming primary HDU is `N × 2880`-byte header blocks (80-byte cards, terminated by a card whose first 8 bytes are `END     `), then data. Data offset = number-of-header-blocks × 2880. Row `y` of a BITPIX `b` image starts at `offset + y * width * (|b|/8)`. Values are big-endian; u16 path applies `BZERO`(default 0)/`BSCALE`(default 1) — mirror the semantics of `rustafits/src/formats/fits.rs:175-305` (that code is the reference for scaling; do NOT import it, it decodes whole images).

- [ ] **Step 1: Write the failing tests** (bottom of `banded.rs`; fixtures are generated with our own `fits_writer` — f32 path — plus a hand-rolled BITPIX=16 writer helper inside the test module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{write_fits_f32, Card};
    use std::io::Write;

    fn f32_fixture(dir: &std::path::Path, name: &str, w: usize, h: usize, fill: impl Fn(usize, usize) -> f32) -> std::path::PathBuf {
        let mut data = vec![0f32; w * h];
        for y in 0..h { for x in 0..w { data[y * w + x] = fill(x, y); } }
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &data, &[]).unwrap();
        p
    }

    /// Minimal BITPIX=16 writer (unsigned convention: BZERO=32768, BSCALE=1)
    /// so the u16 fast path is covered without real camera files.
    fn u16_fixture(dir: &std::path::Path, name: &str, w: usize, h: usize, val: u16) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut header = Vec::new();
        for line in [
            format!("{:<80}", "SIMPLE  =                    T"),
            format!("{:<80}", "BITPIX  =                   16"),
            format!("{:<80}", "NAXIS   =                    2"),
            format!("{:<80}", format!("NAXIS1  = {:>20}", w)),
            format!("{:<80}", format!("NAXIS2  = {:>20}", h)),
            format!("{:<80}", "BZERO   =              32768.0"),
            format!("{:<80}", "BSCALE  =                  1.0"),
            format!("{:<80}", "END"),
        ] { header.extend_from_slice(line.as_bytes()); }
        header.resize(2880, b' ');
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&header).unwrap();
        let raw: i16 = (val as i32 - 32768) as i16; // stored = (phys - BZERO)/BSCALE
        let mut data = Vec::with_capacity(w * h * 2);
        for _ in 0..w * h { data.extend_from_slice(&raw.to_be_bytes()); }
        let pad = (2880 - data.len() % 2880) % 2880;
        data.extend(std::iter::repeat(0u8).take(pad));
        f.write_all(&data).unwrap();
        p
    }

    #[test]
    fn reads_f32_fits_bands_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |x, y| (y * 32 + x) as f32);
        let p2 = f32_fixture(dir.path(), "b.fits", 32, 24, |_, _| 7.0);
        let mut src = BandSource::open(&[p1, p2], dir.path()).unwrap();
        assert_eq!((src.width(), src.height(), src.frame_count()), (32, 24, 2));
        let mut out = vec![Vec::new(), Vec::new()];
        src.read_band(10, 4, &mut out).unwrap();
        assert_eq!(out[0].len(), 4 * 32);
        assert_eq!(out[0][0], (10 * 32) as f32);        // row 10, col 0
        assert_eq!(out[0][4 * 32 - 1], (13 * 32 + 31) as f32);
        assert!(out[1].iter().all(|&v| v == 7.0));
    }

    #[test]
    fn u16_bzero_applied() {
        let dir = tempfile::tempdir().unwrap();
        let p = u16_fixture(dir.path(), "d.fits", 16, 8, 1000);
        let mut src = BandSource::open(&[p], dir.path()).unwrap();
        let mut out = vec![Vec::new()];
        src.read_band(0, 8, &mut out).unwrap();
        assert!(out[0].iter().all(|&v| v == 1000.0), "physical = stored*BSCALE + BZERO");
    }

    #[test]
    fn dimension_mismatch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |_, _| 0.0);
        let p2 = f32_fixture(dir.path(), "b.fits", 16, 24, |_, _| 0.0);
        assert!(matches!(BandSource::open(&[p1, p2], dir.path()), Err(IntegrationError::BadInput(_))));
    }

    #[test]
    fn band_rows_budget_math() {
        // 100 frames of width 6248, budget 256 MiB:
        // rows = 256MiB / ((100+2) * 6248 * 4) ≈ 105 — must be >= 16 and <= height cap by caller.
        let rows = band_rows_for_budget(6248, 100, 256 * 1024 * 1024);
        assert!(rows >= 16 && rows <= 256, "{rows}");
        assert_eq!(band_rows_for_budget(10, 1, usize::MAX), usize::MAX.min(band_rows_for_budget(10, 1, usize::MAX))); // no panic on huge budgets
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core integration::banded`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement**

`integration/mod.rs`:

```rust
//! Master-frame integration engine (spec §4): banded streaming reads,
//! per-pixel robust combination, recipe orchestration. Never holds
//! N full frames in RAM — the working set is N × one band.

pub mod banded;
pub mod combine;
pub mod engine;

#[derive(Debug)]
pub enum IntegrationError {
    Io(std::io::Error),
    BadInput(String),
    Decode(String),
    Cancelled,
}

impl std::fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::BadInput(m) => write!(f, "bad input: {m}"),
            Self::Decode(m) => write!(f, "decode: {m}"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}
impl std::error::Error for IntegrationError {}
impl From<std::io::Error> for IntegrationError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}
```

(Until Task 7/8 land, stub `pub mod combine;`/`pub mod engine;` OUT — add them in their own tasks. Only declare `pub mod banded;` here now.)

`integration/banded.rs` core (complete implementation):

```rust
use super::IntegrationError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const BLOCK: u64 = 2880;

enum FrameReader {
    /// Direct seek-read of an uncompressed single-HDU FITS.
    Fits { file: File, data_offset: u64, bitpix: i32, bzero: f64, bscale: f64 },
    /// Raw little-endian f32 scratch spill (one full frame, row-major).
    Scratch { file: File },
}

pub struct BandSource {
    readers: Vec<FrameReader>,
    width: usize,
    height: usize,
}

struct FitsInfo { data_offset: u64, bitpix: i32, naxis: i32, w: usize, h: usize, naxis3: usize, bzero: f64, bscale: f64 }

/// Scan primary-header blocks for END; harvest the handful of numeric cards
/// the direct reader needs. Returns None for anything that should take the
/// decode-and-spill fallback (never errors on odd files — fallback covers them).
fn probe_fits(path: &Path) -> Option<FitsInfo> {
    let mut f = File::open(path).ok()?;
    let mut info = FitsInfo { data_offset: 0, bitpix: 0, naxis: 0, w: 0, h: 0, naxis3: 1, bzero: 0.0, bscale: 1.0 };
    let mut block = [0u8; BLOCK as usize];
    let mut blocks = 0u64;
    'outer: loop {
        f.read_exact(&mut block).ok()?;
        blocks += 1;
        for card in block.chunks(80) {
            let key = std::str::from_utf8(&card[..8]).ok()?.trim_end();
            if key == "END" { break 'outer; }
            let val = || -> Option<f64> {
                let s = std::str::from_utf8(&card[10..]).ok()?;
                let s = s.split('/').next()?.trim();
                s.parse::<f64>().ok()
            };
            match key {
                "BITPIX" => info.bitpix = val()? as i32,
                "NAXIS" => info.naxis = val()? as i32,
                "NAXIS1" => info.w = val()? as usize,
                "NAXIS2" => info.h = val()? as usize,
                "NAXIS3" => info.naxis3 = val()? as usize,
                "BZERO" => info.bzero = val()?,
                "BSCALE" => info.bscale = val()?,
                _ => {}
            }
        }
        if blocks > 64 { return None; } // headers beyond 64 blocks: fall back
    }
    info.data_offset = blocks * BLOCK;
    let ok_bitpix = matches!(info.bitpix, 8 | 16 | 32 | -32 | -64);
    if info.naxis == 2 && info.naxis3 == 1 && ok_bitpix && info.w > 0 && info.h > 0 {
        Some(info)
    } else {
        None
    }
}

fn spill_via_read_raw(path: &Path, scratch_dir: &Path, idx: usize)
    -> Result<(File, usize, usize), IntegrationError>
{
    let (meta, pixels) = astroimage::ImageConverter::read_raw(path)
        .map_err(|e| IntegrationError::Decode(format!("{}: {e:#}", path.display())))?;
    if meta.channels != 1 {
        return Err(IntegrationError::BadInput(format!(
            "{}: {}-channel image — calibration frames must be 1-channel (CFA mosaics included)",
            path.display(), meta.channels
        )));
    }
    let (w, h) = (meta.width as usize, meta.height as usize);
    let scratch_path: PathBuf = scratch_dir.join(format!("athint_scratch_{}_{idx}.f32", std::process::id()));
    {
        let mut out = std::io::BufWriter::new(File::create(&scratch_path)?);
        use std::io::Write;
        match &pixels {
            astroimage::PixelData::Float32(v) => {
                for &x in v { out.write_all(&x.to_le_bytes())?; }
            }
            astroimage::PixelData::Uint16(v) => {
                for &x in v { out.write_all(&(x as f32).to_le_bytes())?; }
            }
        }
        out.flush()?;
    }
    let file = File::open(&scratch_path)?;
    // Unlink immediately: the open handle keeps the data readable (POSIX);
    // on Windows removal is deferred by the OS — acceptable for temp data.
    let _ = std::fs::remove_file(&scratch_path);
    Ok((file, w, h))
}

impl BandSource {
    pub fn open(paths: &[PathBuf], scratch_dir: &Path) -> Result<BandSource, IntegrationError> {
        if paths.is_empty() {
            return Err(IntegrationError::BadInput("empty frame list".into()));
        }
        let mut readers = Vec::with_capacity(paths.len());
        let mut dims: Option<(usize, usize)> = None;
        for (i, p) in paths.iter().enumerate() {
            let (reader, w, h) = match probe_fits(p) {
                Some(info) => (
                    FrameReader::Fits {
                        file: File::open(p)?,
                        data_offset: info.data_offset,
                        bitpix: info.bitpix,
                        bzero: info.bzero,
                        bscale: info.bscale,
                    },
                    info.w, info.h,
                ),
                None => {
                    let (file, w, h) = spill_via_read_raw(p, scratch_dir, i)?;
                    (FrameReader::Scratch { file }, w, h)
                }
            };
            match dims {
                None => dims = Some((w, h)),
                Some(d) if d != (w, h) => {
                    return Err(IntegrationError::BadInput(format!(
                        "dimension mismatch: {} is {w}x{h}, expected {}x{}",
                        p.display(), d.0, d.1
                    )));
                }
                _ => {}
            }
            readers.push(reader);
        }
        let (width, height) = dims.unwrap();
        Ok(BandSource { readers, width, height })
    }

    pub fn width(&self) -> usize { self.width }
    pub fn height(&self) -> usize { self.height }
    pub fn frame_count(&self) -> usize { self.readers.len() }

    pub fn read_band(&mut self, y0: usize, rows: usize, out: &mut [Vec<f32>]) -> Result<(), IntegrationError> {
        assert_eq!(out.len(), self.readers.len());
        let w = self.width;
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!("band {y0}+{rows} beyond height {}", self.height)));
        }
        for (reader, dst) in self.readers.iter_mut().zip(out.iter_mut()) {
            dst.clear();
            dst.reserve(rows * w);
            match reader {
                FrameReader::Fits { file, data_offset, bitpix, bzero, bscale } => {
                    let bpp = (bitpix.unsigned_abs() as usize) / 8;
                    let mut buf = vec![0u8; rows * w * bpp];
                    file.seek(SeekFrom::Start(*data_offset + (y0 * w * bpp) as u64))?;
                    file.read_exact(&mut buf)?;
                    let (bz, bs) = (*bzero as f32, *bscale as f32);
                    match *bitpix {
                        16 => for c in buf.chunks_exact(2) {
                            let raw = i16::from_be_bytes([c[0], c[1]]) as f32;
                            dst.push(raw * bs + bz);
                        },
                        -32 => for c in buf.chunks_exact(4) {
                            let raw = f32::from_be_bytes([c[0], c[1], c[2], c[3]]);
                            dst.push(raw * bs + bz);
                        },
                        32 => for c in buf.chunks_exact(4) {
                            let raw = i32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f32;
                            dst.push(raw * bs + bz);
                        },
                        -64 => for c in buf.chunks_exact(8) {
                            let raw = f64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                            dst.push((raw * *bscale + *bzero) as f32);
                        },
                        8 => for &b in buf.iter() {
                            dst.push(b as f32 * bs + bz);
                        },
                        other => return Err(IntegrationError::BadInput(format!("BITPIX {other}"))),
                    }
                }
                FrameReader::Scratch { file } => {
                    let mut buf = vec![0u8; rows * w * 4];
                    file.seek(SeekFrom::Start((y0 * w * 4) as u64))?;
                    file.read_exact(&mut buf)?;
                    for c in buf.chunks_exact(4) {
                        dst.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn band_rows_for_budget(width: usize, frame_count: usize, budget_bytes: usize) -> usize {
    let per_row = (frame_count + 2).saturating_mul(width).saturating_mul(4).max(1);
    (budget_bytes / per_row).max(16)
}
```

Check the actual `astroimage` re-export names first: `grep -n "pub use\|pub struct ImageConverter\|pub enum PixelData" rustafits/src/lib.rs rustafits/src/types.rs rustafits/src/converter.rs | head` — adjust `astroimage::ImageConverter` / `astroimage::PixelData` paths and the `ImageMetadata.channels/width/height` field types (`u32` vs `usize`) to what the crate actually exports (analyzer usage at `crates/athenaeum-core/src/analysis/analyzer.rs:10` shows the exact import style to copy).

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core integration::banded`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/integration/ crates/athenaeum-core/src/lib.rs
git commit -m "feat(integration): banded FITS reader with decode-and-spill fallback"
```

---

### Task 7: Combiners

**Files:**
- Create: `crates/athenaeum-core/src/integration/combine.rs` (+ add `pub mod combine;` to `integration/mod.rs`)

**Interfaces:**
- Produces (consumed by Task 8):

```rust
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", tag = "method")]
pub enum CombineMethod {
    Mean,
    Median,
    /// Iterative winsorized sigma clipping, then mean of survivors.
    WinsorizedSigmaClip { sigma_low: f64, sigma_high: f64 },
    /// PixInsight-style percentile clipping around the median:
    /// reject x when (m - x) > low*m or (x - m) > high*m.
    PercentileClip { low: f64, high: f64 },
}

/// Combine one pixel column: `values` holds the same pixel from N frames
/// (already normalized/pre-calibrated by the caller). Returns (value, rejected_count).
pub fn combine_pixel(values: &mut [f32], method: CombineMethod) -> (f32, usize);
```

`combine_pixel` may reorder `values` in place (sorting) — callers pass scratch copies.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_and_median_basics() {
        let (v, r) = combine_pixel(&mut [1.0, 2.0, 3.0, 4.0], CombineMethod::Mean);
        assert_eq!((v, r), (2.5, 0));
        let (v, _) = combine_pixel(&mut [5.0, 1.0, 3.0], CombineMethod::Median);
        assert_eq!(v, 3.0);
        let (v, _) = combine_pixel(&mut [4.0, 1.0, 3.0, 2.0], CombineMethod::Median);
        assert_eq!(v, 2.5); // even N: mean of middle two
    }

    #[test]
    fn winsorized_rejects_hot_pixel() {
        // 20 well-behaved samples around 100 + one cosmic-ray 5000.
        let mut vals: Vec<f32> = (0..20).map(|i| 100.0 + (i % 5) as f32).collect();
        vals.push(5000.0);
        let (v, rejected) = combine_pixel(&mut vals, CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 });
        assert!(rejected >= 1, "outlier must be rejected");
        assert!((v - 102.0).abs() < 3.0, "combined value near the clean mean, got {v}");
    }

    #[test]
    fn winsorized_keeps_clean_data_unclipped() {
        let mut vals: Vec<f32> = (0..30).map(|i| 500.0 + (i % 7) as f32).collect();
        let clean_mean = vals.iter().sum::<f32>() / vals.len() as f32;
        let (v, rejected) = combine_pixel(&mut vals, CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 });
        assert_eq!(rejected, 0);
        assert!((v - clean_mean).abs() < 0.5);
    }

    #[test]
    fn percentile_clip_rejects_star_in_sky_flat() {
        // median ~ 10000; star pixel 10900 is +9% > high limit 2%.
        let mut vals = vec![10000.0, 10050.0, 9980.0, 10020.0, 10900.0];
        let (v, rejected) = combine_pixel(&mut vals, CombineMethod::PercentileClip { low: 0.2, high: 0.02 });
        assert_eq!(rejected, 1);
        assert!(v < 10100.0, "{v}");
    }

    #[test]
    fn degenerate_inputs() {
        let (v, r) = combine_pixel(&mut [42.0], CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 });
        assert_eq!((v, r), (42.0, 0));
        let (v, _) = combine_pixel(&mut [], CombineMethod::Mean);
        assert!(v == 0.0);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core integration::combine`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement**

```rust
//! Per-pixel robust combination. Algorithms per spec §9 / research findings
//! 5 & 7: winsorized sigma clip (PixInsight master recipe) and percentile
//! clip (sky flats / small sets), plus plain mean/median.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", tag = "method")]
pub enum CombineMethod {
    Mean,
    Median,
    WinsorizedSigmaClip { sigma_low: f64, sigma_high: f64 },
    PercentileClip { low: f64, high: f64 },
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() { return 0.0; }
    (v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64) as f32
}

fn median_sorted(v: &[f32]) -> f32 {
    let n = v.len();
    if n == 0 { return 0.0; }
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

fn stddev(v: &[f32], m: f64) -> f64 {
    if v.len() < 2 { return 0.0; }
    let var = v.iter().map(|&x| { let d = x as f64 - m; d * d }).sum::<f64>() / (v.len() - 1) as f64;
    var.sqrt()
}

pub fn combine_pixel(values: &mut [f32], method: CombineMethod) -> (f32, usize) {
    match method {
        CombineMethod::Mean => (mean(values), 0),
        CombineMethod::Median => {
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (median_sorted(values), 0)
        }
        CombineMethod::WinsorizedSigmaClip { sigma_low, sigma_high } => {
            let n = values.len();
            if n < 3 { return (mean(values), 0); }
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            // 1) Winsorized estimate of location/scale (Huber-style iteration):
            //    clamp the working copy at m±1.5σ, recompute, repeat to 0.5% change.
            let mut work: Vec<f64> = values.iter().map(|&x| x as f64).collect();
            let mut m = work.iter().sum::<f64>() / n as f64;
            let mut s = {
                let var = work.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n - 1) as f64;
                var.sqrt()
            };
            for _ in 0..10 {
                if s <= f64::EPSILON { break; }
                let (lo, hi) = (m - 1.5 * s, m + 1.5 * s);
                for x in work.iter_mut() { *x = x.clamp(lo, hi); }
                let new_m = work.iter().sum::<f64>() / n as f64;
                let new_s = 1.134
                    * (work.iter().map(|x| (x - new_m) * (x - new_m)).sum::<f64>() / (n - 1) as f64).sqrt();
                let converged = (new_s - s).abs() <= 0.005 * s.abs();
                m = new_m; s = new_s;
                if converged { break; }
            }
            // 2) Reject original samples outside [m - σ_low·s, m + σ_high·s], mean the rest.
            let (lo, hi) = (m - sigma_low * s, m + sigma_high * s);
            let mut sum = 0.0f64;
            let mut kept = 0usize;
            for &x in values.iter() {
                let xf = x as f64;
                if xf >= lo && xf <= hi { sum += xf; kept += 1; }
            }
            if kept == 0 { return (median_sorted(values), values.len()); }
            ((sum / kept as f64) as f32, values.len() - kept)
        }
        CombineMethod::PercentileClip { low, high } => {
            let n = values.len();
            if n < 3 { return (mean(values), 0); }
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let m = median_sorted(values) as f64;
            if m.abs() <= f64::EPSILON { return (m as f32, 0); }
            let mut sum = 0.0f64;
            let mut kept = 0usize;
            for &x in values.iter() {
                let xf = x as f64;
                let dev = (xf - m) / m.abs();
                let reject = (dev < 0.0 && -dev > low) || (dev > 0.0 && dev > high);
                if !reject { sum += xf; kept += 1; }
            }
            if kept == 0 { return (m as f32, n); }
            ((sum / kept as f64) as f32, n - kept)
        }
    }
}
```

(The 1.134 factor is the standard winsorized-σ correction for the 1.5σ clamp; the winsorized estimate feeds a plain σ-rejection over the ORIGINAL samples — matching the shape of PixInsight's Winsorized Sigma Clipping.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core integration::combine`
Expected: 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/integration/
git commit -m "feat(integration): mean/median/winsorized-sigma/percentile pixel combiners"
```

---

### Task 8: Integration engine — bias-like + flat recipes

**Files:**
- Create: `crates/athenaeum-core/src/integration/engine.rs` (+ `pub mod engine;` in `integration/mod.rs`)

**Interfaces:**
- Consumes: `BandSource`, `band_rows_for_budget`, `combine_pixel`, `CombineMethod`, `ctx.image_pool` (passed as `&rayon::ThreadPool`).
- Produces (consumed by Task 12):

```rust
pub struct IntegrationOutput {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,          // row-major, single channel, ADU scale
    pub rejected_fraction: f64,  // rejected samples / total samples
    pub flat_norm: Option<f64>,  // central-third mean of the OUTPUT (flats only)
}

pub struct EngineProgress<'a> {
    /// (bands_done, bands_total) — called from the integrating thread.
    pub on_band: &'a dyn Fn(usize, usize),
}

/// bias / dark / darkflat: plain combine of raw frames.
pub fn integrate_bias_like(
    paths: &[std::path::PathBuf],
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    scratch_dir: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
    progress: EngineProgress<'_>,
) -> Result<IntegrationOutput, IntegrationError>;

/// Flat pre-calibration source, resolved by the caller (Task 12) from the
/// set's sub-cal links per the fallback chain.
pub enum FlatPrecal {
    MasterFrame { data: Vec<f32>, width: usize, height: usize }, // darkflat/dark/bias master pixels
    SyntheticBias(f32),                                          // constant ADU
    None,
}

/// flat: per-frame precal subtraction + multiplicative normalization to the
/// frame's central-third mean, then combine; flat_norm computed on the output.
pub fn integrate_flat(
    paths: &[std::path::PathBuf],
    precal: &FlatPrecal,
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    scratch_dir: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
    progress: EngineProgress<'_>,
) -> Result<IntegrationOutput, IntegrationError>;

/// Central-third mean over a full-resolution buffer (shared helper — the same
/// region rule as Siril: x,y in [dim/3, 2*dim/3)).
pub fn central_third_mean(data: &[f32], width: usize, height: usize) -> f64;
```

- [ ] **Step 1: Write the failing tests** (bottom of `engine.rs`; fixtures via `fits_writer` like Task 6)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::integration::combine::CombineMethod;
    use std::sync::atomic::AtomicBool;

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap()
    }
    fn nop() -> impl Fn(usize, usize) { |_, _| {} }

    fn write(dir: &std::path::Path, name: &str, w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> std::path::PathBuf {
        let mut d = vec![0f32; w * h];
        for y in 0..h { for x in 0..w { d[y * w + x] = f(x, y); } }
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &d, &[]).unwrap();
        p
    }

    #[test]
    fn dark_master_is_mean_with_outlier_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (48, 33); // non-multiple of band size on purpose
        let mut paths: Vec<_> = (0..16)
            .map(|i| write(dir.path(), &format!("d{i}.fits"), w, h, |_, _| 100.0 + (i % 4) as f32))
            .collect();
        // one frame with a hot pixel at (5,5)
        paths.push(write(dir.path(), "hot.fits", w, h, |x, y| if (x, y) == (5, 5) { 9000.0 } else { 101.0 }));
        let on_band = nop();
        let out = integrate_bias_like(
            &paths,
            CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 },
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
        ).unwrap();
        assert_eq!((out.width, out.height), (w, h));
        let hot = out.data[5 * w + 5];
        assert!(hot < 200.0, "hot pixel must be rejected, got {hot}");
        assert!(out.rejected_fraction > 0.0);
        assert!(out.flat_norm.is_none());
    }

    #[test]
    fn flat_normalization_equalizes_exposure_drift() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (30, 30);
        // Same vignetting shape, different levels (sky brightness drift 1x/2x/4x).
        let shape = |x: usize, _y: usize| 1000.0 + (x as f32) * 10.0;
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |x, y| shape(x, y)),
            write(dir.path(), "f2.fits", w, h, |x, y| shape(x, y) * 2.0),
            write(dir.path(), "f3.fits", w, h, |x, y| shape(x, y) * 4.0),
        ];
        let on_band = nop();
        let out = integrate_flat(
            &paths, &FlatPrecal::None, CombineMethod::Median,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
        ).unwrap();
        // After per-frame normalization all three frames agree, so the master
        // must reproduce the SHAPE: ratio of two positions equals shape ratio.
        let a = out.data[15 * w + 5];
        let b = out.data[15 * w + 25];
        let expect = shape(5, 15) / shape(25, 15);
        assert!(((a / b) - expect).abs() < 0.01, "shape preserved: {} vs {expect}", a / b);
        let fnorm = out.flat_norm.expect("flats carry flat_norm");
        assert!((fnorm - central_third_mean(&out.data, w, h)).abs() < 1e-6);
    }

    #[test]
    fn flat_precal_subtracts_master() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (24, 24);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1500.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1500.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1500.0),
        ];
        let precal = FlatPrecal::MasterFrame { data: vec![500.0; w * h], width: w, height: h };
        let on_band = nop();
        let out = integrate_flat(
            &paths, &precal, CombineMethod::Median,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
        ).unwrap();
        assert!(out.data.iter().all(|&v| (v - 1000.0).abs() < 0.01),
            "1500 - 500 precal = 1000 everywhere");
    }

    #[test]
    fn synthetic_bias_constant() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16, 16);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1100.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1100.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1100.0),
        ];
        let on_band = nop();
        let out = integrate_flat(
            &paths, &FlatPrecal::SyntheticBias(100.0), CombineMethod::Median,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
        ).unwrap();
        assert!(out.data.iter().all(|&v| (v - 1000.0).abs() < 0.01));
    }

    #[test]
    fn cancel_mid_run_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4).map(|i| write(dir.path(), &format!("c{i}.fits"), 64, 256, |_, _| 1.0)).collect();
        let cancel = AtomicBool::new(true); // pre-set: first band check trips
        let on_band = nop();
        let r = integrate_bias_like(
            &paths, CombineMethod::Mean, &pool(), dir.path(), &cancel,
            EngineProgress { on_band: &on_band },
        );
        assert!(matches!(r, Err(IntegrationError::Cancelled)));
    }

    #[test]
    fn negatives_pass_through_unclipped() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8, 8);
        let paths = vec![
            write(dir.path(), "n1.fits", w, h, |_, _| -5.0),
            write(dir.path(), "n2.fits", w, h, |_, _| -5.0),
            write(dir.path(), "n3.fits", w, h, |_, _| -5.0),
        ];
        let on_band = nop();
        let out = integrate_bias_like(
            &paths, CombineMethod::Mean, &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
        ).unwrap();
        assert!(out.data.iter().all(|&v| v == -5.0), "no clipping policy");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core integration::engine`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `engine.rs`**

```rust
//! Recipe orchestration (spec §4, §9): banded streaming + per-pixel combine.
//! Memory: N × band (default 256 MiB budget). Parallelism: rayon over the
//! pixels of the current band via the shared image pool.

use super::banded::{band_rows_for_budget, BandSource};
use super::combine::{combine_pixel, CombineMethod};
use super::IntegrationError;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const BAND_BUDGET_BYTES: usize = 256 * 1024 * 1024;

pub struct IntegrationOutput {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub rejected_fraction: f64,
    pub flat_norm: Option<f64>,
}

pub struct EngineProgress<'a> {
    pub on_band: &'a dyn Fn(usize, usize),
}

pub enum FlatPrecal {
    MasterFrame { data: Vec<f32>, width: usize, height: usize },
    SyntheticBias(f32),
    None,
}

pub fn central_third_mean(data: &[f32], width: usize, height: usize) -> f64 {
    let (x0, x1) = (width / 3, (2 * width) / 3);
    let (y0, y1) = (height / 3, (2 * height) / 3);
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for y in y0..y1.max(y0 + 1).min(height) {
        for x in x0..x1.max(x0 + 1).min(width) {
            sum += data[y * width + x] as f64;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// Shared banded-combine core. `scale[i]`/`offset_fn` transform frame i's
/// samples before combining: v' = (v - offset(i, pixel)) * scale[i].
fn run_banded(
    src: &mut BandSource,
    scales: &[f32],
    precal: Option<&FlatPrecal>,
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &EngineProgress<'_>,
) -> Result<IntegrationOutput, IntegrationError> {
    use rayon::prelude::*;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    let band_rows = band_rows_for_budget(w, n, BAND_BUDGET_BYTES).min(h);
    let bands_total = h.div_ceil(band_rows);
    let mut out = vec![0f32; w * h];
    let rejected = AtomicUsize::new(0);
    let mut band_bufs: Vec<Vec<f32>> = vec![Vec::new(); n];

    for (band_idx, y0) in (0..h).step_by(band_rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let rows = band_rows.min(h - y0);
        src.read_band(y0, rows, &mut band_bufs)?;

        let out_band = &mut out[y0 * w..(y0 + rows) * w];
        pool.install(|| {
            out_band
                .par_chunks_mut(w)                       // one row per work item
                .enumerate()
                .for_each(|(row_in_band, out_row)| {
                    let mut column: Vec<f32> = Vec::with_capacity(n);
                    for (x, out_px) in out_row.iter_mut().enumerate() {
                        column.clear();
                        let idx = row_in_band * w + x;
                        for (i, frame) in band_bufs.iter().enumerate() {
                            let mut v = frame[idx];
                            if let Some(p) = precal {
                                match p {
                                    FlatPrecal::MasterFrame { data, width, .. } => {
                                        let gy = y0 + row_in_band;
                                        v -= data[gy * *width + x];
                                    }
                                    FlatPrecal::SyntheticBias(b) => v -= *b,
                                    FlatPrecal::None => {}
                                }
                            }
                            v *= scales[i];
                            column.push(v);
                        }
                        let (val, rej) = combine_pixel(&mut column, method);
                        *out_px = val;
                        if rej > 0 { rejected.fetch_add(rej, Ordering::Relaxed); }
                    }
                });
        });
        (progress.on_band)(band_idx + 1, bands_total);
    }

    let total_samples = (w * h * n).max(1);
    Ok(IntegrationOutput {
        width: w,
        height: h,
        data: out,
        rejected_fraction: rejected.load(Ordering::Relaxed) as f64 / total_samples as f64,
        flat_norm: None,
    })
}

pub fn integrate_bias_like(
    paths: &[PathBuf],
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
) -> Result<IntegrationOutput, IntegrationError> {
    let mut src = BandSource::open(paths, scratch_dir)?;
    let scales = vec![1.0f32; src.frame_count()];
    run_banded(&mut src, &scales, None, method, pool, cancel, &progress)
}

pub fn integrate_flat(
    paths: &[PathBuf],
    precal: &FlatPrecal,
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
) -> Result<IntegrationOutput, IntegrationError> {
    let mut src = BandSource::open(paths, scratch_dir)?;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    if let FlatPrecal::MasterFrame { width, height, .. } = precal {
        if (*width, *height) != (w, h) {
            return Err(IntegrationError::BadInput(format!(
                "pre-calibration master is {width}x{height}, flats are {w}x{h}"
            )));
        }
    }

    // Pass 1: per-frame central-third mean AFTER precal subtraction.
    let (cy0, cy1) = (h / 3, ((2 * h) / 3).max(h / 3 + 1).min(h));
    let (cx0, cx1) = (w / 3, ((2 * w) / 3).max(w / 3 + 1).min(w));
    let mut sums = vec![0f64; n];
    let mut counts = vec![0usize; n];
    let band_rows = band_rows_for_budget(w, n, BAND_BUDGET_BYTES).min(cy1 - cy0);
    let mut band_bufs: Vec<Vec<f32>> = vec![Vec::new(); n];
    let mut y = cy0;
    while y < cy1 {
        if cancel.load(Ordering::Relaxed) { return Err(IntegrationError::Cancelled); }
        let rows = band_rows.min(cy1 - y);
        src.read_band(y, rows, &mut band_bufs)?;
        for (i, frame) in band_bufs.iter().enumerate() {
            for r in 0..rows {
                let gy = y + r;
                for x in cx0..cx1 {
                    let mut v = frame[r * w + x] as f64;
                    match precal {
                        FlatPrecal::MasterFrame { data, width, .. } => v -= data[gy * *width + x] as f64,
                        FlatPrecal::SyntheticBias(b) => v -= *b as f64,
                        FlatPrecal::None => {}
                    }
                    sums[i] += v;
                    counts[i] += 1;
                }
            }
        }
        y += rows;
    }
    let means: Vec<f64> = sums.iter().zip(&counts).map(|(s, &c)| s / c.max(1) as f64).collect();
    for (i, m) in means.iter().enumerate() {
        if *m <= 0.0 {
            return Err(IntegrationError::BadInput(format!(
                "flat frame {} has non-positive central mean {m:.1} after pre-calibration — wrong precal master?",
                paths[i].display()
            )));
        }
    }
    // Normalize each frame to the mean of means (flux equalization).
    let target: f64 = means.iter().sum::<f64>() / n as f64;
    let scales: Vec<f32> = means.iter().map(|m| (target / m) as f32).collect();

    // Pass 2: full combine with precal + scale applied.
    let mut out = run_banded(&mut src, &scales, Some(precal), method, pool, cancel, &progress)?;
    out.flat_norm = Some(central_third_mean(&out.data, w, h));
    Ok(out)
}
```

(NOTE for the implementer: `run_banded` captures `y0` for the precal master row index — the closure lives inside the `for` loop so `y0` is in scope; keep it that way. `BandSource::read_band` is `&mut self`, so pass 1 and pass 2 reuse the SAME source — readers just seek again.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core integration::engine`
Expected: 6 tests PASS.

- [ ] **Step 5: Full-crate regression + commit**

Run: `cargo test -p athenaeum-core`
Expected: PASS.

```bash
git add crates/athenaeum-core/src/integration/
git commit -m "feat(integration): bias/dark/flat recipes — banded combine, flux-equalized flats, ATH_FNRM source"
```

---

### Task 9: Calibration Library root — kind support end-to-end

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs:298-312` (`upsert_scan_root`)
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:90-170` (`add_scan_root`)
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs` + `crates/athenaeum-web/src/routes/scan_roots.rs` (thread the new optional param)
- Modify: `src/pages/Settings.tsx` (new "Calibration Library" section)
- Modify: `src/hooks/useTauri.ts:34` (`addScanRoot` gains optional kind)

**Interfaces:**
- Produces:
  - `api::scan_roots::add_scan_root(ctx, path, policy, kind: Option<String>) -> Result<ScanRoot, ApiError>` — `kind` defaults to `"normal"`; `"calibration_library"` enforced unique (Conflict if another library root exists); any other value → Invalid.
  - `db::operations::upsert_scan_root(conn, path, kind: &str) -> Result<i64>`.
  - `api::scan_roots::get_calibration_library_root(ctx) -> Result<Option<ScanRoot>, ApiError>` (helper used by Task 12 and the Settings UI).
- Consumed by: Tasks 10, 12 (resolve library root), Settings UI.

- [ ] **Step 1: Write the failing tests** (append to the `#[cfg(test)]` module in `api/scan_roots.rs` if present — check `grep -n "mod tests" crates/athenaeum-core/src/api/scan_roots.rs`; if absent, put the DB-level tests in `db/operations.rs`'s test module)

```rust
    #[test]
    fn upsert_scan_root_stores_kind() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let id = crate::db::upsert_scan_root(&conn, "/data/library", "calibration_library").unwrap();
        let kind: String = conn.query_row(
            "SELECT kind FROM scan_roots WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(kind, "calibration_library");
        // Upserting an existing path must NOT silently flip its kind.
        let id2 = crate::db::upsert_scan_root(&conn, "/data/library", "normal").unwrap();
        assert_eq!(id, id2);
        let kind: String = conn.query_row(
            "SELECT kind FROM scan_roots WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(kind, "calibration_library");
    }

    #[test]
    fn only_one_calibration_library_root() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        // The uniqueness check is code-level in api::add_scan_root; expose the
        // count helper it uses and pin it here:
        let n = crate::db::count_scan_roots_of_kind(&conn, "calibration_library").unwrap();
        assert_eq!(n, 1);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core upsert_scan_root_stores_kind`
Expected: FAIL — `upsert_scan_root` takes 2 args; `count_scan_roots_of_kind` missing.

- [ ] **Step 3: Implement db layer**

`db/operations.rs` — replace `upsert_scan_root` (`:298-312`):

```rust
/// Insert or update a scan root. `kind` applies only on INSERT — an existing
/// row's kind is never silently changed by re-adding the same path.
pub fn upsert_scan_root(conn: &Connection, path: &str, kind: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO scan_roots (path, enabled, find_duplicates, kind) VALUES (?1, 1, 1, ?2)
         ON CONFLICT(path) DO NOTHING",
        params![path, kind],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM scan_roots WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Count scan roots of a given kind (uniqueness guard for calibration_library).
pub fn count_scan_roots_of_kind(conn: &Connection, kind: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM scan_roots WHERE kind = ?1",
        params![kind],
        |row| row.get(0),
    )
}
```

Fix the pre-existing caller: `grep -rn "upsert_scan_root(" crates/ | grep -v "fn upsert"` — pass `"normal"` everywhere except the api handler below.

- [ ] **Step 4: Implement api layer**

`api/scan_roots.rs` — extend the signature (`:90`) and the insert/return (`:152-169`):

```rust
pub fn add_scan_root(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
    kind: Option<String>,
) -> Result<ScanRoot, ApiError> {
    let kind = kind.unwrap_or_else(|| "normal".to_string());
    if kind != "normal" && kind != "calibration_library" {
        return Err(ApiError::Invalid(format!("unknown scan root kind: {kind}")));
    }
    ...existing validation/canonicalization/overlap checks unchanged...

    if kind == "calibration_library"
        && crate::db::count_scan_roots_of_kind(&conn, "calibration_library")? > 0
    {
        return Err(ApiError::Conflict(
            "A Calibration Library root already exists — only one is allowed".to_string(),
        ));
    }

    let path_str = new_path.to_string_lossy().to_string();
    tracing::info!(path = %path_str, kind = %kind, "adding scan root");
    let id = crate::db::upsert_scan_root(&conn, &path_str, &kind).map_err(|e| { ... })?;

    Ok(ScanRoot {
        id: Some(id),
        path: path_str,
        enabled: true,
        find_duplicates: true,
        unique_camera: false,
        last_scan: None,
        last_scan_errors: None,
        monitor_enabled: false,
        kind,
    })
}

/// The (single) calibration library root, if configured.
pub fn get_calibration_library_root(ctx: &ServiceContext) -> Result<Option<ScanRoot>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_scan_roots(&conn)?
        .into_iter()
        .find(|r| r.kind == "calibration_library"))
}
```

Update BOTH wrappers to accept the new optional arg (`kind: Option<String>` on the Tauri command; JSON body field on the web route) — locate them: `grep -rn "add_scan_root" crates/athenaeum-tauri/src/commands/scan_roots.rs crates/athenaeum-web/src/routes/scan_roots.rs`. Add a `get_calibration_library_root` command+route pair (same thin-wrapper shape as `get_scan_roots`), register both.

- [ ] **Step 5: Settings UI section**

`src/pages/Settings.tsx` — add a "Calibration Library" section (place it after the existing "Calibration Matching" tab content or as a card inside the General tab — inspect the page's section pattern first: `grep -n "Calibration Matching" src/pages/Settings.tsx`). Complete component logic to embed:

```tsx
function CalibrationLibrarySection() {
  const [libraryRoot, setLibraryRoot] = useState<ScanRoot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => {
    try {
      const root = await api.invoke<ScanRoot | null>('get_calibration_library_root');
      setLibraryRoot(root);
    } catch (e) { setError(String(e)); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const choose = async () => {
    setError(null);
    const dir = await pickDirectory(); // same helper the archive-root picker uses
    if (!dir) return;
    try {
      await api.invoke<ScanRoot>('add_scan_root', { path: dir, kind: 'calibration_library' });
      await load();
    } catch (e) { setError(String(e)); }
  };

  return (
    <div className="bg-surface-elevated rounded-lg p-4 border border-border">
      <h3 className="text-sm font-medium text-content mb-1">Calibration Library</h3>
      <p className="text-xs text-content-muted mb-3">
        Master calibration frames built by Athenaeum are written here. The folder is
        scanned like any other root, so masters dropped in from elsewhere are imported too.
      </p>
      {libraryRoot ? (
        <div className="text-sm text-content font-mono">{libraryRoot.path}</div>
      ) : (
        <button onClick={choose}
          className="px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded">
          Choose library folder…
        </button>
      )}
      {error && <div className="text-xs text-danger mt-2">{error}</div>}
    </div>
  );
}
```

(Use the file-picker helper Settings/archive already imports — check `grep -rn "pickDirectory" src/ | head -3` and copy that import. Web build: the picker falls back to `FolderBrowserModal` exactly as `ExportTab.tsx:222-254` does; replicate that branch.)

`src/hooks/useTauri.ts:34` — extend `addScanRoot` to pass `kind` through (optional second parameter, default undefined).

- [ ] **Step 6: Tests + regen + commit**

Run: `cargo test -p athenaeum-core && TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract && npx tsc --noEmit`
Expected: PASS.

```bash
git add crates/ src/
git commit -m "feat(library): calibration_library scan-root kind, single-root enforcement, Settings picker"
```

---

### Task 10: Master naming + header consolidation

**Files:**
- Create: `crates/athenaeum-core/src/calibration_library/mod.rs` (`pub mod paths; pub mod headers; pub mod register;` — register lands in Task 11, declare only paths+headers now)
- Create: `crates/athenaeum-core/src/calibration_library/paths.rs`
- Create: `crates/athenaeum-core/src/calibration_library/headers.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (`pub mod calibration_library;`)

**Interfaces:**
- Consumes: `archive::path_layout::sanitize_for_filename` (already `pub`), `fits_writer::keywords::{HeaderBuilder, FrameKind}`, `fits_writer::Card`.
- Produces (consumed by Tasks 11–12):

```rust
// paths.rs
/// Relative path inside the library root, per the fixed v1 template:
/// <INSTRUME>/<MasterType>/master_dark_300s_-10C_g100_bin1_2026-06-28.fits
/// Flats insert the filter token after the type. Missing values collapse
/// to nothing (no "NaN" junk in filenames).
pub struct MasterPathParams<'a> {
    pub instrume: Option<&'a str>,
    pub master_kind: crate::fits_writer::keywords::FrameKind, // MasterDark | MasterFlat | MasterBias | MasterDarkFlat
    pub filter: Option<&'a str>,
    pub exptime: Option<f64>,
    pub ccd_temp: Option<f64>,
    pub gain: Option<f64>,
    pub binning: Option<&'a str>,
    pub date: &'a str, // YYYY-MM-DD (calibration_set.date)
}
pub fn master_relative_path(p: &MasterPathParams) -> std::path::PathBuf;
/// First non-existing variant of `abs`: abs, then stem_2.fits, stem_3.fits…
pub fn resolve_collision(abs: &std::path::Path) -> std::path::PathBuf;

// headers.rs
/// Everything needed to consolidate a master header from its source set.
/// Loaded with load_header_inputs() below.
pub struct MasterHeaderInputs {
    pub kind: crate::fits_writer::keywords::FrameKind,
    pub instrume: Option<String>, pub telescop: Option<String>,
    pub filter: Option<String>, pub exptime: Option<f64>,
    pub gain: Option<f64>, pub offset: Option<f64>,
    pub xbinning: Option<i64>, pub ybinning: Option<i64>,
    pub xpixsz: Option<f64>, pub ypixsz: Option<f64>,
    pub focallen: Option<f64>, pub egain: Option<f64>,
    pub bayerpat: Option<String>, pub xbayroff: Option<i64>, pub ybayroff: Option<i64>,
    pub temp_mean: Option<f64>, pub temp_min: Option<f64>, pub temp_max: Option<f64>,
    pub date_obs_midpoint: Option<chrono::DateTime<chrono::Utc>>,
    pub frame_count: u32,
    pub source_set_uuid: String,
}
pub fn load_header_inputs(conn: &rusqlite::Connection, source_set_id: i64)
    -> anyhow::Result<MasterHeaderInputs>;
pub fn build_master_cards(
    inputs: &MasterHeaderInputs,
    app_version: &str,
    recipe_summary: &str,      // e.g. "winsorized(3.0,3.0) n=24"
    member_hash: &str,
    flat_norm: Option<f64>,    // stamps ATH_FNRM when Some
) -> Result<Vec<crate::fits_writer::Card>, crate::fits_writer::FitsWriteError>;
```

- [ ] **Step 1: Write the failing tests**

`paths.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::FrameKind;

    #[test]
    fn dark_path_shape() {
        let p = master_relative_path(&MasterPathParams {
            instrume: Some("ZWO ASI2600MM Pro"), master_kind: FrameKind::MasterDark,
            filter: None, exptime: Some(300.0), ccd_temp: Some(-10.2),
            gain: Some(100.0), binning: Some("1x1"), date: "2026-06-28",
        });
        assert_eq!(
            p.to_string_lossy(),
            "ZWO ASI2600MM Pro/MasterDark/master_dark_300s_-10C_g100_bin1x1_2026-06-28.fits"
        );
    }

    #[test]
    fn flat_includes_filter_and_missing_fields_collapse() {
        let p = master_relative_path(&MasterPathParams {
            instrume: Some("cam"), master_kind: FrameKind::MasterFlat,
            filter: Some("Ha"), exptime: Some(1.55), ccd_temp: None,
            gain: None, binning: None, date: "2026-07-01",
        });
        assert_eq!(
            p.to_string_lossy(),
            "cam/MasterFlat/master_flat_Ha_1.55s_2026-07-01.fits"
        );
    }

    #[test]
    fn unknown_camera_bucket() {
        let p = master_relative_path(&MasterPathParams {
            instrume: None, master_kind: FrameKind::MasterBias,
            filter: None, exptime: None, ccd_temp: None,
            gain: None, binning: None, date: "2026-01-01",
        });
        assert!(p.starts_with("UnknownCamera/MasterBias/"), "{p:?}");
    }

    #[test]
    fn collision_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("m.fits");
        assert_eq!(resolve_collision(&base), base);
        std::fs::write(&base, b"x").unwrap();
        assert_eq!(resolve_collision(&base), dir.path().join("m_2.fits"));
        std::fs::write(dir.path().join("m_2.fits"), b"x").unwrap();
        assert_eq!(resolve_collision(&base), dir.path().join("m_3.fits"));
    }
}
```

`headers.rs` tests (build a source set in an in-memory DB, then assert the card set):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::FrameKind;
    use rusqlite::Connection;

    fn seed(conn: &Connection) -> i64 {
        crate::db::schema::init_db(conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, telescop,
              date, date_start, date_end, temp_min, temp_max, frame_count, focallen)
             VALUES ('Dark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', 'TestScope',
              '2026-06-28', '2026-06-28T20:00:00Z', '2026-06-28T22:00:00Z',
              -10.6, -9.4, 2, 540.0)",
            [],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        for (i, dt) in ["2026-06-28T20:00:00Z", "2026-06-28T22:00:00Z"].iter().enumerate() {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 10, '2026-06-28', 'FITS')",
                rusqlite::params![format!("/d/f{i}.fits"), format!("f{i}.fits")],
            ).unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset,
                                     binning, xbinning, ybinning, ccd_temp, date_obs, xpixsz, ypixsz)
                 VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', 1, 1, ?2, ?3, 3.76, 3.76)",
                rusqlite::params![file_id, -10.0 - (i as f64) * 0.5, dt],
            ).unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            ).unwrap();
        }
        set_id
    }

    #[test]
    fn consolidated_cards_cover_the_vocabulary() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        assert_eq!(inputs.kind, FrameKind::MasterDark);
        assert_eq!(inputs.frame_count, 2);
        // midpoint of 20:00 and 22:00 is 21:00
        assert_eq!(inputs.date_obs_midpoint.unwrap().to_rfc3339(), "2026-06-28T21:00:00+00:00");
        let cards = build_master_cards(&inputs, "0.2.5", "winsorized(3.0,3.0) n=2", "cafe", None).unwrap();
        let find = |k: &str| cards.iter().find(|c| c.keyword == k);
        assert!(find("IMAGETYP").is_some());
        assert!(find("INSTRUME").is_some());
        assert!(find("EXPTIME").is_some());
        assert!(find("CCD-TEMP").is_some());
        assert!(find("ATH_TMIN").is_some() && find("ATH_TMAX").is_some());
        assert!(find("ATH_SRC").is_some());
        assert!(find("ATH_N").is_some());
        assert!(find("ATH_REJ").is_some());
        assert!(find("ATH_HSH").is_some());
        assert!(find("SWCREATE").is_some());
        assert!(find("ATH_FNRM").is_none(), "darks carry no flat norm");
    }

    #[test]
    fn flat_norm_card_present_for_flats() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        conn.execute("UPDATE calibration_set SET imagetyp='Flat', filter='L' WHERE id=?1", [set_id]).unwrap();
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        assert_eq!(inputs.kind, FrameKind::MasterFlat);
        let cards = build_master_cards(&inputs, "0.2.5", "percentile(0.2,0.02) n=2", "cafe", Some(1234.5)).unwrap();
        let f = cards.iter().find(|c| c.keyword == "ATH_FNRM").expect("ATH_FNRM");
        assert!(matches!(f.value, Some(crate::fits_writer::CardValue::Real(v)) if (v - 1234.5).abs() < 1e-9));
        assert!(cards.iter().any(|c| c.keyword == "FILTER"));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core calibration_library` → FAIL (module missing).

- [ ] **Step 3: Implement `paths.rs`**

```rust
//! Fixed v1 master-file naming (spec §2). No token engine — the layout is a
//! deliberate constant; a user-configurable template is future work.

use crate::archive::path_layout::sanitize_for_filename;
use crate::fits_writer::keywords::FrameKind;
use std::path::{Path, PathBuf};

pub struct MasterPathParams<'a> {
    pub instrume: Option<&'a str>,
    pub master_kind: FrameKind,
    pub filter: Option<&'a str>,
    pub exptime: Option<f64>,
    pub ccd_temp: Option<f64>,
    pub gain: Option<f64>,
    pub binning: Option<&'a str>,
    pub date: &'a str,
}

fn kind_folder(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::MasterDark => "MasterDark",
        FrameKind::MasterFlat => "MasterFlat",
        FrameKind::MasterBias => "MasterBias",
        FrameKind::MasterDarkFlat => "MasterDarkFlat",
        _ => "Master",
    }
}

fn kind_stem(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::MasterDark => "master_dark",
        FrameKind::MasterFlat => "master_flat",
        FrameKind::MasterBias => "master_bias",
        FrameKind::MasterDarkFlat => "master_darkflat",
        _ => "master",
    }
}

/// Trim trailing zeros: 300.0 -> "300", 1.55 -> "1.55".
fn fmt_num(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub fn master_relative_path(p: &MasterPathParams) -> PathBuf {
    let camera = p
        .instrume
        .map(sanitize_for_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UnknownCamera".to_string());
    let mut parts: Vec<String> = vec![kind_stem(p.master_kind).to_string()];
    if matches!(p.master_kind, FrameKind::MasterFlat | FrameKind::MasterDarkFlat) {
        if let Some(f) = p.filter {
            let f = sanitize_for_filename(f);
            if !f.is_empty() { parts.push(f); }
        }
    }
    if let Some(e) = p.exptime { parts.push(format!("{}s", fmt_num(e))); }
    if let Some(t) = p.ccd_temp { parts.push(format!("{}C", fmt_num(t.round()))); }
    if let Some(g) = p.gain { parts.push(format!("g{}", fmt_num(g))); }
    if let Some(b) = p.binning {
        let b = sanitize_for_filename(b);
        if !b.is_empty() { parts.push(format!("bin{b}")); }
    }
    parts.push(p.date.to_string());
    PathBuf::from(camera)
        .join(kind_folder(p.master_kind))
        .join(format!("{}.fits", parts.join("_")))
}

pub fn resolve_collision(abs: &Path) -> PathBuf {
    if !abs.exists() { return abs.to_path_buf(); }
    let stem = abs.file_stem().and_then(|s| s.to_str()).unwrap_or("master");
    let ext = abs.extension().and_then(|s| s.to_str()).unwrap_or("fits");
    for n in 2u32.. {
        let candidate = abs.with_file_name(format!("{stem}_{n}.{ext}"));
        if !candidate.exists() { return candidate; }
    }
    unreachable!()
}
```

(Check `sanitize_for_filename`'s exact behavior first — `sed -n '8,38p' crates/athenaeum-core/src/archive/path_layout.rs` — and align the dark-path test expectation with what it actually produces for `"ZWO ASI2600MM Pro"` (it keeps spaces or replaces them — adjust the expected string in the test to the real output rather than weakening the sanitizer).)

- [ ] **Step 4: Implement `headers.rs`**

```rust
//! Consolidate a master's FITS header from its source calibration set +
//! member frames (spec §3 step 3, arch-doc B3).

use crate::fits_writer::keywords::{Bayer, FrameKind, HeaderBuilder};
use crate::fits_writer::{Card, FitsWriteError};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;

pub struct MasterHeaderInputs { /* …exact fields from the Interfaces block above… */ }

fn master_kind_for(imagetyp: &str) -> Option<FrameKind> {
    match imagetyp {
        "Dark" | "MasterDark" => Some(FrameKind::MasterDark),
        "Flat" | "MasterFlat" => Some(FrameKind::MasterFlat),
        "Bias" | "MasterBias" => Some(FrameKind::MasterBias),
        "DarkFlat" | "MasterDarkFlat" => Some(FrameKind::MasterDarkFlat),
        _ => None,
    }
}

pub fn load_header_inputs(conn: &Connection, source_set_id: i64) -> Result<MasterHeaderInputs> {
    // Set-level values (already aggregated by the scanner's clustering).
    let (imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume, telescop,
         temp_min, temp_max, frame_count, focallen, uuid): (
        String, Option<f64>, Option<String>, Option<f64>, Option<f64>, Option<f64>,
        Option<String>, Option<String>, Option<String>, Option<f64>, Option<f64>,
        i64, Option<f64>, String,
    ) = conn.query_row(
        "SELECT imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume,
                telescop, temp_min, temp_max, frame_count, focallen, uuid
         FROM calibration_set WHERE id = ?1",
        [source_set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?,
                r.get(12)?, r.get(13)?)),
    )?;
    let kind = master_kind_for(&imagetyp)
        .ok_or_else(|| anyhow!("set {source_set_id} has non-calibration imagetyp {imagetyp}"))?;

    // Frame-level aggregates: temp mean, date midpoint, binning ints, pixel size.
    let (temp_mean, min_dt, max_dt, xbin, ybin, xpixsz, ypixsz): (
        Option<f64>, Option<String>, Option<String>, Option<i64>, Option<i64>,
        Option<f64>, Option<f64>,
    ) = conn.query_row(
        "SELECT AVG(f.ccd_temp), MIN(f.date_obs), MAX(f.date_obs),
                MAX(f.xbinning), MAX(f.ybinning), MAX(f.xpixsz), MAX(f.ypixsz)
         FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         WHERE csf.set_id = ?1",
        [source_set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
    )?;
    // frames has no bayerpat column — BAYERPAT lives in the stored raw
    // header. Fetch it from fits_header of the first member file:
    let bayerpat: Option<String> = conn.query_row(
        "SELECT fh.header FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         JOIN fits_header fh ON fh.file_id = f.file_id
         WHERE csf.set_id = ?1 LIMIT 1",
        [source_set_id],
        |r| r.get::<_, String>(0),
    ).ok()
    .and_then(|h| extract_header_string(&h, "BAYERPAT"));

    let midpoint = match (parse_dt(min_dt.as_deref()), parse_dt(max_dt.as_deref())) {
        (Some(a), Some(b)) => Some(a + (b - a) / 2),
        (Some(a), None) => Some(a),
        _ => None,
    };

    Ok(MasterHeaderInputs {
        kind,
        instrume, telescop, filter, exptime,
        gain, offset,
        xbinning: xbin, ybinning: ybin,
        xpixsz, ypixsz, focallen,
        egain: None, // EGAIN is not columnized; omitted from masters (additive later)
        bayerpat,
        xbayroff: None, ybayroff: None,
        temp_mean: temp_mean.or(ccd_temp),
        temp_min, temp_max,
        date_obs_midpoint: midpoint,
        frame_count: frame_count as u32,
        source_set_uuid: uuid,
    })
}

fn parse_dt(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc))
}

/// Pull `KEY     = 'VALUE'` out of a stored raw-header text blob.
fn extract_header_string(header: &str, key: &str) -> Option<String> {
    for line in header.lines() {
        if line.trim_start().to_ascii_uppercase().starts_with(key) {
            if let Some(q1) = line.find('\'') {
                if let Some(q2) = line[q1 + 1..].find('\'') {
                    return Some(line[q1 + 1..q1 + 1 + q2].trim().to_string());
                }
            }
        }
    }
    None
}

pub fn build_master_cards(
    inputs: &MasterHeaderInputs,
    app_version: &str,
    recipe_summary: &str,
    member_hash: &str,
    flat_norm: Option<f64>,
) -> Result<Vec<Card>, FitsWriteError> {
    let mut b = HeaderBuilder::new(inputs.kind).swcreate(app_version);
    if let Some(v) = inputs.exptime { b = b.exptime(v); }
    if let Some(dt) = inputs.date_obs_midpoint { b = b.date_obs(dt); }
    if let Some(t) = inputs.temp_mean { b = b.ccd_temp(t); }
    if let Some(g) = inputs.gain { b = b.gain(g.round() as i64); }
    if let Some(o) = inputs.offset { b = b.offset(o.round() as i64); }
    if let (Some(x), Some(y)) = (inputs.xbinning, inputs.ybinning) { b = b.binning(x, y); }
    if let (Some(x), Some(y)) = (inputs.xpixsz, inputs.ypixsz) { b = b.pixel_size(x, y); }
    if let Some(v) = &inputs.instrume { b = b.instrume(v); }
    if let Some(v) = &inputs.telescop { b = b.telescop(v); }
    if let Some(v) = inputs.focallen { b = b.focallen(v); }
    if let Some(v) = &inputs.filter { b = b.filter(v); }
    if let Some(p) = &inputs.bayerpat {
        let bayer = match p.to_ascii_uppercase().as_str() {
            "RGGB" => Some(Bayer::Rggb), "BGGR" => Some(Bayer::Bggr),
            "GBRG" => Some(Bayer::Gbrg), "GRBG" => Some(Bayer::Grbg),
            _ => None,
        };
        if let Some(bp) = bayer {
            b = b.bayer(bp, inputs.xbayroff.unwrap_or(0), inputs.ybayroff.unwrap_or(0));
        }
    }
    b = b
        .ath_src(&inputs.source_set_uuid)
        .ath_n(inputs.frame_count)
        .ath_rej(recipe_summary)
        .ath_ver(app_version)
        .ath_hsh(member_hash);
    if let (Some(min), Some(max)) = (inputs.temp_min, inputs.temp_max) {
        b = b.ath_temp_span(min, max);
    }
    if let Some(n) = flat_norm {
        b = b.custom(Card::new("ATH_FNRM", crate::fits_writer::CardValue::Real(n))?
            .with_comment("central-third mean of this master flat"));
    }
    b.build()
}
```

(The test seed doesn't insert a fits_header row, so `bayerpat` resolves to None there — correct for a mono test camera.)

- [ ] **Step 5: Run tests** — `cargo test -p athenaeum-core calibration_library` → PASS (6 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/calibration_library/ crates/athenaeum-core/src/lib.rs
git commit -m "feat(library): master path template + consolidated header builder"
```

---

### Task 11: Master registration — provenance, relink, supersede, matcher exclusion

**Files:**
- Create: `crates/athenaeum-core/src/calibration_library/register.rs`
- Create: `crates/athenaeum-core/src/db/master_provenance.rs` (+ `pub mod master_provenance;` in `db/mod.rs`)
- Modify: `crates/athenaeum-core/src/calibration/scan_integration.rs:869` (`fn create_master_sets_from_frames` → `pub fn`)
- Modify: `crates/athenaeum-core/src/calibration/configurable_matcher.rs:353-360` (superseded exclusion)

**Interfaces:**
- Produces (consumed by Task 12):

```rust
// db/master_provenance.rs
pub struct MasterProvenance {
    pub master_set_id: i64,
    pub source_set_id: Option<i64>,
    pub recipe_json: String,
    pub member_frame_uuids: String, // JSON array
    pub member_hash: String,
    pub created_at: String,
}
pub fn insert(conn, p: &MasterProvenance) -> anyhow::Result<()>;
pub fn get(conn, master_set_id: i64) -> anyhow::Result<Option<MasterProvenance>>;
pub fn update_rebuild(conn, master_set_id: i64, recipe_json: &str, member_hash: &str) -> anyhow::Result<()>;

// calibration_library/register.rs
pub struct MasterRegistration {
    pub master_set_id: i64,
    pub master_frame_id: i64,
    pub master_file_id: i64,
    pub relinked_links: usize,
}
/// One transaction: files+frames rows for the just-written master file
/// (parsed with the SAME fits_parser the scanner uses), 1:1 master set via
/// the SAME scan_integration helper, provenance, relink, supersede.
pub fn register_master(
    conn: &rusqlite::Connection,
    source_set_id: i64,
    master_path: &std::path::Path,
    recipe_json: &str,
) -> anyhow::Result<MasterRegistration>;
/// xxh3-of-sorted-member-uuids — the stable member-identity hash stamped as
/// ATH_HSH and stored in provenance. (Content hashes may be absent —
/// files.content_hash is only computed when duplicate detection uses it —
/// so member identity hashes over the always-present Phase-1 uuids.)
pub fn member_hash(conn: &rusqlite::Connection, source_set_id: i64) -> anyhow::Result<(String, Vec<String>)>;
```

- Matcher contract: `find_calibration_candidates` never returns a superseded set.

- [ ] **Step 1: Write the failing tests** (bottom of `register.rs`; this is the load-bearing test of the whole plan — full round-trip on a real temp DB + real FITS files)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::{FrameKind, HeaderBuilder};
    use crate::fits_writer::write_fits_f32;
    use rusqlite::Connection;

    /// Writes a parseable dark frame and registers it in files/frames like the
    /// scanner would (via SQL, since scan_directory needs a full root walk).
    fn seed_source_set(conn: &Connection, dir: &std::path::Path) -> i64 {
        crate::db::schema::init_db(conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, date,
              date_start, date_end, temp_min, temp_max, frame_count)
             VALUES ('Dark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', '2026-06-28',
              '2026-06-28T20:00:00Z', '2026-06-28T22:00:00Z', -10.5, -9.5, 3)",
            [],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        for i in 0..3 {
            let p = dir.join(format!("raw{i}.fits"));
            let cards = HeaderBuilder::new(FrameKind::Dark)
                .instrume("TestCam").exptime(300.0).gain(100).offset(50)
                .binning(1, 1).ccd_temp(-10.0)
                .build().unwrap();
            write_fits_f32(&p, 8, 8, 1, &vec![100.0; 64], &cards).unwrap();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 100, '2026-06-28', 'FITS')",
                rusqlite::params![p.to_string_lossy(), format!("raw{i}.fits")],
            ).unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset, binning, ccd_temp, date_obs)
                 VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', -10.0, '2026-06-28T21:00:00Z')",
                rusqlite::params![file_id],
            ).unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id]).unwrap();
        }
        set_id
    }

    /// A light frame linked to the raw set — the relink subject.
    fn seed_light_link(conn: &Connection, raw_set_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES ('/l/light.fits', 'light.fits', 100, '2026-06-28', 'FITS')", []).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, instrume, exptime) VALUES (?1, 'Light', 'TestCam', 300.0)",
            [file_id]).unwrap();
        let light_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
             VALUES (?1, 'frame', ?2, 'Dark', 0.9, 1)",
            rusqlite::params![light_id, raw_set_id]).unwrap();
        light_id
    }

    fn write_master(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("master_dark.fits");
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .instrume("TestCam").exptime(300.0).gain(100).offset(50)
            .binning(1, 1).ccd_temp(-10.0)
            .build().unwrap();
        write_fits_f32(&p, 8, 8, 1, &vec![100.0; 64], &cards).unwrap();
        p
    }

    #[test]
    fn register_master_full_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let raw = seed_source_set(&conn, dir.path());
        let light = seed_light_link(&conn, raw);
        let master_path = write_master(dir.path());

        let reg = register_master(&conn, raw, &master_path, r#"{"combine":"median"}"#).unwrap();

        // 1:1 master set, is_master_library, correct imagetyp
        let (imagetyp, is_master, count): (String, i64, i64) = conn.query_row(
            "SELECT imagetyp, is_master_library, frame_count FROM calibration_set WHERE id=?1",
            [reg.master_set_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((imagetyp.as_str(), is_master, count), ("MasterDark", 1, 1));

        // frames row is_master, files row points at the library file
        let (is_master_frame,): (i64,) = conn.query_row(
            "SELECT is_master FROM frames WHERE id=?1", [reg.master_frame_id], |r| Ok((r.get(0)?,))).unwrap();
        assert_eq!(is_master_frame, 1);
        let path: String = conn.query_row(
            "SELECT path FROM files WHERE id=?1", [reg.master_file_id], |r| r.get(0)).unwrap();
        assert_eq!(path, master_path.to_string_lossy());

        // relink: the light's Dark link now points at the master, manual flag preserved
        let (set_id, manual): (i64, i64) = conn.query_row(
            "SELECT calibration_set_id, is_manual_override FROM calibration_set_to_frames
             WHERE source_id=?1 AND source_type='frame' AND calibration_type='Dark'",
            [light], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((set_id, manual), (reg.master_set_id, 1));
        assert_eq!(reg.relinked_links, 1);

        // supersede + provenance
        let sup: Option<i64> = conn.query_row(
            "SELECT superseded_by_set_id FROM calibration_set WHERE id=?1", [raw], |r| r.get(0)).unwrap();
        assert_eq!(sup, Some(reg.master_set_id));
        let prov = crate::db::master_provenance::get(&conn, reg.master_set_id).unwrap().unwrap();
        assert_eq!(prov.source_set_id, Some(raw));
        let uuids: Vec<String> = serde_json::from_str(&prov.member_frame_uuids).unwrap();
        assert_eq!(uuids.len(), 3);
    }

    #[test]
    fn register_is_not_repeatable_on_superseded_set() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let raw = seed_source_set(&conn, dir.path());
        let master_path = write_master(dir.path());
        register_master(&conn, raw, &master_path, "{}").unwrap();
        let again = register_master(&conn, raw, &master_path, "{}");
        assert!(again.is_err(), "second registration must be rejected");
    }

    #[test]
    fn direct_registration_matches_scanner_ingestion() {
        // Spec §10 pinning test: register the SAME master file both ways and
        // diff the rows column-by-column (ids/uuids/timestamps excluded).
        let dir = tempfile::tempdir().unwrap();
        let master = write_master(dir.path());

        // Path A: direct registration (fresh DB + fresh source set).
        let conn_a = Connection::open_in_memory().unwrap();
        let raw_a = seed_source_set(&conn_a, dir.path());
        let reg = register_master(&conn_a, raw_a, &master, "{}").unwrap();

        // Path B: scanner ingestion (fresh DB, scan the directory containing
        // ONLY the master file — copy it to an isolated dir first).
        let scan_dir = tempfile::tempdir().unwrap();
        std::fs::copy(&master, scan_dir.path().join("master_dark.fits")).unwrap();
        let conn_b = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn_b).unwrap();
        conn_b.execute("INSERT INTO scan_roots (path) VALUES (?1)",
            [scan_dir.path().to_string_lossy()]).unwrap();
        let _ = crate::scanner::scan_directory(
            scan_dir.path(), &conn_b, None, false, false, 1,
        );

        const FRAME_COLS: &str =
            "imagetyp, is_master, instrume, exptime, gain, offset, binning, ccd_temp";
        let row = |conn: &Connection, where_: &str| -> Vec<Option<String>> {
            conn.query_row(
                &format!("SELECT {FRAME_COLS} FROM frames WHERE {where_}"),
                [],
                |r| Ok((0..8).map(|i| r.get::<_, Option<String>>(i)
                        .unwrap_or_else(|_| r.get::<_, Option<f64>>(i).ok().flatten().map(|v| v.to_string())))
                        .collect()),
            ).unwrap()
        };
        let a = row(&conn_a, &format!("id = {}", reg.master_frame_id));
        let b = row(&conn_b, "is_master = 1");
        assert_eq!(a, b, "direct-registration frame row must equal scanner-ingested frame row");

        const SET_COLS: &str =
            "imagetyp, is_master_library, frame_count, exptime, gain, offset, binning, instrume";
        let set_a: Vec<Option<String>> = conn_a.query_row(
            &format!("SELECT {SET_COLS} FROM calibration_set WHERE id = {}", reg.master_set_id),
            [], |r| Ok((0..8).map(|i| r.get::<_, Option<String>>(i)
                    .unwrap_or_else(|_| r.get::<_, Option<f64>>(i).ok().flatten().map(|v| v.to_string())))
                    .collect())).unwrap();
        let set_b: Vec<Option<String>> = conn_b.query_row(
            &format!("SELECT {SET_COLS} FROM calibration_set WHERE is_master_library = 1"),
            [], |r| Ok((0..8).map(|i| r.get::<_, Option<String>>(i)
                    .unwrap_or_else(|_| r.get::<_, Option<f64>>(i).ok().flatten().map(|v| v.to_string())))
                    .collect())).unwrap();
        assert_eq!(set_a, set_b, "direct-registration set row must equal scanner-ingested set row");
    }
    // (Adjust scan_directory's exact argument list to its real signature —
    // `grep -n "pub fn scan_directory" crates/athenaeum-core/src/scanner/mod.rs` —
    // and the mixed-type column reader to the project's preferred row-diff
    // idiom if one exists; the assertion set is what matters: imagetyp,
    // is_master, camera params, and set shape identical across both paths.)

    #[test]
    fn matcher_excludes_superseded_sets() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let raw = seed_source_set(&conn, dir.path());
        let master_path = write_master(dir.path());
        let reg = register_master(&conn, raw, &master_path, "{}").unwrap();

        // Parse a real frame to drive the matcher.
        let probe = dir.path().join("probe.fits");
        let cards = crate::fits_writer::keywords::HeaderBuilder::new(FrameKind::Light)
            .instrume("TestCam").exptime(300.0).gain(100).offset(50)
            .binning(1, 1).ccd_temp(-10.0)
            .build().unwrap();
        crate::fits_writer::write_fits_f32(&probe, 8, 8, 1, &vec![1.0; 64], &cards).unwrap();
        let frame = crate::fits_parser::parse_fits(&probe, 0).unwrap();
        let config = crate::calibration::config::CalibrationMatchingConfig::default();
        let candidates = crate::calibration::configurable_matcher::find_calibration_candidates(
            &conn, &frame, "lights", "dark", &config,
            crate::calibration::finder::CandidateMode::IncludeIncompatible,
        ).unwrap();
        assert!(candidates.iter().all(|c| c.set_id != raw),
            "superseded raw set must never appear as a candidate");
        assert!(candidates.iter().any(|c| c.set_id == reg.master_set_id),
            "the master must appear instead");
    }
}
```

(Adjust the two `use`-paths for `CandidateMode` / config to the real module exports — `grep -n "pub enum CandidateMode" crates/athenaeum-core/src/calibration/finder.rs` and `grep -n "get_type_config" crates/athenaeum-core/src/calibration/config.rs` confirm; the `"lights"`/`"dark"` string pair matches `find_calibration_candidates`'s existing callers, verify with `grep -rn "find_calibration_candidates(" crates/ | grep -v "fn "`.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core register_master` → FAIL (module missing).

- [ ] **Step 3: Implement `db/master_provenance.rs`** (straightforward CRUD matching the Interfaces block; `insert` uses `INSERT INTO master_provenance … VALUES (?1,…,?6)` with `created_at = chrono::Utc::now().to_rfc3339()` supplied by the caller-facing fn; `update_rebuild` sets `recipe_json`, `member_hash`, `created_at = now`.)

- [ ] **Step 4: Implement `register.rs`**

```rust
//! Direct master registration (spec §3 step 4): rows identical to scanner
//! ingestion BY CONSTRUCTION — the just-written file is parsed with the same
//! fits_parser, inserted with the same db helpers, and turned into a 1:1 set
//! by the same scan_integration function the scanner calls.

use crate::calibration::scan_integration::create_master_sets_from_frames;
use crate::fits_parser::parse_fits_with_header;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use std::path::Path;

pub struct MasterRegistration {
    pub master_set_id: i64,
    pub master_frame_id: i64,
    pub master_file_id: i64,
    pub relinked_links: usize,
}

pub fn member_hash(conn: &Connection, source_set_id: i64) -> Result<(String, Vec<String>)> {
    let mut stmt = conn.prepare(
        "SELECT f.uuid FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         WHERE csf.set_id = ?1 ORDER BY f.uuid",
    )?;
    let uuids: Vec<String> = stmt
        .query_map([source_set_id], |r| r.get::<_, Option<String>>(0))?
        .filter_map(|r| r.ok().flatten())
        .collect();
    let joined = uuids.join(",");
    let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(joined.as_bytes()));
    Ok((hash, uuids))
}

pub fn register_master(
    conn: &Connection,
    source_set_id: i64,
    master_path: &Path,
    recipe_json: &str,
) -> Result<MasterRegistration> {
    let already: Option<i64> = conn.query_row(
        "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
        [source_set_id], |r| r.get(0),
    ).context("source calibration set not found")?;
    if let Some(m) = already {
        bail!("set {source_set_id} is already superseded by master set {m} — use Rebuild");
    }

    let (hash, uuids) = member_hash(conn, source_set_id)?;

    let tx = conn.unchecked_transaction()?;

    // 1. files row — same field sourcing as the scanner (path/filename/size/mtime/format).
    let meta = std::fs::metadata(master_path)?;
    let modified = chrono::DateTime::<chrono::Utc>::from(meta.modified()?).to_rfc3339();
    let path_str = master_path.to_string_lossy().to_string();
    let filename = master_path.file_name().and_then(|s| s.to_str()).unwrap_or("master.fits").to_string();
    tx.execute(
        "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, ?4, 'FITS')",
        rusqlite::params![path_str, filename, meta.len() as i64, modified],
    )?;
    let file_id = tx.last_insert_rowid();

    // 2. frames row + stored header — same parser as the scanner.
    let (frame, header) = parse_fits_with_header(master_path, file_id)
        .map_err(|e| anyhow!("freshly written master failed to parse: {e:#}"))?;
    if !frame.is_master {
        bail!("written master lacks a Master IMAGETYP — header consolidation bug");
    }
    let frame_id = crate::db::insert_frame(&tx, &frame)?;
    crate::db::insert_fits_header(&tx, file_id, &header)?;

    // 3. 1:1 master set — the scanner's own function.
    let imagetyp_str = format!("{:?}", frame.imagetyp); // matches scanner storage form (Debug)
    let created = create_master_sets_from_frames(&tx, &[frame_id], &imagetyp_str)?;
    if created != 1 {
        bail!("expected exactly one master set, created {created}");
    }
    let master_set_id: i64 = tx.query_row(
        "SELECT set_id FROM calibration_set_frames WHERE frame_id = ?1",
        [frame_id], |r| r.get(0),
    )?;

    // 4. provenance
    crate::db::master_provenance::insert(&tx, &crate::db::master_provenance::MasterProvenance {
        master_set_id,
        source_set_id: Some(source_set_id),
        recipe_json: recipe_json.to_string(),
        member_frame_uuids: serde_json::to_string(&uuids)?,
        member_hash: hash,
        created_at: chrono::Utc::now().to_rfc3339(),
    })?;

    // 5. relink every consumer (light links AND sub-cal links targeting the raw set)
    let relinked = tx.execute(
        "UPDATE calibration_set_to_frames SET calibration_set_id = ?1 WHERE calibration_set_id = ?2",
        rusqlite::params![master_set_id, source_set_id],
    )?;

    // 6. supersede
    tx.execute(
        "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
        rusqlite::params![master_set_id, source_set_id],
    )?;

    tx.commit()?;
    Ok(MasterRegistration { master_set_id, master_frame_id: frame_id, master_file_id: file_id, relinked_links: relinked })
}
```

Facts to verify while implementing (compiler + grep enforce): `insert_frame`/`insert_fits_header` take `&Connection` — an `unchecked_transaction` derefs to `&Connection`, so passing `&tx` works; `frames.imagetyp` storage form is the Debug string (`scanner/mod.rs:796-797` comment confirms); `xxhash_rust` is already a dependency (used by `duplicates::compute_xxhash` — check `grep -n xxhash crates/athenaeum-core/Cargo.toml`, reuse the same crate/feature).

- [ ] **Step 5: Make the scanner helper pub + matcher exclusion**

`scan_integration.rs:869`: `fn create_master_sets_from_frames(` → `pub fn create_master_sets_from_frames(`.

`configurable_matcher.rs:353-360` — the candidate query gains the guard:

```rust
    let query = format!(
        "SELECT id, gain, offset, binning, instrume, exptime, focallen, filter,
                ccd_temp, temp_min, temp_max, date_start, date_end, telescop, is_master_library
         FROM calibration_set
         WHERE imagetyp IN ('{}', '{}')
           AND superseded_by_set_id IS NULL
         ORDER BY date_start DESC",
        imagetyp_str, master_imagetyp_str
    );
```

Also sweep the OTHER set-candidate queries that feed auto-link/manual modals: `grep -rn "FROM calibration_set" crates/athenaeum-core/src/calibration/ crates/athenaeum-core/src/api/calibration.rs | grep -v calibration_set_frames | grep -v calibration_set_to_frames`. Add `superseded_by_set_id IS NULL` to those that enumerate candidate sets (manual-selection lists at `api/calibration.rs:377` and `:777`), NOT to by-id lookups.

- [ ] **Step 6: Run tests** — `cargo test -p athenaeum-core register_master && cargo test -p athenaeum-core` → all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/
git commit -m "feat(library): atomic master registration — scanner-equivalent rows, provenance, relink, supersede; matcher excludes superseded sets"
```

---

### Task 12: `api::masters` — build orchestration + wrappers

**Files:**
- Create: `crates/athenaeum-core/src/api/masters.rs` (+ `pub mod masters;` in `api/mod.rs`)
- Modify: `crates/athenaeum-core/src/services/mod.rs` (`MasterBuildHandle` + `active_master_builds`)
- Create: `crates/athenaeum-tauri/src/commands/masters.rs` (+ registration in `lib.rs`, `commands/mod.rs`)
- Create: `crates/athenaeum-web/src/routes/masters.rs` (+ registration in `routes/mod.rs`)
- Modify: `crates/athenaeum-core/src/ts_export.rs`

**Interfaces:**
- Produces:

```rust
// services/mod.rs
pub struct MasterBuildHandle { pub cancel_flag: Arc<AtomicBool> }
// ServiceContext gains:
pub active_master_builds: Arc<Mutex<HashMap<i64, MasterBuildHandle>>>,  // keyed by SOURCE set_id

// api/masters.rs
pub const MIN_MASTER_FRAMES: i64 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterRecipe {
    /// None => Auto (per-type/per-N rule from spec §9).
    pub combine: Option<crate::integration::combine::CombineMethod>,
    /// Constant-ADU fallback for flat pre-calibration when no darkflat/dark/bias master is linked.
    pub synthetic_bias: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterBuildPreview {
    pub set_id: i64,
    pub imagetyp: String,
    pub frame_count: i64,
    pub resolved_combine: crate::integration::combine::CombineMethod,
    pub flat_precal: Option<String>,       // human description: "master darkflat #12" | "synthetic bias 500 ADU" | null
    pub target_path: String,               // absolute, collision-resolved
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterProvenanceInfo {
    pub master_set_id: i64,
    pub source_set_id: Option<i64>,
    pub recipe_json: String,
    pub member_count: usize,
    pub member_hash: String,
    pub created_at: String,
    pub source_frames_on_disk: bool,       // rebuild possible?
    pub originals_archived: bool,          // any source file has archive markers
}

pub fn preview_master_build(ctx: &ServiceContext, set_id: i64, recipe: &MasterRecipe)
    -> Result<MasterBuildPreview, ApiError>;
/// Validates, registers the cancel handle, spawns the detached build thread
/// (queue admission inside), returns immediately.
pub fn start_master_build(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    set_id: i64,
    recipe: MasterRecipe,
) -> Result<(), ApiError>;
pub fn cancel_master_build(ctx: &ServiceContext, set_id: i64) -> Result<(), ApiError>;
pub fn get_master_provenance(ctx: &ServiceContext, master_set_id: i64)
    -> Result<Option<MasterProvenanceInfo>, ApiError>;
```

- Events (snake_case payloads, analysis precedent):
  - `master-build-progress`: `{ set_id, stage, current, total, percent }` — `stage` ∈ `"reading" | "integrating" | "writing" | "registering"` (integrating drives current/total from bands).
  - `master-build-complete`: `{ set_id, master_set_id, success, cancelled, error }`.

- [ ] **Step 1: Write the failing test** (recipe resolution is the unit-testable core; the threaded path is covered by an integration test)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::combine::CombineMethod;

    #[test]
    fn auto_recipe_rules() {
        // spec §9: bias-like N>=15 winsorized else median; flat N>=15 winsorized else percentile
        assert_eq!(resolve_combine(None, "Dark", 20),
            CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 });
        assert_eq!(resolve_combine(None, "Dark", 5), CombineMethod::Median);
        assert_eq!(resolve_combine(None, "Bias", 14), CombineMethod::Median);
        assert_eq!(resolve_combine(None, "Flat", 20),
            CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 });
        assert_eq!(resolve_combine(None, "Flat", 6),
            CombineMethod::PercentileClip { low: 0.2, high: 0.02 });
        // explicit override wins
        assert_eq!(resolve_combine(Some(CombineMethod::Mean), "Flat", 6), CombineMethod::Mean);
    }
}
```

- [ ] **Step 2: Run to verify failure** — module missing.

- [ ] **Step 3: Implement `api/masters.rs`**

Complete build-thread flow (this is the whole file's core; error handling funnels through one `finish` closure so the handle is always removed and `master-build-complete` always fires):

```rust
pub fn resolve_combine(explicit: Option<CombineMethod>, imagetyp: &str, n: i64) -> CombineMethod {
    if let Some(m) = explicit { return m; }
    let is_flat = imagetyp == "Flat";
    if n >= 15 {
        CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 }
    } else if is_flat {
        CombineMethod::PercentileClip { low: 0.2, high: 0.02 }
    } else {
        CombineMethod::Median
    }
}

/// Flat pre-cal resolution per spec §9 fallback chain. Returns
/// (FlatPrecal, human_description, warnings). Called INSIDE the build thread
/// so a just-built darkflat master (earlier in a batch) is visible.
fn resolve_flat_precal(
    conn: &rusqlite::Connection,
    set_id: i64,
    set_exptime: Option<f64>,
    synthetic_bias: Option<f64>,
) -> Result<(FlatPrecal, Option<String>, Vec<String>), ApiError> {
    let mut warnings = Vec::new();
    // sub-cal links of this flat set, by type preference
    for cal_type in ["DarkFlat", "Dark", "Bias"] {
        let row: Option<(i64, String, i64, Option<f64>)> = conn.query_row(
            "SELECT cs.id, cs.imagetyp, cs.is_master_library, cs.exptime
             FROM calibration_set_to_frames l
             JOIN calibration_set cs ON cs.id = l.calibration_set_id
             WHERE l.source_id = ?1 AND l.source_type = 'calibration_set'
               AND l.calibration_type = ?2",
            rusqlite::params![set_id, cal_type],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional()?;
        let Some((precal_set, imagetyp, is_master, precal_expt)) = row else { continue };
        if is_master != 1 {
            warnings.push(format!(
                "linked {cal_type} set #{precal_set} is raw — build its master first (skipped)"));
            continue;
        }
        if cal_type == "Dark" {
            // exposure-matched raw dark only (spec §9): the matcher enforced
            // exptime at link time, re-verify defensively.
            match (set_exptime, precal_expt) {
                (Some(a), Some(b)) if (a - b).abs() <= 0.5 => {}
                _ => {
                    warnings.push(format!(
                        "linked dark master #{precal_set} exposure does not match the flats — skipped"));
                    continue;
                }
            }
        }
        // load the master's pixels (single file, fits via the banded reader in one band)
        let path: String = conn.query_row(
            "SELECT fi.path FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1 LIMIT 1",
            [precal_set], |r| r.get(0),
        )?;
        let scratch = std::env::temp_dir();
        let mut src = crate::integration::banded::BandSource::open(
            &[std::path::PathBuf::from(&path)], &scratch,
        ).map_err(|e| ApiError::Internal(format!("pre-cal master unreadable: {e}")))?;
        let (w, h) = (src.width(), src.height());
        let mut bufs = vec![Vec::new()];
        src.read_band(0, h, &mut bufs).map_err(|e| ApiError::Internal(e.to_string()))?;
        return Ok((
            FlatPrecal::MasterFrame { data: std::mem::take(&mut bufs[0]), width: w, height: h },
            Some(format!("{} master #{precal_set} ({imagetyp})", cal_type.to_lowercase())),
            warnings,
        ));
    }
    if let Some(b) = synthetic_bias {
        return Ok((FlatPrecal::SyntheticBias(b as f32), Some(format!("synthetic bias {b} ADU")), warnings));
    }
    warnings.push("no pre-calibration master linked and no synthetic bias set — flat combined un-pre-calibrated (vignetting zero level slightly off)".into());
    Ok((FlatPrecal::None, None, warnings))
}
```

`start_master_build` body (validation → handle → thread):

1. Validation (Conflict/Invalid errors, same style as analysis): set exists AND `superseded_by_set_id IS NULL` AND `is_master_library = 0`; `frame_count >= MIN_MASTER_FRAMES`; library root configured (`api::scan_roots::get_calibration_library_root`); no active build for `set_id` (`active_master_builds` guard identical to `active_analyses`).
2. Insert `MasterBuildHandle` with a fresh cancel flag.
3. `std::thread::Builder::new().name(format!("master-build-{set_id}"))` spawn — inside:
   - `let (permit, _job) = ctx.compute_queue.acquire(ComputeJobKind::MasterBuild, &label, cancel_flag.clone())` — on `Err(QueueCancelled)` emit `master-build-complete { cancelled: true }`, remove handle, return.
   - open a DB conn (`ctx.db.get()` — the thread-safe `Database` type is the same one the archive worker uses in its closure: `crates/athenaeum-tauri/src/commands/archive.rs:266-267`).
   - load member paths (`SELECT fi.path FROM calibration_set_frames csf JOIN frames f ON f.id=csf.frame_id JOIN files fi ON fi.id=f.file_id WHERE csf.set_id=?1 ORDER BY fi.path`); load set row (imagetyp, exptime, instrume, filter, gain, binning, ccd_temp, date, frame_count).
   - resolve combine + (flats only) `resolve_flat_precal`.
   - emit `master-build-progress {stage:"integrating"}` from the engine's `on_band` callback (`current=bands_done, total=bands_total, percent`).
   - run `integrate_bias_like` / `integrate_flat` with `&ctx.image_pool`, scratch dir = `std::env::temp_dir()`.
   - `stage:"writing"`: `load_header_inputs` + `build_master_cards(inputs, &app_version, &recipe_summary, &member_hash, out.flat_norm)`; target = library_root path + `master_relative_path` + `resolve_collision`; `std::fs::create_dir_all(parent)`; `write_fits_f32`.
   - `stage:"registering"`: `register_master(&conn, set_id, &target, &recipe_json)`. `recipe_json` = `serde_json::json!({ "combine": resolved_combine, "syntheticBias": recipe.synthetic_bias, "precal": precal_desc, "rejectedFraction": out.rejected_fraction, "engine": "athenaeum", "version": app_version }).to_string()`. On failure REMOVE the written file (`std::fs::remove_file(&target)`) so no orphan master sits in the library unregistered.
   - emit `master-build-complete { set_id, master_set_id: Some(id), success: true, cancelled: false, error: null }`; on any error emit with `success:false, error:Some(msg)`; on `IntegrationError::Cancelled` emit `cancelled:true`. Always remove the handle (do it in a drop-guard struct or a single exit path).
4. Return `Ok(())`.

`cancel_master_build`: set the flag via `active_master_builds` (NotFound otherwise).

`preview_master_build`: same validation + combine resolution + precal resolution (speculative, warnings included) + target-path computation — no thread, returns `MasterBuildPreview`.

`get_master_provenance`: join `master_provenance` + source-set file states:

```sql
SELECT COUNT(*) FROM calibration_set_frames csf
JOIN frames f ON f.id = csf.frame_id
JOIN files fi ON fi.id = f.file_id
WHERE csf.set_id = ?1 AND fi.archived_in_operation IS NOT NULL   -- originals_archived
```
and `source_frames_on_disk` = every source file path exists on disk (`std::path::Path::exists`).

- [ ] **Step 4: Wrappers + registration + ts**

Tauri `commands/masters.rs` — five thin commands (`preview_master_build`, `start_master_build`, `cancel_master_build`, `get_master_provenance`, plus Task 13's batch): `start_master_build` wrapper builds `Arc::new(TauriProgressEmitter(app.clone()))` and passes `state.ctx.clone()` + `env!("CARGO_PKG_VERSION").to_string()`. Web mirror with `Arc::new(SseProgressEmitter { tx })`. Register everywhere. ts registry additions (models.ts block):

```rust
            crate::integration::combine::CombineMethod,
            crate::api::masters::MasterRecipe,
            crate::api::masters::MasterBuildPreview,
            crate::api::masters::MasterProvenanceInfo,
```

Run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.

- [ ] **Step 5: Integration test** (append to `register.rs` tests or a new `tests/master_build.rs` in athenaeum-core if a full-context harness exists — check `ls crates/athenaeum-core/tests/`): seed a source set with three REAL 8×8 dark FITS (Task 11's `seed_source_set` already does), call the internal build steps synchronously (integrate → write → register) and assert the final master parses with `ATH_SRC == source uuid`, `ATH_N == 3`, `IMAGETYP == "Master Dark"` via `fits_parser::extract_fits_header`.

- [ ] **Step 6: Run everything** — `cargo test -p athenaeum-core && cargo build -p athenaeum-tauri -p athenaeum-web && npx tsc --noEmit` → PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/ src/types/
git commit -m "feat(masters): master build orchestration on the compute queue — preview/start/cancel/provenance"
```

---

### Task 13: Batch builds, dependency ordering, rebuild

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs`
- Modify: wrappers + registration (both transports)

**Interfaces:**
- Produces:

```rust
/// Enqueue builds for many sets, dependency-ordered: Bias & DarkFlat first,
/// then Dark, then Flat (flats resolve pre-cal at RUN time, so masters built
/// earlier in the batch are found). Sets already superseded / too small are
/// skipped with a per-set reason.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BatchBuildReport { pub started_set_ids: Vec<i64>, pub skipped: Vec<BatchSkip> }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BatchSkip { pub set_id: i64, pub reason: String }

pub fn start_master_builds_batch(
    ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, app_version: String,
    set_ids: Vec<i64>, recipe: MasterRecipe,
) -> Result<BatchBuildReport, ApiError>;

/// Re-integrate an existing built master in place (same path), refresh provenance.
pub fn rebuild_master(
    ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, app_version: String,
    master_set_id: i64,
) -> Result<(), ApiError>;
```

- [ ] **Step 1: Failing test — ordering is pure, test it directly**

```rust
    #[test]
    fn batch_order_bias_darkflat_dark_flat() {
        let order = |t: &str| type_build_rank(t);
        assert!(order("Bias") < order("Dark"));
        assert!(order("DarkFlat") < order("Dark"));
        assert!(order("Dark") < order("Flat"));
    }
```

- [ ] **Step 2: Implement**

```rust
pub(crate) fn type_build_rank(imagetyp: &str) -> u8 {
    match imagetyp {
        "Bias" | "DarkFlat" => 0,
        "Dark" => 1,
        "Flat" => 2,
        _ => 3,
    }
}
```

`start_master_builds_batch`: load `(id, imagetyp, frame_count, superseded_by_set_id, is_master_library)` for every requested id; skip with reasons (`"already has a master"`, `"only N frames (minimum 3)"`, `"is itself a master"`, `"unknown set"`); sort remaining by `(type_build_rank, id)`; call `start_master_build` for each in order (they enqueue instantly and the compute queue serializes execution in submission order — FIFO guarantees darkflats finish before flats run). Collect per-set `Err` from `start_master_build` (e.g. a Conflict from a concurrent click) into `skipped` instead of failing the whole batch.

`rebuild_master`: require an existing provenance row with `source_set_id: Some(src)`; require every source file on disk (else `Invalid("originals are archived — restore them first")`); spawn the same build thread with two differences: target path = the master's EXISTING `files.path` (no collision suffix — `write_fits_f32` replaces atomically), and instead of `register_master` call `db::master_provenance::update_rebuild(...)` followed by a direct SQL refresh of the file row:

```rust
tx.execute(
    "UPDATE files SET size = ?1, modified_at = ?2 WHERE id = ?3",
    rusqlite::params![new_len as i64, new_mtime_rfc3339, master_file_id],
)?;
```

`frames` metadata is untouched — a rebuild changes pixels, not header-derived fields (the header is re-consolidated from the same source set). Links stay untouched.

Batch command wrappers: `start_master_builds_batch` + `rebuild_master` on both transports; ts registry: `BatchBuildReport`, `BatchSkip`; regenerate.

- [ ] **Step 3: Run + commit**

Run: `cargo test -p athenaeum-core && npx tsc --noEmit` → PASS.

```bash
git add crates/ src/types/
git commit -m "feat(masters): dependency-ordered batch builds and in-place rebuild"
```

---

### Task 14: Archive-of-originals for superseded sets

**Files:**
- Modify: `crates/athenaeum-core/src/archive/models.rs` (`ArchivePlan.calibration_set_id`, `ArchiveOperation.frames_set_id: Option<i64>` + `calibration_set_id`)
- Modify: `crates/athenaeum-core/src/archive/path_layout.rs` (calibration dir + filename)
- Modify: `crates/athenaeum-core/src/archive/planner.rs` (`build_calibration_set_plan`, `commit_plan` subject-aware)
- Modify: `crates/athenaeum-core/src/archive/db.rs` (`insert_operation` subject params; `get_operation` mapper)
- Modify: `crates/athenaeum-core/src/archive/executor.rs` + `rollback.rs` + `restore.rs` (skip frame-set marking when `frames_set_id` is NULL)
- Modify: `crates/athenaeum-core/src/api/masters.rs` (`archive_originals` + `MasterRecipe.archive_after` chaining)
- Modify: wrappers (both transports), ts regen

**Interfaces:**
- Produces:

```rust
// path_layout.rs
/// "Calibration_Archive/<INSTRUME sanitized>/<YYYY-MM-DD>" (date = set date_start date part)
pub fn calibration_zip_dir(instrume: Option<&str>, date_start: &str) -> std::path::PathBuf;
/// "<Camera>_<Type>_g<gain>_<exptime>s_<date_start>_<date_end>.zip" (missing tokens collapse)
pub fn calibration_zip_filename(instrume: Option<&str>, imagetyp: &str,
    gain: Option<f64>, exptime: Option<f64>, date_start: &str, date_end: &str) -> String;

// planner.rs
/// Plan archiving a SUPERSEDED calibration set's original frames. Guards:
/// set must be superseded; errors if any member file is already archived or
/// missing on disk. All files disposition=Move, one zip.
pub fn build_calibration_set_plan(conn: &rusqlite::Connection, calibration_set_id: i64,
    archive_root_path: &std::path::Path, compression: ArchiveCompression)
    -> anyhow::Result<ArchivePlan>;

// api/masters.rs
/// Plan+commit+enqueue on the DISK worker (operation_queue), emitting the
/// existing archive-progress / archive-finished events. Returns operation_id.
pub fn archive_originals(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>,
    calibration_set_id: i64) -> Result<i64, ApiError>;
```

- `MasterRecipe` gains `pub archive_after: bool` (default false); on build success the build thread calls `archive_originals`.

- [ ] **Step 1: Write the failing tests** (planner-level, in `planner.rs`'s test module; executor round-trip in the existing archive test style)

```rust
    #[test]
    fn calibration_plan_requires_superseded_set() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        // raw set with one real file, NOT superseded
        conn.execute("INSERT INTO calibration_set (imagetyp, date, instrume, date_start, date_end)
                      VALUES ('Dark','2026-06-28','Cam','2026-06-28T20:00:00Z','2026-06-28T21:00:00Z')", []).unwrap();
        let set = conn.last_insert_rowid();
        let r = build_calibration_set_plan(&conn, set, dir.path(), ArchiveCompression::Store);
        assert!(r.is_err(), "non-superseded set must be rejected");
    }

    #[test]
    fn calibration_plan_layout() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES (?1)",
            [dir.path().to_string_lossy()]).unwrap();
        conn.execute("INSERT INTO calibration_set
            (imagetyp, date, instrume, gain, exptime, date_start, date_end, superseded_by_set_id)
            VALUES ('Dark','2026-06-28','Test Cam',100.0,300.0,
                    '2026-06-28T20:00:00Z','2026-06-28T21:00:00Z', 999)", []).unwrap();
        let set = conn.last_insert_rowid();
        let f = dir.path().join("d1.fits");
        std::fs::write(&f, b"data").unwrap();
        conn.execute("INSERT INTO files (path, filename, size, modified_at, format)
                      VALUES (?1,'d1.fits',4,'2026-06-28','FITS')",
            [f.to_string_lossy()]).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO frames (file_id, imagetyp) VALUES (?1,'Dark')", [file_id]).unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1,?2)",
            rusqlite::params![set, frame_id]).unwrap();

        let plan = build_calibration_set_plan(&conn, set, dir.path(), ArchiveCompression::Store).unwrap();
        assert_eq!(plan.calibration_set_id, Some(set));
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.zips.len(), 1);
        let zp = &plan.zips[0].zip_path;
        assert!(zp.contains("Calibration_Archive"), "{zp}");
        assert!(zp.contains("2026-06-28"), "date dir: {zp}");
        assert!(plan.files.iter().all(|f| f.disposition == "move"));
    }
```

- [ ] **Step 2: Run to verify failure** — functions missing.

- [ ] **Step 3: Implement**

`models.rs`: `ArchivePlan` gains `pub calibration_set_id: Option<i64>` (existing constructor site in `build_plan` sets `None`); `ArchiveOperation.frames_set_id: i64` → `Option<i64>` and add `pub calibration_set_id: Option<i64>` — compiler drives the mapper updates in `archive/db.rs::get_operation` / `list_unfinished_operations` (frame_set_name becomes `None` for calibration ops — LEFT JOIN already tolerates it after switching the join to `LEFT JOIN frames_set fs ON fs.id = ao.frames_set_id`).

`path_layout.rs`:

```rust
pub fn calibration_zip_dir(instrume: Option<&str>, date_start: &str) -> PathBuf {
    let cam = instrume.map(sanitize_for_filename).filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UnknownCamera".into());
    let date = date_start.get(..10).unwrap_or("unknown-date");
    PathBuf::from("Calibration_Archive").join(cam).join(date)
}

pub fn calibration_zip_filename(instrume: Option<&str>, imagetyp: &str,
    gain: Option<f64>, exptime: Option<f64>, date_start: &str, date_end: &str) -> String {
    let cam = instrume.map(sanitize_for_filename).filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UnknownCamera".into());
    let mut parts = vec![cam, sanitize_for_filename(imagetyp)];
    if let Some(g) = gain { parts.push(format!("g{}", g.round() as i64)); }
    if let Some(e) = exptime { parts.push(format!("{}s", e)); }
    parts.push(date_start.get(..10).unwrap_or("x").to_string());
    parts.push(date_end.get(..10).unwrap_or("x").to_string());
    format!("{}.zip", parts.join("_"))
}
```

`planner.rs::build_calibration_set_plan`: load the set row (bail unless `superseded_by_set_id IS NOT NULL`); collect member files via `calibration_set_frames → frames → files` where `archived_in_operation IS NULL` (bail listing already-archived files if any member IS archived — partial archive of a set is not a thing); every file must exist on disk (reuse `build_plan`'s existence bail); role from imagetyp (`Dark→FrameRole::Dark`, `Flat→Flat`, `Bias→Bias`, `DarkFlat→Darkflat`); path_in_zip via the SAME `resolve_scan_root_prefixes` + `path_in_zip` machinery; zip path = `archive_root.join(calibration_zip_dir(...)).join(calibration_zip_filename(...))`; hash every file; single `PlannedZip`; conflicts = zip exists on disk. Fill `Dispositions` with the set's own type = Move, others None. `frames_set_id: 0` in the DTO with `calibration_set_id: Some(set_id)` (documented: the DB row stores NULL — the DTO keeps its numeric field for wire-compat, consumers must check `calibration_set_id` first).

`planner.rs::commit_plan` + `archive/db.rs::insert_operation`: change signature to

```rust
pub fn insert_operation(conn, frames_set_id: Option<i64>, calibration_set_id: Option<i64>, archive_root_path: &str, ...)
```
Planner asserts exactly-one-of at commit time (`anyhow::ensure!`). Update the existing frame-set call site to `Some(frames_set_id), None`.

`executor.rs` finalize phase: locate `mark_frame_set_archived` call (`grep -n mark_frame_set_archived crates/athenaeum-core/src/archive/executor.rs`) and wrap:

```rust
    let op = adb::get_operation(conn, operation_id)?;
    if let Some(fs_id) = op.frames_set_id {
        adb::mark_frame_set_archived(conn, fs_id, operation_id)?;
    }
```
Same guard in `rollback.rs` (`unmark_frame_set_archived`) and `restore.rs`'s catalog-update stage (`grep -n "unmark_frame_set_archived\|clear_zip_markers" crates/athenaeum-core/src/archive/`). Per-file `mark_file_archived`/`unmark_file_archived` paths are subject-agnostic and stay untouched — this is exactly why restore keeps working for calibration originals.

`api/masters.rs::archive_originals`: resolve archive root exactly like the Tauri archive command does (find `resolve_archive_root` — it lives in `commands/archive.rs`; MOVE that helper into `api` (new `api/archive_support.rs` or a pub fn in core `archive::mod`) so both the legacy command and this new handler share it); `build_calibration_set_plan` + `commit_plan(conflict = AddSuffix)`; register `ArchiveHandle` in `ctx.active_archives`; enqueue `QueuedJob { kind: OperationKind::ZipArchive, … }` whose closure mirrors `commands/archive.rs:264-301` (run→status→rollback-on-error→`archive-finished`→handle cleanup) but lives in core using the passed `Arc<dyn ProgressEmitter>`.

`MasterRecipe` gains `#[serde(default)] pub archive_after: bool`; the Task-12 build thread, after a successful `register_master`, calls `archive_originals(ctx.clone(), emitter.clone(), set_id)` and logs (does not fail the build) on error.

Wrappers: `archive_calibration_originals` command/route (thin, calls `api::masters::archive_originals`); regenerate ts (ArchivePlan gained a field).

- [ ] **Step 4: Executor round-trip test** (append to planner tests): commit the layout-test plan, run `executor::run_operation` with `NullEmitter` + never-cancelled flag, then assert: zip exists at planned path, source file deleted, `files.archive_zip_path` set, `frames`/set rows intact, `master_provenance` untouched. Then `restore::run_restore` (RestoreTargetMode::Original) and assert the file is back and markers cleared.

- [ ] **Step 5: Run + commit**

Run: `cargo test -p athenaeum-core && npx tsc --noEmit`
Expected: PASS incl. all pre-existing archive tests (frame-set path unaffected).

```bash
git add crates/ src/types/
git commit -m "feat(archive): calibration-set subject — Calibration_Archive layout, executor/restore subject-aware, archive-after-build chaining"
```

---

### Task 15: Frontend — master-build progress context + global compute-queue indicator

**Files:**
- Modify: `src/types/helpers.ts` (event interfaces)
- Create: `src/hooks/useMasterBuilds.ts`
- Create: `src/contexts/MasterBuildContext.tsx`
- Create: `src/components/ComputeQueueIndicator.tsx`
- Modify: `src/components/Layout.tsx` (provider + indicator wiring)

**Interfaces:**
- Consumes: events `master-build-progress` / `master-build-complete` / `compute-queue-changed` (Tasks 5, 12), commands `get_compute_queue`, `cancel_compute_job`, `start_master_build`, `start_master_builds_batch`, `cancel_master_build`; generated types `ComputeQueueEntry`, `MasterRecipe`, `BatchBuildReport` from `types/models`.
- Produces: `useMasterBuildContext()` with `{ startBuild, startBatch, cancelBuild, buildStates, isBuilding }` — consumed by Tasks 16–17.

- [ ] **Step 1: Event types in `helpers.ts`** (hand-written file — snake_case fields, matching the Rust payloads exactly)

```ts
export interface MasterBuildProgressEvent {
  set_id: number;
  stage: 'reading' | 'integrating' | 'writing' | 'registering';
  current: number;
  total: number;
  percent: number;
}

export interface MasterBuildCompleteEvent {
  set_id: number;
  master_set_id: number | null;
  success: boolean;
  cancelled: boolean;
  error: string | null;
}
```

- [ ] **Step 2: `useMasterBuilds.ts`** — mirror `useAnalysisProgress.ts`'s listener discipline (mount-once listeners, cancelled-cleanup, `notify()` on completion). Backend owns the queue, so NO frontend FIFO — `startBuild` fire-and-forgets the invoke:

```ts
import { useState, useEffect, useCallback } from 'react';
import { api } from '../api';
import type { MasterBuildProgressEvent, MasterBuildCompleteEvent } from '../types/helpers';
import type { MasterRecipe, BatchBuildReport } from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';

export type BuildState =
  | { phase: 'starting' }
  | { phase: 'building'; progress: MasterBuildProgressEvent }
  | { phase: 'done'; result: MasterBuildCompleteEvent };

export function useMasterBuilds() {
  const [buildStates, setBuildStates] = useState<Map<number, BuildState>>(new Map());
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    let un1: (() => void) | null = null;
    let un2: (() => void) | null = null;
    api.listen<MasterBuildProgressEvent>('master-build-progress', (p) => {
      if (cancelled) return;
      setBuildStates(prev => new Map(prev).set(p.set_id, { phase: 'building', progress: p }));
    }).then(fn => { if (cancelled) fn(); else un1 = fn; });
    api.listen<MasterBuildCompleteEvent>('master-build-complete', (p) => {
      if (cancelled) return;
      setBuildStates(prev => new Map(prev).set(p.set_id, { phase: 'done', result: p }));
      notify({
        title: p.cancelled ? 'Master build cancelled'
          : p.success ? 'Master created' : 'Master build failed',
        detail: p.success ? `Set #${p.set_id} → master set #${p.master_set_id}` : (p.error ?? ''),
        kind: 'analysis',
        hasErrors: !p.success && !p.cancelled,
        tone: p.success ? 'success' : p.cancelled ? 'info' : 'warning',
      });
      // The set list changed shape (raw → superseded + new master row).
      window.dispatchEvent(new Event('library-updated'));
    }).then(fn => { if (cancelled) fn(); else un2 = fn; });
    return () => { cancelled = true; un1?.(); un2?.(); };
  }, [notify]);

  const startBuild = useCallback(async (setId: number, recipe: MasterRecipe) => {
    setBuildStates(prev => new Map(prev).set(setId, { phase: 'starting' }));
    try {
      await api.invoke('start_master_build', { setId, recipe });
    } catch (err) {
      setBuildStates(prev => new Map(prev).set(setId, {
        phase: 'done',
        result: { set_id: setId, master_set_id: null, success: false, cancelled: false, error: String(err) },
      }));
      throw err;
    }
  }, []);

  const startBatch = useCallback(async (setIds: number[], recipe: MasterRecipe): Promise<BatchBuildReport> => {
    setBuildStates(prev => {
      const next = new Map(prev);
      for (const id of setIds) next.set(id, { phase: 'starting' });
      return next;
    });
    return api.invoke<BatchBuildReport>('start_master_builds_batch', { setIds, recipe });
  }, []);

  const cancelBuild = useCallback(async (setId: number) => {
    try { await api.invoke('cancel_master_build', { setId }); } catch { /* may have finished */ }
  }, []);

  const isBuilding = useCallback((setId: number) => {
    const s = buildStates.get(setId);
    return !!s && s.phase !== 'done';
  }, [buildStates]);

  return { buildStates, startBuild, startBatch, cancelBuild, isBuilding };
}
```

(Command arg names: Tauri camelCases — verify against the wrapper's Rust param names once Task 12's wrappers exist; follow the invoke style of `analyze_frame_set` at `useAnalysisProgress.ts:39-42`.)

- [ ] **Step 3: `MasterBuildContext.tsx`** — byte-for-byte the `AnalysisProgressContext.tsx` pattern (23 lines) around `useMasterBuilds`.

- [ ] **Step 4: `ComputeQueueIndicator.tsx`** — sidebar card listing running + queued compute jobs (all kinds) with cancel buttons; mirrors `AnalysisQueueIndicator`'s collapsed/expanded rendering:

```tsx
import { useState, useEffect } from 'react';
import { Layers, X } from 'lucide-react';
import { api } from '../api';
import type { ComputeQueueEntry } from '../types/models';

export function ComputeQueueIndicator({ collapsed }: { collapsed: boolean }) {
  const [entries, setEntries] = useState<ComputeQueueEntry[]>([]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    api.invoke<ComputeQueueEntry[]>('get_compute_queue')
      .then(e => { if (!cancelled) setEntries(e); })
      .catch(() => {});
    api.listen<{ entries: ComputeQueueEntry[] }>('compute-queue-changed', (p) => {
      if (!cancelled) setEntries(p.entries);
    }).then(fn => { if (cancelled) fn(); else unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  // Only surface when a non-analysis job is present — analysis already has
  // its own indicator; duplicating it reads as two running jobs.
  const visible = entries.filter(e => e.kind !== 'analysis');
  if (visible.length === 0) return null;

  const cancel = (jobId: number) => api.invoke('cancel_compute_job', { jobId }).catch(() => {});

  if (collapsed) {
    return (
      <div className="px-2 pb-2" title={visible.map(e => e.label).join(', ')}>
        <div className="relative flex items-center justify-center py-3">
          <Layers size={20} className="text-accent" />
          <span className="absolute -top-0.5 -right-0.5 bg-accent text-surface text-[9px] font-bold rounded-full w-3.5 h-3.5 flex items-center justify-center">
            {visible.length}
          </span>
        </div>
      </div>
    );
  }

  return (
    <div className="px-4 pb-2">
      <div className="bg-surface rounded-lg p-2.5 border border-border space-y-1.5">
        {visible.map(e => (
          <div key={e.jobId} className="flex items-center justify-between gap-1.5">
            <div className="flex items-center gap-1.5 min-w-0">
              <Layers size={14} className={e.state === 'running' ? 'text-accent' : 'text-content-muted'} />
              <span className="text-xs text-content-secondary truncate" title={e.label}>{e.label}</span>
            </div>
            <div className="flex items-center gap-1.5 shrink-0">
              <span className="text-[10px] text-content-muted">{e.state === 'running' ? 'running' : 'queued'}</span>
              <button onClick={() => cancel(e.jobId)} title="Cancel"
                className="text-content-muted hover:text-content transition-colors">
                <X size={12} />
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
```

- [ ] **Step 5: Layout wiring** — in `Layout.tsx`: import + wrap `<MasterBuildProvider>` inside the provider nest (after `RegistrationProgressProvider`, `:49`), and mount `<ComputeQueueIndicator collapsed={collapsed} />` right after `<AnalysisQueueIndicator …/>` (`:85`). Close the provider tag in the matching position (`:119`).

- [ ] **Step 6: Verify + commit**

Run: `npx tsc --noEmit` → PASS. Dev smoke: start a master build (via devtools `api.invoke` until Task 16 lands) → indicator shows it; queue a second → shows "queued"; cancel works.

```bash
git add src/
git commit -m "feat(ui): master-build context + global compute-queue sidebar indicator"
```

---

### Task 16: Frontend — Create Master dialog + Equipment integration

**Files:**
- Create: `src/components/calibration/CreateMasterDialog.tsx`
- Modify: `src/components/CalibrationSetTable.tsx` (action button, superseded dimming, provenance block)
- Modify: `src/components/CameraDetail.tsx` (dialog state, handler threading)
- Modify: `src/components/DarkLibrary.tsx`, `src/components/MasterDarkLibrary.tsx`, `src/components/MasterFlatLibrary.tsx` (prop threading — confirm each renders `CalibrationSetTable`: `grep -ln "CalibrationSetTable" src/components/*.tsx`)

**Interfaces:**
- Consumes: `preview_master_build`, `get_master_provenance`, `archive_calibration_originals` commands; `useMasterBuildContext()`.
- Produces: `CalibrationSetTableProps` gains `onCreateMaster?: (setId: number) => void` and `buildingSetIds?: number[]`; `CreateMasterDialogProps = { setIds: number[]; onClose: () => void }`.

- [ ] **Step 1: `CreateMasterDialog.tsx`** (complete component)

```tsx
import { useState, useEffect } from 'react';
import { X, Hammer, AlertTriangle } from 'lucide-react';
import { api } from '../../api';
import type { MasterBuildPreview, MasterRecipe, CombineMethod } from '../../types/models';
import { useMasterBuildContext } from '../../contexts/MasterBuildContext';

interface CreateMasterDialogProps {
  setIds: number[];          // 1 = single set, >1 = batch
  onClose: () => void;
}

type CombineChoice = 'auto' | 'mean' | 'median' | 'winsorized' | 'percentile';

function toCombineMethod(c: CombineChoice, sigLo: number, sigHi: number, pLo: number, pHi: number): CombineMethod | null {
  switch (c) {
    case 'auto': return null;
    case 'mean': return { method: 'mean' };
    case 'median': return { method: 'median' };
    case 'winsorized': return { method: 'winsorized_sigma_clip', sigma_low: sigLo, sigma_high: sigHi };
    case 'percentile': return { method: 'percentile_clip', low: pLo, high: pHi };
  }
}

export function CreateMasterDialog({ setIds, onClose }: CreateMasterDialogProps) {
  const { startBuild, startBatch } = useMasterBuildContext();
  const single = setIds.length === 1;
  const [preview, setPreview] = useState<MasterBuildPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [combine, setCombine] = useState<CombineChoice>('auto');
  const [sigLo, setSigLo] = useState(3.0);
  const [sigHi, setSigHi] = useState(3.0);
  const [pLo, setPLo] = useState(0.2);
  const [pHi, setPHi] = useState(0.02);
  const [syntheticBias, setSyntheticBias] = useState<string>('');
  const [archiveAfter, setArchiveAfter] = useState(false);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);

  const recipe = (): MasterRecipe => ({
    combine: toCombineMethod(combine, sigLo, sigHi, pLo, pHi),
    syntheticBias: syntheticBias.trim() === '' ? null : Number(syntheticBias),
    archiveAfter,
  });

  useEffect(() => {
    if (!single) return;
    api.invoke<MasterBuildPreview>('preview_master_build', { setId: setIds[0], recipe: recipe() })
      .then(setPreview)
      .catch(e => setPreviewError(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setIds, combine, sigLo, sigHi, pLo, pHi, syntheticBias]);

  const start = async () => {
    setStarting(true);
    setStartError(null);
    try {
      if (single) await startBuild(setIds[0], recipe());
      else await startBatch(setIds, recipe());
      onClose();
    } catch (e) {
      setStartError(String(e));
      setStarting(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-surface-elevated rounded-lg border border-border w-[520px] max-h-[80vh] overflow-y-auto p-4"
           onClick={e => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-medium text-content flex items-center gap-2">
            <Hammer size={16} className="text-accent" />
            {single ? `Create master from set #${setIds[0]}` : `Create ${setIds.length} masters`}
          </h3>
          <button onClick={onClose} className="text-content-muted hover:text-content"><X size={16} /></button>
        </div>

        {/* Recipe */}
        <label className="block text-xs text-content-muted mb-1">Combination</label>
        <select value={combine} onChange={e => setCombine(e.target.value as CombineChoice)}
                className="w-full bg-surface border border-border rounded px-2 py-1.5 text-sm mb-2">
          <option value="auto">Auto (recommended — per type & frame count)</option>
          <option value="winsorized">Winsorized sigma clip</option>
          <option value="percentile">Percentile clip</option>
          <option value="median">Median</option>
          <option value="mean">Mean</option>
        </select>
        {combine === 'winsorized' && (
          <div className="flex gap-2 mb-2">
            <label className="text-xs text-content-muted">σ low
              <input type="number" step="0.1" value={sigLo} onChange={e => setSigLo(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
            <label className="text-xs text-content-muted">σ high
              <input type="number" step="0.1" value={sigHi} onChange={e => setSigHi(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
          </div>
        )}
        {combine === 'percentile' && (
          <div className="flex gap-2 mb-2">
            <label className="text-xs text-content-muted">low
              <input type="number" step="0.01" value={pLo} onChange={e => setPLo(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
            <label className="text-xs text-content-muted">high
              <input type="number" step="0.01" value={pHi} onChange={e => setPHi(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
          </div>
        )}
        <label className="block text-xs text-content-muted mb-1 mt-2">
          Synthetic bias for flats (ADU, optional — used only when no darkflat/dark/bias master is linked)
        </label>
        <input value={syntheticBias} onChange={e => setSyntheticBias(e.target.value)} placeholder="e.g. 500"
               className="w-full bg-surface border border-border rounded px-2 py-1.5 text-sm mb-2" />
        <label className="flex items-center gap-2 text-sm text-content-secondary mb-3">
          <input type="checkbox" checked={archiveAfter} onChange={e => setArchiveAfter(e.target.checked)} />
          Archive originals to zip after the master is built
        </label>

        {/* Preview (single-set only) */}
        {single && preview && (
          <div className="bg-surface rounded p-2.5 border border-border text-xs space-y-1 mb-3">
            <div><span className="text-content-muted">Frames:</span> <span className="text-content">{preview.frameCount}</span></div>
            <div><span className="text-content-muted">Method:</span> <span className="text-content font-mono">{JSON.stringify(preview.resolvedCombine)}</span></div>
            {preview.flatPrecal && (
              <div><span className="text-content-muted">Flat pre-cal:</span> <span className="text-content">{preview.flatPrecal}</span></div>
            )}
            <div><span className="text-content-muted">Target:</span> <span className="text-content font-mono break-all">{preview.targetPath}</span></div>
            {preview.warnings.map((w, i) => (
              <div key={i} className="flex items-start gap-1 text-warning">
                <AlertTriangle size={12} className="mt-0.5 shrink-0" />{w}
              </div>
            ))}
          </div>
        )}
        {previewError && <div className="text-xs text-danger mb-2">{previewError}</div>}
        {startError && <div className="text-xs text-danger mb-2">{startError}</div>}

        <div className="flex justify-end gap-2">
          <button onClick={onClose} className="px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover rounded">Cancel</button>
          <button onClick={start} disabled={starting}
                  className="px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded disabled:opacity-50">
            {starting ? 'Starting…' : single ? 'Create master' : 'Create all'}
          </button>
        </div>
      </div>
    </div>
  );
}
```

(Field names `frameCount`/`resolvedCombine`/`flatPrecal`/`targetPath`/`syntheticBias`/`archiveAfter` are the camelCase serde forms of Task 12/14's DTOs — cross-check against the regenerated `types/models.ts` after Task 12 and align. Tailwind tone classes `text-warning`/`text-danger`: confirm the project's palette names via `grep -rn "text-danger\|text-warning" src/components | head -3` and use whatever exists.)

- [ ] **Step 2: `CalibrationSetTable.tsx` integration**

Props (`:12-21`) gain:

```ts
  /** Show a "Create Master" action on raw (non-master, non-superseded) sets. */
  onCreateMaster?: (setId: number) => void;
  /** Set IDs with an in-flight master build (renders a spinner label). */
  buildingSetIds?: number[];
```

Row rendering: dim superseded raws — on the `<tr>` className add `${set.superseded_by_set_id != null ? 'opacity-50' : ''}` and next to the ID cell render `{set.superseded_by_set_id != null && (<span className="text-[10px] text-content-muted ml-1" title={`Superseded by master set #${set.superseded_by_set_id}`}>→ M#{set.superseded_by_set_id}</span>)}`.

Actions cell (after the Sub-Cal button, `:426`):

```tsx
{onCreateMaster && set.id != null && !isMasterType(set.imagetyp) && set.superseded_by_set_id == null && (
  <button
    onClick={(e) => { e.stopPropagation(); onCreateMaster(set.id!); }}
    disabled={buildingSetIds?.includes(set.id)}
    className="inline-flex items-center gap-1 px-2 py-1 bg-surface-hover hover:brightness-110 text-content text-xs rounded transition-colors disabled:opacity-50"
    title="Integrate this set into a master frame"
  >
    <Hammer size={14} />
    {buildingSetIds?.includes(set.id) ? 'Building…' : 'Create Master'}
  </button>
)}
```

Expanded row provenance block (inside the expanded `<td>`, after the metadata `flex-wrap` div, before `ConsumerChipStrip`): for master rows lazily fetch and render:

```tsx
{set.id != null && isMasterType(set.imagetyp) && (
  <MasterProvenanceBlock setId={set.id} />
)}
```

with a small component in the same file:

```tsx
function MasterProvenanceBlock({ setId }: { setId: number }) {
  const [prov, setProv] = useState<MasterProvenanceInfo | null | 'loading'>('loading');
  const [archiving, setArchiving] = useState(false);
  useEffect(() => {
    let gone = false;
    api.invoke<MasterProvenanceInfo | null>('get_master_provenance', { masterSetId: setId })
      .then(p => { if (!gone) setProv(p); })
      .catch(() => { if (!gone) setProv(null); });
    return () => { gone = true; };
  }, [setId]);
  if (prov === 'loading') return null;
  if (prov === null) {
    return <div className="mt-2 text-xs"><span className="px-1.5 py-0.5 rounded bg-surface-hover text-content-muted">imported master</span></div>;
  }
  const archiveOriginals = async () => {
    setArchiving(true);
    try {
      await api.invoke('archive_calibration_originals', { calibrationSetId: prov.sourceSetId });
    } finally { setArchiving(false); }
  };
  return (
    <div className="mt-2 text-xs space-y-1">
      <span className="px-1.5 py-0.5 rounded bg-accent/20 text-accent">built in Athenaeum</span>
      <div><span className="text-content-muted">Source set:</span> <span className="text-content">#{prov.sourceSetId} ({prov.memberCount} frames)</span></div>
      <div><span className="text-content-muted">Recipe:</span> <span className="text-content font-mono">{prov.recipeJson}</span></div>
      <div><span className="text-content-muted">Created:</span> <span className="text-content">{prov.createdAt}</span></div>
      <div>
        <span className="text-content-muted">Originals:</span>{' '}
        {prov.originalsArchived ? <span className="text-content">archived to zip</span>
          : prov.sourceFramesOnDisk ? (
            <button onClick={archiveOriginals} disabled={archiving}
              className="px-2 py-0.5 bg-surface-hover hover:brightness-110 rounded text-content disabled:opacity-50">
              {archiving ? 'Archiving…' : 'Archive originals to zip'}
            </button>
          ) : <span className="text-warning">missing on disk</span>}
      </div>
    </div>
  );
}
```

- [ ] **Step 3: `CameraDetail.tsx` wiring** — add state `const [createMasterSetIds, setCreateMasterSetIds] = useState<number[] | null>(null);`, derive `buildingSetIds` from `useMasterBuildContext().buildStates`, pass `onCreateMaster={(id) => setCreateMasterSetIds([id])}` + `buildingSetIds` down through the darks/flats/master tabs into `CalibrationSetTable`, and render `{createMasterSetIds && <CreateMasterDialog setIds={createMasterSetIds} onClose={() => setCreateMasterSetIds(null)} />}` at the bottom. Thread the two props through `DarkLibrary` / `MasterDarkLibrary` / `MasterFlatLibrary` (each declares them optional and forwards).

- [ ] **Step 4: Verify + commit**

Run: `npx tsc --noEmit` → PASS. Dev smoke (Equipment): raw dark set row shows Create Master → dialog previews target path → build runs (queue indicator) → on completion the list refreshes (`library-updated`), raw row dims with `→ M#`, new master row shows the "built in Athenaeum" provenance block; "Archive originals" moves the raws to a `Calibration_Archive/...` zip.

```bash
git add src/
git commit -m "feat(ui): Create Master dialog + Equipment library integration with provenance and archive actions"
```

---

### Task 17: Frontend — Coverage tab integration

**Files:**
- Modify: `src/components/calibration/CalibrationTableView.tsx` (row buttons + status)
- Modify: `src/components/CalibrationHierarchyView.tsx` (toolbar batch button + dialog hosting)

**Interfaces:**
- Consumes: `useMasterBuildContext()`, `CreateMasterDialog`, existing `deriveTableData` row models (`FlatRow`/`DarkRow`/`BiasRow` expose `setId`, `isMaster` per the MasterBadge rendering) and `onRefresh` callback (already a prop of `CalibrationHierarchyView`).
- Produces: `CalibrationTableViewProps` gains `onCreateMaster?: (setId: number) => void` and `buildStatusBySet?: Record<number, 'starting' | 'building' | 'done'>`.

- [ ] **Step 1: Row-level Create Master buttons** — in `FlatsTable` (`:776`), `DarksTable` (`:913`), `BiasTable` (`:1044`): append one cell to each row (and one `<th/>` to each header). Shared cell snippet (adapt the row variable name per table):

```tsx
<td className="px-2 py-1 text-center">
  {!row.isMaster && onCreateMaster && (
    buildStatusBySet?.[row.setId] && buildStatusBySet[row.setId] !== 'done' ? (
      <span className="text-[10px] text-accent animate-pulse">building…</span>
    ) : (
      <button
        onClick={(e) => { e.stopPropagation(); onCreateMaster(row.setId); }}
        className="p-1 rounded hover:bg-surface-hover text-content-muted hover:text-content"
        title="Create master from this set"
      >
        <Hammer size={13} />
      </button>
    )
  )}
</td>
```

Thread the two new props from `CalibrationTableView`'s top-level props down into the three tables (they already receive per-table prop bundles — follow the existing prop-drilling for `onHighlight`).

- [ ] **Step 2: Toolbar "Create all masters"** — in `CalibrationHierarchyView.tsx`'s right-panel toolbar (next to `CalibrationFinderButton`, `:302-313`):

```tsx
const rawCalSetIds = useMemo(() => {
  // every non-master flat/dark/bias/darkflat set id present in the coverage data
  const ids = new Set<number>();
  for (const dg of data.date_groups) for (const cg of dg.camera_groups) for (const fg of cg.filter_groups) {
    for (const lf of fg.light_frames) {
      // set ids come from the same fields deriveTableData reads — reuse its
      // source: flat_set_id / dark_set_id / bias_set_id when the linked set
      // is not a master. The is-master knowledge lives in the derived rows,
      // so lift this computation FROM CalibrationTableView via a callback
      // instead if the raw hierarchy lacks master flags (see note below).
      if (lf.flat_set_id != null) ids.add(lf.flat_set_id);
      if (lf.dark_set_id != null) ids.add(lf.dark_set_id);
      if (lf.bias_set_id != null) ids.add(lf.bias_set_id);
    }
  }
  return [...ids];
}, [data]);
```

NOTE: whether `CalibrationHierarchyView`'s `data` carries master flags per linked set must be checked at implementation time (`grep -n "is_master" src/types/models.ts` for the `CalibrationHierarchyView`/`LightFrame*` shapes). If it does NOT, hoist the id list out of `CalibrationTableView` (which already computes `FlatRow.isMaster` etc. in `deriveTableData`, `:219-490`) via a `onRawSetsComputed?: (ids: number[]) => void` callback — pick whichever needs fewer type changes; superseded/master ids sent anyway are safely skipped server-side by the batch endpoint's per-set guards (Task 13), so over-sending is harmless.

Button:

```tsx
<button
  onClick={() => setBatchDialogIds(rawCalSetIds)}
  disabled={rawCalSetIds.length === 0}
  className="px-3 py-1.5 bg-surface-hover hover:brightness-110 text-content text-sm rounded disabled:opacity-50"
  title="Integrate every raw calibration set used by this object into masters"
>
  Create all masters ({rawCalSetIds.length})
</button>
```

Host the dialog in `CalibrationHierarchyView` (`const [batchDialogIds, setBatchDialogIds] = useState<number[] | null>(null)`; single-set clicks from the tables call `setBatchDialogIds([setId])`); on `master-build-complete` the context already fires `library-updated` — additionally call the existing `onRefresh` prop when the dialog closes after a batch start? No: refresh when builds COMPLETE. Subscribe in this component:

```tsx
useEffect(() => {
  const h = () => onRefresh?.();
  window.addEventListener('library-updated', h);
  return () => window.removeEventListener('library-updated', h);
}, [onRefresh]);
```

- [ ] **Step 3: Build status map** — derive from context: `const buildStatusBySet = useMemo(() => { const m: Record<number, 'starting'|'building'|'done'> = {}; for (const [id, s] of buildStates) m[id] = s.phase; return m; }, [buildStates]);` and pass into `CalibrationTableView`.

- [ ] **Step 4: Verify + commit**

Run: `npx tsc --noEmit` → PASS. Dev smoke (object → Calibration Coverage): raw flat/dark/bias rows show the hammer; "Create all masters (N)" opens the batch dialog; after builds finish the tables refresh — cal rows now carry the M badge and lights' links point at masters (SetIdBadge jumps still work — they navigate by the NEW master set id).

```bash
git add src/
git commit -m "feat(ui): Coverage tab — per-set Create Master + batch Create all masters with live status"
```

---

### Task 18: Docs, roadmap, final verification

**Files:**
- Modify: `CLAUDE.md` (athenaeum repo — new "Master calibration library" section)
- Modify: `docs/superpowers/plans/2026-07-02-roadmap.md` (Phase 2 checkboxes: linear-decode item, B2, B3, library-root item, B4; research spike marked done with links)
- Create: `scripts/verify_master_vs_reference.md` (dev-only golden-comparison procedure)

- [ ] **Step 1: CLAUDE.md section** — add after the "AmneziaWG"-style feature sections a concise block covering: what the Calibration Library root is (one per catalog, `scan_roots.kind`), the direct-registration invariant (rows identical to scanner ingestion — pinned by test), the relink/supersede semantics (`superseded_by_set_id`, matcher exclusion), the raw-master-dark convention + no-dark-scaling policy pointer to spec §9, the `ATH_FNRM` keyword, the compute queue (`compute.max_concurrent`, analysis rides it too), and the archive-of-originals layout. Point to the spec + research docs.

- [ ] **Step 2: `scripts/verify_master_vs_reference.md`** — documented manual procedure (NOT CI): pick a real 15+-frame dark set → build the master in Athenaeum → build the same master in Siril (`calibrate`-free `stack` with winsorized rejection) → compare with a 10-line Python/astropy snippet (include it verbatim in the doc: load both, `numpy.percentile(|a-b|, [50, 99.9])`, assert median diff ≲ 1 ADU) → record results in the doc's log table. This is the spec §10 golden test in its owner-run form (Siril isn't available in CI).

- [ ] **Step 3: Final verification (whole plan)**

```bash
cargo test --workspace 2>&1 | tail -20          # all green (known rustafits exception noted in Task 1)
cargo clippy -p athenaeum-core --no-deps -- -D warnings
npx tsc --noEmit
cargo test -p athenaeum-core --test ts_contract # generated files committed & in sync
```

Manual E2E (the milestone-M2 proof, run it and record the outcome in the PR/branch notes):
1. Settings → designate a Calibration Library folder.
2. Equipment → camera → darks → Create Master on a real raw dark set (Auto recipe, archive-after ON).
3. Watch: compute-queue indicator → master-build progress → completion toast → master file exists under `<Library>/<Camera>/MasterDark/…` → `awk`-check header: `IMAGETYP='Master Dark'`, `ATH_SRC`, `ATH_N`, `SWCREATE`.
4. Coverage tab of an object that used that raw set: lights now link to the master (M badge), raw set dimmed/`→ M#`.
5. Archive ran: zip under `<archive_root>/Calibration_Archive/<Camera>/<date>/`, originals gone from disk, catalog rows intact; Restore from the Archive page brings them back.
6. Start an analysis while a master build runs → it queues (indicator shows both, FIFO).

- [ ] **Step 4: Commit + wrap up**

```bash
git add CLAUDE.md docs/ scripts/
git commit -m "docs: master calibration library — CLAUDE.md section, roadmap checkboxes, golden-verify procedure"
```

Then follow `superpowers:finishing-a-development-branch` (merge/PR decision is the owner's; version bump + release notes follow the standard release ritual when shipping).

---

## Execution notes for workers

- Tasks 6–8 (engine) and Task 9 (library root) are independent of each other; everything else is ordered as numbered.
- **SIMD (spec §4)**: the engine ships with rayon `par_chunks` parallelism only. Hand-rolled `std::arch` AVX2/NEON kernels for the accumulate/clip loops (the `convolution.rs` precedent) are a DEFERRED optimization — profile a real 50-frame build first; add kernels only if the combine loop (not I/O) dominates. Do not add them speculatively inside this plan's tasks.
- Every "grep first" instruction inside steps is load-bearing: this plan pins line numbers against `main` @ v0.2.4+ (commit `8dae39d6`) and drift is expected — trust symbol names over line numbers.
- If a step's stated expectation and the code's reality disagree (e.g. a helper is named differently), STOP and re-read the surrounding module rather than inventing a parallel helper — the plan's whole architecture leans on reusing scanner/archive/analysis machinery, not duplicating it.

