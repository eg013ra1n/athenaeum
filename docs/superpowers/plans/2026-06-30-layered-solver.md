# Layered Solver (Plan 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the plate-solver query the additive density-tier catalogs — a stack
of disjoint `stars.smac` layers merged per cone — replacing the fixed deep+bright pair.

**Architecture:** Generalize solvemyastro's `Caches` from `{deep, bright}` to an
ordered layer stack. A per-solve `LayeredCones` helper bundles the layers + one
`PixelCache` each and exposes `cone(depth, …)` = the union of the first `depth`
layers (disjoint → concat, no dedup). Quad-match queries the base layer (fast)
with fallback to all; verify queries all. `athenaeum-core` discovers the
`tier_*/` dirs and passes them; a legacy single `stars.smac` becomes a 1-layer
stack (byte-identical to today).

**Tech Stack:** Rust; solvemyastro (`cache.rs`, `orchestrate.rs`, `lib.rs`);
athenaeum-core (`plate_solve/`); rayon; `corpus_bench`.

**Spec:** `docs/superpowers/specs/2026-06-29-tiered-additive-star-catalog-design.md` §3.

> **STATUS (2026-06-30): Plan 2 DONE.** Solver consumes the tier stack; the
> real-data gate `corpus_layered_tiers` passes 14/14 truth (incl M78 + SIP),
> 0 wrong / 0 missed / 0 panic on the 4-tier union. solvemyastro `05bf2d8`
> (+ `bdbdb57` gate); athenaeum `45b78fe0`. **Tasks 5 Step 1–2 (install into
> app-data + live-app solve) and the startup catalog-download flow are deferred
> to Plan 3** (catalog delivery) — the app-data deep cache was already gone, so
> the tier-union corpus gate replaces the deep baseline. Registration stays on
> the legacy path (deferred). Don't upload `publish/` until Plan 3.

## Global Constraints

- Solver hot path → must re-pass the **`corpus_bench` gate** (14/14 truth, 0 wrong,
  per-frame RMS ≤ 1.15× + 0.05, median ≤ 1.05×, 0 panics, no net-speed regression).
  Run QUIET; `cone_calls` is the deterministic speed metric (wall-clock is noisy).
- **One-layer behavior must be byte-identical** to today's `deep_only` (the
  migration + safety baseline).
- Layers are **disjoint** → union = concatenation, no dedup; the union stays
  mag-coherent because layers are consecutive magnitude bands.
- Verify (`NR` count) uses the **full union**; quad-match uses **base + fallback**.
- `catalog_mag_limit` stays 19.0 (`service.rs:141`) — the solver caps cones at
  mag 19 regardless of tier depth.
- Don't name ASTAP in code/comments.

## Deviations applied at execution (2026-06-30)

Per the maintainer's call, **only the plate-solve path is migrated to tiers;
registration is left for a later pass.** This made the change smaller and safer
than the original task list:

