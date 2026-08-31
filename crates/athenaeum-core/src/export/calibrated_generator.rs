//! The calibrated-light generator: one place that turns a catalogued LIGHT
//! frame into a written, calibrated FITS file.
//!
//! Two phases, deliberately separated so a batch can hold the catalog
//! connection for a moment and the pixels for minutes:
//!
//! 1. [`resolve_generation`] — catalog phase. Resolves the frame's master
//!    links, decides what actually applies, and builds the output header.
//!    Produces a [`GenerationSpec`], which is plain owned data: a batch can
//!    resolve every frame in one short connection borrow and then drop it.
//! 2. [`execute_generation`] — pixel phase, no database at all. Runs the
//!    calibration formula, the cosmetic hot-pixel pass, the optional OSC
//!    debayer, finalizes the header and writes the file.
//!
//! Nothing here records a tracking row or touches the catalog after phase 1:
//! a calibrated artifact produced for an export or a transfer is a product,
//! not a catalogued file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::calibration_library::cosmetic::{
    apply_hot_pixel_correction, hot_pixel_map_from_dark, HotPixelMap,
};
use crate::calibration_library::light_cal::{
    calibrate_light_compute, resolve_flat_norm_divisor, scale_divisor_for_bitpix,
    write_calibrated_output, BiasFallback, FlatNormDivisor, FlatNormMode, LightCalInputs,
    LightCalParams,
};
use crate::calibration_library::light_headers::{build_light_cal_cards, LightCalCardInputs};
use crate::calibration_library::light_resolve::resolve_frame_inputs;
use crate::fits_writer::keywords::Bayer;
use crate::fits_writer::{Card, CardValue};
use crate::integration::banded::probe_bitpix;
use crate::integration::cfa::CfaGeometry;
use crate::integration::IntegrationError;
use astroimage::processing::vng::vng_debayer_f32;
use astroimage::BayerPattern;

/// Keywords a debayered output must not carry: they describe a mosaic that no
/// longer exists in the file. `ROWORDER` deliberately stays — it describes the
/// row direction of the DATA, which debayering does not change, and a consumer
/// still needs it to display the frame the right way up.
const MOSAIC_KEYWORDS: [&str; 3] = ["BAYERPAT", "XBAYROFF", "YBAYROFF"];

/// serde default for the three ON-by-default toggles below — the value
/// `#[serde(default = "…")]` needs a named function for. `LightCalParams`
/// carries the same helper, for the same reason.
fn default_true() -> bool {
    true
}

/// Everything a run chooses about how its lights are calibrated. One value per
/// run (an export, a transfer preparation), shared by every frame in it.
///
/// `hot_pixel_correction` and `debayer_osc` are the two stages this generator
/// adds on top of the calibration formula; both default ON, and both degrade
/// silently to "not applicable" rather than failing — a frame with no dark
/// master gets no cosmetic pass, a mono frame is never debayered.
///
/// **Every field is optional on the wire** (`#[serde(default)]`, mirroring
/// [`LightCalParams`]), each defaulting to the recommended behavior. A host
/// command that knows only some of these — or a payload written before a field
/// existed — decodes the rest to [`CalibratedLightOptions::default`] instead of
/// failing the whole request; `{}` is a valid, fully-defaulted payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibratedLightOptions {
    /// Normalize the master flat by its own level before dividing (spec §2).
    #[serde(default = "default_true")]
    pub flat_norm: bool,
    /// Which statistic computes that normalization constant. Plain
    /// `#[serde(default)]` resolves through [`FlatNormMode::default`]
    /// (`CentralThird`), so this tracks the enum's own default instead of
    /// restating it here.
    #[serde(default)]
    pub flat_norm_mode: FlatNormMode,
    /// Advanced per-run parameters (pedestal, trim fraction, bias fallback,
    /// per-CFA-channel flat scaling). Omitting it wholesale is the same as
    /// sending `{}` — every one of ITS fields defaults too.
    #[serde(default)]
    pub params: LightCalParams,
    /// Replace the master dark's hot pixels with a neighbourhood median.
    #[serde(default = "default_true")]
    pub hot_pixel_correction: bool,
    /// Debayer a CFA light to full-resolution planar RGB. Ignored for mono
    /// frames and for a `BAYERPAT` the catalog cannot vouch for.
    #[serde(default = "default_true")]
    pub debayer_osc: bool,
}

