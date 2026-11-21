//! The metadata plan (MCP plan Task 17, §6.3): an assistant asks Athenaeum to fill the object, camera, date or frame
//! type that frames in a scope miss. The values come from evidence (`mcp::infer`: the folder, the file name, agreeing
//! frames of the same folder) or from the user's own word (`statement`). Athenaeum prepares a plan; only the user
//! applies it, in Athenaeum's window.
//!
//! One group per (field, value), its items selected (a date is per (field, folder): every frame has its own time); a frame that misses a field and has no evidence for it is a
//! locked item in a group of its own, with the reason. The apply is the Missing Metadata page's own write
//! (`api::files::bulk_update_frame_metadata`: catalog only, `frames.override = 1`, so a re-scan keeps the value, and
//! the links the cascade severs are the ones the page's confirm warns about; the plan's notes say how many).
//!
//! Everything the planner needs is read before the plan begins, so a request that cannot be planned (a scope that
//! names nothing, nothing to propose, too many items) is refused in words and never replaces the plan the user is
//! looking at. No reader guard and no pooled connection is held across a call that takes the context.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use rmcp::schemars::JsonSchema;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::black_hole::{guard, PlanDescription};
use super::{
    catalog_values, plan_opened_text, stamp_for_plan, stamp_target, DiskStamp, ItemSnapshot, McpPlanView,
    FactTone, PlanApplier, PlanColumn, PlanContent, PlanGroup, PlanItem, PlanKind, PlanReport, PlanSelection,
    PlanState, PlanSummary, ReportLine, SummaryCount, SummaryFact, PLAN_ITEM_CAP,
};
use crate::api::ApiError;
use crate::catalog_query::{fetch_scoped_frames, GROUPS_SHOWN};
use crate::db::{scan_root_prefix_predicate, FRAME_NOT_IN_BLACK_HOLE_SQL};
use crate::mcp::answer::count_of;
use crate::mcp::infer::{by_instruction, infer, normalize_statement, FillField, NoProposal, Proposal, UserStatement};
use crate::mcp::tools::{prepare as prepare_scope, to_json, Prep};
use crate::mcp::{Answer, McpHost};
use crate::models::{FrameMetadataEdits, ImageType};
use crate::scope::{Scope, ScopeFilter};

/// The window's event a write to frames' metadata raises (the Missing Metadata view reloads on it).
pub const METADATA_CHANGED: &str = "frames-metadata-changed";
const TITLE: &str = "Fill missing metadata";
/// The window's one sentence for what a fill changes (the note carries the same fact at more length).
const CATALOG_ONLY_EXPLAIN: &str =
    "Only the catalog changes. Files on disk are not rewritten, and a later scan keeps these values.";
/// A plan answer lists at most this many groups with their items (the rest is summed) ...
const GROUPS_LISTED: usize = GROUPS_SHOWN;
/// ... and at most this many items of a group.
const ITEMS_LISTED: usize = 20;
/// Folders asked for in one statement when the frames beside a frame are read.
const FOLDERS_PER_READ: usize = 200;

/// Arguments of `plan_metadata_fill`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct PlanMetadataArgs {
    /// Which frames. Only frames of the scope that miss a field are proposed for. Empty: the whole catalog.
    #[serde(default)]
    pub scope: Scope,
    /// What to fill from evidence: object, camera, date, frame_type. Empty: all four (or only the statement's field
    /// when a statement is given).
    #[serde(default)]
    pub fields: Vec<FillField>,
    /// What the user said, for example "these are M 31": one field and its value for the frames in the scope that
    /// miss it. The plan says "by your instruction". Do not guess a value: ask the user and pass their word.
    #[serde(default)]
    pub statement: Option<UserStatement>,
}

/// The catalog values a plan reads for a field and re-reads at apply: the field's own, and for the object the
/// frame type too (only a light has one to fill).
fn keys_of(field: FillField) -> &'static [&'static str] {
    match field {
        FillField::Object => &["OBJECT", "IMAGETYP"],
        FillField::Camera => &["INSTRUME"],
        FillField::Date => &["DATE-OBS"],
        FillField::FrameType => &["IMAGETYP"],
    }
}

fn read_failed(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "plan could not read the catalog");
    ApiError::Internal(format!("could not read {what}: {e}"))
}

// ---- reading and inferring, before the plan begins ----

/// Where a frame's file is.
struct FrameRef {
    file_id: i64,
    file_name: String,
    folder: String,
    path: String,
    zip: Option<String>,
}