- **`Caches` is an enum superset, not a slice-only struct.** `Caches::Legacy {
  deep, bright }` (today's behaviour) + `Caches::Layered { layers: &[&StarCache] }`
  (new). `deep_only`/`tiered` stay (→ `Legacy`), so `registration/service.rs`, both
  `registration.rs` routes, `solvemyastro/src/main.rs`, `tests/scale_fallback.rs`,
  the `cache.rs` tests, and the `corpus_bench` legacy path are **untouched**. Only
  `plate_solve/service.rs` (+ Tauri/Web `plate_solve.rs`) switch to `Caches::layered`.
- **`cone_for_quad_match` is kept** (not deleted). `SearchCtx` carries a `ConeSource`
  enum (`Legacy{deep,deep_pc,bright,bright_pc}` / `Layered(&LayeredCones)`); the two
  cone call sites match on it. The legacy single-cache path stays byte-identical and
  serves as the corpus_bench baseline; the new `Caches::layered(&[&deep])` 1-layer
  path is checked equal to it (Task 3 Step 1).
- **Plan bug:** `Caches::single` via `std::slice::from_ref(cache)` yields `&[StarCache]`
  but the field is `&[&StarCache]` → would not compile. The enum removes the need for
  `single`; `deep_only` already covers the byte-identical single case.
- **Task 5 install:** keep the existing `smac_gaia/stars.smac` alongside the new
  `smac_gaia/tier_*/` so `discover_layers` prefers tiers (solver) while registration's
  bare-`stars.smac` open keeps working until its own migration.

## File Structure

- `solvemyastro/src/lib.rs` — `Caches` becomes a layer stack (`single`/`layered`).
- `solvemyastro/src/cache.rs` — `LayeredCones` (per-solve layer stack + pixel caches + `cone`).
- `solvemyastro/src/orchestrate.rs` — build `LayeredCones` in `solve_inner`;
  `SearchCtx` holds it (replacing `cache`/`pixel_cache`/`bright_cache`/`bright_pixel_cache`);
  rewrite the two call sites; delete `cone_for_quad_match`.
- `crates/athenaeum-core/src/plate_solve/` — tier discovery + migration → `Caches::layered`.

---

### Task 1: `Caches` stack + `LayeredCones`

**Files:**

- Modify: `solvemyastro/src/lib.rs:48-72` (Caches)
- Modify: `solvemyastro/src/cache.rs` (add `LayeredCones`)
- Test: inline in `cache.rs`

**Interfaces:**

- Produces: `Caches<'a> { layers: &'a [&'a StarCache] }` with `Caches::single(&StarCache)`
  and `Caches::layered(&[&StarCache])`; `LayeredCones<'a>` with
  `LayeredCones::new(layers: &'a [&'a StarCache], epoch: f64) -> Self`,
  `.cone(depth: usize, ra: f64, dec: f64, radius_deg: f64, mag_limit: f32, epoch: f64) -> Result<Vec<CatalogStar>>`,
  `.n_layers() -> usize`.

- [ ] **Step 1: Write the failing test** (in `cache.rs` tests module)

```rust
#[test]
fn layered_cone_unions_disjoint_layers() {
    let tmp = TempDir::new().unwrap();
    // layer 0: one bright star near (10,20); layer 1: one fainter star same area.
    let l0 = tmp.path().join("l0");
    let l1 = tmp.path().join("l1");
    let mk = |dir: &std::path::Path, mag: f32| {
        build_cache(
            vec![StarRecord { ra: 10.0, dec: 20.0, mag, pmra_mas_yr: 0.0, pmdec_mas_yr: 0.0 }],
            dir, 2016.0, |_| {},
        ).unwrap();
    };
    mk(&l0, 8.0);
    mk(&l1, 12.0);
    let c0 = StarCache::open(&l0).unwrap();
    let c1 = StarCache::open(&l1).unwrap();
    let layers = [&c0, &c1];
    let lc = LayeredCones::new(&layers, 2016.0);
    // depth 1 = base only; depth 2 = union of both.
    assert_eq!(lc.cone(1, 10.0, 20.0, 1.0, 19.0, 2016.0).unwrap().len(), 1);
    assert_eq!(lc.cone(2, 10.0, 20.0, 1.0, 19.0, 2016.0).unwrap().len(), 2);
    assert_eq!(lc.n_layers(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p solvemyastro layered_cone_unions_disjoint_layers`
Expected: FAIL — `LayeredCones` not defined.

- [ ] **Step 3: Implement `LayeredCones`** (in `cache.rs`, after `PixelCache`)

```rust
/// Per-solve view over an ordered stack of catalog layers (base → deepest).
/// `cone(depth, ..)` unions the first `depth` layers; since layers are disjoint
/// magnitude bands the union is a plain concatenation (no dedup), and each
/// layer keeps its own per-solve `PixelCache`.
pub struct LayeredCones<'a> {
    layers: &'a [&'a StarCache],
    pixel_caches: Vec<PixelCache>,
}

impl<'a> LayeredCones<'a> {
    pub fn new(layers: &'a [&'a StarCache], epoch: f64) -> Self {
        let pixel_caches = layers.iter().map(|_| PixelCache::new(epoch)).collect();
        Self { layers, pixel_caches }
    }

    pub fn n_layers(&self) -> usize {
        self.layers.len()
    }

    /// Union the cone across the first `depth` layers (clamped to the stack).
    pub fn cone(
        &self,
        depth: usize,
        ra: f64,
        dec: f64,
        radius_deg: f64,
        mag_limit: f32,
        epoch: f64,
    ) -> Result<Vec<CatalogStar>> {
        let depth = depth.min(self.layers.len());
        let mut out: Vec<CatalogStar> = Vec::new();
        for i in 0..depth {
            let part =
                self.layers[i].cone_cached(&self.pixel_caches[i], ra, dec, radius_deg, mag_limit, epoch)?;
            out.extend(part);
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Generalize `Caches`** (`lib.rs:48-72`) — replace the struct + impl:

```rust
/// An ordered stack of catalog layers (base → deepest, disjoint magnitude
/// bands). Quad matching uses the base layer with fallback to the full union;
/// verify uses the full union. `Copy` — just a slice of references.
#[derive(Clone, Copy)]
pub struct Caches<'a> {
    pub layers: &'a [&'a StarCache],
}

