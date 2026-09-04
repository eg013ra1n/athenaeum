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

use crate::calibration_library::cosmetic::{
    apply_hot_pixel_correction, hot_pixel_map_from_dark, HotPixelMapOutcome,
};
use crate::calibration_library::light_cal::{
    calibrate_light_compute, resolve_flat_norm_divisor, scale_divisor_for_bitpix,
    write_calibrated_output, BiasFallback, FlatNormDivisor, FlatNormMode, LightCalInputs,
};
use crate::calibration_library::light_headers::{build_light_cal_cards, LightCalCardInputs};
use crate::calibration_library::light_resolve::resolve_frame_inputs;
use crate::export::models::{calibrated_output_filename, CalibratedLightOptions};
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

/// What one generated artifact turned out to be. Everything a caller needs to
/// record the file in a manifest or an export result — and nothing that would
/// require re-reading it.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedLight {
    /// Applied-state flags as the engine computed them (`"BDF"`, `"BD"`, …).
    pub calstat: String,
    /// Whether the output is planar RGB rather than the source mosaic.
    pub debayered: bool,
    /// How many pixels the cosmetic pass replaced. `0` in THREE different
    /// shapes that this field alone cannot tell apart:
    ///
    /// - the map was measured and genuinely found nothing — the output still
    ///   carries `ATH_CHPX = 0`;
    /// - the master dark was REFUSED (zero MAD, over the safety cap, or
    ///   unreadable — [`crate::calibration_library::cosmetic`]) — no pass ran
    ///   at all, the output carries no `ATH_CHPX` card, and the refusal is
    ///   reported once via `warnings` below instead;
    /// - the pass never ran because there was nothing to run it FROM — hot-
    ///   pixel correction is off in this run's options, or no dark applies to
    ///   this frame at all (`execute_generation`'s `_ => false` arm) — same as
    ///   a refusal, no `ATH_CHPX` card, but silently: there is nothing
    ///   irregular to warn about.
    pub hot_pixels_replaced: u64,
    /// Non-fatal notes the run should show its operator: today, one line the
    /// first time a master dark's hot-pixel map is refused (a degenerate dark,
    /// or one that could not be read at all). Emitted on the MISS of the
    /// caller's `hot_maps` cache only, so a dark shared by 200 lights produces
    /// one line, not 200 — see [`execute_generation`].
    pub warnings: Vec<String>,
    /// The catalog's SAMPLING xxh3 of the written file
    /// (`duplicates::compute_xxhash` — three 512 KB windows), the same digest
    /// the scanner stores for a file. NOT the full-file digest a package
    /// manifest carries: a caller putting this artifact on the wire has to
    /// compute `package::xxh3_full_file` itself, or the receiver's own
    /// verification will reject the payload.
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

