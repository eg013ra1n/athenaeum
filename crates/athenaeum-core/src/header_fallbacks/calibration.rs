//! What a change to the catalog's header values means for calibration sets (issue #21): which
//! raw calibration frames would have to be regrouped, and which sets may be changed at all.
//!
//! The preview counts these; Apply moves the frames ([`regroup`]) and updates the master-library
//! sets.

use super::apply::write_columns;
use super::resolve::{same_value, FieldValue, ResolvedFields};
use super::HeaderField;
use crate::calibration::scan_integration::{create_calibration_sets_from_scan_with_masters, MasterFrameIds};
use crate::models::effective_temp;
use rusqlite::{params, types::Value, Connection};
use std::collections::{BTreeMap, BTreeSet};

/// The master frame types. The scanner classifies a master by `imagetyp` alone
/// (`scanner/mod.rs`), so these are the frames whose own set is a master-library set.
pub(crate) const MASTER_TYPES: [&str; 4] = ["MasterDark", "MasterFlat", "MasterBias", "MasterDarkFlat"];

/// The columns of a master-library set that `create_master_sets_from_frames` copies from its
/// master frame (`temp_min` and `temp_max` follow `ccd_temp`).
pub(crate) const MASTER_SET_FIELDS: [HeaderField; 7] = [
    HeaderField::Exptime,
    HeaderField::Filter,
    HeaderField::CcdTemp,
    HeaderField::Gain,
    HeaderField::Offset,
    HeaderField::Instrume,
    HeaderField::Focallen,
];

/// The raw calibration frame types the scanner groups into sets.
fn is_raw_calibration(imagetyp: &str) -> bool {
    matches!(imagetyp, "Bias" | "Dark" | "DarkFlat" | "Flat")
}

/// The group-key fields of a raw calibration type, the temperature apart (the scanner clusters
/// on `effective_temp(ccd_temp, set_temp)`, see [`needs_regroup`]). Anything else groups by none.
pub(crate) fn grouping_fields(imagetyp: &str) -> &'static [HeaderField] {
    use HeaderField::*;
    match imagetyp {
        "Bias" => &[Instrume, Gain, Offset],
        "Dark" | "DarkFlat" => &[Instrume, Gain, Offset, Exptime],
        "Flat" => &[Instrume, Filter, Gain, Offset, Exptime, Focallen],
        _ => &[],
    }
}

/// Whether `changes` to a frame of type `imagetyp` change the group it belongs to: a raw
/// calibration frame whose change is in [`grouping_fields`], or whose effective temperature
/// (`ccd_temp`, else `set_temp`) is not the same afterwards (so a `setTemp` change counts only for
/// a frame whose `ccd_temp` is NULL).
pub(crate) fn needs_regroup(
    imagetyp: Option<&str>,
    old: &ResolvedFields,
    changes: &[(HeaderField, Option<FieldValue>)],
) -> bool {
    let Some(imagetyp) = imagetyp.filter(|t| is_raw_calibration(t)) else {
        return false;
    };
    if changes.iter().any(|(f, _)| grouping_fields(imagetyp).contains(f)) {
        return true;
    }
    let mut new = old.clone();
    for (f, v) in changes {
        new.set(*f, v.clone());
    }
    match (
        effective_temp(old.ccd_temp, old.set_temp),
        effective_temp(new.ccd_temp, new.set_temp),
    ) {
        (None, None) => false,
        (Some(a), Some(b)) => !same_value(&FieldValue::Number(a), &FieldValue::Number(b)),
        _ => true,
    }
}

/// The calibration sets holding any of `frame_ids` that Apply may take frames out of: not a
/// master-library set, not superseded, not the source of a master (the lineage guard of
/// `db/schema.rs`).
pub(crate) fn movable_sets_of(conn: &Connection, frame_ids: &[i64]) -> anyhow::Result<BTreeSet<i64>> {
    let mut sets = BTreeSet::new();
    for chunk in frame_ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT csf.set_id
               FROM calibration_set_frames csf
               JOIN calibration_set cs ON cs.id = csf.set_id
              WHERE csf.frame_id IN ({placeholders})
                AND COALESCE(cs.is_master_library, 0) = 0
                AND cs.superseded_by_set_id IS NULL
                AND cs.id NOT IN (SELECT source_set_id FROM master_provenance
                                   WHERE source_set_id IS NOT NULL)"
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| r.get::<_, i64>(0))?;
        for id in ids {
            sets.insert(id?);
        }
    }
    Ok(sets)
}

