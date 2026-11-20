//! The Black Hole plan (MCP plan Task 14, §6.2, §6.4): the first real planning tool. An assistant names duplicate
//! groups or rebuilt-master groups; Athenaeum prepares a plan of copies to move to the Black Hole; only the user
//! applies it, in Athenaeum's window.
//!
//! Only the named groups enter. Each group is a choose-one, "Copy that stays": `chosen` is the one copy under
//! `keepInFolder` (separator-strict) or among `keepFiles`; with none or several there is no keeper, nothing is
//! selected and the group's note says why. The other copies are include/exclude items, selected iff a keeper is set.
//! The kept copy is the user's choice, not a lock: the window may keep another copy, so the planned keeper is an
//! ordinary candidate item. A copy that cannot be moved is locked with its reason: a master with calibration links
//! (or one that another master is built from), a copy whose registration superseded raw frames, a zip-resident copy,
//! a copy whose file cannot be reached, a byte-checked copy whose bytes differ from the keeper's. A file is planned in
//! one group only.
//!
//! The byte checks run on a thread of their own: the tool answers right after the plan begins (a client cancels a
//! call after about a minute, and a cancelled call would give the assistant no handle), and the plan is published
//! when the checks end.
//!
//! The plan holds no catalog connection and no reader guard across a call that takes the context
//! (`verify_duplicate_pair`, `bulk_move_to_black_hole`): the planner reads on the host's reader, drops the guard, and
//! only then stamps files and compares bytes; the applier checks a pooled connection out for its own re-checks and
//! releases it before the move.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rmcp::schemars::JsonSchema;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{
    plan_opened_text, stamp_for_plan, stamp_target, DiskStamp, ItemSnapshot, McpPlanView, PlanApplier, PlanColumn,
    PlanContent, PlanGroup, PlanItem, PlanKind, PlanReport, PlanSelection, PlanState, ReportLine, PLAN_ITEM_CAP,
};
use crate::api::ApiError;
use crate::calibration_library::rebuilt_copies::{find_rebuilt_master_copies, master_link_count};
use crate::db::{file_not_in_black_hole_sql, native_separator_of};
use crate::mcp::answer::{bytes_text, count_of};
use crate::mcp::tools::to_json;
use crate::mcp::{Answer, McpHost};

/// The choose-one label of every group.
pub const KEEP_LABEL: &str = "Copy that stays";
/// A master that calibration links point at stays.
pub const LINKED: &str = "calibration links point at it; it stays in v1";
/// A copy whose registration superseded raw frames stays: moving it would make those frames matchable again.
pub const SUPERSEDING: &str = "raw frames were superseded by it; it stays in v1";
pub const IN_ZIP: &str = "inside a zip; a zip moves to the Black Hole only as a whole";
pub const DIFFERS: &str = "differs in bytes from the copy that stays";
const MOVE_ACTION: &str = "Move to the Black Hole";
/// A plan answer lists at most this many groups with their items (the rest is summed) ...
const GROUPS_LISTED: usize = crate::catalog_query::GROUPS_SHOWN;
/// ... and at most this many items of a group.
const ITEMS_LISTED: usize = 20;
/// Where the Black Hole row says the move came from.
const FROM_WHERE: &str = "assistant";

/// Where the groups come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars")]
pub enum BlackHoleSource {
    /// Groups `find_duplicates` listed (`dup:...`).
    Duplicates,
    /// Groups `find_rebuilt_masters` listed (`rebuilt:...`).
    RebuiltMasters,
}

/// Arguments of `plan_black_hole`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct PlanBlackHoleArgs {
    /// Which kind of group the handles name.
    pub source: BlackHoleSource,
    /// The groups to plan, by the handle the finding tool gave. Only these enter the plan.
    pub groups: Vec<String>,
    /// The copy of each group that lives under this folder stays (separator-strict: the folder or anything below it).
    /// A group with no copy, or more than one copy, under it has no keeper and nothing selected.
    #[serde(default)]
    pub keep_in_folder: Option<String>,
    /// Copies that stay: a `file:<number>` handle an earlier answer gave, or the full path as the catalog stores it.
    #[serde(default)]
    pub keep_files: Vec<String>,
}

/// Compares two catalog files byte for byte: `true` when identical. The planner and the applier take it as a value so
/// a test can count and interrupt the comparisons.
pub type VerifyFn = Arc<dyn Fn(&McpHost, i64, i64) -> Result<bool, ApiError> + Send + Sync>;

/// The real comparison: the Duplicates page's own `verify_duplicate_pair`.
pub fn real_verify() -> VerifyFn {
    Arc::new(|host, a, b| crate::api::files::verify_duplicate_pair(host.ctx(), a, b).map(|r| r.identical))
}

fn plan_columns() -> Vec<PlanColumn> {
    let col = |key: &str, label: &str| PlanColumn { key: key.to_string(), label: label.to_string() };
    vec![
        col("IMAGETYP", "IMAGETYP"),
        col("OBJECT", "OBJECT"),
        col("FILTER", "FILTER"),
        col("EXPTIME", "EXPTIME"),
        col("DATE-OBS", "DATE-OBS"),
        col("INSTRUME", "INSTRUME"),
        col("size", "Size"),
        col("modified", "Modified"),
    ]
}

fn read_failed(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "plan could not read the catalog");
    ApiError::Internal(format!("could not read {what}: {e}"))
}

// ---- what a group is ----

/// A group the assistant named, as the catalog holds it now.
struct GroupSpec {
    /// Words for what makes the copies one group.
    matched: String,
    /// The title when the group has its own words (a rebuilt master's parameters).
    title: Option<String>,
    /// A group whose key does not decide on bytes (`Header`, and `Content`: `files.content_hash` samples 3 x 512 KB):
    /// its copies are byte-checked against the keeper. Only the full-file hash key decides on bytes already.
    byte_checked: bool,
    file_ids: Vec<i64>,
}

