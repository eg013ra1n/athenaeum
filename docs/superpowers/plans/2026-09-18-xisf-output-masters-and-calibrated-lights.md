# XISF Output for Calibration Masters and Calibrated Lights — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every file Athenaeum writes for the user — a built calibration master, a calibrated light on export or send, a stacking master — is written in the container the user chose, FITS or XISF, and an XISF master is accepted by PixInsight's WBPP as a master.

**Architecture:** One `OutputFormat` enum (`fits_writer`) shared by every writer; one dispatcher `write_image_f32` that branches on it; the XISF writer takes its `bounds` from the caller because a calibration master is raw ADU while a calibrated light and a stacking master are unit-scaled. The master format is a settings key read at build time; the calibrated-light format is a sixth field of `CalibratedLightOptions`, so it travels the same road the other five already do (export, send, summary). The stacking run's own intermediates stay FITS.

**Tech Stack:** Rust (`athenaeum-core`, Tauri commands, Axum routes), `fits_writer::{writer, xisf_writer}`, rustafits' XISF reader, React/TS.

**Spec:** `docs/backlog-v0.6.5.md` items 1 and 1b (the trace and the WBPP contract read from `BPP-Helper.js` / `BPP-StackEngine.js` on 2026-09-18). This plan carries the design decisions below; there is no separate spec.

## Design decisions (rulings)

