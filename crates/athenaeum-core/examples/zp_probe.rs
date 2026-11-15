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
//! zp_probe stars   --db <athenaeum.db> --app-data <dir> --frame <id> --out <tsv> [--<field> <v>]...
//! zp_probe common  --db <athenaeum.db> --run <id> --out <tsv>
//! zp_probe verdict --class <broadband|narrowband|osc> [--common <tsv>] <measure.tsv>...
//! ```
//!
//! `--filter -` selects the frames without a filter (an OSC camera).
//! `--<field>` is any `ZpParams` field in snake_case; the default is
//! `ZpParams::default()`. A light whose file is missing, that lives in a zip
//! or that has no stored solve is counted and named on stderr, never skipped
//! silently.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use athenaeum_core::analysis::analyzer::analyze_frame;
use athenaeum_core::analysis::config::{load_config, AnalysisConfig};
use athenaeum_core::analysis::zero_point::{
    catalog_for_frame, measure_zero_point, ZpInput, ZpMeasurement, ZpParams, ZpRefusal,
};
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::hints::observation_epoch;
use athenaeum_core::plate_solve::storage::get_plate_solve;
use athenaeum_core::plate_solve::open_catalog_layers;
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
            match_radius_px: self.take_parsed("match_radius_px")?.unwrap_or(d.match_radius_px),
            central_fraction: self.take_parsed("central_fraction")?.unwrap_or(d.central_fraction),
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

const MEASURE_HEADER: &str = "frame_id\tfile_name\tobject\tfilter\tnight\tdate_obs\texptime\tstars_detected\tmedian_fwhm\tmatched\tkept\tzero_point\tsigma\tstd_error\tmedian_match_px\trefusal";

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
            self.stars_detected.map(|v| v.to_string()).unwrap_or_default(),
            num(self.median_fwhm, 3),
            self.matched.map(|v| v.to_string()).unwrap_or_default(),
            self.kept.map(|v| v.to_string()).unwrap_or_default(),
            num(self.zero_point, 5),
            num(self.sigma, 5),
            num(self.std_error, 5),
            num(self.median_match_px, 4),
            clean(&self.refusal),
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
        }
    }

    /// `date_obs` as seconds since the epoch.
    fn t_s(&self) -> Option<f64> {
        parse_time(&self.date_obs).map(|t| t.timestamp_millis() as f64 / 1000.0)
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
// The verdict arithmetic: plain functions over rows, no files
// ---------------------------------------------------------------------------

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    Some(if n % 2 == 1 { values[n / 2] } else { 0.5 * (values[n / 2 - 1] + values[n / 2]) })
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
    let mut frames: Vec<(f64, f64, f64)> = night_rows
        .iter()
        .filter_map(|r| Some((r.t_s()?, r.zero_point?, r.median_fwhm?)))
        .collect();
    frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    frames
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 < PAIR_MAX_GAP_S)
        .map(|w| Pair { dzp: w[1].1 - w[0].1, dfwhm: w[1].2 - w[0].2 })
        .collect()
}

/// Median of `|Δzero_point|` over the pairs.
fn stability(pairs: &[Pair]) -> Option<f64> {
    let mut d: Vec<f64> = pairs.iter().map(|p| p.dzp.abs()).collect();
    median(&mut d)
}