/// One copy of a group, read from the catalog.
struct CopyRow {
    file_id: i64,
    frame_id: Option<i64>,
    path: String,
    /// The zip a zip-resident row lives in.
    zip: Option<String>,
    name: String,
    folder: String,
    size: i64,
    values: BTreeMap<String, String>,
    master: bool,
    linked: usize,
    superseding: bool,
}

fn show_time(text: &str) -> String {
    DateTime::parse_from_rfc3339(text)
        .map(|d| d.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|_| text.to_string())
}

fn seconds(e: f64) -> String {
    format!("{}", (e * 1000.0).round() / 1000.0)
}

/// Whether the copy's master set registered over raw frames (those frames are matchable again only while it is
/// unregistered, which moving the master to the Black Hole does).
pub fn supersedes_raw_frames(conn: &Connection, file_id: i64) -> Result<bool, rusqlite::Error> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM calibration_set cs \
           JOIN calibration_set_frames csf ON csf.set_id = cs.id \
           JOIN frames fr ON fr.id = csf.frame_id \
          WHERE fr.file_id = ?1 AND COALESCE(cs.is_master_library, 0) = 1 \
            AND EXISTS (SELECT 1 FROM calibration_set raw WHERE raw.superseded_by_set_id = cs.id)",
        params![file_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn load_copy(conn: &Connection, file_id: i64) -> Result<Option<CopyRow>, ApiError> {
    let row = conn.query_row(
        "SELECT f.path, f.filename, f.size, CAST(f.modified_at AS TEXT), f.archive_zip_path, fr.id, \
                CAST(fr.imagetyp AS TEXT), CAST(fr.object AS TEXT), CAST(fr.filter AS TEXT), fr.exptime, \
                CAST(fr.date_obs AS TEXT), CAST(fr.instrume AS TEXT), COALESCE(fr.is_master, 0) \
           FROM files f LEFT JOIN frames fr ON fr.file_id = f.id WHERE f.id = ?1 ORDER BY fr.id LIMIT 1",
        params![file_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                [r.get::<_, Option<String>>(6)?, r.get::<_, Option<String>>(7)?, r.get::<_, Option<String>>(8)?],
                r.get::<_, Option<f64>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, i64>(12)?,
            ))
        },
    );
    let (path, name, size, modified, zip, frame_id, [imagetyp, object, filter], exptime, date_obs, instrume, is_master) =
        match row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(read_failed("a copy of the group", e)),
        };
    let mut values = BTreeMap::new();
    let mut put = |key: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            values.insert(key.to_string(), v);
        }
    };
    put("IMAGETYP", imagetyp);
    put("OBJECT", object);
    put("FILTER", filter);
    put("EXPTIME", exptime.map(seconds));
    put("DATE-OBS", date_obs.map(|d| show_time(&d)));
    put("INSTRUME", instrume);
    put("size", Some(bytes_text(size)));
    put("modified", modified.map(|m| show_time(&m)));
    let master = is_master != 0;
    let (linked, superseding) = if master {
        (
            master_link_count(conn, file_id).map_err(|e| read_failed("a master's calibration links", format!("{e:#}")))?,
            supersedes_raw_frames(conn, file_id).map_err(|e| read_failed("a master's calibration set", e))?,
        )
    } else {
        (0, false)
    };
    Ok(Some(CopyRow {
        file_id,
        frame_id,
        folder: crate::catalog_query::parent_of(&path),
        zip: zip.filter(|z| !z.is_empty()),
        path,
        name,
        size,
        values,
        master,
        linked,
        superseding,
    }))
}

// ---- the keeper ----

/// What the arguments say about which copy stays.
struct KeepRules {
    folder: Option<String>,
    /// `folder` plus its separator: a path is under the folder when it starts with this.
    prefix: Option<String>,
    ids: HashSet<i64>,
    paths: HashSet<String>,
}

impl KeepRules {
    fn new(args: &PlanBlackHoleArgs) -> Self {
        let folder = args.keep_in_folder.as_deref().map(str::trim).filter(|f| !f.is_empty()).map(str::to_string);
        let prefix = folder.as_deref().map(|f| {
            let sep = native_separator_of(f);
            format!("{}{sep}", f.trim_end_matches(sep))
        });
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        for entry in args.keep_files.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
            match entry.strip_prefix("file:").and_then(|n| n.parse::<i64>().ok()) {
                Some(id) => {
                    ids.insert(id);
                }
                None => {
                    paths.insert(entry.to_string());
                }
            }
        }
        KeepRules { folder, prefix, ids, paths }
    }

    fn names_files(&self) -> bool {
        !self.ids.is_empty() || !self.paths.is_empty()
    }

    fn by_folder(&self, c: &CopyRow) -> bool {
        self.prefix.as_deref().is_some_and(|p| c.path.starts_with(p))
    }

    fn by_files(&self, c: &CopyRow) -> bool {
        self.ids.contains(&c.file_id) || self.paths.contains(&c.path)
    }

    /// The index of the one copy the instructions pick, or the words for why no copy is picked.
    fn pick(&self, copies: &[CopyRow]) -> Result<usize, String> {
        let folder: Vec<usize> = (0..copies.len()).filter(|i| self.by_folder(&copies[*i])).collect();
        let files: Vec<usize> = (0..copies.len()).filter(|i| self.by_files(&copies[*i])).collect();
        let picked: BTreeSet<usize> = folder.iter().chain(files.iter()).copied().collect();
        if picked.len() == 1 {
            return Ok(*picked.iter().next().unwrap_or(&0));
        }
        let mut why: Vec<String> = Vec::new();
        if let Some(f) = &self.folder {
            why.push(match folder.len() {
                0 => format!("no copy under {f}"),
                1 => format!("1 copy under {f}"),
                n => format!("{n} copies under {f}"),
            });
        }
        if self.names_files() {
            why.push(match files.len() {
                0 => "none of the named files is a copy here".to_string(),
                1 => "1 named file".to_string(),
                n => format!("{n} named files"),
            });
        }
        Err(if why.is_empty() {
            "no instruction picks the copy that stays here".to_string()
        } else {
            format!("no instruction picks the copy that stays here ({})", why.join("; "))
        })
    }
}

