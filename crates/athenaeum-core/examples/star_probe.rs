//! Dev probe (star measurement part 1, spec
//! `docs/superpowers/specs/2026-10-06-star-truth-lab-design.md` §5): runs one image
//! through the production star-measurement entry points and writes every seed and
//! every measured star as JSON lines for the lab's scorecard (athenaeum-lab
//! `star-lab/`). Nothing here re-implements a measurement.
//!
//!   star_probe measure <image.fits> --out <file.jsonl> [--config <json>]
//!       [--plane all|N] [--debayer RGGB|BGGR|GRBG|GBRG] [--rect x,y,w,h]
//!       [--paths measure,betas,register,analysis] [--betas 2.5,4,6,10]
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
use athenaeum_core::integration::plane_reader::PlaneReader;
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

/// P-4: the harness rule — a plane above 1.5 is in the ADU domain.
fn normalise(planes: &mut [Vec<f32>]) -> &'static str {
    let adu = planes.iter().flatten().any(|v| v.is_finite() && *v > 1.5);
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
        paths: vec![
            "measure".into(),
            "betas".into(),
            "register".into(),
            "analysis".into(),
        ],
        betas: vec![2.5, 4.0, 6.0, 10.0],
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

fn cmd_measure(a: MeasureArgs) {
    let (mut planes, mut w, mut h) = read_planes(&a.image);
    let raw_max = planes
        .iter()
        .flatten()
        .filter(|v| v.is_finite())
        .fold(f32::MIN, |m, &v| m.max(v));
    let conversion = normalise(&mut planes);
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
    let file =
        std::fs::File::create(&a.out).unwrap_or_else(|e| die(format!("{}: {e}", a.out.display())));
    let mut out = BufWriter::new(file);
    let mut emit = |v: Value| writeln!(out, "{v}").unwrap_or_else(|e| die(format!("write: {e}")));
    let want = |p: &str| a.paths.iter().any(|q| q == p);
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
                        "nonFinite": non_finite, "wallMs": wall}),
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
                            "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}),
                );
            }
        }
        if p == selected[0] {
            fits0 = Some(pm.fits.clone())
        }
    }

    if want("register") {
        let t = Instant::now();
        let (stars, how) = if planes.len() == 3 {
            let lum = luminance(&[
                planes[0].as_slice(),
                planes[1].as_slice(),
                planes[2].as_slice(),
            ]);
            (
                detect_stars(&lum, w, h, &det, reg_max, None),
                "detect_stars on the RGB luminance",
            )
        } else {
            let fits = fits0
                .clone()
                .unwrap_or_else(|| die("register on a mono plane needs the measure fits"));
            (
                stars_from_fits(&fits, &det, reg_max),
                "stars_from_fits on the Measure fits",
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
                    "options": options, "conversion": conversion, "nonFinite": non_finite,
                    "wallMs": t.elapsed().as_millis() as u64}),
        );
    }

    if want("analysis") {
        if a.rect.is_some() || a.debayer.is_some() {
            die("the analysis path takes the file as the Analysis tab does: no --rect, no --debayer")
        }
        if !analysis_domain_ok(raw_max) {
            die("the analysis path takes a raw-domain file (16-bit or ADU float), as the Analysis tab does; \
                 this file is in [0, 1] (use its .u16.fits sibling)")
        }
        let t = Instant::now();
        let analyzer = build_analyzer(&AnalysisConfig::default(), None);
        let img = a.image.to_str().unwrap_or_else(|| die("non-UTF-8 path"));
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
                            "options": { "analysis": AnalysisConfig::default() }, "conversion": "file as is (raw domain)",
                            "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}),
                );
            }
            Err(e) => die(format!("analysis: {e:#}")),
        }
    }
    out.flush().unwrap_or_else(|e| die(format!("flush: {e}")));
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
    normalise(&mut planes);
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

fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let mut it = std::env::args().skip(1);
    match it.next().as_deref() {
        Some("measure") => cmd_measure(parse_measure(it)),
        Some("debayer") => cmd_debayer(it),
        Some("dump-fits") => cmd_dump_fits(it),
        Some("warp") => die("warp: Task 6"),
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

    fn run(dir: &Path, data: Vec<f32>, w: usize, h: usize) -> Vec<Value> {
        let img = dir.join("p.fits");
        write_fits_f32(&img, w, h, 1, &data, &[]).unwrap();
        let out = dir.join("p.jsonl");
        cmd_measure(MeasureArgs {
            image: img,
            out: out.clone(),
            config: None,
            plane: None,
            debayer: None,
            rect: None,
            paths: vec!["measure".into(), "betas".into(), "register".into()],
            betas: vec![4.0],
        });
        std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn nan_border_runs_every_path() {
        let d = tempfile::tempdir().unwrap();
        let lines = run(d.path(), gaussian_field(240, 240, 6, true), 240, 240);
        let frames: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "frame").collect();
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f["nonFinite"].as_u64().unwrap() > 0));
        let measure = frames.iter().find(|f| f["path"] == "measure").unwrap();
        assert!(measure["fits"].as_u64().unwrap() > 0, "{measure}");
    }

    #[test]
    fn analysis_refuses_a_unit_domain_file() {
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
}
