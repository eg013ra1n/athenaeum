# Folders Screen Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the File Manager's five stacked folder sections with a single master–detail "Folders" workspace (rail + inspector) per the approved spec `docs/superpowers/specs/2026-07-29-folders-screen-redesign-design.md`.

**Architecture:** Three small backend additions in `athenaeum-core` (`validate_folder_candidate` dry-run, one-step `switch_calibration_library_dir`, `get_folder_overview` stats), each with Tauri + Axum wrappers in the same task. Frontend: a new `src/components/folders/` module (rail, four inspector states, teaching Add dialog, shared role metadata) assembled by `FoldersTab.tsx`, which replaces the `directories` tab body in `FileManager.tsx`; the three old section components are deleted.

**Tech Stack:** Rust (rusqlite, ts-rs, tracing), Tauri 2 / Axum, React + TS + Tailwind design tokens, lucide-react.

## Global Constraints

- **Two backends in sync:** every new command gets its Tauri wrapper (`crates/athenaeum-tauri/src/commands/scan_roots.rs`) AND Axum route (`crates/athenaeum-web/src/routes/scan_roots.rs`) in the same task.
- Command boundary: `#[tracing::instrument(skip_all, err)]` on Tauri commands, `err(Debug)` on web routes. Never swallow errors — log before returning.
- `anyhow::Result`/`ApiError` in core; `.map_err(|e| e.to_string())` at the Tauri boundary; `api_err` at the web boundary.
- Frontend: backend access ONLY via the `api` object; icons ONLY from `lucide-react`; colors ONLY via design tokens (`text-purple`, `text-accent`, `text-info`, `text-success`, `text-warning`, `text-error`, `bg-surface*`, `text-content*`, `border-border`); timestamps via `formatTimestamp` from `src/utils/dateFormatting.ts`; notifications via `notify()` from `useNotifications()`.
- Generated TS: new `ts_rs::TS` types are registered in `crates/athenaeum-core/src/ts_export.rs` `generated_files()` → regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`. Generated fields stay snake_case (matches `ScanRoot`).
- Gates per repo norms: `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. clippy is NOT a gate. Format touched Rust files with `rustfmt <files>` (not `cargo fmt -p`).
- Commit as the repo user (git config already set); UI copy in English. Work on branch `0.5.1`.
- UI copy comes verbatim from the spec §4–6 (labels, switch descriptions, rule texts).

---

### Task 1: Backend — `validate_folder_candidate` dry-run

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs` (append after `check_special_root_uniqueness`, ~line 131; tests in the file's `#[cfg(test)] mod tests` — create the module at end of file if absent)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (add to the `models.ts` decls list after `crate::models::ScanRoot`)
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs`, `crates/athenaeum-tauri/src/lib.rs` (invoke_handler)
- Modify: `crates/athenaeum-web/src/routes/scan_roots.rs`, `crates/athenaeum-web/src/routes/mod.rs`

**Interfaces:**
- Consumes: existing `validate_scan_root_kind`, `SPECIAL_ROOT_KINDS`, `resolve_calibration_library_dir`, `resolve_special_root_dir`, `canonical_or_raw`, `normalize_path`, `PathPolicy`.
- Produces: `pub struct FolderCandidateVerdict { ok: bool, reason: Option<String>, conflicting_path: Option<String>, placement: Option<String> }`; `pub fn validate_folder_candidate(ctx, kind: String, path: String, policy) -> Result<FolderCandidateVerdict, ApiError>`; command name `validate_folder_candidate` with args `{ kind, path }`. Reason values: `not_found | not_a_directory | already_monitored | inside_existing | contains_existing | role_taken`. Placement (calibration only): `covered | standalone`. Kind values accepted: `normal | calibration_library | sync_incoming | collaboration | archive`.

- [ ] **Step 1: Write failing tests** (in `api/scan_roots.rs` tests module):

```rust
#[cfg(test)]
mod candidate_tests {
    use super::*;
    use std::path::Path;

    fn conn() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&c).unwrap();
        c
    }

    #[test]
    fn normal_inside_existing_root_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, root.to_str().unwrap(), "normal").unwrap();
        let v = classify_folder_candidate(&c, "normal", &sub.canonicalize().unwrap()).unwrap();
        assert!(!v.ok);
        assert_eq!(v.reason.as_deref(), Some("inside_existing"));
        assert_eq!(v.conflicting_path.as_deref(), Some(root.to_str().unwrap()));
    }

    #[test]
    fn normal_containing_existing_root_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let root = parent.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, root.to_str().unwrap(), "normal").unwrap();
        let v = classify_folder_candidate(&c, "normal", &parent.canonicalize().unwrap()).unwrap();
        assert_eq!(v.reason.as_deref(), Some("contains_existing"));
    }

    #[test]
    fn normal_duplicate_is_already_monitored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, root.to_str().unwrap(), "normal").unwrap();
        let v = classify_folder_candidate(&c, "normal", &root.canonicalize().unwrap()).unwrap();
        assert_eq!(v.reason.as_deref(), Some("already_monitored"));
    }

    #[test]
    fn calibration_inside_existing_is_ok_covered() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let sub = root.join("masters");
        std::fs::create_dir_all(&sub).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, root.to_str().unwrap(), "normal").unwrap();
        let v = classify_folder_candidate(&c, "calibration_library", &sub.canonicalize().unwrap()).unwrap();
        assert!(v.ok);
        assert_eq!(v.placement.as_deref(), Some("covered"));
    }

    #[test]
    fn calibration_standalone_is_ok_standalone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("masters");
        std::fs::create_dir_all(&dir).unwrap();
        let v = classify_folder_candidate(&conn(), "calibration_library", &dir.canonicalize().unwrap()).unwrap();
        assert!(v.ok);
        assert_eq!(v.placement.as_deref(), Some("standalone"));
    }

    #[test]
    fn taken_role_is_role_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, a.to_str().unwrap(), "sync_incoming").unwrap();
        let v = classify_folder_candidate(&c, "sync_incoming", &b.canonicalize().unwrap()).unwrap();
        assert_eq!(v.reason.as_deref(), Some("role_taken"));
        assert_eq!(v.conflicting_path.as_deref(), Some(a.to_str().unwrap()));
    }

    #[test]
    fn archive_kind_skips_placement_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let sub = root.join("archive");
        std::fs::create_dir_all(&sub).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, root.to_str().unwrap(), "normal").unwrap();
        let v = classify_folder_candidate(&c, "archive", &sub.canonicalize().unwrap()).unwrap();
        assert!(v.ok);
    }
}
```

Note: tests insert canonicalized paths (`upsert_scan_root` with the raw tempdir path is fine — `canonical_or_raw` resolves at compare time; on macOS tempdirs canonicalize to `/private/…`, so insert `root.canonicalize()` paths to keep the `conflicting_path` assertions exact: use `root.canonicalize().unwrap().to_str().unwrap()` when upserting AND asserting).

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p athenaeum-core candidate_tests -- --nocapture`
Expected: compile error "cannot find function `classify_folder_candidate`".

- [ ] **Step 3: Implement** (append to `api/scan_roots.rs` after `check_special_root_uniqueness`):

```rust
/// Dry-run verdict for an Add Folder candidate (teaching dialog, spec §6).
/// `ok == false` carries a machine-readable `reason` the dialog maps to copy.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct FolderCandidateVerdict {
    pub ok: bool,
    /// `not_found` | `not_a_directory` | `already_monitored` |
    /// `inside_existing` | `contains_existing` | `role_taken`
    pub reason: Option<String>,
    /// Conflicting monitored path (`inside_existing`/`contains_existing`) or
    /// the current role path (`role_taken`).
    pub conflicting_path: Option<String>,
    /// Calibration-library only: `covered` (stored as a setting; the parent
    /// root provides scan coverage) or `standalone` (becomes its own root).
    pub placement: Option<String>,
}

fn verdict_fail(reason: &str, conflicting: Option<String>) -> FolderCandidateVerdict {
    FolderCandidateVerdict { ok: false, reason: Some(reason.to_string()), conflicting_path: conflicting, placement: None }
}

fn verdict_ok(placement: Option<&str>) -> FolderCandidateVerdict {
    FolderCandidateVerdict { ok: true, reason: None, conflicting_path: None, placement: placement.map(str::to_string) }
}

/// Connection-level classifier behind [`validate_folder_candidate`] —
/// mirrors `add_scan_root`'s overlap/uniqueness checks (and
/// `set_calibration_library_dir`'s covered-placement rule) WITHOUT writing.
/// `candidate` must already be canonicalized. `kind == "archive"` skips
/// placement checks entirely (archive destinations are never scanned).
pub(crate) fn classify_folder_candidate(
    conn: &rusqlite::Connection,
    kind: &str,
    candidate: &Path,
) -> Result<FolderCandidateVerdict, ApiError> {
    if kind == "archive" {
        return Ok(verdict_ok(None));
    }
    validate_scan_root_kind(kind)?;

    // Role already assigned? (calibration resolves settings-key-aware)
    if kind == "calibration_library" {
        if let Some(dir) = resolve_calibration_library_dir(conn)? {
            return Ok(verdict_fail("role_taken", Some(dir)));
        }
    } else if SPECIAL_ROOT_KINDS.contains(&kind) {
        if let Some(dir) = resolve_special_root_dir(conn, kind)? {
            return Ok(verdict_fail("role_taken", Some(dir)));
        }
    }

    let is_calibration = kind == "calibration_library";
    for root in crate::db::get_scan_roots(conn)?.iter() {
        let existing = canonical_or_raw(&root.path);
        if candidate == existing {
            return Ok(if is_calibration {
                verdict_ok(Some("covered"))
            } else {
                verdict_fail("already_monitored", Some(root.path.clone()))
            });
        }
        if candidate.starts_with(&existing) {
            return Ok(if is_calibration {
                verdict_ok(Some("covered"))
            } else {
                verdict_fail("inside_existing", Some(root.path.clone()))
            });
        }
        if existing.starts_with(candidate) {
            return Ok(verdict_fail("contains_existing", Some(root.path.clone())));
        }
    }
    Ok(verdict_ok(is_calibration.then_some("standalone")))
}

/// Dry-run validation for the Add Folder dialog. Never writes; the actual
/// add/set command remains authoritative (a TOCTOU between validate and add
/// is acceptable — the add's own error still surfaces).
pub fn validate_folder_candidate(
    ctx: &ServiceContext,
    kind: String,
    path: String,
    policy: &PathPolicy,
) -> Result<FolderCandidateVerdict, ApiError> {
    let path_buf = Path::new(&path);
    if !path_buf.exists() {
        return Ok(verdict_fail("not_found", None));
    }
    if !path_buf.is_dir() {
        return Ok(verdict_fail("not_a_directory", None));
    }
    let canon = normalize_path(
        &path_buf
            .canonicalize()
            .map_err(|e| ApiError::Internal(format!("Failed to resolve path: {}", e)))?,
    );
    policy.check(&canon)?;
    let db = db(ctx)?;
    let conn = db.conn();
    classify_folder_candidate(&conn, &kind, &canon)
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p athenaeum-core candidate_tests`
Expected: 7 passed.

- [ ] **Step 5: Register the TS type.** In `ts_export.rs`, inside the `models.ts` `decls![…]` list, add after `crate::models::ScanRoot,`:

```rust
            crate::api::scan_roots::FolderCandidateVerdict,
```

Regenerate: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` → then `cargo test -p athenaeum-core --test ts_contract` must pass; `git diff src/types/models.ts` shows the new `export type FolderCandidateVerdict = { ok: boolean, reason: string | null, conflicting_path: string | null, placement: string | null, };`.

- [ ] **Step 6: Tauri wrapper.** In `crates/athenaeum-tauri/src/commands/scan_roots.rs` (after `clear_collaboration_dir`):

```rust
pub use athenaeum_core::api::scan_roots::FolderCandidateVerdict;

/// Dry-run placement validation for the Add Folder dialog (spec §8.2).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn validate_folder_candidate(
    kind: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<FolderCandidateVerdict, String> {
    api::validate_folder_candidate(&state.ctx, kind, path, &PathPolicy::AllowAll).map_err(|e| e.to_string())
}
```

Register in `crates/athenaeum-tauri/src/lib.rs` `invoke_handler![…]` next to the other `commands::scan_roots::…` entries: `commands::scan_roots::validate_folder_candidate,`.

- [ ] **Step 7: Web mirror.** In `crates/athenaeum-web/src/routes/scan_roots.rs`:

```rust
#[derive(serde::Deserialize)]
pub struct ValidateFolderCandidateArgs {
    pub kind: String,
    pub path: String,
}