// ---- resolving and loading ----

fn clean_handles(args: &PlanBlackHoleArgs) -> Result<Vec<String>, ApiError> {
    let mut wanted: Vec<String> = Vec::new();
    for h in args.groups.iter().map(|h| h.trim()).filter(|h| !h.is_empty()) {
        if !wanted.iter().any(|w| w == h) {
            wanted.push(h.to_string());
        }
    }
    if wanted.is_empty() {
        return Err(ApiError::Invalid(
            "plan_black_hole needs the groups to plan (groups), by the handle find_duplicates or find_rebuilt_masters gave".to_string(),
        ));
    }
    Ok(wanted)
}

/// The groups named, in the order named, as the catalog holds them now. Reads before any plan begins, so a mistyped
/// handle never replaces a plan the user is looking at.
fn resolve(host: &McpHost, args: &PlanBlackHoleArgs) -> Result<Vec<(String, GroupSpec)>, ApiError> {
    let wanted = clean_handles(args)?;
    let (prefix, tool, what) = match args.source {
        BlackHoleSource::Duplicates => ("dup:", "find_duplicates", "duplicate group"),
        BlackHoleSource::RebuiltMasters => ("rebuilt:", "find_rebuilt_masters", "rebuilt-master group"),
    };
    let wrong: Vec<&String> = wanted.iter().filter(|h| !h.starts_with(prefix)).collect();
    if !wrong.is_empty() {
        let list = wrong.iter().map(|h| h.as_str()).collect::<Vec<_>>().join(", ");
        return Err(ApiError::Invalid(format!("{list}: not a {what} handle; name groups as {tool} gives them")));
    }
    let mut available: HashMap<String, GroupSpec> = HashMap::new();
    match args.source {
        BlackHoleSource::Duplicates => {
            // `get_duplicates` takes the context: no guard is held here.
            for g in crate::api::files::get_duplicates(host.ctx())? {
                let Some(key) = g.key else {
                    tracing::warn!(count = g.file_ids.len(), "duplicate group has no key and cannot be planned");
                    continue;
                };
                let (matched, byte_checked) = match key {
                    crate::db::DuplicateKey::Header => ("same header, size and name", true),
                    crate::db::DuplicateKey::Master => ("same full-file hash", false),
                    crate::db::DuplicateKey::Content => ("same content (a sampled hash)", true),
                };
                available.insert(
                    format!("dup:{}:{}", key.hash_type(), g.content_hash),
                    GroupSpec { matched: matched.to_string(), title: None, byte_checked, file_ids: g.file_ids.clone() },
                );
            }
        }
        BlackHoleSource::RebuiltMasters => {
            let found = {
                let conn = host.reader()?;
                find_rebuilt_master_copies(&conn, None)
            }
            .map_err(|e| read_failed("the rebuilt masters", format!("{e:#}")))?;
            for g in found {
                available.insert(
                    g.handle.clone(),
                    GroupSpec {
                        matched: "the same master built again".to_string(),
                        title: Some(g.params_label.clone()),
                        byte_checked: false,
                        file_ids: g.copies.iter().map(|c| c.file_id).collect(),
                    },
                );
            }
        }
    }
    let missing: Vec<&String> = wanted.iter().filter(|h| !available.contains_key(*h)).collect();
    if !missing.is_empty() {
        let list = missing.iter().map(|h| h.as_str()).collect::<Vec<_>>().join(", ");
        return Err(ApiError::Invalid(format!(
            "no {what} {list} in the catalog now (it may have changed, or it is not in a scan root that looks for duplicates); ask {tool} again"
        )));
    }
    Ok(wanted
        .into_iter()
        .filter_map(|h| available.remove(&h).map(|spec| (h, spec)))
        .collect())
}

/// Every copy of every group, from the host's reader; the guard is dropped before the caller touches the disk.
fn load_groups(host: &McpHost, specs: &[(String, GroupSpec)]) -> Result<Vec<Vec<CopyRow>>, ApiError> {
    let conn = host.reader()?;
    let mut out = Vec::with_capacity(specs.len());
    for (_, spec) in specs {
        let mut copies = Vec::with_capacity(spec.file_ids.len());
        for id in &spec.file_ids {
            if let Some(copy) = load_copy(&conn, *id)? {
                copies.push(copy);
            }
        }
        out.push(copies);
    }
    Ok(out)
}

// ---- the plan's content ----

struct ItemInfo {
    file_id: i64,
    name: String,
    folder: String,
    master: bool,
}

struct GroupInfo {
    byte_checked: bool,
    planned_chosen: Option<String>,
}

/// A copy to byte-check against its group's keeper.
struct Pair {
    group: usize,
    item: usize,
    item_file: i64,
    keeper_file: i64,
}

struct Assembled {
    groups: Vec<PlanGroup>,
    /// Per group, the notes that join into `PlanGroup::note`.
    group_notes: Vec<Vec<String>>,
    snapshots: HashMap<String, ItemSnapshot>,
    items: HashMap<String, ItemInfo>,
    group_infos: HashMap<String, GroupInfo>,
    pairs: Vec<Pair>,
    notes: Vec<String>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    count_of(n, one, many)
}

