# Windows Path & Calibration Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the verified Windows/cross-platform path defects from the 2026-07-22 audit (`docs/superpowers/research/2026-07-22-windows-path-crossplatform-audit.md`): directory send from the file manager, directory-rename catalog desync, calibration sanitizer/reconcile failures, the master-build `UNIQUE files.path` dead loop, and the sign-in-blocking device-key lock.

**Architecture:** All fixes live in `athenaeum-core` (shared by both backends — no Tauri command or Axum route surface changes, so the two-backend rule is satisfied by construction) plus two small frontend files. Each fix copies an already-correct in-repo pattern: separator handling from `get_files_by_directory`, dot-trimming from `sanitize_batch_slug`, both-separator prefix checks from `DualPaneFileBrowser`.

**Tech Stack:** Rust (rusqlite, fd-lock 4.0.4), React/TS. Tests: `cargo test -p athenaeum-core` with in-memory SQLite; Windows-shaped paths are plain strings, so every new test runs on macOS/Linux CI.

## Global Constraints

- Branch: `0.5.0` (current version branch; USER RULE — version branches).
- Commit as the user (`eg013ra1n` / `vilen.sharifov@gmail.com`); NEVER add Claude as author/co-author (owner rule overrides harness default).
- `anyhow::Result` inside core; `tracing` only, message = short stable phrase + snake_case fields; zero `println!`.
- Gates per task: the `cargo check` hook fires on edit; run the named tests. Final gates: `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`.
- Minimal scope: no drive-by refactors; the Backlog section at the end is explicitly OUT of this cycle.

---

### Task 1: Folder→frames resolution works with Windows paths (fixes directory send)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs:3008` (`frame_ids_under_paths`) + new helper near `path_prefix_upper` (~line 38)
- Test: same file, test module (existing `insert_frame` helper at the bottom test mod returns the generated `frame_id`)

**Interfaces:**
- Produces: `pub(crate) fn native_separator_of(path: &str) -> char` in `db/operations.rs`. `db/mod.rs` already glob-re-exports (`mod operations; pub use operations::*;` — verified), so Task 2 calls it as `crate::db::native_separator_of`.

- [ ] **Step 1: Write the failing test** (append to the test mod holding `frame_ids_under_paths_matches_file_and_folder`, ~line 3834):

```rust
#[test]
fn frame_ids_under_paths_windows_backslash_paths() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();

    let a = insert_frame(&conn, r"C:\Astro\M31\L_0001.fits", Some("Light"));
    let b = insert_frame(&conn, r"C:\Astro\M31\sub\L_0002.fits", Some("Light"));
    let sib = insert_frame(&conn, r"C:\Astro\M31extra\L_0003.fits", Some("Light"));

    // Folder select sweeps descendants at any depth — the reported Windows
    // bug returned an empty list here (hardcoded '/' prefix).
    let mut got = frame_ids_under_paths(&conn, &[r"C:\Astro\M31".into()]).unwrap();
    got.sort();
    let mut expect = vec![a, b];
    expect.sort();
    assert_eq!(got, expect);

    // Trailing backslash tolerated (trim_end_matches('/') was a no-op).
    let mut got = frame_ids_under_paths(&conn, &[r"C:\Astro\M31\".into()]).unwrap();
    got.sort();
    assert_eq!(got, expect);

    // Sibling folder sharing a name prefix must not be swept in.
    assert!(!got.contains(&sib));

    // Exact-file branch unchanged.
    assert_eq!(
        frame_ids_under_paths(&conn, &[r"C:\Astro\M31extra\L_0003.fits".into()]).unwrap(),
        vec![sib]
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core frame_ids_under_paths_windows -- --nocapture`
Expected: FAIL — first assert gets `[]` instead of `[a, b]`.

- [ ] **Step 3: Implement** — helper next to `path_prefix_upper`:

```rust
/// The native separator a stored catalog path uses. Catalog paths are always
/// absolute native paths (WalkDir + canonicalized roots): POSIX ones start
/// with `/`, Windows ones with a drive letter or UNC `\\` — the leading
/// character decides, not the build OS, which keeps the predicate builders
/// testable with Windows-shaped fixtures from any host.
pub(crate) fn native_separator_of(path: &str) -> char {
    if path.starts_with('/') { '/' } else { '\\' }
}
```

