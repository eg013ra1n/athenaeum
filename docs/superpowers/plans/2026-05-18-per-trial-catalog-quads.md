# Per-Trial Catalog-Quad Construction — Implementation Plan

> **For agentic workers:** execute task-by-task; bench-verify after each task that
> touches the solve path. Steps use checkbox (`- [ ]`) tracking.

**Goal:** Match image quads against catalog quads that are **built per solve
at the frame's own scale/region from the same star set and grouping mode as
the image** — the way ASTAP does — instead of against one fixed
HEALPix-depth-6 global index. This is the evidence-backed fix for the frames
where detection is provably fine but the matcher gets 0 inliers at the true
position even with correct scale + depth-match (M51 galaxy-dominated, M78
ultra-dense Orion equator).

**Architecture:** `QuadLookup { hash_key, longest_dist_deg, stars_ra[4],
stars_dec[4] }` is the seam: the entire downstream verify/refit/gate pipeline
(`catalog_centroid`, `best_permutation_fit`, scale filter, density gate)
consumes catalog quads **only** as `QuadLookup`. So the change is *additive*:
produce a `Vec<QuadLookup>` from a fresh cone read instead of from
`index.lookup_with_tolerance`. Nothing downstream changes. Gated to the
**position-hinted** path (M51/M78 have OBJCTRA/OBJCTDEC); the prebuilt global
index stays for the blind/no-position path. Regression surface is therefore
only the hinted frames, fully covered by the ASTAP-oracle bench.

**Tech stack:** existing `CatalogEngine::cone_search` (region read,
proper-motion to epoch), `GnomonicProjection::sky_to_tangent` (sky→tangent),
`build_quads_multi` (the *same* image quad builder, reused on projected
catalog stars → identical asterisms), `hash_key_from_ratios`.

---

### Why this and not more detection/selection tweaks

Three detection/selection hypotheses (binning, stellarity quad filter,
saturation centroid) were each refuted by the oracle and two regressed
NGC 2024. The `detection_audit` tool proved detection finds the right stars.
The remaining failure is **asterism incomparability**: the global index's
catalog quads are 3-NN over *all* Tycho-2 ≤ mag_limit at ~0.84° cells,
independent of the frame's star density/FOV/grouping; the image quads are
density-/FOV-/grouping-specific. On dense (M78) or galaxy-skewed (M51)
fields the same 4 physical stars do not form the same quad on both sides, so
no hash matches. Per-trial construction makes both sides use the *same*
stars, scale, and grouping — comparable by construction. This is the only
remaining lever the evidence supports.

---

### Task 1: `local_quads` module — build `Vec<QuadLookup>` from a sky region

**Files:**
- Create: `crates/athenaeum-core/src/plate_solve/local_quads.rs`
- Modify: `crates/athenaeum-core/src/plate_solve/mod.rs` (register `mod local_quads;`)
- Test: unit tests in `local_quads.rs`

- [ ] **Step 1 — function signature**

```rust
/// Read catalog stars in the cone and build catalog quads on the fly, with
/// the SAME grouping as the image quads, returning the matcher's existing
/// `QuadLookup` representation (so the whole verify pipeline is unchanged).
pub fn local_catalog_quads(
    catalog: &CatalogEngine,
    ra0: f64, dec0: f64,          // trial centre (from hints)
    fov_diag_deg: f64,            // from scale hint + image size
    mag_limit: f32,               // config.index_mag_limit
    obs_epoch: f64,
    group_size: usize,            // same ladder as the image pass
    hash_tolerance: f64,          // index.hash_tolerance()
    max_stars: usize,             // brightest-N cap (image-comparable depth)
) -> Vec<QuadLookup>
```

- [ ] **Step 2 — read + project + build**
  1. `let (stars, _) = catalog.cone_search(ra0, dec0, (0.6*fov_diag_deg).min(89.0), mag_limit, obs_epoch)?;`
  2. Sort by `mag` ascending; truncate to `max_stars` (brightest-N, ASTAP-style depth match).
  3. Project each to tangent plane: `let (xi, eta) = GnomonicProjection::sky_to_tangent(s.ra, s.dec, ra0, dec0);`
     store `positions: Vec<(f64,f64)>` in **arcsec** (`xi.to_degrees()*3600.0`, same for eta) so `longest_dist` is in a real angular unit. Keep a parallel `radec: Vec<(f32,f32)>`.
  4. `let quads = build_quads_multi(&positions, positions.len(), group_size);` — identical builder/grouping to the image side.
  5. For each `Quad`: `hash_key = hash_key_from_ratios(&q.ratios, hash_tolerance)`;
     `longest_dist_deg = q.longest_dist / 3600.0` (arcsec→deg);
     `stars_ra/stars_dec` = the 4 `radec[idx]` for `q.star_indices`.
     Push `QuadLookup { hash_key, longest_dist_deg, stars_ra, stars_dec }`.