fn assemble(specs: &[(String, GroupSpec)], loaded: Vec<Vec<CopyRow>>, args: &PlanBlackHoleArgs) -> Assembled {
    let rules = KeepRules::new(args);
    let mut out = Assembled {
        groups: Vec::new(),
        group_notes: Vec::new(),
        snapshots: HashMap::new(),
        items: HashMap::new(),
        group_infos: HashMap::new(),
        pairs: Vec::new(),
        notes: Vec::new(),
    };
    let mut matched_entries: HashSet<String> = HashSet::new();
    // The header cache and the master cache are built apart, so one file can sit in two groups: it is planned once.
    let mut placed: HashSet<i64> = HashSet::new();
    for (gi, ((_, spec), mut copies)) in specs.iter().zip(loaded).enumerate() {
        let gid = format!("group:{}", gi + 1);
        let mut notes: Vec<String> = Vec::new();
        let before = copies.len();
        copies.retain(|c| {
            let first = placed.insert(c.file_id);
            if !first {
                notes.push(format!("{} in {} is already in another group of this plan and is left out here", c.name, c.folder));
            }
            first
        });
        if copies.len() < before {
            tracing::warn!(count = before - copies.len(), "plan left a file out of a later group");
        }
        let stamps: Vec<Result<DiskStamp, String>> =
            copies.iter().map(|c| stamp_for_plan(&stamp_target(&c.path, c.zip.as_deref()))).collect();
        for c in &copies {
            if rules.ids.contains(&c.file_id) {
                matched_entries.insert(format!("file:{}", c.file_id));
            }
            if rules.paths.contains(&c.path) {
                matched_entries.insert(c.path.clone());
            }
        }
        let chosen_idx = match rules.pick(&copies) {
            Ok(i) if stamps[i].is_ok() => Some(i),
            Ok(i) => {
                notes.push(format!(
                    "the copy that stays ({} in {}) is not reachable, so no copy is picked here",
                    copies[i].name, copies[i].folder
                ));
                None
            }
            Err(why) => {
                notes.push(why);
                None
            }
        };
        let chosen_id = chosen_idx.map(|i| format!("item:{}", copies[i].file_id));
        let evidence = if spec.byte_checked && chosen_idx.is_some() {
            format!("{}; bytes are compared with the copy that stays", spec.matched)
        } else {
            spec.matched.clone()
        };

        let mut items: Vec<PlanItem> = Vec::with_capacity(copies.len());
        for (i, c) in copies.iter().enumerate() {
            let id = format!("item:{}", c.file_id);
            let keeper = chosen_idx == Some(i);
            // The kept copy is shown by `chosen` alone: the window may keep another one, so it is not locked.
            let locked = if let Err(reason) = &stamps[i] {
                Some(reason.clone())
            } else if c.zip.is_some() {
                Some(IN_ZIP.to_string())
            } else if c.master && c.linked > 0 {
                Some(LINKED.to_string())
            } else if c.master && c.superseding {
                Some(SUPERSEDING.to_string())
            } else {
                None
            };
            if let Ok(stamp) = &stamps[i] {
                out.snapshots.insert(
                    id.clone(),
                    ItemSnapshot { file_id: c.file_id, frame_id: c.frame_id, stamp: stamp.clone(), catalog: BTreeMap::new() },
                );
            }
            if spec.byte_checked && !keeper && locked.is_none() {
                if let Some(k) = chosen_idx {
                    out.pairs.push(Pair { group: gi, item: items.len(), item_file: c.file_id, keeper_file: copies[k].file_id });
                }
            }
            if c.master && c.linked > 0 {
                notes.push(format!(
                    "{} in {}: {} at it, so it stays",
                    c.name,
                    c.folder,
                    plural(c.linked, "calibration link points", "calibration links point")
                ));
            } else if c.master && c.superseding {
                notes.push(format!("{} in {} superseded raw frames and stays", c.name, c.folder));
            }
            out.items.insert(
                id.clone(),
                ItemInfo { file_id: c.file_id, name: c.name.clone(), folder: c.folder.clone(), master: c.master },
            );
            items.push(PlanItem {
                id,
                file_name: c.name.clone(),
                folder: c.folder.clone(),
                values: c.values.clone(),
                action: MOVE_ACTION.to_string(),
                evidence: Some(evidence.clone()),
                selected: chosen_idx.is_some() && !keeper && locked.is_none(),
                locked,
                candidate: stamps[i].is_ok(),
                size_bytes: Some(c.size),
            });
        }
        let title = match (&spec.title, copies.first()) {
            (Some(t), _) => t.clone(),
            (None, Some(first)) => format!("{}: {}", first.name, plural(copies.len(), "copy", "copies")),
            (None, None) if before > 0 => "group whose copies are in another group of this plan".to_string(),
            (None, None) => "group with no copy left in the catalog".to_string(),
        };
        out.group_infos.insert(gid.clone(), GroupInfo { byte_checked: spec.byte_checked, planned_chosen: chosen_id.clone() });
        out.groups.push(PlanGroup {
            id: gid,
            title,
            note: None,
            choose_one: Some(KEEP_LABEL.to_string()),
            chosen: chosen_id,
            items,
        });
        out.group_notes.push(notes);
    }
    for entry in rules.ids.iter().map(|id| format!("file:{id}")).chain(rules.paths.iter().cloned()) {
        if !matched_entries.contains(&entry) {
            out.notes.push(format!("keepFiles names {entry}, which is not a copy in these groups"));
        }
    }
    out.notes.sort();
    out
}

/// Records what the byte checks found: a differing pair is locked and the group says so.
fn record_checks(out: &mut Assembled, results: &[(usize, Result<bool, ApiError>)]) {
    let mut differing: HashMap<usize, usize> = HashMap::new();
    let mut unreadable: HashMap<usize, usize> = HashMap::new();
    for (p, result) in results {
        let pair = &out.pairs[*p];
        let item = &mut out.groups[pair.group].items[pair.item];
        match result {
            Ok(true) => item.evidence = Some("byte-identical to the copy that stays (compared just now)".to_string()),
            Ok(false) => {
                item.locked = Some(DIFFERS.to_string());
                item.selected = false;
                item.evidence = Some("compared byte by byte with the copy that stays: the files are not the same".to_string());
                *differing.entry(pair.group).or_default() += 1;
            }
            Err(e) => {
                item.locked = Some("could not be compared with the copy that stays".to_string());
                item.selected = false;
                item.evidence = Some(format!("the byte comparison failed: {e}"));
                *unreadable.entry(pair.group).or_default() += 1;
            }
        }
    }
    let mut groups: Vec<usize> = differing.keys().chain(unreadable.keys()).copied().collect();
    groups.sort_unstable();
    groups.dedup();
    for g in groups {
        if let Some(n) = differing.get(&g) {
            out.group_notes[g].push(format!(
                "{} the same header but different bytes from the copy that stays; left out",
                plural(*n, "copy has", "copies have")
            ));
        }
        if let Some(n) = unreadable.get(&g) {
            out.group_notes[g]
                .push(format!("{} could not be compared with the copy that stays; left out", plural(*n, "copy", "copies")));
        }
    }
}

