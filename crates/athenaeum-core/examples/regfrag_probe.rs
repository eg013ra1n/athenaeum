//! Research probe (regfrag): the registration stages of one frame, exactly as `register::align` runs them, with the
//! counts each stage leaves; the quad seed's stage columns, the coverage index (through the core `coverage`) and a map
//! grid evaluator.
//!
//!   regfrag_probe detect <image.fits> <out.tsv>              reference_stars(no fits) -> x y flux sx sy
//!   regfrag_probe fits <subject.p0.athf> <out.tsv>           stars_from_fits -> x y flux sx sy
//!   regfrag_probe fitsall <subject.p0.athf> <out.tsv>        every fit with the register-cut inputs
//!   regfrag_probe batch <ref.tsv> <ref_w> <ref_h> <manifest> <out.jsonl> [--seed plain|robust]
//!       manifest line: name sub.tsv sub_w sub_h tol max_stars hint [gate]
//!       hint = '-' or nine comma-separated numbers (a subject -> reference linear map, given to align as a
//!       WcsFirst seed, so the real align confirms it at the seed radius and pairs / fits from it).
//!       gate = '-' (or absent: the fixed `SCALE_RANGE`) or a scale ratio r: the window `scale_gate_for` builds for
//!       a frame whose pixel scale is r times the reference's, as a run builds it.
//!       --seed robust (default): the real `align()`, whose quad seed is `robust_quad_seed`.
//!       --seed plain: today's path before the robust seed, replayed through the restated stages (plain `fit_affine`
//!       seed -> pairing -> `ransac_fit` / `refit_weighted` -> the one re-pair), with `align`'s scale gate. A line
//!       with a hint takes the real `align()` in either mode (the hint path is not restated; `replay` says which).
//!       Every `align` object carries `regularity` (the reference grid) and `coverage` / `coverageCells` (the core
//!       `coverage` over the restated final refit; null when a hint leads, and when the restated stages fail).
//!   regfrag_probe mapgrid <in.jsonl> <out.jsonl>
//!       in line:  {"key", "transformJson", "w", "h"}   (transformJson: a string or an object)
//!       out line: {"key", "grid": [[x, y, fx, fy], ...]} on the 33 x 23 grid over (0..w, 0..h), rows first,
//!       through `PixelMap::from_json` + `forward_exact`.
use std::io::{BufRead, Write};
use std::path::Path;

use athenaeum_core::geometry::{
    ransac_fit, refit_weighted, KdTree2, Linear, LinearKind, Pair, PixelMap, RansacConfig,
    RansacResult, RefitResult,
};
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::fits_artifact;
use athenaeum_core::stacking::measure::{measure_frame_pass1, measure_frame_pass2};
use athenaeum_core::stacking::register::align::{
    align, coverage, resolve_model, robust_quad_seed, AlignError, SeedPolicy, CLIP_SIGMA,
    MIN_INLIERS, QUAD_TOLERANCE, SCALE_RANGE,
};
use athenaeum_core::stacking::register::detect::{stars_from_fits, Star};
use athenaeum_core::stacking::register::frame::reference_stars;
use athenaeum_core::stacking::register::{scale_gate_for, ModelChoice, RegistrationConfig};
use serde_json::json;
use solvemyastro::quad::{build_quads, fit_affine, group_size_for, match_quads_stats, Affine};

fn die(m: impl std::fmt::Display) -> ! {
    eprintln!("regfrag_probe: {m}");
    std::process::exit(1)
}

fn write_stars(path: &str, stars: &[Star]) {
    let mut o = std::io::BufWriter::new(std::fs::File::create(path).unwrap_or_else(|e| die(e)));
    writeln!(o, "x\ty\tflux\tsx\tsy").unwrap();
    for s in stars {
        let (sx, sy) = s.sigma.unwrap_or((0.0, 0.0));
        writeln!(
            o,
            "{:.6}\t{:.6}\t{:.9e}\t{:.6}\t{:.6}",
            s.x, s.y, s.flux, sx, sy
        )
        .unwrap();
    }
}

