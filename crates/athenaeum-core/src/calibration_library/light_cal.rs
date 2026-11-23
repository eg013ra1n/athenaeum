//! Athenaeum's side of the light-calibration engine. The pixel math lives in `starloom`
//! (`starloom::calibration_library::light_cal`) and is re-exported here under the old path. This module keeps what
//! reads or writes Athenaeum's own cards and files: the reader of a master flat's `ATH_FNRM` / `ATH_FNR/G/B`
//! cards ([`AthenaeumFlatCards`]) and the wrappers that hand it to the math, `calibrate_light` (the legacy
//! single-shot path), and `write_calibrated_output` (which hashes the written file with Athenaeum's xxh3).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use starloom::calibration_library::light_cal as sl;

use crate::duplicates::compute_xxhash;
use crate::fits_parser::extract_fits_header;
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::keywords::Bayer;
use crate::fits_writer::{write_image_f32_with, Card, Durability, OutputFormat, XisfBounds};
use crate::integration::band_budget::MIN_BUDGET_BYTES;
use crate::integration::cfa::CfaGeometry;
use crate::integration::IntegrationError;
use crate::models::FileFormat;

pub use starloom::calibration_library::light_cal::*;
// The engine version stays in `models` (spec SL-19); the shared parameter types come through the glob above.
pub use crate::models::LIGHT_CAL_ENGINE_VERSION;

/// Reads a master flat's stamped normalization constants from Athenaeum's own cards. Handed to starloom's math,
/// which asks it exactly where the card used to be read inline.
struct AthenaeumFlatCards;

impl sl::FlatCardReader for AthenaeumFlatCards {
    fn channel_norms(&self, flat: &Path, light: CfaGeometry) -> Option<[f64; 3]> {
        read_ath_channel_norms(flat, light)
    }
    fn fnrm(&self, flat: &Path) -> Option<f64> {
        read_ath_fnrm(flat)
    }
}

/// [`starloom::calibration_library::light_cal::resolve_flat_norm_divisor_with`] reading Athenaeum's flat cards.
/// Same signature as before the move.
pub fn resolve_flat_norm_divisor(
    flat_path: &Path,
    scratch_dir: &Path,
    mode: FlatNormMode,
    params: &LightCalParams,
    cfa: Option<CfaGeometry>,
) -> Result<FlatNormDivisor, IntegrationError> {
    sl::resolve_flat_norm_divisor_with(flat_path, scratch_dir, mode, params, cfa, &AthenaeumFlatCards)
}

/// [`starloom::calibration_library::light_cal::flat_norm_constant_with`] reading Athenaeum's `ATH_FNRM` card.
/// Same signature as before the move.
pub fn flat_norm_constant(
    flat_path: &Path,
    scratch_dir: &Path,
    mode: FlatNormMode,
    trim_fraction: f64,
) -> Result<f64, IntegrationError> {
    sl::flat_norm_constant_with(flat_path, scratch_dir, mode, trim_fraction, &AthenaeumFlatCards)
}

/// The formula pass on its own (see starloom's `calibrate_light_compute`), with Athenaeum's flat cards read.
pub fn calibrate_light_compute(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
    calibrate_light_compute_with(inputs, None, cancel)
}

/// [`calibrate_light_compute`] with a group's shared master dark/bias and flat already decoded (see starloom's
/// `calibrate_light_compute_with`), with Athenaeum's flat cards read.
pub fn calibrate_light_compute_with(
    inputs: &LightCalInputs,
    preloaded: Option<&PreloadedMasters>,
    cancel: &AtomicBool,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
    sl::calibrate_light_compute_with_cards(inputs, preloaded, &AthenaeumFlatCards, cancel)
}

/// [`calibrate_light_compute_with`] with the band-memory budget as a parameter, so tests can force a multi-band
/// run on a small fixture.
fn calibrate_light_compute_inner(
    inputs: &LightCalInputs,
    preloaded: Option<&PreloadedMasters>,
    cancel: &AtomicBool,
    band_budget_bytes: usize,
) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError> {
    sl::calibrate_light_compute_budgeted(inputs, preloaded, cancel, band_budget_bytes, &AthenaeumFlatCards)
}

/// Calibrate one LIGHT frame and write the result to `inputs.output_path`.
///
/// Cancellation is cooperative: checked once per band before any pixel work,
/// so a cancel that lands before the write leaves no output file behind.
pub fn calibrate_light(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
) -> Result<LightCalOutcome, IntegrationError> {
    // A single light is one frame — already 1-2 bands at the floor budget, so
    // the machine-resolved budget (`integration::band_budget`) buys nothing
    // here; kept at the floor deliberately (spec §8).
    calibrate_light_inner(inputs, cancel, MIN_BUDGET_BYTES)
}

/// Write a calibrated plane to `path` and return the xxh3 of the written file.
///
/// The write is atomic — the underlying writer stages a sibling temp file and
/// renames it into place, so a failed write never truncates a good file already
/// sitting at `path`. `channels` is a parameter rather than the constant `1`
/// because a debayered frame goes out through this same door. `format`
/// chooses the container; a calibrated light is always `ATH_CSCL`-scaled, so
/// the XISF bounds are unconditionally [`XisfBounds::Unit`]. `durability`
/// (perf tier 1 Task 6) is the caller's own call — every export/send caller
/// passes [`Durability::Durable`]; only the stacking run's own regenerable
/// intermediates pass [`Durability::Volatile`].
pub fn write_calibrated_output(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
    format: OutputFormat,
    durability: Durability,
) -> Result<String, IntegrationError> {
    write_image_f32_with(path, width, height, channels, data, cards, format, XisfBounds::Unit, durability)
        .map_err(|e| io_err(format!("writing {}: {e}", path.display())))?;
    compute_xxhash(path).map_err(|e| io_err(format!("hashing {}: {e:#}", path.display())))
}

/// [`calibrate_light`] with the band-memory budget as a parameter, so tests can
/// force a multi-band run on a small fixture (the integration engine's
/// `integrate_flat_inner` takes its band size the same way).
fn calibrate_light_inner(
    inputs: &LightCalInputs,
    cancel: &AtomicBool,
    band_budget_bytes: usize,
) -> Result<LightCalOutcome, IntegrationError> {
    let (frame, mut outcome) = calibrate_light_compute_inner(inputs, None, cancel, band_budget_bytes)?;
    // This legacy single-shot path (superseded by
    // `export::calibrated_generator::execute_generation`, which every real
    // caller uses) has no format option of its own — it stays FITS.
    outcome.output_hash = write_calibrated_output(
        &inputs.output_path,
        frame.width,
        frame.height,
        1,
        &frame.data,
        &inputs.cards,
        OutputFormat::Fits,
        Durability::Durable,
    )?;

    tracing::debug!(
        src = %inputs.light_path.display(),
        dest = %inputs.output_path.display(),
        calstat = %outcome.calstat,
        flat_norm_divisor = outcome.flat_norm_divisor,
        cfa_scaling_applied = outcome.cfa_scaling_applied,
        width = frame.width,
        height = frame.height,
        floored_flat_pixels = outcome.floored_flat_pixels,
        non_positive_pixels = outcome.non_positive_pixels,
        "light calibrated"
    );

    Ok(outcome)
}

