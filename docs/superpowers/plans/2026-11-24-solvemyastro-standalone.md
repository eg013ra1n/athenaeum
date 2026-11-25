# solvemyastro standalone Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repository the `lean-execution` skill overrides subagent-driven-development where they conflict (fresh agent per phase, ledger, disk rule §7).

**Goal:** solvemyastro holds all plate-solve functionality that is not about Athenaeum's frame catalog (Gaia ingest from a local folder, tiers, index packs, packaging, layout, manifests, acceptance gate), gets a clap CLI over the whole chain, and is published on crates.io as 0.3.0; Athenaeum's core and `crates/catalog-builder` lose that code.

**Architecture:** One crate, three feature layers: base (solver + the run-time pieces the app needs), `catalog` (building catalogs from Gaia), `cli` (the binary). Athenaeum depends on base only. Code moves verbatim except for one intended change (SA-6: a total sort order in bins); the real-data acceptance proves the move on three Gaia DR3 files.

**Tech Stack:** Rust 1.96.1, clap 4.6 derive, tracing-subscriber 0.3, flate2, md-5, zip 8, sha2, serde/serde_json, cdshealpix 0.9.1, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-24-solvemyastro-standalone-design.md` (owner-approved 2026-11-24). Ids SA-1 … SA-11 are the spec's.

## Global Constraints

- No numeric change in solving, tiers, index packs or the gate. The only intended change: bins sort by `(mag_raw, ra.to_bits(), dec.to_bits(), pmra_raw, pmdec_raw)` (SA-6).
- No on-disk format change: `healpix_NNNNNN.bin` (14-byte records), `stars.smac`, `quads.smaq` (`index::FORMAT_VERSION` 3), `manifest.json` (version 1), `index_manifest.json` (version 1, format 3). Athenaeum's install paths unchanged.
- Dependency versions = Athenaeum's lock (one version per shared crate): `clap` 4.6.4 (`derive`), `tracing-subscriber` 0.3.23 (`env-filter`, `json`), `flate2` 1.0, `md-5` 0.11, `zip` 8 (`default-features = false`, `deflate`, `time`), `sha2` 0.11, `serde` 1 (`derive`), `serde_json` 1, `cdshealpix` 0.9 (locks 0.9.1, moves together with core's). `rustafits` stays `"1.9.1"` by version.
- Features: `default = []`; `catalog = ["dep:flate2", "dep:md-5", "dep:zip", "dep:sha2"]`; `cli = ["catalog", "dep:clap", "dep:tracing-subscriber"]`; `[[bin]] required-features = ["cli"]`. Tests needing a layer declare `required-features`.
- Athenaeum's `athenaeum-core`, `athenaeum-tauri`, `athenaeum-web` depend on solvemyastro's base layer only; `cargo tree -p athenaeum-core -e normal --prefix none | grep -E '^(clap|md-5|tracing-subscriber) '` prints nothing that solvemyastro brought (tracing-subscriber may already be there from core itself: compare with the base commit).
- No third-party program (solver, stacker, processing program) named anywhere in Athenaeum or in solvemyastro's published sources; the comparison program is "the ERP". Crediting a format or data source (FITS, XISF, HEALPix, ESA Gaia) is allowed.
- Athenaeum production code: `tracing` only, no `println!`/`eprintln!`; never swallow an error (log, then return). The solvemyastro binary prints results to stdout; its progress goes through `tracing`.
- Tests pass on macOS, Linux and Windows; test names use letters, digits, `_` and `-` only; no test compares a bare path with log text.
- Disk: job data under `~/.athenaeum-acc/sma-standalone/` with a `REGISTRY.tsv` row; cargo targets only `target`, `target-agent`, `.superpowers/target-acc`; deleted once results are in the ledger.
- solvemyastro: work on branch `standalone`, merged into its `main` with a merge commit (never a squash); Athenaeum pins only commits on solvemyastro `main`; the pin is pushed before Athenaeum's merge.
- Commits: author `eg013ra1n`, with the session's `Co-Authored-By` and `Claude-Session` trailers, in both repositories.
- Outward actions need the owner's word, grouped at the approval points in Tasks 10–11: any push to GitHub, solvemyastro `main`, Athenaeum's remote `main`, the tag, crates.io.

## Review Focus

1. **Re-run after a killed ingest:** stale `<out>/.scratch/` would be appended to and double every star. Expected: ingest clears its scratch at start and refuses an output folder that already holds `healpix_*.bin` or `coverage.json`. Test: Task 4 `stale_scratch_is_cleared_and_filled_output_is_refused`.
2. **A truncated `.csv.gz`** (the user downloads the files now): expected failure naming the file, no `coverage.json`, exit 1. Test: Task 4 `a_truncated_gzip_fails_with_its_name_and_no_coverage`.
3. **A renamed Gaia file** (name without `GaiaSource_NNNNNN-NNNNNN`): it is still ingested, but completeness becomes "unknown", never a false "complete". Test: Task 4 `a_file_without_a_range_makes_coverage_unknown`.
4. **A tier folder with only `tier_500`** for `build-index --preset narrow` or `package`: a clear error naming the missing tier, or a manifest listing only the tiers present. Test: Task 5 `narrow_preset_names_the_missing_tier`, `package_lists_only_present_tiers`.
5. **Paths with spaces** (the app-data path is `Application Support`): every command works. Test: Task 7 `cli_chain_runs_in_a_folder_with_a_space`.

## Plan calls (refinements of the spec, for the owner's check)

| Id | Call | Why |
| ---- | ---- | ---- |
| PC-1 | `bins` (the 14-byte format) lives in base, not `catalog`. | It has no dependencies, and base's `build_cache_from_legacy_dir` and the tiers read it. |
| PC-2 | The `corpus_layered_tiers` gate is replaced by a diff guard: no solving file changes (Task 2 Step 6). If one does, the corpus gate becomes mandatory (`scripts/fetch_bench_data.sh` into the job folder). | The solver's code is not touched. The bench frames are not on the dev Mac (`solvemyastro/tests/files` is empty), and only `tier_500`/`tier_2000` are installed. |
| PC-3 | The one-off identity check on app-data is replaced by Athenaeum's identity tests, kept unchanged. The discovery functions move verbatim with their tests. | `catalog_identity`/`index_identity` do not change; only their input moves. |
| PC-4 | solvemyastro commits `Cargo.lock` (dropped from `.gitignore`) and `rust-toolchain.toml` (channel `1.96.1`), as starloom does. | So `cargo install --locked` and CI `--locked` mean something. |
| PC-5 | The crates.io secret is named `CARGO_TOKEN` (rustafits' name). | One name across the owner's repos. |
| PC-6 | `coverage.json` is written last, so it marks a finished ingest. Without it, `build-tiers` warns and goes on. | Old bins folders stay usable, and an interrupted ingest is visible. |
| PC-7 | `GateStage` is removed. `Gate` has no "enabled" flag: the app checks `blind_gate_enabled`, the CLI has `--no-gate`. The gate's tolerance function is `accept::gate_tol_px`. | The app passes `ScaleCleared` for every solve, and the name `adaptive_tol_px` is already taken by matching's function. |
| PC-8 | CLI `solve --tiers` finds tiers with `layout::discover_layers` (any `tier_<n>`, legacy root) instead of its fixed four names. | One layout rule for the app and the CLI. |
| PC-9 | Index-pack acceptance compares with the old command's output only. | `smac_index/` on the dev Mac holds no installed pack (manifest only). |

## Setup (controller, before Task 1)

- The main checkout is on branch `starloom-p2` (8 commits ahead of `main`, rustafits 1.10.0, its own lock changes), possibly with a live session (`claude agents` / ListAgents first). This job never touches the main checkout.
- Base commit `BASE` = Athenaeum `main` at job start (`ff7e0ca3` on 2026-11-24). Work happens in a worktree on branch `solvemyastro-standalone` (lean-execution §7; worktrees with submodules are removed with `--force` only). The worktree's own `solvemyastro/` checkout carries the `standalone` branch.
- **Ordering with starloom p2:** whichever branch merges into `main` second first merges `main` in, resolves `Cargo.lock` and the submodule pointers, and re-runs its full gates (here: Task 10 Step 2 and Task 9's §4.6 checks that depend on the lock, i.e. the `cmp` of tiers and packs). The dependency versions in Global Constraints were checked against `main`'s lock (clap 4.6.4, tracing-subscriber 0.3.23, md-5 0.11.0, zip 8.6.0, sha2 0.11.0, flate2 1.1.9).
- Ledger `.superpowers/sdd/2026-11-24-sma-standalone/progress.md`.
- Build the old tools at `BASE` before anything changes, and keep them for Task 9: `cargo build --release -p catalog-builder --target-dir .superpowers/target-acc`, then copy the binary to `~/.athenaeum-acc/sma-standalone/bin/catalog-builder-base` and add the `REGISTRY.tsv` row.

| Task | Repo | Deliverable |
| ---- | ---- | ---- |
| 1 | solvemyastro | features, lock, toolchain; `healpix`, `bins`, `manifest`, `layout` |
| 2 | solvemyastro | `accept`; diff guard |
| 3 | Athenaeum | core on the base modules; core's copies deleted |
| 4 | solvemyastro | `gaia` ingest, coverage, MD5 check |
| 5 | solvemyastro | `tiers`, `index::Preset`, `package` |
| 6 | Athenaeum | catalog-builder, Gaia code, examples, deps removed |
| 7 | solvemyastro | clap CLI, README, `QUAD_INDEX.md`, CHANGELOG, 0.3.0 |
| 8 | solvemyastro | CI + release workflows, name scan, package check |
| 9 | both | real-data acceptance on three Gaia files |
| 10 | Athenaeum | docs, `CLAUDE.md`, memory; Windows run; merges |
| 11 | solvemyastro | tag, crates.io, verification, pin |

---

### Task 1: base layer — features, `healpix`, `bins`, `manifest`, `layout`

**Files:**
- Modify: `solvemyastro/Cargo.toml` (features and deps from Global Constraints; `serde`, `serde_json` to `[dependencies]`; `[[bin]] required-features = ["cli"]`), `solvemyastro/.gitignore` (drop `Cargo.lock`), `solvemyastro/src/lib.rs` (`pub mod healpix; pub mod bins; pub mod manifest; pub mod layout;`)
- Create: `solvemyastro/rust-toolchain.toml` (`channel = "1.96.1"`), `solvemyastro/Cargo.lock` (generated), `solvemyastro/src/healpix.rs`, `src/bins.rs`, `src/manifest.rs`, `src/layout.rs`
- Source of the moves: `crates/athenaeum-core/src/catalog/healpix.rs`, `catalog/binary_format.rs`, `catalog/manifest.rs`, `plate_solve/layers.rs:20-46` and `:152-170`
- Temporary: until Task 7 rewrites it, `src/main.rs` keeps compiling only under `cli`. Add the `clap`/`tracing-subscriber` optional deps now; the old `std::env::args` main stays as it is.

**Interfaces (Produces):**
- `solvemyastro::healpix`: verbatim from core: `HEALPIX_DEPTH: u8 = 6`, `n_pixels() -> u64`, `sky_to_pixel(ra_deg: f64, dec_deg: f64) -> u64`, `cone_search_pixels(ra, dec, radius) -> Vec<u64>`, `cone_pixels_at(depth: u8, ra, dec, radius) -> Vec<u64>`, `n_pixels_at_depth(depth: u8) -> u64`, `pixel_center(depth: u8, pixel: u64) -> (f64, f64)`, `sub_pixels_at_depth6`, `region_sub_pixels`. `cache.rs`'s private `sky_to_pixel` is NOT replaced (PC-2).
- `solvemyastro::bins`: `RECORD_SIZE: usize = 14`. `BinRecord { ra: f32, dec: f32, mag_raw: u16, pmra_raw: i16, pmdec_raw: i16 }` (core's `StarRecord`, renamed so it does not clash with `cache::StarRecord`) with `mag()`, `pmra_mas_yr()`, `pmdec_mas_yr()`, `read_from`, `write_to`, `from_values` verbatim. `read_records_until_mag(data: &[u8], mag_limit: f32) -> Vec<BinRecord>`. `write_records<W: Write>(w: &mut W, records: &mut [BinRecord]) -> io::Result<()>` sorting by the SA-6 key. `bin_file_name(pixel: u64) -> String` = `format!("healpix_{pixel:06}.bin")`.
- `solvemyastro::manifest`: `Manifest`, `ManifestTier`, `IndexManifest`, `IndexManifestPack` with their `from_json_slice` verbatim; `MANIFEST_FILE = "manifest.json"`, `INDEX_MANIFEST_FILE = "index_manifest.json"`.
- `solvemyastro::layout`: `discover_layers(catalog_root: &Path) -> Vec<PathBuf>` and `discover_index_packs(root: &Path) -> Vec<PathBuf>` verbatim; `tier_dir_name(density: u32) -> String` = `format!("tier_{density}")`; `STARS_FILE = "stars.smac"`.

- [ ] **Step 1:** Move the unit tests first: core's `healpix.rs` tests, `binary_format.rs` tests (`roundtrip`, `mag_conversion`, `proper_motion_conversion`, `early_termination_on_mag`), `manifest.rs` tests (`parses_real_manifest_shape`, `parses_index_manifest_shape`), and `layers.rs` tests `discovers_tiers_in_density_order_else_legacy` and the discovery half of `index_packs_are_discovered_by_name_and_identified_by_stat` (as `index_packs_are_discovered_by_name`). Add:

```rust
#[test]
fn equal_magnitudes_sort_by_the_rest_of_the_record() {
    let a = BinRecord::from_values(10.0, 5.0, 12.345, 0.0, 0.0);
    let b = BinRecord::from_values(9.0, 5.0, 12.345, 0.0, 0.0);
    let c = BinRecord::from_values(9.0, 4.0, 12.345, 0.0, 0.0);
    let mut one = vec![a.clone(), b.clone(), c.clone()];
    let mut two = vec![c, a, b];
    let (mut x, mut y) = (Vec::new(), Vec::new());
    write_records(&mut x, &mut one).unwrap();
    write_records(&mut y, &mut two).unwrap();
    assert_eq!(x, y); // any input order gives the same bytes
}
```

- [ ] **Step 2:** `cargo test -p solvemyastro --lib` (from Athenaeum's worktree root). Expected: the new tests fail to compile (modules missing).
- [ ] **Step 3:** Create the four modules (verbatim moves, plus the SA-6 key in `write_records` and the constants above). Add `BinRecord: Clone, Debug, PartialEq`.
- [ ] **Step 4:** `cargo test -p solvemyastro --lib` passes; `cargo build -p solvemyastro` (base, no bin) and `cargo build -p solvemyastro --features cli` both succeed.
- [ ] **Step 5:** Commit in `solvemyastro/`: `feat: healpix, bins, manifest and layout in the base layer`. Commit Athenaeum's `Cargo.lock` too: the new optional deps appear in it.
- [ ] **Step 6 (standalone lock):** solvemyastro is a member of Athenaeum's workspace, so `cargo` inside `solvemyastro/` uses Athenaeum's lock. Build the standalone lock the way starloom p3 did, after Step 5 so the clone has the new `Cargo.toml`:
  - `git clone <worktree>/solvemyastro ~/.athenaeum-acc/sma-standalone/repo` (with a `REGISTRY.tsv` row) on `standalone`;
  - `cp <worktree>/Cargo.lock repo/Cargo.lock`, then one `cargo metadata --format-version 1 >/dev/null` in `repo/`, which prunes it to solvemyastro's packages at Athenaeum's versions;
  - `cargo test --locked --all-features` passes there;
  - copy `repo/Cargo.lock` into the submodule and commit `build: the standalone Cargo.lock`.

  **Lock rule for the whole job:** whenever solvemyastro's `Cargo.toml` changes its version or its dependencies (Task 7 bumps to 0.3.0), pull into `repo/` and run `cargo metadata` there without `--locked`. Copy the lock back and commit it in the same task. Every standalone check (Tasks 4, 5, 7, 8) pulls into `repo/` and runs `--locked` there.

### Task 2: `accept` — the acceptance gate

**Files:**
- Create: `solvemyastro/src/accept.rs`; Modify: `src/lib.rs` (`pub mod accept;`), `src/orchestrate.rs:150` (`const VERIFY_MAG_LIMIT` → `pub(crate) const`, the only edit to a solving file)
- Source: `crates/athenaeum-core/src/plate_solve/service.rs:560-643` (types, `blind_gate_ok`), `:696-701` (`adaptive_tol_px`), `:272-318` (the in-field count), the gate tests at `:781-798`, the table `blind_gate_table` at `:1173-1300` and the tolerance asserts at `:1311-1317`

**Interfaces (Produces):**
- `pub struct Gate { pub base_tolerance_arcsec: f64, pub rms_max_px_mult: f64, pub min_inlier_ratio: f64, pub inlier_floor: usize, pub scale_sanity_min: f64, pub scale_sanity_max: f64, pub scale_header_tol: f64 }`, with `Default` = `8.0, 2.5, 0.04, 6, 0.05, 60.0, 8.0` (Athenaeum's `PlateSolveConfig` defaults).
- `pub const DENSE_FIELD_MIN_EXPECTED: usize = 100;`
- `pub struct GateMetrics { pub inliers: usize, pub expected_in_fov: usize, pub rms_px: f64, pub tol_px: f64, pub inlier_ratio: f64, pub recovered_scale_arcsec: f64, pub header_scale_arcsec: Option<f64> }`
- `impl Gate { pub fn accepts(&self, m: &GateMetrics) -> bool }`: `blind_gate_ok`'s body with the stage and enabled checks removed, the same order of checks (PC-7).
- `pub fn gate_tol_px(pixel_scale_arcsec: f64, base_arcsec: f64) -> f64`: `adaptive_tol_px` verbatim (clamp [4, 20], 10 at |scale| < 1e-6). Matching's private `orchestrate::adaptive_tol_px` stays untouched and separate.
- `pub fn expected_bright_in_fov(caches: &Caches<'_>, crval: (f64, f64), pixel_scale_arcsec: f64, total_detected: usize, epoch: f64) -> usize`: the radius `min(3.0, scale·√n / 3600 / 2 · 1.41)` when scale > 0 and n > 0, else 1.5; a cone at `orchestrate::VERIFY_MAG_LIMIT` on the deep cache (`Legacy`) or the first layer (`Layered`); a cone error counts 0 and is logged with `tracing::warn!` (today's closure returns 0 silently; the warn is the only addition).

