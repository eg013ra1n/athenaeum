//! Detection audit — see exactly what the plate-solve detector finds.
//!
//! Renders a FITS/XISF the way the detector sees it, overlays every detected
//! star, and prints quantitative detection stats (count, binning factor,
//! flux distribution, an 8×8 spatial grid that exposes whether detections
//! pile up on a galaxy/nebula instead of spreading over real stars).
//!
//! Usage:
//!   cargo run -p athenaeum-core --example detection_audit --release -- \
//!       <image.fits> [expected_scale_arcsec] [out.png]
//!
//! `expected_scale_arcsec` (e.g. 0.48 for the 1000 mm narrowband frames)
//! enables the production pre-detection binning, exactly as the solver runs
//! it for a frame with that header scale. Omit for the native-scale view.

use std::path::PathBuf;

use astroimage::{ImageAnalyzer, ImageConverter};
use image::{Rgb, RgbImage};
use imageproc::drawing::draw_hollow_circle_mut;

const DOWNSCALE: usize = 2;

fn to_display_y(star_y: f64, full_height: usize) -> f64 {
    full_height as f64 - 1.0 - star_y
}

fn rgb_from_processed(p: &astroimage::ProcessedImage) -> anyhow::Result<RgbImage> {
    let (w, h) = (p.width as u32, p.height as u32);
    let rgb: Vec<u8> = match p.channels {
        3 => p.data.clone(),
        4 => p.data.chunks_exact(4).flat_map(|c| c[..3].to_vec()).collect(),
        n => anyhow::bail!("unexpected channel count: {n}"),
    };
    RgbImage::from_raw(w, h, rgb)
        .ok_or_else(|| anyhow::anyhow!("RgbImage dimension mismatch"))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let fits = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!(
            "usage: detection_audit <image> [expected_scale_arcsec] [out.png]"
        ))?
        .clone();
    let scale: Option<f32> = args.get(2).and_then(|s| s.parse().ok());
    let out = args
        .get(3)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/detection_audit.png"));

    // Same detector configuration the plate solver uses.
    let analyzer = ImageAnalyzer::new()
        .with_detection_sigma(5.0)
        .with_max_stars(600)
        .with_saturation_fraction(1.0);
    let r = analyzer.detect_fast(&fits)?;

    println!("file:            {fits}");
    println!("detector image:  {} x {}", r.width, r.height);
    println!(
        "header scale:    {} (informational)",
        scale.map(|s| format!("{s:.3}\"/px")).unwrap_or_else(|| "none".into())
    );
    println!("stars detected:  {}", r.stars.len());

    if !r.stars.is_empty() {
        let mut fl: Vec<f64> = r.stars.iter().map(|s| s.flux as f64).collect();
        fl.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |q: f64| fl[((fl.len() - 1) as f64 * q) as usize];
        println!(
            "flux:            min={:.0} p50={:.0} p90={:.0} max={:.0}",
            fl[0],
            pct(0.5),
            pct(0.9),
            fl[fl.len() - 1]
        );

        // 8×8 spatial grid: a healthy starfield spreads roughly evenly; a
        // detector fooled by a bright galaxy/nebula piles detections into a
        // few adjacent cells.
        let (gw, gh) = (8usize, 8usize);
        let mut grid = vec![0u32; gw * gh];
        for s in &r.stars {
            let gx = ((s.x as f64 / r.width as f64) * gw as f64) as usize;
            let gy = ((s.y as f64 / r.height as f64) * gh as f64) as usize;
            grid[gy.min(gh - 1) * gw + gx.min(gw - 1)] += 1;
        }
        let maxc = *grid.iter().max().unwrap_or(&0);
        let mean = r.stars.len() as f64 / (gw * gh) as f64;
        println!(
            "spatial spread:  max cell={maxc}  mean/cell={mean:.1}  \
             (max≫mean ⇒ detections clumped on extended object)"
        );
        println!("8x8 detection grid:");
        for gy in 0..gh {
            let row: String = (0..gw)
                .map(|gx| format!("{:4}", grid[gy * gw + gx]))
                .collect();
            println!("  {row}");
        }

        println!("brightest 12 detections (full-res x, y, flux, peak):");
        for s in r.stars.iter().take(12) {
            println!(
                "  ({:8.1}, {:8.1})  flux={:>12.0}  peak={:>10.0}",
                s.x, s.y, s.flux, s.peak
            );
        }
    }

    // Two artefacts so circle-on-star coincidence is unambiguous:
    //   <out>          downscaled overview (bold double-ring markers)
    //   <out>.crop.png  FULL-RESOLUTION 1400×1000 window centred on the
    //                    brightest detections (star-rich), markers at full res
    // The detector reports y in raw file-row order. `ImageConverter::process`
    // flips the display only when `flip_vertical` is set (ROWORDER != TOP-DOWN);
    // flip the overlay ONLY then, else detections mirror and correct detection
    // looks wrong.
    let green = Rgb([60, 255, 60]);
    let mark = |img: &mut RgbImage, cx: i32, cy: i32, r: i32| {
        if cx >= -r && cy >= -r {
            draw_hollow_circle_mut(img, (cx, cy), r, green);
            draw_hollow_circle_mut(img, (cx, cy), r + 2, green);
        }
    };

    // Overview (downscale OVERVIEW_DS).
    const OVERVIEW_DS: usize = 3;
    let pv = ImageConverter::new()
        .with_downscale(OVERVIEW_DS)
        .process(&fits)?;
    println!("flip_vertical:   {}", pv.flip_vertical);
    let full_h_ov = pv.height * OVERVIEW_DS;
    let mut ov = rgb_from_processed(&pv)?;
    for s in &r.stars {
        let cx = (s.x as f64 / OVERVIEW_DS as f64) as i32;
        let yy = if pv.flip_vertical {
            to_display_y(s.y as f64, full_h_ov)
        } else {
            s.y as f64
        };
        mark(&mut ov, cx, (yy / OVERVIEW_DS as f64) as i32, 7);
    }
    ov.save(&out)?;
    println!("annotated overview: {}", out.display());

    // Full-resolution crop around the brightest-25 detection centroid.
    let nb = r.stars.len().min(25);
    if nb > 0 {
        let mcx = r.stars.iter().take(nb).map(|s| s.x as f64).sum::<f64>() / nb as f64;
        let mcy = r.stars.iter().take(nb).map(|s| s.y as f64).sum::<f64>() / nb as f64;
        let pf = ImageConverter::new().with_downscale(1).process(&fits)?;
        let (fw, fh) = (pf.width, pf.height);
        let (cw, ch) = (1400i64.min(fw as i64), 1000i64.min(fh as i64));
        let x0 = ((mcx as i64) - cw / 2).clamp(0, fw as i64 - cw).max(0);
        let dy_full = if pf.flip_vertical {
            (fh as f64) - 1.0 - mcy
        } else {
            mcy
        };
        let y0 = ((dy_full as i64) - ch / 2).clamp(0, fh as i64 - ch).max(0);
        let mut full = rgb_from_processed(&pf)?;
        for s in &r.stars {
            let yy = if pf.flip_vertical {
                (fh as f64) - 1.0 - s.y as f64
            } else {
                s.y as f64
            };
            mark(&mut full, s.x as i32, yy as i32, 16);
        }
        let crop = image::imageops::crop_imm(
            &full,
            x0 as u32,
            y0 as u32,
            cw as u32,
            ch as u32,
        )
        .to_image();
        let cp = out.with_extension("crop.png");
        crop.save(&cp)?;
        println!(
            "full-res crop:      {} (window {}x{} at {},{})",
            cp.display(),
            cw,
            ch,
            x0,
            y0
        );
    }
    Ok(())
}
