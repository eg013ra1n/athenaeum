# Precise WCS & Image Registration (Stacking Preparation)

How Athenaeum produces a **high-precision WCS** and uses it to **register a
frame set's light frames to a common reference** for stacking. This is an
**opt-in** path layered on top of the normal (fast) plate solver — the fast
default solver and its corpus gate are untouched.

> Companion to [`README.md`](./README.md) (the base plate-solving pipeline) and
> [`star-detection.md`](./star-detection.md). The registration feature surfaces
> as the **"Stacking Preparation"** tab in the frame-set detail view.

---

## 1. Two solve profiles

Both profiles share the same blind/hinted search core; they differ only in
centroid quality and the distortion fit. Selected by
`solvemyastro::SolveConfig::centroid_refinement`.

| | **Fast** (default) | **Precise** (registration) |
| ---- | ---- | ---- |
| `centroid_refinement` | `Off` | `Auto` |
| Centroids | pass-1 aperture (`detect_fast`, ~0.3–0.5 px) | PSF/windowed (Moffat-refined, ~0.05–0.3 px) |
| Distortion fit | two-stage `fit_sip` | annealed σ-clipped weighted `fit_sip_annealed` |
| Seed | single `fit_affine` + `refit_affine` | + iterative σ-clip refit + PROSAC (gated, hard frames) |
| Safety | — | refinement-helped guard (never worse than Fast) |
| Used by | per-frame plate solve | reference solve in registration; any precise solve |
| Cost | baseline | +PSF fits + extra refits (per-frame, modest) |

**Invariant:** with `centroid_refinement = Off` the precise additions are
completely bypassed, so Fast is byte-identical to the pre-feature solver
(corpus gate: 14/14 truth, 0 RMS regressions). The Precise profile is measured
separately (`SOLVEMYASTRO_PRECISE=1` in `corpus_bench`): corpus **median RMS
0.317 px vs 0.404 px Fast (−22 %)**, and — by the guard — **no frame is ever
worse than Fast**.

---

## 2. The precise WCS pipeline

```text
FITS/XISF
   │  rustafits ImageAnalyzer::detect_fast (with_centroid_refine = true)
   ▼
[1] Detect + PSF-refine centroids  → FastStar { x,y (refined), raw_x,raw_y (pass-1), sx,sy (σ), fwhm, snr }
   │
[2] Global refine gate (refine::evaluate_refinement)
   │     Accept | Revert{Undersampled: med FWHM<1.5px} | Revert{LowSnr: med SNR<10}
   ▼
[3] Select quad pool  → select_fast_stars_with_raw  (x,y, raw_x,raw_y, σx,σy), SNR-ranked
   │
[4] autoFOV ladder × square-spiral search → quad match → seed affine (per cell)
   │
[5] Robust seed (only on weak seeds):
   │     • PROSAC RANSAC seed affine        when quad matches < 10
   │     • iterative 3σ-clipped affine refit when seed inliers < 50
   │     (≥50 inliers / ≥10 matches → existing single-pass path, unchanged)
   ▼
[6] Distortion fit:
   │     Precise → fit_sip_annealed (re-match @ shrinking tol, σ-weighted, 3σ clip)
   │     Fast    → fit_sip (two-stage)
   ▼
[7] Refinement-helped guard (Precise, when rms > 0.3 px):
   │     fit with refined centroids  vs  fit with pass-1 (raw) centroids
   │     keep whichever has the lower count_inliers RMS   ⇒ precise ≤ fast
   ▼
SolveSolution { wcs, pixel_scale_arcsec, field_rotation_deg, matched_stars,
                rms_residual_px/arcsec, inlier_ratio, psf_fwhm_px, … }
```

### [1] Detection + PSF centroid refinement (rustafits)

The fast detector returns pass-1 intensity-weighted centroids (~0.3–0.5 px).
When `ImageAnalyzer::with_centroid_refine(true)` is set, a refinement pass runs
a 2-D elliptical **Moffat LM** (the same `fit_moffat_2d_fixed_beta` the full
analysis pipeline uses) over each detection's pixel stamp and:

- overwrites `x, y` with the fitted centre,
- records per-axis Gaussian-σ centroid uncertainty in `sx, sy`,
- records the mean `fwhm`,
- **preserves the pass-1 centroid in `raw_x, raw_y`** (so the solver can later
  compare refined vs pass-1 — see the guard).

