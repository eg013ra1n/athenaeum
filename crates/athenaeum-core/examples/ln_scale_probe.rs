//! Research probe for the LN relative-scale PSF-width audit
//! (`docs/superpowers/research/2026-10-04-ln-scale-psf-width-audit.md`).
//! Never part of a production path.
//!
//! For one plane of an LN reference and a list of members ALREADY in the
//! reference geometry (registered artifacts, or calibrated frames warped
//! here through their registration map), it:
//!
//! 1. runs the production detection-path scale
//!    (`ln::relative_scale_against[_masked]`, the reference prepared once,
//!    the target fitted at the reference's β) — `s_lib`;
//! 2. replicates that computation step by step with the same public calls
//!    (`detect_stars` → seeds, `fit_stars`/`fit_stars_with_beta` with
//!    `FitParams::default()`, nearest-within-4-px pairing, RCR 0.3) so every
//!    matched star's two fits are visible — `s_rep` must equal `s_lib`
//!    whenever pass 1 is the pairing production kept;
//! 3. measures every matched star again on the SAME pixels with fit-free
//!    photometry: the production FWTM ellipse (growth 1, 2, 3) with the fit's
//!    background and with a local annulus sky, circular apertures of a
//!    COMMON radius `c·F` in both frames (`F` = the larger of the two
//!    frames' median fitted FWHM), the analytic Moffat total, and writes
//!    one CSV row per star (`--stars-dir`) for offline analysis.
//!
//! One JSON line per member goes to stdout. `--tgt-beta` adds an arm that
//! fits the target at another β (or its own `Auto` β); `--dump-warped DIR`
//! writes each member plane as FITS for the acceptance harness's truth
//! photometry.
//!
//! Two modes need no reference/members:
//! - `--estimators <stars.csv>...` reads per-star CSVs written by
//!   `--stars-dir` and prints the RCR location (limit 0.3, the production
//!   call) of every candidate flux estimator, over all pairs and over the
//!   bright quarter;
//! - `--stack-sim <config.json>` combines the whole group's LN-normalized
//!   pixel stacks in windows around stars with the production per-pixel
//!   combiner, once as the run did (checked against the real master's
//!   pixels) and once with each member's scale corrected (see `stack_sim`).
//!
//! cargo run --release -p athenaeum-core --example ln_scale_probe -- \
//!   --ref <reference.fits> --plane P --beta <b|auto> [--masked] \
//!   [--max-stars 24576] [--stars-dir DIR] [--tgt-beta <b|auto>] \
//!   [--interpolation bicubicBSpline] [--clamping 0.3] [--dump-warped DIR] \
//!   (--member LABEL=<registered.fits> | --warp LABEL=<calibrated.fits>@<map.json>)...
//! cargo run --release -p athenaeum-core --example ln_scale_probe -- --estimators <stars.csv>...
//! cargo run --release -p athenaeum-core --example ln_scale_probe -- --stack-sim <config.json>

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use athenaeum_core::geometry::kdtree::KdTree2;
use athenaeum_core::geometry::PixelMap;
use athenaeum_core::integration::banded::BandPlanes;
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::integration::registered_source::{RegisteredFrame, RegisteredSource};
use athenaeum_core::integration::source::FrameSource;
use athenaeum_core::integration::stats::median_in_place;
use athenaeum_core::resample::Interpolation;
use athenaeum_core::stacking::ln::scale::{
    relative_scale_against, relative_scale_against_masked, PreparedReferenceChannel,
};
use athenaeum_core::stacking::ln::{eroded_mask, LN_MASK_MARGIN_PX};
use athenaeum_core::stacking::psf_signal::{
    fit_stars, fit_stars_with_beta, FitOutcome, FitParams, PsfModel, Seed, StarFit,
};
use athenaeum_core::stacking::register::detect::{detect_stars, Star};
use athenaeum_core::stacking::register::DetectionConfig;
use athenaeum_core::stacking::robust::rcr;

/// Production constants of `ln::normalize_frame`'s scale call.
const MATCH_RADIUS_PX: f64 = 4.0;
const RCR_LIMIT: f64 = 0.3;
/// Common circular apertures, in units of `F` (the larger frame FWHM).
const CIRCLE_RADII: [f64; 6] = [1.0, 1.5, 2.0, 3.0, 4.0, 5.0];
/// Ellipse growth factors (production uses 1.0).
const GROWTHS: [f64; 3] = [1.0, 2.0, 3.0];
/// Local sky annulus, in units of `F`.
const SKY_IN: f64 = 6.0;
const SKY_OUT: f64 = 8.0;

fn usage() -> ! {
    eprintln!(
        "usage: ln_scale_probe --ref <reference.fits> --plane P --beta <b|auto> [--masked] \
[--max-stars N] [--stars-dir DIR] [--tgt-beta <b|auto>] [--interpolation name] [--clamping c] \
(--member LABEL=<registered.fits> | --warp LABEL=<calibrated.fits>@<map.json>)..."
    );
    std::process::exit(2);
}

enum Source {
    Registered(PathBuf),
    Warp(PathBuf, PathBuf),
}

struct Args {
    reference: PathBuf,
    plane: usize,
    beta: PsfModel,
    tgt_beta: Option<PsfModel>,
    masked: bool,
    max_stars: usize,
    stars_dir: Option<PathBuf>,
    interpolation: Interpolation,
    clamping: f32,
    members: Vec<(String, Source)>,
    /// `--estimators <stars.csv>...`: no image work — read per-star CSVs this
    /// probe wrote and print the RCR location of every estimator.
    estimators: Vec<PathBuf>,
    /// `--dump-warped DIR`: write each warped member plane (NaN kept) as
    /// `<DIR>/<label>.p<plane>.fits` so the acceptance truth photometry
    /// (`grid-report.py`'s `photometry`) can read it.
    dump_warped: Option<PathBuf>,
    /// `--stack-sim <config.json>`: the master-window simulation (see
    /// [`stack_sim`]).
    stack_sim: Option<PathBuf>,
}