impl<'a> Caches<'a> {
    /// Single-layer mode (e.g. a legacy single `stars.smac`). Byte-identical to
    /// the pre-tier single-cache behaviour: quad-match and verify both use it.
    pub fn single(cache: &'a StarCache) -> Self {
        Self { layers: std::slice::from_ref(cache) }
    }

    /// Layered mode — `layers[0]` is the fast base, the union is the full depth.
    pub fn layered(layers: &'a [&'a StarCache]) -> Self {
        Self { layers }
    }
}
```

Add `LayeredCones` to the re-export: `lib.rs:31` becomes
`pub use cache::{LayeredCones, PixelCache, StarCache, StarRecord};`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p solvemyastro layered_cone_unions_disjoint_layers`
Expected: PASS.

- [ ] **Step 6: Commit** (solvemyastro submodule)

```bash
cd solvemyastro && git add src/lib.rs src/cache.rs
git commit -m "feat(cache): Caches layer stack + LayeredCones union cone"
```

---

### Task 2: Thread `LayeredCones` through `solve_inner` + the two call sites

**Files:**

- Modify: `solvemyastro/src/orchestrate.rs` (`SearchCtx`, `solve_inner`, quad-match
  `:1293`, verify `:1442`, delete `cone_for_quad_match` `:267-287`)

**Interfaces:**

- Consumes: `LayeredCones`, `Caches { layers }` from Task 1.
- Produces: `SearchCtx.layered: &'a LayeredCones<'a>` (replacing `cache`,
  `pixel_cache`, `bright_cache`, `bright_pixel_cache`).

- [ ] **Step 1: Build `LayeredCones` in `solve_inner`** — replace the PixelCache
  construction (`orchestrate.rs:1019-1025`):

```rust
    let timing = TimingProfile::default();
    let layered = LayeredCones::new(caches.layers, obs_epoch);
```

Delete `let pixel_cache = …;` and `let bright_pixel_cache = …;`. Also delete the
`let cache = caches.deep;` line near `:868` (no longer needed).

- [ ] **Step 2: Update `SearchCtx`** (`:1160-1171`) — replace the four fields:

```rust
    /// Per-solve layer stack + pixel caches. Quad-match uses the base layer with
    /// fallback to the full union; verify uses the full union.
    layered: &'a LayeredCones<'a>,
```

(remove `cache`, `pixel_cache`, `bright_cache`, `bright_pixel_cache`).

- [ ] **Step 3: Update the `SearchCtx { … }` construction** (`:1044-1070`) — replace
  the four fields with `layered: &layered,`.

- [ ] **Step 4: Rewrite the quad-match cone** (`:1293-1299`) — replace the
  `cone_for_quad_match(…)` call:

```rust
                    let stars = {
                        let base = layered.cone(1, cell.0, cell.1, cone_radius,
                                                cfg.catalog_mag_limit, obs_epoch);
                        match base {
                            Ok(b) if b.len() >= cfg.bright_fallback_threshold => Ok(b),
                            // Base too sparse (or errored) → union the full stack.
                            _ => layered.cone(layered.n_layers(), cell.0, cell.1, cone_radius,
                                              cfg.catalog_mag_limit, obs_epoch),
                        }
                    };
                    let stars = match stars {
                        Ok(s) => s,
                        Err(_) => {
                            ct.stop = CellStop::ConeTooSparse;
                            cells_mutex.lock().unwrap().push(ct);
                            return None;
                        }
                    };
```

Here `layered` is read from `ctx.layered` (wherever the trial closure destructures
`SearchCtx` — replace the `cache, pixel_cache, bright_cache, bright_pixel_cache`
bindings with `let layered = ctx.layered;`).

- [ ] **Step 5: Rewrite the verify cone** (`:1442`) — replace `cache.cone_cached(...)`:

```rust
                    let res = layered.cone(layered.n_layers(), seed_ra, seed_dec,
                                           verify_radius, mag_lim, obs_epoch);
```

- [ ] **Step 6: Delete `cone_for_quad_match`** (`:267-287`) and any now-unused
  imports it referenced.

- [ ] **Step 7: Build + fix the borrow/field fallout**

Run: `cargo build -p solvemyastro`
Expected: compiles. Fix every reference to the removed `SearchCtx` fields to use
`ctx.layered` / the local `layered`.

- [ ] **Step 8: Run the unit + integration tests**

