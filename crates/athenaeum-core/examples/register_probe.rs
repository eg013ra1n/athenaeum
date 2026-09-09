//! Dev probe for Checkpoint A: register calibrated subjects onto a
//! calibrated reference and print the alignment as JSON; optionally compare
//! with a WBPP `.xdrz` sidecar, write the registered frame, or compare our
//! resampled frame with a WBPP registered image.
//!
//! cargo run --release -p athenaeum-core --example register_probe -- \
//!   <reference.fits> <subject.fits> [--xdrz <subject.xdrz>] [--write <dir>] \
//!   [--compare <wbpp_registered.xisf>] [--model auto|similarity|affine|homography] \
//!   [--distortion off|polynomial2|polynomial3|polynomial4|auto]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use athenaeum_core::geometry::PixelMap;
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::resample::{warp_rows, Plane};
use athenaeum_core::stacking::register::align::model_name;
use athenaeum_core::stacking::register::detect::{detect_stars, luminance, Star};
use athenaeum_core::stacking::register::frame::{reference_stars, register_frame};
use athenaeum_core::stacking::register::writer::{
    build_registered_cards, source_cards_from_file, write_registered_frame, RegisteredCards,
};
use athenaeum_core::stacking::register::RegistrationConfig;

fn usage() -> ! {
    eprintln!("usage: register_probe <reference.fits> <subject.fits> [--xdrz f] [--write dir] [--compare f.xisf] [--model m] [--distortion d]");
    std::process::exit(2);
}

struct Args {
    reference: PathBuf,
    subject: PathBuf,
    xdrz: Option<PathBuf>,
    write: Option<PathBuf>,
    compare: Option<PathBuf>,
    cfg: RegistrationConfig,
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let reference = PathBuf::from(it.next().unwrap_or_else(|| usage()));
    let subject = PathBuf::from(it.next().unwrap_or_else(|| usage()));
    let mut a = Args {
        reference,
        subject,
        xdrz: None,
        write: None,
        compare: None,
        cfg: RegistrationConfig::default(),
    };
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--xdrz" => a.xdrz = Some(value.into()),
            "--write" => a.write = Some(value.into()),
            "--compare" => a.compare = Some(value.into()),
            "--model" => {
                a.cfg.model = serde_json::from_value(serde_json::Value::String(value))
                    .unwrap_or_else(|_| usage())
            }
            "--distortion" => {
                a.cfg.distortion = serde_json::from_value(serde_json::Value::String(value))
                    .unwrap_or_else(|_| usage())
            }
            _ => usage(),
        }
    }
    a
}

/// `<AlignmentMatrix>` (9 comma-separated floats, row-major, reference →
/// target in the sidecar's coordinates) and `<AlignmentOrigin x y>`.
fn parse_xdrz(path: &Path) -> Result<([[f64; 3]; 3], (f64, f64)), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let between = |open: &str, close: &str| -> Option<String> {
        let s = text.find(open)? + open.len();
        let e = text[s..].find(close)? + s;
        Some(text[s..e].to_string())
    };
    let m = between("<AlignmentMatrix>", "</AlignmentMatrix>").ok_or("no AlignmentMatrix")?;
    let v: Vec<f64> = m
        .split(',')
        .map(|t| t.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if v.len() != 9 {
        return Err(format!("AlignmentMatrix has {} values", v.len()));
    }
    let matrix = [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    let attr = |name: &str| -> Option<f64> {
        let tag = between("<AlignmentOrigin", "/>")?;
        let key = format!("{name}=\"");
        let s = tag.find(&key)? + key.len();
        let e = tag[s..].find('"')? + s;
        tag[s..e].parse().ok()
    };
    Ok((matrix, (attr("x").unwrap_or(0.5), attr("y").unwrap_or(0.5))))
}

fn apply3(m: &[[f64; 3]; 3], x: f64, y: f64) -> (f64, f64) {
    let w = m[2][0] * x + m[2][1] * y + m[2][2];
    (
        (m[0][0] * x + m[0][1] * y + m[0][2]) / w,
        (m[1][0] * x + m[1][1] * y + m[1][2]) / w,
    )
}

/// Max corner/centre disagreement between our reference→subject inverse
/// and the sidecar matrix, under one row-order hypothesis: `flip` mirrors
/// y (`y' = H − 1 − y`) on both sides before the sidecar's origin shift.
fn corner_delta(
    map: &PixelMap,
    theirs: &[[f64; 3]; 3],
    origin: (f64, f64),
    ref_wh: (usize, usize),
    sub_h: usize,
    flip: bool,
) -> f64 {
    let (w, h) = (ref_wh.0 as f64, ref_wh.1 as f64);
    let pts = [
        (0.0, 0.0),
        (w - 1.0, 0.0),
        (0.0, h - 1.0),
        (w - 1.0, h - 1.0),
        (w / 2.0, h / 2.0),
    ];
    let mut worst = 0.0f64;
    for (x, y) in pts {
        let (ox, oy) = map.inverse(x, y);
        let (ty, oy_t) = if flip {
            (h - 1.0 - y, sub_h as f64 - 1.0 - oy)
        } else {
            (y, oy)
        };
        let (px, py) = apply3(theirs, x + origin.0, ty + origin.1);
        let (px, py) = (px - origin.0, py - origin.1);
        worst = worst.max(((px - ox).powi(2) + (py - oy_t).powi(2)).sqrt());
    }
    worst
}

fn read_luminance_any(path: &Path) -> Result<(Vec<f32>, usize, usize), String> {
    if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("fits") || e.eq_ignore_ascii_case("fit"))
        .unwrap_or(false)
    {
        let r = PlaneReader::open(path).map_err(|e| e.to_string())?;
        let planes: Vec<Vec<f32>> = (0..r.channels())
            .map(|p| r.read_plane(p))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        return Ok((luminance(&refs), r.width(), r.height()));
    }
    let (meta, pixels) = astroimage::ImageConverter::read_raw(path).map_err(|e| e.to_string())?;
    let data: Vec<f32> = match pixels {
        astroimage::PixelData::Float32(v) => v,
        astroimage::PixelData::Uint16(v) => v.iter().map(|&u| u as f32 / 65535.0).collect(),
    };
    let n = meta.width * meta.height;
    let planes: Vec<&[f32]> = (0..meta.channels)
        .map(|c| &data[c * n..(c + 1) * n])
        .collect();
    Ok((luminance(&planes), meta.width, meta.height))
}

