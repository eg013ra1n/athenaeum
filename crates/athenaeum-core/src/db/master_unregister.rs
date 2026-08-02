//! Reverse of `calibration_library::register::register_master` at the DB
//! layer: repoint consumers back onto the raw source set, un-supersede it,
//! drop provenance and the master's shell row. Runs inside the CALLER's
//! transaction; file rows / disk files are the caller's responsibility
//! (`file_ids` says which).
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// What [`unregister_master_set`] undid. `file_ids` are the master's own
/// files — still present in `files`/`frames` when this returns; disposing of
/// them (Black Hole, delete, leave alone) is the caller's decision.
pub struct MasterUnregisterSummary {
    pub master_set_id: i64,
    pub restored_raw_set_id: Option<i64>,
    /// Consumer links moved back onto the raw set — or, when there is no raw
    /// set to fall back to, deleted.
    pub links_repointed: usize,
    pub file_ids: Vec<i64>,
}

/// Undo a master registration: consumers go back to the raw source set, the
/// raw set becomes matchable again, provenance and the master's shell row go
/// away.
///
/// Errors if `master_set_id` is unknown or is not a master library set —
/// unregistering an ordinary calibration set would silently shred its links.
///
/// Runs in the CALLER's transaction (it opens none of its own), so a caller
/// that also deletes files can roll the whole thing back as one unit.
pub fn unregister_master_set(
    conn: &Connection,
    master_set_id: i64,
) -> Result<MasterUnregisterSummary> {
    let is_master: i64 = conn
        .query_row(
            "SELECT COALESCE(is_master_library, 0) FROM calibration_set WHERE id = ?1",
            params![master_set_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("calibration set {master_set_id} not found"))?;
    if is_master == 0 {
        bail!("set {master_set_id} is not a master library set");
    }

    let raw_set_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM calibration_set WHERE superseded_by_set_id = ?1 ORDER BY id LIMIT 1",
            params![master_set_id],
            |r| r.get(0),
        )
        .optional()?;

    // Consumers of the master. With a raw set to fall back to this is exactly
    // register_master's relink UPDATE run backwards (is_manual_override and
    // match_score ride along untouched); an imported master has no lineage, so
    // its links would dangle and are dropped instead.
    let links_repointed = match raw_set_id {
        Some(raw) => conn.execute(
            "UPDATE calibration_set_to_frames SET calibration_set_id = ?1 WHERE calibration_set_id = ?2",
            params![raw, master_set_id],
        )?,
        None => conn.execute(
            "DELETE FROM calibration_set_to_frames WHERE calibration_set_id = ?1",
            params![master_set_id],
        )?,
    };
    // Un-supersede BEFORE the master row goes: superseded_by_set_id is a
    // NO-ACTION FK, so a leftover pointer would abort the DELETE below.
    conn.execute(
        "UPDATE calibration_set SET superseded_by_set_id = NULL WHERE superseded_by_set_id = ?1",
        params![master_set_id],
    )?;
    conn.execute(
        "DELETE FROM master_provenance WHERE master_set_id = ?1",
        params![master_set_id],
    )?;
    // Sub-cal links the master held as a SOURCE (e.g. a master flat's dark
    // link). The set-delete trigger would also clear these; doing it here
    // keeps the reversal self-contained rather than trigger-dependent.
    conn.execute(
        "DELETE FROM calibration_set_to_frames WHERE source_type = 'calibration_set' AND source_id = ?1",
        params![master_set_id],
    )?;

    let mut stmt = conn.prepare(
        "SELECT fr.file_id FROM calibration_set_frames csf
           JOIN frames fr ON fr.id = csf.frame_id
          WHERE csf.set_id = ?1",
    )?;
    let file_ids: Vec<i64> = stmt
        .query_map(params![master_set_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    conn.execute(
        "DELETE FROM calibration_set_frames WHERE set_id = ?1",
        params![master_set_id],
    )?;
    // The empty-set prune trigger exempts master rows, so the shell survives
    // losing its last member — this explicit DELETE is what removes it.
    conn.execute(
        "DELETE FROM calibration_set WHERE id = ?1",
        params![master_set_id],
    )?;

    tracing::info!(
        master_set_id,
        restored_raw_set_id = ?raw_set_id,
        links_repointed,
        count = file_ids.len(),
        "master unregistered"
    );
    Ok(MasterUnregisterSummary {
        master_set_id,
        restored_raw_set_id: raw_set_id,
        links_repointed,
        file_ids,
    })
}

/// The master library set a file belongs to, if any — the lookup that turns
/// "user is deleting this file" into "this is a master, unregister it first".
pub fn master_set_id_for_file(conn: &Connection, file_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT cs.id FROM calibration_set cs
               JOIN calibration_set_frames csf ON csf.set_id = cs.id
               JOIN frames fr ON fr.id = csf.frame_id
              WHERE fr.file_id = ?1 AND COALESCE(cs.is_master_library, 0) = 1
              LIMIT 1",
            params![file_id],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    /// A `files` + `frames` pair for one master file, returning `(file_id,
    /// frame_id)`. FK enforcement is on by default in this codebase, so both
    /// rows have to exist before `calibration_set_frames` can reference them.
    fn seed_master_file(conn: &Connection, name: &str, imagetyp: &str) -> (i64, i64) {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 100, '2026-08-01T00:00:00Z', 'FITS')",
            rusqlite::params![format!("/lib/{name}"), name],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, ?2, 1)",
            rusqlite::params![file_id, imagetyp],
        )
        .unwrap();
        (file_id, conn.last_insert_rowid())
    }

    /// The end state `register_master` leaves behind, built by hand: a raw set
    /// superseded by a master set that owns one file, carries provenance, and
    /// has inherited the raw set's consumer link.
    ///
    /// Returns `(raw_set_id, master_set_id, master_file_id, consumer_frame_id)`.
    fn seed_registered_master(conn: &Connection) -> (i64, i64, i64, i64) {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
            [],
        )
        .unwrap();
        let raw_set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library)
             VALUES ('MasterDark', '2026-08-01', 1)",
            [],
        )
        .unwrap();
        let master_set_id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            rusqlite::params![master_set_id, raw_set_id],
        )
        .unwrap();

        let (master_file_id, master_frame_id) = seed_master_file(conn, "m.fits", "MASTERDARK");
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![master_set_id, master_frame_id],
        )
        .unwrap();
        // The raw set keeps its own members across registration — they are the
        // frames the master was integrated from, and unregister must not touch
        // them (nor report their files as the master's).
        let (_, raw_frame_id) = seed_master_file(conn, "raw1.fits", "DARK");
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![raw_set_id, raw_frame_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO master_provenance
             (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (?1, ?2, '{}', '[]', 'h', '2026-08-02T00:00:00Z')",
            rusqlite::params![master_set_id, raw_set_id],
        )
        .unwrap();

        // A Light frame consumer whose link register_master moved onto the master.
        let (_, consumer_frame_id) = seed_master_file(conn, "light.fits", "LIGHT");
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
             VALUES (?1, 'frame', ?2, 'Dark', 0.9, 1)",
            rusqlite::params![consumer_frame_id, master_set_id],
        )
        .unwrap();

        (raw_set_id, master_set_id, master_file_id, consumer_frame_id)
    }

    #[test]
    fn unregister_restores_the_raw_lineage() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (raw, master, master_file_id, consumer) = seed_registered_master(&conn);

        let s = unregister_master_set(&conn, master).unwrap();

        assert_eq!(s.master_set_id, master);
        assert_eq!(s.restored_raw_set_id, Some(raw));
        assert_eq!(s.links_repointed, 1);
        assert_eq!(
            s.file_ids,
            vec![master_file_id],
            "only the master's own files are reported — not the raw members'"
        );

        let (target, manual): (i64, i64) = conn
            .query_row(
                "SELECT calibration_set_id, is_manual_override FROM calibration_set_to_frames
                  WHERE source_id = ?1 AND source_type = 'frame'",
                [consumer],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, raw, "consumer link repointed back to the raw set");
        assert_eq!(manual, 1, "manual-override flag survives the repoint");

        let sup: Option<i64> = conn
            .query_row(
                "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
                [raw],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sup, None, "raw set is matchable again");

        let masters: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [master],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(masters, 0, "master shell row is gone");

        let prov: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM master_provenance WHERE master_set_id = ?1",
                [master],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prov, 0, "provenance dropped");

        let members: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1",
                [master],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(members, 0, "membership row dropped");

        let raw_members: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1",
                [raw],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw_members, 1, "the raw set keeps its own members");

        // The file row itself is the caller's business — the primitive only
        // reports it.
        let files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                [master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(files, 1, "file row is left to the caller");
    }

    #[test]
    fn unregister_refuses_non_master_and_unknown_sets() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (raw, _master, _, _) = seed_registered_master(&conn);

        assert!(
            unregister_master_set(&conn, raw).is_err(),
            "refuses non-master sets"
        );
        assert!(
            unregister_master_set(&conn, 9_999).is_err(),
            "refuses sets that do not exist"
        );
    }

    #[test]
    fn unregister_imported_master_deletes_its_links() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // An imported master: is_master_library = 1, no provenance, no raw set
        // superseded by it — there is nothing to fall back to.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library)
             VALUES ('MasterFlat', '2026-08-01', 1)",
            [],
        )
        .unwrap();
        let master = conn.last_insert_rowid();
        let (master_file_id, master_frame_id) =
            seed_master_file(&conn, "imported.fits", "MASTERFLAT");
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![master, master_frame_id],
        )
        .unwrap();
        let (_, consumer) = seed_master_file(&conn, "l2.fits", "LIGHT");
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Flat')",
            rusqlite::params![consumer, master],
        )
        .unwrap();

        let s = unregister_master_set(&conn, master).unwrap();

        assert_eq!(s.restored_raw_set_id, None, "no lineage to restore");
        assert_eq!(s.links_repointed, 1, "the orphaned link is counted");
        assert_eq!(s.file_ids, vec![master_file_id]);

        let links: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_to_frames WHERE source_id = ?1",
                [consumer],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(links, 0, "consumer link deleted, not left dangling");
        let masters: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [master],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(masters, 0, "master shell row is gone");
    }

    #[test]
    fn unregister_drops_links_the_master_held_as_source() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // A master flat that itself consumes a dark set: the link row has the
        // master as SOURCE (source_type='calibration_set'), not as target, so
        // the consumer repoint never touches it.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library)
             VALUES ('MasterFlat', '2026-08-01', 1)",
            [],
        )
        .unwrap();
        let master_flat = conn.last_insert_rowid();
        let (_, flat_frame_id) = seed_master_file(&conn, "mflat.fits", "MASTERFLAT");
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![master_flat, flat_frame_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
            [],
        )
        .unwrap();
        let dark_set = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'calibration_set', ?2, 'Dark')",
            rusqlite::params![master_flat, dark_set],
        )
        .unwrap();

        let s = unregister_master_set(&conn, master_flat).unwrap();
        assert_eq!(
            s.links_repointed, 0,
            "a link the master held as source is not a consumer of it"
        );

        let held: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_to_frames
                  WHERE source_type = 'calibration_set' AND source_id = ?1",
                [master_flat],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(held, 0, "the master's own sub-cal link is gone");
        let dark_alive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [dark_set],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dark_alive, 1, "the sub-cal's target set is untouched");
    }

    #[test]
    fn master_set_id_for_file_finds_only_master_sets() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (raw, master, master_file_id, _) = seed_registered_master(&conn);

        assert_eq!(
            master_set_id_for_file(&conn, master_file_id).unwrap(),
            Some(master)
        );

        // A raw member of the (non-master) source set is not a master file.
        let (raw_file_id, raw_frame_id) = seed_master_file(&conn, "raw.fits", "DARK");
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![raw, raw_frame_id],
        )
        .unwrap();
        assert_eq!(master_set_id_for_file(&conn, raw_file_id).unwrap(), None);

        // A file in no calibration set at all.
        let (loose_file_id, _) = seed_master_file(&conn, "loose.fits", "LIGHT");
        assert_eq!(master_set_id_for_file(&conn, loose_file_id).unwrap(), None);
    }
}
