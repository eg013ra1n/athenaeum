//! Probe of the zero-point measurement (collab v3, sub-wave 6.5, Task 2): it
//! runs `analysis::zero_point::measure_zero_point`, the code that ships, on
//! real frames and judges the bars the sub-wave's plan fixes before any
//! consumer is built.
//!
//! Writes nothing but its `--out` file: the catalogue is opened READ_ONLY, and
//! a frame is read from its file and analysed in memory (the analysis and its
//! star rows are kept for the measurement and dropped, never saved).
//!
//! ```text
//! zp_probe measure --db <athenaeum.db> --app-data <dir> --instrume <s> --filter <s|-> \
//!                  --object <s> --out <tsv> [--limit <n>] [--<ZpParams field> <v>]...
//! zp_probe stars   --db <athenaeum.db> --app-data <dir> --frame <id> --out <tsv> [--all-out <tsv>] [--<field> <v>]...
//! zp_probe common  --db <athenaeum.db> --run <id> --out <tsv>
//! zp_probe verdict --class <broadband|narrowband|osc> [--common <tsv>] [--osc-c] <measure.tsv>...
//! zp_probe bigap   --db <athenaeum.db> --app-data <dir> --instrume <s> --filter <s|-> --object <s> \
//!                  [--night <label>] [--every <n>] --out <frames.tsv> --stars-out <stars.tsv>
//! zp_probe forced  --db <athenaeum.db> --app-data <dir> --instrume <s> --filter <s|-> --object <s> \
//!                  --out <x.tsv> [--limit <n>] [--identity-every <n>] [--dump-ap 1] \
//!                  [--<ForcedListParams field> <v>]... [--<ZpParams field> <v>]...
//! zp_probe verdict --forced [--common <tsv>] <x.tsv>...
//! ```
//!
//! `bigap` (OSC Task 3c) adds an independent photometry of the raw mosaic: per Gaia-matched star, plain aperture
//! sums at 1.5 … 8 x the frame's FWHM over all sites and over the green sites, each site less its own CFA phase's sky
//! (see `cmd_bigap`).
//!
//! OSC zero point (Phase A): on a Bayer mosaic `measure` also writes, at the END of the row, the green-site aperture
//! correction `c` rustafits reports (`PlaneMeasurement::reported_aperture_*`, never applied), its star count and
//! error, the airmass of the frame, the zero point with `c` (`zero_point + 2.5 log10 c`) and the rig. The airmass is
//! plain `sec z = 1 / sin(altitude)`, refraction-free, from the solve centre's RA / Dec (precessed from J2000 to the
//! date), `frames.sitelat` / `sitelong` (east positive) and `DATE-OBS` + exposure / 2 (mean sidereal time, UT used for
//! TT); a frame without a site has an empty airmass and is counted. `verdict --class osc --osc-c` judges every night
//! of at least 30 measured frames against the bars of the OSC spec section 6 (see `osc_c_verdict_lines`); without
//! `--osc-c` the verdict is the sub-wave 6.5 one, line for line.
//!
//! `forced` (forced-list zero point, G3) runs the shipping composition `analysis::analyzer::measure_with_zero_point`
//! on frames read in memory and writes TSVs only: `<x>.tsv` (one row per light: the anchor, the window, the list, the map,
//! the pedestal and its clip's kept share, the zero point or its refusal, the 6r referee, today's zero point on the same
//! measurement, the identity check, the wall time of the list split into fits and apertures), `<x>.stars.tsv` (one compact
//! row per list star), `<x>.shifts.tsv` (the anchor-shifted re-measures of the identity frames) and, with `--dump-ap 1`,
//! `<x>.ap.tsv` (every aperture figure). `--identity-every <n>` (default 10) measures every n-th frame a second time
//! without a list and compares the two (B9), and re-measures it with the anchor shifted by +-0.1, +-0.5, +-0.44, +-1.0 and
//! +-1.75 mag. `--decimate <n>` keeps every n-th list star by key. `forced` refuses to start on flags whose ratios break
//! rustafits' annulus rule. `verdict --forced [--common <tsv>] <x.tsv>...` judges the bars B1-B9 of the forced-list spec
//! (section 6, with the plan's review rulings) on the zero point reduced to the common window of each rig, object and
//! exposure: every night of at least 30 measured frames, pairs under 15 minutes apart, one line per bar and a last line
//! GO or STOP. A bar judged on no night of its class is a STOP. The pier side of a frame is read from the solve's field
//! rotation against the night's first frame (more than 90 degrees mod 360 is the other side). `--<field> <v>` on `forced`
//! is any `ForcedListParams` field in snake_case (`--cap 800`, `--pedestal_radii_ref 3,4,6`, ...) so a tuning round needs no
//! rebuild; `min_stars` and `clip_sigma` are also `ZpParams` fields and a value given for one of them sets both. The
//! parameters actually used, and the flags that moved them, are echoed on stderr and written to `<x>.flags.txt`.
//!
//! `stars` writes one row per matched star with the flags and numbers the star
//! fit itself carried (`saturated`, `extended`, `flags`, `peak`, `background`,
//! `snr`, `fwhm`, `flux`); `--all-out` adds one row per analysis star inside the
//! central region, matched or not.
//!
//! `--filter -` selects the frames without a filter (an OSC camera).
//! `--<field>` is any `ZpParams` field in snake_case; the default is
//! `ZpParams::default()`. A light whose file is missing, that lives in a zip
//! or that has no stored solve is named on stderr and written as a row with
//! `refusal` = `missing_file`, `in_zip` or `no_solve` and no measurement; the
//! verdict counts those rows apart and leaves them out of the coverage
//! denominator.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use athenaeum_core::analysis::analyzer::{
    frame_options, map_measurement, measure_raw_frame, measure_with_zero_point, ForcedRun, ZpInputs,
};
use athenaeum_core::analysis::config::{load_config, AnalysisConfig};
use athenaeum_core::analysis::zero_point::{
    catalog_for_frame, common_window, measure_zero_point, CommonWindow, ForcedList, ForcedListParams, ForcedRefusal,
    ForcedStar, ForcedStarZp, ZpInput, ZpMeasurement, ZpParams, ZpRefusal,
};
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::hints::observation_epoch;
use athenaeum_core::plate_solve::open_catalog_layers;
use athenaeum_core::plate_solve::storage::get_plate_solve;
use athenaeum_core::star_measure::analysis_mode::{
    FrameOptions, FrameSummary, ListAperture, ListFit,
};
use athenaeum_core::star_measure::raw::{read_raw_frame, RawPlanes};
use athenaeum_core::star_measure::{PlaneMeasurement, StarFit};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use solvemyastro::StarCache;

// ---------------------------------------------------------------------------
// The bars (plan, Task 2). Constants, not flags: the limits never move.
// ---------------------------------------------------------------------------

/// Frames with a zero point over frames measured, at least.
const COVERAGE_MIN: f64 = 0.90;
/// Median of the per-frame median match distance (px), at most.
const GEOMETRY_MAX_PX: f64 = 1.0;
/// Median of `|Δzero_point|` over consecutive clear-night frames (mag), at most.
const STABILITY_MAX_MAG: f64 = 0.03;
/// Two frames closer than this (s) are consecutive.
const PAIR_MAX_GAP_S: f64 = 900.0;
/// A clear night needs at least this many frames.
const CLEAR_NIGHT_MIN_FRAMES: usize = 20;
/// A set's stability is judged from at least this many pairs.
const STABILITY_MIN_PAIRS: usize = 5;
/// Pairs with `|ΔFWHM|` at least this (px) enter the seeing slope.
const SEEING_MIN_DFWHM_PX: f64 = 0.2;
/// Fewest such pairs for the slope to be judged.
const SEEING_MIN_PAIRS: usize = 20;
/// `|slope|` of `Δzero_point` on `ΔFWHM` (mag/px), at most.
const SEEING_MAX_SLOPE: f64 = 0.03;
/// A set enters the cross-target spread with at least this many valid frames.
const CROSS_MIN_FRAMES: usize = 30;
/// Fewest sets for the cross-target spread to be judged.
const CROSS_MIN_SETS: usize = 3;
/// Spread (max − min) of the sets' 90th-percentile zero points (mag), at most.
const CROSS_MAX_SPREAD_MAG: f64 = 0.10;
/// RMS about the median of the zero-point minus common-flux magnitude (mag), at most.
const MECHANISM_MAX_RMS_MAG: f64 = 0.05;
/// A stacking group enters the mechanism bar with at least this many frames.
const MECHANISM_MIN_GROUP: usize = 10;
/// The verdict prints at most this many lines.
const VERDICT_MAX_LINES: usize = 30;
/// A progress line on stderr every this many frames.
const PROGRESS_EVERY: usize = 20;

// ---------------------------------------------------------------------------
// Argument handling
// ---------------------------------------------------------------------------

/// Flags that take no value (stored as "true").
const SWITCHES: [&str; 2] = ["osc-c", "forced"];

struct Args {
    flags: HashMap<String, String>,
    positional: Vec<String>,
}

impl Args {
    /// Every `--flag` takes one value; anything else is positional.
    fn parse(argv: &[String]) -> Result<Self> {
        let mut flags = HashMap::new();
        let mut positional = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            if let Some(name) = argv[i].strip_prefix("--") {
                if SWITCHES.contains(&name) {
                    if flags.insert(name.to_string(), "true".to_string()).is_some() {
                        bail!("flag --{name} given twice");
                    }
                    i += 1;
                    continue;
                }
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("flag --{name} needs a value"))?;
                if flags.insert(name.to_string(), value.clone()).is_some() {
                    bail!("flag --{name} given twice");
                }
                i += 2;
            } else {
                positional.push(argv[i].clone());
                i += 1;
            }
        }
        Ok(Self { flags, positional })
    }

    fn take(&mut self, name: &str) -> Option<String> {
        self.flags.remove(name)
    }

    fn take_switch(&mut self, name: &str) -> bool {
        self.flags.remove(name).is_some()
    }

    fn require(&mut self, name: &str) -> Result<String> {
        self.take(name).ok_or_else(|| anyhow!("missing --{name}"))
    }

    fn take_parsed<T: std::str::FromStr>(&mut self, name: &str) -> Result<Option<T>> {
        match self.take(name) {
            None => Ok(None),
            Some(v) => v
                .parse::<T>()
                .map(Some)
                .map_err(|_| anyhow!("--{name}: cannot read {v:?}")),
        }
    }

    /// The `ZpParams` of the command line, each field defaulting to `ZpParams::default()`.
    fn zp_params(&mut self) -> Result<ZpParams> {
        let d = ZpParams::default();
        Ok(ZpParams {
            match_radius_px: self
                .take_parsed("match_radius_px")?
                .unwrap_or(d.match_radius_px),
            central_fraction: self
                .take_parsed("central_fraction")?
                .unwrap_or(d.central_fraction),
            min_stars: self.take_parsed("min_stars")?.unwrap_or(d.min_stars),
            max_std_error_mag: self
                .take_parsed("max_std_error_mag")?
                .unwrap_or(d.max_std_error_mag),
            clip_sigma: self.take_parsed("clip_sigma")?.unwrap_or(d.clip_sigma),
            bright_drop_fraction: self
                .take_parsed("bright_drop_fraction")?
                .unwrap_or(d.bright_drop_fraction),
            min_snr: self.take_parsed("min_snr")?.unwrap_or(d.min_snr),
            blend_radius_fwhm: self
                .take_parsed("blend_radius_fwhm")?
                .unwrap_or(d.blend_radius_fwhm),
            blend_dmag: self.take_parsed("blend_dmag")?.unwrap_or(d.blend_dmag),
        })
    }

    /// Like [`Args::take_parsed`], but the flag stays for the next reader (a name `ForcedListParams` and `ZpParams` share).
    fn peek_parsed<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>> {
        match self.flags.get(name) {
            None => Ok(None),
            Some(v) => v
                .parse::<T>()
                .map(Some)
                .map_err(|_| anyhow!("--{name}: cannot read {v:?}")),
        }
    }

    /// The `ForcedListParams` of the command line, each field defaulting to `ForcedListParams::default()`. A name
    /// `ZpParams` has too (`min_stars`, `clip_sigma`, `max_std_error_mag`) is read without being taken: call this BEFORE
    /// [`Args::zp_params`], which takes it, so one value sets both.
    fn forced_list_params(&mut self) -> Result<ForcedListParams> {
        let d = ForcedListParams::default();
        let pedestal_radii_ref = match self.take("pedestal_radii_ref") {
            None => d.pedestal_radii_ref,
            Some(v) => {
                let parts: Vec<f64> = v
                    .split(',')
                    .map(|x| x.trim().parse::<f64>())
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|_| anyhow!("--pedestal_radii_ref: cannot read {v:?} (three numbers a,b,c)"))?;
                let [a, b, c] = parts[..] else {
                    bail!("--pedestal_radii_ref: needs three numbers a,b,c, got {v:?}");
                };
                [a, b, c]
            }
        };
        Ok(ForcedListParams {
            fwhm_floor_px: self.take_parsed("fwhm_floor_px")?.unwrap_or(d.fwhm_floor_px),
            fwhm_ref_arcsec: self.take_parsed("fwhm_ref_arcsec")?.unwrap_or(d.fwhm_ref_arcsec),
            anchor_min_matches: self.take_parsed("anchor_min_matches")?.unwrap_or(d.anchor_min_matches),
            window_peak_fraction: self.take_parsed("window_peak_fraction")?.unwrap_or(d.window_peak_fraction),
            window_bright_margin_mag: self
                .take_parsed("window_bright_margin_mag")?
                .unwrap_or(d.window_bright_margin_mag),
            window_faint_width_mag: self.take_parsed("window_faint_width_mag")?.unwrap_or(d.window_faint_width_mag),
            window_faint_max_mag: self.take_parsed("window_faint_max_mag")?.unwrap_or(d.window_faint_max_mag),
            min_isolated: self.take_parsed("min_isolated")?.unwrap_or(d.min_isolated),
            window_ramp_mag: self.take_parsed("window_ramp_mag")?.unwrap_or(d.window_ramp_mag),
            region_fraction: self.take_parsed("region_fraction")?.unwrap_or(d.region_fraction),
            edge_margin_fwhm: self.take_parsed("edge_margin_fwhm")?.unwrap_or(d.edge_margin_fwhm),
            isolation_dmag: self.take_parsed("isolation_dmag")?.unwrap_or(d.isolation_dmag),
            isolation_radius_fwhm: self.take_parsed("isolation_radius_fwhm")?.unwrap_or(d.isolation_radius_fwhm),
            bright_dmag: self.take_parsed("bright_dmag")?.unwrap_or(d.bright_dmag),
            bright_radius_fwhm: self.take_parsed("bright_radius_fwhm")?.unwrap_or(d.bright_radius_fwhm),
            detected_neighbours: self.take_parsed("detected_neighbours")?.unwrap_or(d.detected_neighbours),
            own_seed_radius_px: self.take_parsed("own_seed_radius_px")?.unwrap_or(d.own_seed_radius_px),
            detected_neighbour_min_peak_sigma: self
                .take_parsed("detected_neighbour_min_peak_sigma")?
                .unwrap_or(d.detected_neighbour_min_peak_sigma),
            seed_calibration_min_pairs: self
                .take_parsed("seed_calibration_min_pairs")?
                .unwrap_or(d.seed_calibration_min_pairs),
            cap: self.take_parsed("cap")?.unwrap_or(d.cap),
            map_window_mag: self.take_parsed("map_window_mag")?.unwrap_or(d.map_window_mag),
            map_max_stars: self.take_parsed("map_max_stars")?.unwrap_or(d.map_max_stars),
            map_min_snr: self.take_parsed("map_min_snr")?.unwrap_or(d.map_min_snr),
            map_radius_fwhm: self.take_parsed("map_radius_fwhm")?.unwrap_or(d.map_radius_fwhm),
            map_clip_rounds: self.take_parsed("map_clip_rounds")?.unwrap_or(d.map_clip_rounds),
            map_clip_sigma: self.take_parsed("map_clip_sigma")?.unwrap_or(d.map_clip_sigma),
            map_clip_floor_px: self.take_parsed("map_clip_floor_px")?.unwrap_or(d.map_clip_floor_px),
            map_min_members: self.take_parsed("map_min_members")?.unwrap_or(d.map_min_members),
            map_min_kept_share: self.take_parsed("map_min_kept_share")?.unwrap_or(d.map_min_kept_share),
            centre_tolerance_px: self.take_parsed("centre_tolerance_px")?.unwrap_or(d.centre_tolerance_px),
            centre_tolerance_fwhm: self.take_parsed("centre_tolerance_fwhm")?.unwrap_or(d.centre_tolerance_fwhm),
            aperture_radius_ref: self.take_parsed("aperture_radius_ref")?.unwrap_or(d.aperture_radius_ref),
            aperture_radius_fwhm: self.take_parsed("aperture_radius_fwhm")?.unwrap_or(d.aperture_radius_fwhm),
            pedestal_radii_ref,
            pedestal_clip_sigma: self.take_parsed("pedestal_clip_sigma")?.unwrap_or(d.pedestal_clip_sigma),
            pedestal_clip_rounds: self.take_parsed("pedestal_clip_rounds")?.unwrap_or(d.pedestal_clip_rounds),
            pedestal_min_points: self.take_parsed("pedestal_min_points")?.unwrap_or(d.pedestal_min_points),
            annulus_area_px: self.take_parsed("annulus_area_px")?.unwrap_or(d.annulus_area_px),
            annulus_start_fwhm: self.take_parsed("annulus_start_fwhm")?.unwrap_or(d.annulus_start_fwhm),
            annulus_gap_px: self.take_parsed("annulus_gap_px")?.unwrap_or(d.annulus_gap_px),
            peak_radius_ref: self.take_parsed("peak_radius_ref")?.unwrap_or(d.peak_radius_ref),
            clip_sigma: self.peek_parsed("clip_sigma")?.unwrap_or(d.clip_sigma),
            min_stars: self.peek_parsed("min_stars")?.unwrap_or(d.min_stars),
            max_sigma_mag: self.take_parsed("max_sigma_mag")?.unwrap_or(d.max_sigma_mag),
            screen_taper_sigma: self.take_parsed("screen_taper_sigma")?.unwrap_or(d.screen_taper_sigma),
            slope_min_ramp_members: self.take_parsed("slope_min_ramp_members")?.unwrap_or(d.slope_min_ramp_members),
            decimate: self.take_parsed("decimate")?.unwrap_or(d.decimate),
            anchor_shift_mag: self.take_parsed("anchor_shift_mag")?.unwrap_or(d.anchor_shift_mag),
        })
    }

    /// Fails on a flag nothing took (a typo must not run with a default).
    fn finish(&self) -> Result<()> {
        if let Some(name) = self.flags.keys().min() {
            bail!("unknown flag --{name}");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TSV
// ---------------------------------------------------------------------------

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn num(x: Option<f64>, precision: usize) -> String {
    match x {
        Some(v) if v.is_finite() => format!("{v:.precision$}"),
        _ => String::new(),
    }
}

/// A number for the verdict text: "n/a" when there is none.
fn show(x: Option<f64>, precision: usize) -> String {
    match x {
        Some(v) if v.is_finite() => format!("{v:.precision$}"),
        _ => "n/a".to_string(),
    }
}

fn read_tsv(path: &str) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()
        .ok_or_else(|| anyhow!("{path} is empty"))?
        .split('\t')
        .map(str::to_string)
        .collect();
    let rows = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect();
    Ok((header, rows))
}

/// The field of `name`, or "" when the column or the field is absent.
fn col<'a>(header: &[String], fields: &'a [String], name: &str) -> &'a str {
    header
        .iter()
        .position(|h| h == name)
        .and_then(|i| fields.get(i))
        .map_or("", String::as_str)
}

fn opt_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn opt_i64(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

// ---------------------------------------------------------------------------
// The measure row
// ---------------------------------------------------------------------------

/// Refusal codes of a candidate light the probe did not measure at all.
const UNMEASURED: [&str; 3] = ["no_solve", "in_zip", "missing_file"];

const MEASURE_HEADER: &str = "frame_id\tfile_name\tobject\tfilter\tnight\tdate_obs\texptime\tstars_detected\tmedian_fwhm\tmatched\tkept\tzero_point\tsigma\tstd_error\tmedian_match_px\trefusal\tc\tc_stars\tc_se\tairmass\tzero_point_c\trig";

#[derive(Debug, Clone, Default)]
struct MeasureRow {
    frame_id: Option<i64>,
    file_name: String,
    object: String,
    filter: String,
    night: String,
    date_obs: String,
    exptime: Option<f64>,
    stars_detected: Option<i64>,
    median_fwhm: Option<f64>,
    matched: Option<u32>,
    kept: Option<u32>,
    zero_point: Option<f64>,
    sigma: Option<f64>,
    std_error: Option<f64>,
    median_match_px: Option<f64>,
    refusal: String,
    // The OSC columns, at the end of the row so an older TSV still parses.
    /// The green-site aperture correction of a mosaic (`None` on a mono frame or with too few stars).
    c: Option<f64>,
    /// The stars `c` is the median over; `Some(0)` marks a mosaic that measured none, `None` a non-mosaic frame.
    c_stars: Option<i64>,
    c_se: Option<f64>,
    airmass: Option<f64>,
    /// `zero_point + 2.5 log10 c`.
    zero_point_c: Option<f64>,
    /// Camera, telescope, focal length, gain, binning and filter of the frame.
    rig: String,
}

impl MeasureRow {
    fn to_tsv(&self) -> String {
        let cells = [
            self.frame_id.map(|v| v.to_string()).unwrap_or_default(),
            clean(&self.file_name),
            clean(&self.object),
            clean(&self.filter),
            clean(&self.night),
            clean(&self.date_obs),
            num(self.exptime, 3),
            self.stars_detected
                .map(|v| v.to_string())
                .unwrap_or_default(),
            num(self.median_fwhm, 3),
            self.matched.map(|v| v.to_string()).unwrap_or_default(),
            self.kept.map(|v| v.to_string()).unwrap_or_default(),
            num(self.zero_point, 5),
            num(self.sigma, 5),
            num(self.std_error, 5),
            num(self.median_match_px, 4),
            clean(&self.refusal),
            num(self.c, 5),
            self.c_stars.map(|v| v.to_string()).unwrap_or_default(),
            num(self.c_se, 5),
            num(self.airmass, 4),
            num(self.zero_point_c, 5),
            clean(&self.rig),
        ];
        cells.join("\t")
    }

    fn from_fields(header: &[String], f: &[String]) -> Self {
        Self {
            frame_id: opt_i64(col(header, f, "frame_id")),
            file_name: col(header, f, "file_name").to_string(),
            object: col(header, f, "object").to_string(),
            filter: col(header, f, "filter").to_string(),
            night: col(header, f, "night").to_string(),
            date_obs: col(header, f, "date_obs").to_string(),
            exptime: opt_f64(col(header, f, "exptime")),
            stars_detected: opt_i64(col(header, f, "stars_detected")),
            median_fwhm: opt_f64(col(header, f, "median_fwhm")),
            matched: opt_i64(col(header, f, "matched")).map(|v| v as u32),
            kept: opt_i64(col(header, f, "kept")).map(|v| v as u32),
            zero_point: opt_f64(col(header, f, "zero_point")),
            sigma: opt_f64(col(header, f, "sigma")),
            std_error: opt_f64(col(header, f, "std_error")),
            median_match_px: opt_f64(col(header, f, "median_match_px")),
            refusal: col(header, f, "refusal").to_string(),
            c: opt_f64(col(header, f, "c")),
            c_stars: opt_i64(col(header, f, "c_stars")),
            c_se: opt_f64(col(header, f, "c_se")),
            airmass: opt_f64(col(header, f, "airmass")),
            zero_point_c: opt_f64(col(header, f, "zero_point_c")),
            rig: col(header, f, "rig").to_string(),
        }
    }

    /// `date_obs` as seconds since the epoch.
    fn t_s(&self) -> Option<f64> {
        parse_time(&self.date_obs).map(|t| t.timestamp_millis() as f64 / 1000.0)
    }

    /// A row of a light the probe did not measure (no solve, in a zip, file missing).
    fn is_unmeasured(&self) -> bool {
        UNMEASURED.contains(&self.refusal.as_str())
    }

    fn set_key(&self) -> (String, String) {
        (self.object.clone(), self.filter.clone())
    }
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|n| n.and_utc())
}

/// The night label of a frame: the UTC date of the catalogue night's start
/// minus 12 h (the catalogue's own night, `imaging_nights`), and when the
/// frame has no night row the same from its `date_obs`.
fn night_label(night_start: Option<&str>, date_obs: &str) -> String {
    night_start
        .and_then(parse_time)
        .or_else(|| parse_time(date_obs))
        .map(|t| (t - Duration::hours(12)).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Airmass
// ---------------------------------------------------------------------------

/// Julian date of a Unix time (UT; the difference to TT, about a minute, is far below what the airmass resolves).
fn julian_date(unix_s: f64) -> f64 {
    unix_s / 86_400.0 + 2_440_587.5
}

/// Greenwich mean sidereal time in degrees, `[0, 360)` (Meeus, Astronomical Algorithms, eq. 12.4).
fn gmst_deg(jd: f64) -> f64 {
    let t = (jd - 2_451_545.0) / 36_525.0;
    (280.460_618_37 + 360.985_647_366_29 * (jd - 2_451_545.0) + 0.000_387_933 * t * t
        - t * t * t / 38_710_000.0)
        .rem_euclid(360.0)
}

/// A J2000 position carried to the mean equinox of the date `jd` (Meeus, eq. 21.2 and 21.4), degrees.
fn precess_from_j2000(ra_deg: f64, dec_deg: f64, jd: f64) -> (f64, f64) {
    let t = (jd - 2_451_545.0) / 36_525.0;
    let arcsec = std::f64::consts::PI / (180.0 * 3600.0);
    let zeta = (2306.2181 * t + 0.30188 * t * t + 0.017998 * t * t * t) * arcsec;
    let z = (2306.2181 * t + 1.09468 * t * t + 0.018203 * t * t * t) * arcsec;
    let theta = (2004.3109 * t - 0.42665 * t * t - 0.041833 * t * t * t) * arcsec;
    let (a0, d0) = (ra_deg.to_radians(), dec_deg.to_radians());
    let a = d0.cos() * (a0 + zeta).sin();
    let b = theta.cos() * d0.cos() * (a0 + zeta).cos() - theta.sin() * d0.sin();
    let c = theta.sin() * d0.cos() * (a0 + zeta).cos() + theta.cos() * d0.sin();
    (
        (a.atan2(b) + z).to_degrees().rem_euclid(360.0),
        c.asin().to_degrees(),
    )
}

/// The plain airmass `sec z = 1 / sin(altitude)` of a J2000 position at a Unix time, for a site at `lat_deg`,
/// `lon_deg` (east positive). Refraction-free, no Pickering or Kasten-Young correction: for the extinction fit a
/// near-zenith difference of 0.001 airmass is nothing, and near the horizon the frames are few. `None` at or below
/// the horizon.
fn airmass(ra_deg: f64, dec_deg: f64, unix_s: f64, lat_deg: f64, lon_deg: f64) -> Option<f64> {
    if ![ra_deg, dec_deg, unix_s, lat_deg, lon_deg]
        .iter()
        .all(|v| v.is_finite())
    {
        return None;
    }
    let jd = julian_date(unix_s);
    let (ra, dec) = precess_from_j2000(ra_deg, dec_deg, jd);
    let hour_angle = (gmst_deg(jd) + lon_deg - ra).to_radians();
    let (lat, dec) = (lat_deg.to_radians(), dec.to_radians());
    let sin_alt = lat.sin() * dec.sin() + lat.cos() * dec.cos() * hour_angle.cos();
    (sin_alt > 0.0).then(|| 1.0 / sin_alt)
}

// ---------------------------------------------------------------------------
// The verdict arithmetic: plain functions over rows, no files
// ---------------------------------------------------------------------------

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    })
}

/// Linear-interpolation percentile (`q` in 0..=1) of a sorted slice.
fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let pos = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    Some(sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64))
}

/// Frames with a zero point over frames measured.
fn coverage(rows: &[MeasureRow]) -> Option<f64> {
    if rows.is_empty() {
        return None;
    }
    let valid = rows.iter().filter(|r| r.zero_point.is_some()).count();
    Some(valid as f64 / rows.len() as f64)
}

/// Median of the per-frame median match distance (px).
fn geometry(rows: &[MeasureRow]) -> Option<f64> {
    let mut px: Vec<f64> = rows.iter().filter_map(|r| r.median_match_px).collect();
    median(&mut px)
}

/// The set's clear night (P7), picked before any zero point is looked at: the
/// night with at least `CLEAR_NIGHT_MIN_FRAMES` analysed frames and the highest
/// median `stars_detected` (a tie goes to the earlier night label).
fn clear_night(rows: &[&MeasureRow]) -> Option<String> {
    let mut by_night: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for r in rows {
        if let Some(s) = r.stars_detected {
            by_night.entry(r.night.as_str()).or_default().push(s as f64);
        }
    }
    let mut best: Option<(&str, f64)> = None;
    for (night, mut stars) in by_night {
        if stars.len() < CLEAR_NIGHT_MIN_FRAMES {
            continue;
        }
        let m = median(&mut stars).unwrap_or(0.0);
        if best.is_none_or(|(_, bm)| m > bm) {
            best = Some((night, m));
        }
    }
    best.map(|(n, _)| n.to_string())
}

/// One pair of consecutive frames.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pair {
    dzp: f64,
    dfwhm: f64,
}

/// Consecutive pairs of one night: frames with a zero point and a FWHM in time
/// order, the pair kept when its two frames are less than 15 min apart.
fn consecutive_pairs(night_rows: &[&MeasureRow]) -> Vec<Pair> {
    consecutive_pairs_of(night_rows, |r| r.zero_point)
}

/// [`consecutive_pairs`] of any per-frame number (the zero point, or the zero point with `c`).
fn consecutive_pairs_of(
    night_rows: &[&MeasureRow],
    value: impl Fn(&MeasureRow) -> Option<f64>,
) -> Vec<Pair> {
    let mut frames: Vec<(f64, f64, f64)> = night_rows
        .iter()
        .filter_map(|r| Some((r.t_s()?, value(r)?, r.median_fwhm?)))
        .collect();
    frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    frames
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 < PAIR_MAX_GAP_S)
        .map(|w| Pair {
            dzp: w[1].1 - w[0].1,
            dfwhm: w[1].2 - w[0].2,
        })
        .collect()
}

/// Median of `|Δzero_point|` over the pairs.
fn stability(pairs: &[Pair]) -> Option<f64> {
    let mut d: Vec<f64> = pairs.iter().map(|p| p.dzp.abs()).collect();
    median(&mut d)
}

/// The seeing slope of a pooled set of pairs.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Seeing {
    /// The judged slope: least squares of `Δzero_point` on `ΔFWHM` with an
    /// intercept, `Σ(dx − x̄)(dy − ȳ) / Σ(dx − x̄)²` (mag/px).
    slope: f64,
    /// The same fit through the origin, printed for information, never judged.
    through_origin: f64,
    /// Pairs with `|ΔFWHM| >= 0.2 px` the fit used.
    pairs: usize,
}

