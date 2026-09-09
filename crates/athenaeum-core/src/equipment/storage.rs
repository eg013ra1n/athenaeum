//! Local catalog records only. Confirmations do not alter frames or solve rows.
use super::{
    matching,
    models::{EquipmentEvidence, EquipmentProfile},
};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS equipment_profiles (
        id INTEGER PRIMARY KEY, revision INTEGER NOT NULL, definition TEXT NOT
        NULL);
        CREATE TABLE IF NOT EXISTS equipment_frame_matches (
        frame_id INTEGER PRIMARY KEY REFERENCES frames(id) ON DELETE CASCADE,
        profile_id INTEGER NOT NULL REFERENCES equipment_profiles(id) ON DELETE
        CASCADE,
        profile_revision INTEGER NOT NULL, solved_scale REAL NOT NULL,
        solved_at TEXT NOT NULL, camera TEXT NOT NULL, binning INTEGER NOT NULL,
        confirmed_at TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_equipment_match_frame ON
        equipment_frame_matches(frame_id);
        CREATE INDEX IF NOT EXISTS idx_equipment_match_profile ON
        equipment_frame_matches(profile_id);
        ",
    )
}

pub fn profiles(conn: &Connection) -> Result<Vec<EquipmentProfile>> {
    let mut stmt = conn.prepare(
        "
        SELECT id, revision, definition FROM equipment_profiles ORDER BY id
        ",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (id, revision, json) = row?;
        let mut profile: EquipmentProfile =
            serde_json::from_str(&json).context("Invalid saved equipment profile")?;
        profile.id = id;
        profile.revision = revision;
        matching::validate(&profile)?;
        Ok(profile)
    })
    .collect()
}

pub fn save(conn: &Connection, profile: EquipmentProfile) -> Result<()> {
    matching::validate(&profile)?;
    let json = serde_json::to_string(&profile)?;
    if profile.id == 0 {
        conn.execute(
            "
        INSERT INTO equipment_profiles(revision,definition) VALUES(1,?1)
        ",
            [json],
        )?;
    } else {
        let changed = conn.execute(
            "
        UPDATE equipment_profiles SET revision=revision+1,definition=?1 WHERE
        id=?2 AND revision=?3
        ",
            params![json, profile.id, profile.revision],
        )?;
        if changed != 1 {
            bail!("Configuration changed or was removed; reload before saving");
        }
    }
    Ok(())
}

pub fn evidence(conn: &Connection, camera: &str, after_id: i64) -> Result<Vec<EquipmentEvidence>> {
    let profiles = profiles(conn)?;
    let mut stmt = conn.prepare(
            "
        SELECT f.id,fi.filename,fi.path,f.instrume,f.xbinning,f.ybinning,

        p.pixel_scale_arcsec,p.solved_at,m.profile_id,m.profile_revision,m.solved_scale,m.solved_at,m.camera,m.binning
        FROM frames f JOIN files fi ON fi.id=f.file_id JOIN plate_solves p ON
        p.frame_id=f.id
        LEFT JOIN equipment_frame_matches m ON m.frame_id=f.id
        WHERE f.instrume=?1 AND f.id>?2 ORDER BY f.id LIMIT 100
        ")?;
    let rows = stmt.query_map(params![camera, after_id], |r| {
        let scale: f64 = r.get(6)?;
        let solved_at: String = r.get(7)?;
        let bx: Option<i32> = r.get(4)?;
        let by: Option<i32> = r.get(5)?;
        let confirmed: Option<i64> = r.get(8)?;
        let revision: Option<i64> = r.get(9)?;
        let candidates = matching::candidates(&profiles, camera, bx, by, scale);
        let stale = confirmed.is_some()
            && (!candidates
                .iter()
                .any(|c| Some(c.profile.id) == confirmed && Some(c.profile.revision) == revision)
                || r.get::<_, Option<f64>>(10)? != Some(scale)
                || r.get::<_, Option<String>>(11)?.as_deref() != Some(&solved_at)
                || r.get::<_, Option<String>>(12)?.as_deref() != Some(camera)
                || r.get::<_, Option<i32>>(13)? != bx);
        Ok(EquipmentEvidence {
            frame_id: r.get(0)?,
            filename: r.get(1)?,
            path: r.get(2)?,
            camera: camera.into(),
            binning_x: bx,
            binning_y: by,
            solved_scale: scale,
            solved_at,
            candidates,
            confirmed_profile_id: confirmed,
            confirmation_stale: stale,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Explicit confirmation rechecks the displayed solve and profile revision.
/// Stale UI requests fail; no nearest-candidate auto-assignment is performed.
pub fn confirm(
    conn: &Connection,
    frame_id: i64,
    profile_id: i64,
    revision: i64,
    solved_at: &str,
    scale: f64,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let camera: String = tx.query_row(
        "
        SELECT instrume FROM frames WHERE id=?1
        ",
        [frame_id],
        |r| r.get(0),
    )?;
    let item = evidence(&tx, &camera, frame_id - 1)?
        .into_iter()
        .find(|item| item.frame_id == frame_id)
        .context("Saved solve no longer exists")?;
    if item.solved_at != solved_at
        || item.solved_scale != scale
        || !item
            .candidates
            .iter()
            .any(|c| c.profile.id == profile_id && c.profile.revision == revision)
    {
        bail!("Evidence or configuration changed; reload and review again");
    }
    tx.execute(
            "
        INSERT INTO
        equipment_frame_matches(frame_id,profile_id,profile_revision,solved_scale,solved_at,camera,binning,confirmed_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(frame_id) DO UPDATE SET

        profile_id=excluded.profile_id,profile_revision=excluded.profile_revision,solved_scale=excluded.solved_scale,

        solved_at=excluded.solved_at,camera=excluded.camera,binning=excluded.binning,confirmed_at=excluded.confirmed_at
        ",
        params![frame_id,profile_id,revision,scale,solved_at,camera,item.binning_x,chrono::Utc::now().to_rfc3339()])?;
    tx.commit()?;
    Ok(())
}
