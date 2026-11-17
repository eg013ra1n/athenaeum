//! A frame's transparency loss against the clear sky of its own equipment (collab v3 wave 6.5, spec §5.2, plan P4-P6).
//!
//! A zero point (`analysis::zero_point`) says how many photons a frame caught per second at a given star brightness.
//! Compared across frames of one rig (camera, telescope, focal length, gain, binning, filter) the best ones mark the
//! rig's clear sky, and a frame's loss is how far its zero point sits below that mark, in magnitudes. The reference is
//! computed on read, never stored: the nearest-rank 90th percentile of the rig's own lights, from at least 30 frames
//! over at least 2 sky nights (one evening is one night however many targets were shot on it), in the frame's own frame set when that has one and otherwise (`RIG_WIDE_FALLBACK`)
//! across the whole rig.
//!
//! Only lights the catalogue uses feed a reference: none in the Black Hole, and a duplicate copy once. A frame received through a collaboration project never does (the
//! scanner never creates a `frames` row for one, so none is normally there; the read still excludes a row whose file is
//! a replica's landed path or lies under the Collaboration folder), and masters and non-light frames never do. A frame
//! in the Black Hole still gets its own loss when asked about; it only never feeds a reference.
//!
//! A zero point counts, for its own loss and as a member of a reference, only when it was judged at `ZP_VERSION`
//! against the installed catalogue's identity. A frame that cannot get a loss says why, in one sentence a user can act
//! on (`LossReason`). The read costs a handful of statements however many frames are asked: queries are chunked, never
//! one per frame.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::Connection;

use crate::analysis::zero_point::{is_colour_frame, ZP_VERSION};
use crate::db::MAX_BIND_PARAMS;
use crate::plate_solve::layers::{catalog_identity, CatalogOpenError};
use crate::star_measure::MEASURE_VERSION;

pub use crate::models::Transparency;

/// The share of a rig's zero points that sits at or below its clear-sky reference (nearest rank).
pub const REFERENCE_PERCENTILE: f64 = 0.90;
/// Fewest frames a reference is taken from.
pub const REFERENCE_MIN_FRAMES: u32 = 30;
/// Fewest imaging nights a reference is taken over.
pub const REFERENCE_MIN_NIGHTS: u32 = 2;
/// A frame whose own frame set has no reference uses the whole rig's. The probe's cross-target spread was 0.025 mag.
pub const RIG_WIDE_FALLBACK: bool = true;

/// Why a frame has no loss. The reasons are listed in the order the first match wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossReason {
    /// No `frame_analysis` row.
    NotAnalysed,
    /// The analysis was made by an older star-measurement version.
    StaleAnalysis,
    /// No plate solve.
    NotSolved,
    /// The frame is OSC or CFA (or an externally debayered colour file): colour cameras are not measured yet (Z5).
    ColourCamera,
    /// The analysis arrived through personal sync: it counts stars and carries none of their rows, so a zero point
    /// can never be measured from it here (a frame whose zero point is current is not judged by this).
    NoStarMetrics,
    /// The stored zero point is not current (another `ZP_VERSION` or catalogue), or no catalogue is installed.
    NotMeasured,
    /// The zero point is current and says "refused".
    NoZeroPoint,
    /// `INSTRUME` or `FOCALLEN` is unknown: there is no rig to compare with.
    NoRig,
    /// The rig has no clear-sky reference yet.
    NoReference { have_frames: u32, have_nights: u32 },
}

impl LossReason {
    /// The one sentence a table cell or tooltip shows.
    pub fn sentence(&self) -> String {
        match self {
            LossReason::NotAnalysed => "not analysed yet".to_string(),
            LossReason::StaleAnalysis => "analysed by an older version — analyse the frame again".to_string(),
            LossReason::NotSolved => "not plate-solved".to_string(),
            LossReason::ColourCamera => "not measured for colour cameras yet".to_string(),
            LossReason::NoStarMetrics => {
                "analysed on another computer — analyse the frame here to measure transparency".to_string()
            }
            LossReason::NotMeasured => {
                "zero point not measured yet — the star catalogue may not be installed".to_string()
            }
            LossReason::NoZeroPoint => {
                "too few catalogue stars for a reliable zero point — a deeper star catalogue tier may help".to_string()
            }
            LossReason::NoRig => "camera or focal length unknown — nothing to compare with".to_string(),
            LossReason::NoReference { have_frames, have_nights } => {
                if *have_frames < REFERENCE_MIN_FRAMES {
                    format!("no clear-sky reference yet, {have_frames} of {REFERENCE_MIN_FRAMES} frames")
                } else {
                    format!("no clear-sky reference yet, {have_nights} of {REFERENCE_MIN_NIGHTS} nights")
                }
            }
        }
    }
}

impl Transparency {
    fn without_numbers(reason: LossReason) -> Self {
        Transparency { zero_point: None, zp_stars: None, zp_sigma: None, loss_mag: None, reason: Some(reason.sentence()) }
    }
}

/// The identity of the installed star catalogue, as a stored zero point is compared with it. `None` when none is
/// installed (`debug`: a fresh install or a Docker build has none) or when it cannot be read (`warn`). Stats the tier
/// files only; it opens no star cache, so a read path can call it.
pub fn installed_catalog_identity(app_data_dir: &Path) -> Option<String> {
    match catalog_identity(app_data_dir) {
        Ok(identity) => Some(identity),
        Err(CatalogOpenError::NotInstalled { .. }) => {
            tracing::debug!("transparency read: no star catalogue installed");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "transparency read: star catalogue unreadable");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Counts the statements a read prepares; the test of "no query per frame" reads it.
struct Stmts(Cell<usize>);

impl Stmts {
    fn prepare<'c>(&self, conn: &'c Connection, sql: &str) -> Result<rusqlite::Statement<'c>> {
        self.0.set(self.0.get() + 1);
        conn.prepare(sql).context("preparing a transparency statement")
    }
}

/// The analysis columns a loss reads.
struct AnalysisFacts {
    measure_version: Option<u32>,
    source_channels: Option<i64>,
    zero_point: Option<f64>,
    zp_stars: Option<i64>,
    zp_sigma: Option<f64>,
    zp_version: Option<u32>,
    zp_catalog: Option<String>,
    /// The analysis counts stars and has no `star_metrics` row (one that arrived through personal sync).
    no_star_rows: bool,
}

/// One asked frame: its header facts, its analysis (if any) and whether it has a solve.
struct FrameRow {
    instrume: Option<String>,
    telescop: Option<String>,
    focallen: Option<f64>,
    gain: Option<f64>,
    xbinning: Option<i64>,
    filter: Option<String>,
    bayerpat: Option<String>,
    analysis: Option<AnalysisFacts>,
    solved: bool,
}

/// What makes two frames comparable: camera, telescope, focal length (whole mm), gain, binning, filter. A missing
/// telescope, gain or filter is its own value; text is compared after trimming.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RigKey {
    instrume: String,
    telescop: Option<String>,
    focallen_mm: i64,
    gain_milli: Option<i64>,
    xbinning: Option<i64>,
    filter: Option<String>,
}

fn clean(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned)
}

/// The rig of a frame, `None` when it has no camera or no focal length (P6: two unknown cameras must not become one
/// rig, and two unknown focal lengths are two unit systems).
fn rig_key(
    instrume: Option<&str>,
    telescop: Option<&str>,
    focallen: Option<f64>,
    gain: Option<f64>,
    xbinning: Option<i64>,
    filter: Option<&str>,
) -> Option<RigKey> {
    let instrume = clean(instrume)?;
    let focallen = focallen.filter(|f| f.is_finite() && f.round() >= 1.0)?;
    Some(RigKey {
        instrume,
        telescop: clean(telescop),
        focallen_mm: focallen.round() as i64,
        gain_milli: gain.filter(|g| g.is_finite()).map(|g| (g * 1000.0).round() as i64),
        xbinning,
        filter: clean(filter),
    })
}

