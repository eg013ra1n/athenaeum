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
            measure_version, stars_refused, frame_verdict,
            zero_point, zp_stars, zp_sigma, zp_version, zp_catalog,
            zp_refusal, zp_list_stars, zp_window_bright, zp_window_slope
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                  ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34)",
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
            a.zero_point,
            a.zp_stars,
            a.zp_sigma,
            a.zp_version,
            a.zp_catalog,
            a.zp_refusal,
            a.zp_list_stars,
            a.zp_window_bright,
            a.zp_window_slope,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The nine `zp_*` figures one judgement stores beside the analysis (forced-list spec §5.5), with the version and
/// the catalogue identity it was judged at. The figures are the statistic's, `None` where the outcome has none (a
/// refusal's value, a frame that never reached the list).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredZeroPoint {
    /// The zero point (mag); `None` for a refusal.
    pub zero_point: Option<f64>,
    /// Members of non-zero weight the screen left.
    pub stars: Option<i64>,
    /// Robust scatter of those members (mag).
    pub sigma: Option<f64>,
    /// `zero_point::ZP_VERSION` that judged the frame.
    pub version: u32,
    /// The refusal code of a stored refusal; `None` on a valid zero point.
    pub refusal: Option<String>,
    /// The list before it was measured.
    pub list_stars: Option<i64>,
    /// The window's bright edge `G_b` (Gaia G mag).
    pub window_bright: Option<f64>,
    /// The zero point's change per magnitude the window moves fainter.
    pub window_slope: Option<f64>,
}

impl StoredZeroPoint {
    /// Write the figures and the catalogue identity into an analysis row not yet stored (the analyzer's own row).
    pub fn apply_to(&self, analysis: &mut FrameAnalysis, catalog: &str) {
        analysis.zero_point = self.zero_point;
        analysis.zp_stars = self.stars;
        analysis.zp_sigma = self.sigma;
        analysis.zp_version = Some(self.version);
        analysis.zp_catalog = Some(catalog.to_string());
        analysis.zp_refusal = self.refusal.clone();
        analysis.zp_list_stars = self.list_stars;
        analysis.zp_window_bright = self.window_bright;
        analysis.zp_window_slope = self.window_slope;
    }
}

impl StoredZeroPoint {
    /// A refusal that carries only its code (`no_anchor`, `no_exposure`, `unreadable_solve`, `colour_camera`,
    /// `frame_verdict`): every figure NULL.
    pub fn code_only(version: u32, code: &str) -> Self {
        StoredZeroPoint {
            zero_point: None,
            stars: None,
            sigma: None,
            version,
            refusal: Some(code.to_string()),
            list_stars: None,
            window_bright: None,
            window_slope: None,
        }
    }
}

/// Set every `zp_*` field of an analysis row not yet stored to `None` (the nine columns): a judgement that was
/// superseded before it was written.
pub fn clear_zero_point_fields(analysis: &mut FrameAnalysis) {
    analysis.zero_point = None;
    analysis.zp_stars = None;
    analysis.zp_sigma = None;
    analysis.zp_version = None;
    analysis.zp_catalog = None;
    analysis.zp_refusal = None;
    analysis.zp_list_stars = None;
    analysis.zp_window_bright = None;
    analysis.zp_window_slope = None;
}

