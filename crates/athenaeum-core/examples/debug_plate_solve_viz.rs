//! Visual plate-solve debugger.
//!
//! Loads a FITS frame by ID, runs both fast and precise star detection,
//! runs the plate solver, and writes four annotated PNGs to the given
//! output directory (default: `/tmp`):
//!
//!   - `<id>_fast_stars.png`      fast-detected stars (green circles)
//!   - `<id>_precise_stars.png`   PSF-detected stars (green circles)
//!   - `<id>_solve_overlay.png`   solved WCS: image stars + catalog
//!                                projections + inlier lines
//!   - `<id>_solve_failures.png`  catalog stars that DID NOT land on an
//!                                image star within tolerance (red)
//!
//! Usage:
//!   cargo run -p athenaeum-core --example debug_plate_solve_viz --release -- <frame_id> [output_dir]
//!
//! Env:
//!   ATHENAEUM_DB_PATH    override sqlite path
//!   ATHENAEUM_CATALOG    override catalog dir

use std::path::PathBuf;
use std::sync::Arc;

use athenaeum_core::catalog::CatalogEngine;
use athenaeum_core::db::Database;
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::{
    hints::{extract_hints, observation_epoch},
    quad_index::QuadIndex,
    service::solve_frame_with_hints,
};
use astroimage::platesolving::WcsSolution;
use astroimage::{ImageAnalyzer, ImageConverter};
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_hollow_circle_mut, draw_line_segment_mut};
use rusqlite::Connection;

// Rustafits's `debug-pipeline` feature (enabled for this workspace) exposes
// the same file-read + green-interpolation helper that the detector uses
// internally. We call it here so the "detection view" PNG shows exactly the
// luminance image the detector sees — not the color-stretched output of
// `ImageConverter::process`.
use astroimage::analysis::prepare_luminance;
use astroimage::formats::read_image;

const DOWNSCALE: usize = 4;

