//! The frame query over a resolved scope (MCP plan, Task 2): the frames a [`ScopeFilter`] lets through, read in
//! three statements, then totals, groups and pages computed in Rust, and the frame-set figures that must equal the
//! frame set's own page.
//!
//! Everything below [`fetch_scoped_frames`] is pure: a question reads the frames once and asks [`query_frames`] as
//! often as it needs. Night is the scope's night (`scope::night`, UTC date of `date_obs - 12 h`); month and year are
//! the night's. A frame with `frames.is_master = 1` is a master everywhere: its type reads as the master kind, and
//! only raw LIGHT frames count in `lights` and `integration_s`.

mod calibration;
mod details;
mod findings;
mod frontend_truth;
mod quality;
mod storage;

#[cfg(test)]
mod summary_tests;
#[cfg(test)]
mod tests;

pub use calibration::{calibration_summary, CalibrationLine, FRAME_SETS_PAGE};
pub(crate) use details::file_id_of;
pub use details::{file_details, CalibrationLink, FileDetails};
pub use findings::{
    find_duplicates_in_scope, find_missing_in_scope, processing_folder_note, CopyLine, DuplicateFinding,
    MissingCategory, MissingFinding,
};
pub use frontend_truth::{frame_set_tab, missing_flags, FrameSetTab, MissingFlags};
pub use quality::{quality_summary, EarlierFigures, QualityMetric, QualitySummary, OUTLIERS_SHOWN};
pub use storage::{storage_summary, StorageLine, StorageSummary};

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params_from_iter, Connection};
use serde::{Deserialize, Serialize};

use crate::db::{
    frame_is_used_copy_sql, native_separator_of, FILE_FRAME_COLUMNS, FILE_FRAME_SELECT,
    FRAME_NOT_IN_BLACK_HOLE_SQL, map_file_frame_row,
};
use crate::models::{FileWithFrame, Frame, ImageType};
use crate::scope::night::night_of_date_obs;
use crate::scope::{frame_state_sql, FrameState, ScopeFilter};

/// Groups an answer lists before the rest is summed.
pub const GROUPS_SHOWN: usize = 50;
/// Files shown under each group.
pub const EXAMPLES_PER_GROUP: usize = 3;
/// Files on one page of the file list.
pub const FILES_PAGE: u32 = 50;

/// What the text calls a frame set that has no name.
pub const UNNAMED_SET: &str = "(unnamed)";

/// A frame set a frame belongs to: two sets may share a name, so the id and the Objects tab come with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetRef {
    pub id: i64,
    /// `None` for a set with no name (the page shows it unnamed).
    pub name: Option<String>,
    pub tab: FrameSetTab,
}

impl SetRef {
    /// `set:<id>`.
    pub fn handle(&self) -> String {
        crate::scope::frame_set_handle(self.id)
    }

    /// `'LDN 1272' (Stage, set:231)`: what tells two sets of one name apart.
    pub fn describe(&self) -> String {
        format!("'{}' ({}, {})", self.name.as_deref().unwrap_or(UNNAMED_SET), crate::scope::tab_label(self.tab), self.handle())
    }
}

/// A frame the scope lets through, with what grouping needs that the row itself does not carry.
#[derive(Debug, Clone)]
pub struct ScopedFrame {
    pub file: FileWithFrame,
    /// The file's parent folder, as the catalog stores the path.
    pub folder: String,
    /// R1 night of the observation time.
    pub night: Option<String>,
    pub state: FrameState,
    /// The frame sets that use this copy of the frame, in id order (two sets may share a name).
    pub frame_sets: Vec<SetRef>,
    /// The scan root the file lives under (its path and kind), when one does.
    pub scan_root: Option<(String, String)>,
}

/// What a question may group by.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars")]
pub enum GroupKey {
    Object,
    FrameSet,
    Filter,
    Camera,
    Telescope,
    Night,
    Month,
    Year,
    FrameType,
    Exposure,
    Folder,
    ScanRoot,
    State,
}

/// The figures of a set of frames.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub frames: usize,
    /// Raw LIGHT frames (a master is not one).
    pub lights: usize,
    /// Sum of the raw lights' exposures, seconds.
    pub integration_s: f64,
    pub bytes: i64,
    pub nights: usize,
    pub first_night: Option<String>,
    pub last_night: Option<String>,
}

