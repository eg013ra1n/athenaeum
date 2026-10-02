# 6b-2 research: plan steps 4 (Prepare) and 5 (Solve)

Repo `athenaeum`, branch `collab-wave6b2`, HEAD `e03bc48f`. Read-only. All paths
relative to `crates/athenaeum-core/src/` unless stated. Line numbers are at HEAD.

---

## 0. Headline findings (things that contradict the spec or will surprise a planner)

1. **"In place" for integer BITPIX breaks the pipeline's value-domain assumption.**
   The spec's in-place row (§5.1) lists BITPIX 8/16/32 with BZERO/BSCALE. `PlaneReader`
   decodes **physical** values (BZERO/BSCALE applied, no scaling), so a 16-bit file reaches
   Measure in ADU 0..65535. Every stage after Calibrate assumes **unit-domain float32**
   (`stacking/measure.rs:28` `pub const ADU_SCALE: f32 = 65535.0;` and
   `measure_plane...` does `data.iter().map(|v| v * ADU_SCALE)` at `measure.rs:288`;
   `structure.rs:162` `upper_limit: 1.0` "in unit domain"; the calibrated generator always
   writes float32 divided by `scale_divisor_for_bitpix`, `light_cal.rs:80-88`, stamped
   `ATH_CSCL`). Athenaeum-generated replicas are float32 unit-domain, so they are fine.
   **Attested originals** (R3 / `calibrated_externally`, published as-is,
   `api/collab.rs:~5840-5880`) can be 16-bit or ADU-domain float. Needs an owner ruling;
   my recommendation: in place only when BITPIX is −32/−64; integer BITPIX goes to
   "convert" (`PlaneReader` physical values ÷ `scale_divisor_for_bitpix(bitpix)`, the
   generator's own divisor). Float files in an ADU domain from foreign tools remain an open
   risk (no card says the domain; `ATH_CSCL` only on Athenaeum output).
2. **Tile-compressed FITS cannot be decoded anywhere in the repo.** No Rice/GZIP/HCOMPRESS
   decoder exists (only `fits_writer/header_patch.rs:204-220` *recognizes* `ZIMAGE = T` for
   header patching). rustafits `formats/fits.rs::read_fits_image` reads the primary HDU only
   (no XTENSION, no ZIMAGE). An uncompressed `IMAGE` extension is also unreadable by every
   pixel reader. The spec's "converted to float32 FITS" row needs either a decoder (new
   scope) or "unsupported file". The scanner only catalogs `fits/fit/fts/xisf`
   (`scanner/mod.rs:26`), so `.fz` files never become frames; a tile-compressed file named
   `.fits` is an edge case. Recommend: exclude as "unsupported file" in 6b-2, open-items entry.
3. **XISF conversion must undo rustafits' ×65535.** `astroimage::ImageConverter::read_raw`
   on XISF always returns `PixelData::Float32` in an ADU-like domain: floats are
   bounds-normalized then `× 65535.0` (`rustafits/src/formats/xisf.rs:412-435`, a documented
   production contract at `xisf.rs:553-571`); UInt16 as-is; UInt8 ×257; UInt32 >>16.
   Athenaeum writes XISF calibrated lights with `bounds="0:1"` (`fits_writer/mod.rs:64-84`,
   `XisfBounds::Unit`). So Prepare's XISF → float32 FITS must divide by 65535 to land in the
   unit domain (gives the bounds-normalized value for every sample format). x·65535/65535
   may differ by ≤1 ulp from the stored float; pin with a 1-ulp tolerance, not bit equality.
   rustafits' FITS reader is also lossy/odd for conversion (BITPIX 8 → ×256 into u16,
   nonstandard BZERO clamped to [0,65535], `fits.rs:180-245`) — use `PlaneReader` for any
   FITS Prepare can read, never `read_raw`.
4. **`debayer_file(src, dest, pattern)` cannot be what the generator calls.** The generator
   debayers an **in-memory** calibrated buffer (`calibrated_generator.rs:690-723`); there is
   no source file to read. The shareable unit is a buffer function (VNG + `bayer_for` phase
   fold + pool install) plus a cards function (strip `MOSAIC_KEYWORDS`, add
   `ATH_CDBM = 'VNG'`); `debayer_file` wraps them with a `PlaneReader` read and a FITS
   write. "Bytes identical for the catalog path" is pinnable only on the **pixel data**:
   the header of a Prepare output differs by design (allowlist + `ATH_STK`), and the
   generator's file carries `CALSTAT`/`ATH_C*` cards. Also: the parameter must be a
   `CfaGeometry` (pattern + offsets), not a bare pattern — `bayer_for` folds odd
   `XBAYROFF`/`YBAYROFF` into the pattern (`calibrated_generator.rs:165-208`); dropping the
   offsets swaps R and B on phase-shifted sensors.
5. **There is no "debayer method" config field.** `CalibratedLightOptions`
   (`export/models.rs:320-377`) has `debayer_osc` only; VNG is hard-coded (`ATH_CDBM = 'VNG'`,
   `calibrated_generator.rs:719`). `prepare_subtree` should be
   `{debayerOsc, debayer: "VNG", prepareVersion: N}` with constants.
6. **ROWORDER is deliberately NOT part of CFA geometry.** `integration/cfa.rs:31-50`
   (`CfaGeometry` rustdoc) and `light_resolve.rs:314-319`: BAYERPAT describes the mosaic in
   FILE row order. The spec line "BAYERPAT/ROWORDER come from the file's header" should be
   read as: geometry = BAYERPAT/XBAYROFF/YBAYROFF; ROWORDER is only copied through
   (it is in `REGISTERED_COPY_THROUGH`).
7. **The allowlist drops the mosaic cards.** `REGISTERED_COPY_THROUGH`
   (`stacking/register/writer.rs:38-42`) has no BAYERPAT/XBAYROFF/YBAYROFF (a registered
   frame is resampled, so it has no CFA). A **converted mosaic** (`p_<stem>.fits`, debayer
   off, or the Bayer-drizzle `calibrated_mosaic` of a converted file) would lose its CFA
   identity. Either the Prepare cards add the three mosaic cards for a mosaic output, or the
   Project arm of `cfa_geometry` always reads the landed file's header (geometry is
   unchanged by conversion). Recommend both.