struct Read {
    refs: HashMap<i64, FrameRef>,
    proposals: Vec<Proposal>,
    none: Vec<NoProposal>,
    /// The catalog values each item depends on, as they are now.
    snapshots: HashMap<(i64, FillField), BTreeMap<String, Option<String>>>,
}

/// The frames of the folders in `folders` that have a camera or an object set, read for their own folder only.
fn read_neighbours(conn: &Connection, folders: &BTreeSet<String>) -> Result<Vec<crate::catalog_query::ScopedFrame>, ApiError> {
    let wanted: Vec<String> = folders.iter().cloned().collect();
    let mut out = Vec::new();
    for chunk in wanted.chunks(FOLDERS_PER_READ) {
        let (under, params) = scan_root_prefix_predicate("fl.path", chunk);
        let filter = ScopeFilter {
            sql: format!(
                "fr.id IN (SELECT f.id FROM frames f JOIN files fl ON fl.id = f.file_id WHERE ({under}) \
                 AND {FRAME_NOT_IN_BLACK_HOLE_SQL} \
                 AND ((f.instrume IS NOT NULL AND f.instrume <> '') OR (f.object IS NOT NULL AND f.object <> '')))"
            ),
            params,
        };
        let frames = fetch_scoped_frames(conn, &filter).map_err(|e| read_failed("the frames beside them", format!("{e:#}")))?;
        // The predicate takes the folder and everything below it; a neighbour is in the folder itself.
        out.extend(frames.into_iter().filter(|sf| folders.contains(&sf.folder)));
    }
    Ok(out)
}

fn read_plan(
    host: &McpHost,
    scope: &Scope,
    inferred: &[FillField],
    statement: Option<&(FillField, String)>,
) -> Result<Read, ApiError> {
    let conn = host.reader()?;
    let prepared = match prepare_scope(&conn, scope)? {
        // A scope that names nothing the catalog holds: the sentence says what it does hold, and no plan begins.
        Prep::Answered(answer) => {
            return Err(ApiError::Invalid(format!("{}; nothing was planned", answer.text.trim_end_matches('.'))));
        }
        Prep::Ready(p) => p,
    };
    let frames = fetch_scoped_frames(&conn, &prepared.filter).map_err(|e| read_failed("the frames in scope", format!("{e:#}")))?;

    let folders: BTreeSet<String> = frames
        .iter()
        .filter(|sf| inferred.iter().any(|f| matches!(f, FillField::Object | FillField::Camera) && f.is_missing(sf)))
        .map(|sf| sf.folder.clone())
        .collect();
    let neighbours = if folders.is_empty() { Vec::new() } else { read_neighbours(&conn, &folders)? };

    let (mut proposals, mut none) = infer(&frames, &neighbours, inferred);
    if let Some((field, value)) = statement {
        proposals.extend(by_instruction(&frames, *field, value));
    }
    let items = proposals.len() + none.len();
    if items > PLAN_ITEM_CAP {
        return Err(ApiError::Invalid(format!(
            "this plan would hold {items} items, and a plan holds at most 2 000; narrow the scope (a folder, a night, a frame type) or name fewer fields, and ask again"
        )));
    }

    // The catalog as it is now, in the text the apply re-reads it in. A frame whose stored value is not empty is not
    // missing it by the Missing Metadata view's own test (a type the parser does not know, a date it cannot read):
    // it is left alone.
    let mut snapshots: HashMap<(i64, FillField), BTreeMap<String, Option<String>>> = HashMap::new();
    let mut still_missing = |frame_id: i64, field: FillField| -> Result<bool, ApiError> {
        if snapshots.contains_key(&(frame_id, field)) {
            return Ok(true);
        }
        let values = catalog_values(&conn, frame_id, keys_of(field)).map_err(|e| read_failed("a frame", e))?;
        if values.get(field.header_key()).is_some_and(Option::is_some) {
            tracing::debug!(frame_id, "plan leaves a frame whose value is not empty");
            return Ok(false);
        }
        snapshots.insert((frame_id, field), values);
        Ok(true)
    };
    let mut kept_proposals = Vec::with_capacity(proposals.len());
    for p in proposals {
        if still_missing(p.frame_id, p.field)? {
            kept_proposals.push(p);
        }
    }
    let mut kept_none = Vec::with_capacity(none.len());
    for n in none.drain(..) {
        if still_missing(n.frame_id, n.field)? {
            kept_none.push(n);
        }
    }
    let (proposals, none) = (kept_proposals, kept_none);

    if proposals.is_empty() {
        return Err(ApiError::Invalid(nothing_to_fill(&prepared.resolved.description, inferred, statement, &none)));
    }

    let wanted: HashSet<i64> = proposals.iter().map(|p| p.frame_id).chain(none.iter().map(|n| n.frame_id)).collect();
    let refs = frames
        .iter()
        .filter_map(|sf| {
            let id = sf.file.frame.as_ref().and_then(|f| f.id)?;
            wanted.contains(&id).then(|| {
                (
                    id,
                    FrameRef {
                        file_id: sf.file.file.id.unwrap_or_default(),
                        file_name: sf.file.file.filename.clone(),
                        folder: sf.folder.clone(),
                        path: sf.file.file.path.clone(),
                        zip: sf.file.file.archive_zip_path.clone(),
                    },
                )
            })
        })
        .collect();
    Ok(Read { refs, proposals, none, snapshots })
}

