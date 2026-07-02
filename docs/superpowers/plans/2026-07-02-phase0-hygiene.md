# Phase 0 — Hygiene & Guards: Implementation Plan — 2026-07-02

Expansion of Phase 0 from `2026-07-02-roadmap.md` into implementable tasks. Fourteen tasks (T1–T14 + exit checks T10), ordered by severity × cheapness; T1, T3, T8 touch the `solvemyastro`/`rustafits` submodules (one branch + one bump commit each in the superproject), the rest touch the main repo. T11–T14 come from the platform/web-parity audit (`2026-07-02-platform-parity-audit.md`). No schema migrations anywhere in this phase. Total shape: ~2 weeks.

Verified against `main` v0.2.1 on 2026-07-02; all line refs re-checked today.

---

## T1 — R1: detect meridian-flip (negative determinant) in `register()`

**Where:** `solvemyastro/src/register.rs` — `pub fn register()` (line 140), det computed at line ~323 in Step 4 (`CRPIX' = M⁻¹(CRPIX_ref − t)`).

**Problem:** Only `det_m.abs() < 1e-12` (singularity) is checked. A meridian-flipped sub yields `det(M) < 0` — a mirror transform. It registers "successfully"; the composed WCS is a valid mirror mapping, but any future pixel resampling silently produces mirrored data, and mosaic footprint math inherits wrong orientation.

**Change:**

1. In Step 3 (inlier refit) or Step 4, after computing `det_m`: if `det_m < 0.0`, set a flag instead of proceeding silently.
2. Add `pub flipped: bool` to `pub struct Registration` (line 45). Semantics: transform includes a reflection; consumer decides.
3. Decision (recommend **flag, not reject**): flipped frames are legitimate (post-flip halves of a session). Rejecting throws away half the night. The stacking pipeline later handles flips at resample time; for now the flag must reach the DB.
4. Athenaeum side: `registration/service.rs` — persist into `registration_results` (`status` suffix or a dedicated column; **no new column in Phase 0** — encode as `status = "ok_flipped"`), surface in the frame-set registration UI as an info badge, and `eprintln!` a log line per flipped frame.

**Tests:** unit — synthetic detections, mirror half of them (`x → −x`), assert `flipped == true` and RMS still small; assert non-mirrored case gives `flipped == false`. Athenaeum e2e — status round-trips to `registration_results`.

**Effort:** half a day. **Submodule bump required.**

## T2 — R2: scale/binning consistency gate before frame-set registration

**Where:** `crates/athenaeum-core/src/registration/service.rs` — before the reference-solve loop (members are loaded as `MemberInfo { frame: Frame, … }`, line 67; `Frame` carries `xbinning`, `ybinning`, `focallen`).

**Problem:** `CD' = CD_ref · M` composition assumes every sub shares the reference pixel scale. Mixed binning (1×1 + 2×2) in one frame set composes a scale-wrong WCS with no warning (`ref_pixel_scale` is a single value, line 343).

**Change:** After loading members, group by `(xbinning, ybinning)` (NULL treated as 1×1 with a logged assumption). If more than one group: `bail!` with a message listing group sizes and example filenames — e.g. `"registration: frame set mixes binning 1x1 (42 frames) and 2x2 (8 frames); registration requires uniform binning — split the set or exclude the minority"`. Also compare `focallen` across members with a relative tolerance (±1%; NULL passes with log) — same bail shape. Emit the failure through the existing `stacking-prep-progress` error path so the UI shows it, per the never-swallow-errors rule.