Run: `cargo test -p solvemyastro`
Expected: PASS (existing solver unit tests; `registration_e2e` stays `#[ignore]`).

- [ ] **Step 9: Commit**

```bash
cd solvemyastro && git add src/orchestrate.rs
git commit -m "refactor(solve): query LayeredCones (base+fallback / union verify)"
```

---

### Task 3: `corpus_bench` gate — single-layer baseline + 2-layer union

**Files:**

- Test only: `solvemyastro/tests/corpus_bench.rs` (local-only harness; needs the
  `smac_gaia` cache + frames)

**Interfaces:**

- Consumes: the Task 2 solver.

- [ ] **Step 1: Run corpus_bench (single-layer = baseline)**

Run: `cargo test -p solvemyastro --test corpus_bench --release -- --nocapture` (QUIET)
Expected: 14/14 truth, 0 wrong, **`cone_calls` unchanged vs the pre-Plan-2 baseline**
(single layer must be byte-identical). If `cone_calls` drifts, the 1-layer path
diverged — fix before proceeding.

- [ ] **Step 2: Build a 2-tier test cache from the real bins**

Run:
```bash
cargo run -p catalog-builder --release -- --gaia-dir /Volumes/isos/gaia \
  --work-dir /Volumes/BigMac/Users/astrobureau/catalog_build \
  --out /tmp/two_tier --skip-download --no-zip
# /tmp/two_tier/tier_500 + tier_2000 are the base+Δ1 layers
```
Expected: tier dirs built.

- [ ] **Step 3: Confirm a known frame solves on the 2-layer stack**

Add a temporary `#[test]` (or extend corpus_bench) that opens `tier_500` + `tier_2000`,
builds `Caches::layered(&[&c500, &c2000])`, and solves one known corpus frame:

```rust
let c500 = StarCache::open("/tmp/two_tier/tier_500").unwrap();
let c2000 = StarCache::open("/tmp/two_tier/tier_2000").unwrap();
let caches = Caches::layered(&[&c500, &c2000]);
let sol = solve(Path::new(KNOWN_FRAME), &hints, &caches, &cfg, None).unwrap();
assert!(sol.rms_residual_px < 1.5);
```
Expected: solves with low RMS — proves the union path works on real tiers.

- [ ] **Step 4: Commit** (if the harness changed; else skip)

```bash
cd solvemyastro && git add tests/ && git commit -m "test(corpus): 2-layer union solve check"
```

---

### Task 4: `athenaeum-core` — tier discovery + migration

**Files:**

- Modify: `crates/athenaeum-core/src/plate_solve/service.rs:148-151` + the cache
  resolution in `crates/athenaeum-tauri/src/commands/plate_solve.rs` and
  `crates/athenaeum-web/src/routes/plate_solve.rs` (both backends).
- Create: `crates/athenaeum-core/src/plate_solve/layers.rs` (discovery helper).

**Interfaces:**

- Consumes: `solvemyastro::{Caches, StarCache}`.
- Produces: `pub fn discover_layers(catalog_root: &Path) -> Vec<PathBuf>` — ordered
  `tier_<density>/` dirs by ascending density; falls back to `[catalog_root]` if a
  legacy `stars.smac` is present and no tiers.