/// The seeing slope over the pairs with `|ΔFWHM| >= 0.2 px`; `None` under 20
/// such pairs or when their ΔFWHM do not vary. Both differences are later
/// minus earlier, so a pair's sign is kept, but a drift in zero point alone
/// (a mean Δzero_point other than zero) is why the judged fit has an intercept.
fn seeing_slope(pairs: &[Pair]) -> Option<Seeing> {
    let used: Vec<&Pair> = pairs
        .iter()
        .filter(|p| p.dfwhm.abs() >= SEEING_MIN_DFWHM_PX)
        .collect();
    if used.len() < SEEING_MIN_PAIRS {
        return None;
    }
    let n = used.len() as f64;
    let mx = used.iter().map(|p| p.dfwhm).sum::<f64>() / n;
    let my = used.iter().map(|p| p.dzp).sum::<f64>() / n;
    let sxx: f64 = used.iter().map(|p| (p.dfwhm - mx).powi(2)).sum();
    let sxy: f64 = used.iter().map(|p| (p.dfwhm - mx) * (p.dzp - my)).sum();
    let sxx0: f64 = used.iter().map(|p| p.dfwhm * p.dfwhm).sum();
    let sxy0: f64 = used.iter().map(|p| p.dfwhm * p.dzp).sum();
    (sxx > 1e-12).then(|| Seeing {
        slope: sxy / sxx,
        through_origin: sxy0 / sxx0,
        pairs: used.len(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CrossTarget {
    Judged { spread: f64, sets: usize },
    Unjudged { sets: usize },
}

/// Spread (max − min) of the sets' 90th-percentile zero points, over the sets
/// with at least 30 valid frames; fewer than three such sets is unjudged.
fn cross_target(set_zero_points: &[Vec<f64>]) -> CrossTarget {
    let mut p90: Vec<f64> = set_zero_points
        .iter()
        .filter(|z| z.len() >= CROSS_MIN_FRAMES)
        .filter_map(|z| {
            let mut s = z.clone();
            s.sort_by(|a, b| a.total_cmp(b));
            percentile(&s, 0.9)
        })
        .collect();
    let sets = p90.len();
    if sets < CROSS_MIN_SETS {
        return CrossTarget::Unjudged { sets };
    }
    p90.sort_by(|a, b| a.total_cmp(b));
    CrossTarget::Judged {
        spread: p90[sets - 1] - p90[0],
        sets,
    }
}

/// The common-flux file name of a project frame is the calibrated copy's
/// (`c_` prefix); the local light has the name without it.
fn name_key(file_name: &str) -> &str {
    file_name.strip_prefix("c_").unwrap_or(file_name)
}

#[derive(Debug, Clone, Default)]
struct CommonRow {
    frame_id: Option<i64>,
    file_name: String,
    group_key: String,
    planes: usize,
    /// `commonTflux` of planes 0..3.
    tflux: [Option<f64>; 3],
}

impl CommonRow {
    /// The green plane of a three-plane frame, the only plane of a mono one.
    fn green_tflux(&self) -> Option<f64> {
        let plane = if self.planes >= 3 { 1 } else { 0 };
        self.tflux[plane].filter(|v| *v > 0.0)
    }
}

/// Worst RMS (about the median, per stacking group) of
/// `zero_point − 2.5·log10(commonTflux / exptime)` over the groups with at
/// least `MECHANISM_MIN_GROUP` joined frames; also the groups judged and the
/// frames joined. The two sides are in different units, so only the spread counts.
fn mechanism(measure: &[MeasureRow], common: &[CommonRow]) -> (Option<f64>, usize, usize) {
    let by_id: HashMap<i64, &MeasureRow> = measure
        .iter()
        .filter_map(|r| Some((r.frame_id?, r)))
        .collect();
    let by_name: HashMap<&str, &MeasureRow> =
        measure.iter().map(|r| (r.file_name.as_str(), r)).collect();
    let mut groups: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut joined = 0;
    for c in common {
        let m = match c.frame_id {
            Some(id) => by_id.get(&id).copied(),
            None => by_name.get(name_key(&c.file_name)).copied(),
        };
        let (Some(m), Some(tflux)) = (m, c.green_tflux()) else {
            continue;
        };
        let (Some(zp), Some(t)) = (m.zero_point, m.exptime.filter(|t| *t > 0.0)) else {
            continue;
        };
        groups
            .entry(c.group_key.as_str())
            .or_default()
            .push(zp - 2.5 * (tflux / t).log10());
        joined += 1;
    }
    let mut worst: Option<f64> = None;
    let mut judged = 0;
    for (_, mut v) in groups {
        if v.len() < MECHANISM_MIN_GROUP {
            continue;
        }
        let m = median(&mut v).unwrap_or(0.0);
        let rms = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        judged += 1;
        worst = Some(worst.map_or(rms, |w: f64| w.max(rms)));
    }
    (worst, judged, joined)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
    Unjudged,
}

fn judge_max(value: Option<f64>, limit: f64) -> Outcome {
    match value {
        None => Outcome::Unjudged,
        Some(v) if v <= limit => Outcome::Pass,
        Some(_) => Outcome::Fail,
    }
}

fn tag(o: Outcome) -> &'static str {
    match o {
        Outcome::Pass => "pass",
        Outcome::Fail => "FAIL",
        Outcome::Unjudged => "unjudged",
    }
}

/// The class line: FAIL wins over UNJUDGED, which wins over PASS.
fn class_line(class: &str, bars: &[(&str, Outcome)]) -> String {
    let names = |want: Outcome| {
        bars.iter()
            .filter(|(_, o)| *o == want)
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(",")
    };
    if bars.iter().any(|(_, o)| *o == Outcome::Fail) {
        format!("ZP PROBE {class}: FAIL({})", names(Outcome::Fail))
    } else if bars.iter().any(|(_, o)| *o == Outcome::Unjudged) {
        format!("ZP PROBE {class}: UNJUDGED({})", names(Outcome::Unjudged))
    } else {
        format!("ZP PROBE {class}: PASS")
    }
}

/// The verdict text (at most 30 lines; the last is the class line).
fn verdict_lines(
    class: &str,
    all_rows: &[MeasureRow],
    common: Option<&[CommonRow]>,
) -> Vec<String> {
    // Lights the probe did not measure stay out of every bar; their counts ride
    // on the coverage line.
    let rows: Vec<MeasureRow> = all_rows
        .iter()
        .filter(|r| !r.is_unmeasured())
        .cloned()
        .collect();
    let rows = rows.as_slice();
    let unmeasured: Vec<usize> = UNMEASURED
        .iter()
        .map(|code| all_rows.iter().filter(|r| r.refusal == *code).count())
        .collect();

    let mut sets: BTreeMap<(String, String), Vec<&MeasureRow>> = BTreeMap::new();
    for r in rows {
        sets.entry(r.set_key()).or_default().push(r);
    }
    let mut lines = vec![format!(
        "ZP PROBE {class}: {} frames in {} sets",
        rows.len(),
        sets.len()
    )];
    let mut bars: Vec<(&str, Outcome)> = Vec::new();

    // coverage
    let cov = coverage(rows);
    let o = judge_max(cov.map(|c| COVERAGE_MIN - c), 0.0);
    lines.push(format!(
        "coverage: {} (limit >= {COVERAGE_MIN:.2}) {}; not in the denominator: {} {}, {} {}, {} {}",
        show(cov, 3),
        tag(o),
        unmeasured[0],
        UNMEASURED[0],
        unmeasured[1],
        UNMEASURED[1],
        unmeasured[2],
        UNMEASURED[2],
    ));
    bars.push(("coverage", o));

    // geometry
    let geo = geometry(rows);
    let o = judge_max(geo, GEOMETRY_MAX_PX);
    lines.push(format!(
        "geometry: {} px (limit <= {GEOMETRY_MAX_PX:.1}) {}",
        show(geo, 3),
        tag(o)
    ));
    bars.push(("geometry", o));

    // Stability on every set's clear night; a set it cannot judge says so.
    // The seeing bar pools the pairs of every set that has a clear night.
    let mut set_lines = Vec::new();
    let mut not_judged = Vec::new();
    let mut all_pairs: Vec<Pair> = Vec::new();
    let mut set_stabilities: Vec<f64> = Vec::new();
    let mut set_zps: Vec<Vec<f64>> = Vec::new();
    for ((object, filter), members) in &sets {
        let valid: Vec<f64> = members.iter().filter_map(|r| r.zero_point).collect();
        let night = clear_night(members);
        let (night_text, stab) = match &night {
            None => {
                not_judged.push(format!(
                    "stability {object}/{filter}: not judged (no night with {CLEAR_NIGHT_MIN_FRAMES} frames)"
                ));
                (
                    format!("none (no night with {CLEAR_NIGHT_MIN_FRAMES} frames)"),
                    None,
                )
            }
            Some(n) => {
                let in_night: Vec<&MeasureRow> =
                    members.iter().copied().filter(|r| &r.night == n).collect();
                let pairs = consecutive_pairs(&in_night);
                all_pairs.extend(pairs.iter().copied());
                let s = (pairs.len() >= STABILITY_MIN_PAIRS)
                    .then(|| stability(&pairs))
                    .flatten();
                if s.is_none() {
                    not_judged.push(format!(
                        "stability {object}/{filter}: not judged (clear night {n} has {} consecutive pairs, fewer than {STABILITY_MIN_PAIRS})",
                        pairs.len()
                    ));
                }
                (
                    format!("{n} ({} frames, {} pairs)", in_night.len(), pairs.len()),
                    s,
                )
            }
        };
        if let Some(s) = stab {
            set_stabilities.push(s);
        }
        set_lines.push(format!(
            "set {object}/{filter}: {} frames, {} valid, clear night {night_text}, stability {} mag",
            members.len(),
            valid.len(),
            show(stab, 4)
        ));
        set_zps.push(valid);
    }
    let worst_stab = set_stabilities
        .iter()
        .copied()
        .fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))));
    let o = judge_max(worst_stab, STABILITY_MAX_MAG);
    lines.push(format!(
        "stability: {} mag worst (limit <= {STABILITY_MAX_MAG:.2}) {}, judged {} of {} sets",
        show(worst_stab, 4),
        tag(o),
        set_stabilities.len(),
        sets.len()
    ));
    bars.push(("stability", o));

    let seeing = seeing_slope(&all_pairs);
    let o = judge_max(seeing.map(|s| s.slope.abs()), SEEING_MAX_SLOPE);
    lines.push(match seeing {
        Some(s) => format!(
            "seeing: slope {:.4} mag/px over {} pairs (limit |slope| <= {SEEING_MAX_SLOPE:.2}) {}; through-origin slope {:.4} (information)",
            s.slope,
            s.pairs,
            tag(o),
            s.through_origin
        ),
        None => format!(
            "seeing: fewer than {SEEING_MIN_PAIRS} pairs with |dFWHM| >= {SEEING_MIN_DFWHM_PX} px, {}",
            tag(o)
        ),
    });
    bars.push(("seeing", o));

    // mechanism, only when asked for
    match common {
        Some(c) => {
            let (rms, groups, joined) = mechanism(rows, c);
            let o = judge_max(rms, MECHANISM_MAX_RMS_MAG);
            lines.push(format!(
                "mechanism: rms {} mag over {groups} groups, {joined} frames joined (limit <= {MECHANISM_MAX_RMS_MAG:.2}) {}",
                show(rms, 4),
                tag(o)
            ));
            bars.push(("mechanism", o));
        }
        None => lines.push("mechanism: not requested (no --common)".to_string()),
    }

    // cross-target: decides the rig-wide fallback, never part of the class verdict
    lines.push(match cross_target(&set_zps) {
        CrossTarget::Judged { spread, sets } => format!(
            "cross-target: {spread:.4} mag over {sets} sets -> rig-wide fallback {}",
            if spread <= CROSS_MAX_SPREAD_MAG {
                "ALLOWED"
            } else {
                "NOT ALLOWED"
            }
        ),
        CrossTarget::Unjudged { sets } => {
            format!("cross-target: n/a mag over {sets} sets -> rig-wide fallback UNJUDGED")
        }
    });

    // The class line takes one line and the set lines fill what is left; the
    // sets no bar could judge come first, and a cut says how many it dropped.
    let mut set_lines: Vec<String> = not_judged.into_iter().chain(set_lines).collect();
    let room = VERDICT_MAX_LINES.saturating_sub(lines.len() + 1);
    if set_lines.len() > room {
        let keep = room.saturating_sub(1);
        let cut = set_lines.len() - keep;
        set_lines.truncate(keep);
        set_lines.push(format!("… {cut} more set lines not shown"));
        set_lines.truncate(room);
    }
    lines.extend(set_lines);
    lines.push(class_line(class, &bars));
    lines
}

// ---------------------------------------------------------------------------
// The OSC verdict (`verdict --osc-c`): the bars of the OSC zero-point spec, section 6
// ---------------------------------------------------------------------------

/// A night is judged with at least this many measured frames.
const OSC_NIGHT_MIN_FRAMES: usize = 30;
/// Mosaic frames with a `c`, of those analysed, at least (per night).
const C_MEASURED_MIN: f64 = 0.95;
/// Median `c_se / c`, at most.
const C_SE_REL_MAX: f64 = 0.01;
/// Median `|Δ ln c|` of consecutive frames, at most.
const C_STEADY_MAX: f64 = 0.015;
/// A night's slope is judged only when its standard error is at most this (mag/px).
const SLOPE_SE_MAX: f64 = 0.015;
/// `|slope|` of the corrected zero point on FWHM (mag/px), at most: the seeing bar.
const SLOPE_MAX: f64 = 0.03;
/// `c` is held to do the work on the nights whose uncorrected `|slope|` is beyond this (mag/px).
const WORK_SLOPE_U_MIN: f64 = 0.06;
/// The corrected and the uncorrected slope must be this many standard errors apart there.
const WORK_SIGMAS: f64 = 3.0;
/// Fewest frames a night needs in the shared fit.
const FIT_NIGHT_MIN_POINTS: usize = 5;
/// Fewest consecutive pairs for `c`'s steadiness.
const C_STEADY_MIN_PAIRS: usize = 5;

/// One frame in the shared extinction fit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FitPoint {
    /// Index of the night in the fit.
    night: usize,
    /// The pier side of the frame (0 or 1); read only on a night the fit marks two-sided.
    side: f64,
    airmass: f64,
    fwhm: f64,
    zp: f64,
}

/// One night's part of the shared fit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NightFit {
    /// The night's zero point at airmass 0 and FWHM 0 (mag).
    intercept: f64,
    /// The night's slope of the zero point on FWHM (mag/px) and its standard error.
    slope: f64,
    slope_se: f64,
    n: usize,
    fwhm_min: f64,
    fwhm_max: f64,
    /// The step of the zero point between the two pier sides (side 1 minus side 0), on a two-sided night.
    side_step: Option<f64>,
}

/// `zero point = k · airmass + a_night + s_night · FWHM`, ordinary least squares over every night of a rig (the
/// atmosphere's extinction shows as a negative `k`).
#[derive(Debug, Clone, PartialEq)]
struct SharedFit {
    /// The rig's extinction coefficient (mag per airmass) and its standard error.
    k: f64,
    k_se: f64,
    /// By night index; `None` for a night the fit could not carry (too few frames, one FWHM).
    nights: Vec<Option<NightFit>>,
    /// Frames in the fit and its degrees of freedom.
    n: usize,
    dof: usize,
}

/// Ordinary least squares `y = X β` by the normal equations on column-scaled data, Gauss-Jordan with partial
/// pivoting. Returns β, the diagonal of `(XᵀX)⁻¹` and the residual sum of squares; `None` for a singular design.
fn least_squares(rows: &[Vec<f64>], y: &[f64]) -> Option<(Vec<f64>, Vec<f64>, f64)> {
    let p = rows.first()?.len();
    let n = rows.len();
    if n <= p || y.len() != n {
        return None;
    }
    let scale: Vec<f64> = (0..p)
        .map(|j| rows.iter().map(|r| r[j] * r[j]).sum::<f64>().sqrt())
        .collect();
    if scale.iter().any(|s| !(*s > 0.0) || !s.is_finite()) {
        return None;
    }
    // [A | I] with A = XᵀX scaled to a unit diagonal, b = Xᵀy scaled the same way.
    let mut a = vec![vec![0.0_f64; 2 * p]; p];
    let mut b = vec![0.0_f64; p];
    for (r, yi) in rows.iter().zip(y) {
        for i in 0..p {
            let xi = r[i] / scale[i];
            b[i] += xi * yi;
            for j in i..p {
                a[i][j] += xi * (r[j] / scale[j]);
            }
        }
    }
    for i in 0..p {
        for j in 0..i {
            a[i][j] = a[j][i];
        }
        a[i][p + i] = 1.0;
    }
    for col in 0..p {
        let pivot_row = (col..p).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot_row][col].abs() < 1e-10 {
            return None;
        }
        a.swap(col, pivot_row);
        let pivot = a[col][col];
        for v in a[col].iter_mut() {
            *v /= pivot;
        }
        for i in 0..p {
            if i != col {
                let f = a[i][col];
                if f != 0.0 {
                    for j in 0..2 * p {
                        let d = a[col][j];
                        a[i][j] -= f * d;
                    }
                }
            }
        }
    }
    let beta: Vec<f64> = (0..p)
        .map(|i| (0..p).map(|j| a[i][p + j] * b[j]).sum::<f64>() / scale[i])
        .collect();
    let inv_diag: Vec<f64> = (0..p)
        .map(|i| a[i][p + i] / (scale[i] * scale[i]))
        .collect();
    let rss: f64 = rows
        .iter()
        .zip(y)
        .map(|(r, yi)| (yi - r.iter().zip(&beta).map(|(x, b)| x * b).sum::<f64>()).powi(2))
        .sum();
    beta.iter()
        .all(|v| v.is_finite())
        .then_some((beta, inv_diag, rss))
}

/// The shared extinction fit over `n_nights` nights. A night with fewer than `FIT_NIGHT_MIN_POINTS` points, or one
/// that makes the design singular (one FWHM only), is dropped and reads `None`; `None` overall when no night is left
/// or the fit has no degrees of freedom. Airmass and FWHM are centred on their means inside (conditioning) and the
/// intercepts are returned in the original coordinates.
fn shared_extinction_fit(points: &[FitPoint], n_nights: usize) -> Option<SharedFit> {
    shared_extinction_fit_sided(points, n_nights, &vec![false; n_nights])
}

/// [`shared_extinction_fit`] with a pier-side column on every night `two_sided` marks (one k, an intercept per night, an
/// FWHM slope per night, and on a two-sided night a step between its sides, reported as `side_step`).
fn shared_extinction_fit_sided(
    points: &[FitPoint],
    n_nights: usize,
    two_sided: &[bool],
) -> Option<SharedFit> {
    let mut counts = vec![0usize; n_nights];
    for p in points {
        counts[p.night] += 1;
    }
    let mut keep: Vec<bool> = counts.iter().map(|c| *c >= FIT_NIGHT_MIN_POINTS).collect();
    loop {
        let used: Vec<&FitPoint> = points.iter().filter(|p| keep[p.night]).collect();
        let slots: Vec<usize> = (0..n_nights).filter(|j| keep[*j]).collect();
        if slots.is_empty() || used.is_empty() {
            return None;
        }
        let n = used.len() as f64;
        let mx = used.iter().map(|p| p.airmass).sum::<f64>() / n;
        let mf = used.iter().map(|p| p.fwhm).sum::<f64>() / n;
        // The first column of each night: intercept, FWHM slope, then the side step where there is one.
        let mut offsets = Vec::with_capacity(slots.len());
        let mut width = 1;
        for night in &slots {
            offsets.push(width);
            width += 2 + usize::from(two_sided.get(*night).copied().unwrap_or(false));
        }
        let rows: Vec<Vec<f64>> = used
            .iter()
            .map(|p| {
                let mut r = vec![0.0; width];
                r[0] = p.airmass - mx;
                let j = slots.iter().position(|s| *s == p.night).unwrap_or(0);
                r[offsets[j]] = 1.0;
                r[offsets[j] + 1] = p.fwhm - mf;
                if two_sided.get(p.night).copied().unwrap_or(false) {
                    r[offsets[j] + 2] = p.side;
                }
                r
            })
            .collect();
        let y: Vec<f64> = used.iter().map(|p| p.zp).collect();
        match least_squares(&rows, &y) {
            Some((beta, inv_diag, rss)) => {
                let dof = used.len() - width;
                if dof == 0 {
                    return None;
                }
                let var = rss / dof as f64;
                let k = beta[0];
                let mut nights: Vec<Option<NightFit>> = vec![None; n_nights];
                for (j, night) in slots.iter().enumerate() {
                    let mine: Vec<f64> = used
                        .iter()
                        .filter(|p| p.night == *night)
                        .map(|p| p.fwhm)
                        .collect();
                    let slope = beta[offsets[j] + 1];
                    nights[*night] = Some(NightFit {
                        intercept: beta[offsets[j]] - k * mx - slope * mf,
                        slope,
                        slope_se: (var * inv_diag[offsets[j] + 1]).sqrt(),
                        n: mine.len(),
                        fwhm_min: mine.iter().copied().fold(f64::INFINITY, f64::min),
                        fwhm_max: mine.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                        side_step: two_sided
                            .get(*night)
                            .copied()
                            .unwrap_or(false)
                            .then(|| beta[offsets[j] + 2]),
                    });
                }
                return Some(SharedFit {
                    k,
                    k_se: (var * inv_diag[0]).sqrt(),
                    nights,
                    n: used.len(),
                    dof,
                });
            }
            None => {
                // Drop the night whose FWHM varies least and try again; the fit may still carry the others.
                let worst = slots
                    .iter()
                    .copied()
                    .map(|night| {
                        let f: Vec<f64> = used
                            .iter()
                            .filter(|p| p.night == night)
                            .map(|p| p.fwhm)
                            .collect();
                        let m = f.iter().sum::<f64>() / f.len() as f64;
                        (night, f.iter().map(|v| (v - m).powi(2)).sum::<f64>())
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))?;
                keep[worst.0] = false;
            }
        }
    }
}

/// A night judged under `--osc-c`: its set, its date and its measured frames in time order.
struct OscNight<'a> {
    rig: String,
    object: String,
    night: String,
    rows: Vec<&'a MeasureRow>,
}

impl OscNight<'_> {
    fn label(&self) -> String {
        format!("{}/{}", self.object, self.night)
    }
}

/// The rig of a row: the TSV's column, the filter for an older TSV.
fn rig_of(r: &MeasureRow) -> String {
    if r.rig.is_empty() {
        r.filter.clone()
    } else {
        r.rig.clone()
    }
}

/// Every night of at least `OSC_NIGHT_MIN_FRAMES` measured frames, by rig, object and night, in time order.
fn osc_nights(rows: &[MeasureRow]) -> Vec<OscNight<'_>> {
    let mut by: BTreeMap<(String, String, String), Vec<&MeasureRow>> = BTreeMap::new();
    for r in rows.iter().filter(|r| !r.is_unmeasured()) {
        by.entry((rig_of(r), r.object.clone(), r.night.clone()))
            .or_default()
            .push(r);
    }
    by.into_iter()
        .filter(|(_, v)| v.len() >= OSC_NIGHT_MIN_FRAMES)
        .map(|((rig, object, night), mut rows)| {
            rows.sort_by(|a, b| {
                a.t_s()
                    .unwrap_or(f64::MAX)
                    .total_cmp(&b.t_s().unwrap_or(f64::MAX))
            });
            OscNight {
                rig,
                object,
                night,
                rows,
            }
        })
        .collect()
}

/// Median `|Δ ln c|` over the consecutive frames of a night that both have a `c`.
fn c_steadiness(rows: &[&MeasureRow]) -> (Option<f64>, usize) {
    let mut frames: Vec<(f64, f64)> = rows
        .iter()
        .filter_map(|r| Some((r.t_s()?, r.c.filter(|c| *c > 0.0)?.ln())))
        .collect();
    frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut d: Vec<f64> = frames
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 < PAIR_MAX_GAP_S)
        .map(|w| (w[1].1 - w[0].1).abs())
        .collect();
    let pairs = d.len();
    (median(&mut d), pairs)
}

/// A pair of consecutive frames of the airmass-free mechanism.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MechPair {
    /// Δ zero point without and with `c`, and `2.5 log10` of the pair's common-flux ratio (per second of exposure).
    dzp: f64,
    dzp_c: f64,
    dflux: f64,
    dfwhm: f64,
}

/// The consecutive pairs (under 15 minutes apart) of a night's frames that have a zero point with and without `c`, a
/// FWHM and a common flux in the `--common` join (by frame id, else the local file name).
fn mechanism_pairs(night: &[&MeasureRow], common: &[CommonRow]) -> Vec<MechPair> {
    let by_id: HashMap<i64, &CommonRow> = common
        .iter()
        .filter_map(|c| Some((c.frame_id?, c)))
        .collect();
    let by_name: HashMap<&str, &CommonRow> =
        common.iter().map(|c| (name_key(&c.file_name), c)).collect();
    // time, zp, zp_c, fwhm, 2.5 log10(common flux / exposure)
    let mut frames: Vec<(f64, f64, f64, f64, f64)> = night
        .iter()
        .filter_map(|r| {
            let c = r
                .frame_id
                .and_then(|id| by_id.get(&id).copied())
                .or_else(|| by_name.get(r.file_name.as_str()).copied())?;
            let flux = c.green_tflux()?;
            let t = r.exptime.filter(|t| *t > 0.0)?;
            Some((
                r.t_s()?,
                r.zero_point?,
                r.zero_point_c?,
                r.median_fwhm?,
                2.5 * (flux / t).log10(),
            ))
        })
        .collect();
    frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    frames
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 < PAIR_MAX_GAP_S)
        .map(|w| MechPair {
            dzp: w[1].1 - w[0].1,
            dzp_c: w[1].2 - w[0].2,
            dflux: w[1].4 - w[0].4,
            dfwhm: w[1].3 - w[0].3,
        })
        .collect()
}

/// The slope of the mechanism residual `Δzp − Δ(2.5 log10 flux)` on `ΔFWHM` (with an intercept), its standard error,
/// the residual's RMS about its median, and the pair count; `None` under 20 pairs or with one ΔFWHM.
fn mechanism_stats(pairs: &[MechPair], corrected: bool) -> Option<(f64, f64, f64, usize)> {
    if pairs.len() < SEEING_MIN_PAIRS {
        return None;
    }
    let resid: Vec<f64> = pairs
        .iter()
        .map(|p| (if corrected { p.dzp_c } else { p.dzp }) - p.dflux)
        .collect();
    let n = pairs.len() as f64;
    let mx = pairs.iter().map(|p| p.dfwhm).sum::<f64>() / n;
    let my = resid.iter().sum::<f64>() / n;
    let sxx: f64 = pairs.iter().map(|p| (p.dfwhm - mx).powi(2)).sum();
    if sxx <= 1e-12 {
        return None;
    }
    let sxy: f64 = pairs
        .iter()
        .zip(&resid)
        .map(|(p, r)| (p.dfwhm - mx) * (r - my))
        .sum();
    let slope = sxy / sxx;
    let rss: f64 = pairs
        .iter()
        .zip(&resid)
        .map(|(p, r)| (r - my - slope * (p.dfwhm - mx)).powi(2))
        .sum();
    let se = (rss / (n - 2.0) / sxx).sqrt();
    let mut sorted = resid.clone();
    let m = median(&mut sorted)?;
    let rms = (resid.iter().map(|r| (r - m).powi(2)).sum::<f64>() / n).sqrt();
    Some((slope, se, rms, pairs.len()))
}

/// The verdict of `verdict --osc-c`: per rig the shared extinction fit, per night each bar of the OSC spec section 6,
/// each bar one line (a night's entry per bar), the last line the class line. The airmass-free mechanism needs the
/// `--common` join; without it those two bars are unjudged.
fn osc_c_verdict_lines(all_rows: &[MeasureRow], common: Option<&[CommonRow]>) -> Vec<String> {
    let nights = osc_nights(all_rows);
    let measured = all_rows.iter().filter(|r| !r.is_unmeasured()).count();
    let mut lines = vec![format!(
        "ZP PROBE osc-c: {measured} frames measured, {} judged nights (>= {OSC_NIGHT_MIN_FRAMES} measured frames each)",
        nights.len()
    )];
    let mut bars: Vec<(&str, Outcome)> = Vec::new();

    // The shared fits, per rig: corrected and uncorrected on the same frames.
    let mut fit_c: Vec<Option<NightFit>> = vec![None; nights.len()];
    let mut fit_u: Vec<Option<NightFit>> = vec![None; nights.len()];
    let rigs: std::collections::BTreeSet<String> = nights.iter().map(|n| n.rig.clone()).collect();
    for rig in &rigs {
        let members: Vec<usize> = (0..nights.len())
            .filter(|i| &nights[*i].rig == rig)
            .collect();
        let mut points_c = Vec::new();
        let mut points_u = Vec::new();
        let mut no_airmass = 0usize;
        for (slot, i) in members.iter().enumerate() {
            for r in &nights[*i].rows {
                let (Some(zp), Some(zpc), Some(fwhm)) =
                    (r.zero_point, r.zero_point_c, r.median_fwhm)
                else {
                    continue;
                };
                let Some(x) = r.airmass else {
                    no_airmass += 1;
                    continue;
                };
                points_c.push(FitPoint {
                    side: 0.0,
                    night: slot,
                    airmass: x,
                    fwhm,
                    zp: zpc,
                });
                points_u.push(FitPoint {
                    side: 0.0,
                    night: slot,
                    airmass: x,
                    fwhm,
                    zp,
                });
            }
        }
        let fc = shared_extinction_fit(&points_c, members.len());
        let fu = shared_extinction_fit(&points_u, members.len());
        lines.push(match (&fc, &fu) {
            (Some(c), Some(u)) => format!(
                "rig {rig}: extinction k {:.4} +- {:.4} mag/airmass with c ({:.4} +- {:.4} without), {} frames in {} nights, {} without an airmass",
                c.k,
                c.k_se,
                u.k,
                u.k_se,
                c.n,
                members.len(),
                no_airmass
            ),
            _ => format!("rig {rig}: shared fit not possible ({} frames with an airmass, {no_airmass} without)", points_c.len()),
        });
        for (slot, i) in members.iter().enumerate() {
            fit_c[*i] = fc.as_ref().and_then(|f| f.nights[slot]);
            fit_u[*i] = fu.as_ref().and_then(|f| f.nights[slot]);
        }
    }

    let per_night = |f: &dyn Fn(usize, &OscNight) -> (String, Outcome)| -> (String, Vec<Outcome>) {
        let mut parts = Vec::new();
        let mut outs = Vec::new();
        for (i, n) in nights.iter().enumerate() {
            let (text, o) = f(i, n);
            parts.push(format!("{}: {text} {}", n.label(), tag(o)));
            outs.push(o);
        }
        (
            if parts.is_empty() {
                "no judged night".to_string()
            } else {
                parts.join("; ")
            },
            outs,
        )
    };
    // Fail wins over unjudged, which wins over pass.
    let strict = |outs: &[Outcome]| {
        if outs.contains(&Outcome::Fail) {
            Outcome::Fail
        } else if outs.contains(&Outcome::Unjudged) || outs.is_empty() {
            Outcome::Unjudged
        } else {
            Outcome::Pass
        }
    };

    // c measured
    let (text, outs) = per_night(&|_, n| {
        let analysed = n.rows.iter().filter(|r| r.stars_detected.is_some()).count();
        let with_c = n
            .rows
            .iter()
            .filter(|r| r.stars_detected.is_some() && r.c.is_some())
            .count();
        let share = (analysed > 0).then(|| with_c as f64 / analysed as f64);
        let o = judge_max(share.map(|s| C_MEASURED_MIN - s), 0.0);
        (format!("{} ({with_c}/{analysed})", show(share, 3)), o)
    });
    let o = strict(&outs);
    lines.push(format!(
        "c measured (limit >= {C_MEASURED_MIN:.2}) {}: {text}",
        tag(o)
    ));
    bars.push(("c_measured", o));

    // c's own error
    let (text, outs) = per_night(&|_, n| {
        let mut rel: Vec<f64> = n.rows.iter().filter_map(|r| Some(r.c_se? / r.c?)).collect();
        let m = median(&mut rel);
        (show(m, 4), judge_max(m, C_SE_REL_MAX))
    });
    let o = strict(&outs);
    lines.push(format!(
        "c error, median se/c (limit <= {C_SE_REL_MAX:.2}) {}: {text}",
        tag(o)
    ));
    bars.push(("c_error", o));

    // c steadiness
    let (text, outs) = per_night(&|_, n| {
        let (m, pairs) = c_steadiness(&n.rows);
        let m = (pairs >= C_STEADY_MIN_PAIRS).then_some(m).flatten();
        (
            format!("{} ({pairs} pairs)", show(m, 4)),
            judge_max(m, C_STEADY_MAX),
        )
    });
    let o = strict(&outs);
    lines.push(format!(
        "c steadiness, median |d ln c| (limit <= {C_STEADY_MAX:.3}) {}: {text}",
        tag(o)
    ));
    bars.push(("c_steadiness", o));

    // seeing: the corrected slope on each night the fit judges
    let seeing_outcome = |f: &Option<NightFit>| match f {
        None => Outcome::Unjudged,
        Some(f) if f.slope_se > SLOPE_SE_MAX => Outcome::Unjudged,
        Some(f) => judge_max(Some(f.slope.abs()), SLOPE_MAX),
    };
    let slope_text = |f: &Option<NightFit>| match f {
        None => "no fit".to_string(),
        Some(f) => format!(
            "{:+.4} +- {:.4} (FWHM {:.2}-{:.2}, {} frames)",
            f.slope, f.slope_se, f.fwhm_min, f.fwhm_max, f.n
        ),
    };
    let (text, outs) = per_night(&|i, _| (slope_text(&fit_c[i]), seeing_outcome(&fit_c[i])));
    let judged = outs.iter().filter(|o| **o != Outcome::Unjudged).count();
    let o = if outs.contains(&Outcome::Fail) {
        Outcome::Fail
    } else if judged == 0 {
        Outcome::Unjudged
    } else {
        Outcome::Pass
    };
    lines.push(format!(
        "seeing, slope with c, mag/px (limit |s| <= {SLOPE_MAX:.2}, judged when SE <= {SLOPE_SE_MAX:.3}; {judged} of {} judged) {}: {text}",
        outs.len(),
        tag(o)
    ));
    bars.push(("seeing", o));

    // the uncorrected slope, for information, and "c does the work"
    let info: Vec<String> = nights
        .iter()
        .enumerate()
        .map(|(i, n)| format!("{}: {}", n.label(), slope_text(&fit_u[i])))
        .collect();
    lines.push(format!(
        "seeing, slope without c (information): {}",
        if info.is_empty() {
            "no judged night".to_string()
        } else {
            info.join("; ")
        }
    ));
    let mut work_texts = Vec::new();
    let mut work_outs = Vec::new();
    for (i, n) in nights.iter().enumerate() {
        let (Some(c), Some(u)) = (fit_c[i], fit_u[i]) else {
            continue;
        };
        if seeing_outcome(&fit_c[i]) == Outcome::Unjudged || u.slope.abs() <= WORK_SLOPE_U_MIN {
            continue;
        }
        let gap = (c.slope - u.slope).abs();
        let err = (c.slope_se.powi(2) + u.slope_se.powi(2)).sqrt();
        let o = if gap >= WORK_SIGMAS * err {
            Outcome::Pass
        } else {
            Outcome::Fail
        };
        work_texts.push(format!(
            "{}: {:.4} apart = {:.1} errors {}",
            n.label(),
            gap,
            gap / err,
            tag(o)
        ));
        work_outs.push(o);
    }
    let o = if work_outs.contains(&Outcome::Fail) {
        Outcome::Fail
    } else if nights.is_empty() {
        Outcome::Unjudged
    } else {
        Outcome::Pass
    };
    lines.push(if nights.is_empty() {
        "c does the work: no judged night, unjudged".to_string()
    } else if work_texts.is_empty() {
        format!("c does the work (>= {WORK_SIGMAS:.0} errors apart where |s without c| > {WORK_SLOPE_U_MIN:.2}) pass: no judged night is beyond {WORK_SLOPE_U_MIN:.2}, vacuous")
    } else {
        format!(
            "c does the work (>= {WORK_SIGMAS:.0} errors apart where |s without c| > {WORK_SLOPE_U_MIN:.2}) {}: {}",
            tag(o),
            work_texts.join("; ")
        )
    });
    bars.push(("c_work", o));

    // the airmass-free mechanism
    match common {
        None => {
            lines.push("mechanism: not requested (no --common), unjudged".to_string());
            bars.push(("mechanism_slope", Outcome::Unjudged));
            bars.push(("mechanism_rms", Outcome::Unjudged));
        }
        Some(c) => {
            let mut slope_parts = Vec::new();
            let mut rms_parts = Vec::new();
            let (mut slope_outs, mut rms_outs) = (Vec::new(), Vec::new());
            for n in &nights {
                let pairs = mechanism_pairs(&n.rows, c);
                let (Some(mc), Some(mu)) = (
                    mechanism_stats(&pairs, true),
                    mechanism_stats(&pairs, false),
                ) else {
                    continue;
                };
                let os = judge_max(Some(mc.0.abs()), SLOPE_MAX);
                let or = judge_max(Some(mc.2 - mu.2), 0.0);
                slope_parts.push(format!(
                    "{}: with c {:+.4} +- {:.4}, without {:+.4} +- {:.4}, {} pairs {}",
                    n.label(),
                    mc.0,
                    mc.1,
                    mu.0,
                    mu.1,
                    mc.3,
                    tag(os)
                ));
                rms_parts.push(format!(
                    "{}: with c {:.4}, without {:.4} {}",
                    n.label(),
                    mc.2,
                    mu.2,
                    tag(or)
                ));
                slope_outs.push(os);
                rms_outs.push(or);
            }
            let o_slope = strict(&slope_outs);
            let o_rms = strict(&rms_outs);
            lines.push(format!(
                "mechanism slope on dFWHM, airmass-free (limit |s| <= {SLOPE_MAX:.2}) {}: {}",
                tag(o_slope),
                if slope_parts.is_empty() {
                    "no night with 20 joined pairs".to_string()
                } else {
                    slope_parts.join("; ")
                }
            ));
            lines.push(format!(
                "mechanism rms about the median (with c not above without) {}: {}",
                tag(o_rms),
                if rms_parts.is_empty() {
                    "no night with 20 joined pairs".to_string()
                } else {
                    rms_parts.join("; ")
                }
            ));
            bars.push(("mechanism_slope", o_slope));
            bars.push(("mechanism_rms", o_rms));
        }
    }

    // stability with c, and without for information
    let (text, outs) = per_night(&|_, n| {
        let with = consecutive_pairs_of(&n.rows, |r| r.zero_point_c);
        let without = consecutive_pairs_of(&n.rows, |r| r.zero_point);
        let s = (with.len() >= STABILITY_MIN_PAIRS)
            .then(|| stability(&with))
            .flatten();
        let u = (without.len() >= STABILITY_MIN_PAIRS)
            .then(|| stability(&without))
            .flatten();
        (
            format!(
                "{} mag ({} pairs; without c {})",
                show(s, 4),
                with.len(),
                show(u, 4)
            ),
            judge_max(s, STABILITY_MAX_MAG),
        )
    });
    let o = strict(&outs);
    lines.push(format!(
        "stability with c, median |dzp| (limit <= {STABILITY_MAX_MAG:.2}) {}: {text}",
        tag(o)
    ));
    bars.push(("stability", o));

    // coverage
    let (text, outs) = per_night(&|_, n| {
        let valid = n.rows.iter().filter(|r| r.zero_point_c.is_some()).count();
        let share = (!n.rows.is_empty()).then(|| valid as f64 / n.rows.len() as f64);
        (
            format!("{} ({valid}/{})", show(share, 3), n.rows.len()),
            judge_max(share.map(|s| COVERAGE_MIN - s), 0.0),
        )
    });
    let o = strict(&outs);
    lines.push(format!(
        "coverage, zero points with c over measured frames (limit >= {COVERAGE_MIN:.2}) {}: {text}",
        tag(o)
    ));
    bars.push(("coverage", o));

    lines.push("identity (the new path against FromPlane, bit-identical): judged by rustafits' tests, not by this probe".to_string());
    lines.push(class_line("osc-c", &bars));
    lines
}
// ---------------------------------------------------------------------------
// Frames from the catalogue
// ---------------------------------------------------------------------------