/// One file of a list, with the header values a person reads.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileLine {
    /// `file:<file id>`.
    pub handle: String,
    pub file_name: String,
    pub folder: String,
    pub imagetyp: Option<String>,
    pub object: Option<String>,
    pub filter: Option<String>,
    pub exptime_s: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub ccd_temp: Option<f64>,
    pub binning: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// `YYYY-MM-DD HH:MM:SS`, UTC.
    pub date_obs: Option<String>,
    pub night: Option<String>,
    pub instrume: Option<String>,
    pub telescop: Option<String>,
    pub state: FrameState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameGroup {
    /// The group's value for each key asked for, keyed by the key's snake_case name.
    pub key: BTreeMap<GroupKey, String>,
    pub label: String,
    pub totals: Totals,
    pub examples: Vec<FileLine>,
    /// When grouped by frame set: the handles (`set:<id>`) of the sets this group names, in id order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub frame_set_handles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePage {
    /// Zero-based.
    pub page: u32,
    pub page_count: u32,
    pub items: Vec<FileLine>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameQueryResult {
    pub totals: Totals,
    /// At most [`GROUPS_SHOWN`], most frames first.
    pub groups: Vec<FrameGroup>,
    pub groups_total: usize,
    /// The figures of the groups not listed.
    pub rest: Option<Totals>,
    pub files: Option<FilePage>,
}

/// A frame set's figures, the same the frame set's page shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetTotals {
    /// The members the page counts.
    pub frames: usize,
    /// The page's integration time, seconds.
    pub integration_s: f64,
    /// The set's members now in the Black Hole, which the page's figures leave out: (frames, seconds). `None`
    /// when there are none.
    pub black_hole_left_out: Option<(usize, f64)>,
    /// Per filter: (filter, frames, seconds) over the same members.
    pub by_filter: Vec<(String, usize, f64)>,
}

// ---- helpers over one frame ----

/// The parent of a catalog path, split on the path's own separator; the root keeps its separator.
pub(crate) fn parent_of(path: &str) -> String {
    let sep = native_separator_of(path);
    match path.rfind(sep) {
        None => String::new(),
        Some(i) => {
            let parent = &path[..i];
            if parent.is_empty() || parent.ends_with(':') {
                path[..=i].to_string()
            } else {
                parent.to_string()
            }
        }
    }
}

fn master_of(kind: &ImageType) -> ImageType {
    match kind {
        ImageType::Light => ImageType::MasterLight,
        ImageType::Dark => ImageType::MasterDark,
        ImageType::Flat => ImageType::MasterFlat,
        ImageType::Bias => ImageType::MasterBias,
        ImageType::DarkFlat => ImageType::MasterDarkFlat,
        other => other.clone(),
    }
}

/// The frame type as a person reads it: the master kind for a frame flagged `is_master`.
fn type_name(frame: &Frame) -> Option<String> {
    let kind = frame.imagetyp.as_ref()?;
    let kind = if frame.is_master { master_of(kind) } else { kind.clone() };
    Some(format!("{kind:?}"))
}

fn is_raw_light(frame: &Frame) -> bool {
    frame.imagetyp == Some(ImageType::Light) && !frame.is_master
}

fn state_name(state: FrameState) -> &'static str {
    match state {
        FrameState::Catalog => "catalog",
        FrameState::SoftArchived => "soft_archived",
        FrameState::Zipped => "zipped",
        FrameState::BlackHole => "black_hole",
    }
}

fn state_from_name(name: &str) -> Option<FrameState> {
    FrameState::ALL.into_iter().find(|s| state_name(*s) == name)
}

const NO_VALUE: &str = "(no ";

fn none_of(what: &str) -> String {
    format!("{NO_VALUE}{what})")
}

fn exposure_text(e: f64) -> String {
    format!("{}", (e * 1000.0).round() / 1000.0)
}

