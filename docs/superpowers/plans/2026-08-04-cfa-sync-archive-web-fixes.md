# CFA Projection Family + Web Archive Terminal Events + Web Bounds 422 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three verified follow-up bugs from the dead-code-cleanup cycle: (1) the last CFA-blind `Frame` projection family — including its active data-corruption path through sync/collab — plus back-fill of already-corrupted receiver catalogs; (2) the missing terminal `archive-finished` event in the web archive worker and both resume workers; (3) the web `query_frames_in_bounds` route that 422s on every sky-map rectangle selection.

**Architecture:** (1) Re-base the four blind projections onto the existing canonical `FILE_FRAME_SELECT` + `map_file_frame_row` so the family cannot re-emerge; add a defensive CFA back-fill at sync ingest (covers old senders) and a one-time settings-flag-gated catalog repair that restores CFA columns from the stored `fits_header` blob. (2) Lift the terminal-event block that already exists in three near-identical copies into core (`run_operation_standalone` / `resume_operation_standalone`, mirroring the existing `rollback_operation_standalone` pattern) and switch all four workers to it. (3) Make `routes/spatial.rs` mirror the `CreateFrameSetFromSelectionArgs` precedent: accept the nested snake_case `{ bounds: … }` envelope the frontend has always sent.

**Tech Stack:** Rust (rusqlite, anyhow, tracing, serde), Axum (web routes), Tauri 2 (desktop commands), React/TS frontend.

## Verified findings this plan fixes (evidence)

- **CFA family** — `get_light_frames_for_project` (`crates/athenaeum-core/src/db/operations.rs:2388`), `get_frames_with_files_by_ids` (`operations.rs:2817`), `get_imaging_nights_with_sessions` inner frames query (`operations.rs:3084`), `get_frames_for_calibration_set` (`crates/athenaeum-core/src/db/equipment.rs:259`) all omit `swcreate, bayerpat, xbayroff, ybayroff, roworder, rotation` from their SELECT and hardcode them `None`. `get_frames_with_files_by_ids` feeds `frame_meta` in sync packages (`api/sync.rs:2884`) and collab publish (`api/collab.rs:1144`); the receiver's `sync/ingest.rs::insert_ingested_rows` writes that blind `Frame` via `insert_frame`, so **every synced frame lands with NULL CFA columns**, and `api/lights.rs::resolve_cfa_geometry` reads only those columns → per-channel flat scaling silently skipped for received OSC lights.
- **Archive events** — the tauri start-archive worker emits `archive-finished` (`commands/archive.rs:264`); the web start-archive worker does not (`routes/archive.rs:242-278`); neither resume worker does (`commands/archive.rs:319-330`, `routes/archive.rs:390-401`). `ArchiveProgress.tsx:82` dismisses only on that event — on web the widget hangs forever and `onClose`'s `loadData()` never runs. The gap inventory is already documented in `archive/rollback.rs:47-55`.
- **Web bounds** — frontend sends `{ bounds: { ra_min, … } }` (`src/hooks/useRectangleSelection.ts:242`, passthrough in `src/api/http.ts:127`); `routes/spatial.rs:16-27` expects flat camelCase → 422 on every selection. The tauri command works because it uses `rename_all = "snake_case"` + a named `bounds` argument (`commands/spatial.rs:30`). Correct precedent for the web mirror: `CreateFrameSetFromSelectionArgs` in `routes/frame_sets.rs:56-60`.

## Global Constraints

- **Two backends in sync:** any change to a Tauri command's behavior lands with the matching Axum route change in the same task/commit.
- **Real logic in core:** the Tauri/Axum layers stay thin wrappers; shared behavior (terminal events, repair) lives in `athenaeum-core`.
- **`anyhow::Result` in core;** `.map_err(|e| e.to_string())` only at command boundaries (no boundary signatures change in this plan).
- **Never swallow errors:** every dropped `Result` must already have been logged (via `tracing::error!`) inside the callee; note it in a comment at the drop site.
- **Logging style:** message = short stable phrase, data in snake_case fields from the canonical dictionary (`frame_id`, `operation_id`, `outcome`, `count`, `error`); no new field names.
- **Zero `println!`/`eprintln!`** in production code.
- **No new dependencies.**
- **Commits as the user** (`eg013ra1n` / `vilen.sharifov@gmail.com` — already the repo git config); never Claude as author/co-author. Branch: `0.5.1` (current), no push.
- **Frontend:** design tokens only (`bg-accent`, `text-content`, …), backend access via the `api` object.
- Do not name external stacker/solver tools in code or comments.

---

### Task 1: Re-base the four CFA-blind projections onto the canonical mapper

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` (visibility at lines ~915/941/1027; function bodies at ~2388, ~2817, ~3084-3174; new test mod)
- Modify: `crates/athenaeum-core/src/db/equipment.rs:259-352` (+ new test mod)

**Interfaces:**
- Consumes: existing `FILE_FRAME_SELECT` (52-col projection const, `operations.rs:915`), `map_file_frame_row` (`operations.rs:941`), `require_joined_frame` (`operations.rs:1027`).
- Produces: same public signatures as today — `get_light_frames_for_project(conn, project_id) -> Result<Vec<(i64, Frame)>>`, `get_frames_with_files_by_ids(conn, &[i64]) -> Result<Vec<(i64, File, Frame)>>`, `get_imaging_nights_with_sessions(conn, i64) -> Result<Vec<ImagingNightWithSessions>>`, `get_frames_for_calibration_set(conn, i64) -> Result<Vec<FileWithFrame>>` — but with all `Frame` fields populated. Task 2's ingest back-fill and the sync packagers rely on `get_frames_with_files_by_ids` carrying CFA fields.

Behavior note (intentional): the canonical mapper decodes frame fields leniently (`.ok()` — one bad value degrades to `None` instead of failing the listing), replacing the strict `?` decoding these four had. That is the established listing behavior (see the doc comment on `map_file_frame_row`).

- [ ] **Step 1: Write the failing tests**

In `operations.rs`, add a new test module directly after `get_imaging_nights_with_sessions`'s closing brace (~line 3190):

```rust
#[cfg(test)]
mod cfa_projection_tests {
    use rusqlite::Connection;