/// The asked frames' own facts, one statement per chunk of ids.
fn load_frame_rows(conn: &Connection, stmts: &Stmts, ids: &[i64]) -> Result<HashMap<i64, FrameRow>> {
    let mut out = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(MAX_BIND_PARAMS) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT f.id, f.instrume, f.telescop, f.focallen, f.gain, f.xbinning, f.filter, f.bayerpat,
                    fa.id, fa.measure_version, fa.source_channels, fa.zero_point, fa.zp_stars, fa.zp_sigma,
                    fa.zp_version, fa.zp_catalog,
                    EXISTS (SELECT 1 FROM plate_solves ps WHERE ps.frame_id = f.id),
                    (fa.stars_detected > 0
                     AND NOT EXISTS (SELECT 1 FROM star_metrics sm WHERE sm.frame_analysis_id = fa.id))
             FROM frames f
             LEFT JOIN frame_analysis fa ON fa.frame_id = f.id
             WHERE f.id IN ({marks})"
        );
        let mut stmt = stmts.prepare(conn, &sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                let analysis_id: Option<i64> = r.get(8)?;
                let analysis = if analysis_id.is_some() {
                    Some(AnalysisFacts {
                        measure_version: r.get(9)?,
                        source_channels: r.get(10)?,
                        zero_point: r.get(11)?,
                        zp_stars: r.get(12)?,
                        zp_sigma: r.get(13)?,
                        zp_version: r.get(14)?,
                        zp_catalog: r.get(15)?,
                        no_star_rows: r.get::<_, Option<bool>>(17)?.unwrap_or(false),
                    })
                } else {
                    None
                };
                Ok((
                    r.get::<_, i64>(0)?,
                    FrameRow {
                        instrume: r.get(1)?,
                        telescop: r.get(2)?,
                        focallen: r.get(3)?,
                        gain: r.get(4)?,
                        xbinning: r.get(5)?,
                        filter: r.get(6)?,
                        bayerpat: r.get(7)?,
                        analysis,
                        solved: r.get(16)?,
                    },
                ))
            })
            .context("reading the frames of a transparency read")?;
        for row in rows {
            let (id, frame) = row.context("reading a frame of a transparency read")?;
            out.insert(id, frame);
        }
    }
    Ok(out)
}

/// The frame set of each asked frame that is in one (the lowest id when custom sets overlap), through the catalogue's
/// own session and night rows.
fn load_frame_sets(conn: &Connection, stmts: &Stmts, ids: &[i64]) -> Result<HashMap<i64, i64>> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(MAX_BIND_PARAMS) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT sm.frame_id, MIN(n.frames_set_id)
             FROM session_members sm
             JOIN sessions s ON s.id = sm.session_id
             JOIN imaging_nights n ON n.id = s.imaging_night_id
             WHERE sm.frame_id IN ({marks})
             GROUP BY sm.frame_id"
        );
        let mut stmt = stmts.prepare(conn, &sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .context("reading the frame sets of a transparency read")?;
        for row in rows {
            let (frame_id, set_id) = row.context("reading a frame set of a transparency read")?;
            out.insert(frame_id, set_id);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

/// The catalogue's calendar key of a night row (`n` = `imaging_nights`): the date of the evening, the same expression
/// the Shoot Calendar groups by (`api::calendar`). Night rows belong to a frame set, so one evening on two targets is
/// two rows and one key: the sky does not care which target the telescope pointed at.
const SKY_NIGHT_SQL: &str = "DATE(n.start_time, '-12 hours')";

/// The zero points one reference is taken from: frames by id (a frame in two sets counts once) and the sky nights
/// (`SKY_NIGHT_SQL` keys) they were taken on.
#[derive(Default)]
struct Pool {
    zero_points: HashMap<i64, f64>,
    nights: HashSet<String>,
}

impl Pool {
    fn add(&mut self, frame_id: i64, night: Option<&str>, zero_point: f64) {
        self.zero_points.insert(frame_id, zero_point);
        if let Some(night) = night {
            self.nights.insert(night.to_owned());
        }
    }

    /// The reference, or the counts that fell short of it.
    fn reference(&self) -> std::result::Result<f64, (u32, u32)> {
        let frames = self.zero_points.len() as u32;
        let nights = self.nights.len() as u32;
        if frames < REFERENCE_MIN_FRAMES || nights < REFERENCE_MIN_NIGHTS {
            return Err((frames, nights));
        }
        let mut sorted: Vec<f64> = self.zero_points.values().copied().collect();
        sorted.sort_by(f64::total_cmp);
        Ok(nearest_rank(&sorted, REFERENCE_PERCENTILE))
    }
}

/// Nearest-rank percentile of an ascending list: the value at 1-based rank `ceil(p · n)`.
fn nearest_rank(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    // 0.9 · n is exact in f64 for every n in play (0.9 · 30 is 27.0), so the ceiling is the true rank. The tolerance is
    // only a guard against a product that lands a hair above a whole number.
    let rank = ((p * n as f64 - 1e-9).ceil() as usize).clamp(1, n);
    sorted[rank - 1]
}

/// One rig's lights, whole and by frame set.
#[derive(Default)]
struct RigPools {
    all: Pool,
    by_set: HashMap<i64, Pool>,
}

/// The Collaboration folder's path, if one is designated. Frames under it were received, not shot.
fn collaboration_roots(conn: &Connection, stmts: &Stmts) -> Result<Vec<String>> {
    let mut stmt = stmts.prepare(conn, "SELECT path FROM scan_roots WHERE kind = 'collaboration'")?;
    let roots = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .context("reading the collaboration folder")?
        .collect::<rusqlite::Result<Vec<String>>>()
        .context("reading the collaboration folder")?;
    Ok(roots)
}

/// The raw `instrume` spellings in the catalogue whose trimmed text is one of `instrumes` (already trimmed). The rig key
/// trims in Rust, so SQL must match the same spellings without a `TRIM()` that would defeat the camera index: this one
/// index-only scan of the distinct values gives the exact strings to bind.
fn raw_instrume_spellings(conn: &Connection, stmts: &Stmts, instrumes: &[String]) -> Result<Vec<String>> {
    let wanted: HashSet<&str> = instrumes.iter().map(String::as_str).collect();
    let mut stmt = stmts.prepare(conn, "SELECT DISTINCT instrume FROM frames WHERE instrume IS NOT NULL")?;
    let spellings = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .context("reading the camera names of a transparency read")?
        .collect::<rusqlite::Result<Vec<String>>>()
        .context("reading the camera names of a transparency read")?;
    let mut raw: Vec<String> = spellings.into_iter().filter(|s| wanted.contains(s.trim())).collect();
    raw.sort();
    Ok(raw)
}

/// Every counting zero point of the catalogue's own lights shot with one of `instrumes`, grouped by rig: one statement
/// per chunk of camera spellings. A zero point counts when it was judged at `ZP_VERSION` against `catalog`, the analysis
/// is current, the frame is solved, not a colour frame, in a frame set (a night is needed), a light and not a master,
/// outside the Black Hole, the used copy of its file in its set (a duplicate feeds once), and not received (a replica's
/// landed file, or any file under the Collaboration folder).
///
/// One call costs the lights of the asked cameras (`f.instrume IN (…)` walks the camera index), not of the catalogue.
/// A caller asks once per table, never per row.
fn load_pools(
    conn: &Connection,
    stmts: &Stmts,
    catalog: &str,
    instrumes: &[String],
) -> Result<HashMap<RigKey, RigPools>> {
    let roots = collaboration_roots(conn, stmts)?;
    let (root_pred, root_values) = crate::db::scan_root_prefix_predicate("fi.path", &roots);
    let raw_names = raw_instrume_spellings(conn, stmts, instrumes)?;
    let mut pools: HashMap<RigKey, RigPools> = HashMap::new();
    for chunk in raw_names.chunks(MAX_BIND_PARAMS - 16) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT f.id, f.instrume, f.telescop, f.focallen, f.gain, f.xbinning, f.filter, f.bayerpat,
                    fa.source_channels, fa.zero_point, n.frames_set_id, {night}
             FROM frames f
             JOIN frame_analysis fa ON fa.frame_id = f.id
             JOIN files fi ON fi.id = f.file_id
             JOIN session_members sm ON sm.frame_id = f.id
             JOIN sessions s ON s.id = sm.session_id
             JOIN imaging_nights n ON n.id = s.imaging_night_id
             WHERE f.imagetyp = 'Light' AND COALESCE(f.is_master, 0) = 0
               AND fa.zero_point IS NOT NULL AND fa.zp_version = ? AND fa.zp_catalog = ?
               AND fa.measure_version = ?
               AND EXISTS (SELECT 1 FROM plate_solves ps WHERE ps.frame_id = f.id)
               AND f.focallen >= 0.5
               AND {not_blackholed}
               AND {used_copy}
               AND NOT EXISTS (SELECT 1 FROM project_frames_local pfl
                               WHERE pfl.origin = 'replica' AND pfl.landed_path = fi.path)
               AND NOT ({root_pred})
               AND f.instrume IN ({marks})",
            night = SKY_NIGHT_SQL,
            not_blackholed = crate::db::FRAME_NOT_IN_BLACK_HOLE_SQL,
            used_copy = crate::db::frame_is_used_copy_sql("n.frames_set_id"),
        );
        let mut values: Vec<Value> = vec![
            Value::Integer(i64::from(ZP_VERSION)),
            Value::Text(catalog.to_string()),
            Value::Integer(i64::from(MEASURE_VERSION)),
        ];
        values.extend(root_values.iter().cloned());
        values.extend(chunk.iter().map(|i| Value::Text(i.clone())));
        let mut stmt = stmts.prepare(conn, &sql)?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(values.iter()))
            .context("reading the zero points of a transparency read")?;
        while let Some(r) = rows.next().context("reading a zero point of a transparency read")? {
            let frame_id: i64 = r.get(0)?;
            let instrume: Option<String> = r.get(1)?;
            let telescop: Option<String> = r.get(2)?;
            let bayerpat: Option<String> = r.get(7)?;
            let source_channels: Option<i64> = r.get(8)?;
            let zero_point: f64 = r.get(9)?;
            if !zero_point.is_finite() || is_colour_frame(bayerpat.as_deref(), source_channels) {
                continue;
            }
            let filter: Option<String> = r.get(6)?;
            let Some(rig) =
                rig_key(instrume.as_deref(), telescop.as_deref(), r.get(3)?, r.get(4)?, r.get(5)?, filter.as_deref())
            else {
                continue;
            };
            let set_id: i64 = r.get(10)?;
            let night: Option<String> = r.get(11)?;
            let rig_pools = pools.entry(rig).or_default();
            rig_pools.all.add(frame_id, night.as_deref(), zero_point);
            rig_pools.by_set.entry(set_id).or_default().add(frame_id, night.as_deref(), zero_point);
        }
    }
    Ok(pools)
}

