# solvemyastro standalone: catalogs, CLI, crates.io — design

Date: 2026-11-24. Status: draft for owner review.

## 1. Problem and intent

Plate solving is spread over three places: the solver itself (`solvemyastro/`, a submodule, its own public repository),
a catalog pipeline in `athenaeum-core` (`catalog/gaia.rs`, `gaia_bulk.rs`, `binary_format.rs`, `healpix.rs`,
`manifest.rs`, parts of `plate_solve/`), and a dev-only crate `crates/catalog-builder` that glues the two together to
build the density tiers and quad-index packs published on artfrom.space. Athenaeum's core has become a catch-all; the
owner wants the plate-solve functionality in its own crate so the solver, its catalogs and its tools live together and
Athenaeum keeps only what is about Athenaeum's catalog of frames («выделить это в отдельный crate чтобы не мешать мухи с
котлетами и отделить всю функциональность platesolve из ядра athenaeum так как его основная роль уже размыта»).

The tool is mostly for the owner: rebuilding the tiers and index packs (for example on a later Gaia release) and
publishing them. Getting the Gaia files is the user's job; the tool takes a folder and builds from what is in it
(«скачивание и предоставление путей до каталогов gaia должно быть осуществлено самим пользователем»). Testing runs on a
handful of Gaia files, never the full 700 GB dump.

## 2. Goals, non-goals, anti-goals

Goals
- solvemyastro holds everything about plate solving that does not touch Athenaeum's database or Settings: Gaia ingest
  from a local folder, density tiers, quad-index builds, packaging for publication, the on-disk layout of tiers and
  packs, the publication manifests, and the blind acceptance gate.
- One crate with feature layers (owner's choice, approach A): base = the solver as today; `catalog` = building catalogs
  from Gaia; `cli` = the command-line tool.
- A proper CLI (clap) covering the whole chain: Gaia folder → bins → tiers → index packs → publish tree, plus `solve`
  and `info`.
- `crates/catalog-builder` is gone; Athenaeum's core no longer contains Gaia ingest code.
- solvemyastro 0.3.0 is published on crates.io.

Non-goals
- Downloading Gaia data (bulk or TAP). The existing downloaders are deleted, not moved.
- Changing any numeric behaviour: solving, tiers, index packs, the gate. One intended exception: the order of stars
  with equal magnitude inside a bin becomes deterministic (§4.6, SA-6).
- Changing on-disk formats (`healpix_*.bin`, `stars.smac`, `quads.smaq`, `manifest.json` v1, `index_manifest.json`
  format 3) or where Athenaeum installs catalogs.
- Writing the WCS into the solved image or a sidecar from the CLI. `solve` prints the solution (text or `--json`) as
  it does today. Follow-up if wanted.
- Prebuilt binaries, Homebrew. `cargo install` only.
- Moving starloom's solvemyastro dependency from a git revision to the crates.io version (a starloom change; noted as a
  follow-up in §8).
- Support for a Gaia release other than DR3. The CSV reader is DR3's column set; a later release is its own change.

Anti-goals (a failure even if it compiles)
- A tier, bin file or index pack built by the new code that differs by one byte from what the old code builds from the
  same input.
- Athenaeum's installed catalogs, catalog identity strings or zero points becoming "stale" after the update.
- The Athenaeum app, Docker image or Perseus pulling clap, flate2-for-Gaia or any `catalog`/`cli` dependency.
- An integration test that needs the 700 GB dump.
- A catalog build whose output depends on `--jobs` or on thread timing.

## 3. Owner decisions (this conversation)

| Id | Decision |
| ---- | ---- |
| SA-1 | Approach A: one crate `solvemyastro`, layered features (base / `catalog` / `cli`). |
| SA-2 | The user brings the Gaia files; no download code in solvemyastro; Athenaeum's Gaia downloaders are deleted. |
| SA-3 | All plate-solve functionality that is not about Athenaeum's frame catalog leaves the core. |
| SA-4 | Tests use a small subset of the Gaia DR3 copy on the NAS, never the full dump. |
| SA-5 | Publication on crates.io as the last step, on the owner's "yes" (it is public and irreversible). |

## 4. Design

