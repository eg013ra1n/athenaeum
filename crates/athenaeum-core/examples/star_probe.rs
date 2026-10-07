//! Dev probe (star measurement part 1, spec
//! `docs/superpowers/specs/2026-10-06-star-truth-lab-design.md` §5): runs one image
//! through the production star-measurement entry points and writes every seed and
//! every measured star as JSON lines for the lab's scorecard (athenaeum-lab
//! `star-lab/`). Nothing here re-implements a measurement.
//!
//!   star_probe measure <image.fits> --out <file.jsonl> [--config <json>]
//!       [--plane all|N] [--debayer RGGB|BGGR|GRBG|GBRG] [--rect x,y,w,h]
//!       [--paths measure,betas,register,analysis,legacy-analysis]   (default: measure,betas,register)
//!       [--betas 2.5,4,6,10]   (legacy-analysis = the analysis path, `path: "analysis"`; any other name exits 1)
//!       [--domain auto|unit|adu]   (default auto: the P-4 rule; unit never divides, adu always divides by 65535)
//!   star_probe debayer <mosaic.fits> --pattern RGGB --out <rgb.fits>
//!   star_probe warp <image.fits> --shift dx,dy --rotate deg --out <image.fits>
//!   star_probe dump-fits <fits artifact> --out <file.jsonl>
//!
//! Energies (background, amplitude, signal, peak5; seed peak and flux) are
//! written ÷ 65535, the lab's image units; production carries them ×65535.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use astroimage::processing::vng::vng_debayer_f32;
use astroimage::BayerPattern;
use athenaeum_core::analysis::analyzer::{analyze_frame, build_analyzer};
use athenaeum_core::analysis::config::AnalysisConfig;
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::geometry::{Linear, LinearKind};
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::resample::{warp_rows, Plane};
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::fits_artifact;
use athenaeum_core::stacking::measure::{
    measure_plane_detailed, MeasureOptions, SeedSource, ADU_SCALE,
};
use athenaeum_core::stacking::psf_signal::{self, fit_stars_with_beta, FitParams, StarFit};
use athenaeum_core::stacking::register::detect::{detect_stars, luminance, stars_from_fits};
use athenaeum_core::stacking::register::{DetectionConfig, RegistrationConfig};
use serde_json::{json, Value};

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("star_probe: {msg}");
    std::process::exit(1)
}

fn read_planes(path: &Path) -> (Vec<Vec<f32>>, usize, usize) {
    let r = PlaneReader::open(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
    let planes: Vec<Vec<f32>> = (0..r.channels())
        .map(|p| {
            r.read_plane(p)
                .unwrap_or_else(|e| die(format!("{}: plane {p}: {e}", path.display())))
        })
        .collect();
    (planes, r.width(), r.height())
}

/// P-28: the input's domain, declared by the caller. `Auto` is the P-4 rule; `Unit`
/// never divides (a calibrated frame exceeds 1.5 legitimately where flat division
/// lifts a saturated core); `Adu` always divides by 65535.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Domain {
    Auto,
    Unit,
    Adu,
}

