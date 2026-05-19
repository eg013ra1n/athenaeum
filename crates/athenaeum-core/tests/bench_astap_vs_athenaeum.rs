//! Dev bench (ignored): ASTAP (truth oracle, D50/Gaia) vs the Athenaeum
//! solver over a corpus of varied FITS/XISF frames. Read-only w.r.t. the
//! catalog DB and the user's image library (ASTAP -> temp base via -o;
//! Athenaeum -> in-memory DB).
//!
//!   cargo test -p athenaeum-core --test bench_astap_vs_athenaeum -- --ignored --nocapture
//!
//! Self-validates the oracle: every ASTAP solve is cross-matched to the
//! repo DSO catalog (prints the nearest named object + distance). The
//! detection↔catalog diagnostic rescales detected coords to ASTAP's
//! full-res pixel space (from the .ini DIMENSIONS), auto-picks the Y
//! orientation, and runs a POSITIVE CONTROL on a frame Athenaeum solved
//! so its match% is trustworthy before reading the failing frames.
//!
//! Env: ASTAP_BIN, BENCH_DIR, BENCH_FILES (comma), BENCH_CSV.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use astroimage::platesolving::WcsSolution;
use astroimage::ImageAnalyzer;
use athenaeum_core::catalog::CatalogEngine;
use athenaeum_core::db::schema::init_db;
use athenaeum_core::fits_parser::{parse_fits, parse_xisf};
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::config::PlateSolveConfig;
use athenaeum_core::plate_solve::dso_lookup::DsoCatalog;
use athenaeum_core::plate_solve::hints::observation_epoch;
#[allow(unused_imports)]
use athenaeum_core::plate_solve::quad_index::QuadIndex;
use athenaeum_core::plate_solve::service;
use rusqlite::Connection;

const DEF_ASTAP: &str = "/Applications/ASTAP.app/Contents/MacOS/astap";
const DEF_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Pictures/Astro/Platesolve Bench";
const CATALOG_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs";
const SMAC_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia";

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

fn guard<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        p.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string())
    })
}

struct Astap {
    solved: bool,
    ra: f64,
    dec: f64,
    scale: f64, // arcsec/px, full-res
    full_w: f64,
    full_h: f64,
    wcs: Option<WcsSolution>,
}

fn run_astap(astap: &str, file: &Path, tmpbase: &str) -> Astap {
    let none = Astap {
        solved: false,
        ra: 0.0,
        dec: 0.0,
        scale: 0.0,
        full_w: 0.0,
        full_h: 0.0,
        wcs: None,
    };
    let _ = std::fs::remove_file(format!("{tmpbase}.ini"));
    let _ = std::fs::remove_file(format!("{tmpbase}.wcs"));
    // ASTAP is run with a hard timeout: the macOS GUI binary pops a *blocking
    // modal* ("File not found …") when its star database is missing/moved,
    // which would otherwise hang the whole bench forever. On timeout we kill
    // it and treat the frame as ASTAP-unavailable (the caller falls back to a
    // cached truth so the bench never depends on a working local ASTAP).
    let mut child = match Command::new(astap)
        .args([
            "-f",
            file.to_str().unwrap_or_default(),
            "-r",
            "180",
            "-fov",
            "0",
            "-z",
            "0",
            "-wcs",
            "-o",
            tmpbase,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return none,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(75);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!(
                        "  [astap] TIMEOUT — killed (missing star DB or GUI modal?) for {}",
                        file.display()
                    );
                    return none;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(_) => return none,
        }
    }
    let Ok(ini) = std::fs::read_to_string(format!("{tmpbase}.ini")) else {
        return none;
    };
    let mut kv: HashMap<String, String> = HashMap::new();
    for line in ini.lines() {
        if let Some((k, v)) = line.split_once('=') {
            kv.insert(k.trim().to_uppercase(), v.trim().to_string());
        }
    }
    // DIMENSIONS=4144 x 2822  -> full-res pixel dims
    let (full_w, full_h) = kv
        .get("DIMENSIONS")
        .and_then(|s| {
            let nums: Vec<f64> = s
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .filter(|t| !t.is_empty())
                .filter_map(|t| t.parse::<f64>().ok())
                .collect();
            if nums.len() >= 2 {
                Some((nums[0], nums[1]))
            } else {
                None
            }
        })
        .unwrap_or((0.0, 0.0));
    if kv.get("PLTSOLVD").map(|s| s.as_str()) != Some("T") {
        return none;
    }
    let f = |k: &str| kv.get(k).and_then(|v| v.parse::<f64>().ok());
    let (Some(ra), Some(dec)) = (f("CRVAL1"), f("CRVAL2")) else {
        return none;
    };
    let scale = f("CDELT1")
        .map(|c| c.abs() * 3600.0)
        .or_else(|| Some((f("CD1_1")?.powi(2) + f("CD2_1")?.powi(2)).sqrt() * 3600.0))
        .unwrap_or(0.0);
    let wcs = match (
        f("CRPIX1"),
        f("CRPIX2"),
        f("CD1_1"),
        f("CD1_2"),
        f("CD2_1"),
        f("CD2_2"),
    ) {
        (Some(px), Some(py), Some(a), Some(b), Some(c), Some(d)) => Some(WcsSolution {
            crpix: (px, py),
            crval: (ra, dec),
            cd: [[a, b], [c, d]],
            sip_forward: None,
            sip_reverse: None,
        }),
        _ => None,
    };
    Astap { solved: true, ra, dec, scale, full_w, full_h, wcs }
}