struct Candidate {
    frame_id: i64,
    path: String,
    in_zip: bool,
    object: String,
    filter: String,
    date_obs: String,
    exptime: Option<f64>,
    night: String,
    has_solve: bool,
    /// The rig the frame was taken with (the TSV's `rig` column).
    rig: String,
    sitelat: Option<f64>,
    sitelong: Option<f64>,
}

const CANDIDATE_COLUMNS: &str =
    "f.id, fi.path, fi.archive_zip_path IS NOT NULL, f.object, f.filter, f.date_obs, f.exptime, \
     EXISTS (SELECT 1 FROM plate_solves p WHERE p.frame_id = f.id), \
     f.instrume, f.telescop, f.focallen, f.gain, f.xbinning, f.ybinning, f.sitelat, f.sitelong";

/// Index of the first column after [`CANDIDATE_COLUMNS`] in a query that appends its own.
const CANDIDATE_COLUMN_COUNT: usize = 16;

fn rig_label(r: &rusqlite::Row<'_>, filter: &str) -> rusqlite::Result<String> {
    let text = |i: usize| -> rusqlite::Result<String> {
        Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
    };
    let number = |i: usize| -> rusqlite::Result<String> {
        Ok(r.get::<_, Option<f64>>(i)?
            .map(|v| v.to_string())
            .unwrap_or_default())
    };
    Ok(format!(
        "{}|{}|{}|g{}|{}x{}|{}",
        text(8)?,
        text(9)?,
        number(10)?,
        number(11)?,
        number(12)?,
        number(13)?,
        if filter.is_empty() { "-" } else { filter },
    ))
}

fn candidate_from(
    r: &rusqlite::Row<'_>,
    night_start: Option<String>,
) -> rusqlite::Result<Candidate> {
    let date_obs: String = r.get::<_, Option<String>>(5)?.unwrap_or_default();
    let filter = r.get::<_, Option<String>>(4)?.unwrap_or_default();
    Ok(Candidate {
        rig: rig_label(r, &filter)?,
        sitelat: r.get(14)?,
        sitelong: r.get(15)?,
        frame_id: r.get(0)?,
        path: r.get(1)?,
        in_zip: r.get(2)?,
        object: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        filter,
        night: night_label(night_start.as_deref(), &date_obs),
        date_obs,
        exptime: r.get(6)?,
        has_solve: r.get(7)?,
    })
}

/// The lights of one rig, object and filter, in `date_obs` order, a second copy
/// of a frame the set holds left out (as `analyze_frame_set` does), each with
/// its catalogue night.
fn load_candidates(
    conn: &Connection,
    instrume: &str,
    object: &str,
    filter: &str,
) -> Result<Vec<Candidate>> {
    let no_filter = filter == "-";
    let sql = format!(
        "SELECT {CANDIDATE_COLUMNS}, n.start_time
         FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         INNER JOIN session_members sm ON sm.frame_id = f.id
         INNER JOIN sessions s ON s.id = sm.session_id
         INNER JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE f.imagetyp = 'Light' AND f.instrume = ?1 AND f.object = ?2 AND {filter_cond}
           AND {used_copy}
         ORDER BY f.date_obs, f.id, n.id",
        filter_cond = if no_filter {
            "COALESCE(f.filter, '') = ''"
        } else {
            "f.filter = ?3"
        },
        used_copy = athenaeum_core::db::frame_is_used_copy_sql("n.frames_set_id"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row<'_>| {
        candidate_from(r, r.get::<_, Option<String>>(CANDIDATE_COLUMN_COUNT)?)
    };
    let rows: Vec<Candidate> = if no_filter {
        stmt.query_map(rusqlite::params![instrume, object], map)?
            .collect::<Result<_, _>>()?
    } else {
        stmt.query_map(rusqlite::params![instrume, object, filter], map)?
            .collect::<Result<_, _>>()?
    };
    // A frame the catalogue holds in two nights appears twice: the first stays.
    let mut seen = HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|c| seen.insert(c.frame_id))
        .collect())
}

fn load_candidate_by_id(conn: &Connection, frame_id: i64) -> Result<Candidate> {
    let sql = format!(
        "SELECT {CANDIDATE_COLUMNS} FROM frames f INNER JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1"
    );
    conn.query_row(&sql, [frame_id], |r| candidate_from(r, None))
        .with_context(|| format!("frame {frame_id} not found"))
}

struct Rig {
    layers: Vec<Arc<StarCache>>,
    config: AnalysisConfig,
    config_hash: String,
    pool: Arc<rayon::ThreadPool>,
    params: ZpParams,
}

fn open_rig(conn: &Connection, app_data: &str, params: ZpParams) -> Result<Rig> {
    let layers = open_catalog_layers(Path::new(app_data))
        .map_err(|e| anyhow!("opening the star catalogue under {app_data}: {e}"))?;
    let config = load_config(conn);
    let config_hash = config.config_hash();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()?,
    );
    Ok(Rig {
        layers,
        config,
        config_hash,
        pool,
        params,
    })
}

/// The row of a candidate with no measurement yet.
fn base_row(c: &Candidate) -> MeasureRow {
    MeasureRow {
        frame_id: Some(c.frame_id),
        file_name: Path::new(&c.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| c.path.clone()),
        object: c.object.clone(),
        filter: if c.filter.is_empty() {
            "-".into()
        } else {
            c.filter.clone()
        },
        night: c.night.clone(),
        date_obs: c.date_obs.clone(),
        exptime: c.exptime,
        rig: c.rig.clone(),
        ..MeasureRow::default()
    }
}

/// The row of a light that was not measured: `code` (one of `UNMEASURED`) in
/// `refusal`, every measurement column empty.
fn unmeasured_row(c: &Candidate, code: &str) -> MeasureRow {
    MeasureRow {
        refusal: code.to_string(),
        ..base_row(c)
    }
}

/// The star fits of a frame's measurement, kept for the `stars` dump: they are
/// in the order of the analysis stars `ZpStar::star_index` indexes.
struct FrameFits {
    fits: Vec<StarFit>,
    width: usize,
    height: usize,
}

/// One frame, end to end: the stored solve, an in-memory analysis of its file,
/// the catalogue cone and the measurement. A failure is the row's `refusal`,
/// logged on stderr; it never stops the run. With `keep_fits` the analysis goes
/// through `measure_raw_frame` and `map_measurement` (what `analyze_frame` does)
/// so the plane measurement's fits stay in hand.
fn measure_candidate(
    conn: &Connection,
    rig: &Rig,
    c: &Candidate,
    keep_fits: bool,
) -> (MeasureRow, Option<ZpMeasurement>, Option<FrameFits>) {
    let mut row = base_row(c);
    let fail = |row: &mut MeasureRow, code: &str, detail: &dyn std::fmt::Display| {
        eprintln!("zp_probe: frame {} refused {code}: {detail}", c.frame_id);
        row.refusal = code.to_string();
    };

    let wcs = match get_plate_solve(conn, c.frame_id) {
        Ok(Some(rec)) => match rec.to_solution() {
            Some(w) => w,
            None => {
                fail(&mut row, "bad_wcs", &"the stored SIP tables are unusable");
                return (row, None, None);
            }
        },
        Ok(None) => {
            fail(&mut row, "no_solve", &"no stored solve");
            return (row, None, None);
        }
        Err(e) => {
            fail(&mut row, "bad_wcs", &e);
            return (row, None, None);
        }
    };
    // Read, measure and map: what `analyze_frame` does, taken apart so the plane measurement stays in hand (the
    // fits for `stars`, the mosaic's reported `c` for `measure`).
    let analysed = read_raw_frame(Path::new(&c.path)).map(|mut frame| {
        frame.drop_unmeasured_planes();
        let mosaic = matches!(frame.planes, RawPlanes::Cfa { .. });
        let mut pm = measure_raw_frame(&frame, &rig.config, Some(&rig.pool));
        let (analysis, stars) = map_measurement(&pm, &frame, &rig.config, &rig.config_hash);
        let cfa = mosaic.then_some((
            pm.reported_aperture_correction,
            pm.reported_aperture_stars,
            pm.reported_aperture_se,
        ));
        (
            analysis,
            stars,
            keep_fits.then(|| std::mem::take(&mut pm.stars)),
            cfa,
        )
    });
    let (analysis, stars, fits, cfa) = match analysed {
        Ok(t) => t,
        Err(e) => {
            fail(&mut row, "analysis_failed", &e);
            return (row, None, None);
        }
    };
    row.stars_detected = Some(analysis.stars_detected);
    row.median_fwhm = Some(analysis.median_fwhm);
    let (width, height) = (
        analysis.width.max(0) as usize,
        analysis.height.max(0) as usize,
    );
    if let Some((c_value, c_stars, c_se)) = cfa {
        row.c = c_value;
        row.c_stars = Some(c_stars as i64);
        row.c_se = c_se;
        // The airmass of the solve centre at the middle of the exposure; no site, no airmass.
        let (ra, dec) = wcs.pixel_to_sky((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0);
        row.airmass = match (c.sitelat, c.sitelong, parse_time(&c.date_obs)) {
            (Some(lat), Some(lon), Some(t)) => {
                let mid = t.timestamp_millis() as f64 / 1000.0
                    + c.exptime.filter(|e| e.is_finite()).unwrap_or(0.0) / 2.0;
                airmass(ra, dec, mid, lat, lon)
            }
            _ => None,
        };
    }

    let frame = Frame {
        date_obs: parse_time(&c.date_obs),
        ..Frame::default()
    };
    let epoch = observation_epoch(&frame);
    let layers: Vec<&StarCache> = rig.layers.iter().map(|l| l.as_ref()).collect();
    let catalog = match catalog_for_frame(
        &layers,
        &wcs,
        width,
        height,
        rig.params.central_fraction,
        epoch,
    ) {
        Ok(cat) => cat,
        Err(e) => {
            fail(&mut row, "no_cone", &e);
            return (row, None, None);
        }
    };
    let m = measure_zero_point(
        &ZpInput {
            stars: &stars,
            wcs: &wcs,
            catalog: &catalog,
            exptime_s: c.exptime,
            width,
            height,
            median_fwhm_px: analysis.median_fwhm,
        },
        &rig.params,
    );
    row.matched = Some(m.matched);
    row.median_match_px = m.median_match_px;
    match &m.result {
        Ok(z) => {
            row.kept = Some(z.stars);
            row.zero_point = Some(z.zero_point);
            row.zero_point_c = row.c.map(|c| z.zero_point + 2.5 * c.log10());
            row.sigma = Some(z.sigma);
            row.std_error = Some(z.sigma / f64::from(z.stars.max(1)).sqrt());
        }
        Err(r) => {
            row.refusal = r.code().to_string();
            match r {
                ZpRefusal::NoExposure => {}
                ZpRefusal::TooFewStars { kept, .. } => row.kept = Some(*kept),
                ZpRefusal::TooScattered { std_error, kept } => {
                    row.kept = Some(*kept);
                    row.std_error = Some(*std_error);
                }
            }
        }
    }
    let kept = fits.map(|fits| FrameFits {
        fits,
        width,
        height,
    });
    (row, Some(m), kept)
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn open_read_only(db: &str) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {db} read-only"))
}

fn create_out(path: &str) -> Result<BufWriter<File>> {
    Ok(BufWriter::new(
        File::create(path).with_context(|| format!("creating {path}"))?,
    ))
}

fn cmd_measure(mut args: Args) -> Result<()> {
    let db = args.require("db")?;
    let app_data = args.require("app-data")?;
    let instrume = args.require("instrume")?;
    let filter = args.require("filter")?;
    let object = args.require("object")?;
    let out = args.require("out")?;
    let limit: Option<usize> = args.take_parsed("limit")?;
    let params = args.zp_params()?;
    args.finish()?;

    let conn = open_read_only(&db)?;
    let rig = open_rig(&conn, &app_data, params)?;
    let candidates = load_candidates(&conn, &instrume, &object, &filter)?;
    eprintln!(
        "zp_probe: {} lights of {instrume} / {object} / {filter} in the catalogue; params {params:?}",
        candidates.len()
    );

    let mut w = create_out(&out)?;
    writeln!(w, "{MEASURE_HEADER}")?;
    let (mut measured, mut no_solve, mut in_zip, mut missing, mut refused) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut examined = 0usize;
    let (mut mosaics, mut with_c, mut no_airmass) = (0usize, 0usize, 0usize);
    for c in &candidates {
        if limit.is_some_and(|n| measured >= n) {
            break;
        }
        examined += 1;
        if c.in_zip {
            in_zip += 1;
            eprintln!(
                "zp_probe: frame {} lives in a zip, not measured: {}",
                c.frame_id, c.path
            );
            writeln!(w, "{}", unmeasured_row(c, "in_zip").to_tsv())?;
            continue;
        }
        if !Path::new(&c.path).is_file() {
            missing += 1;
            eprintln!(
                "zp_probe: frame {} file missing, not measured: {}",
                c.frame_id, c.path
            );
            writeln!(w, "{}", unmeasured_row(c, "missing_file").to_tsv())?;
            continue;
        }
        if !c.has_solve {
            no_solve += 1;
            eprintln!(
                "zp_probe: frame {} has no stored solve, not measured: {}",
                c.frame_id, c.path
            );
            writeln!(w, "{}", unmeasured_row(c, "no_solve").to_tsv())?;
            continue;
        }
        let (row, _, _) = measure_candidate(&conn, &rig, c, false);
        if row.zero_point.is_none() {
            refused += 1;
        }
        if let Some(stars) = row.c_stars {
            mosaics += 1;
            with_c += usize::from(row.c.is_some());
            if stars > 0 && row.c.is_none() {
                eprintln!(
                    "zp_probe: frame {} reported {stars} stars but no c",
                    c.frame_id
                );
            }
            if row.airmass.is_none() {
                no_airmass += 1;
                eprintln!(
                    "zp_probe: frame {} has no site or time, airmass left empty",
                    c.frame_id
                );
            }
        }
        writeln!(w, "{}", row.to_tsv())?;
        w.flush()?;
        measured += 1;
        if measured % PROGRESS_EVERY == 0 {
            eprintln!("zp_probe: {measured} frames measured ({refused} without a zero point)");
        }
    }
    w.flush()?;
    eprintln!(
        "zp_probe summary: examined {examined} of {} lights, measured {measured} ({refused} without a zero point), \
         not measured: {no_solve} without a solve, {in_zip} in a zip, {missing} with a missing file; \
         mosaics {mosaics}, with a c {with_c}, without an airmass (no site) {no_airmass}; wrote {out}",
        candidates.len()
    );
    Ok(())
}

/// The fit's flag columns: `saturated`, `extended`, `flags`.
fn flag_fields(f: &StarFit) -> String {
    format!(
        "{}\t{}\t{}",
        u8::from(f.flags.saturated()),
        u8::from(f.flags.extended()),
        f.flags.names().join("+")
    )
}

fn cmd_stars(mut args: Args) -> Result<()> {
    let db = args.require("db")?;
    let app_data = args.require("app-data")?;
    let frame: i64 = args
        .take_parsed("frame")?
        .ok_or_else(|| anyhow!("missing --frame"))?;
    let out = args.require("out")?;
    let all_out = args.take("all-out");
    let params = args.zp_params()?;
    args.finish()?;

    let conn = open_read_only(&db)?;
    let rig = open_rig(&conn, &app_data, params)?;
    let c = load_candidate_by_id(&conn, frame)?;
    if c.in_zip || !Path::new(&c.path).is_file() {
        bail!("frame {frame}: file {} is in a zip or missing", c.path);
    }
    let (row, m, kept) = measure_candidate(&conn, &rig, &c, true);
    let mut w = create_out(&out)?;
    writeln!(
        w,
        "frame_id\tg_mag\tzp\tradius_fraction\tmatch_px\tkept\tsaturated\textended\tflags\tpeak\tbackground\tsnr\tfwhm\tflux"
    )?;
    let mut matched_idx: HashSet<usize> = HashSet::new();
    let mut matched_saturated = 0usize;
    if let (Some(m), Some(k)) = (&m, &kept) {
        for s in &m.per_star {
            let f = k.fits.get(s.star_index).ok_or_else(|| {
                anyhow!(
                    "star_index {} outside the {} fits",
                    s.star_index,
                    k.fits.len()
                )
            })?;
            matched_idx.insert(s.star_index);
            matched_saturated += usize::from(f.flags.saturated());
            writeln!(
                w,
                "{frame}\t{:.3}\t{:.5}\t{:.4}\t{:.4}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.4}\t{:.3}",
                s.g_mag,
                s.zp,
                s.radius_fraction,
                s.match_px,
                u8::from(s.kept),
                flag_fields(f),
                f.amplitude,
                f.background,
                f.snr,
                f.fwhm(),
                f.signal
            )?;
        }
    }
    w.flush()?;

    if let Some(path) = &all_out {
        let mut aw = create_out(path)?;
        writeln!(
            aw,
            "star_index\tx\ty\tsaturated\textended\tflags\tpeak\tsnr\tfwhm\tflux\tmatched"
        )?;
        if let Some(k) = &kept {
            let centre = ((k.width as f64 - 1.0) / 2.0, (k.height as f64 - 1.0) / 2.0);
            let region =
                rig.params.central_fraction * 0.5 * (k.width as f64).hypot(k.height as f64);
            for (i, f) in k.fits.iter().enumerate() {
                if (f.x - centre.0).hypot(f.y - centre.1) > region {
                    continue;
                }
                writeln!(
                    aw,
                    "{i}\t{:.3}\t{:.3}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}",
                    f.x,
                    f.y,
                    flag_fields(f),
                    f.amplitude,
                    f.snr,
                    f.fwhm(),
                    f.signal,
                    u8::from(matched_idx.contains(&i))
                )?;
            }
        }
        aw.flush()?;
    }
    eprintln!(
        "zp_probe: frame {frame}: zero_point {} refusal {:?} matched {:?} kept {:?} matched_saturated {matched_saturated}; wrote {out}",
        num(row.zero_point, 4),
        row.refusal,
        row.matched,
        row.kept
    );
    Ok(())
}

const COMMON_HEADER: &str = "frame_id\tfile_name\tgroup_key\tresolved_by\tplanes\tcommon_tflux_0\tcommon_tflux_1\tcommon_tflux_2";

/// The local frame of a project run's frame: its `source_frame_id`, else the
/// frame of the file at its `landed_path`, else the one local file named like
/// the calibrated copy without its `c_` prefix. `None` when none gives exactly
/// one frame: it is not guessed.
fn resolve_project_frame(
    conn: &Connection,
    file_name: &str,
    source_frame_id: Option<i64>,
    landed_path: Option<&str>,
) -> Result<Option<(i64, &'static str)>> {
    if let Some(id) = source_frame_id {
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM frames WHERE id = ?1", [id], |r| {
            r.get(0)
        })?;
        if n == 1 {
            return Ok(Some((id, "source")));
        }
    }
    let one = |sql: &str, key: &str| -> Result<Option<i64>> {
        let mut stmt = conn.prepare(sql)?;
        let ids: Vec<i64> = stmt
            .query_map([key], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok((ids.len() == 1).then(|| ids[0]))
    };
    if let Some(p) = landed_path {
        if let Some(id) = one(
            "SELECT fr.id FROM files f JOIN frames fr ON fr.file_id = f.id WHERE f.path = ?1",
            p,
        )? {
            return Ok(Some((id, "landed_path")));
        }
    }
    if let Some(id) = one(
        "SELECT fr.id FROM files f JOIN frames fr ON fr.file_id = f.id WHERE f.filename = ?1",
        name_key(file_name),
    )? {
        return Ok(Some((id, "name")));
    }
    Ok(None)
}