/// Convert a detect_fast / analyze star y-coordinate to a display-image y.
/// Empirically: astroimage's detectors report y in the raw-file row
/// convention (y=0 at the first byte row), while `ImageConverter::process()`
/// outputs pixel data that, once wrapped in an `RgbImage`, displays with
/// y=0 at the opposite end. A vertical flip is always needed when drawing
/// overlays on the `ImageConverter::process()` output. Verified with a
/// crop test on the brightest star (cyan crosshair landed on the star,
/// red crosshair landed in empty sky).
#[inline]
fn to_display_y(star_y: f64, full_height: usize) -> f64 {
    full_height as f64 - 1.0 - star_y
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let frame_id: i64 = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("usage: debug_plate_solve_viz <frame_id> [output_dir]"))?
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid frame_id: {e}"))?;
    let output_dir = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&output_dir)?;

    // ── DB + frame ──
    let db_path = match std::env::var("ATHENAEUM_DB_PATH").ok() {
        Some(p) => PathBuf::from(p),
        None => default_db_path()?,
    };
    let db = Database::new(db_path.clone())?;
    let conn = db.conn();
    let (frame, file_path) = load_frame(&conn, frame_id)?;
    println!("frame:  {}", frame_id);
    println!("file:   {}", file_path);

    let hints = extract_hints(&frame, Some(&conn));
    println!(
        "hints:  px_scale={:?} fov={:?}",
        hints.pixel_scale_arcsec, hints.fov_deg
    );

    // ── Shared rayon pool ──
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new().num_threads(cores).build()
            .map_err(|e| anyhow::anyhow!("rayon pool: {e}"))?,
    );

    // ── Load + stretch + downscale the image for display ──
    // ImageConverter handles debayer (if any), auto-stretch, and downscale,
    // producing RGB u8 bytes ready to draw on.
    let processed = ImageConverter::new()
        .with_downscale(DOWNSCALE)
        .with_thread_pool(Arc::clone(&pool))
        .process(&file_path)?;
    println!(
        "display: {}x{} channels={} (downscale={}x)",
        processed.width, processed.height, processed.channels, DOWNSCALE
    );
    let canvas = rgb_from_processed(&processed)?;
    let full_width = processed.width * DOWNSCALE;
    let full_height = processed.height * DOWNSCALE;

    // ── Star detection (fast and precise) on FULL-RES FITS ──
    let mut detector_size: (u32, u32) = (0, 0);
    let fast_stars = {
        let analyzer = ImageAnalyzer::new()
            .with_detection_sigma(5.0)
            .with_max_stars(500)
            .with_saturation_fraction(1.0) // match plate-solve service.rs
            .with_thread_pool(Arc::clone(&pool));
        let r = analyzer.detect_fast(&file_path)?;
        println!("fast:    {} stars detected ({}x{})", r.stars.len(), r.width, r.height);
        detector_size = (r.width as u32, r.height as u32);
        r.stars.into_iter().map(|s| (s.x as f64, s.y as f64, s.flux as f64)).collect::<Vec<_>>()
    };
    let precise_stars = {
        let analyzer = ImageAnalyzer::new()
            .with_detection_sigma(5.0)
            .with_max_stars(500)
            .with_saturation_fraction(1.0) // match plate-solve service.rs
            .with_thread_pool(Arc::clone(&pool));
        let r = analyzer.analyze(&file_path)?;
        println!("precise: {} stars detected ({}x{})", r.stars.len(), r.width, r.height);
        r.stars.into_iter().map(|s| (s.x as f64, s.y as f64, s.flux as f64)).collect::<Vec<_>>()
    };

    // ── Render detection-only PNGs ──
    // Stars from detect_fast / analyze are in raw-row coordinates; flip y
    // before projecting onto the ImageConverter display image.
    let fh = full_height;
    let sx = |x: f64| (x / DOWNSCALE as f64) as i32;
    let sy = |y: f64| (to_display_y(y, fh) / DOWNSCALE as f64) as i32;
    {
        let mut img = canvas.clone();
        for &(x, y, _) in &fast_stars {
            draw_circle_safe(&mut img, sx(x), sy(y), 5, Rgb([80, 255, 80]));
        }
        let path = output_dir.join(format!("{frame_id}_fast_stars.png"));
        img.save(&path)?;
        println!("wrote:  {}", path.display());
    }
    {
        let mut img = canvas.clone();
        for &(x, y, _) in &precise_stars {
            draw_circle_safe(&mut img, sx(x), sy(y), 5, Rgb([80, 255, 80]));
        }
        let path = output_dir.join(format!("{frame_id}_precise_stars.png"));
        img.save(&path)?;
        println!("wrote:  {}", path.display());
    }

    // ── Detection-view PNG: render the EXACT luminance image the
    // detector runs on (green-interpolated for Bayer OSC, as-is for
    // mono), not the pretty color output of ImageConverter. This is what
    // the user needs to see when diagnosing false-negative solves —
    // star shapes, star count, and star brightness here are the inputs
    // the peak-detector actually thresholds against.
    render_detection_view(&file_path, &fast_stars, &output_dir, frame_id)?;

    // ── Full-resolution crop test: render at native resolution and crop
    // a 1200×1200 region around the brightest star. No scaling math, so
    // the only remaining source of misalignment would be a y-axis flip
    // or something equivalent — the simplest isolating test.
    render_native_crop(&file_path, &fast_stars, detector_size, &output_dir, frame_id)?;

    // ── Run plate solve (fast detection, default config) ──
    // For the viz bench we lower the acceptance threshold so we can always
    // get a WCS to visualise — even on frames that fail the production
    // config. This lets us eyeball the algorithm's BEST GUESS and see
    // whether it's correct-but-below-threshold vs. genuinely wrong.
    let ps_config = {
        let conn = db.conn();
        let mut cfg = athenaeum_core::plate_solve::config::load_config(&conn);
        cfg.min_matched_stars = 5;
        cfg
    };
    let catalog_dir = match std::env::var("ATHENAEUM_CATALOG").ok() {
        Some(p) => PathBuf::from(p),
        None => db_path.parent().unwrap().join("catalogs"),
    };
    let catalog = CatalogEngine::with_catalog_dir(&catalog_dir);
    let index = QuadIndex::load(&catalog_dir.join("tycho2").join("quad_index.bin"))?;
    println!("index:   {} quads", index.quad_count());

    let solve = solve_frame_with_hints(
        &frame,
        &file_path,
        &hints,
        &catalog,
        &index,
        &ps_config,
        Some(Arc::clone(&pool)),
    );

    // ── Rotation sweep: brute-force rotate the solved WCS in small
    // steps and count inliers at each rotation. This definitively
    // answers whether a small rotation would recover more matches.
    if let Ok(ref result) = solve {
        rotation_sweep(
            &result.wcs,
            &fast_stars,
            &catalog,
            (full_width as u32, full_height as u32),
            observation_epoch(&frame),
            result.pixel_scale_arcsec,
            ps_config.verification_tolerance_px,
        )?;
    }

    match solve {
        Ok(result) => {
            println!();
            println!("SOLVED  RA={:.4}  Dec={:.4}  scale={:.3}\"/px  rot={:.1}°  inliers={}  rms={:.2}px",
                result.wcs.crval.0, result.wcs.crval.1,
                result.pixel_scale_arcsec, result.field_rotation_deg,
                result.matched_stars, result.rms_residual_px);
            render_solve_overlay(
                &canvas,
                &file_path,
                &fast_stars,
                &result.wcs,
                &catalog,
                (full_width as u32, full_height as u32),
                observation_epoch(&frame),
                result.pixel_scale_arcsec,
                ps_config.verification_tolerance_px,
                &output_dir,
                frame_id,
            )?;
        }
        Err(e) => {
            println!();
            println!("SOLVE FAILED: {e}");
            println!("(no solve overlay PNGs — we have no WCS to project catalog stars with)");
        }
    }

    Ok(())
}

