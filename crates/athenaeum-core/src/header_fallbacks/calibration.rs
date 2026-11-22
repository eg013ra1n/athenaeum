//! What a change to the catalog's header values means for calibration sets (issue #21): which
//! raw calibration frames would have to be regrouped, and which sets may be changed at all.
//!
//! The preview counts these; Apply (a later task) moves the frames.

use super::resolve::{same_value, FieldValue, ResolvedFields};
use super::HeaderField;
use crate::models::effective_temp;
use rusqlite::Connection;
use std::collections::BTreeSet;

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