**Deliberately NOT doing:** auto-rescaling minority frames (that's stacking Phase B territory).

**Tests:** core test with two fake members 1×1/2×2 → error mentions both groups; uniform set passes; NULL binning passes with 1×1 assumption.

**Effort:** half a day. No submodule change (gate lives in athenaeum-core).

## T3 — Mutex-poison hardening + naming policy in submodules

**Where:**
- `solvemyastro/src/orchestrate.rs:1373, 1382, 1420` — `cells_mutex.lock().unwrap()` inside `par_iter` workers (CellTrace diagnostics push).
- `rustafits/src/platesolving/pattern_matcher.rs:1, 3` — doc comments name the external solver codebase (CLAUDE.md naming rule).

**Change:**
- Replace all three with `cells_mutex.lock().unwrap_or_else(|e| e.into_inner())` — the guarded data is a diagnostics `Vec<CellTrace>` push; recovering a poisoned lock is strictly better than cascading panics across the rayon pool. Grep the rest of the crate for `.lock().unwrap()` while there (`rg '\.lock\(\)\.unwrap\(\)' src/`) and apply the same treatment to any diagnostics-path hit.
- Reword the two doc lines to "quad-based pattern matching (nearest-neighbour quad descriptors)" — no behavioural change. Grep both submodules case-insensitively for the name to catch stragglers.

**Optional rider (T3b) — rustafits S3 input hardening:** `formats/fits.rs:99` accepts `NAXIS3 > 3` while downstream assumes 1 or 3 channels (June audit S3, the only malformed-input item among S3–S7). While in the file: reject `NAXIS3 ∉ {1, 3}` with a clear error. The remaining June rustafits items stay deferred by design: S4 (`detection.rs:38` saturation gate dead for float domain) and S5 (`analysis/mod.rs:1211` fixed `init_sigma`) belong with the stacking-era analysis precision work; S7 (fast-detect coverage on dense fields) with the centroid-refinement work.

**Tests:** none needed beyond `cargo test -p` in each submodule (T3b: a malformed-NAXIS3 fixture test).

**Effort:** ~1 hour (+1 h for T3b). **Two submodule bumps** (can share the T1 solvemyastro bump).

## T4 — M2: hash-verify the restore skip-if-exists path

**Where:** `crates/athenaeum-core/src/archive/restore.rs` — reconcile stage, `already_in_place = original.is_file() && !overwrite_existing` (~line 238). Note the *extract* stage already hash-verifies zip → temp (line 205); the gap is only the *reconcile* skip.

**Problem:** Any file sitting at `source_path` — wrong version, different frame, half-written copy — is silently blessed as "restored"; archive markers are then cleared, and the only correct copy may later be deleted with the zip.

**Change:** When `original.is_file() && !overwrite_existing`:

1. `compute_xxhash(original)` (`duplicates/mod.rs:14` — same sampling hash used to produce `expected_hash`; consistent by construction).
2. Match → skip as today (emit "already on disk, verified").
3. Mismatch → **do not overwrite, do not skip-and-clear-markers.** Record the file as conflicted: fail the file (reuse the operation-file disposition machinery), continue the loop, and finish the operation as `CompletedWithErrors` listing conflicted paths. The user resolves by renaming/removing the impostor and re-running restore (restore is already reconcile-based and idempotent — re-run fills exactly the gaps).
4. Hash error (unreadable file) → treat as mismatch, same path.

Also emit a `notify`-able summary through `archive-finished` (`outcome` already exists; make sure the conflicted count reaches the payload).

**Tests:** core test — archive → replace one source file with different bytes → restore → assert: file NOT overwritten, operation reports 1 conflict, archive markers for that file NOT cleared; matching file → skipped and markers cleared as before.

**Effort:** 1 day (test plumbing is most of it).

## T5 — M4: reconcile abandoned cross-volume moves

**Where:** `crates/athenaeum-core/src/file_op/executor.rs` — `run_cross_volume_commit_step` (line ~499): after copy+verify+catalog-sync succeed, `fs::remove_file(source)` failure marks the step `Failed` and bails, leaving BOTH copies on disk with the catalog pointing at dest.

**Problem chain (verified):** scanner move-detection (`scanner/mod.rs:369-385` and `1309-1346`) matches the leftover *source* by header fingerprint against the row whose `path` = dest and **flips `files.path` back to source** — the verified dest copy becomes an invisible disk orphan.

**Change — two complementary halves:**

1. **Scanner guard (cheap, closes the corruption):** in both move-detection sites, before updating `files.path`, `stat` the row's current path (`old_path`). If the file at `old_path` still exists, this is a *duplicate*, not a move — do NOT flip the path; log `"duplicate content at '{}' and '{}' — keeping catalog at existing path"` and let the normal duplicate machinery see it.
2. **Retry on resume (fixes the orphan):** `list_unfinished_file_operations` / resume already exist for file ops. Extend resume handling for a `CommitMove` step in `Failed` state where dest exists, hash matches (`compute_xxhash` vs the step's recorded hash from the verify step), and catalog points at dest: re-attempt `fs::remove_file(source)`; on success mark step `Done`. Surface remaining failures in the unfinished-operations UI instead of silently abandoning.

**Tests:** (1) simulate both-copies-exist + scan of source → path stays at dest, no flip; (2) resume a fabricated failed CommitMove → source removed, step Done. Use a real FITS fixture for the fingerprint path (real-data-first rule).

**Effort:** 1–1.5 days. The scanner guard is the priority half if time-boxed.

## T6 — Dead-command detection & cleanup

**Problem (verified 2026-07-02):** ~13 `#[tauri::command]` functions have **0 frontend references and no web route** — dead or lost features. Known examples: `greet` (template leftover; also delete the stale `#greet-input` CSS in `src/App.css:94`), a cluster of superseded calibration commands in `commands/calibration.rs`, `get_orphaned_files`/`delete_orphaned_files`, `check_scan_root_availability`.

**Task — re-detect, don't trust this list:** extract all `#[tauri::command]` fn names, cross-check each against (a) frontend usage (`rg "'<name>'" src/`), (b) web routes (`crates/athenaeum-web/src/routes/`, including stubs in `routes/mod.rs`), (c) `invoke_handler` registration. For each command with zero frontend refs, classify:

- obvious leftovers (`greet`) → delete (fn + `invoke_handler` registration + `commands/mod.rs` re-export);
- possibly lost features → **do not delete silently**; list them in the PR description for the owner. Watch for `clear_manual_calibration_override` — it's documented in `docs/masters/masters.md` as part of the manual-linking design, so its absence from the UI may be a regression, not dead code.

Also: verify `MissingFilesPanel` handles the web-mode 501 from `relocate_missing_file` gracefully (the stub at `routes/mod.rs:313` is intentional).

**Effort:** half a day.

## T7 — Migrate 3 raw BEGIN/COMMIT pairs to savepoints

**Where:** `crates/athenaeum-core/src/db/operations.rs` — `reconcile_unique_camera_instrume` (BEGIN 298 / COMMIT 356), `delete_scan_root` (400/498), `rebuild_duplicate_groups_cache` (1557/1634).

**Problem:** Raw `BEGIN` errors if a transaction is already open on the connection (the exact H2 scanner bug class from June); these three are why the pool's defensive checkout-rollback exists.

**Change:** Follow the established pattern — either `conn.unchecked_transaction()?` with RAII commit (already used at operations.rs:2055, 2107) or a named `SAVEPOINT`/`RELEASE` pair like `scanner/mod.rs:630` (`reparse_in_place`) when the function may be called inside an open transaction. Check each call site to pick: if only ever called from autocommit, `unchecked_transaction` is cleaner; if callable nested, savepoint. Ensure every early-return path rolls back (RAII guard preferred over manual ROLLBACK).

**Tests:** wrap each function in an outer `unchecked_transaction` in a test and assert no "cannot start a transaction within a transaction" error; existing behaviour tests stay green.

**Effort:** half a day.

## T8 — R3: parameterize the registration inlier tolerance by pixel scale

**Where:** `solvemyastro/src/register.rs:207` — `const INLIER_TOL_PX: f64 = 4.0` (the comment already anticipates this: "Phase 2 may parameterise this via SolveConfig").

**Change:** Add `register_inlier_tol_arcsec: Option<f64>` to `SolveConfig` (default `None` = legacy 4 px). In `register()`, when set, derive `tol_px = tol_arcsec / pixel_scale` from `reference_wcs` CD-matrix scale, clamped to `[1.0, 12.0]` px. Athenaeum passes a settings-driven value later; Phase 0 only opens the API (call sites keep `None`).

**Tests:** unit — tol derived correctly from a known CD matrix; `None` reproduces current behaviour bit-for-bit on the existing register tests.

**Effort:** ~2 hours (rides the T1/T3 submodule bump).

## T9 — Web-mode degradation check for `relocate_missing_file`

Folded into T6 last bullet — listed separately in the roadmap; keep as one checklist item: web build → Missing Files panel → relocate action either hidden (`VITE_TARGET=web`) or shows the 501 message via `notify({ tone: 'warning' })`, not a silent failure.

## T11 — Fix path-prefix `LIKE` queries (case sensitivity + wildcard escaping)

**Where:** `crates/athenaeum-core/src/db/operations.rs:227, 274, 310, 324, 350, 404, 659, 766, 1427` (see `2026-07-02-platform-parity-audit.md` §1 for the full analysis).

**Problem:** SQLite `LIKE` is ASCII-case-insensitive by default (no `case_sensitive_like` pragma anywhere) and `%`/`_` in the prefix argument are live, unescaped wildcards. On Linux, paths differing only by case cross-match; `_` (ubiquitous in astro paths, `M31_Ha/`) matches any character. Both cause silent wrong-row updates in the directory-rename hot-sync and scan-root cascades.

**Change:** one shared helper (e.g. `db::path_prefix_where(column, prefix)`) producing a range predicate `column >= ?prefix AND column < ?prefix_upper` (prefix with last byte incremented — keeps the `schema.rs:471` index, exact case, no wildcards); migrate all nine sites. Grep for any other `LIKE` fed by a path while there.

**Tests:** two dirs differing only by case → rename touches only the exact one; prefix containing `_` and `%` matches literally; existing rename/cascade tests green.

**Effort:** half a day.

## T12 — Rewrite stale `docs/export/README.md`

The Siril script pipeline it documents (script generator, cli_runner, execution modes) **no longer exists in the codebase** (`export/` = data_collector, file_organizer, models only). Rewrite around the current WBPP folder/keyword export (`WbppExportConfig`, `export/models.rs:691-741`). Doc-only; prevents future work being planned against removed code.

**Effort:** ~1 hour.

## T13 — Optional API-key auth for athenaeum-web

**Where:** `crates/athenaeum-web/src/main.rs` (router assembly).

**Problem:** athenaeum-web has zero authentication (verified: no key/token/middleware anywhere) while exposing move/delete/browse. Acceptable on a trusted LAN, but there is no opt-in protection at all (`2026-07-02-platform-parity-audit.md` §2).

**Change:** `ATHENAEUM_API_KEY` env var; when set, an Axum middleware layer requires `X-API-Key` header (or `Bearer`) on `/api/*` **and** the SSE endpoint (query-param fallback for EventSource, which can't set headers). Unset = current open behaviour. Frontend web target: read key from a login prompt once, keep in memory, attach in `src/api/web.ts` fetch/SSE wrappers. Document in docker compose examples.

**Tests:** with key set — 401 without header, 200 with; without key — unchanged. **Effort:** half a day.

## T14 — Web-mode `relink_scan_root` (path-input variant)

**Where:** `crates/athenaeum-web/src/routes/scan_roots.rs:415` (currently 501).

**Problem:** desktop relink uses a native folder picker; web stubs it. Docker users can't re-point a moved scan root at all, and collaboration Stage 2 (portable paths) makes relink the primary recovery flow.

**Change:** implement the route accepting `new_path: String` in the JSON body instead of opening a picker: validate against `ATHENAEUM_ALLOWED_PATHS` (same check as `add_scan_root`), then call the same core relink logic the Tauri command uses. Frontend: in web mode, replace the picker with a text input + existing `browse_directories` route. Remove the stub.

**Tests:** relink within allowed paths succeeds and files resolve; path outside allowed paths → 403. **Effort:** half a day.

## Watch items (from the platform audit — no Phase 0 code change)

- **WBPP export uses symlinks on unix** (`export/file_organizer.rs:363`): breaks if the export root is later moved cross-device or shared via Syncthing/Docker volume. Revisit in pillar C — add a "materialize copies" export option then. Verify what the Windows branch does while touching T12.
- **Windows is the least-exercised platform**: cfg gates are correct and volume detection is sound, but there is no Windows-specific test coverage for file_op/archive path edges (drive letters, UNC, `\\?\`). Add a Windows CI test job when the dev-test-harness plan (`2026-06-10-dev-test-harness.md`) is picked up.

## T10 — Phase-0 exit checks

- [ ] `cargo test --workspace` + submodule test suites green.
- [ ] Both submodule bumps committed in the superproject with matching lockfile.
- [ ] `rg -i` for the external-solver name across all crates/submodules → 0 hits in code/comments.
- [ ] Command count reconciliation: re-run the parity extraction from the audit (`#[tauri::command]` fns vs web route fns) and update the numbers in `2026-07-02-v0.2.1-audit.md` §2.1.
- [ ] Update `2026-06-10-architecture-audit-findings.md` open-findings table: move R1, R2, R3, M2, M4, raw-BEGIN to fixed with commit refs (keeps the "don't re-discover" contract of that doc).
- [ ] `rg -n 'LIKE' crates/athenaeum-core/src/db/` → no path-fed LIKE remains (T11 complete).
- [ ] Web build smoke: with `ATHENAEUM_API_KEY` set, unauthenticated `/api/*` and SSE → 401; relink via path input works within allowed paths (T13/T14).

---

## Sequencing

```
T1 ─┐
T3 ─┼─ one solvemyastro branch/bump ── T8 (same bump)
    │
T2 ──── athenaeum-core, independent
T4 ──── independent
T5 ──── scanner guard first, resume half second
T6/T9 ─ needs owner decisions; mechanical part anytime
T7 ──── independent
T11 ─── independent (highest-value main-repo fix)
T12 ─── doc-only, anytime
T13 ─── independent (web)
T14 ─── independent (web)
T10 ─── last
```

Suggested order for a single developer: T3 → T1 → T8 (one submodule pass) → T11 → T2 → T7 → T4 → T5 → T13 → T14 → T6/T9 → T12 → T10. Nothing here blocks Phase 1 (foundation) from starting in parallel except shared review bandwidth.