/// Project catalog stars via the WCS, compute inlier matches to image
/// stars, and render two overlay images:
///   1. `_solve_overlay.png`  — all image stars (dim green), all projected
///      catalog stars (red cross), inlier matches connected by yellow lines
///   2. `_solve_failures.png` — only the catalog stars that did NOT land
///      within tolerance of any image star (helps spot distortion patches)
#[allow(clippy::too_many_arguments)]
fn render_solve_overlay(
    canvas: &RgbImage,
    file_path: &str,
    image_stars: &[(f64, f64, f64)],
    wcs: &WcsSolution,
    catalog: &CatalogEngine,
    full_size: (u32, u32),
    obs_epoch: f64,
    pixel_scale_arcsec: f64,
    tolerance_px: f64,
    output_dir: &std::path::Path,
    frame_id: i64,
) -> anyhow::Result<()> {
    // Same cone search params as service.rs's verification loop.
    let image_center = (full_size.0 as f64 / 2.0, full_size.1 as f64 / 2.0);
    let (center_ra, center_dec) = wcs.pixel_to_sky(image_center.0, image_center.1);
    let image_fov_deg = (full_size.0 as f64).max(full_size.1 as f64) * pixel_scale_arcsec / 3600.0;
    let cone_radius = image_fov_deg * 0.7;
    let (cat_stars, _) = catalog.cone_search(center_ra, center_dec, cone_radius, 12.0, obs_epoch)?;
    println!("cone:    {} catalog stars within {:.2}°", cat_stars.len(), cone_radius);

    // For each catalog star, project and find nearest image star.
    struct Projection {
        cat_px: (f64, f64),
        nearest_img: Option<(f64, f64)>,
        nearest_d: f64,
    }
    let mut projections: Vec<Projection> = Vec::new();
    let mut inliers_count = 0usize;
    let mut in_frame_catalog = 0usize;
    for cs in &cat_stars {
        let (px, py) = wcs.sky_to_pixel(cs.ra, cs.dec);
        if px < 0.0 || py < 0.0 || px >= full_size.0 as f64 || py >= full_size.1 as f64 {
            continue;
        }
        in_frame_catalog += 1;
        let mut best_d = f64::INFINITY;
        let mut best_img = None;
        for &(ix, iy, _) in image_stars {
            let d = ((ix - px).powi(2) + (iy - py).powi(2)).sqrt();
            if d < best_d {
                best_d = d;
                best_img = Some((ix, iy));
            }
        }
        if best_d < tolerance_px {
            inliers_count += 1;
        }
        projections.push(Projection { cat_px: (px, py), nearest_img: best_img, nearest_d: best_d });
    }
    println!(
        "match:   {} / {} in-frame catalog stars matched within {:.1}px",
        inliers_count, in_frame_catalog, tolerance_px
    );

    // ── Overlay image ──
    // Stars and catalog projections are in raw-row y convention; flip
    // before drawing on the display canvas.
    let fh = full_size.1 as usize;
    let mut overlay = canvas.clone();
    let sx = |v: f64| (v / DOWNSCALE as f64) as i32;
    let sy = |v: f64| (to_display_y(v, fh) / DOWNSCALE as f64) as i32;
    let sxf = |v: f64| (v / DOWNSCALE as f64) as f32;
    let syf = |v: f64| (to_display_y(v, fh) / DOWNSCALE as f64) as f32;

    // Image stars (dim green, small)
    for &(x, y, _) in image_stars {
        draw_circle_safe(&mut overlay, sx(x), sy(y), 3, Rgb([40, 140, 40]));
    }
    // Catalog projections (red crosses, thin)
    for p in &projections {
        draw_cross_safe(&mut overlay, sx(p.cat_px.0), sy(p.cat_px.1), 4, Rgb([255, 60, 60]));
    }
    // Inlier connections (yellow lines image_star ↔ catalog_star)
    for p in &projections {
        if p.nearest_d < tolerance_px {
            if let Some((ix, iy)) = p.nearest_img {
                draw_line_segment_mut(
                    &mut overlay,
                    (sxf(ix), syf(iy)),
                    (sxf(p.cat_px.0), syf(p.cat_px.1)),
                    Rgb([255, 240, 40]),
                );
            }
        }
    }
    let path = output_dir.join(format!("{frame_id}_solve_overlay.png"));
    overlay.save(&path)?;
    println!("wrote:  {}", path.display());

    // ── Failures image ──
    let mut fails = canvas.clone();
    for &(x, y, _) in image_stars {
        draw_circle_safe(&mut fails, sx(x), sy(y), 3, Rgb([40, 100, 40]));
    }
    for p in &projections {
        if p.nearest_d >= tolerance_px {
            draw_cross_safe(&mut fails, sx(p.cat_px.0), sy(p.cat_px.1), 5, Rgb([255, 60, 60]));
            if let Some((ix, iy)) = p.nearest_img {
                if p.nearest_d < tolerance_px * 4.0 {
                    draw_line_segment_mut(
                        &mut fails,
                        (sxf(p.cat_px.0), syf(p.cat_px.1)),
                        (sxf(ix), syf(iy)),
                        Rgb([180, 60, 180]),
                    );
                }
            }
        }
    }
    let path = output_dir.join(format!("{frame_id}_solve_failures.png"));
    fails.save(&path)?;
    println!("wrote:  {}", path.display());

    // Full-res zoom: crop the overlay around the image center so a viewer
    // can see per-star alignment between green (detected) and red
    // (catalog-projected).
    let full = ImageConverter::new().process(file_path).ok();
    if let Some(full) = full {
        let mut native = rgb_from_processed(&full)?;
        let nsx = |v: f64| v as i32;
        let nsy = |v: f64| to_display_y(v, full.height) as i32;
        let nsxf = |v: f64| v as f32;
        let nsyf = |v: f64| to_display_y(v, full.height) as f32;
        for &(x, y, _) in image_stars {
            draw_circle_safe(&mut native, nsx(x), nsy(y), 8, Rgb([80, 255, 80]));
        }
        for p in &projections {
            draw_cross_safe(&mut native, nsx(p.cat_px.0), nsy(p.cat_px.1), 10, Rgb([255, 60, 60]));
        }
        for p in &projections {
            if p.nearest_d < tolerance_px {
                if let Some((ix, iy)) = p.nearest_img {
                    draw_line_segment_mut(
                        &mut native,
                        (nsxf(ix), nsyf(iy)),
                        (nsxf(p.cat_px.0), nsyf(p.cat_px.1)),
                        Rgb([255, 240, 40]),
                    );
                }
            }
        }
        let crop = crop_around(&native, (full_size.0 / 2) as i32, (full_size.1 / 2) as i32, 1600);
        let path = output_dir.join(format!("{frame_id}_solve_overlay_zoom.png"));
        crop.save(&path)?;
        println!("wrote:  {}", path.display());
    }

    Ok(())
}