// ---- the planning tool ----

/// What happened to a plan that did not reach the user.
enum Stopped {
    /// The user cancelled it, or a newer plan replaced it, while it was being prepared.
    Ended,
}

fn title_of(source: BlackHoleSource) -> &'static str {
    match source {
        BlackHoleSource::Duplicates => "Move duplicate copies to the Black Hole",
        BlackHoleSource::RebuiltMasters => "Move rebuilt master copies to the Black Hole",
    }
}

/// `plan_black_hole`: reads the named groups, begins the plan and works out the keepers and locks. A plan with
/// copies to byte-check answers at once, `preparing`: the checks (progress per pair, the plan's cancel flag between
/// pairs) run on a thread of their own and publish the plan when they end. A plan with nothing to check is published
/// before the answer. Runs on the tool's blocking thread.
pub fn prepare(host: &Arc<McpHost>, args: PlanBlackHoleArgs, verify: VerifyFn) -> Result<Answer, ApiError> {
    let specs = resolve(host, &args)?;
    let title = title_of(args.source);
    let (id, cancel) = host.plans.begin(host, PlanKind::BlackHole, title);
    let out = guard(host, &id, || {
        let loaded = load_groups(host, &specs)?;
        Ok(assemble(&specs, loaded, &args))
    })?;
    let item_count: usize = out.groups.iter().map(|g| g.items.len()).sum();
    // A plan that is too large fails at its publish: do not byte-check what will not be shown.
    let pairs = if item_count > PLAN_ITEM_CAP { 0 } else { out.pairs.len() };
    if pairs == 0 {
        let published = guard(host, &id, || check_and_publish(host, &id, &cancel, out, title, &verify, 0))?;
        return match published {
            Ok(()) => ready_answer(host, &id),
            Err(Stopped::Ended) => Ok(ended_answer(host, &id)),
        };
    }

    // The answer reports the state the plan was in when the checks were handed over, in both halves.
    host.plans.progress(&id, 0, pairs);
    let worker_host = Arc::clone(host);
    let worker_id = id.clone();
    // A tracing dispatcher is per thread: carry the caller's so the checks' lines land with it.
    let dispatch = tracing::dispatcher::get_default(tracing::Dispatch::clone);
    // So is the tokio runtime: the plan's expiry timer is spawned on it when the worker publishes.
    let runtime = tokio::runtime::Handle::try_current().ok();
    let spawned = std::thread::Builder::new().name("mcp-plan-prepare".to_string()).spawn(move || {
        let _enter = runtime.as_ref().map(tokio::runtime::Handle::enter);
        tracing::dispatcher::with_default(&dispatch, || {
            // A failure or a panic ends the plan inside `guard`; there is nobody to return it to.
            let _ = guard(&worker_host, &worker_id, || {
                check_and_publish(&worker_host, &worker_id, &cancel, out, title, &verify, pairs)
            });
        });
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, plan_id = %id, "plan preparation thread could not start");
        let error = format!("the plan could not be prepared: {e}");
        host.plans.fail(&id, error.clone());
        return Err(ApiError::Internal(error));
    }
    Ok(preparing_answer(&id, pairs))
}

/// Runs one step of preparing a plan. An error or a panic ends the plan `failed` (a preparing plan is never swept,
/// so nothing else would end it) and is returned to the caller as an error.
fn guard<T>(host: &McpHost, id: &str, step: impl FnOnce() -> Result<T, ApiError>) -> Result<T, ApiError> {
    match std::panic::catch_unwind(AssertUnwindSafe(step)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => {
            host.plans.fail(id, e.to_string());
            Err(e)
        }
        Err(_) => {
            tracing::error!(plan_id = %id, "plan preparation panicked");
            let message = "preparing the plan stopped unexpectedly; ask for it again".to_string();
            host.plans.fail(id, message.clone());
            Err(ApiError::Internal(message))
        }
    }
}

/// Byte-checks the pairs (when there are any) and publishes the plan.
fn check_and_publish(
    host: &McpHost,
    id: &str,
    cancel: &AtomicBool,
    mut out: Assembled,
    title: &str,
    verify: &VerifyFn,
    pairs: usize,
) -> Result<Result<(), Stopped>, ApiError> {
    let mut results: Vec<(usize, Result<bool, ApiError>)> = Vec::with_capacity(pairs);
    for p in 0..pairs {
        if cancel.load(Ordering::SeqCst) {
            return Ok(Err(Stopped::Ended));
        }
        let (a, b) = (out.pairs[p].item_file, out.pairs[p].keeper_file);
        let result = verify(host, a, b);
        if let Err(e) = &result {
            tracing::warn!(error = %e, plan_id = id, "plan pair could not be compared");
        }
        results.push((p, result));
        host.plans.progress(id, p + 1, pairs);
    }
    if cancel.load(Ordering::SeqCst) {
        return Ok(Err(Stopped::Ended));
    }
    record_checks(&mut out, &results);
    for (g, notes) in out.groups.iter_mut().zip(&out.group_notes) {
        g.note = (!notes.is_empty()).then(|| notes.join("; "));
    }
    let applier = BlackHoleApplier { items: out.items, groups: out.group_infos, verify: Arc::clone(verify) };
    let content = PlanContent {
        title: title.to_string(),
        columns: plan_columns(),
        groups: out.groups,
        notes: out.notes,
        snapshots: out.snapshots,
        applier: Box::new(applier),
    };
    match host.plans.publish(id, content) {
        Ok(()) => Ok(Ok(())),
        Err(message) => match host.plans.view(Some(id)).map(|v| v.state) {
            Some(PlanState::Cancelled | PlanState::Replaced) => Ok(Err(Stopped::Ended)),
            _ => Err(ApiError::Invalid(message)),
        },
    }
}