/// The value of `key` for one frame, as the group key carries it.
fn key_value(f: &ScopedFrame, key: GroupKey) -> String {
    let frame = f.file.frame.as_ref();
    let text = |v: Option<&String>, what: &str| match v.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => none_of(what),
    };
    match key {
        GroupKey::Object => text(frame.and_then(|x| x.object.as_ref()), "object"),
        GroupKey::Filter => text(frame.and_then(|x| x.filter.as_ref()), "filter"),
        GroupKey::Camera => text(frame.and_then(|x| x.instrume.as_ref()), "camera"),
        GroupKey::Telescope => text(frame.and_then(|x| x.telescop.as_ref()), "telescope"),
        GroupKey::FrameSet if f.frame_sets.is_empty() => none_of("frame set"),
        GroupKey::FrameSet => f.frame_sets.iter().map(SetRef::describe).collect::<Vec<_>>().join(", "),
        GroupKey::Night => f.night.clone().unwrap_or_else(|| none_of("night")),
        GroupKey::Month => f.night.as_deref().and_then(|n| n.get(..7)).map_or_else(|| none_of("night"), str::to_string),
        GroupKey::Year => f.night.as_deref().and_then(|n| n.get(..4)).map_or_else(|| none_of("night"), str::to_string),
        GroupKey::FrameType => type_name_or_none(frame),
        GroupKey::Exposure => frame.and_then(|x| x.exptime).map_or_else(|| none_of("exposure"), exposure_text),
        GroupKey::Folder => f.folder.clone(),
        GroupKey::ScanRoot => f.scan_root.as_ref().map_or_else(|| none_of("scan root"), |(p, _)| p.clone()),
        GroupKey::State => state_name(f.state).to_string(),
    }
}

fn type_name_or_none(frame: Option<&Frame>) -> String {
    frame.and_then(type_name).unwrap_or_else(|| none_of("frame type"))
}

/// The order a group's label names its parts in.
const LABEL_ORDER: [GroupKey; 13] = [
    GroupKey::Object,
    GroupKey::FrameSet,
    GroupKey::FrameType,
    GroupKey::Camera,
    GroupKey::Telescope,
    GroupKey::Night,
    GroupKey::Month,
    GroupKey::Year,
    GroupKey::Filter,
    GroupKey::Exposure,
    GroupKey::Folder,
    GroupKey::ScanRoot,
    GroupKey::State,
];

/// `IC 5146, night 2024-10-05, filter H, 300 s, 24 frames`.
pub fn group_label(key: &BTreeMap<GroupKey, String>, totals: &Totals) -> String {
    let mut parts: Vec<String> = Vec::new();
    for k in LABEL_ORDER {
        let Some(v) = key.get(&k) else { continue };
        if v.starts_with(NO_VALUE) {
            parts.push(v.clone());
            continue;
        }
        parts.push(match k {
            GroupKey::Object | GroupKey::FrameType | GroupKey::Camera | GroupKey::Telescope => v.clone(),
            GroupKey::FrameSet => format!("frame set {v}"),
            GroupKey::Night => format!("night {v}"),
            GroupKey::Month => format!("month {v}"),
            GroupKey::Year => format!("year {v}"),
            GroupKey::Filter => format!("filter {v}"),
            GroupKey::Exposure => format!("{v} s"),
            GroupKey::Folder => format!("in {v}"),
            GroupKey::ScanRoot => format!("scan root {v}"),
            GroupKey::State => match v.as_str() {
                "soft_archived" => "soft-archived".to_string(),
                "black_hole" => "Black Hole".to_string(),
                other => other.to_string(),
            },
        });
    }
    parts.push(format!("{} {}", totals.frames, if totals.frames == 1 { "frame" } else { "frames" }));
    parts.join(", ")
}

