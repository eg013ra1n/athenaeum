//! One-time catalog repairs keyed by `settings` flags — data fixes that the
//! guarded-`ALTER TABLE` migrations in `schema.rs` can't express because they
//! need Rust-side parsing of stored blobs.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::fits_parser::stored_header::{cfa_fields_from_keys, parse_stored_header_keys};
use crate::models::FileFormat;

const CFA_BACKFILL_FLAG: &str = "repair.cfa_backfill_v1";

/// Back-fill NULL CFA columns (`bayerpat`/`xbayroff`/`ybayroff`/`roworder`)
/// on `frames` from the stored `fits_header` blob.
///
/// Frames that arrived over sync/collab before the
/// `get_frames_with_files_by_ids` projection fix had their CFA columns
/// erased in transit even though the re-extracted header blob beside them
/// still carries the cards — which starves `resolve_cfa_geometry` and the
/// Bayer card copy-through fallback on the receiving device. Runs once per
/// catalog (settings flag), fills only NULLs, and never touches
/// `frames.override`: the filled values restate the file's own header, so
/// there is nothing for the scanner to undo.
pub fn backfill_cfa_from_stored_headers(conn: &Connection) -> Result<usize> {
    let already: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![CFA_BACKFILL_FLAG],
            |r| r.get(0),
        )
        .optional()?;
    if already.is_some() {
        return Ok(0);
    }

    let mut stmt = conn.prepare(
        "SELECT fr.id, f.format, fh.header
         FROM frames fr
         JOIN files f ON f.id = fr.file_id
         JOIN fits_header fh ON fh.file_id = f.id
         WHERE fr.bayerpat IS NULL AND fh.header LIKE '%BAYERPAT%'",
    )?;
    let rows: Vec<(i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut repaired = 0usize;
    for (frame_id, format, header) in rows {
        let format = if format == "FITS" {
            FileFormat::FITS
        } else {
            FileFormat::XISF
        };
        let keys: HashMap<String, String> = parse_stored_header_keys(format, &header);
        let cfa = cfa_fields_from_keys(&keys);
        let Some(bayerpat) = cfa.bayerpat else {
            continue;
        };
        let changed = conn.execute(
            "UPDATE frames SET bayerpat = ?2,
                    xbayroff = COALESCE(xbayroff, ?3),
                    ybayroff = COALESCE(ybayroff, ?4),
                    roworder = COALESCE(roworder, ?5)
             WHERE id = ?1 AND bayerpat IS NULL",
            params![frame_id, bayerpat, cfa.xbayroff, cfa.ybayroff, cfa.roworder],
        )?;
        repaired += changed;
    }

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES (?1, 'done', datetime('now'))",
        params![CFA_BACKFILL_FLAG],
    )?;
    if repaired > 0 {
        tracing::info!(
            count = repaired,
            "cfa columns back-filled from stored headers"
        );
    }
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    const OSC_HEADER: &str =
        "BAYERPAT= 'RGGB'\nXBAYROFF= 1\nYBAYROFF= 0\nROWORDER= 'BOTTOM-UP'\nEND";

    /// init_db itself runs the repair and stamps the flag — clear it so each
    /// test exercises a fresh run.
    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![CFA_BACKFILL_FLAG],
        )
        .unwrap();
        conn
    }

    fn insert_frame_with_header(conn: &Connection, id: i64, bayerpat: Option<&str>, header: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, '/t/f' || ?1 || '.fits', 'f.fits', 1, '2026-01-01T00:00:00+00:00', 'FITS')",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, bayerpat, override) VALUES (?1, ?1, 'Light', ?2, 0)",
            params![id, bayerpat],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![id, header],
        )
        .unwrap();
    }

    #[test]
    fn fills_null_cfa_from_blob_and_stamps_flag() {
        let conn = fresh_conn();
        insert_frame_with_header(&conn, 1, None, OSC_HEADER);

        let repaired = backfill_cfa_from_stored_headers(&conn).unwrap();
        assert_eq!(repaired, 1);

        let (bp, xo, yo, ro, ov): (String, i64, i64, String, i64) = conn
            .query_row(
                "SELECT bayerpat, xbayroff, ybayroff, roworder, override FROM frames WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(bp, "RGGB");
        assert_eq!(xo, 1);
        assert_eq!(yo, 0);
        assert_eq!(ro, "BOTTOM-UP");
        assert_eq!(ov, 0, "repair must not set the override flag");

        // Second run: flag stamped, nothing rescanned.
        assert_eq!(backfill_cfa_from_stored_headers(&conn).unwrap(), 0);
    }

    #[test]
    fn existing_bayerpat_and_mono_rows_are_untouched() {
        let conn = fresh_conn();
        insert_frame_with_header(&conn, 1, Some("GBRG"), OSC_HEADER); // already set — keep
        insert_frame_with_header(&conn, 2, None, "EXPTIME = 300.0\nEND"); // mono — no cards

        let repaired = backfill_cfa_from_stored_headers(&conn).unwrap();
        assert_eq!(repaired, 0);

        let bp: String = conn
            .query_row("SELECT bayerpat FROM frames WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bp, "GBRG");
        let bp2: Option<String> = conn
            .query_row("SELECT bayerpat FROM frames WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert!(bp2.is_none());
    }

    /// Pins the `init_db` tail call itself: startup alone must repair a
    /// catalog that already holds sync-erased rows. Deleting the call in
    /// `schema.rs` leaves the two tests above green (they invoke the repair
    /// directly) — only this one goes red. File-backed on purpose, so the
    /// run also proves the tail call sits *after* the guarded-`ALTER TABLE`
    /// migrations that add the CFA columns it writes.
    #[test]
    fn init_db_repairs_a_catalog_that_already_holds_erased_rows() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("catalog.db");

        // A catalog created before the fix: schema present, flag not yet
        // stamped, and a frame whose CFA columns were erased in transit
        // while its header blob kept the Bayer cards.
        let conn = Connection::open(&db_path).unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![CFA_BACKFILL_FLAG],
        )
        .unwrap();
        insert_frame_with_header(&conn, 1, None, OSC_HEADER);

        // Next app start — init_db and nothing else.
        init_db(&conn).unwrap();

        let bp: Option<String> = conn
            .query_row("SELECT bayerpat FROM frames WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            bp.as_deref(),
            Some("RGGB"),
            "init_db must run the CFA back-fill on an already-corrupted catalog"
        );
        let flag: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![CFA_BACKFILL_FLAG],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(flag.as_deref(), Some("done"), "flag must be stamped");
    }
}