impl Domain {
    fn parse(s: &str) -> Domain {
        match s {
            "auto" => Domain::Auto,
            "unit" => Domain::Unit,
            "adu" => Domain::Adu,
            o => die(format!("--domain auto|unit|adu, not {o}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Domain::Auto => "auto",
            Domain::Unit => "unit",
            Domain::Adu => "adu",
        }
    }
}

/// P-4: the harness rule — a plane above 1.5 is in the ADU domain (`Domain::Auto`);
/// P-28: `Unit` and `Adu` replace the guess with the caller's declaration.
fn normalise(planes: &mut [Vec<f32>], domain: Domain) -> &'static str {
    let adu = match domain {
        Domain::Auto => planes.iter().flatten().any(|v| v.is_finite() && *v > 1.5),
        Domain::Unit => false,
        Domain::Adu => true,
    };
    if adu {
        planes.iter_mut().flatten().for_each(|v| *v /= ADU_SCALE);
        "adu/65535"
    } else {
        "none"
    }
}

fn pattern(s: &str) -> BayerPattern {
    match s {
        "RGGB" => BayerPattern::Rggb,
        "BGGR" => BayerPattern::Bggr,
        "GRBG" => BayerPattern::Grbg,
        "GBRG" => BayerPattern::Gbrg,
        o => die(format!("unknown Bayer pattern {o}")),
    }
}

fn debayer(plane: &[f32], w: usize, h: usize, pat: BayerPattern) -> Vec<Vec<f32>> {
    let rgb = vng_debayer_f32(plane, w, h, pat);
    let n = w * h;
    (0..3).map(|c| rgb[c * n..(c + 1) * n].to_vec()).collect()
}

fn crop(planes: &[Vec<f32>], w: usize, r: (usize, usize, usize, usize)) -> Vec<Vec<f32>> {
    let (x0, y0, cw, ch) = r;
    planes
        .iter()
        .map(|p| {
            (0..ch)
                .flat_map(|y| p[(y0 + y) * w + x0..(y0 + y) * w + x0 + cw].iter().copied())
                .collect()
        })
        .collect()
}

fn n_or_null(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        Value::Null
    }
}

fn fit_line(path: &str, plane: usize, beta: Option<f64>, f: &StarFit) -> Value {
    let s = ADU_SCALE as f64;
    json!({
        "kind": "star", "path": path, "plane": plane, "beta": beta.unwrap_or(f.beta),
        "x": f.x, "y": f.y,
        "background": f.background / s, "amplitude": f.amplitude / s, "signal": f.signal / s,
        "peak5": n_or_null(f.peak5 / s),
        "fwhmX": f.fwhm_x, "fwhmY": f.fwhm_y, "fwhm": f.fwhm(), "ecc": f.eccentricity(),
        "thetaDeg": f.theta.to_degrees(), "residual": f.residual,
    })
}

struct MeasureArgs {
    image: PathBuf,
    out: PathBuf,
    config: Option<PathBuf>,
    plane: Option<usize>,
    debayer: Option<String>,
    rect: Option<(usize, usize, usize, usize)>,
    paths: Vec<String>,
    betas: Vec<f64>,
    domain: Domain,
}

fn parse_measure(mut it: impl Iterator<Item = String>) -> MeasureArgs {
    let image = PathBuf::from(it.next().unwrap_or_else(|| die("measure: <image> missing")));
    let mut a = MeasureArgs {
        image,
        out: PathBuf::new(),
        config: None,
        plane: None,
        debayer: None,
        rect: None,
        paths: vec!["measure".into(), "betas".into(), "register".into()],
        betas: vec![2.5, 4.0, 6.0, 10.0],
        domain: Domain::Auto,
    };
    while let Some(k) = it.next() {
        let mut v = || {
            it.next()
                .unwrap_or_else(|| die(format!("{k}: value missing")))
        };
        match k.as_str() {
            "--out" => a.out = PathBuf::from(v()),
            "--config" => a.config = Some(PathBuf::from(v())),
            "--plane" => {
                let s = v();
                a.plane = if s == "all" {
                    None
                } else {
                    Some(s.parse().unwrap_or_else(|_| die("--plane: all or N")))
                }
            }
            "--debayer" => a.debayer = Some(v()),
            "--rect" => {
                let p: Vec<usize> = v()
                    .split(',')
                    .map(|t| t.parse().unwrap_or_else(|_| die("--rect x,y,w,h")))
                    .collect();
                if p.len() != 4 {
                    die("--rect x,y,w,h")
                }
                a.rect = Some((p[0], p[1], p[2], p[3]));
            }
            "--paths" => a.paths = v().split(',').map(String::from).collect(),
            "--domain" => a.domain = Domain::parse(&v()),
            "--betas" => {
                a.betas = v()
                    .split(',')
                    .map(|t| t.parse().unwrap_or_else(|_| die("--betas")))
                    .collect()
            }
            o => die(format!("unknown flag {o}")),
        }
    }
    if a.out.as_os_str().is_empty() {
        die("--out is required")
    }
    a
}

