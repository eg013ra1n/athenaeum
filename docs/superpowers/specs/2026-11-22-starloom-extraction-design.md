# starloom: the stacking kernel as a public crate: design

Date: 2026-11-22. Status: approved by the owner on 2026-11-22 («одобряю, пиши план»). Builder check (fable, 2026-11-22) folded in; the CLI discussed with the owner the same day (§4.10). The calls made after the check and the discussion are listed in §12. The rulings are SL-1 to SL-20 (§3). Owner decisions are in §11.

Code paths are under `crates/athenaeum-core/src/` unless they start with `crates/`, `docs/`, `rustafits/`, `solvemyastro/`, `athenaeum-lab/` or a repo-root file. Line numbers are taken from `main` at 196f54a0. The seam inventories behind this spec and the builder check were read-only greps at that commit; nothing was compiled.

## 1. Problem and intent

The owner, 2026-11-22: «хотелось бы инструментарий стекинга использовать как отдельный crate и вынести в отдельный продукт чтобы любой желающий мог его улучшать, дорабатывать, переиспользовать независимо от athenaeum и athenaeum автоматом получал все новые фишки если кто-то доделает алгоритмы».

Follow-ups the same day:
- «калибровку тоже можно вынести так?» Yes: the pixel math of calibration goes too (§4.3).
- «внеший cli нового crate тоже же будет писать данные в заголовки и уметь работать с fits и xisf?» Yes (§4.6, §4.7).
- The name is `starloom` (SL-1). «публикацию на crates можно сделать позже».

Today every line of stacking lives inside `athenaeum-core`. Nobody can improve the algorithms without building a Tauri app, a SQLite catalog and a sync stack. The algorithms are already separable, though. By imports, about 44k lines of `stacking/` touch no database, catalog, Settings or collab code. With their support modules (`integration/`, `geometry/`, `resample/`), the calibration math and the writer, the move set is about 70.8k lines, of which about 30.3k are in-file tests (780 `#[test]`).

The intent is a product of its own:
- an outsider clones `starloom`, runs its tests and its CLI on raw frames, and sends a pull request;
- Athenaeum receives each starloom release through a dependency bump that its own gates check before merge.

## 2. Goals, non-goals, anti-goals

**Goals**
1. A crate `starloom` that holds the stacking kernel, the integration engine, the calibration pixel math, frame weighting and selection, and the FITS/XISF writer, and that builds and tests without any Athenaeum crate.
2. Athenaeum uses starloom for all of that code and keeps every output byte (SL-6).
3. Starloom's stage versions are a public contract with a guard (SL-7, SL-19), so a starloom change that moves numbers cannot silently reuse Athenaeum's cached artifacts.
4. A starloom CLI that calibrates and stacks FITS and XISF from raw frames and writes standard header keys plus its own provenance (phase 2).
5. Starloom becomes its own GitHub repository, buildable alone, consumed by Athenaeum as a submodule, with bump PRs checked by CI and the lab (phase 3).

**Non-goals**
- No algorithm change and no change to any default.
- No change to cache semantics, artifact kinds or hash shapes.
- No UI change.
- No crates.io publication in this cycle (owner, 2026-11-22).
- Not moved: catalog-side code. That is calibration matching (`calibration/`), the master library, plan, groups, paths, origin, `run/`, the artifact cache, the readiness gate and the header vocabulary `ATH_*`.

**Anti-goals.** Each of these is a failure even if everything compiles:
- After the update, any user's stacking cache is recomputed, or any master, registration row or sidecar differs by one byte.
- Athenaeum names leak into starloom's public surface: an `ATH_*` card starloom writes or reads on its own, an `ATHENAEUM_*` environment variable, or a `CREATOR` saying Athenaeum when the CLI wrote the file.
- A starloom release changes a stage's output without bumping that stage's version.
- The headless builds (`--no-default-features`: Perseus, athenaeum-mcp) start pulling rustafits or solvemyastro.

## 3. Rulings

| ID | Ruling | Source |
| ---- | ---- | ---- |
| SL-1 | The crate is `starloom`; its FITS provenance keys use the prefix `SLM_`. crates.io publication comes later. | owner, 2026-11-22 |
| SL-2 | Boundary: §4.1 moves, §4.2 stays. | this spec |
| SL-3 | The FITS/XISF writer core moves into starloom's base layer, not into rustafits (§4.6). | this spec; corrects the conversation of 2026-11-22 |
| SL-4 | Three layers by feature. The base layer (no feature) has no rustafits and no solvemyastro and is always on in core. `engine` follows core's `render`. `stack` follows core's `render` + `solver`. | this spec |
| SL-5 | Old `athenaeum_core::` paths keep compiling. A moved module is re-exported. A module that keeps Athenaeum items becomes a **shim**: a glob re-export of the starloom module plus those items and the old-signature wrappers (§4.4). Examples, lab probes and core tests build from unchanged sources on `main` and on the branch. | this spec; builder check F2 |
| SL-6 | Phase 0 changes no output byte (§6). No pinned-hash test is edited. **No test is rewritten**: a test moves verbatim, import paths aside, or stays in core. | this spec; builder check F3 |
| SL-7 | Stage versions are public starloom constants. A change to a stage's output bits bumps that stage's version in the same PR, and a golden guard enforces it (phase 1). | this spec |
| SL-8 | Starloom writes and reads no `ATH_*` card of its own. The header facts Athenaeum owns reach the kernel as parameters: gain, flat normalization constants, the master-light cards. Persistent sidecar formats keep their names and magic (§4.5). | this spec |
| SL-9 | Starloom never builds a thread pool. It runs on the caller's rayon pool; the CLI builds one. | this spec |
| SL-10 | Tracing targets become `starloom::…`. Messages and field names are unchanged, and the lab and the in-app log categories are updated in the same cycle. | this spec |
| SL-11 | ts-rs derives sit behind starloom's `ts` feature, which core enables in every build, headless included: core already depends on ts-rs unconditionally (`crates/athenaeum-core/Cargo.toml:117`), and the base types are fields of ungated TS structs. Serde and ts attributes move verbatim; only the paths inside `#[ts(as = …)]` change. `src/types/` and `tests/ts_contract.rs` do not change. | this spec; builder check F5 |
| SL-12 | Starloom is standalone-publishable: no `[workspace]` table and no workspace-inherited dependencies in its `Cargo.toml` (the same rule as rustafits). Licence Apache-2.0. | rustafits invariant 1 |
| SL-13 | The repository rules that bind this code move into starloom's own `CLAUDE.md` (§4.8). | `CLAUDE.md` |
| SL-14 | "Athenaeum gets new features automatically" means a bump PR plus CI plus the lab checkpoint, then the owner merges. Athenaeum never tracks a branch. | owner's measurement doctrine |
| SL-15 | Orchestration stays in Athenaeum: all of `run/`, including `run/fan_out.rs` and `run/reference.rs`. The CLI has its own simple orchestration (§4.10). Q1 = A (§11). | this spec; CLI discussion |
| SL-16 | The new repository starts from one squashed commit (the solvemyastro precedent), so no old commit message reaches it. | this spec |
| SL-17 | `#[cfg(test)]` hooks in moved code that core's staying tests read become items of starloom's `test-support` feature (§4.9). Core enables it through its dev-dependency only. | builder check F1 |
| SL-18 | A standalone starloom resolves its dependencies by version: `rustafits` from crates.io (1.9.0 is published), `solvemyastro` by git revision. Athenaeum `[patch]`es both to its submodule paths, so one copy of each is built. Starloom commits its `Cargo.lock`, and its stage goldens are valid for the locked rustafits. Phase 0 uses path dependencies; phase 3 switches (§8). | builder check Q-a |
| SL-19 | `LIGHT_CAL_ENGINE_VERSION` (`ATH_CVER`) stays in Athenaeum: it versions Athenaeum's calibrated-light output, which includes steps that stay (debayer and hot-pixel order in `export/calibrated_generator.rs`). Starloom gets its own `LIGHT_CAL_VERSION` for its math. A core tripwire test pins every starloom version Athenaeum was accepted with, so a bump PR that moves any of them fails until it is accepted on purpose (§5). | builder check Q-e |
| SL-20 | The CLI does exactly what the command says: there is no automatic calibration matching. It checks only explicit problems: it refuses a calibration frame whose size or plane count differs from the light's, and it warns where Athenaeum warns on a manual link (§4.10). | owner, 2026-11-22 |