impl Default for CalibratedLightOptions {
    fn default() -> Self {
        Self {
            flat_norm: true,
            flat_norm_mode: FlatNormMode::CentralThird,
            params: LightCalParams::default(),
            hot_pixel_correction: true,
            debayer_osc: true,
        }
    }
}

/// What one generated artifact turned out to be. Everything a caller needs to
/// record the file in a manifest or an export result — and nothing that would
/// require re-reading it.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedLight {
    /// Applied-state flags as the engine computed them (`"BDF"`, `"BD"`, …).
    pub calstat: String,
    /// Whether the output is planar RGB rather than the source mosaic.
    pub debayered: bool,
    /// How many pixels the cosmetic pass replaced. `0` also when the pass ran
    /// and found nothing, which is the honest answer — see
    /// [`crate::calibration_library::cosmetic`] on why a degenerate dark
    /// yields an empty map instead of an error.
    pub hot_pixels_replaced: u64,
    /// xxh3 of the written file.
    pub output_hash: String,
    /// Size of the written file, measured on disk after the write.
    pub byte_size: u64,
}

/// One frame's resolved plan: everything [`execute_generation`] needs, with no
/// database left in it.
///
/// **Two fields are deliberately left empty in `inputs`.** `output_path`,
/// because the destination belongs to the caller and is passed to
/// [`execute_generation`] directly; and `cards`, because the header written
/// here is the FINALIZED list (this module strips mosaic keywords and appends
/// its own provenance), built from [`GenerationSpec::cards`] at write time.
/// So `inputs` is the input of the *compute* phase only — do not hand it to
/// [`crate::calibration_library::light_cal::calibrate_light`], which would
/// write a header-less file to an empty path.
///
/// `cfa_geometry` and `dark_path` mirror the same values inside `inputs`: they
/// are set once, from one resolution, and kept alongside so the post-compute
/// stages read them from the plan rather than reaching through the engine's
/// input struct.
pub struct GenerationSpec {
    /// Compute-phase inputs (see the caveat above about `output_path`/`cards`).
    pub inputs: LightCalInputs,
    /// The output header BEFORE this module's own finalization.
    pub cards: Vec<Card>,
    /// The light's mosaic phase, when it declares one the catalog can vouch
    /// for. Drives both the cosmetic pass's neighbourhood and the debayer.
    pub cfa_geometry: Option<CfaGeometry>,
    /// The master dark actually applied — the ONLY source of a hot-pixel map.
    pub dark_path: Option<PathBuf>,
    /// `true` iff this frame will be debayered: the run asked for it AND the
    /// frame has a usable mosaic. Decided here so the output NAME (`_d`) and
    /// the output CONTENT can never disagree — Task 8 places the file by name
    /// long before the pixels exist.
    pub debayer: bool,
}

impl GenerationSpec {
    /// This frame's output filename. Takes the source filename rather than
    /// reading it back off the plan so a caller placing a frame it already
    /// holds (the export's `ExportFrame`) uses one shared spelling.
    pub fn output_filename(&self, source_filename: &str) -> String {
        calibrated_output_filename(source_filename, self.debayer)
    }
}

