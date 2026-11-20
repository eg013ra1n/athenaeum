//! The read-only catalog questions (MCP plan, Task 5): `catalog_overview`, `query_frames`, `file_details`,
//! `find_duplicates` and `find_missing_metadata`. Each reads through the host's one read-only connection (R11) and
//! the Task 2-4 functions of `catalog_query`; none writes, none opens a path it was given.
//!
//! The text half names files by file name and folder, never by handle; the structured half carries the same figures
//! with every handle beside its `fileName` and `folder`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::{prepare, run, to_json as json, FileArgs, MissingArgs, Prep, QueryFramesArgs, ScopeArgs};
use crate::api::ApiError;
use crate::catalog_query::{
    self as cq, CalibrationLine, FileLine, FrameGroup, FrameSetTotals, MissingCategory, QualityMetric, QualitySummary,
    ScopedFrame, StorageSummary, Totals, FRAME_SETS_PAGE, GROUPS_SHOWN,
};
use crate::mcp::answer::{bytes_text, count_of};
use crate::mcp::{Answer, AthenaeumMcp, McpHost};
use crate::scope::{normalize_value, FrameState, Scope};

/// Folders the text half lists in a storage summary (the structured half carries up to 50).
const STORAGE_FOLDERS_IN_TEXT: usize = 10;
/// Groups listed with their example files when the answer has this few groups.
const GROUPS_WITH_EXAMPLES: usize = 10;
/// Header keywords the text half of `file_details` lists (the structured half carries all).
const HEADER_KEYWORDS_IN_TEXT: usize = 40;

// ---- the adapters ----

#[tool_router(router = catalog_tools, vis = "pub(crate)")]
impl AthenaeumMcp {
    #[tool(
        description = "A first look at the whole catalog: frame counts by frame type with integration time, how many objects, cameras, telescopes and filters it holds, the first and last night, where the files live, duplicate groups and files missing metadata. Black Hole files are left out and counted apart.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn catalog_overview(&self) -> CallToolResult {
        run(&self.host, move |host| catalog_overview(host)).await
    }

    #[tool(
        description = "Count, group and list frames under one scope (object, frame set, folder, scan root, camera, telescope, filter, frame type, exposure, night, date range, year, state). Answers totals and integration time, optionally grouped (groupBy), with a page of files (filesPage), a stored-measurement summary (quality), which light groups have darks, flats and bias linked (calibration) and where the files live (storage). A scope that is exactly one frame set answers the figures of that frame set's page. Black Hole files are left out unless the scope's state asks. A value the catalog does not hold is answered with the nearest ones it does.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn query_frames(&self, Parameters(args): Parameters<QueryFramesArgs>) -> CallToolResult {
        run(&self.host, move |host| query_frames(host, args)).await
    }

    #[tool(
        description = "Everything the catalog keeps about one file: header values, stored measurements and whether they are current, linked calibration, its copies, whether a plate solve is stored. The file is a `file:<number>` handle an earlier answer gave, or its path as the catalog stores it; the path is looked up, never opened.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn file_details(&self, Parameters(args): Parameters<FileArgs>) -> CallToolResult {
        run(&self.host, move |host| file_details(host, args)).await
    }

    #[tool(
        description = "Duplicate groups with a copy inside the scope, as the Duplicates page groups them: what matched, how big one copy is, and every copy with its file name, folder and scan root. 50 groups to a page.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn find_duplicates(&self, Parameters(args): Parameters<ScopeArgs>) -> CallToolResult {
        run(&self.host, move |host| find_duplicates(host, args)).await
    }

    #[tool(
        description = "Frames in the scope that lack coordinates, object, observation time, camera or frame type, as the Missing Metadata view finds them: counts per category, the folders holding them and one page of files.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn find_missing_metadata(&self, Parameters(args): Parameters<MissingArgs>) -> CallToolResult {
        run(&self.host, move |host| find_missing_metadata(host, args)).await
    }
}

// ---- words ----

fn seconds(e: f64) -> String {
    format!("{} s", (e * 1000.0).round() / 1000.0)
}

/// `M31_001.fits in /data/night1 (Light, M31, filter Ha, 300 s, night 2024-10-05, zipped)`.
fn file_text(l: &FileLine) -> String {
    let mut facts: Vec<String> = Vec::new();
    facts.extend(l.imagetyp.clone());
    facts.extend(l.object.clone());
    if let Some(f) = &l.filter {
        facts.push(format!("filter {f}"));
    }
    if let Some(e) = l.exptime_s {
        facts.push(seconds(e));
    }
    if let Some(n) = &l.night {
        facts.push(format!("night {n}"));
    }
    match l.state {
        FrameState::Catalog => {}
        FrameState::SoftArchived => facts.push("soft-archived".to_string()),
        FrameState::Zipped => facts.push("zipped".to_string()),
        FrameState::BlackHole => facts.push("in the Black Hole".to_string()),
    }
    if facts.is_empty() {
        format!("{} in {}", l.file_name, l.folder)
    } else {
        format!("{} in {} ({})", l.file_name, l.folder, facts.join(", "))
    }
}