fn cmd_common(mut args: Args) -> Result<()> {
    let db = args.require("db")?;
    let run: i64 = args
        .take_parsed("run")?
        .ok_or_else(|| anyhow!("missing --run"))?;
    let out = args.require("out")?;
    args.finish()?;

    let conn = open_read_only(&db)?;
    let mut stmt = conn.prepare(
        "SELECT r.frame_id, r.metrics_json, g.group_key, l.file_name, l.source_frame_id, l.landed_path,
                (SELECT fi.filename FROM frames fr JOIN files fi ON fi.id = fr.file_id WHERE fr.id = r.frame_id)
         FROM stacking_run_frames r
         JOIN stacking_run_groups g ON g.id = r.group_id
         LEFT JOIN stack_project_frames p ON p.id = r.project_frame
         LEFT JOIN project_frames_local l ON l.project_id = p.project_id AND l.frame_uuid = p.frame_uuid
         WHERE r.run_id = ?1
         ORDER BY g.group_key, r.id",
    )?;
    type RunRow = (
        Option<i64>,
        Option<String>,
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<RunRow> = stmt
        .query_map([run], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    if rows.is_empty() {
        bail!("run {run} has no frames");
    }
    let mut w = create_out(&out)?;
    writeln!(w, "{COMMON_HEADER}")?;
    let (mut unresolved, mut by_source, mut by_path, mut by_name, mut no_flux) = (0, 0, 0, 0, 0);
    for (own_frame, metrics, group_key, project_name, source_id, landed, set_name) in &rows {
        let file_name = project_name
            .clone()
            .or_else(|| set_name.clone())
            .unwrap_or_default();
        let (frame_id, how) = match own_frame {
            Some(id) => (Some(*id), "set_run"),
            None => {
                match resolve_project_frame(&conn, &file_name, *source_id, landed.as_deref())? {
                    Some((id, how)) => {
                        match how {
                            "source" => by_source += 1,
                            "landed_path" => by_path += 1,
                            _ => by_name += 1,
                        }
                        (Some(id), how)
                    }
                    None => {
                        unresolved += 1;
                        eprintln!("zp_probe: run {run} frame {file_name} has no local frame; frame_id left empty");
                        (None, "none")
                    }
                }
            }
        };
        let channels: Vec<Option<f64>> = metrics
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .and_then(|v| v.get("channels").and_then(|c| c.as_array().cloned()))
            .map(|ch| {
                ch.iter()
                    .map(|p| p.get("commonTflux").and_then(|x| x.as_f64()))
                    .collect()
            })
            .unwrap_or_default();
        if channels.iter().all(Option::is_none) {
            no_flux += 1;
        }
        let plane = |i: usize| num(channels.get(i).copied().flatten(), 6);
        writeln!(
            w,
            "{}\t{}\t{}\t{how}\t{}\t{}\t{}\t{}",
            frame_id.map(|v| v.to_string()).unwrap_or_default(),
            clean(&file_name),
            clean(group_key),
            channels.len(),
            plane(0),
            plane(1),
            plane(2)
        )?;
    }
    w.flush()?;
    eprintln!(
        "zp_probe summary: run {run}: {} frames; resolved by source id {by_source}, by landed path {by_path}, by calibrated-copy name {by_name}; \
         {unresolved} without a local frame; {no_flux} without a commonTflux; wrote {out}",
        rows.len()
    );
    Ok(())
}

fn cmd_verdict(mut args: Args) -> Result<()> {
    if args.take_switch("forced") {
        let common_path = args.take("common");
        args.finish()?;
        return cmd_verdict_forced(&args, common_path);
    }
    let class = args.require("class")?;
    if !["broadband", "narrowband", "osc"].contains(&class.as_str()) {
        bail!("--class must be broadband, narrowband or osc");
    }
    let common_path = args.take("common");
    let osc_c = args.take_switch("osc-c");
    args.finish()?;
    if osc_c && class != "osc" {
        bail!("--osc-c judges the colour class: use it with --class osc");
    }
    if args.positional.is_empty() {
        bail!("verdict needs at least one measure TSV");
    }
    let mut rows = Vec::new();
    for path in &args.positional {
        let (header, data) = read_tsv(path)?;
        rows.extend(data.iter().map(|f| MeasureRow::from_fields(&header, f)));
    }
    let common = match common_path {
        Some(p) => {
            let (header, data) = read_tsv(&p)?;
            let rows: Vec<CommonRow> = data
                .iter()
                .map(|f| CommonRow {
                    frame_id: opt_i64(col(&header, f, "frame_id")),
                    file_name: col(&header, f, "file_name").to_string(),
                    group_key: col(&header, f, "group_key").to_string(),
                    planes: col(&header, f, "planes").trim().parse().unwrap_or(0),
                    tflux: [
                        opt_f64(col(&header, f, "common_tflux_0")),
                        opt_f64(col(&header, f, "common_tflux_1")),
                        opt_f64(col(&header, f, "common_tflux_2")),
                    ],
                })
                .collect();
            Some(rows)
        }
        None => None,
    };
    let lines = if osc_c {
        osc_c_verdict_lines(&rows, common.as_deref())
    } else {
        verdict_lines(&class, &rows, common.as_deref())
    };
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// bigap: a plain large-aperture photometry of the raw mosaic (OSC Task 3c)
// ---------------------------------------------------------------------------

/// Aperture radii of `bigap`, in units of the frame's median FWHM.
const BIGAP_RADII_FWHM: [f64; 6] = [1.5, 2.0, 3.0, 4.0, 6.0, 8.0];
/// The sky annulus starts this far (px) outside the largest aperture.
const BIGAP_ANNULUS_GAP_PX: f64 = 3.0;
/// The sky annulus holds at least this many pixels (a quarter of them per CFA phase).
const BIGAP_ANNULUS_AREA_PX: f64 = 2400.0;
/// A neighbour fit brighter than this share of the star's model flux counts against its isolation.
const BIGAP_NEIGHBOUR_SHARE: f64 = 0.002;

/// Colour of a mosaic site, 0 red, 1 green, 2 blue: the pattern addresses the file's row order, as the mosaic does.
fn cfa_colour(pattern: astroimage::BayerPattern, x: usize, y: usize) -> usize {
    use astroimage::BayerPattern as B;
    let cells: [usize; 4] = match pattern {
        B::Rggb => [0, 1, 1, 2],
        B::Bggr => [2, 1, 1, 0],
        B::Gbrg => [1, 2, 0, 1],
        B::Grbg => [1, 0, 2, 1],
        B::None => [1, 1, 1, 1],
    };
    cells[(y & 1) * 2 + (x & 1)]
}

/// The per-star columns of `bigap`, after the fixed ones: per radius k of [`BIGAP_RADII_FWHM`] and CFA phase p
/// (`(y & 1) * 2 + (x & 1)`), the raw sum `sum_<k>_<p>` and the pixel count `n_<k>_<p>`; the analysis subtracts a sky.
fn bigap_star_header() -> String {
    let mut h = String::from(
        "frame_id\tstar_index\tra\tdec\tg_mag\tkept\tx\ty\tfit_fwhm\tmodel_flux\tzp_model\tpeak\tclip\t\
         med_p0\tmed_p1\tmed_p2\tmed_p3\tmean_p0\tmean_p1\tmean_p2\tmean_p3\tsky_green_mad\tn_sky\tnear6\tnear8\tnearest_px",
    );
    for prefix in ["sum", "n"] {
        for k in BIGAP_RADII_FWHM {
            for p in 0..4 {
                h.push_str(&format!("\t{prefix}_{k}_{p}"));
            }
        }
    }
    h
}

/// One star on the raw mosaic: raw sums and pixel counts per radius and CFA phase, and two continuous sky estimates per
/// phase (the annulus' grouped median and its 3-sigma-clipped mean).
struct BigapSums {
    /// Grouped (interpolated) median of the annulus per phase: continuous on integer ADU, where a plain median is not.
    med: [f64; 4],
    /// Mean of the annulus per phase, clipped at 3 sigma (MAD) about the median, three rounds.
    mean: [f64; 4],
    /// The green phases' pixel scatter about their medians (MAD x 1.4826).
    sky_green_mad: f64,
    /// Fewest annulus pixels of any phase.
    n_sky: usize,
    /// Raw sum per radius and phase, and the pixel count.
    sum: [[f64; 4]; 6],
    cnt: [[usize; 4]; 6],
    /// Highest raw value within 2 FWHM.
    peak: f64,
}

/// The median of integer-valued data as a grouped median, `m - 1/2 + (n/2 - below) / equal` (continuous in the
/// underlying level); a plain median when the data are not integers. Sorts `v`.
fn grouped_median(v: &mut [f64]) -> f64 {
    let Some(m) = median(v) else {
        return f64::NAN;
    };
    if v.iter().any(|x| x.fract() != 0.0) {
        return m;
    }
    let m = m.round();
    let below = v.iter().filter(|x| **x < m).count() as f64;
    let equal = v.iter().filter(|x| **x == m).count() as f64;
    if equal == 0.0 {
        return m;
    }
    m - 0.5 + (v.len() as f64 / 2.0 - below) / equal
}

/// The mean of `v` clipped at 3 sigma (1.4826 MAD) about the median, three rounds.
fn clipped_mean(v: &[f64]) -> f64 {
    let mut keep: Vec<f64> = v.to_vec();
    for _ in 0..3 {
        let mut s = keep.clone();
        let Some(m) = median(&mut s) else {
            return f64::NAN;
        };
        let mut dev: Vec<f64> = keep.iter().map(|x| (x - m).abs()).collect();
        let sig = median(&mut dev).unwrap_or(0.0) * 1.4826;
        if !(sig > 0.0) {
            break;
        }
        keep.retain(|x| (x - m).abs() <= 3.0 * sig);
    }
    if keep.is_empty() {
        f64::NAN
    } else {
        keep.iter().sum::<f64>() / keep.len() as f64
    }
}

/// One matched star on the raw mosaic. `None` when the annulus leaves the frame.
fn bigap_star(
    mosaic: &[f32],
    pattern: astroimage::BayerPattern,
    (w, h): (usize, usize),
    (x, y): (f64, f64),
    fwhm: f64,
) -> Option<BigapSums> {
    let r_max = BIGAP_RADII_FWHM[BIGAP_RADII_FWHM.len() - 1] * fwhm;
    let r_in = r_max + BIGAP_ANNULUS_GAP_PX;
    let r_out = (r_in * r_in + BIGAP_ANNULUS_AREA_PX / std::f64::consts::PI).sqrt();
    let (x0, x1) = ((x - r_out).floor(), (x + r_out).ceil());
    let (y0, y1) = ((y - r_out).floor(), (y + r_out).ceil());
    if x0 < 0.0 || y0 < 0.0 || x1 > (w - 1) as f64 || y1 > (h - 1) as f64 {
        return None;
    }
    let mut sky: [Vec<f64>; 4] = Default::default();
    let (mut sum, mut cnt) = ([[0.0f64; 4]; 6], [[0usize; 4]; 6]);
    let r2: Vec<f64> = BIGAP_RADII_FWHM
        .iter()
        .map(|k| (k * fwhm).powi(2))
        .collect();
    let (r_in2, r_out2) = (r_in * r_in, r_out * r_out);
    let peak_r2 = (2.0 * fwhm).powi(2);
    let mut peak = f64::NEG_INFINITY;
    for py in y0 as usize..=y1 as usize {
        let dy2 = (py as f64 - y).powi(2);
        for px in x0 as usize..=x1 as usize {
            let d2 = (px as f64 - x).powi(2) + dy2;
            let v = mosaic[py * w + px] as f64;
            if !v.is_finite() {
                continue;
            }
            let phase = (py & 1) * 2 + (px & 1);
            if d2 >= r_in2 && d2 <= r_out2 {
                sky[phase].push(v);
                continue;
            }
            for (k, rk2) in r2.iter().enumerate() {
                if d2 <= *rk2 {
                    sum[k][phase] += v;
                    cnt[k][phase] += 1;
                }
            }
            if d2 <= peak_r2 {
                peak = peak.max(v);
            }
        }
    }
    let n_sky = sky.iter().map(Vec::len).min().unwrap_or(0);
    let (mut med, mut mean) = ([f64::NAN; 4], [f64::NAN; 4]);
    for (p, s) in sky.iter_mut().enumerate() {
        mean[p] = clipped_mean(s);
        med[p] = grouped_median(s);
    }
    // The green sky's scatter (MAD of the green phases' pixels about their own medians), for the noise of a sum.
    let mut dev: Vec<f64> = Vec::new();
    for (p, s) in sky.iter().enumerate() {
        if cfa_colour(pattern, p & 1, p >> 1) == 1 {
            dev.extend(s.iter().map(|v| (v - med[p]).abs()));
        }
    }
    let mad = median(&mut dev).unwrap_or(f64::NAN) * 1.4826;
    Some(BigapSums {
        med,
        mean,
        sky_green_mad: mad,
        n_sky,
        sum,
        cnt,
        peak,
    })
}

/// `bigap`: per frame of one rig / object / filter (optionally one night, every n-th frame), the row `measure` writes
/// plus the solve's position angle, the clip level and the CFA colours of the four phases; per Gaia-matched star, the
/// raw mosaic's sums and pixel counts per CFA phase within 1.5 … 8 x the frame's median FWHM, the annulus sky per phase
/// outside the largest aperture (grouped median and clipped mean), the peak raw value within 2 F, the neighbour counts
/// an isolation test reads, and the star's sky position (the cross-frame join). The analysis subtracts the sky of each
/// site's own phase. The stars are the ones the zero point matched by sky position (`measure_zero_point`'s per-star
/// list), so a meridian flip cannot mix them up.
fn cmd_bigap(mut args: Args) -> Result<()> {
    let db = args.require("db")?;
    let app_data = args.require("app-data")?;
    let instrume = args.require("instrume")?;
    let filter = args.require("filter")?;
    let object = args.require("object")?;
    let night = args.take("night");
    let every: usize = args.take_parsed("every")?.unwrap_or(1).max(1);
    let out = args.require("out")?;
    let stars_out = args.require("stars-out")?;
    let params = args.zp_params()?;
    args.finish()?;

    let conn = open_read_only(&db)?;
    let rig = open_rig(&conn, &app_data, params)?;
    let candidates: Vec<Candidate> = load_candidates(&conn, &instrume, &object, &filter)?
        .into_iter()
        .filter(|c| night.as_deref().is_none_or(|n| c.night == n))
        .filter(|c| !c.in_zip && c.has_solve && Path::new(&c.path).is_file())
        .enumerate()
        .filter(|(i, _)| i % every == 0)
        .map(|(_, c)| c)
        .collect();
    eprintln!("zp_probe bigap: {} frames to measure", candidates.len());
    let mut w = create_out(&out)?;
    writeln!(w, "{MEASURE_HEADER}\tpa_deg\tclip\tpattern")?;
    let mut ws = create_out(&stars_out)?;
    writeln!(ws, "{}", bigap_star_header())?;
    let mut done = 0usize;
    for c in &candidates {
        let (row, m, fits) = measure_candidate(&conn, &rig, c, true);
        let (Some(m), Some(fits), Some(fwhm)) = (m, fits, row.median_fwhm) else {
            eprintln!(
                "zp_probe bigap: frame {} has no measurement, skipped",
                c.frame_id
            );
            continue;
        };
        let wcs = match get_plate_solve(&conn, c.frame_id)?.and_then(|r| r.to_solution()) {
            Some(wcs) => wcs,
            None => {
                eprintln!(
                    "zp_probe bigap: frame {} has no usable solve, skipped",
                    c.frame_id
                );
                continue;
            }
        };
        let frame = match read_raw_frame(Path::new(&c.path)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("zp_probe bigap: frame {} unreadable: {e:#}", c.frame_id);
                continue;
            }
        };
        let clip = match frame.domain {
            athenaeum_core::star_measure::Domain::RawAdu { clip, .. } => clip,
            _ => f64::NAN,
        };
        let RawPlanes::Cfa { mosaic, pattern } = &frame.planes else {
            eprintln!(
                "zp_probe bigap: frame {} is not a mosaic, skipped",
                c.frame_id
            );
            continue;
        };
        let pa = wcs.cd[1][0].atan2(wcs.cd[0][0]).to_degrees();
        let colours: String = (0..4)
            .map(|p| ['R', 'G', 'B'][cfa_colour(*pattern, p & 1, p >> 1)])
            .collect();
        writeln!(w, "{}\t{pa:.3}\t{clip:.1}\t{colours}", row.to_tsv())?;
        let r6 = 6.0 * fwhm + 2.0 * fwhm;
        let r8 = 8.0 * fwhm + 2.0 * fwhm;
        for s in &m.per_star {
            let Some(f) = fits.fits.get(s.star_index) else {
                bail!(
                    "star_index {} outside the {} fits",
                    s.star_index,
                    fits.fits.len()
                );
            };
            let Some(b) = bigap_star(mosaic, *pattern, (frame.w, frame.h), (f.x, f.y), fwhm) else {
                continue;
            };
            let (mut near6, mut near8, mut nearest) = (0usize, 0usize, f64::INFINITY);
            for (j, o) in fits.fits.iter().enumerate() {
                if j == s.star_index || !(o.x.is_finite() && o.y.is_finite()) {
                    continue;
                }
                let d = (o.x - f.x).hypot(o.y - f.y);
                if d > r8 {
                    continue;
                }
                nearest = nearest.min(d);
                if o.signal > BIGAP_NEIGHBOUR_SHARE * f.signal {
                    near8 += 1;
                    near6 += usize::from(d <= r6);
                }
            }
            let (ra, dec) = wcs.pixel_to_sky(f.x, f.y);
            let mut cols: Vec<String> = Vec::with_capacity(48);
            for row_k in &b.sum {
                cols.extend(row_k.iter().map(|v| format!("{v:.0}")));
            }
            for row_k in &b.cnt {
                cols.extend(row_k.iter().map(|v| v.to_string()));
            }
            writeln!(
                ws,
                "{}\t{}\t{ra:.6}\t{dec:.6}\t{:.3}\t{}\t{:.3}\t{:.3}\t{:.4}\t{:.3}\t{:.5}\t{:.1}\t{clip:.1}\t\
                 {:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.3}\t{}\t{near6}\t{near8}\t{nearest:.2}\t{}",
                c.frame_id,
                s.star_index,
                s.g_mag,
                u8::from(s.kept),
                f.x,
                f.y,
                f.fwhm(),
                f.signal,
                s.zp,
                b.peak,
                b.med[0],
                b.med[1],
                b.med[2],
                b.med[3],
                b.mean[0],
                b.mean[1],
                b.mean[2],
                b.mean[3],
                b.sky_green_mad,
                b.n_sky,
                cols.join("\t"),
            )?;
        }
        w.flush()?;
        ws.flush()?;
        done += 1;
        if done % PROGRESS_EVERY == 0 {
            eprintln!("zp_probe bigap: {done} frames measured");
        }
    }
    eprintln!(
        "zp_probe bigap summary: {done} of {} frames; wrote {out} and {stars_out}",
        candidates.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// forced: the shipping forced-list zero point on real frames (forced-list plan, G3)
// ---------------------------------------------------------------------------
//
// `forced` calls `analysis::analyzer::measure_with_zero_point`, the one function the analyzer ships, on a frame read in
// memory, and writes TSVs only: one row per light (`<x>.tsv`), one compact row per list star (`<x>.stars.tsv`), on every
// `--identity-every`-th frame the anchor-shifted re-measures (`<x>.shifts.tsv`) and, with `--dump-ap 1`, every aperture
// figure of every list star (`<x>.ap.tsv`). Today's zero point (6.5's matcher over the budget fits, times the reported
// `c` on a mosaic) is measured on the same frame for the comparisons of B2 and B8. The catalogue is opened read-only.

const FORCED_HEADER: &str = "frame_id\tfile_name\tobject\tfilter\tnight\tdate_obs\texptime\trig\tlayout\tairmass\tpier\trotation_deg\tstars_detected\tmedian_fwhm\tscale\tfwhm_ref\tanchor\tanchor_stars\twindow_bright\twindow_faint\tlist_before_cap\tlist_stars\tdetected_neighbours_removed\tseed_calibration_pairs\tmap_stars\tmap_members\tmap_offset_px\tmap_residual_px\tmap_kept_share\taperture_radius_px\tmembers\ttapered\tzp_stars\tpedestal\tpedestal_kept_share\tpedestal_zp_mag\tannulus_off\tc_budget\tzero_point\tsigma\trefusal\tcentre_median_px\twindow_slope\tslope_remeasured\tzero_point_6r\tzero_point_today\ttoday_refusal\tidentity\tcall_ms\tplain_ms\tlist_fit_ms\tlist_refit_ms\tlist_ap_ms\tlist_kb\trss_mb";

const FORCED_STARS_HEADER: &str = "frame_id\tkey\tg\tweight\tmember\tscreened_in";

const FORCED_SHIFTS_HEADER: &str =
    "frame_id\tshift\tzero_point\twindow_bright\twindow_faint\twindow_slope\tmembers\trefusal";

const FORCED_AP_HEADER: &str =
    "frame_id\tkey\tg\tx\ty\ton_frame\tsum\tsum_a\tsum_b\tsum_c\tsky\tsky_scatter\tpeak\tweight\tmember\tscreened_in";

/// Every n-th frame is measured a second time without a list and compared (B9), unless `--identity-every` says else; the
/// same frames carry the anchor-shifted re-measures.
const IDENTITY_EVERY_DEFAULT: usize = 10;

/// The anchor shifts (mag, both signs) of the identity frames' re-measures: +-0.1 and +-0.5 (M3: the window slope against
/// the re-measured one, kappa after the reduction) and +-0.44, +-1.0 and +-1.75 (H2: an exposure step of 1.5x, 2.5x and
/// 5x). Probe only.
const REMEASURE_SHIFTS: [f64; 10] = [0.1, -0.1, 0.5, -0.5, 0.44, -0.44, 1.0, -1.0, 1.75, -1.75];

/// The sets whose list alone costs more than this (ms a frame, median) are named by the verdict and stop Phase A (RR4).
const LIST_COST_MAX_MS: f64 = 70.0;

/// One light of a `forced` run. A light that was not measured carries one of [`UNMEASURED`] in `refusal`.
///
/// `layout` is `mono`, `cfa` or `rgb`. `anchor_stars` is the anchor's survivors, or the matches it found when the anchor
/// refused. `map_stars` is the list's map stars, `map_members` the members of the frame's map after its clip,
/// `map_offset_px` / `map_residual_px` its offset and residual, `map_kept_share` the share of the map stars the second fit
/// kept. `aperture_radius_px` is the disc's radius, `members` the members before the screen, `tapered` those above the
/// screen's upper limit (H1: weight tapered), `zp_stars` the members of non-zero weight the screen kept. `pedestal` is
/// the sky pedestal (working units per pixel; empty when it was not fitted), `pedestal_kept_share` the share of the
/// points offered to its line that the clip kept (set also when the pedestal collapsed) and `pedestal_zp_mag` its part of the zero
/// point (zero point less the same ratio of sums with no pedestal). `annulus_off` counts the list stars whose annulus or
/// disc left the plane. `c_budget` is the budget's reported `c` (mosaic only; the anchor's). `window_slope` is the
/// analytic slope of the window, `slope_remeasured` the two-sided difference of the +-0.1 mag anchor shifts (identity
/// frames only), `zero_point_6r` the ratio of sums at `6 * FWHM_ref` with the same pedestal on the same members (RR3).
#[derive(Debug, Clone, Default)]
struct ForcedRow {
    frame_id: Option<i64>,
    file_name: String,
    object: String,
    filter: String,
    night: String,
    date_obs: String,
    exptime: Option<f64>,
    rig: String,
    layout: String,
    airmass: Option<f64>,
    /// 0 or 1: the pier side, from the rotation against the night's first frame.
    pier: Option<i64>,
    rotation_deg: Option<f64>,
    stars_detected: Option<i64>,
    median_fwhm: Option<f64>,
    scale: Option<f64>,
    fwhm_ref: Option<f64>,
    anchor: Option<f64>,
    anchor_stars: Option<i64>,
    window_bright: Option<f64>,
    window_faint: Option<f64>,
    list_before_cap: Option<i64>,
    list_stars: Option<i64>,
    /// List candidates the frame's own detections removed (G7), and the (star, seed) pairs their flux calibration had.
    detected_neighbours_removed: Option<i64>,
    seed_calibration_pairs: Option<i64>,
    map_stars: Option<i64>,
    map_members: Option<i64>,
    map_offset_px: Option<f64>,
    map_residual_px: Option<f64>,
    map_kept_share: Option<f64>,
    aperture_radius_px: Option<f64>,
    members: Option<i64>,
    tapered: Option<i64>,
    zp_stars: Option<i64>,
    pedestal: Option<f64>,
    pedestal_kept_share: Option<f64>,
    pedestal_zp_mag: Option<f64>,
    annulus_off: Option<i64>,
    c_budget: Option<f64>,
    zero_point: Option<f64>,
    sigma: Option<f64>,
    refusal: String,
    centre_median_px: Option<f64>,
    window_slope: Option<f64>,
    slope_remeasured: Option<f64>,
    zero_point_6r: Option<f64>,
    zero_point_today: Option<f64>,
    today_refusal: String,
    /// `ok` or `mismatch:<field>` on a frame measured a second time without a list; empty on the others.
    identity: String,
    call_ms: Option<f64>,
    plain_ms: Option<f64>,
    /// The fitter's wall time of the first fit pass, the second and the aperture sums, ms.
    list_fit_ms: Option<f64>,
    list_refit_ms: Option<f64>,
    list_ap_ms: Option<f64>,
    /// The list's structures (stars, fits), kB.
    list_kb: Option<f64>,
    /// The process' peak resident set after the frame, MB.
    rss_mb: Option<f64>,
}

fn int(x: Option<i64>) -> String {
    x.map(|v| v.to_string()).unwrap_or_default()
}

impl ForcedRow {
    fn cells(&self) -> Vec<String> {
        vec![
            int(self.frame_id),
            clean(&self.file_name),
            clean(&self.object),
            clean(&self.filter),
            clean(&self.night),
            clean(&self.date_obs),
            num(self.exptime, 3),
            clean(&self.rig),
            clean(&self.layout),
            num(self.airmass, 4),
            int(self.pier),
            num(self.rotation_deg, 3),
            int(self.stars_detected),
            num(self.median_fwhm, 3),
            num(self.scale, 4),
            num(self.fwhm_ref, 3),
            num(self.anchor, 5),
            int(self.anchor_stars),
            num(self.window_bright, 3),
            num(self.window_faint, 3),
            int(self.list_before_cap),
            int(self.list_stars),
            int(self.detected_neighbours_removed),
            int(self.seed_calibration_pairs),
            int(self.map_stars),
            int(self.map_members),
            num(self.map_offset_px, 4),
            num(self.map_residual_px, 4),
            num(self.map_kept_share, 4),
            num(self.aperture_radius_px, 3),
            int(self.members),
            int(self.tapered),
            int(self.zp_stars),
            num(self.pedestal, 5),
            num(self.pedestal_kept_share, 4),
            num(self.pedestal_zp_mag, 5),
            int(self.annulus_off),
            num(self.c_budget, 5),
            num(self.zero_point, 5),
            num(self.sigma, 5),
            clean(&self.refusal),
            num(self.centre_median_px, 4),
            num(self.window_slope, 5),
            num(self.slope_remeasured, 5),
            num(self.zero_point_6r, 5),
            num(self.zero_point_today, 5),
            clean(&self.today_refusal),
            clean(&self.identity),
            num(self.call_ms, 0),
            num(self.plain_ms, 0),
            num(self.list_fit_ms, 0),
            num(self.list_refit_ms, 0),
            num(self.list_ap_ms, 0),
            num(self.list_kb, 1),
            num(self.rss_mb, 0),
        ]
    }

    fn to_tsv(&self) -> String {
        self.cells().join("\t")
    }

    fn from_fields(h: &[String], f: &[String]) -> Self {
        let s = |n: &str| col(h, f, n).to_string();
        let x = |n: &str| opt_f64(col(h, f, n));
        let i = |n: &str| opt_i64(col(h, f, n));
        Self {
            frame_id: i("frame_id"),
            file_name: s("file_name"),
            object: s("object"),
            filter: s("filter"),
            night: s("night"),
            date_obs: s("date_obs"),
            exptime: x("exptime"),
            rig: s("rig"),
            layout: s("layout"),
            airmass: x("airmass"),
            pier: i("pier"),
            rotation_deg: x("rotation_deg"),
            stars_detected: i("stars_detected"),
            median_fwhm: x("median_fwhm"),
            scale: x("scale"),
            fwhm_ref: x("fwhm_ref"),
            anchor: x("anchor"),
            anchor_stars: i("anchor_stars"),
            window_bright: x("window_bright"),
            window_faint: x("window_faint"),
            list_before_cap: i("list_before_cap"),
            list_stars: i("list_stars"),
            detected_neighbours_removed: i("detected_neighbours_removed"),
            seed_calibration_pairs: i("seed_calibration_pairs"),
            map_stars: i("map_stars"),
            map_members: i("map_members"),
            map_offset_px: x("map_offset_px"),
            map_residual_px: x("map_residual_px"),
            map_kept_share: x("map_kept_share"),
            aperture_radius_px: x("aperture_radius_px"),
            members: i("members"),
            tapered: i("tapered"),
            zp_stars: i("zp_stars"),
            pedestal: x("pedestal"),
            pedestal_kept_share: x("pedestal_kept_share"),
            pedestal_zp_mag: x("pedestal_zp_mag"),
            annulus_off: i("annulus_off"),
            c_budget: x("c_budget"),
            zero_point: x("zero_point"),
            sigma: x("sigma"),
            refusal: s("refusal"),
            centre_median_px: x("centre_median_px"),
            window_slope: x("window_slope"),
            slope_remeasured: x("slope_remeasured"),
            zero_point_6r: x("zero_point_6r"),
            zero_point_today: x("zero_point_today"),
            today_refusal: s("today_refusal"),
            identity: s("identity"),
            call_ms: x("call_ms"),
            plain_ms: x("plain_ms"),
            list_fit_ms: x("list_fit_ms"),
            list_refit_ms: x("list_refit_ms"),
            list_ap_ms: x("list_ap_ms"),
            list_kb: x("list_kb"),
            rss_mb: x("rss_mb"),
        }
    }

    fn t_s(&self) -> Option<f64> {
        parse_time(&self.date_obs).map(|t| t.timestamp_millis() as f64 / 1000.0)
    }

    fn is_unmeasured(&self) -> bool {
        UNMEASURED.contains(&self.refusal.as_str())
    }

    fn valid(&self) -> bool {
        self.zero_point.is_some()
    }

    /// The rig of the row: the TSV's column, the filter for an older TSV.
    fn rig_key(&self) -> String {
        if self.rig.is_empty() {
            self.filter.clone()
        } else {
            self.rig.clone()
        }
    }
}

fn forced_base_row(c: &Candidate) -> ForcedRow {
    let m = base_row(c);
    ForcedRow {
        frame_id: m.frame_id,
        file_name: m.file_name,
        object: m.object,
        filter: m.filter,
        night: m.night,
        date_obs: m.date_obs,
        exptime: m.exptime,
        rig: m.rig,
        ..ForcedRow::default()
    }
}

// ---- the pier side ----

/// The signed difference of two rotations, degrees, in `[-180, 180)`.
fn rotation_gap_deg(a: f64, b: f64) -> f64 {
    (a - b + 540.0).rem_euclid(360.0) - 180.0
}

/// The pier side of a frame: 0 while its rotation is within 90 degrees (mod 360) of the night's first frame, else 1.
fn pier_side(first_rotation_deg: f64, rotation_deg: f64) -> i64 {
    i64::from(rotation_gap_deg(rotation_deg, first_rotation_deg).abs() > 90.0)
}

// ---- the process' memory ----

/// The process' peak resident set (MB): `ru_maxrss`, bytes on macOS and kilobytes elsewhere. `None` off Unix.
#[cfg(unix)]
fn peak_rss_mb() -> Option<f64> {
    // SAFETY: `getrusage` writes into the zeroed struct we own and reads nothing else.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    let raw = usage.ru_maxrss as f64;
    Some(if cfg!(target_os = "macos") {
        raw / 1_048_576.0
    } else {
        raw / 1024.0
    })
}

#[cfg(not(unix))]
fn peak_rss_mb() -> Option<f64> {
    None
}

// ---- identity (B9) ----

/// Every number of a fit by name as bits (NaN equals NaN by its bits), the order of rustafits' own identity test.
fn star_bits(f: &StarFit) -> Vec<(&'static str, u64)> {
    vec![
        ("x", f.x.to_bits()),
        ("y", f.y.to_bits()),
        ("background", f.background.to_bits()),
        ("amplitude", f.amplitude.to_bits()),
        ("fwhm_x", f.fwhm_x.to_bits()),
        ("fwhm_y", f.fwhm_y.to_bits()),
        ("fwtm_x", f.fwtm_x.to_bits()),
        ("fwtm_y", f.fwtm_y.to_bits()),
        ("theta", f.theta.to_bits()),
        ("beta", f.beta.to_bits()),
        ("residual", f.residual.to_bits()),
        ("signal", f.signal.to_bits()),
        ("area", f.area.to_bits()),
        ("peak5", f.peak5.to_bits()),
        ("sky_ring", f.sky_ring.to_bits()),
        ("sky_ring_sigma", f.sky_ring_sigma.to_bits()),
        ("snr", f.snr.to_bits()),
        ("ap_sum", f.ap_sum.to_bits()),
        ("hfr", f.hfr.to_bits()),
        ("sky_ring_n", u64::from(f.sky_ring_n)),
        ("flags", u64::from(f.flags.bits())),
    ]
}

/// The measurement with its wall-clock fields reset, as text: `{:?}` prints a float as its shortest round trip.
fn measurement_text(pm: &PlaneMeasurement) -> String {
    let mut pm = pm.clone();
    pm.timings = Default::default();
    pm.sub_steps = Default::default();
    format!("{pm:?}")
}

/// What differs between the measurement a plain `measure_raw_frame` made and the one `measure_with_zero_point` made (B9,
/// rustafits' `the_analysis_measurement_is_identical_with_and_without_a_list`): the first field that does, by name.
fn identity_mismatch(
    plain: &PlaneMeasurement,
    with: &PlaneMeasurement,
    opts: &FrameOptions,
) -> Option<String> {
    if plain.stars.len() != with.stars.len() {
        return Some("stars_len".to_string());
    }
    for (a, b) in plain.stars.iter().zip(&with.stars) {
        for ((name, x), (_, y)) in star_bits(a).into_iter().zip(star_bits(b)) {
            if x != y {
                return Some(format!("star_{name}"));
            }
        }
    }
    if measurement_text(plain) != measurement_text(with) {
        return Some("measurement".to_string());
    }
    let summary = |pm: &PlaneMeasurement| format!("{:?}", FrameSummary::from_measurement(pm, opts));
    if summary(plain) != summary(with) {
        return Some("summary".to_string());
    }
    None
}

/// What one light of a `forced` run writes: its row and the rows of the three side files.
struct FrameOut {
    row: ForcedRow,
    /// `<x>.stars.tsv`.
    stars: Vec<String>,
    /// `<x>.ap.tsv` (only with `--dump-ap 1`).
    aps: Vec<String>,
    /// `<x>.shifts.tsv` (identity frames only).
    shifts: Vec<String>,
}

impl FrameOut {
    fn bare(row: ForcedRow) -> Self {
        Self { row, stars: Vec::new(), aps: Vec::new(), shifts: Vec::new() }
    }
}

/// One list star of a ratio of sums: its catalogue magnitude, the weight it enters the statistic with (0 for a star that
/// is no member or that the screen dropped) and its aperture sum at the radius the ratio is read at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SumStar {
    g: f64,
    weight: f64,
    sum: f64,
}

/// The statistic's ratio of sums over `stars`, the pedestal `eps` (flux per pixel) taken out of every sum at
/// `radius_px`: `2.5 log10( sum w (S - eps pi r^2) / (t sum w 10^(-0.4 G)) )`. With the radius and the pedestal the
/// statistic used it is the statistic's own zero point; at `6 * FWHM_ref` with the same pedestal and the same members and
/// weights it is RR3's referee; with `eps = 0` it is the zero point the pedestal did not touch. `None` for no weight or a
/// sum that is not positive.
fn ratio_of_sums(stars: &[SumStar], eps: f64, radius_px: f64, exptime_s: f64) -> Option<f64> {
    let area = std::f64::consts::PI * radius_px * radius_px;
    let (mut sw_f, mut sw_c) = (0.0, 0.0);
    for s in stars.iter().filter(|s| s.weight > 0.0 && s.sum.is_finite()) {
        sw_f += s.weight * (s.sum - eps * area);
        sw_c += s.weight * 10f64.powf(-0.4 * s.g);
    }
    (sw_f > 0.0 && sw_c > 0.0 && exptime_s > 0.0)
        .then(|| 2.5 * (sw_f / (exptime_s * sw_c)).log10())
        .filter(|z| z.is_finite())
}

/// The list's stars as [`SumStar`]s, the sums read by `sum_of` (the disc, or the pedestal radius `RR3` names).
fn sum_stars(
    list: &ForcedList,
    per: &[ForcedStarZp],
    apertures: &[ListAperture],
    sum_of: impl Fn(&ListAperture) -> Option<f64>,
) -> Vec<SumStar> {
    list.stars
        .iter()
        .zip(per)
        .zip(apertures)
        .map(|((star, p), ap)| SumStar {
            g: f64::from(star.g_mag),
            weight: if p.member && p.screened_in { p.weight } else { 0.0 },
            sum: sum_of(ap).unwrap_or(f64::NAN),
        })
        .collect()
}

/// One list star's row of the stars TSV: its key, magnitude, the weight it enters the statistic with, whether it is a
/// member (in the window, on the frame, under the clip floor) and whether the screen left it.
fn forced_star_line(frame_id: i64, s: &ForcedStar, p: &ForcedStarZp) -> String {
    format!(
        "{frame_id}\t{:016x}\t{:.3}\t{:.4}\t{}\t{}",
        s.key,
        s.g_mag,
        p.weight,
        u8::from(p.member),
        u8::from(p.member && p.screened_in),
    )
}

/// One list star's row of the aperture dump (`--dump-ap 1`): every figure of its aperture, at the position the map put it.
fn forced_ap_line(
    frame_id: i64,
    s: &ForcedStar,
    ap: &ListAperture,
    at: (f64, f64),
    p: &ForcedStarZp,
) -> String {
    let extra = |i: usize| num(ap.extra_sums.get(i).copied(), 2);
    format!(
        "{frame_id}\t{:016x}\t{:.3}\t{:.2}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t{}\t{}",
        s.key,
        s.g_mag,
        at.0,
        at.1,
        u8::from(ap.on_frame),
        num(Some(ap.sum), 2),
        extra(0),
        extra(1),
        extra(2),
        num(Some(ap.sky), 3),
        num(Some(ap.sky_scatter), 3),
        num(Some(ap.peak), 1),
        p.weight,
        u8::from(p.member),
        u8::from(p.member && p.screened_in),
    )
}

/// One re-measure's row of `<x>.shifts.tsv`: the zero point, the window and the statistic's counts at an anchor shifted
/// by `shift` mag, or the refusal.
fn forced_shift_line(frame_id: i64, shift: f64, run: &ForcedRun) -> String {
    let (zp, refusal) = match &run.result {
        Ok(z) => (Some(z.zero_point), String::new()),
        Err(r) => (None, r.code().to_string()),
    };
    let (bright, faint) = run
        .list
        .as_ref()
        .map_or((None, None), |l| (Some(l.window_bright), Some(l.window_faint)));
    let (slope, members) = run
        .outcome
        .as_ref()
        .map_or((None, None), |o| (o.window_slope, Some(i64::from(o.members))));
    format!(
        "{frame_id}\t{shift:+.2}\t{}\t{}\t{}\t{}\t{}\t{refusal}",
        num(zp, 5),
        num(bright, 4),
        num(faint, 4),
        num(slope, 5),
        int(members),
    )
}

fn millis(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// One light, end to end: the stored solve, the frame read in memory, the shipping composition, today's zero point on
/// the same measurement, RR3's 6r referee and, when `identity`, a second plain measurement compared with the first and
/// the anchor-shifted re-measures ([`REMEASURE_SHIFTS`], outside `call_ms`). Returns the light's row (its failure is the
/// `refusal`, logged on stderr) and the rows of the side files.
fn forced_frame(
    conn: &Connection,
    rig: &Rig,
    fp: &ForcedListParams,
    c: &Candidate,
    identity: bool,
    dump_ap: bool,
) -> FrameOut {
    let mut row = forced_base_row(c);
    let fail = |row: &mut ForcedRow, code: &str, detail: &dyn std::fmt::Display| {
        eprintln!("zp_probe: frame {} refused {code}: {detail}", c.frame_id);
        row.refusal = code.to_string();
    };
    let rec = match get_plate_solve(conn, c.frame_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            fail(&mut row, "no_solve", &"no stored solve");
            return FrameOut::bare(row);
        }
        Err(e) => {
            fail(&mut row, "bad_wcs", &e);
            return FrameOut::bare(row);
        }
    };
    row.rotation_deg = Some(rec.field_rotation_deg).filter(|r| r.is_finite());
    let Some(wcs) = rec.to_solution() else {
        fail(&mut row, "bad_wcs", &"the stored SIP tables are unusable");
        return FrameOut::bare(row);
    };
    let mut frame = match read_raw_frame(Path::new(&c.path)) {
        Ok(f) => f,
        Err(e) => {
            fail(&mut row, "analysis_failed", &e);
            return FrameOut::bare(row);
        }
    };
    frame.drop_unmeasured_planes();
    row.layout = match &frame.planes {
        RawPlanes::Mono(_) => "mono",
        RawPlanes::Cfa { .. } => "cfa",
        RawPlanes::Rgb(_) => "rgb",
    }
    .to_string();
    let (width, height) = (frame.w, frame.h);

    let meta = Frame {
        date_obs: parse_time(&c.date_obs),
        ..Frame::default()
    };
    let epoch = observation_epoch(&meta);
    let layers: Vec<&StarCache> = rig.layers.iter().map(|l| l.as_ref()).collect();
    let inputs = ZpInputs {
        wcs: &wcs,
        exptime_s: c.exptime,
        epoch,
    };
    let started = Instant::now();
    let (pm, run) = measure_with_zero_point(
        &frame,
        &rig.config,
        Some(&rig.pool),
        Some(&inputs),
        fp,
        |w, wd, ht, region, ep| catalog_for_frame(&layers, w, wd, ht, region, ep),
    );
    row.call_ms = Some(millis(started));
    let Some(run) = run else {
        fail(&mut row, "analysis_failed", &"no zero-point run came back");
        return FrameOut::bare(row);
    };
    let (analysis, stars) = map_measurement(&pm, &frame, &rig.config, &rig.config_hash);
    row.stars_detected = Some(analysis.stars_detected);
    row.median_fwhm = Some(analysis.median_fwhm).filter(|f| *f > 0.0);
    let mosaic = row.layout == "cfa";
    if mosaic {
        row.c_budget = pm.reported_aperture_correction;
    }

    // The airmass of the solve centre at the middle of the exposure; no site, no airmass.
    let (ra, dec) = wcs.pixel_to_sky((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0);
    row.airmass = match (c.sitelat, c.sitelong, parse_time(&c.date_obs)) {
        (Some(lat), Some(lon), Some(t)) => {
            let mid = t.timestamp_millis() as f64 / 1000.0
                + c.exptime.filter(|e| e.is_finite()).unwrap_or(0.0) / 2.0;
            airmass(ra, dec, mid, lat, lon)
        }
        _ => None,
    };

    if let Some(a) = &run.anchor {
        row.anchor = Some(a.zero_point);
        row.anchor_stars = Some(i64::from(a.stars));
    }
    if let Some(l) = &run.list {
        row.scale = Some(l.scale_arcsec);
        row.fwhm_ref = Some(l.fwhm_ref_px);
        row.window_bright = Some(l.window_bright);
        row.window_faint = Some(l.window_faint);
        row.list_before_cap = Some(l.list_before_cap as i64);
        row.list_stars = Some(l.stars.len() as i64);
        row.detected_neighbours_removed = Some(i64::from(l.detected_neighbours_removed));
        row.seed_calibration_pairs = Some(i64::from(l.seed_calibration_pairs));
        row.map_stars = Some(l.map.len() as i64);
    }
    if let Some(m) = &run.map {
        row.map_members = Some(m.members as i64);
        row.map_offset_px = Some(m.offset_px).filter(|v| v.is_finite());
        row.map_residual_px = Some(m.residual_px).filter(|v| v.is_finite());
    }
    // The second fit's share of the map stars it kept: the gate's, or the one a `poor_solve` refusal carries.
    row.map_kept_share = match (&run.gate, &run.result) {
        (Some(g), _) => Some(g.kept_share),
        (None, Err(ForcedRefusal::PoorSolve { kept_share })) => Some(*kept_share),
        _ => None,
    }
    .filter(|v| v.is_finite());
    row.centre_median_px = run.gate.as_ref().map(|g| g.centre_median_px).filter(|v| v.is_finite());
    row.aperture_radius_px = run.placement.as_ref().map(|p| p.radius_px);
    if let Some(m) = &run.measured {
        row.annulus_off = Some(m.apertures.iter().filter(|a| !a.on_frame).count() as i64);
        row.list_fit_ms = Some(m.timings.fit_ms as f64);
        row.list_refit_ms = Some(m.timings.refit_ms as f64);
        row.list_ap_ms = Some(m.timings.aperture_ms as f64);
    }
    if let (Some(l), Some(m)) = (&run.list, &run.measured) {
        let bytes = l.stars.len() * std::mem::size_of::<ForcedStar>()
            + (m.fits.len() + m.refits.len()) * std::mem::size_of::<ListFit>()
            + m.apertures.len() * std::mem::size_of::<ListAperture>()
            + l.request.fit_positions.len() * std::mem::size_of::<(f64, f64)>();
        row.list_kb = Some(bytes as f64 / 1024.0);
    }
    if let Some(o) = &run.outcome {
        row.members = Some(i64::from(o.members));
        row.tapered = Some(i64::from(o.tapered));
        row.pedestal = o.pedestal.filter(|v| v.is_finite());
        row.pedestal_kept_share =
            (o.pedestal_points > 0).then(|| f64::from(o.pedestal_kept) / f64::from(o.pedestal_points));
        row.window_slope = o.window_slope.filter(|v| v.is_finite());
    }
    match &run.result {
        Ok(z) => {
            row.zero_point = Some(z.zero_point);
            row.sigma = Some(z.sigma);
            row.zp_stars = Some(i64::from(z.stars));
        }
        Err(r) => {
            row.refusal = r.code().to_string();
            match r {
                ForcedRefusal::NoAnchor { kept } => row.anchor_stars = Some(i64::from(*kept)),
                ForcedRefusal::TooFewStars { kept, .. } => row.zp_stars = Some(i64::from(*kept)),
                ForcedRefusal::TooScattered { kept, sigma } => {
                    row.zp_stars = Some(i64::from(*kept));
                    row.sigma = Some(*sigma).filter(|v| v.is_finite());
                }
                _ => {}
            }
        }
    }

    // RR3's referee and the pedestal's part of the zero point, from the outcome's rows and the apertures: the same ratio
    // of sums over the same members and weights, read at the third pedestal radius (`6 * FWHM_ref`) and at the disc with no
    // pedestal at all.
    if let (Ok(z), Some(l), Some(m), Some(o), Some(p), Some(eps)) = (
        &run.result,
        &run.list,
        &run.measured,
        &run.outcome,
        &run.placement,
        run.outcome.as_ref().and_then(|o| o.pedestal),
    ) {
        let t = c.exptime.unwrap_or(f64::NAN);
        let disc = sum_stars(l, &o.per_star, &m.apertures, |a| Some(a.sum));
        match ratio_of_sums(&disc, eps, p.radius_px, t) {
            Some(own) if (own - z.zero_point).abs() <= 1e-6 => {
                row.pedestal_zp_mag = ratio_of_sums(&disc, 0.0, p.radius_px, t).map(|bare| z.zero_point - bare);
            }
            other => eprintln!(
                "zp_probe: frame {} the probe's ratio of sums {} differs from the statistic's {:.6}",
                c.frame_id,
                show(other, 6),
                z.zero_point
            ),
        }
        if let Some(r6) = p.extra_radii_px.get(2).copied() {
            let six = sum_stars(l, &o.per_star, &m.apertures, |a| a.extra_sums.get(2).copied());
            row.zero_point_6r = ratio_of_sums(&six, eps, r6, t);
        }
    }

    // Today's zero point on the same measurement (6.5's matcher over the budget fits, times the reported c on a mosaic).
    match catalog_for_frame(
        &layers,
        &wcs,
        width,
        height,
        rig.params.central_fraction,
        epoch,
    ) {
        Ok(cat) => {
            let today = measure_zero_point(
                &ZpInput {
                    stars: &stars,
                    wcs: &wcs,
                    catalog: &cat,
                    exptime_s: c.exptime,
                    width,
                    height,
                    median_fwhm_px: analysis.median_fwhm,
                },
                &rig.params,
            );
            match today.result {
                Ok(z) if mosaic => match pm.reported_aperture_correction {
                    Some(cc) => row.zero_point_today = Some(z.zero_point + 2.5 * cc.log10()),
                    None => row.today_refusal = "no_c".to_string(),
                },
                Ok(z) => row.zero_point_today = Some(z.zero_point),
                Err(r) => row.today_refusal = r.code().to_string(),
            }
        }
        Err(e) => {
            eprintln!("zp_probe: frame {} today's cone: {e:#}", c.frame_id);
            row.today_refusal = "no_cone".to_string();
        }
    }

    let mut out = FrameOut::bare(ForcedRow::default());
    if let (Some(l), Some(o)) = (&run.list, &run.outcome) {
        for (s, p) in l.stars.iter().zip(&o.per_star) {
            out.stars.push(forced_star_line(c.frame_id, s, p));
        }
        if let (true, Some(m), Some(p)) = (dump_ap, &run.measured, &run.placement) {
            for (((s, zp), ap), at) in l.stars.iter().zip(&o.per_star).zip(&m.apertures).zip(&p.aperture_positions) {
                out.aps.push(forced_ap_line(c.frame_id, s, ap, *at, zp));
            }
        }
    }

    if identity {
        let started = Instant::now();
        let plain = measure_raw_frame(&frame, &rig.config, Some(&rig.pool));
        row.plain_ms = Some(millis(started));
        row.identity = identity_mismatch(&plain, &pm, &frame_options(&rig.config))
            .map_or_else(|| "ok".to_string(), |f| format!("mismatch:{f}"));
        // The re-measures: the same composition with the anchor moved (probe only), outside `call_ms`.
        let mut at: HashMap<i64, Option<f64>> = HashMap::new();
        for shift in REMEASURE_SHIFTS {
            let shifted = ForcedListParams { anchor_shift_mag: fp.anchor_shift_mag + shift, ..*fp };
            let (_, again) = measure_with_zero_point(
                &frame,
                &rig.config,
                Some(&rig.pool),
                Some(&inputs),
                &shifted,
                |w, wd, ht, region, ep| catalog_for_frame(&layers, w, wd, ht, region, ep),
            );
            match again {
                Some(r) => {
                    at.insert((shift * 100.0).round() as i64, r.result.as_ref().ok().map(|z| z.zero_point));
                    out.shifts.push(forced_shift_line(c.frame_id, shift, &r));
                }
                None => eprintln!("zp_probe: frame {} the re-measure at {shift:+.2} came back empty", c.frame_id),
            }
        }
        // The slope the re-measures give, per mag the window moves fainter: the two-sided difference at +-0.1.
        row.slope_remeasured = match (at.get(&10).copied().flatten(), at.get(&-10).copied().flatten()) {
            (Some(up), Some(down)) => Some((up - down) / 0.2),
            _ => None,
        };
    }
    row.rss_mb = peak_rss_mb();
    out.row = row;
    out
}

/// `<x>.<kind>.tsv` for `<x>.tsv`.
fn side_path(out: &str, kind: &str) -> String {
    format!("{}.{kind}.tsv", out.strip_suffix(".tsv").unwrap_or(out))
}

/// `<x>.stars.tsv` for `<x>.tsv`.
fn stars_path(out: &str) -> String {
    side_path(out, "stars")
}

/// The text of `<x>.flags.txt`: the command line's flags (sorted) and every parameter the run used, in full, so a tuning
/// round can be read back without its shell history.
fn forced_flags_text(given: &[(String, String)], fp: &ForcedListParams, zp: &ZpParams) -> String {
    let mut text = String::from("# zp_probe forced: the parameters of this run\n");
    if given.is_empty() {
        text.push_str("flags: none (every parameter at its default)\n");
    } else {
        for (name, value) in given {
            text.push_str(&format!("flag --{name} {value}\n"));
        }
    }
    text.push_str(&format!("ForcedListParams {fp:?}\n"));
    text.push_str(&format!("ZpParams {zp:?}\n"));
    text
}

/// `<x>.flags.txt` for `<x>.tsv`.
fn flags_path(out: &str) -> String {
    format!("{}.flags.txt", out.strip_suffix(".tsv").unwrap_or(out))
}

/// What is left of `forced`'s command line once its structural flags are taken: the `ForcedListParams`, the `ZpParams`
/// and the parameter flags as given (sorted by name). A flag that is neither is an error.
fn take_forced_params(
    args: &mut Args,
) -> Result<(ForcedListParams, ZpParams, Vec<(String, String)>)> {
    let mut given: Vec<(String, String)> = args
        .flags
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    given.sort();
    let fp = args.forced_list_params()?;
    let zp = args.zp_params()?;
    args.finish()?;
    Ok((fp, zp, given))
}

fn cmd_forced(mut args: Args) -> Result<()> {
    let db = args.require("db")?;
    let app_data = args.require("app-data")?;
    let instrume = args.require("instrume")?;
    let filter = args.require("filter")?;
    let object = args.require("object")?;
    let out = args.require("out")?;
    let limit: Option<usize> = args.take_parsed("limit")?;
    let identity_every: usize = args
        .take_parsed("identity-every")?
        .unwrap_or(IDENTITY_EVERY_DEFAULT);
    let dump_ap = args.take_parsed::<u8>("dump-ap")?.unwrap_or(0) != 0;
    let (fp, params, given) = take_forced_params(&mut args)?;
    fp.validate().map_err(|e| anyhow!("--{e}"))?;
    check_annulus_rule(&fp)?;

    let conn = open_read_only(&db)?;
    let rig = open_rig(&conn, &app_data, params)?;
    let candidates = load_candidates(&conn, &instrume, &object, &filter)?;
    eprintln!(
        "zp_probe forced: {} lights of {instrume} / {object} / {filter} in the catalogue; list params {fp:?}",
        candidates.len()
    );
    let flags_text = forced_flags_text(&given, &fp, &params);
    eprint!("{flags_text}");
    std::fs::write(flags_path(&out), &flags_text)
        .with_context(|| format!("writing {}", flags_path(&out)))?;
    let mut w = create_out(&out)?;
    writeln!(w, "{FORCED_HEADER}")?;
    let mut ws = create_out(&stars_path(&out))?;
    writeln!(ws, "{FORCED_STARS_HEADER}")?;
    let mut wh = create_out(&side_path(&out, "shifts"))?;
    writeln!(wh, "{FORCED_SHIFTS_HEADER}")?;
    let mut wa = if dump_ap {
        let mut w = create_out(&side_path(&out, "ap"))?;
        writeln!(w, "{FORCED_AP_HEADER}")?;
        Some(w)
    } else {
        None
    };
    let (mut measured, mut refused, mut examined) = (0usize, 0usize, 0usize);
    let mut first_rotation: HashMap<String, f64> = HashMap::new();
    for c in &candidates {
        if limit.is_some_and(|n| measured >= n) {
            break;
        }
        examined += 1;
        let skip = if c.in_zip {
            Some("in_zip")
        } else if !Path::new(&c.path).is_file() {
            Some("missing_file")
        } else if !c.has_solve {
            Some("no_solve")
        } else {
            None
        };
        if let Some(code) = skip {
            eprintln!(
                "zp_probe: frame {} not measured ({code}): {}",
                c.frame_id, c.path
            );
            let mut row = forced_base_row(c);
            row.refusal = code.to_string();
            writeln!(w, "{}", row.to_tsv())?;
            continue;
        }
        let identity = identity_every > 0 && measured % identity_every == 0;
        let FrameOut { mut row, stars, aps, shifts } = forced_frame(&conn, &rig, &fp, c, identity, dump_ap);
        if let Some(rot) = row.rotation_deg {
            let first = *first_rotation.entry(c.night.clone()).or_insert(rot);
            row.pier = Some(pier_side(first, rot));
        }
        if row.zero_point.is_none() {
            refused += 1;
        }
        writeln!(w, "{}", row.to_tsv())?;
        for l in &stars {
            writeln!(ws, "{l}")?;
        }
        for l in &shifts {
            writeln!(wh, "{l}")?;
        }
        if let Some(wa) = wa.as_mut() {
            for l in &aps {
                writeln!(wa, "{l}")?;
            }
            wa.flush()?;
        }
        w.flush()?;
        ws.flush()?;
        wh.flush()?;
        measured += 1;
        if measured % PROGRESS_EVERY == 0 {
            eprintln!(
                "zp_probe forced: {measured} frames measured ({refused} without a zero point)"
            );
        }
    }
    w.flush()?;
    ws.flush()?;
    wh.flush()?;
    eprintln!(
        "zp_probe summary: examined {examined} of {} lights, measured {measured} ({refused} without a zero point); wrote {out}, {} and {}",
        candidates.len(),
        stars_path(&out),
        side_path(&out, "shifts")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// verdict --forced: the bars B1-B9 of the forced-list spec section 6, as amended by the plan's review rulings
// ---------------------------------------------------------------------------

/// B1: every pair shares at least this share of the first frame's kept keys.
const B1_MIN_SHARE: f64 = 0.80;
/// B2: median |dZP| of a night's pairs, on a common-set night / elsewhere (and not above today's).
const B2_COMMON_MAX: f64 = 0.015;
const B2_OTHER_MAX: f64 = 0.020;
/// B3: RMS about the median of `ZP - 2.5 log10(t_flux / t)`, and |slope| of that on FWHM (mag/px).
const B3_RMS_MAX: f64 = 0.015;
const B3_SLOPE_MAX: f64 = 0.04;
/// B3's slope needs this many joined frames.
const B3_MIN_POINTS: usize = 20;
/// B4: |k - k_stack| (mag per airmass), judged where the side's airmass range is at least `B4_MIN_DX`.
const B4_K_TOL: f64 = 0.02;
const B4_MIN_DX: f64 = 0.4;
const B4_MIN_POINTS: usize = 10;
/// B6: valid over measured per night; 6.5's QHY268M sets must be complete.
const B6_MIN: f64 = 0.90;
const B6_COMPLETE: f64 = 1.00;
/// B7: median |refit - moved seed| over frames (the map's refits' distance from where the map placed them), in units of
/// `FWHM_ref`.
const B7_MAX_FWHM_REF: f64 = 0.25;
/// B8: today's slope plus this many of its errors bounds the forced slope.
const B8_SIGMAS: f64 = 2.0;
/// A night with this many frames joined to `--common` is a common-set night.
const COMMON_NIGHT_MIN_JOINED: usize = 30;
/// 6.5's committed cross-target spread of the QHY268M L sets (mag), printed beside ours.
const CROSS_COMMITTED_6_5_MAG: f64 = 0.025;

/// The `--common` rows by frame id and by local file name.
struct CommonIndex<'a> {
    by_id: HashMap<i64, &'a CommonRow>,
    by_name: HashMap<&'a str, &'a CommonRow>,
}

impl<'a> CommonIndex<'a> {
    fn new(rows: &'a [CommonRow]) -> Self {
        Self {
            by_id: rows.iter().filter_map(|c| Some((c.frame_id?, c))).collect(),
            by_name: rows.iter().map(|c| (name_key(&c.file_name), c)).collect(),
        }
    }

    /// The row's common flux (green plane of a colour frame, the plane of a mono one), by frame id else file name.
    fn tflux(&self, r: &ForcedRow) -> Option<f64> {
        let c = r
            .frame_id
            .and_then(|id| self.by_id.get(&id).copied())
            .or_else(|| self.by_name.get(r.file_name.as_str()).copied())?;
        c.green_tflux()
    }

    /// `2.5 log10(t_flux / t)`, the stack's level of the frame, in the units of the zero point.
    fn level(&self, r: &ForcedRow) -> Option<f64> {
        let t = r.exptime.filter(|t| *t > 0.0)?;
        Some(2.5 * (self.tflux(r)? / t).log10())
    }
}

/// A night judged under `--forced`: its rig, object and date, its measured frames in time order and their pier sides.
struct FNight<'a> {
    rig: String,
    object: String,
    night: String,
    filter: String,
    rows: Vec<&'a ForcedRow>,
    /// The pier side of each row (the night's first frame with a rotation is side 0); `None` without a rotation.
    sides: Vec<Option<i64>>,
    /// Frames joined to `--common`.
    joined: usize,
}

impl FNight<'_> {
    fn label(&self) -> String {
        if self.filter == "-" || self.filter.is_empty() {
            format!("{}/{}", self.object, self.night)
        } else {
            format!("{}/{}/{}", self.object, self.filter, self.night)
        }
    }

    fn common(&self) -> bool {
        self.joined >= COMMON_NIGHT_MIN_JOINED
    }

    fn two_sided(&self) -> bool {
        let has = |s| self.sides.contains(&Some(s));
        has(0) && has(1)
    }

    fn mono(&self) -> bool {
        let layouts: Vec<&str> = self
            .rows
            .iter()
            .map(|r| r.layout.as_str())
            .filter(|l| !l.is_empty())
            .collect();
        !layouts.is_empty() && layouts.iter().all(|l| *l == "mono")
    }

    fn qhy(&self) -> bool {
        self.rig.starts_with("QHY268M")
    }

}

/// A rig key as `camera/filter`.
fn rig_short_of(rig: &str) -> String {
    let parts: Vec<&str> = rig.split('|').collect();
    match (parts.first(), parts.last()) {
        (Some(a), Some(b)) if parts.len() > 1 => format!("{a}/{b}"),
        _ => rig.to_string(),
    }
}

/// Every night of at least [`OSC_NIGHT_MIN_FRAMES`] measured frames, by rig, object and night, in time order.
fn forced_nights<'a>(rows: &'a [ForcedRow], common: Option<&CommonIndex>) -> Vec<FNight<'a>> {
    let mut by: BTreeMap<(String, String, String), Vec<&ForcedRow>> = BTreeMap::new();
    for r in rows.iter().filter(|r| !r.is_unmeasured()) {
        by.entry((r.rig_key(), r.object.clone(), r.night.clone()))
            .or_default()
            .push(r);
    }
    by.into_iter()
        .filter(|(_, v)| v.len() >= OSC_NIGHT_MIN_FRAMES)
        .map(|((rig, object, night), mut rows)| {
            rows.sort_by(|a, b| {
                a.t_s()
                    .unwrap_or(f64::MAX)
                    .total_cmp(&b.t_s().unwrap_or(f64::MAX))
            });
            let first = rows.iter().find_map(|r| r.rotation_deg);
            let sides = rows
                .iter()
                .map(|r| Some(pier_side(first?, r.rotation_deg?)))
                .collect();
            let joined = common.map_or(0, |c| rows.iter().filter(|r| c.tflux(r).is_some()).count());
            let filter = rows.first().map(|r| r.filter.clone()).unwrap_or_default();
            FNight {
                rig,
                object,
                night,
                filter,
                rows,
                sides,
                joined,
            }
        })
        .collect()
}

/// What a reduction group is: a rig, the probe's frame set (the object) and the exposure to 0.1 s. The exposure is in the
/// key because `G_b` carries `+ 2.5 log10(t)`: two exposures in one pool would put a step of the window into the median
/// (H2); one exposure per set in the corpus.
type ReductionKey = (String, String, i64);

fn reduction_key(r: &ForcedRow) -> ReductionKey {
    (r.rig_key(), r.object.clone(), (r.exptime.unwrap_or(0.0) * 10.0).round() as i64)
}

/// The rows with `zero_point` and `zero_point_6r` read in the common window of their group (spec section 5.6): `P` the
/// median `window_bright` and `s` the median `window_slope` (over the frames that have one, 0 when none) of the group's
/// valid rows, a frame reading `ZP - s (G_b - P)` (core's `common_window`, the one `reduce_to_common_window` is made of;
/// a single frame reads its own). The 6r referee is reduced by the same group's window, today's zero point has no window
/// and is read as it is. Returns the rows and each group's window.
fn reduce_rows(rows: &[ForcedRow]) -> (Vec<ForcedRow>, BTreeMap<ReductionKey, CommonWindow>) {
    let mut pools: BTreeMap<ReductionKey, Vec<(f64, Option<f64>)>> = BTreeMap::new();
    for r in rows.iter().filter(|r| !r.is_unmeasured()) {
        if let (Some(_), Some(gb)) = (r.zero_point, r.window_bright) {
            pools.entry(reduction_key(r)).or_default().push((gb, r.window_slope));
        }
    }
    let windows: BTreeMap<ReductionKey, CommonWindow> = pools
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), common_window(v)?)))
        .collect();
    let reduced = rows
        .iter()
        .map(|r| {
            let mut r = r.clone();
            if let (Some(w), Some(gb)) = (windows.get(&reduction_key(&r)), r.window_bright) {
                r.zero_point = r.zero_point.map(|z| w.reduce(z, gb));
                r.zero_point_6r = r.zero_point_6r.map(|z| w.reduce(z, gb));
            }
            r
        })
        .collect();
    (reduced, windows)
}

