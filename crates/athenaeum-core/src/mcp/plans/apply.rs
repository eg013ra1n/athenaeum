//! The apply framework: the generic check of a selection, the re-check of every file against its state at planning,
//! and the run that hands what is still fresh to the plan's applier.
//!
//! The store calls [`check_selection`] before a plan goes to `applying` and [`run`] on a thread of its own. Nothing
//! here is reachable from an MCP tool: the window's command is the only caller of `PlanStore::answer`.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;

use super::model::{
    catalog_values, disk_stamp, file_location, stamp_difference, stamp_target, ItemSnapshot, McpPlanView,
    PlanItem, PlanReport, PlanSelection, ReportLine,
};
use super::{Content, PlanApplier};
use crate::mcp::McpHost;

/// The generic part of "the selection must be a subset of the plan's options": known groups and items, no locked
/// item included, `chosen` a candidate of its group. The error names what is wrong, in words.
pub(super) fn check_selection(view: &McpPlanView, sel: &PlanSelection) -> Result<(), String> {
    let mut seen_groups = HashSet::new();
    let mut seen_items = HashSet::new();
    let mut any = false;
    for g in &sel.groups {
        let group = view
            .groups
            .iter()
            .find(|x| x.id == g.group_id)
            .ok_or_else(|| format!("the plan has no group '{}'", g.group_id))?;
        if !seen_groups.insert(g.group_id.as_str()) {
            return Err(format!("group '{}' is answered twice", g.group_id));
        }
        if let Some(chosen) = &g.chosen {
            let item = group
                .items
                .iter()
                .find(|i| &i.id == chosen)
                .ok_or_else(|| format!("'{chosen}' is not in group '{}'", group.title))?;
            if !item.candidate {
                return Err(format!("{} cannot be chosen in group '{}'", describe(item), group.title));
            }
        }
        for id in &g.included {
            let item = group
                .items
                .iter()
                .find(|i| &i.id == id)
                .ok_or_else(|| format!("'{id}' is not in group '{}'", group.title))?;
            if let Some(reason) = &item.locked {
                return Err(format!("{} is locked and cannot be included: {reason}", describe(item)));
            }
            if !seen_items.insert(id.as_str()) {
                return Err(format!("{} is included twice", describe(item)));
            }
            any = true;
        }
    }
    if !any {
        return Err("nothing is selected to apply".to_string());
    }
    Ok(())
}

fn describe(item: &PlanItem) -> String {
    format!("{} ({})", item.file_name, item.folder)
}

/// Why an item is no longer what the plan saw; `None` when its file and catalog values are unchanged.
fn changed_since_plan(conn: &Connection, snap: &ItemSnapshot) -> Option<String> {
    let location = match file_location(conn, snap.file_id) {
        Ok(Some(l)) => l,
        Ok(None) => return Some("no longer in the catalog".to_string()),
        Err(e) => return Some(e),
    };
    let target = stamp_target(&location.0, location.1.as_deref());
    if target != snap.stamp.path {
        return Some("moved since the plan".to_string());
    }
    if let Some(reason) = stamp_difference(&snap.stamp, &disk_stamp(Path::new(&target))) {
        return Some(reason);
    }
    if snap.catalog.is_empty() {
        return None;
    }
    let Some(frame_id) = snap.frame_id else {
        return Some("it has no frame to read the catalog values from".to_string());
    };
    let keys: Vec<&str> = snap.catalog.keys().map(String::as_str).collect();
    match catalog_values(conn, frame_id, &keys) {
        Err(e) => Some(e),
        Ok(now) => {
            for (key, planned) in &snap.catalog {
                if now.get(key) != Some(planned) {
                    return Some(match now.get(key).cloned().flatten() {
                        Some(v) => format!("catalog changed: {key} is now '{v}'"),
                        None => format!("catalog changed: {key} is now empty"),
                    });
                }
            }
            None
        }
    }
}

fn line(view: &McpPlanView, item_id: &str, reason: impl Into<String>) -> ReportLine {
    let (file_name, folder) = view
        .groups
        .iter()
        .flat_map(|g| g.items.iter())
        .find(|i| i.id == item_id)
        .map(|i| (i.file_name.clone(), i.folder.clone()))
        .unwrap_or_default();
    ReportLine { item_id: item_id.to_string(), file_name, folder, reason: reason.into() }
}

/// Why an apply ended in `failed`, and what it had already worked out (the store's own skip lines).
pub(super) struct Failure {
    pub error: String,
    pub report: Option<PlanReport>,
}

/// Re-stamps every included item and each group's kept copy. Returns the ids still fresh and the lines of what was
/// skipped. Holds a pooled catalog connection only for its own length: the caller releases it before the applier runs,
/// because an applier reaches the catalog through the pool itself.
fn recheck(
    conn: &Connection,
    view: &McpPlanView,
    sel: &PlanSelection,
    content: &Content,
) -> (Vec<String>, Vec<ReportLine>) {
    let mut skipped: Vec<ReportLine> = Vec::new();
    let mut fresh: Vec<String> = Vec::new();
    for g in &sel.groups {
        let keeper = g.chosen.as_deref().and_then(|id| match content.snapshots.get(id) {
            Some(snap) => changed_since_plan(conn, snap),
            None => Some("it was not recorded at planning".to_string()),
        });
        for id in &g.included {
            if let Some(reason) = &keeper {
                skipped.push(line(view, id, format!("the copy that stays changed or is gone: {reason}")));
                continue;
            }
            let reason = match content.snapshots.get(id.as_str()) {
                Some(snap) => changed_since_plan(conn, snap),
                None => Some("it was not recorded at planning".to_string()),
            };
            match reason {
                Some(r) => skipped.push(line(view, id, r)),
                None => fresh.push(id.clone()),
            }
        }
    }
    (fresh, skipped)
}

/// Re-checks the selection (on a connection of its own, released before the next step), hands the fresh items to the
/// applier and merges the reports. The caller wears the `mcp_plan_apply` span; the closing `info!` is here.
pub(super) fn run(
    host: &McpHost,
    plan_id: &str,
    kind: &str,
    view: &McpPlanView,
    sel: &PlanSelection,
    content: &Content,
) -> Result<PlanReport, Failure> {
    let fail = |error: String| Failure { error, report: None };
    let (fresh, mut skipped) = {
        let database = crate::api::db(host.ctx()).map_err(|e| fail(e.to_string()))?;
        let conn = database.try_conn().map_err(|e| {
            tracing::error!(error = %e, "mcp plan apply could not reach the catalog");
            fail(format!("the catalog is busy: {e}"))
        })?;
        recheck(&conn, view, sel, content)
    };
    let mut report = if fresh.is_empty() {
        PlanReport::default()
    } else {
        let applier: &dyn PlanApplier = content.applier.as_ref();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| applier.apply(host, plan_id, sel, &fresh))) {
            Ok(report) => report,
            Err(_) => {
                tracing::error!(plan_id, "mcp plan applier panicked");
                // What the store skipped is still true: the report keeps it.
                return Err(Failure {
                    error: "applying the plan stopped unexpectedly; check the files before trying again".to_string(),
                    report: Some(PlanReport { skipped, ..PlanReport::default() }),
                });
            }
        }
    };
    skipped.append(&mut report.skipped);
    report.skipped = skipped;
    tracing::info!(
        via = "mcp",
        plan_id,
        kind,
        applied = report.applied.len(),
        skipped = report.skipped.len(),
        failed = report.failed.len(),
        "mcp plan applied"
    );
    Ok(report)
}