/// `24 frames (24 raw lights, integration 2 h 00 min), 5 nights 2024-10-05 to 2024-10-09, 1.2 GB`.
fn totals_text(t: &Totals) -> String {
    if t.frames == 0 {
        return "no frames".to_string();
    }
    let lights = if t.lights == 0 {
        "no raw lights".to_string()
    } else {
        format!("{}, integration {}", count_of(t.lights, "raw light", "raw lights"), cq::format_duration(t.integration_s))
    };
    let mut out = format!("{} ({lights})", count_of(t.frames, "frame", "frames"));
    match (t.nights, &t.first_night, &t.last_night) {
        (0, _, _) => {}
        (1, Some(n), _) => out.push_str(&format!(", night {n}")),
        (n, Some(a), Some(b)) => out.push_str(&format!(", {n} nights {a} to {b}")),
        _ => {}
    }
    out.push_str(&format!(", {}", bytes_text(t.bytes)));
    out
}

/// `- IC 5146, filter Ha, 24 frames (24 raw lights, integration 2 h 00 min; 5 nights)`.
fn group_line(g: &FrameGroup) -> String {
    let mut parts: Vec<String> = Vec::new();
    if g.totals.lights > 0 {
        parts.push(format!(
            "{}, integration {}",
            count_of(g.totals.lights, "raw light", "raw lights"),
            cq::format_duration(g.totals.integration_s)
        ));
    }
    if g.totals.nights > 0 {
        parts.push(count_of(g.totals.nights, "night", "nights"));
    }
    if parts.is_empty() {
        format!("- {}", g.label)
    } else {
        format!("- {} ({})", g.label, parts.join("; "))
    }
}

fn metric_label(m: QualityMetric) -> &'static str {
    match m {
        QualityMetric::Fwhm => "FWHM",
        QualityMetric::Hfr => "HFR",
        QualityMetric::Eccentricity => "eccentricity",
        QualityMetric::Stars => "star count",
        QualityMetric::Snr => "star SNR",
        QualityMetric::FrameSnr => "frame SNR",
        QualityMetric::Background => "background",
        QualityMetric::Noise => "noise",
        QualityMetric::TransparencyLoss => "transparency loss (mag)",
    }
}

fn category_label(c: MissingCategory) -> &'static str {
    match c {
        MissingCategory::Coordinates => "no usable coordinates (light frames)",
        MissingCategory::Object => "no object name (light frames)",
        MissingCategory::Date => "no observation time",
        MissingCategory::Camera => "no camera name",
        MissingCategory::FrameType => "no frame type",
    }
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

/// Lines under a header, each indented.
fn indented(lines: impl IntoIterator<Item = String>) -> Vec<String> {
    lines.into_iter().map(|l| format!("  {l}")).collect()
}

// ---- json pieces whose Rust shape is a tuple ----

fn frame_set_json(name: &str, nights: Option<&(String, String)>, t: &FrameSetTotals) -> Value {
    let mut v = json!({
        "name": name,
        "frames": t.frames,
        "integrationS": t.integration_s,
        "blackHoleLeftOut": t.black_hole_left_out.map(|(n, s)| json!({ "frames": n, "integrationS": s })),
        "byFilter": t.by_filter.iter().map(|(f, n, s)| json!({ "filter": f, "frames": n, "integrationS": s })).collect::<Vec<_>>(),
    });
    // Only the several-sets answer names the nights, so the one-set shape is exactly what it was.
    if let (Some((first, last)), Some(obj)) = (nights, v.as_object_mut()) {
        obj.insert("nights".to_string(), json!({ "first": first, "last": last }));
    }
    v
}

fn storage_json(s: &StorageSummary) -> Result<Value, ApiError> {
    let line = |l: &cq::StorageLine| json!({ "label": l.label, "files": l.files, "bytes": l.bytes });
    Ok(json!({
        "byScanRoot": s.by_scan_root.iter().map(line).collect::<Vec<_>>(),
        "byFolder": s.by_folder.iter().map(line).collect::<Vec<_>>(),
        "otherFolders": s.other_folders.as_ref().map(line),
        "byState": s.by_state.iter().map(|(st, n, b)| json!({ "state": json(st).unwrap_or(Value::Null), "files": n, "bytes": b })).collect::<Vec<_>>(),
        "blackHole": { "files": s.black_hole.0, "bytes": s.black_hole.1 },
    }))
}