/// POST /api/validate_folder_candidate
#[tracing::instrument(skip_all, err(Debug))]
pub async fn validate_folder_candidate(
    State(state): State<WebAppState>,
    Json(args): Json<ValidateFolderCandidateArgs>,
) -> Result<Json<athenaeum_core::api::scan_roots::FolderCandidateVerdict>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::validate_folder_candidate(&state.ctx, args.kind, args.path, &policy)
        .map(Json)
        .map_err(api_err)
}
```

Register in `routes/mod.rs` next to the scan_roots block: `.route("/api/validate_folder_candidate", post(scan_roots::validate_folder_candidate))`.

- [ ] **Step 8: Build both backends + commit**

Run: `cargo build -p athenaeum-tauri -p athenaeum-web` → success. Then:

```bash
rustfmt crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-tauri/src/commands/scan_roots.rs crates/athenaeum-web/src/routes/scan_roots.rs
git add -A && git commit -m "feat(folders): validate_folder_candidate dry-run command (core + both backends)"
```

---

### Task 2: Backend — one-step `switch_calibration_library_dir`

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs` (after `clear_calibration_library_dir`, ~line 405)
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs`, `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/scan_roots.rs`, `crates/athenaeum-web/src/routes/mod.rs`

**Interfaces:**
- Consumes: `validate_library_dir_candidate`, `normalize_path`, `canonical_or_raw`, `set_calibration_library_dir`, `crate::db::delete_scan_root`.
- Produces: `pub fn switch_calibration_library_dir(ctx, path: String, policy) -> Result<String, ApiError>`; command `switch_calibration_library_dir` args `{ path }`, returns the normalized effective path. Semantics (spec §8.1): deletes the old dedicated `calibration_library` root (catalog purge, bypassing the deletion guard as an internal step), then delegates to `set_calibration_library_dir`.

- [ ] **Step 1: Write failing tests** (same file, new module):

```rust
#[cfg(test)]
mod switch_library_tests {
    use super::*;

    fn ctx(tmp: &tempfile::TempDir) -> ServiceContext {
        ServiceContext::new_for_tests(tmp.path().join("catalog.db"))
    }

    fn mkdirs(tmp: &tempfile::TempDir, name: &str) -> String {
        let p = tmp.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap().to_string_lossy().to_string()
    }

    #[test]
    fn standalone_to_standalone_replaces_root_and_purges_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let old = mkdirs(&tmp, "lib_old");
        let new = mkdirs(&tmp, "lib_new");
        set_calibration_library_dir(&ctx, old.clone(), &PathPolicy::AllowAll).unwrap();
        // A cataloged file under the old library — must be purged with the root.
        {
            let db = ctx.db.get().unwrap();
            db.conn().execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'm.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![format!("{old}/m.fits")],
            ).unwrap();
        }
        let effective = switch_calibration_library_dir(&ctx, new.clone(), &PathPolicy::AllowAll).unwrap();
        assert_eq!(effective, new);
        let roots = get_scan_roots(&ctx).unwrap();
        let libs: Vec<_> = roots.iter().filter(|r| r.kind == "calibration_library").collect();
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0].path, new);
        assert_eq!(get_calibration_library_dir(&ctx).unwrap().as_deref(), Some(new.as_str()));
        let db = ctx.db.get().unwrap();
        let n: i64 = db.conn().query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "old library's catalog rows must be purged");
    }

    #[test]
    fn standalone_to_covered_removes_old_root_and_keeps_setting_only() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let monitored = mkdirs(&tmp, "astro");
        let covered = mkdirs(&tmp, "astro/masters");
        let old = mkdirs(&tmp, "lib_old");
        add_scan_root(&ctx, monitored.clone(), &PathPolicy::AllowAll, None).unwrap();
        set_calibration_library_dir(&ctx, old, &PathPolicy::AllowAll).unwrap();
        switch_calibration_library_dir(&ctx, covered.clone(), &PathPolicy::AllowAll).unwrap();
        let roots = get_scan_roots(&ctx).unwrap();
        assert!(roots.iter().all(|r| r.kind != "calibration_library"), "no dedicated root for a covered library");
        assert_eq!(get_calibration_library_dir(&ctx).unwrap().as_deref(), Some(covered.as_str()));
    }

    #[test]
    fn switch_with_no_previous_library_behaves_like_set() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let new = mkdirs(&tmp, "lib");
        let effective = switch_calibration_library_dir(&ctx, new.clone(), &PathPolicy::AllowAll).unwrap();
        assert_eq!(effective, new);
        assert_eq!(get_calibration_library_dir(&ctx).unwrap().as_deref(), Some(new.as_str()));
    }

    #[test]
    fn repicking_the_same_folder_is_a_noop_keep() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let lib = mkdirs(&tmp, "lib");
        set_calibration_library_dir(&ctx, lib.clone(), &PathPolicy::AllowAll).unwrap();
        switch_calibration_library_dir(&ctx, lib.clone(), &PathPolicy::AllowAll).unwrap();
        let roots = get_scan_roots(&ctx).unwrap();
        assert_eq!(roots.iter().filter(|r| r.kind == "calibration_library").count(), 1);
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p athenaeum-core switch_library_tests`
Expected: compile error "cannot find function `switch_calibration_library_dir`".

- [ ] **Step 3: Implement** (after `clear_calibration_library_dir`):

```rust
/// One-step calibration-library move (spec §8.1): removes the old dedicated
/// `calibration_library` root (catalog purge — same semantics as deleting it
/// from the folder list; files on disk untouched), then delegates to
/// [`set_calibration_library_dir`] for the covered/standalone placement of
/// the new folder. Replaces the old clear → remove-root → set dance.
///
/// Deliberately bypasses `guard_against_special_root_deletion`: the guard
/// exists to stop an operator removing the library out from under the role;
/// here removing it IS the requested operation. Not atomic across the two
/// phases — if the final set fails, the old root is already gone and no
/// library is configured; the UI confirmation warns about the removal, and
/// the error from the set phase surfaces verbatim.
pub fn switch_calibration_library_dir(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
) -> Result<String, ApiError> {
    let path_buf = Path::new(&path);
    validate_library_dir_candidate(path_buf)?;
    let new_path = normalize_path(
        &path_buf
            .canonicalize()
            .map_err(|e| ApiError::Internal(format!("Failed to resolve path: {}", e)))?,
    );
    policy.check(&new_path)?;

    let old_root = get_calibration_library_root(ctx)?;
    if let Some(old) = old_root {
        if canonical_or_raw(&old.path) != new_path {
            let id = old
                .id
                .ok_or_else(|| ApiError::Internal("calibration library root has no id".to_string()))?;
            tracing::info!(old = %old.path, new = %new_path.display(), "switching calibration library — removing old dedicated root");
            let db = db(ctx)?;
            let conn = db.conn();
            crate::db::delete_scan_root(&conn, id).map_err(|e| {
                tracing::error!(root_id = id, error = %e, "failed to remove old calibration library root");
                ApiError::Internal(format!("Failed to remove old calibration library root: {e}"))
            })?;
        }
    }

    set_calibration_library_dir(ctx, new_path.to_string_lossy().to_string(), policy)
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p athenaeum-core switch_library_tests`
Expected: 4 passed. (If `repicking_the_same_folder` fails on the uniqueness Conflict inside `set_calibration_library_dir`: that setter's standalone branch only runs when the folder is NOT covered; a re-picked identical folder hits `check_special_root_uniqueness`. Fix inside `switch_…` by skipping the delegate's root-add via early return: when `old` exists and equals `new_path`, just re-persist the settings key and return — mirror the last 6 lines of `set_calibration_library_dir`.)

- [ ] **Step 5: Tauri wrapper + registration** (same pattern as Task 1 Step 6):

```rust
/// One-step calibration-library move: removes the old dedicated root
/// (catalog purge; files untouched) and designates the new folder.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn switch_calibration_library_dir(path: String, state: State<'_, AppState>) -> Result<String, String> {
    api::switch_calibration_library_dir(&state.ctx, path, &PathPolicy::AllowAll).map_err(|e| e.to_string())
}
```

Register `commands::scan_roots::switch_calibration_library_dir,` in `lib.rs`.

- [ ] **Step 6: Web mirror + registration:**

```rust
/// POST /api/switch_calibration_library_dir
#[tracing::instrument(skip_all, err(Debug))]
pub async fn switch_calibration_library_dir(
    State(state): State<WebAppState>,
    Json(args): Json<SetCalibrationLibraryDirArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::switch_calibration_library_dir(&state.ctx, args.path, &policy)
        .map(Json)
        .map_err(api_err)
}
```

Route: `.route("/api/switch_calibration_library_dir", post(scan_roots::switch_calibration_library_dir))`.

- [ ] **Step 7: Build + commit**

```bash
cargo build -p athenaeum-tauri -p athenaeum-web
rustfmt crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-tauri/src/commands/scan_roots.rs crates/athenaeum-web/src/routes/scan_roots.rs
git add -A && git commit -m "feat(folders): one-step switch_calibration_library_dir (core + both backends)"
```

---

### Task 3: Backend — `get_folder_overview`

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs` (append near the end, before tests)
- Modify: `crates/athenaeum-core/src/ts_export.rs`
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs`, `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/scan_roots.rs`, `crates/athenaeum-web/src/routes/mod.rs`

**Interfaces:**
- Produces (all `#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]`):
  - `pub struct ScanRootOverview { pub root_id: i64, pub file_count: i64, pub total_bytes: i64 }`
  - `pub struct ArchiveRootOverview { pub archive_root_id: i64, pub path: String, pub set_count: i64, pub total_zip_bytes: i64 }`
  - `pub struct FolderOverview { pub scan_roots: Vec<ScanRootOverview>, pub archive_roots: Vec<ArchiveRootOverview> }`
  - `pub fn get_folder_overview(ctx) -> Result<FolderOverview, ApiError>`; command `get_folder_overview`, no args.

- [ ] **Step 1: Write failing test:**

```rust
#[cfg(test)]
mod overview_tests {
    use super::*;

    #[test]
    fn overview_counts_files_and_bytes_per_root() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let root = tmp.path().join("astro");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap().to_string_lossy().to_string();
        let added = add_scan_root(&ctx, root.clone(), &PathPolicy::AllowAll, None).unwrap();
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            for (name, size) in [("a.fits", 100_i64), ("b.fits", 50)] {
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', 'FITS')",
                    rusqlite::params![format!("{root}/{name}"), name, size],
                ).unwrap();
            }
            // Boundary trap: a sibling whose path shares the prefix without the separator.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'x.fits', 999, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![format!("{root}2/x.fits")],
            ).unwrap();
        }
        let ov = get_folder_overview(&ctx).unwrap();
        let s = ov.scan_roots.iter().find(|s| s.root_id == added.id.unwrap()).unwrap();
        assert_eq!(s.file_count, 2);
        assert_eq!(s.total_bytes, 150);
        assert!(ov.archive_roots.is_empty());
    }
}
```

- [ ] **Step 2: Run, verify failure** — `cargo test -p athenaeum-core overview_tests` → compile error.

- [ ] **Step 3: Implement:**