/// The light links that go with the movable sets every member of which leaves (O8): such a set
/// is emptied, the prune trigger deletes it and its `calibration_set_to_frames` rows cascade
/// (`db/schema.rs`). Returns `(light links dropped, of them made by hand)`; only links whose
/// source is a light frame count (a calibration set's own pre-calibration links are not lights).
///
/// `movable` is [`movable_sets_of`] of `leaving`, the raw calibration frames that would leave
/// their sets. The preview and Apply both call this with the same plan, so their numbers agree.
pub(crate) fn light_links_dropped(
    conn: &Connection,
    movable: &BTreeSet<i64>,
    leaving: &[i64],
) -> anyhow::Result<(usize, usize)> {
    if movable.is_empty() {
        return Ok((0, 0));
    }
    let leaving: BTreeSet<i64> = leaving.iter().copied().collect();
    let movable_ids: Vec<i64> = movable.iter().copied().collect();
    // A set stays when one of its members does not leave.
    let mut stays: BTreeSet<i64> = BTreeSet::new();
    for chunk in movable_ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT set_id, frame_id FROM calibration_set_frames WHERE set_id IN ({placeholders})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (set_id, frame_id) = row?;
            if !leaving.contains(&frame_id) {
                stays.insert(set_id);
            }
        }
    }
    let emptied: Vec<i64> = movable_ids.into_iter().filter(|id| !stays.contains(id)).collect();
    let (mut links, mut manual) = (0usize, 0usize);
    for chunk in emptied.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let (n, m): (i64, i64) = conn.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(is_manual_override), 0)
                   FROM calibration_set_to_frames
                  WHERE source_type = 'frame' AND calibration_set_id IN ({placeholders})"
            ),
            rusqlite::params_from_iter(chunk.iter()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        links += n as usize;
        manual += m as usize;
    }
    Ok((links, manual))
}

/// A frame whose columns Apply writes in its calibration transaction instead of in a page: a raw
/// calibration frame that has to be regrouped, or a master frame with a change in
/// [`MASTER_SET_FIELDS`].
#[derive(Debug, Clone)]
pub(crate) struct Deferred {
    pub frame_id: i64,
    pub imagetyp: String,
    pub changes: Vec<(HeaderField, Option<FieldValue>)>,
}

/// Whether a changed frame waits for the calibration transaction.
pub(crate) fn is_deferred(
    imagetyp: Option<&str>,
    old: &ResolvedFields,
    changes: &[(HeaderField, Option<FieldValue>)],
) -> bool {
    if needs_regroup(imagetyp, old, changes) {
        return true;
    }
    imagetyp.is_some_and(|t| MASTER_TYPES.contains(&t))
        && changes.iter().any(|(f, _)| MASTER_SET_FIELDS.contains(f))
}

/// What [`regroup`] did, for Apply's counts and its log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RegroupReport {
    /// Raw calibration sets that lost frames.
    pub sets_regrouped: usize,
    /// Light links that went with the sets this emptied (O8).
    pub light_links_dropped: usize,
    /// Of those, the links made by hand.
    pub manual_links_dropped: usize,
    /// Master-library sets whose own columns were updated.
    pub master_sets: usize,
    /// Raw frames that sit in a frozen (superseded or provenance) set: their values were written
    /// and their membership kept (D11).
    pub lineage_kept: usize,
}

fn sql_value(v: &Option<FieldValue>) -> Value {
    match v {
        Some(FieldValue::Number(n)) => Value::Real(*n),
        Some(FieldValue::Text(t)) => Value::Text(t.clone()),
        None => Value::Null,
    }
}

/// How many of `frame_ids` are a member of no calibration set.
fn frames_without_set(conn: &Connection, frame_ids: &[i64]) -> anyhow::Result<usize> {
    let mut missing = 0usize;
    for chunk in frame_ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let with_set: i64 = conn.query_row(
            &format!("SELECT COUNT(DISTINCT frame_id) FROM calibration_set_frames WHERE frame_id IN ({placeholders})"),
            rusqlite::params_from_iter(chunk.iter()),
            |r| r.get(0),
        )?;
        missing += chunk.len().saturating_sub(with_set as usize);
    }
    Ok(missing)
}