fn read_stars(path: &str) -> Vec<Star> {
    let f = std::fs::File::open(path).unwrap_or_else(|e| die(format!("{path}: {e}")));
    let mut out = Vec::new();
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.unwrap();
        if i == 0 {
            continue;
        }
        let v: Vec<f64> = line
            .split('\t')
            .map(|t| t.parse().unwrap_or(f64::NAN))
            .collect();
        out.push(Star {
            x: v[0],
            y: v[1],
            flux: v[2],
            sigma: (v[3] > 0.0 && v[4] > 0.0).then_some((v[3], v[4])),
        });
    }
    out
}

fn m9(l: &Linear) -> Vec<f64> {
    l.m.iter().flatten().copied().collect()
}

/// `align`'s pair_through, re-stated over the public KD-tree.
fn pair(
    project: &dyn Fn(f64, f64) -> (f64, f64),
    sub: &[Star],
    refs: &[Star],
    tree: &KdTree2,
    r: f64,
) -> (Vec<Pair>, Vec<(f64, f64)>, bool) {
    let (mut p, mut s, mut all) = (Vec::new(), Vec::new(), true);
    for st in sub {
        let (px, py) = project(st.x, st.y);
        if let Some((j, _)) = tree.nearest_within(px, py, r) {
            let rr = &refs[j];
            p.push(((st.x, st.y), (rr.x, rr.y)));
            match (st.sigma, rr.sigma) {
                (Some(a), Some(b)) => s.push((
                    (a.0 * a.0 + b.0 * b.0).sqrt(),
                    (a.1 * a.1 + b.1 * b.1).sqrt(),
                )),
                _ => {
                    all = false;
                    s.push((0.0, 0.0))
                }
            }
        }
    }
    (p, s, all)
}

/// Which quad seed the restated stages start from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SeedMode {
    /// Today's call before the robust seed: `fit_affine` over every quad match.
    Plain,
    /// `robust_quad_seed`, as `seed_affine` calls it.
    Robust,
}

/// One pairing's RANSAC and refit, as `align`'s `ransac_and_refit` runs them: the stage's JSON and, when both
/// succeeded with at least `MIN_INLIERS` inliers, the fits themselves.
struct StageFit {
    json: serde_json::Value,
    fits: Result<(RansacResult, RefitResult), AlignError>,
}

fn ransac_refit(
    p: &[Pair],
    s: &[(f64, f64)],
    all: bool,
    cfg: &RegistrationConfig,
    geo: (usize, usize),
) -> StageFit {
    let kind = resolve_model(cfg.model, p.len());
    let mut rc = RansacConfig::new(kind, geo.0 as f64, geo.1 as f64);
    rc.tolerance_px = cfg.ransac_tolerance_px;
    rc.max_iterations = cfg.ransac_max_iterations;
    rc.min_inliers = MIN_INLIERS;
    let Some(r) = ransac_fit(p, &rc) else {
        return StageFit {
            json: json!({"pairs": p.len(), "ransac": null}),
            fits: Err(AlignError::TooFewInliers { inliers: 0 }),
        };
    };
    let k2 = resolve_model(cfg.model, r.inliers.len());
    let sig = all.then_some(s);
    let f = refit_weighted(p, &r.inliers, sig, k2, CLIP_SIGMA);
    let json = json!({"pairs": p.len(), "ransacInliers": r.inliers.len(), "ransacIters": r.iterations,
           "overlap": r.quality.overlap, "regularity": r.quality.regularity,
           "refitInliers": f.as_ref().map(|f| f.inliers.len()), "refitRms": f.as_ref().map(|f| f.rms_px),
           "refitM": f.as_ref().map(|f| m9(&f.linear)), "ransacM": m9(&r.linear)});
    let fits = match f {
        Some(f) if f.inliers.len() >= MIN_INLIERS => Ok((r, f)),
        Some(f) => Err(AlignError::TooFewInliers {
            inliers: f.inliers.len(),
        }),
        None => Err(AlignError::TooFewInliers {
            inliers: r.inliers.len(),
        }),
    };
    StageFit { json, fits }
}

/// Where the restated stages end: the last fit `align` takes before its distortion layer, with its coverage.
struct Final {
    pairs: usize,
    repaired: usize,
    seed_matches: usize,
    ransac: RansacResult,
    refit: RefitResult,
    /// The core `coverage` over the refit's inliers: (coverage, cells).
    coverage: (f64, usize),
}