/// `c_<stem>.fits`, or `c_<stem>_d.fits` for a debayered output — the ONE
/// place that spelling is defined. The export path names files before the
/// pixels are generated and the generator names them at write time; a second
/// implementation of this rule would let those two drift.
///
/// The extension is always forced to `.fits`: an XISF source yields a FITS
/// output.
pub fn calibrated_output_filename(source_filename: &str, debayer: bool) -> String {
    let stem = Path::new(source_filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(source_filename);
    if debayer {
        format!("c_{stem}_d.fits")
    } else {
        format!("c_{stem}.fits")
    }
}

/// CALSTAT computed from which masters actually apply — must match the
/// engine's own construction: a dark subtraction removes bias and dark in one
/// go (`"BD"`), else a bias subtraction (`"B"`), plus `"F"` when a flat
/// divides. Predicted here because the header is built before the engine runs.
fn compute_calstat(dark: bool, bias: bool, flat: bool) -> String {
    let base = if dark {
        "BD"
    } else if bias {
        "B"
    } else {
        ""
    };
    if flat {
        format!("{base}F")
    } else {
        base.to_string()
    }
}

/// The mosaic as the debayer must see it: a pattern with the frame's
/// `XBAYROFF`/`YBAYROFF` phase already folded in.
///
/// The debayer takes a pattern and nothing else — it reads the tile at
/// `(x & 1, y & 1)`, i.e. it assumes phase zero. Our geometry carries offsets,
/// and an ODD offset is not a detail: shifting RGGB one column right lands G
/// where R was, which is the pattern GRBG. Ignoring the offsets would hand the
/// debayer a mosaic one step out of phase and swap red with blue.
///
/// Both shifts are their own inverse (shifting twice returns the original), so
/// each is written as a pairing rather than a table of sixteen entries; the
/// unit test cross-checks all four patterns against
/// [`crate::integration::cfa::cfa_channel_at`], the codebase's own definition
/// of where each colour sits.
fn bayer_for(geom: CfaGeometry) -> BayerPattern {
    // A column shift swaps the two columns of the 2x2 tile...
    let shifted = if geom.xoff.rem_euclid(2) == 1 {
        match geom.pattern {
            Bayer::Rggb => Bayer::Grbg,
            Bayer::Grbg => Bayer::Rggb,
            Bayer::Bggr => Bayer::Gbrg,
            Bayer::Gbrg => Bayer::Bggr,
        }
    } else {
        geom.pattern
    };
    // ...and a row shift swaps its two rows.
    let shifted = if geom.yoff.rem_euclid(2) == 1 {
        match shifted {
            Bayer::Rggb => Bayer::Gbrg,
            Bayer::Gbrg => Bayer::Rggb,
            Bayer::Bggr => Bayer::Grbg,
            Bayer::Grbg => Bayer::Bggr,
        }
    } else {
        shifted
    };
    match shifted {
        Bayer::Rggb => BayerPattern::Rggb,
        Bayer::Bggr => BayerPattern::Bggr,
        Bayer::Gbrg => BayerPattern::Gbrg,
        Bayer::Grbg => BayerPattern::Grbg,
    }
}

/// Catalog phase: resolve ONE light frame's calibration links into a complete
/// plan, including the output header.
///
/// Ported from the light-calibration worker's per-frame section, behaviour for
/// behaviour: the "nothing linked" refusal, the dark-over-bias decision, the
/// `bias_fallback` policy, the CALSTAT prediction, and the flat-norm divisor
/// resolved a second time so the stamped `ATH_CFNM`/`ATH_CFN[RGB]` cards and
/// the value the engine divides by are the same number.
///
/// `scratch_dir` is the run's spill directory: resolving the flat's divisor
/// reads the flat (its `ATH_FNRM` card, or its pixels when the card is absent
/// or the run scales per CFA channel), and a decode-and-spill source needs
/// somewhere to land. Pass the same directory later handed to
/// [`execute_generation`].
pub fn resolve_generation(
    conn: &Connection,
    frame_id: i64,
    opts: &CalibratedLightOptions,
    scratch_dir: &Path,
) -> anyhow::Result<GenerationSpec> {
    let resolved = resolve_frame_inputs(conn, frame_id, opts.flat_norm)?;

    if resolved.dark.is_none() && resolved.flat.is_none() && resolved.bias.is_none() {
        anyhow::bail!("no calibration masters available (dark/flat/bias unbuilt or unlinked)");
    }

    let dark_applied = resolved.dark.is_some();
    let bias_applied = resolved.dark.is_none() && resolved.bias.is_some();
    let flat_applied = resolved.flat.is_some();

    if bias_applied && opts.params.bias_fallback == BiasFallback::SkipFrame {
        anyhow::bail!("no dark master (bias fallback disabled)");
    }

    let calstat = compute_calstat(dark_applied, bias_applied, flat_applied);

    let divisor = match (&resolved.flat, opts.flat_norm) {
        (Some(m), true) => resolve_flat_norm_divisor(
            Path::new(&m.path),
            scratch_dir,
            opts.flat_norm_mode,
            &opts.params,
            resolved.cfa_geometry,
        )?,
        _ => FlatNormDivisor::Global(1.0),
    };
    let flat_norm_divisor = divisor.global_value();

    let scale_divisor = scale_divisor_for_bitpix(probe_bitpix(&resolved.light_path));

    let card_inputs = LightCalCardInputs {
        source_uuid: resolved.source_uuid.clone().unwrap_or_default(),
        source_filename: resolved.source_filename.clone(),
        calstat,
        dark: if dark_applied {
            resolved
                .dark
                .as_ref()
                .map(|m| (m.uuid.clone(), m.path.clone()))
        } else {
            None
        },
        flat: if flat_applied {
            resolved
                .flat
                .as_ref()
                .map(|m| (m.uuid.clone(), m.path.clone()))
        } else {
            None
        },
        bias: if bias_applied {
            resolved
                .bias
                .as_ref()
                .map(|m| (m.uuid.clone(), m.path.clone()))
        } else {
            None
        },
        scale_divisor,
        flat_norm_divisor,
        flat_channel_divisors: divisor.channel_values(),
        pedestal_dn: opts.params.pedestal_dn,
        trim_fraction: if flat_applied
            && opts.flat_norm
            && opts.flat_norm_mode == FlatNormMode::PixinsightTrimmed
        {
            Some(opts.params.trim_fraction)
        } else {
            None
        },
    };
    let cards = build_light_cal_cards(&resolved.source_cards, &card_inputs)?;

    let inputs = LightCalInputs {
        light_path: resolved.light_path.clone(),
        dark_path: resolved.dark.as_ref().map(|m| PathBuf::from(&m.path)),
        bias_path: resolved.bias.as_ref().map(|m| PathBuf::from(&m.path)),
        flat_path: resolved.flat.as_ref().map(|m| PathBuf::from(&m.path)),
        flat_norm: opts.flat_norm,
        flat_norm_mode: opts.flat_norm_mode,
        cfa_geometry: resolved.cfa_geometry,
        params: opts.params,
        scale_divisor,
        output_path: PathBuf::new(),
        cards: Vec::new(),
        scratch_dir: scratch_dir.to_path_buf(),
    };

    Ok(GenerationSpec {
        dark_path: inputs.dark_path.clone(),
        cfa_geometry: resolved.cfa_geometry,
        debayer: opts.debayer_osc && resolved.cfa_geometry.is_some(),
        inputs,
        cards,
    })
}

/// Pixel phase: calibrate, repair, optionally debayer, write. No database.
///
/// Stage order is not arbitrary. The cosmetic pass runs on the CALIBRATED
/// frame (a hot pixel survives the subtraction as a wrong value, not as a
/// missing one) and BEFORE the debayer, because a mosaic pixel can only be
/// replaced from same-colour neighbours — after interpolation the defect has
/// already been smeared across three planes.
///
/// `hot_maps` caches one map per master dark for the caller's whole batch:
/// the map depends on the dark alone, and measuring it costs a full plane
/// read plus two sorts, so a set sharing one dark pays that once.
///
/// The write is atomic (temp file + rename), so re-generating over an existing
/// output replaces it in place rather than leaving a truncated file behind.
///
/// Cancellation is cooperative: per band inside the formula, and once more
/// before the debayer, which is the most expensive stage and the one worth not
/// entering. The error carries [`IntegrationError::Cancelled`], so a caller can
/// tell a cancel from a failure by downcasting.
pub fn execute_generation(
    spec: &GenerationSpec,
    output_path: &Path,
    scratch_dir: &Path,
    opts: &CalibratedLightOptions,
    hot_maps: &mut HashMap<PathBuf, Arc<HotPixelMap>>,
    cancel: &AtomicBool,
) -> anyhow::Result<GeneratedLight> {
    let (mut frame, outcome) = calibrate_light_compute(&spec.inputs, cancel)?;

    // ── Cosmetic hot-pixel correction ───────────────────────────────────────
    // Only a master dark can say which pixels are defective, so a frame with
    // no dark applied is honestly skipped rather than guessed at.
    let mut hot_pixels_replaced = 0u64;
    let corrected = match (opts.hot_pixel_correction, &spec.dark_path) {
        (true, Some(dark)) => {
            let map = match hot_maps.get(dark) {
                Some(cached) => Arc::clone(cached),
                None => {
                    // A failure here fails the frame instead of degrading
                    // quietly: the output would otherwise be uncorrected while
                    // its ATH_CHPX card claimed "0 defects found".
                    let built = Arc::new(hot_pixel_map_from_dark(dark, scratch_dir)?);
                    hot_maps.insert(dark.clone(), Arc::clone(&built));
                    built
                }
            };
            hot_pixels_replaced = apply_hot_pixel_correction(
                &mut frame.data,
                frame.width,
                frame.height,
                &map,
                spec.cfa_geometry,
            );
            true
        }
        _ => false,
    };

    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled.into());
    }

    // ── Optional OSC debayer ────────────────────────────────────────────────
    // Full resolution: same width and height, three planes. `spec.debayer`
    // already implies a usable geometry; the fallback arm cannot fire, and if
    // it ever did it would leave the mosaic intact rather than guess a phase.
    let (data, channels) = match (spec.debayer, spec.cfa_geometry) {
        (true, Some(geom)) => (
            vng_debayer_f32(&frame.data, frame.width, frame.height, bayer_for(geom)),
            3usize,
        ),
        _ => (frame.data, 1usize),
    };
    let debayered = channels == 3;

    // ── Final header ────────────────────────────────────────────────────────
    let mut cards = spec.cards.clone();
    if debayered {
        cards.retain(|c| !MOSAIC_KEYWORDS.contains(&c.keyword.as_str()));
        cards.push(Card::new("ATH_CDBM", CardValue::Str("VNG".into()))?);
    }
    if corrected {
        cards.push(
            Card::new("ATH_CHPX", CardValue::Integer(hot_pixels_replaced as i64))?
                .with_comment("hot pixels replaced"),
        );
    }

    // The write stages a sibling temp file and renames it into place, so the
    // destination's directory has to exist before it starts — and callers
    // place these files into a hierarchy they build as they go.
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output_hash = write_calibrated_output(
        output_path,
        frame.width,
        frame.height,
        channels,
        &data,
        &cards,
    )?;
    let byte_size = std::fs::metadata(output_path)?.len();

    tracing::debug!(
        src = %spec.inputs.light_path.display(),
        dest = %output_path.display(),
        calstat = %outcome.calstat,
        flat_norm_divisor = outcome.flat_norm_divisor,
        cfa_scaling_applied = outcome.cfa_scaling_applied,
        width = frame.width,
        height = frame.height,
        floored_flat_pixels = outcome.floored_flat_pixels,
        "light calibrated"
    );

    Ok(GeneratedLight {
        calstat: outcome.calstat,
        debayered,
        hot_pixels_replaced,
        output_hash,
        byte_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::fits_parser::FitsHeader;
    use crate::fits_writer::write_fits_f32;
    use crate::integration::cfa::cfa_channel_at;
    use rusqlite::params;

    const W: usize = 16;
    const H: usize = 16;

    fn seed_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// A 32-bit-float FITS plane at `path`, filled by `fill(x, y)`.
    fn write_plane(path: &Path, fill: impl Fn(usize, usize) -> f32) -> PathBuf {
        let mut data = vec![0f32; W * H];
        for y in 0..H {
            for x in 0..W {
                data[y * W + x] = fill(x, y);
            }
        }
        write_fits_f32(path, W, H, 1, &data, &[]).unwrap();
        path.to_path_buf()
    }

    /// A master dark with a measurable spread (alternating 300/302) and two
    /// genuine spikes, so [`hot_pixel_map_from_dark`] produces a real map: a
    /// dark with zero MAD is refused whole by design, and a uniform fixture
    /// would test nothing but that refusal.
    fn spiky_dark(x: usize, y: usize) -> f32 {
        if (x, y) == (5, 5) || (x, y) == (9, 9) {
            5000.0
        } else if (x + y) % 2 == 0 {
            300.0
        } else {
            302.0
        }
    }

    /// RGGB mosaic light: bright at the R sites, dim everywhere else, so a
    /// debayered output's R plane is unmistakably above its B plane.
    fn rggb_light(x: usize, y: usize) -> f32 {
        if x % 2 == 0 && y % 2 == 0 {
            5000.0
        } else {
            1000.0
        }
    }

    fn seed_light(
        conn: &Connection,
        frame_id: i64,
        path: &Path,
        bayerpat: Option<&str>,
        offsets: Option<i64>,
    ) {
        let file_id = frame_id + 2_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                path.to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs,
                                 bayerpat, xbayroff, ybayroff)
             VALUES (?1, ?2, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', ?3, ?4, ?5)",
            params![frame_id, file_id, bayerpat, offsets, offsets],
        )
        .unwrap();
    }

    /// A MASTER calibration set whose single member file is a real FITS.
    fn seed_master_set(conn: &Connection, set_id: i64, imagetyp: &str, path: &Path) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', 1)",
            params![set_id, imagetyp],
        )
        .unwrap();
        let file_id = set_id + 3_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                path.to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
            ],
        )
        .unwrap();
        let frame_id = set_id + 4_000_000;
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?2, ?3, 1)",
            params![frame_id, file_id, imagetyp],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )
        .unwrap();
    }

    fn add_link(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'frame', ?2, ?3, '2026-07-05T00:00:00Z')",
            params![frame_id, set_id, cal_type],
        )
        .unwrap();
    }

    /// Read back a file this module wrote: `(width, height, channels, data)`.
    /// Deliberately a raw byte read rather than a library reader — the point is
    /// to assert on what actually landed on disk, in the order it landed.
    fn read_written(path: &Path) -> (usize, usize, usize, Vec<f32>) {
        let bytes = std::fs::read(path).unwrap();
        let (mut w, mut h, mut ch) = (0usize, 0usize, 1usize);
        let mut data_start = None;
        let mut i = 0usize;
        while i + 80 <= bytes.len() {
            let card = &bytes[i..i + 80];
            let key = String::from_utf8_lossy(&card[..8]).trim().to_string();
            let value = String::from_utf8_lossy(&card[10..80]);
            let value = value.split('/').next().unwrap_or("").trim().to_string();
            match key.as_str() {
                "NAXIS1" => w = value.parse().unwrap(),
                "NAXIS2" => h = value.parse().unwrap(),
                "NAXIS3" => ch = value.parse().unwrap(),
                "END" => {
                    data_start = Some((i / 2880 + 1) * 2880);
                    break;
                }
                _ => {}
            }
            i += 80;
        }
        let start = data_start.expect("END card");
        let n = w * h * ch;
        let data = (0..n)
            .map(|k| {
                let o = start + k * 4;
                f32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
            })
            .collect();
        (w, h, ch, data)
    }

    fn plane_mean(data: &[f32], plane: usize) -> f64 {
        let n = W * H;
        data[plane * n..(plane + 1) * n]
            .iter()
            .map(|v| *v as f64)
            .sum::<f64>()
            / n as f64
    }

    /// An OSC catalog: one RGGB light, a spiky master dark, a flat master.
    /// Returns `(conn, light filename)`.
    fn seed_osc(dir: &Path) -> (Connection, String) {
        let light = write_plane(&dir.join("light_b.fits"), rggb_light);
        let dark = write_plane(&dir.join("dark.fits"), spiky_dark);
        let flat = write_plane(&dir.join("flat.fits"), |_, _| 2000.0);
        let conn = seed_db();
        seed_light(&conn, 1, &light, Some("RGGB"), Some(0));
        seed_master_set(&conn, 10, "Dark", &dark);
        seed_master_set(&conn, 11, "Flat", &flat);
        add_link(&conn, 1, 10, "Dark");
        add_link(&conn, 1, 11, "Flat");
        (conn, "light_b.fits".to_string())
    }

    #[test]
    fn mono_generation_produces_c_fits_with_calstat() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(&dir.path().join("light_a.fits"), |_, _| 1000.0);
        let dark = write_plane(&dir.path().join("dark.fits"), spiky_dark);
        let conn = seed_db();
        seed_light(&conn, 1, &light, None, None);
        seed_master_set(&conn, 10, "Dark", &dark);
        add_link(&conn, 1, 10, "Dark");

        let opts = CalibratedLightOptions::default();
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        assert!(!spec.debayer, "a mono light is never debayered");
        assert_eq!(spec.output_filename("light_a.fits"), "c_light_a.fits");

        // Into a directory that does not exist yet: callers place these files
        // into a hierarchy they are still building.
        let out = dir
            .path()
            .join("wbpp/M31/lights")
            .join(spec.output_filename("light_a.fits"));
        let mut hot_maps = HashMap::new();
        let generated = execute_generation(
            &spec,
            &out,
            dir.path(),
            &opts,
            &mut hot_maps,
            &AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(generated.calstat, "BD");
        assert!(!generated.debayered);
        assert_eq!(generated.hot_pixels_replaced, 2);
        assert!(!generated.output_hash.is_empty());
        assert_eq!(
            generated.byte_size,
            std::fs::metadata(&out).unwrap().len(),
            "byte_size must be the written file's own size"
        );

        let header = FitsHeader::from_path(&out).unwrap();
        assert_eq!(header.get_str("CALSTAT").as_deref(), Some("BD"));
        assert_eq!(header.get_i32("NAXIS"), Some(2));
        assert_eq!(
            header.get_i32("NAXIS3"),
            None,
            "mono output has no 3rd axis"
        );
        assert_eq!(header.get_i32("ATH_CHPX"), Some(2));

        let (w, h, ch, data) = read_written(&out);
        assert_eq!((w, h, ch), (W, H, 1));
        // A float source is never re-scaled, so the pixel is the plain
        // difference: 1000 − 300.
        assert!((data[0] - 700.0).abs() < 1e-3, "got {}", data[0]);
        // The dark's spike at (5,5) would leave −4000 here; the cosmetic pass
        // replaces it with the neighbourhood median.
        assert!(
            data[5 * W + 5] > 600.0,
            "hot pixel not repaired: {}",
            data[5 * W + 5]
        );
    }

    #[test]
    fn osc_generation_debayers_to_3_planes() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, filename) = seed_osc(dir.path());

        let opts = CalibratedLightOptions::default();
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        assert!(spec.debayer);
        assert_eq!(spec.output_filename(&filename), "c_light_b_d.fits");

        let out = dir.path().join(spec.output_filename(&filename));
        let mut hot_maps = HashMap::new();
        let generated = execute_generation(
            &spec,
            &out,
            dir.path(),
            &opts,
            &mut hot_maps,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(generated.debayered);
        assert_eq!(generated.calstat, "BDF");

        let header = FitsHeader::from_path(&out).unwrap();
        assert_eq!(header.get_i32("NAXIS3"), Some(3));
        assert_eq!(
            header.get_str("BAYERPAT"),
            None,
            "a debayered output no longer carries a mosaic"
        );
        assert_eq!(header.get_i32("XBAYROFF"), None);
        assert_eq!(header.get_i32("YBAYROFF"), None);
        assert_eq!(header.get_str("ATH_CDBM").as_deref(), Some("VNG"));

        let (w, h, ch, data) = read_written(&out);
        assert_eq!((w, h, ch), (W, H, 3), "full resolution, three planes");
        // The mosaic put the signal on the R sites; the R plane must carry it
        // and the B plane must not — this is what pins our pattern mapping
        // against the debayer's own convention.
        let (r, b) = (plane_mean(&data, 0), plane_mean(&data, 2));
        assert!(r > 3000.0, "R plane mean {r}");
        assert!(b < 2000.0, "B plane mean {b}");
    }

    #[test]
    fn debayer_off_keeps_cfa() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, filename) = seed_osc(dir.path());

        let opts = CalibratedLightOptions {
            debayer_osc: false,
            ..CalibratedLightOptions::default()
        };
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        assert!(!spec.debayer);
        assert_eq!(spec.output_filename(&filename), "c_light_b.fits");

        let out = dir.path().join(spec.output_filename(&filename));
        let mut hot_maps = HashMap::new();
        let generated = execute_generation(
            &spec,
            &out,
            dir.path(),
            &opts,
            &mut hot_maps,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!generated.debayered);

        let header = FitsHeader::from_path(&out).unwrap();
        assert_eq!(header.get_i32("NAXIS3"), None, "mosaic stays one plane");
        assert_eq!(header.get_str("BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(header.get_i32("XBAYROFF"), Some(0));
        assert_eq!(header.get_str("ATH_CDBM"), None);

        let (_, _, ch, data) = read_written(&out);
        assert_eq!(ch, 1);
        assert_eq!(data.len(), W * H);
    }

    #[test]
    fn hot_map_cached_per_dark() {
        let dir = tempfile::tempdir().unwrap();
        let light_a = write_plane(&dir.path().join("light_a.fits"), |_, _| 1000.0);
        let light_c = write_plane(&dir.path().join("light_c.fits"), |_, _| 1200.0);
        let dark = write_plane(&dir.path().join("dark.fits"), spiky_dark);
        let conn = seed_db();
        seed_light(&conn, 1, &light_a, None, None);
        seed_light(&conn, 2, &light_c, None, None);
        seed_master_set(&conn, 10, "Dark", &dark);
        add_link(&conn, 1, 10, "Dark");
        add_link(&conn, 2, 10, "Dark");

        let opts = CalibratedLightOptions::default();
        let mut hot_maps = HashMap::new();
        for (frame_id, name) in [(1i64, "light_a.fits"), (2, "light_c.fits")] {
            let spec = resolve_generation(&conn, frame_id, &opts, dir.path()).unwrap();
            let out = dir.path().join(spec.output_filename(name));
            let generated = execute_generation(
                &spec,
                &out,
                dir.path(),
                &opts,
                &mut hot_maps,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(generated.hot_pixels_replaced, 2, "{name}");
        }
        assert_eq!(
            hot_maps.len(),
            1,
            "two frames sharing one dark must measure it once"
        );
        assert_eq!(hot_maps.values().next().unwrap().len(), 2);
    }

    /// The pattern handed to the debayer must describe the SAME mosaic the
    /// rest of the codebase reads through [`cfa_channel_at`] — including the
    /// `XBAYROFF`/`YBAYROFF` phase, which the debayer has no parameter for.
    #[test]
    fn bayer_for_folds_offset_parity_into_the_pattern() {
        // The debayer's pattern back to ours, so the comparison below runs in
        // one vocabulary.
        fn back(p: BayerPattern) -> Bayer {
            match p {
                BayerPattern::Rggb => Bayer::Rggb,
                BayerPattern::Bggr => Bayer::Bggr,
                BayerPattern::Gbrg => Bayer::Gbrg,
                BayerPattern::Grbg => Bayer::Grbg,
                BayerPattern::None => panic!("a declared mosaic never maps to None"),
            }
        }
        for pattern in [Bayer::Rggb, Bayer::Bggr, Bayer::Gbrg, Bayer::Grbg] {
            // Both parities, and one even + one negative spelling of each, so
            // the modulo folding is covered too.
            for xoff in [0i64, 1, 2, -1] {
                for yoff in [0i64, 1, 2, -1] {
                    let geom = CfaGeometry {
                        pattern,
                        xoff,
                        yoff,
                    };
                    let phase_free = CfaGeometry {
                        pattern: back(bayer_for(geom)),
                        xoff: 0,
                        yoff: 0,
                    };
                    for y in 0..4 {
                        for x in 0..4 {
                            assert_eq!(
                                cfa_channel_at(x, y, geom),
                                cfa_channel_at(x, y, phase_free),
                                "{pattern:?} xoff {xoff} yoff {yoff} at ({x},{y})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Every field is optional on the wire: a host that knows only some of
    /// these — or a payload written before a field existed — must decode, not
    /// fail. `{}` is the extreme case and has to equal the documented default.
    #[test]
    fn options_decode_from_a_partial_payload() {
        let empty: CalibratedLightOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, CalibratedLightOptions::default());

        let partial: CalibratedLightOptions =
            serde_json::from_str(r#"{"debayerOsc": false}"#).unwrap();
        assert!(!partial.debayer_osc, "the sent field wins");
        assert!(partial.flat_norm, "omitted flat_norm defaults ON");
        assert!(
            partial.hot_pixel_correction,
            "omitted hot_pixel_correction defaults ON"
        );
        assert_eq!(partial.flat_norm_mode, FlatNormMode::CentralThird);
        // An omitted `params` defaults wholesale, its own fields included.
        assert_eq!(partial.params, LightCalParams::default());
        assert_eq!(
            partial,
            CalibratedLightOptions {
                debayer_osc: false,
                ..CalibratedLightOptions::default()
            }
        );

        // The camelCase spelling is the wire contract; round-tripping our own
        // serialization must land back on the same value.
        let round: CalibratedLightOptions =
            serde_json::from_str(&serde_json::to_string(&partial).unwrap()).unwrap();
        assert_eq!(round, partial);
    }
}
