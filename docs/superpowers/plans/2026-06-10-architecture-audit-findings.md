# Architecture Audit Findings — 2026-06-10

Record of a four-track deep audit (data layer/concurrency, precision pipeline, testability, targeted bug hunt). High-severity scanner findings were fixed the same day on `feature/stacking-prep`; everything else is tracked here so it isn't re-discovered (or re-flagged) later. A follow-up rustafits-specific audit (previews + analysis) is appended at the end.

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

Status column added 2026-07-03 (T10 doc reconciliation) once R1/R2/M2/M4/R3 were
fixed on `0.2.2`; rows kept in place per this doc's ID-anchor contract — see
"Fixed in this audit" above for the same FIXED-marker convention.

| ID | Status | Severity | Location | Finding |
| ---- | ---- | ---- | ---- | ---- |
| R1 | **FIXED** (v0.2.2 / Wave 1) | High (future stacking) | `solvemyastro/src/register.rs` (~line 323) | Meridian-flipped frames register successfully with `det(M) < 0` — a mirror transform. Only `abs(det) < 1e-12` is checked. Silent data corruption once pixels are resampled. Fix: solvemyastro `98b39c6` (`Registration.flipped` derived from refit `det(M)`), surfaced in athenaeum `255d9717` (persist `ok_flipped` + UI badge). |
| R2 | **FIXED** (v0.2.2 / Wave 6) | High (future stacking) | `solvemyastro/src/register.rs` + `athenaeum-core/src/registration/` | Mixed binning / pixel scale within a frame set composes a scale-wrong WCS (`CD' = CD_ref · M` assumes a shared reference CD). Fix: athenaeum `38615d55` — pre-registration consistency gate (binning groups + focallen ±1%) bailing through the `stacking-prep-progress` error path. |
| M2 | **FIXED** (v0.2.2 / Wave 2) | Medium | `athenaeum-core/src/archive/restore.rs:236-248` | Restore skip-if-exists accepts whatever file sits at `source_path` without comparing to the stored `expected_hash`; archive markers are then cleared. A wrong file is silently blessed as "restored". Fix: athenaeum `49f77f66` + `594a0e76` — hash-verified skip path, conflict disposition (markers intact, `CompletedWithErrors`, frame set stays archived so retry is reachable). |
| M4 | **FIXED** (v0.2.2 / Wave 3) | Medium | `athenaeum-core/src/file_op/executor.rs:499-532` | Cross-volume move: copy+verify+catalog-update succeed, then source-delete fails and the operation is abandoned → next scan's fingerprint-based move detection flips `files.path` back to source, leaving the dest copy as an invisible disk orphan. Fix: athenaeum `48e0fa80` (`file_op::reconcile` auto-heal at queue startup + pre-enqueue) and `020acdda` + `f8cdd174` (volume-aware move-detection guard at both fingerprint sites). |
| R3 | **FIXED** (v0.2.2 / Wave 1) | Low | `solvemyastro/src/register.rs` (~line 207) | `INLIER_TOL_PX = 4` hardcoded regardless of pixel scale (too loose at 0.5"/px, too tight at 2"/px). Fix: solvemyastro `f850406` — `register_with_config` + `register_inlier_tol_arcsec` in `SolveConfig` (clamp [1,12] px, degenerate-CD fallback). |
| R4 | Open | Low | `solvemyastro/src/sip.rs` | No coordinate normalization to [-1, 1] before SIP least-squares (standard practice in TWEAK/SCAMP). Conditioning floor for order ≥ 3 on large sensors. Not urgent at order ≤ 3. |
| S1 | Open | Low | `athenaeum-core/src/clustering/mod.rs:279-334` | Greedy O(n²) clustering with the full frame list in memory. Fine at current catalog sizes; revisit past ~20k lights. |
| S2 | Open | Low | `athenaeum-core/src/db/calibration_links.rs:1031+`, `db/operations.rs:517+` | Unbounded list queries materialize full result sets (no pagination). Same trigger: revisit at scale. |

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
- ~~Two raw `BEGIN`/`COMMIT` sites remain in `db/operations.rs` (the reason for the defensive checkout rollback); migrate to savepoints/`unchecked_transaction` when touched.~~ **FIXED** (v0.2.2 / Wave 2, 2026-07-03): the July recount found three raw pairs, not two — fixed in athenaeum `1c0d5108` (`SavepointGuard` RAII); zero raw `BEGIN`s remain in `db/operations.rs`.
- SSE broadcast channel (1024) drops events for slow clients with no resync mechanism — relevant only if web mode gains real concurrent users.

---

# Rustafits Audit (previews + analysis) — 2026-06-11