/// The reference for a rig in a frame set: the set's own when it has one, else the rig's across sets
/// (`RIG_WIDE_FALLBACK`). The `Err` counts are those of the widest pool tried.
fn resolve_reference(pools: &HashMap<RigKey, RigPools>, rig: &RigKey, set: Option<i64>) -> std::result::Result<f64, (u32, u32)> {
    let Some(rig_pools) = pools.get(rig) else {
        return Err((0, 0));
    };
    if let Some(set_pool) = set.and_then(|s| rig_pools.by_set.get(&s)) {
        match set_pool.reference() {
            Ok(r) => return Ok(r),
            Err(counts) if !RIG_WIDE_FALLBACK => return Err(counts),
            Err(_) => {}
        }
    } else if set.is_some() && !RIG_WIDE_FALLBACK {
        return Err((0, 0));
    }
    rig_pools.all.reference()
}

// ---------------------------------------------------------------------------
// The read
// ---------------------------------------------------------------------------

/// A frame judged up to the reference.
enum Stage {
    Done(Transparency),
    NeedsReference { zero_point: f64, zp_stars: Option<i64>, zp_sigma: Option<f64>, rig: RigKey },
}

/// Reasons 1 to 8 of [`LossReason`], in order; a frame that clears them all waits for a reference.
fn judge(row: Option<&FrameRow>, catalog: Option<&str>) -> Stage {
    let done = |reason: LossReason| Stage::Done(Transparency::without_numbers(reason));
    let Some(row) = row else {
        return done(LossReason::NotAnalysed);
    };
    let Some(analysis) = &row.analysis else {
        return done(LossReason::NotAnalysed);
    };
    if analysis.measure_version != Some(MEASURE_VERSION) {
        return done(LossReason::StaleAnalysis);
    }
    if !row.solved {
        return done(LossReason::NotSolved);
    }
    if is_colour_frame(row.bayerpat.as_deref(), analysis.source_channels) {
        return done(LossReason::ColourCamera);
    }
    let current = analysis.zp_version == Some(ZP_VERSION)
        && catalog.is_some_and(|c| analysis.zp_catalog.as_deref() == Some(c));
    if !current {
        if analysis.no_star_rows {
            return done(LossReason::NoStarMetrics);
        }
        return done(LossReason::NotMeasured);
    }
    let Some(zero_point) = analysis.zero_point.filter(|z| z.is_finite()) else {
        return done(LossReason::NoZeroPoint);
    };
    let numbers = |reason: LossReason| {
        Stage::Done(Transparency {
            zero_point: Some(zero_point),
            zp_stars: analysis.zp_stars,
            zp_sigma: analysis.zp_sigma,
            loss_mag: None,
            reason: Some(reason.sentence()),
        })
    };
    let Some(rig) = rig_key(
        row.instrume.as_deref(),
        row.telescop.as_deref(),
        row.focallen,
        row.gain,
        row.xbinning,
        row.filter.as_deref(),
    ) else {
        return numbers(LossReason::NoRig);
    };
    Stage::NeedsReference { zero_point, zp_stars: analysis.zp_stars, zp_sigma: analysis.zp_sigma, rig }
}