/// One file of a list.
pub fn file_line(f: &ScopedFrame) -> FileLine {
    let frame = f.file.frame.as_ref();
    FileLine {
        handle: format!("file:{}", f.file.file.id.unwrap_or_default()),
        file_name: f.file.file.filename.clone(),
        folder: f.folder.clone(),
        imagetyp: frame.and_then(type_name),
        object: frame.and_then(|x| x.object.clone()),
        filter: frame.and_then(|x| x.filter.clone()),
        exptime_s: frame.and_then(|x| x.exptime),
        gain: frame.and_then(|x| x.gain),
        offset: frame.and_then(|x| x.offset),
        ccd_temp: frame.and_then(|x| x.ccd_temp),
        binning: frame.and_then(|x| x.binning.clone()),
        width: frame.and_then(|x| x.naxis1).map(i64::from),
        height: frame.and_then(|x| x.naxis2).map(i64::from),
        date_obs: frame.and_then(|x| x.date_obs).map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()),
        night: f.night.clone(),
        instrume: frame.and_then(|x| x.instrume.clone()),
        telescop: frame.and_then(|x| x.telescop.clone()),
        state: f.state,
    }
}

// ---- grouping ----

#[derive(Default)]
struct Acc {
    frames: usize,
    lights: usize,
    integration_s: f64,
    bytes: i64,
    nights: BTreeSet<String>,
    /// Indices of the first frames of the group, in input order.
    examples: Vec<usize>,
    /// Handles of the frame sets of the group's frames (kept only when grouping by frame set).
    set_handles: BTreeMap<i64, String>,
}

impl Acc {
    fn add(&mut self, index: usize, f: &ScopedFrame) {
        self.frames += 1;
        self.bytes += f.file.file.size;
        if let Some(frame) = f.file.frame.as_ref().filter(|x| is_raw_light(x)) {
            self.lights += 1;
            self.integration_s += frame.exptime.unwrap_or(0.0);
        }
        if let Some(n) = &f.night {
            if !self.nights.contains(n) {
                self.nights.insert(n.clone());
            }
        }
        if self.examples.len() < EXAMPLES_PER_GROUP {
            self.examples.push(index);
        }
    }

    fn add_sets(&mut self, f: &ScopedFrame) {
        for s in &f.frame_sets {
            self.set_handles.entry(s.id).or_insert_with(|| s.handle());
        }
    }

    fn merge(&mut self, other: &Acc) {
        self.frames += other.frames;
        self.lights += other.lights;
        self.integration_s += other.integration_s;
        self.bytes += other.bytes;
        self.nights.extend(other.nights.iter().cloned());
    }

    fn totals(&self) -> Totals {
        Totals {
            frames: self.frames,
            lights: self.lights,
            integration_s: self.integration_s,
            bytes: self.bytes,
            nights: self.nights.len(),
            first_night: self.nights.first().cloned(),
            last_night: self.nights.last().cloned(),
        }
    }
}

/// Totals, groups by `group_by` (most frames first, then label), the rest summed, and one page of files when
/// `files_page` (zero-based) is given. `frames` is read in the order given; examples and pages keep it.
pub fn query_frames(frames: &[ScopedFrame], group_by: &[GroupKey], files_page: Option<u32>) -> FrameQueryResult {
    let mut keys: Vec<GroupKey> = Vec::new();
    for k in group_by {
        if !keys.contains(k) {
            keys.push(*k);
        }
    }

    let by_set = keys.contains(&GroupKey::FrameSet);
    let mut all = Acc::default();
    let mut groups: HashMap<Vec<String>, Acc> = HashMap::new();
    for (i, f) in frames.iter().enumerate() {
        all.add(i, f);
        if !keys.is_empty() {
            let values: Vec<String> = keys.iter().map(|k| key_value(f, *k)).collect();
            let acc = groups.entry(values).or_default();
            acc.add(i, f);
            if by_set {
                acc.add_sets(f);
            }
        }
    }

    let groups_total = groups.len();
    let mut built: Vec<(BTreeMap<GroupKey, String>, Acc, String)> = groups
        .into_iter()
        .map(|(values, acc)| {
            let key: BTreeMap<GroupKey, String> = keys.iter().copied().zip(values).collect();
            let label = group_label(&key, &acc.totals());
            (key, acc, label)
        })
        .collect();
    built.sort_by(|a, b| b.1.frames.cmp(&a.1.frames).then_with(|| a.2.cmp(&b.2)));

    let mut rest_acc = Acc::default();
    let mut shown = Vec::new();
    for (n, (key, acc, label)) in built.into_iter().enumerate() {
        if n < GROUPS_SHOWN {
            shown.push(FrameGroup {
                totals: acc.totals(),
                examples: acc.examples.iter().map(|i| file_line(&frames[*i])).collect(),
                frame_set_handles: acc.set_handles.values().cloned().collect(),
                key,
                label,
            });
        } else {
            rest_acc.merge(&acc);
        }
    }
    let rest = (groups_total > GROUPS_SHOWN).then(|| rest_acc.totals());

    let files = files_page.map(|page| {
        let page_count = u32::try_from(frames.len().div_ceil(FILES_PAGE as usize)).unwrap_or(u32::MAX);
        let start = (page as usize).saturating_mul(FILES_PAGE as usize);
        let items = frames.iter().skip(start).take(FILES_PAGE as usize).map(file_line).collect();
        FilePage { page, page_count, items }
    });

    FrameQueryResult { totals: all.totals(), groups: shown, groups_total, rest, files }
}