/// Consecutive pairs of the frames `keep` accepts, in time order, a pair kept when its frames are under 15 minutes apart.
fn time_pairs<'a>(
    rows: &[&'a ForcedRow],
    keep: impl Fn(&ForcedRow) -> bool,
) -> Vec<(&'a ForcedRow, &'a ForcedRow)> {
    let mut frames: Vec<(f64, &ForcedRow)> = rows
        .iter()
        .copied()
        .filter(|r| keep(r))
        .filter_map(|r| Some((r.t_s()?, r)))
        .collect();
    frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    frames
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 < PAIR_MAX_GAP_S)
        .map(|w| (w[0].1, w[1].1))
        .collect()
}

/// The kept keys of the frames, read from `<x>.stars.tsv`: sorted, per frame id.
type KeptKeys = HashMap<i64, Vec<u64>>;

/// Streams a stars TSV for the keys of the list stars a frame's statistic kept, per frame: a member (`member = 1`) of
/// non-zero weight (`weight > 0`) that the screen left (`screened_in = 1`), the stars `zp_stars` counts (B1 counts those
/// and no others: a member the screen dropped is left out).
fn read_kept_keys(path: &str, into: &mut KeptKeys) -> Result<()> {
    let file = File::open(path).with_context(|| format!("reading {path}"))?;
    let mut lines = BufReader::new(file).lines();
    let header: Vec<String> = lines
        .next()
        .ok_or_else(|| anyhow!("{path} is empty"))??
        .split('\t')
        .map(str::to_string)
        .collect();
    let at = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| anyhow!("{path} has no {name} column"))
    };
    let (i_frame, i_key, i_weight, i_member, i_screened) =
        (at("frame_id")?, at("key")?, at("weight")?, at("member")?, at("screened_in")?);
    for line in lines {
        let line = line?;
        let f: Vec<&str> = line.split('\t').collect();
        let (Some(id), Some(key), Some(weight), Some(member), Some(screened)) =
            (f.get(i_frame), f.get(i_key), f.get(i_weight), f.get(i_member), f.get(i_screened))
        else {
            continue;
        };
        if *member != "1" || *screened != "1" || !weight.parse::<f64>().is_ok_and(|w| w > 0.0) {
            continue;
        }
        if let (Ok(id), Ok(key)) = (id.parse::<i64>(), u64::from_str_radix(key, 16)) {
            into.entry(id).or_default().push(key);
        }
    }
    for v in into.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    Ok(())
}