    /// One OSC light wired into a frames_set/night/session so every family
    /// projection sees it. CFA + provenance columns all populated — the four
    /// mappers used to erase exactly these six fields (the `420bde27` family).
    fn osc_fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1, '/data/M31/L_001.fits', 'L_001.fits', 7, '2026-01-01T00:00:00+00:00', 'FITS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, date_obs, instrume, imagetyp, is_master,
                                 swcreate, bayerpat, xbayroff, ybayroff, roworder, rotation, uuid)
             VALUES (1, 1, 'M31', '2026-01-01T00:00:00+00:00', 'ASI2600MC', 'Light', 0,
                     'CaptureApp', 'RGGB', 1, 0, 'BOTTOM-UP', 12.5, 'frame-uuid-1')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2026-01-01', '2026-01-02')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'ASI2600MC')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 1)", [])
            .unwrap();
        conn
    }

    fn assert_cfa_complete(frame: &crate::models::Frame, ctx: &str) {
        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"), "{ctx}: bayerpat");
        assert_eq!(frame.xbayroff, Some(1), "{ctx}: xbayroff");
        assert_eq!(frame.ybayroff, Some(0), "{ctx}: ybayroff");
        assert_eq!(frame.roworder.as_deref(), Some("BOTTOM-UP"), "{ctx}: roworder");
        assert_eq!(frame.swcreate.as_deref(), Some("CaptureApp"), "{ctx}: swcreate");
        assert_eq!(frame.rotation, Some(12.5), "{ctx}: rotation");
    }

    #[test]
    fn light_frames_for_project_carries_cfa_columns() {
        let conn = osc_fixture();
        let rows = super::get_light_frames_for_project(&conn, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 1);
        assert_cfa_complete(&rows[0].1, "get_light_frames_for_project");
    }

    #[test]
    fn frames_with_files_by_ids_carries_cfa_columns() {
        let conn = osc_fixture();
        let rows = super::get_frames_with_files_by_ids(&conn, &[1]).unwrap();
        assert_eq!(rows.len(), 1);
        let (file_id, file, frame) = &rows[0];
        assert_eq!(*file_id, 1);
        assert_eq!(file.id, Some(1));
        assert_cfa_complete(frame, "get_frames_with_files_by_ids");
    }

    #[test]
    fn imaging_nights_with_sessions_carries_cfa_columns() {
        let conn = osc_fixture();
        let nights = super::get_imaging_nights_with_sessions(&conn, 1).unwrap();
        assert_eq!(nights.len(), 1);
        let frames = &nights[0].sessions[0].frames;
        assert_eq!(frames.len(), 1);
        let frame = frames[0].frame.as_ref().expect("joined frame");
        assert_cfa_complete(frame, "get_imaging_nights_with_sessions");
    }
}
```

In `equipment.rs`, add after the existing `stored_timestamp_tests` module (~line 403):

```rust
#[cfg(test)]
mod cfa_projection_tests {
    use crate::db::schema::init_db;
    use rusqlite::Connection;