/// `45 s`, `12 min`, `2 h 05 min`.
pub fn format_duration(seconds: f64) -> String {
    let s = seconds.max(0.0);
    if s < 60.0 {
        return format!("{} s", s.round() as u64);
    }
    let minutes = (s / 60.0).round() as u64;
    if minutes < 60 {
        return format!("{minutes} min");
    }
    format!("{} h {:02} min", minutes / 60, minutes % 60)
}

// ---- fetching ----

/// The frames `filter` lets through, in three statements however many there are: the rows (with their night text
/// and their state, the state decided by the scope's own [`frame_state_sql`]), the [`SetRef`]s of the frame sets
/// that use them (one grouped read; a set with no name is kept, unnamed), and the scan roots (read once, matched in Rust). Ordered by observation time,
/// then path.
pub fn fetch_scoped_frames(conn: &Connection, filter: &ScopeFilter) -> Result<Vec<ScopedFrame>> {
    // The state test is written for `f` = frames / `fl` = files; this scalar subquery names them so, and the row's
    // own `f` (files) is shadowed inside it.
    let state_case = format!(
        "(SELECT CASE WHEN {bh} THEN 'black_hole' WHEN {zip} THEN 'zipped' \
                      WHEN {soft} THEN 'soft_archived' WHEN {cat} THEN 'catalog' END \
          FROM frames f JOIN files fl ON fl.id = f.file_id WHERE f.id = fr.id)",
        bh = frame_state_sql(FrameState::BlackHole),
        zip = frame_state_sql(FrameState::Zipped),
        soft = frame_state_sql(FrameState::SoftArchived),
        cat = frame_state_sql(FrameState::Catalog),
    );
    let sql = format!(
        "SELECT {select}, fr.date_obs, {state_case} FROM files f JOIN frames fr ON fr.file_id = f.id \
         WHERE {filter} ORDER BY fr.date_obs, f.path, fr.id",
        select = FILE_FRAME_SELECT,
        filter = filter.sql,
    );
    let mut stmt = conn.prepare(&sql).with_context(|| format!("prepare scoped frames: {sql}"))?;
    let rows = stmt
        .query_map(params_from_iter(filter.params.iter()), |row| {
            let (file, frame) = map_file_frame_row(row)?;
            let date_obs: Option<String> = row.get(FILE_FRAME_COLUMNS)?;
            let state: Option<String> = row.get(FILE_FRAME_COLUMNS + 1)?;
            Ok((file, frame, date_obs, state))
        })
        .context("run scoped frames")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read scoped frames")?;
    drop(stmt);

    let sets = fetch_set_refs(conn, filter)?;
    let roots = fetch_scan_roots(conn)?;

    let mut out = Vec::with_capacity(rows.len());
    for (file, frame, date_obs, state) in rows {
        let state = state
            .as_deref()
            .and_then(state_from_name)
            .ok_or_else(|| anyhow!("file {:?} has no state ({state:?})", file.id))?;
        let frame_sets = frame.as_ref().and_then(|x| x.id).and_then(|id| sets.get(&id).cloned()).unwrap_or_default();
        out.push(ScopedFrame {
            folder: parent_of(&file.path),
            night: date_obs.as_deref().and_then(night_of_date_obs),
            state,
            frame_sets,
            scan_root: scan_root_of(&roots, &file.path),
            file: FileWithFrame { file, frame },
        });
    }
    Ok(out)
}