- [ ] **Step 1:** Move every gate test onto `Gate::default().accepts`: the noise/real cases at `:781-798`; the whole `blind_gate_table` (rms, inlier floor, ratio, sparse-field exemption, sanity range, header tolerance), which today runs through `GateStage::FullBlind`, the same path as `ScaleCleared` (`:596`); and the tolerance asserts at `:1311-1317`. Drop only the asserts for `Hinted` and for the disabled gate. Add `gate_defaults_are_the_apps_defaults` (asserts the seven values above), `gate_tol_px_clamps` (0.1″/px → 20, 100″/px → 4, 0 → 10, base 8.0), and `expected_bright_in_fov_uses_the_first_layer_and_the_radius_formula`. That test builds a tiny cache with `build_cache` over records at known offsets from (10°, 20°): mag 10 at 0.30° and at 0.40°, mag 13 at 0.10°. With scale 3.6″/px and `total_detected` 250 000, the radius is 3.6·500/3600/2·1.41 = 0.3525°, so the count is 1. With `total_detected = 0` the radius is 1.5° and the count is 2; the mag-13 star is never counted. As `Layered` with that cache first and an empty second layer, the count is 1.
- [ ] **Step 2:** Run `cargo test -p solvemyastro --lib accept`. Expected: compile failure.
- [ ] **Step 3:** Implement `accept.rs` per the Interfaces.
- [ ] **Step 4:** Tests pass.
- [ ] **Step 5:** **Diff guard (PC-2)**, a script kept in the ledger folder and run here and again in Task 10 Step 2 on the final tree:
  - `git -C solvemyastro diff --stat main...HEAD -- src/orchestrate.rs src/quad.rs src/verify.rs src/sip.rs src/refine.rs src/wcs.rs src/wcs_fit.rs src/project.rs src/select.rs src/nn.rs src/math.rs src/register.rs src/diag.rs src/cache.rs` shows only `orchestrate.rs | 2 +-` (the `pub(crate)`);
  - `git -C solvemyastro diff main...HEAD -- src/index.rs | grep -E '^-[^-]'` prints nothing (additions only: `Preset`, `open_preset_layers`, `catalog_epoch`).

  Record both outputs in the ledger.