and in `frame_ids_under_paths` replace

```rust
let prefix = format!("{}/", path.trim_end_matches('/'));
```

with

```rust
let sep = native_separator_of(path);
let prefix = format!("{}{}", path.trim_end_matches(sep), sep);
```

Also update the function's doc comment (lines ~2968-2991) where it spells the range as `path >= "<path>/"` — say "path + native separator" instead.

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core frame_ids_under_paths -- --nocapture`
Expected: PASS (new test + the existing POSIX one — the POSIX fixtures start with `/`, so they take the `'/'` branch unchanged).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/db/operations.rs
git commit -m "fix(files): folder->frame resolution uses the path's native separator (Windows dir send)"
```

---

### Task 2: Directory-rename catalog hot-sync works with Windows paths

**Files:**
- Modify: `crates/athenaeum-core/src/api/files.rs:736-737` (`rename_path`, dir branch)
- Test: `crates/athenaeum-core/src/db/operations.rs` test mod (pin `rename_files_path_prefix` with backslash prefixes) + a pure-helper test in `api/files.rs`

**Interfaces:**
- Consumes: `crate::db::operations::native_separator_of` from Task 1.
- Produces: `fn dir_rename_prefixes(old_str: &str, new_str: &str) -> (String, String)` (private to `api/files.rs`).

- [ ] **Step 1: Write the failing tests.**

In `api/files.rs` test mod (create `#[cfg(test)] mod tests` at the bottom if the file has none):

```rust
#[test]
fn dir_rename_prefixes_use_native_separator() {
    assert_eq!(
        dir_rename_prefixes(r"C:\data\Old", r"C:\data\New"),
        (r"C:\data\Old\".to_string(), r"C:\data\New\".to_string())
    );
    assert_eq!(
        dir_rename_prefixes("/data/Old", "/data/New"),
        ("/data/Old/".to_string(), "/data/New/".to_string())
    );
}
```

In `db/operations.rs` test mod, next to the existing `rename_files_path_prefix` tests (~line 4128):

```rust
#[test]
fn rename_files_path_prefix_windows_backslash() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    insert_frame(&conn, r"C:\data\Old\a.fits", Some("Light"));
    insert_frame(&conn, r"C:\data\Old\sub\b.fits", Some("Light"));
    insert_frame(&conn, r"C:\data\Oldextra\c.fits", Some("Light"));

    let n = rename_files_path_prefix(&conn, r"C:\data\Old\", r"C:\data\New\").unwrap();
    assert_eq!(n, 2, "both descendants rewired, sibling-prefix folder untouched");

    let moved: i64 = conn
        .query_row(
            r"SELECT COUNT(*) FROM files WHERE path LIKE 'C:\data\New\%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(moved, 2);
}
```

- [ ] **Step 2: Run to verify state** — the `operations.rs` test PASSES already (the SUBSTR swap is separator-agnostic; it pins the contract). The `api/files.rs` test FAILS to compile (`dir_rename_prefixes` undefined).

Run: `cargo test -p athenaeum-core rename_files_path_prefix_windows dir_rename_prefixes`

- [ ] **Step 3: Implement** in `api/files.rs` — add above `rename_path`:

```rust
/// Directory-rename prefixes for `rename_files_path_prefix`, built with the
/// path's own native separator (the doc contract there requires a trailing
/// separator; hardcoding '/' silently matched zero rows on Windows).
fn dir_rename_prefixes(old_str: &str, new_str: &str) -> (String, String) {
    let sep = crate::db::native_separator_of(old_str);
    (format!("{old_str}{sep}"), format!("{new_str}{sep}"))
}
```

and in the dir branch replace lines 736-737 with:

```rust
let (prefix_old, prefix_new) = dir_rename_prefixes(&old_str, &new_str);
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core rename_files_path_prefix dir_rename_prefixes`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/api/files.rs crates/athenaeum-core/src/db/operations.rs
git commit -m "fix(files): dir-rename hot-sync builds prefixes with the native separator (Windows catalog desync)"
```

---

### Task 3: Sanitizer hardening — trailing dots, reserved names, dot-segments, DATE-OBS

**Files:**
- Modify: `crates/athenaeum-core/src/archive/path_layout.rs:8-24` (`sanitize_for_filename`)
- Modify: `crates/athenaeum-core/src/api/lights.rs:701-707` (`date_part`)
- Test: both files' test mods

**Interfaces:**
- `sanitize_for_filename` keeps its signature `pub fn sanitize_for_filename(&str) -> String`; behavior change: trailing `.`/space trimmed, Windows reserved device names suffixed with `_`, `"."`/`".."` collapse to `""` (callers' `sanitized_or`/`token` fallbacks then apply). All existing callers of THIS function (master/light paths, archive zip names, sync slugs) inherit the hardening — export folder names do NOT (they use their own `sanitize_display_folder_name` in `export/models.rs:242`, a separate audit gap left for backlog); `sync/receiver.rs::sanitize_batch_slug`'s own `.trim_matches('.')` becomes redundant but harmless — leave it (minimal scope).

- [ ] **Step 1: Write the failing tests.** In `path_layout.rs` test mod:

```rust
#[test]
fn sanitize_trims_trailing_dots_and_guards_reserved_names() {
    // Windows strips trailing dots at create time; the sanitizer must match,
    // or DB paths diverge from disk (audit F3).
    assert_eq!(sanitize_for_filename("Sh2-155."), "Sh2-155");
    assert_eq!(sanitize_for_filename("NGC 7000 "), "NGC_7000");
    // Dot-segments must not survive as path components (library-root escape).
    assert_eq!(sanitize_for_filename("."), "");
    assert_eq!(sanitize_for_filename(".."), "");
    // Reserved device names (any case, with or without extension) are illegal
    // as any Windows path segment — defuse with a trailing underscore.
    assert_eq!(sanitize_for_filename("NUL"), "NUL_");
    assert_eq!(sanitize_for_filename("nul"), "nul_");
    assert_eq!(sanitize_for_filename("COM3"), "COM3_");
    assert_eq!(sanitize_for_filename("lpt9.fits"), "lpt9.fits_");
    // Not reserved: COM0, COM10, plain names, inner dots.
    assert_eq!(sanitize_for_filename("COM0"), "COM0");
    assert_eq!(sanitize_for_filename("com10"), "com10");
    assert_eq!(sanitize_for_filename("M31"), "M31");
    assert_eq!(sanitize_for_filename("DMK 41AU02.AS"), "DMK_41AU02.AS");
}
```

In `api/lights.rs` test mod:

```rust
#[test]
fn date_part_sanitizes_non_iso_values() {
    assert_eq!(date_part(Some("2026-07-05T20:30:00Z")), "2026-07-05");
    // Malformed locale date: '/' must not become directory nesting, ':' is
    // Windows-illegal — both map to '_' (audit F6).
    assert_eq!(date_part(Some("05/07/2026")), "05_07_2026");
    assert_eq!(date_part(None), "UnknownDate");
    assert_eq!(date_part(Some("")), "UnknownDate");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core sanitize_trims_trailing date_part_sanitizes`
Expected: FAIL (`"Sh2-155."` comes back unchanged; `05/07/2026` keeps its slashes).

- [ ] **Step 3: Implement.** `sanitize_for_filename` — replace the final line `out.trim_matches('_').to_string()` with:

```rust
    let out = out.trim_matches('_').trim_end_matches(['.', ' ']).to_string();
    // Windows reserves CON/PRN/AUX/NUL/COM1-9/LPT1-9 as any path segment,
    // case-insensitive, with or without an extension — suffix to defuse.
    let base = out.split('.').next().unwrap_or("");
    let upper = base.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        format!("{out}_")
    } else {
        out
    }
