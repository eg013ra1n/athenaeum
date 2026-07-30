# Cross-platform path audit — Windows (NTFS) + Linux (ext4)

Date: 2026-07-30. Scope: file management (file_op), folder scanning (scanner/monitor/scan_roots), relinking, missing files, duplicates, archive/restore, calibration library + light calibration outputs, WBPP export, DB path queries, frontend path handling. Method: 6 parallel read-only audit agents (file_op / scanner+relinking / DB SQL / archive / library+export / frontend), findings verified against source and (where marked) against a live SQLite / executed JS snippets.

Baseline: development happens on macOS; Windows/Linux builds ship from CI (compile is proven). This audit targets **runtime behavior** on NTFS (`\` separators, drive letters, UNC, case-insensitive, MAX_PATH 260, reserved names) and ext4 (case-sensitive, 255-byte components).

## Verdict

- **Linux**: mostly works. Main risks are the unescaped-`LIKE` family (SQLite `LIKE` is ASCII case-insensitive → distinct `M31/` and `m31/` dirs cross-match; `_` acts as a wildcard and is ubiquitous in astro names) and the bind-mount `EXDEV` rename case in the Docker build.
- **Windows**: several features are **broken outright** (restore-to-original, duplicates keep-rule "path contains", UNC breadcrumb navigation, web-build PathPolicy) and two write paths **corrupt the catalog's path spelling** (`relink_scan_root` persists `\\?\` verbatim paths; restore persists mixed `/`+`\` separators).
- **All platforms incl. macOS**: the three surviving unescaped `LIKE root || '%'` write paths (relinking, missing-files, calibration-set rebuild) can sweep name-prefix **sibling roots** — the exact hazard commit `81aedae7` fixed elsewhere; these sites were missed.

No compile blockers found: every `std::os::unix` use is `#[cfg(unix)]`-gated with a working Windows counterpart (verified by inspection; local `--target x86_64-pc-windows-msvc` check aborts in `ring`'s C build on this host — CI's Windows build is the compile gate).

---

## Critical

**C1 — `relink_files` sweeps sibling roots and rewrites their paths.**
`crates/athenaeum-core/src/relinking/mod.rs:53-61` (predicate `f.path LIKE ?1` with `format!("{}%", old_root_path)`), `:137-141` (the UPDATE); same pair again in `verify_files_at_location` at `:195`/`:198`. Three defects: no trailing separator, no `ESCAPE`, ASCII case-insensitive `LIKE`. Verified in SQLite: `/data/M31%` matches `/data/M31_Ha/…`, `/data/M31XHa/…`, `/data/m31/…`. Sibling-root rows enter the fingerprint map; matches get `files.path` **rewritten** into the new root; the rest are reported orphaned. Catalog corruption, not a bad read.
Fix: byte-range form (`native_separator_of` + `trim_end_matches(sep)` + `path_prefix_upper`), same shape as `db::frame_ids_under_paths`.

**C2 — `recreate_calibration_sets_for_root` groups sibling roots' calibration frames.**
`crates/athenaeum-core/src/api/scan_roots.rs:1348` + `:1351-1356` — same `LIKE` defects. Asymmetric with its own delete step (`delete_calibration_sets_for_root` IS separator-strict since 81aedae7): deletes narrowly, rebuilds widely → a sibling root's darks/flats are written into this root's rebuilt sets → wrong-camera matching downstream. Fix: same byte-range substitution.

**C3 — Restore-to-original is unreachable on Windows; UI silently relocates data.**
`crates/athenaeum-tauri/src/commands/archive.rs:348-350` + mirror `crates/athenaeum-web/src/routes/archive.rs:325-326`: `source_path.strip_suffix(&path_in_zip)` strips a `/`-separated zip path off a `\`-separated OS path — never matches on Windows; `trim_end_matches('/')` is a second dead hardcoded `/`. Consequence chain (verified through the UI): suggestion `None` → `RestoreDialog.tsx:119` permanently disables "Original location" → defaults to `scanRoots[0]` (arbitrary) → `classify_target` → `UnderRoot` → files restored under the wrong root **and `files.path` rewritten there** (`restore.rs:400-402`). Fix: component-wise comparison (`Path::ancestors`/`components`), never string `strip_suffix`.

**C4 — `relink_scan_root` persists Windows `\\?\` verbatim paths.**
`crates/athenaeum-core/src/api/scan_roots.rs:1091-1098` — the only path-writing site in the file whose `canonicalize()` is NOT wrapped in `normalize_path()` (cf. `:233`, `:289`, `:443`, `:641`, `:828`). On Windows writes `\\?\C:\Astro` into `scan_roots.path` and, via the relink walk, `\\?\C:\Astro\…` into `files.path` → two spellings of one location in one catalog; unmatched rows fall permanently outside the root's byte-range (delete cascade, overview counts, missing-files, duplicates silently skip them); raw `\\?\` shown in Folders UI. Fix: one-line `normalize_path(...)` wrapper.

---

## Important

### SQL / prefix family

- **I1 `find_missing_files` uses the same unbounded `LIKE` prefix, both backends** — `api/scan_roots.rs:1232`+`:1235`; `athenaeum-web/src/routes/missing_files.rs:117`+`:121`. Sibling-root files reported "missing" under the wrong root; entry point to user-driven relink/remove. Change both backends together.
- **I2 `get_folder_overview` returns 0 files/0 bytes for a trailing-separator root** — `api/scan_roots.rs:1592-1594`. Both separator arms are correct but it's the only prefix site skipping `trim_end_matches(sep)`; verified: root `C:\` or `/` → 0 rows counted. Windows drive-letter roots are the common case.
- **I3 `get_files_by_directory{,_for_camera}` key on build-OS `MAIN_SEPARATOR`** — `db/operations.rs:856-859`, `:966-972`. Wrong for a Linux-hosted web build serving Windows-shaped rows; also no trailing-sep trim → a `D:\` drive root lists **zero** files in the dual-pane browser (prefix `D:\\`, depth off-by-one). Fix: `native_separator_of(directory_path)` + trim.
- **I4 `native_separator_of` sniffs only the first char** — `db/operations.rs:74-76`. A legal Windows path spelled `C:/Astro/Old` classifies as `\`-separated → prefix `C:/Astro/Old\` matches nothing → rename/delete/scan prefix ops silently no-op. Reachable via any non-canonicalized writer. Fix: pick separator by last occurrence of either, or normalize separators at every write boundary.
- **I5 `enrich_duplicate_groups` root attribution via bare `starts_with`** — `db/operations.rs:1725`. `/data/M31x` claims `/data/M31xyz/a.fits` despite longest-first ordering. Display-only but drives user deletion decisions. Fix: reuse `scanner::path_has_root_prefix`.

### file_op

- **I6 Same-device ≠ rename-works** — `file_op/planner.rs:196-202`. Linux bind mounts (same `st_dev`, `rename(2)` → `EXDEV`; reachable in Docker compose volumes) and Windows folder-mounted volumes (canonicalize hides the mount → `ERROR_NOT_SAME_DEVICE`) both produce a false `AtomicRename`; batch aborts loudly on the first file with a raw os error. Fix: on `EXDEV`/`ERROR_NOT_SAME_DEVICE` downgrade that row to `CopyVerifyDelete` instead of failing.
- **I7 Case-only rename rejected on Windows/macOS** — `api/files.rs:715-717`. `new.exists()` is true for `NGC7000.fits` when renaming `ngc7000.fits` → "target already exists". Normal astro-user action. Fix: skip the collision check when old/new canonicalize to the same file.
- **I8 Move hot-sync silently succeeds at zero rows** — `file_op/executor.rs:418-441`. Path-based UPDATE misses → id fallback `None` → `Ok(())` with no log at any level; both mechanisms miss together whenever path spelling drifts. Violates never-swallow-errors (the rename path DOES log counts). Fix: `warn!` on zero-row sync.
- **I9 `browse_directories` returns `\\?\C:\…` to the frontend** — `api/files.rs:790` → `:835`; only canonicalize in the domain whose result escapes. Lands in `FolderBrowserModal` → `add_scan_root` / `export_to_wbpp` verbatim; also renders `\\?\` junk in breadcrumbs. Fix: apply existing `normalize_path` to `current`/`parent`/`directories[].path`.

### Web build / policy

- **I10 `PathPolicy` denies everything on a Windows-hosted web build** — `athenaeum-web/src/routes/scan_roots.rs:394`, `routes/files.rs:339-345`. Allowed roots are canonicalized but NOT normalized (`\\?\C:\data`) while candidates are `normalize_path`-ed (`C:\data\x`); `Prefix::Disk` vs `Prefix::VerbatimDisk` never match → every sandboxed add/validate/set-library call is Forbidden. Fix: normalize allowed roots with the same helper (export it from `api::scan_roots`); case-fold non-prefix components under `#[cfg(windows)]` only.

### Relinking (additional)

- **I11 `relink_files` stores lossy paths** — `relinking/mod.rs:134` `to_string_lossy()` → a non-UTF-8 name becomes a U+FFFD path no later lookup can find and `std::fs` cannot open; the scanner deliberately rejects these (`scanner/mod.rs:28-35`). Fix: same `path_to_utf8` reject+log.
- **I12 Relink's WalkDir lacks the scanner's `.max_depth(64)` symlink-loop cap** — `relinking/mod.rs:91-95` with `follow_links(true)`; Windows junctions / SMB re-exports are realistic loop triggers. (Filed as important-adjacent; one line.)

### Archive / restore

- **I13 `classify_target` raw string `starts_with`** — `archive/restore.rs:84-85`. No component boundary, case-sensitive, separator-sensitive; a false negative flips restore from "put back" to "dump under root + rewrite catalog". Fix: `Path::starts_with` + Windows case-fold.
- **I14 Mixed separators persisted on restore** — `restore.rs:336` `root.join(&f.target_path_in_zip)` on Windows yields `C:\root\Lights/M31/x.fits`, written into `files.path` (`:400-402`); every exact-string consumer then diverges from the scanner's spelling. Fix: rebuild destination component-wise from the `/`-split.
- **I15 Planner scan-root match is case-sensitive → flattened zip layout → hash-mismatch abort** — `archive/planner.rs:130-136`, `:338-344`; fallback consumed at `path_layout.rs:108-118`. On Windows `C:\Astro` vs row `C:\astro\…` misses → tree flattens to `<parent>/<basename>`; two flattened files can collide in staging → `verify_copy_phase` aborts with a misleading hash-mismatch error (no data loss; archive permanently fails). Partner defect: `zip_reader::verify_zip_contents:18-25` HashSet compare can't detect duplicate entry names. Fix: case-fold root match on Windows; make fallback unique + assert `path_in_zip` uniqueness in `build_plan`.
- **I16 `archive_roots` matched by exact SQL string, never canonicalized on insert** — `archive/root.rs:44`, `commands/archive.rs:67-83`. `C:\Archive` vs `c:\Archive` vs UNC alias of the same share → "not a configured archive folder" / duplicate rows. Fix: canonicalize + `normalize_path` on insert and lookup, like `add_scan_root`.
- **I17 Swallowed `remove_file` results** — `restore.rs:546`, `:577-580`, `commands/archive.rs:589` (`delete_archive`). Windows sharing violations (Explorer preview, AV) are silent; `delete_archive` then deletes the DB rows → orphaned zip with no restore path. Violates never-swallow-errors. Fix: check + `warn!`/surface before deleting catalog rows.

### Export / calibration-library outputs

- **I18 `sanitize_display_folder_name` lacks all Windows rules; `..` escapes the export root** — `export/models.rs:242-268` (consumed `file_organizer.rs:285`, `data_collector.rs:1683`). Frame-set name is free user text: `..` → export lands in the PARENT of the chosen folder (all platforms); `CON`/`NUL`/`COM1` → opaque fatal `ERROR_INVALID_NAME` on Windows; trailing dot → silently renamed dir; interior control chars survive. Fix: route through the same tail as `archive::path_layout::sanitize_for_filename` + non-empty/not-dots fallback.
- **I19 WBPP export silently drops case-colliding frames and counts them as organized** — `file_organizer.rs:348-351` `if dest.exists() { return Ok(()); }`. Two lights differing only in filename case → second silently skipped on NTFS/APFS, `files_organized += 1`, success report. Fix: per-dir case-insensitive dedup (pattern exists at `api/sync.rs:2738`) + warn on unexpected pre-existing dest.
- **I20 MAX_PATH: no `longPathAware` manifest anywhere** — verified no `.manifest`/`.rc`/manifest key in `crates/athenaeum-tauri/`. Deep generated trees (light-cal `c_<original>.fits` under `<Library>/<OBJECT>/<INSTRUME>/<date>/`, archive staging `+~30` chars, restore temp `+~32`) plausibly cross 260; tmp-name suffix `.fits.tmp.<pid>.<seq>` (`fits_writer/writer.rs:51-56`) hits first; fails as `os error 3` per frame. Fix: ship a `longPathAware` manifest for the Windows bundle (and/or `\\?\`-prefix inside `write_fits_f32` on Windows).
- **I21 Atomic replace lacks a sharing-violation retry** — `fits_writer/writer.rs:73` (rebuild-in-place `api/masters.rs:951,956`; light-cal re-run `api/lights.rs:968-970`). Note: Rust's `fs::rename` on Windows DOES replace-existing (`MoveFileExW(MOVEFILE_REPLACE_EXISTING)`) — the real Windows-only failure is `ERROR_SHARING_VIOLATION` from AV/indexer/PixInsight holding the destination. Fix: bounded retry (≈5 × 50-200 ms backoff) on Windows.
- **I22 Non-ASCII → `?` in identity header cards breaks scanner adoption** — `fits_writer/card.rs:134-141` maps non-ASCII to `?` in `ATH_CSRN`/`ATH_C{DRK,FLT,BIA}`. A localized Windows profile path (`C:\Users\Вилен\…`) or non-ASCII filename → adoption branch 4 never matches (`warn!` every scan, deferred forever); `resolve_master_set_id` path fallback silently misses. FITS mandates ASCII — the fix is a reversible encoding (percent/base64url + version marker) for identity-bearing cards, lossy `?` only for display cards.

### Frontend

- **I23 Duplicates keep-rule "path contains" never matches on Windows and silently changes the deletion set** — `src/components/duplicates/keepRules.ts:100-105`. `includes` of `Backup/2023` against `C:\…\Backup\2023\…` fails; empty set = abstain → chain falls through to the NEXT rule → different deletion set than configured, feeding Black Hole. Fix: normalize both sides `replace(/[\\/]/g,'/')` before compare.
- **I24 Breadcrumbs drop the UNC `\\` prefix** — `DualPaneFileBrowser.tsx:2005-2016`. Verified: `\\nas\astro\Lights` → `nas\astro\Lights` (relative) → breadcrumb clicks fail. Backend intentionally emits UNC (`normalize_path` converts `\\?\UNC\…` → `\\…`); NAS is a core astro use case. Fix: preserve the leading separator run (reuse `getParentPath`'s UNC-correct approach from `types.ts:155-164`).

---

## Minor (grouped)

**Rust:**
- `dir_rename_prefixes` no trailing-sep trim (`api/files.rs:666-669`) — doubled separator on caller-supplied trailing slash.
- `native_separator_of` fails open to `\` for empty/relative input (`db/operations.rs:74-76`).
- Bare `canonicalize` without `normalize_path` — latent verbatim hazard, currently containment-only: `file_op/planner.rs:253,258`; `api/files.rs:390,628,704,723-724`; asymmetric fallback flips allow→deny on Windows web (`api/files.rs:390-395`) and reports "not inside any scan root" for nonexistent sources (`planner.rs:252-261` — move `exists()` check first).
- No intra-plan duplicate-destination detection (`file_op/planner.rs:140-162`) — caught late at execute, half-done op.
- `fs::rename` overwrite guards are TOCTOU (`executor.rs:225-230`, `api/files.rs:715,732`) — note std DOES overwrite on Windows, the pre-checks are the only protection on every platform.
- Windows sharing violations during source delete after cross-volume copy — already healed by `file_op::reconcile`; will be a Windows-frequent path (no change needed).
- Exact-case SQL path matching relies on "every writer canonicalizes" (`planner.rs:355`, `file_op/db.rs:348,429`, `db/operations.rs:3660`) — holds today; I4/C4/M-writers are the exceptions to close.
- `relink` updates `path` but not `filename` (`relinking/mod.rs:137-141`); poisons `get_camera_directories`' SUBSTR derivation (`db/equipment.rs:354`).
- `relocate_missing_file` stores the raw user path — no canonicalize/normalize/policy (`commands/missing_files.rs:361-364`; web mirror is a 501 stub).
- Duplicate groups transported as `'|'`-joined `GROUP_CONCAT` (`db/operations.rs:1617,1634`) — `|` is legal on POSIX; display-only split corruption.
- `browse_directories` `"/"` root sentinel displayed on Windows (`api/files.rs:773,786`).
- Non-UTF-8 scan classification vs ingestion split (`scanner/mod.rs:240,1667`) — honest per-scan re-reporting; filter once at discovery.
- Archive: `path_layout.rs:120` blanket `replace('\\', "/")` corrupts legal POSIX backslash filenames (cfg-gate it); in-zip entry components not Windows-sanitized (Linux-built archive can be unextractable on Windows — document or sanitize); `date_start.get(..10)` segment bypasses sanitizer (`:134,163`); `token()` emptiness check before sanitize → doubled underscore (`:46-53`); `cleanup_staging` failure fails `finalize_phase` after sources are deleted (make best-effort + `warn!`, `staging.rs:47-54` / `executor.rs:469`); `File::create` over a locked zip → sharing violation (Overwrite mode).
- Reserved-name guard misses `COM0`/`LPT0`/superscript variants (`path_layout.rs:30-33`).
- No component-length cap: NTFS 255 UTF-16 units vs ext4 255 bytes (`path_layout.rs:8-43`, `calibration_library/paths.rs`).
- `master_relative_path` interpolates `date` unsanitized, unlike the light path's `date_part()` (`paths.rs:82`).
- `c_<original_filename>` verbatim is unsafe for synced/foreign catalogs (POSIX-legal, NTFS-illegal chars) — sanitize (idempotent on clean names) (`paths.rs:109-121`).
- CONTINUE chunking can destroy a space at a 67-char boundary (`fits_writer/card.rs:202-231` / reader `trim_end`) — uuid-first lookup bounds the damage; fix by never ending a chunk on a space (or subsume under I22's encoding).
- `output_path_exists` byte-compare over a case-insensitive namespace (`db/light_calibrations.rs:341-348`) — mitigated in both live call paths via `Path::exists()`.
- `use_symlinks` accepted unconditionally by the Axum route (`routes/export.rs:84`) — non-elevated Windows web host → per-file error 1314 collected as warnings, export returns Ok with an empty tree; reject server-side on Windows + error when `files_organized == 0 && !warnings.is_empty()`.

**Frontend:**
- `evalShortestPath` ranks by char length → systematically prefers deleting the UNC copy (`keepRules.ts:125-135`); segment count first.
- `DuplicateGroupCard.tsx:49-55` `startsWith` without boundary (display).
- Root-path edge cases in basename/parent helpers: `BlackHole.tsx:125-128`, `MissingMetadataTable.tsx:6-10` (`C:\a.fits` → `C:`), `types.ts:155-170` (`getParentPath('C:\astro')`→`C:`; `parentPath('/data')` returns itself → duplicated label in `FolderRail.tsx`).
- `joinPath` sniffs separator via `includes('\\')` — POSIX dir with a literal backslash joins wrong (`types.ts:173-178`).
- `ExportTab.tsx:299-300` OS detection via `navigator.userAgent` — source platform from backend.
- Tail-truncation without `title=` tooltips: `Settings.tsx:1302,1316`, `FolderBrowserModal.tsx:167-169`, `ArchiveDispositionDialog.tsx:124,135-137`, `MonitoredInspector.tsx:68`, `ArchiveInspector.tsx:60`, `CatalogSearch.tsx:112`.
- GTK file-dialog filter case-sensitivity hides `.FITS` on Linux (`MissingFilesPanel.tsx:137-140`) — add uppercase variants.
- Exact-equality root matching in `FolderRail.tsx:91`, `ArchiveInspector.tsx:19` (case-variant re-add shows 0 sets).
- Six divergent basename impls + shadowed `splitPath` export — consolidate into `src/utils/path.ts`.
- Two different path sort orders (`BlackHole.tsx:121` vs `MissingMetadataTable.tsx:134`).
- Latent: `useTauri.ts:352-355` passes snake_case `directory_path` (no callers today).

---

## Verified OK — do not "fix"

- **Byte-range prefix machinery is correct for `\`** — `path_prefix_upper` (`db/operations.rs:38-67`) is pure lexicographic prefix-successor; the byte after the prefix never matters, `\` (0x5C) sorting mid-ASCII is a non-issue. Boundaries test-pinned incl. Windows-shaped fixtures. All 81aedae7 sites (`delete_scan_root`, `delete_calibration_sets_for_root`, `reconcile_unique_camera_instrume`, `frame_ids_under_paths`, `rename_files_path_prefix`) verified separator-strict with content-derived separator.
- **SQLite `LENGTH`/`SUBSTR` are char-based and consistent with Rust `chars()`-based prefix building** — non-ASCII rename swap verified safe.
- **ZIP entry names**: single mint point `path_layout::path_in_zip` normalizes to `/` before storage; `zip` crate `start_file` stores verbatim (checked vendored 8.6.0); read-back symmetric via the same stored string. Windows will not write `\` entry names.
- **`sanitize_for_filename`** (`path_layout.rs:8-43`) is genuinely thorough: 9 forbidden chars, control-char drop, trailing dot/space trim, reserved-name defusal on the pre-first-dot token, case-insensitive, test-pinned.
- **file_op path construction** is `PathBuf::join`/component-based throughout; mkdir/rename inputs reject both separators; frontend dual-pane helpers detect separators per-path.
- **No `/Volumes`↔`/private/Volumes` special-casing exists** — the property is structural (planner stores and executor matches the scanner's non-canonicalized spelling). Correct design; do not add canonicalization.
- **Monitor is a poller by explicit design** (NAS reliability), no watcher-backend quirks. Perseus's `notify` watcher handles the platform matrix correctly (events seed, periodic sweep is truth).
- **Extension matching case-insensitive everywhere** (scanner, relinking, register, lights, sync ingest); frontend gates on backend `file.format`, not extensions.
- **`PathPolicy::check` semantics** are component-wise `Path::starts_with` with canonicalize-both-sides — only the web-layer verbatim/normalize mismatch (I10) breaks it.
- **`fs::rename` on Windows replaces existing** (std maps to `MOVEFILE_REPLACE_EXISTING`/`FileRenameInfoEx`) — the "fails if dest exists" folklore is wrong for Rust; plan around sharing violations instead. Std also handles the read-only attribute on delete (`posix_delete` fallback on `ACCESS_DENIED`).
- **`reconcile_calibrated_light` branch 2b** canonicalizes both spellings — correct same-file-two-names handling incl. Windows case/trailing-dot.
- **Wire `rel_path`** is `/`-separated by contract end-to-end (sync, transfers FileTree, WBPP placements) with platform-independent backslash/drive/UNC rejection, test-pinned.
- **`add_scan_root`** canonicalize+`normalize_path`+component-wise overlap checks; `upsert_scan_root` converges case-variant spellings via canonicalize + `UNIQUE(path)`.

## Stale documentation noted en route

- CLAUDE.md: the Delete pipeline (`enqueue_delete_operation`, `MoveStrategy::Delete`, deepest-first rmdir, `OperationKind::FileOpDelete`) no longer exists — user-facing delete is Black Hole; real `OperationKind` variants are `ZipArchive`, `FileOpMove`, `FileOpReconcile`. The "`/Volumes` vs `/private/Volumes` edge cases" survival is structural, not special-cased. **Resolved** in the Task 18 doc truth-up (see Status below); `list_unfinished_file_operations` was also stale and was removed in the same pass. Note the `MoveStrategy::Delete` and `FileOpKind::Delete` *variants* do still exist in `file_op/models.rs` — they are unreachable (planner never emits them, `executor::run_operation` rejects `kind='delete'` loudly), which is how CLAUDE.md now describes them.
- `archive/planner.rs:612-617` `available_disk_space` always errors by design → the "insufficient disk space" guard is dead code on every platform (behavioral, not platform). Still open after this cycle.

## Proposed fix grouping (for the plan)

1. **LIKE → byte-range sweep** (C1, C2, I1 + `verify_files_at_location`): mechanical substitution to the standardized helper, Windows-shaped sibling tests per site, both backends for missing-files.
2. **Path-spelling hygiene at write boundaries** (C4, I9, I10, I16, minor writers `relocate_missing_file`, lossy relink): `normalize_path` everywhere a canonicalized path escapes or is stored; export the helper.
3. **Restore/archive Windows correctness** (C3, I13, I14, I15, I17): component-wise suggestion strip, `Path::starts_with` + case-fold, component-wise join, planner root match, honest deletes.
4. **file_op behavior** (I6 EXDEV downgrade, I7 case-only rename, I8 zero-row warn, I3/I4 separator derivation + trailing trim).
5. **Frontend** (I23 keep-rule, I24 UNC breadcrumbs, consolidated `src/utils/path.ts`, minors).
6. **Export/light-cal Windows hardening** (I18 sanitizer, I19 case-collision dedup, I20 longPathAware manifest, I21 rename retry, I22 reversible identity encoding).
7. **Minors sweep + CLAUDE.md refresh** (stale Delete pipeline, dead disk-space guard note).
8. **One-time gate**: run `cargo check --workspace` on a Windows runner to convert the by-inspection compile claim into a compiler fact (local cross-check dies in `ring`'s MSVC build).

---

## Status (2026-07-30 fix cycle)

Plan: `docs/superpowers/plans/2026-07-30-cross-platform-path-fixes.md` (19 tasks). All 4 Critical and all 24 Important findings are fixed on branch `0.5.1`, commits `a1040617..b3634a78`. The Minor group was swept selectively. *Deferred* below is the true residual ledger: it carries both the items deliberately queued out of this cycle **and** the remainder of the Minor group, which was left untouched — it is not a claim that everything else in Minor was fixed.

### Critical

| Finding | Status | Commit(s) |
| ---- | ---- | ---- |
| C1 — `relink_files` sweeps sibling roots | Fixed | `a1040617` |
| C2 — `recreate_calibration_sets_for_root` groups sibling roots | Fixed | `0ac0f047` |
| C3 — restore-to-original unreachable on Windows | Fixed | `f205a04e` |
| C4 — `relink_scan_root` persists `\\?\` verbatim paths | Fixed | `9e0fefa1` |

### Important

| Finding | Status | Commit(s) |
| ---- | ---- | ---- |
| I1 — `find_missing_files` unbounded `LIKE`, both backends | Fixed | `ddea802f` |
| I2 — `get_folder_overview` trailing-separator root → 0 rows | Fixed | `aed53f1a` |
| I3 — `get_files_by_directory{,_for_camera}` build-OS separator | Fixed | `aed53f1a` |
| I4 — `native_separator_of` sniffs only the first char | Fixed | `c45a1b6a` |
| I5 — `enrich_duplicate_groups` bare `starts_with` attribution | Fixed | `aed53f1a` |
| I6 — same-device ≠ rename-works (EXDEV) | Fixed | `c45a1b6a` |
| I7 — case-only rename rejected on Windows/macOS | Fixed | `c45a1b6a` |
| I8 — move hot-sync silently succeeds at zero rows | Fixed | `c45a1b6a` |
| I9 — `browse_directories` returns `\\?\C:\…` to the frontend | Fixed | `9e0fefa1` |
| I10 — `PathPolicy` denies everything on a Windows-hosted web build | Fixed | `9e0fefa1` |
| I11 — `relink_files` stores lossy (U+FFFD) paths | Fixed | `a1040617` |
| I12 — relink WalkDir lacks the scanner's `.max_depth(64)` cap | Fixed | `a1040617` |
| I13 — `classify_target` raw string `starts_with` | Fixed | `ae507068` + `11860a98` |
| I14 — mixed separators persisted on restore | Fixed | `ae507068` + `11860a98` |
| I15 — case-sensitive planner root match → flattened zip → hash abort | Fixed | `ae507068` + `11860a98` (fix round: fold-aware `path_in_zip` strip + per-operation collision guard) |
| I16 — `archive_roots` matched by exact SQL string | Fixed | `6e036ba5` |
| I17 — swallowed `remove_file` results | Fixed | `46ae5ddc` + `9fe5f881` |
| I18 — `sanitize_display_folder_name` lacks Windows rules; `..` escapes | Fixed | `ca860ed0` (F8 date-token bypass fully closed in `2e489184`) |
| I19 — WBPP export silently drops case-colliding frames | Fixed | `f69d57f2` |
| I20 — MAX_PATH: no `longPathAware` manifest | Fixed | `0f412913` |
| I21 — atomic replace lacks a sharing-violation retry | Fixed | `d01c85f2` + `f6d9974a` (`stamp.rs` too) |
| I22 — non-ASCII → `?` in identity header cards breaks adoption | Fixed | `2970e24c` |
| I23 — duplicates keep-rule "path contains" never matches on Windows | Fixed | `2ef09f57` |
| I24 — breadcrumbs drop the UNC `\\` prefix | Fixed | `b3634a78` |

### Deferred

Carried out of this cycle deliberately — none is a correctness regression, each is scoped work with its own trade-off:

- Frontend platform-detection for the symlink checkbox (source platform from the backend, not `navigator.userAgent`).
- `use_symlinks` server-side reject on a Windows web host (+ error when `files_organized == 0 && !warnings.is_empty()`).
- Path-util consolidation into `src/utils/path.ts` (six divergent basename impls, shadowed `splitPath` export).
- Duplicate groups transported as `'|'`-joined `GROUP_CONCAT` — `|` is legal on POSIX.
- Non-UTF-8 discovery filter (filter once at discovery instead of per-scan re-reporting).
- Component-length caps (NTFS 255 UTF-16 units vs ext4 255 bytes).
- `path_in_zip` blanket `replace('\\', "/")` — needs a `cfg`-gate so legal POSIX backslash filenames survive.
- In-zip Windows sanitization so a Linux-built archive extracts on Windows with foreign tools.
- Staging path-length trims.
- `browse_directories` `"/"` root sentinel displayed on Windows.
- TS path sort unification (`BlackHole.tsx` vs `MissingMetadataTable.tsx`).
- Case-folded root equality in the Folders UI (`FolderRail.tsx`, `ArchiveInspector.tsx`).

Remainder of the Minor group — not swept, no work started:

- `native_separator_of` fails open to `\` for empty/relative input (`db/operations.rs:74-76`). The related `C:/`-spelled mixed-separator blind spot (audit item I4/T2) is now PARTLY covered: `normalize_separators` folds the spelling at the rename boundary, so that entry point is handled — every other caller of the sniffer still is not.
- `is_inside_any` reports "not inside any scan root" for nonexistent sources — `exists()` check ordering (`file_op/planner.rs:252`).
- No intra-plan duplicate-destination detection (`file_op/planner.rs:140-162`) — two plan rows targeting one destination are caught late at execute, leaving a half-done operation.
- `relocate_missing_file` stores the raw user path (separator-folded only; no canonicalize, no `normalize_path`, no `PathPolicy` check).
- `master_relative_path` interpolates `date` unsanitized (`calibration_library/paths.rs:82`), unlike the light path's `date_part()`.
- `c_<original_filename>` used verbatim (`calibration_library/paths.rs:109-121`) — POSIX-legal, NTFS-illegal characters survive from a synced or foreign catalog.
- `File::create` over a locked zip in Overwrite mode → sharing violation (`archive/zip_writer.rs`).
- `output_path_exists` byte-compares over a case-insensitive namespace (`db/light_calibrations.rs`) — mitigated in both live call paths by `Path::exists()`.
- `DuplicateGroupCard.tsx:49` `startsWith` without a component boundary (display only).
- `types.ts` path-helper root edges — `getParentPath('C:\astro')` → `C:`, `parentPath('/data')` returns itself, `joinPath` sniffs the separator via `includes('\\')`. T17 fixed only the `BlackHole.tsx` / `MissingMetadataTable.tsx` basename helpers; `types.ts` was left as-is.
- `useTauri.ts:352` passes snake_case `directory_path` (latent — no callers today).

Discovered during the cycle, deferred with it:

- OBJECT disambiguator is still `?`-mangled — needs a lenient scanner compare (I22's reversible encoding covers the identity cards, not this one).
- `delete_archive` should be hoisted out of the command layer into `core::api`.
- `archive_roots` row back-fill migration, plus a verbatim writer for `migrate_legacy_archive_root`.
- `relink_files` wants an enclosing transaction.
- `stamp.rs` fixed tmp suffix is concurrency-unsafe.

### Release-note lines owed

- Pre-fix `missing_files` rows clear on re-run (sibling-root false positives were persisted).
- Exact-case matching is a safer **under**-report, not a silent rewrite — expect fewer, not more, matches.
- Re-export naming migration for frame-set names with dots or Windows reserved names (folder names change).
- On Linux, plans over case-only-distinct roots are now refused loudly instead of merging.
- Duplicate keep-rule auto-delete sets may shift now that "path contains" actually matches on Windows — re-review before running.
- Pre-existing `?`-mangled calibrated-light headers are unrecoverable until those frames are re-calibrated.
- Windows long paths need the OS policy `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled=1` **in addition to** the shipped manifest.
- Moves that previously aborted on Linux bind mounts / Windows folder-mounted volumes now succeed via copy-verify-delete (slower per file).
- Deleting an archive now fails loudly if a zip can't be removed (catalog rows kept) instead of orphaning the zip.
- Case-only renames (`m31.fits` → `M31.fits`) now work on Windows/macOS.
- On Linux, an archive plan with two files whose in-zip paths differ only by case is now refused (zip portability).

### Still open from the plan

- Task 19 / audit item 8: `cargo check --workspace` on a Windows runner, to turn the by-inspection compile claim into a compiler fact.
- `archive/planner.rs` `available_disk_space` always errors by design → the "insufficient disk space" guard remains dead code on every platform (behavioral, not platform-specific; untouched by this cycle).