/// Full-resolution crop test. Loads the file at downscale=1, renders the
/// N brightest fast-detected stars as big red crosshairs, and writes a
/// 1200x1200 crop centered on the brightest star. Also writes the same
/// crop with stars flipped in y (height - 1 - y) as a control — one of
/// the two should show crosshairs on actual stars, revealing the
/// coordinate convention.
fn render_native_crop(
    file_path: &str,
    fast_stars: &[(f64, f64, f64)],
    detector_size: (u32, u32),
    output_dir: &std::path::Path,
    frame_id: i64,
) -> anyhow::Result<()> {
    let full = ImageConverter::new().process(file_path)?;
    let w = full.width as u32;
    let h = full.height as u32;
    println!("native crop: full image {}x{} (detector {}x{})",
        w, h, detector_size.0, detector_size.1);

    // OSC images go through super-pixel debayer in `ImageConverter::process()`,
    // halving width and height (4 raw Bayer pixels → 1 RGB super-pixel). The
    // detector, in contrast, runs green-channel interpolation that preserves
    // native resolution. So `detect_fast` reports coordinates in the original
    // 6248x4176 space, but the displayed `full` image is 3124x2088. We must
    // scale detector coords by the ratio of converter-output to detector-input
    // before drawing the reticle, otherwise reticles land at half (or beyond)
    // their intended position.
    //
    // For mono frames (no Bayer), debayer is a no-op so both dimensions match
    // 1:1 and the scale is 1.0.
    let sx = w as f64 / detector_size.0 as f64;
    let sy = h as f64 / detector_size.1 as f64;
    let to_disp = |(x, y): (f64, f64)| (x * sx, y * sy);

    // Sort stars by flux desc and take the top 30.
    let mut sorted: Vec<(f64, f64, f64)> = fast_stars.to_vec();
    sorted.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let top: Vec<(f64, f64)> = sorted.iter().take(30).map(|s| (s.0, s.1)).collect();

    let brightest = top.first().copied().unwrap_or((
        detector_size.0 as f64 / 2.0,
        detector_size.1 as f64 / 2.0,
    ));
    let brightest_disp = to_disp(brightest);
    println!(
        "brightest star @ detector=({:.1}, {:.1}) → display=({:.1}, {:.1})",
        brightest.0, brightest.1, brightest_disp.0, brightest_disp.1
    );

    // Variant A: y as-is (display coords = detector coords × scale).
    {
        let img = rgb_from_processed(&full)?;
        let mut img = img;
        for &(x, y) in &top {
            let (dx, dy) = to_disp((x, y));
            draw_reticle(&mut img, dx as i32, dy as i32, Rgb([255, 0, 0]));
        }
        let crop = crop_around(&img, brightest_disp.0 as i32, brightest_disp.1 as i32, 1200);
        let path = output_dir.join(format!("{frame_id}_crop_y_asis.png"));
        crop.save(&path)?;
        println!("wrote:  {}", path.display());
    }

    // Variant B: y flipped (h - 1 - scaled_y). The Y flip happens AFTER the
    // detector→display scale conversion so the flip is in display coords.
    let fy = |y: f64| h as f64 - 1.0 - y;
    {
        let img = rgb_from_processed(&full)?;
        let mut img = img;
        for &(x, y) in &top {
            let (dx, dy) = to_disp((x, y));
            draw_reticle(&mut img, dx as i32, fy(dy) as i32, Rgb([0, 255, 255]));
        }
        let crop = crop_around(&img, brightest_disp.0 as i32, fy(brightest_disp.1) as i32, 1200);
        let path = output_dir.join(format!("{frame_id}_crop_y_flipped.png"));
        crop.save(&path)?;
        println!("wrote:  {}", path.display());
    }

    // Variant C: tight 200×200 close-up centered on the brightest centroid
    // in each Y convention. A real bright star is ~5-10 px wide on the
    // converter output and unambiguously visible inside the open-center
    // reticle if the centroid is correct.
    {
        let img = rgb_from_processed(&full)?;
        let mut img = img;
        draw_reticle(&mut img, brightest_disp.0 as i32, brightest_disp.1 as i32, Rgb([255, 0, 0]));
        let crop = crop_around(&img, brightest_disp.0 as i32, brightest_disp.1 as i32, 200);
        let path = output_dir.join(format!("{frame_id}_brightest_y_asis_200px.png"));
        crop.save(&path)?;
        println!(
            "wrote:  {} (display ({:.1}, {:.1}))",
            path.display(),
            brightest_disp.0,
            brightest_disp.1
        );
    }
    {
        let img = rgb_from_processed(&full)?;
        let mut img = img;
        draw_reticle(&mut img, brightest_disp.0 as i32, fy(brightest_disp.1) as i32, Rgb([0, 255, 255]));
        let crop = crop_around(&img, brightest_disp.0 as i32, fy(brightest_disp.1) as i32, 200);
        let path = output_dir.join(format!("{frame_id}_brightest_y_flipped_200px.png"));
        crop.save(&path)?;
        println!(
            "wrote:  {} (display ({:.1}, {:.1}))",
            path.display(),
            brightest_disp.0,
            fy(brightest_disp.1)
        );
    }

    Ok(())
}