/// Known ASTAP ground truth (RA°, Dec°, scale "/px) captured from many prior
/// clean ASTAP runs. These are stable physical facts about the frames, so the
/// bench can score correctness even when the local ASTAP install is broken
/// (missing/moved star database). Keyed by a distinctive filename substring.
/// `wcs`/dimensions are omitted (None/0) — only correctness scoring uses these
/// and that needs RA/Dec/scale only; the pixel-space `diagnose()` simply skips
/// a cached-truth frame.
fn none_astap() -> Astap {
    Astap {
        solved: false,
        ra: 0.0,
        dec: 0.0,
        scale: 0.0,
        full_w: 0.0,
        full_h: 0.0,
        wcs: None,
    }
}

fn cached_astap_truth(path: &Path) -> Option<Astap> {
    let name = path.file_name()?.to_str()?;
    let table: &[(&str, f64, f64, f64)] = &[
        ("2024-01-13_20-27-32_L_-10.00_120.00s_0081", 86.682, 0.042, 0.96),
        ("2024-02-24_19-42-32_H_-20.60_300.00s_0074", 114.184, 65.610, 0.48),
        ("M 82_H_-20.60_300.00s_0254", 148.949, 69.679, 0.48),
        ("M 51_H_-20.60_300.00s_0060", 202.483, 47.194, 0.48),
        ("NGC 2903_H_-19.80_300.00s_0004", 143.034, 21.507, 0.48),
        ("2024-09-08_04-30-13_L_-9.90_180.00s_0008", 85.391, -2.441, 0.39),
        ("2025-08-23_01-38-32_B_-10.00_180.00s_0012", 326.954, 47.242, 2.22),
        ("2025-10-05_00-50-11_R_-0.00_120.00s_0000", 56.655, 24.068, 1.73),
        ("2025-10-05_04-30-45_R_0.00_30.00s_0003", 151.538, 41.938, 1.73),
        ("2025-10-26_01-35-09_L_-10.00_300.00s_0034", 83.780, -5.330, 1.73),
        ("Light_Gamma Scuti_180.0s_Bin1_20251018-201649_0010", 276.506, -13.584, 2.88),
        ("Light_Pane 4_300.0s_Bin1_O_gain111_20211011-024956", 36.702, 60.880, 0.88),
        ("Light_Pane 7_300.0s_Bin1_O_gain111_20211014-045033", 36.113, 61.488, 0.88),
        ("Light_veil_300.0s_Bin1_0015", 312.045, 30.958, 1.89),
        ("_DSC2522", 10.762, 41.257, 1.00),
        ("_DSC5767", 10.727, 41.707, 2.77),
    ];
    for &(key, ra, dec, scale) in table {
        if name.contains(key) {
            return Some(Astap {
                solved: true,
                ra,
                dec,
                scale,
                full_w: 0.0,
                full_h: 0.0,
                wcs: None,
            });
        }
    }
    None
}

fn load_frame(path: &Path) -> Result<Frame, String> {
    let xisf = path
        .extension()
        .map(|e| e.to_ascii_lowercase() == "xisf")
        .unwrap_or(false);
    if xisf {
        parse_xisf(path, 0).map_err(|e| e.to_string())
    } else {
        parse_fits(path, 0).map_err(|e| e.to_string())
    }
}

struct Ath {
    solved: bool,
    ra: f64,
    dec: f64,
    scale: f64,
    ratio: f64,
    err: String,
}

fn solve_athenaeum(path: &Path, frame: &Frame, cache: &solvemyastro::StarCache) -> Ath {
    let conn = Connection::open_in_memory().expect("mem db");
    init_db(&conn).expect("init schema");
    let cfg = PlateSolveConfig::default();
    match service::solve_frame(frame, path.to_str().unwrap(), &conn, cache, &cfg) {
        Ok(s) => Ath {
            solved: true,
            ra: s.wcs.crval.0,
            dec: s.wcs.crval.1,
            scale: s.pixel_scale_arcsec,
            ratio: s.inlier_ratio,
            err: String::new(),
        },
        Err(e) => Ath {
            solved: false,
            ra: 0.0,
            dec: 0.0,
            scale: 0.0,
            ratio: 0.0,
            err: e.to_string().lines().next().unwrap_or("").chars().take(90).collect(),
        },
    }
}

