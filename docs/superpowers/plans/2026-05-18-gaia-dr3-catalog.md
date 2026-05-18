# Gaia DR3 Catalog Ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development (or executing-plans). Steps use `- [ ]` tracking. DRY/YAGNI/TDD, frequent commits.

**Goal:** Add a Gaia DR3 (G ≤ 16) all-sky star catalog so the plate solver can solve the deep-catalog holdouts (M78, `_DSC5767`) that Tycho-2 physically cannot.

**Architecture:** New `crates/athenaeum-core/src/catalog/gaia.rs` mirroring the proven `tycho2.rs` pipeline, but acquiring data via the ESA Gaia **TAP async** service tiled by `source_id` range (HEALPix level 3 = 768 resumable jobs, server-side `phot_g_mean_mag < 16` + 5-column projection). Streams each tile into an on-disk per-HEALPix-6 binner (300M stars ≈ 4.2 GB — must NOT be held in RAM, unlike Tycho-2's 2.5 M), then sorts each pixel file by magnitude into the existing `StarRecord` format at `catalogs/gaia_dr3/`. `CatalogEngine::with_catalog_dir` already auto-discovers `gaia_dr3/` and `cone_search` already prefers it (epoch 2016.0) — **zero solver changes, no quad index** (the per-trial astap backend builds quads on the fly).

**Tech stack:** Rust, `reqwest` (already used by tycho2), `cdshealpix` (already a dep), the existing `catalog::binary_format::StarRecord` / `write_catalog_to_healpix`.

---

## Context & verified facts

- **Why:** M78/`_DSC5767` are NOT solver bugs (proven 2026-05-18: not saturation, not parity; ASTAP itself fails M78 with shallow V50, solves only with deep D50). They need Gaia-depth faint stars. See auto-memory `project_m78_dsc5767_catalog_depth.md`.
- **Seam (verified, `catalog/mod.rs`):** `with_catalog_dir` sets `gaia_path` iff `catalog_dir/gaia_dr3/` exists; `cone_search` prefers Gaia (epoch **2016.0**, returns `"gaia_dr3"`) over Tycho-2. No other wiring needed.
- **On-disk contract (verified):** `catalog/healpix.rs` depth 6 (49,152 pixels), files `healpix_NNNNNN.bin`, records 14 bytes `[ra f32, dec f32, mag×1000 u16, pmra 0.01mas/yr i16, pmdec 0.01mas/yr i16]`, **sorted by mag ascending per pixel**. `StarRecord::from_values(ra:f32,dec:f32,mag:f32,pmra_mas_yr:f64,pmdec_mas_yr:f64)`. `write_catalog_to_healpix(&[StarRecord], &Path)` bins+sorts+writes (holds all in RAM — unusable as-is for 300M; see Task 4).
- **Gaia facts (verified via ESA docs):** TAP async `https://gea.esac.esa.int/tap-server/tap/async`, anon cap 3M rows/job. Columns `ra,dec` (deg, ICRS @ **J2016.0**), `phot_g_mean_mag` (Vega), `pmra` (mas/yr, **includes cos δ — same convention as Tycho-2 pmRA\***), `pmdec` (mas/yr). `source_id` encodes HEALPix: level-n index = `source_id / (2^35 · 4^(12−n))`; **level-3 ⇒ divisor `2^53`, 768 tiles**, tile `t` ⇔ `source_id ∈ [t·2^53, (t+1)·2^53 − 1]`, ≈390k rows/tile at G≤16 (well under 3M). No magnitude partition in the bulk repo (why TAP-tiled, not bulk-download). No reputable pre-built G≤16 DR3 subset exists.
- **Pattern to mirror (verified, `catalog/tycho2.rs`):** `enum Tycho2Progress { Downloading{file_index,total_files,bytes}, Converting{stars_processed,total_stars}, Complete{total_stars}, Error(String) }`; `download_tycho2(out:&Path, cancel:Arc<AtomicBool>, progress:&dyn Fn(Tycho2Progress))->Result<Vec<PathBuf>>`; `convert_tycho2_to_healpix(gz:&Path,cat:&Path,cancel,progress)->Result<usize>`; `setup_tycho2_catalog(app_data:&Path,cancel,progress)->Result<PathBuf>` (idempotent: skips if `catalogs/tycho2/` has >100 files). Tauri `download_tycho2_catalog` (`commands/plate_solve.rs:768`) + web mirror (`routes/plate_solve.rs:693`, registered `routes/mod.rs:204`).

## Staging reality (set expectations)

- **Tasks 1–7 (code + unit tests):** doable now; fully testable without network via mocked CSV.
- **Task 8 (the real all-sky ingest):** a **multi-hour/overnight** 768-job TAP run the user triggers on their machine — not executed in-session.
- **Task 9 (bench closure):** only after Task 8's data exists — re-run the astap-backend bench; success = M78 **and** `_DSC5767` now solve on `gaia_dr3`.

---

## Task 1: `gaia.rs` scaffold + tile math (pure, TDD)

**Files:** create `crates/athenaeum-core/src/catalog/gaia.rs`; modify `crates/athenaeum-core/src/catalog/mod.rs` (add `pub mod gaia;`).

- [ ] Add `pub mod gaia;` to `catalog/mod.rs` (next to `pub mod tycho2;`).
- [ ] In `gaia.rs`: consts `const GAIA_TAP_ASYNC: &str = "https://gea.esac.esa.int/tap-server/tap/async";`, `const GAIA_MAG_LIMIT: f32 = 16.0;`, `const GAIA_HEALPIX_LEVEL: u32 = 3;`, `const GAIA_TILE_COUNT: u64 = 768;`, `const SOURCE_ID_TILE_SPAN: u64 = 1 << 53;` (= `2^35 · 4^(12−3)`).
- [ ] `pub fn tile_source_id_range(tile: u64) -> (u64, u64)` → `(tile * SOURCE_ID_TILE_SPAN, (tile + 1) * SOURCE_ID_TILE_SPAN - 1)`.
- [ ] `pub enum GaiaProgress { Querying { tile: u64, total_tiles: u64, stars: usize }, Converting { stars_processed: usize, total_stars: usize }, Complete { total_stars: usize }, Error(String) }` (mirrors `Tycho2Progress`).
- [ ] **Test** `tile_range_partitions_source_id_space`: tile 0 starts at 0; tile 767 end == `768*2^53 - 1`; ranges are contiguous and non-overlapping (`range(t).1 + 1 == range(t+1).0`); 768 tiles cover `[0, 768*2^53)`.
- [ ] Run `cargo test -p athenaeum-core --lib catalog::gaia`. Commit `feat(catalog): gaia.rs scaffold + source_id tile math`.

## Task 2: ADQL builder + TAP async client (TDD on the URL/ADQL, network isolated)

**Files:** `gaia.rs`. **Reuse:** `reqwest::blocking` (as `tycho2.rs` does).

- [ ] `pub fn tile_adql(tile: u64) -> String` → exactly:
  `SELECT ra,dec,phot_g_mean_mag,pmra,pmdec FROM gaiadr3.gaia_source WHERE phot_g_mean_mag < 16 AND source_id BETWEEN {lo} AND {hi}` using `tile_source_id_range`.
- [ ] `fn submit_tap_job(client:&reqwest::blocking::Client, adql:&str) -> Result<String>`: POST form to `GAIA_TAP_ASYNC` with `REQUEST=doQuery LANG=ADQL FORMAT=csv PHASE=RUN QUERY=<adql>`; return the job URL from the `Location` redirect (or job id).
- [ ] `fn poll_job(client, job_url, cancel:&Arc<AtomicBool>) -> Result<()>`: GET `{job}/phase` every 5 s until `COMPLETED` (Err on `ERROR`/`ABORTED`, early-return on cancel). Bounded backoff; no infinite loop.
- [ ] `fn fetch_job_csv(client, job_url) -> Result<String>`: GET `{job}/results/result`.
- [ ] **Test** `adql_is_well_formed`: `tile_adql(0)` contains `phot_g_mean_mag < 16`, `gaiadr3.gaia_source`, `source_id BETWEEN 0 AND 9007199254740991`; `tile_adql(1)` lower bound == `2^53`.
- [ ] Commit `feat(catalog/gaia): ADQL builder + TAP async client`.

## Task 3: CSV row → `StarRecord` parser (TDD)

**Files:** `gaia.rs`. **Reuse:** `StarRecord::from_values`.

- [ ] `pub fn parse_gaia_csv_row(row: &str) -> Option<StarRecord>`: split CSV `ra,dec,phot_g_mean_mag,pmra,pmdec`; skip header row and any row with empty `pmra`/`pmdec` (Gaia 2-param solutions have null PM → treat as 0.0); `StarRecord::from_values(ra as f32, dec as f32, g_mag, pmra, pmdec)`. pmra is already μα\* (cos δ included) — same as the Tycho-2 path, so **no cos δ adjustment** (matches `cone_search`'s `ProperMotionCorrector::propagate` expectation; this parity is asserted in Task 7).
- [ ] **Test** `parse_row_and_header`: header line → `None`; a real-format row → correct `StarRecord` (mag within 1e-3, pm within quantization); null-PM row → PM 0, still `Some`.
- [ ] Commit `feat(catalog/gaia): Gaia CSV row parser`.

## Task 4: Streaming HEALPix-6 binner (TDD — the memory-safety core)

**Files:** `gaia.rs`. **Reuse:** `catalog::healpix` (depth-6 pixel-of(ra,dec)), `StarRecord` byte (de)serialize, `write_catalog_to_healpix`'s file-name/format contract.

`write_catalog_to_healpix` holds all records in RAM — fine for Tycho-2 (35 MB), **OOM for 300M Gaia (4.2 GB)**. Stream instead:

- [ ] `pub struct HealpixBinner { dir: PathBuf, ... }` with `open(scratch_dir)`, `push(&StarRecord)` (append raw 14 bytes to per-pixel scratch file `dir/p_{pixel}.raw`, pixel = depth-6 nest index of the record's ra/dec), `finalize(out_dir) -> Result<usize>` (for each scratch pixel file: read all records, sort by `mag_raw` ascending, write `out_dir/healpix_{:06}.bin`; delete scratch; return total count).
- [ ] **Test** `binner_roundtrips_and_sorts`: push ~10 synthetic records spanning ≥2 pixels (incl. out-of-order mags) → finalize → read back via the existing reader path → each pixel file mag-sorted, every record present, byte-identical to `write_catalog_to_healpix` output for the same input.
- [ ] Commit `feat(catalog/gaia): streaming HEALPix-6 binner (RAM-safe ingest)`.

## Task 5: `download_gaia_dr3` + `setup_gaia_dr3_catalog` (resumable, idempotent)

**Files:** `gaia.rs`. **Reuse:** Tasks 1–4.

- [ ] `pub fn download_gaia_dr3(scratch_dir:&Path, cancel:Arc<AtomicBool>, progress:&dyn Fn(GaiaProgress)) -> Result<usize>`: ensure `scratch_dir`; load `scratch_dir/done.manifest` (one tile id per line) → resume set; for `tile in 0..768` skip if done; submit→poll→fetch→`parse_gaia_csv_row` each line→`binner.push`; append tile id to `done.manifest` (fsync) after success; emit `GaiaProgress::Querying{tile,total_tiles:768,stars}`; honor `cancel` between tiles. After all tiles: `binner.finalize` → `GaiaProgress::Complete`.
- [ ] `pub fn setup_gaia_dr3_catalog(app_data_dir:&Path, cancel:Arc<AtomicBool>, progress:&dyn Fn(GaiaProgress)) -> Result<PathBuf>`: `catalog_dir = app_data_dir/catalogs/gaia_dr3`; if it exists with >100 files, log + return (idempotent, mirrors `setup_tycho2_catalog`); else scratch = `app_data_dir/gaia_dr3_raw`, run `download_gaia_dr3`, finalize into `catalog_dir`, leave `done.manifest` for resumability, return `catalog_dir`.
- [ ] **Test** `setup_is_idempotent_when_catalog_present`: pre-create `<tmp>/catalogs/gaia_dr3/` with 101 dummy files → `setup_gaia_dr3_catalog` returns immediately without network (inject a no-network guard or assert via a cancel-flag-set fast path).
- [ ] Commit `feat(catalog/gaia): resumable setup_gaia_dr3_catalog pipeline`.

## Task 6: Tauri command + Axum mirror + progress event (two-backends rule)

**Files:** `crates/athenaeum-tauri/src/commands/plate_solve.rs`, `.../lib.rs` (invoke_handler), `crates/athenaeum-web/src/routes/plate_solve.rs`, `routes/mod.rs`. **Reuse:** the `download_tycho2_catalog` command/route verbatim as the template.

- [ ] Tauri `download_gaia_dr3_catalog` (clone of `download_tycho2_catalog:768`): call `gaia::setup_gaia_dr3_catalog`, map `GaiaProgress` → emit `gaia-progress` events (mirror the `tycho2-progress` emission shape). Register in `invoke_handler`.
- [ ] Axum mirror in `routes/plate_solve.rs` (clone of `:693`), register in `routes/mod.rs` (`/api/download_gaia_dr3_catalog`), SSE `gaia-progress` via `SseProgressEmitter`.
- [ ] `cargo check -p athenaeum-tauri -p athenaeum-web` clean. Commit `feat(plate-solve): download_gaia_dr3_catalog command + web mirror`.

## Task 7: Proper-motion convention assertion + full test pass

**Files:** `gaia.rs` test module.

- [ ] **Test** `gaia_pm_convention_matches_tycho2_path`: construct a `StarRecord` via `parse_gaia_csv_row` with a known `pmra` (μα\*), run it through the same `ProperMotionCorrector::propagate` call `cone_search` uses with epoch 2016→2025; assert the RA shift equals `pmra/cos(dec)` direction (i.e. the corrector treats stored pmra as μα\*, same as Tycho-2) — guards against a cos δ double-count regression.
- [ ] `cargo test -p athenaeum-core --lib catalog` all green; `cargo test --workspace` (only the known-unrelated rustafits `mass_effect_heart` fixture may fail). Commit `test(catalog/gaia): proper-motion convention parity`.

## Task 8: Run the all-sky ingest (USER-RUN, out of session)

- [ ] User invokes `download_gaia_dr3_catalog` (UI button or `cargo run` harness). 768 TAP jobs, resumable via `done.manifest`, ~hours/overnight, ~few GB transfer, ≈4 GB final at `catalogs/gaia_dr3/`. Re-runnable safely after interruption.
- [ ] Sanity: `ls catalogs/gaia_dr3 | wc -l` ≈ 49,152; spot-check a known pixel has mag-sorted G≤16 stars.

## Task 9: Bench closure (after Task 8)

- [ ] `BENCH_SOLVER_BACKEND=astap BENCH_SKIP_ASTAP=1 cargo test --release -p athenaeum-core --test bench_astap_vs_athenaeum -- --ignored --nocapture` (Gaia now auto-selected by `with_catalog_dir`).
- [ ] **Success = M78 AND `_DSC5767` now solve** (Δpos within tol of cached truth), 0 false positives/panics. This validates the entire plate-solver rewrite premise. Record outcome; update auto-memory.

## Risks
1. **TAP throttling / anon job purge** — keep ≤2–3 jobs in flight, download promptly, manifest-resume on failure. Mitigated by per-tile checkpointing (Task 5).
2. **Memory** — never collect all stars; the streaming binner (Task 4) is mandatory and tested for parity with `write_catalog_to_healpix`.
3. **Footprint** — ~4 GB; document; do not bundle (download-on-demand like Tycho-2).
4. **PM cos δ double-count** — explicitly asserted in Task 7.
5. **M78 still unsolved on Gaia** — possible if 0.96″/px saturated centroids are the *additional* blocker; Task 9 is the definitive test. If so, that is a separate detector workstream (saturation-aware centroiding), documented — not a Gaia-ingest failure.

## Verification
- Unit: `cargo test -p athenaeum-core --lib catalog::gaia` (tile math, ADQL, CSV parse, binner parity, PM convention).
- Build: `cargo check -p athenaeum-tauri -p athenaeum-web` (two-backends sync).
- Closure (post-ingest): the astap-backend bench gains M78 + `_DSC5767`.