/// Size of the intersection of two sorted, de-duplicated slices.
fn sorted_intersection(a: &[u64], b: &[u64]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// B1 of a night: for every pair of consecutive frames (under 15 minutes apart) that both hold a zero point, the share of
/// the first frame's kept keys the second also kept. Only consecutive pairs count (never frame j with j+2, and a frame
/// without a zero point breaks its pair rather than joining its neighbours).
fn kept_shares(night: &FNight, keys: &KeptKeys) -> Vec<f64> {
    time_pairs(&night.rows, |_| true)
        .iter()
        .filter(|(a, b)| a.valid() && b.valid())
        .filter_map(|(a, b)| {
            let (ka, kb) = (keys.get(&a.frame_id?)?, keys.get(&b.frame_id?)?);
            (!ka.is_empty()).then(|| sorted_intersection(ka, kb) as f64 / ka.len() as f64)
        })
        .collect()
}

/// Pearson's correlation of `(x, y)` pairs; `None` under 3 pairs or no spread in either.
fn correlation(pairs: &[(f64, f64)]) -> Option<f64> {
    let n = pairs.len();
    if n < 3 {
        return None;
    }
    let nf = n as f64;
    let (mx, my) = (pairs.iter().map(|p| p.0).sum::<f64>() / nf, pairs.iter().map(|p| p.1).sum::<f64>() / nf);
    let sxx: f64 = pairs.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let syy: f64 = pairs.iter().map(|p| (p.1 - my).powi(2)).sum();
    let sxy: f64 = pairs.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    (sxx > 1e-24 && syy > 1e-24).then(|| sxy / (sxx * syy).sqrt())
}

/// The flag combinations that break rustafits' annulus rule (the annulus must start at least 0.75 px beyond the largest
/// radius, or the second pass is dropped and every frame reads `internal`), by the flags' names. The frame's FWHM `F` is
/// not known before a frame, so the rule is read for every `F`: the disc's `F` part under the annulus' (`aperture_radius_fwhm`
/// not above `annulus_start_fwhm`), the `FWHM_ref` radii under the outer pedestal radius the annulus starts from
/// (`pedestal_radii_ref` rising to its last, `aperture_radius_ref` not above it), and a gap of at least 0.75 px.
fn check_annulus_rule(p: &ForcedListParams) -> Result<()> {
    let mut why = Vec::new();
    if !(p.annulus_gap_px >= 0.75) {
        why.push(format!("--annulus_gap_px {} is under 0.75", p.annulus_gap_px));
    }
    if !(p.aperture_radius_fwhm <= p.annulus_start_fwhm) {
        why.push(format!(
            "--aperture_radius_fwhm {} is above --annulus_start_fwhm {}",
            p.aperture_radius_fwhm, p.annulus_start_fwhm
        ));
    }
    let outer = p.pedestal_radii_ref[2];
    if !(p.pedestal_radii_ref[0] <= outer && p.pedestal_radii_ref[1] <= outer) {
        why.push(format!(
            "--pedestal_radii_ref {:?} has a radius above its last (the annulus starts from the last)",
            p.pedestal_radii_ref
        ));
    }
    if !(p.aperture_radius_ref <= outer) {
        why.push(format!(
            "--aperture_radius_ref {} is above the last --pedestal_radii_ref {outer}",
            p.aperture_radius_ref
        ));
    }
    if why.is_empty() {
        Ok(())
    } else {
        bail!(
            "the annulus would reach into a disc (it must start 0.75 px beyond the largest radius, or the second fit pass is dropped and every frame reads internal): {}",
            why.join("; ")
        )
    }
}

/// Least squares `y = a + s x`: the slope, its standard error and the points; `None` under 3 points or one x.
fn ols(xs: &[f64], ys: &[f64]) -> Option<(f64, f64, usize)> {
    let n = xs.len();
    if n < 3 || ys.len() != n {
        return None;
    }
    let nf = n as f64;
    let (mx, my) = (xs.iter().sum::<f64>() / nf, ys.iter().sum::<f64>() / nf);
    let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    if sxx <= 1e-12 {
        return None;
    }
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let slope = sxy / sxx;
    let rss: f64 = xs
        .iter()
        .zip(ys)
        .map(|(x, y)| (y - my - slope * (x - mx)).powi(2))
        .sum();
    Some((slope, (rss / (nf - 2.0) / sxx).sqrt(), n))
}

/// RMS of `values` about their median (only the test below reads it since the one-pier-side B3 centres each side itself;
/// the helper went missing in 7d26e764 and left that test unbuilt).
#[cfg(test)]
fn rms_about_median(values: &[f64]) -> Option<f64> {
    let m = median(&mut values.to_vec())?;
    Some((values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / values.len() as f64).sqrt())
}

/// `y = k x + a + s side` over the points `(x, y, side)`: `(k, a, s)`; `None` for a singular design.
fn side_fit(points: &[(f64, f64, f64)]) -> Option<(f64, f64, f64)> {
    let rows: Vec<Vec<f64>> = points.iter().map(|p| vec![p.0, 1.0, p.2]).collect();
    let y: Vec<f64> = points.iter().map(|p| p.1).collect();
    let (beta, _, _) = least_squares(&rows, &y)?;
    Some((beta[0], beta[1], beta[2]))
}

/// A bar's result over its nights: a miss anywhere wins, else a pass anywhere, else it was judged on no night.
fn bar_outcome(outs: &[Outcome]) -> Outcome {
    if outs.contains(&Outcome::Fail) {
        Outcome::Fail
    } else if outs.contains(&Outcome::Pass) {
        Outcome::Pass
    } else {
        Outcome::Unjudged
    }
}

/// One bar's line: its name and limit, its outcome, a night's entry each.
fn bar_line(name: &str, limit: &str, entries: &[(String, Outcome)]) -> (String, Outcome) {
    let outs: Vec<Outcome> = entries.iter().map(|e| e.1).collect();
    let o = bar_outcome(&outs);
    let text = if entries.is_empty() {
        "judged on no night".to_string()
    } else {
        entries
            .iter()
            .map(|(t, o)| format!("{t} {}", tag(*o)))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let verdict = if entries.is_empty() {
        "NO NIGHT"
    } else {
        tag(o)
    };
    (format!("{name} {limit} {verdict}: {text}"), o)
}

fn dzp_median(
    pairs: &[(&ForcedRow, &ForcedRow)],
    value: impl Fn(&ForcedRow) -> Option<f64>,
) -> Option<f64> {
    let mut d: Vec<f64> = pairs
        .iter()
        .filter_map(|(a, b)| Some((value(b)? - value(a)?).abs()))
        .collect();
    median(&mut d)
}

/// B2 of a night: median |dZP| of consecutive pairs; the common bar on a common-set night, else the other bar and not
/// above the same statistic of today's zero point on the same pairs.
fn b2_night(night: &FNight) -> (String, Outcome) {
    let common = night.common();
    let pairs = time_pairs(&night.rows, |r| {
        r.valid() && (common || r.zero_point_today.is_some())
    });
    if pairs.len() < STABILITY_MIN_PAIRS {
        return (
            format!("{}: {} pairs, too few", night.label(), pairs.len()),
            Outcome::Unjudged,
        );
    }
    let ours = dzp_median(&pairs, |r| r.zero_point);
    let today = dzp_median(&pairs, |r| r.zero_point_today);
    let o = if common {
        judge_max(ours, B2_COMMON_MAX)
    } else {
        match (ours, today) {
            (Some(o), Some(t)) if o <= B2_OTHER_MAX && o <= t => Outcome::Pass,
            (Some(_), Some(_)) => Outcome::Fail,
            _ => Outcome::Unjudged,
        }
    };
    let bar = if common { B2_COMMON_MAX } else { B2_OTHER_MAX };
    (
        format!(
            "{}: {} mag ({} pairs; today {}; limit {bar:.3}{})",
            night.label(),
            show(ours, 4),
            pairs.len(),
            show(today, 4),
            if common {
                ", common set"
            } else {
                " and not above today"
            }
        ),
        o,
    )
}

/// B3 of a common-set night.
struct B3Night {
    points: usize,
    rms: f64,
    /// ZP - stack against the frame's FWHM: slope and its error.
    slope: f64,
    se: f64,
    /// ZP - 6r against the FWHM (the referee's own slope, on the same members and weights): slope, error, points.
    slope_zp_6r: Option<(f64, f64, usize)>,
    /// 6r - stack against the FWHM, for information.
    slope_6r_stack: Option<(f64, f64, usize)>,
}

/// The slope of `y` on `x` with a step between the pier sides when `sided` (`y = s x + a + step side`): slope, its
/// standard error, points. One-sided: plain least squares.
fn slope_by_side(xs: &[f64], ys: &[f64], sides: &[f64], sided: bool) -> Option<(f64, f64, usize)> {
    if !sided {
        return ols(xs, ys);
    }
    let rows: Vec<Vec<f64>> = xs.iter().zip(sides).map(|(x, s)| vec![*x, 1.0, *s]).collect();
    let (beta, inv_diag, rss) = least_squares(&rows, ys)?;
    let dof = xs.len().checked_sub(3).filter(|d| *d > 0)? as f64;
    Some((beta[0], (rss / dof * inv_diag[0]).sqrt(), xs.len()))
}

/// B3 of a night. On a two-sided night it is judged one pier side at a time (owner ruling F7: the flip step is not a
/// judge of the method): the rms is about each side's own median and every slope carries a step between the sides. The
/// rows are the reduced ones (spec section 5.6).
fn b3_night(night: &FNight, common: &CommonIndex) -> Option<B3Night> {
    let sided = night.two_sided();
    let mut d = Vec::new();
    let mut fw = Vec::new();
    let mut sd = Vec::new();
    let (mut x6, mut y6, mut r6, mut s6) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (r, side) in night.rows.iter().zip(&night.sides) {
        let (Some(zp), Some(level), Some(f)) = (r.zero_point, common.level(r), r.median_fwhm)
        else {
            continue;
        };
        let side = side.unwrap_or(0) as f64;
        d.push(zp - level);
        fw.push(f);
        sd.push(side);
        if let Some(z6) = r.zero_point_6r {
            x6.push(f);
            y6.push(zp - z6);
            r6.push(z6 - level);
            s6.push(side);
        }
    }
    if d.len() < B3_MIN_POINTS {
        return None;
    }
    let (slope, se, _) = slope_by_side(&fw, &d, &sd, sided)?;
    // The rms about the median of each side (of the night when it has one side).
    let mut centred = Vec::with_capacity(d.len());
    for side in [0.0, 1.0] {
        let mine: Vec<f64> = d.iter().zip(&sd).filter(|(_, s)| !sided || **s == side).map(|(v, _)| *v).collect();
        if let Some(m) = median(&mut mine.clone()) {
            centred.extend(mine.iter().map(|v| v - m));
        }
        if !sided {
            break;
        }
    }
    let rms = (centred.iter().map(|v| v * v).sum::<f64>() / centred.len().max(1) as f64).sqrt();
    let referee = |ys: &[f64]| (x6.len() >= B3_MIN_POINTS).then(|| slope_by_side(&x6, ys, &s6, sided)).flatten();
    Some(B3Night {
        points: d.len(),
        rms,
        slope,
        se,
        slope_zp_6r: referee(&y6),
        slope_6r_stack: referee(&r6),
    })
}

/// B3's slope against the stack. Within +-0.04 it passes. Beyond it the bar is missed whether or not 6r echoes it (M2: 6r
/// shares the list, positions, annulus, pedestal, screen and weights, so it cannot clear a slope the stack shows): the
/// line says which. Echoed means ZP - 6r is beyond +-0.04 on the same side, 6r agreeing with the stack against ZP; not
/// echoed means 6r agrees with ZP and the stack stands alone, the owner's as a stated limit with the three slopes.
fn b3_slope_outcome(b: &B3Night) -> (Outcome, String) {
    if b.slope.abs() <= B3_SLOPE_MAX {
        return (Outcome::Pass, String::new());
    }
    let note = match &b.slope_zp_6r {
        Some((s6, _, _)) if s6.abs() > B3_SLOPE_MAX && s6 * b.slope > 0.0 => " (echoed by 6r: a miss)".to_string(),
        Some(_) => " (not echoed by 6r, no pass: the owner's as a stated limit)".to_string(),
        None => " (no 6r to say: no pass, the owner's)".to_string(),
    };
    (Outcome::Fail, note)
}

fn b3_entry(night: &FNight, common: &CommonIndex) -> (String, Outcome) {
    let Some(b) = b3_night(night, common) else {
        return (
            format!(
                "{}: fewer than {B3_MIN_POINTS} joined frames",
                night.label()
            ),
            Outcome::Unjudged,
        );
    };
    let rms_ok = b.rms <= B3_RMS_MAX;
    let (slope_o, note) = b3_slope_outcome(&b);
    let o = if !rms_ok { Outcome::Fail } else { slope_o };
    let sl = |s: &Option<(f64, f64, usize)>| s.map_or_else(|| "n/a".to_string(), |s| format!("{:+.4}", s.0));
    (
        format!(
            "{}: rms {:.4}, slopes ZP-stack {:+.4} +- {:.4}, ZP-6r {}, 6r-stack {} ({} frames){note}",
            night.label(),
            b.rms,
            b.slope,
            b.se,
            sl(&b.slope_zp_6r),
            sl(&b.slope_6r_stack),
            b.points,
        ),
        o,
    )
}

/// B4 of a common-set night: ZP = a + kX and the stack's level fitted alike, one pier side at a time on a two-sided night;
/// the step between the sides is printed and never judged.
fn b4_entry(night: &FNight, common: &CommonIndex) -> (String, Outcome) {
    let mut pts: Vec<(f64, f64, f64, i64)> = Vec::new(); // airmass, zp, level, side
    for (r, side) in night.rows.iter().zip(&night.sides) {
        if let (Some(x), Some(zp), Some(level)) = (r.airmass, r.zero_point, common.level(r)) {
            pts.push((x, zp, level, side.unwrap_or(0)));
        }
    }
    let sides: Vec<i64> = if night.two_sided() {
        vec![0, 1]
    } else {
        vec![0]
    };
    let mut parts = Vec::new();
    let mut outs = Vec::new();
    for side in sides {
        let mine: Vec<&(f64, f64, f64, i64)> = pts
            .iter()
            .filter(|p| !night.two_sided() || p.3 == side)
            .collect();
        let name = if night.two_sided() {
            format!("side {side} ")
        } else {
            String::new()
        };
        let xs: Vec<f64> = mine.iter().map(|p| p.0).collect();
        let dx = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - xs.iter().copied().fold(f64::INFINITY, f64::min);
        if mine.len() < B4_MIN_POINTS || !(dx >= B4_MIN_DX) {
            parts.push(format!(
                "{name}unjudged (airmass range {}, {} frames)",
                show(Some(dx), 2),
                mine.len()
            ));
            outs.push(Outcome::Unjudged);
            continue;
        }
        let (zs, ls): (Vec<f64>, Vec<f64>) = mine.iter().map(|p| (p.1, p.2)).unzip();
        match (ols(&xs, &zs), ols(&xs, &ls)) {
            (Some(z), Some(l)) => {
                let o = judge_max(Some((z.0 - l.0).abs()), B4_K_TOL);
                // dk's own standard error, for the owner (RR2: no allowance in the bar): the slope of the difference
                // of the two series on the airmass, whose error holds what the two fits share.
                let dzp: Vec<f64> = zs.iter().zip(&ls).map(|(a, b)| a - b).collect();
                let dk_se = ols(&xs, &dzp).map(|d| d.1);
                parts.push(format!(
                    "{name}k {:+.4} vs stack {:+.4} (dk {:+.4} +- {}, range {dx:.2}, {} frames) {}",
                    z.0,
                    l.0,
                    z.0 - l.0,
                    show(dk_se, 4),
                    z.2,
                    tag(o)
                ));
                outs.push(o);
            }
            _ => {
                parts.push(format!("{name}no fit"));
                outs.push(Outcome::Unjudged);
            }
        }
    }
    if night.two_sided() {
        let with_side = |value: fn(&(f64, f64, f64, i64)) -> f64| {
            let p: Vec<(f64, f64, f64)> = pts.iter().map(|q| (q.0, value(q), q.3 as f64)).collect();
            side_fit(&p).map(|f| f.2)
        };
        parts.push(format!(
            "flip step zp {} stack {} (reported, not judged)",
            show(with_side(|q| q.1), 3),
            show(with_side(|q| q.2), 3)
        ));
    }
    (
        format!("{}: {}", night.label(), parts.join(", ")),
        bar_outcome(&outs),
    )
}

/// The shared extinction fit over the nights of one rig, on the frames `value` gives and `also` accepts: one k, an
/// intercept and an FWHM slope per night, a side step on a night with both sides. The frames in the fit, those without an
/// airmass counted out.
fn rig_fit(
    nights: &[&FNight],
    value: impl Fn(&ForcedRow) -> Option<f64>,
    also: impl Fn(&ForcedRow) -> bool,
) -> (Option<SharedFit>, usize, usize) {
    let mut points = Vec::new();
    let mut two_sided = Vec::new();
    let mut no_airmass = 0;
    for (slot, n) in nights.iter().enumerate() {
        let mut sides_used = [0usize; 2];
        for (r, side) in n.rows.iter().zip(&n.sides) {
            let (Some(zp), Some(f)) = (value(r), r.median_fwhm) else {
                continue;
            };
            if !also(r) {
                continue;
            }
            let Some(x) = r.airmass else {
                no_airmass += 1;
                continue;
            };
            let s = side.unwrap_or(0).clamp(0, 1);
            sides_used[s as usize] += 1;
            points.push(FitPoint {
                night: slot,
                side: s as f64,
                airmass: x,
                fwhm: f,
                zp,
            });
        }
        two_sided.push(sides_used[0] >= 3 && sides_used[1] >= 3);
    }
    let frames = points.len();
    (
        shared_extinction_fit_sided(&points, nights.len(), &two_sided),
        no_airmass,
        frames,
    )
}

fn slope_text(f: &Option<NightFit>) -> String {
    match f {
        None => "no fit".to_string(),
        Some(f) => format!(
            "{:+.4} +- {:.4} (FWHM {:.2}-{:.2}, {} frames{})",
            f.slope,
            f.slope_se,
            f.fwhm_min,
            f.fwhm_max,
            f.n,
            f.side_step
                .map_or(String::new(), |s| format!(", flip step {s:+.3}"))
        ),
    }
}

/// The 90th-percentile zero point of each object's frames, over the objects with at least [`CROSS_MIN_FRAMES`] valid
/// frames, per rig: `(spread, sets)`.
fn cross_spread(sets: &[Vec<f64>]) -> Option<(f64, usize)> {
    let mut p90: Vec<f64> = sets
        .iter()
        .filter(|z| z.len() >= CROSS_MIN_FRAMES)
        .filter_map(|z| {
            let mut s = z.clone();
            s.sort_by(|a, b| a.total_cmp(b));
            percentile(&s, 0.9)
        })
        .collect();
    if p90.len() < 2 {
        return None;
    }
    p90.sort_by(|a, b| a.total_cmp(b));
    Some((p90[p90.len() - 1] - p90[0], p90.len()))
}

/// The verdict of `verdict --forced`: each bar one line (a night's entry each), the reported figures, the decision rows,
/// the last line the class line, GO or STOP. `keys` are the kept list keys of `<x>.stars.tsv`; without them B1 is judged
/// on no night.
fn forced_verdict_lines(
    all_rows: &[ForcedRow],
    common: Option<&[CommonRow]>,
    keys: Option<&KeptKeys>,
) -> Vec<String> {
    let index = common.map(CommonIndex::new);
    // B2-B5 and B8 judge the reduced zero point (spec section 5.6, per rig, object and exposure); the decision rows and
    // today's zero point read the stored ones.
    let (reduced, windows) = reduce_rows(all_rows);
    let nights = forced_nights(&reduced, index.as_ref());
    let measured = all_rows.iter().filter(|r| !r.is_unmeasured()).count();
    let unmeasured = all_rows.len() - measured;
    let mut lines = vec![format!(
        "ZP PROBE forced: {measured} frames measured ({unmeasured} not measured), {} judged nights (>= {OSC_NIGHT_MIN_FRAMES} measured frames, {} common-set), zero points reduced in {} (rig, object, exposure) groups",
        nights.len(),
        nights.iter().filter(|n| n.common()).count(),
        windows.len()
    )];
    let mut bars: Vec<(&str, Outcome)> = Vec::new();
    let mut push = |lines: &mut Vec<String>, name: &'static str, r: (String, Outcome)| {
        lines.push(r.0);
        bars.push((name, r.1));
    };

    // B1
    let entries: Vec<(String, Outcome)> = match keys {
        None => Vec::new(),
        Some(k) => nights
            .iter()
            .map(|n| {
                let mut s = kept_shares(n, k);
                if s.len() < STABILITY_MIN_PAIRS {
                    return (
                        format!("{}: {} pairs, too few", n.label(), s.len()),
                        Outcome::Unjudged,
                    );
                }
                let lo = s.iter().copied().fold(f64::INFINITY, f64::min);
                let med = median(&mut s).unwrap_or(f64::NAN);
                (
                    format!(
                        "{}: min {lo:.3}, median {med:.3} over {} pairs",
                        n.label(),
                        s.len()
                    ),
                    judge_max(Some(B1_MIN_SHARE - lo), 0.0),
                )
            })
            .collect(),
    };
    push(
        &mut lines,
        "B1",
        bar_line(
            "B1 members of non-zero weight left by the screen that the successor also has (valid pairs)",
            &format!("(every pair >= {B1_MIN_SHARE:.2})"),
            &entries,
        ),
    );

    // B2
    let entries: Vec<_> = nights.iter().map(b2_night).collect();
    push(
        &mut lines,
        "B2",
        bar_line(
            "B2 stability, median |dZP|",
            "(<= 0.015 common set; else <= 0.020 and not above today)",
            &entries,
        ),
    );

    // B3
    let entries: Vec<_> = match &index {
        Some(c) => nights
            .iter()
            .filter(|n| n.common())
            .map(|n| b3_entry(n, c))
            .collect(),
        None => Vec::new(),
    };
    push(
        &mut lines,
        "B3",
        bar_line(
            "B3 ZP - 2.5 log10(t_flux/t), common-set nights",
            &format!("(reduced; rms <= {B3_RMS_MAX:.3}; |slope on FWHM| <= {B3_SLOPE_MAX:.2} against the stack, echoed by 6r or not)"),
            &entries,
        ),
    );

    // B4
    let entries: Vec<_> = match &index {
        Some(c) => nights
            .iter()
            .filter(|n| n.common())
            .map(|n| b4_entry(n, c))
            .collect(),
        None => Vec::new(),
    };
    push(
        &mut lines,
        "B4",
        bar_line(
            "B4 extinction k against the stack's",
            &format!("(|dk| <= {B4_K_TOL:.2} where the airmass range >= {B4_MIN_DX:.1}, one side at a time)"),
            &entries,
        ),
    );

    // B5: nights without a common set, one night at a time (RR1): `zp = a + k X + s F` (+ a step between the sides)
    // fitted jointly on the night, so a night's own extinction is not read as a seeing term.
    let mut entries: Vec<(String, Outcome)> = Vec::new();
    for n in nights.iter().filter(|n| !n.common()) {
        let (fit, no_airmass, frames) = rig_fit(&[n], |r| r.zero_point, |_| true);
        match fit {
            None => entries.push((
                format!("{}: no fit ({frames} frames, {no_airmass} without an airmass)", n.label()),
                Outcome::Unjudged,
            )),
            Some(f) => {
                let nf = f.nights[0];
                let o = match nf {
                    Some(x) if x.slope_se <= SLOPE_SE_MAX => judge_max(Some(x.slope.abs()), SLOPE_MAX),
                    _ => Outcome::Unjudged,
                };
                entries.push((
                    format!(
                        "{} [k {:+.3} +- {:.3}, {no_airmass} without an airmass]: {}",
                        n.label(),
                        f.k,
                        f.k_se,
                        slope_text(&nf)
                    ),
                    o,
                ));
            }
        }
    }
    push(
        &mut lines,
        "B5",
        bar_line(
            "B5 seeing slope, nights without a common set, one night's own fit",
            &format!("(reduced; |s| <= {SLOPE_MAX:.2} where its error <= {SLOPE_SE_MAX:.3})"),
            &entries,
        ),
    );

    // B6
    let entries: Vec<_> = nights
        .iter()
        .map(|n| {
            let counted: Vec<&&ForcedRow> = n
                .rows
                .iter()
                .filter(|r| r.refusal != "frame_verdict")
                .collect();
            let exempt = n.rows.len() - counted.len();
            let valid = counted.iter().filter(|r| r.valid()).count();
            let share = (!counted.is_empty()).then(|| valid as f64 / counted.len() as f64);
            let limit = if n.qhy() { B6_COMPLETE } else { B6_MIN };
            (
                format!(
                    "{}: {} ({valid}/{}; {exempt} with a plane verdict; limit {limit:.2})",
                    n.label(),
                    show(share, 3),
                    counted.len()
                ),
                judge_max(share.map(|s| limit - s), 0.0),
            )
        })
        .collect();
    push(
        &mut lines,
        "B6",
        bar_line(
            "B6 valid / measured",
            "(>= 0.90; 6.5's QHY268M sets 1.00; plane verdicts exempt)",
            &entries,
        ),
    );

    // B7
    let entries: Vec<_> = nights
        .iter()
        .map(|n| {
            let mut c: Vec<f64> = n.rows.iter().filter_map(|r| r.centre_median_px).collect();
            let mut f: Vec<f64> = n.rows.iter().filter_map(|r| r.fwhm_ref).collect();
            let (m, fr) = (median(&mut c), median(&mut f));
            let o = match (m, fr) {
                (Some(m), Some(fr)) => judge_max(Some(m), B7_MAX_FWHM_REF * fr),
                _ => Outcome::Unjudged,
            };
            (
                format!(
                    "{}: {} px (limit {} = {B7_MAX_FWHM_REF:.2} FWHM_ref {})",
                    n.label(),
                    show(m, 3),
                    show(fr.map(|f| f * B7_MAX_FWHM_REF), 3),
                    show(fr, 2)
                ),
                o,
            )
        })
        .collect();
    push(
        &mut lines,
        "B7",
        bar_line(
            "B7 map stars, median |refit - moved seed|",
            &format!("(<= {B7_MAX_FWHM_REF:.2} FWHM_ref)"),
            &entries,
        ),
    );

    // B8: mono nights, the seeing slope of the forced zero point against today's on the same frames, each night fitted
    // on its own (RR1), the forced zero point reduced and today's as it is.
    let mut entries: Vec<(String, Outcome)> = Vec::new();
    for n in nights.iter().filter(|n| n.mono()) {
        let both = |r: &ForcedRow| r.zero_point.is_some() && r.zero_point_today.is_some();
        let (ours, _, _) = rig_fit(&[n], |r| r.zero_point, both);
        let (today, _, _) = rig_fit(&[n], |r| r.zero_point_today, both);
        let o_fit = ours.as_ref().and_then(|f| f.nights[0]);
        let t_fit = today.as_ref().and_then(|f| f.nights[0]);
        let o = match (o_fit, t_fit) {
            (Some(a), Some(t)) if a.slope_se <= SLOPE_SE_MAX => {
                if a.slope.abs() <= SLOPE_MAX && a.slope.abs() <= t.slope.abs() + B8_SIGMAS * t.slope_se {
                    Outcome::Pass
                } else {
                    Outcome::Fail
                }
            }
            _ => Outcome::Unjudged,
        };
        entries.push((
            format!("{}: {} against today {}", n.label(), slope_text(&o_fit), slope_text(&t_fit)),
            o,
        ));
    }
    push(
        &mut lines,
        "B8",
        bar_line(
            "B8 mono nights, seeing slope, one night's own fit",
            &format!("(reduced; |s| <= {SLOPE_MAX:.2} and <= |today| + {B8_SIGMAS:.0} sigma, error <= {SLOPE_SE_MAX:.3})"),
            &entries,
        ),
    );

    // B9
    let compared: Vec<&ForcedRow> = all_rows.iter().filter(|r| !r.identity.is_empty()).collect();
    let bad: Vec<&str> = compared
        .iter()
        .filter(|r| r.identity != "ok")
        .map(|r| r.identity.as_str())
        .collect();
    let entries = if compared.is_empty() {
        Vec::new()
    } else {
        vec![(
            format!(
                "{} frames compared, {} differ{}; rustafits' test the_analysis_measurement_is_identical_with_and_without_a_list judges the layouts",
                compared.len(),
                bad.len(),
                if bad.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", bad.join(","))
                }
            ),
            if bad.is_empty() { Outcome::Pass } else { Outcome::Fail },
        )]
    };
    push(
        &mut lines,
        "B9",
        bar_line(
            "B9 Analysis numbers with and without a list",
            "(every compared frame ok)",
            &entries,
        ),
    );

    // Reported only
    let med = |rows: &[&ForcedRow], f: &dyn Fn(&ForcedRow) -> Option<f64>| {
        median(&mut rows.iter().filter_map(|r| f(r)).collect::<Vec<_>>())
    };
    let min_of = |rows: &[&ForcedRow], f: &dyn Fn(&ForcedRow) -> Option<f64>| {
        rows.iter().filter_map(|r| f(r)).fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.min(v))))
    };
    let per_night: Vec<String> = nights
        .iter()
        .map(|n| {
            format!(
                "{}: list {} (seed rule removed {}, {} pairs), members {} (tapered {}), map {}/{} kept {}, pedestal {} (clip kept min {} med {}, r with the reduced ZP {})",
                n.label(),
                show(med(&n.rows, &|r| r.list_stars.map(|v| v as f64)), 0),
                show(med(&n.rows, &|r| r.detected_neighbours_removed.map(|v| v as f64)), 0),
                show(med(&n.rows, &|r| r.seed_calibration_pairs.map(|v| v as f64)), 0),
                show(med(&n.rows, &|r| r.members.map(|v| v as f64)), 0),
                show(med(&n.rows, &|r| r.tapered.map(|v| v as f64)), 0),
                show(med(&n.rows, &|r| r.map_members.map(|v| v as f64)), 0),
                show(med(&n.rows, &|r| r.map_stars.map(|v| v as f64)), 0),
                show(med(&n.rows, &|r| r.map_kept_share), 3),
                show(med(&n.rows, &|r| r.pedestal), 4),
                show(min_of(&n.rows, &|r| r.pedestal_kept_share), 3),
                show(med(&n.rows, &|r| r.pedestal_kept_share), 3),
                show(
                    correlation(
                        &n.rows.iter().filter_map(|r| Some((r.zero_point?, r.pedestal_kept_share?))).collect::<Vec<_>>()
                    ),
                    2
                ),
            )
        })
        .collect();
    lines.push(format!(
        "reported, per night (median over its frames): {}",
        if per_night.is_empty() {
            "no judged night".to_string()
        } else {
            per_night.join("; ")
        }
    ));
    let per_night: Vec<String> = nights
        .iter()
        .map(|n| {
            let mut refusals: BTreeMap<&str, usize> = BTreeMap::new();
            for r in n.rows.iter().filter(|r| !r.refusal.is_empty()) {
                *refusals.entry(r.refusal.as_str()).or_default() += 1;
            }
            let refused = if refusals.is_empty() {
                "none".to_string()
            } else {
                refusals.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(",")
            };
            let both: Vec<&ForcedRow> = n
                .rows
                .iter()
                .copied()
                .filter(|r| r.window_slope.is_some() && r.slope_remeasured.is_some())
                .collect();
            let group = n.rows.first().and_then(|r| windows.get(&reduction_key(r)));
            let valid = n.rows.iter().filter(|r| r.valid()).count();
            let with_slope = n.rows.iter().filter(|r| r.valid() && r.window_slope.is_some()).count();
            format!(
                "{}: window slope analytic {} / re-measured {} ({} frames), set slope {}, {with_slope}/{valid} with one, refusals {refused}",
                n.label(),
                show(med(&both, &|r| r.window_slope), 4),
                show(med(&both, &|r| r.slope_remeasured), 4),
                both.len(),
                show(group.map(|w| w.slope), 4),
            )
        })
        .collect();
    lines.push(format!(
        "reported, per night (window slope, mag per mag): {}",
        if per_night.is_empty() {
            "no judged night".to_string()
        } else {
            per_night.join("; ")
        }
    ));

    // The cost: per frame set the medians of the fitter's three steps and of the list alone (the call less the plain
    // measurement, on the frames measured both ways); a set above `LIST_COST_MAX_MS` is named (RR4's STOP).
    let mut call: Vec<f64> = all_rows.iter().filter_map(|r| r.call_ms).collect();
    let mut list_ms: Vec<f64> = all_rows
        .iter()
        .filter_map(|r| Some(r.call_ms? - r.plain_ms?))
        .collect();
    let mut plain: Vec<f64> = all_rows.iter().filter_map(|r| r.plain_ms).collect();
    let mut kb: Vec<f64> = all_rows.iter().filter_map(|r| r.list_kb).collect();
    let rss = all_rows
        .iter()
        .filter_map(|r| r.rss_mb)
        .fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))));
    lines.push(format!(
        "list cost: median call {} ms, plain measurement {} ms, list alone {} ms (frames measured both ways), list structures {} kB per frame, peak RSS {} MB",
        show(median(&mut call), 0),
        show(median(&mut plain), 0),
        show(median(&mut list_ms), 0),
        show(median(&mut kb), 0),
        show(rss, 0)
    ));
    let mut sets: BTreeMap<(String, String), Vec<&ForcedRow>> = BTreeMap::new();
    for r in all_rows.iter().filter(|r| !r.is_unmeasured()) {
        sets.entry((rig_short_of(&r.rig_key()), r.object.clone())).or_default().push(r);
    }
    let mut over = Vec::new();
    let per_set: Vec<String> = sets
        .iter()
        .map(|((rig, object), rows)| {
            let alone = med(rows, &|r| Some(r.call_ms? - r.plain_ms?));
            if alone.is_some_and(|a| a > LIST_COST_MAX_MS) {
                over.push(format!("{rig}/{object}"));
            }
            format!(
                "{rig}/{object} {}/{}/{}/{}",
                show(med(rows, &|r| r.list_fit_ms), 0),
                show(med(rows, &|r| r.list_refit_ms), 0),
                show(med(rows, &|r| r.list_ap_ms), 0),
                show(alone, 0)
            )
        })
        .collect();
    lines.push(format!(
        "list cost per set, median ms fit/refit/apertures/list alone: {}; list alone over {LIST_COST_MAX_MS:.0} ms: {}",
        if per_set.is_empty() { "no set".to_string() } else { per_set.join("; ") },
        if over.is_empty() { "none".to_string() } else { over.join(", ") }
    ));

    // Decision rows (not bars): the cross-target spread per rig, forced against today on the same frames.
    // Per rig, per object: the forced and today's zero points of the frames that have both.
    type PerObject = BTreeMap<String, (Vec<f64>, Vec<f64>)>;
    let mut by_rig: BTreeMap<String, PerObject> = BTreeMap::new();
    for r in all_rows.iter().filter(|r| !r.is_unmeasured()) {
        if let (Some(z), Some(t)) = (r.zero_point, r.zero_point_today) {
            let e = by_rig
                .entry(r.rig_key())
                .or_default()
                .entry(r.object.clone())
                .or_default();
            e.0.push(z);
            e.1.push(t);
        }
    }
    let mut decisions = Vec::new();
    for (rig, objects) in &by_rig {
        let ours: Vec<Vec<f64>> = objects.values().map(|v| v.0.clone()).collect();
        let today: Vec<Vec<f64>> = objects.values().map(|v| v.1.clone()).collect();
        if let (Some((o, n)), Some((t, _))) = (cross_spread(&ours), cross_spread(&today)) {
            let short = rig.split('|').next().unwrap_or(rig).to_string()
                + "/"
                + rig.split('|').next_back().unwrap_or("");
            let committed = if rig.starts_with("QHY268M") && rig.ends_with("|L") {
                format!(", 6.5 committed {CROSS_COMMITTED_6_5_MAG:.3}")
            } else {
                String::new()
            };
            decisions.push(format!(
                "{short} {o:.4} over {n} targets (today {t:.4}{committed})"
            ));
        }
    }
    lines.push(format!(
        "decision rows, cross-target spread of the 90th-percentile ZP (not a bar; QHY268M L limit 0.10 and not above today): {}",
        if decisions.is_empty() { "no rig with two targets of 30 valid frames".to_string() } else { decisions.join("; ") }
    ));
    lines.push("no data in the corpus, unjudged: haze (largest dip 0.09 mag), under 16 bit, long focal length, high latitude; the faint-edge extension is unjudged".to_string());

    // The class line: GO only when every bar passed on at least one night and no night missed.
    let fails: Vec<&str> = bars
        .iter()
        .filter(|(_, o)| *o == Outcome::Fail)
        .map(|(n, _)| *n)
        .collect();
    let none: Vec<&str> = bars
        .iter()
        .filter(|(_, o)| *o == Outcome::Unjudged)
        .map(|(n, _)| *n)
        .collect();
    lines.push(if fails.is_empty() && none.is_empty() {
        "ZP PROBE forced: GO".to_string()
    } else {
        format!(
            "ZP PROBE forced: STOP(missed {}; judged on no night {})",
            if fails.is_empty() {
                "none".to_string()
            } else {
                fails.join(",")
            },
            if none.is_empty() {
                "none".to_string()
            } else {
                none.join(",")
            }
        )
    });
    // At most 30 lines: the reported lines are the ones to give up, the bars and the class line stay.
    while lines.len() > VERDICT_MAX_LINES {
        lines.remove(lines.len() - 2);
    }
    lines
}

fn cmd_verdict_forced(args: &Args, common_path: Option<String>) -> Result<()> {
    let mut rows = Vec::new();
    let mut keys: KeptKeys = HashMap::new();
    let mut keys_ok = true;
    for path in args
        .positional
        .iter()
        .filter(|p| !p.ends_with(".stars.tsv"))
    {
        let (header, data) = read_tsv(path)?;
        rows.extend(data.iter().map(|f| ForcedRow::from_fields(&header, f)));
        let sp = stars_path(path);
        if let Err(e) = read_kept_keys(&sp, &mut keys) {
            eprintln!("zp_probe: no list stars for {path}: {e:#}; B1 is judged on no night");
            keys_ok = false;
        }
    }
    if rows.is_empty() {
        bail!("verdict --forced needs at least one forced TSV");
    }
    let common = match common_path {
        Some(p) => Some(read_common(&p)?),
        None => None,
    };
    for line in forced_verdict_lines(&rows, common.as_deref(), keys_ok.then_some(&keys)) {
        println!("{line}");
    }
    Ok(())
}

/// The `--common` TSV (`common` subcommand's output).
fn read_common(path: &str) -> Result<Vec<CommonRow>> {
    let (header, data) = read_tsv(path)?;
    Ok(data
        .iter()
        .map(|f| CommonRow {
            frame_id: opt_i64(col(&header, f, "frame_id")),
            file_name: col(&header, f, "file_name").to_string(),
            group_key: col(&header, f, "group_key").to_string(),
            planes: col(&header, f, "planes").trim().parse().unwrap_or(0),
            tflux: [
                opt_f64(col(&header, f, "common_tflux_0")),
                opt_f64(col(&header, f, "common_tflux_1")),
                opt_f64(col(&header, f, "common_tflux_2")),
            ],
        })
        .collect())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = match argv.first().map(String::as_str) {
        Some(sub @ ("measure" | "stars" | "common" | "verdict" | "bigap" | "forced")) => {
            Args::parse(&argv[1..]).and_then(|a| match sub {
                "measure" => cmd_measure(a),
                "forced" => cmd_forced(a),
                "stars" => cmd_stars(a),
                "common" => cmd_common(a),
                "bigap" => cmd_bigap(a),
                _ => cmd_verdict(a),
            })
        }
        _ => Err(anyhow!(
            "usage: zp_probe <measure|stars|common|verdict|bigap|forced> ... (see the file's header)"
        )),
    };
    if let Err(e) = result {
        eprintln!("zp_probe: {e:#}");
        std::process::exit(2);
    }
}