/// Best-effort read of a master flat's per-channel `ATH_FNR`/`ATH_FNG`/`ATH_FNB`
/// cards. All three must be present and usable as divisors (finite, > 0) — a
/// partial or broken triple counts as absent, so the caller recomputes from the
/// pixels rather than mixing a stamped constant with a recomputed one.
///
/// **The stamped constants are only usable if the flat's mosaic phase matches
/// the light's.** They were measured under the FLAT set's own consensus
/// geometry, while the pixel loop selects among them by the LIGHT's geometry —
/// so a disagreement (say the flat's members declared no `XBAYROFF`, making the
/// build assume 0, while this light declares 1) would feed every R pixel the B
/// constant and vice versa, silently, with `cfa_scaling_applied = true`
/// claiming it went fine. `build_master_cards` writes `BAYERPAT` (+ the offsets
/// it was given) into this same header in the same branch that produces the
/// constants, so the phase they were measured under is recoverable right here,
/// from the parse already being done. On a mismatch this returns `None`, which
/// drops the caller onto the recompute path — self-consistent by construction,
/// because that path measures the flat's pixels under the LIGHT's geometry.
///
/// A missing `XBAYROFF`/`YBAYROFF` reads as 0: that is exactly what
/// `measure_flat_channel_norms` assumed when it computed the constants, so it
/// reconstructs the build-time phase rather than guessing a new one. A flat
/// with the constants but no `BAYERPAT` cannot be verified at all and is
/// treated as a mismatch — unreachable for an Athenaeum-built master (the
/// pattern and the constants ride together, both gated on the same parse).
fn read_ath_channel_norms(flat_path: &Path, light: CfaGeometry) -> Option<[f64; 3]> {
    let header_text = extract_fits_header(flat_path).ok()?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header_text);
    let read = |kw: &str| -> Option<f64> {
        keys.get(kw)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    let k = [read("ATH_FNR")?, read("ATH_FNG")?, read("ATH_FNB")?];

    let offset = |kw: &str| -> i64 {
        keys.get(kw)
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0)
    };
    let flat_geom = keys
        .get("BAYERPAT")
        .and_then(|p| Bayer::parse(p))
        .map(|pattern| CfaGeometry {
            pattern,
            xoff: offset("XBAYROFF"),
            yoff: offset("YBAYROFF"),
        });
    // Offsets compare modulo 2 ([`CfaGeometry::same_phase`], the single home of
    // the parity rule): an `XBAYROFF` of 2 and one of 0 are the same phase, and
    // calling that a disagreement would push a perfectly good flat onto the
    // recompute path for nothing.
    match flat_geom {
        Some(g) if g.same_phase(light) => Some(k),
        _ => {
            tracing::warn!(
                path = %flat_path.display(),
                flat_pattern = flat_geom.map(|g| g.pattern.as_str()).unwrap_or("none"),
                light_pattern = light.pattern.as_str(),
                "flat card geometry disagrees with light — per-channel constants recomputed from the flat plane"
            );
            None
        }
    }
}

/// Best-effort read of a flat's `ATH_FNRM` card. Any failure (unreadable
/// header, card absent, unparseable value) returns `None` so the caller falls
/// back to recomputing the constant. A card that parses but cannot serve as a
/// divisor (non-finite, zero, negative) counts as absent for the same reason —
/// recomputing from the pixels beats trusting a broken stamp.
fn read_ath_fnrm(flat_path: &Path) -> Option<f64> {
    let header_text = extract_fits_header(flat_path).ok()?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header_text);
    keys.get("ATH_FNRM")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
}