/// Store the outcome of a zero-point judgement on a frame's analysis row: the nine figures (NULL where the outcome has
/// none), `zp_version` and the catalogue identity it was judged against, so the frame is not retried until either
/// changes.
///
/// Compare-and-set: the write lands only on the analysis row `analysis_id` (the one the judgement read) and, when
/// `solve_id` is `Some`, only while the frame's `plate_solves` row is still `solve_id`. A re-analysis (`INSERT OR
/// REPLACE` gives the row a new id) or a new or deleted solve in the meantime makes the judgement stale, and nothing is
/// written. Returns whether a row was written; `false` is not an error, the frame is simply judged again by whatever
/// superseded this judgement.
pub fn set_zero_point(
    conn: &Connection,
    frame_id: i64,
    analysis_id: i64,
    solve_id: Option<i64>,
    figures: &StoredZeroPoint,
    catalog: &str,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE frame_analysis
         SET zero_point = ?3, zp_stars = ?4, zp_sigma = ?5, zp_version = ?6, zp_catalog = ?7,
             zp_refusal = ?9, zp_list_stars = ?10, zp_window_bright = ?11, zp_window_slope = ?12
         WHERE id = ?1 AND frame_id = ?2
           AND (?8 IS NULL OR EXISTS (SELECT 1 FROM plate_solves WHERE frame_id = ?2 AND id = ?8))",
        params![
            analysis_id,
            frame_id,
            figures.zero_point,
            figures.stars,
            figures.sigma,
            figures.version,
            catalog,
            solve_id,
            figures.refusal,
            figures.list_stars,
            figures.window_bright,
            figures.window_slope,
        ],
    )?;
    Ok(n > 0)
}

/// Set all nine `zp_*` columns of a frame's analysis row to NULL: a zero point never outlives the solve it was
/// measured against. Plain SQL on the caller's connection, so it joins whatever transaction the caller holds; a frame
/// without an analysis row is a no-op.
pub fn clear_zero_point(conn: &Connection, frame_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE frame_analysis
         SET zero_point = NULL, zp_stars = NULL, zp_sigma = NULL, zp_version = NULL, zp_catalog = NULL,
             zp_refusal = NULL, zp_list_stars = NULL, zp_window_bright = NULL, zp_window_slope = NULL
         WHERE frame_id = ?1",
        [frame_id],
    )?;
    Ok(())
}