fn parse_model(s: &str) -> PsfModel {
    if s == "auto" {
        PsfModel::Auto
    } else {
        PsfModel::Fixed(s.parse().unwrap_or_else(|_| usage()))
    }
}

fn parse_args() -> Args {
    let mut a = Args {
        reference: PathBuf::new(),
        plane: 0,
        beta: PsfModel::Auto,
        tgt_beta: None,
        masked: false,
        max_stars: 24576,
        stars_dir: None,
        interpolation: Interpolation::BicubicBSpline,
        clamping: 0.3,
        members: Vec::new(),
        estimators: Vec::new(),
        dump_warped: None,
        stack_sim: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--ref" => a.reference = PathBuf::from(val()),
            "--plane" => a.plane = val().parse().unwrap_or_else(|_| usage()),
            "--beta" => a.beta = parse_model(&val()),
            "--tgt-beta" => a.tgt_beta = Some(parse_model(&val())),
            "--masked" => a.masked = true,
            "--max-stars" => a.max_stars = val().parse().unwrap_or_else(|_| usage()),
            "--stars-dir" => a.stars_dir = Some(PathBuf::from(val())),
            "--clamping" => a.clamping = val().parse().unwrap_or_else(|_| usage()),
            "--interpolation" => {
                let v = val();
                a.interpolation = serde_json::from_str(&format!("\"{v}\"")).unwrap_or_else(|e| {
                    eprintln!("interpolation {v:?}: {e}");
                    usage()
                })
            }
            "--member" => {
                let v = val();
                let (label, path) = v.split_once('=').unwrap_or_else(|| usage());
                a.members
                    .push((label.to_string(), Source::Registered(PathBuf::from(path))));
            }
            "--warp" => {
                let v = val();
                let (label, rest) = v.split_once('=').unwrap_or_else(|| usage());
                let (path, map) = rest.split_once('@').unwrap_or_else(|| usage());
                a.members.push((
                    label.to_string(),
                    Source::Warp(PathBuf::from(path), PathBuf::from(map)),
                ));
            }
            "--estimators" => a.estimators.push(PathBuf::from(val())),
            "--dump-warped" => a.dump_warped = Some(PathBuf::from(val())),
            "--stack-sim" => a.stack_sim = Some(PathBuf::from(val())),
            _ => usage(),
        }
    }
    if !a.estimators.is_empty() || a.stack_sim.is_some() {
        return a;
    }
    if a.reference.as_os_str().is_empty() || a.members.is_empty() {
        usage();
    }
    a
}

fn die(msg: String) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

fn median_of_finite(plane: &[f32]) -> f64 {
    let mut v: Vec<f32> = plane.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return f64::NAN;
    }
    median_in_place(&mut v) as f64
}

fn median64(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// `scale::to_seed`, verbatim.
fn to_seed(star: &Star) -> Seed {
    let peak = match star.sigma {
        Some((sx, sy)) if sx > 0.0 && sy > 0.0 => {
            star.flux / (2.0 * std::f64::consts::PI * sx * sy)
        }
        _ => 0.0,
    };
    Seed {
        x: star.x,
        y: star.y,
        peak,
        flux: star.flux,
    }
}

/// `scale::allowed_at`, verbatim.
fn allowed_at(allowed: &[bool], width: usize, x: f64, y: f64) -> bool {
    let (cx, cy) = (x.round(), y.round());
    if !(cx >= 0.0 && cy >= 0.0) || cx as usize >= width {
        return false;
    }
    allowed
        .get(cy as usize * width + cx as usize)
        .copied()
        .unwrap_or(false)
}

/// `scale::detect_seeds` + the masked retain.
fn seeds_of(
    plane: &[f32],
    w: usize,
    h: usize,
    max_stars: usize,
    allowed: Option<&[bool]>,
    pool: &Arc<rayon::ThreadPool>,
) -> Vec<Seed> {
    let stars = detect_stars(
        plane,
        w,
        h,
        &DetectionConfig::default(),
        max_stars,
        Some(pool),
    );
    let mut seeds: Vec<Seed> = stars.iter().map(to_seed).collect();
    if let Some(a) = allowed {
        seeds.retain(|s| allowed_at(a, w, s.x, s.y));
    }
    seeds
}

/// Sum of pixel values and pixel count inside the production ellipse
/// (`psf_signal::aperture`'s pixel rule) at growth `k`. `None` when a
/// non-finite pixel lies inside (the aperture touches uncovered sky).
fn ellipse_sum(data: &[f32], w: usize, h: usize, f: &StarFit, k: f64) -> Option<(f64, usize)> {
    let (a, b) = (0.5 * k * f.fwtm_x, 0.5 * k * f.fwtm_y);
    let (st, ct) = f.theta.sin_cos();
    let hx = ((a * ct).powi(2) + (b * st).powi(2)).sqrt();
    let hy = ((a * st).powi(2) + (b * ct).powi(2)).sqrt();
    if f.x - hx < 0.0 || f.y - hy < 0.0 || f.x + hx > (w - 1) as f64 || f.y + hy > (h - 1) as f64 {
        return None;
    }
    let x0 = (f.x - hx).floor().max(0.0) as usize;
    let x1 = ((f.x + hx).ceil().max(0.0) as usize).min(w - 1);
    let y0 = (f.y - hy).floor().max(0.0) as usize;
    let y1 = ((f.y + hy).ceil().max(0.0) as usize).min(h - 1);
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f64 - f.x, y as f64 - f.y);
            let u = dx * ct + dy * st;
            let v = -dx * st + dy * ct;
            if (u / a).powi(2) + (v / b).powi(2) <= 1.0 {
                let pv = data[y * w + x];
                if !pv.is_finite() {
                    return None;
                }
                sum += pv as f64;
                n += 1;
            }
        }
    }
    Some((sum, n))
}