fn quality_json(q: &QualitySummary) -> Result<Value, ApiError> {
    let outliers = |v: &[(FileLine, f64)]| -> Result<Vec<Value>, ApiError> {
        v.iter().map(|(l, x)| Ok(json!({ "file": json(l)?, "value": x }))).collect()
    };
    Ok(json!({
        "metric": json(&q.metric)?,
        "measured": q.measured,
        "notMeasured": q.not_measured,
        "olderVersion": q.older_version,
        "refused": q.refused.iter().map(|(why, n)| json!({ "reason": why, "frames": n })).collect::<Vec<_>>(),
        "min": q.min,
        "median": q.median,
        "max": q.max,
        "lowOutliers": outliers(&q.low_outliers)?,
        "highOutliers": outliers(&q.high_outliers)?,
        "notMeasuredReason": q.not_measured_reason,
    }))
}

fn calibration_lines(lines: &[CalibrationLine], sets_total: usize) -> Vec<String> {
    let mut out = vec![format!(
        "Calibration of the lights ({}{}):",
        count_of(sets_total, "frame set", "frame sets"),
        if sets_total > FRAME_SETS_PAGE as usize { format!(", the first {FRAME_SETS_PAGE} listed") } else { String::new() }
    )];
    for l in lines {
        let mut line = format!(
            "- {}, night {}, {}, filter {}: {} — darks {}, flats {}, bias {}",
            l.frame_set,
            l.night,
            l.camera,
            l.filter,
            count_of(l.lights, "light", "lights"),
            yes_no(l.darks),
            yes_no(l.flats),
            yes_no(l.bias)
        );
        if let Some(reason) = &l.reason {
            line.push_str(&format!(" ({reason})"));
        }
        out.push(line);
    }
    out
}

fn storage_lines(s: &StorageSummary) -> Vec<String> {
    let mut out = vec!["Where the files live:".to_string()];
    out.push("  By scan root:".to_string());
    out.extend(s.by_scan_root.iter().map(|l| format!("    - {}: {}, {}", l.label, count_of(l.files, "file", "files"), bytes_text(l.bytes))));
    out.push(format!("  By folder (largest first, {} of them):", s.by_folder.len().min(STORAGE_FOLDERS_IN_TEXT)));
    out.extend(
        s.by_folder
            .iter()
            .take(STORAGE_FOLDERS_IN_TEXT)
            .map(|l| format!("    - {}: {}, {}", l.label, count_of(l.files, "file", "files"), bytes_text(l.bytes))),
    );
    let beyond = s.by_folder.iter().skip(STORAGE_FOLDERS_IN_TEXT);
    let (hidden_n, hidden_files, hidden_bytes) = beyond.fold((0usize, 0usize, 0i64), |a, l| (a.0 + 1, a.1 + l.files, a.2 + l.bytes));
    if hidden_n > 0 {
        out.push(format!("    and {} more ({}, {})", count_of(hidden_n, "folder", "folders"), count_of(hidden_files, "file", "files"), bytes_text(hidden_bytes)));
    }
    if let Some(o) = &s.other_folders {
        out.push(format!("    and {} ({}, {})", o.label, count_of(o.files, "file", "files"), bytes_text(o.bytes)));
    }
    if !s.by_state.is_empty() {
        out.push("  By archive state:".to_string());
        for (state, files, bytes) in &s.by_state {
            let name = match state {
                FrameState::Catalog => "loose files",
                FrameState::SoftArchived => "soft-archived",
                FrameState::Zipped => "zipped",
                FrameState::BlackHole => "Black Hole",
            };
            out.push(format!("    - {name}: {}, {}", count_of(*files, "file", "files"), bytes_text(*bytes)));
        }
    }
    if s.black_hole.0 > 0 {
        out.push(format!(
            "  Black Hole, counted apart: {}, {}",
            count_of(s.black_hole.0, "file", "files"),
            bytes_text(s.black_hole.1)
        ));
    }
    out
}

fn quality_lines(q: &QualitySummary) -> Vec<String> {
    let label = metric_label(q.metric);
    let mut out = Vec::new();
    let range = match (q.min, q.median, q.max) {
        (Some(a), Some(m), Some(b)) => format!("; min {a:.3}, median {m:.3}, max {b:.3}"),
        _ => String::new(),
    };
    out.push(format!("Quality, {label}: measured on {}{range}", count_of(q.measured, "raw light", "raw lights")));
    if q.not_measured > 0 {
        out.push(format!("  {} not measured", count_of(q.not_measured, "raw light is", "raw lights are")));
    }
    if q.older_version > 0 {
        out.push(format!(
            "  {} measured by an older version of Athenaeum's measurement",
            count_of(q.older_version, "raw light was", "raw lights were")
        ));
    }
    for (why, n) in &q.refused {
        out.push(format!("  {}: {why}", count_of(*n, "frame", "frames")));
    }
    if let Some(reason) = &q.not_measured_reason {
        out.push(format!("  To measure the rest: {reason}"));
    }
    for (title, list) in [("Low outliers", &q.low_outliers), ("High outliers", &q.high_outliers)] {
        if !list.is_empty() {
            out.push(format!("  {title}:"));
            out.extend(list.iter().map(|(l, v)| format!("    - {v:.3}: {}", file_text(l))));
        }
    }
    out
}