/// Get analysis for a single frame.
pub fn get_frame_analysis(conn: &Connection, frame_id: i64) -> Result<Option<FrameAnalysis>> {
    let mut stmt = conn.prepare(
        "SELECT id, frame_id, file_id, stars_detected, median_fwhm, median_eccentricity,
                median_snr, median_hfr, frame_snr, snr_weight, psf_signal,
                background, noise, detection_threshold, width, height,
                source_channels, trail_r_squared, possibly_trailed,
                median_beta, quality_score, config_hash, analyzed_at,
                measure_version, stars_refused, frame_verdict,
                zero_point, zp_stars, zp_sigma, zp_version, zp_catalog,
                zp_refusal, zp_list_stars, zp_window_bright, zp_window_slope
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
                measure_version, stars_refused, frame_verdict,
                zero_point, zp_stars, zp_sigma, zp_version, zp_catalog,
                zp_refusal, zp_list_stars, zp_window_bright, zp_window_slope
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
                fa.measure_version, fa.stars_refused, fa.frame_verdict,
                fa.zero_point, fa.zp_stars, fa.zp_sigma, fa.zp_version, fa.zp_catalog,
                fa.zp_refusal, fa.zp_list_stars, fa.zp_window_bright, fa.zp_window_slope
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
        zero_point: row.get(26)?,
        zp_stars: row.get(27)?,
        zp_sigma: row.get(28)?,
        zp_version: row.get(29)?,
        zp_catalog: row.get(30)?,
        zp_refusal: row.get(31)?,
        zp_list_stars: row.get(32)?,
        zp_window_bright: row.get(33)?,
        zp_window_slope: row.get(34)?,
        zp_outdated: false,
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
            zero_point: None,
            zp_stars: None,
            zp_sigma: None,
            zp_version: None,
            zp_catalog: None,
            zp_refusal: None,
            zp_list_stars: None,
            zp_window_bright: None,
            zp_window_slope: None,
            zp_outdated: false,
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

    /// Today's (pre-C6) `frame_analysis` and `star_metrics`, with one row each and two stars. Only the two tables, without the
    /// star table's foreign key; the rebuild adds it and the bundled build enforces it.
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

    /// An orphan star row (its analysis deleted with enforcement off) must not make the catalog unopenable once the
    /// rebuilt table carries the foreign key and enforcement is ON.
    #[test]
    fn star_metrics_rebuild_survives_an_orphan_row_under_foreign_keys() {
        let conn = old_catalog();
        conn.execute(
            "INSERT INTO star_metrics (frame_analysis_id, x, y, peak, flux, fwhm, fwhm_x, fwhm_y, eccentricity, snr,
                hfr, theta, beta, fit_method, fit_residual)
             VALUES (99, 1.0, 2.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.1, 1.0, 1.0, 0.0, NULL, 'gaussian', 0.0)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        init_db(&conn).unwrap();
        assert_eq!(get_star_metrics(&conn, 1).unwrap().len(), 2);
        assert_eq!(get_star_metrics(&conn, 99).unwrap().len(), 0, "the orphan is gone");
        assert_eq!(counts(&conn), (1, 2));
        // The retired ids are not reused: the next star gets an id above the orphan's.
        let mut s = star(1);
        s.snr = None;
        upsert_star_metrics(&conn, 1, &[s]).unwrap();
        let max: i64 = conn.query_row("SELECT MAX(id) FROM star_metrics", [], |r| r.get(0)).unwrap();
        assert!(max >= 4, "AUTOINCREMENT high-water mark carried, got {max}");
    }

    /// Every star an orphan: nothing is copied, and the high-water mark still survives the rebuild.
    #[test]
    fn star_metrics_rebuild_keeps_the_high_water_mark_when_nothing_is_kept() {
        let conn = old_catalog();
        conn.execute("DELETE FROM frame_analysis", []).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        init_db(&conn).unwrap();
        assert_eq!(counts(&conn), (0, 0));
        let seq: i64 = conn
            .query_row("SELECT seq FROM sqlite_sequence WHERE name = 'star_metrics'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seq, 2, "the old table's high-water mark");
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

    const ZP_COLUMNS: [&str; 9] = [
        "zero_point",
        "zp_stars",
        "zp_sigma",
        "zp_version",
        "zp_catalog",
        "zp_refusal",
        "zp_list_stars",
        "zp_window_bright",
        "zp_window_slope",
    ];

    type ZpRow = (
        Option<f64>,
        Option<i64>,
        Option<f64>,
        Option<u32>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<f64>,
        Option<f64>,
    );

    fn zp_row(a: &FrameAnalysis) -> ZpRow {
        (
            a.zero_point,
            a.zp_stars,
            a.zp_sigma,
            a.zp_version,
            a.zp_catalog.clone(),
            a.zp_refusal.clone(),
            a.zp_list_stars,
            a.zp_window_bright,
            a.zp_window_slope,
        )
    }

    const NO_ZP: ZpRow = (None, None, None, None, None, None, None, None, None);

    fn full_zp(a: &mut FrameAnalysis) {
        a.zero_point = Some(21.3);
        a.zp_stars = Some(87);
        a.zp_sigma = Some(0.04);
        a.zp_version = Some(3);
        a.zp_catalog = Some("tier_500:1:2".into());
        a.zp_refusal = None;
        a.zp_list_stars = Some(412);
        a.zp_window_bright = Some(12.7);
        a.zp_window_slope = Some(-0.021);
    }

    #[test]
    fn migration_adds_the_zero_point_columns_and_keeps_rows() {
        let conn = old_catalog();
        init_db(&conn).unwrap();
        for column in ZP_COLUMNS {
            let has: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('frame_analysis') WHERE name = ?1",
                    [column],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has, 1, "column {column}");
        }
        let a = get_frame_analysis(&conn, 7).unwrap().expect("the row survives");
        assert_eq!(zp_row(&a), NO_ZP);
        assert_eq!((a.stars_detected, a.median_fwhm), (42, 3.1));
        // A second init is a no-op.
        init_db(&conn).unwrap();
        assert_eq!(counts(&conn), (1, 2));
    }

    #[test]
    fn frame_analysis_round_trips_the_zero_point() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (file_id, frame_id) = insert_frame(&conn, "/x/a.fits");
        let mut a = analysis(frame_id, file_id);
        full_zp(&mut a);
        upsert_frame_analysis(&conn, &a).unwrap();
        let want = zp_row(&a);
        assert_eq!(zp_row(&get_frame_analysis(&conn, frame_id).unwrap().unwrap()), want);
        assert_eq!(zp_row(&get_frame_analyses_by_ids(&conn, &[frame_id]).unwrap()[0]), want);
        // The frame-set getter joins through sessions and nights; seed that chain for the one frame.
        conn.execute_batch(
            "INSERT INTO frames_set (id, name) VALUES (1, 'set');
             INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
                 VALUES (1, 1, '2026-01-01T00:00:00Z', '2026-01-01T06:00:00Z');
             INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (1, 1, 'cam');",
        )
        .unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (1, ?1)", [frame_id]).unwrap();
        let by_set = get_frame_analyses_for_frame_set(&conn, 1).unwrap();
        assert_eq!(zp_row(&by_set[0]), want);
        // A refusal's code and window round-trip as well.
        let mut r = analysis(frame_id, file_id);
        r.zp_version = Some(3);
        r.zp_catalog = Some("c".into());
        r.zp_refusal = Some("too_scattered".into());
        r.zp_list_stars = Some(300);
        r.zp_window_bright = Some(12.0);
        upsert_frame_analysis(&conn, &r).unwrap();
        assert_eq!(zp_row(&get_frame_analysis(&conn, frame_id).unwrap().unwrap()), zp_row(&r));
    }

    #[test]
    fn an_old_frame_analysis_json_decodes_without_a_zero_point() {
        let mut v = serde_json::to_value(analysis(1, 1)).unwrap();
        let o = v.as_object_mut().unwrap();
        for k in ZP_COLUMNS {
            assert!(o.remove(k).is_some(), "the wire carries {k} under its snake_case name");
        }
        let old: FrameAnalysis = serde_json::from_value(v).unwrap();
        assert_eq!(zp_row(&old), NO_ZP);
        // The nine keys round-trip.
        let mut a = analysis(1, 1);
        full_zp(&mut a);
        let back: FrameAnalysis = serde_json::from_value(serde_json::to_value(&a).unwrap()).unwrap();
        assert_eq!(zp_row(&back), zp_row(&a));
    }

    #[test]
    fn zp_outdated_never_reaches_the_wire_or_the_database() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (file_id, frame_id) = insert_frame(&conn, "/x/a.fits");
        let a = analysis(frame_id, file_id);
        // False: the key is absent from the JSON.
        let j = serde_json::to_value(&a).unwrap();
        assert!(j.get("zp_outdated").is_none(), "a current row carries no zp_outdated key");
        // True serializes (the listing's form), and a row read back is false whatever was written.
        let mut flagged = a.clone();
        flagged.zp_outdated = true;
        let jt = serde_json::to_value(&flagged).unwrap();
        assert_eq!(jt.get("zp_outdated"), Some(&serde_json::json!(true)));
        let decoded: FrameAnalysis = serde_json::from_value(jt).unwrap();
        assert!(decoded.zp_outdated);
        upsert_frame_analysis(&conn, &decoded).unwrap();
        assert!(!get_frame_analysis(&conn, frame_id).unwrap().unwrap().zp_outdated);
        assert!(!get_frame_analyses_by_ids(&conn, &[frame_id]).unwrap()[0].zp_outdated);
    }

    fn figures(zero_point: Option<f64>) -> StoredZeroPoint {
        StoredZeroPoint {
            zero_point,
            stars: Some(87),
            sigma: Some(0.04),
            version: 3,
            refusal: None,
            list_stars: Some(412),
            window_bright: Some(12.7),
            window_slope: Some(-0.021),
        }
    }

    #[test]
    fn set_zero_point_writes_every_row_of_the_stored_figures_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (file_id, frame_id) = insert_frame(&conn, "/x/a.fits");
        let analysis_id = upsert_frame_analysis(&conn, &analysis(frame_id, file_id)).unwrap();
        let read = || zp_row(&get_frame_analysis(&conn, frame_id).unwrap().unwrap());
        let with = |f: StoredZeroPoint, catalog: &str| {
            assert!(set_zero_point(&conn, frame_id, analysis_id, None, &f, catalog).unwrap());
        };
        // measured: the value, the members, the scatter, no refusal, the list, the window and its slope.
        with(figures(Some(21.3)), "A");
        assert_eq!(
            read(),
            (Some(21.3), Some(87), Some(0.04), Some(3), Some("A".into()), None, Some(412), Some(12.7), Some(-0.021))
        );
        // measured with no slope under ten bright-ramp members.
        with(StoredZeroPoint { window_slope: None, ..figures(Some(21.3)) }, "A");
        assert_eq!(read().8, None);
        // too_scattered: no value, members and (when fitted) sigma, the code, the list and the window, no slope.
        let scattered = StoredZeroPoint {
            refusal: Some("too_scattered".into()),
            window_slope: None,
            ..figures(None)
        };
        with(scattered.clone(), "B");
        assert_eq!(
            read(),
            (None, Some(87), Some(0.04), Some(3), Some("B".into()), Some("too_scattered".into()), Some(412), Some(12.7), None)
        );
        // too_scattered with no pedestal: sigma NULL.
        with(StoredZeroPoint { sigma: None, ..scattered }, "B");
        assert_eq!(read().2, None);
        // poor_solve: members and sigma NULL, the list and the window kept.
        let poor = StoredZeroPoint {
            stars: None,
            sigma: None,
            refusal: Some("poor_solve".into()),
            window_slope: None,
            ..figures(None)
        };
        with(poor, "B");
        assert_eq!(
            read(),
            (None, None, None, Some(3), Some("B".into()), Some("poor_solve".into()), Some(412), Some(12.7), None)
        );
        // no_anchor and the like: only the code, the version and the catalogue.
        let bare = StoredZeroPoint {
            zero_point: None,
            stars: None,
            sigma: None,
            version: 3,
            refusal: Some("no_anchor".into()),
            list_stars: None,
            window_bright: None,
            window_slope: None,
        };
        with(bare, "B");
        assert_eq!(read(), (None, None, None, Some(3), Some("B".into()), Some("no_anchor".into()), None, None, None));
        // Another analysis id is a superseded judgement: nothing is written, and it is not an error.
        assert!(!set_zero_point(&conn, frame_id, analysis_id + 100, None, &figures(Some(1.0)), "C").unwrap());
        assert_eq!(read().4.as_deref(), Some("B"));
        // A solve id that is no longer the frame's solve is the same.
        assert!(!set_zero_point(&conn, frame_id, analysis_id, Some(5), &figures(Some(1.0)), "C").unwrap());
    }

    #[test]
    fn clear_zero_point_nulls_all_nine_columns() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (file_id, frame_id) = insert_frame(&conn, "/x/a.fits");
        let mut a = analysis(frame_id, file_id);
        full_zp(&mut a);
        a.zp_refusal = Some("poor_solve".into());
        upsert_frame_analysis(&conn, &a).unwrap();
        clear_zero_point(&conn, frame_id).unwrap();
        assert_eq!(zp_row(&get_frame_analysis(&conn, frame_id).unwrap().unwrap()), NO_ZP);
        // No analysis row is a no-op.
        clear_zero_point(&conn, frame_id + 1).unwrap();
    }
}