/// Why there is nothing to propose, so the assistant knows what to ask the user.
fn nothing_to_fill(
    description: &str,
    inferred: &[FillField],
    statement: Option<&(FillField, String)>,
    none: &[NoProposal],
) -> String {
    let mut fields: Vec<FillField> = inferred.to_vec();
    fields.extend(statement.map(|s| s.0));
    fields.sort();
    fields.dedup();
    let mut parts: Vec<String> = Vec::new();
    for field in &fields {
        let these: Vec<&NoProposal> = none.iter().filter(|n| n.field == *field).collect();
        if let Some(first) = these.first() {
            parts.push(format!(
                "{} missing {} with no evidence for a value (for example: {})",
                count_of(these.len(), "frame", "frames"),
                field.header_key(),
                first.why
            ));
        }
    }
    if parts.is_empty() {
        let keys: Vec<&str> = fields.iter().map(|f| f.header_key()).collect();
        return format!("no frame in the scope ({description}) misses {}; nothing was planned", keys.join(", "));
    }
    format!(
        "nothing in the scope ({description}) can be filled from evidence: {}. If the user knows the value, pass it as the statement (field and value) and ask again; nothing was planned",
        parts.join("; ")
    )
}

// ---- the plan ----

fn plan_columns() -> Vec<PlanColumn> {
    let col = |key: &str, label: &str| PlanColumn { key: key.to_string(), label: label.to_string() };
    vec![col("field", "Field"), col("current", "Now"), col("proposed", "Proposed")]
}

/// What the plan remembers to apply.
struct ItemInfo {
    frame_id: i64,
    field: FillField,
    value: String,
    file_name: String,
    folder: String,
}

/// A value as a person reads it: a date is `YYYY-MM-DD HH:MM:SS` (the proposal keeps its `T`).
fn shown(field: FillField, value: &str) -> String {
    if field == FillField::Date { value.replacen('T', " ", 1) } else { value.to_string() }
}

fn item_id(frame_id: i64, field: FillField) -> String {
    format!("item:{frame_id}:{}", field.as_str())
}

/// The links the changes sever, as the metadata editor's confirm counts them.
enum Relations {
    /// Nothing is severed.
    None,
    Counted { sessions: usize, links: usize },
    NotCounted,
}

impl Relations {
    /// The note that says how many links the changes sever.
    fn note(&self) -> Option<String> {
        match self {
            Relations::None => None,
            Relations::Counted { sessions, links } => Some(format!(
                "these changes take {} out of their sessions and {links} out of calibration links",
                count_of(*sessions, "frame", "frames"),
            )),
            Relations::NotCounted => Some("the links these changes would remove could not be counted".to_string()),
        }
    }

    /// The same values as the summary's warnings (a zero is left out) and, when the count failed, a fact.
    fn add_to(&self, summary: &mut PlanSummary) {
        match self {
            Relations::None => {}
            Relations::Counted { sessions, links } => {
                if *sessions > 0 {
                    summary.warnings.push(SummaryCount::new(*sessions, "frame leaves its session", "frames leave their sessions"));
                }
                if *links > 0 {
                    summary.warnings.push(SummaryCount::new(*links, "calibration link removed", "calibration links removed"));
                }
            }
            Relations::NotCounted => summary.facts.push(SummaryFact::new("Links removed", "not counted", FactTone::Warn)),
        }
    }
}