fn state_json(state: &PlanState) -> Value {
    to_json(state).unwrap_or(Value::Null)
}

fn ended_answer(host: &McpHost, id: &str) -> Answer {
    let state = host.plans.view(Some(id)).map(|v| v.state);
    let words = match &state {
        Some(PlanState::Replaced) => "replaced by a newer plan",
        _ => "cancelled in Athenaeum's window",
    };
    Answer::new(
        format!("The plan was {words} while it was being prepared; nothing changed. Ask again if it is still wanted."),
        json!({ "planId": id, "state": state.as_ref().map_or(Value::Null, state_json) }),
    )
}

/// The answer while the byte checks run: the handle, `preparing`, and where to ask.
fn preparing_answer(id: &str, pairs: usize) -> Answer {
    let state = PlanState::Preparing { done: 0, total: pairs };
    Answer::new(
        format!(
            "Plan {id} is being prepared. Athenaeum is checking {} byte for byte; the plan opens in Athenaeum's window when it is ready. Ask plan_status.",
            plural(pairs, "copy", "copies")
        ),
        json!({ "planId": id, "kind": PlanKind::BlackHole.as_str(), "state": state_json(&state) }),
    )
}

fn ready_answer(host: &McpHost, id: &str) -> Result<Answer, ApiError> {
    let Some(view) = host.plans.view(Some(id)) else {
        return Err(ApiError::Internal("the plan is gone".to_string()));
    };
    Ok(answer_of(host, &view))
}

// ---- what the answer says about the plan ----

/// A plan that waits for the user, in words and as data. One function for the planning answer and for `plan_status`.
pub struct PlanDescription {
    /// `N files proposed for the Black Hole (size) from M groups`.
    pub summary: String,
    /// What follows the summary: counts that need attention, each group with its copies, the plan's notes.
    pub lines: Vec<String>,
    /// `proposedFiles`, `proposedBytes`, `lockedFiles`, `groupsWithoutKeeper`, `groups`, `otherGroups`, `notes`.
    pub data: Map<String, Value>,
}

fn count_where(view: &McpPlanView, f: impl Fn(&PlanItem) -> bool) -> usize {
    view.groups.iter().flat_map(|g| g.items.iter()).filter(|i| f(i)).count()
}

/// What happens to a copy, in words.
fn fate_of(item: &PlanItem) -> String {
    match (&item.locked, item.selected) {
        (Some(reason), _) => format!("locked: {reason}"),
        (None, true) => "moves".to_string(),
        (None, false) => "not selected".to_string(),
    }
}

fn item_json(item: &PlanItem) -> Value {
    json!({
        "fileName": item.file_name,
        "folder": item.folder,
        "values": item.values,
        "selected": item.selected,
        "locked": item.locked,
        "candidate": item.candidate,
        "evidence": item.evidence,
    })
}

struct Sums {
    files: usize,
    proposed: usize,
    locked: usize,
}

fn sums_of<'a>(items: impl Iterator<Item = &'a PlanItem>) -> Sums {
    items.fold(Sums { files: 0, proposed: 0, locked: 0 }, |a, i| Sums {
        files: a.files + 1,
        proposed: a.proposed + usize::from(i.selected),
        locked: a.locked + usize::from(i.locked.is_some()),
    })
}

fn sums_json(s: &Sums) -> Value {
    json!({ "files": s.files, "proposed": s.proposed, "locked": s.locked })
}