fn affine_linear(a: &Affine) -> Linear {
    Linear::from_flat(
        LinearKind::Affine,
        [a.a1, a.b1, a.c1, a.a2, a.b2, a.c2, 0.0, 0.0, 1.0],
    )
}

/// The stages 1-4b of `align` (quad seed, pairing, RANSAC + refit, the one re-pair), restated over the public
/// pieces so each leaves its counts. `mode` picks the seed.
fn stages(
    sub: &[Star],
    refs: &[Star],
    cfg: &RegistrationConfig,
    geo: (usize, usize),
    mode: SeedMode,
) -> (serde_json::Value, Result<Final, AlignError>) {
    let sp: Vec<(f64, f64)> = sub.iter().map(|s| (s.x, s.y)).collect();
    let rp: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
    let sq = build_quads(&sp, sp.len(), group_size_for(sp.len()));
    let rq = build_quads(&rp, rp.len(), group_size_for(rp.len()));
    let (matches, raw) = match_quads_stats(&sq, &rq, QUAD_TOLERANCE);
    let qpairs: Vec<[f64; 4]> = matches
        .iter()
        .map(|&(i, j)| {
            [
                sq[i].center.0,
                sq[i].center.1,
                rq[j].center.0,
                rq[j].center.1,
            ]
        })
        .collect();
    let qstars: Vec<[usize; 8]> = matches
        .iter()
        .map(|&(i, j)| {
            let (a, b) = (sq[i].star_idx, rq[j].star_idx);
            [a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]]
        })
        .collect();
    let plain = fit_affine(&matches, &sq, &rq);
    let robust = robust_quad_seed(&matches, &sq, &rq);
    let mut out = json!({"subQuads": sq.len(), "refQuads": rq.len(), "rawMatches": raw, "matches": matches.len(),
                         "quadPairs": qpairs, "quadStars": qstars,
                         "seed": plain.as_ref().map(|a| m9(&affine_linear(a))),
                         "robustSeed": robust.as_ref().map(|r| m9(&affine_linear(&r.affine))),
                         "seedRoute": robust.as_ref().map(|r| format!("{:?}", r.route)),
                         "seedHypotheses": robust.as_ref().map(|r| r.hypotheses),
                         "seedFits": robust.as_ref().map(|r| r.fits),
                         "droppedQuads": robust.as_ref().map(|r| r.kept.iter().enumerate().filter(|(_, k)| !**k).map(|(i, _)| i).collect::<Vec<_>>())});
    let chosen = match mode {
        SeedMode::Plain => plain.as_ref().map(affine_linear),
        SeedMode::Robust => robust.as_ref().map(|r| affine_linear(&r.affine)),
    };
    let Some(seed) = chosen else {
        return (
            out,
            Err(AlignError::NoSeed {
                matches: matches.len(),
                after_wcs: false,
            }),
        );
    };
    let tree = KdTree2::build(&rp);
    let radius = 2.0 * cfg.ransac_tolerance_px;
    let (p, s, all) = pair(&|x, y| seed.apply(x, y), sub, refs, &tree, radius);
    if p.len() < MIN_INLIERS {
        out["first"] = json!({"pairs": p.len(), "ransac": null});
        return (out, Err(AlignError::TooFewMatches { matches: p.len() }));
    }
    let first = ransac_refit(&p, &s, all, cfg, geo);
    out["first"] = first.json;
    let (mut pairs, mut ransac, mut refit) = match first.fits {
        Ok((r, f)) => (p, r, f),
        Err(e) => return (out, Err(e)),
    };
    let first_pairs = pairs.len();
    let (p2, s2, all2) = pair(&|x, y| refit.linear.apply(x, y), sub, refs, &tree, radius);
    out["repair"] = json!({"pairs": p2.len(), "taken": false});
    if p2.len() > first_pairs {
        let again = ransac_refit(&p2, &s2, all2, cfg, geo);
        let mut j = again.json;
        j["taken"] = json!(again.fits.is_ok());
        out["repair"] = j;
        if let Ok((r2, f2)) = again.fits {
            pairs = p2;
            ransac = r2;
            refit = f2;
        }
    }
    let inlier_pts: Vec<(f64, f64)> = refit.inliers.iter().map(|&i| pairs[i].0).collect();
    let cov = coverage(&sp, &rp, &inlier_pts, &refit.linear, geo);
    let repaired = pairs.len() - first_pairs;
    (
        out,
        Ok(Final {
            pairs: pairs.len(),
            repaired,
            seed_matches: matches.len(),
            ransac,
            refit,
            coverage: cov,
        }),
    )
}