- [ ] **Step 6:** Commit: `feat(accept): the acceptance gate and the in-field bright-star count`.

### Task 3: Athenaeum core on the base modules

**Files:**
- Delete: `crates/athenaeum-core/src/catalog/healpix.rs`, `catalog/manifest.rs`
- Modify:
  - `Cargo.lock`
  - `crates/catalog-builder/src/publish.rs:11` and `publish_index.rs:18` (`use solvemyastro::manifest::…`; the crate lives until Task 6)
  - `catalog/mod.rs` (drop `pub mod healpix; pub mod manifest;`; `write_catalog_to_healpix` switches to `solvemyastro::healpix::sky_to_pixel` until Task 6 deletes it)
  - the rest: `catalog/gaia_prebuilt.rs:27-28` and `catalog/index_prebuilt.rs:27-28` (`use solvemyastro::manifest::…`), `catalog/gaia.rs` and `catalog/gaia_bulk.rs` (import `solvemyastro::healpix`; they live until Task 6), `hips/pack.rs:132` (`solvemyastro::healpix::cone_pixels_at`), `plate_solve/layers.rs` (delete the two discovery bodies and their moved tests; `pub use solvemyastro::layout::{discover_index_packs, discover_layers};` so callers keep their paths; identity tests stay unchanged), `plate_solve/config.rs` (new `pub fn acceptance_gate(cfg: &PlateSolveConfig) -> solvemyastro::accept::Gate`), `plate_solve/service.rs` (delete `GateStage`, `BlindGateMetrics`, `blind_gate_ok`, `adaptive_tol_px` and their tests; `acceptance_refusal` builds `GateMetrics` and calls `acceptance_gate(config).accepts`, same signature and same refusal text; `solve_frame_with_hints` calls `solvemyastro::accept::expected_bright_in_fov(caches, solution.wcs.crval, solution.pixel_scale_arcsec, solution.total_detected, obs_epoch)`)