/// Cache key for one resolved flat-normalization divisor: the master flat's
/// path plus the LIGHT's mosaic phase, folded the way
/// [`CfaGeometry::same_phase`] folds it — pattern plus offsets MODULO 2,
/// because that is the only part of them the divisor depends on. Two lights
/// declaring `XBAYROFF` 0 and 2 therefore share one entry instead of paying
/// for the same plane read twice.
type DivisorKey = (PathBuf, Option<(&'static str, i64, i64)>);

fn divisor_key(flat_path: &str, cfa: Option<CfaGeometry>) -> DivisorKey {
    (
        PathBuf::from(flat_path),
        cfa.map(|g| {
            (
                g.pattern.as_str(),
                g.xoff.rem_euclid(2),
                g.yoff.rem_euclid(2),
            )
        }),
    )
}

/// One run's memo of resolved flat-normalization divisors.
///
/// [`resolve_flat_norm_divisor`] costs a FULL read of the master flat's plane
/// whenever the constant cannot come off a card — an imported flat with no
/// `ATH_FNRM`/`ATH_FNR[GB]`, or an Athenaeum flat whose stamped phase
/// disagrees with the light's. It is resolved once per LIGHT, so a 200-frame
/// set sharing one such flat used to read that plane 200 times. The divisor
/// depends only on the flat, the light's mosaic phase and the run's options,
/// so within one batch each distinct pair needs computing exactly once.
///
/// **A cache belongs to exactly one [`CalibratedLightOptions`].** The
/// normalization mode, the trim fraction and the per-channel switch are part
/// of what the divisor depends on and are deliberately NOT in the key: a batch
/// resolves every frame under one fixed `opts`, so they are constant for the
/// cache's whole life. Never share a cache across two option sets. A
/// single-frame caller passes a throwaway one and pays nothing.
#[derive(Default)]
pub struct DivisorCache {
    entries: HashMap<DivisorKey, FlatNormDivisor>,
}

impl DivisorCache {
    /// An empty cache. Equivalent to [`Default::default`]; named so a call site
    /// reads as "this batch starts its own memo".
    pub fn new() -> Self {
        Self::default()
    }

    /// How many distinct (flat, phase) pairs have been resolved. The memo's
    /// observable effect — a test asserting the plane was read once asserts on
    /// this rather than on timing.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been resolved yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
///
/// This is the single-frame form: it allocates its own throwaway
/// [`DivisorCache`], so a caller resolving a whole batch should use
/// [`resolve_generation_cached`] instead and keep one cache across the loop.
pub fn resolve_generation(
    conn: &Connection,
    frame_id: i64,
    opts: &CalibratedLightOptions,
    scratch_dir: &Path,
) -> anyhow::Result<GenerationSpec> {
    resolve_generation_cached(conn, frame_id, opts, scratch_dir, &mut DivisorCache::new())
}

/// [`resolve_generation`] with the run's flat-normalization memo threaded
/// through — see [`DivisorCache`] for what it saves and the one invariant it
/// asks for (one cache per `opts`).
pub fn resolve_generation_cached(
    conn: &Connection,
    frame_id: i64,
    opts: &CalibratedLightOptions,
    scratch_dir: &Path,
    divisors: &mut DivisorCache,
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
        (Some(m), true) => {
            // Memoized per (flat, light phase): the miss path can read the
            // whole flat plane, and a set's lights overwhelmingly share one
            // flat and one phase.
            let key = divisor_key(&m.path, resolved.cfa_geometry);
            match divisors.entries.get(&key) {
                Some(d) => *d,
                None => {
                    let d = resolve_flat_norm_divisor(
                        Path::new(&m.path),
                        scratch_dir,
                        opts.flat_norm_mode,
                        &opts.params,
                        resolved.cfa_geometry,
                    )?;
                    divisors.entries.insert(key, d);
                    d
                }
            }
        }
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

/// Every DISTINCT master path a resolved batch actually READS — dark and
/// flat unconditionally, plus bias ONLY for a spec whose `dark_path` is
/// `None` (review fix #1) — unioned across every spec (review fix C-2: a
/// preflight existence check before generation starts). The raw-master-dark
/// convention means the engine never touches the bias plane once a dark
/// applies (`Light − MasterDark` already removes the bias, and `ATH_CBIA` is
/// only written when `bias_applied`), so a bias that is linked but never read
/// must not block a run that would produce byte-identical output either way.
/// Deliberate: readiness (`api::lights::compute_export_readiness`) applies
/// the exact same dark-path-gated rule over the exact same link resolution,
/// so this preflight and that gate must count the same set of files, or one
/// could pass a batch the other would refuse. A frame set shares its masters
/// (one dark covers a whole night), so a batch of specs collapses to a
/// handful of paths worth stat-ing once each, never once per frame.
pub fn resolved_master_paths(
    specs: &HashMap<i64, GenerationSpec>,
) -> std::collections::BTreeSet<PathBuf> {
    let mut paths = std::collections::BTreeSet::new();
    for spec in specs.values() {
        paths.extend(spec.inputs.dark_path.clone());
        paths.extend(spec.inputs.flat_path.clone());
        if spec.inputs.dark_path.is_none() {
            paths.extend(spec.inputs.bias_path.clone());
        }
    }
    paths
}

/// Pixel phase: calibrate, repair, optionally debayer, write. No database.
///
/// Stage order is not arbitrary. The cosmetic pass runs on the CALIBRATED
/// frame (a hot pixel survives the subtraction as a wrong value, not as a
/// missing one) and BEFORE the debayer, because a mosaic pixel can only be
/// replaced from same-colour neighbours — after interpolation the defect has
/// already been smeared across three planes.
///
/// `hot_maps` caches one OUTCOME per master dark for the caller's whole batch:
/// the answer depends on the dark alone, and measuring it costs a full plane
/// read plus two sorts, so a set sharing one dark pays that once. Refusals and
/// read failures are cached too — re-measuring a dark that already said no
/// would cost the same read per frame and warn the operator once per frame.
///
/// A refused map is NOT a correction that found nothing: the output carries no
/// `ATH_CHPX` card (the card would claim a pass that never ran) and the first
/// frame to hit that dark returns the reason in
/// [`GeneratedLight::warnings`] for the run to surface.
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
    hot_maps: &mut HashMap<PathBuf, Arc<HotPixelMapOutcome>>,
    cancel: &AtomicBool,
) -> anyhow::Result<GeneratedLight> {
    let (mut frame, outcome) = calibrate_light_compute(&spec.inputs, cancel)?;

    // ── Cosmetic hot-pixel correction ───────────────────────────────────────
    // Only a master dark can say which pixels are defective, so a frame with
    // no dark applied is honestly skipped rather than guessed at.
    let mut hot_pixels_replaced = 0u64;
    let mut warnings = Vec::new();
    let corrected = match (opts.hot_pixel_correction, &spec.dark_path) {
        (true, Some(dark)) => {
            // `newly_measured` is what keeps the refusal warning to ONE line
            // per dark for the whole batch: only the frame that actually
            // measured it reports it.
            let (hot_outcome, newly_measured) = match hot_maps.get(dark) {
                Some(cached) => (Arc::clone(cached), false),
                None => {
                    // A read failure does not fail the frame: the light is
                    // still calibrated, just without a cosmetic pass. It is
                    // cached as a refusal for the same reason the degenerate
                    // darks are — otherwise every frame in the batch pays the
                    // failed read again and warns the operator again.
                    let measured = match hot_pixel_map_from_dark(dark, scratch_dir) {
                        Ok(o) => o,
                        Err(e) => {
                            tracing::warn!(
                                path = %dark.display(),
                                error = %format!("{e:#}"),
                                "measuring the master dark failed — hot-pixel correction refused"
                            );
                            HotPixelMapOutcome::Refused(format!("measuring it failed: {e:#}"))
                        }
                    };
                    let built = Arc::new(measured);
                    hot_maps.insert(dark.clone(), Arc::clone(&built));
                    (built, true)
                }
            };
            match &*hot_outcome {
                HotPixelMapOutcome::Map(map) => {
                    hot_pixels_replaced = apply_hot_pixel_correction(
                        &mut frame.data,
                        frame.width,
                        frame.height,
                        map,
                        spec.cfa_geometry,
                    );
                    true
                }
                HotPixelMapOutcome::Refused(reason) => {
                    if newly_measured {
                        // Already logged at `warn!` where the refusal was
                        // decided; this is the operator-facing half of it.
                        warnings.push(format!(
                            "Hot-pixel correction skipped for {}: {reason}",
                            dark.display()
                        ));
                    }
                    false
                }
            }
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
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::fits_parser::FitsHeader;
    use crate::fits_writer::write_fits_f32;
    use crate::integration::cfa::cfa_channel_at;
    use crate::models::LightCalParams;
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

    /// A RAW (unbuilt) calibration set with a real member file on disk. The
    /// file exists on purpose: it makes `is_master_library = 0` — and nothing
    /// else — the reason the term is skipped.
    fn seed_raw_set(conn: &Connection, set_id: i64, imagetyp: &str, path: &Path) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', 0)",
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
            "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, ?3)",
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

    /// `BiasFallback::SkipFrame` is user-selectable (persisted by
    /// `lightCalPrefs.ts` into `LightCalParams`), and it is the ONLY thing that
    /// turns a bias-only light into a refusal instead of a `"B"` calibration.
    /// Both arms are pinned here: the policy has no other coverage since the
    /// standalone flow's own suite was removed with it.
    #[test]
    fn skip_frame_policy_refuses_a_bias_only_light() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(&dir.path().join("light_c.fits"), |_, _| 1000.0);
        let bias = write_plane(&dir.path().join("bias.fits"), |_, _| 50.0);
        let conn = seed_db();
        seed_light(&conn, 1, &light, None, None);
        seed_master_set(&conn, 12, "Bias", &bias);
        add_link(&conn, 1, 12, "Bias");

        // skipFrame → resolution refuses, naming the missing dark.
        let skip = CalibratedLightOptions {
            params: LightCalParams {
                bias_fallback: BiasFallback::SkipFrame,
                ..LightCalParams::default()
            },
            ..CalibratedLightOptions::default()
        };
        let err = match resolve_generation(&conn, 1, &skip, dir.path()) {
            Ok(_) => panic!("skipFrame must refuse a light with no dark master"),
            Err(e) => e,
        };
        assert_eq!(
            format!("{err}"),
            "no dark master (bias fallback disabled)",
            "the refusal must name the policy that caused it"
        );

        // subtractBias (the default) → the same light resolves and calibrates
        // to a bias-only CALSTAT.
        let opts = CalibratedLightOptions::default();
        assert_eq!(opts.params.bias_fallback, BiasFallback::SubtractBias);
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        assert!(
            spec.dark_path.is_none(),
            "no dark is linked, so nothing can drive a cosmetic pass"
        );

        let out = dir.path().join("wbpp/M31/lights/c_light_c.fits");
        let generated = execute_generation(
            &spec,
            &out,
            dir.path(),
            &opts,
            &mut HashMap::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            generated.calstat, "B",
            "bias subtracted, no dark and no flat → a bare 'B'"
        );
        assert_eq!(
            generated.hot_pixels_replaced, 0,
            "a hot-pixel map needs a master dark"
        );
        assert!(out.exists());

        let (_, _, _, data) = read_written(&out);
        // Float source, no re-scale: the plain difference 1000 − 50.
        assert!((data[0] - 950.0).abs() < 1e-3, "got {}", data[0]);
    }

    /// The built-master-wins / unbuilt-raw-skipped rule
    /// (`light_resolve::resolve_master`: `Some` only for
    /// `is_master_library = 1`; `resolve_type` treats `None` as "skip this
    /// term"). Pinned through `resolve_frame_inputs`, the one function the
    /// export generator and the transfer preparation both resolve through.
    #[test]
    fn resolution_prefers_a_built_master_and_skips_an_unbuilt_raw_set() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(&dir.path().join("light_d.fits"), |_, _| 1000.0);
        let dark = write_plane(&dir.path().join("dark.fits"), spiky_dark);
        let bias = write_plane(&dir.path().join("bias.fits"), |_, _| 50.0);
        // The raw flat has a member FILE, exactly like the masters — only its
        // `is_master_library` flag differs.
        let raw_flat = write_plane(&dir.path().join("raw_flat.fits"), |_, _| 2000.0);

        let conn = seed_db();
        seed_light(&conn, 1, &light, None, None);
        seed_master_set(&conn, 10, "Dark", &dark);
        seed_master_set(&conn, 12, "Bias", &bias);
        seed_raw_set(&conn, 200, "Flat", &raw_flat);
        add_link(&conn, 1, 10, "Dark");
        add_link(&conn, 1, 12, "Bias");
        add_link(&conn, 1, 200, "Flat");

        let r = crate::calibration_library::light_resolve::resolve_frame_inputs(&conn, 1, true)
            .unwrap();

        let rd = r.dark.expect("built dark master must resolve");
        assert_eq!(rd.set_id, 10);
        assert_eq!(rd.path, dark.to_string_lossy());
        assert!(
            !rd.uuid.is_empty(),
            "master uuid comes from the identity trigger"
        );

        let rb = r.bias.expect("built bias master must resolve");
        assert_eq!(rb.set_id, 12);
        assert_eq!(rb.path, bias.to_string_lossy());

        assert!(
            r.flat.is_none(),
            "a raw, unbuilt set must not resolve even with a member file on disk"
        );

        // Identity fields the caller names the output by.
        assert_eq!(r.frame_id, 1);
        assert_eq!(r.source_filename, "light_d.fits");
        assert!(
            r.source_uuid.is_some(),
            "light uuid populated by the trigger"
        );

        // And the generator agrees: the skipped flat leaves nothing to divide by.
        let opts = CalibratedLightOptions::default();
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        assert!(spec.inputs.flat_path.is_none());
        assert!(spec.dark_path.is_some());
    }

    /// Review fix #1: a light resolved to both a master Dark and a master
    /// Bias must NOT surface the bias in [`resolved_master_paths`] — the
    /// raw-master-dark convention means the engine subtracts the dark and
    /// never reads the bias plane at all, so a bias file missing from disk
    /// must never block a run that would produce byte-identical output.
    #[test]
    fn resolved_master_paths_excludes_bias_when_dark_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(&dir.path().join("light_e.fits"), |_, _| 1000.0);
        let dark = write_plane(&dir.path().join("dark.fits"), spiky_dark);
        let bias = write_plane(&dir.path().join("bias.fits"), |_, _| 50.0);

        let conn = seed_db();
        seed_light(&conn, 1, &light, None, None);
        seed_master_set(&conn, 10, "Dark", &dark);
        seed_master_set(&conn, 12, "Bias", &bias);
        add_link(&conn, 1, 10, "Dark");
        add_link(&conn, 1, 12, "Bias");

        let opts = CalibratedLightOptions::default();
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        assert_eq!(spec.inputs.dark_path, Some(dark.clone()));
        assert_eq!(
            spec.inputs.bias_path,
            Some(bias.clone()),
            "the link still resolves — resolved_master_paths is what filters it"
        );

        let mut specs = HashMap::new();
        specs.insert(1i64, spec);
        let paths = resolved_master_paths(&specs);
        assert!(paths.contains(&dark), "the dark IS read");
        assert!(
            !paths.contains(&bias),
            "the dark applies — the engine never reads the bias plane"
        );
    }

    /// Same shape, but the Dark LINK exists and points at a RAW, unbuilt set
    /// — `resolve_master` returns `None` for it (`is_master_library = 0`),
    /// so the engine falls back to the bias and its file must be counted.
    ///
    /// `resolved_master_paths` itself only ever sees `spec.inputs.dark_path`
    /// (already resolved by the time a `GenerationSpec` exists — the raw
    /// link is gone), so its own condition cannot regress to a link-existence
    /// check the way `api::lights::compute_export_readiness`'s could. The
    /// discriminating power here is upstream, in `resolve_generation`: a test
    /// that instead seeds NO Dark link at all (the previous shape of this
    /// test) can never catch a regression where a merely-LINKED-but-unbuilt
    /// Dark gets resolved into `dark_path` anyway — there being no link at
    /// all leaves nothing for such a bug to latch onto. This shape asserts
    /// `spec.inputs.dark_path.is_none()` for exactly the C-2 scenario the
    /// review named: a Dark link to an unbuilt raw set, so a regression
    /// there — not just in this function's own `if` — is what this test
    /// actually pins.
    #[test]
    fn resolved_master_paths_includes_bias_when_dark_link_is_unbuilt() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(&dir.path().join("light_f.fits"), |_, _| 1000.0);
        let raw_dark = write_plane(&dir.path().join("raw_dark.fits"), spiky_dark);
        let bias = write_plane(&dir.path().join("bias.fits"), |_, _| 50.0);

        let conn = seed_db();
        seed_light(&conn, 1, &light, None, None);
        seed_raw_set(&conn, 200, "Dark", &raw_dark);
        seed_master_set(&conn, 12, "Bias", &bias);
        add_link(&conn, 1, 200, "Dark");
        add_link(&conn, 1, 12, "Bias");

        let opts = CalibratedLightOptions::default();
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        assert!(
            spec.inputs.dark_path.is_none(),
            "a raw, unbuilt set must not resolve"
        );
        assert_eq!(spec.inputs.bias_path, Some(bias.clone()));

        let mut specs = HashMap::new();
        specs.insert(1i64, spec);
        let paths = resolved_master_paths(&specs);
        assert!(
            paths.contains(&bias),
            "the dark link never resolved — the bias IS read"
        );
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
        match &**hot_maps.values().next().unwrap() {
            HotPixelMapOutcome::Map(m) => assert_eq!(m.len(), 2),
            HotPixelMapOutcome::Refused(r) => panic!("the spiky dark must map: {r}"),
        }
    }

    /// A REFUSED hot-pixel map must not be dressed up as a correction that
    /// found nothing: no `ATH_CHPX` card at all (the card would claim a pass
    /// that never ran), and one human-readable warning naming the dark.
    ///
    /// The refusal is also cached: a second light sharing the same dark neither
    /// re-measures it nor repeats the warning, so a 200-frame set with one
    /// degenerate dark reports ONE line.
    #[test]
    fn refused_hot_map_stamps_no_card_and_warns_once_per_dark() {
        let dir = tempfile::tempdir().unwrap();
        let light_a = write_plane(&dir.path().join("light_a.fits"), |_, _| 1000.0);
        let light_c = write_plane(&dir.path().join("light_c.fits"), |_, _| 1200.0);
        // Uniform: every pixel is the median, so MAD is 0 and the map is
        // refused (the stacked integer-BITPIX master-dark signature).
        let dark = write_plane(&dir.path().join("dark.fits"), |_, _| 300.0);
        let conn = seed_db();
        seed_light(&conn, 1, &light_a, None, None);
        seed_light(&conn, 2, &light_c, None, None);
        seed_master_set(&conn, 10, "Dark", &dark);
        add_link(&conn, 1, 10, "Dark");
        add_link(&conn, 2, 10, "Dark");

        let opts = CalibratedLightOptions::default();
        assert!(opts.hot_pixel_correction, "the pass is on by default");
        let mut hot_maps = HashMap::new();
        let mut generated = Vec::new();
        for (frame_id, name) in [(1i64, "light_a.fits"), (2, "light_c.fits")] {
            let spec = resolve_generation(&conn, frame_id, &opts, dir.path()).unwrap();
            let out = dir.path().join(spec.output_filename(name));
            let g = execute_generation(
                &spec,
                &out,
                dir.path(),
                &opts,
                &mut hot_maps,
                &AtomicBool::new(false),
            )
            .unwrap();
            let header = FitsHeader::from_path(&out).unwrap();
            assert_eq!(
                header.get_i32("ATH_CHPX"),
                None,
                "{name}: a refused map must stamp no correction card"
            );
            assert_eq!(g.hot_pixels_replaced, 0, "{name}");
            assert_eq!(g.calstat, "BD", "{name}: the frame is still calibrated");
            generated.push(g);
        }

        assert_eq!(
            generated[0].warnings.len(),
            1,
            "the first frame reports the refusal: {:?}",
            generated[0].warnings
        );
        let warning = &generated[0].warnings[0];
        assert!(
            warning.contains(&dark.to_string_lossy().to_string()),
            "the warning must name the dark: {warning}"
        );
        assert!(
            warning.contains("zero MAD"),
            "the warning must carry the reason: {warning}"
        );
        assert!(
            generated[1].warnings.is_empty(),
            "the cached refusal must not warn again: {:?}",
            generated[1].warnings
        );
        assert_eq!(
            hot_maps.len(),
            1,
            "a refused dark is measured once per batch, like a mapped one"
        );
    }

    /// The other half of the distinction: a dark that WAS measured and held no
    /// outlier stamps `ATH_CHPX = 0` — the correction ran and replaced nothing,
    /// which is a different (and true) statement from "it was refused".
    ///
    /// Alternating 300/302 gives median 301 and MAD 1.0, so the threshold is
    /// 315.8 and nothing in the plane comes near it.
    #[test]
    fn measured_dark_with_no_hits_still_stamps_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let light = write_plane(&dir.path().join("light_a.fits"), |_, _| 1000.0);
        let dark = write_plane(&dir.path().join("dark.fits"), |x, y| {
            if (x + y) % 2 == 0 {
                300.0
            } else {
                302.0
            }
        });
        let conn = seed_db();
        seed_light(&conn, 1, &light, None, None);
        seed_master_set(&conn, 10, "Dark", &dark);
        add_link(&conn, 1, 10, "Dark");

        let opts = CalibratedLightOptions::default();
        let spec = resolve_generation(&conn, 1, &opts, dir.path()).unwrap();
        let out = dir.path().join(spec.output_filename("light_a.fits"));
        let generated = execute_generation(
            &spec,
            &out,
            dir.path(),
            &opts,
            &mut HashMap::new(),
            &AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(generated.hot_pixels_replaced, 0);
        assert!(
            generated.warnings.is_empty(),
            "a measured dark is not a refusal: {:?}",
            generated.warnings
        );
        let header = FitsHeader::from_path(&out).unwrap();
        assert_eq!(
            header.get_i32("ATH_CHPX"),
            Some(0),
            "the pass ran and replaced nothing — say so"
        );
    }

    /// Resolving a light's flat-normalization divisor reads the master flat's
    /// ENTIRE plane whenever the constant is not on a card (an imported flat,
    /// or a phase disagreement) — and resolution runs once per light. A batch
    /// must therefore compute each distinct (flat, mosaic phase) pair once.
    ///
    /// Proven two ways, because a cache-size assertion alone would still pass
    /// if the miss path ran twice and overwrote its own entry: the flat file is
    /// DELETED between the two resolves, so the second frame can only succeed
    /// by reading the memo, and the divisor it lands on must be the identical
    /// number the first frame stamped.
    #[test]
    fn flat_norm_divisor_memoized_across_a_batch() {
        let dir = tempfile::tempdir().unwrap();
        let light_a = write_plane(&dir.path().join("light_a.fits"), |_, _| 1000.0);
        let light_b = write_plane(&dir.path().join("light_b.fits"), |_, _| 1200.0);
        // No ATH_FNRM card (write_plane writes none), so the divisor can only
        // come from the pixels — the path the memo exists for.
        let flat = write_plane(&dir.path().join("flat.fits"), |x, _| 800.0 + x as f32);
        let conn = seed_db();
        seed_light(&conn, 1, &light_a, None, None);
        seed_light(&conn, 2, &light_b, None, None);
        seed_master_set(&conn, 20, "Flat", &flat);
        add_link(&conn, 1, 20, "Flat");
        add_link(&conn, 2, 20, "Flat");

        let opts = CalibratedLightOptions::default();
        assert!(
            opts.flat_norm,
            "the memo only runs while normalization is on"
        );
        let mut divisors = DivisorCache::new();
        assert!(divisors.is_empty());

        let spec_a = resolve_generation_cached(&conn, 1, &opts, dir.path(), &mut divisors).unwrap();
        assert_eq!(divisors.len(), 1, "the first light resolves the flat once");

        // The plane is gone. A second read would fail; the memo cannot.
        std::fs::remove_file(&flat).unwrap();
        let spec_b = resolve_generation_cached(&conn, 2, &opts, dir.path(), &mut divisors).unwrap();
        assert_eq!(
            divisors.len(),
            1,
            "two lights sharing one flat must resolve its divisor once"
        );

        let fnm = |spec: &GenerationSpec| {
            spec.cards
                .iter()
                .find(|c| c.keyword == "ATH_CFNM")
                .map(|c| format!("{:?}", c.value))
        };
        assert!(fnm(&spec_a).is_some(), "a normalized flat stamps ATH_CFNM");
        assert_eq!(
            fnm(&spec_a),
            fnm(&spec_b),
            "the memoized divisor must be the same number, not a re-derived one"
        );
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
}
