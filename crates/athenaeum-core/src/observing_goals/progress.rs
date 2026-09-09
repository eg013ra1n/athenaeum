//! One representative per confirmed exposure, including versions scoped to this
//! field only. No best-quality-version selection or cross-field summation.
use super::{
    models::ObservingProgress,
    policy::{qualify, Qualification},
    storage,
};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

/// Read per-filter counts and accepted integration seconds for one frame-set ID.
/// Uses a consistent catalog snapshot; filters with a goal but no frames remain
/// present with zero counts. Unknown exposure times contribute no seconds.
/// Database or stored-data errors propagate; original images are never opened.
pub fn progress(conn: &Connection, id: i64) -> Result<Vec<ObservingProgress>> {
    // One read transaction keeps goals, membership and metrics mutually consistent.
    let tx = conn.unchecked_transaction()?;
    let mut rows = BTreeMap::new();
    for goal in storage::goals(&tx, id)? {
        rows.insert(
            goal.filter.clone(),
            ObservingProgress {
                filter: goal.filter.clone(),
                goal: Some(goal),
                accepted: 0,
                rejected: 0,
                unknown: 0,
                accepted_seconds: 0.0,
            },
        );
    }
    let ids = tx
        .prepare(
            "
        SELECT DISTINCT f.id FROM frames f JOIN session_members sm ON sm.frame_id=f.id
        JOIN sessions s ON s.id=sm.session_id JOIN imaging_nights n ON
        n.id=s.imaging_night_id
        WHERE n.frames_set_id=?1 AND UPPER(COALESCE(f.imagetyp,''))='LIGHT'
        AND NOT EXISTS(SELECT 1 FROM black_hole b WHERE b.file_id=f.file_id)
        ",
        )?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    let ids = crate::exposure_versions::effective_ids(&tx, &ids)?;
    {
        let mut stmt = tx.prepare(
            "
        SELECT COALESCE(f.filter, ''
        ),f.exptime,a.stars_detected,a.median_fwhm,a.median_eccentricity,a.possibly_trailed
            FROM frames f LEFT JOIN frame_analysis a ON a.frame_id=f.id
            WHERE f.id IN(SELECT value FROM json_each(?1))
        ",
        )?;
        let data = stmt.query_map([serde_json::to_string(&ids)?], |r| {
            let stars: Option<i64> = r.get(2)?;
            let metrics = match stars {
                Some(n) => Some((
                    n,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, bool>(5)?,
                )),
                None => None,
            };
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?, metrics))
        })?;
        for datum in data {
            let (filter, seconds, metrics) = datum?;
            let row = rows.entry(filter.clone()).or_insert(ObservingProgress {
                filter,
                goal: None,
                accepted: 0,
                rejected: 0,
                unknown: 0,
                accepted_seconds: 0.0,
            });
            match qualify(row.goal.as_ref(), seconds, metrics) {
                Qualification::Accepted => {
                    row.accepted += 1;
                    row.accepted_seconds += seconds.unwrap();
                }
                Qualification::Rejected => row.rejected += 1,
                Qualification::Unknown => row.unknown += 1,
            }
        }
    }
    tx.commit()?;
    Ok(rows.into_values().collect())
}
