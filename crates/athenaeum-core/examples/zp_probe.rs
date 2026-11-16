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
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use athenaeum_core::analysis::analyzer::{map_measurement, measure_raw_frame};
use athenaeum_core::analysis::config::{load_config, AnalysisConfig};
use athenaeum_core::analysis::zero_point::{
    catalog_for_frame, measure_zero_point, ZpInput, ZpMeasurement, ZpParams, ZpRefusal,
};
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::hints::observation_epoch;
use athenaeum_core::plate_solve::open_catalog_layers;
use athenaeum_core::plate_solve::storage::get_plate_solve;
use athenaeum_core::star_measure::raw::{read_raw_frame, RawPlanes};
use athenaeum_core::star_measure::StarFit;
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
const SWITCHES: [&str; 1] = ["osc-c"];

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
        let width = 1 + 2 * slots.len();
        let rows: Vec<Vec<f64>> = used
            .iter()
            .map(|p| {
                let mut r = vec![0.0; width];
                r[0] = p.airmass - mx;
                let j = slots.iter().position(|s| *s == p.night).unwrap_or(0);
                r[1 + 2 * j] = 1.0;
                r[2 + 2 * j] = p.fwhm - mf;
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
                    let slope = beta[2 + 2 * j];
                    nights[*night] = Some(NightFit {
                        intercept: beta[1 + 2 * j] - k * mx - slope * mf,
                        slope,
                        slope_se: (var * inv_diag[2 + 2 * j]).sqrt(),
                        n: mine.len(),
                        fwhm_min: mine.iter().copied().fold(f64::INFINITY, f64::min),
                        fwhm_max: mine.iter().copied().fold(f64::NEG_INFINITY, f64::max),
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
                    night: slot,
                    airmass: x,
                    fwhm,
                    zp: zpc,
                });
                points_u.push(FitPoint {
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

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = match argv.first().map(String::as_str) {
        Some(sub @ ("measure" | "stars" | "common" | "verdict" | "bigap")) => {
            Args::parse(&argv[1..]).and_then(|a| match sub {
                "measure" => cmd_measure(a),
                "stars" => cmd_stars(a),
                "common" => cmd_common(a),
                "bigap" => cmd_bigap(a),
                _ => cmd_verdict(a),
            })
        }
        _ => Err(anyhow!(
            "usage: zp_probe <measure|stars|common|verdict|bigap> ... (see the file's header)"
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
                night: 0,
                airmass,
                fwhm: 2.0 + 0.03 * ((i * 7) % 30) as f64,
                zp: 21.0 - 0.2 * airmass,
            });
            points.push(FitPoint {
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
}