fn relations_of(host: &McpHost, frame_ids: &[i64]) -> Relations {
    if frame_ids.is_empty() {
        return Relations::None;
    }
    match crate::api::files::count_frame_metadata_relations(host.ctx(), frame_ids.to_vec()) {
        Ok(r) if r.affected_frame_count > 0 => Relations::Counted {
            sessions: usize::try_from(r.session_member_count).unwrap_or(0),
            links: usize::try_from(r.calibration_set_member_count + r.calibration_set_consumer_count).unwrap_or(0),
        },
        Ok(_) => Relations::None,
        Err(e) => {
            tracing::warn!(error = %e, "plan could not count the links its changes would remove");
            Relations::NotCounted
        }
    }
}

enum Stopped {
    /// The user cancelled it, or a newer plan replaced it, while it was being prepared.
    Ended,
}

/// Stamps the files, builds the groups and publishes the plan.
fn build_and_publish(host: &McpHost, id: &str, read: Read) -> Result<Result<(), Stopped>, ApiError> {
    let Read { refs, proposals, none, mut snapshots } = read;
    let frame_ref = |frame_id: i64| refs.get(&frame_id).ok_or_else(|| ApiError::Internal("a planned frame is gone".to_string()));

    // One stamp per file: a frame with two fields is one file.
    let mut stamps: HashMap<i64, Result<DiskStamp, String>> = HashMap::new();
    for p in &proposals {
        let r = frame_ref(p.frame_id)?;
        stamps.entry(r.file_id).or_insert_with(|| stamp_for_plan(&stamp_target(&r.path, r.zip.as_deref())));
    }
    let reachable: BTreeSet<i64> = proposals
        .iter()
        .filter_map(|p| {
            let r = refs.get(&p.frame_id)?;
            stamps.get(&r.file_id)?.is_ok().then_some(p.frame_id)
        })
        .collect();
    let reachable: Vec<i64> = reachable.into_iter().collect();
    let relations = relations_of(host, &reachable);

    let mut groups: Vec<PlanGroup> = Vec::new();
    let mut plan_snapshots: HashMap<String, ItemSnapshot> = HashMap::new();
    let mut infos: HashMap<String, ItemInfo> = HashMap::new();

    // One group per (field, value), the largest first within a field. A date is the exception: every frame has its
    // own time, so its group is the folder, and each item keeps its own value.
    let mut by_group: BTreeMap<(FillField, String), Vec<&Proposal>> = BTreeMap::new();
    for p in &proposals {
        let key = if p.field == FillField::Date { frame_ref(p.frame_id)?.folder.clone() } else { p.value.clone() };
        by_group.entry((p.field, key)).or_default().push(p);
    }
    let mut ordered: Vec<((FillField, String), Vec<&Proposal>)> = by_group.into_iter().collect();
    ordered.sort_by(|a, b| a.0 .0.cmp(&b.0 .0).then_with(|| b.1.len().cmp(&a.1.len())).then_with(|| a.0 .1.cmp(&b.0 .1)));
    for ((field, key), members) in ordered {
        let title = if field == FillField::Date {
            let first = members.iter().map(|p| p.value.as_str()).min().unwrap_or_default();
            let last = members.iter().map(|p| p.value.as_str()).max().unwrap_or_default();
            let span = if first == last {
                shown(field, first)
            } else {
                format!("{} to {}", shown(field, first), shown(field, last))
            };
            format!("{}: {key} ({span})", field.label())
        } else {
            format!("{}: {key}", field.label())
        };
        let mut items = Vec::with_capacity(members.len());
        for p in members {
            let value = &p.value;
            let r = frame_ref(p.frame_id)?;
            let iid = item_id(p.frame_id, field);
            let lock = match stamps.get(&r.file_id) {
                Some(Ok(stamp)) => {
                    plan_snapshots.insert(
                        iid.clone(),
                        ItemSnapshot {
                            file_id: r.file_id,
                            frame_id: Some(p.frame_id),
                            stamp: stamp.clone(),
                            catalog: snapshots.remove(&(p.frame_id, field)).unwrap_or_default(),
                        },
                    );
                    None
                }
                Some(Err(reason)) => Some(reason.clone()),
                None => Some(super::NOT_REACHABLE.to_string()),
            };
            infos.insert(
                iid.clone(),
                ItemInfo { frame_id: p.frame_id, field, value: value.clone(), file_name: r.file_name.clone(), folder: r.folder.clone() },
            );
            let value = shown(field, value);
            items.push(PlanItem {
                id: iid,
                file_name: r.file_name.clone(),
                folder: r.folder.clone(),
                values: BTreeMap::from([
                    ("field".to_string(), field.header_key().to_string()),
                    ("current".to_string(), "(empty)".to_string()),
                    ("proposed".to_string(), value.clone()),
                ]),
                action: format!("Set {} to '{value}'", field.header_key()),
                evidence: Some(p.evidence.clone()),
                selected: lock.is_none(),
                locked: lock,
                candidate: false,
                size_bytes: None,
            });
        }
        groups.push(PlanGroup {
            id: format!("group:{}", groups.len() + 1),
            title,
            note: None,
            choose_one: None,
            chosen: None,
            items,
        });
    }

    // The frames nothing can be said for, one locked group per field.
    let mut by_field: BTreeMap<FillField, Vec<&NoProposal>> = BTreeMap::new();
    for n in &none {
        by_field.entry(n.field).or_default().push(n);
    }
    for (field, members) in by_field {
        let mut items = Vec::with_capacity(members.len());
        for n in members {
            let r = frame_ref(n.frame_id)?;
            items.push(PlanItem {
                id: item_id(n.frame_id, field),
                file_name: r.file_name.clone(),
                folder: r.folder.clone(),
                values: BTreeMap::from([
                    ("field".to_string(), field.header_key().to_string()),
                    ("current".to_string(), "(empty)".to_string()),
                    ("proposed".to_string(), "(none)".to_string()),
                ]),
                action: format!("Leave {} empty", field.header_key()),
                evidence: None,
                selected: false,
                locked: Some(n.why.clone()),
                candidate: false,
                size_bytes: None,
            });
        }
        groups.push(PlanGroup {
            id: format!("group:{}", groups.len() + 1),
            title: format!("{}: nothing proposed", field.label()),
            note: Some("no value could be found for these frames".to_string()),
            choose_one: None,
            chosen: None,
            items,
        });
    }

    let mut notes: Vec<String> = Vec::new();
    notes.extend(relations.note());
    let mut summary = PlanSummary { explain: Some(CATALOG_ONLY_EXPLAIN.to_string()), ..PlanSummary::default() };
    let left_empty = none.iter().map(|n| n.frame_id).collect::<BTreeSet<i64>>().len();
    if left_empty > 0 {
        summary.left_out.push(SummaryCount::new(left_empty, "frame with no value found", "frames with no value found"));
    }
    relations.add_to(&mut summary);
    notes.push(
        "Only the catalog changes: the files on disk are not rewritten, and a frame keeps the value when Athenaeum scans its file again."
            .to_string(),
    );
    let content = PlanContent {
        title: TITLE.to_string(),
        columns: plan_columns(),
        groups,
        notes,
        summary,
        snapshots: plan_snapshots,
        applier: Box::new(MetadataApplier { items: infos }),
    };
    match host.plans.publish(id, content) {
        Ok(()) => Ok(Ok(())),
        Err(message) => match host.plans.view(Some(id)).map(|v| v.state) {
            Some(PlanState::Cancelled | PlanState::Replaced) => Ok(Err(Stopped::Ended)),
            _ => Err(ApiError::Invalid(message)),
        },
    }
}