fn sep_deg(r1: f64, d1: f64, r2: f64, d2: f64) -> f64 {
    let (a, b) = (d1.to_radians(), d2.to_radians());
    let v = a.sin() * b.sin() + a.cos() * b.cos() * (r1 - r2).to_radians().cos();
    v.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Detection↔catalog cross-match using ASTAP's accurate WCS. Detected
/// coords are rescaled to ASTAP's full-res pixel grid (the detector may
/// internally bin); both Y orientations are tried. Returns the best
/// (matched, total, flip, fov_deg). `label` distinguishes CONTROL output.
fn diagnose(path: &Path, frame: &Frame, astap: &Astap, cat: &CatalogEngine, label: &str) {
    let Some(wcs) = astap.wcs.as_ref() else {
        println!("  [{label}] no ASTAP CD matrix — skipped");
        return;
    };
    if astap.full_w < 1.0 || astap.full_h < 1.0 {
        println!("  [{label}] no ASTAP DIMENSIONS — skipped");
        return;
    }
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(5.0)
        .with_max_stars(600)
        .with_saturation_fraction(1.0);
    let _ = &mut analyzer;
    let det = match analyzer.detect_fast(path.to_str().unwrap()) {
        Ok(d) => d,
        Err(e) => {
            println!("  [{label}] detect_fast failed: {e}");
            return;
        }
    };
    // Convention-free, SENSOR-BOUNDED pixel-space analysis. ASTAP CRPIX/CD
    // define the file's FITS pixel grid; map each catalog star to a pixel
    // via CD-inverse + gnomonic (the control validates this: bright stars
    // land within ~2 px). Only catalog stars whose pixel is inside the
    // sensor count — this removes the earlier cone-larger-than-sensor bias.
    use astroimage::platesolving::GnomonicProjection;
    let fov_deg = astap.full_w.max(astap.full_h) * astap.scale / 3600.0;
    let epoch = observation_epoch(frame);
    let (cat_stars, _) = match cat.cone_search(astap.ra, astap.dec, fov_deg * 0.75, 12.0, epoch) {
        Ok(v) => v,
        Err(e) => {
            println!("  [{label}] cone_search failed: {e}");
            return;
        }
    };
    let [[a, b], [c, d]] = wcs.cd;
    let det2 = a * d - b * c;
    if det2.abs() < 1e-18 {
        println!("  [{label}] singular CD — skipped");
        return;
    }
    let (i00, i01, i10, i11) = (d / det2, -b / det2, -c / det2, a / det2);
    let to_px = |ra: f64, dec: f64| -> (f64, f64) {
        let (xi, eta) = GnomonicProjection::sky_to_tangent(ra, dec, astap.ra, astap.dec);
        let (xd, ed) = (xi.to_degrees(), eta.to_degrees());
        (i00 * xd + i01 * ed + wcs.crpix.0, i10 * xd + i11 * ed + wcs.crpix.1)
    };
    let (w, h) = (astap.full_w, astap.full_h);
    let mut inframe: Vec<(f64, f64, f32)> = cat_stars
        .iter()
        .filter_map(|c| {
            let (x, y) = to_px(c.ra, c.dec);
            if x >= 0.0 && x <= w && y >= 0.0 && y <= h {
                Some((x, y, c.mag))
            } else {
                None
            }
        })
        .collect();
    inframe.sort_by(|p, q| p.2.partial_cmp(&q.2).unwrap());
    let mut byflux: Vec<_> = det.stars.iter().collect();
    byflux.sort_by(|s, t| t.flux.partial_cmp(&s.flux).unwrap());
    let near_det = |x: f64, y: f64| {
        det.stars
            .iter()
            .any(|s| ((s.x as f64 - x).powi(2) + (s.y as f64 - y).powi(2)).sqrt() <= 5.0)
    };
    let near_cat = |x: f64, y: f64| {
        inframe
            .iter()
            .any(|(cx, cy, _)| ((cx - x).powi(2) + (cy - y).powi(2)).sqrt() <= 5.0)
    };
    let bright: Vec<&(f64, f64, f32)> = inframe.iter().filter(|t| t.2 <= 11.5).collect();
    let comp = bright.iter().filter(|t| near_det(t.0, t.1)).count();
    let k = 50.min(byflux.len());
    let real_top = byflux[..k]
        .iter()
        .filter(|s| near_cat(s.x as f64, s.y as f64))
        .count();
    let mut pf: Vec<f64> = byflux[..k]
        .iter()
        .map(|s| s.peak as f64 / (s.flux as f64).max(1e-6))
        .collect();
    pf.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pf_med = if pf.is_empty() { 0.0 } else { pf[pf.len() / 2] };
    println!(
        "  [{label}] in-sensor Tycho-2={} (V≤11.5={})  detections={}  sensorFOV≈{:.2}°x{:.2}°",
        inframe.len(),
        bright.len(),
        det.stars.len(),
        w * astap.scale / 3600.0,
        h * astap.scale / 3600.0
    );
    println!(
        "  [{label}] COMPLETENESS: {}/{} bright in-sensor stars have a detection ≤5px ({:.0}%)",
        comp,
        bright.len(),
        if bright.is_empty() { 0.0 } else { 100.0 * comp as f64 / bright.len() as f64 }
    );
    println!(
        "  [{label}] QUAD POOL: of brightest {} detections, {} are real catalog stars ({:.0}%); median peak/flux={:.3}",
        k,
        real_top,
        100.0 * real_top as f64 / k.max(1) as f64,
        pf_med
    );
    let det_bottleneck = (bright.len() >= 8 && (comp as f64) < 0.4 * bright.len() as f64)
        || (real_top as f64) < 0.3 * k as f64;
    println!(
        "  [{label}] VERDICT: {}",
        if det_bottleneck {
            "DETECTION/SELECTION is the bottleneck — real stars under-detected and/or the brightest detections are not stars (extended-object/noise pollution of the flux-ranked quad pool)."
        } else {
            "Detection adequate — investigate the matcher (hash tolerance / scale & position gates)."
        }
    );
    println!("  [{label}] brightest 8 detections (x,y,peak,flux,peak/flux,class):");
    for s in byflux.iter().take(8) {
        println!(
            "         ({:7.1},{:7.1}) peak={:10.0} flux={:12.0} pf={:.3} {}",
            s.x,
            s.y,
            s.peak,
            s.flux,
            s.peak as f64 / (s.flux as f64).max(1e-6),
            if near_cat(s.x as f64, s.y as f64) { "STAR" } else { "junk" }
        );
    }
}

/// Centroid-bias probe for M78 vs the Flame (NGC 2024) control pair.
///
/// Tests the hypothesis: our detector's centroids are biased on M78's steep
/// reflection-nebula gradients (global background, no per-star annulus like
/// ASTAP's HFD) → matched quads but wrong absolute positions → 0 inliers.
/// Method: align detections to the catalog at ASTAP's KNOWN centre+scale
/// (only rotation/parity/translation brute-forced), then look at matched
/// positional residuals split by SNR (low SNR ≈ nebula-embedded). If M78
/// shows large residuals concentrated in low-SNR / bright quad-pool stars
/// while the Flame is tight → centroid-on-gradient bias confirmed.
/// M78 GROUND-TRUTH residual probe. ASTAP solves M78 locally in 3.3 s
/// (proving it IS solvable; the bug is ours). Using ASTAP's exact solved
/// WCS for M78, reproject the catalog to pixels and measure how far our
/// detected stars — especially the brightest-by-flux quad pool — sit from
/// the true catalog positions. Small residuals ⇒ detections are fine, the
/// bug is in our solver/matching. Large/systematic ⇒ centroid bias.
#[test]
#[ignore = "dev diagnostic; needs the M78 frame + catalog"]
fn m78_groundtruth_residuals() {
    use astroimage::platesolving::WcsSolution;
    let dir = env_or("BENCH_DIR", DEF_DIR);
    let path = std::path::PathBuf::from(&dir)
        .join("2024-01-13_20-27-32_L_-10.00_120.00s_0081.fits");
    let frame = load_frame(&path).expect("load M78");

    // ASTAP's solved WCS for M78 (astap -f m78 -r 20 -fov 0 -wcs, 3.3 s).
    // CRPIX is FITS 1-based → 0-based for array/detection coords.
    let wcs = WcsSolution {
        crpix: (2072.5 - 1.0, 1411.5 - 1.0),
        crval: (86.681973819866826, 0.042483541842011370),
        cd: [
            [-4.7165954328681712e-5, -2.6222404043238338e-4],
            [2.6196290398406243e-4, -4.666360322280e-5],
        ],
        sip_forward: None,
        sip_reverse: None,
    };
    let fits_w = (2.0 * 2072.5) as usize; // ≈4145
    let fits_h = (2.0 * 1411.5) as usize; // ≈2823

    // Also report the PRIMARY solver path's completeness (detect_fast).
    {
        let fd = ImageAnalyzer::new()
            .with_detection_sigma(5.0)
            .with_max_stars(600)
            .with_saturation_fraction(1.0)
            .detect_fast(path.to_str().unwrap())
            .expect("detect_fast");
        let fsx = fits_w as f64 / fd.width as f64;
        let fsy = fits_h as f64 / fd.height as f64;
        let cat2 = CatalogEngine::with_catalog_dir(Path::new(CATALOG_DIR));
        let ep2 = observation_epoch(&frame);
        if let Ok((cs, _)) = cat2.cone_search(86.681973819866826, 0.042483541842011370, 0.8, 12.0, ep2) {
            let cpx: Vec<(f64, f64)> = cs
                .iter()
                .filter_map(|c| {
                    let (px, py) = wcs.sky_to_pixel(c.ra, c.dec);
                    (px >= 0.0 && px < fits_w as f64 && py >= 0.0 && py < fits_h as f64)
                        .then_some((px, py))
                })
                .collect();
            let dpx: Vec<(f64, f64)> =
                fd.stars.iter().map(|s| (s.x as f64 * fsx, s.y as f64 * fsy)).collect();
            let comp = cpx
                .iter()
                .filter(|&&(cx, cy)| {
                    dpx.iter().any(|&(x, y)| (x - cx).powi(2) + (y - cy).powi(2) <= 9.0)
                })
                .count();
            println!(
                "\n== M78 detect_fast (PRIMARY path) ==  {} det, {} bright(≤12) cat → completeness {}/{} ({:.0}%)",
                fd.stars.len(),
                cpx.len(),
                comp,
                cpx.len(),
                100.0 * comp as f64 / cpx.len().max(1) as f64
            );
        }
    }

    let an = ImageAnalyzer::new()
        .with_detection_sigma(5.0)
        .with_max_stars(600)
        .with_saturation_fraction(1.0)
        .analyze(path.to_str().unwrap())
        .expect("analyze");
    // The detector may bin internally → rescale detection coords to the
    // FITS pixel grid the ASTAP WCS is defined on.
    let sx = fits_w as f64 / an.width as f64;
    let sy = fits_h as f64 / an.height as f64;

    let epoch = observation_epoch(&frame);
    let cat = CatalogEngine::with_catalog_dir(Path::new(CATALOG_DIR));
    let (cat_stars, cat_name) = cat
        .cone_search(wcs.crval.0, wcs.crval.1, 0.8, 16.0, epoch)
        .expect("cone");

    // True catalog pixel positions (in-frame only), brightest first.
    let mut catpx: Vec<(f64, f64, f32)> = cat_stars
        .iter()
        .filter_map(|c| {
            let (px, py) = wcs.sky_to_pixel(c.ra, c.dec);
            if px >= 0.0 && px < fits_w as f64 && py >= 0.0 && py < fits_h as f64 {
                Some((px, py, c.mag))
            } else {
                None
            }
        })
        .collect();
    catpx.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());

    // Detection coords rescaled to FITS grid; try identity vs y-flip
    // (ROWORDER/orientation convention) and keep whichever aligns better.
    let nearest = |x: f64, y: f64| -> f64 {
        let mut best = f64::MAX;
        for &(cx, cy, _) in &catpx {
            let d = (x - cx).powi(2) + (y - cy).powi(2);
            if d < best {
                best = d;
            }
        }
        best.sqrt()
    };
    for flip in [false, true] {
        let map = |s: &astroimage::analysis::StarMetrics| -> (f64, f64) {
            let x = s.x as f64 * sx;
            let y0 = s.y as f64 * sy;
            (x, if flip { fits_h as f64 - 1.0 - y0 } else { y0 })
        };
        // Completeness: bright catalog stars (mag≤12) with a detection ≤3px.
        let bright: Vec<&(f64, f64, f32)> =
            catpx.iter().filter(|c| c.2 <= 12.0).collect();
        let dets: Vec<(f64, f64)> = an.stars.iter().map(&map).collect();
        let near_det = |cx: f64, cy: f64| {
            dets.iter()
                .any(|&(x, y)| (x - cx).powi(2) + (y - cy).powi(2) <= 9.0)
        };
        let comp = bright.iter().filter(|c| near_det(c.0, c.1)).count();

        // Brightest-20 detections by FLUX (the actual quad pool) → residual
        // to nearest true catalog position.
        let mut byflux: Vec<&_> = an.stars.iter().collect();
        byflux.sort_by(|a, b| b.flux.partial_cmp(&a.flux).unwrap());
        let qp: Vec<f64> = byflux
            .iter()
            .take(20)
            .map(|s| {
                let (x, y) = map(s);
                nearest(x, y)
            })
            .collect();
        let qp_ok = qp.iter().filter(|&&r| r < 3.0).count();
        let qp_mean = qp.iter().sum::<f64>() / qp.len().max(1) as f64;

        // Residual vs SNR (low SNR ≈ on nebulosity), matched ≤8px.
        let snr_med = {
            let mut v: Vec<f64> = an.stars.iter().map(|s| s.snr as f64).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let (mut lo, mut hi) = (Vec::new(), Vec::new());
        for s in &an.stars {
            let (x, y) = map(s);
            let r = nearest(x, y);
            if r < 8.0 {
                if (s.snr as f64) < snr_med {
                    lo.push(r);
                } else {
                    hi.push(r);
                }
            }
        }
        let mean = |v: &[f64]| {
            if v.is_empty() {
                f64::NAN
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        println!(
            "\n== M78 ground-truth (flip={flip}) ==  catalog={cat_name}, {} det, {} in-frame cat",
            an.stars.len(),
            catpx.len()
        );
        println!(
            "  bright(≤12mag) completeness: {}/{} have a detection ≤3px ({:.0}%)",
            comp,
            bright.len(),
            100.0 * comp as f64 / bright.len().max(1) as f64
        );
        println!(
            "  brightest-20 quad pool: {qp_ok}/20 within 3px of a true star; mean residual {:.2}px",
            qp_mean
        );
        println!(
            "  residual mean: low-SNR(nebula) {:.2}px (n={})  high-SNR {:.2}px (n={})",
            mean(&lo),
            lo.len(),
            mean(&hi),
            hi.len()
        );
    }
}

#[test]
#[ignore = "dev diagnostic; needs the corpus + catalog"]
fn centroid_bias_probe_m78_vs_flame() {
    use astroimage::platesolving::GnomonicProjection;
    let dir = env_or("BENCH_DIR", DEF_DIR);
    let cat = CatalogEngine::with_catalog_dir(Path::new(CATALOG_DIR));
    const TOL: f64 = 3.0; // px match tolerance at known scale

    for (label, fname) in [
        ("M78   (reflection nebula, FAILS)", "2024-01-13_20-27-32_L_-10.00_120.00s_0081.fits"),
        ("Flame (NGC 2024, SOLVES)", "2024-09-08_04-30-13_L_-9.90_180.00s_0008.fits"),
    ] {
        let path = std::path::PathBuf::from(&dir).join(fname);
        let frame = match load_frame(&path) {
            Ok(f) => f,
            Err(e) => {
                println!("\n== {label} ==  load failed: {e}");
                continue;
            }
        };
        let truth = cached_astap_truth(&path).expect("astap truth");
        let an = ImageAnalyzer::new()
            .with_detection_sigma(5.0)
            .with_max_stars(600)
            .with_saturation_fraction(1.0)
            .analyze(path.to_str().unwrap())
            .expect("analyze");
        let (w, h, scale) = (an.width as f64, an.height as f64, truth.scale);
        let fov = w.max(h) * scale / 3600.0;
        let epoch = observation_epoch(&frame);
        let (cat_stars, _) = cat
            .cone_search(truth.ra, truth.dec, fov * 0.6, 16.0, epoch)
            .expect("cone");

        // Catalog → ideal pixels about (ra,dec), scale pinned (unknown:
        // rotation, parity, translation).
        let mut catpx: Vec<(f64, f64, f32)> = cat_stars
            .iter()
            .map(|c| {
                let (xi, eta) =
                    GnomonicProjection::sky_to_tangent(c.ra, c.dec, truth.ra, truth.dec);
                (
                    xi.to_degrees() * 3600.0 / scale,
                    eta.to_degrees() * 3600.0 / scale,
                    c.mag,
                )
            })
            .collect();
        catpx.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
        catpx.truncate(400); // brightest 400 catalog
        let mut det: Vec<(f64, f64, f64)> = an
            .stars
            .iter()
            .map(|s| (s.x as f64, s.y as f64, s.snr as f64))
            .collect();
        // brightest-by-SNR detections for the search
        det.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        let det_b: Vec<(f64, f64, f64)> = det.iter().take(80).cloned().collect();

        // Brute-force rotation×parity; translation by Hough vote.
        let mut best = (0usize, 0.0f64, 0.0f64, 1.0f64, (0.0, 0.0)); // matches,rms,theta,parity,t
        for ti in 0..180 {
            let theta = (ti as f64) * 2.0 * std::f64::consts::PI / 180.0;
            let (st, ct) = theta.sin_cos();
            for &par in &[1.0f64, -1.0] {
                let rot: Vec<(f64, f64, f32)> = catpx
                    .iter()
                    .map(|&(x, y, m)| (ct * x - st * (par * y), st * x + ct * (par * y), m))
                    .collect();
                // Hough vote translation (5px bins) using brightest 60 each.
                use std::collections::HashMap;
                let mut votes: HashMap<(i64, i64), u32> = HashMap::new();
                for d in det_b.iter().take(60) {
                    for r in rot.iter().take(120) {
                        let (ox, oy) = (d.0 - r.0, d.1 - r.1);
                        *votes
                            .entry(((ox / 5.0).round() as i64, (oy / 5.0).round() as i64))
                            .or_insert(0) += 1;
                    }
                }
                let Some((&(bx, by), _)) = votes.iter().max_by_key(|(_, &v)| v) else {
                    continue;
                };
                let t = (bx as f64 * 5.0, by as f64 * 5.0);
                // count matches det_b → nearest rotated+translated catalog
                let mut m = 0usize;
                let mut sse = 0.0;
                for d in &det_b {
                    let mut bd = f64::MAX;
                    for r in &rot {
                        let dx = d.0 - (r.0 + t.0);
                        let dy = d.1 - (r.1 + t.1);
                        let dd = dx * dx + dy * dy;
                        if dd < bd {
                            bd = dd;
                        }
                    }
                    if bd.sqrt() < TOL {
                        m += 1;
                        sse += bd;
                    }
                }
                if m > best.0 {
                    let rms = if m > 0 { (sse / m as f64).sqrt() } else { 0.0 };
                    best = (m, rms, theta, par, t);
                }
            }
        }

        let (m, rms, theta, par, t) = best;
        // Re-measure residuals over ALL detections at the best alignment, and
        // split by SNR (low SNR ≈ on nebulosity) + brightest-20 quad pool.
        let (st, ct) = theta.sin_cos();
        let rot: Vec<(f64, f64)> = catpx
            .iter()
            .map(|&(x, y, _)| {
                (ct * x - st * (par * y) + t.0, st * x + ct * (par * y) + t.1)
            })
            .collect();
        let mut res_lo = Vec::new();
        let mut res_hi = Vec::new();
        let snr_med = {
            let mut v: Vec<f64> = det.iter().map(|d| d.2).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        for d in &det {
            let mut bd = f64::MAX;
            for r in &rot {
                let dd = (d.0 - r.0).powi(2) + (d.1 - r.1).powi(2);
                if dd < bd {
                    bd = dd;
                }
            }
            let r = bd.sqrt();
            if r < 8.0 {
                if d.2 < snr_med {
                    res_lo.push(r);
                } else {
                    res_hi.push(r);
                }
            }
        }
        let mean = |v: &[f64]| if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 };
        // brightest-20 by flux (the real quad pool) residual
        let mut byflux: Vec<&_> = an.stars.iter().collect();
        byflux.sort_by(|a, b| b.flux.partial_cmp(&a.flux).unwrap());
        let qp: Vec<f64> = byflux
            .iter()
            .take(20)
            .map(|s| {
                let mut bd = f64::MAX;
                for r in &rot {
                    let dd = (s.x as f64 - r.0).powi(2) + (s.y as f64 - r.1).powi(2);
                    if dd < bd {
                        bd = dd;
                    }
                }
                bd.sqrt()
            })
            .collect();

        println!("\n== {label} ==  {} det, {} cat, scale {:.2}\"/px, FOV {:.2}°",
            an.stars.len(), cat_stars.len(), scale, fov);
        println!(
            "  best align: {m}/{} det_b matched ≤{TOL}px, RMS {:.2}px (θ={:.0}°, parity {})",
            det_b.len(), rms, theta.to_degrees(), if par > 0.0 { "normal" } else { "mirrored" }
        );
        println!(
            "  residual mean: low-SNR(neb) {:.2}px (n={})  vs  high-SNR {:.2}px (n={})",
            mean(&res_lo), res_lo.len(), mean(&res_hi), res_hi.len()
        );
        println!(
            "  brightest-20 quad-pool residual: mean {:.2}px, max {:.2}px  [<{TOL} = usable]",
            mean(&qp), qp.iter().cloned().fold(0.0, f64::max)
        );
    }
}

/// Saturation probe for the M78 vs NGC 2024 (Flame) control pair.
///
/// Pure detector measurement — no catalog, WCS, or parity solver. Tests the
/// hypothesis that M78 fails because its bright, catalog-matchable stars are
/// saturated (clipped flat tops → biased centroids → quads don't match),
/// while the Flame at 1960mm/0.39"/px spreads the same stars over ~6× more
/// pixels and stays unsaturated. The Flame solves; M78 doesn't — same
/// catalog density, same detection count, so the discriminator must be here.
#[test]
#[ignore = "dev diagnostic; needs the corpus"]
fn saturation_probe_m78_vs_flame() {
    let dir = env_or("BENCH_DIR", DEF_DIR);
    // 16-bit sensors clip at 65535; ASTAP/our analyzer treat ≳62000 as
    // saturated (a flat top biases the centroid).
    const SAT: f32 = 60000.0;
    for (label, fname, scale) in [
        ("M78    1000mm 0.96\"/px (FAILS)", "2024-01-13_20-27-32_L_-10.00_120.00s_0081.fits", 0.96),
        ("Flame  1960mm 0.39\"/px (SOLVES)", "2024-09-08_04-30-13_L_-9.90_180.00s_0008.fits", 0.39),
    ] {
        let path = std::path::PathBuf::from(&dir).join(fname);
        let an = ImageAnalyzer::new()
            .with_detection_sigma(5.0)
            .with_max_stars(600)
            .with_saturation_fraction(1.0);
        let det = an.detect_fast(path.to_str().unwrap()).expect("detect");
        let mut by_flux: Vec<_> = det.stars.iter().collect();
        by_flux.sort_by(|s, t| t.flux.partial_cmp(&s.flux).unwrap());

        let k = 50.min(by_flux.len());
        let sat_top = by_flux[..k].iter().filter(|s| s.peak >= SAT).count();
        let max_peak = by_flux
            .iter()
            .map(|s| s.peak)
            .fold(0.0f32, f32::max);
        println!(
            "\n== {label} ==  {} stars, {}x{}",
            det.stars.len(),
            det.width,
            det.height
        );
        println!(
            "  brightest {k} detections (the quad pool): {sat_top} saturated (peak≥{:.0}) = {:.0}%; max peak overall = {:.0} (scale {scale}\"/px)",
            SAT,
            100.0 * sat_top as f64 / k as f64,
            max_peak
        );
        println!("  top 12 by flux  (x, y, peak, flux, peak/flux):");
        for s in by_flux.iter().take(12) {
            println!(
                "    ({:7.1},{:7.1}) peak={:9.0} flux={:12.0} pf={:.3} {}",
                s.x,
                s.y,
                s.peak,
                s.flux,
                s.peak as f64 / (s.flux as f64).max(1e-6),
                if s.peak >= SAT { "SAT" } else { "ok" }
            );
        }
    }
}

#[test]
#[ignore = "dev bench; needs ASTAP, the installed quad index, and the corpus"]
fn bench_astap_vs_athenaeum() {
    let astap = env_or("ASTAP_BIN", DEF_ASTAP);
    let dir = env_or("BENCH_DIR", DEF_DIR);
    let csv_path = env_or("BENCH_CSV", "/tmp/bench_astap.csv");

    if !Path::new(&astap).exists() {
        eprintln!("SKIP: ASTAP not found at {astap}");
        return;
    }
    if !Path::new(SMAC_DIR).exists() {
        eprintln!("SKIP: smac_gaia star cache not found at {SMAC_DIR}");
        return;
    }
    let cat = CatalogEngine::with_catalog_dir(Path::new(CATALOG_DIR));
    let cache = solvemyastro::StarCache::open(Path::new(SMAC_DIR)).expect("open smac_gaia");
    let dso = DsoCatalog::load().ok();
    if dso.is_none() {
        println!("(DSO catalog unavailable — object self-check disabled)");
    }
    let obj_of = |ra: f64, dec: f64| -> String {
        dso.as_ref()
            .and_then(|d| d.find_best(ra, dec))
            .map(|m| format!("{} ({:.2}°)", m.designation, m.distance_deg))
            .unwrap_or_else(|| "(no DSO)".into())
    };

    let files: Vec<PathBuf> = if let Ok(list) = std::env::var("BENCH_FILES") {
        list.split(',').map(|s| PathBuf::from(s.trim())).collect()
    } else {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("read BENCH_DIR")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()),
                    Some(ref e) if e == "fits" || e == "fit" || e == "xisf"
                )
            })
            .collect();
        v.sort();
        v
    };
    println!("\nBench: {} files | ASTAP={astap}\n", files.len());

    let tmp = std::env::temp_dir().join("athenaeum_astap_bench");
    let _ = std::fs::create_dir_all(&tmp);
    let tmpbase = tmp.join("a").to_string_lossy().to_string();

    let mut csv = String::from(
        "file,header_fl_mm,astap_solved,astap_ra,astap_dec,astap_scale,astap_object,\
ath_solved,ath_ra,ath_dec,ath_scale,ath_ratio,dpos_arcsec,dscale_pct,err\n",
    );
    let mut n = 0;
    let (mut astap_ok, mut ath_ok, mut ath_correct) = (0, 0, 0);
    let mut flagged: Vec<(PathBuf, Frame, Astap, Ath)> = Vec::new();
    let mut control: Option<(PathBuf, Frame, Astap)> = None;

    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let frame = match load_frame(path) {
            Ok(f) => f,
            Err(e) => {
                println!("{name:50} PARSE-FAIL: {e}");
                continue;
            }
        };
        let hfl = frame.focallen.unwrap_or(0.0);
        n += 1;
        let t0 = Instant::now();
        // BENCH_SKIP_ASTAP=1 bypasses the local ASTAP entirely (use when the
        // install is broken — missing star DB) and scores against cached truth.
        let a = if std::env::var("BENCH_SKIP_ASTAP").is_ok() {
            none_astap()
        } else {
            run_astap(&astap, path, &tmpbase)
        };
        // Fall back to cached ground truth if the local ASTAP is broken
        // (missing star DB / GUI modal) so the bench still scores correctness.
        let a = if a.solved {
            a
        } else {
            match cached_astap_truth(path) {
                Some(c) => {
                    println!("  [astap] local ASTAP unavailable — using cached truth");
                    c
                }
                None => a,
            }
        };
        let ta = t0.elapsed().as_secs_f64();
        let t1 = Instant::now();
        let ath = guard(|| solve_athenaeum(path, &frame, &cache)).unwrap_or_else(|m| Ath {
            solved: false,
            ra: 0.0,
            dec: 0.0,
            scale: 0.0,
            ratio: 0.0,
            err: format!("PANIC: {}", m.chars().take(90).collect::<String>()),
        });
        let tb = t1.elapsed().as_secs_f64();

        if a.solved {
            astap_ok += 1;
        }
        if ath.solved {
            ath_ok += 1;
        }
        let (dpos, dscale) = if a.solved && ath.solved {
            (
                sep_deg(a.ra, a.dec, ath.ra, ath.dec) * 3600.0,
                if a.scale > 0.0 {
                    (ath.scale - a.scale).abs() / a.scale * 100.0
                } else {
                    -1.0
                },
            )
        } else {
            (-1.0, -1.0)
        };
        let correct = a.solved && ath.solved && dpos >= 0.0 && dpos < 30.0 && dscale < 2.0;
        if correct {
            ath_correct += 1;
        }
        let astap_obj = if a.solved { obj_of(a.ra, a.dec) } else { "-".into() };
        println!(
            "{name:50} hFL={hfl:7.1} | ASTAP {} {:8.3}/{:7.3} {:.2}\"/px obj={astap_obj} [{ta:3.0}s] | ATH {} {:.2}\"/px r={:.4} [{tb:3.0}s] | Δpos={:.1}\" Δscl={:.2}% {}",
            if a.solved { "OK " } else { "FAIL" },
            a.ra,
            a.dec,
            a.scale,
            if ath.solved { "OK " } else { "FAIL" },
            ath.scale,
            ath.ratio,
            dpos,
            dscale,
            if a.solved && !correct { "  <-- ASTAP solved, ATH failed/wrong" } else { "" }
        );
        csv.push_str(&format!(
            "{name:?},{hfl},{},{:.5},{:.5},{:.4},{astap_obj:?},{},{:.5},{:.5},{:.4},{:.5},{:.2},{:.3},{:?}\n",
            a.solved, a.ra, a.dec, a.scale, ath.solved, ath.ra, ath.dec, ath.scale, ath.ratio, dpos, dscale, ath.err
        ));
        // First clean Athenaeum-correct, non-polar frame = positive control.
        if correct && control.is_none() && a.dec.abs() < 45.0 {
            control = Some((path.clone(), frame.clone(), Astap {
                solved: a.solved, ra: a.ra, dec: a.dec, scale: a.scale,
                full_w: a.full_w, full_h: a.full_h, wcs: a.wcs.clone(),
            }));
        }
        if a.solved && !correct {
            flagged.push((path.clone(), frame, a, ath));
        }
    }

    let _ = std::fs::write(&csv_path, &csv);
    println!(
        "\n=== SUMMARY ===\nframes={n}  ASTAP solved={astap_ok}  Athenaeum solved={ath_ok}  \
         Athenaeum CORRECT (Δpos<30\",Δscl<2%)={ath_correct}\nCSV: {csv_path}"
    );

    if let Some((p, frame, a)) = &control {
        println!(
            "\n=== POSITIVE CONTROL (Athenaeum solved this correctly) ===\n* {}",
            p.file_name().unwrap().to_string_lossy()
        );
        if let Err(m) = guard(|| diagnose(p, frame, a, &cat, "CONTROL")) {
            println!("  [CONTROL] PANICKED: {m}");
        }
        println!(
            "  (interpretation: CONTROL match% should be HIGH for the correct flip — \
             that validates the cross-match. Only then are the failing-frame %s meaningful.)"
        );
    } else {
        println!("\n(no non-polar Athenaeum-correct frame for a positive control)");
    }

    println!("\n=== ASTAP solved → Athenaeum failed/wrong ({}) ===", flagged.len());
    for (p, frame, a, ath) in &flagged {
        let nm = p.file_name().unwrap().to_string_lossy();
        println!(
            "\n* {nm}\n  ASTAP truth: RA={:.4} Dec={:.4} scale={:.3}\"/px obj={} | Athenaeum: {}",
            a.ra,
            a.dec,
            a.scale,
            obj_of(a.ra, a.dec),
            if ath.solved {
                format!("solved WRONG RA={:.4} Dec={:.4} r={:.4}", ath.ra, ath.dec, ath.ratio)
            } else {
                format!("FAILED: {}", ath.err)
            }
        );
        if let Err(m) = guard(|| diagnose(p, frame, a, &cat, "diag")) {
            println!("  [diag] PANICKED: {m} (cdshealpix high-dec cone bug — separate finding)");
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