pub fn describe_plan(view: &McpPlanView) -> PlanDescription {
    let proposed = count_where(view, |i| i.selected);
    let bytes: i64 = view.groups.iter().flat_map(|g| g.items.iter()).filter(|i| i.selected).filter_map(|i| i.size_bytes).sum();
    let locked = count_where(view, |i| i.locked.is_some());
    let waiting = view.groups.iter().filter(|g| g.chosen.is_none()).count();
    let summary = format!(
        "{} proposed for the Black Hole ({}) from {}",
        plural(proposed, "file", "files"),
        bytes_text(bytes),
        plural(view.groups.len(), "group", "groups")
    );
    let mut lines: Vec<String> = Vec::new();
    if waiting > 0 {
        lines.push(format!(
            "{} no copy that stays picked yet: nothing is proposed there until the user picks one in the window.",
            plural(waiting, "group has", "groups have")
        ));
    }
    if locked > 0 {
        lines.push(format!("{} locked and will not move.", plural(locked, "file is", "files are")));
    }

    let mut groups: Vec<Value> = Vec::new();
    for g in view.groups.iter().take(GROUPS_LISTED) {
        let keeper = g.chosen.as_ref().and_then(|c| g.items.iter().find(|i| &i.id == c));
        let keeps = match keeper {
            Some(i) => format!("the copy that stays is {} in {}", i.file_name, i.folder),
            None => "no copy stays yet".to_string(),
        };
        let sums = sums_of(g.items.iter());
        let mut line = format!("- {}: {keeps}; {} proposed", g.title, sums.proposed);
        if let Some(note) = &g.note {
            line.push_str(&format!(" ({note})"));
        }
        lines.push(line);
        let others: Vec<&PlanItem> = g.items.iter().filter(|i| keeper.is_none_or(|k| k.id != i.id)).collect();
        for i in others.iter().take(ITEMS_LISTED) {
            lines.push(format!("    {} in {}: {}", i.file_name, i.folder, fate_of(i)));
        }
        if others.len() > ITEMS_LISTED {
            let rest = sums_of(others.iter().skip(ITEMS_LISTED).copied());
            lines.push(format!(
                "    and {} more ({} proposed, {} locked)",
                plural(rest.files, "copy", "copies"),
                rest.proposed,
                rest.locked
            ));
        }
        // The copy that stays leads the data list; then the others in plan order.
        let listed: Vec<&PlanItem> = keeper.into_iter().chain(others.iter().copied()).collect();
        let shown: Vec<Value> = listed.iter().take(ITEMS_LISTED + usize::from(keeper.is_some())).map(|i| item_json(i)).collect();
        let hidden = sums_of(listed.iter().skip(ITEMS_LISTED + usize::from(keeper.is_some())).copied());
        groups.push(json!({
            "title": g.title,
            "chosen": keeper.map(|i| json!({ "fileName": i.file_name, "folder": i.folder })),
            "proposed": sums.proposed,
            "locked": sums.locked,
            "note": g.note,
            "items": shown,
            "itemsNotShown": (hidden.files > 0).then(|| sums_json(&hidden)),
        }));
    }
    let beyond = sums_of(view.groups.iter().skip(GROUPS_LISTED).flat_map(|g| g.items.iter()));
    let other_groups = view.groups.len().saturating_sub(GROUPS_LISTED);
    if other_groups > 0 {
        lines.push(format!(
            "- and {} more ({}, {} proposed, {} locked)",
            plural(other_groups, "group", "groups"),
            plural(beyond.files, "copy", "copies"),
            beyond.proposed,
            beyond.locked
        ));
    }
    for n in &view.notes {
        lines.push(n.clone());
    }

    let mut data = Map::new();
    data.insert("proposedFiles".to_string(), json!(proposed));
    data.insert("proposedBytes".to_string(), json!(bytes));
    data.insert("lockedFiles".to_string(), json!(locked));
    data.insert("groupsWithoutKeeper".to_string(), json!(waiting));
    data.insert("groups".to_string(), Value::Array(groups));
    data.insert(
        "otherGroups".to_string(),
        if other_groups > 0 {
            json!({ "groups": other_groups, "files": beyond.files, "proposed": beyond.proposed, "locked": beyond.locked })
        } else {
            Value::Null
        },
    );
    data.insert("notes".to_string(), json!(view.notes));
    PlanDescription { summary, lines, data }
}

fn answer_of(host: &McpHost, view: &McpPlanView) -> Answer {
    let described = describe_plan(view);
    let mut lines = vec![format!("Plan {} is ready: {}.", view.plan_id, described.summary)];
    lines.extend(described.lines);
    lines.push(format!("{}.", plan_opened_text(host)));
    match &view.expires_at {
        Some(t) => lines.push(format!("It waits for the user until {t}. Nothing moves until they press Apply there.")),
        None => lines.push("Nothing moves until the user presses Apply there.".to_string()),
    }
    lines.push("Ask plan_status for the outcome.".to_string());
    let mut data = Map::new();
    data.insert("planId".to_string(), json!(view.plan_id));
    data.insert("kind".to_string(), json!(view.kind.as_str()));
    data.insert("state".to_string(), state_json(&view.state));
    data.insert("expiresAt".to_string(), json!(view.expires_at));
    data.extend(described.data);
    Answer::new(lines.join("\n"), Value::Object(data))
}

// ---- the applier ----

/// What the plan remembers to apply: its items' files, which are masters, and which copy each group planned to keep.
pub struct BlackHoleApplier {
    items: HashMap<String, ItemInfo>,
    groups: HashMap<String, GroupInfo>,
    verify: VerifyFn,
}

fn report_line(items: &HashMap<String, ItemInfo>, item_id: &str, reason: impl Into<String>) -> ReportLine {
    let (file_name, folder) =
        items.get(item_id).map(|i| (i.name.clone(), i.folder.clone())).unwrap_or_default();
    ReportLine { item_id: item_id.to_string(), file_name, folder, reason: reason.into() }
}

/// Whether a file is in the Black Hole itself or inside a zip that is.
fn in_black_hole(conn: &Connection, file_id: i64) -> Result<bool, rusqlite::Error> {
    let sql = format!("SELECT {}", file_not_in_black_hole_sql("?1"));
    let not_in: i64 = conn.query_row(&sql, params![file_id], |r| r.get(0))?;
    Ok(not_in == 0)
}

/// The included items of one group still to move, with the keeper they stay beside.
struct Work<'a> {
    group_id: &'a str,
    chosen: &'a str,
    keeper_file: i64,
    ids: Vec<&'a str>,
}

impl PlanApplier for BlackHoleApplier {
    fn validate(&self, view: &McpPlanView, sel: &PlanSelection) -> Result<(), String> {
        // Every group's kept copy, whatever group the selection names it in: none of them is ever moved.
        let kept: HashSet<&str> = sel.groups.iter().filter_map(|g| g.chosen.as_deref()).collect();
        for g in &sel.groups {
            if g.included.is_empty() {
                continue;
            }
            let title = view.groups.iter().find(|x| x.id == g.group_id).map_or("this group", |x| x.title.as_str());
            match &g.chosen {
                None => {
                    return Err(format!("pick the copy that stays in \"{title}\" before moving its other copies"));
                }
                Some(c) if g.included.contains(c) => {
                    return Err(format!("the copy that stays in \"{title}\" cannot also be moved"));
                }
                Some(_) => {}
            }
            if let Some(id) = g.included.iter().find(|id| kept.contains(id.as_str())) {
                let name = view
                    .groups
                    .iter()
                    .flat_map(|x| x.items.iter())
                    .find(|i| &i.id == id)
                    .map_or_else(|| id.clone(), |i| format!("{} in {}", i.file_name, i.folder));
                return Err(format!("{name} is the copy that stays in another group and cannot also be moved"));
            }
        }
        Ok(())
    }

