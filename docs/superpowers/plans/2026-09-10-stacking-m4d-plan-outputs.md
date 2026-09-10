# Stacking M4d — Outputs and Product Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The product-facing items of spec §14 M4: Bayer drizzle (stage 1 keeps the CFA mosaic, drizzle deposits each colour's own samples), XISF output for masters and drizzled masters, cataloging masters (a `master_lights` entity linked to the set and a thumbnail preview on the Results cards), and preset management (named user presets beside the three built-ins).

**Architecture:** Bayer drizzle adds one optional artifact to stage 1 — the calibrated CFA mosaic beside the debayered frame, produced by the same generator run (no second calibration) — and one optional input to the drizzle deposit: a CFA mask that routes each source pixel to the plane of its own colour; registration, rejection bitmaps, weights and LN are unchanged (all in reference geometry). XISF output is a new writer in `fits_writer`'s sibling module `xisf_writer` (monolithic XISF 1.0: signature, XML header with one `<Image>` carrying the master's cards as `FITSKeyword` elements, one uncompressed Float32 planar attachment), selected by the existing `output.format`. Cataloging is a new table `master_lights` written by stage 9 and a command that returns a rendered JPEG preview of a master by run/group/kind (both backends), consumed by the Results card. Presets: the three built-in transforms stay the single Rust source of truth; user presets are named config documents in one settings key, with three commands to list/save/delete them, and the tab's preset menu gains them plus "Save current as…".