**Interfaces:** Consumes Task 1 and Task 2 items. Produces `plate_solve::config::acceptance_gate`.

- [ ] **Step 1:** Add `plate_solve::config` test `acceptance_gate_maps_every_field`: a config with seven distinct non-default values maps one-to-one onto `Gate`. Add `plate_solve::service` test `acceptance_refusal_keeps_its_decisions`: with `PlateSolveConfig::default()`, the moved noise case is refused with text starting `"Rejected: low confidence ("`, the real case is accepted, and `blind_gate_enabled = false` accepts the noise case.
- [ ] **Step 2:** Run `cargo test -p athenaeum-core --lib plate_solve`. Expected: compile failure.
- [ ] **Step 3:** Make the edits listed under Files.
- [ ] **Step 4:** `cargo test -p athenaeum-core --lib plate_solve catalog hips` passes; `cargo check --workspace` passes; `cargo check -p athenaeum-core --no-default-features` passes; `rg -n "catalog::(healpix|manifest)" crates` finds nothing.
- [ ] **Step 5:** Commit (Athenaeum, with the submodule pointer at the `standalone` HEAD): `refactor(plate_solve): healpix, manifests, layout and the gate come from solvemyastro`.

### Task 4: `gaia` — ingest from a local folder

**Files:**
- Create: `solvemyastro/src/gaia/mod.rs` (`#[cfg(feature = "catalog")] pub mod gaia;` in `lib.rs`), `src/gaia/ingest.rs`, `src/gaia/binner.rs`, `src/gaia/coverage.rs`, `src/gaia/md5_list.rs`; `solvemyastro/tests/common/gaia_fixture.rs`; `solvemyastro/tests/gaia_ingest.rs` (`[[test]] required-features = ["catalog"]`)
- Source: `crates/athenaeum-core/src/catalog/gaia.rs:357-440` (`HealpixBinner`), `gaia.rs:65` (`GAIA_RUWE_MAX`), `gaia_bulk.rs`: `parse_md5_manifest`, `md5_of_file`, `read_manifest_cache`, `ColumnIndex`, `parse_bulk_row`, `ingest_one_file`, `ingest_bulk`, `INGEST_CHUNK_SIZE`, their tests. Not moved: anything that downloads (Task 6 deletes it).