/// `plan_metadata_fill`: reads and infers, begins the plan, stamps the files and publishes it before it answers.
/// Runs on the tool's blocking thread.
pub fn prepare(host: &Arc<McpHost>, args: PlanMetadataArgs) -> Result<Answer, ApiError> {
    let statement = match &args.statement {
        Some(s) => Some((s.field, normalize_statement(s.field, &s.value).map_err(ApiError::Invalid)?)),
        None => None,
    };
    let inferred: Vec<FillField> = if args.fields.is_empty() {
        if statement.is_some() { Vec::new() } else { FillField::ALL.to_vec() }
    } else {
        args.fields.iter().copied().filter(|f| statement.as_ref().is_none_or(|s| s.0 != *f)).collect()
    };
    let read = read_plan(host, &args.scope, &inferred, statement.as_ref())?;

    // From here the plan exists: a failure ends it `failed` (a preparing plan is never swept).
    let (id, _cancel) = host.plans.begin(host, PlanKind::Metadata, TITLE);
    let published = guard(host, &id, || build_and_publish(host, &id, read))?;
    match published {
        Ok(()) => {
            let Some(view) = host.plans.view(Some(&id)) else {
                return Err(ApiError::Internal("the plan is gone".to_string()));
            };
            Ok(answer_of(host, &view))
        }
        Err(Stopped::Ended) => Ok(ended_answer(host, &id)),
    }
}