### 4.1 Feature layers

```toml
[features]
default = []
catalog = ["dep:flate2", "dep:md-5", "dep:zip", "dep:sha2"]
cli     = ["catalog", "dep:clap", "dep:tracing-subscriber"]

[[bin]]
name = "solvemyastro"
path = "src/main.rs"
required-features = ["cli"]
```

- Base (no features): the solver as today (`solve`, `solve_with_index`, `StarCache`, `QuadIndex`, `Caches`, `quad`,
  `register`, the `stars.smac` writer `build_cache*`, `build_index`), plus the moved pieces Athenaeum needs at run
  time: `layout`, `manifest`, `healpix`, `accept` (§4.2). New base dependencies: `serde` with `derive` and
  `serde_json` 1 (moves from dev-dependencies; `Manifest::from_json_slice` / `IndexManifest::from_json_slice` parse
  with it and the app calls them).
- `catalog`: Gaia ingest, the bins format, tier slicing, packaging. Dependencies at the versions Athenaeum's lock
  already has (one version per shared crate): `flate2` 1.0, `md-5` 0.11, `zip` 8 (`deflate`, `time`, no defaults),
  `sha2` 0.11.
- `cli`: `clap` 4.6 (derive), `tracing-subscriber` 0.3 (`env-filter`, `json`), the same versions starloom p2 uses.
- Install: `cargo install solvemyastro --features cli --locked`. The README says so. This changes today's behaviour
  (the binary builds with no features now; after the change a plain `cargo install` builds none, as with starloom);
  the CHANGELOG says so. `cli` is not a default feature because a default one would reach Athenaeum through starloom's
  git dependency, which takes default features.