Per-star fall-backs (keep the pass-1 centroid): SNR < 10, non-physical/failed
fit, or a fitted centre that moved > 2 px (likely a bad fit).

### [2] Global refine gate (`solvemyastro/src/refine.rs`)

PSF fitting hurts on undersampled or low-SNR fields, so before trusting the
refined centroids the field is checked: median FWHM < 1.5 px → `Undersampled`,
median SNR < 10 → `LowSnr`. On a `Revert`, the solve falls back to pass-1
centroids for the whole frame.

### [3]–[4] Selection & search

`select_fast_stars_with_raw` returns refined `(x,y)`, pass-1 `(raw_x,raw_y)` and
σ `(sx,sy)` together, SNR-ranked. The search core is unchanged from the base
pipeline (autoFOV ladder `fov_rungs`, square-spiral sky cells, scale-invariant
quad matching, gnomonic-consistent affine seed — see `README.md`).

### [5] Robust seed (`orchestrate.rs`)

Hardens the seed **only when it is weak**, so easy frames are unchanged:

- **PROSAC RANSAC** seed affine when a cell yields `< 10` quad matches —
  SNR-prioritised 3-pair sampling, deterministic via `SolveConfig::ransac_seed`.
- **Iterative σ-clipped affine refit** (`refit_affine_iter`, 3–5 rounds,
  shrinking tolerance, 3.0 σ clip, converges on RMS change ≤ 3 % or inlier-set
  Jaccard > 0.97) when the seed has `< 50` inliers.

Frames with ≥ 50 seed inliers and ≥ 10 quad matches take the existing
single-pass affine + refit (byte-identical).

### [6] Annealed σ-clipped weighted SIP (`sip.rs::fit_sip_annealed`, Precise only)

Replaces the one-shot SIP fit with the loop mature solvers use:

1. Start from the linear WCS, generous match tolerance (~4 × base).
2. Project the catalog cone through the current WCS, pair each catalog star to
   the nearest detection within tolerance.
3. Fit SIP by **inverse-variance weighted** LSQ (`1/σ²` from the per-star
   centroid σ; uniform when σ is unavailable) with **3.0 σ clipping** inside the
   refit.
4. Shrink tolerance (`max(2 · median_resid, 0.5 px)`) and repeat (≤ 6 iters),
   stopping on inlier-set Jaccard > 0.95 or RMS change < 5 %.
5. Apply the "no worse than linear" guard **only at the end**.

Adaptive order (3 default; 4 only on rich fields; order-2 fallback). The
σ-weighting + clipping is what lets a few poorly-centroided stars stop
*dragging* the fit.

### [7] Refinement-helped guard (`orchestrate.rs`, Precise only)

PSF refinement is a big win on well-sampled fields but can produce *noisier*
centroids than the pass-1 aperture on pathological PSFs (heavily defocused,
blended, sparse) — and FWHM alone doesn't separate the cases. So at the final
fit (when RMS > 0.3 px) the solver computes the solution **both ways** — refined
centroids (annealed) and pass-1 centroids (`raw_x/raw_y`, two-stage) — and keeps
whichever yields the lower `count_inliers` RMS. This makes the Precise profile
**provably ≥ Fast** on every frame (the search/seed still benefit from the
refined centroids; only the final positions can revert).

---

## 3. How registration is done

**Goal:** give every light frame in a frame set a WCS in a *single shared sky
frame* plus its geometric transform to a chosen reference — the inputs a stacker
needs for alignment. **No image resampling** is performed; Athenaeum produces
the WCS + transforms and persists them.

### Architecture — reference (absolute) + members (relative)

```text
            ┌─ detect stars (precise) in every LIGHT member
            │
REFERENCE = user's choice (Analysis tab → frame_set_reference); auto-pick fallback
            │
   precise-solve REFERENCE  ──────────────►  absolute WCS (sky anchor for the whole set)
            │
   for each other MEMBER:
        register(member.detections → reference.detections)   [frame-to-frame, no catalog]
            │   quad-match → affine (sub→ref) → star-level refit
            ▼
        member WCS = reference_WCS ∘ affine     +   affine transform-to-reference + RMS
            │
   persist per-member row → registration_results
```

**Why relative for members?** Matching the *same stars* between a member and the
reference cancels catalog position error and atmospheric chromatic effects, so
frame-to-frame alignment is inherently more precise (and more robust) for
stacking than independently absolute-solving each frame. Only the **reference**
needs an absolute (catalog) solve, to anchor the set to the sky. A member that
can't absolute-solve can still register.