/// Circle sum (pixel centres within `r` of `(cx, cy)`) and count; `None`
/// on a non-finite pixel or the image border.
fn circle_sum(data: &[f32], w: usize, h: usize, cx: f64, cy: f64, r: f64) -> Option<(f64, usize)> {
    if cx - r < 0.0 || cy - r < 0.0 || cx + r > (w - 1) as f64 || cy + r > (h - 1) as f64 {
        return None;
    }
    let (x0, x1) = ((cx - r).floor() as usize, (cx + r).ceil() as usize);
    let (y0, y1) = ((cy - r).floor() as usize, (cy + r).ceil() as usize);
    let r2 = r * r;
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in y0..=y1 {
        let dy = y as f64 - cy;
        for x in x0..=x1 {
            let dx = x as f64 - cx;
            if dx * dx + dy * dy <= r2 {
                let pv = data[y * w + x];
                if !pv.is_finite() {
                    return None;
                }
                sum += pv as f64;
                n += 1;
            }
        }
    }
    Some((sum, n))
}

/// Median of the annulus `[r_in, r_out]` around `(cx, cy)`, finite pixels
/// only; `None` at the border or with fewer than half the pixels finite.
fn annulus_median(
    data: &[f32],
    w: usize,
    h: usize,
    cx: f64,
    cy: f64,
    r_in: f64,
    r_out: f64,
    scratch: &mut Vec<f32>,
) -> Option<f64> {
    if cx - r_out < 0.0
        || cy - r_out < 0.0
        || cx + r_out > (w - 1) as f64
        || cy + r_out > (h - 1) as f64
    {
        return None;
    }
    scratch.clear();
    let (x0, x1) = ((cx - r_out).floor() as usize, (cx + r_out).ceil() as usize);
    let (y0, y1) = ((cy - r_out).floor() as usize, (cy + r_out).ceil() as usize);
    let (ri2, ro2) = (r_in * r_in, r_out * r_out);
    let mut total = 0usize;
    for y in y0..=y1 {
        let dy = y as f64 - cy;
        for x in x0..=x1 {
            let dx = x as f64 - cx;
            let d2 = dx * dx + dy * dy;
            if d2 >= ri2 && d2 <= ro2 {
                total += 1;
                let pv = data[y * w + x];
                if pv.is_finite() {
                    scratch.push(pv);
                }
            }
        }
    }
    if scratch.len() * 2 < total || scratch.is_empty() {
        return None;
    }
    Some(median_in_place(scratch) as f64)
}

/// Moffat analytic total flux `A·π·αx·αy/(β−1)`, `α = FWHM/(2√(2^{1/β}−1))`.
fn moffat_total(f: &StarFit) -> f64 {
    let c = 2.0 * (2f64.powf(1.0 / f.beta) - 1.0).sqrt();
    let (ax, ay) = (f.fwhm_x / c, f.fwhm_y / c);
    f.amplitude * std::f64::consts::PI * ax * ay / (f.beta - 1.0)
}