    fn apply(&self, host: &McpHost, plan_id: &str, sel: &PlanSelection, fresh: &[String]) -> PlanReport {
        let mut report = PlanReport::default();
        let fresh: HashSet<&str> = fresh.iter().map(String::as_str).collect();
        let mut work: Vec<Work> = Vec::new();
        for g in &sel.groups {
            let ids: Vec<&str> = g.included.iter().map(String::as_str).filter(|id| fresh.contains(id)).collect();
            if ids.is_empty() {
                continue;
            }
            let keeper = g.chosen.as_deref().and_then(|c| self.items.get(c).map(|i| (c, i.file_id)));
            match keeper {
                Some((chosen, keeper_file)) => work.push(Work { group_id: &g.group_id, chosen, keeper_file, ids }),
                None => {
                    for id in ids {
                        report.skipped.push(report_line(&self.items, id, "no copy that stays was chosen for its group"));
                    }
                }
            }
        }

        // The slow step first, on no catalog connection: a byte-checked group whose keeper is not the one the plan
        // checked against compares each copy with it now. It can take minutes.
        let mut verdicts: HashMap<&str, Result<bool, String>> = HashMap::new();
        for w in &work {
            let info = self.groups.get(w.group_id);
            if !info.is_some_and(|g| g.byte_checked && g.planned_chosen.as_deref() != Some(w.chosen)) {
                continue;
            }
            for id in &w.ids {
                let Some(item) = self.items.get(*id) else { continue };
                let verdict = (self.verify)(host, item.file_id, w.keeper_file).map_err(|e| {
                    tracing::warn!(error = %e, plan_id, "plan pair could not be compared");
                    e.to_string()
                });
                verdicts.insert(id, verdict);
            }
        }

        // Then the re-checks, immediately before the move, on a pooled connection of its own that is released before
        // anything that takes the context.
        let mut to_move: Vec<&str> = Vec::new();
        {
            let database = match crate::api::db(host.ctx()) {
                Ok(d) => d,
                Err(e) => return self.skip_all(report, &work, &format!("the catalog is not available: {e}")),
            };
            let conn = match database.try_conn() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, plan_id, "plan apply could not reach the catalog");
                    return self.skip_all(report, &work, &format!("the catalog is busy: {e}"));
                }
            };
            for w in &work {
                match in_black_hole(&conn, w.keeper_file) {
                    Ok(true) => {
                        for id in &w.ids {
                            report.skipped.push(report_line(
                                &self.items,
                                id,
                                "the copy that stays is in the Black Hole now, so nothing of this group moves",
                            ));
                        }
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, plan_id, "plan could not re-check the copy that stays");
                        for id in &w.ids {
                            report.skipped.push(report_line(&self.items, id, format!("the copy that stays could not be re-checked: {e}")));
                        }
                        continue;
                    }
                }
                for id in &w.ids {
                    if let Some(reason) = self.recheck_item(&conn, plan_id, id) {
                        report.skipped.push(report_line(&self.items, id, reason));
                        continue;
                    }
                    match verdicts.get(id) {
                        None | Some(Ok(true)) => to_move.push(id),
                        Some(Ok(false)) => report.skipped.push(report_line(&self.items, id, DIFFERS)),
                        Some(Err(e)) => report.skipped.push(report_line(
                            &self.items,
                            id,
                            format!("could not be compared with the copy that stays: {e}"),
                        )),
                    }
                }
            }
        }
        if to_move.is_empty() {
            return report;
        }

        let file_ids: Vec<i64> = to_move.iter().filter_map(|id| self.items.get(*id).map(|i| i.file_id)).collect();
        match crate::api::black_hole::bulk_move_to_black_hole(host.ctx(), &file_ids, FROM_WHERE, None, host.emitter().as_ref()) {
            Ok(result) => {
                let failed: HashMap<i64, &str> = result.failed.iter().map(|(id, why)| (*id, why.as_str())).collect();
                for id in &to_move {
                    let file_id = self.items.get(*id).map_or(0, |i| i.file_id);
                    match failed.get(&file_id) {
                        Some(why) => report.failed.push(report_line(&self.items, id, *why)),
                        None => report.applied.push(report_line(&self.items, id, "moved to the Black Hole")),
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, plan_id, "plan black hole move failed");
                for id in &to_move {
                    report.failed.push(report_line(&self.items, id, e.to_string()));
                }
            }
        }
        report
    }
}

impl BlackHoleApplier {
    fn skip_all(&self, mut report: PlanReport, work: &[Work], reason: &str) -> PlanReport {
        for w in work {
            for id in &w.ids {
                report.skipped.push(report_line(&self.items, id, reason));
            }
        }
        report
    }

    /// Why an item must not move now, from the catalog as it is: the Black Hole already holds it, or (a master) links
    /// pointed at it or its registration superseded raw frames since the plan.
    fn recheck_item(&self, conn: &Connection, plan_id: &str, item_id: &str) -> Option<String> {
        let item = self.items.get(item_id)?;
        match in_black_hole(conn, item.file_id) {
            Ok(true) => return Some("already in the Black Hole".to_string()),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, plan_id, "plan could not re-check an item");
                return Some(format!("could not be re-checked: {e}"));
            }
        }
        if !item.master {
            return None;
        }
        match master_link_count(conn, item.file_id) {
            Ok(0) => {}
            Ok(_) => return Some(LINKED.to_string()),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), plan_id, "plan could not re-read a master's links");
                return Some(format!("its calibration links could not be re-read: {e:#}"));
            }
        }
        match supersedes_raw_frames(conn, item.file_id) {
            Ok(false) => None,
            Ok(true) => Some(SUPERSEDING.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, plan_id, "plan could not re-read a master's set");
                Some(format!("its calibration set could not be re-read: {e}"))
            }
        }
    }
}

#[cfg(test)]
#[path = "black_hole_tests.rs"]
pub(crate) mod tests;
