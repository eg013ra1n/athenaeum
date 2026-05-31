# Scale-Recovery Fallback for Hinted Plate Solves

- **Date:** 2026-05-31
- **Status:** Approved (design); plan pending
- **Component:** `solvemyastro` submodule (solver only; no athenaeum-core / Tauri / Axum change)
- **Related:** plate-solving design (`2026-04-11-plate-solving-design.md`); solvemyastro roadmap (local)

## 1. Problem

A wide-field, low-galactic-latitude frame (Heart/W3 region, b ≈ 0.2°, `Light_Pane 4_…_O_…_0002_c_lps_r.xisf`) was reported as extremely slow to plate-solve. Measurement on the real frame isolated the cause to the **scale hint**, not the catalog and not the field density:

| Scale hint | Source | Result |
| ---- | ---- | ---- |
| 0.879″/px (correct) | DB `focallen=563 mm`, `xpixsz=2.4 µm` | **677 ms** — solves at the second FOV rung, 85 inliers |
| 3.52″/px (4× wrong) | a misread `focallen=140 mm` (the *aperture*; 140 mm f/4 ≈ 563 mm FL) | **164 s** — hinted pass fails in ~3 s, then an all-sky blind search eventually solves |

The cost is a ~240× swing driven by one number. When the scale hint is wrong, the current solver does the following (`solvemyastro/src/lib.rs::solve`, ~lines 217-244):

1. Hinted solve (position + scale). Fails fast (~3 s).
2. Degrade-to-blind: **clears the position prior, keeps the scale hint**, retries all-sky. This is the ~160 s pass.

The degrade path was built for a *wrong position with a trusted scale*. It has no symmetric path for the common inverse — a **good position with a wrong/garbage scale** (focal-length-as-aperture, binning/drizzle factor errors, missing scale). For those frames it discards the good hint (position) and keeps the bad one (scale), forcing the expensive all-sky search.

Empirically, the failed hinted pass already *recovered the true scale*: solvemyastro's matcher is scale-invariant (quad hash = pairwise distances normalised by the longest) and computes `scale = catalog.longest / image.longest` (median, outlier-rejected) for every matched pair. The structured failure for the wrong-hint run carried `best_attempt.scale_arcsec_per_px = 0.879` (the true scale) with 2 inliers — it knew the scale, it just failed verification at an off-centre FOV rung.

## 2. Goals / Non-goals

### Goals

- On hinted-solve failure where a position prior exists, recover quickly when the *scale* was the bad input, by searching the known position area across scale instead of going all-sky.
- Reuse existing machinery (scale-invariant matcher, recovered-scale, FOV ladder, verification). No quad-format or database changes.
- The correctly-hinted fast path pays nothing (new work only runs after a failure).
- Prove the win and the no-regression with the existing corpus-bench gate (precision + deterministic `cone_calls`).

### Non-goals (this change)

- Image binning/downsampling before star detection.
- A global indexed quad database (replacing per-cell cone pulls).
- A dedicated quorum-based scale estimator beyond harvesting the existing `best_attempt`.
- Any athenaeum-side metadata auto-correction (e.g. focal-length sanity warnings) — tracked separately.

## 3. Design

### 3.1 Fallback ladder (control flow in `lib.rs::solve`)

Each rung runs only if the previous one failed; a step-A success returns immediately, so correctly-hinted frames are untouched.

```text
A  orchestrate::solve(hints)                          [unchanged]
   └ on Err AND (hints.ra or hints.dec present):
B1   harvest best_attempt.scale from A's failure → retry ONCE at that scale (position kept)
B2   if B1 ineligible or B1 fails → bounded scale sweep (position kept, FOV ladder = hint ÷8 … ×2)
C  clear position, keep scale → all-sky retry          [existing degrade path]
   └ all fail → return step A's original SolveFailure (richest diagnostics)
```

### 3.2 Step B1 — scale harvest (high-confidence, near-free)

Downcast step A's `anyhow::Error` to the existing `diag::SolveFailure` and read `best_attempt` (`diag::BestAttempt`, fields already present: `scale_arcsec_per_px`, `inliers`, `required`, `log_odds`, `seed_ra/dec`).

Eligibility gate (all must hold):