**Interfaces (Produces):**
- `pub const GAIA_RUWE_MAX: f64 = 1.4; pub const DEFAULT_MAG_LIMIT: f32 = 21.0; pub const DEFAULT_JOBS: usize = 4; pub const MD5_LIST_FILE: &str = "_MD5SUM.txt"; pub const COVERAGE_FILE: &str = "coverage.json";`
- `pub struct IngestOptions { pub mag_limit: f32, pub jobs: usize, pub verify_md5: bool }` with `Default` (21.0, 4, false).
- `pub enum IngestProgress { Started { total_files: usize }, Verified { name: String }, FileDone { name: String, completed: usize, total_files: usize, stars_kept: usize }, Finalizing, Complete { total_stars: usize } }`
- `pub struct IngestSummary { pub files: usize, pub stars: usize, pub coverage: Coverage }`
- `pub fn ingest(gaia_dir: &Path, out_bins: &Path, opts: &IngestOptions, progress: &(dyn Fn(IngestProgress) + Sync)) -> anyhow::Result<IngestSummary>`
- `#[derive(Serialize, Deserialize)] pub struct Coverage { pub level: u8 /* 8 */, pub files: Vec<CoveredFile>, pub unknown: Vec<String> }`, `pub struct CoveredFile { pub name: String, pub from: u64, pub to: u64 }`; `impl Coverage { pub fn from_names(names: &[String]) -> Self; pub fn complete_cells(&self) -> Option<Vec<u64>> /* None when unknown is non-empty */; pub fn partial_cells(&self) -> Option<Vec<u64>>; pub fn read(bins_dir: &Path) -> anyhow::Result<Option<Coverage>> }`. A level-6 cell `c` is complete when all of level-8 pixels `16c ..= 16c+15` lie in the union of `[from, to]`; partial when some do.
- Behaviour that the tests pin:
  - Files are listed as today: the `_MD5SUM.txt` order filtered to files present, else a sorted `*.csv.gz` listing. The output does not depend on that order (SA-6).
  - `out_bins` holding any `healpix_*.bin` or `coverage.json` → error "output folder already holds bins: <path>; choose an empty folder". `<out>/.scratch/` is removed at start and at the end.
  - `verify_md5`: with `_MD5SUM.txt` present, every file is checked before ingest, in parallel over `jobs`. A mismatch is an error naming the file. A missing list with `verify_md5` set is an error. The list parser trims `\r`.
  - Any file error (gzip, header, I/O) stops the run with the file's name in the error, and no `coverage.json` is written. `coverage.json` is written last (PC-6).
  - Row filters are verbatim: G must parse and be `< mag_limit`; RUWE `>= 1.4` drops the row, an empty one keeps it; empty or `null` PM reads 0.

- [ ] **Step 1:** Write `tests/common/gaia_fixture.rs`:
  - `fn write_gaia_file(dir: &Path, name: &str, rows: &[Row])` writes a gzip with a `#` preamble and a header naming `source_id,ra,dec,parallax,phot_g_mean_mag,pmra,pmdec,ruwe` in that order, so the column lookup is exercised.
  - `fn star_in_cell(cell: u64, k: u32) -> (f64, f64)` gives distinct positions inside a level-6 cell (around `healpix::pixel_center(6, cell)`, checked with `healpix::sky_to_pixel`).
  - Files: `GaiaSource_000000-000007.csv.gz` and `GaiaSource_000008-000020.csv.gz` (cell 0 complete, cell 1 partial).
  - Cell 0 gets 2 000 generated stars spread over both files, so the tiers split it 420 / 1 259 / 321 / 0. Among them are 20 pairs with equal G, one star of each pair in each file, to exercise SA-6.
  - Cell 1 gets 50 stars, plus the filter cases of Step 2.
- [ ] **Step 2:** Write `tests/gaia_ingest.rs`:
  - `ingest_keeps_filters_and_sorts`: counts per cell; G 21.0 dropped at the default limit; RUWE 1.4 dropped; empty RUWE kept; `null` PM → 0; malformed `ra` dropped; each bin sorted by the SA-6 key.
  - `ingest_is_the_same_for_any_jobs_and_order`: bytes of every bin equal for `jobs` 1 and 4, and with an `_MD5SUM.txt` listing the files in reverse.
  - `coverage_reports_complete_and_partial_cells`: `complete_cells() == Some(vec![0])`, `partial_cells() == Some(vec![1])`.
  - `a_file_without_a_range_makes_coverage_unknown`: adds `mine.csv.gz`; its stars are ingested; `complete_cells() == None`; `unknown == ["mine.csv.gz"]`.
  - `stale_scratch_is_cleared_and_filled_output_is_refused`: a pre-made `.scratch/p_000000.raw` holding one record does not appear in the output; a second run into the same folder errors with "already holds bins".
  - `a_truncated_gzip_fails_with_its_name_and_no_coverage`: the error text contains the file name, and `coverage.json` does not exist.
  - `md5_verify_catches_a_changed_file_and_parses_crlf`: a CRLF list; one file changed after listing → error naming it; `verify_md5` without a list → error.
- [ ] **Step 3:** Run `cargo test -p solvemyastro --features catalog --test gaia_ingest`. Expected: compile failure.
- [ ] **Step 4:** Implement the `gaia` module by moving the listed code. `ingest` replaces `ingest_bulk`'s app-data paths with `out_bins` and `<out_bins>/.scratch`. It keeps the internal first-error cancel flag and has no public cancel parameter. Tracing events: `info!` per file done, `info!` summary, `warn!` before each error return.
- [ ] **Step 5:** The tests pass; the moved unit tests pass (`cargo test -p solvemyastro --features catalog --lib gaia`).
- [ ] **Step 6:** Commit: `feat(gaia): ingest Gaia DR3 files from a folder into bins, with coverage and an MD5 check`.

### Task 5: `tiers`, `index::Preset`, `package`

**Files:**
- Create: `solvemyastro/src/tiers.rs`, `src/package.rs` (both `#[cfg(feature = "catalog")]`), `tests/catalog_build.rs` (`required-features = ["catalog"]`)
- Modify: `solvemyastro/src/index.rs` (add `Preset`, additive only)
- Source: `crates/catalog-builder/src/tiers.rs`, `layers.rs`, `publish.rs`, `publish_index.rs` and their tests

**Interfaces (Produces):**
- `tiers`: `TIER_DENSITIES: [u32; 4] = [500, 2000, 5000, 8000]`, `cell_cum_counts() -> [usize; 5]` (`[0, 420, 1679, 4196, 6714]`), `slice_select`, and `build_tiers(bins_dir: &Path, out_dir: &Path, epoch: f64) -> anyhow::Result<Vec<(u32, usize)>>`. That is `build_layers`, with reads through `bins`. It reads `Coverage::read`: none → `warn!` "no coverage record"; partial cells → `warn!` with their count.
- `index::Preset { Base, Narrow }`: `name() -> &'static str` ("base", "narrow"); `params() -> BuildParams` (`Base` = `BuildParams::default()`, `Narrow` = `{ min_depth: 8, max_depth: 8, stars_per_cell: 10, quads_per_cell: 4 }`); `tier_dirs() -> &'static [&'static str]` (`Base` = `["tier_500"]`, `Narrow` = `["tier_500", "tier_2000"]`); `from_name(&str) -> Option<Preset>`.
- `index::open_preset_layers(tiers_dir: &Path, preset: Preset) -> anyhow::Result<Vec<StarCache>>`: opens every `tier_dirs()` entry; a missing one is an error naming it. Today's no-preset path keeps silently skipping a missing `tier_2000` (`main.rs:271-276`).
- `QuadIndex::catalog_epoch(&self) -> f64`: reads the header field at `index.rs:441`. It is additive and used by `info` and `package_index`.
- `package`:
  - `package_tiers(tiers_dir: &Path, publish_dir: &Path) -> anyhow::Result<Manifest>`: each present `tier_<d>/stars.smac` → `tier_<d>.zip` (entry `tier_<d>/stars.smac`) + `tier_<d>.zip.sha256` + `manifest.json`, via `serde_json::to_vec_pretty`; version 1; `catalog_epoch` from the first tier's header; `min_fov_for` verbatim.
  - `package_index(packs: &[(&str, &Path)], publish_dir: &Path) -> anyhow::Result<IndexManifest>`: `index_<name>.zip` (entry `<name>/quads.smaq`) + sidecar + `index_manifest.json`; version 1; `format = index::FORMAT_VERSION`; `min_fov_deg` from `QuadIndex::min_fov_deg()`; `catalog_epoch` from the packs' `catalog_epoch()` (the value the old tool wrote; packs that disagree are an error).
  - `sha256_file`, `zip_file`, `write_sha256_sidecar` verbatim. Field order and pretty printing are as today, so the manifests come out byte-identical.