// ---------------------------------------------------------------------------
// Tests: the verdict arithmetic on hand-made rows
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_760_000_000;

    fn parsed(argv: &[&str]) -> Result<(ForcedListParams, ZpParams, Vec<(String, String)>)> {
        let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        take_forced_params(&mut Args::parse(&argv)?)
    }

    #[test]
    fn a_forced_flag_reaches_the_list_params() {
        let d = ForcedListParams::default();
        let (fp, zp, given) = parsed(&[
            "--cap",
            "800",
            "--window_bright_margin_mag",
            "1.25",
            "--map_max_stars",
            "128",
            "--min_stars",
            "40",
            "--central_fraction",
            "0.8",
        ])
        .unwrap();
        assert_eq!((fp.cap, fp.map_max_stars, fp.window_bright_margin_mag), (800, 128, 1.25));
        // Everything not named keeps its default.
        assert_eq!(
            ForcedListParams {
                cap: d.cap,
                map_max_stars: d.map_max_stars,
                window_bright_margin_mag: d.window_bright_margin_mag,
                min_stars: d.min_stars,
                ..fp
            },
            d
        );
        // A name both parameter sets have sets both; a ZpParams-only name reaches ZpParams only.
        assert_eq!((fp.min_stars, zp.min_stars), (40, 40));
        assert_eq!(zp.central_fraction, 0.8);
        assert_eq!(fp.clip_sigma, d.clip_sigma);
        // The echo carries the flags as given and the parameters used.
        let text = forced_flags_text(&given, &fp, &zp);
        assert!(text.contains("flag --cap 800") && text.contains("cap: 800"), "{text}");
        assert!(text.contains("flag --min_stars 40") && text.contains("min_stars: 40"), "{text}");
        let (fp0, zp0, none) = parsed(&[]).unwrap();
        assert_eq!((fp0, zp0.min_stars), (d, ZpParams::default().min_stars));
        assert!(forced_flags_text(&none, &fp0, &zp0).contains("flags: none"));
    }

    #[test]
    fn every_forced_list_field_has_a_flag() {
        // A field added to `ForcedListParams` without a flag fails to compile in `forced_list_params`; this checks each
        // flag is wired to its own field (a value no default has reaches only that field).
        let d = ForcedListParams::default();
        let flags: &[(&str, fn(&ForcedListParams) -> f64)] = &[
            ("fwhm_floor_px", |p| p.fwhm_floor_px),
            ("fwhm_ref_arcsec", |p| p.fwhm_ref_arcsec),
            ("anchor_min_matches", |p| p.anchor_min_matches as f64),
            ("window_peak_fraction", |p| p.window_peak_fraction),
            ("window_bright_margin_mag", |p| p.window_bright_margin_mag),
            ("window_faint_width_mag", |p| p.window_faint_width_mag),
            ("window_faint_max_mag", |p| p.window_faint_max_mag),
            ("min_isolated", |p| p.min_isolated as f64),
            ("window_ramp_mag", |p| p.window_ramp_mag),
            ("region_fraction", |p| p.region_fraction),
            ("edge_margin_fwhm", |p| p.edge_margin_fwhm),
            ("isolation_dmag", |p| p.isolation_dmag),
            ("isolation_radius_fwhm", |p| p.isolation_radius_fwhm),
            ("bright_dmag", |p| p.bright_dmag),
            ("bright_radius_fwhm", |p| p.bright_radius_fwhm),
            ("own_seed_radius_px", |p| p.own_seed_radius_px),
            ("detected_neighbour_min_peak_sigma", |p| p.detected_neighbour_min_peak_sigma),
            ("seed_calibration_min_pairs", |p| p.seed_calibration_min_pairs as f64),
            ("cap", |p| p.cap as f64),
            ("map_window_mag", |p| p.map_window_mag),
            ("map_max_stars", |p| p.map_max_stars as f64),
            ("map_min_snr", |p| p.map_min_snr),
            ("map_radius_fwhm", |p| p.map_radius_fwhm),
            ("map_clip_rounds", |p| p.map_clip_rounds as f64),
            ("map_clip_sigma", |p| p.map_clip_sigma),
            ("map_clip_floor_px", |p| p.map_clip_floor_px),
            ("map_min_members", |p| p.map_min_members as f64),
            ("map_min_kept_share", |p| p.map_min_kept_share),
            ("centre_tolerance_px", |p| p.centre_tolerance_px),
            ("centre_tolerance_fwhm", |p| p.centre_tolerance_fwhm),
            ("aperture_radius_ref", |p| p.aperture_radius_ref),
            ("aperture_radius_fwhm", |p| p.aperture_radius_fwhm),
            ("pedestal_clip_sigma", |p| p.pedestal_clip_sigma),
            ("pedestal_clip_rounds", |p| p.pedestal_clip_rounds as f64),
            ("pedestal_min_points", |p| p.pedestal_min_points as f64),
            ("annulus_area_px", |p| p.annulus_area_px),
            ("annulus_start_fwhm", |p| p.annulus_start_fwhm),
            ("annulus_gap_px", |p| p.annulus_gap_px),
            ("peak_radius_ref", |p| p.peak_radius_ref),
            ("clip_sigma", |p| p.clip_sigma),
            ("min_stars", |p| p.min_stars as f64),
            ("max_sigma_mag", |p| p.max_sigma_mag),
            ("screen_taper_sigma", |p| p.screen_taper_sigma),
            ("slope_min_ramp_members", |p| p.slope_min_ramp_members as f64),
            ("decimate", |p| p.decimate as f64),
            ("anchor_shift_mag", |p| p.anchor_shift_mag),
        ];
        for &(name, read) in flags {
            let (fp, _, _) = parsed(&[&format!("--{name}"), "7"]).unwrap();
            assert_eq!(read(&fp), 7.0, "--{name}");
            assert_ne!(read(&d), 7.0, "--{name}: pick a probe value that is not the default");
            assert_ne!(fp, d, "--{name} changed nothing");
        }
        // The one array field: its own flag reaches it and nothing else.
        let (fp, _, _) = parsed(&["--pedestal_radii_ref", "7,8,9"]).unwrap();
        assert_eq!(fp.pedestal_radii_ref, [7.0, 8.0, 9.0]);
        assert_eq!(ForcedListParams { pedestal_radii_ref: d.pedestal_radii_ref, ..fp }, d);
        // Every field is named below (a field added without a flag fails here to compile): the flag list and this
        // destructuring are the two halves of the same check.
        let ForcedListParams {
            fwhm_floor_px: _, fwhm_ref_arcsec: _, anchor_min_matches: _, window_peak_fraction: _,
            window_bright_margin_mag: _, window_faint_width_mag: _, window_faint_max_mag: _, min_isolated: _,
            window_ramp_mag: _, region_fraction: _, edge_margin_fwhm: _, isolation_dmag: _, isolation_radius_fwhm: _,
            bright_dmag: _, bright_radius_fwhm: _, detected_neighbours: _, own_seed_radius_px: _,
            detected_neighbour_min_peak_sigma: _, seed_calibration_min_pairs: _, cap: _, map_window_mag: _, map_max_stars: _, map_min_snr: _,
            map_radius_fwhm: _, map_clip_rounds: _, map_clip_sigma: _, map_clip_floor_px: _, map_min_members: _,
            map_min_kept_share: _, centre_tolerance_px: _, centre_tolerance_fwhm: _, aperture_radius_ref: _,
            aperture_radius_fwhm: _, pedestal_radii_ref: _, pedestal_clip_sigma: _, pedestal_clip_rounds: _,
            pedestal_min_points: _, annulus_area_px: _, annulus_start_fwhm: _, annulus_gap_px: _, peak_radius_ref: _,
            clip_sigma: _, min_stars: _, max_sigma_mag: _, screen_taper_sigma: _, slope_min_ramp_members: _,
            decimate: _, anchor_shift_mag: _,
        } = d;
        // The one boolean field: read as true / false.
        let (fp, _, _) = parsed(&["--detected_neighbours", "false"]).unwrap();
        assert!(d.detected_neighbours && !fp.detected_neighbours);
        assert_eq!(ForcedListParams { detected_neighbours: d.detected_neighbours, ..fp }, d);
        assert_eq!(flags.len() + 2, 48, "46 numeric fields have a flag each, the array and the boolean have their own");
    }

    #[test]
    fn the_pedestal_radii_flag_reads_three_numbers() {
        let (fp, _, _) = parsed(&["--pedestal_radii_ref", "2.5, 3.5,5"]).unwrap();
        assert_eq!(fp.pedestal_radii_ref, [2.5, 3.5, 5.0]);
        assert!(parsed(&["--pedestal_radii_ref", "3,4"]).is_err());
        assert!(parsed(&["--pedestal_radii_ref", "3,4,x"]).is_err());
    }

    #[test]
    fn an_unknown_or_unreadable_forced_flag_is_an_error() {
        let e = parsed(&["--capp", "800"]).unwrap_err().to_string();
        assert!(e.contains("unknown flag --capp"), "{e}");
        let e = parsed(&["--cap", "many"]).unwrap_err().to_string();
        assert!(e.contains("--cap") && e.contains("cannot read"), "{e}");
        // A typo beside a good flag is still refused (nothing runs with a default in its place).
        assert!(parsed(&["--cap", "800", "--window_bright_margin", "1"]).is_err());
    }

    fn iso(t_s: i64) -> String {
        DateTime::from_timestamp(t_s, 0).unwrap().to_rfc3339()
    }

    fn row(night: &str, t_s: i64, zp: Option<f64>, fwhm: f64, stars: i64) -> MeasureRow {
        MeasureRow {
            frame_id: Some(t_s),
            file_name: format!("f{t_s}.fits"),
            object: "T".into(),
            filter: "L".into(),
            night: night.into(),
            date_obs: iso(t_s),
            exptime: Some(180.0),
            stars_detected: Some(stars),
            median_fwhm: Some(fwhm),
            zero_point: zp,
            ..MeasureRow::default()
        }
    }

    fn refs(rows: &[MeasureRow]) -> Vec<&MeasureRow> {
        rows.iter().collect()
    }

    #[test]
    fn stability_is_the_median_of_consecutive_differences() {
        // Differences 0.01, 0.05, 0.02 -> median 0.02 (the outlier step does not move it).
        let rows = vec![
            row("n", T0, Some(21.00), 3.0, 100),
            row("n", T0 + 180, Some(21.01), 3.0, 100),
            row("n", T0 + 360, Some(21.06), 3.0, 100),
            row("n", T0 + 540, Some(21.04), 3.0, 100),
        ];
        let pairs = consecutive_pairs(&refs(&rows));
        assert_eq!(pairs.len(), 3);
        let s = stability(&pairs).unwrap();
        assert!((s - 0.02).abs() < 1e-9, "{s}");
    }

    #[test]
    fn pairs_further_than_15_minutes_are_not_consecutive() {
        let rows = vec![
            row("n", T0, Some(21.00), 3.0, 100),
            row("n", T0 + 899, Some(21.10), 3.0, 100),
            row("n", T0 + 899 + 900, Some(21.50), 3.0, 100),
        ];
        let pairs = consecutive_pairs(&refs(&rows));
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        assert!((pairs[0].dzp - 0.10).abs() < 1e-9);
    }

    #[test]
    fn the_clear_night_is_the_one_with_most_stars() {
        let mut rows = Vec::new();
        for i in 0..25 {
            rows.push(row("a", T0 + i * 180, Some(21.0), 3.0, 500));
            rows.push(row("b", T0 + 86_400 + i * 180, Some(21.0), 3.0, 900));
        }
        // The richest night has only 10 frames: not eligible.
        for i in 0..10 {
            rows.push(row("c", T0 + 172_800 + i * 180, Some(21.0), 3.0, 5000));
        }
        assert_eq!(clear_night(&refs(&rows)).as_deref(), Some("b"));
        // Nothing reaches 20 frames: no clear night.
        let few: Vec<MeasureRow> = rows.iter().filter(|r| r.night == "c").cloned().collect();
        assert_eq!(clear_night(&refs(&few)), None);
    }

    #[test]
    fn a_slope_needs_twenty_pairs() {
        // dzp = 0.02·dFWHM + 0.005: slope 0.02 mag/px whatever the intercept.
        let steps = [0.3, -0.4, 0.5, -0.6];
        let mk = |n: usize| {
            (0..n)
                .map(|i| Pair {
                    dfwhm: steps[i % 4],
                    dzp: 0.02 * steps[i % 4] + 0.005,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(seeing_slope(&mk(19)), None);
        let s = seeing_slope(&mk(20)).unwrap();
        assert_eq!(s.pairs, 20);
        assert!((s.slope - 0.02).abs() < 1e-12, "{}", s.slope);
        // The fit through the origin is pulled by the intercept: it is not the judged one.
        assert!(
            (s.through_origin - 0.02).abs() > 1e-4,
            "{}",
            s.through_origin
        );
        // Pairs under 0.2 px do not count toward the 20.
        let mut mixed = mk(19);
        mixed.extend(vec![
            Pair {
                dzp: 0.5,
                dfwhm: 0.1
            };
            30
        ]);
        assert_eq!(seeing_slope(&mixed), None);
    }

    #[test]
    fn cross_target_needs_three_sets() {
        let set =
            |base: f64, n: usize| (0..n).map(|i| base + i as f64 * 1e-4).collect::<Vec<f64>>();
        // Two sets of 40 and one of 10: only two count.
        assert_eq!(
            cross_target(&[set(21.0, 40), set(21.05, 40), set(21.5, 10)]),
            CrossTarget::Unjudged { sets: 2 }
        );
        match cross_target(&[set(21.0, 40), set(21.05, 40), set(21.08, 40)]) {
            CrossTarget::Judged { spread, sets } => {
                assert_eq!(sets, 3);
                assert!((spread - 0.08).abs() < 1e-9, "{spread}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_ninetieth_percentile_is_not_the_maximum() {
        let set = |n: usize| (0..n).map(|i| 21.0 + i as f64 * 1e-4).collect::<Vec<f64>>();
        // One set carries a single frame 0.5 mag above the rest.
        let mut outlier = set(29);
        outlier.push(21.5);
        match cross_target(&[set(30), set(30), outlier]) {
            CrossTarget::Judged { spread, sets } => {
                assert_eq!(sets, 3);
                assert!(spread < 1e-6, "the outlier moved the spread to {spread}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_slope_keeps_its_sign_through_the_pairing() {
        // FWHM falls (steps -0.3 and -0.5 alternately) while the zero point
        // rises by 0.02 mag per px of FWHM lost: dzp / dFWHM = -0.02.
        let rows: Vec<MeasureRow> = (0..25)
            .map(|i| {
                let fwhm = 12.0 - 0.4 * i as f64 + 0.1 * (i % 2) as f64;
                row(
                    "n",
                    T0 + i * 180,
                    Some(21.0 + 0.02 * (12.0 - fwhm)),
                    fwhm,
                    100,
                )
            })
            .collect();
        let pairs = consecutive_pairs(&refs(&rows));
        assert_eq!(pairs.len(), 24);
        let s = seeing_slope(&pairs).unwrap();
        assert_eq!(s.pairs, 24);
        assert!(s.slope < 0.0, "{}", s.slope);
        assert!((s.slope + 0.02).abs() < 1e-9, "{}", s.slope);
    }

    #[test]
    fn a_failed_bar_wins_over_an_unjudged_one() {
        let bars = [
            ("coverage", Outcome::Pass),
            ("geometry", Outcome::Fail),
            ("stability", Outcome::Unjudged),
        ];
        assert_eq!(
            class_line("broadband", &bars),
            "ZP PROBE broadband: FAIL(geometry)"
        );
        assert_eq!(
            class_line("osc", &bars[2..]),
            "ZP PROBE osc: UNJUDGED(stability)"
        );
        assert_eq!(class_line("osc", &bars[..1]), "ZP PROBE osc: PASS");
    }

    #[test]
    fn the_verdict_ends_in_one_class_line_within_thirty_lines() {
        // Two nights of 30 frames, steady zero point: every bar passes.
        let mut rows = Vec::new();
        for night in 0..2 {
            for i in 0..30 {
                let t = T0 + night * 86_400 + i * 180;
                let mut r = row(
                    &format!("n{night}"),
                    t,
                    Some(21.3 + 0.001 * (i % 3) as f64),
                    3.0 + 0.3 * (i % 2) as f64,
                    800 + 10 * night,
                );
                r.median_match_px = Some(0.4);
                rows.push(r);
            }
        }
        // A light the probe could not measure stays out of the coverage denominator.
        let mut gone = row("n0", T0 + 5, None, 3.0, 800);
        gone.refusal = "missing_file".into();
        rows.push(gone);
        let lines = verdict_lines("broadband", &rows, None);
        assert!(lines.len() <= VERDICT_MAX_LINES);
        assert_eq!(lines.last().unwrap(), "ZP PROBE broadband: PASS");
        assert!(lines[1].contains("coverage: 1.000"), "{}", lines[1]);
        assert!(lines[1].contains("1 missing_file"), "{}", lines[1]);

        // Forty sets of three frames: none can be judged, every one says so, and
        // the cut of the set lines is announced.
        let many: Vec<MeasureRow> = (0..40)
            .flat_map(|k| {
                (0..3).map(move |i| {
                    let mut r = row("n", T0 + i * 180, Some(21.3), 3.0, 100);
                    r.object = format!("S{k:02}");
                    r
                })
            })
            .collect();
        let lines = verdict_lines("broadband", &many, None);
        assert_eq!(lines.len(), VERDICT_MAX_LINES);
        assert_eq!(
            lines.last().unwrap(),
            "ZP PROBE broadband: UNJUDGED(geometry,stability,seeing)"
        );
        assert!(
            lines[lines.len() - 2].starts_with("… "),
            "{}",
            lines[lines.len() - 2]
        );
        assert!(lines[lines.len() - 2].ends_with(" more set lines not shown"));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("stability S00/L: not judged (no night with 20 frames")));
    }

    // ---- the OSC verdict ----

    /// Deterministic spread in `[-1, 1]`, no two neighbours alike.
    fn wobble(i: usize) -> f64 {
        ((i * 7919) % 13) as f64 / 6.0 - 1.0
    }

    #[test]
    fn the_shared_fit_recovers_k_intercepts_and_slopes() {
        let (k, a, s) = (0.21, [21.3, 21.1, 21.25], [0.05, -0.02, 0.011]);
        let mut points = Vec::new();
        for (night, (a, s)) in a.iter().zip(&s).enumerate() {
            for i in 0..40 {
                let airmass = 1.1 + 0.9 * ((i * 11 + night * 5) % 40) as f64 / 40.0;
                let fwhm = 1.6 + 1.3 * ((i * 17 + night * 3) % 40) as f64 / 40.0;
                points.push(FitPoint {
                    side: 0.0,
                    night,
                    airmass,
                    fwhm,
                    zp: k * airmass + a + s * fwhm,
                });
            }
        }
        let fit = shared_extinction_fit(&points, 3).unwrap();
        assert!((fit.k - k).abs() < 1e-9, "{}", fit.k);
        assert_eq!(fit.n, 120);
        assert_eq!(fit.dof, 120 - 7);
        for j in 0..3 {
            let n = fit.nights[j].unwrap();
            assert!(
                (n.intercept - a[j]).abs() < 1e-9,
                "night {j}: {}",
                n.intercept
            );
            assert!((n.slope - s[j]).abs() < 1e-9, "night {j}: {}", n.slope);
            assert_eq!(n.n, 40);
            assert!(
                n.slope_se < 1e-9,
                "an exact fit has no error: {}",
                n.slope_se
            );
            assert!(n.fwhm_min >= 1.6 && n.fwhm_max < 2.9);
        }
    }

    #[test]
    fn the_shared_fit_standard_errors_follow_the_residuals() {
        // One night, airmass and FWHM independent: the slope's error is sigma / sqrt(Sxx) after the airmass and the
        // intercept are projected out; with the noise orthogonal to nothing in particular we only demand the textbook
        // sigma / sqrt(Sxx) within a factor 3 for these nearly orthogonal columns.
        let mut points = Vec::new();
        for i in 0..200 {
            let airmass = 1.0 + ((i * 37) % 200) as f64 / 200.0;
            let fwhm = 2.0 + ((i * 91) % 200) as f64 / 200.0;
            points.push(FitPoint {
                side: 0.0,
                night: 0,
                airmass,
                fwhm,
                zp: 21.0 - 0.2 * airmass + 0.02 * fwhm + 0.01 * wobble(i),
            });
        }
        let fit = shared_extinction_fit(&points, 1).unwrap();
        let n = fit.nights[0].unwrap();
        let mean = points.iter().map(|p| p.fwhm).sum::<f64>() / 200.0;
        let sxx: f64 = points.iter().map(|p| (p.fwhm - mean).powi(2)).sum();
        let sigma = 0.01 * ((0..200).map(|i| wobble(i).powi(2)).sum::<f64>() / 200.0).sqrt();
        let floor = sigma / sxx.sqrt();
        assert!(
            n.slope_se > 0.5 * floor && n.slope_se < 3.0 * floor,
            "{} vs {floor}",
            n.slope_se
        );
        assert!((n.slope - 0.02).abs() < 5.0 * n.slope_se, "{}", n.slope);
    }

    #[test]
    fn a_night_with_one_fwhm_is_dropped_and_the_others_are_kept() {
        let mut points = Vec::new();
        for i in 0..30 {
            let airmass = 1.1 + 0.02 * i as f64;
            points.push(FitPoint {
                side: 0.0,
                night: 0,
                airmass,
                fwhm: 2.0 + 0.03 * ((i * 7) % 30) as f64,
                zp: 21.0 - 0.2 * airmass,
            });
            points.push(FitPoint {
                side: 0.0,
                night: 1,
                airmass,
                fwhm: 2.5,
                zp: 21.0 - 0.2 * airmass,
            });
        }
        let fit = shared_extinction_fit(&points, 2).unwrap();
        assert!(fit.nights[0].is_some());
        assert!(fit.nights[1].is_none());
        // A night under the minimum is dropped as well.
        let few = &points[..4];
        assert_eq!(shared_extinction_fit(few, 2), None);
    }

    #[test]
    fn sidereal_time_and_precession_match_the_textbook() {
        // Meeus, Astronomical Algorithms, example 12.a: 1987 April 10, 0h UT, mean sidereal time 13h10m46.3668s.
        let gmst = gmst_deg(2_446_895.5);
        assert!(
            (gmst - (13.0 + 10.0 / 60.0 + 46.3668 / 3600.0) * 15.0).abs() < 1e-6,
            "{gmst}"
        );
        // Example 21.b: theta Persei from J2000 (proper motion already applied) to 2028 Nov 13.19.
        let (ra, dec) = precess_from_j2000(41.054063, 49.227750, 2_462_088.69);
        assert!(
            (ra - 41.547214).abs() < 1e-4 && (dec - 49.348483).abs() < 1e-4,
            "{ra} {dec}"
        );
    }

    #[test]
    fn the_airmass_is_the_secant_of_the_zenith_distance() {
        // A star at the pole stands at the site's latitude at any time: sec(90 - 35).
        let want = 1.0 / 35.0_f64.to_radians().sin();
        for hours in [0.0, 3.0, 7.7, 12.0] {
            let x = airmass(10.0, 89.999, 1.76e9 + hours * 3600.0, 35.0, 32.9).unwrap();
            assert!((x - want).abs() < 0.01, "{x} vs {want}");
        }
        // Below the horizon: none. At the zenith: one (the position follows the sidereal clock).
        let t = 1.76e9;
        let jd = julian_date(t);
        let lst = gmst_deg(jd) + 32.9;
        // Carry the zenith position of the date back to J2000 by the inverse rotation: close enough at 1e-3 airmass.
        let (ra0, dec0) = precess_from_j2000(lst, 35.0, julian_date(2.0 * 946_728_000.0 - t));
        let x = airmass(ra0, dec0, t, 35.0, 32.9).unwrap();
        assert!((x - 1.0).abs() < 0.01, "{x}");
        assert_eq!(airmass(lst + 180.0, -35.0, t, 35.0, 32.9), None);
        assert_eq!(airmass(f64::NAN, 0.0, t, 35.0, 32.9), None);
    }

    /// Two nights of 40 mosaic frames. The corrected zero point is flat in FWHM; without `c` it rises 0.1 mag/px.
    fn osc_rows() -> Vec<MeasureRow> {
        let mut rows = Vec::new();
        for night in 0..2usize {
            for i in 0..40usize {
                let t = T0 + night as i64 * 86_400 + i as i64 * 180;
                // The seeing wanders slowly, the airmass climbs: consecutive frames are alike, as in a real night.
                let airmass = 1.15 + 0.8 * ((i + night * 7) % 40) as f64 / 40.0;
                let fwhm = 2.3 + 0.5 * (i as f64 / 4.0 + night as f64).sin();
                let zp_c =
                    21.0 + 0.1 * night as f64 - 0.2 * airmass + 0.004 * wobble(i + 3 * night);
                // c = 10^(-0.4 * 0.1 * fwhm): zp_u = zp_c - 2.5 log10 c = zp_c + 0.1 * fwhm.
                let c = 10f64.powf(-0.4 * 0.1 * fwhm);
                let mut r = row(&format!("n{night}"), t, Some(zp_c + 0.1 * fwhm), fwhm, 900);
                r.object = "T".into();
                r.filter = "-".into();
                r.rig = "cam|scope|414|g100|1x1|-".into();
                r.c = Some(c);
                r.c_stars = Some(40);
                r.c_se = Some(0.002 * c);
                r.airmass = Some(airmass);
                r.zero_point_c = Some(zp_c + 0.1 * fwhm + 2.5 * c.log10());
                rows.push(r);
            }
        }
        rows
    }

    fn osc_common(rows: &[MeasureRow]) -> Vec<CommonRow> {
        rows.iter()
            .map(|r| CommonRow {
                frame_id: r.frame_id,
                file_name: r.file_name.clone(),
                group_key: "osc".into(),
                planes: 3,
                // The common flux follows the true transparency (the corrected zero point), per second.
                tflux: [
                    None,
                    Some(180.0 * 10f64.powf(0.4 * (r.zero_point_c.unwrap() - 21.0))),
                    None,
                ],
            })
            .collect()
    }

    #[test]
    fn the_osc_verdict_passes_a_corrected_night_and_shows_the_work() {
        let rows = osc_rows();
        let common = osc_common(&rows);
        let lines = osc_c_verdict_lines(&rows, Some(&common));
        let text = lines.join("\n");
        assert!(lines.len() <= VERDICT_MAX_LINES, "{text}");
        assert_eq!(lines.last().unwrap(), "ZP PROBE osc-c: PASS", "{text}");
        assert!(text.contains("2 judged nights"), "{text}");
        // zero point = k * airmass + ...: the extinction shows as a negative k.
        assert!(
            text.contains("extinction k -0.19") || text.contains("extinction k -0.20"),
            "{text}"
        );
        assert!(
            text.contains("c does the work") && !text.contains("vacuous"),
            "{text}"
        );
        let work = lines
            .iter()
            .find(|l| l.starts_with("c does the work"))
            .unwrap();
        assert!(
            work.contains("T/n0") && work.contains("T/n1") && work.contains(" pass"),
            "{work}"
        );
        // Without the join the two mechanism bars are unjudged and say so.
        let lines = osc_c_verdict_lines(&rows, None);
        assert_eq!(
            lines.last().unwrap(),
            "ZP PROBE osc-c: UNJUDGED(mechanism_slope,mechanism_rms)"
        );
    }

    #[test]
    fn the_osc_verdict_fails_a_corrected_slope_and_a_missing_c() {
        // A corrected zero point that still follows the seeing: the seeing bar fails.
        let mut rows = osc_rows();
        for r in &mut rows {
            r.zero_point_c = r.zero_point_c.map(|z| z + 0.08 * r.median_fwhm.unwrap());
        }
        let lines = osc_c_verdict_lines(&rows, None);
        assert!(
            lines.last().unwrap().contains("FAIL(") && lines.last().unwrap().contains("seeing"),
            "{lines:?}"
        );
        // Half the frames without a c: the measured bar fails, and no zero point with c exists for them.
        let mut rows = osc_rows();
        for (i, r) in rows.iter_mut().enumerate() {
            if i % 2 == 0 {
                r.c = None;
                r.c_se = None;
                r.zero_point_c = None;
            }
        }
        let lines = osc_c_verdict_lines(&rows, None);
        let last = lines.last().unwrap();
        assert!(
            last.contains("c_measured") && last.contains("coverage"),
            "{last}"
        );
    }

    #[test]
    fn an_older_tsv_parses_and_a_new_row_round_trips() {
        let old = MEASURE_HEADER
            .split('\t')
            .take(16)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let fields: Vec<String> = (0..16)
            .map(|i| {
                if i == 0 {
                    "7".to_string()
                } else {
                    String::new()
                }
            })
            .collect();
        let r = MeasureRow::from_fields(&old, &fields);
        assert_eq!(
            (r.frame_id, r.c, r.c_stars, r.airmass, r.zero_point_c),
            (Some(7), None, None, None, None)
        );
        let mut full = row("n", T0, Some(21.0), 2.0, 100);
        full.c = Some(1.0345);
        full.c_stars = Some(31);
        full.c_se = Some(0.0041);
        full.airmass = Some(1.2345);
        full.zero_point_c = Some(21.0368);
        full.rig = "a|b|414|g100|1x1|-".into();
        let header: Vec<String> = MEASURE_HEADER.split('\t').map(str::to_string).collect();
        let cells: Vec<String> = full.to_tsv().split('\t').map(str::to_string).collect();
        assert_eq!(cells.len(), header.len());
        let back = MeasureRow::from_fields(&header, &cells);
        assert_eq!(
            (back.c, back.c_stars, back.c_se, back.airmass),
            (Some(1.0345), Some(31), Some(0.0041), Some(1.2345))
        );
        assert_eq!(
            (back.zero_point_c, back.rig.as_str()),
            (Some(21.0368), "a|b|414|g100|1x1|-")
        );
    }

    // ---- the forced verdict ----

    const RIG_A: &str = "cam|scope|414|g100|1x1|-";
    const RIG_MONO: &str = "QHY268M|scope|400|g56|1x1|L";

    fn frow(night: &str, t_s: i64, zp: Option<f64>, fwhm: f64) -> ForcedRow {
        ForcedRow {
            frame_id: Some(t_s),
            file_name: format!("f{t_s}.fits"),
            object: "T".into(),
            filter: "-".into(),
            night: night.into(),
            date_obs: iso(t_s),
            exptime: Some(180.0),
            rig: RIG_A.into(),
            layout: "cfa".into(),
            median_fwhm: Some(fwhm),
            fwhm_ref: Some(2.0),
            centre_median_px: Some(0.2),
            zero_point: zp,
            ..ForcedRow::default()
        }
    }

    /// A night of the given rows as the verdict sees it (no 30-frame floor, no sides).
    fn night_of(rows: &[ForcedRow], joined: usize) -> FNight<'_> {
        FNight {
            rig: RIG_A.into(),
            object: "T".into(),
            night: "n".into(),
            filter: "-".into(),
            rows: rows.iter().collect(),
            sides: vec![None; rows.len()],
            joined,
        }
    }

    #[test]
    fn the_kept_share_counts_consecutive_pairs_only() {
        let rows: Vec<ForcedRow> = (0..3)
            .map(|i| frow("n", T0 + i * 180, Some(21.0), 2.0))
            .collect();
        let mut keys: KeptKeys = HashMap::new();
        // A holds 0..100, B 50..150, C 100..200: A and B share half, B and C share half, A and C nothing.
        for (r, lo) in rows.iter().zip([0u64, 50, 100]) {
            keys.insert(r.frame_id.unwrap(), (lo..lo + 100).collect());
        }
        let shares = kept_shares(&night_of(&rows, 0), &keys);
        assert_eq!(shares, vec![0.5, 0.5], "{shares:?}");
        // A frame without a zero point breaks the pair: its neighbours are not paired over it.
        let mut gap = rows.clone();
        gap[1].zero_point = None;
        assert!(kept_shares(&night_of(&gap, 0), &keys).is_empty());
        // Frames more than 15 minutes apart are not a pair.
        let mut far = rows.clone();
        far[1].date_obs = iso(T0 + 180 + 1200);
        assert_eq!(kept_shares(&night_of(&far, 0), &keys).len(), 1);
    }

    #[test]
    fn the_stars_tsv_keeps_members_of_non_zero_weight_the_screen_left() {
        // Per frame: a kept member, a member the screen dropped (weight 0, screened_in 0), a member whose weight is
        // tapered (kept), a window-edge member of weight 0, and a list star that is no member.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.stars.tsv");
        let text = format!(
            "{FORCED_STARS_HEADER}\n\
             7\t{:016x}\t14.000\t1.0000\t1\t1\n\
             7\t{:016x}\t14.100\t0.0000\t1\t0\n\
             7\t{:016x}\t14.200\t0.4000\t1\t1\n\
             7\t{:016x}\t14.300\t0.0000\t1\t1\n\
             7\t{:016x}\t09.000\t0.0000\t0\t0\n\
             8\t{:016x}\t14.000\t1.0000\t1\t1\n",
            11u64, 22u64, 33u64, 44u64, 55u64, 11u64
        );
        std::fs::write(&path, text).unwrap();
        let mut keys: KeptKeys = HashMap::new();
        read_kept_keys(path.to_str().unwrap(), &mut keys).unwrap();
        assert_eq!(keys[&7], vec![11, 33], "{keys:?}");
        assert_eq!(keys[&8], vec![11]);
        // A file without the new columns is an error that names it, not a silent empty B1.
        let old = dir.path().join("old.stars.tsv");
        std::fs::write(&old, "frame_id\tkey\tkept\n7\t000000000000000b\t1\n").unwrap();
        let e = read_kept_keys(old.to_str().unwrap(), &mut HashMap::new()).unwrap_err().to_string();
        assert!(e.contains("no weight column"), "{e}");
    }

    #[test]
    fn b2_uses_the_common_bar_only_on_a_joined_night() {
        // |dZP| = 0.018 every pair; today's 0.03.
        let rows: Vec<ForcedRow> = (0..40)
            .map(|i| {
                let z = 21.0 + if i % 2 == 0 { 0.0 } else { 0.018 };
                let mut r = frow("n", T0 + i * 180, Some(z), 2.0);
                r.zero_point_today = Some(21.0 + if i % 2 == 0 { 0.0 } else { 0.03 });
                r
            })
            .collect();
        let (text, o) = b2_night(&night_of(&rows, 0));
        assert_eq!(o, Outcome::Pass, "{text}");
        let (text, o) = b2_night(&night_of(&rows, 40));
        assert_eq!(o, Outcome::Fail, "{text}");
        // Elsewhere the forced statistic must also stay at or under today's: 0.018 against today's 0.01 misses.
        let mut rows = rows;
        for (i, r) in rows.iter_mut().enumerate() {
            r.zero_point_today = Some(21.0 + if i % 2 == 0 { 0.0 } else { 0.01 });
        }
        assert_eq!(b2_night(&night_of(&rows, 0)).1, Outcome::Fail);
    }

    #[test]
    fn the_side_follows_the_rotation() {
        assert_eq!(pier_side(91.9, 91.9), 0);
        assert_eq!(pier_side(91.9, -90.4), 1);
        // A +179 / -179 pair is 2 degrees apart, not 358: the same side.
        assert_eq!(pier_side(179.0, -179.0), 0);
        assert_eq!(pier_side(-179.0, 179.0), 0);
        assert_eq!(pier_side(10.0, 189.0), 1);
        assert!((rotation_gap_deg(-179.0, 179.0) - 2.0).abs() < 1e-12);
        // The night's first frame with a rotation is side 0.
        let mut rows: Vec<ForcedRow> = (0..40)
            .map(|i| frow("n", T0 + i * 180, Some(21.0), 2.0))
            .collect();
        for (i, r) in rows.iter_mut().enumerate() {
            r.rotation_deg = Some(if i < 20 { 91.9 } else { -90.4 });
        }
        let nights = forced_nights(&rows, None);
        assert_eq!(nights.len(), 1);
        assert!(nights[0].two_sided());
        assert_eq!(nights[0].sides[0], Some(0));
        assert_eq!(nights[0].sides[39], Some(1));
    }

    /// 40 frames of a night with a common set: ZP and the stack's level follow `k * X + a (+ step after the flip)`.
    fn b4_rows(
        x_of: impl Fn(usize) -> f64,
        k: (f64, f64),
        step: (f64, f64),
        flip_at: Option<usize>,
    ) -> (Vec<ForcedRow>, Vec<CommonRow>) {
        let mut rows = Vec::new();
        let mut common = Vec::new();
        for i in 0..40usize {
            let side = flip_at.is_some_and(|f| i >= f);
            let x = x_of(i);
            let zp = 21.0 + k.0 * x + if side { step.0 } else { 0.0 };
            let level = 5.0 + k.1 * x + if side { step.1 } else { 0.0 };
            let mut r = frow(
                "n",
                T0 + i as i64 * 180,
                Some(zp),
                2.0 + 0.2 * (i as f64 / 3.0).sin(),
            );
            r.airmass = Some(x);
            r.rotation_deg = Some(if side { -90.0 } else { 90.0 });
            common.push(CommonRow {
                frame_id: r.frame_id,
                file_name: r.file_name.clone(),
                group_key: "g".into(),
                planes: 1,
                tflux: [Some(180.0 * 10f64.powf(0.4 * level)), None, None],
            });
            rows.push(r);
        }
        (rows, common)
    }

    #[test]
    fn the_flip_term_is_fitted_and_not_judged() {
        // Recovered to 1e-9 by the joint fit with a side column.
        let pts: Vec<(f64, f64, f64)> = (0..40)
            .map(|i| {
                let x = 1.1 + 0.03 * i as f64;
                let side = f64::from(i >= 17);
                (x, 21.0 - 0.17 * x + 0.15 * side, side)
            })
            .collect();
        let (k, a, s) = side_fit(&pts).unwrap();
        assert!(
            (k + 0.17).abs() < 1e-9 && (a - 21.0).abs() < 1e-9 && (s - 0.15).abs() < 1e-9,
            "{k} {a} {s}"
        );
        // The shared fit carries the step too.
        let fit_points: Vec<FitPoint> = pts
            .iter()
            .enumerate()
            .map(|(i, p)| FitPoint {
                night: 0,
                side: p.2,
                airmass: p.0,
                fwhm: 2.0 + 0.3 * ((i * 7) % 11) as f64 / 11.0,
                zp: p.1,
            })
            .collect();
        let fit = shared_extinction_fit_sided(&fit_points, 1, &[true]).unwrap();
        let n = fit.nights[0].unwrap();
        assert!((fit.k + 0.17).abs() < 1e-9, "{}", fit.k);
        assert!(
            (n.side_step.unwrap() - 0.15).abs() < 1e-9 && n.slope.abs() < 1e-9,
            "{n:?}"
        );
        // The two series step by very different amounts and still pass: the step is printed, never judged.
        let (rows, common) = b4_rows(
            |i| 1.1 + 0.03 * i as f64,
            (-0.17, -0.17),
            (0.15, 0.9),
            Some(20),
        );
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        assert!(night.two_sided());
        let (text, o) = b4_entry(&night, &idx);
        assert_eq!(o, Outcome::Pass, "{text}");
        assert!(
            text.contains("flip step zp 0.150 stack 0.900 (reported, not judged)"),
            "{text}"
        );
        // A different k does miss.
        let (rows, common) = b4_rows(
            |i| 1.1 + 0.03 * i as f64,
            (-0.17, -0.12),
            (0.0, 0.0),
            Some(20),
        );
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        assert_eq!(b4_entry(&night, &idx).1, Outcome::Fail);
    }

    #[test]
    fn b4_is_unjudged_below_four_tenths_of_an_airmass() {
        // One side, 40 frames: the airmass range is 0.39 or 0.41.
        for (span, want) in [(0.39, Outcome::Unjudged), (0.41, Outcome::Pass)] {
            let (rows, common) = b4_rows(
                |i| 1.2 + span * i as f64 / 39.0,
                (-0.17, -0.17),
                (0.0, 0.0),
                None,
            );
            let idx = CommonIndex::new(&common);
            let night = forced_nights(&rows, Some(&idx)).remove(0);
            let (text, o) = b4_entry(&night, &idx);
            assert_eq!(o, want, "{span}: {text}");
        }
        // A side with a range under 0.4 is unjudged while the other side is judged: 20 frames each, one side wide.
        let (rows, common) = b4_rows(
            |i| {
                if i < 20 {
                    1.2 + 0.03 * i as f64
                } else {
                    1.2 + 0.005 * i as f64
                }
            },
            (-0.17, -0.17),
            (0.0, 0.0),
            Some(20),
        );
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let (text, o) = b4_entry(&night, &idx);
        assert_eq!(o, Outcome::Pass, "{text}");
        assert!(text.contains("side 1 unjudged"), "{text}");
    }

    #[test]
    fn b3_rms_is_about_the_median() {
        // A single outlier moves the mean, not the median: rms about the median of [1,1,1,1,5] is sqrt(16/5).
        let v = [1.0, 1.0, 1.0, 1.0, 5.0];
        assert!((rms_about_median(&v).unwrap() - (16.0f64 / 5.0).sqrt()).abs() < 1e-12);
        // And through the night: a constant offset (a different constant per run) is no scatter.
        let (rows, common) = b4_rows(|i| 1.2 + 0.01 * i as f64, (-0.17, -0.17), (0.0, 0.0), None);
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let b = b3_night(&night, &idx).unwrap();
        assert!(
            b.rms < 1e-9 && b.slope.abs() < 1e-9,
            "{} {}",
            b.rms,
            b.slope
        );
        assert_eq!(b3_entry(&night, &idx).1, Outcome::Pass);
        // A stack slope of 0.06 is a miss with or without a 6r to say anything (the next test reads the notes).
        let mut rows = rows;
        for r in rows.iter_mut() {
            let f = r.median_fwhm.unwrap();
            r.zero_point = r.zero_point.map(|z| z + 0.06 * f);
        }
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        assert_eq!(b3_entry(&night, &idx).1, Outcome::Fail, "no 6r is not a pass");
    }

    /// Frames of `b4_rows` whose zero point carries a seeing term `s` against the stack, with the 6r referee `z6(zp, f)`.
    fn b3_rows(s: f64, z6: impl Fn(f64, f64) -> Option<f64>) -> (Vec<ForcedRow>, Vec<CommonRow>) {
        let (mut rows, common) = b4_rows(|i| 1.2 + 0.01 * i as f64, (-0.17, -0.17), (0.0, 0.0), None);
        for r in rows.iter_mut() {
            let f = r.median_fwhm.unwrap();
            r.zero_point = r.zero_point.map(|z| z + s * f);
            r.zero_point_6r = z6(r.zero_point.unwrap(), f);
        }
        (rows, common)
    }

    #[test]
    fn b3_a_stack_slope_miss_six_r_does_not_echo_is_no_pass() {
        // Within the bar: passes with the three slopes printed.
        let (rows, common) = b3_rows(0.03, |z, _| Some(z));
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let (text, o) = b3_entry(&night, &idx);
        assert_eq!(o, Outcome::Pass, "{text}");
        assert!(text.contains("slopes ZP-stack +0.0300") && text.contains("ZP-6r ") && text.contains("6r-stack "), "{text}");
        let b = b3_night(&night, &idx).unwrap();
        assert!((b.slope - 0.03).abs() < 1e-9 && b.slope_zp_6r.unwrap().0.abs() < 1e-9, "{:?}", b.slope_zp_6r);
        assert!((b.slope_6r_stack.unwrap().0 - 0.03).abs() < 1e-9);
        // Beyond +-0.04 against the stack, 6r agreeing with the stack against ZP: a miss, echoed.
        let (rows, common) = b3_rows(0.06, |z, f| Some(z - 0.06 * f));
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let (text, o) = b3_entry(&night, &idx);
        assert_eq!(o, Outcome::Fail, "{text}");
        assert!(text.contains("echoed by 6r: a miss"), "{text}");
        let b = b3_night(&night, &idx).unwrap();
        assert!((b.slope_zp_6r.unwrap().0 - 0.06).abs() < 1e-9 && b.slope_6r_stack.unwrap().0.abs() < 1e-9);
        // Beyond +-0.04 with 6r equal to ZP (the referee shares everything but the outer light): still no pass, and the
        // line says it is the owner's, with the three slopes.
        let (rows, common) = b3_rows(0.06, |z, _| Some(z));
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let (text, o) = b3_entry(&night, &idx);
        assert_eq!(o, Outcome::Fail, "{text}");
        assert!(text.contains("not echoed by 6r, no pass: the owner's as a stated limit"), "{text}");
        assert!(text.contains("slopes ZP-stack +0.0600") && text.contains("ZP-6r ") && text.contains("6r-stack +0.0600"), "{text}");
        let b = b3_night(&night, &idx).unwrap();
        assert!(b.slope_zp_6r.unwrap().0.abs() < 1e-9 && (b.slope_6r_stack.unwrap().0 - 0.06).abs() < 1e-9);
        // No 6r at all: no pass either.
        let (rows, common) = b3_rows(0.06, |_, _| None);
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let (text, o) = b3_entry(&night, &idx);
        assert_eq!(o, Outcome::Fail, "{text}");
        assert!(text.contains("no 6r to say"), "{text}");
        // The scatter bar is its own: a clean slope with a noisy night misses on the rms.
        let (mut rows, common) = b3_rows(0.0, |z, _| Some(z));
        for (i, r) in rows.iter_mut().enumerate() {
            r.zero_point = r.zero_point.map(|z| z + if i % 2 == 0 { 0.03 } else { -0.03 });
        }
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        assert_eq!(b3_entry(&night, &idx).1, Outcome::Fail);
    }

    #[test]
    fn b4_prints_the_standard_error_of_dk_and_judges_it_unchanged() {
        // k -0.17 against the stack's -0.12, noise-free: dk -0.05 exactly, a miss with no allowance, its error printed.
        let (rows, common) = b4_rows(|i| 1.1 + 0.03 * i as f64, (-0.17, -0.12), (0.0, 0.0), None);
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let (text, o) = b4_entry(&night, &idx);
        assert_eq!(o, Outcome::Fail, "{text}");
        assert!(text.contains("dk -0.0500 +- 0.0000"), "{text}");
        // A noisy ZP: the error is the regression's and is not zero.
        let (mut rows, common) = b4_rows(|i| 1.1 + 0.03 * i as f64, (-0.17, -0.17), (0.0, 0.0), None);
        for (i, r) in rows.iter_mut().enumerate() {
            r.zero_point = r.zero_point.map(|z| z + 0.01 * wobble(i));
        }
        let idx = CommonIndex::new(&common);
        let night = forced_nights(&rows, Some(&idx)).remove(0);
        let (text, _) = b4_entry(&night, &idx);
        assert!(text.contains("+- 0.00") && !text.contains("+- 0.0000,"), "{text}");
    }

    #[test]
    fn an_identity_mismatch_names_its_field() {
        // A frame of eight Gaussian stars on a noisy sky, measured the way the analyzer does.
        let (w, h) = (320usize, 320usize);
        let mut plane = vec![1000.0f32; w * h];
        let mut state = 12345u64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64) / (1u64 << 31) as f64
        };
        for k in 0..8 {
            let (cx, cy) = (40.0 + 35.0 * k as f64, 60.0 + 29.0 * ((k * 5) % 8) as f64);
            for y in 0..h {
                for x in 0..w {
                    let r2 = (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2);
                    plane[y * w + x] += (4000.0 * (-r2 / (2.0 * 1.6f64.powi(2))).exp()) as f32;
                }
            }
        }
        for v in plane.iter_mut() {
            *v += (next() * 8.0 - 4.0) as f32;
        }
        let frame = athenaeum_core::star_measure::raw::RawFrame {
            planes: RawPlanes::Mono(plane),
            w,
            h,
            domain: athenaeum_core::star_measure::Domain::RawAdu {
                clip: 65535.0,
                full_scale: 65535.0,
                gain_e_per_adu: None,
            },
            flip_vertical: false,
            channels: 1,
        };
        let config = AnalysisConfig::default();
        let opts = frame_options(&config);
        let a = measure_raw_frame(&frame, &config, None);
        assert!(!a.stars.is_empty(), "the synthetic frame has stars");
        let b = measure_raw_frame(&frame, &config, None);
        assert_eq!(identity_mismatch(&a, &b, &opts), None);
        let mut moved = b.clone();
        moved.stars[0].x += 1e-9;
        assert_eq!(
            identity_mismatch(&a, &moved, &opts).as_deref(),
            Some("star_x")
        );
        let mut ring = b.clone();
        ring.stars[0].sky_ring_n += 1;
        assert_eq!(
            identity_mismatch(&a, &ring, &opts).as_deref(),
            Some("star_sky_ring_n")
        );
        let mut fewer = b.clone();
        fewer.stars.pop();
        assert_eq!(
            identity_mismatch(&a, &fewer, &opts).as_deref(),
            Some("stars_len")
        );
        let mut whole = b;
        whole.seeds += 1;
        assert_eq!(
            identity_mismatch(&a, &whole, &opts).as_deref(),
            Some("measurement")
        );
    }

    /// A clean dataset: a common-set night, an ordinary night and a mono QHY268M night, every bar fed.
    fn go_dataset() -> (Vec<ForcedRow>, Vec<CommonRow>, KeptKeys) {
        let mut rows = Vec::new();
        let mut common = Vec::new();
        let mut keys: KeptKeys = HashMap::new();
        for (n, (object, rig, layout)) in [
            ("T0", RIG_A, "cfa"),
            ("T1", RIG_A, "cfa"),
            ("T2", RIG_MONO, "mono"),
        ]
        .into_iter()
        .enumerate()
        {
            for i in 0..40usize {
                let t = T0 + n as i64 * 86_400 + i as i64 * 180;
                let x = 1.1 + 0.6 * i as f64 / 39.0;
                let fwhm = 2.3 + 0.5 * (i as f64 / 4.0).sin();
                let zp = 21.0 - 0.2 * x + 0.003 * wobble(i + 5 * n);
                let mut r = frow(&format!("n{n}"), t, Some(zp), fwhm);
                r.frame_id = Some(1000 * n as i64 + i as i64);
                r.file_name = format!("f{n}_{i}.fits");
                r.object = object.into();
                r.rig = rig.into();
                r.layout = layout.into();
                r.airmass = Some(x);
                r.zero_point_today = Some(21.0 - 0.2 * x + 0.008 * wobble(3 * i + 1));
                r.list_stars = Some(900);
                r.list_before_cap = Some(1400);
                r.map_stars = Some(120);
                r.map_members = Some(110);
                r.map_kept_share = Some(0.93);
                r.members = Some(700);
                r.tapered = Some(6);
                r.pedestal = Some(0.4);
                r.list_fit_ms = Some(20.0);
                r.list_refit_ms = Some(18.0);
                r.list_ap_ms = Some(9.0);
                r.call_ms = Some(1960.0);
                if i % 10 == 0 {
                    r.identity = "ok".into();
                    r.plain_ms = Some(1900.0);
                }
                keys.insert(
                    r.frame_id.unwrap(),
                    (2 * i as u64..2 * i as u64 + 100).collect(),
                );
                if n == 0 {
                    common.push(CommonRow {
                        frame_id: r.frame_id,
                        file_name: r.file_name.clone(),
                        group_key: "g".into(),
                        planes: 3,
                        tflux: [None, Some(180.0 * 10f64.powf(0.4 * (zp - 21.0))), None],
                    });
                }
                rows.push(r);
            }
        }
        (rows, common, keys)
    }

    #[test]
    fn the_forced_verdict_ends_in_one_class_line_within_thirty_lines() {
        let (rows, common, keys) = go_dataset();
        let lines = forced_verdict_lines(&rows, Some(&common), Some(&keys));
        let text = lines.join("\n");
        assert!(lines.len() <= VERDICT_MAX_LINES, "{text}");
        assert_eq!(lines.last().unwrap(), "ZP PROBE forced: GO", "{text}");
        for bar in ["B1", "B2", "B3", "B4", "B5", "B6", "B7", "B8", "B9"] {
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with(bar) && l.contains(" pass:")),
                "{bar} must pass on a night:\n{text}"
            );
        }
        // The class line is the only GO / STOP line.
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("ZP PROBE forced: GO")
                    || l.starts_with("ZP PROBE forced: STOP("))
                .count(),
            1
        );

        // A bar judged on no night is a STOP: no common set gives no B3 / B4 night, no keys no B1 night.
        let lines = forced_verdict_lines(&rows, None, None);
        let last = lines.last().unwrap();
        assert!(last.starts_with("ZP PROBE forced: STOP("), "{last}");
        assert!(last.contains("judged on no night B1,B3,B4"), "{last}");
        // No frame compared is a STOP too.
        let mut none = rows.clone();
        for r in none.iter_mut() {
            r.identity.clear();
        }
        let lines = forced_verdict_lines(&none, Some(&common), Some(&keys));
        assert!(
            lines.last().unwrap().contains("judged on no night B9"),
            "{lines:?}"
        );
        // A miss names its bar: a night whose frames share 70 % of their keys with the successor.
        let mut thin = keys.clone();
        for (id, k) in thin.iter_mut() {
            if *id < 1000 {
                *k = (30 * (*id as u64)..30 * (*id as u64) + 100).collect();
            }
        }
        let lines = forced_verdict_lines(&rows, Some(&common), Some(&thin));
        assert!(lines.last().unwrap().contains("missed B1"), "{lines:?}");
        // Many nights never push the verdict past thirty lines.
        let mut many = Vec::new();
        for k in 0..12 {
            for r in &rows {
                let mut c = r.clone();
                c.object = format!("S{k:02}");
                many.push(c);
            }
        }
        let lines = forced_verdict_lines(&many, None, None);
        assert!(lines.len() <= VERDICT_MAX_LINES);
        assert!(lines.last().unwrap().starts_with("ZP PROBE forced: STOP("));
    }

    #[test]
    fn an_older_tsv_parses_and_a_forced_row_round_trips() {
        let header: Vec<String> = FORCED_HEADER.split('\t').map(str::to_string).collect();
        // The first fifteen columns only, as an older file would end: every new column parses empty.
        let fields: Vec<String> = (0..15)
            .map(|i| {
                if i == 0 {
                    "7".to_string()
                } else {
                    String::new()
                }
            })
            .collect();
        let old = ForcedRow::from_fields(&header[..15], &fields);
        assert_eq!(
            (old.frame_id, old.zero_point, old.identity.as_str(), old.tapered, old.zero_point_6r, old.window_slope),
            (Some(7), None, "", None, None, None)
        );
        // A row of the r0 / f0 header (it has `c`, `accepted`, `zero_point_6f`, no map columns) still reads its common ones.
        let f0 = "frame_id\tzero_point\tc\taccepted\tzero_point_6f\tidentity\n9\t21.5\t1.08\t400\t21.4\tok";
        let (h, d) = f0.split_once('\n').unwrap();
        let (h, d): (Vec<String>, Vec<String>) = (
            h.split('\t').map(str::to_string).collect(),
            d.split('\t').map(str::to_string).collect(),
        );
        let r0 = ForcedRow::from_fields(&h, &d);
        assert_eq!((r0.frame_id, r0.zero_point, r0.identity.as_str(), r0.map_members), (Some(9), Some(21.5), "ok", None));
        let mut full = frow("n", T0, Some(21.0123), 2.4);
        full.pier = Some(1);
        full.rotation_deg = Some(-90.4);
        full.airmass = Some(1.2345);
        full.anchor = Some(21.2);
        full.anchor_stars = Some(31);
        full.window_bright = Some(13.4);
        full.window_faint = Some(16.4);
        full.list_stars = Some(998);
        full.detected_neighbours_removed = Some(37);
        full.seed_calibration_pairs = Some(640);
        full.map_stars = Some(400);
        full.map_members = Some(371);
        full.map_offset_px = Some(1.2345);
        full.map_residual_px = Some(0.0612);
        full.map_kept_share = Some(0.9125);
        full.aperture_radius_px = Some(8.0);
        full.members = Some(812);
        full.tapered = Some(9);
        full.zp_stars = Some(790);
        full.pedestal = Some(0.31234);
        full.pedestal_kept_share = Some(0.8667);
        full.pedestal_zp_mag = Some(-0.00412);
        full.annulus_off = Some(3);
        full.window_slope = Some(0.0345);
        full.slope_remeasured = Some(0.0301);
        full.zero_point_6r = Some(21.0111);
        full.zero_point_today = Some(21.0368);
        full.identity = "mismatch:star_x".into();
        full.call_ms = Some(2310.0);
        full.list_fit_ms = Some(21.0);
        full.list_refit_ms = Some(19.0);
        full.list_ap_ms = Some(11.0);
        full.refusal = "too_scattered".into();
        let cells: Vec<String> = full.to_tsv().split('\t').map(str::to_string).collect();
        assert_eq!(cells.len(), header.len(), "one cell per column");
        let back = ForcedRow::from_fields(&header, &cells);
        assert_eq!(
            (back.pier, back.rotation_deg, back.airmass, back.anchor_stars, back.list_stars),
            (Some(1), Some(-90.4), Some(1.2345), Some(31), Some(998))
        );
        assert_eq!((back.detected_neighbours_removed, back.seed_calibration_pairs), (Some(37), Some(640)));
        assert_eq!(
            (back.map_stars, back.map_members, back.map_offset_px, back.map_residual_px, back.map_kept_share),
            (Some(400), Some(371), Some(1.2345), Some(0.0612), Some(0.9125))
        );
        assert_eq!(
            (back.aperture_radius_px, back.members, back.tapered, back.zp_stars, back.annulus_off),
            (Some(8.0), Some(812), Some(9), Some(790), Some(3))
        );
        assert_eq!(
            (back.pedestal, back.pedestal_zp_mag, back.window_slope, back.slope_remeasured, back.zero_point_6r),
            (Some(0.31234), Some(-0.00412), Some(0.0345), Some(0.0301), Some(21.0111))
        );
        assert_eq!(back.pedestal_kept_share, Some(0.8667));
        assert_eq!(
            (back.zero_point_today, back.call_ms, back.list_fit_ms, back.list_refit_ms, back.list_ap_ms),
            (Some(21.0368), Some(2310.0), Some(21.0), Some(19.0), Some(11.0))
        );
        assert_eq!(
            (back.identity.as_str(), back.refusal.as_str(), back.zero_point),
            ("mismatch:star_x", "too_scattered", Some(21.0123))
        );
        assert_eq!(stars_path("a/b/x.tsv"), "a/b/x.stars.tsv");
        assert_eq!(side_path("a/b/x.tsv", "shifts"), "a/b/x.shifts.tsv");
    }

    /// 40 frames of one night of one set, every frame 180 s unless `exptime` says else, the zero point from `zp(i)`.
    fn set_rows(object: &str, exptime: f64, zp: impl Fn(usize) -> f64, bright: impl Fn(usize) -> f64) -> Vec<ForcedRow> {
        (0..40usize)
            .map(|i| {
                let t = T0 + i as i64 * 180 + (exptime as i64) * 100_000;
                let mut r = frow("n", t, Some(zp(i)), 2.3 + 0.4 * (i as f64 / 5.0).sin());
                r.object = object.into();
                r.exptime = Some(exptime);
                r.window_bright = Some(bright(i));
                r.airmass = Some(1.2 + 0.01 * i as f64);
                r.zero_point_today = Some(21.0 + if i % 2 == 0 { 0.0 } else { 0.005 });
                r
            })
            .collect()
    }

    #[test]
    fn the_verdict_reads_the_reduced_zero_point() {
        // The window alternates 1 mag; the raw zero point follows it at 0.2 mag per mag: 0.2 mag apart frame to frame.
        let with_slope = |rows: &mut Vec<ForcedRow>| {
            for r in rows.iter_mut() {
                r.window_slope = Some(0.2);
            }
        };
        let bright = |i: usize| 13.0 + f64::from(i as u8 % 2);
        let mut rows = set_rows("T", 180.0, |i| 21.0 + 0.2 * (bright(i) - 13.5), bright);
        // Unreduced (no slope on any frame: s = 0) the night misses B2 by far ...
        let lines = forced_verdict_lines(&rows, None, None);
        let b2 = lines.iter().find(|l| l.starts_with("B2")).unwrap();
        assert!(b2.contains("FAIL"), "{b2}");
        // ... and read in the common window it is flat and passes, today's alternation 0.005 not exceeded.
        with_slope(&mut rows);
        let lines = forced_verdict_lines(&rows, None, None);
        let b2 = lines.iter().find(|l| l.starts_with("B2")).unwrap();
        assert!(b2.contains(" pass:") && b2.contains("0.0000 mag"), "{b2}");
        assert!(lines[0].contains("reduced in 1 (rig, object, exposure) groups"), "{}", lines[0]);
        // The reduction itself: every frame reads the zero point it would have at the median window.
        let (reduced, windows) = reduce_rows(&rows);
        let w = windows.values().next().unwrap();
        assert!((w.pivot - 13.5).abs() < 1e-12 && (w.slope - 0.2).abs() < 1e-12);
        assert!(reduced.iter().all(|r| (r.zero_point.unwrap() - 21.0).abs() < 1e-9));
        // Today's zero point is read as it is, and a frame without a zero point stays without.
        assert_eq!(reduced[3].zero_point_today, rows[3].zero_point_today);
        let mut gap = rows.clone();
        gap[5].zero_point = None;
        assert_eq!(reduce_rows(&gap).0[5].zero_point, None);
        // The reduction is core's: the probe's group window and `reduce_to_common_window` agree.
        let mut core: Vec<(f64, f64, Option<f64>)> =
            rows.iter().map(|r| (r.zero_point.unwrap(), r.window_bright.unwrap(), r.window_slope)).collect();
        athenaeum_core::analysis::zero_point::reduce_to_common_window(&mut core);
        assert!(core.iter().zip(&reduced).all(|(c, r)| (c.0 - r.zero_point.unwrap()).abs() < 1e-12));
    }

    #[test]
    fn the_reduction_groups_by_exposure() {
        // 20 frames at 300 s with the window at 13.0 and 20 at 60 s with it 1.75 mag fainter (a 5x exposure step moves
        // the window by 2.5 log10(5)), the same 0.2 mag per mag slope: each exposure is read at its own window, so the
        // 1.75 mag step is not put into one pool's slope term.
        let mut rows = set_rows("T", 300.0, |i| 21.0 + 0.2 * 0.01 * i as f64, |i| 13.0 + 0.01 * i as f64);
        rows.truncate(20);
        let mut fast = set_rows("T", 60.0, |i| 21.35 + 0.2 * 0.01 * i as f64, |i| 14.75 + 0.01 * i as f64);
        fast.truncate(20);
        rows.extend(fast);
        for r in rows.iter_mut() {
            r.window_slope = Some(0.2);
        }
        let (reduced, windows) = reduce_rows(&rows);
        assert_eq!(windows.len(), 2, "two exposures, two references: {windows:?}");
        let pivots: Vec<f64> = windows.values().map(|w| w.pivot).collect();
        assert!(pivots.iter().any(|p| (p - 13.095).abs() < 1e-9) && pivots.iter().any(|p| (p - 14.845).abs() < 1e-9), "{pivots:?}");
        // Within each exposure the reduced zero point is constant, and the two references stay 0.35 mag apart: the
        // 1.75 mag step is the cost of keeping each exposure in its own window (the linear reduction is measured to
        // +-0.5 mag only), which Phase A's anchor shifts of 0.44, 1.0 and 1.75 mag measure.
        let level = |exposure: f64| {
            let z: Vec<f64> = reduced.iter().filter(|r| r.exptime == Some(exposure)).map(|r| r.zero_point.unwrap()).collect();
            assert!(z.iter().all(|v| (v - z[0]).abs() < 1e-9), "{exposure}: {z:?}");
            z[0]
        };
        assert!((level(60.0) - level(300.0) - 0.35).abs() < 1e-9);
        // Were both exposures one pool, the slope term would carry the whole step and fold the two together.
        let mut one_pool = rows.clone();
        for r in one_pool.iter_mut() {
            r.exptime = Some(300.0);
        }
        let (flat, one) = reduce_rows(&one_pool);
        assert_eq!(one.len(), 1);
        let z: Vec<f64> = flat.iter().map(|r| r.zero_point.unwrap()).collect();
        assert!(z.iter().all(|v| (v - z[0]).abs() < 1e-9), "{z:?}");
    }

    #[test]
    fn b5_fits_one_night_at_a_time() {
        // Two nights of one set: k -0.07 and -0.11, no seeing term on either; on night a the FWHM tracks the airmass.
        let mut rows = Vec::new();
        for (night, k, track) in [("a", -0.07, 1.0), ("b", -0.11, 0.0)] {
            for i in 0..40usize {
                let x = 1.1 + 0.02 * i as f64;
                let fwhm = 2.0 + track * (x - 1.1) * 1.5 + 0.25 * (i as f64 * 0.9).sin() + 0.1 * (i as f64 * 0.37).cos();
                let t = T0 + i as i64 * 180 + if night == "b" { 86_400 } else { 0 };
                let mut r = frow(night, t, Some(21.0 + k * x), fwhm);
                r.airmass = Some(x);
                rows.push(r);
            }
        }
        let nights = forced_nights(&rows, None);
        assert_eq!(nights.len(), 2);
        let alone: Vec<_> = nights.iter().map(|n| rig_fit(&[n], |r| r.zero_point, |_| true).0.unwrap()).collect();
        for (fit, k) in alone.iter().zip([-0.07, -0.11]) {
            assert!((fit.k - k).abs() < 1e-9, "{}", fit.k);
            assert!(fit.nights[0].unwrap().slope.abs() < 1e-9, "{:?}", fit.nights[0]);
        }
        // One k for both nights would have handed a night a slope it does not have.
        let joint = rig_fit(&nights.iter().collect::<Vec<_>>(), |r| r.zero_point, |_| true).0.unwrap();
        let worst = joint.nights.iter().flatten().map(|n| n.slope.abs()).fold(0.0, f64::max);
        assert!(worst > 0.003, "the rig-wide fit is not the night's own: {worst}");
        // The verdict judges each night on its own: both pass, with their own k printed.
        let lines = forced_verdict_lines(&rows, None, None);
        let b5 = lines.iter().find(|l| l.starts_with("B5")).unwrap();
        assert!(b5.contains(" pass:") && b5.contains("k -0.070") && b5.contains("k -0.110"), "{b5}");
        assert!(!b5.contains("FAIL"), "{b5}");
        // B8 reads the night's own fit too (mono nights; today's has the same terms and the same nights).
        let mut mono = rows.clone();
        for r in mono.iter_mut() {
            r.layout = "mono".into();
            r.zero_point_today = r.zero_point.map(|z| z + 0.001);
        }
        let lines = forced_verdict_lines(&mono, None, None);
        let b8 = lines.iter().find(|l| l.starts_with("B8")).unwrap();
        assert!(b8.contains(" pass:"), "{b8}");
    }

    #[test]
    fn the_per_night_line_reports_the_pedestal_clip_and_its_correlation_with_the_zero_point() {
        // The zero point falls as the clip keeps less: r = +1 over the night, min and median of the share printed.
        let mut rows = set_rows("T", 180.0, |i| 21.0 - 0.001 * i as f64, |_| 13.0);
        for (i, r) in rows.iter_mut().enumerate() {
            r.pedestal = Some(0.4);
            r.pedestal_kept_share = Some(1.0 - 0.005 * i as f64);
        }
        let lines = forced_verdict_lines(&rows, None, None);
        let line = lines.iter().find(|l| l.starts_with("reported, per night (median")).unwrap();
        assert!(line.contains("clip kept min 0.805 med 0.903, r with the reduced ZP 1.00"), "{line}");
        // No share on any frame: n/a, not a crash.
        for r in rows.iter_mut() {
            r.pedestal_kept_share = None;
        }
        let lines = forced_verdict_lines(&rows, None, None);
        let line = lines.iter().find(|l| l.starts_with("reported, per night (median")).unwrap();
        assert!(line.contains("clip kept min n/a med n/a, r with the reduced ZP n/a"), "{line}");
        assert_eq!(correlation(&[(1.0, 2.0), (2.0, 4.0)]), None, "two pairs are no correlation");
        assert_eq!(correlation(&[(1.0, 2.0), (1.0, 4.0), (1.0, 5.0)]), None, "no spread in x");
    }

    #[test]
    fn a_flag_that_breaks_the_annulus_rule_stops_forced_naming_it() {
        let d = ForcedListParams::default();
        assert!(check_annulus_rule(&d).is_ok());
        let bad = |argv: &[&str]| {
            let (fp, _, _) = parsed(argv).unwrap();
            check_annulus_rule(&fp).unwrap_err().to_string()
        };
        let e = bad(&["--aperture_radius_fwhm", "5"]);
        assert!(e.contains("--aperture_radius_fwhm 5") && e.contains("--annulus_start_fwhm 4"), "{e}");
        let e = bad(&["--annulus_gap_px", "0.5"]);
        assert!(e.contains("--annulus_gap_px 0.5"), "{e}");
        let e = bad(&["--pedestal_radii_ref", "3,7,6"]);
        assert!(e.contains("--pedestal_radii_ref"), "{e}");
        let e = bad(&["--aperture_radius_ref", "7"]);
        assert!(e.contains("--aperture_radius_ref 7"), "{e}");
        // Two at once are both named; a flag that stays inside the rule is not refused.
        let e = bad(&["--annulus_gap_px", "0.1", "--aperture_radius_fwhm", "9"]);
        assert!(e.contains("--annulus_gap_px") && e.contains("--aperture_radius_fwhm"), "{e}");
        let (fp, _, _) = parsed(&["--aperture_radius_fwhm", "3", "--annulus_gap_px", "0.75", "--decimate", "10"]).unwrap();
        assert!(check_annulus_rule(&fp).is_ok());
    }

    #[test]
    fn b7_reads_the_map_refits() {
        let mut rows = set_rows("T", 180.0, |_| 21.0, |_| 13.0);
        for r in rows.iter_mut() {
            r.centre_median_px = Some(0.2);
            r.fwhm_ref = Some(2.0);
        }
        let lines = forced_verdict_lines(&rows, None, None);
        let b7 = lines.iter().find(|l| l.starts_with("B7")).unwrap();
        assert!(b7.contains(" pass:") && b7.contains("map stars") && b7.contains("0.200 px"), "{b7}");
        // 0.6 px against a quarter of FWHM_ref 2: a miss; frames without the figure leave the night unjudged.
        for r in rows.iter_mut() {
            r.centre_median_px = Some(0.6);
        }
        let lines = forced_verdict_lines(&rows, None, None);
        assert!(lines.iter().find(|l| l.starts_with("B7")).unwrap().contains("FAIL"));
        for r in rows.iter_mut() {
            r.centre_median_px = None;
        }
        let lines = forced_verdict_lines(&rows, None, None);
        assert!(lines.iter().find(|l| l.starts_with("B7")).unwrap().contains("unjudged"));
    }

    #[test]
    fn the_six_ref_referee_uses_the_pedestal() {
        // 120 stars of a zero point 21.0 on a sky pedestal: the sum at a radius is the flux plus eps pi r^2.
        let (t, eps, zp0) = (180.0, 30.0, 21.0);
        let (r_disc, r6) = (8.0, 12.0);
        let star = |i: usize, r: f64| {
            let g = 13.0 + 3.0 * i as f64 / 119.0;
            SumStar {
                g,
                weight: 1.0,
                sum: t * 10f64.powf(0.4 * (zp0 - g)) + eps * std::f64::consts::PI * r * r,
            }
        };
        let six: Vec<SumStar> = (0..120).map(|i| star(i, r6)).collect();
        let z = ratio_of_sums(&six, eps, r6, t).unwrap();
        assert!((z - zp0).abs() < 1e-9, "{z}");
        // Without the pedestal taken out the referee reads too bright: the 6r annulus sky bias the plan measured.
        let bare = ratio_of_sums(&six, 0.0, r6, t).unwrap();
        assert!(bare - zp0 > 0.01, "{bare}");
        // The disc's ratio and its bare twin differ by the pedestal's part (negative: the pedestal lowers the zero point).
        let disc: Vec<SumStar> = (0..120).map(|i| star(i, r_disc)).collect();
        let own = ratio_of_sums(&disc, eps, r_disc, t).unwrap();
        let part = own - ratio_of_sums(&disc, 0.0, r_disc, t).unwrap();
        assert!((own - zp0).abs() < 1e-9 && part < -0.001, "{own} {part}");
        // The same members and weights: a star the screen dropped (weight 0) or a sum that is not finite takes no part.
        let mut six = six;
        six[3].weight = 0.0;
        six[4].sum = f64::NAN;
        six[5].weight = 0.5;
        let z = ratio_of_sums(&six, eps, r6, t).unwrap();
        assert!(z.is_finite() && (z - zp0).abs() < 1e-9, "{z}");
        // Nothing to sum, or a sum that is not positive: no zero point.
        assert_eq!(ratio_of_sums(&[], eps, r6, t), None);
        let dark = [SumStar { g: 14.0, weight: 1.0, sum: -5.0 }];
        assert_eq!(ratio_of_sums(&dark, 0.0, r6, t), None);
    }

    #[test]
    fn a_set_whose_list_costs_over_seventy_ms_is_named() {
        let cost = |call: f64| {
            let mut rows = set_rows("LDN 1272", 180.0, |_| 21.0, |_| 13.0);
            for (i, r) in rows.iter_mut().enumerate() {
                r.list_fit_ms = Some(30.0);
                r.list_refit_ms = Some(25.0);
                r.list_ap_ms = Some(12.0);
                r.call_ms = Some(call);
                if i % 10 == 0 {
                    r.plain_ms = Some(1900.0);
                }
            }
            let lines = forced_verdict_lines(&rows, None, None);
            lines.iter().find(|l| l.starts_with("list cost per set")).unwrap().clone()
        };
        let slow = cost(2000.0);
        assert!(slow.contains("cam/-/LDN 1272 30/25/12/100"), "{slow}");
        assert!(slow.ends_with("over 70 ms: cam/-/LDN 1272"), "{slow}");
        let ok = cost(1960.0);
        assert!(ok.contains("30/25/12/60") && ok.ends_with("over 70 ms: none"), "{ok}");
    }
}