fn read(
    conn: &Connection,
    stmts: &Stmts,
    catalog: Option<&str>,
    frame_ids: &[i64],
) -> Result<HashMap<i64, Transparency>> {
    let mut seen = HashSet::new();
    let ids: Vec<i64> = frame_ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = load_frame_rows(conn, stmts, &ids)?;
    let stages: Vec<(i64, Stage)> = ids.iter().map(|&id| (id, judge(rows.get(&id), catalog))).collect();

    // A reference is read only when some frame got that far, and only for the cameras those frames name.
    let mut instrumes: Vec<String> = stages
        .iter()
        .filter_map(|(_, s)| match s {
            Stage::NeedsReference { rig, .. } => Some(rig.instrume.clone()),
            Stage::Done(..) => None,
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    instrumes.sort();
    let (pools, sets) = match catalog {
        Some(catalog) if !instrumes.is_empty() => {
            let waiting: Vec<i64> = stages
                .iter()
                .filter(|(_, s)| matches!(s, Stage::NeedsReference { .. }))
                .map(|(id, _)| *id)
                .collect();
            (load_pools(conn, stmts, catalog, &instrumes)?, load_frame_sets(conn, stmts, &waiting)?)
        }
        _ => (HashMap::new(), HashMap::new()),
    };

    let mut memo: HashMap<(RigKey, Option<i64>), std::result::Result<f64, (u32, u32)>> = HashMap::new();
    let mut out = HashMap::with_capacity(stages.len());
    let mut with_loss = 0usize;
    for (id, stage) in stages {
        let transparency = match stage {
            Stage::Done(t) => t,
            Stage::NeedsReference { zero_point, zp_stars, zp_sigma, rig } => {
                let set = sets.get(&id).copied();
                let reference =
                    *memo.entry((rig.clone(), set)).or_insert_with(|| resolve_reference(&pools, &rig, set));
                match reference {
                    Ok(reference) => {
                        with_loss += 1;
                        Transparency {
                            zero_point: Some(zero_point),
                            zp_stars,
                            zp_sigma,
                            loss_mag: Some((reference - zero_point).max(0.0)),
                            reason: None,
                        }
                    }
                    Err((have_frames, have_nights)) => {
                        let reason = LossReason::NoReference { have_frames, have_nights };
                        Transparency {
                            zero_point: Some(zero_point),
                            zp_stars,
                            zp_sigma,
                            loss_mag: None,
                            reason: Some(reason.sentence()),
                        }
                    }
                }
            }
        };
        out.insert(id, transparency);
    }
    tracing::debug!(frames = out.len(), measured = with_loss, "transparency read");
    Ok(out)
}

/// The transparency of each of `frame_ids`: its zero point against the clear-sky reference of its rig. `catalog` is the
/// installed star catalogue's identity ([`installed_catalog_identity`]); `None` means none is installed, and no zero
/// point counts. Every asked id is in the result (an unknown id reads as `NotAnalysed`). One handful of statements for
/// the whole set: chunked by the SQLite variable limit, never one per frame. The reference read costs the lights of the
/// asked cameras, so a caller asks once per table with every id it shows, never once per row.
pub fn transparency_for_frames(
    conn: &Connection,
    catalog: Option<&str>,
    frame_ids: &[i64],
) -> Result<HashMap<i64, Transparency>> {
    read(conn, &Stmts(Cell::new(0)), catalog, frame_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FrameAnalysis;
    use crate::plate_solve::storage::{insert_plate_solve, PlateSolveRecord};

    /// What a seeded frame is made of; the defaults are a counting mono light of rig A, in frame set 1.
    #[derive(Clone)]
    struct Spec {
        instrume: Option<&'static str>,
        telescop: Option<&'static str>,
        focallen: Option<f64>,
        gain: Option<f64>,
        xbinning: Option<i64>,
        filter: Option<&'static str>,
        bayerpat: Option<&'static str>,
        imagetyp: &'static str,
        is_master: bool,
        set: Option<i64>,
        night: i64,
        analysed: bool,
        measure_version: Option<u32>,
        source_channels: i64,
        solved: bool,
        zp: Option<f64>,
        zp_version: Option<u32>,
        zp_catalog: Option<&'static str>,
        path: Option<String>,
        /// A file name other than `<id>.fits` (two copies of one file share it).
        filename: Option<String>,
        /// A header fingerprint, which makes the file one of the duplicate-detection identities.
        fingerprint: Option<String>,
        /// The analysis has a star row (`false`: it arrived through personal sync, which brings the summary only).
        star_rows: bool,
    }

    impl Default for Spec {
        fn default() -> Self {
            Spec {
                instrume: Some("CamA"),
                telescop: Some("Scope"),
                focallen: Some(540.0),
                gain: Some(56.0),
                xbinning: Some(1),
                filter: Some("L"),
                bayerpat: None,
                imagetyp: "Light",
                is_master: false,
                set: Some(1),
                night: 1,
                analysed: true,
                measure_version: Some(MEASURE_VERSION),
                source_channels: 1,
                solved: true,
                zp: Some(20.0),
                zp_version: Some(ZP_VERSION),
                zp_catalog: Some("A"),
                path: None,
                filename: None,
                fingerprint: None,
                star_rows: true,
            }
        }
    }

    struct World {
        conn: Connection,
        next_id: i64,
        sessions: HashMap<(i64, i64), i64>,
    }

    impl World {
        fn new() -> Self {
            let conn = Connection::open_in_memory().unwrap();
            crate::db::schema::init_db(&conn).unwrap();
            World { conn, next_id: 1, sessions: HashMap::new() }
        }

        /// The session of `night` in frame set `set`, created with the set and the night on first use.
        fn session(&mut self, set: i64, night: i64) -> i64 {
            if let Some(&s) = self.sessions.get(&(set, night)) {
                return s;
            }
            self.conn.execute("INSERT OR IGNORE INTO frames_set (id, name) VALUES (?1, 'set')", [set]).unwrap();
            self.conn
                .execute(
                    "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) VALUES (?1, ?2, ?2)",
                    rusqlite::params![set, format!("2026-03-{night:02}T20:00:00Z")],
                )
                .unwrap();
            let night_id = self.conn.last_insert_rowid();
            self.conn
                .execute("INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'x')", [night_id])
                .unwrap();
            let session = self.conn.last_insert_rowid();
            self.sessions.insert((set, night), session);
            session
        }

        fn seed(&mut self, s: &Spec) -> i64 {
            let id = self.next_id;
            self.next_id += 1;
            let path = s.path.clone().unwrap_or_else(|| format!("/x/{id}.fits"));
            self.conn
                .execute(
                    "INSERT INTO files (id, path, filename, size, modified_at, format)
                     VALUES (?1, ?2, ?3, 0, '2026-03-01', 'FITS')",
                    rusqlite::params![id, path, s.filename.clone().unwrap_or_else(|| format!("{id}.fits"))],
                )
                .unwrap();
            if let Some(fp) = &s.fingerprint {
                self.conn
                    .execute(
                        "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'H', ?2)",
                        rusqlite::params![id, fp],
                    )
                    .unwrap();
            }
            self.conn
                .execute(
                    "INSERT INTO frames (id, file_id, imagetyp, is_master, instrume, telescop, focallen, gain,
                                         xbinning, filter, bayerpat)
                     VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        id, s.imagetyp, s.is_master, s.instrume, s.telescop, s.focallen, s.gain, s.xbinning,
                        s.filter, s.bayerpat
                    ],
                )
                .unwrap();
            // The solve first: storing a solve clears the frame's zero point, and the analysis row carries it.
            if s.solved {
                insert_plate_solve(&self.conn, &solve(id)).unwrap();
            }
            if s.analysed {
                let analysis_id = crate::db::analysis::upsert_frame_analysis(&self.conn, &analysis(id, s)).unwrap();
                if s.star_rows {
                    self.conn
                        .execute(
                            "INSERT INTO star_metrics (frame_analysis_id, x, y, peak, flux, fwhm, fwhm_x, fwhm_y,
                                                       eccentricity, theta, fit_method, fit_residual)
                             VALUES (?1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 'gaussian', 0)",
                            [analysis_id],
                        )
                        .unwrap();
                }
            }
            if let Some(set) = s.set {
                let session = self.session(set, s.night);
                self.conn
                    .execute("INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)", [session, id])
                    .unwrap();
            }
            id
        }

        /// One frame per value, alternating over `nights` nights from the base's night.
        fn series(&mut self, base: &Spec, nights: i64, values: &[f64]) -> Vec<i64> {
            values
                .iter()
                .enumerate()
                .map(|(i, &zp)| {
                    self.seed(&Spec { zp: Some(zp), night: base.night + (i as i64) % nights, ..base.clone() })
                })
                .collect()
        }
    }

    fn analysis(id: i64, s: &Spec) -> FrameAnalysis {
        FrameAnalysis {
            id: None,
            frame_id: id,
            file_id: id,
            stars_detected: 200,
            median_fwhm: 4.0,
            median_eccentricity: 0.1,
            median_snr: 100.0,
            median_hfr: 2.0,
            frame_snr: 30.0,
            snr_weight: 1.0,
            psf_signal: 100.0,
            background: 100.0,
            noise: 5.0,
            detection_threshold: 5.0,
            width: 4000,
            height: 3000,
            source_channels: s.source_channels,
            trail_r_squared: 0.0,
            possibly_trailed: false,
            median_beta: None,
            quality_score: None,
            config_hash: Some("t".into()),
            analyzed_at: "2026-03-01T23:00:00Z".into(),
            measure_version: s.measure_version,
            stars_refused: None,
            frame_verdict: None,
            zero_point: s.zp,
            zp_stars: s.zp.map(|_| 80),
            zp_sigma: s.zp.map(|_| 0.03),
            zp_version: s.zp_version,
            zp_catalog: s.zp_catalog.map(str::to_owned),
            zp_refusal: None,
            zp_list_stars: None,
            zp_window_bright: None,
            zp_window_slope: None,
            zp_outdated: false,
        }
    }

    fn solve(id: i64) -> PlateSolveRecord {
        PlateSolveRecord {
            id: None,
            frame_id: id,
            crpix1: 2000.0,
            crpix2: 1500.0,
            crval1: 10.0,
            crval2: 41.0,
            cd1_1: -2.5e-4,
            cd1_2: 0.0,
            cd2_1: 0.0,
            cd2_2: 2.5e-4,
            sip_order: None,
            sip_a_coeffs: None,
            sip_b_coeffs: None,
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 300,
            total_detected: 400,
            rms_residual_px: 0.3,
            rms_residual_arcsec: 0.3,
            pixel_scale_arcsec: 1.0,
            field_rotation_deg: 0.0,
            solve_time_ms: 10,
            catalog_used: "test".into(),
            algorithm_used: "test".into(),
            solved_at: "2026-03-01T23:00:00Z".into(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }

    /// 20.00, 20.01, … for `n` frames.
    fn ramp(n: usize) -> Vec<f64> {
        (0..n).map(|i| 20.0 + 0.01 * i as f64).collect()
    }

    fn ask(w: &World, catalog: Option<&str>, ids: &[i64]) -> HashMap<i64, Transparency> {
        transparency_for_frames(&w.conn, catalog, ids).unwrap()
    }

    fn loss(t: &Transparency) -> f64 {
        t.loss_mag.unwrap_or_else(|| panic!("no loss, reason: {:?}", t.reason))
    }

    #[test]
    fn the_reference_is_the_nearest_rank_ninetieth_percentile() {
        let mut w = World::new();
        let ids = w.series(&Spec::default(), 2, &ramp(30));
        let got = ask(&w, Some("A"), &ids);
        // rank = ceil(0.9 · 30) = 27, the value 20.26; the frame at 20.00 lost 0.26 mag.
        assert!((loss(&got[&ids[0]]) - 0.26).abs() < 1e-9);
        assert_eq!(got[&ids[0]].reason, None);
        assert_eq!(got[&ids[0]].zero_point, Some(20.0));
        assert_eq!(got[&ids[0]].zp_stars, Some(80));
        assert_eq!(got[&ids[0]].zp_sigma, Some(0.03));
        assert!((loss(&got[&ids[26]])).abs() < 1e-9, "the frame at the reference itself lost nothing");
        assert_eq!(nearest_rank(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0], 0.9), 9.0);
    }

    #[test]
    fn a_frame_brighter_than_the_reference_has_no_loss() {
        let mut w = World::new();
        let ids = w.series(&Spec::default(), 2, &ramp(30));
        let got = ask(&w, Some("A"), &ids);
        assert_eq!(got[&ids[29]].loss_mag, Some(0.0));
        assert_eq!(got[&ids[29]].reason, None);
    }

    #[test]
    fn twenty_nine_frames_give_no_reference() {
        let mut w = World::new();
        let ids = w.series(&Spec::default(), 2, &ramp(29));
        let got = ask(&w, Some("A"), &ids);
        let t = &got[&ids[0]];
        assert_eq!(t.loss_mag, None);
        assert_eq!(t.reason.as_deref(), Some("no clear-sky reference yet, 29 of 30 frames"));
    }

    #[test]
    fn one_night_gives_no_reference() {
        let mut w = World::new();
        let ids = w.series(&Spec::default(), 1, &ramp(40));
        let got = ask(&w, Some("A"), &ids);
        let t = &got[&ids[0]];
        assert_eq!(t.loss_mag, None);
        assert_eq!(t.reason.as_deref(), Some("no clear-sky reference yet, 1 of 2 nights"));
    }

    #[test]
    fn the_zero_point_shows_without_a_reference() {
        let mut w = World::new();
        let ids = w.series(&Spec::default(), 2, &ramp(5));
        let got = ask(&w, Some("A"), &ids);
        for (i, id) in ids.iter().enumerate() {
            let t = &got[id];
            assert_eq!(t.loss_mag, None);
            assert!((t.zero_point.unwrap() - (20.0 + 0.01 * i as f64)).abs() < 1e-12);
            assert_eq!(t.zp_stars, Some(80));
            assert_eq!(t.zp_sigma, Some(0.03));
            assert_eq!(t.reason.as_deref(), Some("no clear-sky reference yet, 5 of 30 frames"));
        }
    }

    #[test]
    fn another_rig_does_not_feed_the_reference() {
        let mut w = World::new();
        let own = w.series(&Spec::default(), 2, &ramp(30));
        // Same camera, gain 0: a different rig, brighter by 5 mag; too few of them to have a reference of their own.
        let other = w.series(&Spec { gain: Some(0.0), ..Spec::default() }, 2, &vec![25.0; 10]);
        let got = ask(&w, Some("A"), &[own[0], other[0]]);
        assert!((loss(&got[&own[0]]) - 0.26).abs() < 1e-9, "the gain-0 frames did not move the reference");
        assert_eq!(got[&other[0]].reason.as_deref(), Some("no clear-sky reference yet, 10 of 30 frames"));
    }

    #[test]
    fn a_different_focal_length_filter_or_binning_is_another_rig() {
        let mut w = World::new();
        let own = w.series(&Spec::default(), 2, &ramp(30));
        let f_len = w.series(&Spec { focallen: Some(1000.0), ..Spec::default() }, 2, &vec![25.0; 10]);
        let filt = w.series(&Spec { filter: Some("Ha"), ..Spec::default() }, 2, &vec![25.0; 10]);
        let bin = w.series(&Spec { xbinning: Some(2), ..Spec::default() }, 2, &vec![25.0; 10]);
        let got = ask(&w, Some("A"), &[own[0], f_len[0], filt[0], bin[0]]);
        assert!((loss(&got[&own[0]]) - 0.26).abs() < 1e-9, "none of the other rigs moved the reference");
        assert!(got[&f_len[0]].loss_mag.is_none());
        assert!(got[&filt[0]].loss_mag.is_none());
        assert_eq!(got[&bin[0]].reason.as_deref(), Some("no clear-sky reference yet, 10 of 30 frames"));
        // A focal length that rounds to the same whole mm is the same rig.
        let same = w.series(&Spec { focallen: Some(540.3), ..Spec::default() }, 2, &vec![20.0; 5]);
        let got = ask(&w, Some("A"), &[same[0]]);
        assert!(got[&same[0]].loss_mag.is_some(), "540.3 mm is 540 mm: the same rig as the thirty");
    }

    #[test]
    fn a_missing_telescope_gain_or_filter_is_its_own_rig() {
        let mut w = World::new();
        let own = w.series(&Spec::default(), 2, &ramp(30));
        let no_scope = w.series(&Spec { telescop: None, ..Spec::default() }, 2, &vec![25.0; 10]);
        let no_gain = w.series(&Spec { gain: None, ..Spec::default() }, 2, &vec![25.0; 10]);
        let no_filter = w.series(&Spec { filter: None, ..Spec::default() }, 2, &vec![25.0; 10]);
        let got = ask(&w, Some("A"), &[own[0], no_scope[0], no_gain[0], no_filter[0]]);
        assert!((loss(&got[&own[0]]) - 0.26).abs() < 1e-9, "no frame with a missing field moved the reference");
        for id in [no_scope[0], no_gain[0], no_filter[0]] {
            assert_eq!(got[&id].loss_mag, None);
            assert_eq!(got[&id].reason.as_deref(), Some("no clear-sky reference yet, 10 of 30 frames"));
        }
        // The three missing-field series do not join each other either: each has its 10, not 30.
    }

    #[test]
    fn a_camera_name_with_a_trailing_tab_is_the_same_rig() {
        let mut w = World::new();
        // Fifteen frames spelled "CamA" and fifteen "CamA\t": one rig of thirty over two nights, the Rust-side trim.
        let plain = w.series(&Spec::default(), 2, &ramp(15));
        let tabbed = w.series(&Spec { instrume: Some("CamA\t"), ..Spec::default() }, 2, &ramp(15));
        let got = ask(&w, Some("A"), &[plain[0], tabbed[0]]);
        assert!(got[&plain[0]].loss_mag.is_some(), "{:?}", got[&plain[0]].reason);
        assert!(got[&tabbed[0]].loss_mag.is_some(), "{:?}", got[&tabbed[0]].reason);
        assert_eq!(got[&plain[0]].loss_mag, got[&tabbed[0]].loss_mag, "one rig, one reference");
    }

    #[test]
    fn one_evening_on_two_targets_is_one_night() {
        let mut w = World::new();
        // Set 1 and set 2 each have a night row that starts the same evening: two rows, one night of sky.
        let one = w.series(&Spec::default(), 1, &ramp(20));
        w.series(&Spec { set: Some(2), ..Spec::default() }, 1, &ramp(15));
        let got = ask(&w, Some("A"), &[one[0]]);
        assert_eq!(got[&one[0]].loss_mag, None);
        assert_eq!(got[&one[0]].reason.as_deref(), Some("no clear-sky reference yet, 1 of 2 nights"));
        // The next evening, on either target, completes the two nights.
        w.seed(&Spec { night: 2, ..Spec::default() });
        let got = ask(&w, Some("A"), &[one[0]]);
        assert!(got[&one[0]].loss_mag.is_some(), "{:?}", got[&one[0]].reason);
    }

    #[test]
    fn black_hole_frames_do_not_feed_a_reference() {
        let mut w = World::new();
        let kept = w.series(&Spec::default(), 1, &ramp(25));
        let rejected = w.series(&Spec { night: 2, ..Spec::default() }, 1, &vec![25.0; 10]);
        for id in &rejected {
            w.conn
                .execute(
                    "INSERT INTO black_hole (file_id, from_where, moved_at, original_path)
                     VALUES (?1, 'test', '2026-03-05T00:00:00Z', '/x')",
                    [id],
                )
                .unwrap();
        }
        // Without the ten rejected frames: one night, 25 of 30 frames.
        let got = ask(&w, Some("A"), &[kept[0], rejected[0]]);
        assert_eq!(got[&kept[0]].reason.as_deref(), Some("no clear-sky reference yet, 25 of 30 frames"));
        // A rejected frame asked about still shows its own zero point.
        assert_eq!(got[&rejected[0]].zero_point, Some(25.0));
        // Five more kept frames on night 2 complete a reference, and the rejected frame gets its own loss from it
        // (it is brighter than the reference, so the loss is 0, not absent).
        w.series(&Spec { night: 2, ..Spec::default() }, 1, &ramp(5));
        let got = ask(&w, Some("A"), &[kept[0], rejected[0]]);
        assert_eq!(got[&rejected[0]].loss_mag, Some(0.0));
        assert!(got[&kept[0]].loss_mag.is_some());
    }

    #[test]
    fn a_duplicate_copy_feeds_a_reference_once() {
        let mut w = World::new();
        // Fifteen frames, each present as two files with one fingerprint, size and name, over two nights: 15 frames
        // are not 30, so there is no reference. Counted twice they would complete one.
        let mut ids = Vec::new();
        for i in 0..15 {
            for copy in ["a", "b"] {
                ids.push(w.seed(&Spec {
                    zp: Some(20.0 + 0.01 * i as f64),
                    night: 1 + i % 2,
                    path: Some(format!("/{copy}/dup{i}.fits")),
                    filename: Some(format!("dup{i}.fits")),
                    fingerprint: Some(format!("fp{i}")),
                    ..Spec::default()
                }));
            }
        }
        let got = ask(&w, Some("A"), &ids[..1]);
        assert_eq!(got[&ids[0]].reason.as_deref(), Some("no clear-sky reference yet, 15 of 30 frames"));
    }

    #[test]
    fn a_frame_without_a_camera_has_no_rig() {
        let mut w = World::new();
        w.series(&Spec::default(), 2, &ramp(30));
        let a = w.seed(&Spec { instrume: None, ..Spec::default() });
        let b = w.seed(&Spec { instrume: None, zp: Some(23.0), ..Spec::default() });
        let blank = w.seed(&Spec { instrume: Some("  "), ..Spec::default() });
        let no_focal = w.seed(&Spec { focallen: None, ..Spec::default() });
        let got = ask(&w, Some("A"), &[a, b, blank, no_focal]);
        for id in [a, b, blank, no_focal] {
            let t = &got[&id];
            assert_eq!(t.loss_mag, None);
            assert_eq!(t.reason.as_deref(), Some("camera or focal length unknown — nothing to compare with"));
            assert!(t.zero_point.is_some(), "the zero point still shows");
        }
    }

    #[test]
    fn received_project_frames_do_not_feed_a_reference() {
        let mut w = World::new();
        let own = w.series(&Spec::default(), 2, &ramp(30));
        // In production the scanner never creates a `frames` row for a received frame, so none is normally here.
        // The read still excludes one, so this seeds the two ways such a row could exist: a file that is a replica's
        // landed path, and a file under the Collaboration folder. Both are 5 mag brighter and would move the reference.
        w.conn
            .execute(
                "INSERT INTO collab_projects
                    (project_id, slug, title, data_role, target_name, target_ra_deg, target_dec_deg,
                     target_radius_deg, membership_version, snapshot_payload_b64, snapshot_signature_b64, members_json)
                 VALUES ('p1','m31','M31','send_receive','M31',10.7,41.3,1.5,1,'x','x','[]')",
                [],
            )
            .unwrap();
        w.conn
            .execute("INSERT INTO scan_roots (path, kind) VALUES ('/collab', 'collaboration')", [])
            .unwrap();
        for i in 0..10 {
            let path = format!("/elsewhere/r{i}.fits");
            w.seed(&Spec { zp: Some(25.0), night: 1 + i % 2, path: Some(path.clone()), ..Spec::default() });
            w.conn
                .execute(
                    "INSERT INTO project_frames_local
                        (project_id, frame_uuid, content_version, origin, publisher_account_id, publisher_display,
                         file_name, filter_canonical, state, byte_size, xxh3, blake3, landed_path)
                     VALUES ('p1', ?1, 1, 'replica', 'a', 'Ann', ?2, 'L', 'published', 1, 'x', 'b', ?3)",
                    rusqlite::params![format!("u{i}"), format!("r{i}.fits"), path],
                )
                .unwrap();
        }
        for i in 0..10 {
            w.seed(&Spec {
                zp: Some(25.0),
                night: 1 + i % 2,
                path: Some(format!("/collab/p1/x{i}.fits")),
                ..Spec::default()
            });
        }
        let got = ask(&w, Some("A"), &[own[0]]);
        assert!((loss(&got[&own[0]]) - 0.26).abs() < 1e-9, "twenty received frames at 25.0 did not move the reference");
    }

    #[test]
    fn masters_and_non_light_frames_do_not_feed_a_reference() {
        let mut w = World::new();
        let own = w.series(&Spec::default(), 2, &ramp(30));
        w.series(&Spec { is_master: true, ..Spec::default() }, 2, &vec![25.0; 10]);
        w.series(&Spec { imagetyp: "Master", ..Spec::default() }, 2, &vec![25.0; 10]);
        w.series(&Spec { imagetyp: "Dark", ..Spec::default() }, 2, &vec![25.0; 10]);
        let got = ask(&w, Some("A"), &[own[0]]);
        assert!((loss(&got[&own[0]]) - 0.26).abs() < 1e-9);
    }

    #[test]
    fn a_colour_camera_frame_says_so() {
        let mut w = World::new();
        let own = w.series(&Spec::default(), 2, &ramp(30));
        // OSC by header, and an externally debayered file (no BAYERPAT, three planes); both stored with a value that
        // would move the reference if it counted.
        let osc = w.seed(&Spec { bayerpat: Some("RGGB"), zp: Some(25.0), ..Spec::default() });
        let debayered = w.seed(&Spec { source_channels: 3, zp: Some(25.0), ..Spec::default() });
        w.series(&Spec { bayerpat: Some("RGGB"), ..Spec::default() }, 2, &vec![25.0; 20]);
        let got = ask(&w, Some("A"), &[own[0], osc, debayered]);
        for id in [osc, debayered] {
            let t = &got[&id];
            assert_eq!(t.reason.as_deref(), Some("not measured for colour cameras yet"));
            assert_eq!((t.zero_point, t.loss_mag), (None, None));
        }
        assert!((loss(&got[&own[0]]) - 0.26).abs() < 1e-9, "colour frames did not feed the reference");
    }

    #[test]
    fn a_synced_analysis_says_to_analyse_here() {
        let mut w = World::new();
        let own = w.series(&Spec::default(), 2, &ramp(30));
        // Stars counted, no star rows, no zero point: the catalogue is not the reason, and the sentence says what is.
        let synced = w.seed(&Spec { star_rows: false, zp: None, zp_version: None, zp_catalog: None, ..Spec::default() });
        // The same shape but with a zero point already current is judged by that value, not by the missing rows.
        let measured = w.seed(&Spec { star_rows: false, zp: Some(20.2), ..Spec::default() });
        // No stars counted at all is not a synced analysis: it keeps the ordinary reasons.
        let empty = w.seed(&Spec { star_rows: false, zp: None, zp_version: None, zp_catalog: None, ..Spec::default() });
        w.conn.execute("UPDATE frame_analysis SET stars_detected = 0 WHERE frame_id = ?1", [empty]).unwrap();
        let got = ask(&w, Some("A"), &[synced, measured, empty, own[0]]);
        let t = &got[&synced];
        assert_eq!(
            t.reason.as_deref(),
            Some("analysed on another computer — analyse the frame here to measure transparency")
        );
        assert_eq!((t.zero_point, t.loss_mag), (None, None));
        assert!(got[&measured].loss_mag.is_some(), "{:?}", got[&measured]);
        assert_eq!(
            got[&empty].reason.as_deref(),
            Some("zero point not measured yet — the star catalogue may not be installed")
        );
        assert!(got[&own[0]].loss_mag.is_some());
    }

    #[test]
    fn a_zero_point_from_another_catalogue_neither_counts_nor_shows() {
        let mut w = World::new();
        let ids = w.series(&Spec::default(), 2, &ramp(40));
        let got = ask(&w, Some("B"), &ids);
        for id in &ids {
            let t = &got[id];
            assert_eq!(
                t.reason.as_deref(),
                Some("zero point not measured yet — the star catalogue may not be installed")
            );
            assert_eq!((t.zero_point, t.zp_stars, t.zp_sigma, t.loss_mag), (None, None, None, None));
        }
        // One frame recomputed under B among the forty: the others, still under A, do not make its reference.
        let fresh = w.seed(&Spec { zp_catalog: Some("B"), ..Spec::default() });
        let got = ask(&w, Some("B"), &[fresh]);
        assert_eq!(got[&fresh].reason.as_deref(), Some("no clear-sky reference yet, 1 of 30 frames"));
        // A zero point of another ZP_VERSION is as stale as another catalogue.
        let old = w.seed(&Spec { zp_version: Some(ZP_VERSION + 1), ..Spec::default() });
        let got = ask(&w, Some("A"), &[old]);
        assert_eq!(got[&old].zero_point, None);
    }

    #[test]
    fn a_stale_analysis_does_not_feed_a_reference() {
        let mut w = World::new();
        // 29 current frames are one short of a reference; ten frames of an older star measurement, each with a bright
        // stored zero point, would complete it (and raise it) if they counted.
        let ids = w.series(&Spec::default(), 2, &ramp(29));
        let stale = Spec { measure_version: Some(MEASURE_VERSION - 1), ..Spec::default() };
        let stale_ids = w.series(&stale, 2, &[25.0; 10]);
        let got = ask(&w, Some("A"), &ids);
        assert_eq!(got[&ids[0]].loss_mag, None);
        assert_eq!(got[&ids[0]].reason.as_deref(), Some("no clear-sky reference yet, 29 of 30 frames"));
        // The stale frames neither show their own zero point nor a loss.
        let own = ask(&w, Some("A"), &stale_ids);
        for id in &stale_ids {
            assert_eq!((own[id].zero_point, own[id].loss_mag), (None, None));
        }
        // One more current frame completes the reference, and it is the current frames' ninetieth percentile.
        let more = w.seed(&Spec { zp: Some(20.29), ..Spec::default() });
        let got = ask(&w, Some("A"), &[ids[0], more]);
        assert!((loss(&got[&ids[0]]) - 0.26).abs() < 1e-9, "the stale 25.0 values did not raise the reference");
    }

    #[test]
    fn no_installed_catalogue_gives_not_measured() {
        let mut w = World::new();
        let ids = w.series(&Spec::default(), 2, &ramp(40));
        let got = ask(&w, None, &ids);
        for id in &ids {
            let t = &got[id];
            assert_eq!(
                t.reason.as_deref(),
                Some("zero point not measured yet — the star catalogue may not be installed")
            );
            assert_eq!((t.zero_point, t.loss_mag), (None, None));
        }
    }

    #[test]
    fn the_frame_set_reference_wins_over_the_rig() {
        let mut w = World::new();
        // Set 1: 26 frames at 20.00 and 4 at 20.50 over two nights, so its rank-27 value is 20.50.
        let mut set1 = vec![20.0; 26];
        set1.extend([20.5; 4]);
        let one = w.series(&Spec::default(), 2, &set1);
        // Set 2, another target of the same rig: 33 frames at 21.00 and 7 at 22.00, so its own rank-36 value is 22.00,
        // while the rig's 70 frames reach their rank-63 value at set 2's 33rd frame: 21.00. The two references differ.
        let mut set2 = vec![21.0; 33];
        set2.extend([22.0; 7]);
        let two = w.series(&Spec { set: Some(2), ..Spec::default() }, 2, &set2);
        let got = ask(&w, Some("A"), &[one[0], two[0]]);
        assert!((loss(&got[&one[0]]) - 0.5).abs() < 1e-9, "measured from the set's 20.50, not the rig's 21.00");
        assert!((loss(&got[&two[0]]) - 1.0).abs() < 1e-9, "21.00 against the set's own 22.00, not the rig's 21.00");
    }

    #[test]
    fn the_rig_stands_in_when_the_set_is_short() {
        let mut w = World::new();
        let short = w.series(&Spec::default(), 2, &vec![20.0; 5]);
        w.series(&Spec { set: Some(2), ..Spec::default() }, 2, &vec![21.0; 40]);
        let got = ask(&w, Some("A"), &[short[0]]);
        let t = &got[&short[0]];
        if RIG_WIDE_FALLBACK {
            // 45 frames over two sky nights (both sets shot on the same two evenings), rank 41: 21.00.
            assert!((loss(t) - 1.0).abs() < 1e-9);
        } else {
            assert_eq!(t.loss_mag, None);
            assert_eq!(t.reason.as_deref(), Some("no clear-sky reference yet, 5 of 30 frames"));
        }
    }

    #[test]
    fn a_frame_in_no_set_uses_the_rig_wide_reference() {
        let mut w = World::new();
        w.series(&Spec::default(), 2, &ramp(30));
        let loose = w.seed(&Spec { set: None, zp: Some(20.0), ..Spec::default() });
        let got = ask(&w, Some("A"), &[loose]);
        if RIG_WIDE_FALLBACK {
            assert!((loss(&got[&loose]) - 0.26).abs() < 1e-9);
        } else {
            assert_eq!(got[&loose].loss_mag, None);
        }
    }

    #[test]
    fn each_state_has_its_reason() {
        let mut w = World::new();
        let good = w.series(&Spec::default(), 2, &ramp(30));
        let base = Spec::default();
        let cases: Vec<(Spec, &str)> = vec![
            (Spec { analysed: false, ..base.clone() }, "not analysed yet"),
            (
                Spec { measure_version: Some(MEASURE_VERSION - 1), ..base.clone() },
                "analysed by an older version — analyse the frame again",
            ),
            (Spec { solved: false, ..base.clone() }, "not plate-solved"),
            (Spec { bayerpat: Some("RGGB"), ..base.clone() }, "not measured for colour cameras yet"),
            (
                Spec { star_rows: false, zp: None, zp_version: None, zp_catalog: None, ..base.clone() },
                "analysed on another computer — analyse the frame here to measure transparency",
            ),
            (
                Spec { zp: None, zp_version: None, zp_catalog: None, ..base.clone() },
                "zero point not measured yet — the star catalogue may not be installed",
            ),
            (
                Spec { zp: None, ..base.clone() },
                "too few catalogue stars for a reliable zero point — a deeper star catalogue tier may help",
            ),
            (
                Spec { instrume: None, ..base.clone() },
                "camera or focal length unknown — nothing to compare with",
            ),
            // Precedence: the earlier row wins when a frame matches two.
            (
                Spec { star_rows: false, bayerpat: Some("RGGB"), zp: None, zp_version: None, ..base.clone() },
                "not measured for colour cameras yet",
            ),
            (
                Spec { star_rows: false, zp: None, zp_version: None, instrume: None, ..base.clone() },
                "analysed on another computer — analyse the frame here to measure transparency",
            ),
            (
                Spec { measure_version: Some(MEASURE_VERSION - 1), solved: false, ..base.clone() },
                "analysed by an older version — analyse the frame again",
            ),
            (Spec { analysed: false, solved: false, ..base.clone() }, "not analysed yet"),
            (
                Spec { solved: false, bayerpat: Some("RGGB"), ..base.clone() },
                "not plate-solved",
            ),
            (
                Spec { bayerpat: Some("RGGB"), zp: None, zp_version: None, ..base.clone() },
                "not measured for colour cameras yet",
            ),
            (
                Spec { zp: None, zp_version: None, instrume: None, ..base.clone() },
                "zero point not measured yet — the star catalogue may not be installed",
            ),
            (
                Spec { zp: None, instrume: None, ..base.clone() },
                "too few catalogue stars for a reliable zero point — a deeper star catalogue tier may help",
            ),
        ];
        let ids: Vec<i64> = cases.iter().map(|(spec, _)| w.seed(spec)).collect();
        let mut asked = ids.clone();
        asked.push(good[0]);
        let got = ask(&w, Some("A"), &asked);
        for (id, (_, sentence)) in ids.iter().zip(&cases) {
            let t = &got[id];
            assert_eq!(t.reason.as_deref(), Some(*sentence));
            assert_eq!(t.loss_mag, None, "a reason means no loss");
        }
        assert_eq!(got[&good[0]].reason, None, "a loss means no reason");
        // An id the catalogue does not know reads as not analysed.
        let missing = ask(&w, Some("A"), &[9_999_999]);
        assert_eq!(missing[&9_999_999].reason.as_deref(), Some("not analysed yet"));
    }

    #[test]
    fn five_thousand_frames_cost_few_statements() {
        let mut w = World::new();
        w.conn.execute_batch("BEGIN").unwrap();
        let ids = w.series(&Spec::default(), 4, &(0..5000).map(|i| 20.0 + 0.0001 * i as f64).collect::<Vec<_>>());
        w.conn.execute_batch("COMMIT").unwrap();
        // This counts the statements the module itself prepares through `Stmts::prepare` (rusqlite's trace hooks are not
        // enabled in this crate), so it proves the count does not grow per frame for the queries that go through
        // `Stmts`. It cannot catch a query added outside `Stmts` (a bare `conn.prepare` or `conn.query_row`): a new
        // statement must be routed through `Stmts`, and review has to hold that line.
        let stmts = Stmts(Cell::new(0));
        let got = read(&w.conn, &stmts, Some("A"), &ids).unwrap();
        assert_eq!(got.len(), 5000);
        assert!(got.values().all(|t| t.loss_mag.is_some()), "every frame got a loss");
        let used = stmts.0.get();
        // 6 chunks of 900 for the frames, 6 for their sets, 1 for the collaboration folder, 1 for the camera names,
        // 1 for the zero points.
        assert!(used <= 15, "{used} statements for 5000 frames");
        // The cost is a function of the chunk count, not of the frame count.
        let few = Stmts(Cell::new(0));
        read(&w.conn, &few, Some("A"), &ids[..50]).unwrap();
        assert_eq!(few.0.get(), 5, "frames, sets, folder, camera names, zero points");
    }

    #[test]
    fn the_sentences_are_the_binding_ones() {
        assert_eq!(
            LossReason::NoReference { have_frames: 30, have_nights: 1 }.sentence(),
            "no clear-sky reference yet, 1 of 2 nights"
        );
        assert_eq!(
            LossReason::NoReference { have_frames: 29, have_nights: 5 }.sentence(),
            "no clear-sky reference yet, 29 of 30 frames"
        );
    }

    #[test]
    fn no_installed_catalogue_has_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(installed_catalog_identity(dir.path()), None);
    }
}
