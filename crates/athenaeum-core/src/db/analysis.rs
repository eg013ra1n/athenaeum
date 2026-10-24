use crate::models::{FrameAnalysis, FrameVerdict, StarMetric};
use rusqlite::{params, Connection, Result};

/// Insert or replace frame analysis results (keyed by frame_id via UNIQUE constraint).
pub fn upsert_frame_analysis(conn: &Connection, a: &FrameAnalysis) -> Result<i64> {
    conn.execute(
        "INSERT OR REPLACE INTO frame_analysis (
            frame_id, file_id, stars_detected, median_fwhm, median_eccentricity,
            median_snr, median_hfr, frame_snr, snr_weight, psf_signal,
            background, noise, detection_threshold, width, height,
            source_channels, trail_r_squared, possibly_trailed,
            median_beta, quality_score, config_hash, analyzed_at,
            measure_version, stars_refused, frame_verdict
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![
            a.frame_id,
            a.file_id,
            a.stars_detected,
            a.median_fwhm,
            a.median_eccentricity,
            a.median_snr,
            a.median_hfr,
            a.frame_snr,
            a.snr_weight,
            a.psf_signal,
            a.background,
            a.noise,
            a.detection_threshold,
            a.width,
            a.height,
            a.source_channels,
            a.trail_r_squared,
            a.possibly_trailed,
            a.median_beta,
            a.quality_score,
            a.config_hash,
            a.analyzed_at,
            a.measure_version,
            a.stars_refused,
            a.frame_verdict.map(FrameVerdict::as_str),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get analysis for a single frame.
pub fn get_frame_analysis(conn: &Connection, frame_id: i64) -> Result<Option<FrameAnalysis>> {
    let mut stmt = conn.prepare(
        "SELECT id, frame_id, file_id, stars_detected, median_fwhm, median_eccentricity,
                median_snr, median_hfr, frame_snr, snr_weight, psf_signal,
                background, noise, detection_threshold, width, height,
                source_channels, trail_r_squared, possibly_trailed,
                median_beta, quality_score, config_hash, analyzed_at,
                measure_version, stars_refused, frame_verdict
         FROM frame_analysis WHERE frame_id = ?1",
    )?;

    let mut rows = stmt.query_map(params![frame_id], row_to_analysis)?;
    match rows.next() {
        Some(Ok(a)) => Ok(Some(a)),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

/// Get analyses for multiple frame IDs.
pub fn get_frame_analyses_by_ids(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<Vec<FrameAnalysis>> {
    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT id, frame_id, file_id, stars_detected, median_fwhm, median_eccentricity,
                median_snr, median_hfr, frame_snr, snr_weight, psf_signal,
                background, noise, detection_threshold, width, height,
                source_channels, trail_r_squared, possibly_trailed,
                median_beta, quality_score, config_hash, analyzed_at,
                measure_version, stars_refused, frame_verdict
         FROM frame_analysis WHERE frame_id IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = frame_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), row_to_analysis)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get all analyses for frames belonging to a frame set.
pub fn get_frame_analyses_for_frame_set(
    conn: &Connection,
    frame_set_id: i64,
) -> Result<Vec<FrameAnalysis>> {
    let mut stmt = conn.prepare(
        "SELECT fa.id, fa.frame_id, fa.file_id, fa.stars_detected, fa.median_fwhm, fa.median_eccentricity,
                fa.median_snr, fa.median_hfr, fa.frame_snr, fa.snr_weight, fa.psf_signal,
                fa.background, fa.noise, fa.detection_threshold, fa.width, fa.height,
                fa.source_channels, fa.trail_r_squared, fa.possibly_trailed,
                fa.median_beta, fa.quality_score, fa.config_hash, fa.analyzed_at,
                fa.measure_version, fa.stars_refused, fa.frame_verdict
         FROM frame_analysis fa
         INNER JOIN session_members sm ON sm.frame_id = fa.frame_id
         INNER JOIN sessions s ON s.id = sm.session_id
         INNER JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE n.frames_set_id = ?1"
    )?;

    let rows = stmt.query_map(params![frame_set_id], row_to_analysis)?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Bulk-insert star metrics for a frame analysis.
/// Deletes any existing stars for this analysis_id first, then inserts all new ones.
pub fn upsert_star_metrics(
    conn: &Connection,
    analysis_id: i64,
    stars: &[StarMetric],
) -> Result<()> {
    conn.execute(
        "DELETE FROM star_metrics WHERE frame_analysis_id = ?1",
        params![analysis_id],
    )?;

    let mut stmt = conn.prepare(
        "INSERT INTO star_metrics (
            frame_analysis_id, x, y, peak, flux, fwhm, fwhm_x, fwhm_y,
            eccentricity, snr, hfr, theta, beta, fit_method, fit_residual
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;

    for s in stars {
        stmt.execute(params![
            analysis_id,
            s.x,
            s.y,
            s.peak,
            s.flux,
            s.fwhm,
            s.fwhm_x,
            s.fwhm_y,
            s.eccentricity,
            s.snr,
            s.hfr,
            s.theta,
            s.beta,
            s.fit_method,
            s.fit_residual,
        ])?;
    }
    Ok(())
}

/// Get all star metrics for a frame analysis.
pub fn get_star_metrics(conn: &Connection, analysis_id: i64) -> Result<Vec<StarMetric>> {
    let mut stmt = conn.prepare(
        "SELECT id, frame_analysis_id, x, y, peak, flux, fwhm, fwhm_x, fwhm_y,
                eccentricity, snr, hfr, theta, beta, fit_method, fit_residual
         FROM star_metrics WHERE frame_analysis_id = ?1",
    )?;

    let rows = stmt.query_map(params![analysis_id], |row| {
        Ok(StarMetric {
            id: row.get(0)?,
            frame_analysis_id: row.get(1)?,
            x: row.get(2)?,
            y: row.get(3)?,
            peak: row.get(4)?,
            flux: row.get(5)?,
            fwhm: row.get(6)?,
            fwhm_x: row.get(7)?,
            fwhm_y: row.get(8)?,
            eccentricity: row.get(9)?,
            snr: row.get(10)?,
            hfr: row.get(11)?,
            theta: row.get(12)?,
            beta: row.get(13)?,
            fit_method: row.get(14)?,
            fit_residual: row.get(15)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get star metrics for a frame by frame_id (joins through frame_analysis).
pub fn get_star_metrics_by_frame_id(conn: &Connection, frame_id: i64) -> Result<Vec<StarMetric>> {
    let mut stmt = conn.prepare(
        "SELECT sm.id, sm.frame_analysis_id, sm.x, sm.y, sm.peak, sm.flux,
                sm.fwhm, sm.fwhm_x, sm.fwhm_y, sm.eccentricity, sm.snr,
                sm.hfr, sm.theta, sm.beta, sm.fit_method, sm.fit_residual
         FROM star_metrics sm
         INNER JOIN frame_analysis fa ON fa.id = sm.frame_analysis_id
         WHERE fa.frame_id = ?1",
    )?;

    let rows = stmt.query_map(params![frame_id], |row| {
        Ok(StarMetric {
            id: row.get(0)?,
            frame_analysis_id: row.get(1)?,
            x: row.get(2)?,
            y: row.get(3)?,
            peak: row.get(4)?,
            flux: row.get(5)?,
            fwhm: row.get(6)?,
            fwhm_x: row.get(7)?,
            fwhm_y: row.get(8)?,
            eccentricity: row.get(9)?,
            snr: row.get(10)?,
            hfr: row.get(11)?,
            theta: row.get(12)?,
            beta: row.get(13)?,
            fit_method: row.get(14)?,
            fit_residual: row.get(15)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// The stored verdict text; an unknown one (a newer build's) reads as none, with a warning.
fn verdict_of(text: Option<String>) -> Option<FrameVerdict> {
    let text = text?;
    let v = FrameVerdict::parse(&text);
    if v.is_none() {
        tracing::warn!(frame_verdict = %text, "frame_analysis: unknown frame_verdict text read as none");
    }
    v
}

fn row_to_analysis(row: &rusqlite::Row) -> Result<FrameAnalysis> {
    Ok(FrameAnalysis {
        id: row.get(0)?,
        frame_id: row.get(1)?,
        file_id: row.get(2)?,
        stars_detected: row.get(3)?,
        median_fwhm: row.get(4)?,
        median_eccentricity: row.get(5)?,
        median_snr: row.get(6)?,
        median_hfr: row.get(7)?,
        frame_snr: row.get(8)?,
        snr_weight: row.get(9)?,
        psf_signal: row.get(10)?,
        background: row.get(11)?,
        noise: row.get(12)?,
        detection_threshold: row.get(13)?,
        width: row.get(14)?,
        height: row.get(15)?,
        source_channels: row.get(16)?,
        trail_r_squared: row.get(17)?,
        possibly_trailed: row.get(18)?,
        median_beta: row.get(19)?,
        quality_score: row.get(20)?,
        config_hash: row.get(21)?,
        analyzed_at: row.get(22)?,
        measure_version: row.get(23)?,
        stars_refused: row.get(24)?,
        frame_verdict: verdict_of(row.get::<_, Option<String>>(25)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    /// Insert a `files` + `frames` row pair, returning `(file_id, frame_id)`.
    fn insert_frame(conn: &Connection, path: &str) -> (i64, i64) {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, 'a.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [path],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Light')",
            [file_id],
        )
        .unwrap();
        let frame_id = conn
            .query_row("SELECT id FROM frames WHERE file_id = ?1", [file_id], |r| {
                r.get(0)
            })
            .unwrap();
        (file_id, frame_id)
    }

    fn analysis(frame_id: i64, file_id: i64) -> FrameAnalysis {
        FrameAnalysis {
            id: None,
            frame_id,
            file_id,
            stars_detected: 42,
            median_fwhm: 3.1,
            median_eccentricity: 0.4,
            median_snr: 12.0,
            median_hfr: 1.8,
            frame_snr: 25.0,
            snr_weight: 1.0,
            psf_signal: 100.0,
            background: 500.0,
            noise: 5.0,
            detection_threshold: 20.0,
            width: 4096,
            height: 4096,
            source_channels: 1,
            trail_r_squared: 0.1,
            possibly_trailed: false,
            median_beta: None,
            quality_score: Some(0.9),
            config_hash: None,
            analyzed_at: "2026-01-01T00:00:00Z".into(),
            measure_version: None,
            stars_refused: None,
            frame_verdict: None,
        }
    }

    fn star(analysis_id: i64) -> StarMetric {
        StarMetric {
            id: None,
            frame_analysis_id: analysis_id,
            x: 10.0,
            y: 20.0,
            peak: 1000.0,
            flux: 5000.0,
            fwhm: 3.0,
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            eccentricity: 0.2,
            snr: Some(30.0),
            hfr: Some(1.5),
            theta: 0.0,
            beta: None,
            fit_method: "gaussian".into(),
            fit_residual: 0.01,
        }
    }

    fn counts(conn: &Connection) -> (i64, i64) {
        let a = conn
            .query_row("SELECT COUNT(*) FROM frame_analysis", [], |r| r.get(0))
            .unwrap();
        let s = conn
            .query_row("SELECT COUNT(*) FROM star_metrics", [], |r| r.get(0))
            .unwrap();
        (a, s)
    }

    /// Happy path: the persist loop's two writes land together on commit.
    #[test]
    fn persist_unit_commits_analyses_and_their_stars() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (file_id, frame_id) = insert_frame(&conn, "/x/a.fits");

        let tx = conn.unchecked_transaction().unwrap();
        let analysis_id = upsert_frame_analysis(&conn, &analysis(frame_id, file_id)).unwrap();
        upsert_star_metrics(&conn, analysis_id, &[star(analysis_id)]).unwrap();
        tx.commit().unwrap();

        assert_eq!(counts(&conn), (1, 1));
    }

    /// audit M11: the analysis persist phase is ONE unit. A mid-loop failure
    /// (or a failed COMMIT) returns early and drops the transaction — every
    /// analysis AND its star metrics must vanish with it. The old code
    /// hand-rolled `ROLLBACK` inside `map_err` closures, which left the
    /// COMMIT-failure path entirely unguarded.
    #[test]
    fn dropped_persist_transaction_rolls_back_analyses_and_stars() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (file_a, frame_a) = insert_frame(&conn, "/x/a.fits");
        let (file_b, frame_b) = insert_frame(&conn, "/x/b.fits");

        let tx = conn.unchecked_transaction().unwrap();
        let id_a = upsert_frame_analysis(&conn, &analysis(frame_a, file_a)).unwrap();
        upsert_star_metrics(&conn, id_a, &[star(id_a)]).unwrap();
        upsert_frame_analysis(&conn, &analysis(frame_b, file_b)).unwrap();
        assert_eq!(counts(&conn), (2, 1), "writes are visible inside the tx");

        drop(tx); // the `?` early-return path — rollback, no manual ROLLBACK

        assert_eq!(
            counts(&conn),
            (0, 0),
            "dropping the persist transaction must undo analyses and stars alike"
        );
    }

    /// Today's (pre-C6) `frame_analysis` and `star_metrics`, with one row each and two stars. Only the two tables: the
    /// foreign keys are not enforced on a bare connection.
    fn old_catalog() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE frame_analysis (
                id INTEGER PRIMARY KEY AUTOINCREMENT, frame_id INTEGER NOT NULL, file_id INTEGER NOT NULL,
                stars_detected INTEGER NOT NULL, median_fwhm REAL NOT NULL, median_eccentricity REAL NOT NULL,
                median_snr REAL NOT NULL, median_hfr REAL NOT NULL, frame_snr REAL NOT NULL,
                snr_weight REAL NOT NULL, psf_signal REAL NOT NULL, background REAL NOT NULL, noise REAL NOT NULL,
                detection_threshold REAL NOT NULL, width INTEGER NOT NULL, height INTEGER NOT NULL,
                source_channels INTEGER NOT NULL, trail_r_squared REAL NOT NULL DEFAULT 0.0,
                possibly_trailed INTEGER NOT NULL DEFAULT 0, median_beta REAL, quality_score REAL, config_hash TEXT,
                analyzed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(frame_id));
            CREATE TABLE star_metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT, frame_analysis_id INTEGER NOT NULL, x REAL NOT NULL,
                y REAL NOT NULL, peak REAL NOT NULL, flux REAL NOT NULL, fwhm REAL NOT NULL, fwhm_x REAL NOT NULL,
                fwhm_y REAL NOT NULL, eccentricity REAL NOT NULL, snr REAL NOT NULL, hfr REAL NOT NULL,
                theta REAL NOT NULL, beta REAL, fit_method TEXT NOT NULL, fit_residual REAL NOT NULL);
            CREATE INDEX idx_star_metrics_analysis_id ON star_metrics(frame_analysis_id);
            INSERT INTO frame_analysis (frame_id, file_id, stars_detected, median_fwhm, median_eccentricity,
                median_snr, median_hfr, frame_snr, snr_weight, psf_signal, background, noise, detection_threshold,
                width, height, source_channels, trail_r_squared, possibly_trailed, median_beta, quality_score,
                config_hash, analyzed_at)
            VALUES (7, 7, 42, 3.1, 0.4, 12.0, 1.8, 25.0, 1.0, 100.0, 500.0, 5.0, 20.0, 4096, 4096, 1, 0.1, 1, NULL,
                0.9, 'old', '2026-01-01T00:00:00Z');
            INSERT INTO star_metrics (frame_analysis_id, x, y, peak, flux, fwhm, fwhm_x, fwhm_y, eccentricity, snr,
                hfr, theta, beta, fit_method, fit_residual)
            VALUES (1, 10.0, 20.0, 1000.0, 5000.0, 3.0, 3.0, 3.0, 0.2, 30.0, 1.5, 0.0, NULL, 'gaussian', 0.01),
                   (1, 11.0, 21.0, 900.0, 4000.0, 3.2, 3.2, 3.2, 0.3, 25.0, 1.6, 0.1, 4.0, 'moffat', 0.02);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn migration_adds_measure_version_and_stars_refused_and_keeps_rows() {
        let conn = old_catalog();
        init_db(&conn).unwrap();
        let a = get_frame_analysis(&conn, 7).unwrap().expect("the row survives");
        assert_eq!((a.measure_version, a.stars_refused, a.frame_verdict), (None, None, None));
        let want = analysis(7, 7);
        assert_eq!(
            (a.stars_detected, a.median_fwhm, a.median_eccentricity, a.median_snr, a.median_hfr),
            (42, 3.1, 0.4, 12.0, 1.8)
        );
        assert_eq!((a.frame_snr, a.background, a.noise), (want.frame_snr, want.background, want.noise));
        assert_eq!((a.width, a.trail_r_squared, a.possibly_trailed), (4096, 0.1, true));
        assert_eq!((a.quality_score, a.config_hash.as_deref()), (Some(0.9), Some("old")));
        // A second init is a no-op.
        init_db(&conn).unwrap();
        assert_eq!(counts(&conn), (1, 2));
    }

    #[test]
    fn star_metrics_rebuild_keeps_rows_and_accepts_null_snr() {
        let conn = old_catalog();
        init_db(&conn).unwrap();
        let stars = get_star_metrics(&conn, 1).unwrap();
        assert_eq!(stars.len(), 2);
        assert_eq!((stars[0].snr, stars[0].hfr, stars[0].fit_method.as_str()), (Some(30.0), Some(1.5), "gaussian"));
        assert_eq!((stars[1].beta, stars[1].x, stars[1].fit_residual), (Some(4.0), 11.0, 0.02));
        let mut s = star(1);
        s.snr = None;
        s.hfr = None;
        // The upsert replaces the analysis's stars: one row with NULL snr / hfr.
        upsert_star_metrics(&conn, 1, &[s]).unwrap();
        let back = get_star_metrics(&conn, 1).unwrap();
        assert_eq!((back[0].snr, back[0].hfr), (None, None));
        // A second init neither rebuilds nor loses anything, and the index is present.
        init_db(&conn).unwrap();
        assert_eq!(counts(&conn), (1, 1));
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_star_metrics_analysis_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn frame_analysis_round_trips_measure_version_stars_refused_and_verdict() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (file_id, frame_id) = insert_frame(&conn, "/x/a.fits");
        let mut a = analysis(frame_id, file_id);
        a.measure_version = Some(5);
        a.stars_refused = Some(17);
        a.frame_verdict = Some(FrameVerdict::NoStars);
        upsert_frame_analysis(&conn, &a).unwrap();
        let back = get_frame_analysis(&conn, frame_id).unwrap().unwrap();
        assert_eq!((back.measure_version, back.stars_refused, back.frame_verdict), (Some(5), Some(17), Some(FrameVerdict::NoStars)));
        let text: String = conn
            .query_row("SELECT frame_verdict FROM frame_analysis", [], |r| r.get(0))
            .unwrap();
        assert_eq!(text, "no_stars");
        let by_ids = get_frame_analyses_by_ids(&conn, &[frame_id]).unwrap();
        assert_eq!(by_ids[0].frame_verdict, Some(FrameVerdict::NoStars));
    }

    #[test]
    fn an_old_frame_analysis_json_decodes_with_none_and_an_unknown_key_decodes() {
        let mut v = serde_json::to_value(analysis(1, 1)).unwrap();
        let o = v.as_object_mut().unwrap();
        for k in ["measureVersion", "measure_version", "starsRefused", "stars_refused", "frameVerdict", "frame_verdict"] {
            o.remove(k);
        }
        let old: FrameAnalysis = serde_json::from_value(v.clone()).unwrap();
        assert_eq!((old.measure_version, old.stars_refused, old.frame_verdict), (None, None, None));
        v.as_object_mut().unwrap().insert("someFutureKey".into(), serde_json::json!(1));
        serde_json::from_value::<FrameAnalysis>(v).expect("an unknown key decodes");
        // The new keys round-trip through the wire form.
        let mut a = analysis(1, 1);
        a.frame_verdict = Some(FrameVerdict::Trailed);
        let j = serde_json::to_value(&a).unwrap();
        assert_eq!(serde_json::from_value::<FrameAnalysis>(j).unwrap().frame_verdict, Some(FrameVerdict::Trailed));
    }
}
