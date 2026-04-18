//! Debug bench for a single frame's plate solve.
//!
//! Usage:
//!   cargo run -p athenaeum-core --example debug_plate_solve --release -- <frame_id>
//!
//! Optional env vars:
//!   ATHENAEUM_DB_PATH   override the SQLite path (default: macOS app-data dir)
//!   ATHENAEUM_CATALOG   override the catalog directory (default: <db_parent>/catalogs)
//!
//! Runs the plate solver against the given frame under several config
//! variants so we can see whether the correct solution exists but is cut off
//! by a threshold, or whether the algorithm never gets close.

use std::path::PathBuf;
use std::sync::Arc;

use athenaeum_core::catalog::CatalogEngine;
use athenaeum_core::db::Database;
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::{
    config::PlateSolveConfig,
    hints::extract_hints,
    quad_index::QuadIndex,
    service::{solve_frame_with_hints, SolveResult},
};
use rusqlite::Connection;

fn main() -> anyhow::Result<()> {
    // ── Args ──
    let frame_id: i64 = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: debug_plate_solve <frame_id>"))?
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid frame_id: {e}"))?;

    // ── DB path ──
    let db_path = match std::env::var("ATHENAEUM_DB_PATH").ok() {
        Some(p) => PathBuf::from(p),
        None => default_db_path()?,
    };
    println!("db: {}", db_path.display());

    // ── Open DB ──
    let db = Database::new(db_path.clone())?;
    let conn = db.conn();

    // ── Load frame + hints ──
    let (frame, file_path) = load_frame(&conn, frame_id)?;
    println!("frame id:     {}", frame_id);
    println!("file:         {}", file_path);
    println!("instrume:     {:?}", frame.instrume);
    println!("focallen:     {:?} mm", frame.focallen);
    println!("xpixsz:       {:?} um", frame.xpixsz);
    println!("naxis1:       {:?}", frame.naxis1);
    println!("naxis2:       {:?}", frame.naxis2);
    println!("xbinning:     {:?}", frame.xbinning);
    println!("ra/dec (num): {:?} / {:?}", frame.ra, frame.dec);
    println!("objctra/dec:  {:?} / {:?}", frame.objctra, frame.objctdec);
    println!("date_obs:     {:?}", frame.date_obs);

    let hints = extract_hints(&frame, Some(&conn));
    println!(
        "hints: ra={:?} dec={:?} px_scale={:?} fov={:?}",
        hints.ra, hints.dec, hints.pixel_scale_arcsec, hints.fov_deg
    );

    // ── Catalog + index ──
    let catalog_dir = match std::env::var("ATHENAEUM_CATALOG").ok() {
        Some(p) => PathBuf::from(p),
        None => db_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no parent dir for db path"))?
            .join("catalogs"),
    };
    println!("catalog dir:  {}", catalog_dir.display());

    let catalog = CatalogEngine::with_catalog_dir(&catalog_dir);
    let index_path = catalog_dir.join("tycho2").join("quad_index.bin");
    println!("index path:   {}", index_path.display());
    let index = QuadIndex::load(&index_path)?;
    println!("index quads:  {}", index.quad_count());

    // ── Build a shared rayon pool so star detection parallelises ──
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(cores)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build rayon pool: {e}"))?,
    );

    // ── Config variants ──
    let base = PlateSolveConfig::default();
    println!();
    println!("base config: {:#?}", base);
    println!();

    let variants: Vec<(&str, PlateSolveConfig)> = vec![
        ("default", base.clone()),
        (
            "precise-detection",
            PlateSolveConfig { use_fast_detection: false, ..base.clone() },
        ),
        (
            "loose-verify (tol=20px)",
            PlateSolveConfig { verification_tolerance_px: 20.0, ..base.clone() },
        ),
        (
            "lower-threshold (min_stars=5)",
            PlateSolveConfig { min_matched_stars: 5, ..base.clone() },
        ),
        (
            "more-image-stars (400)",
            PlateSolveConfig { max_image_stars: 400, ..base.clone() },
        ),
        (
            "kitchen-sink (precise+loose+lower+more)",
            PlateSolveConfig {
                use_fast_detection: false,
                verification_tolerance_px: 20.0,
                min_matched_stars: 5,
                max_image_stars: 400,
                ..base.clone()
            },
        ),
    ];

    let mut rows: Vec<Row> = Vec::new();
    for (label, cfg) in &variants {
        println!("\n======== VARIANT: {label} ========");
        let t = std::time::Instant::now();
        let res = solve_frame_with_hints(
            &frame,
            &file_path,
            &hints,
            &catalog,
            &index,
            cfg,
            Some(pool.clone()),
        );
        let elapsed_ms = t.elapsed().as_millis();
        match res {
            Ok(result) => {
                println!(
                    "→ SOLVED: RA={:.4} Dec={:.4} scale={:.3}\"/px rot={:.1}° inliers={} rms_px={:.2}",
                    result.wcs.crval.0,
                    result.wcs.crval.1,
                    result.pixel_scale_arcsec,
                    result.field_rotation_deg,
                    result.matched_stars,
                    result.rms_residual_px,
                );
                rows.push(Row::ok(label, elapsed_ms, &result));
            }
            Err(e) => {
                println!("→ FAILED: {e}");
                rows.push(Row::err(label, elapsed_ms, e.to_string()));
            }
        }
    }

    // ── Summary table ──
    println!("\n\n======== SUMMARY ========");
    println!(
        "{:<42} {:>8} {:>7} {:>9} {:>7} {:>7}",
        "variant", "time_ms", "status", "inliers", "rms_px", "scale"
    );
    for r in &rows {
        match &r.outcome {
            Outcome::Ok { inliers, rms_px, scale } => {
                println!(
                    "{:<42} {:>8} {:>7} {:>9} {:>7.2} {:>6.2}\"",
                    r.label, r.elapsed_ms, "ok", inliers, rms_px, scale
                );
            }
            Outcome::Err { msg } => {
                let short: String = msg.chars().take(30).collect();
                println!(
                    "{:<42} {:>8} {:>7} {:>9} {:>7} {:>7}  {}",
                    r.label, r.elapsed_ms, "FAIL", "-", "-", "-", short
                );
            }
        }
    }

    Ok(())
}