fn load_config(path: &Option<PathBuf>) -> (MeasureOptions, DetectionConfig, usize, bool) {
    match path {
        None => (
            MeasureOptions::default(),
            DetectionConfig::default(),
            RegistrationConfig::default().max_stars,
            false,
        ),
        Some(p) => {
            let text =
                std::fs::read_to_string(p).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            let v: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            let doc = v.get("config").cloned().unwrap_or(v);
            let cfg: StackingConfig =
                serde_json::from_value(doc).unwrap_or_else(|e| die(format!("config: {e}")));
            let opts = cfg
                .measurement
                .measure_options(cfg.normalization.scale_estimator);
            (
                opts,
                cfg.registration.detection,
                cfg.registration.max_stars,
                true,
            )
        }
    }
}

/// P-20: the Analysis tab reads raw lights; its detector bins the background in integer ADU.
fn analysis_domain_ok(raw_max: f32) -> bool {
    raw_max > 1.5
}

/// Ruling F2 (Plan A): the `--paths` names. `legacy-analysis` is the `analysis` path
/// under the name it keeps once the Analysis tab measures through the shared module;
/// its JSONL `path` stays `"analysis"`. Any other name is an error.
fn resolve_paths(paths: &[String]) -> Result<Vec<String>, String> {
    paths
        .iter()
        .map(|p| match p.as_str() {
            "measure" | "betas" | "register" | "analysis" => Ok(p.clone()),
            "legacy-analysis" => Ok("analysis".to_string()),
            o => Err(format!(
                "unknown path: {o} (known: measure, betas, register, analysis, legacy-analysis)"
            )),
        })
        .collect()
}