- **R1 — one enum.** `stacking::config::OutputFormat` moves to `fits_writer::OutputFormat` and is re-exported from its old path, so every existing `use`, the `ts_export` registry line and the generated `OutputFormat = "fits" | "xisf"` TS type are unchanged.
- **R2 — bounds are the caller's.** `XisfBounds::Unit` (`"0:1"`) for anything in the `ATH_CSCL` domain (stacking masters, calibrated lights); `XisfBounds::Adu16` (`"0:65535"`) for a calibration master, whose samples are raw ADU. rustafits' reader normalizes by `bounds` and multiplies by 65535, so both round-trip exactly to the domain the light-calibration engine expects. Nothing ever rescales the samples themselves.
- **R3 — `imageType` is authoritative for WBPP.** `image_type_for` maps `Master Dark → MasterDark`, `Master Bias → MasterBias`, `Master Flat → MasterFlat`, **`Master Dark Flat → MasterDark`** (WBPP treats a dark flat as a dark matched by exposure, and its `IMAGETYP` parser turns `Master Dark Flat` into `Unknown` and then guesses FLAT from the file name), `Light Frame → Light`. The `IMAGETYP` keyword keeps Athenaeum's own value, so the scanner still classifies a dark flat as `MasterDarkFlat`.
- **R4 — the master format is a settings key**, `calibration.master_format` (`fits` default), read by `run_build` for a `New` target. A **rebuild keeps the container the master already has** (its catalog path's extension) — the catalog row is the contract, not the current setting. Read and written from the UI through the generic `get_setting` / `set_setting`; no new command.
- **R5 — the calibrated-light format is a field of `CalibratedLightOptions`** (`format`, `#[serde(default)]`), resolved by `CalibratedLightOptions::resolve` like the other five host arguments, chosen on the Export tab and the Send dialog (remembered in `lightCalPrefs`). `calibrated_output_filename` takes it, so export placement, send `rel_path`s and the run all name the file through the one rule.
- **R6 — the stacking run's intermediates stay FITS.** `PlaneReader` (and the Measure/Register readers behind it) are FITS-only; stage 1 sets `calibration_opts.format = Fits` explicitly, with the comment saying why, and the Stacking → Calibrate panel says "Intermediate calibrated frames are always FITS" instead of showing a radio. XISF intermediates are a follow-up of their own (a `PlaneReader` XISF arm), listed in the backlog, not in this plan. **Open for the owner:** if the working folder's `calibrated/` tree is meant to be handed to WBPP directly, say so and the follow-up moves up.
- **R7 — the CFA mosaic stays FITS.** It is a run-internal artifact (`keep_mosaic`, never on the wire).
- **R8 — acceptance is external.** The one check that proves the contract is opening an Athenaeum XISF master in PixInsight and running WBPP with it; it is an owner smoke recorded in `docs/superpowers/open-items.md`, not a unit test.

## Global Constraints

- Two backends in sync: any command whose arguments change (`export_to_wbpp`, `get_export_summary`, the frame-set send commands) changes on Tauri and Axum in the same task.
- Serde `camelCase` on the wire; `src/types/*.ts` regenerated through `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` when a `ts_rs` struct changes.
- Never swallow errors; `tracing` fields in snake_case, message a short stable phrase.
- Gates after every Rust task: `cargo check -p athenaeum-core --all-targets` (the headless check does not compile `integration/`), `cargo check --workspace`, the named tests; after every frontend task: `npx tsc --noEmit`.
- Never name the external tool in code or comments ("the external tool", "the external reference").
- No `println!`; `rustfmt <files>` on touched Rust files, never `cargo fmt -p`.

---

## File map

| File | Responsibility in this plan |
| ---- | ---- |
| `crates/athenaeum-core/src/fits_writer/mod.rs` | `OutputFormat`, `XisfBounds`, `write_image_f32` (new) |
| `crates/athenaeum-core/src/fits_writer/xisf_writer.rs` | `write_xisf_f32_with` (bounds param), `image_type_for` arms |
| `crates/athenaeum-core/src/stacking/config.rs` | re-export of `OutputFormat` |
| `crates/athenaeum-core/src/stacking/master_cards.rs` | `write_output` → `write_image_f32(…, XisfBounds::Unit)` |
| `crates/athenaeum-core/src/settings/mod.rs` | key `calibration.master_format`, `get_master_format` |
| `crates/athenaeum-core/src/calibration_library/paths.rs` | `MasterPathParams.format`, extension |
| `crates/athenaeum-core/src/api/masters.rs` | format at the write site, rebuild keeps its container |
| `crates/athenaeum-core/src/calibration_library/register.rs` | parse dispatch on extension; parity test for XISF |
| `crates/athenaeum-core/src/export/models.rs` | `CalibratedLightOptions.format`, `resolve`, `calibrated_output_filename` |
| `crates/athenaeum-core/src/calibration_library/light_cal.rs` | `write_calibrated_output(…, format)` |
| `crates/athenaeum-core/src/export/calibrated_generator.rs`, `data_collector.rs`, `file_organizer.rs`, `stacking/run.rs`, `api/lights.rs` | naming call sites |
| `crates/athenaeum-tauri/src/commands/export.rs`, `crates/athenaeum-web/src/routes/export.rs` | the sixth argument |
| `src/hooks/useExportProgress.ts`, `src/hooks/useExportData.ts`, `src/components/export/{ExportTab.tsx,lightCalPrefs.ts}`, `src/components/transfers/SendToNodeDialog.tsx`, `src/components/dualpane/DualPaneFileBrowser.tsx` | the format on the Export tab and the Send dialog |
| `src/pages/Settings.tsx` | "Master file format" select on the Calibration tab |
| `src/components/stacking/panels/CalibratePanel.tsx` | the "intermediates are FITS" line |
| `docs/export/README.md`, `CLAUDE.md`, `docs/superpowers/open-items.md` | docs and the owed smoke |

---

### Task 1: One `OutputFormat`, caller-chosen `bounds`, the master `imageType` arms

**Files:**
- Modify: `crates/athenaeum-core/src/fits_writer/mod.rs`
- Modify: `crates/athenaeum-core/src/fits_writer/xisf_writer.rs:225-245` (`FLOAT_BOUNDS`, `image_type_for`), `:246-260` (`build_xml`), `:352-410` (`write_xisf_f32`, `write_xisf_f32_to`)
- Modify: `crates/athenaeum-core/src/stacking/config.rs:317-331` (the enum → re-export)
- Modify: `crates/athenaeum-core/src/stacking/master_cards.rs:427-472` (`output_extension`, `write_output`)
- Test: `crates/athenaeum-core/src/fits_writer/xisf_writer.rs` (`mod tests`)

**Interfaces:**
- Produces:
  - `fits_writer::OutputFormat { Fits, Xisf }` with `fn extension(self) -> &'static str` and `fn from_path(path: &Path) -> OutputFormat` (`.xisf` → `Xisf`, anything else → `Fits`).
  - `fits_writer::XisfBounds { Unit, Adu16 }` with `fn attr(self) -> &'static str` (`"0:1"` / `"0:65535"`).
  - `xisf_writer::write_xisf_f32_with(path, width, height, channels, data, cards, bounds: XisfBounds) -> Result<(), FitsWriteError>`; the existing `write_xisf_f32(…)` becomes `write_xisf_f32_with(…, XisfBounds::Unit)` so every current caller is unchanged.
  - `fits_writer::write_image_f32(path, width, height, channels, data, cards, format: OutputFormat, bounds: XisfBounds) -> Result<(), FitsWriteError>`.

- [x] **Step 1: Write the failing tests** (append to `xisf_writer.rs`'s `mod tests`, next to `image_type_follows_imagetyp_where_the_format_names_it`):

```rust
#[test]
fn image_type_names_every_master_kind_wbpp_reads() {
    // WBPP takes the <Image imageType> attribute as authoritative (it
    // overrides its own IMAGETYP guess), and treats a dark flat as a dark.
    let cases = [
        ("Master Dark", Some("MasterDark")),
        ("Master Bias", Some("MasterBias")),
        ("Master Flat", Some("MasterFlat")),
        ("Master Dark Flat", Some("MasterDark")),
        ("Light Frame", Some("Light")),
        ("Master Light", Some("MasterLight")),
        ("Drizzle Weight", Some("WeightMap")),
        ("Dark Frame", None),
    ];
    for (imagetyp, want) in cases {
        let cards = vec![Card::new("IMAGETYP", CardValue::Str(imagetyp.into())).unwrap()];
        assert_eq!(image_type_for(&cards), want, "IMAGETYP {imagetyp:?}");
    }
}

#[test]
fn adu_bounds_round_trip_through_the_reader_unscaled() {
    // A calibration master is raw ADU. Written with bounds 0:65535, the
    // reader's `v/65535 * 65535` hands the same ADU back.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("adu.xisf");
    let data: Vec<f32> = vec![0.0, 1234.5, 40000.0, 65535.0];
    write_xisf_f32_with(&path, 4, 1, 1, &data, &sample_cards(), XisfBounds::Adu16).unwrap();
    let (_, header) = split_header(&std::fs::read(&path).unwrap());
    assert!(header.contains("bounds=\"0:65535\""), "header: {header}");
    let img = astroimage::ImageConverter::read_raw(&path).unwrap();
    for (got, want) in img.data.iter().zip(&data) {
        assert!((got - want).abs() < 1e-2, "got {got}, want {want}");
    }
}
```

(`read_raw`'s exact return shape: copy the call the existing `round_trips_through_the_reader` test makes at `xisf_writer.rs:617-640` and drop its `/ 65535.0`.)

- [x] **Step 2: Run them to see them fail**

Run: `cargo test -p athenaeum-core --lib fits_writer::xisf_writer::tests -- image_type_names adu_bounds`
Expected: compile error — `write_xisf_f32_with` and `XisfBounds` do not exist.

- [x] **Step 3: Add the shared types to `fits_writer/mod.rs`**

```rust
/// The container an output image is written in. One enum for every writer
/// in the app — stacking masters, calibration masters, calibrated lights —
/// so no two features can disagree about what "xisf" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    #[default]
    Fits,
    /// Monolithic XISF 1.0, one uncompressed Float32 image, the same cards
    /// as the FITS file (`xisf_writer`).
    Xisf,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Fits => "fits",
            OutputFormat::Xisf => "xisf",
        }
    }

    /// The container a file on disk already has — a rebuild keeps it.
    pub fn from_path(path: &std::path::Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("xisf") => OutputFormat::Xisf,
            _ => OutputFormat::Fits,
        }
    }
}

/// The representable range an XISF Float32 image declares (XISF 1.0
/// §11.5.1, mandatory). Which one applies is the CALLER's knowledge: a
/// stacking master and a calibrated light are unit-scaled (`ATH_CSCL`), a
/// calibration master is raw ADU. rustafits' reader normalizes by these
/// bounds and multiplies by 65535, so both come back in the ADU domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XisfBounds {
    Unit,
    Adu16,
}

impl XisfBounds {
    pub fn attr(self) -> &'static str {
        match self {
            XisfBounds::Unit => "0:1",
            XisfBounds::Adu16 => "0:65535",
        }
    }
}

/// One image in the chosen container — the ONE place the two writers are
/// chosen between. `bounds` only matters for XISF.
pub fn write_image_f32(
    path: &std::path::Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[card::Card],
    format: OutputFormat,
    bounds: XisfBounds,
) -> Result<(), card::FitsWriteError> {
    match format {
        OutputFormat::Fits => writer::write_fits_f32(path, width, height, channels, data, cards),
        OutputFormat::Xisf => {
            xisf_writer::write_xisf_f32_with(path, width, height, channels, data, cards, bounds)
        }
    }
}
```

Add `pub use` lines for `OutputFormat`, `XisfBounds`, `write_image_f32` beside the existing re-exports at `fits_writer/mod.rs:14`.

- [x] **Step 4: Thread `bounds` through the XISF writer**

In `xisf_writer.rs`: delete `const FLOAT_BOUNDS`; give `build_xml` a `bounds: XisfBounds` parameter and write `bounds=\"{}\"` from `bounds.attr()`; give `write_xisf_f32_to` a trailing `bounds: XisfBounds` and pass it on; rename the body of `write_xisf_f32` to `write_xisf_f32_with(…, bounds)` and keep:

```rust
/// The stacking convention: unit-scaled samples (`ATH_CSCL`), `bounds="0:1"`.
pub fn write_xisf_f32(path: &Path, width: usize, height: usize, channels: usize,
                      data: &[f32], cards: &[Card]) -> Result<(), FitsWriteError> {
    write_xisf_f32_with(path, width, height, channels, data, cards, XisfBounds::Unit)
}
```

Update the module doc comment paragraph that explains `bounds` (`xisf_writer.rs:35-50`) to say the range is the caller's and name the two callers' domains.

- [x] **Step 5: Grow `image_type_for`**

```rust
fn image_type_for(cards: &[Card]) -> Option<&'static str> {
    let imagetyp = cards.iter().find(|c| c.keyword == "IMAGETYP")?;
    let Some(CardValue::Str(s)) = &imagetyp.value else { return None; };
    match s.trim() {
        "Master Light" => Some("MasterLight"),
        "Drizzle Weight" => Some("WeightMap"),
        "Master Dark" => Some("MasterDark"),
        "Master Bias" => Some("MasterBias"),
        "Master Flat" => Some("MasterFlat"),
        // The consumer's own vocabulary has no dark-flat master; it matches a
        // dark flat as a DARK by exposure, and its IMAGETYP parser reads
        // "Master Dark Flat" as unknown and then guesses FLAT from the file
        // name. The attribute overrides both — the keyword keeps our value.
        "Master Dark Flat" => Some("MasterDark"),
        "Light Frame" => Some("Light"),
        _ => None,
    }
}
```

- [x] **Step 6: Re-export from stacking and use the dispatcher there**

`stacking/config.rs`: replace the `OutputFormat` enum definition with `pub use crate::fits_writer::OutputFormat;` (keep the doc comment about the rejection maps on `OutputConfig.format`). `stacking/master_cards.rs`: `output_extension(format)` becomes `format.extension()` (keep the function as a one-line wrapper if it is `pub` and used elsewhere — `grep -rn output_extension crates/`), and `write_output` becomes:

```rust
fn write_output(path: &Path, width: usize, height: usize, channels: usize,
                data: &[f32], cards: &[Card], format: OutputFormat) -> Result<(), FitsWriteError> {
    crate::fits_writer::write_image_f32(path, width, height, channels, data, cards, format,
                                        crate::fits_writer::XisfBounds::Unit)
}
```

`ts_export.rs:332`: keep `crate::stacking::config::OutputFormat` (it resolves through the re-export); confirm the generated `src/types/stacking.ts` `OutputFormat` line is byte-identical after `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.

- [x] **Step 7: Run the gates**

Run: `cargo test -p athenaeum-core --lib fits_writer:: stacking::master_cards`
Expected: all pass, the two new tests included; `image_type_follows_imagetyp_where_the_format_names_it` still passes (a `Dark Frame` still gets no attribute).
Run: `cargo check -p athenaeum-core --all-targets && cargo check --workspace`
Expected: clean.

- [x] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/fits_writer crates/athenaeum-core/src/stacking/config.rs crates/athenaeum-core/src/stacking/master_cards.rs src/types/stacking.ts
git commit -m "fits_writer: one OutputFormat, caller-chosen XISF bounds, the master imageType arms"
```

---

### Task 2: Built masters take the format from `calibration.master_format`; a rebuild keeps its container

**Files:**
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (`defaults`, `keys`, `SettingsManager`)
- Modify: `crates/athenaeum-core/src/calibration_library/paths.rs:14-90`
- Modify: `crates/athenaeum-core/src/api/masters.rs:1377-1440` (the write site)
- Test: `paths.rs` `mod tests`, `settings/mod.rs` tests, `api/masters.rs` tests

**Interfaces:**
- Consumes: `fits_writer::{OutputFormat, XisfBounds, write_image_f32}` (Task 1).
- Produces: `keys::CALIBRATION_MASTER_FORMAT = "calibration.master_format"`, `defaults::CALIBRATION_MASTER_FORMAT = "fits"`, `SettingsManager::get_master_format(&self, conn) -> Result<OutputFormat>`; `MasterPathParams.format: OutputFormat`.

- [x] **Step 1: Failing tests**

`paths.rs` tests (next to the existing `master_relative_path` tests — find them with `grep -n "fn .*relative_path" crates/athenaeum-core/src/calibration_library/paths.rs`):

```rust
#[test]
fn master_relative_path_takes_the_chosen_container() {
    let base = MasterPathParams {
        instrume: Some("ASI2600MM"), master_kind: FrameKind::MasterDark, filter: None,
        exptime: Some(300.0), ccd_temp: Some(-10.0), gain: Some(100.0), binning: Some("1x1"),
        date: "2026-09-18", format: OutputFormat::Fits,
    };
    assert!(master_relative_path(&base).to_string_lossy().ends_with("_2026-09-18.fits"));
    let xisf = MasterPathParams { format: OutputFormat::Xisf, ..base };
    assert!(master_relative_path(&xisf).to_string_lossy().ends_with("_2026-09-18.xisf"));
}
```

`settings/mod.rs` tests (next to the `get_integration_band_budget_mb` test):

```rust
#[test]
fn master_format_defaults_to_fits_and_tolerates_garbage() {
    let (conn, mgr) = test_manager();   // whatever helper the neighbouring tests use
    assert_eq!(mgr.get_master_format(&conn).unwrap(), OutputFormat::Fits);
    mgr.persist_setting(&conn, keys::CALIBRATION_MASTER_FORMAT, "xisf").unwrap();
    assert_eq!(mgr.get_master_format(&conn).unwrap(), OutputFormat::Xisf);
    mgr.persist_setting(&conn, keys::CALIBRATION_MASTER_FORMAT, "tiff").unwrap();
    assert_eq!(mgr.get_master_format(&conn).unwrap(), OutputFormat::Fits, "unknown value falls back, with a warn!");
}
```

- [x] **Step 2: Run them to see them fail**

Run: `cargo test -p athenaeum-core --lib calibration_library::paths settings::`
Expected: compile errors (`format` field, `get_master_format` missing).

- [x] **Step 3: The settings key and accessor**

`settings/mod.rs` — in `defaults`: `pub const CALIBRATION_MASTER_FORMAT: &str = "fits";` beside `INTEGRATION_BAND_BUDGET_MB`; in `keys`, right under `CALIBRATION_LIBRARY_DIR`:

```rust
    /// The container a built calibration master is written in: `fits`
    /// (default) or `xisf`. Read once per build for a NEW target; a rebuild
    /// keeps the container its file already has.
    pub const CALIBRATION_MASTER_FORMAT: &str = "calibration.master_format";
```

On `SettingsManager`, beside `get_integration_band_budget_mb`:

```rust
    pub fn get_master_format(&self, conn: &Connection) -> Result<crate::fits_writer::OutputFormat> {
        use crate::fits_writer::OutputFormat;
        let value = self.get_with_precedence(conn, keys::CALIBRATION_MASTER_FORMAT,
                                             defaults::CALIBRATION_MASTER_FORMAT)?;
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "fits" => OutputFormat::Fits,
            "xisf" => OutputFormat::Xisf,
            _ => {
                tracing::warn!(key = keys::CALIBRATION_MASTER_FORMAT, value = %value,
                               "unknown master format — falling back to fits");
                OutputFormat::Fits
            }
        })
    }
```

- [x] **Step 4: The path builder**

`paths.rs`: add `pub format: OutputFormat` to `MasterPathParams` and end `master_relative_path` with

```rust
    PathBuf::from(camera)
        .join(kind_folder(p.master_kind))
        .join(format!("{}.{}", parts.join("_"), p.format.extension()))
```

Update the layout doc comment at `paths.rs:8-10` (`…_<date>.fits` → `…_<date>.<fits|xisf>`). Every constructor of `MasterPathParams` (`grep -rn "MasterPathParams {" crates/`) gets a `format:`; tests use `OutputFormat::Fits`.

- [x] **Step 5: The write site**

`api/masters.rs`, in `run_build`: resolve the format before the `match &target`:

```rust
    let format = match &target {
        BuildTarget::New => ctx_settings_master_format(&conn)?,   // ctx.settings.get_master_format(&conn)
        // The catalog row's path is the contract: a FITS master rebuilds as
        // FITS whatever the setting says today, and vice versa.
        BuildTarget::Rebuild { target_path, .. } => OutputFormat::from_path(target_path),
    };
```

(`run_build` already has `state`/`ctx` in scope for the settings — use the same handle `library_dir_or_err` gets its connection from.) Pass `format` into `MasterPathParams`, and replace the `write_fits_f32(&target_abs, …)` call with

```rust
    write_image_f32(&target_abs, out.width, out.height, 1, &out.data, &cards, format, XisfBounds::Adu16)?;
```

Adjust the two comments around it that name `write_fits_f32` ("atomic rename inside `write_fits_f32`" → "inside the writer"). Add `format = ?format` to the `debug!`/`info!` event that already reports the target path of the build (find it with `grep -n "master written\|target_abs" crates/athenaeum-core/src/api/masters.rs`).

- [x] **Step 6: A rebuild test**

In `api/masters.rs` tests there is a rebuild test that writes a master then calls `rebuild_master` (find it: `grep -n "fn rebuild" crates/athenaeum-core/src/api/masters.rs`). Add a sibling that sets `calibration.master_format = xisf` in the settings AFTER the FITS master exists, rebuilds, and asserts the file at the catalog path is still FITS (`std::fs::read(&path)[..6] == b"SIMPLE"`) and that no `.xisf` sibling appeared.

- [x] **Step 7: Gates and commit**

Run: `cargo test -p athenaeum-core --lib calibration_library::paths settings:: api::masters`
Expected: pass.
Run: `cargo check -p athenaeum-core --all-targets && cargo check --workspace`

```bash
git add crates/athenaeum-core/src/settings/mod.rs crates/athenaeum-core/src/calibration_library/paths.rs crates/athenaeum-core/src/api/masters.rs
git commit -m "masters: the built master's container follows calibration.master_format; a rebuild keeps its own"
```

---

### Task 3: Registration and light calibration accept an XISF master

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/register.rs:100-140` and its `mod tests` (`write_master` helper `:317-326`, `direct_registration_matches_scanner_ingestion` `:428-478`)
- Test: `crates/athenaeum-core/src/calibration_library/light_cal.rs` `mod tests` (or `cosmetic.rs` tests — wherever a master-dark fixture is already built through `BandSource`)

**Interfaces:**
- Consumes: `fits_parser::{parse_fits_with_header, parse_xisf, extract_xisf_header}`; `fits_writer::write_xisf_f32_with(…, XisfBounds::Adu16)`.

- [x] **Step 1: Failing parity test for XISF**

In `register.rs` tests, make `write_master` take the format:

```rust
    fn write_master_as(dir: &std::path::Path, format: OutputFormat) -> std::path::PathBuf {
        let p = dir.join(format!("master_dark.{}", format.extension()));
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .instrume("TestCam").exptime(300.0).gain(100).offset(50)
            .binning(1, 1).ccd_temp(-10.0)
            .bayer(Bayer::Rggb, 1, 0).roworder("BOTTOM-UP")
            .build().unwrap();
        crate::fits_writer::write_image_f32(&p, 8, 8, 1, &vec![100.0; 64], &cards, format,
                                            crate::fits_writer::XisfBounds::Adu16).unwrap();
        p
    }
    fn write_master(dir: &std::path::Path) -> std::path::PathBuf { write_master_as(dir, OutputFormat::Fits) }
```

Turn `direct_registration_matches_scanner_ingestion` into a helper `parity_for(format: OutputFormat)` called by two `#[test]`s (`…_fits`, `…_xisf`); the scanner copy in path B keeps the same extension (`scan_dir.path().join(format!("master_dark.{}", format.extension()))`). Add to the compared columns the `files.format` value (`SELECT format FROM files WHERE id = …`) and assert it is `"XISF"` on both sides for the XISF case.

- [x] **Step 2: Run it to see it fail**

Run: `cargo test -p athenaeum-core --lib calibration_library::register::tests::direct_registration_matches_scanner_ingestion_xisf`
Expected: FAIL at `register_master` — "freshly written master failed to parse" (the FITS parser refuses the XISF signature).

- [x] **Step 3: Dispatch the re-parse on the extension**

`register.rs:107-108` becomes:

```rust
    // The same parser pair the scanner uses for each container
    // (scanner/mod.rs, the XISF branch), so a directly registered master and
    // a scanned one go through identical parsing — the invariant
    // `direct_registration_matches_scanner_ingestion_*` pins.
    let is_xisf = master_path.extension()
        .map(|e| e.to_string_lossy().eq_ignore_ascii_case("xisf"))
        .unwrap_or(false);
    let (mut frame, header_text) = if is_xisf {
        let frame = parse_xisf(master_path, 0)
            .with_context(|| format!("freshly written master failed to parse: {}", master_path.display()))?;
        let header = extract_xisf_header(master_path)
            .with_context(|| format!("freshly written master has no readable header: {}", master_path.display()))?;
        (frame, header)
    } else {
        parse_fits_with_header(master_path, 0)
            .with_context(|| format!("freshly written master failed to parse: {}", master_path.display()))?
    };
```

The `format` computed at `:133-136` is now redundant with `is_xisf` — derive it from the same boolean. The `unwrap_or("master.fits")` filename fallback at `:129` stays (it is unreachable for a real path).

- [x] **Step 4: Pin that light calibration reads an XISF master as ADU**

Find the existing test that builds a master dark fixture and runs `calibrate_light` or `hot_pixel_map_from_dark` on it (`grep -n "master_dark" crates/athenaeum-core/src/calibration_library/light_cal.rs crates/athenaeum-core/src/calibration_library/cosmetic.rs | head`). Add a sibling that writes the SAME dark as XISF through `write_image_f32(…, Xisf, XisfBounds::Adu16)` and asserts the calibrated output (or the hot-pixel map's median) is identical to the FITS run within `1e-3` — this is the test that would have caught a 65535× inflation.

- [x] **Step 5: Gates and commit**

Run: `cargo test -p athenaeum-core --lib calibration_library::`
Expected: pass, both parity tests included.
Run: `cargo check -p athenaeum-core --all-targets && cargo check --workspace`

```bash
git add crates/athenaeum-core/src/calibration_library/register.rs crates/athenaeum-core/src/calibration_library/light_cal.rs crates/athenaeum-core/src/calibration_library/cosmetic.rs
git commit -m "masters: an XISF master registers through the scanner's own XISF parser and calibrates as ADU"
```

---

### Task 4: Settings → Calibration: "Master file format"

**Files:**
- Modify: `src/pages/Settings.tsx:695-754` (the "Master Build Memory" block; add a sibling block), the load effect `:118-131`
- Modify: `docs/calibration-reference.md` (the library layout section), `CLAUDE.md` (the "Master Calibration Library" paragraph's fixed-layout sentence)

**Interfaces:**
- Consumes: generic `get_setting` / `set_setting` with key `calibration.master_format`.

- [x] **Step 1: State and load**

Beside `budgetInfo` state: `const [masterFormat, setMasterFormat] = useState<'fits' | 'xisf'>('fits');`. In the mount effect that calls `loadIntegrationBudget()`:

```ts
    api.invoke<string>('get_setting', { key: 'calibration.master_format', defaultValue: 'fits' })
      .then((v) => setMasterFormat(v === 'xisf' ? 'xisf' : 'fits'))
      .catch((err) => console.error('[Settings] get_setting calibration.master_format failed:', err));
```

- [x] **Step 2: The control** — a new `border-t` block after "Master Build Memory", saved on change (no Save button, same as the Archive compression select at `Settings.tsx:1542-1550`):

```tsx
          <div className="mt-6 pt-6 border-t border-border">
            <h3 className="text-xl font-semibold mb-4">Master File Format</h3>
            <label className="block text-sm font-medium text-content-secondary mb-2">Container for masters built here</label>
            <select
              value={masterFormat}
              onChange={async (e) => {
                const next = e.target.value === 'xisf' ? 'xisf' : 'fits';
                setMasterFormat(next);
                try {
                  await api.invoke('set_setting', { key: 'calibration.master_format', value: next });
                } catch (err) {
                  console.error('[Settings] set_setting calibration.master_format failed:', err);
                  setError(`Failed to save master format: ${err}`);
                }
              }}
              className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
            >
              <option value="fits">FITS — float32, the format every tool reads</option>
              <option value="xisf">XISF — what WBPP requires for master calibration files</option>
            </select>
            <p className="text-xs text-content-muted mt-2">
              Applies to masters built from now on. A rebuild keeps the container the master already has.
              XISF masters carry the same header cards, plus the image type WBPP reads.
            </p>
          </div>
```

- [x] **Step 3: Docs** — `docs/calibration-reference.md`: in the library layout section, the filename template gains `.<fits|xisf>` and one sentence on the setting. `CLAUDE.md` "Master Calibration Library": change `…_<date>.fits` to `…_<date>.<fits|xisf>` and add "container from `calibration.master_format` (Settings → Calibration), XISF written with `bounds="0:65535"` and the WBPP-readable `imageType` attribute; a rebuild keeps the file's own container".

- [x] **Step 4: Gate and commit**

Run: `npx tsc --noEmit`
```bash
git add src/pages/Settings.tsx docs/calibration-reference.md CLAUDE.md
git commit -m "settings: pick the container built masters are written in"
```

---

### Task 5: Calibrated lights carry a format (core + both hosts)

**Files:**
- Modify: `crates/athenaeum-core/src/export/models.rs:320-420` (`CalibratedLightOptions`, `resolve`, `calibrated_output_filename`)
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs:270-292` (`write_calibrated_output`)
- Modify: `crates/athenaeum-core/src/export/calibrated_generator.rs:133-140` (`output_filename`), `:655-675` (the primary write); `export/data_collector.rs:622`; `export/file_organizer.rs:729-732`; `api/lights.rs:2376-2379`; `stacking/run.rs:1565-1590`
- Modify: `crates/athenaeum-tauri/src/commands/export.rs:267-268`, `:461-462`; `crates/athenaeum-web/src/routes/export.rs:259`, `:389` and their args structs
- Modify: `docs/export/README.md:266-268`
- Test: `export/models.rs` tests, `export/calibrated_generator.rs` tests (`:1141`, `:1203`, `:1252`), `export/data_collector.rs:3095-3098`

**Interfaces:**
- Consumes: `fits_writer::{OutputFormat, XisfBounds, write_image_f32}`.
- Produces: `CalibratedLightOptions.format: OutputFormat` (`#[serde(default)]`), `CalibratedLightOptions::resolve(flat_norm, flat_norm_mode, params, hot_pixel, debayer, format: Option<OutputFormat>)`, `calibrated_output_filename(source_filename, debayer, format: OutputFormat)`, `write_calibrated_output(path, w, h, c, data, cards, format)`.

- [x] **Step 1: Failing tests**

`export/models.rs` tests:

```rust
#[test]
fn calibrated_output_filename_takes_the_container() {
    assert_eq!(calibrated_output_filename("L_1.fits", false, OutputFormat::Fits), "c_L_1.fits");
    assert_eq!(calibrated_output_filename("L_1.fits", true, OutputFormat::Xisf), "c_L_1_d.xisf");
    assert_eq!(calibrated_output_filename("L_1.xisf", false, OutputFormat::Fits), "c_L_1.fits",
               "an XISF source still yields the chosen container, not its own");
}

#[test]
fn options_format_defaults_to_fits_and_decodes_old_documents() {
    let old: CalibratedLightOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(old.format, OutputFormat::Fits);
    let r = CalibratedLightOptions::resolve(None, None, None, None, None, Some(OutputFormat::Xisf));
    assert_eq!(r.format, OutputFormat::Xisf);
}
```

`calibrated_generator.rs`: duplicate the mono generation test at `:1141-1160` as `…_writes_xisf_when_asked` with `opts.format = OutputFormat::Xisf`, asserting the output name ends in `.xisf`, the file starts with `b"XISF0100"`, and its header contains `bounds="0:1"` (calibrated lights are `ATH_CSCL`-scaled).

- [x] **Step 2: Run them to see them fail**

Run: `cargo test -p athenaeum-core --lib export::models export::calibrated_generator`
Expected: compile errors on the new parameter and field.

- [x] **Step 3: The model**

In `CalibratedLightOptions` after `debayer_osc`:

```rust
    /// The container a calibrated light is written in on export and send.
    /// The stacking run ignores it for its own intermediates (they stay
    /// FITS — `PlaneReader` reads nothing else), and says so in stage 1.
    #[serde(default)]
    pub format: crate::fits_writer::OutputFormat,
```

`Default` sets `format: OutputFormat::Fits`; `resolve` gains `format: Option<OutputFormat>` as its LAST parameter and `format: format.unwrap_or(d.format)`; `calibrated_output_filename(source_filename, debayer, format)` ends with `format!("c_{stem}_d.{}", format.extension())` / `format!("c_{stem}.{}", …)` and its doc comment's "always forced to `.fits`" sentence becomes "the extension is the chosen container's, never the source's". `GenerationSpec` gets a `format: OutputFormat` field set from the options in `resolve_generation`, and `output_filename` passes `self.format`.

- [x] **Step 4: The writer**

`light_cal.rs::write_calibrated_output` gains `format: OutputFormat` and calls `write_image_f32(path, width, height, channels, data, cards, format, XisfBounds::Unit)`; `calibrated_generator.rs:664` passes `spec.format`. The mosaic write at `:603-610` stays `write_fits_f32` (R7) — add "(R7: the mosaic is a run-internal artifact, always FITS)" to its comment.

- [x] **Step 5: Every naming call site**

- `data_collector.rs:622`: `calibrated_output_filename(&frame.filename, debayer, opts.format)`.
- `file_organizer.rs:729-732` (`remove_stale_sibling` or whatever the enclosing fn is called): it takes `debayer: bool` — give it `format: OutputFormat` too and pass it at its call site (`grep -n "calibrated_output_filename(source_name" crates/athenaeum-core/src/export/file_organizer.rs` finds both lines; the caller has the options in scope as `gen_opts`).
- `api/lights.rs:2376-2379` (a test): add `OutputFormat::Fits`.
- `stacking/run.rs:1565-1590`: before `execute_generation`, force the run's own container:

```rust
    let mut calibration_opts = cfg.calibration.clone();
    calibration_opts.keep_mosaic = want_mosaic;
    // R6: the run's calibrated frames are read back by `PlaneReader`
    // (Measure, Register, Integrate), which is FITS-only — the user-facing
    // format choice applies to export and send, never to the working tree.
    calibration_opts.format = crate::fits_writer::OutputFormat::Fits;
```

and the two `format!("{stem}.fits")` names stay (`spec.output_filename` now yields `.fits` because the spec was resolved from these forced options — verify `resolve_generation` is called AFTER the override; if the spec is resolved earlier in `calibrate_frame`, move the override to the options it is resolved from).

- [x] **Step 6: Both hosts**

Tauri `commands/export.rs`: the two commands that call `resolve` (`:267`, `:461`) gain `format: Option<athenaeum_core::fits_writer::OutputFormat>` as an argument and pass it sixth. Axum `routes/export.rs` (`:259`, `:389`): add `#[serde(default)] pub format: Option<OutputFormat>` to the two args structs and pass it. Then the frame-set send: `grep -rn "hot_pixel" crates/athenaeum-tauri/src/commands crates/athenaeum-web/src/routes` — every command that threads `hot_pixel`/`debayer` into `CalibratedLightOptions` (the frame-set send and the frame-selection send in `sync.rs`/`transfers.rs`) gets `format` the same way. `data_collector.rs:3095-3098` tests: add the sixth `None`/`Some(…)`.

- [x] **Step 7: Docs, TS, gates, commit**

`docs/export/README.md:266-268`: replace "An XISF source always yields a `.fits` output" with "The output container is the export's own choice (FITS default, XISF optional); an XISF source does not decide it." Regenerate TS: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` (adds `format: OutputFormat` to `CalibratedLightOptions` in `src/types/stacking.ts`).

Run: `cargo test -p athenaeum-core --lib export:: calibration_library::light_cal stacking::run`
Run: `cargo check -p athenaeum-core --all-targets && cargo check --workspace && npx tsc --noEmit`

```bash
git add crates/athenaeum-core/src/export crates/athenaeum-core/src/calibration_library/light_cal.rs crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/api crates/athenaeum-tauri/src/commands crates/athenaeum-web/src/routes docs/export/README.md src/types/stacking.ts
git commit -m "export: calibrated lights are written in the chosen container, on export and on send"
```

---

### Task 6: The format on the Export tab and the Send dialog; the Calibrate panel's line

**Files:**
- Modify: `src/hooks/useExportProgress.ts:12-20` (`ExportLightCalPrefs`), `:114-140` (`startExport` invoke args)
- Modify: `src/hooks/useExportData.ts:25-60` (`useExportSummary` invoke args)
- Modify: `src/components/export/lightCalPrefs.ts` (new key + reader), `src/components/export/ExportTab.tsx` (state, radio in the calibrated-lights section `:588`, the two `{ flatNorm, … }` literals `:252`, `:944`)
- Modify: `src/components/transfers/SendToNodeDialog.tsx:40-46` and wherever it builds its invoke args; `src/components/dualpane/DualPaneFileBrowser.tsx` (its `lightCalOptions` literal)
- Modify: `src/components/stacking/panels/CalibratePanel.tsx` (one line)

**Interfaces:**
- Consumes: wire field `format: 'fits' | 'xisf'` on `export_to_wbpp`, `get_export_summary` and the send commands (Task 5).

- [x] **Step 1: The preference**

`lightCalPrefs.ts`:

```ts
/** localStorage key for the calibrated-light container (default 'fits'). */
export const LIGHTCAL_FORMAT_KEY = 'athenaeum.lightcal.format';

export type CalibratedLightFormat = 'fits' | 'xisf';

export function readLightCalFormatPref(): CalibratedLightFormat {
  try {
    return localStorage.getItem(LIGHTCAL_FORMAT_KEY) === 'xisf' ? 'xisf' : 'fits';
  } catch {
    return 'fits';
  }
}
```

`ExportLightCalPrefs` gains `format: CalibratedLightFormat;`. `useExportSummary` and `startExport` add `format: lightCal.format` next to `debayer` in their invoke args.

- [x] **Step 2: The Export tab**

State `const [format, setFormat] = useState<CalibratedLightFormat>(readLightCalFormatPref)`, persisted in the same effect that writes the other prefs (`localStorage.setItem(LIGHTCAL_FORMAT_KEY, format)` in a try/catch). Add `format` to both literals (`:252`, `:944`). In the calibrated-lights section (`:588`), after the debayer toggle:

```tsx
<div>
  <div className="text-xs font-medium text-content-secondary mb-1">Output format</div>
  <div className="flex gap-4">
    {(['fits', 'xisf'] as const).map((v) => (
      <label key={v} className="flex items-center gap-2 text-xs text-content-secondary cursor-pointer">
        <input type="radio" name="lightcal-format" checked={format === v} onChange={() => setFormat(v)}
               className="w-3.5 h-3.5 text-accent border-border focus:ring-accent" />
        {v === 'fits' ? 'FITS' : 'XISF'}
      </label>
    ))}
  </div>
  <p className="text-[11px] text-content-muted mt-1">Same header cards either way. XISF is the container WBPP reads natively.</p>
</div>
```

- [x] **Step 3: The Send dialog and the browser send**

`SendToNodeDialog.tsx`: its `lightCalOptions` prop type gains `format`, and the invoke that carries `hotPixel`/`debayer` carries `format` too. `DualPaneFileBrowser.tsx`: where it builds `lightCalOptions` from `readLightCalParamsPref()` and friends, add `format: readLightCalFormatPref()`.

- [x] **Step 4: The Calibrate panel line** — after the debayer checkbox in `CalibratePanel.tsx`:

```tsx
        <p className="text-[11px] text-content-muted">
          Intermediate calibrated frames in the working folder are always FITS. The export and send
          format is chosen on the Export tab.
        </p>
```

- [x] **Step 5: Gate and commit**

Run: `npx tsc --noEmit`
```bash
git add src/hooks/useExportProgress.ts src/hooks/useExportData.ts src/components/export src/components/transfers/SendToNodeDialog.tsx src/components/dualpane/DualPaneFileBrowser.tsx src/components/stacking/panels/CalibratePanel.tsx
git commit -m "export ui: choose FITS or XISF for calibrated lights; the run's intermediates say they stay FITS"
```

---

### Task 7: Real-data run and the owed smokes

**Files:**
- Modify: `docs/superpowers/open-items.md` (new subsection), `docs/backlog-v0.6.5.md` (items 1/1b → SHIPPED, the R6 follow-up listed)

- [x] **Step 1: Build one real master as XISF** on the dev catalog: set `calibration.master_format = xisf` in Settings, build a master dark from a real raw set (Coverage → Create master), then:
  - `sqlite3` the dev DB: `SELECT path, format FROM files WHERE id = (SELECT file_id FROM frames WHERE is_master = 1 ORDER BY id DESC LIMIT 1);` → `.xisf`, `XISF`.
  - `head -c 4096 <path> | strings | grep -o 'bounds="[^"]*"\|imageType="[^"]*"'` → `bounds="0:65535"`, `imageType="MasterDark"`.
  - Export the linked frame set in **Calibrated lights** mode with format FITS, then compare one output against the same export made before this plan (a saved copy): `integrate_probe`-free check — read both with `examples/measure_probe.rs` or `python`+`astropy` and assert median difference `< 1e-3` (an XISF master must calibrate identically to its FITS twin).
- [x] **Step 2: Rebuild that master** — the file keeps `.xisf`; flip the setting back to `fits`, rebuild again — still `.xisf` (R4).
- [x] **Step 3: Export calibrated lights as XISF** — the tree holds `c_*.xisf`, each starts with `XISF0100`, `bounds="0:1"`, `imageType="Light"`.
- [x] **Step 3b: A mixed-container stacking run (owner requirement 2026-09-18: the stacker takes FITS and XISF side by side at every stage).** On a small real set (20 best LDN 1272 mono frames): convert half of the lights to XISF through `fits_writer::write_image_f32(…, Xisf, XisfBounds::Adu16)` from a throwaway `examples/` probe (read with `astroimage::ImageConverter::read_raw`, write the same cards), place them in a scan root beside the FITS half, rescan, build the master dark as XISF (the setting from Task 4) and the flat as FITS, then run the Stacking tab end to end. Pass: every frame calibrates (`stacking_artifacts` has a `calibrated` row per light, none excluded for "calibration failed"), the master light's median and MAD match the all-FITS run of the same 20 frames within 0.1 %, and the run log shows no `warn!` about a frame's container. This is the first end-to-end proof; stage 1 is the only stage that ever reads a source, and it funnels every container into `c_*.fits`.
- [x] **Step 4: Record the owner smokes** in `docs/superpowers/open-items.md`:
  - Open an Athenaeum XISF master dark, flat and dark flat in PixInsight; run WBPP with them added as masters: each must be listed as a master (no "must be in XISF format" refusal, the dark flat under DARK with the flat's exposure), and a light calibrated by WBPP with the Athenaeum master must match one calibrated by Athenaeum within noise.
  - Load a `c_*.xisf` calibrated light in PixInsight and in WBPP's calibrated-lights mode.
- [x] **Step 5: Backlog** — mark items 1 and 1b shipped with the commit range, add "XISF intermediates in the stacking run (`PlaneReader` XISF arm)" as the R6 follow-up.
- [x] **Step 6: Commit**

```bash
git add docs/superpowers/open-items.md docs/backlog-v0.6.5.md
git commit -m "docs: XISF masters and calibrated lights — the owed WBPP smokes"
```

---

## Self-review

- **Coverage.** Item 1 (masters): Tasks 1–4 + 7. Item 1b (calibrated lights): Tasks 5–6. Stacking masters: unchanged behaviour through Task 1's re-export. WBPP contract: R3 + the `imageType` test + Task 7's smoke.
- **Types.** `OutputFormat` (Task 1) is the type used in `MasterPathParams.format` (Task 2), `CalibratedLightOptions.format` (Task 5), the wire (Task 6). `XisfBounds::Adu16` for masters (Tasks 2, 3), `Unit` for calibrated lights and stacking (Tasks 1, 5). `calibrated_output_filename(source, debayer, format)` is the same three-argument shape at every call site listed in Task 5.
- **Placeholders.** Every "find it with grep" step names the grep and what the line must become; no step defers content.