## 4. Design

### 4.1 What moves

| Layer (feature) | Content | Notes |
| ---- | ---- | ---- |
| base (none) | `geometry/` (including `ransac::SplitMix64`); `resample/` (`mod`, `kernels`, `warp`, `window`) | ungated today |
| base | FITS/XISF writer core: `fits_writer/card.rs`, `writer.rs`, `xisf_writer.rs`. From `fits_writer/mod.rs`: `Durability`, `XisfBounds`, `OutputFormat`, `write_image_f32(_with)`. Also `keywords::Bayer` (+ `parse`), `orientation::ROW_ORDER_*`, `writer::rename_replace`, `escape_xml`, and a copy of `zip_store::fs::sync_best_effort` | §4.6 |
| base | the machine RAM probe `total_ram_bytes` (sysctl / proc / cgroup, `services/machine.rs:41-53`) with its parsing tests (`linux_total_ram_from`, `parse_cgroup_limit`), as `starloom::sys`. Core's `services::machine` calls it | §4.5 |
| base | `models::{LightCalParams, FlatNormMode, BiasFallback, TRIMMED_MEAN_TAIL_FRACTION}` (`models.rs:1340-1470`); headless code uses `LightCalParams` (`export/models.rs:3`). Not `LIGHT_CAL_ENGINE_VERSION` (SL-19) | pure serde + TS types |
| base | pure helpers: `stacking/crop.rs:23-197` (`CropRect`, `crop_plane`, `largest_covered_rect`); `groups::median_f64` (`groups.rs:403`) | split modules |
| `engine` | `integration/` (whole, `combine/simd.rs` included), minus the Settings resolvers (§4.4); the pixel math of `calibration_library/light_cal.rs` (§4.3); `calibration_library/cosmetic.rs` | needs rustafits |
| `stack` | `stacking/`: `integrate`, `measure` (minus §4.4's items), `common_set`, `rej`, `structure`, `pass1_artifact`, `star_measure_identity`, `prefilter`, `psf_signal`; `register/{mod, align, frame, detect, local_loop}`; the pure part of `register/wcs_seed.rs` (`seed_radius_px`, `WCS_SEED_RADIUS_*`, lines 24-79); `drizzle/*`; `ln/*` including `grid.rs` | needs solvemyastro (`align.rs:9`, `solvemyastro::quad`) |
| `stack` | weighting and selection: `weights.rs:13-401` (`compute_weights`, `select_frames`, `WeightMode`, `FormulaWeights`, `SelectionConfig`, `FrameWeight`, `best_by_weight`). They import only serde, `measure` and `geometry` | the CLI needs them (Q-b) |
| `stack` | the pure algorithm config from `config.rs`: `MeasurementConfig` (`:156`, with its custom `Deserialize` at `:251,265`), `DrizzleConfig` (`:396`), `SeedSource`, `DrizzleKernel`, `DRIZZLE_KERNEL_VERSION`, `MIN/MAX_PROTECTED_LAYERS`, `MAX_GROWTH` (`:208, 448, 884, 757-761`). Their fields are all algorithm types. A field found impure stops that struct's move, not the phase | the CLI's config shape |

Starloom's dependencies:
- **Always:** `serde`, `tracing`, `anyhow`, `rayon`, `byteorder`, `xxhash-rust`, `chrono` (XISF `CreationTime`), `libc` (fsync, RAM probe).
- **`engine`:** `rustafits`, with feature `serde`.
- **`stack`:** `solvemyastro`.
- **`ts`:** `ts-rs` 12 with `no-serde-warnings`.
- **Never:** `rusqlite`. It leaves with the Settings resolvers.

If starloom uses `serde_json`, it enables only features core already enables (`float_roundtrip`) and never `preserve_order` or `arbitrary_precision` (§5).

### 4.2 What stays in Athenaeum

- **Orchestration and cache:** all of `stacking/run/`, including `fan_out.rs` and `reference.rs`. Both are `use super::*` over `RunContext` and cannot move as they are (SL-15). Also `plan.rs`, `groups.rs` (it re-exports `median_f64`), `paths.rs`, `origin.rs`, `rebase.rs`, `project_source.rs`, `source_identity.rs`, `fits_artifact.rs`, `solve.rs`, `grid_map.rs`, `provenance.rs`, `telemetry.rs`, `master_cards.rs`, `prepare.rs`.
- **Config:** `StackingConfig` is not split. It stays an Athenaeum struct composed of starloom types: `MeasurementConfig`, `RegistrationConfig`, `NormalizationConfig`, `IntegrationConfig`, `DrizzleConfig`, `SelectionConfig`. Its JSON, hashes and TS do not change (type paths never enter them, §5). App-only parts stay: `GroupingConfig`, `ReferenceConfig`, `OutputConfig`, `PathsConfig`, presets, `resolve_config`, the hash functions, `SourceIdentity`, `calibration: CalibratedLightOptions`.
- **Pieces of moved modules that stay:**
  - `weights.rs`: `LnField` / `ln_field` (needs collab) and the two-pass pick with `TWO_PASS_PICK_VERSION` (`:555`).
  - `crop.rs::{restate_crpix, crop_file}`.
  - `wcs_seed::seed_from_solves`.
  - `register/writer.rs`.
  - `register/frame.rs::to_record`, in the `register::frame` shim.
  - `measure.rs::gain_from_header` and the path-taking measure wrappers, in the `measure` shim.
- **Calibration:** matching (`calibration/`); the master library (`calibration_library/{headers, light_headers, light_resolve, paths, register, rebuilt_copies}`); `light_cal.rs`'s file-level wrapper (§4.3); `export/`; the readiness gate `api::lights::check_mode_ready`; `LIGHT_CAL_ENGINE_VERSION`.
- **FITS:** `fits_writer/{header_patch, xisf_patch, keywords::HeaderBuilder, wcs}`; the catalog parser `fits_parser/`.

### 4.3 Calibration

Starloom gets the per-frame math of `light_cal.rs`:
- `L_c = ((L − S) / (F / divisor)) / scale_divisor + pedestal_dn / scale_divisor`;
- the flat floor;
- per-CFA flat normalization;
- hot-pixel correction (`cosmetic.rs`).

It also gets the pixel side of master building, which is `integration/`. The debayer is already in rustafits (`processing::vng`).

Athenaeum keeps everything that decides which masters a light gets, and everything that writes a calibrated light's file and cards. Concretely, these stay in core as the file-level wrapper over starloom's math:
- `calibrate_light` and `calibrate_light_inner` (`light_cal.rs:394`). They have no production caller, but 27 test call sites use them.
- `write_calibrated_output` (`:367`). It returns Athenaeum's sampled identity hash (`duplicates::compute_xxhash`, `:379`); its callers are `export/calibrated_generator.rs:33,1028` and `calibrate_light_inner`.
- The two header readers: `read_ath_channel_norms` (`:754-795`: `ATH_FNR/G/B`, `BAYERPAT`, `XBAYROFF`, `YBAYROFF`) and `read_ath_fnrm` (`:982-989`).

The readers' values enter starloom's calibration inputs as `Option`s, and `None` keeps today's behaviour: a master flat that is not FITS falls back to recomputing from pixels.

Of light_cal's 37 tests, those that call only the math move to starloom. Those that go through `calibrate_light` or the readers stay in core verbatim (SL-6). That includes the pins at `:1525`, `:1749`, `:1928-1931` and `:2055-2058`. Starloom's own end-to-end coverage of the math comes with the phase-1 goldens.

### 4.4 Re-exports, shims and the items that change path

Plain re-exports:
- `lib.rs`: `pub use starloom::{geometry, resample};`
- `stacking/mod.rs`: `pub use starloom::stack::{integrate, common_set, rej, …};`
- `fits_writer/mod.rs`, `models.rs`, `orientation.rs`, `crop.rs`, `groups.rs`, `config.rs`, `weights.rs` and `services/machine.rs` re-export what they lost.
- `api/lights.rs:45` and `ts_export.rs` keep compiling unchanged.

Shim modules, where a module keeps Athenaeum items. Each is `pub use starloom::…::<module>::*;` plus local items; a local item shadows a glob import of the same name.
- **`stacking::measure`:**
  - `gain_from_header`, imported by `examples/d0_weight_probe.rs:29` and `star_probe.rs:167,749,771`;
  - the path-taking entry points with today's signatures (`measure_frame`, `measure_frame_with_seeds`, `measure_frame_with_fits`, `measure_frame_pass1`). Each reads the gain the way it does today (`measure.rs:1668,2173`) and calls starloom's entry point, which takes the gain as a parameter;
  - callers that stay unchanged: `examples/measure_probe.rs:115`, `ln_probe.rs:660,756`, `integrate_probe.rs:744`, `regfrag_probe.rs:474`, `run/measure.rs:794,1232`.
- **`stacking::register::frame`:** `to_record`. Its only caller is `run/mod.rs:127`.
- **`calibration_library::light_cal`:** the wrapper items of §4.3 and the re-export of `LIGHT_CAL_ENGINE_VERSION`.

Consumers that compile through these paths need no edit:
- `examples/` (about 110 references in 12 files);
- `tests/master_build.rs` and `tests/fits_roundtrip.rs`;
- `athenaeum-web`, `perseus`, `analysis` and collab's `SplitMix64` users;
- the lab's probe builds (`-p athenaeum-core --example …`).

Starloom-internal path fixes, not visible to core:
- the `stacking::robust` re-export used at `ln/scale.rs:1469,1977` becomes `astroimage::star_measure::robust`;
- `#[ts(as = "crate::integration::stats::ScaleEstimatorTs")]` and `#[ts(as = "crate::stacking::psf_signal::PsfModelTs")]` (`integrate.rs:139,261`) take starloom's paths.

Items that stay in core but sat in a moved module, and so change path:
- `integration::band_budget::{resolve_budget_bytes, auto_budget_bytes}` and `integration::io_policy::resolve` move to a new core module that reads Settings. Callers: `run/output.rs:1195`, `api/masters.rs:1272`, `api/compute.rs:117`. Their tests stay with them (`band_budget.rs:49-190`).
- `storage_class`: `StorageClass` and `read_concurrency` move to starloom. `classify*` (a statfs probe, also used by `settings/mod.rs`, `collab/storage/sweep.rs`, `api/masters.rs`) stays in core and returns starloom's enum.

### 4.5 Ambient reads: what becomes a parameter and what stays

| Today | After |
| ---- | ---- |
| `measure.rs:1492` `gain_from_header` reads `EGAIN × ATH_CSCL` with Athenaeum's header rules | stays in core (the shim). Starloom's measure entry points take the gain |
| `ln/reference.rs:277-287` `stacking_cards()` prepends `IMAGETYP`, `ATH_STK`, `ATH_STKV` | moves to Athenaeum; `write_reference` takes the final card list |
| `rej.rs:39,852` and `drizzle/mod.rs:895` probe machine RAM through `band_budget::total_ram_bytes` | unchanged code: the probe moves into `starloom::sys` (§4.1), so `RejForcedSource::load` (only caller `integrate.rs:1567`), `DrizzleInput.ram_total_bytes: None` (`examples/drizzle_probe.rs:419`, `run/output.rs:2331`) and `IntegrationConfig`'s hash stay as they are. The CLI uses the same probe |
| `combine/simd.rs:47,145` reads `ATHENAEUM_SCALAR_SELECTION` (production, process-wide); `combine.rs:2991,2999` reads `ATHENAEUM_REQUIRE_VECTOR_KERNEL` (tests) | `STARLOOM_SCALAR_SELECTION` and `STARLOOM_REQUIRE_VECTOR_KERNEL`. Update the live docs that name them (`docs/logging/README.md:161-162`, `docs/stacking/README.md:4238`); archived lab results stay as written |
| `xisf_writer.rs:125` writes `CREATOR = "Athenaeum " + env!(CARGO_PKG_VERSION)` | the writer takes the creator string. Athenaeum passes the same string built from **core's** version; inside starloom `env!` would give starloom's version and change bytes. The builder also greps every moved file for `CARGO_PKG_VERSION` and `env!` |
| bare rayon parallel iterators (`structure.rs:238…428`, `prefilter.rs:78`, `geometry/pixel_map.rs:117`, `resample/warp.rs`) | unchanged code, documented contract (SL-9): they run on the caller's pool |

Persistent formats written by moved code keep their names and magic: the `.athln` sidecar (`ln/grid.rs:51`, version 1), the pass-1 artifact (`pass1_artifact.rs:34`, version 1) and the temp-name patterns. Renaming any of them would invalidate every user's cache (anti-goal). Starloom's docs record the names as historical. A rename, if ever wanted, is a version bump with its own decision.

### 4.6 The FITS/XISF writer goes to starloom, not rustafits (SL-3)

On 2026-11-22 the conversation said the writer would move into rustafits beside its readers. The inventory rules that out:

1. **Headless builds need the writer without rustafits.** Perseus (`crates/perseus/Cargo.toml:44`) and athenaeum-mcp (`crates/athenaeum-mcp/Cargo.toml:32`) build core with `default-features = false`. The ungated `calibration_library/{headers, light_headers, paths}`, `fits_writer/{keywords, header_patch}` and Perseus' tests use `card.rs` and `write_fits_f32`. rustafits has heavy non-optional dependencies (`image`, `libjpeg-turbo-rs`, `nalgebra`), and the GitLab headless guard (`.gitlab-ci.yml:177`, `cargo check -p athenaeum-core --no-default-features`) exists to keep them out.
2. **Name clash:** `fits_writer::OutputFormat` would collide with rustafits' root export `astroimage::OutputFormat` (`rustafits/src/lib.rs:35`).
3. **Shorter path for every writer change:** a starloom PR alone, instead of a rustafits PR + release + bump on top of starloom's.

Reading stays in rustafits: pixels through `ImageConverter::read_raw`, header values through the reader made public in phase 2 (§4.7).

Crate-private items that core's remaining siblings use become `pub` in starloom:
- `format_card`, `BLOCK_SIZE`, `CARD_SIZE`, `rename_replace` (`header_patch.rs:17,300`);
- `escape_xml` (`xisf_patch.rs:24`);
- `Card` (`keywords.rs:8`, `wcs.rs:10`);
- `Card::structural` (`stacking/test_fixtures.rs:856`).

Two XISF writer tests read back through rustafits (`xisf_writer.rs:625-636, 714-726`, `cfg(feature = "render")`). In starloom they run under `--features engine`, not under `--no-default-features`.

### 4.7 Header keys and the CLI's frame facts

Keys in the files starloom writes:

| Layer | Keys | Who writes them |
| ---- | ---- | ---- |
| standard | `IMAGETYP`, `EXPTIME`, `NCOMBINE`, `CALSTAT`, WCS, `HISTORY`; `BAYERPAT`/`XBAYROFF`/`YBAYROFF` removed after debayer | starloom, always |
| provenance | `SLM_VER` (crate version), per-stage versions, config hash; exact key list is the builder's within 8 characters | starloom, from phase 2, when the caller asks. Phase 0 adds none, because a new card would change master bytes (SL-6). Whether Athenaeum's masters carry them later is a separate decision |
| Athenaeum | `ATH_CSRC`, `ATH_CDRK`, `ATH_CFLT`, `ATH_CHPX`, `ATH_CDBM`, `ATH_CVER`, `ATH_STK*`, `ATH_REG*`, … | Athenaeum only, as extra cards handed to the writer |

The CLI never writes `ATH_CSRC`. The scanner skips a file carrying `CALSTAT` + `ATH_CSRC` as Athenaeum's own temporary artifact (`docs/export/calibrated-lights.md`). A CLI-calibrated light without that key is cataloged like any externally calibrated light and can be attested (`calibrated_externally`).

Athenaeum takes a frame's facts from the catalog: exposure, gain, filter, CFA geometry, temperature and the user's overrides. The CLI has no catalog, so it reads the same facts from headers. Two paths into one set of facts is the main risk of the CLI, because a disagreement makes different masters from the same frames. The rules:
- The CLI reads header values with rustafits' port of Athenaeum's rules (`rustafits/src/formats/stored_keys`). Today that port is `pub(crate)`, sits under the private `formats` module (`rustafits/src/lib.rs:8-11`), and lacks the FITS card-text parser `fits_parser/stored_header.rs:300` (`parse_fits_card_text`, with its `KEY = value` fallback at `:84`). Phase 2 ports that parser verbatim (a new path copies the proven path) and makes the reader public, in a rustafits PR and release.
- A corpus test compares, for every corpus file, the facts from Athenaeum's catalog with the facts the CLI reads. A file with `frames.override = 1` is a legitimate difference; it is listed, not hidden.

### 4.8 Starloom's own rules (SL-13)

`starloom/CLAUDE.md` (with `AGENTS.md` as a symlink, per the user-wide rule) carries:

- **Naming.** Never name another code base consulted (solver, stacker or processing program) in code, comments, docs or commit messages; the comparison program is "the ERP". Never name the reference implementation of the debayer.
- **Versions.** The stage-version rule (SL-7) and the list of versions (§5).
- **Measurement doctrine.** A numeric change is judged on a whole group at master level. Measured-out ideas are not re-proposed; the list lives in `docs/stacking/README.md` and is linked.
- **Calibration rulings.**
  - Raw-master-dark, no dark scaling. Proposals ship off by default and pass the corpus.
  - Hot pixels are mapped from the master dark only, never from the light.
  - A MAD of exactly 0 gives no hot-pixel map; over-flagging is refused.
- **Contracts.** SL-8 (no `ATH_*` of its own), SL-9 (caller's pool), SL-12 (standalone `Cargo.toml`), SL-17 (test hooks only under `test-support`).

### 4.9 Test hooks that cross the crate line (SL-17)

Starloom's `cfg(test)` is off when core's tests run. These hooks in moved code are read by tests that stay in core:

| Hook | In | Read by (examples) |
| ---- | ---- | ---- |
| `geometry::pixel_map::grid_counters` | `geometry/pixel_map.rs:187` | 44 uses: `run/crop_tests.rs:80`, `run/output_tests.rs:1521`, `stacking/grid_map.rs` |
| `measure::{PASS1_LOG, PASS1_CANCEL_AFTER, PASS1_TOUCH_AFTER, pass1_peak}` | `measure.rs:1862-1916` | `run/tests.rs:540,811`; `run/measure_tests.rs:779,1116,1262` |
| `register::frame::DETECT_FRAME_STARS_LOG` | `register/frame.rs:94` | `run/register_tests.rs:2792` and 4 more |
| `ln::fallback_counters`, `integration::registered_source::fallback_counters` | `ln/mod.rs:155`, `registered_source.rs:103` | `run/normalize_tests.rs:568,601`; `run/register_tests.rs:170,640` |

They become `#[cfg(any(test, feature = "test-support"))]` items. Core enables `starloom/test-support` in its `[dev-dependencies]` only.

Cargo builds examples with dev-dependencies, so the lab's probe builds carry these hooks. That is acceptable only while each hook is inert when unset and is touched per frame, per grid or per call, never per pixel. The builder confirms this for each hook in P0-4. A hook found in a per-pixel loop gets a cheaper form (a `const` check, or a separate feature the examples do not enable) before the lab's timing work relies on those probes.

Starloom also gets its own copy of the fixtures its moved tests use:
- the nine `test_support` fields;
- the two log captures (`telemetry::capture_logs`, `solve::tests::captured_logs`);
- `synthetic_star_field` (`test_fixtures.rs:929`) without its module.

### 4.10 The CLI (phase 2)

The owner, 2026-11-22: «я подразумеваю, что cli сможет быть вызван чисто для калибровки файлов (надо придумать как передавать в параметрах пути дарков флетов и тп и их группы), для measure - выдать в конце csv или другой файл (предложи какой) c расчетами, регистрации файлов между собой с указанием референса, локальную нормализацию, конечный стекинг. везде поддерживать ключи с алгоритмами и настройками, а так же оставить дефолтные которые уже имеет athenaeum».

On matching: «то как команда будет выполнена на стороне пользователя, никакого автоподбора нет. проверяются только явные проблемы с разным разрешением и тп».

**Commands.** Each runs on its own; `run` chains them.

| Command | Does | Input | Output |
| ---- | ---- | ---- | ---- |
| `master` | builds a master bias, dark or flat | raw calibration frames | master FITS/XISF |
| `calibrate` | calibrates lights; debayer and hot-pixel correction by flag | lights + bias/dark/flat (masters or raw sets) | calibrated frames |
| `measure` | measures frames, computes weights and selection | frames | `measure.csv` + `measure.json` |
| `register` | registers frames to a reference | frames, `--reference <file>` or `auto` | `transforms.json`; resampled frames by flag |
| `normalize` | local normalization to the reference | frames + transforms | normalization grids; normalized frames by flag |
| `stack` | integration with rejection and weights, drizzle by flag | frames + transforms + grids + `measure.json` | master light, rejection maps, weight map |
| `run` | all of the above in one call | lights + calibration | all of the above |
| `config` | prints the full default config or a preset | — | JSON |

**Chaining.** Each command writes its products into a working folder (`-w <dir>`) with a small manifest, and the next command finds them there. Explicit paths work too.

**Settings, three levels:**
1. Defaults are Athenaeum's defaults and presets (`--preset <name>`).
2. `--config <file>` takes the same shape as the algorithm subtrees of Athenaeum's `StackingConfig`. A config exported from Athenaeum works unchanged.
3. `--set <path>=<value>` sets any field, e.g. `--set integration.rejection=winsorized`. Short flags cover the common ones (`--rejection`, `--weights`, `--drizzle`, `--reference`).

`config show` prints the effective config with every value, so a user sees what was applied.

**Calibration groups:**
- **One group by flags.** `--bias`, `--dark` and `--flat` take either a master file or a folder or glob of raw frames. Given raw frames, the CLI first builds the master into the working folder, as `master` would.
- **Many groups by a plan file.** Each `[[group]]` names its `lights` (globs), `dark`, `flat` and `bias`. Plans are TOML (for hand-written ones) or JSON (for programs); both deserialize into the same structs.

**Checks (SL-20).**
- **Refused:** a calibration frame whose size differs from the light's (the hard rule of `calibration::geometry`), and a plane-count mismatch the math cannot apply.
- **Warned:** the cases Athenaeum warns about on a manual link, such as another recorded sub-frame origin (G-7) and a CFA mismatch (advisory in calibrated lights).
- **Not checked:** exposure, temperature, filter and gain are the user's choice.

**`measure` output:**
- **`measure.csv`:** one row per frame and channel. Columns: file, channel, FWHM in pixels (and in arcseconds when the scale is known), eccentricity, star count, SNR and PSF signal, noise, background, weight, included or excluded with the reason. The exact columns follow the fields of starloom's frame measurement; the names carry units (`fwhm_px`) and are part of the format's version.
- **`measure.json`:** the full report, with the config, the stage versions and every per-channel field. `stack` reads it so frames are not measured twice.
- **`--stars`:** an extra per-star CSV (x, y, flux, FWHM, eccentricity) for looking into one frame.

No Parquet: it is more compact, but it pulls a heavy dependency for one file.

**Reference.** `--reference <file>` is explicit. `auto` is the best frame by weight (`best_by_weight`, moved in phase 0). Athenaeum's richer reference choice stays in Athenaeum (Q1 = A), so `auto` can pick another frame than Athenaeum would.

**What the CLI adds to starloom (phase 2):**
1. **Public intermediate formats, each with a version:** `measure.json`, `transforms.json`, the normalization grid (today's `.athln`, documented as a starloom format with unchanged bytes) and the working-folder manifest.
2. **Algorithm presets move into starloom.** Today `preset()` (`stacking/config.rs:526`) builds a whole `StackingConfig`, including app fields (`reference.two_pass`, `output.cleanup`). Starloom gets the algorithm part, and Athenaeum's `preset()` composes it with its app fields.
3. **The master recipe moves into starloom.** `resolve_recipe` (`api/masters.rs:268`) is pure and already unit-tested; `api::masters` re-exports it.

## 5. Versions and hashes: what must not move

Algorithm versions that move with their code and become starloom's public contract:

| Constant | Value | Enters |
| ---- | ---- | ---- |
| `ALIGN_VERSION` (`register/align.rs:34`) | 1 | `registration_subtree` (`config.rs:1112,1126`); `registration_hash_for` (`plan.rs:884,905`); `two_pass_key` (`run/two_pass.rs:138`) |
| `LN_MODEL_VERSION` (`ln/mod.rs:96`) | 4 | `normalization_subtree` (`config.rs:1178`); `normalization_hash_for` (`plan.rs:1011,1066`); `seeds_calibration_hash_for` (`plan.rs:1092`) |
| `SNR_GAIN_VERSION` (`integrate.rs:519`) | 2 | `GroupStats.snr_gain_version`; `stacking_run_groups.stats_json`; `RunSummary`; `master_cards.rs:1299` |
| `DRIZZLE_KERNEL_VERSION` (`config.rs:884`) | 2 | run-level `config_hash` only (`config.rs:895-902`) |
| `PASS1_ARTIFACT_VERSION` (`pass1_artifact.rs:34`) | 1 | file header; upstream token of `pass1_hash_for` (`plan.rs:758-763`) |
| `.athln` `VERSION` (`ln/grid.rs:51`) | 1 | sidecar header |
| `LIGHT_CAL_VERSION` (new in starloom) | 1 | nothing in phase 0; read by the tripwire below |
| `MEASURE_VERSION` | 8 | already in rustafits (`rustafits/src/star_measure/mod.rs:238`); unchanged |

These stay in Athenaeum: `LIGHT_CAL_ENGINE_VERSION` (3, card `ATH_CVER`, SL-19), `PREPARE_VERSION`, `FITS_ARTIFACT_VERSION`, `TWO_PASS_PICK_VERSION`, `COMPOSE_VERSION`, `ATH_STK_VERSION`, `ATH_REG_VERSION` and `STACKING_CONFIG_VERSION`.

**Tripwire.** A core test, `starloom_versions_are_the_accepted_ones`, pins the value of every starloom constant in the first table. When a bump PR raises one, the test fails and names it.
- Versions that enter a hash invalidate the right cache entries by themselves; the tripwire only makes the change visible.
- For `LIGHT_CAL_VERSION`, the same PR also raises `LIGHT_CAL_ENGINE_VERSION`, because `ATH_CVER` is what marks Athenaeum's calibrated lights as current.

How the hashes are built:
- `stage_hash` (`config.rs:969-992`) is xxh3 over `serde_json::to_string` of a `json!` value. `config_hash` (`:895`) does the same over the whole `StackingConfig`.
- Type names and module paths never enter the bytes.
- Field names, `rename_all`, variant spellings, `default`, `skip_serializing_if` and tag layout do enter them. So serde attributes move verbatim.
- Keys come out sorted only because no crate in the workspace enables serde_json `preserve_order`. Today `cargo tree --workspace` shows only `alloc`, `default`, `float_roundtrip`, `raw_value`, `std` and `unbounded_depth`.

These tests guard the bytes and stay unedited:
- `config_hash_is_canonical_and_pinned` (`config.rs:2491`) and `a_sources_two_shapes_serialize_as_pinned` (`:1914`);
- `stage_hashes_are_pinned_to_golden_literals` (`plan.rs:9026`), `calibration_hash_for_is_the_pinned_formula` (`:9266`), `the_prepare_subtree_and_the_project_hash_are_pinned` (`:9725`), `a_same_group_mono_registration_hash_is_pinned` (`:11637`);
- `the_solve_artifact_payload_and_hash_are_pinned` (`solve.rs:775`);
- `module_measure_equals_the_adapter_bit_for_bit` (`star_measure_identity.rs:17`);
- the byte-identity tests in `run/output_tests.rs:1900,1996` and `run/measure_tests.rs:1307`.

Phase 0 adds the tripwire and one more guard in core: a test that fails if serde_json starts preserving insertion order.

## 6. Bit identity (SL-6)

Phase 0 is accepted only when every output is unchanged.

**The ruler.** The lab's default base `tierA-baseline` is gone from this machine, and `checkpoint.sh`'s header still describes it as a run on `main` 548eeec3. Phase 0 therefore makes its own ruler first (P0-0):
- Run the checkpoint on a `main` worktree at the branch point, as `starloom-main`.
- `checkpoint.sh` always compares against a base after the run (`athenaeum-lab/harness/acceptance/tier1/checkpoint.sh:121`). For this first run the builder either passes an existing base and ignores that verdict, or adds a `--no-compare` flag in athenaeum-lab.
- The job folders `tierA-starloom-*` get rows in `~/.athenaeum-acc/REGISTRY.tsv` and are deleted once the results are committed.

**The checks:**
- **Master lights and drizzle outputs:**
  - `ATHENAEUM_REPO=<branch worktree> zsh ~/Documents/Projects/athenaeum-lab/harness/acceptance/tier1/checkpoint.sh starloom-p0 starloom-main`;
  - byte-identity mode compares the masters' header card set plus data bytes, the `registration_results` rows and the `.athln` sidecars (`tier1/tier1-compare.py:2-3`);
  - expected: 0 differences.
- **Measurement:**
  - `python3 tier1/tier1-compare.py --measure-identity ~/.athenaeum-acc/tierA-starloom-main ~/.athenaeum-acc/tierA-starloom-p0 --set-id 204 --run-id-a <main run> --run-id-b <branch run>`, as `t13-real.sh:256` does, with the run ids from each checkpoint's `run.txt`;
  - it compares `.athf` sha256, the metrics payload minus `durationMs`, `config_hash` and β bits;
  - expected: `differences: 0`.
- **Paths set 204 does not run.** The template config has `largeScale.enabled = false`, `distortion = off` and `localDistortion = false`. One more pair of runs (main and branch) uses a config variant with all three on, on an OSC set, with XISF output. The builder picks the set within the disk budget and compares as above; t13c mode compares XISF by pixels plus cards.
- **Calibration masters:** a master bias, dark and flat built from the same raw sets on `main` and on the branch compare equal by `cmp`, with time-dependent cards excepted by name only.
- **Calibrated-light export:** one frame set exported as calibrated lights, mono and OSC with debayer, on `main` and on the branch. Data bytes and card set are equal.
- **Hashes and TS:** the pinned tests in §5 stay green unedited, and `tests/ts_contract.rs` passes with no change under `src/types/`.

## 7. Approaches considered

| Approach | Trade-off | Verdict |
| ---- | ---- | ---- |
| A. Own repository, consumed as a submodule (like rustafits and solvemyastro) | an independent product with its own CI, issues and releases; costs a pointer bump per release and a release procedure | **chosen.** The owner asked for a separate product, improvable "независимо от athenaeum". Phase 0 starts it inside the workspace, so nothing is lost if phase 3 waits |
| B. Crate inside the Athenaeum repo, published from there | cheapest: atomic changes across the boundary, no bumps; but a contributor clones a Tauri app to work on a stacker, and the product has no home of its own | rejected as the end state; it is exactly phase 0 |
| C. Inside rustafits | one fewer repository; but it puts 70k lines into a FITS viewer library with its own Homebrew users and cadence, and rustafits' heavy dependencies would reach the headless builds | rejected |

## 8. Phases and task outline

One implementer at a time (16 GB dev Mac). Test runs, catalog copies and worktrees follow the disk rule (`~/.athenaeum-acc/` + `REGISTRY.tsv`, cargo target `target-agent`). Every test passes on macOS, Linux and Windows. Test names use letters, digits, `_` and `-` only.

### Phase 0: the crate inside the workspace, bit-identical

**P0-0 The ruler.** Make the `starloom-main` checkpoint and the second-config pair's main half (§6) on a `main` worktree at the branch point, before any code moves. Acceptance: both runs complete, and their folders are registered.

**P0-1 Skeleton and packaging**
- Files:
  - `crates/starloom/{Cargo.toml, src/lib.rs, LICENSE, README.md, CLAUDE.md}`;
  - root `Cargo.toml` (members at `:3-13`; `[profile.dev.package.starloom]` opt-level like core's at `:17-33`);
  - `crates/athenaeum-core/Cargo.toml` (dependency + features at `:186-197`, dev-dependency at `:249-251`);
  - `docker/Dockerfile` (manifest layer `:18-20`, member sed `:55-59`, source layer `:75-76`);
  - `.gitlab/ci/scripts/check_versions.sh:32`.
- Approach:
  - Starloom features: none = base; `engine = ["dep:rustafits"]`; `stack = ["engine", "dep:solvemyastro"]`; `ts = ["dep:ts-rs"]`; `test-support`; `default = ["stack", "ts"]`.
  - Dependencies are path dependencies to `../../rustafits` and `../../solvemyastro` in phase 0 (SL-18).
  - Core depends on starloom with `default-features = false, features = ["ts"]` always, and with `test-support` added in `[dev-dependencies]`. `render` adds `starloom/engine`; `render` + `solver` reaches `starloom/stack`. The exact feature mapping is the builder's, under one rule: `--no-default-features` pulls neither rustafits nor solvemyastro.
- Tests:
  - `cargo check -p athenaeum-core --no-default-features`;
  - `cargo tree -p athenaeum-core --no-default-features -e normal` lists no `rustafits` or `solvemyastro`;
  - `cargo check -p perseus` and `cargo check -p athenaeum-mcp`;
  - the Docker web image builds.
- Acceptance: an empty starloom builds in every configuration, and so does the workspace.

**P0-2 Base layer**
- Moves the base rows of §4.1 and adds core's re-exports (§4.4), the RAM probe (`services::machine` calls `starloom::sys`) and the writer's creator parameter (§4.5).
- Tests:
  - moved unit tests pass under `cargo test -p starloom --no-default-features`, and the two XISF read-back tests under `--features engine`;
  - `tests/fits_roundtrip.rs`, `tests/master_build.rs`, Perseus and web tests compile unchanged;
  - the headless guard passes.
- Acceptance: no file outside the moved set edits an import, except the callers §4.4 lists; an XISF written by Athenaeum is byte-identical apart from `CreationTime`.

**P0-3 Integration engine (`engine`)**
- Moves `integration/` minus the resolvers (§4.4): the resolver module in core, the `storage_class` split, the two `STARLOOM_*` variables and their docs.
- Tests: integration tests pass in starloom; the resolver tests pass in core.
- Acceptance: calibration masters are byte-identical (§6).

**P0-4 Stacking kernel, weighting and selection (`stack`)**
- Moves the `stack` rows of §4.1 and makes the splits, shims and parameters of §4.2, §4.4 and §4.5: `wcs_seed`, `register/writer`, the `measure` and `register::frame` shims, `stacking_cards`.
- Test hooks and fixtures as in §4.9, including the per-pixel check of each hook. Tests that pin log text keep the text.
- Tests: the moved tests pass in starloom; the `run/` tests (312) pass in core with `starloom/test-support` on.
- Acceptance: §6 checkpoints, both modes, both configs.

**P0-5 Calibration math (`engine`)**
- Moves the light-cal math and `cosmetic.rs` as in §4.3. The `light_cal` shim keeps the wrapper, the readers, `write_calibrated_output` and their tests. Adds `LIGHT_CAL_VERSION` and the tripwire (§5).
- Tests: the math-only light-cal tests and the cosmetic tests pass in starloom; the wrapper tests pass in core verbatim, including the non-FITS flat fallback.
- Acceptance: calibrated-light export is byte-identical (§6).

**P0-6 TS (`ts`)**
- About 35 derives become `#[cfg_attr(feature = "ts", derive(TS))]`. Every `#[ts(...)]` helper moves under `cfg_attr` too. The mirror types `ScaleEstimatorTs` and `PsfModelTs` move with their enums, and the `#[ts(as = …)]` paths take starloom's layout.
- Acceptance: `tests/ts_contract.rs` passes, and `src/types/` has no diff.

**P0-7 Logs and the lab**
- Tracing targets become `starloom::…` (SL-10). In-app logging:
  - `logging/config.rs:41`: the "calibration" category, prefix `athenaeum_core::calibration`, today also raises `calibration_library::light_cal`. Add starloom's calibration module prefix to it, so the switch keeps covering the same code.
  - Add a stacking category, if wanted (none today, `:39-44`).
  - Update the log-MCP examples in `docs/logging/README.md`.
- In athenaeum-lab, the same cycle, one commit:
  - `ATHENAEUM_LOG` filters: `harness/acceptance/tier1/tier1-run.sh:94`, `tier1/project-run.sh:92`, `star-lab-mono-run.sh:280`, `server.sh:35` (the `calibration_library` filter);
  - the pins of those filters in `tests/test_t14b_run_log.py:135` and `tests/test_planb_tools.py:260`;
  - target fixtures in `tests/test_pland_sh.py:150` and `tests/test_t11c_tools.py:26`;
  - target parsing in `grid-report.py:1189-1217`;
  - the test pin in `star-lab/bin/gates.py:908-910`, which moves to `-p starloom` because `measure_plane_detailed_returns_the_seeds_it_fitted_and_the_same_measurement` moves with `measure.rs`.
- Probe examples stay in core and compile through the re-exports and shims.
- Acceptance: the checkpoint's reports parse and show the same rows, and the lab's own tests pass.

**P0-8 Docs and the map**
- Update:
  - `CLAUDE.md:17, :81, :132`;
  - `docs/architecture/commands.md:8`;
  - `docs/stacking/README.md`, about 52 lines naming kernel paths (run table `:19-32`, headless paragraph `:755-761`, key files `:2371-2387`, `:5475-5495`);
  - `docs/masters/README.md` and `docs/export/calibrated-lights.md`.
- Write starloom's `CLAUDE.md` (§4.8) and README. Any owed smoke goes to `docs/superpowers/open-items.md`.
- Phase 0 branch gate, before merge to `main`:
  - `cargo test --workspace`;
  - the headless guard, Perseus and athenaeum-mcp builds;
  - `cargo check -p athenaeum-core --examples`;
  - the Docker build;
  - a Windows run on the test box;
  - §6 in full;
  - one stack run in `npm run tauri dev` on the dev catalog.

### Phase 1: contract hardening (still in the workspace)

- **Stage golden guard (SL-7)** in `crates/starloom/tests/stage_goldens.rs`.
  - Each stage runs on fixed synthetic inputs built from the `test_support` fields, the light-cal math included.
  - The test keeps a table of (stage, version, output checksum).
  - A changed checksum under an unchanged version fails with "bump <NAME>_VERSION".
  - Acceptance: perturbing one constant in a moved stage turns it red.
- **Public surface:**
  - `pub` only what Athenaeum or the CLI uses;
  - rustdoc on every public item, with the units (DN, normalized, pixels), the SL-9 pool contract and the RAM-probe fallback;
  - starloom's README lists the versions of §5.

### Phase 2: the CLI and the public header reader

**P2-1 Public header reader (rustafits PR).** Port `parse_fits_card_text` verbatim into `formats/stored_keys`, make the reader public, release rustafits, then bump the submodule here.

**P2-2 Presets and the master recipe.**
- First add core tests that pin `config_hash(preset(p))` for every preset on the current code. Only then split `preset()` (§4.10 item 2) and move `resolve_recipe`.
- Acceptance: the new pins and every §5 pin stay green.

**P2-3 Public intermediate formats.**
- Define and document `measure.json`, `transforms.json`, the normalization grid and the working-folder manifest, each with a version constant.
- Acceptance: a round-trip test per format, and the grid bytes equal today's `.athln`.

**P2-4 The commands (feature `cli`).**
- The commands, chaining, settings, plan files and checks of §4.10.
- One rayon pool (`--threads`), the RAM budget from `starloom::sys` (`--memory` overrides it), progress on stderr, logs through `tracing` (`-v`).
- The command-line shape beyond §4.10 is the builder's.
- Tests:
  - end-to-end per command on synthetic fields: calibrate → measure → register → normalize → stack, and `run`;
  - the size refusal and the warnings of SL-20.

**P2-5 Frame-facts equality** of §4.7 on the corpus, in the lab.

**P2-6 Public CI** with synthetic goldens (Q2).

Acceptance: `run` makes a master from raw frames of one corpus set in the lab, and the facts equality test passes.

### Phase 3: own repository

- Create the GitHub repository `starloom` from a squashed initial commit (SL-16), Apache-2.0, with its `Cargo.lock` committed (SL-18).
- **Dependencies (SL-18):**
  - starloom: `rustafits = "1.9"` (crates.io) and `solvemyastro = { git = "…", rev = "…" }`;
  - solvemyastro: its `rustafits = { path = "../rustafits" }` (`solvemyastro/Cargo.toml:28`) becomes a version dependency, because a path dependency inside a git dependency cannot leave that repository;
  - Athenaeum: `[patch.crates-io] rustafits = { path = "rustafits" }` and a `[patch."<solvemyastro git url>"]` to `solvemyastro/`, so the workspace builds one copy of each. Core and tauri keep their direct path dependencies (rustafits invariant 2);
  - a fresh clone of starloom alone builds `engine` and `stack` with `cargo test`.
- **In Athenaeum:**
  - `crates/starloom/` becomes the submodule `starloom/` at the repo root;
  - `.gitmodules` and path dependencies `../../starloom`;
  - Docker `COPY starloom/`;
  - a `starloom/**` line in the `ci.yml` paths filter (`:51-53`). `submodules: recursive` is already there at `:79,:240`.
- **Release procedure:** a `starloom-release` skill copied from `rustafits-release`, with the rustafits lessons:
  - `CHANGELOG.md` covers every commit;
  - tag = `Cargo.toml` version;
  - releases come from `main` only, and pins name only `main` commits (merge commits, never squash);
  - release starloom before the superproject;
  - push the pin before the superproject merges.
- Dependabot `gitsubmodule` updates in Athenaeum open the bump PRs (SL-14). The tripwire (§5) and the lab checkpoint run before the owner merges.
- Starloom CI:
  - `cargo test` with none / `engine` / `stack` / `ts`;
  - the golden guard;
  - `cargo publish --dry-run` (expected to fail until Q3, recorded).
- Acceptance: a fresh `git clone --recursive` of Athenaeum builds; a fresh clone of starloom alone builds and tests; a no-op starloom release flows through a bump PR with CI green.

### Later, each its own decision

- crates.io publication (Q3).
- Moving a single-group pipeline driver into starloom (Q1 = B), so that `auto` reference and two-pass match Athenaeum.
- Athenaeum exporting a frame set as a starloom plan file, to repeat a stack outside Athenaeum.
- `SLM_` provenance cards in Athenaeum's own masters.
- Renaming the historical sidecar formats.

## 9. Risks

| Risk | Where it bites | Guard |
| ---- | ---- | ---- |
| serde attribute or feature drift changes hash bytes | every user's cache | §5 pinned tests unedited; the `preserve_order` guard |
| `env!` evaluated inside starloom | XISF `CREATOR` bytes | creator parameter; grep in P0-2 |
| headless builds pull rustafits | Perseus, MCP, GitLab guard | layered features; `cargo tree` check in P0-1. `solver` without `render` pulls rustafits through solvemyastro itself (`solvemyastro/Cargo.toml:28`), so that combination was never rustafits-free; it is accepted, and only `--no-default-features` is guarded |
| test hooks compiled into probe builds | lab timing | §4.9 per-pixel check |
| a shim drifts from starloom's entry point | examples and the app read the gain or RAM differently | shims are thin wrappers that only read and forward; §6 runs through the shims |
| tracing target rename | lab parsers, log-MCP filters, the in-app calibration switch, target-filtered test captures | P0-7 in the same cycle |
| CLI and catalog read different frame facts | CLI masters differ from Athenaeum's | facts equality test (§4.7) |
| an outside PR moves numbers without a bump | stale cache mixed with new frames | golden guard (phase 1); tripwire and lab checkpoint before merge |
| a rustafits bump changes measurement without a starloom release | starloom's goldens no longer match | goldens valid for the locked rustafits (SL-18); the rustafits rule that a measurement change bumps `MEASURE_VERSION` |

## 10. What is left to the builder

- The exact feature mapping in core (the P0-1 rule).
- The name of the core module that holds the Settings resolvers.
- The form of the lab's first-run compare skip (§6).
- Which OSC set carries the second config pair (§6), within the disk budget.
- The `SLM_` key list within 8 characters.
- The CLI's command-line shape.
- The order of P0-2 to P0-6, as long as each lands green on its own.

## 11. Owner decisions

- **Q1: how much pipeline goes into starloom. A, as a call after the CLI discussion (2026-11-22).** The owner's CLI scenarios (§4.10) need the stages, weighting and selection with an explicit reference, which A gives. B, a single-group driver in starloom so that reference choice and two-pass match Athenaeum, stays a later cycle. The owner can overturn this call at the spec review.
- **Q2: the corpus for starloom's public CI. Decided by the owner, 2026-11-22:** synthetic goldens in public CI; the real-data check runs in Athenaeum's lab before each bump merge. No owner frames are published.
- **Q3: crates.io.** Starloom's `stack` layer depends on `solvemyastro`, which is not on crates.io (it is public on GitHub). Publishing starloom later needs solvemyastro published first, or the quad matcher moved. Recommendation: decide together with the crates.io publication; nothing in phases 0-3 depends on it.

## 12. Calls made after the builder check

The builder check (fable, 2026-11-22) found six gaps and asked five questions. Each was settled here as a call the owner can overturn:

- **F1 → SL-17, §4.9.** Test hooks go to a `test-support` feature, with a per-pixel check.
- **F2 / Q-c → SL-5, §4.4.** Shim modules, so examples and lab probes build from unchanged sources. The RAM probe moves into starloom instead of becoming a parameter (§4.5).
- **F3 → SL-6, §4.3.** `calibrate_light`, its readers and their tests stay in core, and no test is rewritten in phase 0.
- **F4 → §6, P0-0.** A fresh ruler on `main`, and the correct measure-identity command.
- **F5 → SL-11.** `ts` is on in every core build, and the `ts(as)` paths are rewritten.
- **F6 →** the second environment variable, the longer lab list, the in-app calibration log switch, and the XISF read-back tests under `engine`.
- **Q-a → SL-18.** Version dependencies, plus `[patch]` in Athenaeum, plus a committed `Cargo.lock`.
- **Q-b → §4.1.** Weighting, selection and the pure algorithm config move in phase 0.
- **Q-d → §6.** A second config pair on an OSC set with distortion, local distortion, large-scale rejection and XISF output.
- **Q-e → SL-19.** `LIGHT_CAL_ENGINE_VERSION` stays in Athenaeum; starloom gets `LIGHT_CAL_VERSION`; a tripwire pins every starloom version.

After the CLI discussion with the owner (2026-11-22):

- **Q1 → A.** Stages, weighting and selection move; reference choice beyond best-by-weight and two-pass stay in Athenaeum.
- **Matching → SL-20 (owner).** No automatic matching; refuse a size or plane-count mismatch; warn where Athenaeum warns on a manual link.
- **Formats → §4.10.**
  - `measure` writes CSV plus JSON; there is no Parquet.
  - Plan files are TOML or JSON.
  - Intermediate files become versioned public formats (P2-3).
- **Presets and `resolve_recipe` → P2-2.** They move in phase 2, behind new pins written first.