- [ ] **Step 3 — failing test**: synthetic 6-star sky patch with a known
  asterism; assert `local_catalog_quads` returns ≥1 quad whose `hash_key`
  equals the hash of the same 4 stars built via `build_quads_multi` on their
  pixel positions at an arbitrary scale (scale-invariance of the ratios).
- [ ] **Step 4 — run test → fail (no fn) → implement → pass.**
- [ ] **Step 5 — commit** (`feat(plate-solve): local per-trial catalog quad builder`).

### Task 2: wire an alternative candidate source into `try_solve_pass`

**Files:** Modify `crates/athenaeum-core/src/plate_solve/service.rs`
(`try_solve_pass`, candidate-generation block ~L1089–1098; `run_retry_passes`
to pass the trial centre/fov through).

- [ ] **Step 1** — add params to `try_solve_pass`/`run_retry_passes`:
  `local_quad_center: Option<(f64,f64)>` and reuse existing
  `expected_scale_arcsec` + `image_size` to derive `fov_diag_deg`.
- [ ] **Step 2** — candidate generation, replacing the index lookup **only
  when `local_quad_center` is `Some` and a scale is known**:

```rust
let cat_quads: Vec<QuadLookup> = if let (Some((ra0,dec0)), Some(scale)) =
    (local_quad_center, expected_scale_arcsec) {
    let fov = ((image_size.0 as f64).hypot(image_size.1 as f64)) * scale / 3600.0;
    local_catalog_quads(catalog, ra0, dec0, fov, config.index_mag_limit,
        obs_epoch, group_size, index.hash_tolerance(),
        image_positions.len().max(QUAD_MIN_STARS))
} else { Vec::new() };
// brute-force match image quads vs cat_quads (set is small, per-trial)
```
  Fall back to the existing `index.lookup_with_tolerance` path when
  `cat_quads` is empty (blind / no position) — **the global index path is
  retained, not removed**.
- [ ] **Step 3** — pass `local_quad_center = hints.ra.zip(hints.dec)` from
  `solve_cascade` stage 1 (hinted) only; `None` for the scale-cleared/blind
  stages and the FOV-ladder rungs (those keep global-index behaviour).
- [ ] **Step 4 — bench gate**: `BENCH_SKIP_ASTAP=1` full bench.
  **Acceptance:** the 13 currently-correct frames stay correct (Δpos<30",
  Δscl<2%), 0 panics, 0 false positives; M51 and/or M78 newly correct.
  If any of the 13 regress → iterate on `max_stars`/`group_size`/cone radius
  before proceeding; do not commit a net-negative.
- [ ] **Step 5 — commit** once acceptance holds.

### Task 3: verify + finalize

- [ ] `cargo test -p athenaeum-core --lib` + `-p rustafits --lib` green.
- [ ] Full ASTAP-oracle bench: record final N/16, confirm no regression vs
  the committed `875d72d4` 13/16 baseline.
- [ ] `detection_audit` unaffected (no detector change in this project).
- [ ] Update auto-memory only if a durable, non-obvious convention emerged.

---

## Risk / kill criteria

- **Primary risk:** changing the hinted candidate source affects the 13
  working frames. Mitigation: Task 2 Step 4 is a hard gate — net-negative is
  reverted, not committed (same discipline applied to binning/stellarity/
  saturation).
- **Cost risk:** per-trial cone read + quad build runs each hinted pass.
  cone_search is already used in verification; expected sub-second for the
  bench FOVs. If a pass becomes pathologically slow, cap `max_stars` and
  cache the cone per (centre,radius) like the existing `cone_cache`.
- **Kill criterion:** if Task 2 cannot reach ≥13/16 with M51 or M78 added
  after reasonable parameter iteration, stop, revert, and conclude the
  Tycho-2 limit for these two specific fields is real — do not keep firing
  hypotheses at the oracle (the lesson from the prior three attempts).

## Out of scope

- Blind/no-position path (keeps the global index + FOV ladder unchanged).
- `_DSC5767` (headerless, no position to read a cone at — would need the
  ASTAP spiral; separate project).
- Any detector change (detection is proven adequate by `detection_audit`).
- Gaia / multi-scale prebuilt index (superseded by per-trial construction).