/// The calibration transaction's work (spec §5.5); the caller holds one `BEGIN IMMEDIATE`.
///
/// 1. Writes the columns of every deferred frame.
/// 2. Raw calibration frames: takes each out of the movable sets that hold it (the empty-set
///    trigger prunes the sets this emptied), and groups the moved frames again through the
///    scanner's own path as if newly scanned. A frame of a frozen set keeps its membership (D11).
/// 3. Master frames: the `is_master_library` set holding the frame takes the changed values.
///
/// A set that keeps other members is left as it is (the bulk edit leaves it so too): its
/// `frame_count` follows the membership triggers, its `temp_min` / `temp_max` do not move.
pub(crate) fn regroup(conn: &Connection, deferred: &[Deferred]) -> anyhow::Result<RegroupReport> {
    let mut report = RegroupReport::default();
    for d in deferred {
        write_columns(conn, d.frame_id, &d.changes)?;
    }

    let raw: Vec<&Deferred> = deferred.iter().filter(|d| is_raw_calibration(&d.imagetyp)).collect();
    if !raw.is_empty() {
        let raw_ids: Vec<i64> = raw.iter().map(|d| d.frame_id).collect();
        let movable = movable_sets_of(conn, &raw_ids)?;
        report.sets_regrouped = movable.len();
        (report.light_links_dropped, report.manual_links_dropped) = light_links_dropped(conn, &movable, &raw_ids)?;

        let mut moved: BTreeSet<i64> = BTreeSet::new();
        let mut frozen: BTreeSet<i64> = BTreeSet::new();
        let mut memberships: Vec<(i64, i64)> = Vec::new();
        for chunk in raw_ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let mut stmt = conn.prepare(&format!(
                "SELECT frame_id, set_id FROM calibration_set_frames WHERE frame_id IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                memberships.push(row?);
            }
        }
        for (frame_id, set_id) in &memberships {
            if movable.contains(set_id) {
                moved.insert(*frame_id);
            } else {
                frozen.insert(*frame_id);
            }
        }
        report.lineage_kept = frozen.len();
        if report.lineage_kept > 0 {
            tracing::debug!(
                lineage_kept = report.lineage_kept,
                "frames of a frozen calibration set kept their membership"
            );
        }
        {
            let mut delete =
                conn.prepare("DELETE FROM calibration_set_frames WHERE set_id = ?1 AND frame_id = ?2")?;
            for (frame_id, set_id) in &memberships {
                if movable.contains(set_id) {
                    delete.execute(params![set_id, frame_id])?;
                }
            }
        }
        // No whole-table prune: the `calibration_set_empty_prune` trigger removes every set this
        // emptied, with the same filter as `movable_sets_of`, and leaves a concurrent scan's new set alone.

        let mut by_type: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
        for d in &raw {
            if moved.contains(&d.frame_id) {
                by_type.entry(d.imagetyp.as_str()).or_default().push(d.frame_id);
            }
        }
        if !by_type.is_empty() {
            let regrouped: Vec<i64> = by_type.values().flatten().copied().collect();
            let mut take = |t: &str| by_type.remove(t).unwrap_or_default();
            let (flats, darks, bias, darkflats) = (take("Flat"), take("Dark"), take("Bias"), take("DarkFlat"));
            create_calibration_sets_from_scan_with_masters(
                conn,
                flats,
                darks,
                bias,
                darkflats,
                MasterFrameIds::default(),
            )?;
            // The creation logs and swallows a per-set error, so a frame can be left in no set,
            // and the next preview would read 0 for it. Failing here rolls the transaction back.
            let homeless = frames_without_set(conn, &regrouped)?;
            if homeless > 0 {
                anyhow::bail!("{homeless} regrouped calibration frame(s) were left in no calibration set");
            }
        }
    }

    let mut master_sets: BTreeSet<i64> = BTreeSet::new();
    for d in deferred.iter().filter(|d| MASTER_TYPES.contains(&d.imagetyp.as_str())) {
        let changed: Vec<&(HeaderField, Option<FieldValue>)> =
            d.changes.iter().filter(|(f, _)| MASTER_SET_FIELDS.contains(f)).collect();
        if changed.is_empty() {
            continue;
        }
        let set_id: Option<i64> = conn
            .query_row(
                "SELECT csf.set_id
                   FROM calibration_set_frames csf
                   JOIN calibration_set cs ON cs.id = csf.set_id
                  WHERE csf.frame_id = ?1 AND COALESCE(cs.is_master_library, 0) = 1
                  ORDER BY csf.set_id LIMIT 1",
                params![d.frame_id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let Some(set_id) = set_id else { continue };
        let mut sets: Vec<String> = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        for (field, value) in changed {
            sets.push(format!("{} = ?", field.column()));
            values.push(sql_value(value));
            if *field == HeaderField::CcdTemp {
                sets.push("temp_min = ?".to_string());
                values.push(sql_value(value));
                sets.push("temp_max = ?".to_string());
                values.push(sql_value(value));
            }
        }
        values.push(Value::Integer(set_id));
        conn.execute(
            &format!("UPDATE calibration_set SET {} WHERE id = ?", sets.join(", ")),
            rusqlite::params_from_iter(values.iter()),
        )?;
        master_sets.insert(set_id);
    }
    report.master_sets = master_sets.len();
    Ok(report)
}