- [ ] **Step 1: Write the failing test** (`layers.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_tiers_in_density_order_else_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in ["tier_2000", "tier_500", "tier_8000"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("stars.smac"), b"x").unwrap();
        }
        let got = discover_layers(root);
        let names: Vec<_> = got.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["tier_500", "tier_2000", "tier_8000"]);

        // legacy fallback: a bare stars.smac, no tiers
        let leg = tempfile::tempdir().unwrap();
        std::fs::write(leg.path().join("stars.smac"), b"x").unwrap();
        assert_eq!(discover_layers(leg.path()), vec![leg.path().to_path_buf()]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core discovers_tiers_in_density_order`
Expected: FAIL — `discover_layers` not defined.

- [ ] **Step 3: Implement `discover_layers`** (`plate_solve/layers.rs`)

```rust
//! Discover the installed density-tier catalog dirs (or a legacy single cache).

use std::path::{Path, PathBuf};

/// Ordered `tier_<density>/` dirs (ascending density) under `catalog_root`.
/// Falls back to `[catalog_root]` when no tiers exist but a legacy
/// `stars.smac` is present (so old installs keep solving).
pub fn discover_layers(catalog_root: &Path) -> Vec<PathBuf> {
    let mut tiers: Vec<(u32, PathBuf)> = match std::fs::read_dir(catalog_root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let density: u32 = name.strip_prefix("tier_")?.parse().ok()?;
                if e.path().join("stars.smac").is_file() {
                    Some((density, e.path()))
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    tiers.sort_by_key(|(d, _)| *d);
    if !tiers.is_empty() {
        return tiers.into_iter().map(|(_, p)| p).collect();
    }
    if catalog_root.join("stars.smac").is_file() {
        return vec![catalog_root.to_path_buf()];
    }
    Vec::new()
}
```

Register the module: add `pub mod layers;` to `plate_solve/mod.rs`.

- [ ] **Step 4: Wire discovery into the solve path** (`service.rs:148-151`)

The caller (Tauri/Axum cache resolution) currently opens `smac_gaia` (deep) and
optional `smac_gaia_bright`. Change it to: `let dirs = discover_layers(catalog_root)`
where `catalog_root = <app-data>/catalogs/smac_gaia`; open each as `StarCache`;
build `Caches::layered(&refs)` (or `Caches::single` for the 1-element legacy case,
which `layered` already handles). Replace `service.rs:148-151`:

```rust
    // `layer_caches` (Vec<StarCache>) is opened by the caller from
    // discover_layers(); `layer_refs` is the &[&StarCache] view.
    let caches = solvemyastro::Caches::layered(layer_refs);
```

Update both `crates/athenaeum-tauri/src/commands/plate_solve.rs` and
`crates/athenaeum-web/src/routes/plate_solve.rs` to open the discovered dirs
(keeping the existing `<app-data>/catalogs/smac_gaia` root resolution), and drop
the separate `smac_gaia_bright` resolution.

- [ ] **Step 5: Run tests + both-backend build**

Run: `cargo test -p athenaeum-core layers && cargo build -p athenaeum-tauri -p athenaeum-web`
Expected: PASS + both backends compile.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/plate_solve crates/athenaeum-tauri crates/athenaeum-web solvemyastro
git commit -m "feat(plate-solve): discover tier layers (legacy single fallback), pass Caches::layered"
```

---

### Task 5: End-to-end on the real tiers

**Files:** none (validation)

- [ ] **Step 1: Install the built tiers into the app-data catalog dir**

Run:
```bash
APP="$HOME/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia"
rm -rf "$APP"; mkdir -p "$APP"
cp -R /Volumes/BigMac/Users/astrobureau/catalog_out/tier_* "$APP/"
```
Expected: `tier_500 … tier_8000` under the catalog dir.

- [ ] **Step 2: Solve a known small/sparse field via the app/web backend**

Run the plate-solve on a representative frame (desktop dev, or the `athenaeum-web`
solve route). Expected: solves; the base tier carries most cells, the deeper
tiers carry small/sparse fields.

- [ ] **Step 3: Re-run `corpus_bench` against the installed tiers (4-layer)**

Run: `cargo test -p solvemyastro --test corpus_bench --release -- --nocapture`
(point its catalog at the 4-tier dir) Expected: 14/14 truth, 0 wrong, no net-speed
regression (`cone_calls` within budget). This is the real-data gate for Plan 2.

## Self-Review

- **Spec coverage (§3):** `LayeredCatalog`/union → Task 1 (`LayeredCones.cone`);
  `Caches` stack → Task 1; the two call sites (base+fallback / union verify) →
  Task 2; migration (legacy single = 1 layer) → Task 1 (`single`) + Task 4
  (discovery fallback); discovery in athenaeum-core → Task 4; corpus_bench gate →
  Tasks 3 + 5. mmap memory model is unchanged (StarCache/PixelCache untouched).
- **Placeholders:** none — every step has concrete code or an exact command.
- **Type consistency:** `LayeredCones::new(layers, epoch)` / `.cone(depth, …)` /
  `.n_layers()` used identically in Tasks 1–2; `Caches { layers }` / `single` /
  `layered` consistent across Tasks 1, 4; `discover_layers(&Path) -> Vec<PathBuf>`
  consistent in Task 4.

## Risks

- **Hot-path refactor (Task 2)** is the delicate one — the corpus_bench
  `cone_calls` baseline (Task 3 Step 1) is the guard that the 1-layer path didn't
  regress. Do Task 3 immediately after Task 2.
- **SearchCtx field removal** ripples to `run_search` and the trial closure; Step 7
  is "fix all fallout" — expect several mechanical edits.

## Follow-on

- **Plan 3 — app:** `download_catalog_layers` and `get_catalog_status` (two
  backends) and the FOV helper. Then Phase 4 — upload `publish/` to artfrom.space.
