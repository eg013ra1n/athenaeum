//! Plan D task D0 probe (research): replays the stacking Measure stage on a list of frames exactly as
//! a run does (`measure_frame_pass1`, then `measure_frame_pass2` at the group β per plane) and writes, per plane, the
//! channel measurement, candidate weight totals (`signal_totals` over subsets of the accepted fits), the `c`-sample
//! classifier counts, and one compact TSV row per accepted fit (for the frame-common star set, built offline).
//!
//!   d0_weight_probe --config <stacking config json> --betas b0[,b1,b2] --out-dir <dir> --ap-radius <px>
//!       [--threads N] [--tag <name>] <file>...
//!
//! Writes `<out-dir>/frames.jsonl` (appends one line per plane) and `<out-dir>/stars/<stem>.p<k>.tsv`.
//! Energies are the module's ADU units (x 65535 of the [0, 1] domain), as `StarFit` holds them.
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::measure::{
    gain_from_header, measure_frame_pass1, measure_frame_pass2, measure_planes_pass1, measure_planes_pass2,
    ChannelMeasurement, SeedOverride, ADU_SCALE,
};
use athenaeum_core::star_measure::{model_flux_in_fwtm, signal_totals, PixelNoise, StarFit};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::convert::Infallible;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("d0_weight_probe: {msg}");
    std::process::exit(1)
}

fn nn(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        Value::Null
    }
}

fn totals(fits: &[StarFit], keep: impl Fn(&StarFit) -> bool) -> Value {
    let sub: Vec<StarFit> = fits.iter().filter(|f| keep(f)).cloned().collect();
    let t = signal_totals(&sub);
    let s = ADU_SCALE as f64;
    json!({"n": sub.len(), "tflux": nn(t.tflux / s), "tmeanFlux": nn(t.tmean_flux / s), "rejected": t.rejected})
}