/// Open-center reticle: a hollow circle around the centroid plus four outward
/// tick marks (N/S/E/W) that stop short of the center. Leaves the inner area
/// visually unobstructed so you can see whether a star is actually there.
///
/// Sized large (inner radius 25 px, outer tick 50 px) so the reticle is clearly
/// visible in 1200x1200 native-resolution crops. Two-pixel stroke thickness
/// keeps it visible against bright star halos.
fn draw_reticle(img: &mut RgbImage, cx: i32, cy: i32, c: Rgb<u8>) {
    let r_in: i32 = 25;
    let r_tick_start: i32 = 32;
    let r_tick_end: i32 = 50;

    // Hollow circle (3-pixel-thick stroke).
    for r in r_in..=(r_in + 2) {
        draw_hollow_circle_mut(img, (cx, cy), r, c);
    }

    // Four outward tick marks: N, S, E, W. Each tick is 3 px thick.
    let (w, h) = (img.width() as i32, img.height() as i32);
    let put = |img: &mut RgbImage, x: i32, y: i32| {
        if (0..w).contains(&x) && (0..h).contains(&y) {
            img.put_pixel(x as u32, y as u32, c);
        }
    };
    for r in r_tick_start..=r_tick_end {
        for thick in -1..=1 {
            // N (up), S (down)
            put(img, cx + thick, cy - r);
            put(img, cx + thick, cy + r);
            // E (right), W (left)
            put(img, cx + r, cy + thick);
            put(img, cx - r, cy + thick);
        }
    }
}