**Tech Stack:** Rust (athenaeum-core `export/calibrated_generator.rs`, `stacking/{run,drizzle/mod,master_cards,config,provenance}.rs`, `fits_writer/xisf_writer.rs`, `db/{schema,stacking}.rs`, `api/stacking.rs`; Tauri `commands/stacking.rs`; Axum `routes/stacking.rs`), React/TS (`DrizzlePanel.tsx`, `OutputPanel.tsx`, `ResultsPanel.tsx`, `StackingTab.tsx`, `stageSummary.ts`, `useStackingRuns.ts`), ts-rs regeneration, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §7 (drizzle), §6.4/§9.5 (output, names), §9.1 (tables), §9.2 (`output.format`, presets), §10.1 (commands), §11.2 (Results cards: "thumbnail until M4 catalogs masters"), §14 M4, §15 (the Objects-page question is the owner's, still open) · **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §6.4 (Bayer drizzle) · **Prior rulings:** M3 R-M3-2/4/5 (deposit units, rejection and LN lookups per source pixel), M4b R-M4b-5 (per-group geometry).

## Global Constraints

- No new crate dependencies; `tracing` only; never name other software in code or comments.
- Two backends in sync: every new command in `api/stacking.rs` + `commands/stacking.rs` + `routes/stacking.rs` + `invoke_handler` + `build_router` + `ts_export.rs` in the same task, with a route test in `athenaeum-web` mirroring the existing stacking route tests.
- Headless build: `cargo check -p athenaeum-core --no-default-features` must stay clean — `fits_writer/xisf_writer.rs` is ungated (like `fits_writer/writer.rs`); the preview command is `render`-gated like `get_frame_preview`.
- New log field names go into the "Unified event schema" dictionary of `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in the same task.
- rustfmt only on new leaf files and rustfmt-clean files; never `run.rs`, `master_cards.rs`, `schema.rs`, `ts_export.rs`, `mod.rs`, `engine.rs`.
- Serde names are spec §9.2's verbatim; new config fields are `#[serde(default)]`; no `STACKING_CONFIG_VERSION` bump. The `stacking_runs`/`stacking_run_groups` tables are unchanged; `master_lights` is a NEW table (`CREATE TABLE IF NOT EXISTS`, indexed FKs — the `every_foreign_key_child_column_is_indexed` test in `db/schema.rs` enforces it).
- The scanner's skip rule (a file with `CALSTAT` + `ATH_CSRC` is never cataloged) is untouched; masters carry `ATH_STK*` and are cataloged ONLY through `master_lights`, never as `frames` rows (spec §15's Objects-page question stays the owner's).
- Commit as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit …` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY` trailers.

## Rulings made while planning (binding unless the spec says otherwise)

- **R-M4d-1 The mosaic is a second output of the SAME generation.** `execute_generation` gains `keep_mosaic: bool` in its options: when set for an OSC frame, it writes `c_<stem>.fits` (the calibrated, hot-pixel-corrected CFA mosaic with its `BAYERPAT`/`XBAYROFF`/`YBAYROFF` cards intact) beside `c_<stem>_d.fits` from the same in-memory frame — one read, one calibration, two writes. The run sets `keep_mosaic = drizzle.enabled && drizzle.bayer` for OSC groups; the mosaic is a `stacking_artifacts` row `kind = "calibrated_mosaic"` keyed by the same calibration hash (so a re-run reuses it), removed by the same cleanup policy as `calibrated/`.
- **R-M4d-2 Bayer drizzle deposits colour-pure samples into the debayered geometry.** `drizzle.bayer: bool` (default false; ignored for mono groups). With it on, `DrizzleFrame` carries `cfa: Option<CfaSource { path: &Path, pattern: BayerPattern }>`: the source plane for EVERY output plane `c` is the mosaic; a source pixel `(x, y)` is deposited into plane `c` only when `pattern[(y mod 2)·2 + (x mod 2)] == "RGB"[c]` (math §6.4, `n = 2` — every supported pattern is 2×2); G has two sites per cell and gets both. The `.rej` lookup (reference coordinate, plane `c`), the plane weight, the plane's output pair and the LN grid are the DEBAYERED run's — identical geometry, per math §6.4 "alignment data still come from the registration of the debayered frame". Coverage is lower per plane (R and B deposit a quarter of the pixels each): `dropShrink` is forced to `max(dropShrink, BAYER_MIN_DROP = 0.9)`... no — a forced value hides the user's choice; instead the panel shows a note and `DrizzleStats.coverage` reports honestly. The level-preserving `I / W` is unchanged.
- **R-M4d-3 XISF writer scope.** Monolithic XISF 1.0: 16-byte signature block (`XISF0100`, header length u32 LE, 4 reserved bytes), the XML header (`<?xml…?><xisf version="1.0" xmlns="http://www.pixinsight.com/xisf" …>` — the namespace URI is part of the format's own schema and is written verbatim; it is a URI string, not a naming of the software, and the comment says so), ONE `<Image geometry="w:h:c" sampleFormat="Float32" colorSpace="Gray|RGB" pixelStorage="Planar" location="attachment:<pos>:<size>">` with every master card as `<FITSKeyword name="…" value="…" comment="…"/>` (values formatted exactly as the FITS card writer formats them, so a reader gets the same strings), padding to a 4096-byte-aligned attachment, then the Float32 little-endian planar samples. No compression, no thumbnails, no `<Property>` metadata beyond `XISF:CreationTime` and `XISF:CreatorApplication` (= "Athenaeum <version>"). `output.format: "xisf"` writes `<stem>.xisf` for the master, the drizzled master and the weight map alike; the rejection maps stay FITS (diagnostic outputs). The written file is verified by reading it back through rustafits' own XISF reader in the writer's tests (pixel round trip bit-exact, cards present).
- **R-M4d-4 `master_lights` shape.** `master_lights (id INTEGER PRIMARY KEY, frames_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE CASCADE, run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE, group_key TEXT NOT NULL, kind TEXT NOT NULL /* master | drizzle | weight_map */, path TEXT NOT NULL, format TEXT NOT NULL /* fits | xisf */, width INTEGER NOT NULL, height INTEGER NOT NULL, channels INTEGER NOT NULL, frames INTEGER NOT NULL, total_exposure_s REAL, created_at TEXT NOT NULL, UNIQUE(run_id, group_key, kind))`, indexes on `frames_set_id` and `run_id`. Written by stage 9 in the same transaction that updates the group row; a run's rows cascade with the run; `delete_stacking_run` (if it exists — check; else the rows die with the run through the FK) needs no change. `get_stacking_run` returns the rows as `SummaryGroup.master_light_ids` is NOT added — the results card fetches the preview by `(run_id, group_key, kind)`, which it already has.
- **R-M4d-5 Preview command.** `get_master_light_preview(run_id, group_key, kind, max_px = 512) -> Vec<u8>` (JPEG; both backends; the Axum route returns `image/jpeg`) renders through the SAME path `get_frame_preview` uses for a catalog frame (the rustafits auto-stretch preview from a file path) — extract that path-based render into `api::files::render_preview_from_path(path, max_px)` if `get_frame_preview` does not already call one; the preview is cached under `<working_dir>/<set_slug>/previews/run-<id>/<group>_<kind>_<max_px>.jpg` (regenerated when the master file's mtime is newer). The Results card shows the master's thumbnail (the `Thumbnail preview — arrives in M4` placeholder goes) and, when present, the drizzled master's.
- **R-M4d-6 User presets.** Settings key `stacking.presets` = JSON `[{ "name": "…", "config": StackingConfig }]` (max 50, names unique case-insensitively, 1–60 chars); commands `list_stacking_presets() -> Vec<NamedPreset>`, `save_stacking_preset(name, config) -> Vec<NamedPreset>` (upsert by name; `paths` stripped before saving — a preset never carries folders), `delete_stacking_preset(name) -> Vec<NamedPreset>`. `get_stacking_presets` is unchanged (built-ins). The tab's preset menu lists built-ins, a divider, user presets (apply / delete with a confirm), and "Save current as…" (a name prompt inline, not a browser `prompt()`); the label logic gains user presets (canonical-JSON match, like the built-ins).
- **R-M4d-7 Acceptance data.** LDN 1272 OSC group for Bayer drizzle (compared with the debayered drizzle of the same run: level within 1 %, coverage R/B ≈ 0.25–0.5 of G's at dropShrink 0.9 and scale 2, star FWHM within ± 5 % of the debayered drizzle's on G, no colour fringing on 20 bright stars measured as the R/B centroid offset from G ≤ 0.1 px); XISF output read back by the M4a harness (`weight_audit` reads XISF) with the same terms as the FITS master ± 1e-6; the catalog rows and previews on both groups; presets round trip.

---

## File structure

- Modify `crates/athenaeum-core/src/export/calibrated_generator.rs` (`keep_mosaic`), `export/models.rs` (`CalibratedLightOptions.keep_mosaic`, `#[serde(skip)]` — a run-internal flag, never on the wire), `stacking/run.rs` (stage 1 mosaic artifact, the drizzle input `cfa`, stage 9 `master_lights` rows + XISF branch), `stacking/drizzle/mod.rs` (`CfaSource`, the mask in `deposit_band`), `stacking/config.rs` (`DrizzleConfig.bayer`, `OutputFormat::Xisf`), `stacking/master_cards.rs` (`write_master_light`/`write_drizzled_master` format switch), `stacking/paths.rs` (`previews/`, `calibrated_mosaic` cleanup).
- Create `crates/athenaeum-core/src/fits_writer/xisf_writer.rs` (`write_xisf_f32(path, width, height, channels, data, cards)`).
- Modify `crates/athenaeum-core/src/db/schema.rs` (`master_lights`), `db/stacking.rs` (`insert_master_light`, `list_master_lights`), `api/stacking.rs` (`get_master_light_preview`, the three preset commands, `NamedPreset`), `api/files.rs` (`render_preview_from_path`), `crates/athenaeum-tauri/src/commands/stacking.rs`, `crates/athenaeum-tauri/src/lib.rs`, `crates/athenaeum-web/src/routes/stacking.rs`, `routes/mod.rs`, `ts_export.rs`, `settings/mod.rs` (`STACKING_PRESETS`).
- Modify `src/components/stacking/panels/DrizzlePanel.tsx` (Bayer toggle + note), `panels/OutputPanel.tsx` (format radio live), `ResultsPanel.tsx` (thumbnails), `StackingTab.tsx` (preset menu), `stageSummary.ts`, `src/hooks/useStackingRuns.ts` (preview fetch helper), `src/types/stacking.ts` (regenerated), `src/api/` only if the JPEG transport needs a helper (`get_frame_preview` already returns bytes — reuse its transport).
- Modify the spec (§7 Bayer, §6.4/§9.5 XISF, §9.1 `master_lights`, §10.1 commands, §11.2 results thumbnails, §9.2 presets), the logging dictionary, `CLAUDE.md` (command count, Stacking paragraph).
- Create `docs/superpowers/research/2026-09-1x-m4d-acceptance-run.md` (Task 6).

---

### Task 1: The CFA mosaic artifact and Bayer drizzle

**Files:**
- Modify: `crates/athenaeum-core/src/export/models.rs`, `export/calibrated_generator.rs`, `crates/athenaeum-core/src/stacking/config.rs` (`DrizzleConfig.bayer`), `stacking/run.rs` (`calibrate_one_frame` — the mosaic artifact; `process_group_output` — `cfa` for OSC when `bayer`), `stacking/drizzle/mod.rs` (`CfaSource`, `FrameDepositCtx.cfa`, `deposit_band`), `stacking/paths.rs` (`calibrated_mosaic` kind in cleanup and usage), `src/components/stacking/panels/DrizzlePanel.tsx`, `stageSummary.ts`, `src/types/stacking.ts`, spec §7, the logging dictionary
- Test: `calibrated_generator.rs` tests, `drizzle/mod.rs` tests, `run.rs` tests

**Interfaces:**
- Consumes: `vng_debayer_f32`, `CfaGeometry`/`bayer_for(geom)` (the generator's own), `rustafits::types::BayerPattern`, `DrizzleFrame`, `FrameDepositCtx`, `RejBitmap::is_rejected(plane, x, y)`.
- Produces: `CalibratedLightOptions.keep_mosaic: bool` (`#[serde(skip)]`, default false); `execute_generation` writes the mosaic to `mosaic_path: Option<&Path>` when set (a new parameter beside the output path, `None` = today); `pub struct CfaSource<'a> { pub path: &'a Path, pub pattern: BayerPattern }`; `DrizzleFrame.cfa: Option<CfaSource<'a>>`; `DrizzleConfig.bayer: bool` (serde `bayer`); artifact kind `"calibrated_mosaic"`; `pub fn cfa_plane_of(pattern: BayerPattern, x: usize, y: usize) -> usize` (0 = R, 1 = G, 2 = B) in `drizzle/geom.rs`.

- [ ] **Step 1: `cfa_plane_of` and the mask (failing tests first)** — `cfa_plane_of(Rggb, 0, 0) == 0`, `(1, 0) == 1`, `(0, 1) == 1`, `(1, 1) == 2`; `Bggr` mirrors; `Gbrg`/`Grbg` per their names. Drizzle test: a 64×64 mosaic where every R site is 0.4, G 0.6, B 0.2 (constant per colour), identity map, scale 1, dropShrink 1.0, `cfa = Some(Rggb)` → the three output planes read 0.4 / 0.6 / 0.2 exactly where `W > 0`, and R/B coverage is 0.25, G 0.5 (each output pixel is covered by exactly its own-colour source pixel); with scale 2 the same levels hold (level preservation across the mask). Implement: `deposit_band` skips a source pixel whose `cfa_plane_of(pattern, x, y) != plane` when `ctx.cfa` is set.
- [ ] **Step 2: The generator's second write (failing test first)** — `execute_generation` with `keep_mosaic` on a synthetic RGGB frame writes both files; the mosaic's pixels equal the corrected (pre-debayer) frame bit-exact, its cards keep `BAYERPAT` and carry no `ATH_CDBM`; without the flag nothing else changes (the existing tests pass untouched).
- [ ] **Step 3: The run** — `calibrate_one_frame`: for an OSC frame in a run with `drizzle.enabled && drizzle.bayer`, set `keep_mosaic`, write to `calibrated_dir/c_<stem>.fits`, upsert the `calibrated_mosaic` artifact (same hash as the calibrated one); a cached calibrated frame whose mosaic artifact is missing regenerates BOTH (one generation). `process_group_output`: `DrizzleFrame.cfa = Some(CfaSource { path: mosaic, pattern })` per OSC frame when `bayer` (the pattern from `frames.bayerpat` via the group frame — parse with the same function the generator uses); `DrizzleStats` unchanged. `DrizzlePanel.tsx`: a "Bayer drizzle (deposit each colour's own samples, OSC only)" checkbox under the kernel; note text "R and B cover a quarter of the pixels each — use dropShrink ≥ 0.9 or more frames"; `stageSummary.ts` appends ` · Bayer`. Run test (RunContext-driven, an OSC synthetic group of 6 frames): with `bayer` the drizzled R plane equals the mosaic's R sites' level within 1 % and the `calibrated_mosaic` artifacts exist; cleanup `deleteIntermediates` removes them.
- [ ] **Step 4: Commit** — `feat(stacking): Bayer drizzle — the calibrated CFA mosaic artifact and colour-pure deposits (M4d Task 1, rulings R-M4d-1/2)`.

---

### Task 2: XISF output

**Files:**
- Create: `crates/athenaeum-core/src/fits_writer/xisf_writer.rs`
- Modify: `crates/athenaeum-core/src/fits_writer/mod.rs`, `crates/athenaeum-core/src/stacking/config.rs` (`OutputFormat::Xisf`), `stacking/master_cards.rs` (`write_master_light`/`write_drizzled_master` take `format`; names `.xisf`), `stacking/run.rs` (passes `cfg.output.format`), `src/components/stacking/panels/OutputPanel.tsx` (the format radio live), `stageSummary.ts`, spec §6.4/§9.5
- Test: `xisf_writer.rs` tests (round trip through `astroimage::ImageConverter::read_raw` — available in core's test build with the `render` feature; the writer itself is ungated), `master_cards.rs` tests

**Interfaces:**
- Produces: `pub fn write_xisf_f32(path: &Path, width: usize, height: usize, channels: usize, data: &[f32], cards: &[Card]) -> Result<(), FitsWriteError>` (tmp + atomic rename, same as `write_fits_f32`); `pub fn xisf_keyword_value(card: &Card) -> (String, String)` (the FITS-formatted value and comment strings); `OutputFormat::Xisf` (serde `xisf`); `master_cards::output_extension(format) -> &'static str`.

- [ ] **Step 1: Writer (failing tests first)** — write a 5×3×3 planar image with values `i as f32 / 100.0` and four cards (`OBJECT` string, `EXPTIME` real, `ATH_STKN` integer, `SIMPLE`-like logical is not allowed — use `ATH_TEST` logical); read it back with `read_raw`: geometry 5×3×3, samples bit-exact after dividing by the reader's convention (R-M4a-11: the rustafits XISF reader returns floats × 65535 — divide by 65535 and compare to 1e-7, since ×65535/65535 loses one ulp); the XML contains `<FITSKeyword name="OBJECT" value="'LDN 1272'"`, `name="EXPTIME" value="180.0"`. Implement R-M4d-3.
- [ ] **Step 2: The switch** — `write_master_light`/`write_drizzled_master` branch on `format`; `resolve_collision` works on the chosen extension; `OutputPanel.tsx` radio FITS/XISF with the note "XISF: one image, uncompressed, the same cards"; `stageSummary.ts` output row shows the format. `run.rs` test: an XISF run's `master_path` ends in `.xisf` and the file reads back through `PlaneReader`? — no: `PlaneReader` is FITS-only; assert through `read_raw` in the test.
- [ ] **Step 3: Commit** — `feat(stacking): XISF output for masters, drizzled masters and weight maps (M4d Task 2, ruling R-M4d-3)`.

---

### Task 3: Cataloging masters and the preview command

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (`master_lights` + indexes), `db/stacking.rs` (`NewMasterLight`, `insert_master_light`, `list_master_lights`), `stacking/run.rs` (stage 9 rows), `stacking/paths.rs` (`previews_dir`), `api/files.rs` (`render_preview_from_path`), `api/stacking.rs` (`get_master_light_preview`, `MasterLightKind`), `crates/athenaeum-tauri/src/commands/stacking.rs`, `crates/athenaeum-tauri/src/lib.rs`, `crates/athenaeum-web/src/routes/stacking.rs` + `routes/mod.rs`, `ts_export.rs`, `src/components/stacking/ResultsPanel.tsx`, `src/hooks/useStackingRuns.ts`, spec §9.1/§10.1/§11.2, `CLAUDE.md` (command count 249 → 250 + the three preset commands in Task 4 → 253), the logging dictionary
- Test: `db/schema.rs` FK-index test (extends automatically), `db/stacking.rs` tests, `api/stacking.rs` tests, `routes/stacking.rs` tests

**Interfaces:**
- Produces: the table of R-M4d-4; `pub enum MasterLightKind { Master, Drizzle, WeightMap }` (serde camelCase, ts-rs); `pub fn get_master_light_preview(ctx, run_id: i64, group_key: &str, kind: MasterLightKind, max_px: u32) -> Result<Vec<u8>, ApiError>` (`render`-gated); `pub fn render_preview_from_path(path: &Path, max_px: u32) -> Result<Vec<u8>, ApiError>` in `api/files.rs` (the shared body).

- [ ] **Step 1: Table + rows (failing tests first)** — `insert_master_light` then `list_master_lights(run_id)` round-trips every column; the FK-index test passes; a run delete cascades the rows. `run.rs`: stage 9 inserts one row per written output (master, drizzle, weight map) with `frames = included_count`, `total_exposure_s = Σ included exposure`, in the group-row update transaction.
- [ ] **Step 2: Preview (failing route test first)** — `routes/stacking.rs` test: `GET /stacking/master-preview?runId=…&groupKey=…&kind=master&maxPx=256` on a seeded DB with a small written master returns `200 image/jpeg` with a JPEG magic prefix; unknown run → 404. Implement the shared render, the cache file (mtime check), the Tauri command (returns bytes like `get_frame_preview`), registration in both routers.
- [ ] **Step 3: The card** — `ResultsPanel.tsx`: the placeholder becomes an `<img>` (object-fit contain, 160 px tall, `bg-surface` while loading, an `alt` naming the file) fetched through a `useMasterPreview(runId, groupKey, kind)` hook in `useStackingRuns.ts` (blob URL, revoked on unmount, the StrictMode-safe cancelled-flag pattern); a second thumbnail for the drizzled master when present. Spec §11.2 sentence updated; `CLAUDE.md` command count.
- [ ] **Step 4: Commit** — `feat(stacking): master_lights catalog rows and master previews on the results cards (M4d Task 3, rulings R-M4d-4/5)`.

---

### Task 4: Preset management

**Files:**
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (`STACKING_PRESETS`), `api/stacking.rs` (`NamedPreset`, `list/save/delete_stacking_preset`, validation), `crates/athenaeum-tauri/src/commands/stacking.rs` + `lib.rs`, `crates/athenaeum-web/src/routes/stacking.rs` + `routes/mod.rs`, `ts_export.rs`, `src/components/stacking/StackingTab.tsx` (menu + save-as inline form + label), `src/types/stacking.ts`, spec §9.2/§10.1, `CLAUDE.md`
- Test: `api/stacking.rs` tests, `routes/stacking.rs` tests

**Interfaces:**
- Produces: `pub struct NamedPreset { pub name: String, pub config: StackingConfig }` (serde camelCase, ts-rs); `pub const PRESET_NAME_MAX: usize = 60; pub const PRESETS_MAX: usize = 50;`; the three commands of R-M4d-6 (each returns the full list after the change); errors: `ApiError::BadRequest("preset name must be 1–60 characters")`, `("too many presets (50)")`, `("no such preset")`.

- [ ] **Step 1: Core + routes (failing tests first)** — save two presets, list returns both sorted by name (case-insensitive); saving the same name (different case) upserts, not duplicates; `paths` is stripped on save (assert the stored JSON has default paths); delete removes; the 51st save fails; route tests for the three endpoints.
- [ ] **Step 2: The tab** — the preset dropdown: built-ins, `<hr>`, user presets (click applies; a trash icon per row asks "Delete preset '<name>'?" inline — two buttons, no browser dialog), `Save current as…` opens an inline text field + Save/Cancel; the label logic matches user presets by canonical JSON (paths excluded) — `'<name>'` shown when matched. Notifications via `notify()` on save/delete (`kind: 'generic'`, `toast: true`).
- [ ] **Step 3: Commit** — `feat(stacking): user presets — list/save/delete commands and the tab's preset menu (M4d Task 4, ruling R-M4d-6)`.

---

### Task 5: Docs

- `CLAUDE.md` → Stacking (M4d paragraph; the command count line), spec §14 (M4d line; §15's Objects-page question stays open with a note "masters are cataloged in `master_lights` since M4d; their appearance on the Objects page is still the owner's call"), `docs/superpowers/open-items.md`. Commit `docs(stacking): M4d — Bayer drizzle, XISF output, master catalog, presets`.

---

### Task 6 (controller-run): M4d acceptance run on LDN 1272

**Setup:** the M4c acceptance's build recipe; set 109; runs: (A) OSC group with `drizzle.bayer` on (from Calibrate — the mosaics are new artifacts) and the M4a/M4b/M4c config otherwise; (B) both groups with `output.format = xisf` from Integrate; presets exercised through the tab.

**Measurements (note `docs/superpowers/research/<date>-m4d-acceptance-run.md`):**

| Metric | Target | How |
| ---- | ---- | ---- |
| Bayer drizzle vs debayered drizzle (A vs the M4c run), OSC | level per plane within 1 %; coverage R/B ≥ 0.9 of full at dropShrink 0.9 (the drops overlap), G ≥ 0.99; G-plane FWHM within ± 5 %; R/B centroid offset from G ≤ 0.1 px on 20 bright stars | `measure_probe`, `samestar.py`, `drzcheck.py` |
| Mosaic artifacts | 160 `c_*.fits` mosaics beside the `_d` frames, re-used on a re-run from Register (no regeneration), removed by `deleteIntermediates` | artifacts table + `ls` |
| XISF masters (B) | `weight_audit` on the `.xisf` master reports the same terms as on the FITS master of the same run ± 1e-6; the file opens in the external tool (owner smoke, owed) | harness |
| `master_lights` rows | one per output per group; previews render on the results cards (both groups, master + drizzle) | sqlite + click-through |
| Presets | save "LDN test", reload the tab, the label shows it, delete works | click-through |

**Docs:** the note; open-items (the owed XISF owner smoke); `CLAUDE.md`; the memory file.

---

## Self-review (done while writing)

- **Spec coverage:** §14 M4 "Bayer drizzle (stage 1 keeps the CFA mosaic)" (Task 1), "XISF output" (Task 2), "cataloging masters (a `master_light` entity linked to the set and a preview in the Results cards)" (Task 3), "preset management" (Task 4); §15's Objects-page appearance deliberately left to the owner.
- **Placeholders:** none.
- **Type consistency:** `CfaSource`/`cfa_plane_of`/`DrizzleConfig.bayer` (Task 1); `OutputFormat::Xisf`/`write_xisf_f32` (Task 2) used by `master_cards.rs`; `MasterLightKind`/`get_master_light_preview` (Task 3) consumed by `useStackingRuns.ts`; `NamedPreset` + the three commands (Task 4) consumed by `StackingTab.tsx`.
