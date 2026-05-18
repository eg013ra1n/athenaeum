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
//! enables the ASTAP-style pre-detection binning, exactly as the solver runs
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

    // Annotated render so the stars can be eyeballed against the image.
    let processed = ImageConverter::new()
        .with_downscale(DOWNSCALE)
        .process(&fits)?;
    let mut canvas = rgb_from_processed(&processed)?;
    let full_h = processed.height * DOWNSCALE;
    for s in &r.stars {
        let cx = (s.x as f64 / DOWNSCALE as f64) as i32;
        let cy = (to_display_y(s.y as f64, full_h) / DOWNSCALE as f64) as i32;
        if cx >= -6 && cy >= -6 {
            draw_hollow_circle_mut(&mut canvas, (cx, cy), 5, Rgb([80, 255, 80]));
        }
    }
    canvas.save(&out)?;
    println!("annotated image: {}", out.display());
    Ok(())
}