```

`date_part` in `api/lights.rs`:

```rust
/// YYYY-MM-DD from a DATE-OBS string (`2026-07-05T20:30:00Z` → `2026-07-05`).
/// The result becomes a path segment, so it goes through the shared
/// sanitizer — a malformed non-ISO DATE-OBS must not nest directories ('/')
/// or hit Windows-illegal chars (':'). Missing/empty/unsalvageable →
/// `"UnknownDate"` so the layout never gets an empty segment.
fn date_part(date_obs: Option<&str>) -> String {
    let raw: String = date_obs
        .and_then(|d| d.split('T').next())
        .map(|d| d.chars().take(10).collect())
        .unwrap_or_default();
    let sanitized = crate::archive::path_layout::sanitize_for_filename(&raw);
    if sanitized.is_empty() {
        "UnknownDate".to_string()
    } else {
        sanitized
    }
}
```

- [ ] **Step 4: Run the full core suite** (this fn has many callers — archive zip names, export folders, sync slugs; existing tests pin their shapes):

Run: `cargo test -p athenaeum-core`
Expected: PASS. If an existing pin asserts a name ending in `.`/space or a reserved name (unlikely), the PIN is asserting the broken behavior — update it and say so in the commit body.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/archive/path_layout.rs crates/athenaeum-core/src/api/lights.rs
git commit -m "fix(calibration): filename sanitizer trims trailing dots, guards reserved names; DATE-OBS segment sanitized"
```

**Migration note (document, no code):** rows already written with a trailing-dot segment on Windows stay divergent; Task 5's same-file repair heals `light_calibrations.output_path` on the next scan. Phantom dotted MASTER `files` rows are left in place (project rule: missing files are not orphans) — Task 4 makes builds route around them.

---