/// Statement two: frame id -> the sets that use this copy of the frame, in id order.
fn fetch_set_refs(conn: &Connection, filter: &ScopeFilter) -> Result<HashMap<i64, Vec<SetRef>>> {
    let sql = format!(
        "SELECT sm.frame_id, fs.id, fs.name, COALESCE(fs.is_custom, 0), COALESCE(fs.is_archived, 0) \
         FROM session_members sm \
         JOIN sessions ss ON ss.id = sm.session_id \
         JOIN imaging_nights sn ON sn.id = ss.imaging_night_id \
         JOIN frames_set fs ON fs.id = sn.frames_set_id \
         JOIN frames f ON f.id = sm.frame_id \
         WHERE sm.frame_id IN (SELECT fr.id FROM frames fr WHERE {filter}) \
           AND {used} \
         ORDER BY sm.frame_id, fs.id",
        filter = filter.sql,
        used = frame_is_used_copy_sql("fs.id"),
    );
    let mut stmt = conn.prepare(&sql).with_context(|| format!("prepare set names: {sql}"))?;
    let rows = stmt
        .query_map(params_from_iter(filter.params.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, bool>(3)?, r.get::<_, bool>(4)?))
        })
        .context("run set names")?;
    let mut out: HashMap<i64, Vec<SetRef>> = HashMap::new();
    for row in rows {
        let (frame_id, id, name, is_custom, is_archived) = row.context("read set names")?;
        let entry = out.entry(frame_id).or_default();
        // A frame in one set through two sessions is listed once.
        if entry.iter().all(|s| s.id != id) {
            let name = name.filter(|n| !n.trim().is_empty());
            entry.push(SetRef { id, name, tab: frame_set_tab(is_custom, is_archived) });
        }
    }
    Ok(out)
}

/// Statement three: every scan root, as (prefix with its separator, path, kind).
fn fetch_scan_roots(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare("SELECT path, kind FROM scan_roots").context("prepare scan roots")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .context("run scan roots")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read scan roots")?;
    Ok(rows
        .into_iter()
        .map(|(path, kind)| {
            let sep = native_separator_of(&path);
            (format!("{}{}", path.trim_end_matches(sep), sep), path, kind)
        })
        .collect())
}

/// The longest root whose folder holds `file_path` (separator-strict, as the scope's own match is).
fn scan_root_of(roots: &[(String, String, String)], file_path: &str) -> Option<(String, String)> {
    roots
        .iter()
        .filter(|(prefix, _, _)| file_path.starts_with(prefix.as_str()))
        .max_by_key(|(prefix, _, _)| prefix.len())
        .map(|(_, path, kind)| (path.clone(), kind.clone()))
}

// ---- frame set parity ----

/// What tells a frame set apart from another of the same name: the Objects tab, the cameras and telescopes of its
/// members and the centre it stores.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameSetFacts {
    pub tab: FrameSetTab,
    /// Distinct non-empty INSTRUME of the members the page counts, sorted.
    pub cameras: Vec<String>,
    /// Distinct non-empty TELESCOP of the same members, sorted.
    pub telescopes: Vec<String>,
    /// `objctra` / `objctdec` as the set stores them; `None` when it stores neither.
    pub centre: Option<(Option<String>, Option<String>)>,
}