8. **Most OSC replicas are already debayered.** The publish generation uses the
   publisher's options; `debayer_osc` defaults to `true` (`export/models.rs:338-341`), so OSC
   replicas are typically 3-plane `c_<stem>_d.*` with BAYERPAT stripped, while the manifest's
   `meta.bayerpat` still says OSC (it comes from the raw source frame, `frame_meta.rs:96-101`).
   Prepare must decide from the FILE (NAXIS3 = 3 → RGB in place; 1 plane + BAYERPAT →
   mosaic). Consequences: (a) Bayer drizzle has no mosaic for such frames (falls back with
   the existing warning, `run.rs:~7985`); (b) with `debayer_osc` **off**, a group can mix
   3-plane replicas and 1-plane mosaics — mixed plane counts in one group, and
   `plan::expected_ln_channels` (`plan.rs:824-830`) returns 1 for OSC+debayer-off while the
   in-place RGB files are 3-plane. Needs a rule (owner ruling); recommendation: for a project
   OSC group, the plane count is decided per group — debayer every mosaic when any member is
   already RGB, or force `debayer_osc` on for project OSC groups and say so once.
9. **The plate-solve catalog is only opened in the hosts.** `resolve_layer_caches` lives in
   `athenaeum-tauri/src/commands/plate_solve.rs:22-47` and
   `athenaeum-web/src/routes/plate_solve.rs:43-~70` (both `<db dir>/catalogs/smac_gaia` +
   `discover_layers` + `StarCache::open`). The run in core needs its own opener (core has
   `plate_solve::discover_layers` only). There is no existing solver config hash and no
   catalog identity; both must be defined.
10. **Solving a unit-domain float frame is unproven.** rustafits' analyzer
    (`rustafits/src/analysis/mod.rs:640-700`) passes FITS float data through un-normalized;
    every production solve today runs on raw ADU lights. Detection is mostly
    noise-relative, but this must be checked on a real calibrated replica early (real data
    first). If it fails, solve on an ADU-scaled scratch copy (×65535) of the prepared file.
11. **`stored_solve(conn, origin, frame_id)` has no subject.** The locally-solved record of a
    project frame lives in `stacking_artifacts` keyed by `project_id`; the spec's
    `FrameOrigin::Project { frame_uuid, publisher, landed_path, blake3, size_mtime, manifest }`
    has no `project_id` and no `byte_size` (the Prepare hash needs "blake3 and byte size of
    the row"). Add `project_id` and `byte_size` to the variant (or pass the subject into
    `stored_solve`).
12. **Negative solve outcomes.** "Never re-solved while the hash holds" only covers a stored
    result. Without caching failed/refused outcomes, every run re-attempts every unsolvable
    frame (minutes per frame on the autoFOV ladder). Recommend caching the outcome in the
    same `solve` artifact (`payload_json = {"outcome":"failed"|"refused","reason":…}`),
    which also lets the plan count "will be solved locally" honestly.

---

## 1. Prepare's "use in place" rule — what the readers decode

### `integration/banded.rs`
- `PlaneKind` (`banded.rs:31-39`): `U8/I16Be/I32Be/F32Be {bzero,bscale}` (f32),
  `F64Be {bzero,bscale}` (f64), `F32Le` (spill scratch, already physical). Decode is
  `raw * bscale + bzero` — **physical values, no unit scaling**.
- `plane_kind_for_bitpix(bitpix, bzero, bscale)` (`banded.rs:141-150`).
- `probe_fits(path) -> Option<(File, FitsInfo)>` (`banded.rs:252-296`, `pub(crate)`): reads
  primary-header blocks until END (gives up after 64 blocks), harvests
  BITPIX/NAXIS/NAXIS1/2/3/BZERO/BSCALE; no SIMPLE check, no BLANK handling, no XTENSION.
  Accepts:
  ```rust
  let ok_bitpix = matches!(info.bitpix, 8 | 16 | 32 | -32 | -64);
  let ok_shape = (info.naxis == 2 && info.naxis3 == 1)
      || (info.naxis == 3 && (info.naxis3 == 1 || info.naxis3 == 3));
  if ok_shape && ok_bitpix && info.w > 0 && info.h > 0 { Some((f, info)) } else { None }
  ```
  A tile-compressed file (primary `NAXIS = 0`) and an IMAGE-extension file return `None`.
  It returns an OPEN `File` — a cheap "decodable in place" test must drop it immediately
  (spec: never hold a file open across stages).
- `probe_bitpix(path)` (`banded.rs:298-302`, `pub`) answers only for 2-D single-plane files.
- `BandSource` (master builds, light calibration) spills anything `probe_fits` rejects
  through `astroimage::ImageConverter::read_raw` (`spill_via_read_raw`, `banded.rs:321-370`)
  and refuses multi-channel; it is not the stacking stages' reader.

### `integration/plane_reader.rs`
- `PlaneReader::open(path)` (`plane_reader.rs:22-37`) = `probe_fits` or
  `BadInput("…: not an uncompressed 1- or 3-plane FITS image")`; `channels = naxis3`.
  `read_rows_with_scratch` / `read_plane` positional reads per plane. This is the reader of
  every stage that touches the calibrated file (Measure `measure.rs:608`, Register writer
  `register/writer.rs:~186`, `RegisteredSource` fallback, drizzle).

**Cheap in-place test** = `probe_fits(path).is_some()` (header only, ≤64 blocks), plus
(recommended, see §0.1) `bitpix ∈ {-32,-64}`. Expose as a `pub(crate) fn`
`plane_readable(path) -> Option<FitsInfo>` that drops the handle. For CFA/headers use
`fits_parser::FitsHeader::from_path` (`fits_parser/fits_header_reader.rs:20-25`, header
blocks only).

### Reading non-in-place files today
- **XISF**: only `astroimage::ImageConverter::read_raw` (`rustafits/src/converter.rs:75-77`
  → `formats::read_image`, `rustafits/src/formats/mod.rs:12-25`). Monolithic XISF with
  `location="attachment:…"` or `"embedded"`; other schemes (distributed `path(…)`/`url(…)`,
  inline) bail "Unsupported XISF location" (`xisf.rs:113-126`); compression zlib / lz4 /
  lz4hc / zstd with optional byte shuffle (`xisf.rs:129-157, 344-400`); sample formats
  UInt8/16/32, Float32/64; Planar or Normal (converted to planar, `xisf.rs:~654`); picks the
  largest `<Image>`. Always returns `PixelData::Float32` in the ×65535 domain (§0.3).
  Header text: `fits_parser::extract_xisf_header(path)` (`fits_parser/mod.rs:150`, reads ≤1 MB);
  frame-shaped metadata incl. CFA from `<ColorFilterArray>`:
  `fits_parser::parse_xisf(path, 0)` (`fits_parser/mod.rs:580`, CFA at `:678-760`).
- **FITS that `probe_fits` rejects**: `read_raw` → `read_fits_image`
  (`rustafits/src/formats/fits.rs:126-312`) — primary HDU only; NAXIS3 must be 1 or 3;
  BITPIX 16 → u16 (BZERO 32768 fast path; other BZERO clamped to [0,65535]), 8 → u16 ×256,
  32/−32/−64 → f32 physical. No tile compression, no extensions.