fn ended_answer(host: &McpHost, id: &str) -> Answer {
    let state = host.plans.view(Some(id)).map(|v| v.state);
    let words = match &state {
        Some(PlanState::Replaced) => "replaced by a newer plan",
        _ => "cancelled in Athenaeum's window",
    };
    Answer::new(
        format!("The plan was {words} while it was being prepared; nothing changed. Ask again if it is still wanted."),
        json!({ "planId": id, "state": state.as_ref().map_or(Value::Null, |s| to_json(s).unwrap_or(Value::Null)) }),
    )
}

// ---- what the answer says about the plan ----

fn item_json(item: &PlanItem) -> Value {
    json!({
        "fileName": item.file_name,
        "folder": item.folder,
        "values": item.values,
        "selected": item.selected,
        "locked": item.locked,
        "evidence": item.evidence,
    })
}

fn fate_of(item: &PlanItem) -> String {
    match (&item.locked, &item.evidence) {
        (Some(reason), _) => format!("locked: {reason}"),
        (None, Some(evidence)) => evidence.clone(),
        (None, None) => "not selected".to_string(),
    }
}

/// A group of locked frames nothing could be said for.
fn is_nothing_group(g: &PlanGroup) -> bool {
    g.items.iter().all(|i| i.evidence.is_none())
}

/// What a group's title counts: its frames, or, when some are locked, the frames that can be filled now (a group of
/// the frames nothing could be said for is not counted as one that can be filled).
fn group_count(g: &PlanGroup) -> String {
    let fillable = g.items.iter().filter(|i| i.locked.is_none()).count();
    if fillable == g.items.len() || is_nothing_group(g) {
        count_of(g.items.len(), "frame", "frames")
    } else {
        format!("{fillable} of {} can be filled now", count_of(g.items.len(), "frame", "frames"))
    }
}