fn crop_around(img: &RgbImage, cx: i32, cy: i32, size: i32) -> RgbImage {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let half = size / 2;
    let x0 = (cx - half).max(0).min(w - size) as u32;
    let y0 = (cy - half).max(0).min(h - size) as u32;
    let sw = size.min(w) as u32;
    let sh = size.min(h) as u32;
    let mut out = RgbImage::new(sw, sh);
    for y in 0..sh {
        for x in 0..sw {
            out.put_pixel(x, y, *img.get_pixel(x0 + x, y0 + y));
        }
    }
    out
}

/// Sweep the WCS through small rotations (in pixel space about the
/// image center) and report the inlier count at each. This is an
/// empirical test of whether a small rotation would recover more
/// matches. If the answer is "yes, at ~1°", then the refit math is
/// producing the correct rotation but my verification is missing it;
/// if the answer is "no, no rotation helps", then rotation isn't the
/// real problem.
#[allow(clippy::too_many_arguments)]
fn rotation_sweep(
    wcs: &WcsSolution,
    image_stars: &[(f64, f64, f64)],
    catalog: &CatalogEngine,
    full_size: (u32, u32),
    obs_epoch: f64,
    pixel_scale_arcsec: f64,
    tolerance_px: f64,
) -> anyhow::Result<()> {
    println!();
    println!("── Rotation sweep around solved WCS ──");

    let (center_ra, center_dec) = wcs.pixel_to_sky(full_size.0 as f64 / 2.0, full_size.1 as f64 / 2.0);
    let image_fov_deg = (full_size.0 as f64).max(full_size.1 as f64) * pixel_scale_arcsec / 3600.0;
    let cone_radius = image_fov_deg * 0.7;
    let (cat_stars, _) = catalog.cone_search(center_ra, center_dec, cone_radius, 12.0, obs_epoch)?;

    let cx = full_size.0 as f64 / 2.0;
    let cy = full_size.1 as f64 / 2.0;
    let image_stars_simple: Vec<astroimage::platesolving::ImageStar> = image_stars
        .iter()
        .map(|&(x, y, flux)| astroimage::platesolving::ImageStar { x, y, flux })
        .collect();

    let tol2 = tolerance_px * tolerance_px;

    let mut best_angle = 0.0;
    let mut best_count = 0;
    println!("  angle(°)  inliers  rms_px");
    for i in -30i32..=30 {
        let angle_deg = i as f64 * 0.1;
        let theta = angle_deg.to_radians();
        let (st, ct) = (theta.sin(), theta.cos());

        // For each catalog star, project via original WCS to pixel space,
        // then rotate around (cx, cy) by `angle_deg`, then count if any
        // image star falls within tolerance of the rotated position.
        let mut inliers = 0usize;
        let mut total_rs = 0.0f64;
        for cs in &cat_stars {
            let (px, py) = wcs.sky_to_pixel(cs.ra, cs.dec);
            if px < 0.0 || py < 0.0 || px >= full_size.0 as f64 || py >= full_size.1 as f64 {
                continue;
            }
            // Rotate (px, py) about (cx, cy) by angle_deg.
            let u = px - cx;
            let v = py - cy;
            let rx = ct * u - st * v + cx;
            let ry = st * u + ct * v + cy;
            if rx < 0.0 || ry < 0.0 || rx >= full_size.0 as f64 || ry >= full_size.1 as f64 {
                continue;
            }
            let mut best_d2 = f64::INFINITY;
            for is in &image_stars_simple {
                let d2 = (is.x - rx).powi(2) + (is.y - ry).powi(2);
                if d2 < best_d2 {
                    best_d2 = d2;
                }
            }
            if best_d2 < tol2 {
                inliers += 1;
                total_rs += best_d2;
            }
        }
        let rms = if inliers > 0 {
            (total_rs / inliers as f64).sqrt()
        } else {
            0.0
        };
        if i.abs() % 5 == 0 || inliers > best_count {
            println!("  {:+5.2}      {:>5}    {:>6.2}", angle_deg, inliers, rms);
        }
        if inliers > best_count {
            best_count = inliers;
            best_angle = angle_deg;
        }
    }
    println!("  best rotation: {:+.2}° → {} inliers", best_angle, best_count);
    println!();
    Ok(())
}