- [ ] **Step 1:** Move the tests: `boundaries_match_density_times_cell_area`, the `slice_select` tests, `produces_zip_sha_and_manifest_per_tier`, the publish-index tests. Add:
  - `presets_pin_the_published_parameters`: the values above.
  - `narrow_preset_names_the_missing_tier`: a folder with only `tier_500`; `open_preset_layers(dir, Preset::Narrow)` errors with text containing `tier_2000`.
  - `package_lists_only_present_tiers`: two tiers → two manifest entries.
  - `tiers_from_fixture_bins_are_deterministic`: Task 4's fixture → `build_tiers` twice → identical `stars.smac` bytes; cell 0's records split 420/1259/… by rank.
- [ ] **Step 2:** Run `cargo test -p solvemyastro --features catalog --test catalog_build`. Expected: compile failure.
- [ ] **Step 3:** Implement the moves and `Preset`.
- [ ] **Step 4:** All solvemyastro tests pass with `--all-features`; base `cargo test -p solvemyastro` still passes.
- [ ] **Step 5:** Commit: `feat(catalog): density tiers, index presets and the publish tree`.

### Task 6: Athenaeum — remove what moved or is no longer needed

**Files:**
- Delete:
  - `crates/catalog-builder/` (whole)
  - `crates/athenaeum-core/src/catalog/gaia.rs`, `catalog/gaia_bulk.rs`, `catalog/binary_format.rs`
  - `crates/athenaeum-core/examples/ingest_gaia.rs`, `ingest_gaia_bulk.rs`, `download_gaia_bulk.rs`