/// A plan that waits for the user, in words and as data: one function for the planning answer and for `plan_status`.
pub fn describe_plan(view: &McpPlanView) -> PlanDescription {
    let all = || view.groups.iter().flat_map(|g| g.items.iter());
    let proposed = all().filter(|i| i.selected).count();
    let locked = all().filter(|i| i.locked.is_some()).count();
    let value_groups = view.groups.iter().filter(|g| !is_nothing_group(g)).count();
    let summary = format!(
        "{} proposed in {}",
        count_of(proposed, "value", "values"),
        count_of(value_groups, "group", "groups")
    );
    let mut lines: Vec<String> = Vec::new();
    if locked > 0 {
        lines.push(format!(
            "{} locked and will not change (the reason is on each).",
            count_of(locked, "item is", "items are")
        ));
    }
    let mut groups: Vec<Value> = Vec::new();
    for g in view.groups.iter().take(GROUPS_LISTED) {
        let selected = g.items.iter().filter(|i| i.selected).count();
        let locked_here = g.items.iter().filter(|i| i.locked.is_some()).count();
        lines.push(format!("- {}: {}", g.title, group_count(g)));
        for i in g.items.iter().take(ITEMS_LISTED) {
            lines.push(format!("    {} in {}: {}", i.file_name, i.folder, fate_of(i)));
        }
        let hidden: Vec<&PlanItem> = g.items.iter().skip(ITEMS_LISTED).collect();
        if !hidden.is_empty() {
            lines.push(format!(
                "    and {} more ({} proposed, {} locked)",
                count_of(hidden.len(), "frame", "frames"),
                hidden.iter().filter(|i| i.selected).count(),
                hidden.iter().filter(|i| i.locked.is_some()).count()
            ));
        }
        let first = g.items.first();
        // The one value of the group; a group of dates holds one per frame, which its items carry.
        let common = first
            .and_then(|f| f.values.get("proposed"))
            .filter(|v| g.items.iter().all(|i| i.values.get("proposed") == Some(*v)));
        groups.push(json!({
            "title": g.title,
            "field": first.and_then(|i| i.values.get("field")),
            "value": if is_nothing_group(g) { None } else { common },
            "proposed": selected,
            "locked": locked_here,
            "note": g.note,
            "items": g.items.iter().take(ITEMS_LISTED).map(item_json).collect::<Vec<_>>(),
            "itemsNotShown": (!hidden.is_empty()).then(|| json!({
                "frames": hidden.len(),
                "proposed": hidden.iter().filter(|i| i.selected).count(),
                "locked": hidden.iter().filter(|i| i.locked.is_some()).count(),
            })),
        }));
    }
    let beyond: Vec<&PlanItem> = view.groups.iter().skip(GROUPS_LISTED).flat_map(|g| g.items.iter()).collect();
    let other_groups = view.groups.len().saturating_sub(GROUPS_LISTED);
    if other_groups > 0 {
        lines.push(format!(
            "- and {} more ({}, {} proposed, {} locked)",
            count_of(other_groups, "group", "groups"),
            count_of(beyond.len(), "frame", "frames"),
            beyond.iter().filter(|i| i.selected).count(),
            beyond.iter().filter(|i| i.locked.is_some()).count()
        ));
    }
    for n in &view.notes {
        lines.push(n.clone());
    }
    let mut data = Map::new();
    data.insert("proposedValues".to_string(), json!(proposed));
    data.insert("lockedItems".to_string(), json!(locked));
    data.insert("groups".to_string(), Value::Array(groups));
    data.insert(
        "otherGroups".to_string(),
        if other_groups > 0 {
            json!({
                "groups": other_groups,
                "frames": beyond.len(),
                "proposed": beyond.iter().filter(|i| i.selected).count(),
                "locked": beyond.iter().filter(|i| i.locked.is_some()).count(),
            })
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
        Some(t) => lines.push(format!("It waits for the user until {t}. Nothing changes until they press Apply there.")),
        None => lines.push("Nothing changes until the user presses Apply there.".to_string()),
    }
    lines.push("Ask plan_status for the outcome.".to_string());
    let mut data = Map::new();
    data.insert("planId".to_string(), json!(view.plan_id));
    data.insert("kind".to_string(), json!(view.kind.as_str()));
    data.insert("state".to_string(), to_json(&view.state).unwrap_or(Value::Null));
    data.insert("expiresAt".to_string(), json!(view.expires_at));
    data.extend(described.data);
    Answer::new(lines.join("\n"), Value::Object(data))
}

// ---- the applier ----

/// What the plan remembers to apply: each item's frame, field and value.
pub struct MetadataApplier {
    items: HashMap<String, ItemInfo>,
}

fn report_line(items: &HashMap<String, ItemInfo>, item_id: &str, reason: impl Into<String>) -> ReportLine {
    let (file_name, folder) = items.get(item_id).map(|i| (i.file_name.clone(), i.folder.clone())).unwrap_or_default();
    ReportLine { item_id: item_id.to_string(), file_name, folder, reason: reason.into() }
}

/// The edit a value stands for. A date is stored as the metadata editor stores one: UTC, with its zone.
fn edits_of(field: FillField, value: &str) -> FrameMetadataEdits {
    let mut edits = FrameMetadataEdits::default();
    match field {
        FillField::Object => edits.object = Some(Some(value.to_string())),
        FillField::Camera => edits.instrume = Some(Some(value.to_string())),
        FillField::Date => edits.date_obs = Some(Some(format!("{value}Z"))),
        FillField::FrameType => edits.imagetyp = Some(Some(value.to_string())),
    }
    edits
}

/// Why the frame must not be filled now, from the catalog as it is: the field has a value, or (the object) the
/// frame is no longer a light.
fn no_longer_missing(conn: &Connection, info: &ItemInfo) -> Option<String> {
    match catalog_values(conn, info.frame_id, keys_of(info.field)) {
        Err(e) => Some(e),
        Ok(now) => {
            if let Some(Some(v)) = now.get(info.field.header_key()) {
                return Some(format!("it has {} now: '{v}'", info.field.header_key()));
            }
            if info.field == FillField::Object {
                let light = now
                    .get("IMAGETYP")
                    .and_then(|t| t.as_deref())
                    .and_then(ImageType::from_str)
                    .is_some_and(|k| k == ImageType::Light);
                if !light {
                    return Some("it is no longer a light frame".to_string());
                }
            }
            None
        }
    }
}

/// Why a frame the write did not reach is reported failed.
const NOT_IN_CATALOG: &str = "not in the catalog any more";

/// After a write that reached `updated` of `frame_ids`: the frames that are no longer in the catalog. A frame that
/// cannot be looked up is counted as not written too (it is not claimed applied).
fn frames_gone(host: &McpHost, plan_id: &str, frame_ids: &[i64], updated: usize) -> BTreeSet<i64> {
    tracing::warn!(plan_id, count = updated, "plan wrote fewer rows than it named");
    let all = || frame_ids.iter().copied().collect::<BTreeSet<i64>>();
    let database = match crate::api::db(host.ctx()) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, plan_id, "plan could not reopen the catalog to find the frames it missed");
            return all();
        }
    };
    let conn = match database.try_conn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, plan_id, "plan could not reach the catalog to find the frames it missed");
            return all();
        }
    };
    let mut gone = BTreeSet::new();
    for id in frame_ids {
        match conn.query_row("SELECT 1 FROM frames WHERE id = ?1", [id], |_| Ok(())).optional() {
            Ok(Some(())) => {}
            Ok(None) => {
                gone.insert(*id);
            }
            Err(e) => {
                tracing::error!(error = %e, plan_id, frame_id = *id, "plan could not look a frame up after a short write");
                gone.insert(*id);
            }
        }
    }
    gone
}