Method: 3 exploration agents over rustafits decode/render, analysis, and the athenaeum integration layer; every significant claim then independently verified by reading source AND by a faithful numerical simulation of the stretch algorithm (Rust NaN-comparison/clamp/saturating-cast semantics). The verification step killed five agent findings and one of the auditor's own.

## Fixed (rustafits `feature/stacking-prep` + athenaeum)

| ID | Severity | Finding | Fix |
| ---- | ---- | ---- | ---- |
| V1 | Medium | NaN-heavy float frames (FITS BITPIX=-32, XISF Float32/64 — e.g. registration borders) silently rendered an **all-black preview**: NaN samples poisoned the stretch median/MADN (`stretch.rs`), all params went NaN, every pixel saturating-cast to 0. Empirically: ~2% scattered NaN was survivable (NaN pixels render black — fine); ~30% NaN killed the whole frame; breakpoint partition-luck dependent. Analysis was already mostly protected (`background.rs:53` filters non-finite; detection local-max rejects NaN peaks). | Filter non-finite samples in `compute_stretch_params`; neutral-params fallback when all samples are non-finite. Unit + e2e tests incl. NaN-border frame. |
| V2 | Medium | Stale preview cache in BOTH backends: key was `path:resolution` with no mtime (`commands_rustafits.rs`, `routes/images.rs`) — in-place edit/restore/re-parse served a stale JPEG up to 30 min; deleted files could keep serving from cache. | Shared `preview_cache_key` helper in `athenaeum-core::cache` keys on `path:mtime:resolution`; stat doubles as fail-fast existence check. Both backends wired. |
| S1 | Low | XISF Uint8 scaled ×256 → max 65280 ≠ 65535; 8-bit XISF rendered ~0.4% darker than the same data as u16. | ×257. |
| S2 | Low | `with_downscale(0)` unvalidated → division by zero in downscale kernels (latent API hazard; app only passes 1/4). | `.max(1)` + regression test. |

## Open (documented, not fixed)

| ID | Severity | Finding |
| ---- | ---- | ---- |
| S3 | Low-Med | `fits.rs:132-136` accepts NAXIS3 > 3 while downstream assumes 1 or 3 channels — malformed-input robustness (stride assumptions). |
| S4 | Low | Analysis `saturation_limit` fixed at `0.95·65535` (`detection.rs:38`) — dead gate for [0,1]-domain float inputs, so clipped stars pass into PSF measurement for processed float files. Analysis targets raw u16 subs in practice. |
| S5 | Low | Fast-detect centroid refinement hardcodes `init_sigma = 3.0` (`analysis/mod.rs:1213`) vs field-FWHM-derived in the full path; ±0.5 px divergence possible on defocused frames, bounded by the >2 px-shift revert gate. |
| S6 | Low | Odd dimensions silently lose the last row/col in debayer/downscale (standard practice). Rayleigh trail-test p-value approximation ~5% loose at the n=20 minimum. |
| S7 | Info | Local-only diagnostic test `rustafits/tests/fast_detect_real.rs` (gitignored, untracked) fails pre-existing on `feature/stacking-prep`: fast path covers only 28% of the slow path's top-100 stars on the dense cocoon field (threshold 85%). Unrelated to this audit's changes (verified by stash); investigate with the centroid-refinement work. |

## Claims REJECTED after verification (do NOT re-flag)

| Claim | Verdict |
| ---- | ---- |
| "[0,1] float FITS render dark — no normalization + hardcoded `max_input=65536`" | **WRONG (auditor's own initial finding, retracted after simulation).** The median/MADN-based STF is scale-adaptive: the same sub as u16 and as [0,1] float renders near-identically (bg 64 vs 63, stars 242 vs 241). Locked in by `unit_range_floats_stretch_like_u16` test. |
| "HFR formula dimension error" | WRONG — `Σ(flux·d)/Σ(flux)` is the standard flux-weighted HFR; units are pixels (`metrics.rs:462-497`). |
| "find_median on empty slice panics" | WRONG — explicit empty guard returns 0.0 (`stretch.rs` quickselect). |
| "STF division-by-zero on constant/all-zero images" | WRONG — `x==0`/`x==m` guards exist; m=0.25-branch denominators algebraically ≤ −0.25; constant → gray, all-zero → black (simulated). |
| "NaN as u8 is undefined behavior" | WRONG — Rust float→int casts saturate; NaN → 0. |
| "Saturated stars bias FWHM statistics" | MOSTLY WRONG — saturated candidates are rejected at detection (`detection.rs:393`); only the float-domain dead-gate (S4) is real. |
| "NaN poisons the analysis pipeline" | OVERSTATED — background cell stats filter non-finite (`background.rs:53`); detection local-max comparisons reject NaN peaks. The preview stretch was the unguarded path (V1, fixed). |