fn default_db_path() -> anyhow::Result<PathBuf> {
    // macOS default: ~/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("com.vsharifov.athenaeum")
        .join("athenaeum.db"))
}

fn load_frame(conn: &Connection, frame_id: i64) -> anyhow::Result<(Frame, String)> {
    let mut stmt = conn.prepare(
        "SELECT f.*, fl.path FROM frames f JOIN files fl ON fl.id = f.file_id WHERE f.id = ?1",
    )?;
    let (frame, path) = stmt.query_row([frame_id], |row| {
        let frame = Frame {
            id: row.get("id")?,
            file_id: row.get("file_id")?,
            object: row.get("object")?,
            date_obs: row
                .get::<_, Option<String>>("date_obs")
                .ok()
                .flatten()
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .or_else(|| {
                            chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
                                .ok()
                                .map(|ndt| ndt.and_utc().fixed_offset())
                        })
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                }),
            telescop: row.get("telescop")?,
            instrume: row.get("instrume")?,
            exptime: row.get("exptime")?,
            filter: row.get("filter")?,
            imagetyp: None,
            is_master: row.get::<_, i32>("is_master").unwrap_or(0) != 0,
            gain: row.get("gain")?,
            offset: row.get("offset")?,
            binning: row.get("binning")?,
            xbinning: row.get("xbinning")?,
            ybinning: row.get("ybinning")?,
            ccd_temp: row.get("ccd_temp")?,
            set_temp: row.get("set_temp")?,
            focallen: row.get("focallen")?,
            xpixsz: row.get("xpixsz")?,
            ypixsz: row.get("ypixsz")?,
            naxis1: row.get("naxis1")?,
            naxis2: row.get("naxis2")?,
            ra: row.get("ra")?,
            dec: row.get("dec")?,
            sitelat: row.get("sitelat")?,
            lat_obs: row.get("lat_obs")?,
            sitelong: row.get("sitelong")?,
            long_obs: row.get("long_obs")?,
            objctra: row.get("objctra")?,
            objctdec: row.get("objctdec")?,
            override_: row.get::<_, i32>("override").unwrap_or(0) != 0,
            swcreate: row.get("swcreate")?,
            bayerpat: row.get("bayerpat")?,
            rotation: row.get("rotation")?,
        };
        let path: String = row.get("path")?;
        Ok((frame, path))
    })?;
    Ok((frame, path))
}

struct Row {
    label: String,
    elapsed_ms: u128,
    outcome: Outcome,
}

enum Outcome {
    Ok { inliers: usize, rms_px: f64, scale: f64 },
    Err { msg: String },
}

impl Row {
    fn ok(label: &str, elapsed_ms: u128, r: &SolveResult) -> Self {
        Self {
            label: label.to_string(),
            elapsed_ms,
            outcome: Outcome::Ok {
                inliers: r.matched_stars,
                rms_px: r.rms_residual_px,
                scale: r.pixel_scale_arcsec,
            },
        }
    }
    fn err(label: &str, elapsed_ms: u128, msg: String) -> Self {
        Self { label: label.to_string(), elapsed_ms, outcome: Outcome::Err { msg } }
    }
}
