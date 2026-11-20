//! One filter over the catalog: the `Scope` every MCP question and plan is asked under (MCP plan, Task 1).
//!
//! A [`Scope`] is what the assistant says ("the Ha lights of IC 5146 in 2024"); [`resolve_scope`] turns it into one
//! SQL predicate on `fr.id` plus the words that say what it means ([`describe_scope`]). A value that names nothing
//! the catalog holds (a camera, a filter, a frame set...) is never matched as a substring: the scope resolves to no
//! filter and carries a [`NearestHint`] naming what does exist, so a typo answers "no camera 'ASI2600'; the catalog
//! has ..." instead of an empty list that reads as "no frames".
//!
//! Black Hole files are left out unless the scope's `state` asks for them (MCP plan R2).

pub mod nearest;
pub mod night;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::{
    frame_is_used_copy_sql, normalize_separators, scan_root_prefix_predicate,
    FRAME_NOT_IN_BLACK_HOLE_SQL,
};
use crate::models::ImageType;

/// Where a frame's file is, from the catalog's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars")]
pub enum FrameState {
    /// A loose file in a scanned folder, its frame set not archived.
    Catalog,
    /// A loose file whose frame set is marked archived (not zipped yet).
    SoftArchived,
    /// An entry inside a ZIP archive.
    Zipped,
    /// In the Black Hole (rejected, or inside a zip that is).
    BlackHole,
}

impl FrameState {
    /// What a scope without a `state` covers: everything but the Black Hole.
    pub const DEFAULT: [FrameState; 3] = [FrameState::Catalog, FrameState::SoftArchived, FrameState::Zipped];
    pub(crate) const ALL: [FrameState; 4] =
        [FrameState::Catalog, FrameState::SoftArchived, FrameState::Zipped, FrameState::BlackHole];

    fn label(self) -> &'static str {
        match self {
            FrameState::Catalog => "catalog",
            FrameState::SoftArchived => "soft-archived",
            FrameState::Zipped => "zipped",
            FrameState::BlackHole => "Black Hole",
        }
    }
}

/// The common filter of every question and plan. Every field narrows; an absent field does not. Text values
/// match a value the catalog holds, ignoring case and runs of spaces, never a part of one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct Scope {
    /// Object name: the OBJECT header value, or the name of a frame set the frame belongs to.
    pub object: Option<String>,
    /// Frame set name.
    pub frame_set: Option<String>,
    /// A folder; includes its subfolders. A full path as the catalog spells it.
    pub folder: Option<String>,
    /// A scan root (a folder Athenaeum watches), by its full path.
    pub scan_root: Option<String>,
    /// Camera name (INSTRUME).
    pub camera: Option<String>,
    /// Telescope name (TELESCOP).
    pub telescope: Option<String>,
    /// Filter name.
    pub filter: Option<String>,
    /// light, dark, flat, bias, darkflat or a master of one (`master dark`); plural is fine (`lights`).
    pub frame_type: Option<String>,
    /// Exposure in seconds; matches within 0.005 s.
    pub exposure_s: Option<f64>,
    /// One night, `YYYY-MM-DD`: the UTC date of the observation time minus 12 hours.
    pub night: Option<String>,
    /// First night of a range, `YYYY-MM-DD`, inclusive.
    pub date_from: Option<String>,
    /// Last night of a range, `YYYY-MM-DD`, inclusive.
    pub date_to: Option<String>,
    /// A calendar year of nights, e.g. 2024.
    pub year: Option<i32>,
    /// Which files: default is catalog, soft_archived and zipped; ask for `black_hole` to include rejected frames.
    pub state: Option<Vec<FrameState>>,
}

/// One predicate on `fr.id`. Inside it `f` is `frames` and `fl` is `files`; a caller's query only sees `fr.id`.
#[derive(Debug, Clone)]
pub struct ScopeFilter {
    pub sql: String,
    pub params: Vec<Value>,
}