/// Sum of (pixel - the fit's ring sky) over a disc of fixed radius `r` px about the fit's centre; NaN when the disc
/// leaves the plane, holds a non-finite pixel or the ring sky is missing.
fn fixed_aperture(data: &[f32], w: usize, h: usize, f: &StarFit, r: f64) -> f64 {
    if !f.sky_ring.is_finite() {
        return f64::NAN;
    }
    let (x0, x1) = ((f.x - r).floor(), (f.x + r).ceil());
    let (y0, y1) = ((f.y - r).floor(), (f.y + r).ceil());
    if x0 < 0.0 || y0 < 0.0 || x1 >= w as f64 || y1 >= h as f64 {
        return f64::NAN;
    }
    let s = ADU_SCALE as f64;
    let mut sum = 0.0f64;
    let r2 = r * r;
    for y in y0 as usize..=y1 as usize {
        let dy = y as f64 - f.y;
        for x in x0 as usize..=x1 as usize {
            let dx = x as f64 - f.x;
            if dx * dx + dy * dy <= r2 {
                let v = data[y * w + x];
                if !v.is_finite() {
                    return f64::NAN;
                }
                sum += v as f64 * s - f.sky_ring;
            }
        }
    }
    sum
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut config, mut betas, mut out_dir, mut ap_r, mut threads, mut tag) =
        (None::<PathBuf>, Vec::<f64>::new(), None::<PathBuf>, 0.0f64, 4usize, String::new());
    let mut files: Vec<PathBuf> = Vec::new();
    let mut adu = false;
    while let Some(a) = args.next() {
        let mut v = || args.next().unwrap_or_else(|| die(format!("{a} needs a value")));
        match a.as_str() {
            "--config" => config = Some(PathBuf::from(v())),
            "--betas" => {
                betas = v()
                    .split(',')
                    .map(|t| t.parse().unwrap_or_else(|_| die("--betas")))
                    .collect()
            }
            "--out-dir" => out_dir = Some(PathBuf::from(v())),
            "--ap-radius" => ap_r = v().parse().unwrap_or_else(|_| die("--ap-radius")),
            "--threads" => threads = v().parse().unwrap_or_else(|_| die("--threads")),
            "--tag" => tag = v(),
            "--adu" => adu = true,
            o if o.starts_with("--") => die(format!("unknown flag {o}")),
            _ => files.push(PathBuf::from(a)),
        }
    }
    let config = config.unwrap_or_else(|| die("--config is required"));
    let out_dir = out_dir.unwrap_or_else(|| die("--out-dir is required"));
    if betas.is_empty() || !(ap_r > 0.0) {
        die("--betas and --ap-radius are required")
    }
    let text = std::fs::read_to_string(&config).unwrap_or_else(|e| die(format!("{}: {e}", config.display())));
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| die(format!("config: {e}")));
    let doc = v.get("config").cloned().unwrap_or(v);
    let cfg: StackingConfig = serde_json::from_value(doc).unwrap_or_else(|e| die(format!("config: {e}")));
    let opts = cfg.measurement.measure_options(cfg.normalization.scale_estimator);
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap_or_else(|e| die(format!("pool: {e}"))),
    );
    std::fs::create_dir_all(out_dir.join("stars")).unwrap_or_else(|e| die(format!("out-dir: {e}")));
    let mut frames = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir.join("frames.jsonl"))
        .unwrap_or_else(|e| die(format!("frames.jsonl: {e}")));
    let cancel = AtomicBool::new(false);
    for path in &files {
        let t0 = std::time::Instant::now();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("frame").to_string();
        let reader = PlaneReader::open(Path::new(path)).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
        let n = reader.channels();
        let (w, h) = (reader.width(), reader.height());
        let b: Vec<f64> = if betas.len() == n { betas.clone() } else if betas.len() == 1 { vec![betas[0]; n] } else {
            die(format!("{}: {} planes, {} betas", path.display(), n, betas.len()))
        };
        // `--adu`: a raw light (ADU samples) divided by 65535 and measured through the same two passes the frame entry
        // points wrap (the run only ever sees calibrated [0, 1] frames).
        let raw_planes: Option<Vec<Vec<f32>>> = adu.then(|| {
            (0..n)
                .map(|p| {
                    let mut v = reader.read_plane(p).unwrap_or_else(|e| die(format!("{}: plane {p}: {e}", path.display())));
                    v.iter_mut().for_each(|x| *x /= ADU_SCALE);
                    v
                })
                .collect()
        });
        let (channels, fits): (Vec<ChannelMeasurement>, Vec<Vec<StarFit>>) = if let Some(planes) = &raw_planes {
            let read = |p: usize| Ok::<_, Infallible>(Cow::Borrowed(planes[p].as_slice()));
            let noise = if n == 1 { PixelNoise::White } else { PixelNoise::Mixed };
            let pass1 = measure_planes_pass1(n, w, h, &opts, Some(&pool), SeedOverride::Fast, false, noise, gain_from_header(path), read)
                .unwrap_or_else(|never| match never {});
            let mp = measure_planes_pass2(pass1, &b, Some(&pool), read).unwrap_or_else(|never| match never {});
            mp.into_iter().map(|m| (m.measurement.channel, m.measurement.fits)).unzip()
        } else {
            let pass1 = match measure_frame_pass1(path, &opts, Some(&pool), &cancel) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("d0_weight_probe: {}: pass 1: {e}", path.display());
                    continue;
                }
            };
            match measure_frame_pass2(path, pass1, &b, &opts, Some(&pool), &cancel) {
                Ok((fm, fits)) => (fm.channels, fits),
                Err(e) => {
                    eprintln!("d0_weight_probe: {}: pass 2: {e}", path.display());
                    continue;
                }
            }
        };
        for (p, (ch, pf)) in channels.iter().zip(&fits).enumerate() {
            let data = match &raw_planes {
                Some(planes) => planes[p].clone(),
                None => reader.read_plane(p).unwrap_or_else(|e| die(format!("{}: plane {p}: {e}", path.display()))),
            };
            // `c`'s candidate pool before the `extended` gate: accepted, unsaturated, finite model flux, SNR >= 20,
            // by model flux (the order `aperture_correction` sorts in).
            let mut pool0: Vec<(f64, bool)> = pf
                .iter()
                .filter(|f| !f.flags.saturated())
                .map(|f| (model_flux_in_fwtm(f), f.flags.extended(), f.snr))
                .filter(|&(m, _, snr)| m.is_finite() && m > 0.0 && snr >= 20.0)
                .map(|(m, e, _)| (m, e))
                .collect();
            pool0.sort_by(|a, b| b.0.total_cmp(&a.0));
            let ext_in = |k: usize| pool0.iter().take(k).filter(|x| x.1).count();
            // How deep the 64 non-extended candidates reach into the pool (1-based rank of the 64th), and their
            // median model flux against the top 64's.
            let mut kept = 0usize;
            let mut depth = 0usize;
            let mut kept_m: Vec<f64> = Vec::new();
            for (i, (m, e)) in pool0.iter().enumerate() {
                if !e {
                    kept += 1;
                    kept_m.push(*m);
                    if kept == 64 {
                        depth = i + 1;
                        break;
                    }
                }
            }
            let med = |v: &mut Vec<f64>| {
                if v.is_empty() {
                    f64::NAN
                } else {
                    v.sort_by(f64::total_cmp);
                    v[v.len() / 2]
                }
            };
            let mut top_m: Vec<f64> = pool0.iter().take(64).map(|x| x.0).collect();
            let q1 = json!({
                "pool": pool0.len(),
                "ext64": ext_in(64), "ext128": ext_in(128), "ext256": ext_in(256),
                "extPool": pool0.iter().filter(|x| x.1).count(),
                "depth64": depth,
                "medModelKept64": nn(med(&mut kept_m) / ADU_SCALE as f64),
                "medModelTop64": nn(med(&mut top_m) / ADU_SCALE as f64),
                "extAccepted": pf.iter().filter(|f| f.flags.extended()).count(),
                "satAccepted": pf.iter().filter(|f| f.flags.saturated()).count(),
            });
            let cand = json!({
                "all": totals(pf, |_| true),
                "unsat": totals(pf, |f| !f.flags.saturated()),
                "stat": totals(pf, |f| f.flags.in_statistic()),
            });
            let line = json!({"tag": tag, "file": path.display().to_string(), "stem": stem, "plane": p,
                              "beta": b[p], "width": w, "height": h, "apRadius": ap_r,
                              "channel": ch, "cand": cand, "q1": q1,
                              "wallMs": t0.elapsed().as_millis() as u64});
            writeln!(frames, "{line}").unwrap_or_else(|e| die(format!("write: {e}")));
            let tsv = out_dir.join("stars").join(format!("{stem}.p{p}.tsv"));
            let file = std::fs::File::create(&tsv).unwrap_or_else(|e| die(format!("{}: {e}", tsv.display())));
            let mut o = std::io::BufWriter::new(file);
            let s = ADU_SCALE as f64;
            writeln!(o, "x\ty\tsignal\tmodel\tarea\tsnr\tflags\tfwhm\tecc\tamplitude\tpeak5\tskyRing\tresidual\tapFixed")
                .unwrap_or_else(|e| die(format!("write: {e}")));
            for f in pf {
                let ap = fixed_aperture(&data, w, h, f, ap_r);
                writeln!(
                    o,
                    "{:.3}\t{:.3}\t{:.6e}\t{:.6e}\t{:.4}\t{:.3}\t{}\t{:.4}\t{:.4}\t{:.6e}\t{:.6e}\t{:.6e}\t{:.5e}\t{:.6e}",
                    f.x,
                    f.y,
                    f.signal / s,
                    model_flux_in_fwtm(f) / s,
                    f.area,
                    f.snr,
                    f.flags.bits(),
                    f.fwhm(),
                    f.eccentricity(),
                    f.amplitude / s,
                    f.peak5 / s,
                    f.sky_ring / s,
                    f.residual,
                    ap / s
                )
                .unwrap_or_else(|e| die(format!("write: {e}")));
            }
            o.flush().unwrap_or_else(|e| die(format!("write: {e}")));
        }
        eprintln!("d0_weight_probe: {} {} ms", stem, t0.elapsed().as_millis());
    }
}