/// Returns `Err` instead of exiting once the output file exists, so the
/// `BufWriter` drops (and flushes) first: a failed run leaves complete lines.
/// An unknown `--paths` name is an `Err` before the image is read or any output
/// file exists (ruling F2; `main` exits 1 on it).
fn cmd_measure(a: MeasureArgs) -> Result<(), String> {
    let paths = resolve_paths(&a.paths)?;
    let (mut planes, mut w, mut h) = read_planes(&a.image);
    let raw_max = planes
        .iter()
        .flatten()
        .filter(|v| v.is_finite())
        .fold(f32::MIN, |m, &v| m.max(v));
    let want = |p: &str| paths.iter().any(|q| q == p);
    // Every `analysis` precondition is checked before any output file exists.
    if want("analysis") {
        if a.rect.is_some() || a.debayer.is_some() {
            die("the analysis path takes the file as the Analysis tab does: no --rect, no --debayer")
        }
        if !analysis_domain_ok(raw_max) {
            die(format!(
                "the analysis path takes a raw-domain file (16-bit or ADU float), as the Analysis tab does; \
                 {} is in [0, 1] (use its .u16.fits sibling)",
                a.image.display()
            ))
        }
    }
    let conversion = normalise(&mut planes, a.domain);
    let domain = a.domain.as_str();
    if let Some(p) = &a.debayer {
        if planes.len() != 1 {
            die("--debayer needs a one-plane mosaic")
        }
        planes = debayer(&planes[0], w, h, pattern(p));
    }
    if let Some(r) = a.rect {
        if r.0 + r.2 > w || r.1 + r.3 > h {
            die("--rect outside the image")
        }
        planes = crop(&planes, w, r);
        w = r.2;
        h = r.3;
    }
    let (opts, det, reg_max, from_config) = load_config(&a.config);
    let non_finite: usize = planes.iter().flatten().filter(|v| !v.is_finite()).count();
    let selected: Vec<usize> = match a.plane {
        Some(p) if p < planes.len() => vec![p],
        Some(p) => die(format!("plane {p} of {}", planes.len())),
        None => (0..planes.len()).collect(),
    };
    if want("analysis") && a.image.to_str().is_none() {
        die("non-UTF-8 path")
    }
    let file =
        std::fs::File::create(&a.out).unwrap_or_else(|e| die(format!("{}: {e}", a.out.display())));
    let mut out = BufWriter::new(file);
    let mut emit = |v: Value| writeln!(out, "{v}").unwrap_or_else(|e| die(format!("write: {e}")));
    let options = json!({ "measure": opts, "detection": { "minSnr": det.min_snr, "maxEccentricity": det.max_eccentricity },
                          "registerMaxStars": reg_max, "fromConfig": from_config, "rawMax": raw_max });

    let mut fits0: Option<Vec<StarFit>> = None;
    for &p in &selected {
        let need_measure =
            want("measure") || want("betas") || (want("register") && planes.len() == 1);
        if !need_measure {
            continue;
        }
        let t = Instant::now();
        let (pm, seeds) = measure_plane_detailed(&planes[p], w, h, &opts, None, SeedSource::Fast);
        let wall = t.elapsed().as_millis() as u64;
        let s = ADU_SCALE as f64;
        if want("measure") {
            for (i, sd) in seeds.iter().enumerate() {
                emit(
                    json!({"kind": "seed", "plane": p, "i": i, "x": sd.x, "y": sd.y, "peak": sd.peak / s, "flux": sd.flux / s}),
                );
            }
            for f in &pm.fits {
                emit(fit_line("measure", p, None, f))
            }
            emit(
                json!({"kind": "frame", "path": "measure", "plane": p, "beta": pm.channel.beta,
                        "seeds": seeds.len(), "fits": pm.fits.len(),
                        "statistic": "residual-weighted mean over all accepted fits",
                        "frameFwhm": pm.channel.fwhm_px, "frameEcc": pm.channel.eccentricity,
                        "channel": pm.channel, "options": options, "conversion": conversion,
                        "domain": domain, "nonFinite": non_finite, "wallMs": wall}),
            );
        }
        if want("betas") {
            let scaled: Vec<f32> = planes[p].iter().map(|v| v * ADU_SCALE).collect();
            for &b in &a.betas {
                let t = Instant::now();
                let o = fit_stars_with_beta(&scaled, w, h, &seeds, b, &FitParams::default(), None);
                let (ff, fe) = psf_signal::frame_shape(&o.fits).unwrap_or((0.0, 0.0));
                for f in &o.fits {
                    emit(fit_line("betas", p, Some(b), f))
                }
                emit(
                    json!({"kind": "frame", "path": "betas", "plane": p, "beta": b, "seeds": seeds.len(),
                            "fits": o.fits.len(), "statistic": "residual-weighted mean over all accepted fits",
                            "frameFwhm": ff, "frameEcc": fe, "options": options, "conversion": conversion,
                            "domain": domain, "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}),
                );
            }
        }
        if p == selected[0] {
            fits0 = Some(pm.fits.clone())
        }
    }

    if want("register") {
        let t = Instant::now();
        // P-17, as `register/frame.rs::reference_stars`: one plane reuses the
        // Measure fits, any other count detects on the luminance of all planes.
        let (stars, how) = if planes.len() == 1 {
            let fits = fits0
                .clone()
                .ok_or_else(|| "register on a mono plane needs the measure fits".to_string())?;
            (
                stars_from_fits(&fits, &det, reg_max),
                "stars_from_fits on the Measure fits",
            )
        } else {
            let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
            let lum = luminance(&refs);
            (
                detect_stars(&lum, w, h, &det, reg_max, None),
                "detect_stars on the luminance of all planes",
            )
        };
        for s in &stars {
            let (sx, sy) = s
                .sigma
                .map(|(a, b)| (json!(a), json!(b)))
                .unwrap_or((Value::Null, Value::Null));
            emit(
                json!({"kind": "star", "path": "register", "plane": selected[0], "beta": null,
                        "x": s.x, "y": s.y, "flux": s.flux, "sigmaX": sx, "sigmaY": sy}),
            );
        }
        emit(
            json!({"kind": "frame", "path": "register", "plane": selected[0], "beta": null, "seeds": null,
                    "fits": stars.len(), "statistic": how, "frameFwhm": null, "frameEcc": null,
                    "options": options, "conversion": conversion, "domain": domain, "nonFinite": non_finite,
                    "wallMs": t.elapsed().as_millis() as u64}),
        );
    }

    if want("analysis") {
        let t = Instant::now();
        let analyzer = build_analyzer(&AnalysisConfig::default(), None);
        let img = a.image.to_str().expect("checked before File::create");
        match analyze_frame(img, &analyzer, "star_probe") {
            Ok((fa, stars, flip)) => {
                for s in &stars {
                    emit(
                        json!({"kind": "star", "path": "analysis", "plane": 0, "beta": s.beta,
                                "x": s.x, "y": s.y, "peak": s.peak, "flux": s.flux, "fwhm": s.fwhm,
                                "fwhmX": s.fwhm_x, "fwhmY": s.fwhm_y, "ecc": s.eccentricity,
                                "thetaDeg": s.theta.to_degrees(), "snr": s.snr, "hfr": s.hfr,
                                "fitMethod": s.fit_method, "residual": s.fit_residual}),
                    );
                }
                emit(
                    json!({"kind": "frame", "path": "analysis", "plane": 0, "beta": null, "seeds": null,
                            "fits": stars.len(), "statistic": "clipped weighted median over e <= 0.8",
                            "frameFwhm": fa.median_fwhm, "frameEcc": fa.median_eccentricity,
                            "frame": fa, "flipVertical": flip,
                            "options": { "analysis": AnalysisConfig::default() }, "conversion": "none", "domain": domain,
                            "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}),
                );
            }
            Err(e) => return Err(format!("analysis: {e:#}")),
        }
    }
    out.flush().map_err(|e| format!("flush: {e}"))
}

fn cmd_debayer(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(
        it.next()
            .unwrap_or_else(|| die("debayer: <mosaic> missing")),
    );
    let (mut pat, mut out) = (String::from("RGGB"), PathBuf::new());
    while let Some(k) = it.next() {
        let v = it
            .next()
            .unwrap_or_else(|| die(format!("{k}: value missing")));
        match k.as_str() {
            "--pattern" => pat = v,
            "--out" => out = PathBuf::from(v),
            o => die(format!("unknown flag {o}")),
        }
    }
    let (mut planes, w, h) = read_planes(&input);
    normalise(&mut planes, Domain::Auto);
    if planes.len() != 1 {
        die("debayer needs a one-plane mosaic")
    }
    let rgb: Vec<f32> = debayer(&planes[0], w, h, pattern(&pat)).concat();
    write_fits_f32(&out, w, h, 3, &rgb, &[])
        .unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
}

fn cmd_dump_fits(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(
        it.next()
            .unwrap_or_else(|| die("dump-fits: <artifact> missing")),
    );
    let out = match (it.next().as_deref(), it.next()) {
        (Some("--out"), Some(p)) => PathBuf::from(p),
        _ => die("dump-fits <artifact> --out <file.jsonl>"),
    };
    let fits = fits_artifact::read_fits(&input)
        .unwrap_or_else(|e| die(format!("{}: {e}", input.display())));
    let mut w = BufWriter::new(
        std::fs::File::create(&out).unwrap_or_else(|e| die(format!("{}: {e}", out.display()))),
    );
    for f in &fits {
        writeln!(w, "{}", fit_line("artifact", 0, None, f)).unwrap_or_else(|e| die(e))
    }
    w.flush().unwrap_or_else(|e| die(e));
}

fn cmd_warp(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(it.next().unwrap_or_else(|| die("warp: <image> missing")));
    let (mut dx, mut dy, mut rot, mut out) = (0.0f64, 0.0f64, 0.0f64, PathBuf::new());
    while let Some(k) = it.next() {
        let v = it
            .next()
            .unwrap_or_else(|| die(format!("{k}: value missing")));
        match k.as_str() {
            "--shift" => {
                let p: Vec<f64> = v
                    .split(',')
                    .map(|t| t.parse().unwrap_or_else(|_| die("--shift dx,dy")))
                    .collect();
                if p.len() != 2 {
                    die("--shift dx,dy")
                }
                dx = p[0];
                dy = p[1];
            }
            "--rotate" => rot = v.parse().unwrap_or_else(|_| die("--rotate deg")),
            "--out" => out = PathBuf::from(v),
            o => die(format!("unknown flag {o}")),
        }
    }
    if out.as_os_str().is_empty() {
        die("warp: --out missing")
    }
    let (planes, w, h) = read_planes(&input);
    let warped = warp_affine(&planes, w, h, dx, dy, rot);
    write_fits_f32(&out, w, h, warped.len(), &warped.concat(), &[])
        .unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
}

/// Output pixel p reads the source at R(θ)(p − c) + c + (dx, dy), through the
/// production resampler with the registration defaults (B-spline, clamping 0.30).
/// A bare `Linear` is an inverse map applied as-is (`geometry::pixel_map`), pixel
/// centres sit on integer coordinates (`resample::sample_at` covers `[0, w−1]`).
fn warp_affine(
    planes: &[Vec<f32>],
    w: usize,
    h: usize,
    dx: f64,
    dy: f64,
    rot_deg: f64,
) -> Vec<Vec<f32>> {
    let (c, s) = (rot_deg.to_radians().cos(), rot_deg.to_radians().sin());
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let m = [
        [c, -s, cx - c * cx + s * cy + dx],
        [s, c, cy - s * cx - c * cy + dy],
        [0.0, 0.0, 1.0],
    ];
    let map = Linear {
        kind: LinearKind::Affine,
        m,
    };
    let reg = RegistrationConfig::default();
    planes
        .iter()
        .map(|p| {
            let mut o = vec![0.0f32; w * h];
            warp_rows(
                &Plane::full(p, w, h),
                &map,
                w,
                0,
                h,
                reg.interpolation,
                reg.clamping_threshold,
                &mut o,
            );
            o
        })
        .collect()
}

fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let mut it = std::env::args().skip(1);
    match it.next().as_deref() {
        Some("measure") => cmd_measure(parse_measure(it)).unwrap_or_else(|e| die(e)),
        Some("debayer") => cmd_debayer(it),
        Some("dump-fits") => cmd_dump_fits(it),
        Some("warp") => cmd_warp(it),
        _ => die("usage: star_probe measure|debayer|warp|dump-fits …"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gaussian_field(w: usize, h: usize, nan_border: usize, stars: bool) -> Vec<f32> {
        let mut d = vec![0.05f32; w * h];
        if stars {
            for k in 0..40 {
                let (cx, cy) = (30.0 + (k % 8) as f32 * 25.0, 30.0 + (k / 8) as f32 * 25.0);
                for y in 0..h {
                    for x in 0..w {
                        let r2 = (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2);
                        d[y * w + x] += 0.4 * (-r2 / (2.0 * 1.5f32.powi(2))).exp();
                    }
                }
            }
        }
        // deterministic pseudo-noise (the MRS noise estimate needs some)
        for (i, v) in d.iter_mut().enumerate() {
            *v += 0.002 * (((i as u64).wrapping_mul(2_654_435_761) % 1000) as f32 / 1000.0 - 0.5);
        }
        for y in 0..h {
            for x in 0..w {
                if x < nan_border || y < nan_border {
                    d[y * w + x] = f32::NAN
                }
            }
        }
        d
    }

    fn args(img: PathBuf, out: PathBuf, paths: &[&str]) -> MeasureArgs {
        MeasureArgs {
            image: img,
            out,
            config: None,
            plane: None,
            debayer: None,
            rect: None,
            paths: paths.iter().map(|p| p.to_string()).collect(),
            betas: vec![4.0],
            domain: Domain::Auto,
        }
    }

    fn read_lines(out: &Path) -> Vec<Value> {
        std::fs::read_to_string(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn run_paths(dir: &Path, data: Vec<f32>, w: usize, h: usize, paths: &[&str]) -> Vec<Value> {
        let img = dir.join("p.fits");
        write_fits_f32(&img, w, h, 1, &data, &[]).unwrap();
        let out = dir.join("p.jsonl");
        cmd_measure(args(img, out.clone(), paths)).unwrap();
        read_lines(&out)
    }

    fn run(dir: &Path, data: Vec<f32>, w: usize, h: usize) -> Vec<Value> {
        run_paths(dir, data, w, h, &["measure", "betas", "register"])
    }

    #[test]
    fn nan_border_runs_every_path() {
        let d = tempfile::tempdir().unwrap();
        let lines = run(d.path(), gaussian_field(240, 240, 6, true), 240, 240);
        let frames: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "frame").collect();
        assert_eq!(frames.len(), 3);
        // 6 full rows of 240 + 6 columns of the remaining 234 rows.
        assert!(frames.iter().all(|f| f["nonFinite"] == 6 * 240 + 6 * 234));
        for path in ["measure", "betas", "register"] {
            let f = frames.iter().find(|f| f["path"] == path).unwrap();
            assert!(f["fits"].as_u64().unwrap() > 0, "{f}");
            assert!(
                lines
                    .iter()
                    .any(|v| v["kind"] == "star" && v["path"] == path),
                "no star line for {path}"
            );
        }
    }

    #[test]
    fn domain_auto_is_the_p4_rule_on_both_sides_of_1_5() {
        let mut p = vec![vec![0.2f32, 1.5]];
        assert_eq!(normalise(&mut p, Domain::Auto), "none");
        assert_eq!(p[0], vec![0.2f32, 1.5]);
        let mut p = vec![vec![0.2f32, 1.5001]];
        assert_eq!(normalise(&mut p, Domain::Auto), "adu/65535");
        assert_eq!(p[0], vec![0.2f32 / ADU_SCALE, 1.5001f32 / ADU_SCALE]);
    }

    #[test]
    fn domain_unit_keeps_a_calibrated_plane_above_1_5() {
        let mut p = vec![vec![0.05f32, 1.645], vec![0.04f32, 0.9]];
        assert_eq!(normalise(&mut p, Domain::Unit), "none");
        assert_eq!(p, vec![vec![0.05f32, 1.645], vec![0.04f32, 0.9]]);
    }

    #[test]
    fn domain_adu_always_divides() {
        let mut p = vec![vec![0.5f32, 1.0]];
        assert_eq!(normalise(&mut p, Domain::Adu), "adu/65535");
        assert_eq!(p[0], vec![0.5f32 / ADU_SCALE, 1.0f32 / ADU_SCALE]);
    }

    #[test]
    fn domain_parses_and_the_frame_lines_record_it() {
        assert_eq!(Domain::parse("auto"), Domain::Auto);
        assert_eq!(Domain::parse("unit"), Domain::Unit);
        assert_eq!(Domain::parse("adu"), Domain::Adu);
        let d = tempfile::tempdir().unwrap();
        // a unit-domain field with one pixel at 1.645 (a flat-lifted saturated core)
        let mut data = gaussian_field(240, 240, 0, true);
        data[5 * 240 + 5] = 1.645;
        let img = d.path().join("u.fits");
        write_fits_f32(&img, 240, 240, 1, &data, &[]).unwrap();
        let out = d.path().join("u.jsonl");
        let mut a = args(img, out.clone(), &["measure", "betas", "register"]);
        a.domain = Domain::Unit;
        cmd_measure(a).unwrap();
        let lines = read_lines(&out);
        let frames: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "frame").collect();
        assert_eq!(frames.len(), 3);
        for f in &frames {
            assert_eq!(f["domain"], "unit", "{f}");
            assert_eq!(f["conversion"], "none", "{f}");
        }
        let m = frames.iter().find(|f| f["path"] == "measure").unwrap();
        assert!(m["fits"].as_u64().unwrap() > 0, "{m}");
    }

    #[test]
    fn analysis_refuses_a_unit_domain_file() {
        // the 1.5 boundary, both sides
        assert!(!analysis_domain_ok(1.5));
        assert!(analysis_domain_ok(1.5001));
        assert!(!analysis_domain_ok(1.0));
        assert!(!analysis_domain_ok(f32::MIN));
        assert!(analysis_domain_ok(40000.0));
    }

    #[test]
    fn starless_plane_writes_frame_lines_only() {
        let d = tempfile::tempdir().unwrap();
        let lines = run(d.path(), gaussian_field(128, 128, 0, false), 128, 128);
        assert!(lines.iter().all(|v| v["kind"] == "frame"));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn starless_raw_domain_plane_writes_the_analysis_frame_line() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<f32> = gaussian_field(128, 128, 0, false)
            .iter()
            .map(|v| (v * 65535.0).round())
            .collect();
        let lines = run_paths(d.path(), data, 128, 128, &["analysis"]);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["path"], "analysis");
        assert_eq!(lines[0]["fits"], 0);
        assert_eq!(lines[0]["conversion"], "none");
    }

    /// The lines of a probe output without the analysis path's two clock fields: the
    /// frame line's `wallMs` and the production `FrameAnalysis.analyzed_at` stamp it
    /// carries in `frame`. Two runs of the same path differ in those two only.
    fn without_clocks(out: &Path) -> Vec<String> {
        read_lines(out)
            .into_iter()
            .map(|mut v| {
                if let Some(o) = v.as_object_mut() {
                    o.remove("wallMs");
                    if let Some(f) = o.get_mut("frame").and_then(Value::as_object_mut) {
                        f.remove("analyzed_at");
                    }
                }
                v.to_string()
            })
            .collect()
    }

    /// Ruling F2 (Plan A, Task 1b): `legacy-analysis` is the `analysis` path under its
    /// lasting name (the same lines, `path: "analysis"`), and an unknown `--paths` name
    /// is refused before any output file exists. The outputs are compared without their
    /// clock fields (`without_clocks`): a byte compare fails on two runs of one path.
    #[test]
    fn legacy_analysis_is_an_alias_of_analysis_and_an_unknown_path_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<f32> = gaussian_field(128, 128, 0, true).iter().map(|v| (v * 65535.0).round()).collect();
        let img = d.path().join("p.fits");
        write_fits_f32(&img, 128, 128, 1, &data, &[]).unwrap();
        let a = d.path().join("a.jsonl");
        cmd_measure(args(img.clone(), a.clone(), &["analysis"])).unwrap();
        let b = d.path().join("b.jsonl");
        cmd_measure(args(img.clone(), b.clone(), &["legacy-analysis"])).unwrap();
        assert_eq!(without_clocks(&a), without_clocks(&b));
        let lines = read_lines(&b);
        assert!(lines.iter().filter(|v| v["kind"] == "star").count() > 0, "the control is not empty");
        assert!(lines.iter().all(|v| v["path"] == "analysis"));
        let c = d.path().join("c.jsonl");
        let err = cmd_measure(args(img, c.clone(), &["bogus"])).unwrap_err();
        assert!(err.contains("unknown path: bogus"), "{err}");
        assert!(!c.exists(), "no output file for a refused path");
    }

    #[test]
    fn a_half_pixel_shift_moves_a_star_and_keeps_its_flux() {
        let (w, h) = (96usize, 96usize);
        let mut d = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let r2 = (x as f32 - 48.0).powi(2) + (y as f32 - 48.0).powi(2);
                d[y * w + x] = (-r2 / (2.0 * 2.0f32.powi(2))).exp();
            }
        }
        let o = warp_affine(&[d.clone()], w, h, 0.5, 0.0, 0.0).remove(0);
        // the resampler writes NaN where the source is out of range: here the last column only
        assert_eq!(o.iter().filter(|v| !v.is_finite()).count(), h);
        let s0: f64 = d.iter().map(|&v| v as f64).sum();
        let s1: f64 = o.iter().filter(|v| v.is_finite()).map(|&v| v as f64).sum();
        assert!((s1 / s0 - 1.0).abs() < 1e-3, "flux {s1} vs {s0}");
        let cx: f64 = o
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .map(|(i, &v)| (i % w) as f64 * v as f64)
            .sum::<f64>()
            / s1;
        assert!((cx - 47.5).abs() < 0.02, "centroid {cx}");
    }
}