```rust
/// Per-folder stats for the Folders rail/inspector (spec §8.3) — one call,
/// no N+1 from the frontend.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct ScanRootOverview {
    pub root_id: i64,
    pub file_count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct ArchiveRootOverview {
    pub archive_root_id: i64,
    pub path: String,
    pub set_count: i64,
    /// Sum of on-disk sizes of the distinct zips recorded for operations
    /// under this root; missing zips contribute 0 (mirrors `list_archive_zips`).
    pub total_zip_bytes: i64,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct FolderOverview {
    pub scan_roots: Vec<ScanRootOverview>,
    pub archive_roots: Vec<ArchiveRootOverview>,
}

pub fn get_folder_overview(ctx: &ServiceContext) -> Result<FolderOverview, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let mut scan_roots = Vec::new();
    for root in crate::db::get_scan_roots(&conn)? {
        let Some(root_id) = root.id else { continue };
        // Separator-safe prefix match (`/data/Set1` must not capture `/data/Set10`).
        let (file_count, total_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files WHERE path LIKE ?1 || '/%' OR path LIKE ?1 || '\\%'",
                rusqlite::params![root.path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| ApiError::Internal(format!("overview query failed: {e}")))?;
        scan_roots.push(ScanRootOverview { root_id, file_count, total_bytes });
    }

    let mut archive_roots = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id, path FROM archive_roots ORDER BY id")
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let roots: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    for (id, path) in roots {
        let set_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM frames_set fs
                 JOIN archive_operations op ON fs.archive_operation_id = op.id
                 WHERE fs.archived_at IS NOT NULL AND op.archive_root_path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            )
            .map_err(|e| ApiError::Internal(format!("archive set count failed: {e}")))?;
        let mut zstmt = conn
            .prepare(
                "SELECT DISTINCT aof.target_zip_path FROM archive_operation_files aof
                 JOIN archive_operations op ON aof.operation_id = op.id
                 WHERE op.archive_root_path = ?1",
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let zips: Vec<String> = zstmt
            .query_map(rusqlite::params![path], |row| row.get(0))
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let total_zip_bytes = zips
            .iter()
            .map(|z| std::fs::metadata(z).map(|m| m.len() as i64).unwrap_or(0))
            .sum();
        archive_roots.push(ArchiveRootOverview { archive_root_id: id, path, set_count, total_zip_bytes });
    }

    Ok(FolderOverview { scan_roots, archive_roots })
}
```

(If the `frames_set`/`archive_operations` column names differ, crib the exact join from `list_archived_frame_sets` in `crates/athenaeum-tauri/src/commands/archive.rs:~460` — it selects `fs.archived_at`, `fs.archive_operation_id`, `op.archive_root_path`.)

- [ ] **Step 4: Run tests** — `cargo test -p athenaeum-core overview_tests` → 1 passed.

- [ ] **Step 5: ts_export + regenerate.** Add to the `models.ts` decls after `FolderCandidateVerdict`:

```rust
            crate::api::scan_roots::ScanRootOverview,
            crate::api::scan_roots::ArchiveRootOverview,
            crate::api::scan_roots::FolderOverview,
```

`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` then plain run passes.

- [ ] **Step 6: Wrappers.** Tauri:

```rust
pub use athenaeum_core::api::scan_roots::FolderOverview;

/// Per-folder stats (file counts/bytes, archive set counts/zip bytes) for
/// the Folders tab — one call for the whole rail.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_folder_overview(state: State<'_, AppState>) -> Result<FolderOverview, String> {
    api::get_folder_overview(&state.ctx).map_err(|e| e.to_string())
}
```

Register in `lib.rs`. Web:

```rust
/// POST /api/get_folder_overview
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_folder_overview(
    State(state): State<WebAppState>,
) -> Result<Json<athenaeum_core::api::scan_roots::FolderOverview>, (StatusCode, String)> {
    api::get_folder_overview(&state.ctx).map(Json).map_err(api_err)
}
```

Route: `.route("/api/get_folder_overview", post(scan_roots::get_folder_overview))`.

- [ ] **Step 7: Build + commit**

```bash
cargo build -p athenaeum-tauri -p athenaeum-web && cargo test -p athenaeum-core
rustfmt crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-tauri/src/commands/scan_roots.rs crates/athenaeum-web/src/routes/scan_roots.rs
git add -A && git commit -m "feat(folders): get_folder_overview stats command (core + both backends)"
```

---

### Task 4: Frontend foundation — `roleMeta.ts`, `format.ts`, `SwitchRow.tsx`

**Files:**
- Create: `src/components/folders/roleMeta.ts`
- Create: `src/components/folders/format.ts`
- Create: `src/components/folders/SwitchRow.tsx`

**Interfaces:**
- Produces: `RoleKind`, `AddableKind`, `RailSelection`, `ROLE_META`, `ROLE_ORDER`, `KIND_META` (icon/tint/label for `normal` + `archive`), `verdictMessage()`, `formatBytes()`, `basename()`, `parentPath()`, `<SwitchRow/>`. All later tasks import from these.

- [ ] **Step 1: Write `src/components/folders/format.ts`:**

```ts
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(2)} GB`;
}