/// Wrap a non-`IntegrationError` failure (FITS write / hashing) as an IO
/// error so it flows through the engine's single error type.
fn io_err(msg: String) -> IntegrationError {
    IntegrationError::Io(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::Bayer;
    use crate::fits_writer::{write_fits_f32, Card, CardValue};
    use crate::integration::banded::{BandPlanes, BandSource};
    use crate::integration::engine::central_third_mean;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn write_plane(dir: &Path, name: &str, w: usize, h: usize, val: f32, cards: &[Card]) -> PathBuf {
        let data = vec![val; w * h];
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &data, cards).unwrap();
        p
    }

    fn write_fill(
        dir: &Path,
        name: &str,
        w: usize,
        h: usize,
        fill: impl Fn(usize, usize) -> f32,
        cards: &[Card],
    ) -> PathBuf {
        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = fill(x, y);
            }
        }
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &data, cards).unwrap();
        p
    }

    /// Read a whole f32 FITS back into RAM (one band) for exact assertions.
    fn read_all(path: &Path, scratch: &Path) -> (usize, usize, Vec<f32>) {
        let src = BandSource::open(&[path.to_path_buf()], scratch, 1).unwrap();
        let (w, h) = (src.width(), src.height());
        let mut planes = BandPlanes::new(&src);
        src.read_band(0, h, &mut planes, 1).unwrap();
        let mut data = vec![0f32; w * h];
        planes.decode_frame_into(0, &mut data);
        (w, h, data)
    }

    fn fnrm_card(v: f64) -> Card {
        Card::new("ATH_FNRM", CardValue::Real(v)).unwrap()
    }

    /// Mirror the engine's per-pixel math exactly (f64 throughout, cast at the
    /// end) so tests can assert bit-exact f32 output.
    fn expect_px(l: f64, s: Option<f64>, f: Option<f64>, divisor: f64) -> f32 {
        expect_px_ped(l, s, f, divisor, 0.0)
    }

    /// [`expect_px`] with an output pedestal (DN) added after the scale divide,
    /// mirroring the engine's `add_pedestal` step exactly (f64 throughout).
    fn expect_px_ped(l: f64, s: Option<f64>, f: Option<f64>, divisor: f64, pedestal_dn: f64) -> f32 {
        let mut v = l;
        if let Some(s) = s {
            v -= s;
        }
        if let Some(f) = f {
            v /= f / divisor;
        }
        v /= OUTPUT_SCALE_DIVISOR;
        if pedestal_dn != 0.0 {
            v += pedestal_dn / OUTPUT_SCALE_DIVISOR;
        }
        v as f32
    }

    #[allow(clippy::too_many_arguments)]
    fn inputs(
        dir: &Path,
        light: PathBuf,
        dark: Option<PathBuf>,
        bias: Option<PathBuf>,
        flat: Option<PathBuf>,
        flat_norm: bool,
        out: PathBuf,
    ) -> LightCalInputs {
        LightCalInputs {
            light_path: light,
            dark_path: dark,
            bias_path: bias,
            flat_path: flat,
            flat_norm,
            flat_norm_mode: FlatNormMode::CentralThird,
            // Mono by default: the CFA fixtures below opt in explicitly, so
            // every pre-existing expectation stays on the global path.
            cfa_geometry: None,
            params: LightCalParams::default(),
            // The fixtures below assert against `expect_px`, which mirrors the
            // engine with this same divisor; the per-BITPIX resolution itself is
            // covered by `scale_divisor_follows_source_bit_depth`.
            scale_divisor: OUTPUT_SCALE_DIVISOR,
            output_path: out,
            cards: vec![],
            scratch_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn full_formula_bdf() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, Some(flat), true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BDF");
        assert!((outcome.flat_norm_divisor - 2.0).abs() < 1e-12);
        assert!(!outcome.output_hash.is_empty());

        let (rw, rh, data) = read_all(&out, dir.path());
        assert_eq!((rw, rh), (w, h));
        // F_norm = 2.0/2.0 = 1.0, so every pixel = (1100-100)/1.0/65535.
        let expected = expect_px(1100.0, Some(100.0), Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    /// Fixture for the perf tier 1 Task 5 preload pins: a light calibrated
    /// against a dark and a flat, mirroring `full_formula_bdf`'s setup but
    /// with per-pixel VARYING values (not uniform planes) — a preload path
    /// that reads the wrong SAMPLE shows up as a numeric mismatch rather
    /// than being masked by every pixel already being equal. At the default
    /// `MIN_BUDGET_BYTES` this 8x9 fixture is a SINGLE band, so `full ==
    /// idx` for every pixel here and this fixture alone cannot catch a
    /// band-local-vs-frame-global indexing bug (fix round 1, Important
    /// finding 3) — `preloaded_masters_match_across_multiple_bands` below
    /// forces several bands on a taller fixture for that case.
    fn fixture_light_dark_flat() -> (tempfile::TempDir, LightCalInputs) {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_fill(
            dir.path(),
            "light.fits",
            w,
            h,
            |x, y| 1000.0 + (x + y * w) as f32,
            &[],
        );
        let dark = write_fill(dir.path(), "dark.fits", w, h, |x, _y| 50.0 + x as f32, &[]);
        let flat = write_fill(
            dir.path(),
            "flat.fits",
            w,
            h,
            |_x, y| 1.5 + y as f32 * 0.1,
            &[fnrm_card(2.0)],
        );
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, Some(flat), true, out);
        (dir, cfg)
    }

    /// Perf tier 1 Task 5: a frame calibrated from preloaded master planes is
    /// bit-identical to one calibrated from the files.
    #[test]
    fn preloaded_masters_calibrate_bit_identically() {
        let (dir, inputs) = fixture_light_dark_flat();
        let cancel = AtomicBool::new(false);
        let (from_files, _) = calibrate_light_compute(&inputs, &cancel).unwrap();
        let mut planes = HashMap::new();
        for p in [inputs.dark_path.clone(), inputs.flat_path.clone()].into_iter().flatten() {
            planes.insert(p.clone(), preload_master_plane(&p, dir.path(), &cancel).unwrap());
        }
        let pre = PreloadedMasters { planes };
        let (from_ram, _) = calibrate_light_compute_with(&inputs, Some(&pre), &cancel).unwrap();
        assert_eq!(from_files.data, from_ram.data);
    }

    /// Fix round 1, Important finding 3: `preloaded_masters_calibrate_
    /// bit_identically` runs its 8x9 fixture at `MIN_BUDGET_BYTES`, which is
    /// a SINGLE band — `full == idx` for every pixel there, so writing
    /// `sub[idx]`/`f[idx]` instead of `sub[full]`/`f[full]` would still pass
    /// it. This pin forces several bands (a 300-byte budget, the same
    /// technique `multi_band_run_keeps_the_global_cfa_row_phase` uses below)
    /// on a taller fixture and calls the PRIVATE, budget-parameterized
    /// `calibrate_light_compute_inner` directly, covering BOTH arms: the
    /// flat varies in y (`1.5 + y*0.1`), and the dark varies in y TOO (fix
    /// round 2, Important finding 3 residue — `idx % w == full % w` always,
    /// so an x-only-varying dark made `sub.data[idx]` and `sub.data[full]`
    /// read equal VALUES from different rows and this pin could not have
    /// caught `sub.data[full]` regressing to `sub.data[idx]` in the
    /// subtrahend arm; `50.0 + x + y*0.25` closes that gap).
    #[test]
    fn preloaded_masters_match_across_multiple_bands() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 12usize);
        let light = write_fill(dir.path(), "light.fits", w, h, |x, y| 1000.0 + (x + y * w) as f32, &[]);
        let dark = write_fill(dir.path(), "dark.fits", w, h, |x, y| 50.0 + x as f32 + y as f32 * 0.25, &[]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, |_x, y| 1.5 + y as f32 * 0.1, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light.clone(), Some(dark.clone()), None, Some(flat.clone()), true, out);

        // Confirm the fixture actually forces multiple bands at this budget
        // — a passing pin on a single-band fixture would prove nothing.
        let probe = BandSource::open(&[light, dark.clone(), flat.clone()], dir.path(), 1).unwrap();
        let band_rows = probe.band_rows_for_budget(300);
        assert!(band_rows < h, "fixture must force multiple bands at this budget, got {band_rows} rows of {h}");

        let cancel = AtomicBool::new(false);
        let mut planes = HashMap::new();
        for p in [dark, flat] {
            planes.insert(p.clone(), preload_master_plane(&p, dir.path(), &cancel).unwrap());
        }
        let pre = PreloadedMasters { planes };

        let (from_files, _) = calibrate_light_compute_inner(&cfg, None, &cancel, 300).unwrap();
        let (from_ram, _) = calibrate_light_compute_inner(&cfg, Some(&pre), &cancel, 300).unwrap();
        assert_eq!(from_files.data, from_ram.data);
    }

    /// A preloaded plane whose geometry doesn't match the light's own is
    /// refused loudly (ruling 4) rather than read out of bounds or silently
    /// ignored. Transposed relative to the light's 8x9 — same total sample
    /// count (72) as a length-only check would miss, but the wrong shape
    /// (fix round 1, ruled-in item 4: `PreloadedPlane` carries `(width,
    /// height)`, not just a sample count).
    #[test]
    fn preloaded_master_geometry_mismatch_is_refused() {
        let (dir, inputs) = fixture_light_dark_flat();
        let dark_path = inputs.dark_path.clone().unwrap();
        let mut planes = HashMap::new();
        planes.insert(dark_path, PreloadedPlane { data: Arc::new(vec![0f32; 9 * 8]), width: 9, height: 8 });
        let pre = PreloadedMasters { planes };
        let cancel = AtomicBool::new(false);
        match calibrate_light_compute_with(&inputs, Some(&pre), &cancel) {
            Err(IntegrationError::BadInput(msg)) => {
                assert!(msg.contains("preloaded master geometry mismatch"), "{msg}");
            }
            other => panic!(
                "expected Err(BadInput), got {}",
                match other {
                    Ok(_) => "Ok".to_string(),
                    Err(e) => format!("{e:?}"),
                }
            ),
        }
        drop(dir);
    }

    /// A calibration master's samples are raw ADU (`XisfBounds::Adu16`), the
    /// same domain a FITS master's are. The SAME dark written in both
    /// containers must calibrate a light identically — this is the test that
    /// would have caught the dark being read back scaled by 65535x (or
    /// divided by it) if the wrong bounds were ever wired to a master read.
    #[test]
    fn full_formula_matches_with_an_xisf_dark() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);

        // FITS dark — the same fixture full_formula_bdf uses.
        let dark_fits = write_plane(dir.path(), "dark_fits.fits", w, h, 100.0, &[]);
        let out_fits = dir.path().join("out_fits.fits");
        let cfg_fits = inputs(
            dir.path(), light.clone(), Some(dark_fits), None, Some(flat.clone()), true,
            out_fits.clone(),
        );
        calibrate_light(&cfg_fits, &AtomicBool::new(false)).unwrap();
        let (_, _, data_fits) = read_all(&out_fits, dir.path());

        // The SAME dark value, written as an XISF master (raw ADU domain,
        // XisfBounds::Adu16 — the bounds `register_master`'s written masters
        // and `run_build`'s use).
        let dark_xisf = dir.path().join("dark_xisf.xisf");
        crate::fits_writer::write_image_f32(
            &dark_xisf,
            w,
            h,
            1,
            &vec![100.0f32; w * h],
            &[],
            OutputFormat::Xisf,
            XisfBounds::Adu16,
        )
        .unwrap();
        let out_xisf = dir.path().join("out_xisf.fits");
        let cfg_xisf =
            inputs(dir.path(), light, Some(dark_xisf), None, Some(flat), true, out_xisf.clone());
        calibrate_light(&cfg_xisf, &AtomicBool::new(false)).unwrap();
        let (_, _, data_xisf) = read_all(&out_xisf, dir.path());

        assert_eq!(data_fits.len(), data_xisf.len());
        for (a, b) in data_fits.iter().zip(&data_xisf) {
            assert!(
                (a - b).abs() < 1e-3,
                "FITS-dark output {a} vs XISF-dark output {b} diverge by more than 1e-3 — \
                 an XISF master must calibrate identically to its FITS twin"
            );
        }
    }

    #[test]
    fn bias_fallback_bf() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let bias = write_plane(dir.path(), "bias.fits", w, h, 50.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, None, Some(bias), Some(flat), true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BF");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(1100.0, Some(50.0), Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    #[test]
    fn dark_only_bd() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BD");
        assert!((outcome.flat_norm_divisor - 1.0).abs() < 1e-12, "no flat -> divisor 1.0");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(1100.0, Some(100.0), None, 1.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    #[test]
    fn flat_only_f() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "F");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(1100.0, None, Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
    }

    #[test]
    fn flat_norm_off_changes_scale() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let out = dir.path().join("out.fits");
        // Same inputs as full_formula_bdf but with normalization OFF.
        let cfg = inputs(dir.path(), light, Some(dark), None, Some(flat), false, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BDF", "flat still applied, just not normalized");
        assert!((outcome.flat_norm_divisor - 1.0).abs() < 1e-12, "normalization off -> divisor 1.0");

        let (_, _, data) = read_all(&out, dir.path());
        // divisor 1.0 => divide by (F/1) = 2.0, so half the normalized value.
        let expected_off = expect_px(1100.0, Some(100.0), Some(2.0), 1.0);
        let expected_on = expect_px(1100.0, Some(100.0), Some(2.0), 2.0);
        assert!(data.iter().all(|&v| v == expected_off), "got {}, want {expected_off}", data[0]);
        assert!(
            (data[0] as f64 * 2.0 - expected_on as f64).abs() < 1e-9,
            "flat-norm-off output must be 2x smaller (flat=2.0): {} vs {}",
            data[0],
            expected_on
        );
    }

    #[test]
    fn negatives_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        // light < dark => negative numerator, must not be clamped.
        let light = write_plane(dir.path(), "light.fits", w, h, 50.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, true, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.calstat, "BD");

        let (_, _, data) = read_all(&out, dir.path());
        let expected = expect_px(50.0, Some(100.0), None, 1.0);
        assert!(expected < 0.0, "fixture must produce a negative expectation");
        assert!(data.iter().all(|&v| v == expected), "got {}, want {expected}", data[0]);
        assert!(data.iter().all(|&v| v < 0.0), "negatives must survive unclamped");
    }

    #[test]
    fn the_non_positive_count_is_the_pixels_a_dark_leaves_at_or_below_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        // The light reads 500 everywhere; the dark is 800 on the left three
        // columns and 100 elsewhere, so exactly 3 columns go below zero.
        let light = write_plane(dir.path(), "light.fits", w, h, 500.0, &[]);
        let dark = write_fill(
            dir.path(),
            "dark.fits",
            w,
            h,
            |x, _| if x < 3 { 800.0 } else { 100.0 },
            &[],
        );
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light.clone(), Some(dark), None, None, true, out);
        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.non_positive_pixels, 3 * h as u64);
        assert_eq!(outcome.pixels, (w * h) as u64);

        // A pixel exactly at zero counts (the bar is `<= 0`).
        let dark_eq = write_plane(dir.path(), "dark_eq.fits", w, h, 500.0, &[]);
        let out_eq = dir.path().join("out_eq.fits");
        let cfg = inputs(dir.path(), light.clone(), Some(dark_eq), None, None, true, out_eq);
        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.non_positive_pixels, (w * h) as u64);

        // A flat-only frame subtracts nothing: nothing counted, and no denominator.
        let out_none = dir.path().join("out_none.fits");
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
        let cfg = inputs(dir.path(), light, None, None, Some(flat), true, out_none);
        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!((outcome.non_positive_pixels, outcome.pixels), (0, 0));
    }

    #[test]
    fn geometry_mismatch_errors() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(dir.path(), "light.fits", 8, 9, 1100.0, &[]);
        // Dark is a different size — BandSource::open must reject the set.
        let dark = write_plane(dir.path(), "dark.fits", 16, 9, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, false, out.clone());

        let r = calibrate_light(&cfg, &AtomicBool::new(false));
        assert!(matches!(r, Err(IntegrationError::BadInput(_))), "expected BadInput, got {r:?}");
        assert!(!out.exists(), "no output on a geometry error");
    }

    #[test]
    fn fnrm_recomputed_when_card_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        // Column gradient, NO ATH_FNRM card -> constant must be recomputed.
        let fill = |x: usize, _y: usize| 100.0 + x as f32;
        let flat = write_fill(dir.path(), "flat_nocard.fits", w, h, fill, &[]);
        let mut fdata = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                fdata[y * w + x] = fill(x, y);
            }
        }
        let expected = central_third_mean(&fdata, w, h);
        assert!(expected > 100.0, "central-third mean must reflect the gradient");

        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        assert!((got - expected).abs() < 1e-9, "recomputed {got}, want central-third mean {expected}");

        // And when the card IS present it takes precedence over recomputation,
        // even with identical pixel data (value differs from the mean above).
        let flat_carded = write_fill(dir.path(), "flat_carded.fits", w, h, fill, &[fnrm_card(999.0)]);
        let carded = flat_norm_constant(&flat_carded, dir.path(), FlatNormMode::CentralThird, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        assert!((carded - 999.0).abs() < 1e-9, "ATH_FNRM card must win over recomputation, got {carded}");
    }

    /// The seam guard (spec SL-8): Athenaeum's flat-norm reading honours the `ATH_FNRM` card; starloom's own
    /// reading never looks at cards, so the same file gives the pixel-derived constant there.
    #[test]
    fn core_flat_norm_reads_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        let fill = |x: usize, _y: usize| 100.0 + x as f32;
        let flat = write_fill(dir.path(), "flat_card_differs.fits", w, h, fill, &[fnrm_card(999.0)]);
        let mut fdata = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                fdata[y * w + x] = fill(x, y);
            }
        }
        let from_pixels = central_third_mean(&fdata, w, h);
        assert!((from_pixels - 999.0).abs() > 1.0, "fixture: the card must differ from the pixels");

        let core =
            flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        assert!((core - 999.0).abs() < 1e-9, "core must return the ATH_FNRM card value, got {core}");

        let starloom = starloom::calibration_library::light_cal::flat_norm_constant(
            &flat,
            dir.path(),
            FlatNormMode::CentralThird,
            TRIMMED_MEAN_TAIL_FRACTION,
        )
        .unwrap();
        assert!(
            (starloom - from_pixels).abs() < 1e-9,
            "starloom must return the pixel-derived constant {from_pixels}, got {starloom}"
        );
    }

    /// Regression guard (ruling R3): the `ATH_FNRM` reader is FITS-only, so a master flat written as XISF never has
    /// its card honoured; the constant is recomputed from the pixels.
    #[test]
    fn non_fits_master_flat_recomputes_from_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        let fill = |x: usize, _y: usize| 100.0 + x as f32;
        let mut fdata = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                fdata[y * w + x] = fill(x, y);
            }
        }
        let flat = dir.path().join("flat_card_differs.xisf");
        crate::fits_writer::write_xisf_f32(&flat, w, h, 1, &fdata, &[fnrm_card(999.0)]).unwrap();
        let from_pixels = central_third_mean(&fdata, w, h);
        assert!((from_pixels - 999.0).abs() > 1.0, "fixture: the card must differ from the pixels");

        let core =
            flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        // The pixel reader takes an XISF float plane as unit-range and scales it to 16-bit ADU, so the
        // pixel-derived constant is the plane's mean times 65535.
        let expected = from_pixels * 65535.0;
        assert!(
            (core - expected).abs() <= expected * 1e-9,
            "a non-FITS master flat must recompute from pixels ({expected}), got {core}"
        );
        assert!((core - 999.0).abs() > 1.0, "the ATH_FNRM card of a non-FITS flat must be ignored, got {core}");
    }

    #[test]
    fn pedestal_adds_after_scale_divide() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);

        // Zero-pedestal run must be bit-identical to the pre-feature output.
        let out0 = dir.path().join("out0.fits");
        let cfg0 = inputs(dir.path(), light.clone(), Some(dark.clone()), None, None, true, out0.clone());
        calibrate_light(&cfg0, &AtomicBool::new(false)).unwrap();
        let (_, _, d0) = read_all(&out0, dir.path());
        let base = expect_px(1100.0, Some(100.0), None, 1.0);
        assert!(d0.iter().all(|&v| v == base), "zero pedestal must match legacy output");

        // +100 DN must add exactly 100/65535 to every pixel (in f64, cast once).
        let out1 = dir.path().join("out1.fits");
        let mut cfg1 = inputs(dir.path(), light, Some(dark), None, None, true, out1.clone());
        cfg1.params.pedestal_dn = 100.0;
        calibrate_light(&cfg1, &AtomicBool::new(false)).unwrap();
        let (_, _, d1) = read_all(&out1, dir.path());
        let expected = expect_px_ped(1100.0, Some(100.0), None, 1.0, 100.0);
        assert!(d1.iter().all(|&v| v == expected), "got {}, want {expected}", d1[0]);
        assert!(
            (expected as f64 - base as f64 - 100.0 / OUTPUT_SCALE_DIVISOR).abs() < 1e-9,
            "pedestal must shift output by exactly pedestal_dn/scale"
        );
    }

    #[test]
    fn trimmed_mean_whole_mode_ignores_fnrm_card() {
        // A 40x25 = 1000-pixel plane of the distinct values 0..=999, stamped
        // with a BOGUS ATH_FNRM card. TrimmedMeanWhole must IGNORE the card and
        // return the trimmed mean 499.5; CentralThird must read the card (999.0).
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (40usize, 25usize);
        let flat = write_fill(dir.path(), "flat_trim.fits", w, h, |x, y| (y * w + x) as f32, &[fnrm_card(999.0)]);

        let trimmed = flat_norm_constant(&flat, dir.path(), FlatNormMode::TrimmedMeanWhole, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        assert!((trimmed - 499.5).abs() < 1e-6, "trimmed {trimmed}, want 499.5 (card ignored)");

        let central = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        assert!((central - 999.0).abs() < 1e-9, "central-third must read the card verbatim, got {central}");
    }

    /// Run the engine over a 2x2 light of 100.0 divided by an explicit 4-pixel
    /// flat plane, returning the outcome plus the written pixels. With
    /// `flat_norm` off the divisor stays 1.0, so `flat_px` ARE the denominators;
    /// with it on the flat carries no `ATH_FNRM` card, so the divisor is the
    /// recomputed central-third mean — which on a 2x2 plane is `flat_px[0]`.
    fn run_engine_with_flat(flat_px: &[f32], flat_norm: bool) -> (LightCalOutcome, Vec<f32>) {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (2usize, 2usize);
        assert_eq!(flat_px.len(), w * h, "fixture must be a full 2x2 plane");
        let light = write_plane(dir.path(), "light.fits", w, h, 100.0, &[]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, |x, y| flat_px[y * w + x], &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, None, None, Some(flat), flat_norm, out.clone());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        let (_, _, data) = read_all(&out, dir.path());
        (outcome, data)
    }

    #[test]
    fn zero_and_negative_flat_pixels_are_floored_not_inf() {
        // Pixel 0 = 0.0 is the master-build seam: a pixel whose every sample was
        // rejected as non-finite arrives here as 0.0 in a built master flat.
        // Pixel 1 = -0.5 is the sign-flip case. Both must floor, neither may
        // produce Inf/NaN or turn a positive light negative.
        let (outcome, data) = run_engine_with_flat(&[0.0, -0.5, 1.0, 1.0], false);
        assert_eq!(outcome.calstat, "F");
        assert!(data.iter().all(|v| v.is_finite()), "no Inf/NaN survives the floor: {data:?}");
        assert!(data[0] > 0.0 && data[1] > 0.0, "no sign flips: {} {}", data[0], data[1]);
        assert_eq!(outcome.floored_flat_pixels, 2, "exactly the dead + negative pixels");

        // Both floored pixels divide by the same FLAT_DENOM_FLOOR, so they land
        // on the identical value; the healthy pixels are untouched.
        let floored = expect_px(100.0, None, Some(FLAT_DENOM_FLOOR), 1.0);
        assert_eq!(data[0], floored, "dead flat pixel must divide by the floor");
        assert_eq!(data[1], floored, "negative flat pixel must divide by the floor");
        let healthy = expect_px(100.0, None, Some(1.0), 1.0);
        assert_eq!(data[2], healthy);
        assert_eq!(data[3], healthy);
    }

    #[test]
    fn floor_applies_with_flat_normalization_on() {
        // The default-ON mode: the floor is compared against the NORMALIZED
        // denominator (flat / ATH_FNRM), not the raw flat value. Divisor =
        // central-third mean = flat_px[0] = 1000.0, so the healthy pixels
        // normalize to 1.0 while the dead (0.0) and negative (-5.0) ones stay
        // below the floor and must be floored exactly as in the off mode.
        let (outcome, data) = run_engine_with_flat(&[1000.0, 0.0, -5.0, 1000.0], true);
        assert_eq!(outcome.calstat, "F");
        assert!(
            (outcome.flat_norm_divisor - 1000.0).abs() < 1e-9,
            "divisor {} must be the recomputed central-third mean",
            outcome.flat_norm_divisor
        );
        assert_eq!(outcome.floored_flat_pixels, 2, "exactly the dead + negative pixels");
        assert!(data.iter().all(|v| v.is_finite()), "no Inf/NaN survives the floor: {data:?}");
        assert!(data.iter().all(|&v| v > 0.0), "no sign flips: {data:?}");

        let floored = expect_px(100.0, None, Some(FLAT_DENOM_FLOOR), 1.0);
        assert_eq!(data[1], floored, "dead flat pixel must divide by the floor");
        assert_eq!(data[2], floored, "negative flat pixel must divide by the floor");
        // The healthy pixels normalize to exactly 1.0 — untouched by the floor.
        let healthy = expect_px(100.0, None, Some(1000.0), 1000.0);
        assert_eq!(data[0], healthy);
        assert_eq!(data[3], healthy);
    }

    #[test]
    fn denominator_exactly_at_the_floor_is_not_floored() {
        // Boundary pin on the `denom >= FLAT_DENOM_FLOOR` test. An f32 flat
        // pixel can never *be* 2.0e-5 exactly, but a normalized one can:
        // 2.0 / 100000.0 rounds to the same f64 as the FLAT_DENOM_FLOOR
        // literal, so pixel 1 sits exactly ON the boundary. It must take the
        // unfloored arm (count 0) while landing on the arithmetically
        // identical value the floored arm would have produced.
        let (outcome, data) = run_engine_with_flat(&[100000.0, 2.0, 100000.0, 100000.0], true);
        assert_eq!(2.0f64 / 100000.0, FLAT_DENOM_FLOOR, "fixture must sit exactly on the floor");
        assert_eq!(
            outcome.floored_flat_pixels, 0,
            "a denominator equal to the floor is not a floored pixel"
        );
        assert_eq!(
            data[1],
            expect_px(100.0, None, Some(FLAT_DENOM_FLOOR), 1.0),
            "on-boundary pixel must equal the floored-arm value"
        );
    }

    #[test]
    fn healthy_flat_never_counts_a_floored_pixel() {
        // Regression guard on the counter: a flat whose smallest pixel is still
        // above the floor must report zero hits (the warn must stay silent).
        let (outcome, data) = run_engine_with_flat(&[1.0, 0.5, 2.0, 1e-4], false);
        assert_eq!(outcome.floored_flat_pixels, 0);
        assert!(data.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn bogus_ath_fnrm_card_falls_back_to_recompute() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (9usize, 9usize);
        let fill = |x: usize, _y: usize| 100.0 + x as f32;
        // A non-positive ATH_FNRM is unusable as a divisor — treated as absent,
        // so the constant is recomputed from the pixels instead.
        let flat = write_fill(dir.path(), "flat_zero_card.fits", w, h, fill, &[fnrm_card(0.0)]);
        let mut fdata = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                fdata[y * w + x] = fill(x, y);
            }
        }
        let expected = central_third_mean(&fdata, w, h);

        let got = flat_norm_constant(&flat, dir.path(), FlatNormMode::CentralThird, TRIMMED_MEAN_TAIL_FRACTION).unwrap();
        assert!((got - expected).abs() < 1e-9, "bogus card must fall through to recompute: {got} vs {expected}");
    }

    #[test]
    fn scale_divisor_follows_source_bit_depth() {
        assert_eq!(scale_divisor_for_bitpix(Some(8)), 255.0);
        assert_eq!(scale_divisor_for_bitpix(Some(16)), 65535.0);
        assert_eq!(scale_divisor_for_bitpix(Some(32)), 4294967295.0);
        assert_eq!(scale_divisor_for_bitpix(Some(-32)), 1.0);
        assert_eq!(scale_divisor_for_bitpix(Some(-64)), 1.0);
        // Unknown depth (spill-path formats, unreadable header) keeps the
        // historic 16-bit divisor rather than guessing.
        assert_eq!(scale_divisor_for_bitpix(None), 65535.0);
        assert_eq!(scale_divisor_for_bitpix(Some(0)), OUTPUT_SCALE_DIVISOR);
    }

    // ── Per-channel (CFA) flat scaling ───────────────────────────────────────

    /// Paint a `w`x`h` RGGB mosaic at zero phase with one constant per colour.
    /// Painted from the pattern definition directly (even/even = R, odd/odd = B,
    /// the diagonal = G), NOT via `cfa_channel_at`, so the assertions test the
    /// engine's mapping instead of restating it.
    fn rggb_fill(r: f32, g: f32, blue: f32) -> impl Fn(usize, usize) -> f32 {
        move |x, y| match (x % 2 == 0, y % 2 == 0) {
            (true, true) => r,
            (false, false) => blue,
            _ => g,
        }
    }

    fn rggb_geom() -> CfaGeometry {
        CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 0,
            yoff: 0,
        }
    }

    /// Run the engine over an RGGB light divided by an RGGB flat, with
    /// per-channel scaling on or off. No dark; the flat carries no `ATH_FN*`
    /// cards, so both modes exercise the recompute-from-pixels path.
    fn run_cfa(
        w: usize,
        h: usize,
        light: (f32, f32, f32),
        flat: (f32, f32, f32),
        cfa_flat_scaling: bool,
    ) -> (tempfile::TempDir, LightCalOutcome, Vec<f32>) {
        let dir = tempfile::tempdir().unwrap();
        let light_path = write_fill(dir.path(), "light.fits", w, h, rggb_fill(light.0, light.1, light.2), &[]);
        let flat_path = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(flat.0, flat.1, flat.2), &[]);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light_path, None, None, Some(flat_path), true, out.clone());
        cfg.params.cfa_flat_scaling = cfa_flat_scaling;
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        let (_, _, data) = read_all(&out, dir.path());
        (dir, outcome, data)
    }

    /// THE tool-parity pin. A flat that carries a strong colour of its own (the
    /// normal case: a CFA flat's R/G/B levels differ by the sensor's own colour
    /// response) must divide each colour by ITS OWN level, so the light's
    /// channel ratios survive calibration.
    ///
    /// Fixture: light R=1000 / G=2000 / B=500, flat R=2000 / G=4000 / B=1000 —
    /// the flat is exactly 2x the light in every channel. Per-channel: every
    /// pixel divides by its own channel constant (denominator exactly 1.0), so
    /// the output IS the light, ratios 2:4:1 intact. Globally: one constant
    /// (the central-third blend 2750) divides all three, and because this flat's
    /// colour matches the light's, every channel lands on the SAME value —
    /// 1375/65535 — the light's colour flattened out of existence. That
    /// collapse is the bug this feature fixes, so both arms are pinned exactly.
    #[test]
    fn per_channel_scaling_preserves_the_lights_channel_ratios() {
        let (w, h) = (6usize, 6usize);
        let (light, flat) = ((1000.0, 2000.0, 500.0), (2000.0, 4000.0, 1000.0));

        // ---- per-channel ON ----
        let (dir, outcome, data) = run_cfa(w, h, light, flat, true);
        assert_eq!(outcome.calstat, "F");
        assert!(outcome.cfa_scaling_applied, "an RGGB light + flat must scale per channel");
        // The per-channel constants are the painted flat levels, so the
        // global-equivalent number is their mosaic-weighted blend — and on this
        // fixture that lands exactly on the central-third global constant
        // (2750, asserted below), which is the point of weighting by G twice.
        let want_blend = (2000.0 + 2.0 * 4000.0 + 1000.0) / 4.0;
        assert_eq!(want_blend, 2750.0);
        assert!(
            (outcome.flat_norm_divisor - want_blend).abs() < 1e-9,
            "global-equivalent divisor {} must be the mosaic-weighted blend",
            outcome.flat_norm_divisor
        );

        // Bit-exact per pixel, through the same f64 mirror the global tests use:
        // each channel's denominator is flat_c / k_c = 1.0.
        let expect_on = |px: f64, k: f64| expect_px(px, None, Some(k), k);
        let on_r = expect_on(1000.0, 2000.0);
        let on_g = expect_on(2000.0, 4000.0);
        let on_b = expect_on(500.0, 1000.0);
        for y in 0..h {
            for x in 0..w {
                let want = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => on_r,
                    (false, false) => on_b,
                    _ => on_g,
                };
                assert_eq!(data[y * w + x], want, "per-channel pixel ({x},{y})");
            }
        }
        // The whole point: the light's colour ratios survive. 2:4:1 in, 2:4:1 out.
        assert!((on_g as f64 / on_r as f64 - 2.0).abs() < 1e-6, "G/R must stay 2");
        assert!((on_r as f64 / on_b as f64 - 2.0).abs() < 1e-6, "R/B must stay 2");
        drop(dir);

        // ---- per-channel OFF (global, the pre-feature behavior) ----
        let (dir, outcome, data) = run_cfa(w, h, light, flat, false);
        assert!(!outcome.cfa_scaling_applied, "the toggle must turn it off");
        // Central-third window is x,y in 2..4 → one R, two G, one B.
        let global = (2000.0 + 4000.0 + 4000.0 + 1000.0) / 4.0;
        assert_eq!(global, 2750.0, "fixture's global constant");
        assert!((outcome.flat_norm_divisor - global).abs() < 1e-9);

        let off_r = expect_px(1000.0, None, Some(2000.0), global);
        let off_g = expect_px(2000.0, None, Some(4000.0), global);
        let off_b = expect_px(500.0, None, Some(1000.0), global);
        for y in 0..h {
            for x in 0..w {
                let want = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => off_r,
                    (false, false) => off_b,
                    _ => off_g,
                };
                assert_eq!(data[y * w + x], want, "global pixel ({x},{y})");
            }
        }
        // …and this is what that costs: the flat's colour has white-balanced the
        // light's away — all three channels land on one value.
        assert_eq!(off_r, off_g, "global scaling flattens R and G together");
        assert_eq!(off_r, off_b, "global scaling flattens R and B together");
        assert_eq!(off_r, expect_px(1000.0, None, Some(2000.0), 2750.0));
        drop(dir);
    }

    /// A master flat as `build_master_cards` writes one: the per-channel
    /// constants ride WITH the `BAYERPAT` (+ offsets) they were measured under,
    /// because that phase is what makes them interpretable.
    fn master_flat_cards(pattern: &str, xoff: Option<i64>, yoff: Option<i64>, k: [f64; 3]) -> Vec<Card> {
        let mut cards = vec![
            Card::new("BAYERPAT", CardValue::Str(pattern.into())).unwrap(),
            Card::new("ATH_FNRM", CardValue::Real(2750.0)).unwrap(),
            Card::new("ATH_FNR", CardValue::Real(k[0])).unwrap(),
            Card::new("ATH_FNG", CardValue::Real(k[1])).unwrap(),
            Card::new("ATH_FNB", CardValue::Real(k[2])).unwrap(),
        ];
        // Offsets are stamped only when the members declared them — an absent
        // card means the build assumed 0, exactly as this reader does.
        if let Some(x) = xoff {
            cards.push(Card::new("XBAYROFF", CardValue::Integer(x)).unwrap());
        }
        if let Some(y) = yoff {
            cards.push(Card::new("YBAYROFF", CardValue::Integer(y)).unwrap());
        }
        cards
    }

    /// The stamped `ATH_FNR`/`ATH_FNG`/`ATH_FNB` cards win over recomputation,
    /// exactly as `ATH_FNRM` does for the global constant. Cards deliberately
    /// disagree with the pixels (10x off) so precedence is visible.
    #[test]
    fn per_channel_constants_read_from_the_master_cards_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = master_flat_cards("RGGB", Some(0), Some(0), [200.0, 400.0, 100.0]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert!(outcome.cfa_scaling_applied);
        assert_eq!(
            outcome.flat_channel_divisors,
            Some([200.0, 400.0, 100.0]),
            "the stamped cards must win over recomputation"
        );
        let (_, _, data) = read_all(&out, dir.path());
        // R pixel: 1000 / (2000/200) = 100, i.e. 10x the recomputed answer.
        assert_eq!(data[0], expect_px(1000.0, None, Some(2000.0), 200.0));
    }

    /// The stamped constants are only meaningful under the phase they were
    /// MEASURED in. The flat's cards say RGGB at phase (0,0); this light
    /// declares xoff=1 — so index 0 of the triple is the flat's R while pixel
    /// (0,0) of the light is a G. Using the cards anyway would divide every
    /// channel by another channel's level and report success. The engine must
    /// fall through to recomputation, which measures the flat's own pixels under
    /// the LIGHT's geometry and is therefore self-consistent by construction.
    #[test]
    fn card_constants_rejected_when_flat_phase_disagrees_with_the_light() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = master_flat_cards("RGGB", Some(0), Some(0), [200.0, 400.0, 100.0]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 1, // one column out of phase with the flat's cards
            yoff: 0,
        });

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert!(outcome.cfa_scaling_applied, "still per-channel, just not from the cards");
        // Recomputed under xoff=1 over the painted 6x6 mosaic: the window's
        // pixels relabel to G, R, B, G, giving [4000, 1500, 4000] — nothing like
        // the stamped [200, 400, 100], so the assertion cannot pass by accident.
        let k = outcome.flat_channel_divisors.expect("per-channel constants");
        assert_eq!(k, [4000.0, 1500.0, 4000.0], "must be recomputed under the light's phase");
        assert_ne!(k, [200.0, 400.0, 100.0], "the disagreeing cards must not be used");

        // A pattern mismatch is rejected for the same reason, phase aside.
        let mut cfg2 = LightCalInputs {
            output_path: dir.path().join("out2.fits"),
            ..cfg
        };
        cfg2.cfa_geometry = Some(CfaGeometry {
            pattern: Bayer::Bggr,
            xoff: 0,
            yoff: 0,
        });
        let outcome2 = calibrate_light(&cfg2, &AtomicBool::new(false)).unwrap();
        assert_ne!(
            outcome2.flat_channel_divisors,
            Some([200.0, 400.0, 100.0]),
            "a different pattern must not read the flat's cards either"
        );
    }

    /// An offset that differs only by a multiple of 2 is the SAME phase —
    /// `cfa_channel_at` folds offsets modulo 2 — so it must not be treated as a
    /// disagreement and must keep using the stamped cards.
    #[test]
    fn offsets_congruent_modulo_two_still_use_the_cards() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = master_flat_cards("RGGB", Some(0), Some(0), [200.0, 400.0, 100.0]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(CfaGeometry {
            pattern: Bayer::Rggb,
            xoff: 2,
            yoff: -2,
        });

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            outcome.flat_channel_divisors,
            Some([200.0, 400.0, 100.0]),
            "phase (2,-2) is phase (0,0) — the cards stay usable"
        );
    }

    /// A flat carrying the constants but NO `BAYERPAT` cannot have its phase
    /// verified, so the constants are unusable and the engine recomputes.
    /// (`build_master_cards` never emits that shape — both come out of the same
    /// parse — so this guards a hand-edited or foreign header.)
    #[test]
    fn card_constants_without_a_pattern_are_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let cards = [
            Card::new("ATH_FNRM", CardValue::Real(2750.0)).unwrap(),
            Card::new("ATH_FNR", CardValue::Real(200.0)).unwrap(),
            Card::new("ATH_FNG", CardValue::Real(400.0)).unwrap(),
            Card::new("ATH_FNB", CardValue::Real(100.0)).unwrap(),
        ];
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &cards);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            outcome.flat_channel_divisors,
            Some([2000.0, 4000.0, 1000.0]),
            "unverifiable constants must be recomputed, not trusted"
        );
    }

    /// A mono light (no CFA geometry) is untouched by the toggle: same output,
    /// same row flag, whatever `cfa_flat_scaling` says.
    #[test]
    fn mono_light_is_unaffected_by_the_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1000.0, &[]);
        let flat = write_plane(dir.path(), "flat.fits", w, h, 2000.0, &[]);

        let mut outputs = Vec::new();
        for (i, on) in [true, false].into_iter().enumerate() {
            let out = dir.path().join(format!("out{i}.fits"));
            let mut cfg = inputs(
                dir.path(),
                light.clone(),
                None,
                None,
                Some(flat.clone()),
                true,
                out.clone(),
            );
            cfg.params.cfa_flat_scaling = on;
            cfg.cfa_geometry = None; // mono: no pattern declared
            let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
            assert!(!outcome.cfa_scaling_applied, "mono can never scale per channel");
            assert!(outcome.flat_channel_divisors.is_none());
            let (_, _, data) = read_all(&out, dir.path());
            outputs.push(data);
        }
        assert_eq!(outputs[0], outputs[1], "mono output must not depend on the toggle");
        assert_eq!(outputs[0][0], expect_px(1000.0, None, Some(2000.0), 2000.0));
    }

    /// `TrimmedMeanWhole` is a whole-frame parity statistic — per-channel
    /// scaling is ignored there, so the output matches the toggle-off run
    /// bit-for-bit and the row records that nothing per-channel was applied.
    #[test]
    fn trimmed_mean_whole_mode_ignores_per_channel_scaling() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &[]);

        let mut outputs = Vec::new();
        for (i, on) in [true, false].into_iter().enumerate() {
            let out = dir.path().join(format!("out{i}.fits"));
            let mut cfg = inputs(
                dir.path(),
                light.clone(),
                None,
                None,
                Some(flat.clone()),
                true,
                out.clone(),
            );
            cfg.flat_norm_mode = FlatNormMode::TrimmedMeanWhole;
            cfg.params.cfa_flat_scaling = on;
            cfg.cfa_geometry = Some(rggb_geom());
            let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
            assert!(
                !outcome.cfa_scaling_applied,
                "TrimmedMeanWhole must stay whole-frame (toggle = {on})"
            );
            let (_, _, data) = read_all(&out, dir.path());
            outputs.push(data);
        }
        assert_eq!(outputs[0], outputs[1], "the toggle must not change trimmed-mean-mode output");
    }

    /// A degenerate channel constant (the flat has no pixels of one colour at
    /// any usable level) must not divide a whole channel by garbage: the frame
    /// falls back to the GLOBAL constant as a whole — never mixed-mode, where
    /// two channels would be per-channel-scaled and the third not.
    #[test]
    fn degenerate_channel_falls_back_to_global_for_the_whole_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (6usize, 6usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        // B is 0.0 everywhere → its channel mean is 0.0, unusable as a divisor.
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 0.0), &[]);
        let out = dir.path().join("out.fits");
        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light(&cfg, &AtomicBool::new(false)).unwrap();
        assert!(!outcome.cfa_scaling_applied, "a degenerate channel disables per-channel scaling");
        assert!(outcome.flat_channel_divisors.is_none());
        // Global constant over the central third: one R (2000), two G (4000),
        // one B (0) → 2500.
        assert!((outcome.flat_norm_divisor - 2500.0).abs() < 1e-9, "{}", outcome.flat_norm_divisor);
        let (_, _, data) = read_all(&out, dir.path());
        // The healthy R channel is scaled globally too — not per-channel.
        assert_eq!(data[0], expect_px(1000.0, None, Some(2000.0), 2500.0));
    }

    /// The band loop indexes pixels within a band, but the CFA phase is a
    /// property of the frame's GLOBAL row. A band whose height is odd shifts
    /// every following band's phase by one row, so a band-local `y` swaps R and
    /// B from band 2 onward. Forced multi-band here via a tall frame and a tiny
    /// budget-equivalent, and asserted against the same painted expectation the
    /// single-band test uses.
    #[test]
    fn multi_band_run_keeps_the_global_cfa_row_phase() {
        // A frame tall enough that the engine's real budget still yields one
        // band would prove nothing, so drive the band size directly.
        let dir = tempfile::tempdir().unwrap();
        // 6 wide, not 4: the central-third window of a 4-wide frame is a single
        // column, which contains no R pixel at all — the constants would come
        // back degenerate and the run would (correctly) fall back to global,
        // proving nothing about band phase.
        let (w, h) = (6usize, 12usize);
        let light = write_fill(dir.path(), "light.fits", w, h, rggb_fill(1000.0, 2000.0, 500.0), &[]);
        let flat = write_fill(dir.path(), "flat.fits", w, h, rggb_fill(2000.0, 4000.0, 1000.0), &[]);
        let out = dir.path().join("out.fits");

        // 3 rows per band → 4 bands, and an ODD band height so a band-local row
        // index lands on the wrong phase from band 2 onward. Driven through the
        // budget the same way `calibrate_light_compute_inner` is: per_row =
        // width*4 (light, f32) + width*4 (flat, f32) + width*8 headroom = 96
        // bytes here, so 300 bytes buys exactly 3 rows — this fixture happens to
        // land on the same number the old (n+2)*w*4 formula gave, since both
        // frames here are f32.
        let probe = BandSource::open(&[light.clone(), flat.clone()], dir.path(), 1).unwrap();
        assert_eq!(probe.band_rows_for_budget(300), 3, "fixture must produce 3-row bands");

        let mut cfg = inputs(dir.path(), light, None, None, Some(flat), true, out.clone());
        cfg.cfa_geometry = Some(rggb_geom());

        let outcome = calibrate_light_inner(&cfg, &AtomicBool::new(false), 300).unwrap();
        assert!(outcome.cfa_scaling_applied);
        let (_, _, data) = read_all(&out, dir.path());
        let on_r = expect_px(1000.0, None, Some(2000.0), 2000.0);
        let on_g = expect_px(2000.0, None, Some(4000.0), 4000.0);
        let on_b = expect_px(500.0, None, Some(1000.0), 1000.0);
        for y in 0..h {
            for x in 0..w {
                let want = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => on_r,
                    (false, false) => on_b,
                    _ => on_g,
                };
                assert_eq!(data[y * w + x], want, "pixel ({x},{y}) in band {}", y / 3);
            }
        }
    }

    #[test]
    fn cancel_mid_run_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32usize, 32usize);
        let light = write_plane(dir.path(), "light.fits", w, h, 1100.0, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        let out = dir.path().join("out.fits");
        let cfg = inputs(dir.path(), light, Some(dark), None, None, true, out.clone());

        // Pre-set: the first per-band cancel check trips before any write.
        let cancel = AtomicBool::new(true);
        let r = calibrate_light(&cfg, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)), "expected Cancelled, got {r:?}");
        assert!(!out.exists(), "cancelled run must leave no output file");
    }

    /// The compute/write split is a pure refactor: running the two phases by
    /// hand must produce the SAME file bytes and the SAME outcome as the
    /// one-shot [`calibrate_light`]. Pins the seam a later task inserts
    /// hot-pixel correction and debayering into — if the split ever starts
    /// dropping a step (the flat divisor, the pedestal, the floored counter,
    /// the header cards), this diverges.
    #[test]
    fn compute_then_write_equals_calibrate_light() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 9usize);
        // Full BDF path with a non-trivial flat, a real header card and a
        // pedestal, so every field the split carries across is exercised.
        let light = write_fill(dir.path(), "light.fits", w, h, |x, y| 1000.0 + (x + y * w) as f32, &[]);
        let dark = write_plane(dir.path(), "dark.fits", w, h, 100.0, &[]);
        // One dead flat pixel so the floored counter is NON-zero: comparing
        // `0 == 0` would pass even if the split dropped the counter entirely.
        // The `ATH_FNRM` card still fixes the divisor at 2.0 (card path, no
        // recompute), so the dead pixel does not move `flat_norm_divisor`.
        let flat = write_fill(
            dir.path(),
            "flat.fits",
            w,
            h,
            |x, y| if (x, y) == (3, 4) { 0.0 } else { 2.0 },
            &[fnrm_card(2.0)],
        );
        let cards = vec![Card::new("OBJECT", CardValue::Str("M42".into())).unwrap()];

        let out_a = dir.path().join("out_a.fits");
        let mut cfg_a = inputs(
            dir.path(),
            light.clone(),
            Some(dark.clone()),
            None,
            Some(flat.clone()),
            true,
            out_a.clone(),
        );
        cfg_a.params.pedestal_dn = 50.0;
        cfg_a.cards = cards.clone();
        let outcome_a = calibrate_light(&cfg_a, &AtomicBool::new(false)).unwrap();

        let out_b = dir.path().join("out_b.fits");
        let mut cfg_b = inputs(dir.path(), light, Some(dark), None, Some(flat), true, out_b.clone());
        cfg_b.params.pedestal_dn = 50.0;
        cfg_b.cards = cards;
        let (frame, outcome_b) = calibrate_light_compute(&cfg_b, &AtomicBool::new(false)).unwrap();
        // The compute phase writes nothing and hashes nothing.
        assert!(outcome_b.output_hash.is_empty(), "compute must not hash");
        assert!(!out_b.exists(), "compute must not write");
        assert_eq!((frame.width, frame.height), (w, h));
        assert_eq!(frame.data.len(), w * h);
        let hash_b = write_calibrated_output(
            &out_b,
            frame.width,
            frame.height,
            1,
            &frame.data,
            &cfg_b.cards,
            OutputFormat::Fits,
            Durability::Durable,
        )
        .unwrap();

        assert_eq!(std::fs::read(&out_a).unwrap(), std::fs::read(&out_b).unwrap(), "file bytes differ");
        assert_eq!(outcome_a.output_hash, hash_b, "hash differs");
        assert_eq!(outcome_a.calstat, outcome_b.calstat);
        assert_eq!(outcome_a.flat_norm_divisor, outcome_b.flat_norm_divisor);
        assert_eq!(outcome_a.flat_channel_divisors, outcome_b.flat_channel_divisors);
        assert_eq!(outcome_a.cfa_scaling_applied, outcome_b.cfa_scaling_applied);
        assert_eq!(outcome_a.floored_flat_pixels, outcome_b.floored_flat_pixels);
        assert!(outcome_a.floored_flat_pixels > 0, "fixture must exercise the floor");
    }
}