- **Writing float32 FITS**: `fits_writer::write_fits_f32_with(path, w, h, channels, data,
  cards, Durability)` (`fits_writer/writer.rs:123-…`; atomic tmp + rename;
  `Durability::Volatile` = the run's regenerable intermediates, `fits_writer/mod.rs:31-34`).
  Plane-at-a-time variant: `write_fits_f32_streaming_with` (`writer.rs:269`), used by the
  registered writer to keep one plane resident.
  Recommended converter: `prepare::convert_to_fits(src, dest, cards, durability) ->
  (w, h, planes)`: FITS readable by `PlaneReader` → read planes, ÷ divisor for integer
  BITPIX; XISF → `read_raw` ÷ 65535; anything else → `Unsupported(reason)`.

---

## 2. The debayer inside `execute_generation`

`export/calibrated_generator.rs` (module is `#[cfg(feature = "render")]`,
`export/mod.rs:6-7`).

- Imports: `use astroimage::processing::vng::vng_debayer_f32;` and `astroimage::BayerPattern`
  (`:42-43`). Signature (`rustafits/src/processing/vng.rs:141-146`):
  `pub fn vng_debayer_f32(data: &[f32], width: usize, height: usize, pattern: BayerPattern) -> Vec<f32>`
  — planar `[R|G|B]`, "any float scale".
- `const MOSAIC_KEYWORDS: [&str; 3] = ["BAYERPAT", "XBAYROFF", "YBAYROFF"];` (`:49`;
  ROWORDER kept on purpose, `:45-48`).
- Pattern source: `spec.cfa_geometry` ← `resolve_frame_inputs` ←
  `light_resolve::resolve_cfa_geometry(conn, frame_id)` (catalog `frames` columns).
  `spec.debayer = opts.debayer_osc && resolved.cfa_geometry.is_some()` (`:416`).
  `bayer_for(geom: CfaGeometry) -> BayerPattern` (`:182-208`, `pub`) folds odd offsets.
  No ROWORDER handling anywhere (by design, §0.6).
- The block (`:690-723`, verbatim):
  ```rust
  let t = Instant::now();
  let (data, channels) = match (spec.debayer, spec.cfa_geometry) {
      (true, Some(geom)) => {
          let pattern = bayer_for(geom);
          let rgb = match pool {
              Some(p) => {
                  p.install(|| vng_debayer_f32(&frame.data, frame.width, frame.height, pattern))
              }
              None => vng_debayer_f32(&frame.data, frame.width, frame.height, pattern),
          };
          (rgb, 3usize)
      }
      _ => (frame.data, 1usize),
  };
  let debayered = channels == 3;
  ...
  let mut cards = spec.cards.clone();
  if debayered {
      cards.retain(|c| !MOSAIC_KEYWORDS.contains(&c.keyword.as_str()));
      cards.push(Card::new("ATH_CDBM", CardValue::Str("VNG".into()))?);
  }
  if let Some(card) = hot_card { cards.push(card); }
  ```
  then `write_calibrated_output(output_path, w, h, channels, &data, &cards, spec.format,
  durability)` (`:732-741`; `light_cal.rs:355-368`: `write_image_f32_with(..., XisfBounds::Unit, ...)`
  + sampling xxh3).
- The `keep_mosaic` second output (`:621-688`) is written BEFORE the debayer from the same
  buffer with the mosaic cards kept and no `ATH_CDBM`.

**Proposed extraction** (keeps the generator byte-identical):
```rust
/// The VNG debayer of one calibrated CFA plane, phase folded in.
pub(crate) fn debayer_plane(data: &[f32], w: usize, h: usize, geom: CfaGeometry,
                            pool: Option<&Arc<rayon::ThreadPool>>) -> Vec<f32>;
/// A debayered file's cards: the mosaic keywords removed, `ATH_CDBM = 'VNG'` added.
pub(crate) fn debayered_cards(cards: &mut Vec<Card>) -> Result<(), FitsWriteError>;
/// Prepare's entry: read a 1-plane FITS mosaic (PlaneReader), debayer, write a 3-plane
/// float32 FITS with `cards` (already debayered_cards'd by the caller). Cancel-aware.
pub fn debayer_file(src: &Path, dest: &Path, geom: CfaGeometry, cards: &[Card],
                    durability: Durability, pool: Option<&Arc<rayon::ThreadPool>>,
                    cancel: &AtomicBool) -> anyhow::Result<u64 /* bytes */>;
```
The generator calls `debayer_plane` + `debayered_cards` in place of the inline block. Could
live in the generator module or a new `calibration_library::debayer`; both are `render`.

**Pin "bytes identical for the catalog path"**: (1) all existing generator tests unchanged
(`osc_generation_debayers_to_3_planes` `:1319`, `debayer_off_keeps_cfa` `:1367`,
`keep_mosaic_writes_the_corrected_mosaic_beside_the_debayered_output` `:1693`,
`no_mosaic_without_the_flag_or_without_a_mosaic` `:1785`,
`bayer_for_folds_offset_parity_into_the_pattern` `:1643`) and
`stacking::run::tests::calibrate_fan_out_matches_the_sequential_stage_byte_for_byte`
(`run.rs:10438`); (2) a new test: generate the same OSC light debayer-off (its output IS the
corrected mosaic — already pinned bit-exact against the `keep_mosaic` file), run
`debayer_file` on it, compare pixel planes `to_bits()` with the debayer-on generator output
(f32 FITS round trip is exact). Add an odd-`XBAYROFF` fixture so the phase fold is covered.

---

## 3. Header policy

- `REGISTERED_COPY_THROUGH` (`stacking/register/writer.rs:38-42`, verbatim):
  ```rust
  pub const REGISTERED_COPY_THROUGH: &[&str] = &[
      "EXPTIME", "GAIN", "OFFSET", "EGAIN", "XBINNING", "YBINNING", "XPIXSZ", "YPIXSZ", "CCD-TEMP",
      "SET-TEMP", "INSTRUME", "TELESCOP", "FOCALLEN", "APTDIA", "FILTER", "DATE-OBS", "OBJECT",
      "OBJCTRA", "OBJCTDEC", "RA", "DEC", "SITELAT", "SITELONG", "SITEELEV", "OBSERVER", "ROWORDER",
  ];
  ```
- `source_cards_from_file(path)` (`writer.rs:58-70`): FITS-only
  (`FitsHeader::from_path` → `parse_stored_header_keys(FileFormat::FITS, …)` → allowlist →
  `card_from_kv`, sorted). An XISF variant is `extract_xisf_header` +
  `parse_stored_header_keys(FileFormat::XISF, …)`.
- `build_registered_cards` (`writer.rs:74-124`) appends `IMAGETYP='LIGHT'`, `ATH_REG=T`,
  `ATH_REGV`, `ATH_REGR/M/K/C/S/T`.
- `ATH_STK = T` is stamped by masters (`master_cards.rs:167, 499, 522`) AND by the LN
  reference, a non-master stacking intermediate (`stacking/ln/reference.rs:253-272`,
  `reference_cards`: `IMAGETYP='LN Reference'`, `ATH_STK=T` "stacked by Athenaeum; never
  cataloged", `ATH_STKV`). That is the precedent for Prepare outputs.
- Scanner (`scanner/mod.rs:468-497`): `calibrated_light_identity(&keys)` (needs `CALSTAT` and
  `ATH_CSRC` or `ATH_CSRN`, `fits_parser/calibrated_light.rs:157-180`) → if `ATH_PRJ` present
  → `reconcile_project_file` (diversion), else "never cataloged"; then
  `if keys.contains_key("ATH_REG") || keys.contains_key("ATH_STK")` → never cataloged.
  Existing pin: `scanner::tests::scan_never_catalogs_a_stacking_artifact` (`scanner/mod.rs:3502`).
- `ATH_PRJ`/`ATH_FILT` are stamped on publish (`api/collab.rs:3312-3336`,
  `stamp_publish_cards`), together with the plate-solve WCS swap.

**Prepare output cards (recommendation)**: allowlist from the source header (FITS or XISF)
+ `BAYERPAT/XBAYROFF/YBAYROFF` when the output is still a mosaic + `IMAGETYP='LIGHT'` +
`ATH_STK = T` + `ATH_STKV` + a provenance card (e.g. `ATH_PREP = 'debayered'|'converted'`,
new keyword) + `ATH_CDBM='VNG'` when debayered. Never `CALSTAT`/`ATH_CSRC`/`ATH_PRJ`/`ATH_FILT`
(test: assert absent on every Prepare output, and a scanner test that a Prepare output is
skipped). Note FILTER copied through is the publisher's raw name (§5.9's canonical FILTER is
step 11's concern).

---

## 4. Calibrate stage today and how Prepare mirrors it

- Driver: `run_pipeline` (`run.rs:1261-1312`): masters → **calibrate** → measure → reference
  → register → output (normalize/integrate/drizzle live inside `stage_output` →
  `process_group_output`, `run.rs:6790`).
- `stage_calibrate` (`run.rs:2001-2290`) begins with the 6b-1 guard
  `let Some(set_id) = rc.subject.frame_set_id() else { return Err(RunError::Other(PROJECT_STACKING_UNAVAILABLE.into())); };`
  (`:2002-2005`). Per group: resolve loop on the run thread (`resolve_calibrate_job`,
  `:1604-1752`) → `prebuild_hot_maps` → `build_preloaded_masters` → `fan_out` of
  `execute_generation` → `commit_calibrated` (`:1867-1952`) in frame order; per-frame
  failure = `rc.runtime_exclusions.push((frame_id, reason))`; reuse sets
  `rc.cached_calibrated.insert(frame_id, true)`.
- Artifacts: `calibrated` (path, `config_hash` = stage-1 hash, size, modified_at from
  `file_identity`, `run.rs:518-523`, rfc3339 mtime) and, when
  `wants_cfa_mosaic(cfg, group)` (`plan.rs:805-810`: `drizzle.enabled && drizzle.bayer &&
  calibration.debayer_osc && OSC`), `calibrated_mosaic` with the SAME hash, written in one
  transaction. Output names: `layout.calibrated_dir(key).join(spec.output_filename("<stem>.fits"))`
  with `calibrated_file_stem(group, frame)` (`run.rs:1574-1591`, `<stem>_f<id>` on a
  basename collision).
- Freshness: `find_artifact(…, "calibrated", Some(frame_id))` + `is_fresh(row, &hash)`
  (`plan.rs:715-722`: `config_hash ==` and on-disk `len == row.size`), and the mosaic row
  when wanted. `rerun_from == Calibrate` skips reuse.
- Stage-1 hash: `HashMemo::calibration_hash_checked` (`plan.rs:430-442`) → `hash_by_origin`
  (`plan.rs:381-391`, 6b-1 dispatcher, `Catalog` arm only) → `calibration_hash_for`
  (`plan.rs:316-350`) = `stage_hash(&calibration_subtree(cfg), &master_identities,
  &[SourceIdentity{file_id, size, modified_at}])`. `stage_hash` (`config.rs:626-648`) =
  xxh3 of canonical JSON `{config, upstream(sorted), sources(sorted)}`.
  `calibration_subtree` (`config.rs:654-672`) is golden-pinned
  (`plan.rs:6278 stage_hashes_are_pinned_to_golden_literals`,
  `plan.rs:6356 calibration_hash_for_is_the_pinned_formula`).
- Every later hash chains on the stage-1 hash: `measurement_hash_for(cfg, calibrated_hash)`
  (`plan.rs:462`), `registration_hash_for(… reference_calibrated_hash, frame_calibrated_hash,
  frame_measurement_hash)` (`plan.rs:494`), LN. So **the HashMemo Project arm returning the
  Prepare hash is the single plug point** — Measure (`run.rs:~3090`), the plan gate
  (`plan.rs:2141-2185`) and Register pick it up unchanged.
- Consumers of the `calibrated` path afterwards: Measure reads the artifact row
  (`run.rs:2953-2965`) into `MeasuredFrame.calibrated`, which feeds Reference
  (`run.rs:3612, 3739-3820`), Register (`4317, 4521, 5189, 5357, 5458-5567`, cards via
  `source_cards_from_file` at `5820`, pixels via `write_registered_frame` at `5833`), Output
  (`process_group_output` members `6824`, LN `7027/7132`, reference cards `7629-7630`),
  Drizzle (`calibrated_mosaic` lookup `7955-7975`).

**Prepare mirror (recommendation)**
- `stage_calibrate` dispatches: Project subject → `stage_prepare(rc)` (and, in step 13, an
  attested catalog set → the same Prepare core). Write a Prepare core that takes
  `(src_path, cfa geometry source, identity token)` rather than a `FrameOrigin::Project`, so
  R3 reuses it.
- `prepare_subtree(cfg)` in `config.rs` = `json!({"debayerOsc": cfg.calibration.debayer_osc,
  "debayer": "VNG", "prepareVersion": PREPARE_VERSION})` (+ the conversion domain rule
  version). Prepare hash = `stage_hash(&prepare_subtree(cfg), &[&format!("blake3:{b}")],
  &[SourceIdentity{file_id: 0, size: byte_size, modified_at: String::new()}])`, pinned by a
  new golden literal; `calibration_subtree` and the existing literals must not move.
  Keep `drizzle.bayer` out (the mosaic rides the same hash, as today).
- Decision per frame (header only): in place (FITS, `probe_fits` ok, float BITPIX — §0.1)
  → `calibrated` artifact path = `landed_path`, size/mtime from `file_identity(landed)`;
  mosaic + `debayer_osc` → `debayer_file` → `prepared/<group>/p_<stem>_d.fits`, and when
  `wants_cfa_mosaic` the `calibrated_mosaic` row points at the landed file; convert
  (XISF / integer FITS) → `p_<stem>.fits` (then debayer as above; mosaic row → `p_<stem>.fits`);
  unreadable / distributed / tile-compressed → exclusion "unsupported file".
- `WorkingLayout` (`stacking/paths.rs:27-140`) gains `prepared_root()` /
  `prepared_dir(group)`. `work_usage` (`paths.rs:741-764`) and `cleanup_work`
  (`paths.rs:840-890`, Intermediates/All remove directories, then rows via
  `INTERMEDIATE_ARTIFACT_KINDS` `paths.rs:806-815`) gain the prepared root;
  `WorkUsage` is ts-exported, so `src/types` regenerates (6b-1 note:
  `git diff --exit-code src/types`). Cleanup never deletes an artifact's path, only
  directories (`paths.rs:869-880`; run-end cleanup `run.rs:9821-9837` goes through the same
  function), so an in-place `calibrated` row never removes the landed file.
- File name: `calibrated_file_stem` uses `GroupFrame.filename` = the manifest `file_name`
  (often `c_<stem>_d.fits`), so outputs read `p_c_<stem>_d_d.fits` / `r_c_<stem>_d_d.fits`
  — cosmetic, but decide now.
- Logs (§14): `debug!(run_id, frame_uuid, group_key, prepared = "in_place"|"debayered"|"converted")`.

---

## 5. CFA geometry from a file's own header

- Today: `light_resolve::resolve_cfa_geometry(conn, frame_id)` (`calibration_library/light_resolve.rs:320-363`)
  reads `frames.bayerpat/xbayroff/ybayroff`, `Bayer::parse` (warn on unrecognized → `None`),
  missing offset → 0 with a `debug!`. Routed through `stacking::origin::cfa_geometry`
  (`stacking/origin.rs:53-63`, `Catalog` arm only).
- Header readers available:
  - FITS: `FitsHeader::from_path` (`get_str("BAYERPAT")`, `get_i32("XBAYROFF")`), or
    `fits_parser::parse_fits(path, 0)` / `parse_fits_with_header` which fill
    `Frame.bayerpat/xbayroff/ybayroff/roworder` (`fits_parser/mod.rs:428-436`).
  - XISF: `fits_parser::parse_xisf(path, 0)` — BAYERPAT FITSKeyword, falling back to the
    native `<ColorFilterArray pattern=…>` element (offsets stay `None`), `mod.rs:734-770`.
  - Key-map form: `stored_header::cfa_fields_from_keys(&keys)` (`stored_header.rs:243-262`)
    → `CfaHeaderFields {bayerpat, xbayroff, ybayroff, roworder}`.
- Recommendation: extract the column-to-geometry rule of `resolve_cfa_geometry` into a pure
  `cfa_geometry_from_fields(bayerpat, xoff, yoff, log_ctx) -> Option<CfaGeometry>` (one
  Bayer::parse + default-0 rule), keep `resolve_cfa_geometry` byte-for-byte (pinned by
  `origin::tests::the_catalog_arms_read_what_they_read_today`, `origin.rs:91-112`), and give
  the Project arm a `cfa_geometry_of_file(landed_path)` using `parse_fits`/`parse_xisf`.
  Read it once per frame in Prepare and keep it on the run (the drizzle arm then needs no
  file read), or read the landed header in the drizzle arm (`run.rs:7970-7975`) behind the
  identity check.

---

## 6. Identity re-check (`size:mtime`)

- Format: `project_frames_local.size_mtime_seen TEXT` = `"<len>:<mtime_secs>"`
  (`collab/storage/sweep.rs:35-46`). Existing helpers (ungated, `pub`):
  ```rust
  pub struct Stamp { pub size: u64, pub mtime: i64 }      // sweep.rs:16-19
  Stamp::of(&Metadata), Stamp::parse(&str), Stamp::encode(), Stamp::matches(&other)
  // size equal AND |mtime diff| <= MTIME_TOLERANCE_SECS (2)
  pub fn stat_verdict(path: &Path, recorded: Option<Stamp>) -> StatVerdict  // sweep.rs:64-76
  // Same | Drifted(Stamp) | Missing | Unreadable(String)
  ```
  The column can be NULL while `landed_path` is set (cleared on a version bump,
  `db/collab_frames.rs:428-436, 604, 1290, 1578`) — treat NULL as not stackable / changed.
- Reads of the calibrated (= landed, when in place) path, by stage:
  - Prepare: header probe + pixel read when converting/debayering.
  - Solve sub-step: the solver opens the prepared/landed file.
  - Measure: `PlaneReader::open(&calibrated_path)` probe (`run.rs:2983`), keyword weights
    `FitsHeader::from_path` (`run.rs:3342`), `measure_frame_with_fits` in the fan-out
    (`run.rs:3189` → `measure.rs:608`).
  - Reference/Register: reference stars and per-frame detection on the calibrated file
    (`run.rs:3612-3820`, `register_group_pass` candidate list `4317`, two-pass `5189`),
    `write_one_registered_frame` (`5808-5838`: header + pixels).
  - Normalize/Integrate: read the materialized registered artifact; the calibrated file only
    on the `RegisteredSource` fallback warp. Output reference cards `7629-7630`.
  - Drizzle: deposits from the calibrated file / `calibrated_mosaic`.
- **Where one hook can live**: a funnel next to the others,
  `stacking::origin::landed_unchanged(origin, path) -> Result<(), String>`: `Catalog` → Ok;
  `Project` → Ok unless `path == landed_path` and `stat_verdict(path,
  Stamp::parse(size_mtime))` is not `Same` → `Err("changed during the run")`. Called from a
  per-frame pre-pass at each stage boundary on the run thread: Prepare resolve loop, the
  Solve sub-step, Measure's per-frame loop (before `run.rs:2983`), Register's candidate build
  (`register_group_pass`, `run.rs:~4310`) and reference read, and `process_group_output`'s
  member snapshot (`run.rs:6817-6830`, covers LN/Integrate/Drizzle). Before Register a
  failure is `rc.runtime_exclusions.push`; after Register (rows already written as
  included) use `exclude_frame_and_persist` (`run.rs:6024-6047`), then re-check the
  `included_count < 3` skip. A per-read wrapper inside `PlaneReader` is not practical
  (opens sit inside measure/register/integration internals). POSIX keeps an already-open
  inode readable; reads are per call, so no file is held across stages today.

---

## 7. Solve

### `plate_solve/service.rs`
- `solve_frame_with_hints` (`service.rs:130-366`):
  ```rust
  pub fn solve_frame_with_hints(frame: &Frame, file_path: &str, hints: &SolveHints,
      caches: &solvemyastro::Caches<'_>, config: &PlateSolveConfig,
      cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<SolveResult>
  ```
  DB-free. Uses from `frame`: `id` (span field `frame_id`, `unwrap_or(-1)` — pass `None`),
  `date_obs` (`observation_epoch`, `hints.rs:247`), `xpixsz`/`instrume`/`telescop` (focal
  length derivation, `camera_defaults`), `focallen`. Hard-coded `SolveConfig
  {quad_tolerance: 0.007, catalog_mag_limit: 19.0, fit_sip: true, sip_order: config.sip_order}`.
  solvemyastro decodes the image itself via rustafits (FITS/XISF, 1 or 3 planes).
  `SolveHints` (`rustafits/src/platesolving/types.rs:37-48`): `ra, dec, fov_deg, rotation,
  pixel_scale_arcsec` (all `Option<f64>`). `Frame` derives `Default` (`models.rs:55`), so a
  transient frame is `Frame { date_obs, focallen, naxis1, naxis2, instrume, ..Default::default() }`.
  The manifest has no XPIXSZ, so `frame_fov_deg` (`hints.rs:29-31`) gives `None`; compute
  `fov_deg = pixelScaleArcsec × max(naxis1, naxis2) / 3600` directly. (Alternative: the
  prepared/landed file's own header carries XPIXSZ/FOCALLEN/OBJCTRA/RA/DEC through the
  allowlist; `parse_fits(path, 0)` + `extract_hints(&frame, None)` then override with
  manifest scale / target position.)
- `store_result` (`service.rs:387-510`) — gate block verbatim (`:402-432`):
  ```rust
  if config.blind_gate_enabled {
      let m = BlindGateMetrics {
          inliers: result.matched_stars,
          expected_in_fov: result.expected_catalog_stars_in_fov,
          rms_px: result.rms_residual_px,
          adaptive_tol_px: adaptive_tol_px(
              result.pixel_scale_arcsec,
              config.base_verification_tolerance_arcsec,
          ),
          inlier_ratio: result.inlier_ratio,
          recovered_scale_arcsec: result.pixel_scale_arcsec,
          header_scale_arcsec: header_pixel_scale_arcsec(conn, frame_id),
      };
      if !blind_gate_ok(GateStage::ScaleCleared, &m, config) {
          let reason = format!(
              "Rejected: low confidence ({} inliers, ratio {:.2}, {:.2}\"/px)",
              result.matched_stars, result.inlier_ratio, result.pixel_scale_arcsec
          );
          tracing::warn!(frame_id, stage = "store", outcome = "rejected_low_confidence", …,
              "refusing to persist low-confidence solve, WCS/focal length not written back");
          return Ok(StoreOutcome::RejectedLowConfidence { reason });
      }
  }
  ```
  then builds `PlateSolveRecord` inline (`:439-468`), `insert_plate_solve`,
  `update_frame_from_solve` (writes `frames` by id), optional DSO label
  (`update_frame_object_if_missing`).
- `blind_gate_ok` (`service.rs:535-578`, verbatim):
  ```rust
  pub(crate) fn blind_gate_ok(stage: GateStage, m: &BlindGateMetrics, cfg: &PlateSolveConfig) -> bool {
      if stage == GateStage::Hinted || !cfg.blind_gate_enabled { return true; }
      if !m.rms_px.is_finite() || m.rms_px > cfg.blind_rms_max_px_mult * m.adaptive_tol_px { return false; }
      if m.inliers < cfg.blind_inlier_floor { return false; }
      if m.expected_in_fov > 100 && m.inlier_ratio < cfg.blind_min_inlier_ratio { return false; }
      if !(cfg.blind_scale_sanity_min..=cfg.blind_scale_sanity_max).contains(&m.recovered_scale_arcsec) { return false; }
      if let Some(hs) = m.header_scale_arcsec {
          if hs > 0.0 {
              let r = m.recovered_scale_arcsec / hs;
              if r < 1.0 / cfg.blind_scale_header_tol || r > cfg.blind_scale_header_tol { return false; }
          }
      }
      true
  }
  ```
  `GateStage {Hinted, ScaleCleared, FullBlind}` and `BlindGateMetrics` are `pub(crate)`
  (`:479-506`); `header_pixel_scale_arcsec(conn, frame_id)` (`:616-630`, reads
  `frames.focallen/xpixsz`); `adaptive_tol_px` (`:635-640`).
- **Pure extraction**:
  ```rust
  /// The persist gate as a pure check: `Some(user-facing reason)` = refuse.
  pub(crate) fn acceptance_refusal(result: &SolveResult, expected_scale_arcsec: Option<f64>,
                                   config: &PlateSolveConfig) -> Option<String>;
  /// The plate_solves row a SolveResult becomes (pure).
  pub(crate) fn record_from_result(frame_id: i64, result: &SolveResult) -> PlateSolveRecord;
  ```
  `store_result` keeps `if config.blind_gate_enabled` around the
  `header_pixel_scale_arcsec` DB read (no new DB read when the gate is off) and keeps its
  `warn!` with `frame_id`; the project path calls `acceptance_refusal(&r,
  meta.pixelScaleArcsec, cfg)`. Constraining tests: `blind_gate_refuses_a_six_match_noise_alignment`
  (`:683`), `blind_gate_table` (`:960`), `store_result_refuses_a_solve_that_contradicts_the_headers_optics`
  (`:786`), `store_result_refuses_low_confidence_writeback` (`:825`),
  `store_result_persists_defocused_low_count_solve` (`:903`); ignored real-data
  `tests/blind_gate.rs`, `tests/blind_index_solve.rs`, `tests/fallback_blind_scale.rs`.
- `input_gate_reason` (`service.rs:589-608`) reads `frame_analysis` by id → not applied (spec).

### `PlateSolveRecord` (`plate_solve/storage.rs:10-41`)
`id: Option<i64>, frame_id: i64, crpix1, crpix2 (0-based), crval1, crval2, cd1_1, cd1_2,
cd2_1, cd2_2: f64, sip_order: Option<i32>, sip_a_coeffs/sip_b_coeffs/sip_ap_coeffs/sip_bp_coeffs:
Option<String> (JSON [[f64]]), matched_stars, total_detected: i32, rms_residual_px,
rms_residual_arcsec, pixel_scale_arcsec, field_rotation_deg: f64, solve_time_ms: i64,
catalog_used, algorithm_used, solved_at: String, expected_catalog_stars_in_fov: Option<i32>,
inlier_ratio: Option<f64>`. Derives `Serialize/Deserialize` → usable as `payload_json`.
`to_solution()` (`:63-88`) uses crpix/crval/cd/sip only and refuses half-stored SIP. Stacking
consumers use only those plus `pixel_scale_arcsec` (`fits_writer/wcs.rs:166`
`scale_plate_solve`; `wcs_cards` `wcs.rs:~183-215` adds +1 to crpix).

### Manifest `meta.wcs` → `PlateSolveRecord`
Written by `collab/frame_meta.rs:133-158`:
```rust
wcs.insert("crval1", s.crval1); wcs.insert("crval2", s.crval2);
wcs.insert("crpix1", s.crpix1 + 1.0); wcs.insert("crpix2", s.crpix2 + 1.0);   // 1-based
wcs.insert("cd", [s.cd1_1, s.cd1_2, s.cd2_1, s.cd2_2]);
if let Some(order) = s.sip_order { if let (Some(a), Some(b)) = (..) {
    sip = {order, a: from_str::<Value>(a_json), b: …, ap?: …, bp?: …} } }
meta.insert("wcs", wcs);
```
plus `meta.pixelScaleArcsec` (= the solve's scale when a solve exists, else header-derived,
`frame_meta.rs:103-106`), `naxis1/2`, `focalLen`, `instrume`, `telescope`, `xbinning`,
`bayerpat`. Locally cached in `project_frames_local.manifest_json` as the whole
`FrameViewWire` (`collab/hub_client.rs:151-197`; `meta: serde_json::Value`), parsed with
`api::collab_exchange::parse_manifest_wire(project_id, uuid, json, caller)`
(`collab_exchange.rs:729-747`). No reader of `meta.wcs` exists app-side yet.
Inverse (new, e.g. `frame_meta::solve_record_from_meta(meta) -> Option<PlateSolveRecord>`):
crpix −1.0; cd split; SIP tables `serde_json::to_string(&value)` (a `Vec<Vec<f64>>`
round-trips exactly through serde_json/ryu); `sip_order` from `sip.order`;
`pixel_scale_arcsec` = `meta.pixelScaleArcsec` (or `sqrt(|det CD|)·3600`);
`field_rotation_deg` from CD (document the convention; stacking does not read it);
`frame_id 0`; `catalog_used/algorithm_used = "manifest"`; counts 0. Pin with a round trip:
seed a solve → `build_frame_meta` → inverse → `to_solution()` equal to the original's.
Step 6 (grid proposal) will want the same inverse.

### Solver config hash and catalog identity (both new)
- `PlateSolveConfig` (`plate_solve/config.rs:17-116`, stored JSON `plate_solve.config`,
  `config::load_config(&conn)`). Solve-relevant: `sip_order`,
  `base_verification_tolerance_arcsec`, `blind_gate_enabled`, `blind_rms_max_px_mult`,
  `blind_min_inlier_ratio`, `blind_inlier_floor`, `blind_scale_sanity_min/max`,
  `blind_scale_header_tol`, `camera_defaults`. Not: `batch_concurrency`,
  `autofind_tolerance_deg`, `input_gate_*`. Plus a const recipe version for the hard-coded
  `SolveConfig` values. Hash with xxh3 of canonical JSON (the `stage_hash` style).
- Catalog identity: `<app-data>/catalogs/smac_gaia` → `discover_layers`
  (`plate_solve/layers.rs:18-42`) → per tier `(dir name, stars.smac size, mtime)` (cheap,
  `master_identity`-style, `plan.rs:~270-292`), or `StarCache::star_count()` +
  `catalog_epoch()` (`catalog/gaia_prebuilt.rs:~395`). App-data dir in core:
  `rc.ctx.db.get()` → `db.path().parent()` (`db/mod.rs:200`), as the hosts do.
- Solve artifact hash = `stage_hash(&json!({"solver": solve_cfg_hash, "catalog": catalog_id}),
  &[&format!("blake3:{b}")], &[])` — blake3 only (not the Prepare hash), so a debayer toggle
  never re-solves.

### Where the run uses a stored solve
- `type SolveCache = HashMap<i64, Option<PlateSolveRecord>>` (`run.rs:4011`), `solve_of`
  (`run.rs:4016-4043`) → `origin_of` + `stored_solve(conn, origin, frame_id)`; a read error is
  a `warn!` + no seed. Per-stage caches: `register_native_groups` (`run.rs:5411`),
  `stage_register` (`run.rs:5673`), `stage_output` (`run.rs:9751`, the reference's WCS for
  the master — the grid record replaces it for projects, step 11).
- `registration_gate_and_hint` (`run.rs:4070-4100`) builds the WCS seed
  (`register/wcs_seed.rs:~90-175`, `to_solution()` of both) when subject AND reference have
  a solve.
- `stored_solve` (`stacking/origin.rs:42-50`) is THE plug point. Project arm: manifest
  `meta.wcs` → `solve_record_from_meta` (pure, from the origin's parsed manifest); else the
  `solve` artifact (`find_artifact(conn, &StackSubject::Project(pid), "", "solve",
  Some(handle))`, payload → record); an artifact with a negative outcome → `None`. Needs
  `project_id` (§0.11). Use `group_key = ""` for the `solve` kind so a regroup (exposure
  mode change) never re-solves. The `stacking_artifacts` table accepts any kind/group key
  (`db/schema.rs:358-376`); `solve` must stay out of `INTERMEDIATE_ARTIFACT_KINDS` (like
  `metrics`, a payload row worth minutes).
- Target RA/Dec: `collab_projects.target_ra_deg / target_dec_deg REAL NOT NULL`
  (`db/schema.rs:2496-2499`; `db/collab.rs:74, 145`).

---

## 8. Measure and the Solve sub-step; Reference reads solve availability

- `stage_measure` (`run.rs:2871-…`) loops groups → frames: excluded/stage-1-failed entries,
  the `calibrated` artifact lookup (`:2953-2965`), `PlaneReader::open` probe (`:2983`), the
  metrics/fits freshness (`:3080-3140`), fan-out `measure_frame_with_fits` (`:3189`),
  weights, selection.
- **Solve sub-step**: a `solve_project_frames(rc)` at the top of `stage_measure`, Project
  subject only, after Prepare has written the `calibrated` rows: for each non-excluded
  frame without `meta.wcs` and without a fresh `solve` artifact → identity re-check → solve
  on the `calibrated` path → `acceptance_refusal` with `meta.pixelScaleArcsec` →
  `upsert_artifact(kind "solve", group_key "", path None, payload)`; never `plate_solves` or
  `frames`. Progress `Stage::Measure` with message "solving" (no new `Stage` variant — the
  board keeps ten rows; its time is part of Measure's `StageTiming`). Pass `Some(&rc.cancel)`.
  Run it inside `rc.ctx.image_pool.install` so the solver's rayon work lands on the run's
  pool; frame-sequential is enough at first (the hosts' batch uses `batch_concurrency`).
  Record per frame a `WcsSource {Manifest, Local, None}` on `RunContext`.
- Reference (§5.4, not explicitly numbered in §18 — natural home is the end of step 5):
  Auto = `best_included_idx(entries)` (`run.rs:3528-3547`) → `weights::best_by_weight`
  (`weights.rs:333-346`); add a WCS-candidate mask (manifest tier first, local tier only if
  no manifest candidate in the chosen scope). Manual = `stored_reference_frame` (funnel) —
  fail with the spec's message if it has no WCS. The two-pass re-pick (`weights::two_pass_pick`
  `weights.rs:591`, candidates built in `two_pass_refine` `run.rs:5097`) must filter by WCS
  too. No candidate → "no frame of this project could be plate-solved; the grid needs one".

---

## 9. Recommended task decomposition

Dependencies: steps 4/5's run integration needs step 3 (the `FrameOrigin::Project` variant,
the project frame source/plan, project test fixtures — today's `test_context`
`run.rs:~9840` is frame-set-only; `db::stacking::project_frame_handles`/`insert_test_project`
exist from 6b-1). Everything marked "pure" lands before step 3.

**Step 4 — Prepare**
- 4.1 (pure) `debayer_plane` + `debayered_cards` extracted from `execute_generation`;
  `debayer_file`. Tests: all generator tests + `calibrate_fan_out_matches_the_sequential_stage_byte_for_byte`
  unchanged; new pixel-bit pin `debayer_file(debayer-off output) == debayer-on output` incl.
  odd `XBAYROFF`.
- 4.2 (pure) file probe/classification: `prepare::classify(path) -> InPlace{planes} |
  Debayer | Convert | ConvertThenDebayer | Unsupported(reason)` from headers only (FITS via
  `probe_fits` + `FitsHeader`; XISF via `extract_xisf_header` location/compression check);
  `cfa_geometry_from_fields` extracted from `resolve_cfa_geometry` + `cfa_geometry_of_file`.
  Tests: synthetic FITS (mono, RGB, mosaic with offsets, 16-bit), XISF from `write_xisf_f32`,
  a distributed-XISF header, a tile-compressed header; origin catalog-arm test unchanged.
- 4.3 (pure) converter to float32 FITS with the domain rule (÷65535 XISF; PlaneReader ÷
  `scale_divisor_for_bitpix` integer FITS) + Prepare card builder (allowlist, mosaic cards
  for mosaic outputs, `ATH_STK`, provenance). Tests: values (1-ulp tolerance), cards never
  carry `ATH_PRJ`/`CALSTAT`/`ATH_CSRC`, scanner skips a Prepare output (mirror
  `scan_never_catalogs_a_stacking_artifact`).
- 4.4 (pure) `prepare_subtree` + Prepare hash + golden literal; `WorkingLayout::prepared_*`,
  `work_usage`/`cleanup_work` (+ ts type regen). Tests: `stage_hashes_are_pinned_to_golden_literals`
  and `config_hash_is_canonical_and_pinned` unchanged; paths tests (`paths.rs:~1640-1700`).
- 4.5 (after step 3) HashMemo Project arm + `stage_prepare` (dispatch from
  `stage_calibrate`, fan-out like Calibrate, `calibrated`/`calibrated_mosaic` rows, reuse,
  exclusions) + Project arm of `origin::cfa_geometry`. Tests on a project fixture: in place
  mono and RGB, OSC mosaic debayer on (+ Bayer drizzle mosaic row → landed file) / off, XISF
  converted, unsupported excluded, second run reuses, landed file untouched (mtime/bytes),
  no catalog read (the 6b seam test pattern).
- 4.6 (after 4.5) the identity funnel `origin::landed_unchanged` + calls at the five stage
  boundaries. Test: modify a landed file between stages → excluded "changed during the
  run", run completes.

**Step 5 — Solve**
- 5.1 (pure) `acceptance_refusal` + `record_from_result` extracted from `store_result`;
  behaviour-preserving (service tests above). New: the gate refuses a wrong-scale solve
  given an expected scale.
- 5.2 (pure) `solve_record_from_meta` (manifest WCS inverse) with the round-trip pin.
- 5.3 (pure) solver config hash, catalog identity, a core `open_catalog_layers(app_data)`
  (optionally the hosts adopt it later; no command change, so no two-host rule triggered).
- 5.4 (after step 3) project solve fn (transient `Frame`, hints from manifest + target,
  `solve_frame_with_hints`, gate) + Solve sub-step in `stage_measure` + `stored_solve`
  Project arm + `solve` artifact (positive and negative outcomes). Tests: never writes
  `plate_solves`/`frames` (row counts, plus a `frames` row whose id equals a handle stays
  untouched); second run does not solve again (inject the solver as a fn param/trait for the
  test — a real solve needs the catalog); refused → frame continues without WCS. Real-data
  check of solving a unit-domain calibrated replica (§0.10) before merge.
- 5.5 (after 5.4) the §5.4 WCS-candidate rule for Reference + two-pass.

**Existing tests that constrain these steps** (beyond those named above):
`light_cal::tests::preloaded_masters_calibrate_bit_identically` (`light_cal.rs:1071`),
`run.rs:10585 calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones`,
`run.rs:10717 calibrate_outputs_disambiguate_same_basename_within_a_group`,
`run.rs:17461 turning_bayer_on_regenerates_the_mosaic_of_an_already_calibrated_frame`,
`run.rs:20060 solve_of_reads_through_the_origin_and_treats_a_missing_origin_as_no_solve`,
`plan.rs:6566 frame_source_returns_one_catalog_origin_per_frame` (asserts the project
refusal — step 3 changes it), `run.rs:19880` and `plan.rs:6420/6595` (the
`PROJECT_STACKING_UNAVAILABLE` assertions, lifted as each guard goes:
`run.rs:760`, `run.rs:2004`, `groups.rs:611`, `plan.rs:1679`), the collab publish tests
that go through `execute_generation` (`api/collab.rs`, e.g. `:11780` ATH_PRJ assertion),
`frame_meta.rs:~290-351` (the `meta.wcs` shape), `scanner/mod.rs:3502`. Per the memory
rule, run the whole `cargo test -p athenaeum-core` (all targets) before any push; stacking
needs `render + solver`, which `cargo check --no-default-features` does not compile.