fn files_lines(page: &cq::FilePage, title: &str) -> Vec<String> {
    let mut out = vec![format!("{title}, page {} of {}:", page.page + 1, page.page_count.max(1))];
    if page.items.is_empty() {
        out.push("  (no files on this page)".to_string());
    }
    out.extend(indented(page.items.iter().map(|l| format!("- {}", file_text(l)))));
    out
}

fn internal(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "mcp: {what} failed");
    ApiError::Internal(format!("could not read {what}: {e}"))
}

// ---- query_frames ----

/// The frame sets a scope names and nothing else: the id and stored name of every frame set the catalog holds under
/// that name (the same normalized-name equality `resolve_scope` uses), in id order. Empty for any other scope, and
/// for a name no set carries.
fn frame_sets_named(conn: &Connection, scope: &Scope) -> Result<Vec<(i64, String)>, ApiError> {
    let blank = |v: &Option<String>| v.as_deref().map_or(true, |s| s.trim().is_empty());
    let Some(name) = scope.frame_set.as_deref().map(str::trim).filter(|s| !s.is_empty()) else { return Ok(Vec::new()) };
    let only_a_set = [&scope.object, &scope.folder, &scope.scan_root, &scope.camera, &scope.telescope, &scope.filter, &scope.frame_type, &scope.night, &scope.date_from, &scope.date_to]
        .into_iter()
        .all(blank)
        && scope.exposure_s.is_none()
        && scope.year.is_none()
        && scope.state.as_deref().map_or(true, |s| s.is_empty() || (FrameState::DEFAULT.iter().all(|d| s.contains(d)) && !s.contains(&FrameState::BlackHole)));
    if !only_a_set {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT id, name FROM frames_set WHERE name IS NOT NULL AND name <> '' ORDER BY id")
        .map_err(|e| internal("the frame sets", e))?;
    let sets = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| internal("the frame sets", e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| internal("the frame sets", e))?;
    let wanted = normalize_value(name);
    Ok(sets.into_iter().filter(|(_, n)| normalize_value(n) == wanted).collect())
}

/// The frame set's own figures as a sentence: the page's frame count and integration time, and the Black Hole
/// members the page leaves out.
fn frame_set_lines(heading: &str, t: &FrameSetTotals) -> Vec<String> {
    let mut out = vec![format!(
        "{heading}, as its page shows it: {}, integration {}.",
        count_of(t.frames, "frame", "frames"),
        cq::format_duration(t.integration_s)
    )];
    if let Some((n, s)) = t.black_hole_left_out {
        let (noun, verb) = if n == 1 { ("frame", "is") } else { ("frames", "are") };
        out.push(format!("{n} {noun} of this set {verb} in the Black Hole and not counted ({}).", cq::format_duration(s)));
    }
    if !t.by_filter.is_empty() {
        let per: Vec<String> = t
            .by_filter
            .iter()
            .map(|(f, n, s)| format!("{f} {} {}", count_of(*n, "frame", "frames"), cq::format_duration(*s)))
            .collect();
        out.push(format!("By filter: {}.", per.join("; ")));
    }
    out
}

/// The installed star catalogue's identity, which a transparency reading needs.
fn star_catalogue(host: &McpHost) -> Option<String> {
    #[cfg(all(feature = "render", feature = "solver"))]
    {
        host.ctx().app_data_dir().and_then(|d| crate::analysis::transparency::installed_catalog_identity(&d))
    }
    #[cfg(not(all(feature = "render", feature = "solver")))]
    {
        let _ = host;
        None
    }
}

#[tracing::instrument(skip_all, fields(tool = "query_frames"), err)]
pub fn query_frames(host: &McpHost, args: QueryFramesArgs) -> Result<Answer, ApiError> {
    // Asked of the app's context, so before the reader guard is taken (see `McpHost::reader`).
    let catalogue = if args.quality.is_some() { star_catalogue(host) } else { None };
    let conn = host.reader()?;
    let prep = match prepare(&conn, &args.scope)? {
        Prep::Answered(answer) => return Ok(answer),
        Prep::Ready(prep) => prep,
    };
    let frames = cq::fetch_scoped_frames(&conn, &prep.filter).map_err(|e| internal("the frames in scope", format!("{e:#}")))?;
    let result = cq::query_frames(&frames, &args.group_by, args.files_page);

    let mut lines: Vec<String> = Vec::new();
    let mut data = Map::new();

    // A scope that is exactly one frame set says what that set's own page says (integration time included). A name
    // two or more sets share is answered set by set, each with its own page's figures, so no figure is one no page shows.
    let named = frame_sets_named(&conn, &args.scope)?;
    match named.as_slice() {
        [] => lines.push(format!("Totals: {}.", totals_text(&result.totals))),
        [(set_id, name)] => {
            let t = cq::frame_set_totals(&conn, *set_id).map_err(|e| internal("the frame set", format!("{e:#}")))?;
            lines.extend(frame_set_lines(&format!("Frame set '{name}'"), &t));
            data.insert("frameSet".to_string(), frame_set_json(name, None, &t));
        }
        many => {
            let mut read = Vec::with_capacity(many.len());
            for (set_id, name) in many {
                let t = cq::frame_set_totals(&conn, *set_id).map_err(|e| internal("the frame set", format!("{e:#}")))?;
                let nights = cq::frame_set_nights(&conn, *set_id).map_err(|e| internal("the frame set's nights", format!("{e:#}")))?;
                read.push((*set_id, name, nights, t));
            }
            // By first night (a set with no dated member last), the id breaking a tie.
            read.sort_by(|a, b| {
                let key = |x: &(i64, &String, Option<(String, String)>, FrameSetTotals)| (x.2.is_none(), x.2.as_ref().map(|n| n.0.clone()), x.0);
                key(a).cmp(&key(b))
            });
            let wanted = args.scope.frame_set.as_deref().map(str::trim).unwrap_or_default();
            lines.push(format!("{} frame sets are named '{wanted}'; each is shown with its own page's figures.", many.len()));
            for (_, name, nights, t) in &read {
                let heading = match nights {
                    Some((first, last)) => format!("Frame set '{name}', nights {first} … {last}"),
                    None => format!("Frame set '{name}'"),
                };
                lines.extend(frame_set_lines(&heading, t));
            }
            data.insert(
                "frameSets".to_string(),
                Value::Array(read.iter().map(|(_, name, nights, t)| frame_set_json(name, nights.as_ref(), t)).collect()),
            );
        }
    }
    data.insert("totals".to_string(), json(&result.totals)?);

    if !args.group_by.is_empty() {
        let keys: Vec<String> = args.group_by.iter().map(|k| json(k).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()).collect();
        lines.push(format!("Grouped by {}: {}.", keys.join(", "), count_of(result.groups_total, "group", "groups")));
        let with_examples = result.groups_total <= GROUPS_WITH_EXAMPLES;
        for g in &result.groups {
            lines.push(group_line(g));
            if with_examples {
                lines.extend(g.examples.iter().map(|e| format!("    e.g. {}", file_text(e))));
            }
        }
        if let Some(rest) = &result.rest {
            lines.push(format!(
                "- and {} more: {}",
                count_of(result.groups_total.saturating_sub(GROUPS_SHOWN), "group", "groups"),
                totals_text(rest)
            ));
        }
        data.insert("groups".to_string(), json(&result.groups)?);
        data.insert("groupsTotal".to_string(), json(&result.groups_total)?);
        data.insert("rest".to_string(), json(&result.rest)?);
    }

    if let Some(page) = &result.files {
        lines.extend(files_lines(page, "Files"));
        data.insert("files".to_string(), json(page)?);
    }

    if let Some(metric) = args.quality {
        let q = cq::quality_summary(&conn, catalogue.as_deref(), &frames, metric)?;
        lines.extend(quality_lines(&q));
        data.insert("quality".to_string(), quality_json(&q)?);
    }

    // Every read the reader does comes first; the calibration summary takes `ctx` and checks out pool connections.
    let storage = if args.storage {
        Some(cq::storage_summary(&conn, &frames).map_err(|e| internal("the storage", format!("{e:#}")))?)
    } else {
        None
    };
    drop(conn);

    if args.calibration {
        let (found, sets_total) = cq::calibration_summary(host.ctx(), &frames, 0)?;
        lines.extend(calibration_lines(&found, sets_total));
        data.insert("calibration".to_string(), json!({ "lines": json(&found)?, "frameSetsTotal": sets_total }));
    }

    if let Some(s) = &storage {
        lines.extend(storage_lines(s));
        data.insert("storage".to_string(), storage_json(s)?);
    }

    Ok(prep.answer(&lines.join("\n"), data))
}

// ---- catalog_overview ----

#[tracing::instrument(skip_all, fields(tool = "catalog_overview"), err)]
pub fn catalog_overview(host: &McpHost) -> Result<Answer, ApiError> {
    let conn = host.reader()?;
    let prep = match prepare(&conn, &Scope::default())? {
        Prep::Answered(answer) => return Ok(answer),
        Prep::Ready(prep) => prep,
    };
    let frames = cq::fetch_scoped_frames(&conn, &prep.filter).map_err(|e| internal("the frames", format!("{e:#}")))?;
    let by_type = cq::query_frames(&frames, &[cq::GroupKey::FrameType], None);

    let distinct = |value: fn(&ScopedFrame) -> Option<&String>| -> usize {
        frames
            .iter()
            .filter_map(|f| value(f).map(|v| normalize_value(v)).filter(|v| !v.is_empty()))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    let objects = distinct(|f| f.file.frame.as_ref().and_then(|x| x.object.as_ref()));
    let cameras = distinct(|f| f.file.frame.as_ref().and_then(|x| x.instrume.as_ref()));
    let telescopes = distinct(|f| f.file.frame.as_ref().and_then(|x| x.telescop.as_ref()));
    let filters = distinct(|f| f.file.frame.as_ref().and_then(|x| x.filter.as_ref()));

    let storage = cq::storage_summary(&conn, &frames).map_err(|e| internal("the storage", format!("{e:#}")))?;
    let missing = cq::find_missing_in_scope(&conn, &prep.resolved, &MissingCategory::ALL, 0)?;
    // The reader is done: the duplicate scan takes `ctx` and checks out pool connections.
    drop(conn);
    // Only the totals: a page far past the last one lists no group.
    let (_, dup_groups, dup_extra_bytes) = cq::find_duplicates_in_scope(host.ctx(), &prep.resolved, u32::MAX)?;

    let mut lines = vec![format!("Catalog: {}.", totals_text(&by_type.totals))];
    if !by_type.groups.is_empty() {
        lines.push("By frame type:".to_string());
        lines.extend(by_type.groups.iter().map(group_line));
    }
    lines.push(format!(
        "It holds {}, {}, {} and {}.",
        count_of(objects, "object", "objects"),
        count_of(cameras, "camera", "cameras"),
        count_of(telescopes, "telescope", "telescopes"),
        count_of(filters, "filter", "filters"),
    ));
    if let (Some(a), Some(b)) = (&by_type.totals.first_night, &by_type.totals.last_night) {
        lines.push(format!("First night {a}, last night {b}."));
    }
    lines.extend(storage_lines(&storage));
    lines.push(if dup_groups == 0 {
        "Duplicates: no duplicate groups.".to_string()
    } else {
        format!("Duplicates: {}; their extra copies take {}.", count_of(dup_groups, "group", "groups"), bytes_text(dup_extra_bytes))
    });
    let counts: Vec<String> = missing.counts.iter().map(|(c, n)| format!("{}: {}", category_label(*c), count_of(*n, "file", "files"))).collect();
    lines.push(format!("Missing metadata: {}.", counts.join("; ")));

    let mut data = Map::new();
    data.insert("totals".to_string(), json(&by_type.totals)?);
    data.insert("byFrameType".to_string(), json(&by_type.groups)?);
    data.insert("distinct".to_string(), json!({ "objects": objects, "cameras": cameras, "telescopes": telescopes, "filters": filters }));
    data.insert("firstNight".to_string(), json(&by_type.totals.first_night)?);
    data.insert("lastNight".to_string(), json(&by_type.totals.last_night)?);
    data.insert("storage".to_string(), storage_json(&storage)?);
    data.insert("duplicates".to_string(), json!({ "groups": dup_groups, "extraCopyBytes": dup_extra_bytes }));
    data.insert(
        "missingMetadata".to_string(),
        Value::Array(missing.counts.iter().map(|(c, n)| json!({ "category": json(c).unwrap_or(Value::Null), "files": n })).collect()),
    );
    Ok(prep.answer(&lines.join("\n"), data))
}

// ---- file_details ----

#[tracing::instrument(skip_all, fields(tool = "file_details"), err)]
pub fn file_details(host: &McpHost, args: FileArgs) -> Result<Answer, ApiError> {
    // The catalog must be open before any question; the details themselves read through the app's own queries.
    host.source()?;
    let d = cq::file_details(host.ctx(), &args.file)?;
    let l = &d.line;

    let mut lines = vec![format!("{} in {}", l.file_name, l.folder), format!("Path as the catalog stores it: {}", d.path)];
    lines.push(format!("Size {}, modified {}.", bytes_text(d.size), d.modified_at));
    let mut facts: Vec<String> = Vec::new();
    facts.extend(l.imagetyp.clone().map(|t| format!("type {t}")));
    facts.extend(l.object.clone().map(|o| format!("object {o}")));
    facts.extend(l.filter.clone().map(|f| format!("filter {f}")));
    facts.extend(l.exptime_s.map(|e| format!("exposure {}", seconds(e))));
    facts.extend(l.gain.map(|g| format!("gain {g}")));
    facts.extend(l.offset.map(|o| format!("offset {o}")));
    facts.extend(l.ccd_temp.map(|t| format!("sensor temperature {t} C")));
    facts.extend(l.binning.clone().map(|b| format!("binning {b}")));
    if let (Some(w), Some(h)) = (l.width, l.height) {
        facts.push(format!("{w} x {h} px"));
    }
    facts.extend(l.date_obs.clone().map(|t| format!("observed {t} UTC")));
    facts.extend(l.night.clone().map(|n| format!("night {n}")));
    facts.extend(l.instrume.clone().map(|c| format!("camera {c}")));
    facts.extend(l.telescop.clone().map(|t| format!("telescope {t}")));
    facts.push(
        match l.state {
            FrameState::Catalog => "a loose file",
            FrameState::SoftArchived => "soft-archived",
            FrameState::Zipped => "inside a zip",
            FrameState::BlackHole => "in the Black Hole",
        }
        .to_string(),
    );
    lines.push(format!("Header values: {}.", facts.join(", ")));

    match &d.analysis {
        Some(a) if d.analysis_current => lines.push(format!(
            "Measurements: {} stars, FWHM {:.2}, HFR {:.2}, eccentricity {:.2}, star SNR {:.1}, frame SNR {:.1}{}.",
            a.stars_detected,
            a.median_fwhm,
            a.median_hfr,
            a.median_eccentricity,
            a.median_snr,
            a.frame_snr,
            if a.possibly_trailed { "; the stars look trailed" } else { "" }
        )),
        Some(_) => lines.push("Measurements: stored by an older version of Athenaeum's measurement; run Analysis on this frame in Athenaeum to refresh them.".to_string()),
        None => lines.push("Measurements: none stored; run Analysis on this frame in Athenaeum.".to_string()),
    }
    match (d.transparency_loss, &d.transparency_reason) {
        (Some(loss), _) => lines.push(format!("Transparency loss: {loss:.2} mag.")),
        (None, Some(reason)) => lines.push(format!("Transparency loss: not available ({reason}).")),
        (None, None) => {}
    }
    lines.push(if d.solved { "A plate solve is stored for this frame.".to_string() } else { "No plate solve is stored for this frame.".to_string() });

    if d.calibration.is_empty() {
        lines.push("Calibration: none linked.".to_string());
    } else {
        lines.push("Calibration linked:".to_string());
        lines.extend(indented(d.calibration.iter().map(|c| format!("- {}: {} in {}", c.kind, c.file_name, c.folder))));
    }
    if !d.copies.is_empty() {
        lines.push("Other copies the catalog knows:".to_string());
        lines.extend(indented(d.copies.iter().map(|c| format!("- {}", file_text(c)))));
    }
    if !d.header.is_empty() {
        let shown: Vec<String> = d.header.iter().take(HEADER_KEYWORDS_IN_TEXT).map(|(k, v)| format!("{k} = {v}")).collect();
        let more = d.header.len().saturating_sub(HEADER_KEYWORDS_IN_TEXT);
        lines.push(format!(
            "Stored header ({}): {}{}",
            count_of(d.header.len(), "keyword", "keywords"),
            shown.join("; "),
            if more > 0 { format!("; and {more} more") } else { String::new() }
        ));
    }

    let analysis = d.analysis.as_ref().map(|a| {
        json!({
            "starsDetected": a.stars_detected,
            "medianFwhm": a.median_fwhm,
            "medianHfr": a.median_hfr,
            "medianEccentricity": a.median_eccentricity,
            "medianSnr": a.median_snr,
            "frameSnr": a.frame_snr,
            "background": a.background,
            "noise": a.noise,
            "possiblyTrailed": a.possibly_trailed,
            "frameVerdict": json(&a.frame_verdict).unwrap_or(Value::Null),
            "analyzedAt": a.analyzed_at,
        })
    });
    let data = json!({
        "file": json(&d.line)?,
        "path": d.path,
        "size": d.size,
        "modifiedAt": d.modified_at,
        "header": d.header.iter().map(|(k, v)| json!({ "keyword": k, "value": v })).collect::<Vec<_>>(),
        "analysis": analysis,
        "analysisCurrent": d.analysis_current,
        "transparencyLoss": d.transparency_loss,
        "transparencyReason": d.transparency_reason,
        "calibration": json(&d.calibration)?,
        "copies": json(&d.copies)?,
        "solved": d.solved,
    });
    Ok(Answer::new(lines.join("\n"), data))
}

// ---- find_duplicates ----

/// The duplicate group as the page groups it; the group's handle sits beside the file name and folder of its first
/// copy, so a handle never stands without them.
fn duplicate_json(f: &cq::DuplicateFinding) -> Result<Value, ApiError> {
    let mut v = json(f)?;
    if let (Some(obj), Some(first)) = (v.as_object_mut(), f.copies.first()) {
        obj.insert("fileName".to_string(), json!(first.line.file_name));
        obj.insert("folder".to_string(), json!(first.line.folder));
    }
    Ok(v)
}

#[tracing::instrument(skip_all, fields(tool = "find_duplicates"), err)]
pub fn find_duplicates(host: &McpHost, args: ScopeArgs) -> Result<Answer, ApiError> {
    let conn = host.reader()?;
    let prep = match prepare(&conn, &args.scope)? {
        Prep::Answered(answer) => return Ok(answer),
        Prep::Ready(prep) => prep,
    };
    drop(conn);
    let page = args.page.unwrap_or(0);
    let (groups, total, extra_bytes) = cq::find_duplicates_in_scope(host.ctx(), &prep.resolved, page)?;
    let pages = total.div_ceil(GROUPS_SHOWN);

    let mut lines: Vec<String> = Vec::new();
    if total == 0 {
        lines.push("No duplicate groups in this scope.".to_string());
    } else {
        lines.push(format!(
            "{} with a copy in this scope; their extra copies take {}.",
            count_of(total, "duplicate group", "duplicate groups"),
            bytes_text(extra_bytes)
        ));
        if groups.is_empty() {
            lines.push(format!("Page {} is past the last page ({pages}).", page.saturating_add(1)));
        } else {
            lines.push(format!("Page {} of {pages}: {}.", page + 1, count_of(groups.len(), "group", "groups")));
        }
    }
    for (n, g) in groups.iter().enumerate() {
        lines.push(format!(
            "{}. {} copies, {} each, matched by {}:",
            page as usize * GROUPS_SHOWN + n + 1,
            g.copies.len(),
            bytes_text(g.size),
            g.matched_by
        ));
        for c in &g.copies {
            let mut line = format!("  - {}", file_text(&c.line));
            if let Some(root) = &c.scan_root {
                line.push_str(&format!("; under {root}"));
            }
            if let Some(kind) = c.root_kind.as_deref().filter(|k| *k != "normal") {
                line.push_str(&format!("; scan root kind {}", kind.replace('_', " ")));
            }
            if let Some(note) = &c.folder_note {
                line.push_str(&format!("; {note}"));
            }
            lines.push(line);
        }
    }

    let mut data = Map::new();
    data.insert("groups".to_string(), Value::Array(groups.iter().map(duplicate_json).collect::<Result<_, _>>()?));
    data.insert("groupsInScope".to_string(), json!(total));
    data.insert("extraCopyBytes".to_string(), json!(extra_bytes));
    data.insert("page".to_string(), json!(page));
    data.insert("pageCount".to_string(), json!(pages));
    Ok(prep.answer(&lines.join("\n"), data))
}

// ---- find_missing_metadata ----

#[tracing::instrument(skip_all, fields(tool = "find_missing_metadata"), err)]
pub fn find_missing_metadata(host: &McpHost, args: MissingArgs) -> Result<Answer, ApiError> {
    let conn = host.reader()?;
    let prep = match prepare(&conn, &args.scope)? {
        Prep::Answered(answer) => return Ok(answer),
        Prep::Ready(prep) => prep,
    };
    let found = cq::find_missing_in_scope(&conn, &prep.resolved, &args.categories, args.page.unwrap_or(0))?;

    let files_total: usize = found.by_folder.iter().map(|(_, n, _)| n).sum();
    let mut lines: Vec<String> = Vec::new();
    if files_total == 0 {
        lines.push("No frame in this scope is missing the metadata asked about.".to_string());
    } else {
        let counts: Vec<String> =
            found.counts.iter().map(|(c, n)| format!("{}: {}", category_label(*c), count_of(*n, "file", "files"))).collect();
        lines.push(format!("{} lack metadata. {}.", count_of(files_total, "file", "files"), counts.join("; ")));
        lines.push(format!("In {}:", count_of(found.by_folder.len(), "folder", "folders")));
        for (folder, n, examples) in found.by_folder.iter().take(GROUPS_SHOWN) {
            let names: Vec<&str> = examples.iter().map(|e| e.file_name.as_str()).collect();
            lines.push(format!("  - {folder}: {}, e.g. {}", count_of(*n, "file", "files"), names.join(", ")));
        }
        let beyond: Vec<&(String, usize, Vec<FileLine>)> = found.by_folder.iter().skip(GROUPS_SHOWN).collect();
        if !beyond.is_empty() {
            lines.push(format!(
                "  and {} more {} ({})",
                beyond.len(),
                if beyond.len() == 1 { "folder" } else { "folders" },
                count_of(beyond.iter().map(|(_, n, _)| n).sum(), "file", "files")
            ));
        }
        lines.extend(files_lines(&found.page, "Files, oldest first"));
    }

    let mut data = Map::new();
    data.insert(
        "counts".to_string(),
        Value::Array(found.counts.iter().map(|(c, n)| json!({ "category": json(c).unwrap_or(Value::Null), "files": n })).collect()),
    );
    data.insert("filesMissing".to_string(), json!(files_total));
    let folder_entry = |(folder, n, examples): &(String, usize, Vec<FileLine>)| -> Result<Value, ApiError> {
        Ok(json!({ "folder": folder, "files": n, "examples": json(examples)? }))
    };
    data.insert(
        "byFolder".to_string(),
        Value::Array(found.by_folder.iter().take(GROUPS_SHOWN).map(folder_entry).collect::<Result<_, _>>()?),
    );
    let beyond = found.by_folder.iter().skip(GROUPS_SHOWN);
    let (more_folders, more_files) = beyond.fold((0usize, 0usize), |a, (_, n, _)| (a.0 + 1, a.1 + n));
    data.insert("otherFolders".to_string(), json!({ "folders": more_folders, "files": more_files }));
    data.insert("page".to_string(), json(&found.page)?);
    Ok(prep.answer(&lines.join("\n"), data))
}