/// `align`'s record for a restated frame: the same keys the real `align()` record carries, through `align`'s scale
/// gate (`gate`), or the error the stages end on.
fn restated_record(res: &Result<Final, AlignError>, gate: (f64, f64)) -> serde_json::Value {
    let fin = match res {
        Ok(f) => f,
        Err(e) => return json!({"ok": false, "error": e.to_string(), "replay": "restated"}),
    };
    let lin = &fin.refit.linear;
    let scale = lin.scale();
    if !(scale >= gate.0 && scale <= gate.1) {
        let e = AlignError::ScaleOutOfRange {
            scale,
            expected: (gate.0 * gate.1).sqrt(),
        };
        return json!({"ok": false, "error": e.to_string(), "replay": "restated",
                      "coverage": fin.coverage.0, "coverageCells": fin.coverage.1});
    }
    json!({"ok": true, "replay": "restated", "inliers": fin.refit.inliers.len(), "pairs": fin.pairs,
           "seedMatches": fin.seed_matches, "repaired": fin.repaired, "rms": fin.refit.rms_px, "scale": scale,
           "rot": lin.rotation_deg(), "seed": "Quads", "overlap": fin.ransac.quality.overlap,
           "regularity": fin.ransac.quality.regularity, "m": m9(lin), "warnings": Vec::<String>::new(),
           "coverage": fin.coverage.0, "coverageCells": fin.coverage.1})
}

/// `mapgrid`: each line `{"key", "transformJson", "w", "h"}` -> `{"key", "grid": [[x, y, fx, fy], ...]}` on the
/// 33 x 23 grid over (0..w, 0..h), rows first.
fn mapgrid(input: &str, output: &str) {
    let f = std::fs::File::open(input).unwrap_or_else(|e| die(format!("{input}: {e}")));
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(output).unwrap_or_else(|e| die(format!("{output}: {e}"))),
    );
    for (n, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.unwrap_or_else(|e| die(e));
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(&line).unwrap_or_else(|e| die(format!("line {}: {e}", n + 1)));
        let key = v.get("key").cloned().unwrap_or(serde_json::Value::Null);
        let tj = match v.get("transformJson") {
            Some(serde_json::Value::String(t)) => t.clone(),
            Some(o) if !o.is_null() => o.to_string(),
            _ => die(format!("line {}: no transformJson", n + 1)),
        };
        let map = PixelMap::from_json(&tj)
            .unwrap_or_else(|e| die(format!("line {}: transformJson: {e}", n + 1)));
        let dim = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_f64())
                .unwrap_or_else(|| die(format!("line {}: no {k}", n + 1)))
        };
        let (w, h) = (dim("w"), dim("h"));
        let mut grid = Vec::with_capacity(33 * 23);
        for j in 0..23 {
            for i in 0..33 {
                let (x, y) = (w * i as f64 / 32.0, h * j as f64 / 22.0);
                let (fx, fy) = map.forward_exact(x, y);
                grid.push([x, y, fx, fy]);
            }
        }
        writeln!(out, "{}", json!({"key": key, "grid": grid})).unwrap();
    }
    out.flush().unwrap();
}