/// Least-squares slope (through the origin: the differences are symmetric) of
/// `Δzero_point` on `ΔFWHM` over the pairs with `|ΔFWHM| >= 0.2 px`, and their
/// count; `None` under 20 such pairs.
fn seeing_slope(pairs: &[Pair]) -> Option<(f64, usize)> {
    let used: Vec<&Pair> = pairs.iter().filter(|p| p.dfwhm.abs() >= SEEING_MIN_DFWHM_PX).collect();
    if used.len() < SEEING_MIN_PAIRS {
        return None;
    }
    let sxx: f64 = used.iter().map(|p| p.dfwhm * p.dfwhm).sum();
    let sxy: f64 = used.iter().map(|p| p.dfwhm * p.dzp).sum();
    (sxx > 0.0).then(|| (sxy / sxx, used.len()))
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
    CrossTarget::Judged { spread: p90[sets - 1] - p90[0], sets }
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
    let by_id: HashMap<i64, &MeasureRow> =
        measure.iter().filter_map(|r| Some((r.frame_id?, r))).collect();
    let by_name: HashMap<&str, &MeasureRow> =
        measure.iter().map(|r| (r.file_name.as_str(), r)).collect();
    let mut groups: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut joined = 0;
    for c in common {
        let m = match c.frame_id {
            Some(id) => by_id.get(&id).copied(),
            None => by_name.get(name_key(&c.file_name)).copied(),
        };
        let (Some(m), Some(tflux)) = (m, c.green_tflux()) else { continue };
        let (Some(zp), Some(t)) = (m.zero_point, m.exptime.filter(|t| *t > 0.0)) else { continue };
        groups.entry(c.group_key.as_str()).or_default().push(zp - 2.5 * (tflux / t).log10());
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

/// The verdict text (at most 30 lines; the last is the class line).
fn verdict_lines(
    class: &str,
    rows: &[MeasureRow],
    common: Option<&[CommonRow]>,
) -> Vec<String> {
    let mut sets: BTreeMap<(String, String), Vec<&MeasureRow>> = BTreeMap::new();
    for r in rows {
        sets.entry(r.set_key()).or_default().push(r);
    }
    let mut lines = vec![format!("ZP PROBE {class}: {} frames in {} sets", rows.len(), sets.len())];
    let mut bars: Vec<(&str, Outcome)> = Vec::new();

    // coverage
    let cov = coverage(rows);
    let o = judge_max(cov.map(|c| COVERAGE_MIN - c), 0.0);
    lines.push(format!(
        "coverage: {} (limit >= {COVERAGE_MIN:.2}) {}",
        show(cov, 3),
        tag(o)
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

    // stability and seeing on every set's clear night
    let mut set_lines = Vec::new();
    let mut all_pairs: Vec<Pair> = Vec::new();
    let mut set_stabilities: Vec<f64> = Vec::new();
    let mut set_zps: Vec<Vec<f64>> = Vec::new();
    for ((object, filter), members) in &sets {
        let valid: Vec<f64> = members.iter().filter_map(|r| r.zero_point).collect();
        let night = clear_night(members);
        let (night_text, stab) = match &night {
            None => ("none (no night with 20 frames)".to_string(), None),
            Some(n) => {
                let in_night: Vec<&MeasureRow> =
                    members.iter().copied().filter(|r| &r.night == n).collect();
                let pairs = consecutive_pairs(&in_night);
                let s = (pairs.len() >= STABILITY_MIN_PAIRS).then(|| stability(&pairs)).flatten();
                if s.is_some() {
                    all_pairs.extend(pairs.iter().copied());
                }
                (format!("{n} ({} frames, {} pairs)", in_night.len(), pairs.len()), s)
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
    let worst_stab = set_stabilities.iter().copied().fold(None, |a: Option<f64>, v| {
        Some(a.map_or(v, |x| x.max(v)))
    });
    let o = judge_max(worst_stab, STABILITY_MAX_MAG);
    lines.push(format!(
        "stability: {} mag worst of {} judged sets (limit <= {STABILITY_MAX_MAG:.2}) {}",
        show(worst_stab, 4),
        set_stabilities.len(),
        tag(o)
    ));
    bars.push(("stability", o));

    let slope = seeing_slope(&all_pairs);
    let o = judge_max(slope.map(|(s, _)| s.abs()), SEEING_MAX_SLOPE);
    lines.push(match slope {
        Some((s, n)) => format!(
            "seeing: slope {s:.4} mag/px over {n} pairs (limit |slope| <= {SEEING_MAX_SLOPE:.2}) {}",
            tag(o)
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
            if spread <= CROSS_MAX_SPREAD_MAG { "ALLOWED" } else { "NOT ALLOWED" }
        ),
        CrossTarget::Unjudged { sets } => {
            format!("cross-target: n/a mag over {sets} sets -> rig-wide fallback UNJUDGED")
        }
    });

    // The final line takes one line and the set lines fill what is left.
    let room = VERDICT_MAX_LINES.saturating_sub(lines.len() + 1);
    lines.extend(set_lines.into_iter().take(room));

    let names = |want: Outcome| {
        bars.iter().filter(|(_, o)| *o == want).map(|(n, _)| *n).collect::<Vec<_>>().join(",")
    };
    lines.push(if bars.iter().any(|(_, o)| *o == Outcome::Fail) {
        format!("ZP PROBE {class}: FAIL({})", names(Outcome::Fail))
    } else if bars.iter().any(|(_, o)| *o == Outcome::Unjudged) {
        format!("ZP PROBE {class}: UNJUDGED({})", names(Outcome::Unjudged))
    } else {
        format!("ZP PROBE {class}: PASS")
    });
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
}

const CANDIDATE_COLUMNS: &str = "f.id, fi.path, fi.archive_zip_path IS NOT NULL, f.object, f.filter, f.date_obs, f.exptime, \
     EXISTS (SELECT 1 FROM plate_solves p WHERE p.frame_id = f.id)";

fn candidate_from(r: &rusqlite::Row<'_>, night_start: Option<String>) -> rusqlite::Result<Candidate> {
    let date_obs: String = r.get::<_, Option<String>>(5)?.unwrap_or_default();
    Ok(Candidate {
        frame_id: r.get(0)?,
        path: r.get(1)?,
        in_zip: r.get(2)?,
        object: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        filter: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        night: night_label(night_start.as_deref(), &date_obs),
        date_obs,
        exptime: r.get(6)?,
        has_solve: r.get(7)?,
    })
}

/// The lights of one rig, object and filter, in `date_obs` order, a second copy
/// of a frame the set holds left out (as `analyze_frame_set` does), each with
/// its catalogue night.
fn load_candidates(conn: &Connection, instrume: &str, object: &str, filter: &str) -> Result<Vec<Candidate>> {
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
        filter_cond = if no_filter { "COALESCE(f.filter, '') = ''" } else { "f.filter = ?3" },
        used_copy = athenaeum_core::db::frame_is_used_copy_sql("n.frames_set_id"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row<'_>| candidate_from(r, r.get::<_, Option<String>>(8)?);
    let rows: Vec<Candidate> = if no_filter {
        stmt.query_map(rusqlite::params![instrume, object], map)?.collect::<Result<_, _>>()?
    } else {
        stmt.query_map(rusqlite::params![instrume, object, filter], map)?.collect::<Result<_, _>>()?
    };
    // A frame the catalogue holds in two nights appears twice: the first stays.
    let mut seen = HashSet::new();
    Ok(rows.into_iter().filter(|c| seen.insert(c.frame_id)).collect())
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
    let pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(threads).build()?);
    Ok(Rig { layers, config, config_hash, pool, params })
}

/// One frame, end to end: the stored solve, an in-memory analysis of its file,
/// the catalogue cone and the measurement. A failure is the row's `refusal`,
/// logged on stderr; it never stops the run.
fn measure_candidate(conn: &Connection, rig: &Rig, c: &Candidate) -> (MeasureRow, Option<ZpMeasurement>) {
    let mut row = MeasureRow {
        frame_id: Some(c.frame_id),
        file_name: Path::new(&c.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| c.path.clone()),
        object: c.object.clone(),
        filter: if c.filter.is_empty() { "-".into() } else { c.filter.clone() },
        night: c.night.clone(),
        date_obs: c.date_obs.clone(),
        exptime: c.exptime,
        ..MeasureRow::default()
    };
    let fail = |row: &mut MeasureRow, code: &str, detail: &dyn std::fmt::Display| {
        eprintln!("zp_probe: frame {} refused {code}: {detail}", c.frame_id);
        row.refusal = code.to_string();
    };

    let wcs = match get_plate_solve(conn, c.frame_id) {
        Ok(Some(rec)) => match rec.to_solution() {
            Some(w) => w,
            None => {
                fail(&mut row, "bad_wcs", &"the stored SIP tables are unusable");
                return (row, None);
            }
        },
        Ok(None) => {
            fail(&mut row, "no_solve", &"no stored solve");
            return (row, None);
        }
        Err(e) => {
            fail(&mut row, "bad_wcs", &e);
            return (row, None);
        }
    };
    let (analysis, stars, _flip) =
        match analyze_frame(&c.path, &rig.config, Some(&rig.pool), &rig.config_hash) {
            Ok(t) => t,
            Err(e) => {
                fail(&mut row, "analysis_failed", &e);
                return (row, None);
            }
        };
    row.stars_detected = Some(analysis.stars_detected);
    row.median_fwhm = Some(analysis.median_fwhm);
    let (width, height) = (analysis.width.max(0) as usize, analysis.height.max(0) as usize);

    let frame = Frame { date_obs: parse_time(&c.date_obs), ..Frame::default() };
    let epoch = observation_epoch(&frame);
    let layers: Vec<&StarCache> = rig.layers.iter().map(|l| l.as_ref()).collect();
    let catalog = match catalog_for_frame(&layers, &wcs, width, height, rig.params.central_fraction, epoch) {
        Ok(cat) => cat,
        Err(e) => {
            fail(&mut row, "no_cone", &e);
            return (row, None);
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
    (row, Some(m))
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn open_read_only(db: &str) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {db} read-only"))
}

fn create_out(path: &str) -> Result<BufWriter<File>> {
    Ok(BufWriter::new(File::create(path).with_context(|| format!("creating {path}"))?))
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
    let (mut measured, mut no_solve, mut in_zip, mut missing, mut refused) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut examined = 0usize;
    for c in &candidates {
        if limit.is_some_and(|n| measured >= n) {
            break;
        }
        examined += 1;
        if c.in_zip {
            in_zip += 1;
            eprintln!("zp_probe: frame {} lives in a zip, not measured: {}", c.frame_id, c.path);
            continue;
        }
        if !Path::new(&c.path).is_file() {
            missing += 1;
            eprintln!("zp_probe: frame {} file missing, not measured: {}", c.frame_id, c.path);
            continue;
        }
        if !c.has_solve {
            no_solve += 1;
            eprintln!("zp_probe: frame {} has no stored solve, not measured: {}", c.frame_id, c.path);
            continue;
        }
        let (row, _) = measure_candidate(&conn, &rig, c);
        if row.zero_point.is_none() {
            refused += 1;
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
         not measured: {no_solve} without a solve, {in_zip} in a zip, {missing} with a missing file; wrote {out}",
        candidates.len()
    );
    Ok(())
}

fn cmd_stars(mut args: Args) -> Result<()> {
    let db = args.require("db")?;
    let app_data = args.require("app-data")?;
    let frame: i64 = args.take_parsed("frame")?.ok_or_else(|| anyhow!("missing --frame"))?;
    let out = args.require("out")?;
    let params = args.zp_params()?;
    args.finish()?;

    let conn = open_read_only(&db)?;
    let rig = open_rig(&conn, &app_data, params)?;
    let c = load_candidate_by_id(&conn, frame)?;
    if c.in_zip || !Path::new(&c.path).is_file() {
        bail!("frame {frame}: file {} is in a zip or missing", c.path);
    }
    let (row, m) = measure_candidate(&conn, &rig, &c);
    let mut w = create_out(&out)?;
    writeln!(w, "frame_id\tg_mag\tzp\tradius_fraction\tmatch_px\tkept")?;
    if let Some(m) = &m {
        for s in &m.per_star {
            writeln!(
                w,
                "{frame}\t{:.3}\t{:.5}\t{:.4}\t{:.4}\t{}",
                s.g_mag,
                s.zp,
                s.radius_fraction,
                s.match_px,
                u8::from(s.kept)
            )?;
        }
    }
    w.flush()?;
    eprintln!(
        "zp_probe: frame {frame}: zero_point {} refusal {:?} matched {:?} kept {:?}; wrote {out}",
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
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM frames WHERE id = ?1", [id], |r| r.get(0))?;
        if n == 1 {
            return Ok(Some((id, "source")));
        }
    }
    let one = |sql: &str, key: &str| -> Result<Option<i64>> {
        let mut stmt = conn.prepare(sql)?;
        let ids: Vec<i64> = stmt.query_map([key], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok((ids.len() == 1).then(|| ids[0]))
    };
    if let Some(p) = landed_path {
        if let Some(id) = one("SELECT fr.id FROM files f JOIN frames fr ON fr.file_id = f.id WHERE f.path = ?1", p)? {
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
    let run: i64 = args.take_parsed("run")?.ok_or_else(|| anyhow!("missing --run"))?;
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
    type RunRow = (Option<i64>, Option<String>, String, Option<String>, Option<i64>, Option<String>, Option<String>);
    let rows: Vec<RunRow> = stmt
        .query_map([run], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<Result<_, _>>()?;
    if rows.is_empty() {
        bail!("run {run} has no frames");
    }
    let mut w = create_out(&out)?;
    writeln!(w, "{COMMON_HEADER}")?;
    let (mut unresolved, mut by_source, mut by_path, mut by_name, mut no_flux) = (0, 0, 0, 0, 0);
    for (own_frame, metrics, group_key, project_name, source_id, landed, set_name) in &rows {
        let file_name = project_name.clone().or_else(|| set_name.clone()).unwrap_or_default();
        let (frame_id, how) = match own_frame {
            Some(id) => (Some(*id), "set_run"),
            None => match resolve_project_frame(&conn, &file_name, *source_id, landed.as_deref())? {
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
            },
        };
        let channels: Vec<Option<f64>> = metrics
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .and_then(|v| v.get("channels").and_then(|c| c.as_array().cloned()))
            .map(|ch| ch.iter().map(|p| p.get("commonTflux").and_then(|x| x.as_f64())).collect())
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
    args.finish()?;
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
    for line in verdict_lines(&class, &rows, common.as_deref()) {
        println!("{line}");
    }
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = match argv.first().map(String::as_str) {
        Some(sub @ ("measure" | "stars" | "common" | "verdict")) => {
            Args::parse(&argv[1..]).and_then(|a| match sub {
                "measure" => cmd_measure(a),
                "stars" => cmd_stars(a),
                "common" => cmd_common(a),
                _ => cmd_verdict(a),
            })
        }
        _ => Err(anyhow!(
            "usage: zp_probe <measure|stars|common|verdict> ... (see the file's header)"
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
        // Every pair carries dFWHM 0.5 px and dzp 0.01: slope 0.02 mag/px.
        let mk = |n: usize| vec![Pair { dzp: 0.01, dfwhm: 0.5 }; n];
        assert_eq!(seeing_slope(&mk(19)), None);
        let (slope, n) = seeing_slope(&mk(20)).unwrap();
        assert_eq!(n, 20);
        assert!((slope - 0.02).abs() < 1e-12, "{slope}");
        // Pairs under 0.2 px do not count toward the 20.
        let mut mixed = mk(19);
        mixed.extend(vec![Pair { dzp: 0.5, dfwhm: 0.1 }; 30]);
        assert_eq!(seeing_slope(&mixed), None);
    }

    #[test]
    fn cross_target_needs_three_sets() {
        let set = |base: f64, n: usize| (0..n).map(|i| base + i as f64 * 1e-4).collect::<Vec<f64>>();
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
    fn the_verdict_ends_in_one_class_line_within_thirty_lines() {
        // Two nights of 30 frames, steady zero point: stability and coverage pass.
        let mut rows = Vec::new();
        for night in 0..2 {
            for i in 0..30 {
                let t = T0 + night * 86_400 + i * 180;
                rows.push(row(
                    &format!("n{night}"),
                    t,
                    Some(21.3 + 0.001 * (i % 3) as f64),
                    3.0 + 0.3 * (i % 2) as f64,
                    800 + 10 * night,
                ));
            }
        }
        let lines = verdict_lines("broadband", &rows, None);
        assert!(lines.len() <= VERDICT_MAX_LINES);
        let last = lines.last().unwrap();
        assert!(last.starts_with("ZP PROBE broadband: "), "{last}");
        assert!(last.contains("UNJUDGED(") || last.ends_with("PASS") || last.contains("FAIL("), "{last}");
    }
}