export function basename(p: string): string {
  const norm = p.replace(/[\\/]+$/, '');
  const idx = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

export function parentPath(p: string): string {
  const norm = p.replace(/[\\/]+$/, '');
  const idx = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  return idx > 0 ? norm.slice(0, idx) : norm;
}
```

- [ ] **Step 2: Write `src/components/folders/roleMeta.ts`** (single source of truth — spec §9):

```ts
import { Folder, Library, Inbox, Users, Archive, type LucideIcon } from 'lucide-react';

export type RoleKind = 'calibration_library' | 'sync_incoming' | 'collaboration';
export type AddableKind = 'normal' | 'archive' | RoleKind;

export type RailSelection =
  | { type: 'scan'; id: number }
  | { type: 'archive'; id: number }
  | { type: 'placeholder'; kind: RoleKind };

export interface RoleMeta {
  kind: RoleKind;
  label: string;
  icon: LucideIcon;
  /** Icon tint (design token text class) — role-colored lucide, decision D5. */
  tint: string;
  /** Badge chip classes (bg/text/border tokens). */
  chip: string;
  /** One-liner for placeholder rows and the Add dialog. */
  purpose: string;
  /** Placement rule shown in the Add dialog BEFORE picking (spec §6). */
  placementRule: string;
  /** Inspector explainer card text (spec §5.2). */
  explainer: string;
  /** Switch visibility matrix (spec §5.2). */
  switches: { watch: boolean; duplicates: boolean; uniqueCamera: boolean };
  getCommand: string;
  setCommand: string;
  clearCommand: string;
}

export const ROLE_ORDER: RoleKind[] = ['calibration_library', 'sync_incoming', 'collaboration'];

export const ROLE_META: Record<RoleKind, RoleMeta> = {
  calibration_library: {
    kind: 'calibration_library',
    label: 'Calibration Library',
    icon: Library,
    tint: 'text-purple',
    chip: 'bg-purple/20 text-purple border border-purple/40',
    purpose: 'Master calibration frames live here.',
    placementRule:
      'May be inside a monitored folder, or standalone — a standalone folder is also scanned, so masters you drop in by hand are imported.',
    explainer:
      'Master calibration frames built by Athenaeum are written here, and masters you drop in by hand are imported on scan.',
    switches: { watch: true, duplicates: false, uniqueCamera: false },
    getCommand: 'get_calibration_library_dir',
    setCommand: 'set_calibration_library_dir',
    clearCommand: 'clear_calibration_library_dir',
  },
  sync_incoming: {
    kind: 'sync_incoming',
    label: 'Sync Incoming',
    icon: Inbox,
    tint: 'text-accent',
    chip: 'bg-accent/20 text-accent border border-accent/40',
    purpose: 'Files received from your capture devices land here.',
    placementRule: 'Must be its own folder, outside every monitored folder.',
    explainer: 'Transfers from your paired capture devices land here and are cataloged on scan.',
    switches: { watch: true, duplicates: true, uniqueCamera: false },
    getCommand: 'get_sync_incoming_dir',
    setCommand: 'set_sync_incoming_dir',
    clearCommand: 'clear_sync_incoming_dir',
  },
  collaboration: {
    kind: 'collaboration',
    label: 'Collaboration',
    icon: Users,
    tint: 'text-success',
    chip: 'bg-success/20 text-success border border-success/40',
    purpose: 'Received project contributions are stored here.',
    placementRule: 'Must be its own folder, outside every monitored folder.',
    explainer: 'Contributions received for collaboration projects are stored here and cataloged on scan.',
    switches: { watch: true, duplicates: true, uniqueCamera: false },
    getCommand: 'get_collaboration_dir',
    setCommand: 'set_collaboration_dir',
    clearCommand: 'clear_collaboration_dir',
  },
};

export const KIND_META = {
  normal: {
    label: 'Monitored folder',
    icon: Folder,
    tint: 'text-info',
    purpose: 'Watch a folder of FITS/XISF files and catalog everything in it.',
    placementRule: "A monitored folder can't sit inside another monitored folder — pick a separate directory.",
  },
  archive: {
    label: 'Archive destination',
    icon: Archive,
    tint: 'text-warning',
    purpose: 'Where "Move and ZIP" stores finished sets. Not scanned.',
    placementRule: 'Never scanned — it may live anywhere, even inside a monitored folder.',
  },
} as const;

export function metaForKind(kind: AddableKind) {
  if (kind === 'normal' || kind === 'archive') return KIND_META[kind];
  return ROLE_META[kind];
}

/** Map a FolderCandidateVerdict to dialog copy (spec §6). */
export function verdictMessage(reason: string | null, conflictingPath: string | null): string {
  switch (reason) {
    case 'not_found':
      return 'This directory does not exist.';
    case 'not_a_directory':
      return 'This path is not a directory.';
    case 'already_monitored':
      return 'This folder is already monitored.';
    case 'inside_existing':
      return `This folder is inside «${conflictingPath ?? 'a monitored folder'}», which is already monitored. Choose a folder outside it.`;
    case 'contains_existing':
      return `This folder contains the monitored folder «${conflictingPath ?? ''}». Choose a folder that does not wrap an existing one.`;
    case 'role_taken':
      return `This role is already assigned to «${conflictingPath ?? ''}». Release it first, or use Change folder on that row.`;
    default:
      return 'This folder cannot be used here.';
  }
}
```

- [ ] **Step 3: Write `src/components/folders/SwitchRow.tsx`** (described toggle — spec §5.1):

```tsx
interface SwitchRowProps {
  title: string;
  description: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}

export function SwitchRow({ title, description, checked, disabled, onChange }: SwitchRowProps) {
  return (
    <label className={`flex items-start gap-3 px-2 py-2 rounded-lg transition hover:bg-surface-hover/40 ${disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}>
      <input
        type="checkbox"
        role="switch"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-1 w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
      />
      <span className="flex-1 min-w-0">
        <span className="block text-sm font-medium text-content">{title}</span>
        <span className="block text-xs text-content-muted leading-relaxed">{description}</span>
      </span>
    </label>
  );
}
```

- [ ] **Step 4: Typecheck + commit**

Run: `npx tsc --noEmit` → clean.

```bash
git add src/components/folders && git commit -m "feat(folders): role metadata, formatting helpers, SwitchRow"
```

---

### Task 5: Frontend — `FolderRail.tsx`

**Files:**
- Create: `src/components/folders/FolderRail.tsx`

**Interfaces:**
- Consumes: `roleMeta.ts` (`ROLE_META`, `ROLE_ORDER`, `KIND_META`, `RailSelection`, `RoleKind`, `AddableKind`), `format.ts`, types `ScanRootWithAvailability`/`ArchiveRoot`/`ArchivedFrameSetSummary` from `../../types/helpers`, `FolderOverview` from `../../types/models`.
- Produces: `<FolderRail/>` with props below — Task 9 renders it.

- [ ] **Step 1: Write the component:**

```tsx
import { Plus, RefreshCw, Star } from 'lucide-react';
import type { ScanRootWithAvailability, ArchiveRoot, ArchivedFrameSetSummary } from '../../types/helpers';
import type { FolderOverview } from '../../types/models';
import { ROLE_META, ROLE_ORDER, KIND_META, type RailSelection, type RoleKind, type AddableKind } from './roleMeta';
import { basename, parentPath, formatBytes } from './format';

interface FolderRailProps {
  scanRoots: ScanRootWithAvailability[];
  archiveRoots: ArchiveRoot[];
  archivedSets: ArchivedFrameSetSummary[];
  overview: FolderOverview | null;
  missingCounts: Record<number, number>;
  selection: RailSelection | null;
  onSelect: (sel: RailSelection) => void;
  onAdd: (preselect?: AddableKind) => void;
  onRescan: (rootId: number) => void;
  isScanning: (rootId: number) => boolean;
  scanPercent: (rootId: number) => number | null;
}

const isSel = (sel: RailSelection | null, other: RailSelection) =>
  !!sel && sel.type === other.type &&
  (sel.type === 'placeholder' ? sel.kind === (other as { kind: RoleKind }).kind : sel.id === (other as { id: number }).id);

function GroupHeader({ label }: { label: string }) {
  return <div className="px-2 mt-4 mb-1 first:mt-0 text-[10px] font-bold uppercase tracking-wider text-content-muted">{label}</div>;
}

function ScanRow({ root, sub, tint, Icon, selected, onClick, onRescan, scanning, percent, missing }: {
  root: ScanRootWithAvailability; sub: string; tint: string;
  Icon: React.ComponentType<{ size?: number; className?: string }>;
  selected: boolean; onClick: () => void; onRescan: () => void;
  scanning: boolean; percent: number | null; missing: number;
}) {
  const offline = !root.is_available;
  return (
    <div
      onClick={onClick}
      className={`flex items-center gap-2 px-2 py-1.5 rounded-lg cursor-pointer transition ${selected ? 'bg-surface-hover shadow-[inset_2px_0_0] shadow-accent' : 'hover:bg-surface-hover/50'}`}
    >
      <Icon size={16} className={`${tint} shrink-0 ${offline ? 'opacity-50' : ''}`} />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 text-sm font-semibold text-content truncate">
          <span className="truncate">{basename(root.path)}</span>
          {missing > 0 && (
            <span className="shrink-0 px-1.5 rounded-full text-[10px] font-semibold bg-orange/20 text-orange border border-orange/40">{missing} missing</span>
          )}
          {offline && (
            <span className="shrink-0 px-1.5 rounded-full text-[10px] font-semibold bg-error-muted text-error border border-error/40">offline</span>
          )}
        </div>
        <div className="text-[11px] text-content-muted truncate">
          {scanning ? `scanning…${percent != null ? ` ${Math.round(percent)}%` : ''}` : sub}
        </div>
      </div>
      <button
        onClick={(e) => { e.stopPropagation(); if (!offline && !scanning) onRescan(); }}
        disabled={offline || scanning}
        title={offline ? 'Folder is offline' : 'Rescan this folder'}
        className={`p-1 rounded shrink-0 transition ${offline ? 'opacity-30 cursor-not-allowed text-content-muted' : 'text-content-muted hover:text-accent hover:bg-surface-hover'}`}
      >
        <RefreshCw size={14} className={scanning ? 'animate-spin text-accent' : ''} />
      </button>
    </div>
  );
}

export function FolderRail({
  scanRoots, archiveRoots, archivedSets, overview, missingCounts,
  selection, onSelect, onAdd, onRescan, isScanning, scanPercent,
}: FolderRailProps) {
  const monitored = scanRoots
    .filter((r) => r.kind === 'normal')
    .sort((a, b) => basename(a.path).localeCompare(basename(b.path)));
  const roleRoots = new Map(scanRoots.filter((r) => r.kind !== 'normal').map((r) => [r.kind as RoleKind, r]));
  const sortedArchive = [...archiveRoots].sort((a, b) =>
    a.is_default === b.is_default ? basename(a.path).localeCompare(basename(b.path)) : a.is_default ? -1 : 1);

  const setCount = (root: ArchiveRoot) => archivedSets.filter((s) => (s.archive_root_path ?? '') === root.path).length;
  const archiveBytes = (root: ArchiveRoot) =>
    overview?.archive_roots.find((a) => a.archive_root_id === root.id)?.total_zip_bytes ?? 0;

  return (
    <div className="w-[300px] shrink-0 bg-surface-elevated rounded-lg p-3 overflow-y-auto">
      <button
        onClick={() => onAdd()}
        className="w-full flex items-center justify-center gap-2 px-3 py-2 mb-1 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg transition"
      >
        <Plus size={16} /> Add Folder
      </button>

      <GroupHeader label="Monitored" />
      {monitored.length === 0 && <p className="px-2 text-xs text-content-muted">No monitored folders yet.</p>}
      {monitored.map((root) => (
        <ScanRow
          key={root.id}
          root={root}
          sub={parentPath(root.path)}
          tint={KIND_META.normal.tint}
          Icon={KIND_META.normal.icon}
          selected={isSel(selection, { type: 'scan', id: root.id! })}
          onClick={() => onSelect({ type: 'scan', id: root.id! })}
          onRescan={() => onRescan(root.id!)}
          scanning={root.id ? isScanning(root.id) : false}
          percent={root.id ? scanPercent(root.id) : null}
          missing={root.id ? (missingCounts[root.id] ?? 0) : 0}
        />
      ))}

      <GroupHeader label="Special roles" />
      {ROLE_ORDER.map((kind) => {
        const meta = ROLE_META[kind];
        const root = roleRoots.get(kind);
        if (root) {
          return (
            <ScanRow
              key={kind}
              root={root}
              sub={`${meta.label} · ${parentPath(root.path)}`}
              tint={meta.tint}
              Icon={meta.icon}
              selected={isSel(selection, { type: 'scan', id: root.id! })}
              onClick={() => onSelect({ type: 'scan', id: root.id! })}
              onRescan={() => onRescan(root.id!)}
              scanning={root.id ? isScanning(root.id) : false}
              percent={root.id ? scanPercent(root.id) : null}
              missing={root.id ? (missingCounts[root.id] ?? 0) : 0}
            />
          );
        }
        const selected = isSel(selection, { type: 'placeholder', kind });
        return (
          <div
            key={kind}
            onClick={() => onSelect({ type: 'placeholder', kind })}
            className={`flex items-center gap-2 px-2 py-1.5 rounded-lg border border-dashed border-border cursor-pointer transition ${selected ? 'bg-surface-hover' : 'hover:bg-surface-hover/50'}`}
          >
            <meta.icon size={16} className={`${meta.tint} opacity-60 shrink-0`} />
            <div className="flex-1 min-w-0">
              <div className="text-sm text-content-muted truncate">{meta.label}</div>
              <div className="text-[11px] text-content-muted/70 truncate">{meta.purpose}</div>
            </div>
            <button
              onClick={(e) => { e.stopPropagation(); onAdd(kind); }}
              className="shrink-0 px-2 py-1 rounded bg-surface-hover text-xs text-accent hover:brightness-110 transition"
            >
              Set up…
            </button>
          </div>
        );
      })}

      <GroupHeader label="Archive destinations" />
      {sortedArchive.length === 0 && <p className="px-2 text-xs text-content-muted">No archive folders yet.</p>}
      {sortedArchive.map((root) => {
        const selected = isSel(selection, { type: 'archive', id: root.id });
        const bytes = archiveBytes(root);
        return (
          <div
            key={root.id}
            onClick={() => onSelect({ type: 'archive', id: root.id })}
            className={`flex items-center gap-2 px-2 py-1.5 rounded-lg cursor-pointer transition ${selected ? 'bg-surface-hover shadow-[inset_2px_0_0] shadow-accent' : 'hover:bg-surface-hover/50'}`}
          >
            <KIND_META.archive.icon size={16} className={`${KIND_META.archive.tint} shrink-0`} />
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-1.5 text-sm font-semibold text-content truncate">
                <span className="truncate">{basename(root.path)}</span>
                {root.is_default && <Star size={12} className="text-warning shrink-0" fill="currentColor" />}
              </div>
              <div className="text-[11px] text-content-muted truncate">
                {parentPath(root.path)} · {setCount(root)} sets{bytes > 0 ? ` · ${formatBytes(bytes)}` : ''}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 2: Typecheck + commit**

Run: `npx tsc --noEmit` → clean.

```bash
git add src/components/folders/FolderRail.tsx && git commit -m "feat(folders): rail component — groups, badges, inline rescan, role placeholders"
```

---

### Task 6: Frontend — `AddFolderDialog.tsx`

**Files:**
- Create: `src/components/folders/AddFolderDialog.tsx`

**Interfaces:**
- Consumes: `roleMeta.ts` (`metaForKind`, `ROLE_META`, `ROLE_ORDER`, `KIND_META`, `verdictMessage`, `AddableKind`, `RoleKind`), `api`, `pickDirectory`, `isTauri`, `FolderBrowserModal`, `addArchiveRoot` from `../../api/archive`, `FolderCandidateVerdict` from `../../types/models`, `notify`.
- Produces: `<AddFolderDialog isOpen preselect scanRoots onClose onAdded/>` — Task 9 renders it. `onAdded()` fires after a successful add/designate so the parent refreshes.

- [ ] **Step 1: Write the component:**

```tsx
import { useEffect, useState } from 'react';
import { X, Check, Info, AlertCircle, FolderOpen, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { addArchiveRoot } from '../../api/archive';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { useNotifications } from '../../contexts/NotificationContext';
import type { ScanRoot, FolderCandidateVerdict } from '../../types/models';
import { ROLE_META, ROLE_ORDER, KIND_META, metaForKind, verdictMessage, type AddableKind, type RoleKind } from './roleMeta';

interface AddFolderDialogProps {
  isOpen: boolean;
  /** Pre-select a type (e.g. a role's "Set up…" row) and jump to step 2. */
  preselect?: AddableKind;
  scanRoots: ScanRoot[];
  onClose: () => void;
  onAdded: () => void;
}

export function AddFolderDialog({ isOpen, preselect, scanRoots, onClose, onAdded }: AddFolderDialogProps) {
  const { notify } = useNotifications();
  const [kind, setKind] = useState<AddableKind | null>(null);
  const [pickedPath, setPickedPath] = useState<string | null>(null);
  const [verdict, setVerdict] = useState<FolderCandidateVerdict | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showBrowser, setShowBrowser] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setKind(preselect ?? null);
      setPickedPath(null);
      setVerdict(null);
      setError(null);
    }
  }, [isOpen, preselect]);

  if (!isOpen) return null;

  const takenRolePath = (role: RoleKind): string | undefined =>
    scanRoots.find((r) => r.kind === role)?.path;

  const validate = async (candidateKind: AddableKind, path: string) => {
    setError(null);
    setPickedPath(path);
    setVerdict(null);
    try {
      const v = await api.invoke<FolderCandidateVerdict>('validate_folder_candidate', { kind: candidateKind, path });
      setVerdict(v);
    } catch (e) {
      console.error('[AddFolderDialog] validate failed:', e);
      setError(typeof e === 'string' ? e : String(e));
    }
  };

  const pick = async () => {
    if (!kind) return;
    if (!isTauri) { setShowBrowser(true); return; }
    const picked = await pickDirectory();
    if (picked && typeof picked === 'string') await validate(kind, picked);
  };

  const confirmAdd = async () => {
    if (!kind || !pickedPath || !verdict?.ok) return;
    setBusy(true);
    setError(null);
    try {
      if (kind === 'normal') {
        await api.invoke('add_scan_root', { path: pickedPath });
      } else if (kind === 'archive') {
        await addArchiveRoot(pickedPath, null);
      } else {
        await api.invoke<string>(ROLE_META[kind].setCommand, { path: pickedPath });
      }
      onAdded();
      onClose();
    } catch (e) {
      // Backend stays authoritative — surface its message verbatim (TOCTOU
      // between validate and add is possible and must not be hidden).
      const msg = typeof e === 'string' ? e : String(e);
      console.error('[AddFolderDialog] add failed:', e);
      setError(msg);
      notify({ title: 'Could not add folder', detail: msg, kind: 'files', tone: 'warning', hasErrors: true });
    } finally {
      setBusy(false);
    }
  };

  const meta = kind ? metaForKind(kind) : null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={onClose}>
      <div className="w-[560px] max-w-[92vw] bg-surface-elevated border border-border rounded-xl p-5" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-lg font-semibold text-content">Add Folder</h3>
          <button onClick={onClose} className="p-1 rounded text-content-muted hover:text-content hover:bg-surface-hover transition"><X size={18} /></button>
        </div>

        {/* Step 1 — type picker */}
        {!kind && (
          <div className="space-y-1">
            {(['normal', 'archive'] as const).map((k) => (
              <button key={k} onClick={() => setKind(k)} className="w-full flex items-start gap-3 p-3 rounded-lg text-left hover:bg-surface-hover transition">
                <KindIcon k={k} />
                <span><span className="block text-sm font-semibold text-content">{KIND_META[k].label}</span>
                <span className="block text-xs text-content-muted">{KIND_META[k].purpose}</span></span>
              </button>
            ))}
            <div className="pt-2 pb-1 px-3 text-[10px] font-bold uppercase tracking-wider text-content-muted">Assign a role — one of each</div>
            {ROLE_ORDER.map((role) => {
              const taken = takenRolePath(role);
              const rm = ROLE_META[role];
              return (
                <button key={role} onClick={() => !taken && setKind(role)} disabled={!!taken}
                  className={`w-full flex items-start gap-3 p-3 rounded-lg text-left transition ${taken ? 'opacity-45 cursor-not-allowed' : 'hover:bg-surface-hover'}`}>
                  <rm.icon size={18} className={`${rm.tint} mt-0.5 shrink-0`} />
                  <span className="min-w-0">
                    <span className="block text-sm font-semibold text-content">{rm.label} {taken && <Check size={12} className="inline text-success" />}</span>
                    <span className="block text-xs text-content-muted truncate">{taken ? `already set → ${taken}` : rm.purpose}</span>
                  </span>
                </button>
              );
            })}
          </div>
        )}

        {/* Step 2 — rule, pick, inline validation */}
        {kind && meta && (
          <div className="space-y-3">
            <div className="flex items-center gap-2 text-sm font-semibold text-content">
              <KindIcon k={kind} /> {meta.label}
              <button onClick={() => { setKind(null); setPickedPath(null); setVerdict(null); setError(null); }} className="ml-auto text-xs text-accent hover:underline">change type</button>
            </div>
            <div className="flex items-start gap-2 p-3 rounded-lg bg-surface border border-accent/30 text-xs text-content-muted">
              <Info size={14} className="text-accent shrink-0 mt-0.5" />
              <span>{meta.placementRule}</span>
            </div>
            <button onClick={pick} disabled={busy} className="flex items-center gap-2 px-4 py-2 bg-surface-hover hover:brightness-110 rounded-lg text-sm text-content transition">
              <FolderOpen size={16} /> {pickedPath ? 'Pick a different folder…' : 'Choose folder…'}
            </button>
            {pickedPath && (
              <div className="p-3 rounded-lg bg-surface border border-border">
                <div className="font-mono text-xs text-content break-all">{pickedPath}</div>
                {verdict === null && !error && <div className="mt-1 text-xs text-content-muted flex items-center gap-1"><Loader2 size={12} className="animate-spin" /> checking…</div>}
                {verdict?.ok && (
                  <div className="mt-1 text-xs text-success flex items-center gap-1">
                    <Check size={12} />
                    {verdict.placement === 'covered'
                      ? 'Inside a monitored folder — stored as the library destination; the parent folder keeps scanning it.'
                      : verdict.placement === 'standalone'
                        ? 'Standalone — becomes its own scanned library folder.'
                        : 'Looks good.'}
                  </div>
                )}
                {verdict && !verdict.ok && (
                  <div className="mt-1 text-xs text-error flex items-start gap-1">
                    <AlertCircle size={12} className="shrink-0 mt-0.5" /> {verdictMessage(verdict.reason, verdict.conflicting_path)}
                  </div>
                )}
              </div>
            )}
            {error && <div className="text-xs text-error">{error}</div>}
            <div className="flex justify-end gap-2 pt-1">
              <button onClick={onClose} className="px-4 py-2 rounded-lg text-sm text-content-muted hover:bg-surface-hover transition">Cancel</button>
              <button onClick={confirmAdd} disabled={busy || !verdict?.ok}
                className="px-4 py-2 rounded-lg text-sm font-semibold bg-accent hover:bg-accent-hover text-surface transition disabled:opacity-50 disabled:cursor-not-allowed">
                {busy ? 'Adding…' : 'Add'}
              </button>
            </div>
          </div>
        )}

        <FolderBrowserModal
          isOpen={showBrowser}
          scope="scan"
          onSelect={(path) => { setShowBrowser(false); if (kind) void validate(kind, path); }}
          onClose={() => setShowBrowser(false)}
        />
      </div>
    </div>
  );
}

