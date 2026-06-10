# Architecture Audit Findings — 2026-06-10

Record of a four-track deep audit (data layer/concurrency, precision pipeline, testability, targeted bug hunt). High-severity scanner findings were fixed the same day on `feature/stacking-prep`; everything else is tracked here so it isn't re-discovered (or re-flagged) later.

Companion plans produced from this audit:

- `2026-06-10-dev-test-harness.md` — dev-loop testing (highest leverage)
- `2026-06-10-collaboration-readiness.md` — schema groundwork for sync/collab
- `2026-06-10-stacking-engine-roadmap.md` — pixel pipeline to integration

---

## Fixed in this audit (scanner, commit on `feature/stacking-prep`)

| ID | Severity | Finding | Fix |
| ---- | ---- | ---- | ---- |
| H1 | High | `reparse_and_update_in_place` wrote `override = 0` + raw header values unconditionally, silently destroying user metadata edits whenever size/mtime drifted (archive→restore round-trip, touch, clock skew). Violated the `frames.override` contract. | Read existing `override` first; when 1, refresh only `files` (size/mtime/hashes) + `fits_header` snapshot, leave `frames` untouched. |
| H2 | High | `scan_directory_parallel` opened `BEGIN TRANSACTION`, then called `reparse_and_update_in_place` on the same connection → nested `BEGIN` via `unchecked_transaction()` → SQLite error on **every** modified-file re-parse. The feature was dead on the production path (`run_registered_scan` = manual scan + monitor). | Replaced the inner `BEGIN` with a `SAVEPOINT reparse_in_place` (nests inside an open transaction, acts as a plain transaction in the serial autocommit path). |
| H3 | High | `result.errors = phase1_errors` at the end of `scan_directory_parallel` **overwrote** write-loop errors (including every H2 failure) — which is why the regression test passed for the wrong reason and H2 went unnoticed. | `extend` instead of assign (both the cancel branch and the final collection). Existing test strengthened to assert `files_processed` and a refreshed `files.modified_at`, so it can no longer pass via silent failure. |

Lesson recorded: an "errors are empty" assertion is worthless if the code can drop errors on the floor. Assert on positive effects (row actually updated), not only on absence of errors.

## Open findings — worth fixing (ordered)

| ID | Severity | Location | Finding |
| ---- | ---- | ---- | ---- |
| R1 | High (future stacking) | `solvemyastro/src/register.rs` (~line 323) | Meridian-flipped frames register successfully with `det(M) < 0` — a mirror transform. Only `abs(det) < 1e-12` is checked. Silent data corruption once pixels are resampled. Add a negative-determinant check: either flag `flipped: true` in the result or reject with a clear message. |
| R2 | High (future stacking) | `solvemyastro/src/register.rs` + `athenaeum-core/src/registration/` | Mixed binning / pixel scale within a frame set composes a scale-wrong WCS (`CD' = CD_ref · M` assumes a shared reference CD). Add a scale-consistency gate on the frame set before registration. |
| M2 | Medium | `athenaeum-core/src/archive/restore.rs:236-248` | Restore skip-if-exists accepts whatever file sits at `source_path` without comparing to the stored `expected_hash`; archive markers are then cleared. A wrong file is silently blessed as "restored". Compare hashes; on mismatch surface a conflict (don't overwrite silently either). |
| M4 | Medium | `athenaeum-core/src/file_op/executor.rs:499-532` | Cross-volume move: copy+verify+catalog-update succeed, then source-delete fails and the operation is abandoned → next scan's fingerprint-based move detection flips `files.path` back to source, leaving the dest copy as an invisible disk orphan. Needs a reconciliation path (re-list unfinished ops, or make move-back also remove the dest copy). |
| R3 | Low | `solvemyastro/src/register.rs` (~line 207) | `INLIER_TOL_PX = 4` hardcoded regardless of pixel scale (too loose at 0.5"/px, too tight at 2"/px). Parameterize via config. |
| R4 | Low | `solvemyastro/src/sip.rs` | No coordinate normalization to [-1, 1] before SIP least-squares (standard practice in TWEAK/SCAMP). Conditioning floor for order ≥ 3 on large sensors. Not urgent at order ≤ 3. |
| S1 | Low | `athenaeum-core/src/clustering/mod.rs:279-334` | Greedy O(n²) clustering with the full frame list in memory. Fine at current catalog sizes; revisit past ~20k lights. |
| S2 | Low | `athenaeum-core/src/db/calibration_links.rs:1031+`, `db/operations.rs:517+` | Unbounded list queries materialize full result sets (no pagination). Same trigger: revisit at scale. |

## Confirmed-intentional (do NOT re-flag)

| Topic | Decision |
| ---- | ---- |
| Sampling xxHash (first/middle/last 512 KB) used for cross-volume move verification (`duplicates/mod.rs`, `file_op/executor.rs`) | **Intentional.** Owner accepts the sampling trade-off for speed; `verify_byte_identical` exists for callers that want a full check. |
| Calibration `Warning`-mode matching passes when CCD-TEMP is NULL on either side (`calibration/configurable_matcher.rs:185-210`) | **Intentional.** Frames without temperature data should match rather than be excluded. |

## Architecture assessment (summary)

**Sound:** transport-agnostic core (`ProgressEmitter`, `ServiceContext`), r2d2 pool (max 8) + WAL + 5 s busy timeout, defensive transaction-leak rollback on checkout, registration math (see stacking roadmap doc).

**Structural debts (not bugs):**

- ~120 hand-duplicated Tauri-command / Axum-route pairs; no shared helper, drift caught only by discipline.
- Hand-maintained TS mirrors of Rust models (`src/types/models.ts`); serde/enum drift fails silently at runtime. → ts-rs codegen, see collaboration plan.
- Schema has no portable IDs / change tracking. → collaboration plan.
- Two raw `BEGIN`/`COMMIT` sites remain in `db/operations.rs` (the reason for the defensive checkout rollback); migrate to savepoints/`unchecked_transaction` when touched.
- SSE broadcast channel (1024) drops events for slow clients with no resync mechanism — relevant only if web mode gains real concurrent users.
