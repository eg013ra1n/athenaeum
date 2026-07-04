// Master provenance CRUD (Phase 2 calibration library, Task 11).
//
// A row in `master_provenance` is the authoritative marker that a
// `calibration_set` is a master built *by Athenaeum* (as opposed to a master
// merely ingested from disk with no known recipe/source). `master_set_id` is
// the table's primary key and a FK to `calibration_set(id)` (ON DELETE
// CASCADE — deleting the master set cleans up its provenance row for free).

use anyhow::Result;
use rusqlite::{params, Connection};

/// Provenance for one master calibration set: the recipe that built it, the
/// exact member frames that went into it, and a stable hash of that
/// membership (see `calibration_library::register::member_hash` for why the
/// hash is uuid-based rather than content-hash-based).
#[derive(Debug, Clone, PartialEq)]
pub struct MasterProvenance {
    pub master_set_id: i64,
    pub source_set_id: Option<i64>,
    pub recipe_json: String,
    /// JSON array of the member frames' `frames.uuid` values, in the same
    /// sorted order the hash was computed over.
    pub member_frame_uuids: String,
    pub member_hash: String,
    pub created_at: String,
}

/// Insert a new provenance row. `master_set_id` is the primary key, so this
/// fails (via the underlying `UNIQUE`/`PRIMARY KEY` constraint) if a
/// provenance row already exists for that master set — provenance is written
/// once at registration time and thereafter only mutated via
/// [`update_rebuild`].
pub fn insert(conn: &Connection, p: &MasterProvenance) -> Result<()> {
    conn.execute(
        "INSERT INTO master_provenance
         (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            p.master_set_id,
            p.source_set_id,
            p.recipe_json,
            p.member_frame_uuids,
            p.member_hash,
            p.created_at,
        ],
    )?;
    Ok(())
}

/// Look up the provenance row for a master set, if any (`None` means either
/// the set isn't a master, or it's a master ingested from disk with no known
/// Athenaeum-built recipe).
pub fn get(conn: &Connection, master_set_id: i64) -> Result<Option<MasterProvenance>> {
    conn.query_row(
        "SELECT master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at
         FROM master_provenance WHERE master_set_id = ?1",
        params![master_set_id],
        |row| {
            Ok(MasterProvenance {
                master_set_id: row.get(0)?,
                source_set_id: row.get(1)?,
                recipe_json: row.get(2)?,
                member_frame_uuids: row.get(3)?,
                member_hash: row.get(4)?,
                created_at: row.get(5)?,
            })
        },
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        e => Err(e.into()),
    })
}

/// Update an existing master's recipe/hash after a rebuild-in-place (Task
/// 13). Bumps `created_at` to now — it doubles as "last (re)built at" for a
/// master, matching the file on disk being rewritten. Does NOT touch
/// `source_set_id` or `member_frame_uuids`: a rebuild recombines the SAME
/// member set (possibly with a different recipe), it doesn't relink to a
/// different source.
pub fn update_rebuild(
    conn: &Connection,
    master_set_id: i64,
    recipe_json: &str,
    member_hash: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let changed = conn.execute(
        "UPDATE master_provenance
         SET recipe_json = ?1, member_hash = ?2, created_at = ?3
         WHERE master_set_id = ?4",
        params![recipe_json, member_hash, now, master_set_id],
    )?;
    if changed == 0 {
        anyhow::bail!("no master_provenance row for master_set_id {master_set_id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn seed_master_set(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library) VALUES ('MasterDark', '2026-01-01', 1)",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// source_set_id carries a real FK to calibration_set(id) — seed one so
    /// insert() doesn't trip the constraint.
    fn seed_source_set(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn insert_then_get_roundtrips() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let master_set_id = seed_master_set(&conn);
        let source_set_id = seed_source_set(&conn);

        let p = MasterProvenance {
            master_set_id,
            source_set_id: Some(source_set_id),
            recipe_json: r#"{"combine":"median"}"#.to_string(),
            member_frame_uuids: r#"["a","b","c"]"#.to_string(),
            member_hash: "deadbeef".to_string(),
            created_at: "2026-06-28T00:00:00Z".to_string(),
        };
        insert(&conn, &p).unwrap();

        let got = get(&conn, master_set_id).unwrap().unwrap();
        assert_eq!(got, p);
    }

    #[test]
    fn get_missing_returns_none() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert!(get(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn update_rebuild_changes_recipe_and_hash_preserves_source_and_uuids() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let master_set_id = seed_master_set(&conn);
        let source_set_id = seed_source_set(&conn);
        let p = MasterProvenance {
            master_set_id,
            source_set_id: Some(source_set_id),
            recipe_json: r#"{"combine":"median"}"#.to_string(),
            member_frame_uuids: r#"["a","b"]"#.to_string(),
            member_hash: "old-hash".to_string(),
            created_at: "2026-06-28T00:00:00Z".to_string(),
        };
        insert(&conn, &p).unwrap();

        update_rebuild(&conn, master_set_id, r#"{"combine":"mean"}"#, "new-hash").unwrap();

        let got = get(&conn, master_set_id).unwrap().unwrap();
        assert_eq!(got.recipe_json, r#"{"combine":"mean"}"#);
        assert_eq!(got.member_hash, "new-hash");
        assert_eq!(got.source_set_id, Some(source_set_id));
        assert_eq!(got.member_frame_uuids, r#"["a","b"]"#);
        assert_ne!(got.created_at, "2026-06-28T00:00:00Z");
    }

    #[test]
    fn update_rebuild_missing_row_errors() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert!(update_rebuild(&conn, 999, "{}", "hash").is_err());
    }
}