- Tests that need a layer declare it (`[[test]] required-features = ["catalog"]`), so Athenaeum's `cargo test
  --workspace` (base features only) skips them; solvemyastro's own CI runs `--all-features`.

Athenaeum's crates (`athenaeum-core`, `athenaeum-tauri`, `athenaeum-web`) keep depending on base only. The workspace
`[patch]` that points the git URL at the submodule stays.

### 4.2 What moves

| From (Athenaeum) | To (solvemyastro) | Layer |
| ---- | ---- | ---- |
| `core/src/catalog/gaia.rs`: `HealpixBinner`, `GAIA_RUWE_MAX` (the only parts the bulk path uses; `parse_gaia_csv_row`, `GAIA_MAG_LIMIT` 19 and the TAP tile constants are TAP-only and are deleted) | `gaia::binner` | catalog |
| `core/src/catalog/gaia_bulk.rs`: `ingest_bulk`, column index, `parse_bulk_row`, `_MD5SUM.txt` parse, `md5_of_file`, progress type | `gaia::ingest` | catalog |
| `core/src/catalog/binary_format.rs` (14-byte `healpix_*.bin` records) | `gaia::bins` | catalog |
| `core/src/catalog/mod.rs::write_catalog_to_healpix` | with the bins writer (only if a moved test still uses it; else deleted) | catalog |
| `core/src/catalog/healpix.rs` (depth-6 helpers, `cone_pixels_at`, …) | `healpix` | base |
| `core/src/catalog/manifest.rs` (`Manifest`, `ManifestTier`, `IndexManifest`, `IndexManifestPack`) | `manifest` | base |
| `core/src/plate_solve/layers.rs`: `discover_layers`, `discover_index_packs` (the directory rules: `tier_<d>/stars.smac` ascending by density, legacy root `stars.smac`, `<pack>/quads.smaq`) | `layout` | base |
| `core/src/plate_solve/service.rs`: `GateStage`, `BlindGateMetrics`, `blind_gate_ok`, `adaptive_tol_px`, and the "expected bright catalog stars in the field" count (`count_bright_in_fov` with its radius formula and `VERIFY_MAG_LIMIT` 12) | `accept` (a `Gate` thresholds struct with today's defaults, `GateMetrics`, `check`, `expected_bright_in_fov`) | base |
| `crates/catalog-builder/src/tiers.rs`, `layers.rs` (rank-band slicing, `TIER_DENSITIES`, `build_layers`) | `tiers` | catalog |
| `crates/catalog-builder/src/publish.rs`, `publish_index.rs` (zip + `.sha256` + manifests, `min_fov_for`, pack presets `base`/`narrow`) | `package` | catalog |
| `crates/catalog-builder/src/main.rs` pipeline | CLI commands (§4.5) | cli |

Every unit test of moved code moves with it. Names follow solvemyastro's style; the plan fixes the exact paths.

Trap: solvemyastro already has a private `adaptive_tol_px` (`orchestrate.rs:977`, clamp [1.5, `MAX_MATCH_PX`], 5 at
zero scale) used by matching. The gate's function (clamp [4, 20], 10 at zero scale) moves as a separate item in
`accept` under its own name; the two are never merged.

### 4.3 What stays in Athenaeum, and how it changes

- `plate_solve/service.rs`: hints from the frame row, `solve_frame*`, `store_result`, `record_from_result`, the input
  gate (it reads the frame's analysis from the database), the derived focal length (it reads Settings' camera
  defaults). `acceptance_refusal` stays as the adapter: it maps `PlateSolveConfig`'s gate fields onto
  `solvemyastro::accept::Gate` and keeps today's refusal text. `solve_frame_with_hints` calls
  `accept::expected_bright_in_fov` instead of its inline closure.
- `plate_solve/layers.rs` keeps the app-data wrappers: `catalog_root`, `open_catalog_layers`, `catalog_identity`,
  `index_root`, `open_index_packs`, `index_identity`, `CatalogOpenError`, now calling `solvemyastro::layout`.
  `catalog_identity` and `index_identity` must return byte-identical strings (zero points, the solver config hash and
  the transparency code key on them).
- `plate_solve/config.rs` (Settings), `storage.rs`, `hints.rs`, `failure.rs` (UI text), `dso_lookup.rs`,
  `object_fill.rs` (labelling, not solving): unchanged.
- `catalog/gaia_prebuilt.rs`, `catalog/index_prebuilt.rs` (the app downloads ready tiers and packs from artfrom.space):
  stay, reading `solvemyastro::manifest`. `catalog/mod.rs` shrinks to these two.
- `hips/pack.rs` calls `solvemyastro::healpix::cone_pixels_at` (the module is already `render + solver` gated).
- `stacking/solve.rs`, `analysis/zero_point.rs`, `registration`, `fits_writer/wcs.rs`: unchanged apart from paths.
- Tauri/Axum: no command changes (no new command, none removed). The two `tier_status` call sites keep their path.

### 4.4 What is deleted

- `crates/catalog-builder/` (crate, README) and its workspace member line; the `sed` in `docker/Dockerfile` that
  drops it from the workspace.
- From `core/src/catalog/gaia.rs`: the TAP client, job submission/polling, `download_gaia_dr3`,
  `setup_gaia_dr3_catalog`. From `gaia_bulk.rs`: `http_client`, `fetch_md5_manifest`, `download_one`,
  `download_bulk`, `setup_gaia_dr3_from_bulk`. The files themselves go once their remaining parts have moved.
- `core/examples/ingest_gaia.rs`, `ingest_gaia_bulk.rs`, `download_gaia_bulk.rs`.
- `md-5` (and `flate2` if no other user remains) from `athenaeum-core`'s dependencies; the plan checks with
  `cargo tree`.

### 4.5 The CLI

clap derive, one subcommand per stage. Progress and the closing summary go to stderr through `tracing` (the
`tracing-subscriber` fmt layer; `--log <file>` adds a JSON-lines file), the result to stdout. Exit code 0 on success,
1 on failure or a refused solve, 2 on bad arguments (clap). No signal handling: Ctrl-C kills the process. Each
command runs its stage every time (no "output present, skip" logic like `catalog-builder`'s) and writes its final
files only at the end (scratch, then rename), so a killed run never leaves output that looks complete. No public base
API gains a cancel parameter.

| Command | Does | Key flags (defaults = what the published catalogs were built with) |
| ---- | ---- | ---- |
| `ingest` | Gaia DR3 `GaiaSource_*.csv.gz` in a folder → `healpix_NNNNNN.bin` (HEALPix 6, mag-sorted) | `--gaia <dir>` `--out <bins-dir>` `--mag-limit 21` `--jobs 4` `--verify` (checks each file against `_MD5SUM.txt` when present) |
| `build-tiers` | bins → `tier_500` … `tier_8000/stars.smac` | `--bins` `--out` `--epoch 2016.0` |
| `build-cache` | bins → one `stars.smac` (today's legacy-dir import) | `--bins` `--out` `--epoch` |
| `build-bright-cache` | as today | as today |
| `build-index` | tiers → `<pack>/quads.smaq` | `--tiers` `--out`, then either `--preset base\|narrow` or today's flags (`--min-depth` `--max-depth` `--stars-per-cell` `--quads-per-cell`) |
| `package` | tiers and/or packs → `publish/` (`tier_<d>.zip`, `index_<pack>.zip`, `.sha256` sidecars, `manifest.json`, `index_manifest.json`) | `--tiers` `--index <pack-dir>`… `--out` |
| `solve` | as today, plus the acceptance gate | `--tiers\|--catalog` `--index`… hints, `--json`, `--no-gate` |
| `info` | header and counts of a cache, a tier folder, an index pack or a bins folder (`cache-info` stays as an alias) | `<dir>` |

- `ingest` takes whatever files are in the folder. It records which level-8 HEALPix ranges the file names cover
  (`GaiaSource_<from>-<to>.csv.gz`) in `<bins-dir>/coverage.json` and reports how many level-6 cells are complete;
  `build-tiers` warns when the coverage is partial. A cell with only part of its stars would give a wrong tier
  without a warning otherwise.
- The `solve` defaults are `SolveConfig::default()`, which already equals what Athenaeum solves with (quad tolerance
  0.007, catalog G limit 19, SIP on, order 3). The gate's defaults are `PlateSolveConfig`'s defaults; Athenaeum's
  Settings keep overriding them.
- **The gate in `solve` behaves as in the app:** every solve is checked (the app passes `GateStage::ScaleCleared` for
  all of them, `service.rs:513`), with the thresholds of `PlateSolveConfig`'s defaults. The app takes the expected
  scale from the frame's FOCALLEN/XPIXSZ; the CLI takes it from `--scale` when given, otherwise the scale-vs-header
  check is skipped and the other checks apply. A refused solve: the reason on stderr, nothing on stdout, exit 1;
  `--no-gate` prints the solution anyway. `accept::check` takes the metrics and the thresholds; whether
  `GateStage` survives (today only tests build `Hinted`/`FullBlind`) is the builder's call as long as the app's
  decisions stay the same.
- **`build-index` presets fix the layers and the parameters** of what was published (`publish_index.rs:131`):
  `base` = `tier_500` alone at `BuildParams::default()`; `narrow` = `tier_500` + `tier_2000`, depth 8, 10 stars,
  4 quads per cell. Without `--preset` the command does what it does today (`tier_500` + `tier_2000` at the given or
  default parameters), so today's command and `QUAD_INDEX.md`'s examples keep producing the same files;
  `QUAD_INDEX.md` adds that the published packs are rebuilt with `--preset`.

### 4.6 Bit identity: what must not move

Today's ingest is not deterministic. Several files are read at once, one consumer appends 64K-record chunks to each
pixel's scratch in arrival order (`gaia_bulk.rs:735-760`), and `write_records` sorts by `mag_raw` alone, a u16 in
millimag (`binary_format.rs:102`). Equal magnitudes are common, so their order depends on thread timing in every cell
fed by two files (Gaia's file ranges split level-6 cells), and that order decides which tier gets a star at a band
edge. **SA-6 (my call):** the new writer sorts by a total key, `(mag_raw, ra.to_bits(), dec.to_bits(), pmra_raw,
pmdec_raw)`, which covers every field of the record (equal keys mean identical bytes), so the output
is the same for any `--jobs` and any file order. The acceptance is restated on that basis:

1. **Bins:** for every cell, the new `healpix_*.bin` holds the same multiset of records as today's `ingest_bulk` on the
   same files; the new bins are byte-identical between `--jobs 1` and `--jobs 4`, and between two runs.
2. **Tiers:** `build-tiers` writes the same `stars.smac` bytes as `catalog-builder`'s `build_layers` run over the new
   bins (the tier code is deterministic given its bins, so this isolates the move).
3. **Index packs:** `build-index --preset base|narrow` on the installed `tier_500`/`tier_2000` writes the same
   `quads.smaq` as `catalog-builder publish-index` (deterministic today). References: the old command's output and
   the installed `<app-data>/catalogs/smac_index/<pack>/quads.smaq` (the published `.sha256` is of the zip, which
   is not deterministic, so it is not a reference).
4. **Publish tree:** the manifests are byte-identical. The zips are not (timestamps), and that is accepted.
5. **Solver:** the `corpus_layered_tiers` gate passes against its frozen baseline; Athenaeum's plate-solve, analysis
   and stacking tests pass unchanged; starloom's stage goldens are not touched (starloom uses only `quad::*` from
   base, which does not change).
6. **Identity strings:** `catalog_identity` / `index_identity` over the installed catalogs return the same strings
   before and after (a test pins them on a temp layout; a one-off check on the dev Mac's app-data).

### 4.7 Testing on a subset

- **CI-sized fixture** (solvemyastro `tests/`, `catalog` feature): about two thousand generated Gaia DR3 rows (enough
  to fill three tier bands of one cell) in two small `.csv.gz` files named like real ones, covering one complete level-6 cell and one partial one. `ingest` →
  `build-tiers` → `build-index` (tiny parameters) → `package` → `info` end to end; checks counts, sort order, filters
  (G limit, RUWE, missing values), the coverage report, manifest fields.
- **Real-data acceptance** (dev Mac, once, before the merge): 3 consecutive `GaiaSource_*.csv.gz` files copied from the
  NAS (`/Volumes/isos/gaia`, must be mounted) into `~/.athenaeum-acc/<job>/gaia/` with a `REGISTRY.tsv` row, about
  0.6 GB. Run the old pipeline (Athenaeum at the base commit: `ingest_bulk` + `catalog-builder`) and the new CLI on the
  same files and check §4.6 items 1–2. As a secondary check, compare the complete cells of the new tiers with the same
  cells of the installed `tier_500`/`tier_2000`. Differences are expected where equal-magnitude stars straddle a band
  edge (the June build had a random tie order, SA-6); the report counts them. A difference of any other kind (a star
  missing, a different magnitude or position) is reported to the owner, not fixed silently.
  Index packs: §4.6 item 3 on the installed tiers (about 35 s each). Everything under `~/.athenaeum-acc/` is deleted
  once the results are in the ledger.

### 4.8 Athenaeum wiring and the release

- solvemyastro: version 0.3.0, `CHANGELOG.md` section, README (CLI chain, install line, Gaia data note with the ESA
  credit Gaia's terms ask for), `QUAD_INDEX.md` updated for the new command names. New `.github/workflows/ci.yml`
  (test base and `--all-features` on Linux/macOS/Windows) and `release.yml` mirrored from starloom's, plus a
  crates.io leg modelled on rustafits' (loud publish, skips a version already on crates.io). The owner adds the
  `CARGO_REGISTRY_TOKEN` secret to the GitHub repository. `solvemyastro` is not on crates.io yet, so 0.3.0 is its
  first publish: the token needs the "publish new crates" scope, and the name is reserved only once it goes through.
- Before the tag: a scan of the packaged sources for third-party program names (comment text with lines joined, the
  lesson from starloom p3), `cargo publish --dry-run`, `cargo package --list` reviewed (no `docs/`, `.claude/`,
  `.superpowers/`).
- solvemyastro work follows the submodule rule: a branch merged into its `main` with a merge commit, the tag cut from
  `main`, the pointer bumped in Athenaeum only to a commit on solvemyastro `main`, the pin pushed before Athenaeum's
  merge.
- Athenaeum: `Cargo.toml` members, Dockerfile, `CLAUDE.md` (workspace layout gains the `solvemyastro/` submodule line,
  which it lacks today; module map: `catalog` and `plate_solve` described as the app half), `docs/platesolving/README.md`
  (catalog building now lives in solvemyastro; "Detail moved" note), the top-level `README.md` mention of
  catalog-builder, the memory notes on catalog delivery.
- A Windows run before Athenaeum's main moves (`wintest/<name>` on GitLab), as every cross-crate change.

## 5. Approaches considered

- **A. One crate with feature layers (chosen).** One name on crates.io, one version, one release; mirrors starloom's
  layering; Athenaeum compiles only the base layer.
- **B. Two crates in a solvemyastro workspace** (`solvemyastro` + `solvemyastro-catalog`). Slightly cleaner dependency
  split, but two crates.io names, lockstep releases, and the builder depends on the solver anyway (the `stars.smac`
  writer is in the solver). Rejected: overhead with no gain.
- **C. Move only `catalog-builder` into the CLI.** Leaves the gate, layout, manifests and Gaia code in the core, which
  is the problem the owner named. Rejected.

## 6. Phases (outline for the plan)

1. solvemyastro, on a branch: move the base-layer pieces (`healpix`, `manifest`, `layout`, `accept`) with their tests;
   Athenaeum on a branch calls them; §4.6 items 5–6 green.
2. solvemyastro: the `catalog` layer (ingest, bins, tiers, package) with its tests and the CI fixture; Athenaeum drops
   the moved and deleted code, `catalog-builder`, the examples.
3. solvemyastro: the clap CLI over both layers; README, `QUAD_INDEX.md`, CHANGELOG.
4. Real-data acceptance (§4.7) and the identity check; ledger.
5. CI and release workflows in solvemyastro; Windows run of Athenaeum; merges (solvemyastro `main`, Athenaeum local
   `main`); Athenaeum docs and `CLAUDE.md`.
6. On the owner's "yes": tag v0.3.0, the workflow publishes to crates.io and makes the GitHub release; verify by fact
   (crates.io `max_version`, the release page, `cargo install solvemyastro --features cli` in a temp dir); pointer
   bump in Athenaeum to the tag.

## 7. Risks

- **Old and new ingest disagree** beyond tie order: the comparison is on the final bins as multisets per cell, and
  any difference stops the job.
- **The secondary check against the installed tiers shows more than tie-order differences** because the June build
  used other bins or older code: reported, not a blocker unless the primary checks also fail.
- **A crates.io publish is final**: a wrong 0.3.0 can only be yanked. Hence the dry run, the package list and the
  owner's yes.
- **NAS not mounted** at acceptance time: the job waits for the owner rather than using other data.

## 8. Left to the builder

- Module names and file split inside solvemyastro, the clap struct layout, help texts.
- Whether `write_catalog_to_healpix` survives (only if a moved test needs it).
- The fixture's exact rows, as long as it covers the cases in §4.7.
- Where `ingest` keeps its scratch files (inside `--out`, removed on success).

## 9. Calls made after the builder check

| Id | Call | Why |
| ---- | ---- | ---- |
| SA-6 | Bins sort by a total key (`mag_raw`, `ra.to_bits()`, `dec.to_bits()`, `pmra_raw`, `pmdec_raw`); the acceptance compares multisets per cell and the tier code over the same bins. | Today's tie order depends on thread timing, so a byte comparison with the old ingest cannot hold, and a rebuild would give a different index for no reason. |
| SA-7 | The CLI gates every solve like the app; the expected scale is `--scale` or none; a refusal exits 1 with the reason on stderr. | The CLI and the app accept the same solves. |
| SA-8 | `build-index --preset base\|narrow` fixes layers and parameters of the published packs; no preset keeps today's behaviour. | Published packs rebuild byte-identical, and today's command does not change. |
| SA-9 | `serde_json` joins the base layer. | The manifest parsers the app calls use it. |
| SA-10 | No Ctrl-C handling, no new cancel parameter; outputs are written at the end, every command runs its stage. | Only ingest takes a cancel flag today; adding one elsewhere would change the base API for no user need. |
| SA-11 | `cli` stays a non-default feature. | A default one would reach Athenaeum through starloom's git dependency. |

Follow-ups, each its own decision later: starloom's dependency on solvemyastro by crates.io version (needed before
starloom itself can go to crates.io); writing the WCS from `solve`; a later Gaia release; a `solvemyastro-release`
skill if releases become frequent.