### Reference selection — user-chosen, persisted per object

The reference is **chosen by the user** in the frame-set **Analysis tab**
("Lights Analysis & Stats"): sort/filter the lights by quality (SNR, FWHM,
quality score…) and click **"Set as reference"** on the best frame. The choice
is persisted in the **`frame_set_reference`** table, **keyed by `frames_set_id`**
(object-scoped — the same frame can be the reference for one set without
affecting another). `set_frame_set_reference` validates the frame is a LIGHT
member of the set.

`register_frame_set` uses the persisted choice: when no explicit override is
passed it reads `frame_set_reference` and feeds it to
`select_reference` (`registration/reference.rs`). The **auto-pick fallback**
(most detected stars, tie-break smallest `frame_id`, then a ≤ 3-candidate
retry if the chosen reference fails to precise-solve) only applies when no
choice is stored — e.g. programmatic callers.

**Gating.** The Stacking Preparation tab is **available only when the set is
analyzed AND a reference is chosen** (`registrationTabReady` in
`FrameSetDetail.tsx`); otherwise the tab is disabled with a tooltip pointing to
the Analysis tab.

**Staleness.** Each `registration_results` row records the
`reference_frame_id` it was computed against. If the user later changes the
reference, the stored results are **kept but flagged stale** in the UI
(`StackingPrepTab` compares the rows' `reference_frame_id` to the current
`frame_set_reference`) — re-run to refresh. No schema flag is needed.

### The `register()` primitive (`solvemyastro/src/register.rs`)

```rust
register(reference_wcs: &WcsSolution,
         ref_detections: &[(f64,f64)],
         sub_detections: &[(f64,f64)],
         cfg: &SolveConfig) -> Result<Registration>
```

returns

```rust
Registration {
    transform: Affine,                       // sub-frame pixel → reference pixel
    refined_wcs: WcsSolution,                // the sub's absolute WCS
    residual_pairs: Vec<((f64,f64),(f64,f64))>, // post-affine (predicted, actual), ref-space
    matched: usize,
    rms_px: f64,
}
```

Steps: build quads on both detection lists → `match_quads` (scale-invariant) →
`fit_affine` from quad centres (bootstrap) → **star-level SVD refit** on the
nearest-neighbour star pairs (more robust than quad centres) → RMS over inliers.

**WCS composition.** With `M = [[a1,b1],[a2,b2]]`, `t = (c1,c2)` the member's WCS
is `reference_wcs ∘ transform`:

```text
CRVAL' = reference.CRVAL
CD'    = reference.CD · M
CRPIX' = M⁻¹ · (reference.CRPIX − t)
```

so `refined_wcs.pixel_to_sky(p) == reference_wcs.pixel_to_sky(transform(p))` for
any member pixel `p`. The whole set therefore shares one sky frame
(co-registration).

### Differential (frame-to-frame) distortion

Two subs of the same target can carry *different* distortion (flexure, focus/
temperature drift, filter glass-path, changing airmass, dither rotation). That
belongs in the **alignment transform**, not the absolute WCS: the transform is
`affine → +SIP (distortion-aware) → TPS (gated)`. `residual_pairs` is the hook —
the post-affine member↔reference residual field — used to decide whether to fit a
distortion term. (Absolute per-frame distortion is centroid-bound across the
corpus, so this is a relative-alignment concern; TPS remains a gated future
fallback.)

### Orchestration & persistence (`crates/athenaeum-core/src/registration/`)

`service::register_frame_set(conn, frames_set_id, override_reference_id, cache,
bright_cache, ps_config, emitter, cancel) -> RegistrationSummary`:

1. Load LIGHT members (`db::get_light_frame_ids_for_frame_set`, via
   `frames_set → imaging_nights → sessions → session_members`).
2. Detect stars per member (precise; capped ~400). Emit per-frame progress;
   honour `cancel`.
3. Select + precise-solve the reference.
4. `register()` each other member to it.
5. Persist each result via `db::upsert_registration`.

Results live in the **`registration_results`** table — keyed
`UNIQUE(frames_set_id, frame_id)`, holding the WCS (`crpix/crval/cd`), the affine
(`affine_a1..c2`), `matched_stars`, `rms_residual_px/arcsec`, `status`
(`reference` | `aligned` | `failed`), `error`, and timing. **It never overwrites
a frame's primary WCS or the `plate_solves` table** — registration is additive
and isolated.