fn main() {
    // `--seed plain|robust` may stand anywhere; everything else is positional.
    let mut a: Vec<String> = Vec::new();
    let mut seed_mode = SeedMode::Robust;
    let mut argv = std::env::args().skip(1);
    while let Some(x) = argv.next() {
        let value = if x == "--seed" {
            argv.next()
        } else {
            x.strip_prefix("--seed=").map(str::to_string)
        };
        match (x.starts_with("--seed"), value.as_deref()) {
            (false, _) => a.push(x),
            (true, Some("plain")) => seed_mode = SeedMode::Plain,
            (true, Some("robust")) => seed_mode = SeedMode::Robust,
            (true, other) => die(format!("--seed takes plain or robust, not {other:?}")),
        }
    }
    let cfg0 = RegistrationConfig::default();
    match a.first().map(String::as_str) {
        Some("detect") => {
            let r = reference_stars(Path::new(&a[1]), &cfg0, None, None).unwrap_or_else(|e| die(e));
            eprintln!("detect: {} stars, {}x{}", r.stars.len(), r.width, r.height);
            write_stars(&a[2], &r.stars);
        }
        Some("fits") => {
            let fits = fits_artifact::read_fits(Path::new(&a[1])).unwrap_or_else(|e| die(e));
            let s = stars_from_fits(&fits, &cfg0.detection, cfg0.max_stars);
            eprintln!("fits: {} fits -> {} stars", fits.len(), s.len());
            write_stars(&a[2], &s);
        }
        Some("measure") => {
            // measure <stacking config json> <beta> <calibrated mono frame> <list.tsv> <fitsall.tsv>: the run's two
            // Measure passes at the group beta (as d0_weight_probe replays them), then stars_from_fits.
            let text = std::fs::read_to_string(&a[1]).unwrap_or_else(|e| die(e));
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| die(e));
            let doc = v.get("config").cloned().unwrap_or(v);
            let scfg: StackingConfig = serde_json::from_value(doc).unwrap_or_else(|e| die(e));
            let opts = scfg
                .measurement
                .measure_options(scfg.normalization.scale_estimator);
            let beta: f64 = a[2].parse().unwrap();
            let pool = std::sync::Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(4)
                    .build()
                    .unwrap(),
            );
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let path = Path::new(&a[3]);
            let p1 =
                measure_frame_pass1(path, &opts, Some(&pool), &cancel).unwrap_or_else(|e| die(e));
            let (_, fits) = measure_frame_pass2(path, p1, &[beta], &opts, Some(&pool), &cancel)
                .unwrap_or_else(|e| die(e));
            let f0 = &fits[0];
            let s = stars_from_fits(
                f0,
                &scfg.registration.detection,
                scfg.registration.max_stars,
            );
            eprintln!("measure: {} fits -> {} stars", f0.len(), s.len());
            write_stars(&a[4], &s);
            let mut o =
                std::io::BufWriter::new(std::fs::File::create(&a[5]).unwrap_or_else(|e| die(e)));
            writeln!(
                o,
                "x\ty\tsignal\tbackground\tamplitude\tresidual\tfwhm_x\tfwhm_y\tflags"
            )
            .unwrap();
            for f in f0 {
                writeln!(
                    o,
                    "{:.4}\t{:.4}\t{:.6e}\t{:.6e}\t{:.6e}\t{:.6e}\t{:.4}\t{:.4}\t{}",
                    f.x,
                    f.y,
                    f.signal,
                    f.background,
                    f.amplitude,
                    f.residual,
                    f.fwhm_x,
                    f.fwhm_y,
                    f.flags.bits()
                )
                .unwrap();
            }
        }
        Some("fitsall") => {
            let fits = fits_artifact::read_fits(Path::new(&a[1])).unwrap_or_else(|e| die(e));
            let mut o =
                std::io::BufWriter::new(std::fs::File::create(&a[2]).unwrap_or_else(|e| die(e)));
            writeln!(
                o,
                "x\ty\tsignal\tbackground\tamplitude\tresidual\tfwhm_x\tfwhm_y\tflags"
            )
            .unwrap();
            for f in &fits {
                writeln!(
                    o,
                    "{:.4}\t{:.4}\t{:.6e}\t{:.6e}\t{:.6e}\t{:.6e}\t{:.4}\t{:.4}\t{}",
                    f.x,
                    f.y,
                    f.signal,
                    f.background,
                    f.amplitude,
                    f.residual,
                    f.fwhm_x,
                    f.fwhm_y,
                    f.flags.bits()
                )
                .unwrap();
            }
        }
        Some("batch") => {
            let refs = read_stars(&a[1]);
            let geo: (usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
            let man = std::fs::read_to_string(&a[4]).unwrap_or_else(|e| die(e));
            let mut out =
                std::io::BufWriter::new(std::fs::File::create(&a[5]).unwrap_or_else(|e| die(e)));
            for line in man.lines().filter(|l| !l.trim().is_empty()) {
                let t: Vec<&str> = line.split_whitespace().collect();
                let (name, sub_path) = (t[0], t[1]);
                let sgeo: (usize, usize) = (t[2].parse().unwrap(), t[3].parse().unwrap());
                let mut cfg = cfg0.clone();
                cfg.model = ModelChoice::Auto;
                cfg.ransac_tolerance_px = t[4].parse().unwrap();
                cfg.max_stars = t[5].parse().unwrap();
                let hint: Option<Linear> = (t[6] != "-").then(|| {
                    let v: Vec<f64> = t[6].split(',').map(|x| x.parse().unwrap()).collect();
                    Linear::from_flat(
                        LinearKind::Homography,
                        [v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]],
                    )
                });
                let mut sub = read_stars(sub_path);
                sub.truncate(cfg.max_stars);
                let mut r = refs.clone();
                r.truncate(cfg.max_stars);
                let gate = match t.get(7).copied().filter(|g| *g != "-") {
                    Some(r) => scale_gate_for(
                        Some(
                            r.parse()
                                .unwrap_or_else(|_| die(format!("{name}: gate {r}"))),
                        ),
                        Some(1.0),
                    ),
                    None => SCALE_RANGE,
                };
                let policy = if hint.is_some() {
                    SeedPolicy::WcsFirst
                } else {
                    SeedPolicy::QuadFirst
                };
                // The restated stages: the stage columns, and the coverage of their final refit. A hint leads through
                // the real `align()` only.
                let (st, restated) = if hint.is_none() {
                    let (j, res) = stages(&sub, &r, &cfg, geo, seed_mode);
                    (j, Some(res))
                } else {
                    (serde_json::Value::Null, None)
                };
                let (al, summary) = match (&restated, seed_mode) {
                    (Some(res), SeedMode::Plain) => {
                        let summary = match res {
                            Ok(f) => format!("{} inliers", f.refit.inliers.len()),
                            Err(e) => e.to_string(),
                        };
                        (restated_record(res, gate), summary)
                    }
                    _ => {
                        let res = align(&sub, &r, geo, sgeo, &cfg, hint.as_ref(), policy, gate);
                        let mut al = match &res {
                            Ok(x) => {
                                json!({"ok": true, "replay": "align", "inliers": x.inliers, "pairs": x.pairs, "seedMatches": x.seed_matches,
                                            "repaired": x.repaired, "rms": x.rms_px, "scale": x.scale, "rot": x.rotation_deg,
                                            "seed": format!("{:?}", x.seed), "overlap": x.overlap, "regularity": x.regularity,
                                            "m": m9(&x.map.linear), "warnings": x.warnings})
                            }
                            Err(e) => {
                                json!({"ok": false, "replay": "align", "error": e.to_string()})
                            }
                        };
                        let cov = restated
                            .as_ref()
                            .and_then(|r| r.as_ref().ok())
                            .map(|f| f.coverage);
                        al["coverage"] = json!(cov.map(|c| c.0));
                        al["coverageCells"] = json!(cov.map(|c| c.1));
                        let summary = match &res {
                            Ok(x) => format!("{} inliers", x.inliers),
                            Err(e) => e.to_string(),
                        };
                        (al, summary)
                    }
                };
                writeln!(out, "{}", json!({"name": name, "tol": cfg.ransac_tolerance_px, "maxStars": cfg.max_stars, "nSub": sub.len(), "nRef": r.len(),
                                           "seedMode": format!("{seed_mode:?}").to_lowercase(), "gate": [gate.0, gate.1],
                                           "align": al, "stages": st})).unwrap();
                out.flush().unwrap();
                eprintln!("{name}: {summary}");
            }
        }
        Some("mapgrid") => mapgrid(&a[1], &a[2]),
        _ => die("usage: detect|fits|fitsall|measure|batch [--seed plain|robust]|mapgrid …"),
    }
}