fn read_plane(path: &PathBuf, plane: usize) -> (Vec<f32>, usize, usize) {
    let r = PlaneReader::open(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
    let (w, h) = (r.width(), r.height());
    let p = r
        .read_plane(plane)
        .unwrap_or_else(|e| die(format!("{} plane {plane}: {e}", path.display())));
    (p, w, h)
}

/// `ln_probe::warp_channel` — the on-the-fly warp `normalize_frame` falls
/// back to when there is no materialized registered artifact.
fn warp_plane(
    path: &PathBuf,
    map_path: &PathBuf,
    w: usize,
    h: usize,
    plane: usize,
    interpolation: Interpolation,
    clamping: f32,
) -> Vec<f32> {
    let json = std::fs::read_to_string(map_path)
        .unwrap_or_else(|e| die(format!("{}: {e}", map_path.display())));
    let map = PixelMap::from_json(json.trim())
        .unwrap_or_else(|e| die(format!("{}: {e}", map_path.display())));
    let src = RegisteredSource::open(
        &[RegisteredFrame {
            path: path.clone(),
            map,
        }],
        w,
        h,
        plane,
        interpolation,
        clamping,
    )
    .unwrap_or_else(|e| die(format!("warp {}: {e}", path.display())));
    let mut band = BandPlanes::new(&src);
    src.read_band_with_progress(0, h, &mut band, 1, &|_: u64| {}, &AtomicBool::new(false))
        .unwrap_or_else(|e| die(format!("warp {}: {e}", path.display())));
    let mut out = vec![0f32; w * h];
    band.decode_frame_into(0, &mut out);
    out
}

fn fwhm_median(fits: &[StarFit]) -> f64 {
    let mut v: Vec<f64> = fits.iter().map(|f| f.fwhm()).collect();
    median64(&mut v)
}

fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let args = parse_args();
    if !args.estimators.is_empty() {
        for p in &args.estimators {
            estimator_battery(p);
        }
        return;
    }
    if let Some(cfg) = &args.stack_sim {
        stack_sim(cfg);
        return;
    }
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap_or_else(|e| die(format!("thread pool: {e}"))),
    );
    let fit_params = FitParams::default();

    // ---- the reference side, prepared once (ln::LnReferenceForDetection) ----
    let t = Instant::now();
    let (ref_raw, w, h) = read_plane(&args.reference, args.plane);
    let ref_finite: Vec<bool> = ref_raw.iter().map(|v| v.is_finite()).collect();
    let ref_fill = if args.masked {
        0.0f32
    } else {
        median_of_finite(&ref_raw) as f32
    };
    let ref_sane: Vec<f32> = ref_raw
        .iter()
        .map(|&v| if v.is_finite() { v } else { ref_fill })
        .collect();
    let ref_allowed = args
        .masked
        .then(|| eroded_mask(&ref_finite, w, h, LN_MASK_MARGIN_PX));
    let prepared = match ref_allowed.as_deref() {
        Some(a) => PreparedReferenceChannel::build_masked(
            &ref_sane,
            w,
            h,
            args.beta,
            args.max_stars,
            Some(&pool),
            a,
        ),
        None => {
            PreparedReferenceChannel::build(&ref_sane, w, h, args.beta, args.max_stars, Some(&pool))
        }
    };
    let beta = prepared.default_beta();
    // The replication's own reference fits — the same calls `build_with` makes.
    let ref_seeds = seeds_of(
        &ref_sane,
        w,
        h,
        args.max_stars,
        ref_allowed.as_deref(),
        &pool,
    );
    let ref_out: FitOutcome = fit_stars(
        &ref_sane,
        w,
        h,
        &ref_seeds,
        args.beta,
        &fit_params,
        Some(&pool),
    );
    if ref_out.beta != beta {
        die(format!(
            "replicated reference beta {} != prepared {beta}",
            ref_out.beta
        ));
    }
    let ref_tree = KdTree2::build(&ref_out.fits.iter().map(|f| (f.x, f.y)).collect::<Vec<_>>());
    let ref_fwhm = fwhm_median(&ref_out.fits);
    eprintln!(
        "reference {}x{} plane {} beta {beta} fits {} median fit FWHM {ref_fwhm:.3} ({} ms)",
        w,
        h,
        args.plane,
        ref_out.fits.len(),
        t.elapsed().as_millis()
    );

    if let Some(d) = &args.stars_dir {
        std::fs::create_dir_all(d).unwrap_or_else(|e| die(format!("{}: {e}", d.display())));
        // Every accepted reference fit (the isolation test of the offline
        // analysis needs the neighbours a pair list does not carry).
        let p = d.join(format!("reference.p{}.csv", args.plane));
        let mut f = std::io::BufWriter::new(
            std::fs::File::create(&p).unwrap_or_else(|e| die(format!("{}: {e}", p.display()))),
        );
        writeln!(f, "x,y,fwhm,amp,b,sig").ok();
        for r in &ref_out.fits {
            writeln!(
                f,
                "{:.3},{:.3},{:.4},{:.6e},{:.9e},{:.6e}",
                r.x,
                r.y,
                r.fwhm(),
                r.amplitude,
                r.background,
                r.signal
            )
            .ok();
        }
    }

    for (label, source) in &args.members {
        let t = Instant::now();
        let mut tgt_raw = match source {
            Source::Registered(p) => {
                let (pl, tw, th) = read_plane(p, args.plane);
                if (tw, th) != (w, h) {
                    die(format!("{label}: {tw}x{th} is not the reference's {w}x{h}"));
                }
                pl
            }
            Source::Warp(p, m) => {
                warp_plane(p, m, w, h, args.plane, args.interpolation, args.clamping)
            }
        };
        if let Some(d) = &args.dump_warped {
            std::fs::create_dir_all(d).unwrap_or_else(|e| die(format!("{}: {e}", d.display())));
            let out = d.join(format!("{label}.p{}.fits", args.plane));
            athenaeum_core::fits_writer::write_fits_f32(&out, w, h, 1, &tgt_raw, &[])
                .unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
        }

        // ---- normalize_frame's own preparation of the target plane ----
        let mut allowed: Option<Vec<bool>> = None;
        if args.masked {
            let mut overlap = vec![false; tgt_raw.len()];
            for (k, o) in overlap.iter_mut().enumerate() {
                *o = tgt_raw[k].is_finite() && ref_raw[k].is_finite();
            }
            for (v, &o) in tgt_raw.iter_mut().zip(&overlap) {
                if !o {
                    *v = f32::NAN;
                }
            }
            allowed = Some(eroded_mask(&overlap, w, h, LN_MASK_MARGIN_PX));
        }
        let fill = if args.masked {
            0.0f32
        } else {
            median_of_finite(&tgt_raw) as f32
        };
        let tgt_sane: Vec<f32> = tgt_raw
            .iter()
            .map(|&v| if v.is_finite() { v } else { fill })
            .collect();

        // ---- 1. the production call ----
        let lib = match allowed.as_deref() {
            Some(a) => relative_scale_against_masked(
                &prepared,
                &tgt_sane,
                w,
                h,
                args.max_stars,
                MATCH_RADIUS_PX,
                RCR_LIMIT,
                false,
                Some(&pool),
                a,
            ),
            None => relative_scale_against(
                &prepared,
                &tgt_sane,
                w,
                h,
                args.max_stars,
                MATCH_RADIUS_PX,
                RCR_LIMIT,
                false,
                Some(&pool),
            ),
        };
        let (s_lib, lib_pass, lib_matches) = match &lib {
            Ok(r) => (r.scale, r.pass, r.matches),
            Err(e) => {
                eprintln!("{label}: production scale failed: {e}");
                (f64::NAN, 0, 0)
            }
        };

        // ---- 2. the replication ----
        let tgt_seeds = seeds_of(&tgt_sane, w, h, args.max_stars, allowed.as_deref(), &pool);
        let tgt_out =
            fit_stars_with_beta(&tgt_sane, w, h, &tgt_seeds, beta, &fit_params, Some(&pool));
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for (ti, f) in tgt_out.fits.iter().enumerate() {
            if let Some((ri, _)) = ref_tree.nearest_within(f.x, f.y, MATCH_RADIUS_PX) {
                pairs.push((ri, ti));
            }
        }
        let pass1_ok = pairs.len() as f64 >= 0.8 * tgt_out.fits.len() as f64;
        let mut ratios = Vec::new();
        let mut kept_pairs = Vec::new();
        for &(ri, ti) in &pairs {
            let (fr, ft) = (ref_out.fits[ri].signal, tgt_out.fits[ti].signal);
            if fr > 0.0 && ft > 0.0 {
                ratios.push(fr / ft);
                kept_pairs.push((ri, ti));
            }
        }
        let rep = rcr(&ratios, RCR_LIMIT);
        let tgt_fwhm = fwhm_median(&tgt_out.fits);

        // Optional: the target fitted at another β (hypothesis H3), on the
        // same seeds; matched to the same reference fits.
        let alt = args.tgt_beta.map(|m| {
            let o = fit_stars(&tgt_sane, w, h, &tgt_seeds, m, &fit_params, Some(&pool));
            let mut z = Vec::new();
            for f in &o.fits {
                if let Some((ri, _)) = ref_tree.nearest_within(f.x, f.y, MATCH_RADIUS_PX) {
                    let fr = ref_out.fits[ri].signal;
                    if fr > 0.0 && f.signal > 0.0 {
                        z.push(fr / f.signal);
                    }
                }
            }
            let r = rcr(&z, RCR_LIMIT);
            (o.beta, r.location, z.len(), fwhm_median(&o.fits))
        });

        // ---- 3. fit-free photometry on the same stars ----
        let big_f = ref_fwhm.max(tgt_fwhm);
        let mut scratch = Vec::new();
        let mut csv: Option<std::io::BufWriter<std::fs::File>> = args.stars_dir.as_ref().map(|d| {
            let p = d.join(format!("{label}.p{}.csv", args.plane));
            std::io::BufWriter::new(
                std::fs::File::create(&p).unwrap_or_else(|e| die(format!("{}: {e}", p.display()))),
            )
        });
        if let Some(c) = csv.as_mut() {
            let mut head = String::from(
                "rx,ry,tx,ty,kept,r_fwhm,t_fwhm,r_ecc,t_ecc,r_amp,t_amp,r_b,t_b,r_sig,t_sig,r_res,t_res,r_sky,t_sky,r_mtot,t_mtot",
            );
            for g in GROWTHS {
                head.push_str(&format!(",r_e{g}_s,r_e{g}_n,t_e{g}_s,t_e{g}_n"));
            }
            for c_ in CIRCLE_RADII {
                head.push_str(&format!(",r_c{c_}_s,r_c{c_}_n,t_c{c_}_s,t_c{c_}_n"));
            }
            writeln!(c, "{head}").ok();
        }
        // The production signal check: Σp − N·b over the ellipse must
        // reproduce `signal` to rounding.
        let mut sig_check_max = 0.0f64;
        let (mut z_ellsky, mut z_c3, mut z_c5) = (Vec::new(), Vec::new(), Vec::new());
        for (k, &(ri, ti)) in kept_pairs.iter().enumerate() {
            let (fr, ft) = (&ref_out.fits[ri], &tgt_out.fits[ti]);
            let sky_r = annulus_median(
                &ref_raw,
                w,
                h,
                fr.x,
                fr.y,
                SKY_IN * big_f,
                SKY_OUT * big_f,
                &mut scratch,
            );
            let sky_t = annulus_median(
                &tgt_raw,
                w,
                h,
                ft.x,
                ft.y,
                SKY_IN * big_f,
                SKY_OUT * big_f,
                &mut scratch,
            );
            let ells: Vec<(Option<(f64, usize)>, Option<(f64, usize)>)> = GROWTHS
                .iter()
                .map(|&g| {
                    (
                        ellipse_sum(&ref_raw, w, h, fr, g),
                        ellipse_sum(&tgt_raw, w, h, ft, g),
                    )
                })
                .collect();
            let circ: Vec<(Option<(f64, usize)>, Option<(f64, usize)>)> = CIRCLE_RADII
                .iter()
                .map(|&c| {
                    (
                        circle_sum(&ref_raw, w, h, fr.x, fr.y, c * big_f),
                        circle_sum(&tgt_raw, w, h, ft.x, ft.y, c * big_f),
                    )
                })
                .collect();
            if let (Some((s, n)), _) = ells[0] {
                let d =
                    (s - n as f64 * fr.background - fr.signal).abs() / fr.signal.abs().max(1e-12);
                sig_check_max = sig_check_max.max(d);
            }
            if let (Some(sr), Some(st)) = (sky_r, sky_t) {
                if let (
                    (Some((sr1, nr1)), Some((st1, nt1))),
                    (Some((sr3, nr3)), Some((st3, nt3))),
                    (Some((sr5, nr5)), Some((st5, nt5))),
                ) = (ells[0], circ[3], circ[5])
                {
                    let (er, et) = (sr1 - nr1 as f64 * sr, st1 - nt1 as f64 * st);
                    if er > 0.0 && et > 0.0 {
                        z_ellsky.push(er / et);
                    }
                    let (cr, ct) = (sr3 - nr3 as f64 * sr, st3 - nt3 as f64 * st);
                    if cr > 0.0 && ct > 0.0 {
                        z_c3.push(cr / ct);
                    }
                    let (cr, ct) = (sr5 - nr5 as f64 * sr, st5 - nt5 as f64 * st);
                    if cr > 0.0 && ct > 0.0 {
                        z_c5.push(cr / ct);
                    }
                }
            }
            if let Some(c) = csv.as_mut() {
                let o = |v: Option<f64>| v.map_or(String::from("nan"), |x| format!("{x:.9e}"));
                let mut row = format!(
                    "{:.3},{:.3},{:.3},{:.3},{},{:.4},{:.4},{:.4},{:.4},{:.6e},{:.6e},{:.9e},{:.9e},{:.6e},{:.6e},{:.5},{:.5},{},{},{:.6e},{:.6e}",
                    fr.x,
                    fr.y,
                    ft.x,
                    ft.y,
                    rep.kept.get(k).copied().unwrap_or(false) as u8,
                    fr.fwhm(),
                    ft.fwhm(),
                    fr.eccentricity(),
                    ft.eccentricity(),
                    fr.amplitude,
                    ft.amplitude,
                    fr.background,
                    ft.background,
                    fr.signal,
                    ft.signal,
                    fr.residual,
                    ft.residual,
                    o(sky_r),
                    o(sky_t),
                    moffat_total(fr),
                    moffat_total(ft),
                );
                for e in ells.iter().chain(circ.iter()) {
                    let f = |v: Option<(f64, usize)>| match v {
                        Some((s, n)) => format!("{s:.9e},{n}"),
                        None => String::from("nan,0"),
                    };
                    row.push_str(&format!(",{},{}", f(e.0), f(e.1)));
                }
                writeln!(c, "{row}").ok();
            }
        }
        let loc = |z: &[f64]| {
            if z.len() < 3 {
                f64::NAN
            } else {
                rcr(z, RCR_LIMIT).location
            }
        };
        let line = serde_json::json!({
            "label": label,
            "plane": args.plane,
            "beta": beta,
            "refFwhmFit": ref_fwhm,
            "tgtFwhmFit": tgt_fwhm,
            "F": big_f,
            "sLib": s_lib,
            "libPass": lib_pass,
            "libMatches": lib_matches,
            "sRep": rep.location,
            "repSigma": rep.scale,
            "repMatches": ratios.len(),
            "repRejected": rep.rejected,
            "pass1Covers80": pass1_ok,
            "signalCheckMaxRel": sig_check_max,
            "sEllipseAnnulusSky": loc(&z_ellsky),
            "sCircle3F": loc(&z_c3),
            "sCircle5F": loc(&z_c5),
            "nPhot": z_c5.len(),
            "alt": alt.map(|(b, s, n, fw)| serde_json::json!({"beta": b, "s": s, "n": n, "fwhm": fw})),
            "ms": t.elapsed().as_millis() as u64,
        });
        println!("{line}");
        std::io::stdout().flush().ok();
    }
}