### Task 4: Catalog-aware collision resolution — kills the `UNIQUE files.path` build loop

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/paths.rs` (add `resolve_collision_free`)
- Modify: `crates/athenaeum-core/src/api/masters.rs:601` (preview), `:800` (build), `:825-833` (failure branch)
- Modify: `crates/athenaeum-core/src/api/lights.rs:970` (light output resolution)
- Modify: `crates/athenaeum-core/src/db/light_calibrations.rs` (add `output_path_exists`)
- Test: `paths.rs` + `light_calibrations.rs` test mods

**Interfaces:**
- Produces: `pub fn resolve_collision_free(abs: &Path, is_taken: &dyn Fn(&str) -> bool) -> PathBuf` (paths.rs); `pub fn output_path_exists(conn: &Connection, path: &str) -> anyhow::Result<bool>` (db/light_calibrations.rs).
- Consumes: `crate::db::file_exists(conn, &str) -> Result<bool>` (exists, `db/operations.rs:1887`).
- `resolve_collision` stays for callers with no catalog domain; masters/lights call sites switch to `resolve_collision_free`.

- [ ] **Step 1: Write the failing tests.** In `paths.rs` test mod:

```rust
#[test]
fn resolve_collision_free_skips_catalog_taken_paths() {
    let dir = tempfile::tempdir().unwrap();
    let abs = dir.path().join("master_dark_300s.fits");

    // Disk-free + catalog-free → as-is.
    let taken_none = |_: &str| false;
    assert_eq!(resolve_collision_free(&abs, &taken_none), abs);

    // Disk-free but a catalog row survived its file (audit F4b): today's
    // disk-only resolve_collision returns `abs` and registration dies on
    // UNIQUE files.path forever — the resolver must suffix past it.
    let phantom = abs.to_string_lossy().to_string();
    let taken_phantom = move |p: &str| p == phantom;
    assert_eq!(
        resolve_collision_free(&abs, &taken_phantom),
        dir.path().join("master_dark_300s_2.fits")
    );

    // Disk-taken behaves like resolve_collision.
    std::fs::write(&abs, b"x").unwrap();
    assert_eq!(
        resolve_collision_free(&abs, &taken_none),
        dir.path().join("master_dark_300s_2.fits")
    );
}
```

In `db/light_calibrations.rs` test mod (its tests use `Connection::open_in_memory` + `init_db`; `frame_id: None` is legal — "adopted" rows — so no FK seeding needed):

```rust
#[test]
fn output_path_exists_matches_stored_rows() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    assert!(!output_path_exists(&conn, "/lib/M31/c_a.fits").unwrap());
    let row = LightCalRow {
        id: 0,
        frame_id: None,
        source_uuid: Some("uuid-op-exists".into()),
        source_filename: Some("a.fits".into()),
        output_path: "/lib/M31/c_a.fits".into(),
        dark_set_id: None,
        flat_set_id: None,
        bias_set_id: None,
        calstat: "D".into(),
        flat_norm_applied: false,
        flat_norm_mode: "centralThird".into(),
        output_hash: "h".into(),
        engine_version: LIGHT_CAL_ENGINE_VERSION,
        created_at: "2026-01-01T00:00:00Z".into(),
        cal_params: "{}".into(),
    };
    upsert_light_calibration(&conn, &row).unwrap();
    assert!(output_path_exists(&conn, "/lib/M31/c_a.fits").unwrap());
    assert!(!output_path_exists(&conn, "/lib/M31/c_b.fits").unwrap());
}
```

- [ ] **Step 2: Run to verify compile failure** (`resolve_collision_free`, `output_path_exists` undefined):

Run: `cargo test -p athenaeum-core resolve_collision_free output_path_exists`

- [ ] **Step 3: Implement.** `paths.rs`, under `resolve_collision`:

```rust
/// Like [`resolve_collision`], but a candidate is free only when it is BOTH
/// absent on disk AND not claimed by the catalog domain the output registers
/// into (`is_taken`). A catalog row that outlived its on-disk file otherwise
/// wedges every future build on a UNIQUE-path constraint (2026-07-22 audit
/// F4b) — and the failure path used to delete the freshly built file,
/// making the state permanent.
pub fn resolve_collision_free(abs: &Path, is_taken: &dyn Fn(&str) -> bool) -> PathBuf {
    let free = |p: &Path| !p.exists() && !is_taken(&p.to_string_lossy());
    if free(abs) {
        return abs.to_path_buf();
    }
    let stem = abs.file_stem().and_then(|s| s.to_str()).unwrap_or("master");
    let ext = abs.extension().and_then(|s| s.to_str()).unwrap_or("fits");
    for n in 2u32.. {
        let candidate = abs.with_file_name(format!("{stem}_{n}.{ext}"));
        if free(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}
```

`db/light_calibrations.rs`:

```rust
/// Whether any tracking row already claims `path` as its output — the
/// UNIQUE(output_path) domain the light-output collision resolver checks.
pub fn output_path_exists(conn: &Connection, path: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM light_calibrations WHERE output_path = ?1",
        params![path],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}
```

`api/masters.rs` — both call sites (`:601` preview, `:800` build; `conn` is in scope at both):

```rust
resolve_collision_free(&library_dir.join(&target_rel), &|p| {
    crate::db::file_exists(&conn, p).unwrap_or(false)
})
```

(update the `use` at line 44 to import `resolve_collision_free`; drop `resolve_collision` from the import if now unused).

`api/masters.rs:825-833` failure branch — stop destroying the artifact, carry the path in the error:

```rust
BuildTarget::New => match register_master(&conn, set_id, &target_abs, &recipe_json) {
    Ok(reg) => Ok(reg.master_set_id),
    Err(e) => {
        // Keep the written master on disk: deleting it on a catalog
        // conflict re-created the exact divergence that caused the
        // failure (permanent build loop, audit F4b). The next library
        // scan ingests it as an imported master instead — visible, not
        // lost. The path in the error makes user reports actionable.
        Err(BuildStepError::Other(format!(
            "register master at {}: {e:#}",
            target_abs.display()
        )))
    }
},
```

`api/lights.rs:970` (re-acquire a conn — the earlier `conn` was scoped to the resolve block):

```rust
None => {
    let rel = calibrated_light_relative_path(
        &resolved.object,
        &resolved.instrume,
        &resolved.date_obs_date,
        &output_basename_fits(&resolved.source_filename),
    );
    let conn = db.conn();
    resolve_collision_free(&library_dir.join(&rel), &|p| {
        crate::db::light_calibrations::output_path_exists(&conn, p).unwrap_or(false)
    })
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core resolve_collision output_path_exists masters lights`
Expected: PASS, including the existing `register_master_full_roundtrip` and `direct_registration_matches_scanner_ingestion` pins.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/calibration_library/paths.rs crates/athenaeum-core/src/api/masters.rs crates/athenaeum-core/src/api/lights.rs crates/athenaeum-core/src/db/light_calibrations.rs
git commit -m "fix(calibration): collision resolver checks the catalog too; registration failure keeps the built master and names the path"
```

**Race note (documented, no code this cycle):** the library-root monitor can still scan-ingest a master in the write→register window; after this task that attempt fails once with an actionable error (file kept, row visible) and the NEXT build suffixes `_2` and succeeds — degraded, never wedged. A broader guard (pause monitor per build) is Backlog.

---

### Task 5: Scanner reconcile — same-physical-file repair instead of phantom duplicate

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` (~line 697, the `reconcile_calibrated_light` branch chain)
- Test: scanner test mod (or the module's existing reconcile tests — extend there)

**Interfaces:**
- Consumes: `update_output_path(conn, row.id, current_path)` (exists, `db/light_calibrations.rs:341`).

- [ ] **Step 1: Write the failing test.** `reconcile_calibrated_light` (`scanner/mod.rs:667`) is a private fn with signature `(conn: &Connection, path: &Path, current_path: &str, identity: &CalibratedIdentity, root_id: i64, calibrated_duplicates_out: &mut Vec<CalibratedDuplicate>) -> anyhow::Result<()>` — it has no dedicated unit tests today, so this test seeds its own row (add to the `scanner/mod.rs` test mod; same file, so the private fn is callable):

```rust
#[test]
fn reconcile_repairs_row_when_stored_and_scanned_paths_are_same_file() {
    use crate::db::light_calibrations::{upsert_light_calibration, LightCalRow};
    use crate::fits_parser::calibrated_light::CalibratedIdentity;
    use crate::models::LIGHT_CAL_ENGINE_VERSION;

    // Windows path normalization (trailing dots, case) can make the stored
    // output_path and the scanned path spell the SAME physical file
    // differently; branch 3 then flags a phantom duplicate on every scan.
    // Cross-platform stand-in for normalization: a lexically different but
    // canonically identical spelling of one path (`M31/../M31/…`).
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("M31");
    std::fs::create_dir_all(&sub).unwrap();
    let real = sub.join("c_L_0001.fits");
    std::fs::write(&real, b"stub").unwrap();
    let detoured = dir.path().join("M31").join("..").join("M31").join("c_L_0001.fits");

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    upsert_light_calibration(&conn, &LightCalRow {
        id: 0,
        frame_id: None,
        source_uuid: Some("uuid-same-file".into()),
        source_filename: Some("L_0001.fits".into()),
        output_path: detoured.to_string_lossy().to_string(),
        dark_set_id: None,
        flat_set_id: None,
        bias_set_id: None,
        calstat: "D".into(),
        flat_norm_applied: false,
        flat_norm_mode: "centralThird".into(),
        output_hash: "h".into(),
        engine_version: LIGHT_CAL_ENGINE_VERSION,
        created_at: "2026-01-01T00:00:00Z".into(),
        cal_params: "{}".into(),
    }).unwrap();

    let identity = CalibratedIdentity {
        source_uuid: Some("uuid-same-file".into()),
        source_filename: Some("L_0001.fits".into()),
        source_object: None,
        source_date_obs: None,
    };
    let current = real.to_string_lossy().to_string();
    let mut dups = Vec::new();
    reconcile_calibrated_light(&conn, &real, &current, &identity, 1, &mut dups).unwrap();

    assert!(dups.is_empty(), "same physical file must not be flagged duplicate");
    let stored: String = conn
        .query_row(
            "SELECT output_path FROM light_calibrations WHERE source_uuid = 'uuid-same-file'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, current, "output_path repaired to the scanned spelling");
}
```

(If `CalibratedIdentity` has fields beyond these four, initialize the extras with `..Default::default()` or `None` per its definition in `fits_parser/calibrated_light.rs:27`.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core reconcile_repairs_row -- --nocapture`
Expected: FAIL — `dups` contains one `(kept, duplicate)` pair (branch 3 fires: the detoured path `exists()`).

- [ ] **Step 3: Implement.** In the branch chain, replace the bare `else` (branch 3) with:

```rust
} else if std::fs::canonicalize(&row.output_path)
    .ok()
    .zip(std::fs::canonicalize(Path::new(current_path)).ok())
    .is_some_and(|(a, b)| a == b)
{
    // Branch 2b: both spellings resolve to one physical file (Windows
    // trailing-dot/case normalization, symlinked mounts). Repair the
    // pointer to the scanned spelling — flagging it as a duplicate would
    // warn on every scan forever.
    update_output_path(conn, row.id, current_path)?;
    tracing::info!(
        root_id,
        old_path = %row.output_path,
        path = %current_path,
        "calibrated light path spelling normalized — output_path repaired"
    );
} else {
    // Branch 3 (unchanged): a real second copy elsewhere.
    ...existing branch-3 body...
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core reconcile -- --nocapture`
Expected: PASS, including the existing branch-3 duplicate test (two distinct real files still flag).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/scanner/mod.rs
git commit -m "fix(scanner): reconcile repairs same-physical-file path spellings instead of phantom duplicates"
```

---

### Task 6: Device-key lock moves to a sidecar file (fixes Windows sign-in, os error 33)

**Files:**
- Modify: `crates/athenaeum-core/src/account/keys.rs` (`DeviceKeyLock::acquire`, new `device_key_lock_path`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs:799` (the only `acquire` caller)
- Test: `keys.rs` test mod

**Interfaces:**
- Produces: `pub fn device_key_lock_path(sync_dir: &Path) -> PathBuf` (returns `<sync_dir>/device_key.lock`); `DeviceKeyLock::acquire(lock_path: &Path)` keeps its signature but now CREATES the file if absent (`create(true)`) — it receives the sidecar path, never the key path.

- [ ] **Step 1: Write the failing test** (in `keys.rs` test mod, next to the existing lock tests):

```rust
#[test]
fn key_file_stays_readable_while_lock_is_held() {
    // fd-lock on Windows is LockFileEx — a MANDATORY lock: any other handle's
    // read of the key file fails with os error 33 while the node holds it
    // (seen live: account_sign_in_verify in the 2026-07-18 user log). The
    // lock must therefore live on a sidecar, never on the key file itself.
    let dir = tempfile::tempdir().unwrap();
    let key = DeviceKey::load_or_create_in(dir.path()).unwrap();

    let lock_path = device_key_lock_path(dir.path());
    let _held = DeviceKeyLock::acquire(&lock_path).unwrap();
    assert!(lock_path.ends_with("device_key.lock"), "lock must be the sidecar");

    // Re-reading the key while the lock is held must work on EVERY platform.
    let reread = DeviceKey::load_or_create_in(dir.path()).unwrap();
    assert_eq!(key.secret_bytes(), reread.secret_bytes());

    // Exclusivity is preserved: a second acquire on the sidecar fails.
    assert!(DeviceKeyLock::acquire(&lock_path).is_err());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core key_file_stays_readable -- --nocapture`
Expected: FAIL to compile (`device_key_lock_path` undefined). (On unix the read would pass even today — advisory flock — which is exactly why this never reproduced on macOS; the compile-gate + sidecar assertion is what pins the fix.)

- [ ] **Step 3: Implement.** In `keys.rs`, next to `device_key_path`:

```rust
/// Sidecar lock file guarding the device identity. The lock must NOT be on
/// `device_key` itself: fd-lock maps to `LockFileEx` on Windows, a MANDATORY
/// lock that fails every other handle's read of the key (os error 33) —
/// including account sign-in — for as long as the iroh node holds it. Unix
/// `flock` is advisory, which is why this only broke on Windows.
pub fn device_key_lock_path(sync_dir: &Path) -> PathBuf {
    sync_dir.join("device_key.lock")
}
```

In `DeviceKeyLock::acquire` (~line 126) change the open to create the sidecar when missing, and update the doc ("which must already exist" no longer holds):

```rust
let file = std::fs::OpenOptions::new()
    .create(true)
    .truncate(false)
    .read(true)
    .write(true)
    .open(key_path)
    .with_context(|| format!("open device key lock {}", key_path.display()))?;
```

(rename the local/doc wording from "device key" to "device key lock"; the `device_key_in_use_message` text keeps working — it prints the path it was given.)

In `node.rs:799`:

```rust
let key_lock = DeviceKeyLock::acquire(&device_key_lock_path(sync_dir))?;
```

(adjust the `use` to import `device_key_lock_path`).

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core keys -- --nocapture` and `cargo test -p athenaeum-core sharing::iroh` (node lock/re-bind tests, if present).
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/account/keys.rs crates/athenaeum-core/src/sharing/iroh/node.rs
git commit -m "fix(account): device-key lock moves to a sidecar file — Windows mandatory lock blocked sign-in (os error 33)"
```

**Mixed-version note (document only):** an OLD build locks `device_key`, a NEW build locks the sidecar — running both simultaneously on one machine loses mutual exclusion between them. Accepted edge; single-version machines are unaffected in both directions.

---

### Task 7: Frontend — coverage hint separator + Black Hole folder boundary

**Files:**
- Modify: `src/components/CalibrationFolderSection.tsx:99`
- Modify: `src/pages/FileManager.tsx:871`, `:916` (folder A/B "Move Folder to Black Hole" handlers)

No FE test runner in this repo — the gate is `npx tsc --noEmit` + the owner smoke below.

- [ ] **Step 1: CalibrationFolderSection** — check both separators (same pattern as `DualPaneFileBrowser.tsx:265-266`):

```tsx
const coveringRoot = dir
  ? scanRoots.find(r =>
      r.kind !== 'calibration_library' &&
      (dir === r.path || dir.startsWith(r.path + '/') || dir.startsWith(r.path + '\\')))
  : undefined;
```

- [ ] **Step 2: FileManager** — add one helper above the component (or near the handlers):

```tsx
// Boundary-safe folder membership: `/data/Set1` must not capture
// `/data/Set10` (this feeds a move-to-Black-Hole, so a false match
// relocates real files). Separator detected from the folder path itself.
const isInFolder = (path: string, folder: string) =>
  path.startsWith(folder + (folder.includes('\\') ? '\\' : '/'));
```

and in BOTH handlers replace `if (path.startsWith(folder.folder_a))` / `folder_b` with `if (isInFolder(path, folder.folder_a))` / `isInFolder(path, folder.folder_b)`.

- [ ] **Step 3: Typecheck**

Run: `npx tsc --noEmit`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/components/CalibrationFolderSection.tsx src/pages/FileManager.tsx
git commit -m "fix(ui): Windows-safe coverage-root hint; boundary-safe Black Hole folder match"
```

---

### Final gates + owner smoke (Windows)

- [ ] `cargo build --workspace` && `cargo test -p athenaeum-core` && `npx tsc --noEmit`
- [ ] Owner smoke on the Windows machine:
  1. File manager → select a directory with nested files → Send to device → dialog opens with the frame count (was: "No cataloged frames in the selection").
  2. Rename a directory inside a scan root → `sqlite3` check: `SELECT COUNT(*) FROM files WHERE path LIKE '<new>%'` matches the tree; no missing-file rows appear.
  3. Set a frame set's OBJECT to `M31.` → Calibrate Lights → output lands under `M31\`, DB `output_path` matches disk, second scan produces NO duplicate warning.
  4. Master build on a set whose target path has a leftover catalog row → build succeeds at `_2` (was: `UNIQUE constraint failed: files.path` forever).
  5. With sync running (iroh node started), sign out/in → no `os error 33`.

### Out of scope — Backlog (needs its own cycle / design decision)

- `relinking/mod.rs:57,195` — `LIKE '{root}%'`: add trailing-separator boundary + `ESCAPE` for `%`/`_` (candidates are fingerprint-filtered today, so impact is accounting noise).
- `db/operations.rs:78` `scan_root_prefix_predicate` (+ callers at `:381`, `:459`) — append the native separator to the root before building the range (sibling-name bleed, both platforms).
- `DirectoryTree.tsx:49,164,209,217`, `DuplicateGroupCard.tsx:51` — boundary-less `startsWith` (display-level).
- `export/models.rs:242` `sanitize_display_folder_name` — same trailing-dot/reserved-name gaps as the (now-fixed) shared sanitizer; export folders were NOT covered by Task 3.
- `PathPolicy::check` case-insensitivity on Windows (dormant: only the web `AllowedRoots` transport, and Docker is Linux).
- Byte-exact path identity as a design question (`files.path UNIQUE` under BINARY collation vs case-insensitive FS; drive-letter case, 8.3 names) — needs a normalization decision, not a spot fix.
- Monitor-vs-registration ingest race guard (pause library-root monitor during a build).
- One-time repair migration for existing dotted MASTER rows on Windows (left as visible missing-file rows by design).
- Re-calibration overwrite vs an OPEN destination file on Windows (audit F7): `fits_writer/writer.rs:73` `rename` fails with a sharing violation when a viewer/AV holds the output — add a bounded retry (e.g. 3×100 ms on `PermissionDenied`) or a per-frame "file in use" error. Speculative severity (no log evidence yet), hence backlog.