/// A value that named nothing the catalog holds, and what it holds that is near.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearestHint {
    /// `camera`, `telescope`, `filter`, `object`, `frame set`, `frame type` or `scan root`.
    pub condition: &'static str,
    pub given: String,
    pub existing: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedScope {
    /// `None` = nothing matches (a hint says why).
    pub filter: Option<ScopeFilter>,
    pub description: String,
    pub hints: Vec<NearestHint>,
    /// Black Hole files are not in the answer.
    pub black_hole_left_out: bool,
}

/// Lowercase, runs of whitespace as one space, trimmed (the Objects page's `normalize`).
pub fn normalize_value(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The one clean value of an optional text field: trimmed, empty is absent.
fn given(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// `frames f` is a member of a frame set matching `extra` (SQL on `fs`); `used_copy` limits it to the sets that
/// use this copy of the raw frame.
fn member_of_set_sql(used_copy: bool, extra: &str) -> String {
    let used = if used_copy { format!(" AND {}", frame_is_used_copy_sql("fs.id")) } else { String::new() };
    format!(
        "EXISTS (SELECT 1 FROM session_members sm \
             JOIN sessions ss ON ss.id = sm.session_id \
             JOIN imaging_nights sn ON sn.id = ss.imaging_night_id \
             JOIN frames_set fs ON fs.id = sn.frames_set_id \
             WHERE sm.frame_id = f.id{used} AND {extra})"
    )
}

/// The SQL test for one [`FrameState`], inside the filter's `f` (frames) / `fl` (files) aliases. The four
/// partition the catalog: every frame is in exactly one.
pub fn frame_state_sql(state: FrameState) -> String {
    let not_bh = FRAME_NOT_IN_BLACK_HOLE_SQL;
    // The set's page counts its used copy of a raw frame only, so the state does too.
    let in_archived_set = member_of_set_sql(true, "fs.is_archived = 1");
    match state {
        FrameState::BlackHole => format!("(NOT {not_bh})"),
        FrameState::Zipped => format!("(fl.archive_zip_path IS NOT NULL AND {not_bh})"),
        FrameState::SoftArchived => {
            format!("(fl.archive_zip_path IS NULL AND {not_bh} AND {in_archived_set})")
        }
        FrameState::Catalog => {
            format!("(fl.archive_zip_path IS NULL AND {not_bh} AND NOT {in_archived_set})")
        }
    }
}

/// The states a scope covers, in `FrameState::ALL` order, without repeats.
fn effective_states(scope: &Scope) -> Vec<FrameState> {
    match scope.state.as_deref() {
        Some(s) if !s.is_empty() => FrameState::ALL.into_iter().filter(|c| s.contains(c)).collect(),
        _ => FrameState::DEFAULT.to_vec(),
    }
}

/// The nights range after folding `year` into `date_from` / `date_to`.
fn night_range(scope: &Scope) -> (Option<String>, Option<String>) {
    let mut from = given(&scope.date_from).map(str::to_string);
    let mut to = given(&scope.date_to).map(str::to_string);
    if let Some(y) = scope.year {
        let (ys, ye) = (format!("{y:04}-01-01"), format!("{y:04}-12-31"));
        from = Some(from.map_or(ys.clone(), |f| f.max(ys)));
        to = Some(to.map_or(ye.clone(), |t| t.min(ye)));
    }
    (from, to)
}

/// `"A"`, `"A and B"`, `"A, B and C"`, each in single quotes.
fn quoted_list(items: &[String]) -> String {
    let q: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    match q.split_last() {
        None => String::new(),
        Some((only, [])) => only.clone(),
        Some((last, rest)) => format!("{} and {}", rest.join(", "), last),
    }
}

/// The hints as one sentence each, joined with `. `:
/// `no camera 'ASI2600'; the catalog has 'ZWO ASI2600MC Duo' and 'ZWO ASI2600MM Pro'`.
pub fn hints_text(hints: &[NearestHint]) -> String {
    hints
        .iter()
        .map(|h| {
            if h.existing.is_empty() {
                format!("no {} '{}'; nothing like it in the catalog", h.condition, h.given)
            } else {
                format!("no {} '{}'; the catalog has {}", h.condition, h.given, quoted_list(&h.existing))
            }
        })
        .collect::<Vec<_>>()
        .join(". ")
}

/// A scope in words, for the first line of an answer.
pub fn describe_scope(scope: &Scope) -> String {
    let mut parts: Vec<String> = Vec::new();
    for v in [&scope.object, &scope.frame_set, &scope.camera, &scope.telescope, &scope.filter] {
        if let Some(v) = given(v) {
            parts.push(v.to_string());
        }
    }
    if let Some(e) = scope.exposure_s {
        parts.push(format!("{e} s"));
    }
    if let Some(n) = given(&scope.night) {
        parts.push(format!("night {n}"));
    }
    match night_range(scope) {
        (Some(f), Some(t)) => parts.push(format!("nights {f} … {t}")),
        (Some(f), None) => parts.push(format!("nights from {f}")),
        (None, Some(t)) => parts.push(format!("nights up to {t}")),
        (None, None) => {}
    }
    if let Some(t) = given(&scope.frame_type) {
        let t = t.to_lowercase();
        parts.push(if t.ends_with("biases") || (t.ends_with('s') && !t.ends_with("bias")) {
            t
        } else if t.ends_with("bias") {
            format!("{t}es")
        } else {
            format!("{t}s")
        });
    }
    if let Some(f) = given(&scope.folder) {
        parts.push(format!("in {f}"));
    }
    if let Some(r) = given(&scope.scan_root) {
        parts.push(format!("under scan root {r}"));
    }
    let states = effective_states(scope);
    if states.len() == FrameState::ALL.len() {
        parts.push("including the Black Hole".to_string());
    } else if states != FrameState::DEFAULT {
        let names: Vec<&str> = states.iter().map(|s| s.label()).collect();
        parts.push(format!("state {}", names.join(", ")));
    }
    if parts.is_empty() {
        "the whole catalog".to_string()
    } else {
        parts.join(", ")
    }
}

/// The distinct non-empty values a query returns.
fn distinct_values(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql).with_context(|| format!("prepare distinct values: {sql}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().with_context(|| format!("read distinct values: {sql}"))
}

/// The distinct values that equal `given` after normalisation (never a substring), sorted.
fn equal_values(distinct: &[String], given: &str) -> Vec<String> {
    let g = normalize_value(given);
    let mut hits: Vec<String> = distinct.iter().filter(|v| normalize_value(v) == g).cloned().collect();
    hits.sort();
    hits.dedup();
    hits
}

fn in_list(column: &str, values: &[String]) -> (String, Vec<Value>) {
    let marks = vec!["?"; values.len()].join(",");
    (format!("{column} IN ({marks})"), values.iter().cloned().map(Value::Text).collect())
}

/// What one condition adds: SQL with its parameters, or nothing (a hint was pushed instead).
type Cond = (String, Vec<Value>);

/// The raw kind a master kind is made from (`MasterDark` -> `Dark`); a raw kind is its own.
fn raw_kind(kind: &ImageType) -> ImageType {
    match kind {
        ImageType::MasterLight => ImageType::Light,
        ImageType::MasterDark => ImageType::Dark,
        ImageType::MasterFlat => ImageType::Flat,
        ImageType::MasterBias => ImageType::Bias,
        ImageType::MasterDarkFlat => ImageType::DarkFlat,
        raw => raw.clone(),
    }
}

/// A frame type, decided by the catalog's own signals. The scanner stores a master it found by file name as the
/// raw `imagetyp` with `is_master = 1`, so a raw kind excludes the flagged rows and a master kind takes both the
/// master `imagetyp` and the flagged raw one. The spellings are the ones `insert_frame` writes (`{:?}`).
fn frame_type_cond(kind: &ImageType) -> Cond {
    if kind.is_master() {
        (
            "(f.imagetyp = ? OR (f.imagetyp = ? AND f.is_master = 1))".to_string(),
            vec![Value::Text(format!("{kind:?}")), Value::Text(format!("{:?}", raw_kind(kind)))],
        )
    } else {
        ("f.imagetyp = ? AND COALESCE(f.is_master, 0) = 0".to_string(), vec![Value::Text(format!("{kind:?}"))])
    }
}

/// `label` must be a date `YYYY-MM-DD` that exists on the calendar.
fn check_date(label: &str, value: Option<&str>) -> Result<()> {
    let Some(v) = value else { return Ok(()) };
    let shaped = v.len() == 10
        && v.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    if shaped && chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok() {
        return Ok(());
    }
    // The refusal is the answer; the command boundary that asked logs it.
    Err(anyhow::anyhow!("{label} must be a date YYYY-MM-DD, got '{v}'"))
}

/// Resolve a [`Scope`] against the catalog `conn` reads. Reads the distinct values of the text columns a scope
/// names; the filter it returns reads nothing else.
pub fn resolve_scope(conn: &Connection, scope: &Scope) -> Result<ResolvedScope> {
    check_date("night", given(&scope.night))?;
    check_date("date from", given(&scope.date_from))?;
    check_date("date to", given(&scope.date_to))?;

    let mut conds: Vec<Cond> = Vec::new();
    let mut hints: Vec<NearestHint> = Vec::new();

    // Text columns of `frames`: camera, telescope, filter.
    for (condition, value, col) in [
        ("camera", &scope.camera, "instrume"),
        ("telescope", &scope.telescope, "telescop"),
        ("filter", &scope.filter, "filter"),
    ] {
        let Some(g) = given(value) else { continue };
        let distinct = distinct_values(
            conn,
            &format!("SELECT DISTINCT {col} FROM frames WHERE {col} IS NOT NULL AND {col} <> ''"),
        )?;
        let hits = equal_values(&distinct, g);
        if hits.is_empty() {
            hints.push(NearestHint { condition, given: g.to_string(), existing: nearest::nearest(&distinct, g) });
        } else {
            conds.push(in_list(&format!("f.{col}"), &hits));
        }
    }

    let set_names = || {
        distinct_values(conn, "SELECT DISTINCT name FROM frames_set WHERE name IS NOT NULL AND name <> ''")
    };
    if let Some(g) = given(&scope.object) {
        let objects = distinct_values(
            conn,
            "SELECT DISTINCT object FROM frames WHERE object IS NOT NULL AND object <> ''",
        )?;
        let sets = set_names()?;
        let (by_object, by_set) = (equal_values(&objects, g), equal_values(&sets, g));
        if by_object.is_empty() && by_set.is_empty() {
            let mut all = objects;
            all.extend(sets);
            all.sort();
            all.dedup();
            hints.push(NearestHint { condition: "object", given: g.to_string(), existing: nearest::nearest(&all, g) });
        } else {
            let mut branches: Vec<String> = Vec::new();
            let mut params: Vec<Value> = Vec::new();
            if !by_object.is_empty() {
                let (sql, p) = in_list("f.object", &by_object);
                branches.push(sql);
                params.extend(p);
            }
            if !by_set.is_empty() {
                let (sql, p) = in_list("fs.name", &by_set);
                branches.push(member_of_set_sql(true, &sql));
                params.extend(p);
            }
            conds.push((format!("({})", branches.join(" OR ")), params));
        }
    }
    if let Some(g) = given(&scope.frame_set) {
        let sets = set_names()?;
        let hits = equal_values(&sets, g);
        if hits.is_empty() {
            hints.push(NearestHint { condition: "frame set", given: g.to_string(), existing: nearest::nearest(&sets, g) });
        } else {
            let (sql, p) = in_list("fs.name", &hits);
            conds.push((member_of_set_sql(true, &sql), p));
        }
    }

    if let Some(g) = given(&scope.frame_type) {
        let lower = g.to_lowercase();
        let kind = [Some(lower.as_str()), lower.strip_suffix('s'), lower.strip_suffix("es")]
            .into_iter()
            .flatten()
            .find_map(ImageType::from_str);
        match kind {
            Some(k) => conds.push(frame_type_cond(&k)),
            None => {
                let distinct = distinct_values(
                    conn,
                    "SELECT DISTINCT imagetyp FROM frames WHERE imagetyp IS NOT NULL AND imagetyp <> ''",
                )?;
                hints.push(NearestHint {
                    condition: "frame type",
                    given: g.to_string(),
                    existing: nearest::nearest(&distinct, g),
                });
            }
        }
    }

    if let Some(e) = scope.exposure_s {
        conds.push(("ABS(f.exptime - ?) < 0.005".to_string(), vec![Value::Real(e)]));
    }

    let night_col = night::night_sql("f.date_obs");
    if let Some(n) = given(&scope.night) {
        conds.push((format!("{night_col} = ?"), vec![Value::Text(n.to_string())]));
    }
    match night_range(scope) {
        (Some(f), Some(t)) => {
            conds.push((format!("{night_col} BETWEEN ? AND ?"), vec![Value::Text(f), Value::Text(t)]))
        }
        (Some(f), None) => conds.push((format!("{night_col} >= ?"), vec![Value::Text(f)])),
        (None, Some(t)) => conds.push((format!("{night_col} <= ?"), vec![Value::Text(t)])),
        (None, None) => {}
    }

    if let Some(folder) = given(&scope.folder) {
        let (sql, p) = scan_root_prefix_predicate("fl.path", &[normalize_separators(folder)]);
        conds.push((format!("({sql})"), p));
    }
    if let Some(g) = given(&scope.scan_root) {
        let roots = distinct_values(conn, "SELECT path FROM scan_roots")?;
        let trim = |s: &str| s.trim_end_matches(['/', '\\']).to_string();
        // Equal as stored, after separators only: a path is never case-folded.
        let wanted = trim(&normalize_separators(g));
        let hits: Vec<String> =
            roots.iter().filter(|r| trim(&normalize_separators(r)) == wanted).cloned().collect();
        if hits.is_empty() {
            hints.push(NearestHint { condition: "scan root", given: g.to_string(), existing: nearest::nearest(&roots, g) });
        } else {
            let (sql, p) = scan_root_prefix_predicate("fl.path", &hits);
            conds.push((format!("({sql})"), p));
        }
    }

    let states = effective_states(scope);
    if states.len() < FrameState::ALL.len() {
        let ors: Vec<String> = states.iter().map(|s| frame_state_sql(*s)).collect();
        conds.push((format!("({})", ors.join(" OR ")), Vec::new()));
    }

    let description = describe_scope(scope);
    let black_hole_left_out = !states.contains(&FrameState::BlackHole);
    if !hints.is_empty() {
        return Ok(ResolvedScope { filter: None, description, hints, black_hole_left_out });
    }
    let (clauses, params): (Vec<String>, Vec<Vec<Value>>) = conds.into_iter().unzip();
    let where_sql = if clauses.is_empty() { "1".to_string() } else { clauses.join(" AND ") };
    let sql = format!(
        "fr.id IN (SELECT f.id FROM frames f JOIN files fl ON fl.id = f.file_id WHERE {where_sql})"
    );
    Ok(ResolvedScope {
        filter: Some(ScopeFilter { sql, params: params.into_iter().flatten().collect() }),
        description,
        hints,
        black_hole_left_out,
    })
}