function KindIcon({ k }: { k: AddableKind }) {
  const m = metaForKind(k);
  const Icon = m.icon;
  return <Icon size={18} className={`${m.tint} mt-0.5 shrink-0`} />;
}
```

- [ ] **Step 2: Typecheck + commit**

Run: `npx tsc --noEmit` → clean.

```bash
git add src/components/folders/AddFolderDialog.tsx && git commit -m "feat(folders): teaching Add Folder dialog with inline dry-run validation"
```

---

### Task 7: Frontend — `MonitoredInspector.tsx` (incl. offline banner)

**Files:**
- Create: `src/components/folders/MonitoredInspector.tsx`

**Interfaces:**
- Consumes: `SwitchRow`, `format.ts`, `MissingFilesPanel` (`../MissingFilesPanel`), `formatTimestamp` (`../../utils/dateFormatting`), types `ScanRootWithAvailability`, `MissingFileRecord`, `ScanResult` from `../../types/helpers`, `RelinkResult`, `ScanRootOverview` from `../../types/models`, `api`, `revealItemInDir`, `isTauri`.
- Produces: `<MonitoredInspector/>` used by Task 9 with props exactly as defined below.

- [ ] **Step 1: Write the component:**

```tsx
import { useEffect, useState } from 'react';
import { RefreshCw, ExternalLink, AlertTriangle, AlertCircle, ChevronDown, ChevronRight, Loader2, CheckCircle2, Info } from 'lucide-react';
import { api } from '../../api';
import { revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { formatTimestamp } from '../../utils/dateFormatting';
import { MissingFilesPanel } from '../MissingFilesPanel';
import { SwitchRow } from './SwitchRow';
import { basename, formatBytes } from './format';
import type { ScanRootWithAvailability, MissingFileRecord, ScanResult } from '../../types/helpers';
import type { RelinkResult, ScanRootOverview } from '../../types/models';

interface MonitoredInspectorProps {
  root: ScanRootWithAvailability;
  overview: ScanRootOverview | undefined;
  missingCount: number;
  scanResult: ScanResult | null;
  isScanning: boolean;
  relinking: boolean;
  relinkResult: RelinkResult | null;
  onScan: () => void;
  onRelink: () => void;
  onShowScanDetails: () => void;
  onToggleDuplicates: (v: boolean) => void;
  onToggleUniqueCamera: (v: boolean) => void;
  onToggleMonitor: (v: boolean) => void;
  onRemove: () => void;
  onMissingChanged: () => void;
}

export function MonitoredInspector(props: MonitoredInspectorProps) {
  const { root, overview, missingCount, scanResult, isScanning, relinking, relinkResult } = props;
  const offline = !root.is_available;
  const [missingOpen, setMissingOpen] = useState(false);
  const [missingFiles, setMissingFiles] = useState<MissingFileRecord[] | null>(null);
  const [errorsOpen, setErrorsOpen] = useState(false);
  const displayErrors = scanResult?.errors ?? root.last_scan_errors ?? [];

  useEffect(() => { setMissingOpen(false); setMissingFiles(null); setErrorsOpen(false); }, [root.id]);

  const loadMissing = async () => {
    try {
      const files = await api.invoke<MissingFileRecord[]>('get_missing_files', { rootId: root.id });
      setMissingFiles(files);
    } catch (e) {
      console.error('[MonitoredInspector] get_missing_files failed:', e);
    }
  };

  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 overflow-y-auto">
      {/* Header */}
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-lg font-bold text-content">
            <span className="truncate">{basename(root.path)}</span>
            <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-surface-hover text-content-muted border border-border">Monitored</span>
          </div>
          <div className="flex items-center gap-2 font-mono text-xs text-content-muted">
            <span className="truncate">{root.path}</span>
            {isTauri && !offline && (
              <button onClick={() => revealItemInDir(root.path).catch((e) => console.error('reveal failed:', e))}
                title="Reveal in file manager" className="p-0.5 rounded hover:text-accent transition"><ExternalLink size={12} /></button>
            )}
          </div>
        </div>
        {!offline && (
          <div className="flex gap-2 shrink-0">
            <button onClick={props.onScan} disabled={isScanning}
              className="flex items-center gap-2 px-3 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg text-sm transition disabled:opacity-50">
              <RefreshCw size={14} className={isScanning ? 'animate-spin' : ''} /> {isScanning ? 'Scanning…' : 'Scan now'}
            </button>
            <button onClick={props.onRelink} disabled={relinking}
              className="px-3 py-2 bg-surface-hover hover:brightness-110 rounded-lg text-sm text-content transition disabled:opacity-50">
              {relinking ? 'Relinking…' : 'Relink…'}
            </button>
          </div>
        )}
      </div>

      {/* Offline banner (spec §5.4) */}
      {offline && (
        <div className="mt-4 p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
          <AlertTriangle className="text-error shrink-0 mt-0.5" size={18} />
          <div className="flex-1">
            <p className="text-sm font-semibold text-error">Folder not reachable</p>
            <p className="text-xs text-error/80 mt-0.5 mb-2">
              Drive unmounted, renamed or moved. The catalog still remembers all
              {overview ? ` ${overview.file_count.toLocaleString()}` : ''} files — Relink points them to the new location;
              frame sets, calibration links and tags survive.
            </p>
            <button onClick={props.onRelink} disabled={relinking}
              className="flex items-center gap-2 px-3 py-1.5 bg-error hover:brightness-90 text-white rounded text-sm transition disabled:opacity-50">
              <RefreshCw size={14} className={relinking ? 'animate-spin' : ''} /> {relinking ? 'Relinking…' : 'Relink — point to new location…'}
            </button>
          </div>
        </div>
      )}

      {/* Relink result */}
      {relinkResult && (
        <div className="mt-4 p-4 bg-surface rounded-lg border border-border">
          <h4 className="text-sm font-semibold text-content flex items-center gap-2 mb-2"><CheckCircle2 className="text-success" size={16} /> Relinking complete</h4>
          <div className="grid grid-cols-3 gap-4 text-sm">
            <div><p className="text-content-muted text-xs">Matched</p><p className="text-lg font-bold text-success">{relinkResult.files_matched}</p></div>
            <div><p className="text-content-muted text-xs">New files</p><p className="text-lg font-bold text-accent">{relinkResult.files_new}</p></div>
            <div><p className="text-content-muted text-xs">Orphaned</p><p className="text-lg font-bold text-warning">{relinkResult.files_orphaned}</p></div>
          </div>
        </div>
      )}

      {/* Stats */}
      <div className="flex flex-wrap gap-2 mt-4">
        <Stat label="files cataloged" value={overview ? overview.file_count.toLocaleString() : '—'} />
        <Stat label="on disk" value={overview ? formatBytes(overview.total_bytes) : '—'} />
        <Stat label="last scan" value={root.last_scan ? formatTimestamp(root.last_scan) : 'never'} />
        <Stat label="watching" value={root.monitor_enabled ? 'background interval' : 'manual only'} />
      </div>

      {/* Last scan result strip */}
      {scanResult && (
        <div className="mt-3 p-3 bg-success-muted border border-success/50 rounded-lg flex items-center justify-between text-sm">
          <span className="flex items-center gap-2 text-success font-semibold"><CheckCircle2 size={14} /> Scan complete — {scanResult.files_processed} processed</span>
          <button onClick={props.onShowScanDetails} title="View scan details" className="p-1 rounded hover:bg-surface-hover transition"><Info size={14} className="text-content-muted" /></button>
        </div>
      )}

      {!offline && (
        <Section title="Behavior">
          <SwitchRow title="Watch for new files" checked={root.monitor_enabled} onChange={props.onToggleMonitor}
            description="Re-scan this folder periodically in the background. The interval is global — Settings → Scanning." />
          <SwitchRow title="Include in duplicate detection" checked={root.find_duplicates} onChange={props.onToggleDuplicates}
            description="Files here are content-hashed and compared against every other folder with this enabled." />
          <SwitchRow title="Treat camera as unique to this folder" checked={root.unique_camera} onChange={props.onToggleUniqueCamera}
            description="Two rigs with the same camera model? Keeps their calibration frames apart. Takes effect after the next scan." />
        </Section>
      )}

      {(missingCount > 0 || displayErrors.length > 0) && (
        <Section title="Needs attention">
          {missingCount > 0 && (
            <div className="rounded-lg border border-orange/40 bg-surface">
              <button onClick={() => { const next = !missingOpen; setMissingOpen(next); if (next && !missingFiles) void loadMissing(); }}
                className="w-full flex items-center gap-2 p-2.5 text-left text-sm text-orange hover:bg-orange/10 rounded-lg transition">
                {missingOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                <AlertTriangle size={14} /> {missingCount} file{missingCount !== 1 ? 's' : ''} missing from disk
              </button>
              {missingOpen && (missingFiles
                ? <div className="p-2"><MissingFilesPanel rootId={root.id!} missingFiles={missingFiles} onRefresh={() => { void loadMissing(); props.onMissingChanged(); }} /></div>
                : <div className="p-3 text-xs text-content-muted flex items-center gap-2"><Loader2 size={12} className="animate-spin" /> loading…</div>)}
            </div>
          )}
          {displayErrors.length > 0 && (
            <div className="rounded-lg border border-error/30 bg-surface mt-2">
              <button onClick={() => setErrorsOpen((v) => !v)}
                className="w-full flex items-center gap-2 p-2.5 text-left text-sm text-error hover:bg-error-muted rounded-lg transition">
                {errorsOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                <AlertCircle size={14} /> {displayErrors.length} file{displayErrors.length !== 1 ? 's' : ''} failed in last scan
              </button>
              {errorsOpen && (
                <div className="px-3 py-2 max-h-40 overflow-y-auto space-y-1">
                  {displayErrors.map((err, i) => <p key={i} className="text-xs text-error/80 font-mono break-all">{err}</p>)}
                </div>
              )}
            </div>
          )}
        </Section>
      )}

      <Section title="Remove">
        <div className="flex items-center gap-3 p-3 rounded-lg border border-error/30 bg-surface">
          <p className="flex-1 text-xs text-content-muted">
            Forgets the folder and its catalog entries (frames, the sets they belong to).{' '}
            <span className="font-semibold text-content">Files on disk are never touched.</span>
          </p>
          <button onClick={props.onRemove}
            className="shrink-0 px-3 py-1.5 rounded-lg border border-error/50 text-error text-sm hover:bg-error-muted transition">
            Remove folder…
          </button>
        </div>
      </Section>
    </div>
  );
}

export function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="px-3 py-1.5 bg-surface rounded-lg">
      <div className="text-sm font-bold text-content">{value}</div>
      <div className="text-[10px] text-content-muted">{label}</div>
    </div>
  );
}