/// The facts of one frame set (see [`FrameSetFacts`]).
pub fn frame_set_facts(conn: &Connection, frames_set_id: i64) -> Result<FrameSetFacts> {
    let (is_custom, is_archived, ra, dec): (bool, bool, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT COALESCE(is_custom, 0), COALESCE(is_archived, 0), objctra, objctdec FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .with_context(|| format!("read frame set {frames_set_id}"))?;
    let used = frame_is_used_copy_sql("?1");
    let distinct = |column: &str| -> Result<Vec<String>> {
        let sql = format!(
            "SELECT DISTINCT TRIM(f.{column}) FROM frames f \
             WHERE {MEMBER_OF_SET} AND {FRAME_NOT_IN_BLACK_HOLE_SQL} AND {used} \
               AND f.{column} IS NOT NULL AND TRIM(f.{column}) <> '' ORDER BY 1"
        );
        let mut stmt = conn.prepare(&sql).with_context(|| format!("prepare set {column}: {sql}"))?;
        let values = stmt
            .query_map([frames_set_id], |r| r.get::<_, String>(0))
            .with_context(|| format!("run set {column}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .with_context(|| format!("read set {column}"))?;
        Ok(values)
    };
    let present = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    let (ra, dec) = (present(ra), present(dec));
    Ok(FrameSetFacts {
        tab: frame_set_tab(is_custom, is_archived),
        cameras: distinct("instrume")?,
        telescopes: distinct("telescop")?,
        centre: (ra.is_some() || dec.is_some()).then_some((ra, dec)),
    })
}

/// SQL on `frames f`: a member of set `?1`.
const MEMBER_OF_SET: &str = "f.id IN (SELECT msm.frame_id FROM session_members msm \
     JOIN sessions ms ON ms.id = msm.session_id \
     JOIN imaging_nights mn ON mn.id = ms.imaging_night_id \
     WHERE mn.frames_set_id = ?1)";

/// The first and last night (the scope's night rule, `YYYY-MM-DD`) of the members a frame set's page counts; `None`
/// when no counted member carries a usable `date_obs`.
pub fn frame_set_nights(conn: &Connection, frames_set_id: i64) -> Result<Option<(String, String)>> {
    let used = frame_is_used_copy_sql("?1");
    let night = crate::scope::night::night_sql("f.date_obs");
    let sql = format!(
        "SELECT MIN({night}), MAX({night}) FROM frames f WHERE {MEMBER_OF_SET} AND {FRAME_NOT_IN_BLACK_HOLE_SQL} AND {used}"
    );
    let (first, last): (Option<String>, Option<String>) = conn
        .query_row(&sql, [frames_set_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .with_context(|| format!("read set nights: {sql}"))?;
    Ok(first.zip(last))
}

/// A frame set's figures from the catalog's own page query: `frames` is the member count and `integration_s` the
/// total the page shows (`get_frames_sets_by_project`), so the two cannot differ from the page. `by_filter` reads
/// the same members. `black_hole_left_out` counts the set's members now in the Black Hole (used copy, every member
/// type), which the page's figures leave out.
pub fn frame_set_totals(conn: &Connection, frames_set_id: i64) -> Result<FrameSetTotals> {
    let (set, members) = crate::db::get_frames_sets_by_project(conn, 0)
        .context("read frame sets")?
        .into_iter()
        .find(|(s, _)| s.id == Some(frames_set_id))
        .ok_or_else(|| anyhow!("frame set {frames_set_id} not found"))?;
    let integration_s = set.total_exp_time.unwrap_or(0.0);

    let used = frame_is_used_copy_sql("?1");
    let usable = format!("{MEMBER_OF_SET} AND {FRAME_NOT_IN_BLACK_HOLE_SQL} AND {used}");

    let no_filter = none_of("filter");
    let by_filter_sql = format!(
        "SELECT COALESCE(NULLIF(TRIM(f.filter), ''), '{no_filter}'), COUNT(*), COALESCE(SUM(f.exptime), 0) \
         FROM frames f WHERE {usable} GROUP BY 1 ORDER BY 1"
    );
    let mut stmt = conn.prepare(&by_filter_sql).with_context(|| format!("prepare set filters: {by_filter_sql}"))?;
    let by_filter = stmt
        .query_map([frames_set_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize, r.get::<_, f64>(2)?)))
        .context("run set filters")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read set filters")?;
    drop(stmt);

    let left_out_sql = format!(
        "SELECT COUNT(*), COALESCE(SUM(f.exptime), 0) FROM frames f \
         WHERE {MEMBER_OF_SET} AND NOT {FRAME_NOT_IN_BLACK_HOLE_SQL} AND {used}"
    );
    let (left_out_n, left_out_s): (i64, f64) = conn
        .query_row(&left_out_sql, [frames_set_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .with_context(|| format!("read set Black Hole members: {left_out_sql}"))?;
    let black_hole_left_out = (left_out_n > 0).then_some((left_out_n as usize, left_out_s));

    Ok(FrameSetTotals { frames: members, integration_s, black_hole_left_out, by_filter })
}