/// Render the EXACT luminance image the detector runs peak-detection on.
///
/// For mono frames this is just the u16→f32 conversion. For OSC/Bayer
/// frames this is the green-interpolated array (R/B pixels replaced by
/// their weighted green-neighbour average). Output is 8-bit grayscale,
/// auto-stretched via 0.5 % / 99.5 % percentile clipping, downscaled by
/// `DOWNSCALE`, with the same star circles the pretty PNG gets.
fn render_detection_view(
    file_path: &str,
    stars: &[(f64, f64, f64)],
    output_dir: &std::path::Path,
    frame_id: i64,
) -> anyhow::Result<()> {
    // Read the raw image and apply the exact debayer the detector uses.
    let (meta, pixels) = read_image(std::path::Path::new(file_path))?;
    // apply_debayer=true mirrors `ImageAnalyzer` defaults.
    let (lum, full_w, full_h, channels, _green_mask) = prepare_luminance(&meta, &pixels, true);
    println!(
        "detect:  luminance {}x{} channels={} (before detection)",
        full_w, full_h, channels
    );

    // Percentile-based auto-stretch so faint stars are visible.
    // Sample ~2M pixels to keep this quick.
    let total = lum.len();
    let stride = (total / 2_000_000).max(1);
    let mut sample: Vec<f32> = lum.iter().step_by(stride).copied().collect();
    sample.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo_idx = (sample.len() as f64 * 0.005) as usize;
    let hi_idx = ((sample.len() as f64 * 0.995) as usize).min(sample.len() - 1);
    let lo = sample[lo_idx];
    let hi = sample[hi_idx].max(lo + 1.0);
    let range = hi - lo;
    println!(
        "stretch: lo={:.1} hi={:.1} (0.5%/99.5% percentile, sampled {})",
        lo, hi, sample.len()
    );

    // Downscale by DOWNSCALE with box-filter averaging (simple + fast).
    let out_w = full_w / DOWNSCALE;
    let out_h = full_h / DOWNSCALE;
    let mut gray = vec![0u8; out_w * out_h];
    let ds = DOWNSCALE;
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = 0.0f32;
            for dy in 0..ds {
                for dx in 0..ds {
                    let sx = ox * ds + dx;
                    let sy = oy * ds + dy;
                    sum += lum[sy * full_w + sx];
                }
            }
            let avg = sum / (ds * ds) as f32;
            let norm = ((avg - lo) / range).clamp(0.0, 1.0);
            gray[oy * out_w + ox] = (norm * 255.0) as u8;
        }
    }

    // Pack as RGB (replicate channel) so we can draw colored circles on top.
    let mut rgb_pixels: Vec<u8> = Vec::with_capacity(out_w * out_h * 3);
    for &g in &gray {
        rgb_pixels.push(g);
        rgb_pixels.push(g);
        rgb_pixels.push(g);
    }
    let mut img = RgbImage::from_raw(out_w as u32, out_h as u32, rgb_pixels)
        .ok_or_else(|| anyhow::anyhow!("RgbImage::from_raw dimension mismatch"))?;

    // Coord convention: we wrote `lum` rows 0..N directly into `RgbImage`
    // rows 0..N via `RgbImage::from_raw`, so PNG row == FITS/detector row.
    // Detector reports y in raw-row space — so NO y-flip here.  (The
    // pretty PNGs do flip, because `ImageConverter::process` presents the
    // sky north-up, which is the opposite convention.)
    for &(x, y, _) in stars {
        let cx = (x / ds as f64) as i32;
        let cy = (y / ds as f64) as i32;
        draw_circle_safe(&mut img, cx, cy, 5, Rgb([80, 255, 80]));
    }

    let path = output_dir.join(format!("{frame_id}_detection_view.png"));
    img.save(&path)?;
    println!("wrote:  {}", path.display());

    // ── Native-resolution 1:1 crop, 1200×1200 around image centre ──
    // No downscale, so pixel positions map 1:1 from the detector. Best
    // way to verify circles truly sit on star centroids.
    let crop_size = 1200usize.min(full_w.min(full_h));
    let cx0 = (full_w.saturating_sub(crop_size)) / 2;
    let cy0 = (full_h.saturating_sub(crop_size)) / 2;
    let mut rgb_crop: Vec<u8> = Vec::with_capacity(crop_size * crop_size * 3);
    for ry in 0..crop_size {
        for rx in 0..crop_size {
            let fx = cx0 + rx;
            let fy = cy0 + ry;
            let v = lum[fy * full_w + fx];
            let norm = ((v - lo) / range).clamp(0.0, 1.0);
            let g = (norm * 255.0) as u8;
            rgb_crop.push(g);
            rgb_crop.push(g);
            rgb_crop.push(g);
        }
    }
    let mut crop_img = RgbImage::from_raw(crop_size as u32, crop_size as u32, rgb_crop)
        .ok_or_else(|| anyhow::anyhow!("crop RgbImage::from_raw dimension mismatch"))?;
    let mut circled_in_crop = 0usize;
    for &(x, y, _) in stars {
        let xi = x as i32 - cx0 as i32;
        let yi = y as i32 - cy0 as i32;
        if xi >= -8 && yi >= -8
            && xi < crop_size as i32 + 8
            && yi < crop_size as i32 + 8
        {
            draw_circle_safe(&mut crop_img, xi, yi, 6, Rgb([80, 255, 80]));
            if xi >= 0 && yi >= 0 && xi < crop_size as i32 && yi < crop_size as i32 {
                circled_in_crop += 1;
            }
        }
    }
    let path_crop = output_dir.join(format!("{frame_id}_detection_native_crop.png"));
    crop_img.save(&path_crop)?;
    println!(
        "wrote:  {} (1:1 native {}×{}, center crop, {} stars inside)",
        path_crop.display(), crop_size, crop_size, circled_in_crop
    );
    Ok(())
}