/// The estimator battery over one per-star CSV (`--estimators`): every
/// ratio estimator's RCR location (limit 0.3, the production call) over ALL
/// pairs and over the BRIGHT quarter (reference flux in the 3F common
/// circle at or above its 75th percentile — the deep reference's own flux,
/// so the cut does not select on the member's noise).
fn estimator_battery(path: &PathBuf) {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();
    let col = |name: &str| {
        head.iter()
            .position(|h| *h == name)
            .unwrap_or_else(|| die(format!("{}: no column {name}", path.display())))
    };
    let rows: Vec<Vec<f64>> = lines
        .map(|l| {
            l.split(',')
                .map(|v| v.parse::<f64>().unwrap_or(f64::NAN))
                .collect()
        })
        .collect();
    let get = |r: &Vec<f64>, name: &str| r[col(name)];
    let flux = |r: &Vec<f64>, side: &str, key: &str, bg: f64| {
        get(r, &format!("{side}_{key}_s")) - get(r, &format!("{side}_{key}_n")) * bg
    };
    let names: [&str; 12] = [
        "prod", "e1sky", "e2sky", "e3sky", "e2fit", "e3fit", "c1.5", "c2", "c3", "c4", "c5", "mtot",
    ];
    let mut bright_key = Vec::new();
    for r in &rows {
        bright_key.push(flux(r, "r", "c3", get(r, "r_sky")));
    }
    let mut finite_bk: Vec<f64> = bright_key
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    finite_bk.sort_by(|a, b| a.total_cmp(b));
    let q75 = if finite_bk.is_empty() {
        f64::INFINITY
    } else {
        finite_bk[(0.75 * (finite_bk.len() - 1) as f64) as usize]
    };
    let mut out = serde_json::Map::new();
    for name in names {
        let (mut all, mut bright) = (Vec::new(), Vec::new());
        for (i, r) in rows.iter().enumerate() {
            let (rs, ts) = (get(r, "r_sky"), get(r, "t_sky"));
            let (rb, tb) = (get(r, "r_b"), get(r, "t_b"));
            let (a, b) = match name {
                "prod" => (get(r, "r_sig"), get(r, "t_sig")),
                "e1sky" => (flux(r, "r", "e1", rs), flux(r, "t", "e1", ts)),
                "e2sky" => (flux(r, "r", "e2", rs), flux(r, "t", "e2", ts)),
                "e3sky" => (flux(r, "r", "e3", rs), flux(r, "t", "e3", ts)),
                "e2fit" => (flux(r, "r", "e2", rb), flux(r, "t", "e2", tb)),
                "e3fit" => (flux(r, "r", "e3", rb), flux(r, "t", "e3", tb)),
                "mtot" => (get(r, "r_mtot"), get(r, "t_mtot")),
                c => (flux(r, "r", c, rs), flux(r, "t", c, ts)),
            };
            if a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0 {
                all.push(a / b);
                if bright_key[i] >= q75 {
                    bright.push(a / b);
                }
            }
        }
        let loc = |z: &[f64]| {
            if z.len() < 3 {
                f64::NAN
            } else {
                rcr(z, RCR_LIMIT).location
            }
        };
        out.insert(
            name.to_string(),
            serde_json::json!({"all": loc(&all), "nAll": all.len(), "bright": loc(&bright), "nBright": bright.len()}),
        );
    }
    let label = path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    println!("{}", serde_json::json!({"file": label, "est": out}));
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimMember {
    label: String,
    /// Calibrated frame + registration map (warped here), or a registered artifact.
    calibrated: Option<PathBuf>,
    map: Option<PathBuf>,
    registered: Option<PathBuf>,
    athln: PathBuf,
    weight: f32,
    /// `s_true / s_applied` for this member — the corrected variant's factor.
    correction: f64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimConfig {
    reference: PathBuf,
    master: PathBuf,
    plane: usize,
    stars: Vec<(f64, f64)>,
    half: usize,
    sigma_low: f64,
    sigma_high: f64,
    /// FWHM (px) of the LN reference: core = r ≤ fRef, wing = fRef < r ≤ 3·fRef.
    f_ref: f64,
    /// Photometry on every window: aperture 3·fPhot, sky annulus [5, 7]·fPhot.
    f_phot: f64,
    interpolation: Interpolation,
    clamping: f32,
    members: Vec<SimMember>,
    out_stars: PathBuf,
    out_frames: PathBuf,
}

/// Master-window simulation (research only): for every star window, the
/// whole group's LN-normalized pixel stacks (`A·v + B` from each member's own
/// `.athln`, exactly the engine's per-pixel reconstruction) are combined with
/// the production per-pixel combiner (`combine_pixel_weighted`, average +
/// linear-fit clip, the run's weights) — once as the run did (`cur`, checked
/// against the real master's pixels), once with each member's scale corrected
/// to the photometric truth (`cor`: `v'' = v' + (c − 1)·(v' − sky_ref)`, the
/// same `B = B_ref − A·B_tgt` bookkeeping with `A·c`, `sky_ref` the LN
/// reference's local sky), and both again without rejection. Rejection
/// normalization is assumed `local` (the working copy IS the LN copy).
fn stack_sim(cfg_path: &PathBuf) {
    use athenaeum_core::integration::combine::{
        combine_pixel_weighted, mask_clear, mask_get, mask_words, IntegrationRecipe, Rejection,
    };
    use athenaeum_core::stacking::ln::LnFrameGrids;
    let cfg: SimConfig = serde_json::from_str(
        &std::fs::read_to_string(cfg_path)
            .unwrap_or_else(|e| die(format!("{}: {e}", cfg_path.display()))),
    )
    .unwrap_or_else(|e| die(format!("{}: {e}", cfg_path.display())));
    let (ref_plane, w, h) = read_plane(&cfg.reference, cfg.plane);
    let (master, mw, mh) = read_plane(&cfg.master, cfg.plane);
    if (mw, mh) != (w, h) {
        die(format!("master {mw}x{mh} is not the reference's {w}x{h}"));
    }
    let side = 2 * cfg.half + 1;
    let npix = side * side;
    let n = cfg.members.len();
    let stars: Vec<(usize, usize)> = cfg
        .stars
        .iter()
        .map(|&(x, y)| (x.round() as usize, y.round() as usize))
        .filter(|&(x, y)| x >= cfg.half && y >= cfg.half && x + cfg.half < w && y + cfg.half < h)
        .collect();
    let mut scratch = Vec::new();
    // The LN reference's local sky per star (the `B_ref` of the correction).
    let sky_ref: Vec<f64> = stars
        .iter()
        .map(|&(x, y)| {
            annulus_median(
                &ref_plane,
                w,
                h,
                x as f64,
                y as f64,
                5.0 * cfg.f_phot,
                7.0 * cfg.f_phot,
                &mut scratch,
            )
            .unwrap_or(f64::NAN)
        })
        .collect();
    // values[star][pixel * n + frame]
    let mut cur: Vec<Vec<f32>> = vec![vec![f32::NAN; npix * n]; stars.len()];
    for (i, m) in cfg.members.iter().enumerate() {
        let t = Instant::now();
        let plane = match (&m.registered, &m.calibrated, &m.map) {
            (Some(r), _, _) => read_plane(r, cfg.plane).0,
            (None, Some(c), Some(mp)) => {
                warp_plane(c, mp, w, h, cfg.plane, cfg.interpolation, cfg.clamping)
            }
            _ => die(format!(
                "{}: no registered artifact and no calibrated+map",
                m.label
            )),
        };
        let grids = LnFrameGrids::read(&m.athln)
            .unwrap_or_else(|e| die(format!("{}: {e}", m.athln.display())));
        let grid = &grids.channels[cfg.plane];
        let (mut a_row, mut b_row) = (vec![0f32; w], vec![0f32; w]);
        for (s, &(sx, sy)) in stars.iter().enumerate() {
            for (wy, y) in (sy - cfg.half..=sy + cfg.half).enumerate() {
                grid.evaluate_row(y, &mut a_row, &mut b_row);
                for (wx, x) in (sx - cfg.half..=sx + cfg.half).enumerate() {
                    let v = plane[y * w + x];
                    if v.is_finite() {
                        cur[s][(wy * side + wx) * n + i] = a_row[x] * v + b_row[x];
                    }
                }
            }
        }
        eprintln!(
            "member {} ({}/{n}) {} ms",
            m.label,
            i + 1,
            t.elapsed().as_millis()
        );
    }
    let weights: Vec<f32> = cfg.members.iter().map(|m| m.weight).collect();
    let recipe = IntegrationRecipe::average(Rejection::LinearFitClip {
        sigma_low: cfg.sigma_low,
        sigma_high: cfg.sigma_high,
    });
    // Per-frame rejection tallies: [variant][frame] (core rejected, core total, wing rejected, wing total).
    let mut tally = vec![vec![[0u64; 4]; n]; 2];
    let mut max_rel_dev = 0.0f64;
    let mut max_abs_dev = 0.0f64;
    let mut star_rows = Vec::new();
    let mut mask = vec![0u64; mask_words(n)];
    let mut work: Vec<(f32, u16)> = Vec::with_capacity(n);
    let mut sc = Vec::new();
    for (s, &(sx, sy)) in stars.iter().enumerate() {
        let mut win = [
            vec![0f32; npix],
            vec![0f32; npix],
            vec![0f32; npix],
            vec![0f32; npix],
        ]; // cur, cor, cur_norej, cor_norej
        let mut out_cor = vec![f32::NAN; n];
        for p in 0..npix {
            let (wy, wx) = (p / side, p % side);
            let (dx, dy) = (wx as f64 - cfg.half as f64, wy as f64 - cfg.half as f64);
            let r = (dx * dx + dy * dy).sqrt();
            let zone = if r <= cfg.f_ref {
                Some(0)
            } else if r <= 3.0 * cfg.f_ref {
                Some(2)
            } else {
                None
            };
            let vals = &cur[s][p * n..(p + 1) * n];
            for i in 0..n {
                let v = vals[i];
                out_cor[i] = if v.is_finite() {
                    (v as f64 + (cfg.members[i].correction - 1.0) * (v as f64 - sky_ref[s])) as f32
                } else {
                    f32::NAN
                };
            }
            for (variant, out) in [vals, out_cor.as_slice()].into_iter().enumerate() {
                work.clear();
                for (i, &v) in out.iter().enumerate() {
                    if v.is_finite() {
                        work.push((v, i as u16));
                    }
                }
                mask_clear(&mut mask);
                let (value, _rej, _) =
                    combine_pixel_weighted(&mut work, out, &weights, recipe, &mut mask, &mut sc);
                win[variant][p] = value;
                // no rejection: the weighted mean of every finite sample
                let (mut num, mut den) = (0.0f64, 0.0f64);
                for (i, &v) in out.iter().enumerate() {
                    if v.is_finite() && v != 0.0 && weights[i] > 0.0 {
                        num += v as f64 * weights[i] as f64;
                        den += weights[i] as f64;
                    }
                }
                win[variant + 2][p] = (num / den) as f32;
                if let Some(z) = zone {
                    for (i, &v) in out.iter().enumerate() {
                        if v.is_finite() {
                            tally[variant][i][z + 1] += 1;
                            if !mask_get(&mask, i) {
                                tally[variant][i][z] += 1;
                            }
                        }
                    }
                }
                if variant == 0 {
                    let mv = master[(sy - cfg.half + wy) * w + (sx - cfg.half + wx)] as f64;
                    let d = (value as f64 - mv).abs();
                    max_abs_dev = max_abs_dev.max(d);
                    max_rel_dev = max_rel_dev.max(d / mv.abs().max(1e-12));
                }
            }
        }
        // photometry on the windows (centre = window centre)
        let c = cfg.half as f64;
        let phot = |img: &[f32]| -> f64 {
            let mut sc2 = Vec::new();
            let sky = annulus_median(
                img,
                side,
                side,
                c,
                c,
                5.0 * cfg.f_phot,
                7.0 * cfg.f_phot,
                &mut sc2,
            );
            match (sky, circle_sum(img, side, side, c, c, 3.0 * cfg.f_phot)) {
                (Some(sk), Some((sum, nn))) => sum - nn as f64 * sk,
                _ => f64::NAN,
            }
        };
        let cut = |img: &[f32], x0: usize, y0: usize| -> Vec<f32> {
            let mut o = vec![0f32; npix];
            for wy in 0..side {
                for wx in 0..side {
                    o[wy * side + wx] = img[(y0 + wy) * w + x0 + wx];
                }
            }
            o
        };
        let refw = cut(&ref_plane, sx - cfg.half, sy - cfg.half);
        let masw = cut(&master, sx - cfg.half, sy - cfg.half);
        // Half-maximum FWHM proxy: 2·sqrt(N/π) over the pixels within
        // 3·fPhot at or above half the sky-subtracted peak (the acceptance
        // truth's own definition, without its connected-region rule — these
        // windows hold one isolated star).
        let fwhm = |img: &[f32]| -> f64 {
            let mut sc2 = Vec::new();
            let Some(sky) = annulus_median(
                img,
                side,
                side,
                c,
                c,
                5.0 * cfg.f_phot,
                7.0 * cfg.f_phot,
                &mut sc2,
            ) else {
                return f64::NAN;
            };
            let r2 = (3.0 * cfg.f_phot).powi(2);
            let mut peak = f64::NEG_INFINITY;
            for wy in cfg.half - 1..=cfg.half + 1 {
                for wx in cfg.half - 1..=cfg.half + 1 {
                    peak = peak.max(img[wy * side + wx] as f64 - sky);
                }
            }
            let mut count = 0usize;
            for wy in 0..side {
                for wx in 0..side {
                    let (dx, dy) = (wx as f64 - c, wy as f64 - c);
                    if dx * dx + dy * dy <= r2 && img[wy * side + wx] as f64 - sky >= 0.5 * peak {
                        count += 1;
                    }
                }
            }
            2.0 * (count as f64 / std::f64::consts::PI).sqrt()
        };
        star_rows.push(format!(
            "{sx},{sy},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.4},{:.4},{:.4},{:.4}",
            phot(&refw),
            phot(&masw),
            phot(&win[0]),
            phot(&win[1]),
            phot(&win[2]),
            phot(&win[3]),
            sky_ref[s],
            fwhm(&refw),
            fwhm(&masw),
            fwhm(&win[0]),
            fwhm(&win[1])
        ));
    }
    let mut f = std::fs::File::create(&cfg.out_stars)
        .unwrap_or_else(|e| die(format!("{}: {e}", cfg.out_stars.display())));
    writeln!(
        f,
        "x,y,ref,master,sim_cur,sim_cor,sim_cur_norej,sim_cor_norej,sky_ref,fwhm_ref,fwhm_master,fwhm_cur,fwhm_cor"
    )
    .ok();
    for r in &star_rows {
        writeln!(f, "{r}").ok();
    }
    let mut f = std::fs::File::create(&cfg.out_frames)
        .unwrap_or_else(|e| die(format!("{}: {e}", cfg.out_frames.display())));
    writeln!(f, "label,weight,correction,cur_core_rej,cur_core_n,cur_wing_rej,cur_wing_n,cor_core_rej,cor_core_n,cor_wing_rej,cor_wing_n").ok();
    for (i, m) in cfg.members.iter().enumerate() {
        let (a, b) = (tally[0][i], tally[1][i]);
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{}",
            m.label, m.weight, m.correction, a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]
        )
        .ok();
    }
    println!(
        "{}",
        serde_json::json!({"stars": stars.len(), "members": n, "validationMaxAbsDev": max_abs_dev, "validationMaxRelDev": max_rel_dev})
    );
}