export function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mt-5 pt-4 border-t border-border">
      <div className="text-[10px] font-bold uppercase tracking-wider text-content-muted mb-2">{title}</div>
      {children}
    </div>
  );
}
```

- [ ] **Step 2: Typecheck + commit**

Run: `npx tsc --noEmit` → clean.

```bash
git add src/components/folders/MonitoredInspector.tsx && git commit -m "feat(folders): monitored-folder inspector with offline banner and attention panels"
```

---

### Task 8: Frontend — `RoleInspector.tsx` + `ArchiveInspector.tsx`

**Files:**
- Create: `src/components/folders/RoleInspector.tsx`
- Create: `src/components/folders/ArchiveInspector.tsx`

**Interfaces:**
- Consumes: `Stat`/`Section` from `MonitoredInspector.tsx`, `SwitchRow`, `roleMeta.ts`, `format.ts`, `listArchiveZips` from `../../api/archive`, types as below.
- Produces: `<RoleInspector/>`, `<RolePlaceholderInspector/>`, `<ArchiveInspector/>` for Task 9.

- [ ] **Step 1: Write `RoleInspector.tsx`:**

```tsx
import { ExternalLink, RefreshCw } from 'lucide-react';
import { revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { formatTimestamp } from '../../utils/dateFormatting';
import { SwitchRow } from './SwitchRow';
import { Stat, Section } from './MonitoredInspector';
import { basename, formatBytes } from './format';
import { ROLE_META, type RoleKind } from './roleMeta';
import type { ScanRootWithAvailability } from '../../types/helpers';
import type { ScanRootOverview } from '../../types/models';

interface RoleInspectorProps {
  kind: RoleKind;
  /** The dedicated scan root — null for a covered calibration library (settings-only). */
  root: ScanRootWithAvailability | null;
  /** Effective directory (root path, or the covered library path). */
  dir: string;
  /** Monitored root covering a settings-only calibration library, if any. */
  coveredBy: string | null;
  overview: ScanRootOverview | undefined;
  isScanning: boolean;
  onScan: () => void;
  onChangeFolder: () => void;
  onReleaseRole: () => void;
  onToggleDuplicates: (v: boolean) => void;
  onToggleMonitor: (v: boolean) => void;
}

export function RoleInspector(props: RoleInspectorProps) {
  const meta = ROLE_META[props.kind];
  const { root, dir, coveredBy, overview } = props;
  const offline = root ? !root.is_available : false;
  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 overflow-y-auto">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-lg font-bold text-content">
            <span className="truncate">{basename(dir)}</span>
            <span className={`px-2 py-0.5 rounded-full text-[10px] font-semibold ${meta.chip}`}>{meta.label}</span>
            {offline && <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-error-muted text-error border border-error/40">offline</span>}
          </div>
          <div className="flex items-center gap-2 font-mono text-xs text-content-muted">
            <span className="truncate">{dir}</span>
            {isTauri && !offline && (
              <button onClick={() => revealItemInDir(dir).catch((e) => console.error('reveal failed:', e))}
                title="Reveal in file manager" className="p-0.5 rounded hover:text-accent transition"><ExternalLink size={12} /></button>
            )}
          </div>
        </div>
        {root && !offline && (
          <button onClick={props.onScan} disabled={props.isScanning}
            className="flex items-center gap-2 px-3 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg text-sm transition disabled:opacity-50 shrink-0">
            <RefreshCw size={14} className={props.isScanning ? 'animate-spin' : ''} /> {props.isScanning ? 'Scanning…' : 'Scan now'}
          </button>
        )}
      </div>

      <div className={`mt-4 p-3 rounded-lg bg-surface border text-xs text-content-muted ${meta.chip.includes('purple') ? 'border-purple/40' : 'border-border'}`}>
        {meta.explainer}
        {overview && <span className="font-semibold text-content"> {overview.file_count.toLocaleString()} files cataloged.</span>}
      </div>

      <div className="flex flex-wrap gap-2 mt-3">
        <Stat label="placement" value={coveredBy ? `inside ${basename(coveredBy)}` : 'standalone · own scanned folder'} />
        {root?.last_scan && <Stat label="last scan" value={formatTimestamp(root.last_scan)} />}
        {overview && <Stat label="on disk" value={formatBytes(overview.total_bytes)} />}
      </div>

      <Section title="Role">
        <div className="flex items-center gap-3 p-3 rounded-lg border border-border bg-surface">
          <p className="flex-1 text-xs text-content-muted">
            Move the role to a different folder, or release it. Releasing keeps the folder monitored and never touches files.
          </p>
          <button onClick={props.onChangeFolder} className="shrink-0 px-3 py-1.5 rounded-lg bg-surface-hover text-sm text-content hover:brightness-110 transition">Change folder…</button>
          <button onClick={props.onReleaseRole} className="shrink-0 px-3 py-1.5 rounded-lg bg-surface-hover text-sm text-content hover:brightness-110 transition">Release role</button>
        </div>
      </Section>

      {root && !offline && (
        <Section title="Behavior">
          {meta.switches.watch && (
            <SwitchRow title="Watch for new files" checked={root.monitor_enabled} onChange={props.onToggleMonitor}
              description={props.kind === 'calibration_library'
                ? 'Imports masters dropped in from outside. The interval is global — Settings → Scanning.'
                : 'Re-scan this folder periodically in the background. The interval is global — Settings → Scanning.'} />
          )}
          {meta.switches.duplicates && (
            <SwitchRow title="Include in duplicate detection" checked={root.find_duplicates} onChange={props.onToggleDuplicates}
              description="Files here are content-hashed and compared against every other folder with this enabled." />
          )}
        </Section>
      )}
    </div>
  );
}

/** Placeholder-selection state — role not assigned yet (spec §4). */
export function RolePlaceholderInspector({ kind, onSetUp }: { kind: RoleKind; onSetUp: () => void }) {
  const meta = ROLE_META[kind];
  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 flex flex-col items-center justify-center text-center">
      <meta.icon size={36} className={`${meta.tint} opacity-70`} />
      <div className="mt-3 text-base font-bold text-content">{meta.label} — not set</div>
      <p className="mt-1 max-w-sm text-xs text-content-muted">{meta.purpose} {meta.placementRule}</p>
      <button onClick={onSetUp} className="mt-4 px-4 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg text-sm transition">Set up…</button>
    </div>
  );
}
```

- [ ] **Step 2: Write `ArchiveInspector.tsx`** (contents browsing moves here from the old `ArchiveRootRow`):

```tsx
import { useEffect, useState } from 'react';
import { Archive as ArchiveIcon, Star, ExternalLink } from 'lucide-react';
import { listArchiveZips } from '../../api/archive';
import { revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { Stat, Section } from './MonitoredInspector';
import { basename, formatBytes } from './format';
import type { ArchiveRoot, ArchivedFrameSetSummary, ArchiveZip } from '../../types/helpers';

interface ArchiveInspectorProps {
  root: ArchiveRoot;
  archivedSets: ArchivedFrameSetSummary[];
  totalZipBytes: number;
  onSetDefault: () => void;
  onRemove: () => void;
}

export function ArchiveInspector({ root, archivedSets, totalZipBytes, onSetDefault, onRemove }: ArchiveInspectorProps) {
  const sets = archivedSets.filter((s) => (s.archive_root_path ?? '') === root.path);
  const [zipsBySet, setZipsBySet] = useState<Record<number, ArchiveZip[]>>({});

  useEffect(() => {
    setZipsBySet({});
    let cancelled = false;
    (async () => {
      for (const s of sets) {
        if (!s.operation_id) continue;
        try {
          const zips = await listArchiveZips(s.operation_id);
          if (cancelled) return;
          setZipsBySet((prev) => ({ ...prev, [s.operation_id!]: zips }));
        } catch (e) {
          console.error('[ArchiveInspector] list zips failed:', e);
        }
      }
    })();
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root.id]);

  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 overflow-y-auto">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-lg font-bold text-content">
            <span className="truncate">{basename(root.path)}</span>
            {root.is_default
              ? <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-warning/20 text-warning border border-warning/40">★ Default destination</span>
              : <button onClick={onSetDefault} className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-surface-hover text-content-muted border border-border hover:text-warning transition flex items-center gap-1"><Star size={10} /> Make default</button>}
          </div>
          <div className="flex items-center gap-2 font-mono text-xs text-content-muted">
            <span className="truncate">{root.path}</span>
            {isTauri && (
              <button onClick={() => revealItemInDir(root.path).catch((e) => console.error('reveal failed:', e))}
                title="Reveal in file manager" className="p-0.5 rounded hover:text-accent transition"><ExternalLink size={12} /></button>
            )}
          </div>
        </div>
      </div>

      <div className="mt-4 p-3 rounded-lg bg-surface border border-warning/40 text-xs text-content-muted">
        &ldquo;Move and ZIP&rdquo; writes finished frame sets here. Never scanned — it may live anywhere, even inside a monitored folder.
      </div>

      <div className="flex flex-wrap gap-2 mt-3">
        <Stat label="archived sets" value={String(sets.length)} />
        <Stat label="total size" value={totalZipBytes > 0 ? formatBytes(totalZipBytes) : '—'} />
      </div>

      <Section title="Contents">
        {sets.length === 0 && <p className="text-xs text-content-muted">No archived frame sets stored in this folder yet.</p>}
        <div className="space-y-2">
          {sets.map((set) => {
            const zips = set.operation_id ? zipsBySet[set.operation_id] : undefined;
            return (
              <div key={set.frames_set_id} className="rounded-lg border border-border bg-surface p-3">
                <div className="flex items-center gap-2 text-sm font-medium text-content">
                  <ArchiveIcon size={14} className="text-content-muted" /> {set.name ?? `Frame Set #${set.frames_set_id}`}
                </div>
                <div className="text-xs text-content-muted mt-0.5">
                  {set.archived_at?.slice(0, 10) ?? ''} · {set.lights_count} lights / {set.flats_count} flats / {set.darks_count} darks / {set.bias_count} bias / {set.darkflats_count} darkflats
                </div>
                {zips && zips.length > 0 && (
                  <ul className="mt-2 space-y-1">
                    {zips.map((z) => (
                      <li key={z.path} className="flex items-center gap-2 text-xs">
                        <span className="font-mono text-content-muted truncate flex-1">{z.filename}</span>
                        <span className="text-content-muted whitespace-nowrap">{formatBytes(z.size_bytes)}</span>
                        {!z.exists && <span className="text-error whitespace-nowrap">missing</span>}
                        {isTauri && z.exists && (
                          <button onClick={() => revealItemInDir(z.path).catch((e) => console.error('reveal failed:', e))}
                            title="Reveal in file manager" className="p-0.5 rounded text-content-muted hover:text-accent transition"><ExternalLink size={11} /></button>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            );
          })}
        </div>
      </Section>

      <Section title="Remove">
        <div className="flex items-center gap-3 p-3 rounded-lg border border-error/30 bg-surface">
          <p className="flex-1 text-xs text-content-muted">Removes it from this list only — zips on disk stay.</p>
          <button onClick={onRemove} className="shrink-0 px-3 py-1.5 rounded-lg border border-error/50 text-error text-sm hover:bg-error-muted transition">Remove…</button>
        </div>
      </Section>
    </div>
  );
}
```

- [ ] **Step 3: Typecheck + commit**

Run: `npx tsc --noEmit` → clean.

```bash
git add src/components/folders && git commit -m "feat(folders): role and archive inspectors"
```

---

### Task 9: Frontend — `FoldersTab.tsx` assembly

**Files:**
- Create: `src/components/folders/FoldersTab.tsx`

**Interfaces:**
- Consumes: everything from Tasks 4–8; `useScanRootsWithAvailability`, `useScanProgressContext`, `useNotifications`, `ConfirmDialog`, `AlertDialog`, `ScanSummaryModal`, `FolderBrowserModal`, `pickDirectory`, `listArchiveRoots`/`listArchivedFrameSets`/`deleteArchiveRoot`/`setDefaultArchiveRoot` from `../../api/archive`, commands `get_folder_overview`, `get_missing_files_counts`, `switch_calibration_library_dir`, role get/set/clear.
- Produces: `<FoldersTab selectSyncIncomingToken={number} />` — Task 10 mounts it. `selectSyncIncomingToken` increments each time the Transfers deep-link fires; the tab then selects the Sync Incoming row (or its placeholder).

- [ ] **Step 1: Write the component:**

```tsx
import { useCallback, useEffect, useState } from 'react';
import { FolderPlus } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useScanRootsWithAvailability } from '../../hooks/useTauri';
import { useScanProgressContext } from '../../contexts/ScanProgressContext';
import { useNotifications } from '../../contexts/NotificationContext';
import { listArchiveRoots, listArchivedFrameSets, deleteArchiveRoot, setDefaultArchiveRoot } from '../../api/archive';
import { ConfirmDialog } from '../ConfirmDialog';
import { AlertDialog } from '../AlertDialog';
import { ScanSummaryModal } from '../ScanSummaryModal';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { FolderRail } from './FolderRail';
import { AddFolderDialog } from './AddFolderDialog';
import { MonitoredInspector } from './MonitoredInspector';
import { RoleInspector, RolePlaceholderInspector } from './RoleInspector';
import { ArchiveInspector } from './ArchiveInspector';
import { ROLE_META, type RailSelection, type RoleKind, type AddableKind } from './roleMeta';
import type { ArchiveRoot, ArchivedFrameSetSummary, ScanResult } from '../../types/helpers';
import type { FolderOverview, RelinkResult } from '../../types/models';

interface FoldersTabProps {
  /** Increments when the Transfers deep-link asks to focus Sync Incoming. */
  selectSyncIncomingToken: number;
}

export default function FoldersTab({ selectSyncIncomingToken }: FoldersTabProps) {
  const {
    scanRoots, loading: rootsLoading, error: rootsError, clearError: clearRootsError,
    deleteScanRoot, toggleDuplicatesFlag, toggleUniqueCameraFlag, toggleMonitorEnabled,
    relinkScanRoot, refresh: refreshScanRoots,
  } = useScanRootsWithAvailability();
  const { startRescanWithProgress, isScanning, activeScans } = useScanProgressContext();
  const { notify } = useNotifications();

  const [archiveRoots, setArchiveRoots] = useState<ArchiveRoot[]>([]);
  const [archivedSets, setArchivedSets] = useState<ArchivedFrameSetSummary[]>([]);
  const [overview, setOverview] = useState<FolderOverview | null>(null);
  const [missingCounts, setMissingCounts] = useState<Record<number, number>>({});
  const [selection, setSelection] = useState<RailSelection | null>(null);
  const [addDialog, setAddDialog] = useState<{ open: boolean; preselect?: AddableKind }>({ open: false });
  const [scanResultMap, setScanResultMap] = useState<Record<number, ScanResult>>({});
  const [relinkingRootId, setRelinkingRootId] = useState<number | null>(null);
  const [relinkResult, setRelinkResult] = useState<RelinkResult | null>(null);
  const [relinkBrowserRootId, setRelinkBrowserRootId] = useState<number | null>(null);
  const [roleChangeBrowser, setRoleChangeBrowser] = useState<RoleKind | null>(null);
  const [scanSummary, setScanSummary] = useState<{ rootId: number; rootPath: string; missingFilesCount?: number } | null>(null);
  const [confirm, setConfirm] = useState<{ title: string; message: string; onConfirm: () => void; danger?: boolean } | null>(null);
  const [alert, setAlert] = useState<{ title: string; message: string; variant: 'error' | 'warning' | 'info' } | null>(null);

  const showAlert = (title: string, message: string) => setAlert({ title, message, variant: 'error' });

  const refreshAux = useCallback(async () => {
    try {
      const [roots, sets, ov, counts] = await Promise.all([
        listArchiveRoots(),
        listArchivedFrameSets(),
        api.invoke<FolderOverview>('get_folder_overview'),
        api.invoke<Record<number, number>>('get_missing_files_counts'),
      ]);
      setArchiveRoots(roots);
      setArchivedSets(sets);
      setOverview(ov);
      setMissingCounts(counts);
    } catch (e) {
      console.error('[FoldersTab] aux refresh failed:', e);
    }
  }, []);

  useEffect(() => { void refreshAux(); }, [refreshAux]);

  const refreshAll = useCallback(() => { void refreshScanRoots(); void refreshAux(); }, [refreshScanRoots, refreshAux]);

  // Default selection: first monitored folder once loaded.
  useEffect(() => {
    if (selection || rootsLoading) return;
    const first = scanRoots.filter((r) => r.kind === 'normal')[0] ?? scanRoots[0];
    if (first?.id) setSelection({ type: 'scan', id: first.id });
  }, [scanRoots, rootsLoading, selection]);

  // Transfers deep-link → select Sync Incoming (root or placeholder).
  useEffect(() => {
    if (selectSyncIncomingToken === 0) return;
    const root = scanRoots.find((r) => r.kind === 'sync_incoming');
    setSelection(root?.id ? { type: 'scan', id: root.id } : { type: 'placeholder', kind: 'sync_incoming' });
  }, [selectSyncIncomingToken, scanRoots]);

  const scanPercent = useCallback((rootId: number) => {
    const p = activeScans.get(rootId)?.progress;
    return p ? p.percent : null;
  }, [activeScans]);

  const handleScan = async (rootId: number) => {
    const root = scanRoots.find((r) => r.id === rootId);
    if (!root) return;
    try {
      const result = await startRescanWithProgress(rootId, root.path);
      setScanResultMap((prev) => ({ ...prev, [rootId]: result }));
      setScanSummary({ rootId, rootPath: root.path, missingFilesCount: result.missingFilesCount });
      refreshAll();
    } catch (e) {
      console.error('[FoldersTab] scan failed:', e);
      showAlert('Scan failed', typeof e === 'string' ? e : String(e));
    }
  };

  const finishRelink = async (rootId: number, path: string) => {
    try {
      setRelinkingRootId(rootId);
      setRelinkResult(null);
      const result = await relinkScanRoot(rootId, path);
      setRelinkResult(result);
      refreshAll();
    } catch (e) {
      console.error('[FoldersTab] relink failed:', e);
      const msg = typeof e === 'string' ? e : String(e);
      showAlert('Relink failed', msg);
      notify({ title: 'Relink failed', detail: msg, kind: 'files', tone: 'warning' });
    } finally {
      setRelinkingRootId(null);
    }
  };

  const handleRelink = async (rootId: number) => {
    if (!isTauri) { setRelinkResult(null); setRelinkBrowserRootId(rootId); return; }
    const picked = await pickDirectory();
    if (picked && typeof picked === 'string') await finishRelink(rootId, picked);
  };

  const handleRemoveScanRoot = (id: number) => setConfirm({
    title: 'Remove folder',
    message: 'Remove this folder from the catalog? Its catalog entries are forgotten; files on disk are never touched.',
    danger: true,
    onConfirm: async () => {
      try {
        await deleteScanRoot(id);
        setSelection(null);
        refreshAll();
      } catch (e) {
        console.error('[FoldersTab] remove failed:', e);
        clearRootsError();
        showAlert('Remove failed', typeof e === 'string' ? e : String(e));
      }
    },
  });

  const handleReleaseRole = (kind: RoleKind) => setConfirm({
    title: `Release ${ROLE_META[kind].label} role`,
    message: 'The folder stays monitored and files on disk are untouched. You can assign the role again at any time.',
    onConfirm: async () => {
      try {
        await api.invoke(ROLE_META[kind].clearCommand);
        setSelection(null);
        refreshAll();
      } catch (e) {
        console.error('[FoldersTab] release role failed:', e);
        showAlert('Release failed', typeof e === 'string' ? e : String(e));
      }
    },
  });

  const applyRoleChange = async (kind: RoleKind, path: string) => {
    try {
      if (kind === 'calibration_library') {
        await api.invoke<string>('switch_calibration_library_dir', { path });
      } else {
        await api.invoke(ROLE_META[kind].clearCommand);
        await api.invoke<string>(ROLE_META[kind].setCommand, { path });
      }
      refreshAll();
    } catch (e) {
      console.error('[FoldersTab] change role folder failed:', e);
      const msg = typeof e === 'string' ? e : String(e);
      showAlert('Change folder failed', msg);
      notify({ title: `Could not move ${ROLE_META[kind].label}`, detail: msg, kind: 'files', tone: 'warning', hasErrors: true });
    }
  };

  const handleChangeRoleFolder = (kind: RoleKind) => {
    const proceed = async () => {
      if (!isTauri) { setRoleChangeBrowser(kind); return; }
      const picked = await pickDirectory();
      if (picked && typeof picked === 'string') await applyRoleChange(kind, picked);
    };
    if (kind === 'calibration_library') {
      setConfirm({
        title: 'Move Calibration Library',
        message: 'The old library folder is removed from the catalog (its masters’ catalog entries are deleted; files on disk are kept). The new folder becomes the master destination in one step.',
        danger: true,
        onConfirm: proceed,
      });
    } else {
      void proceed();
    }
  };

  const handleDeleteArchiveRoot = (root: ArchiveRoot) => setConfirm({
    title: 'Remove archive folder',
    message: `Remove "${root.path}" from the configured list? Files in that folder are not deleted.`,
    danger: true,
    onConfirm: async () => {
      try {
        await deleteArchiveRoot(root.id);
        setSelection(null);
        refreshAll();
      } catch (e) {
        console.error('[FoldersTab] delete archive root failed:', e);
        showAlert('Remove failed', typeof e === 'string' ? e : String(e));
      }
    },
  });

  const empty = !rootsLoading && scanRoots.length === 0 && archiveRoots.length === 0;

  // ── Inspector resolution ──────────────────────────────────────────────────
  let inspector: React.ReactNode = null;
  if (selection?.type === 'scan') {
    const root = scanRoots.find((r) => r.id === selection.id);
    if (root) {
      const ov = overview?.scan_roots.find((s) => s.root_id === root.id);
      if (root.kind === 'normal') {
        inspector = (
          <MonitoredInspector
            root={root}
            overview={ov}
            missingCount={root.id ? (missingCounts[root.id] ?? 0) : 0}
            scanResult={root.id ? (scanResultMap[root.id] ?? null) : null}
            isScanning={root.id ? isScanning(root.id) : false}
            relinking={relinkingRootId === root.id}
            relinkResult={relinkResult}
            onScan={() => root.id && handleScan(root.id)}
            onRelink={() => root.id && handleRelink(root.id)}
            onShowScanDetails={() => root.id && setScanSummary({ rootId: root.id, rootPath: root.path })}
            onToggleDuplicates={(v) => root.id && toggleDuplicatesFlag(root.id, v)}
            onToggleUniqueCamera={(v) => { if (root.id) void toggleUniqueCameraFlag(root.id, v).catch((e) => console.error(e)); }}
            onToggleMonitor={(v) => { if (root.id) void toggleMonitorEnabled(root.id, v).catch((e) => console.error(e)); }}
            onRemove={() => root.id && handleRemoveScanRoot(root.id)}
            onMissingChanged={() => void refreshAux()}
          />
        );
      } else {
        const kind = root.kind as RoleKind;
        inspector = (
          <RoleInspector
            kind={kind}
            root={root}
            dir={root.path}
            coveredBy={null}
            overview={ov}
            isScanning={root.id ? isScanning(root.id) : false}
            onScan={() => root.id && handleScan(root.id)}
            onChangeFolder={() => handleChangeRoleFolder(kind)}
            onReleaseRole={() => handleReleaseRole(kind)}
            onToggleDuplicates={(v) => root.id && toggleDuplicatesFlag(root.id, v)}
            onToggleMonitor={(v) => { if (root.id) void toggleMonitorEnabled(root.id, v).catch((e) => console.error(e)); }}
          />
        );
      }
    }
  } else if (selection?.type === 'placeholder') {
    inspector = <RolePlaceholderInspector kind={selection.kind} onSetUp={() => setAddDialog({ open: true, preselect: selection.kind })} />;
  } else if (selection?.type === 'archive') {
    const root = archiveRoots.find((r) => r.id === selection.id);
    if (root) {
      inspector = (
        <ArchiveInspector
          root={root}
          archivedSets={archivedSets}
          totalZipBytes={overview?.archive_roots.find((a) => a.archive_root_id === root.id)?.total_zip_bytes ?? 0}
          onSetDefault={async () => { try { await setDefaultArchiveRoot(root.id); refreshAll(); } catch (e) { showAlert('Failed', String(e)); } }}
          onRemove={() => handleDeleteArchiveRoot(root)}
        />
      );
    }
  }

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      {rootsError && (
        <div className="mb-3 p-3 bg-error-muted border border-error/50 rounded-lg">
          <p className="text-error text-sm">Error loading folders: {String(rootsError)}</p>
        </div>
      )}

      {empty ? (
        <div className="flex-1 flex flex-col items-center justify-center text-center bg-surface-elevated rounded-lg">
          <FolderPlus size={40} className="text-info opacity-70" />
          <div className="mt-3 text-lg font-bold text-content">No folders yet</div>
          <p className="mt-1 max-w-sm text-sm text-content-muted">
            Add a folder with your FITS/XISF files to start cataloging. Roles and archive destinations can come later.
          </p>
          <button onClick={() => setAddDialog({ open: true })}
            className="mt-4 flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg transition">
            <FolderPlus size={18} /> Add Folder
          </button>
        </div>
      ) : (
        <div className="flex-1 min-h-0 flex gap-3">
          <FolderRail
            scanRoots={scanRoots}
            archiveRoots={archiveRoots}
            archivedSets={archivedSets}
            overview={overview}
            missingCounts={missingCounts}
            selection={selection}
            onSelect={(sel) => { setSelection(sel); setRelinkResult(null); }}
            onAdd={(preselect) => setAddDialog({ open: true, preselect })}
            onRescan={handleScan}
            isScanning={isScanning}
            scanPercent={scanPercent}
          />
          {inspector ?? (
            <div className="flex-1 bg-surface-elevated rounded-lg flex items-center justify-center text-sm text-content-muted">
              Select a folder on the left.
            </div>
          )}
        </div>
      )}

      <AddFolderDialog
        isOpen={addDialog.open}
        preselect={addDialog.preselect}
        scanRoots={scanRoots}
        onClose={() => setAddDialog({ open: false })}
        onAdded={refreshAll}
      />

      <ConfirmDialog
        isOpen={confirm !== null}
        title={confirm?.title ?? ''}
        message={confirm?.message ?? ''}
        onConfirm={() => { const c = confirm; setConfirm(null); c?.onConfirm(); }}
        onCancel={() => setConfirm(null)}
        confirmDanger={confirm?.danger}
      />
      <AlertDialog
        isOpen={alert !== null}
        title={alert?.title ?? ''}
        message={alert?.message ?? ''}
        variant={alert?.variant ?? 'info'}
        onClose={() => setAlert(null)}
      />
      {scanSummary && scanResultMap[scanSummary.rootId] && (
        <ScanSummaryModal
          isOpen={true}
          onClose={() => setScanSummary(null)}
          scanResult={scanResultMap[scanSummary.rootId]}
          rootPath={scanSummary.rootPath}
          missingFilesCount={scanSummary.missingFilesCount}
        />
      )}
      {/* Web mode: relink + role-change directory browsers */}
      <FolderBrowserModal
        isOpen={relinkBrowserRootId !== null}
        scope="scan"
        onSelect={(path) => { const id = relinkBrowserRootId; setRelinkBrowserRootId(null); if (id != null) void finishRelink(id, path); }}
        onClose={() => setRelinkBrowserRootId(null)}
      />
      <FolderBrowserModal
        isOpen={roleChangeBrowser !== null}
        scope="scan"
        onSelect={(path) => { const k = roleChangeBrowser; setRoleChangeBrowser(null); if (k) void applyRoleChange(k, path); }}
        onClose={() => setRoleChangeBrowser(null)}
      />
    </div>
  );
}
```

Note: `useScanProgressContext` must expose `activeScans` — it already does (returned by `useScanProgress`). If `ConfirmDialog`/`AlertDialog` prop names differ from the usage above, match the existing usage in `src/pages/FileManager.tsx:978-998`.

- [ ] **Step 2: Typecheck + commit**

Run: `npx tsc --noEmit` → clean.

```bash
git add src/components/folders/FoldersTab.tsx && git commit -m "feat(folders): FoldersTab master-detail assembly"
```

---

### Task 10: Integration — FileManager rename + delete old sections

**Files:**
- Modify: `src/pages/FileManager.tsx`
- Delete: `src/components/SpecialFolderSection.tsx`, `src/components/CalibrationFolderSection.tsx`, `src/components/archive/ArchiveFoldersSection.tsx`

- [ ] **Step 1: Rewire `FileManager.tsx`.** Replace the `directories` tab body and strip the state it owned:
  - Tab button label `Monitored Directories` → `Folders`; icon `FolderPlus` → `Folder` (add to the lucide import).
  - Add `const [syncIncomingToken, setSyncIncomingToken] = useState(0);` and change the existing `focusSyncIncoming` effect to `setActiveTab('directories'); setSyncIncomingToken((t) => t + 1);` (delete the scroll-into-view effect and the `focusSyncIncoming` state).
  - Replace the whole `{activeTab === 'directories' && ( … )}` block with:

```tsx
{activeTab === 'directories' && <FoldersTab selectSyncIncomingToken={syncIncomingToken} />}
```

  - Add `import FoldersTab from '../components/folders/FoldersTab';`.
  - The `browse` tab still needs `scanRoots` for `DualPaneFileBrowser` — KEEP `useScanRootsWithAvailability` in FileManager for that (two instances of the hook are fine; it's fetch-on-mount state).
  - Delete from FileManager everything the old tab owned and nothing else uses: `scanResultMap`, `scanError`, `relinkingRootId`, `relinkResult`, `scanSummaryModal`, `confirmDialog`, `alertDialog`, `expandedErrors`, `missingFilesCountMap`, `missingFilesMap`, `expandedMissingPanels`, `loadingMissingFiles`, `showFolderBrowser`, `relinkTargetRootId`, the handlers `handleAddDirectory`, `handleFolderBrowserSelect`, `handleRemoveScanRoot`, `handleStartScan`, `handleRelinkScanRoot`, `handleRelinkFolderBrowserSelect`, `loadMissingFilesForRoot`, `handleToggleMissingPanel`, `handleRefreshMissingFiles`, `showConfirm`/`showAlert`, and the now-unused JSX (`ConfirmDialog`, `AlertDialog`, `ScanSummaryModal`, both `FolderBrowserModal`s at page level, error banners for `scanError`) plus their imports. Keep the Duplicates-tab `showConfirm`/`showAlert` usages working: the Duplicates folder-view uses them (`FileManager.tsx:868-940`) — so KEEP `confirmDialog`/`alertDialog` state, `showConfirm`/`showAlert`, `ConfirmDialog`/`AlertDialog` JSX, and remove only what the duplicates tab does not reference. Verify each removal with a grep before deleting.
  - Remove imports of the three deleted section components.

- [ ] **Step 2: Delete the old components:**

```bash
grep -rn "SpecialFolderSection\|CalibrationFolderSection\|ArchiveFoldersSection" src/ --include="*.tsx" --include="*.ts"
# Expected: no references outside the three files themselves
git rm src/components/SpecialFolderSection.tsx src/components/CalibrationFolderSection.tsx src/components/archive/ArchiveFoldersSection.tsx
```

- [ ] **Step 3: Typecheck + build + commit**

Run: `npx tsc --noEmit` → clean. Run: `npm run build` (or `npx vite build`) → success.

```bash
git add -A && git commit -m "feat(folders): replace Monitored Directories tab with the Folders workspace; drop old section components"
```

---

### Task 11: Final gates + manual smoke

- [ ] **Step 1: Full gates**

```bash
cargo build --workspace
cargo test -p athenaeum-core
npx tsc --noEmit
```

All must pass.

- [ ] **Step 2: Desktop smoke** (`npm run tauri dev`) — walk the checklist, fix anything broken before committing fixes:
  1. Folders tab renders rail + inspector; role-tinted lucide icons; default selection = first monitored folder.
  2. Rail ↻ scans without changing selection; spinner + percent in the sub-line; ScanSummaryModal appears after.
  3. Add Folder → Monitored: rule shown before picking; picking a folder inside an existing root shows the inline `inside_existing` message and Add stays disabled; a valid folder adds and appears in the rail.
  4. Add Folder → each role from the global button AND from a "Set up…" row (dialog pre-selected); taken roles disabled with ✓ + path.
  5. Calibration: covered pick shows the "inside a monitored folder" success note; standalone shows "becomes its own scanned library folder"; Change folder… on the calibration row shows the purge confirmation and completes in one step; Release role demotes to Monitored group.
  6. Sync Incoming / Collaboration: Change folder = clear+set; Release keeps it monitored.
  7. Offline folder (unmount a test volume): badge in rail, banner in inspector, Relink round-trip shows matched/new/orphaned.
  8. Missing files: badge in rail, Review panel works, count refreshes after actions.
  9. Archive: inspector lists sets + zips, Make default works, Remove leaves zips on disk.
  10. Transfers → app-data warning deep-link lands on the Sync Incoming row (or placeholder).
  11. Empty state: fresh DB (`ATHENAEUM_DB_PATH` to a temp file) shows the centered Add Folder panel.
- [ ] **Step 3: Web smoke** (`npm run dev:web` + `cargo run -p athenaeum-web`): Add Folder via FolderBrowserModal for monitored AND archive kinds; role setup; validation messages render.
- [ ] **Step 4: Commit any smoke fixes**

```bash
git add -A && git commit -m "fix(folders): smoke-test fixes"
```

---

## Self-Review (done at plan-writing time)

- **Spec coverage:** D1–D9 → Tasks 5 (rail, ↻, badges), 6 (dialog), 7–8 (inspector states, switch matrix, hidden toggles), 9 (assembly, deep-link, empty state), 10 (tab rename, dedup by deletion of old sections), 1–3 (backend §8). Spec §5.2 covered-calibration rail row: `RoleInspector` accepts `root: null`/`coveredBy` for it; the rail currently derives role rows from scan roots only — a settings-only covered library has no root, so **FoldersTab must also surface it**: covered case ships via the placeholder-row slot replaced by a covered-row. Follow-up wired into Task 9: when `get_calibration_library_dir` returns a dir with no `calibration_library` root, render the role row from that dir (`RailSelection {type:'placeholder',kind:'calibration_library'}` selecting a `RoleInspector` with `root=null, coveredBy=<covering root>`). Implementer: fetch it in `refreshAux` via `api.invoke<string | null>('get_calibration_library_dir')`, store as `coveredCalibrationDir`, pass to `FolderRail` as an extra optional prop `coveredCalibrationDir?: string | null` rendered as an assigned-style row (no ↻), and branch in the inspector resolution.
- **Placeholder scan:** none of the forbidden patterns remain; all steps carry code.
- **Type consistency:** `FolderCandidateVerdict`/`FolderOverview` snake_case in TS everywhere (`conflicting_path`, `scan_roots`, `archive_root_id`, `total_zip_bytes`); `RailSelection` shape identical in Tasks 4/5/9; `Stat`/`Section` exported from Task 7 and imported in Task 8; command names match across core/tauri/web/frontend.