fn rgb_from_processed(p: &astroimage::ProcessedImage) -> anyhow::Result<RgbImage> {
    let w = p.width as u32;
    let h = p.height as u32;
    let pixels_rgb: Vec<u8> = match p.channels {
        3 => p.data.clone(),
        4 => {
            let mut rgb = Vec::with_capacity((w * h * 3) as usize);
            for chunk in p.data.chunks_exact(4) {
                rgb.extend_from_slice(&chunk[..3]);
            }
            rgb
        }
        other => anyhow::bail!("unexpected channel count: {other}"),
    };
    RgbImage::from_raw(w, h, pixels_rgb)
        .ok_or_else(|| anyhow::anyhow!("RgbImage::from_raw dimension mismatch"))
}

fn draw_circle_safe(img: &mut RgbImage, cx: i32, cy: i32, r: i32, c: Rgb<u8>) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    if cx < -r || cy < -r || cx >= w + r || cy >= h + r {
        return;
    }
    draw_hollow_circle_mut(img, (cx, cy), r, c);
}

fn draw_cross_safe(img: &mut RgbImage, cx: i32, cy: i32, size: i32, c: Rgb<u8>) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    for k in -size..=size {
        let x = cx + k;
        let y = cy;
        if (0..w).contains(&x) && (0..h).contains(&y) {
            img.put_pixel(x as u32, y as u32, c);
        }
        let x = cx;
        let y = cy + k;
        if (0..w).contains(&x) && (0..h).contains(&y) {
            img.put_pixel(x as u32, y as u32, c);
        }
    }
}

fn default_db_path() -> anyhow::Result<PathBuf> {
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
