//! Additive goals only; read-only progress never alters acquisition metadata.
use super::{models::ObservingGoal, policy};
use anyhow::{bail, Result};
use rusqlite::{params, Connection};

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS observing_goals (
        frame_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE
        CASCADE,
        filter TEXT NOT NULL, revision INTEGER NOT NULL, definition TEXT NOT
        NULL,
        PRIMARY KEY(frame_set_id,filter));
        ",
    )
}

pub fn goals(conn: &Connection, id: i64) -> Result<Vec<ObservingGoal>> {
    let mut stmt = conn.prepare(
        "
        SELECT filter,revision,definition FROM observing_goals WHERE
        frame_set_id=?1 ORDER BY filter
        ",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (filter, revision, json) = row?;
        let mut goal: ObservingGoal = serde_json::from_str(&json)?;
        goal.frame_set_id = id;
        goal.filter = filter;
        goal.revision = revision;
        policy::validate(&goal)?;
        Ok(goal)
    })
    .collect()
}

pub fn save(conn: &Connection, goal: ObservingGoal) -> Result<()> {
    policy::validate(&goal)?;
    let json = serde_json::to_string(&goal)?;
    let changed = if goal.revision == 0 {
        conn.execute(
            "
        INSERT INTO observing_goals(frame_set_id,filter,revision,definition)
        VALUES(?1,?2,1,?3) ON CONFLICT(frame_set_id,filter) DO NOTHING
        ",
            params![goal.frame_set_id, goal.filter, json],
        )?
    } else {
        conn.execute(
            "
        UPDATE observing_goals SET definition=?3,revision=revision+1 WHERE
        frame_set_id=?1 AND filter=?2 AND revision=?4
        ",
            params![goal.frame_set_id, goal.filter, json, goal.revision],
        )?
    };
    if changed != 1 {
        bail!("Goal changed; refresh before saving");
    }
    Ok(())
}

pub fn remove(conn: &Connection, id: i64, filter: &str, revision: i64) -> Result<()> {
    if conn.execute(
        "
        DELETE FROM observing_goals WHERE frame_set_id=?1 AND filter=?2 AND revision=?3
        ",
        params![id, filter, revision],
    )? != 1
    {
        bail!("Goal changed; refresh before removing");
    }
    Ok(())
}