    /// The calibration-set frame listing shares the 47-column SELECT that
    /// erased the CFA columns — pinned here against the canonical projection.
    #[test]
    fn calibration_set_frames_carry_cfa_columns() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1, '/t/flat.fits', 'flat.fits', 1, '2026-01-01T00:00:00+00:00', 'FITS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, swcreate,
                                 bayerpat, xbayroff, ybayroff, roworder, rotation)
             VALUES (1, 1, 'FLAT', 'CamA', 'CaptureApp', 'RGGB', 1, 0, 'BOTTOM-UP', 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (1, 'FLAT', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (1, 1)", [])
            .unwrap();

        let rows = super::get_frames_for_calibration_set(&conn, 1).unwrap();
        assert_eq!(rows.len(), 1);
        let frame = rows[0].frame.as_ref().expect("joined frame");
        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
        assert_eq!(frame.roworder.as_deref(), Some("BOTTOM-UP"));
        assert_eq!(frame.swcreate.as_deref(), Some("CaptureApp"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core cfa_projection_tests -- --nocapture`
Expected: 4 FAILs, each on a `bayerpat`/CFA assertion (`left: None, right: Some("RGGB")`).

- [ ] **Step 3: Widen visibility of the canonical trio**

In `operations.rs`, change (keeping doc comments):
- line ~915: `const FILE_FRAME_SELECT` → `pub(crate) const FILE_FRAME_SELECT`
- line ~941: `fn map_file_frame_row` → `pub(crate) fn map_file_frame_row`
- line ~1027: `fn require_joined_frame` → `pub(crate) fn require_joined_frame`

- [ ] **Step 4: Rewrite the three `operations.rs` bodies**

`get_light_frames_for_project` (~2388) — replace the whole body:

```rust
pub fn get_light_frames_for_project(
    conn: &Connection,
    _project_id: i64,
) -> Result<Vec<(i64, crate::models::Frame)>> {
    // For now, we'll get all LIGHT frames regardless of project
    // In the future, we can add project filtering at the frame level
    let query = format!(
        "SELECT {select}
         FROM files f
         INNER JOIN frames fr ON f.id = fr.file_id
         WHERE fr.imagetyp = 'Light' OR fr.imagetyp IS NULL
         ORDER BY f.id",
        select = FILE_FRAME_SELECT,
    );
    let mut stmt = conn.prepare(&query)?;
    let results = stmt.query_map([], |row| {
        let file_id: i64 = row.get(0)?;
        let (_file, frame) = map_file_frame_row(row)?;
        Ok((file_id, require_joined_frame(frame)?))
    })?;
    results.collect()
}
```

`get_frames_with_files_by_ids` (~2817) — keep the empty-input guard, placeholder builder and `params_vec` exactly as they are; replace the SQL string and the mapper closure:

```rust
    let query = format!(
        "SELECT {select}
         FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE fr.id IN ({placeholders})
         ORDER BY fr.date_obs ASC",
        select = FILE_FRAME_SELECT,
        placeholders = placeholders,
    );

    let mut stmt = conn.prepare(&query)?;

    // Convert frame_ids to params
    let params_vec: Vec<&dyn rusqlite::ToSql> = frame_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let results = stmt.query_map(params_vec.as_slice(), |row| {
        let file_id: i64 = row.get(0)?;
        let (file, frame) = map_file_frame_row(row)?;
        Ok((file_id, file, require_joined_frame(frame)?))
    })?;

    results.collect()
```

`get_imaging_nights_with_sessions` (~3084) — replace only the inner per-session frames query + mapper (nights/sessions loops untouched):

```rust
            // Get frames for this session
            let frames_query = format!(
                "SELECT {select}
                 FROM session_members sm
                 JOIN frames fr ON sm.frame_id = fr.id
                 JOIN files f ON fr.file_id = f.id
                 WHERE sm.session_id = ?1
                 ORDER BY fr.date_obs ASC",
                select = FILE_FRAME_SELECT,
            );
            let mut frames_stmt = conn.prepare(&frames_query)?;

            let frames = frames_stmt.query_map(params![session_id], |row| {
                let (file, frame) = map_file_frame_row(row)?;
                let frame = require_joined_frame(frame)?;
                Ok(crate::models::FileWithFrame { file, frame: Some(frame) })
            })?;
```

- [ ] **Step 5: Rewrite `equipment.rs::get_frames_for_calibration_set`**

Replace the whole body (~259-352):

```rust
/// Get frames for a specific calibration set
pub fn get_frames_for_calibration_set(
    conn: &Connection,
    set_id: i64,
) -> Result<Vec<FileWithFrame>> {
    let query = format!(
        "SELECT {select}
         FROM calibration_set_frames csf
         JOIN frames fr ON csf.frame_id = fr.id
         JOIN files f ON fr.file_id = f.id
         WHERE csf.set_id = ?1
         ORDER BY fr.date_obs ASC",
        select = super::operations::FILE_FRAME_SELECT,
    );
    let mut stmt = conn.prepare(&query)?;

    let frames = stmt.query_map(params![set_id], |row| {
        let (file, frame) = super::operations::map_file_frame_row(row)?;
        let frame = super::operations::require_joined_frame(frame)?;
        Ok(FileWithFrame { file, frame: Some(frame) })
    })?;

    frames.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
}
```

Remove `use` items the old body needed if now unused (`DateTime`, `Utc` — only if nothing else in the file uses them; the compiler will say).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core cfa_projection_tests`
Expected: 4 PASS. Also run the neighbors that pin the shared projection and the existing timestamp behavior:
`cargo test -p athenaeum-core shared_projection_column_count_matches_const stored_timestamp_tests`
Expected: PASS (the equipment timestamp test asserts `UNIX_EPOCH` fallback on malformed timestamps — `map_file_frame_row` uses the same `parse_stored_ts`, so behavior is preserved).

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/db/equipment.rs
git commit -m "fix(db): last four CFA-blind Frame projections onto the canonical FILE_FRAME_SELECT mapper"
```

---

### Task 2: CFA header helpers + defensive back-fill at sync ingest

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/stored_header.rs` (new helpers + tests)
- Modify: `crates/athenaeum-core/src/sync/ingest.rs:609-635` (`insert_ingested_rows`)

**Interfaces:**
- Consumes: `parse_stored_header_keys(FileFormat, &str) -> HashMap<String, String>` (`stored_header.rs:72`).
- Produces: `pub struct CfaHeaderFields { bayerpat: Option<String>, xbayroff: Option<i64>, ybayroff: Option<i64>, roworder: Option<String> }`, `pub fn cfa_fields_from_keys(&HashMap<String, String>) -> CfaHeaderFields`, `pub fn backfill_frame_cfa(&mut Frame, FileFormat, header_text: &str)` — Task 3's repair uses `cfa_fields_from_keys`.

- [ ] **Step 1: Write the failing tests**

In `stored_header.rs`'s existing `#[cfg(test)]` module (or a new `mod cfa_backfill_tests` at the end of the file):

```rust
#[cfg(test)]
mod cfa_backfill_tests {
    use super::*;
    use crate::models::{FileFormat, Frame};

    // "KEY = value" dump form — explicitly supported by the ASIAIR fallback
    // parser, so the test doesn't depend on 80-col card layout.
    const OSC_HEADER: &str = "BAYERPAT= 'RGGB'\nXBAYROFF= 1\nYBAYROFF= 0\nROWORDER= 'BOTTOM-UP'\nEND";

    fn blank_frame() -> Frame {
        // Frame derives Default — everything None/false except the required id.
        Frame { file_id: 1, ..Default::default() }
    }

    #[test]
    fn cfa_fields_parse_from_keys() {
        let keys = parse_stored_header_keys(FileFormat::FITS, OSC_HEADER);
        let cfa = cfa_fields_from_keys(&keys);
        assert_eq!(cfa.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(cfa.xbayroff, Some(1));
        assert_eq!(cfa.ybayroff, Some(0));
        assert_eq!(cfa.roworder.as_deref(), Some("BOTTOM-UP"));
    }

    #[test]
    fn backfill_fills_only_missing_fields() {
        let mut frame = blank_frame();
        frame.bayerpat = Some("GBRG".to_string()); // snapshot value must win
        backfill_frame_cfa(&mut frame, FileFormat::FITS, OSC_HEADER);
        assert_eq!(frame.bayerpat.as_deref(), Some("GBRG"), "existing value never overwritten");
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
        assert_eq!(frame.roworder.as_deref(), Some("BOTTOM-UP"));
    }

    #[test]
    fn backfill_is_inert_on_a_mono_header() {
        let mut frame = blank_frame();
        backfill_frame_cfa(&mut frame, FileFormat::FITS, "EXPTIME = 300.0\nEND");
        assert!(frame.bayerpat.is_none());
        assert!(frame.xbayroff.is_none());
        assert!(frame.ybayroff.is_none());
        assert!(frame.roworder.is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core cfa_backfill_tests`
Expected: compile FAIL — `cfa_fields_from_keys` / `backfill_frame_cfa` not found.

- [ ] **Step 3: Implement the helpers**

Append to `stored_header.rs` (after `snapshot_from_keys`):

```rust
/// The four CFA identity fields as stored in a raw header, parsed with the
/// same leniency as [`snapshot_from_keys`]. Consumers back-fill catalog
/// columns from these when a frame snapshot arrived without them: sync
/// ingest from a sender whose `frame_meta` was built by a CFA-blind
/// projection, and the one-time catalog repair for rows already landed
/// that way (`db::repair`).
pub struct CfaHeaderFields {
    pub bayerpat: Option<String>,
    pub xbayroff: Option<i64>,
    pub ybayroff: Option<i64>,
    pub roworder: Option<String>,
}

pub fn cfa_fields_from_keys(keys: &HashMap<String, String>) -> CfaHeaderFields {
    let get = |k: &str| keys.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    CfaHeaderFields {
        bayerpat: get("BAYERPAT"),
        xbayroff: get("XBAYROFF").and_then(|s| s.parse::<i64>().ok()),
        ybayroff: get("YBAYROFF").and_then(|s| s.parse::<i64>().ok()),
        roworder: get("ROWORDER"),
    }
}

/// Fill a [`Frame`]'s missing CFA fields from the file's own raw header.
/// Only `None` fields are filled — a value the snapshot carried always wins.
pub fn backfill_frame_cfa(
    frame: &mut crate::models::Frame,
    format: FileFormat,
    header_text: &str,
) {
    if frame.bayerpat.is_some()
        && frame.xbayroff.is_some()
        && frame.ybayroff.is_some()
        && frame.roworder.is_some()
    {
        return;
    }
    let keys = parse_stored_header_keys(format, header_text);
    let cfa = cfa_fields_from_keys(&keys);
    if frame.bayerpat.is_none() {
        frame.bayerpat = cfa.bayerpat;
    }
    if frame.xbayroff.is_none() {
        frame.xbayroff = cfa.xbayroff;
    }
    if frame.ybayroff.is_none() {
        frame.ybayroff = cfa.ybayroff;
    }
    if frame.roworder.is_none() {
        frame.roworder = cfa.roworder;
    }
}
```

- [ ] **Step 4: Run helper tests**

Run: `cargo test -p athenaeum-core cfa_backfill_tests`
Expected: 3 PASS.

- [ ] **Step 5: Wire the back-fill into `insert_ingested_rows`**

In `ingest.rs`, the header-extraction block currently reads (lines ~609-625):

```rust
    let header = match format {
        FileFormat::FITS => crate::fits_parser::extract_fits_header(landed),
        FileFormat::XISF => crate::fits_parser::extract_xisf_header(landed),
    };
    match header {
        Ok(text) => {
            crate::db::insert_fits_header(tx, file_id, &text).context("insert fits_header row")?;
        }
        Err(e) => {
            tracing::warn!(frame_uuid = %record.frame_uuid, error = %e, "sync ingest header extract failed");
            crate::db::insert_fits_header(tx, file_id, "").context("insert empty fits_header row")?;
        }
    }
```

Change it to keep the text (note: `FileFormat` is `Clone`, not `Copy` — clone before the consuming `match`):

```rust
    let format_for_backfill = format.clone();
    let header = match format {
        FileFormat::FITS => crate::fits_parser::extract_fits_header(landed),
        FileFormat::XISF => crate::fits_parser::extract_xisf_header(landed),
    };
    let header_text = match header {
        Ok(text) => {
            crate::db::insert_fits_header(tx, file_id, &text).context("insert fits_header row")?;
            Some(text)
        }
        Err(e) => {
            tracing::warn!(frame_uuid = %record.frame_uuid, error = %e, "sync ingest header extract failed");
            crate::db::insert_fits_header(tx, file_id, "").context("insert empty fits_header row")?;
            None
        }
    };
```

Then, in the frame-insert block just below, after `frame.file_id = file_id;` and before `insert_frame`:

```rust
    let mut frame = snapshot.clone();
    frame.id = None;
    frame.file_id = file_id;
    // Defensive CFA back-fill: a sender running a build with the CFA-blind
    // `get_frames_with_files_by_ids` projection ships `frame_meta` with the
    // Bayer fields erased. The landed file's own header is authoritative for
    // what the snapshot left empty; a value the snapshot carried always wins.
    if let Some(text) = &header_text {
        crate::fits_parser::stored_header::backfill_frame_cfa(
            &mut frame,
            format_for_backfill,
            text,
        );
    }
    let frame_id = crate::db::insert_frame(tx, &frame).context("insert frames row")?;
```

(Only `swcreate`/`rotation` remain un-back-filled here — deliberate: this fix is scoped to the CFA family; future sends carry them via Task 1.)

- [ ] **Step 6: Compile + core sync tests**

Run: `cargo test -p athenaeum-core sync::`
Expected: PASS (existing ingest tests unaffected — the back-fill is inert when the snapshot already carries CFA or the header has no Bayer cards).

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/fits_parser/stored_header.rs crates/athenaeum-core/src/sync/ingest.rs
git commit -m "fix(sync): back-fill CFA columns from the landed file's header at ingest"
```

---

### Task 3: One-time catalog repair for already-corrupted receivers

**Files:**
- Create: `crates/athenaeum-core/src/db/repair.rs`
- Modify: `crates/athenaeum-core/src/db/mod.rs` (add `pub mod repair;` beside the other `pub mod` lines)
- Modify: `crates/athenaeum-core/src/db/schema.rs::init_db` (tail call, immediately before the final `Ok(())`)

**Interfaces:**
- Consumes: `parse_stored_header_keys` + `cfa_fields_from_keys` (Task 2), `settings` table (`key/value/updated_at`).
- Produces: `pub fn backfill_cfa_from_stored_headers(conn: &Connection) -> anyhow::Result<usize>` gated by settings key `repair.cfa_backfill_v1`.

- [ ] **Step 1: Write the failing test**

Create `crates/athenaeum-core/src/db/repair.rs`:

```rust
//! One-time catalog repairs keyed by `settings` flags — data fixes that the
//! guarded-`ALTER TABLE` migrations in `schema.rs` can't express because they
//! need Rust-side parsing of stored blobs.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::fits_parser::stored_header::{cfa_fields_from_keys, parse_stored_header_keys};
use crate::models::FileFormat;

const CFA_BACKFILL_FLAG: &str = "repair.cfa_backfill_v1";

/// Back-fill NULL CFA columns (`bayerpat`/`xbayroff`/`ybayroff`/`roworder`)
/// on `frames` from the stored `fits_header` blob.
///
/// Frames that arrived over sync/collab before the
/// `get_frames_with_files_by_ids` projection fix had their CFA columns
/// erased in transit even though the re-extracted header blob beside them
/// still carries the cards — which starves `resolve_cfa_geometry` and the
/// Bayer card copy-through fallback on the receiving device. Runs once per
/// catalog (settings flag), fills only NULLs, and never touches
/// `frames.override`: the filled values restate the file's own header, so
/// there is nothing for the scanner to undo.
pub fn backfill_cfa_from_stored_headers(conn: &Connection) -> Result<usize> {
    let already: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![CFA_BACKFILL_FLAG],
            |r| r.get(0),
        )
        .optional()?;
    if already.is_some() {
        return Ok(0);
    }

    let mut stmt = conn.prepare(
        "SELECT fr.id, f.format, fh.header
         FROM frames fr
         JOIN files f ON f.id = fr.file_id
         JOIN fits_header fh ON fh.file_id = f.id
         WHERE fr.bayerpat IS NULL AND fh.header LIKE '%BAYERPAT%'",
    )?;
    let rows: Vec<(i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut repaired = 0usize;
    for (frame_id, format, header) in rows {
        let format = if format == "FITS" { FileFormat::FITS } else { FileFormat::XISF };
        let keys: HashMap<String, String> = parse_stored_header_keys(format, &header);
        let cfa = cfa_fields_from_keys(&keys);
        let Some(bayerpat) = cfa.bayerpat else { continue };
        let changed = conn.execute(
            "UPDATE frames SET bayerpat = ?2,
                    xbayroff = COALESCE(xbayroff, ?3),
                    ybayroff = COALESCE(ybayroff, ?4),
                    roworder = COALESCE(roworder, ?5)
             WHERE id = ?1 AND bayerpat IS NULL",
            params![frame_id, bayerpat, cfa.xbayroff, cfa.ybayroff, cfa.roworder],
        )?;
        repaired += changed;
    }

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES (?1, 'done', datetime('now'))",
        params![CFA_BACKFILL_FLAG],
    )?;
    if repaired > 0 {
        tracing::info!(count = repaired, "cfa columns back-filled from stored headers");
    }
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    const OSC_HEADER: &str = "BAYERPAT= 'RGGB'\nXBAYROFF= 1\nYBAYROFF= 0\nROWORDER= 'BOTTOM-UP'\nEND";

    /// init_db itself runs the repair and stamps the flag — clear it so each
    /// test exercises a fresh run.
    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("DELETE FROM settings WHERE key = ?1", params![CFA_BACKFILL_FLAG])
            .unwrap();
        conn
    }

    fn insert_frame_with_header(
        conn: &Connection,
        id: i64,
        bayerpat: Option<&str>,
        header: &str,
    ) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, '/t/f' || ?1 || '.fits', 'f.fits', 1, '2026-01-01T00:00:00+00:00', 'FITS')",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, bayerpat, override) VALUES (?1, ?1, 'Light', ?2, 0)",
            params![id, bayerpat],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![id, header],
        )
        .unwrap();
    }

    #[test]
    fn fills_null_cfa_from_blob_and_stamps_flag() {
        let conn = fresh_conn();
        insert_frame_with_header(&conn, 1, None, OSC_HEADER);

        let repaired = backfill_cfa_from_stored_headers(&conn).unwrap();
        assert_eq!(repaired, 1);

        let (bp, xo, yo, ro, ov): (String, i64, i64, String, i64) = conn
            .query_row(
                "SELECT bayerpat, xbayroff, ybayroff, roworder, override FROM frames WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(bp, "RGGB");
        assert_eq!(xo, 1);
        assert_eq!(yo, 0);
        assert_eq!(ro, "BOTTOM-UP");
        assert_eq!(ov, 0, "repair must not set the override flag");

        // Second run: flag stamped, nothing rescanned.
        assert_eq!(backfill_cfa_from_stored_headers(&conn).unwrap(), 0);
    }

    #[test]
    fn existing_bayerpat_and_mono_rows_are_untouched() {
        let conn = fresh_conn();
        insert_frame_with_header(&conn, 1, Some("GBRG"), OSC_HEADER); // already set — keep
        insert_frame_with_header(&conn, 2, None, "EXPTIME = 300.0\nEND"); // mono — no cards

        let repaired = backfill_cfa_from_stored_headers(&conn).unwrap();
        assert_eq!(repaired, 0);

        let bp: String = conn
            .query_row("SELECT bayerpat FROM frames WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bp, "GBRG");
        let bp2: Option<String> = conn
            .query_row("SELECT bayerpat FROM frames WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert!(bp2.is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core db::repair`
Expected: compile FAIL (`mod repair` not declared).

- [ ] **Step 3: Register the module and the init_db tail call**

`db/mod.rs`: add `pub mod repair;` after `pub mod schema;`.

`schema.rs::init_db`, immediately before the final `Ok(())`:

```rust
    // One-time data repairs (settings-flag gated). Best-effort by design: a
    // repair failure must not brick catalog init — the error is logged here
    // and the flag stays unstamped so the next start retries.
    if let Err(e) = super::repair::backfill_cfa_from_stored_headers(conn) {
        tracing::error!(error = ?e, "cfa back-fill repair failed");
    }

    Ok(())
```

(Note the flag is stamped inside the repair only after the scan loop succeeds, so a mid-run error retries on next startup — matches the comment.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core db::repair`
Expected: 2 PASS.
Then the whole core suite for init_db fallout: `cargo test -p athenaeum-core`
Expected: PASS (in-memory DBs: repair sees empty tables, stamps the flag, returns 0).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/db/repair.rs crates/athenaeum-core/src/db/mod.rs crates/athenaeum-core/src/db/schema.rs
git commit -m "fix(db): one-time CFA back-fill of sync-erased columns from stored headers"
```

---

### Task 4: Core `run_operation_standalone` / `resume_operation_standalone`

**Files:**
- Modify: `crates/athenaeum-core/src/archive/executor.rs` (new fns + tests)
- Modify: `crates/athenaeum-core/src/archive/resume.rs` (new fn)
- Modify: `crates/athenaeum-core/src/archive/rollback.rs:46-55` (doc-comment inventory update)

**Interfaces:**
- Consumes: `run_operation(conn, operation_id, &CancelFlag, &dyn ProgressEmitter) -> Result<()>` (`executor.rs:54`), `resume_operation` with the same signature (`resume.rs:20`), `rollback_operation` (`rollback.rs:72`), `emit_event` (`events.rs`), `was_cancelled` (`executor.rs:45`).
- Produces: `pub fn run_operation_standalone(conn: &Connection, operation_id: i64, cancel: &CancelFlag, emitter: &dyn ProgressEmitter) -> Result<()>` (executor.rs), `pub fn resume_operation_standalone(…same signature…)` (resume.rs), `pub(crate) fn finish_forward_operation(conn, operation_id, emitter, result: Result<()>) -> Result<()>` (executor.rs). Wire shape of the terminal event is byte-compatible with today's tauri emit: `{"operation_id": <i64>, "outcome": "completed"|"cancelled"|"failed"}` (no `kind` — the frontend defaults to `'archive'`). Task 5's workers call the two standalone fns.

- [ ] **Step 1: Write the failing tests**

In `executor.rs`'s existing `mod tests`, add (copying `CapturingEmitter` from `rollback.rs:206-213` — the trait method is `emit_json`):

```rust
    use std::sync::Mutex;

    #[derive(Default)]
    struct CapturingEmitter(Mutex<Vec<(String, serde_json::Value)>>);

    impl ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    /// The standalone wrapper owns the terminal event both hosts' workers
    /// relied on hand-rolling (web forgot it; both resume workers forgot it).
    #[test]
    fn run_operation_standalone_emits_completed_terminal_event() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CapturingEmitter::default();

        run_operation_standalone(&conn, op_id, &cancel, &emitter).unwrap();

        let events = emitter.0.lock().unwrap();
        let finished: Vec<_> = events.iter().filter(|(n, _)| n == "archive-finished").collect();
        assert_eq!(finished.len(), 1, "exactly one terminal event");
        assert_eq!(finished[0].1["operation_id"], op_id);
        assert_eq!(finished[0].1["outcome"], "completed");
    }

    /// Failure path still emits — and still runs the inner rollback.
    #[test]
    fn run_operation_standalone_emits_failed_for_unknown_operation() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CapturingEmitter::default();

        let result = run_operation_standalone(&conn, 999, &cancel, &emitter);
        assert!(result.is_err(), "standalone must still surface the error to its caller");

        let events = emitter.0.lock().unwrap();
        assert!(
            events.iter().any(|(n, p)| n == "archive-finished" && p["outcome"] == "failed"),
            "terminal event must fire on failure too"
        );
    }
```

In `resume.rs`'s test module (create `#[cfg(test)] mod standalone_tests` at the end if none fits), add:

```rust
#[cfg(test)]
mod standalone_tests {
    use super::*;
    use crate::events::ProgressEmitter;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct CapturingEmitter(Mutex<Vec<(String, serde_json::Value)>>);

    impl ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    #[test]
    fn resume_operation_standalone_emits_terminal_event() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CapturingEmitter::default();

        let result = resume_operation_standalone(&conn, 999, &cancel, &emitter);
        assert!(result.is_err());

        let events = emitter.0.lock().unwrap();
        assert!(events
            .iter()
            .any(|(n, p)| n == "archive-finished" && p["outcome"] == "failed"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core run_operation_standalone resume_operation_standalone`
Expected: compile FAIL — the fns don't exist yet.

- [ ] **Step 3: Implement**

In `executor.rs`, after `run_operation`:

```rust
/// [`run_operation`] plus the outcome bookkeeping + terminal
/// `archive-finished` event every forward worker needs. Both hosts' start
/// workers call this (and `resume::resume_operation_standalone` the resume
/// workers) so the terminal block exists once — the web worker used to
/// hand-roll it and forgot the emit, leaving the progress widget mounted
/// forever. Same wire shape the desktop worker always emitted:
/// `{operation_id, outcome}` — no `kind`, the frontend defaults to
/// `'archive'`.
pub fn run_operation_standalone(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let result = run_operation(conn, operation_id, cancel, emitter);
    finish_forward_operation(conn, operation_id, emitter, result)
}

/// Shared tail of the two standalone entry points: status bookkeeping on
/// error (cancelled/failed + inner rollback), then the terminal event, then
/// the original result so callers can still see the error (already logged
/// here — dropping it at the call site is fine).
pub(crate) fn finish_forward_operation(
    conn: &Connection,
    operation_id: i64,
    emitter: &dyn ProgressEmitter,
    result: Result<()>,
) -> Result<()> {
    let outcome = match &result {
        Ok(()) => {
            tracing::info!(operation_id, "archive operation completed");
            "completed"
        }
        Err(e) => {
            let outcome = if was_cancelled(e) {
                let _ = adb::update_operation_status(conn, operation_id, ArchiveStatus::Cancelled, None);
                "cancelled"
            } else {
                tracing::error!(operation_id, error = ?e, "archive operation failed");
                let msg = format!("{:#}", e);
                let _ = adb::update_operation_status(conn, operation_id, ArchiveStatus::Failed, Some(&msg));
                "failed"
            };
            if let Err(rb_err) = crate::archive::rollback::rollback_operation(conn, operation_id, emitter) {
                tracing::error!(operation_id, error = ?rb_err, "rollback after failed archive operation also failed, operation may be left in an inconsistent state");
            }
            outcome
        }
    };
    emit_event(
        emitter,
        "archive-finished",
        &serde_json::json!({ "operation_id": operation_id, "outcome": outcome }),
    );
    result
}
```

(`executor.rs` already imports `adb`, `emit_event`, `ProgressEmitter`; add `use crate::archive::rollback;`-free full path as written. If `adb::update_operation_status` isn't in scope under that alias at module level, use `crate::archive::db::update_operation_status`.)

In `resume.rs`, after `resume_operation`:

```rust
/// [`resume_operation`] plus the shared terminal bookkeeping — see
/// `executor::run_operation_standalone`. Both hosts' resume workers call
/// this; neither used to emit any terminal event at all.
pub fn resume_operation_standalone(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let result = resume_operation(conn, operation_id, cancel, emitter);
    crate::archive::executor::finish_forward_operation(conn, operation_id, emitter, result)
}
```

Update `rollback.rs:46-55` doc comment — replace the "For the record, those callers are inconsistent today…" paragraph and its two bullets with:

```rust
/// The forward-path callers get the same treatment from
/// `executor::run_operation_standalone` / `resume::resume_operation_standalone`
/// (one shared terminal block in core, both hosts' workers call it); the
/// calibration-archive worker (`api::masters::archive_originals`) still emits
/// its own richer terminal payload. Only the *inner* [`rollback_operation`]
/// stays event-free — there a rollback is a sub-step of a failed operation,
/// and the outcome the UI must end on is the archive's, not the rollback's.
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core archive::`
Expected: PASS, including the three new tests and the existing `inner_rollback_emits_no_finished_event`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/archive/executor.rs crates/athenaeum-core/src/archive/resume.rs crates/athenaeum-core/src/archive/rollback.rs
git commit -m "feat(archive): core-owned terminal archive-finished for start + resume workers"
```

---

### Task 5: Switch all four workers to the standalone wrappers (both backends, one commit)

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/archive.rs:234-272` (start worker) and `:319-330` (resume worker)
- Modify: `crates/athenaeum-web/src/routes/archive.rs:242-278` (start worker) and `:390-401` (resume worker)

**Interfaces:**
- Consumes: `executor::run_operation_standalone`, `resume::resume_operation_standalone` (Task 4).
- Produces: identical terminal-event behavior on desktop and web; no route/command signatures change.

- [ ] **Step 1: Tauri start worker**

Replace the worker body match + emit block (`commands/archive.rs`, inside `run: Box::new(move || { … })`, currently lines ~238-268) with:

```rust
            let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            // Outcome bookkeeping + terminal `archive-finished` live in core
            // (`run_operation_standalone`) — shared with the web worker so the
            // two backends can't drift again. Errors are logged (and rolled
            // back) inside; nothing to add here.
            let _ = executor::run_operation_standalone(&conn, op_id, &cancel_flag, &emitter);
            let mut map = ctx_for_worker.active_archives.lock().unwrap();
            map.remove(&op_id);
```

- [ ] **Step 2: Tauri resume worker**

Replace the body of the resume closure (~lines 320-329) with:

```rust
            let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            // Terminal event + failure bookkeeping in core — this worker used
            // to emit nothing, so the (future) resume widget could never
            // dismiss. Errors logged inside.
            let _ = resume::resume_operation_standalone(&conn, operation_id, &cancel_flag, &emitter);
            ctx_for_worker.active_archives.lock().unwrap().remove(&operation_id);
```

- [ ] **Step 3: Web start worker**

Replace the worker body match block (`routes/archive.rs`, ~lines 243-277) with:

```rust
            let emitter = SseProgressEmitter::new(event_tx);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            // Outcome bookkeeping + terminal `archive-finished` live in core
            // (`run_operation_standalone`) — this worker used to hand-roll the
            // failure path and forgot the emit, so the web progress widget
            // never auto-dismissed. Errors are logged (and rolled back) inside.
            let _ = executor::run_operation_standalone(&conn, op_id, &cancel_flag, &emitter);
            ctx_for_worker
                .active_archives
                .lock()
                .unwrap()
                .remove(&op_id);
```

- [ ] **Step 4: Web resume worker**

Replace the resume closure body (~lines 391-400) with:

```rust
            let emitter = SseProgressEmitter::new(event_tx);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            // Terminal event + failure bookkeeping in core — kept in lockstep
            // with the desktop resume worker. Errors logged inside.
            let _ = resume::resume_operation_standalone(&conn, op_id, &cancel_flag, &emitter);
            ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
```

Clean up now-unused imports in both files (`ArchiveStatus`, `adb` — only if genuinely unused after the edit; `rollback` stays used by the rollback route/command; the compiler will report).

- [ ] **Step 5: Build both hosts + run their tests**

Run: `cargo build -p athenaeum-tauri -p athenaeum-web && cargo test -p athenaeum-web -p athenaeum-tauri`
Expected: clean build (one known pre-existing `FullBlind` warning elsewhere is OK), tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-tauri/src/commands/archive.rs crates/athenaeum-web/src/routes/archive.rs
git commit -m "fix(archive): web + resume workers emit the terminal archive-finished via core"
```

---

### Task 6: Resume shows the progress widget (frontend)

**Files:**
- Modify: `src/components/archive/ArchiveResumeBanner.tsx`

**Interfaces:**
- Consumes: `archive-finished` now emitted by resume (Task 5); existing `ArchiveProgress` component (dismisses via `onClose` on that event).
- Produces: UI-only change; mirrors the rollback widget pattern already in this file.

- [ ] **Step 1: Implement**

In `ArchiveResumeBanner.tsx`:

1. Extend the state + handler block (lines 10-18) to cover both worker kinds:

```tsx
  // Resume and rollback both run on the operation queue and report through
  // the shared archive-progress / archive-finished events — keep the widget
  // mounted until the terminal event, then retire the banner.
  const [rollingBack, setRollingBack] = useState(false);
  const [resuming, setResuming] = useState(false);

  const handleWorkerFinished = useCallback(() => {
    setRollingBack(false);
    setResuming(false);
    setDismissed(true);
  }, []);
```

2. Resume button `onClick` — keep the widget up instead of dismissing immediately:

```tsx
          onClick={async () => {
            setBusy(true);
            setResuming(true);
            try {
              await resumeArchiveOperation(op.id);
            } catch (e) {
              console.error('resume archive failed', e);
              setResuming(false);
              alert(`Resume failed: ${e}`);
            } finally {
              setBusy(false);
            }
          }}
```

3. All three buttons' `disabled` become `disabled={busy || rollingBack || resuming}` (the "Decide later" button gets the same guard so the banner can't vanish under a live widget).

4. Widget mount condition + handler swap (lines 86-90):

```tsx
      {(rollingBack || resuming) && (
        <div className="fixed bottom-4 right-4 z-50 w-80">
          <ArchiveProgress operationId={op.id} onClose={handleWorkerFinished} />
        </div>
      )}
```

5. Remove the now-unused `handleRollbackFinished` (replaced by `handleWorkerFinished` — update the rollback button's flow only in that it shares the new handler; its `onClick` logic is otherwise unchanged).

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/components/archive/ArchiveResumeBanner.tsx
git commit -m "fix(frontend): resume from the archive banner mounts the progress widget"
```

---

### Task 7: Web `query_frames_in_bounds` accepts the frontend's real payload

**Files:**
- Modify: `crates/athenaeum-web/src/routes/spatial.rs:14-39` (args struct) and `:63-73` (handler), + new test mod

**Interfaces:**
- Consumes: core `SelectionBounds` (`models.rs:446` — snake_case fields, `crosses_meridian` has `#[serde(default)]`), core `api::spatial::query_frames_in_bounds(conn, SelectionBounds)`.
- Produces: `pub struct QueryFramesInBoundsArgs { pub bounds: SelectionBounds }` — the wire shape `useRectangleSelection.ts:242` has always sent. Response shape unchanged. The dead `selected_object_ids` field is dropped (declared, never read, never sent).

- [ ] **Step 1: Write the failing test**

Append to `routes/spatial.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::QueryFramesInBoundsArgs;

    /// Byte-for-byte the payload `useRectangleSelection.ts` builds (nested
    /// `bounds`, snake_case — same envelope the Tauri command's named
    /// `bounds` argument + `rename_all = "snake_case"` accepts). Pinned so
    /// the web route can never again drift from the frontend's wire shape:
    /// it used to expect flat camelCase and 422'd every selection.
    #[test]
    fn deserializes_the_frontend_selection_payload() {
        let json = r#"{"bounds":{"ra_min":10.5,"ra_max":11.5,"dec_min":41.0,"dec_max":42.0,"crosses_meridian":false}}"#;
        let args: QueryFramesInBoundsArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.bounds.ra_min, 10.5);
        assert_eq!(args.bounds.ra_max, 11.5);
        assert_eq!(args.bounds.dec_min, 41.0);
        assert_eq!(args.bounds.dec_max, 42.0);
        assert_eq!(args.bounds.crosses_meridian, Some(false));
    }

    /// `crosses_meridian` is `#[serde(default)]` on the core type — an older
    /// client omitting it must still deserialize.
    #[test]
    fn crosses_meridian_is_optional() {
        let json = r#"{"bounds":{"ra_min":0.0,"ra_max":1.0,"dec_min":0.0,"dec_max":1.0}}"#;
        let args: QueryFramesInBoundsArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.bounds.crosses_meridian, None);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-web spatial`
Expected: compile FAIL — `QueryFramesInBoundsArgs` doesn't exist.

- [ ] **Step 3: Implement**

Replace lines 14-39 (the wrong-comment + `SelectionBoundsArgs` + `From` impl) with:

```rust
/// Wire shape of `query_frames_in_bounds`: the frontend sends the core
/// `SelectionBounds` (snake_case, no `rename_all`) nested under a `bounds`
/// key — the same envelope the Tauri command (named `bounds` argument +
/// `rename_all = "snake_case"`) has always accepted. Mirrors the
/// `CreateFrameSetFromSelectionArgs` precedent in `frame_sets.rs`.
#[derive(Deserialize)]
pub struct QueryFramesInBoundsArgs {
    pub bounds: SelectionBounds,
}
```

Handler (lines ~63-73): change the extractor and the call:

```rust
#[tracing::instrument(skip_all, err(Debug))]
pub async fn query_frames_in_bounds(
    State(state): State<WebAppState>,
    Json(args): Json<QueryFramesInBoundsArgs>,
) -> Result<Json<SelectionCandidates>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    api::query_frames_in_bounds(&db.conn(), args.bounds)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
```

(If `api::query_frames_in_bounds` takes the bounds by value vs reference differently, match the existing call — today it's `args.into()` by value, so `args.bounds` is a drop-in.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-web`
Expected: PASS including the 2 new tests.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-web/src/routes/spatial.rs
git commit -m "fix(web): query_frames_in_bounds accepts the nested snake_case bounds payload"
```

---

### Task 8: Full gates

**Files:** none (verification only).

- [ ] **Step 1: Workspace build**

Run: `cargo build --workspace`
Expected: clean (known pre-existing `FullBlind` warning only).

- [ ] **Step 2: Full test suites**

Run: `cargo test --workspace`
Expected: PASS (core ≈1525+ baseline + new tests, web 24+2, tauri 22).

- [ ] **Step 3: Frontend gates**

Run: `npx tsc --noEmit && npm run build:web`
Expected: both clean.

- [ ] **Step 4: No stray commit content**

Run: `git status && git log --oneline -8`
Expected: clean tree; 7 new commits atop `8e2743f6` on `0.5.1`. Do not push.

## Post-plan notes (not tasks)

- **Owner smoke additions:** web build — archive a frame set, widget must auto-dismiss and the page reload; banner Resume shows a progress widget; web sky map rectangle select returns candidates (no 422); sync one OSC frame between two devices and check `bayerpat` lands non-NULL on the receiver; existing catalogs back-fill on first start (`query_logs` for `"cfa columns back-filled from stored headers"`).
- **Release-note lines owed:** web archive progress now completes/dismisses; web sky-map selection fixed; OSC frames transferred between devices keep their Bayer metadata (existing received frames repaired automatically on first start).
- Out of scope, deliberately: `swcreate`/`rotation` back-fill for historical synced rows (future sends carry them after Task 1); peer-side repair of catalogs never opened by a fixed build; the dead `file_op::models::StepStatus::RolledBack` variant (tracked separately).