impl PlanApplier for MetadataApplier {
    /// Every item is an independent include/exclude: the generic check is all a selection needs.
    fn validate(&self, _view: &McpPlanView, _sel: &PlanSelection) -> Result<(), String> {
        Ok(())
    }

    fn apply(&self, host: &McpHost, plan_id: &str, sel: &PlanSelection, fresh: &[String]) -> PlanReport {
        let mut report = PlanReport::default();
        let fresh: HashSet<&str> = fresh.iter().map(String::as_str).collect();
        let included: Vec<&str> =
            sel.groups.iter().flat_map(|g| g.included.iter().map(String::as_str)).filter(|id| fresh.contains(id)).collect();

        // The re-check, immediately before the write, on a pooled connection of its own that is released before the
        // write takes the context.
        let mut ready: Vec<&str> = Vec::new();
        {
            let database = match crate::api::db(host.ctx()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(error = %e, plan_id, "plan apply could not open the catalog");
                    return self.skip_all(report, &included, &format!("the catalog is not available: {e}"));
                }
            };
            let conn = match database.try_conn() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, plan_id, "plan apply could not reach the catalog");
                    return self.skip_all(report, &included, &format!("the catalog is busy: {e}"));
                }
            };
            for id in included {
                match self.items.get(id) {
                    None => report.skipped.push(report_line(&self.items, id, "it was not recorded at planning")),
                    Some(info) => match no_longer_missing(&conn, info) {
                        Some(reason) => report.skipped.push(report_line(&self.items, id, reason)),
                        None => ready.push(id),
                    },
                }
            }
        }

        // Equal edits are one write.
        let mut by_edit: BTreeMap<(FillField, &str), Vec<&str>> = BTreeMap::new();
        for id in ready {
            if let Some(info) = self.items.get(id) {
                by_edit.entry((info.field, info.value.as_str())).or_default().push(id);
            }
        }
        let mut changed: BTreeSet<i64> = BTreeSet::new();
        for ((field, value), ids) in by_edit {
            let frame_ids: Vec<i64> = ids.iter().filter_map(|id| self.items.get(*id).map(|i| i.frame_id)).collect();
            match crate::api::files::bulk_update_frame_metadata(host.ctx(), frame_ids.clone(), edits_of(field, value)) {
                Ok(updated) => {
                    // A write that reached fewer rows than it named is not reported applied: the frames it did not
                    // reach are found again in the catalog and reported failed.
                    let missed: BTreeSet<i64> =
                        if updated < frame_ids.len() { frames_gone(host, plan_id, &frame_ids, updated) } else { BTreeSet::new() };
                    for id in ids {
                        let line = match self.items.get(id) {
                            Some(info) if missed.contains(&info.frame_id) => {
                                report.failed.push(report_line(&self.items, id, NOT_IN_CATALOG));
                                continue;
                            }
                            _ => report_line(&self.items, id, format!("{} set to '{}'", field.header_key(), shown(field, value))),
                        };
                        report.applied.push(line);
                    }
                    changed.extend(frame_ids.iter().filter(|f| !missed.contains(f)));
                }
                Err(e) => {
                    tracing::error!(error = %e, plan_id, "plan metadata write failed");
                    for id in ids {
                        report.failed.push(report_line(&self.items, id, e.to_string()));
                    }
                }
            }
        }
        if !changed.is_empty() {
            tracing::info!(via = "mcp", plan_id, count = changed.len(), "mcp metadata fill applied");
            host.emitter().emit_json(METADATA_CHANGED, json!({ "count": changed.len() }));
        }
        report
    }
}

impl MetadataApplier {
    fn skip_all(&self, mut report: PlanReport, ids: &[&str], reason: &str) -> PlanReport {
        for id in ids {
            report.skipped.push(report_line(&self.items, id, reason));
        }
        report
    }
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
pub(crate) mod tests;