- Modify:
  - `Cargo.toml:8` (member line), `Cargo.lock`
  - `docker/Dockerfile:60` (the `sed` line, keeping the `&&` chain valid) and the `:52` comment
  - `crates/athenaeum-core/src/catalog/mod.rs` (only `gaia_prebuilt` and `index_prebuilt` remain; `write_catalog_to_healpix` deleted)
  - `crates/athenaeum-core/Cargo.toml`: remove `md-5`, `flate2` and `byteorder` when `rg -n "md5|Md5|flate2|byteorder" crates/athenaeum-core/src` finds no user. `cdshealpix` moves to `[dev-dependencies]`: `hips/mod.rs:861` is its last user and is a test. It stays at 0.9 in step with solvemyastro.
  - `README.md` (the catalog-builder mention points to solvemyastro's README); the comments in `src/components/plate-solve/cameraPresets.ts:16` and `crates/athenaeum-core/src/catalog/index_prebuilt.rs:6`

- [ ] **Step 1:** Make the deletions and edits.
- [ ] **Step 2:** Run `cargo check --workspace`, `cargo check -p athenaeum-core --no-default-features`, `cargo test -p athenaeum-core --lib catalog plate_solve hips`, `cargo test -p athenaeum-core --test catalog_download_e2e`. All pass. `rg -n "catalog-builder|catalog_builder|gaia_bulk|binary_format" --glob '!docs/superpowers/**' .` shows only docs Task 10 rewrites.
- [ ] **Step 3:** Docker: `docker build -f docker/Dockerfile .` gets past the cargo build stage (run it, or record it as owed in open-items if Docker is not running on the dev Mac).
- [ ] **Step 4:** Commit: `refactor: catalog building leaves Athenaeum (catalog-builder, Gaia ingest and downloads removed)`.

### Task 7: the CLI

**Files:**
- Delete: `solvemyastro/src/main.rs`
- Create: `solvemyastro/src/bin/solvemyastro/main.rs`, `solve.rs`, `catalog.rs` (ingest, build-tiers, build-cache, build-bright-cache, build-index, package), `info.rs`, `log.rs`; `tests/cli.rs` (`required-features = ["cli"]`, runs `env!("CARGO_BIN_EXE_solvemyastro")`)
- Modify: `Cargo.toml` (`[[bin]] path = "src/bin/solvemyastro/main.rs"`, `version = "0.3.0"`, description), `README.md`, `QUAD_INDEX.md`, `CHANGELOG.md`

**Interfaces:**
- Consumes everything from Tasks 1–5.
- Produces the command line in spec §4.5 with these exact flags:
  - `ingest --gaia <dir> --out <dir> [--mag-limit 21] [--jobs 4] [--verify]`
  - `build-tiers --bins <dir> --out <dir> [--epoch 2016.0]`
  - `build-cache (--bins|--from) <dir> --out <dir> [--epoch 2016.0]`
  - `build-bright-cache --from <dir> --out <dir> [--bright-floor 14.0] [--target-density 100] [--mode hybrid|floor-only|cap-only]`
  - `build-index --tiers <dir> --out <dir> [--preset base|narrow | --min-depth --max-depth --stars-per-cell --quads-per-cell]`; `--preset` conflicts with the four flags; without a preset, layers are `tier_500` + `tier_2000` as today.
  - `package [--tiers <dir>] [--index <name>=<dir>]... --out <dir>`
  - `solve <image> (--tiers <dir> | --catalog <dir>) [--index <dir>]... [--ra --dec] [--fov | --scale] [--radius] [--epoch] [--json] [--no-gate]`
  - `info <dir>` (alias `cache-info`)
  - global `--log <file>` and `-q/--quiet`
- `solve --json` prints the existing keys `ra, dec, scale, rot, matched`, plus `rms_px`, `inlier_ratio`, `expected_in_fov` and `gate` (`"passed"` or `"off"`). A refusal prints nothing on stdout; the reason starting `rejected:` goes to stderr; exit 1. The gate's expected scale is `--scale`, else `None`. The epoch for the in-field count is `--epoch`, else the first layer's `catalog_epoch()`.
- Exit codes come from `main() -> ExitCode`: 0 ok, 1 error or refusal (`error: {e:#}` on stderr), 2 clap usage.
- `info` detects what the folder is: a cache (stars, epoch), a tier folder (each tier), an index pack (format, epoch, `min_fov_deg`, quads), or a bins folder (files, records, coverage summary).
- Logging: a stderr fmt layer at `info` (`RUST_LOG` overrides; `-q` → `warn`). `--log` adds a JSON-lines layer to the file. Library progress callbacks become `tracing::info!` events with fields.

- [ ] **Step 1:** Write `tests/cli.rs`:
  - `cli_chain_runs_in_a_folder_with_a_space`: the Task 4 fixture under `tempdir/"a b"/`; `ingest` → `build-tiers` → `build-index --min-depth 2 --max-depth 3` → `package --tiers … --index base=…` → `info` on each. Every exit is 0; `info` on the bins prints `complete cells: 1`; the publish folder holds `manifest.json` and `index_manifest.json`.
  - `ingest_into_a_filled_folder_exits_1`: stderr contains `already holds bins`.
  - `bad_arguments_exit_2`: `build-index --preset base --min-depth 3`.
  - `solve_without_a_catalog_exits_1`.
  - `build_index_without_preset_keeps_todays_layers`: fixture tiers → identical `quads.smaq` bytes to a direct `index::build_index` call over `tier_500` + `tier_2000` with `BuildParams` from the same flags.
- [ ] **Step 2:** Run `cargo test -p solvemyastro --features cli --test cli`. Expected: compile failure.
- [ ] **Step 3:** Implement the binary. Port `build-bright-cache` and `build-cache` bodies from the old `main.rs` as they are. `info` on a bins folder words its report "complete by file range" (Gaia splits files by the HEALPix index inside `source_id`, and bins use the DR3 `ra/dec`, so a star near a cell edge can sit in a neighbouring file). Bump the version to 0.3.0 and refresh the standalone lock (Task 1 lock rule).
- [ ] **Step 4:** Tests pass; `cargo test -p solvemyastro --all-features` passes; in the standalone `repo/` (Task 1 Step 4b), `cargo test --locked --all-features` passes.
- [ ] **Step 5:** Docs:
  - **README:** the CLI chain with one example per command; the install line `cargo install solvemyastro --features cli --locked`; a Gaia data section saying the user downloads DR3 `GaiaSource_*.csv.gz` from ESA and that published catalogs carry ESA's credit line "This work has made use of data from the European Space Agency (ESA) mission Gaia (https://www.cosmos.esa.int/gaia), processed by the Gaia Data Processing and Analysis Consortium (DPAC)".
  - **`QUAD_INDEX.md`:** the published packs are rebuilt with `--preset`.
  - **`CHANGELOG.md`:** a `## [0.3.0] — unreleased` section covering the moved modules, the CLI, SA-6, and the `cargo install` change.
- [ ] **Step 6:** Commit: `feat(cli): one command per stage, from Gaia files to a solved frame`.

### Task 8: CI, release workflow, publication checks

**Files:** Create `solvemyastro/.github/workflows/ci.yml`, `release.yml`.

- **`ci.yml`:** modelled on `starloom/.github/workflows/ci.yml`.
  - Linux matrix: `base: ""`, `catalog: "--features catalog"`, `cli: "--features cli"`, `all: "--all-features"`.
  - macOS and Windows: `--all-features`.
  - All jobs use `cargo test --locked`.
  - A `publish-dry-run` job runs `cargo publish --dry-run --locked`, with no expected-failure branch.
- **`release.yml`:** starloom's (tag vs `Cargo.toml` check, notes from the CHANGELOG section, `gh release create/edit`), plus rustafits' `publish-crates-io` job (`rustafits/.github/workflows/release.yml:132-155`): secret `CARGO_TOKEN`; a version already on crates.io is skipped with a notice; any other failure fails the job.

- [ ] **Step 1:** Write both workflows. If `actionlint` is installed, it reports nothing; otherwise record that the first run is the check (Task 10).
- [ ] **Step 2:** Name scan over what will be published. Join comment lines with `git -C solvemyastro ls-files | xargs cat | tr '\n' ' '`, then grep (case-insensitive) for the names of other plate solvers, stackers and image-processing programs, and for numerical-library and book titles. The pattern list is the one used before starloom's publication (ledger `.superpowers/sdd/2026-11-23-starloom-p3/`); add any solver name found in `solvemyastro/docs/` (local-only). Expected: no hits besides the FITS/XISF/HEALPix/Gaia/ESA credits. Fix hits in comments and record them in the ledger.
- [ ] **Step 3:** In the standalone `repo/`, `cargo package --list` shows no `docs/`, `.claude/`, `.superpowers/` or `tests/files/`, and `Cargo.lock` is present. `cargo publish --dry-run --locked` passes.
- [ ] **Step 4:** Commit: `ci: test every feature layer on three platforms; release to GitHub and crates.io`.

### Task 9: real-data acceptance (controller, one script)

Job folder `~/.athenaeum-acc/sma-standalone/` (`REGISTRY.tsv` row: owner, end date = the merge). The NAS share `/Volumes/isos/gaia` must be mounted; if it is not, ask the owner and wait (no other data). `NEW` = the worktree's `solvemyastro` release binary (`--features cli`, `--target-dir .superpowers/target-acc`); `OLD` = `bin/catalog-builder-base` from Setup. `TIERS` = the release app's `~/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia` (`tier_500` + `tier_2000`). The dev app's `com.vsharifov.athenaeum.dev` holds only `tier_500`, and `publish-index` needs `tier_2000`.

- [ ] **Step 1:** Copy the first three files of `ls /Volumes/isos/gaia | grep '^GaiaSource_' | sort` into `gaia/`. Check that their ranges are contiguous: each file's `from` equals the previous `to` + 1.
- [ ] **Step 2:** `OLD --gaia-dir gaia --out old_out --work-dir old_work --skip-download --no-zip` (defaults: mag limit 21, epoch 2016).
- [ ] **Step 3:** `NEW ingest --gaia gaia --out bins4 --jobs 4` and `NEW ingest --gaia gaia --out bins1 --jobs 1`. `cmp` every file of `bins4` against `bins1`; expected: identical.
- [ ] **Step 4:** A script kept in the scratchpad, run with `python3 -I`:
  - **Multiset check:** for every `healpix_*.bin` in `old_work/catalogs/gaia_dr3/` and `bins4/`, sort the 14-byte chunks and compare. Expected: the same file names and equal multisets.
  - **Order check:** each new bin is non-decreasing in the SA-6 key. Unpack `<HIIhh>` from bytes 8–13, 0–3 and 4–7, i.e. `(mag_raw, ra_bits, dec_bits, pmra_raw, pmdec_raw)`.
- [ ] **Step 5:** Tier isolation:
  - Put a copy of `bins4` at `mix_work/catalogs/gaia_dr3/` and run `OLD --gaia-dir gaia --out mix_out --work-dir mix_work --skip-download --no-zip`. OLD reuses bins only when the folder holds more than 100 `healpix_*.bin` (`crates/catalog-builder/src/main.rs:239-253`). Assert that count, and that OLD's stdout has the line `[2/4] reuse existing bins`; otherwise stop, because it re-ingested.
  - Run `NEW build-tiers --bins bins4 --out new_tiers`.
  - `cmp` each `tier_<d>/stars.smac`. Expected: identical.
- [ ] **Step 6:** Secondary check against the installed tiers:
  - A throwaway crate `tools/cellcmp` under the job folder, with a path dependency on the worktree's solvemyastro, built with `--target-dir .superpowers/target-acc`.
  - It takes the interior cells: complete cells (`Coverage::read(bins4)?.complete_cells()`) whose eight level-6 neighbours (`cdshealpix::nested::neighbours`) are complete too. For each, it compares `iter_pixel_records` of `new_tiers/tier_500` + `tier_2000` with `TIERS/tier_500` + `tier_2000`.
  - Edge cells (complete, with a neighbour outside the range) are compared and reported separately: a missing star there can be one that Gaia's file split put in the next file.
  - It prints counts: cells equal; cells that differ only in records whose magnitude equals the cell's band-edge magnitude (tie-only, SA-6); other differences.
  - Expected: no "other" among interior cells. Any other difference is reported to the owner.
- [ ] **Step 7:** Index packs:
  - Old: `OLD publish-index --tiers "$TIERS" --out old_idx`.
  - New: `NEW build-index --tiers "$TIERS" --out new_idx/base --preset base`, and the same with `narrow`.
  - `cmp` both `quads.smaq`. Expected: identical (PC-9).
- [ ] **Step 8:** Publish tree:
  - `OLD --gaia-dir gaia --out mix_out2 --work-dir mix_work --skip-download` (with zip; assert the reuse line again) against `NEW package --tiers new_tiers --index base=new_idx/base --index narrow=new_idx/narrow --out new_pub`.
  - `cmp manifest.json`. Expected: identical.
  - Compare `index_manifest.json` with the one from Step 7's `old_idx/publish/`. A `size_bytes` difference is accepted only if the unzipped entries are byte-identical; the report says so.
- [ ] **Step 9:** Write the results (each `cmp`, the multiset counts, cellcmp's three counts, timings) to `.superpowers/sdd/2026-11-24-sma-standalone/acceptance.md`, then delete the job folder's data and close the `REGISTRY.tsv` row.

### Task 10: Athenaeum docs, Windows run, merges

**Files:**
- `CLAUDE.md`:
  - Workspace Layout gains a `solvemyastro/` line: the plate solver, its own repository, the feature layers, catalogs built with its CLI.
  - Module Map: `catalog` = the app's downloads of ready tiers and packs; `plate_solve` = the app half.
  - The Plate solving block names solvemyastro for the gate and the catalog layout.
- `docs/platesolving/README.md`: the catalog-building text moves to a pointer to solvemyastro's README and CLI, under "Detail moved"; the input/acceptance gates doc says the thresholds live in `solvemyastro::accept`.
- Memory: `project_platesolve_catalog_delivery.md` (no catalog-builder; CLI commands; bins at G < 21) and `project_solvemyastro.md` (features, CLI, crates.io state). `docs/superpowers/open-items.md` gets any owed smoke (Docker build if not run in Task 6; the first GitHub CI run if it has not happened).

- [ ] **Step 1:** Docs and memory as above; commit `docs: plate solving now lives in solvemyastro`.
- [ ] **Step 2:** Full gates in the worktree:
  - the diff guard from Task 2 Step 5, on the final tree;
  - `cargo test --workspace` (known flakes per memory are re-run once);
  - `cargo test -p solvemyastro --all-features`;
  - `cargo check -p athenaeum-core --no-default-features`.
- [ ] **Step 3 (approval point A):** ask the owner, one question covering:
  - push solvemyastro `standalone` to GitHub and GitLab (CI runs on GitHub);
  - push `wintest/solvemyastro-standalone` to GitLab for the EVO-X2 Windows job.

  Then do both. Expected: GitHub CI green on all jobs, and the Windows job green.
- [ ] **Step 4 (approval point B):** with the owner's word:
  - merge `standalone` into solvemyastro `main` with a merge commit and push it to both remotes;
  - bump Athenaeum's pointer to that merge commit (with `Cargo.lock`);
  - merge `solvemyastro-standalone` into Athenaeum's local `main`;
  - push Athenaeum `main` (GitLab, then GitHub) only on the owner's word;
  - remove the worktree (`--force`).

### Task 11: release 0.3.0 (approval point C)

- [ ] **Step 1:** The owner adds the `CARGO_TOKEN` secret (a crates.io token with the "publish new crates" scope) to `github.com/eg013ra1n/solvemyastro`.
- [ ] **Step 2:** On solvemyastro `main`, give the CHANGELOG section today's date (`## [0.3.0] — YYYY-MM-DD`), commit `release: v0.3.0`, merge-commit rule as above, and tag `v0.3.0`. Pushing the tag may be refused for the session; if so, hand the owner the exact `git push` lines.
- [ ] **Step 3:** Verify by fact:
  - crates.io API `max_version` is `0.3.0`;
  - the GitHub release page shows the CHANGELOG notes;
  - in a temp folder, `cargo install solvemyastro --features cli --locked --root <tmp>` and `<tmp>/bin/solvemyastro --help` lists the eight commands.
- [ ] **Step 4:** Bump Athenaeum's pointer to the release commit, with `Cargo.lock` (solvemyastro 0.2.0 → 0.3.0): `chore: solvemyastro at v0.3.0`. Push on the owner's word.
- [ ] **Step 5:** Ledger closed; the job memory is cut to its non-derivable facts. Follow-ups go to open-items as their own decisions: starloom by crates.io version, WCS output from `solve`, Gaia DR4 (out 2026-12-02).