fn main() {
    let args = parse_args();
    let reference = reference_stars(&args.reference, &args.cfg, None).unwrap_or_else(|e| {
        eprintln!("reference: {e}");
        std::process::exit(1);
    });
    let reg = register_frame(
        &reference,
        &args.subject,
        &args.cfg,
        None,
        &AtomicBool::new(false),
    )
    .unwrap_or_else(|e| {
        eprintln!("subject: {e}");
        std::process::exit(1);
    });
    let mut out = serde_json::json!({
        "reference": args.reference.display().to_string(),
        "subject": args.subject.display().to_string(),
        "referenceStars": reference.stars.len(),
        "detections": reg.detections,
        "durationMs": reg.duration_ms,
    });
    let a = match &reg.outcome {
        Ok(a) => a,
        Err(e) => {
            out["status"] = serde_json::json!("failed");
            out["error"] = serde_json::json!(e.to_string());
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
            std::process::exit(1);
        }
    };
    out["status"] = serde_json::json!(if a.flipped {
        "aligned_flipped"
    } else {
        "aligned"
    });
    out["model"] = serde_json::json!(model_name(a.model, a.distortion_order));
    out["seedMatches"] = serde_json::json!(a.seed_matches);
    out["pairs"] = serde_json::json!(a.pairs);
    out["inliers"] = serde_json::json!(a.inliers);
    out["inlierRatio"] = serde_json::json!(a.inlier_ratio);
    out["rmsPx"] = serde_json::json!(a.rms_px);
    out["sigmaRmsPx"] = serde_json::json!(a.sigma_rms_px);
    out["peakPx"] = serde_json::json!([a.peak_px.0, a.peak_px.1]);
    out["scale"] = serde_json::json!(a.scale);
    out["rotationDeg"] = serde_json::json!(a.rotation_deg);
    out["translation"] = serde_json::json!([a.translation.0, a.translation.1]);
    out["quality"] = serde_json::json!({ "score": a.quality_score, "overlap": a.overlap, "regularity": a.regularity });
    out["warnings"] = serde_json::json!(a.warnings);
    out["linear"] = serde_json::json!(a.map.linear.m);
    out["linearInv"] = serde_json::json!(a.map.linear_inv.m);

    if let Some(x) = &args.xdrz {
        match parse_xdrz(x) {
            Ok((theirs, origin)) => {
                let plain = corner_delta(
                    &a.map,
                    &theirs,
                    origin,
                    (reference.width, reference.height),
                    reg.height,
                    false,
                );
                let flipped = corner_delta(
                    &a.map,
                    &theirs,
                    origin,
                    (reference.width, reference.height),
                    reg.height,
                    true,
                );
                let (sx, sy) = (
                    (theirs[0][0].powi(2) + theirs[1][0].powi(2)).sqrt(),
                    (theirs[0][1].powi(2) + theirs[1][1].powi(2)).sqrt(),
                );
                out["xdrz"] = serde_json::json!({
                    "matrix": theirs,
                    "origin": [origin.0, origin.1],
                    "theirScale": (sx * sy).sqrt(),
                    "theirRotationDeg": theirs[1][0].atan2(theirs[0][0]).to_degrees(),
                    "cornerDeltaPx": { "sameRowOrder": plain, "flippedRowOrder": flipped },
                });
            }
            Err(e) => out["xdrz"] = serde_json::json!({ "error": e }),
        }
    }

    if let Some(dir) = &args.write {
        let stem = args
            .subject
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("subject");
        let target = dir.join(format!("r_{}.fits", stem.trim_start_matches("c_")));
        let result = source_cards_from_file(&args.subject).and_then(|src| {
            let json = a.map.to_json();
            let ref_name = args
                .reference
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("reference");
            let cards = build_registered_cards(
                &src,
                &RegisteredCards {
                    reference_name: ref_name,
                    model: &model_name(a.model, a.distortion_order),
                    transform_json: &json,
                    interpolation: args.cfg.interpolation,
                    clamping: args.cfg.clamping_threshold,
                    rms_px: a.rms_px,
                },
            )?;
            write_registered_frame(
                &args.subject,
                &a.map,
                reference.width,
                reference.height,
                args.cfg.interpolation,
                args.cfg.clamping_threshold,
                &cards,
                &target,
            )
        });
        out["written"] = match result {
            Ok(ch) => serde_json::json!({ "path": target.display().to_string(), "channels": ch }),
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
    }

    if let Some(theirs) = &args.compare {
        match read_luminance_any(theirs) {
            Ok((tl, tw, th)) => {
                // Our resampled luminance of the subject in reference geometry.
                let ours: Result<(Vec<f32>, usize, usize), String> =
                    read_luminance_any(&args.subject).map(|(sl, sw, sh)| {
                        let mut o = vec![f32::NAN; reference.width * reference.height];
                        warp_rows(
                            &Plane::full(&sl, sw, sh),
                            &a.map,
                            reference.width,
                            0,
                            reference.height,
                            args.cfg.interpolation,
                            args.cfg.clamping_threshold,
                            &mut o,
                        );
                        (o, reference.width, reference.height)
                    });
                match ours {
                    Ok((ol, ow, oh)) if (ow, oh) == (tw, th) => {
                        let stars_o: Vec<Star> =
                            detect_stars(&ol, ow, oh, &args.cfg.detection, 500, None);
                        let stars_t: Vec<Star> =
                            detect_stars(&tl, tw, th, &args.cfg.detection, 500, None);
                        let mut dx = Vec::new();
                        let mut dy = Vec::new();
                        let mut ratio = Vec::new();
                        for s in &stars_o {
                            // Try both row orders for the WBPP image.
                            let cand = stars_t.iter().min_by(|p, q| {
                                let d = |t: &Star| {
                                    ((t.x - s.x).powi(2) + (t.y - s.y).powi(2)).min(
                                        (t.x - s.x).powi(2)
                                            + ((th as f64 - 1.0 - t.y) - s.y).powi(2),
                                    )
                                };
                                d(p).total_cmp(&d(q))
                            });
                            if let Some(t) = cand {
                                let same = ((t.x - s.x).powi(2) + (t.y - s.y).powi(2)).sqrt();
                                let flip = ((t.x - s.x).powi(2)
                                    + ((th as f64 - 1.0 - t.y) - s.y).powi(2))
                                .sqrt();
                                let (d, ty) = if same <= flip {
                                    (same, t.y)
                                } else {
                                    (flip, th as f64 - 1.0 - t.y)
                                };
                                if d < 1.5 {
                                    dx.push(t.x - s.x);
                                    dy.push(ty - s.y);
                                    if t.flux > 0.0 && s.flux > 0.0 {
                                        ratio.push(s.flux / t.flux);
                                    }
                                }
                            }
                        }
                        let med = |v: &mut Vec<f64>| {
                            v.sort_by(|a, b| a.total_cmp(b));
                            if v.is_empty() {
                                f64::NAN
                            } else {
                                v[v.len() / 2]
                            }
                        };
                        out["compare"] = serde_json::json!({
                            "ourStars": stars_o.len(), "theirStars": stars_t.len(), "matched": dx.len(),
                            "medianDxPx": med(&mut dx), "medianDyPx": med(&mut dy), "medianFluxRatioOursOverTheirs": med(&mut ratio),
                        });
                    }
                    Ok((_, ow, oh)) => {
                        out["compare"] = serde_json::json!({ "error": format!("geometry mismatch: ours {ow}x{oh}, theirs {tw}x{th}") })
                    }
                    Err(e) => out["compare"] = serde_json::json!({ "error": e }),
                }
            }
            Err(e) => out["compare"] = serde_json::json!({ "error": e }),
        }
    }

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