### Commands, events, UI (two backends in sync)

| Layer | Surface |
| ---- | ---- |
| Tauri + Axum | `register_frame_set` · `get_frame_set_registration` · `cancel_frame_set_registration` |
| Events | `stacking-prep-progress` (per frame) · `stacking-prep-complete` (summary) |
| Frontend | "Stacking Preparation" tab in `FrameSetDetail` · `StackingPrepTab.tsx` · `useRegistrationQueue.ts` |

The tab shows the chosen reference, a per-member table (status badge, matched
stars, RMS arcsec, errors), and a prepare / re-run / cancel flow with live
progress; it loads persisted results via `get_frame_set_registration` and
notifies on completion (`NotificationKind = 'registration'`).

---

## 4. Outputs

Per light frame, after a registration run:

- **Absolute WCS** in the set's shared sky frame (`crval/crpix/cd`).
- **Affine transform to the reference** (`affine_a1..c2`, sub-pixel → ref-pixel).
- **Alignment quality** (`matched_stars`, `rms_residual_px/arcsec`, `status`).

These are the handoff for an external stacker/resampler. The reference frame
carries the identity transform and its own absolute WCS.

---

## 5. Configuration & diagnostics

| Knob / env | Effect |
| ---- | ---- |
| `SolveConfig::centroid_refinement` | `Off` (fast) / `Auto` (precise) |
| `SolveConfig::ransac_seed` | deterministic PROSAC seeding |
| `SolveConfig::sip_order` | SIP polynomial order |
| `SOLVEMYASTRO_PRECISE=1` | run `corpus_bench` in the precise profile (measurement) |
| `SOLVEMYASTRO_DUMP_RESIDUALS=1` | dump per-star residual vector fields (centroid- vs distortion-bound) |

---

## 6. Gates & verification

- **Fast corpus gate** (`corpus_bench`, fast profile) — the hard CI gate:
  14/14 truth, 0 wrong, per-frame RMS ≤ 1.15× + 0.05, **median ≤ 1.05×**, 0
  panics. The precise additions never touch this path.
- **Precise measurement** (`SOLVEMYASTRO_PRECISE=1`) — informational; confirms
  the median improvement and the guard (no frame worse than Fast).
- **Registration e2e** — `crates/athenaeum-core/tests/registration_e2e.rs`
  (`#[ignore]`): real frame + real catalog, exercises detect → precise-solve
  reference → align → persist end-to-end.

---

## 7. Deferred (Phases 5–6)

The "exceed-PixInsight *absolute*" levers, not required for registration (which
is relative): a **catalog record v2** (f64 RA/Dec end-to-end + per-star σ for
inverse-variance weighting + BP−RP colour + de-clipped PM) needing a Gaia
rebuild + re-host, and **DCR correction** (a new astrometry module computing
LST / alt-az / parallactic angle, applied via BP−RP colour with graceful
per-frame skip). TPS distortion stays a gated fallback.

---

## 8. File map

| Area | Files |
| ---- | ---- |
| Centroid refinement | `rustafits/src/analysis/mod.rs` (`FastStar`, `with_centroid_refine`), `analysis/fitting.rs` (Moffat LM) |
| Precise solve | `solvemyastro/src/{orchestrate.rs, sip.rs, select.rs, refine.rs, lib.rs}` |
| Registration primitive | `solvemyastro/src/register.rs` |
| Registration feature | `crates/athenaeum-core/src/registration/{service.rs, reference.rs, db.rs, mod.rs}` |
| Tables | `crates/athenaeum-core/src/db/schema.rs` (`registration_results`, `frame_set_reference`) |
| Commands | `crates/athenaeum-{tauri/src/commands,web/src/routes}/registration.rs` (incl. `set/get/clear_frame_set_reference`) |
| UI — registration | `src/components/StackingPrepTab.tsx`, `src/hooks/useRegistrationQueue.ts`, `src/pages/FrameSetDetail.tsx` (tab gating) |
| UI — reference picking | `src/components/LightsAnalysisView.tsx`, `src/components/calibration/LightsAnalysisTable.tsx` (Analysis tab "Set as reference") |
| Tests | `solvemyastro` unit tests (`register`, `sip`, `refine`, orchestrate), `crates/athenaeum-core/tests/registration_e2e.rs` |