- `scale_arcsec_per_px` is finite and ∈ [0.05, 120]″/px (the solver's existing physical bounds),
- `inliers ≥ b1_min_inliers` (default **2** — see §5; B1's retry is fully verified so a low threshold is correctness-safe),
- harvested scale differs from the supplied hint by > **15%** (otherwise step A already searched there).

If eligible, retry once via `orchestrate::solve` with `ra/dec` kept and the harvested scale passed as a **point** `pixel_scale_arcsec`. Passing it as a point hint re-centres `fov_rungs` finely around the true scale (rungs land on ≈ the true full-frame FOV and its halving), reproducing the fast hinted-solve geometry — this is the mechanism by which Pane 4 then solves in ~0.7 s. Cost: one targeted, position-constrained pass.

### 3.3 Step B2 — bounded scale sweep (robustness)

Runs when B1 is ineligible (no clean estimate) or B1's single retry fails verification. Keeps `ra/dec`, clears the point scale hint, and drives the FOV ladder over a **bounded, asymmetric** range derived from the hint:

- range = `[hint_scale ÷8, hint_scale ×2]` (asymmetric toward *finer* scale because aperture-as-focal-length errors always make the recorded focal length too short → true scale finer → true FOV smaller),
- if **no** scale hint exists at all, fall back to the full blind ladder (`FOV_MAX_DEG=9.5°` → `FOV_MIN_DEG=0.38°`, `FOV_DIV=1.5`).

Implementation seam: `fov_rungs(long_px, hints)` (`orchestrate.rs:126`) gains an optional bounded range so step B2 can request rungs spanning `[lo, hi]` instead of the default centre-±4-steps span.

### 3.4 Configuration (`SolveConfig`, defaults preserve current behaviour on success and make the feature opt-out-able)

- `scale_fallback_enabled: bool` — default **true**
- `b1_min_inliers: usize` — default **2** (correctness-safe; B1's retry is fully verified)
- `b2_scale_lo_factor: f64` — default **0.125** (÷8)
- `b2_scale_hi_factor: f64` — default **2.0** (×2)

### 3.5 Error handling, cancellation, latency

- Every rung honours the existing `cancel: Option<&AtomicBool>` and bails immediately if set.
- On total failure, return step A's original `SolveFailure` (richest reason), not B/C's.
- Worst-case latency for a genuinely unsolvable frame increases (A + B1 + B2 + C), but B1 and B2 are position-constrained (cheap); only C is all-sky. This is the accepted trade for the chosen A→B→C ordering. Cancellation keeps it bounded for interactive use.

## 4. Components & files touched (all in `solvemyastro`)

| File | Change |
| ---- | ---- |
| `src/lib.rs` | Rework `solve()` degrade orchestration into the A→B1→B2→C ladder; consume `SolveFailure.best_attempt` for B1. |
| `src/orchestrate.rs` | Add an optional bounded scale range to `fov_rungs` (B2). No change to the search/verify core. |
| `src/lib.rs` (`SolveConfig`) | Add the four config knobs with defaults. |
| `tests/scale_fallback.rs` (new) | Dedicated integration test: real Pane 4 frame + reconstructed wrong (aperture-derived) scale hint, app-faithful tiered catalog; asserts a correct, cheap solve (see §5). |
| `tests/corpus_bench.rs` | Unchanged; re-run as the no-regression gate. |

No changes to the scale-invariant quad matcher, the cone/database access, or verification thresholds.

## 5. Testing & acceptance criteria — and implementation findings (2026-05-31)

Implemented and verified via subagent-driven development. The findings below amend the original plan honestly:

**Integration test (`tests/scale_fallback.rs`).** Loads the real Pane 4 frame, supplies the aperture-derived wrong hint (3.52″/px) with the correct position, and the app-faithful **tiered** catalog (deep + bright); asserts a correct, cheap solve (RA ≈ 36.70, Dec ≈ 60.88, scale ≈ 0.879″/px, ≥ 10 inliers, solve < 30 s). Skips cleanly if the cache/frame are absent. (A full-corpus frame would not exercise this — the corpus harness prefers an existing-WCS scale, and the file's header lacks the bad focal length.)

**Key finding — B1/B2 not exercised end-to-end.** Under the current baseline solver, this frame's wrong-hint case is solved **directly in step A's density-balanced pass** (~1–2 s, ~84 inliers), reproducibly across debug and release runs — so the fallback rungs do not fire for it. (An earlier one-off measurement showed step A failing → ~160 s blind solve; that did not reproduce, and there is some debug/release variation in the dense-field quad matching — a pre-existing baseline property, out of scope here.) Consequently the integration test guards the **user-facing outcome** (wrong-scale frame → correct, fast solve) but does not isolate B1/B2. The fallback is validated by the unit tests + the control-flow review, and remains a correctness-safe net for frames where step A genuinely fails. *Possible follow-up:* a deterministic forced-fallback test using a hint whose ±4-step step-A ladder cannot reach the true scale but B2's ÷8 bound can.

**Unit tests (all green):**

- `harvest_scale` (B1 gate): physical scale bounds, inlier threshold, hint-divergence accept/reject — 4 tests.
- `b2_bounds` (B2): bounds derived from the hint; `None` when no scale hint — 2 tests.
- `fov_rungs_bounded`: bounded span clamped to `[FOV_MIN, FOV_MAX]`, strictly descending, reaches the fine end — 1 test.

**No-regression gate (`corpus_bench`, release, uncapped).** PASSED: **0 rms precision regressions** across the corpus (14/14 ground-truth frames correct, 0 wrong; M78 solves, dpos 2.9″); net wall 0.41× baseline. Confirms the fast path is untouched — every existing frame still solves at step A, and the new rungs only run after a step-A failure.

## 6. Risks & mitigations

- **B1 harvests a spurious scale** → gated by sanity + inlier threshold + hint-divergence; if its one retry fails, B2 and C still run, so a bad estimate costs only one quick pass.
- **B2 false positive in dense fields** → bounded range + unchanged verification thresholds; the corpus gate is the proof, and C remains the safety net.
- **`b1_min_inliers = 2`** (lowered from the original 3). Because B1's retry must pass full verification, a weak/spurious harvested scale cannot cause a false solve — it only costs one extra pass before B2/C run. So a low threshold is correctness-safe, and 2 catches a 2-inlier near-miss directly.
- **Worst-case latency** for truly unsolvable frames grows; acceptable because it only occurs after the fast path has already failed, and cancellation bounds it.

## 7. Out of scope / roadmap

- Image binning/downsampling before detection (fewer, brighter stars → fewer quads) — a general speed lever, separate item.
- A global indexed quad database vs per-cell cone pulls — larger re-architecture, separate item.
- A dedicated quorum-based scale estimator beyond the `best_attempt` harvest.
- athenaeum-side focal-length/f-ratio sanity warning at scan time (≈ 68 frames currently have `focallen < 200 mm`, candidates for aperture-as-focal-length errors).
