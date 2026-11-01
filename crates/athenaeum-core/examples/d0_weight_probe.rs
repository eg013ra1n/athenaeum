//! Plan D task D0 probe (research): replays the stacking Measure stage on a list of frames exactly as
//! a run does (`measure_planes_pass1`, then `measure_planes_pass2` at the group β per plane, the two passes the frame
//! entry points wrap) and writes, per plane, the
//! channel measurement, candidate weight totals (`signal_totals` over subsets of the accepted fits), the `c`-sample
//! classifier counts, and one compact TSV row per accepted fit (for the frame-common star set, built offline).
//!
//! Plan D D2 (PD-9): the `q1` record also reads the colour sample's taking-back. `depth64Rescue` / `medModelKept64Rescue`
//! are `depth64` / `medModelKept64` with a rescued extended candidate kept (D0's fields keep their old meaning);
//! `rescued` is the probe's count at `aperture_correction`'s entry filter, built from the core's own
//! `point_like_sibling_seeds` (through `PlanesPass1::point_like_siblings`), radius and index, and `rescuedCore` the
//! Measure's own (`PlaneMeasurement::aperture_rescued`). The two must agree on every plane or the probe stops.
//!
//!   d0_weight_probe --config <stacking config json> --betas b0[,b1,b2] --out-dir <dir> --ap-radius <px>
//!       [--threads N] [--tag <name>] <file>...
//!
//! Writes `<out-dir>/frames.jsonl` (appends one line per plane) and `<out-dir>/stars/<stem>.p<k>.tsv`.
//!
//!   d0_weight_probe common --dumps <D0 probe dir> --clone <C10 clone dir> [--out-dir <dir>]
//!
//! (Plan D D1a) judges the product's frame-common star set against D0's offline set (b) on those dumps; see the `common`
//! module below. Plan D D2b (PD-26), D2c (PD-30): the star dump carries each fit's aperture sum `S` (`apSum`: the sum of
//! `v - B_i` within 3 x the frame's ring FWHM, read in Measure's HFR walk; the common-set weight reads it) beside `signal`
//! and `apFixed`, the noise of that sum (`apNoise`) and the fit's flags with `SIBLING_POINT` (bit value 8, PD-9's rescue of an
//! extended fit) in `flags`; a dump measured before D2c has no `apNoise` column and the `common` arm refuses it.
//! Energies are the module's ADU units (x 65535 of the [0, 1] domain), as `StarFit` holds them.
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::measure::{
    gain_from_header, measure_planes_pass1, measure_planes_pass2, MeasuredPlane, SeedOverride, ADU_SCALE,
};
use athenaeum_core::star_measure::photometry::{point_source_match_px, NearIndex};
use athenaeum_core::star_measure::{model_flux_in_fwtm, signal_totals, PixelNoise, StarFit};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("common") {
        return common::run(&argv[1..]);
    }
    let mut args = argv.into_iter();
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
        // Both frame kinds go through the group measure's two passes on the planes' own reads (the run only ever sees
        // calibrated [0, 1] frames; `--adu` hands the raw light divided by 65535). The probe holds the passes apart so
        // it can read the colour sample's point-like sibling seeds off the first pass (Plan D D2, PD-9).
        let read = |p: usize| -> Result<Cow<'_, [f32]>, String> {
            match &raw_planes {
                Some(planes) => Ok(Cow::Borrowed(planes[p].as_slice())),
                None => reader
                    .read_plane(p)
                    .map(Cow::Owned)
                    .map_err(|e| format!("{}: plane {p}: {e}", path.display())),
            }
        };
        let noise = match n {
            1 => PixelNoise::White,
            3 => PixelNoise::Mixed,
            _ => PixelNoise::Unknown,
        };
        let pass1 = match measure_planes_pass1(n, w, h, &opts, Some(&pool), SeedOverride::Fast, false, noise, gain_from_header(path), read) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("d0_weight_probe: {}: pass 1: {e}", path.display());
                continue;
            }
        };
        let point_siblings = pass1.point_like_siblings();
        let measured: Vec<MeasuredPlane> = match measure_planes_pass2(pass1, &b, Some(&pool), read) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("d0_weight_probe: {}: pass 2: {e}", path.display());
                continue;
            }
        };
        for (p, mp) in measured.iter().enumerate() {
            let (ch, pf) = (&mp.measurement.channel, &mp.measurement.fits);
            let data = match &raw_planes {
                Some(planes) => planes[p].clone(),
                None => reader.read_plane(p).unwrap_or_else(|e| die(format!("{}: plane {p}: {e}", path.display()))),
            };
            // Plan D D2 (PD-9): the colour sample's taking-back, read off the core's own pieces. `near` is the index
            // `aperture_correction` builds (the plane's entry of `point_like_sibling_seeds`, at the PB-12' radius of
            // the field FWHM the fit ran on, which is `field_width` when the caller gave it, as the measure path
            // does); an extended fit is rescued when it holds a point-like seed of another plane within that radius.
            let field_fwhm = mp.measurement.field_width.fwhm_px;
            let near = point_siblings[p]
                .as_ref()
                .map(|seeds| NearIndex::new(seeds.iter().map(|s| (s.x, s.y)), point_source_match_px(field_fwhm)));
            let rescue = |f: &StarFit| f.flags.extended() && near.as_ref().is_some_and(|nr| nr.any_within(f.x, f.y));
            // The core's entry filter (photometry.rs, `aperture_correction`): not saturated, and not extended unless
            // rescued, a finite positive model flux; its `rescued` counts the extended ones left in the list. The
            // self-check: the probe's count and the Measure's own (`aperture_rescued`) must agree on every plane.
            let rescued = pf
                .iter()
                .filter(|f| !f.flags.saturated() && rescue(f))
                .map(model_flux_in_fwtm)
                .filter(|m| m.is_finite() && *m > 0.0)
                .count();
            let rescued_core = mp.measurement.aperture_rescued;
            if rescued != rescued_core {
                die(format!(
                    "SELF-CHECK MISMATCH {} plane {p} ({n} planes): the probe counts {rescued} rescued, the Measure {rescued_core}",
                    path.display()
                ));
            }
            // Plan D D2c (PD-30): the Measure's SIBLING_POINT is this same test on every extended fit, not only `c`'s candidates.
            // The self-check: the flag is set on exactly the extended fits the probe's own index rescues, saturated ones included,
            // and on no other.
            for f in pf.iter() {
                if f.flags.sibling_point() != rescue(f) {
                    die(format!(
                        "SELF-CHECK MISMATCH {} plane {p} ({n} planes): fit at ({:.2}, {:.2}) flags {} but the probe's sibling test says {}",
                        path.display(),
                        f.x,
                        f.y,
                        f.flags.bits(),
                        rescue(f)
                    ));
                }
            }
            // `c`'s candidate pool before the `extended` gate: accepted, unsaturated, finite model flux, SNR >= 20,
            // by model flux (the order `aperture_correction` sorts in); each with its extended flag (D0's meaning)
            // and whether the colour sample takes it back.
            let mut pool0: Vec<(f64, bool, bool)> = pf
                .iter()
                .filter(|f| !f.flags.saturated())
                .map(|f| (model_flux_in_fwtm(f), f.flags.extended(), rescue(f), f.snr))
                .filter(|&(m, _, _, snr)| m.is_finite() && m > 0.0 && snr >= 20.0)
                .map(|(m, e, r, _)| (m, e, r))
                .collect();
            pool0.sort_by(|a, b| b.0.total_cmp(&a.0));
            let ext_in = |k: usize| pool0.iter().take(k).filter(|x| x.1).count();
            // How deep the 64 non-extended candidates reach into the pool (1-based rank of the 64th), and their
            // median model flux against the top 64's; and the same with a rescued extended candidate kept as well.
            let kept_64 = |keep: &dyn Fn(&(f64, bool, bool)) -> bool| -> (usize, Vec<f64>) {
                let mut kept = 0usize;
                let mut depth = 0usize;
                let mut kept_m: Vec<f64> = Vec::new();
                for (i, x) in pool0.iter().enumerate() {
                    if keep(x) {
                        kept += 1;
                        kept_m.push(x.0);
                        if kept == 64 {
                            depth = i + 1;
                            break;
                        }
                    }
                }
                (depth, kept_m)
            };
            let (depth, mut kept_m) = kept_64(&|x| !x.1);
            let (depth_r, mut kept_m_r) = kept_64(&|x| !x.1 || x.2);
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
                "depth64Rescue": depth_r,
                "medModelKept64Rescue": nn(med(&mut kept_m_r) / ADU_SCALE as f64),
                "rescued": rescued,
                "rescuedCore": rescued_core,
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
            writeln!(o, "x\ty\tsignal\tmodel\tarea\tsnr\tflags\tfwhm\tecc\tamplitude\tpeak5\tskyRing\tresidual\tapFixed\tapSum\tapNoise")
                .unwrap_or_else(|e| die(format!("write: {e}")));
            let ring_fwhm = ch.fwhm_px;
            for f in pf {
                let ap = fixed_aperture(&data, w, h, f, ap_r);
                // The 3 F sum's noise (PD-30): `star_snr`'s sigma of the sum at max(1.5 F, 3) px, `S_1.5 / snr`, scaled by the root of
                // the aperture areas, (3 ring F)^2 / max(1.5 F, 3)^2 in pixels^2 (the field FWHM of the plane stands for the ring width).
                let r15 = (1.5 * f.fwhm()).max(3.0);
                let s15 = fixed_aperture(&data, w, h, f, r15);
                let ap_noise = if f.snr.is_finite() && f.snr > 0.0 && s15.is_finite() {
                    (s15 / f.snr).abs() * (3.0 * ring_fwhm / r15)
                } else {
                    f64::NAN
                };
                writeln!(
                    o,
                    "{:.3}\t{:.3}\t{:.6e}\t{:.6e}\t{:.4}\t{:.3}\t{}\t{:.4}\t{:.4}\t{:.6e}\t{:.6e}\t{:.6e}\t{:.5e}\t{:.6e}\t{:.6e}\t{:.6e}",
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
                    ap / s,
                    f.ap_sum / s,
                    ap_noise / s
                )
                .unwrap_or_else(|e| die(format!("write: {e}")));
            }
            o.flush().unwrap_or_else(|e| die(format!("write: {e}")));
        }
        eprintln!("d0_weight_probe: {} {} ms", stem, t0.elapsed().as_millis());
    }
}

// ------------------------------------------------------------------------------------------------------------------
// The `common` arm (Plan D, D1a): the product's frame-common star set (`stacking::common_set`) against D0's offline set
// (b), on D0's per-star dumps and the C10 clones' stored registration.
//
//   d0_weight_probe common --dumps <D0 probe dir> --clone <C10 clone dir> [--out-dir <dir>] [--proxy-floor <snr>]
//       [--set-rescue off|on]
//
// `--proxy-floor` (default 20, the module's CORRECTION_MIN_SNR) post-filters the candidates to those whose proxy SNR reaches
// it: the what-if for "a higher proxy floor", never the gate. `--set-rescue` (default `on`, the product) clears the dump's
// SIBLING_POINT bit before the module reads the fits when `off`: the set then admits no extended fit (PD-30's gate runs both). Reads (read only): `<dumps>/frames.jsonl` + `<dumps>/stars/<stem>.p<k>.tsv` (D0's rows: x, y, signal, area, snr, flags,
// apFixed) and `<clone>/athenaeum.db` through `immutable=1` (the last run's frames, calibrated artifacts, registration).
// Prints, per group-plane and group, the `D1A` lines `star-lab/bin/pland_bars.py d1a` judges, then free-form lines
// (`plane-info`, `frame`, `replay`) that no judge reads. A dump measured at another beta than the clone's group beta (the
// beta 3.07 subset) is judged by its `beta307` line only: the clone's stored numbers are not its numbers.
//
// Members are the clone run's included frames that carry a registration map and a dump row for every plane. Ranks are over
// those members only, 1 = top weight, ties averaged; the sharpest decile is round(0.1 n) frames (at least 1, ties to even as
// D0's Python rounded: 105 frames give 10) by mean FWHM. D0's (b) is recomputed here on the same members (reference anchor, dump SNR >= 20, isolation 2 R of the
// dump's aperture radius, extended flag ignored, ratios over every matched star), not read from D0's tables.
//
// Where this (b) differs from D0's `analyse.py`: it maps every fit through `PixelMap::forward_exact` (the stored registration with its
// distortion layer, as the product maps) where `analyse.py` applied the linear homography alone, so a star far from the centre of a
// distorted field can sit a fraction of a pixel elsewhere; and its members are this clone run's included frames with a map (hc11 L: 73,
// D0 counted 76). The (b) decile cells equal D0's printed ones on all five groups; the weights differ from D0's tables by 2e-7 (s204 OSC).
//
// PD-16 (D1a's set bars are a drift and a noise, fable 2026-11-01), the numbers of fable's `t1_drift.py` / `t2_set.py`:
//   `D1A drift <ds> <group> slope_pct_per_px=<s> se=<se>`: the OLS fit of ln(w_product / w_b) on the frame's mean FWHM over the members
//       with both weights finite and > 0; s = 100 (e^b - 1) and se = 100 SE(b), %/px.
//   `rf_noise_med / rf_noise_p90 / rf_noise_max` on the `D1A plane` line: per member sqrt(sum (s_i / snr_i)^2) / sum s_i over the set stars
//       it pairs, plus the anchor's same term over the same stars, in quadrature (s_i the fit's signal, snr_i the dump's per-star SNR; a
//       pair whose either SNR is not finite and positive is left out of both legs); fractions of r_f; the median, the 90th percentile
//       (linear interpolation) and the maximum over the members. The anchor's own entry takes the same formula (both legs are its fits).
//   `recovered` and `proxy_only` stay on the plane line as notes.
// The pairs come from `MemberIndex::at`, the ONE re-match of `common_set::match_member`'s search (built on the module's constants), shared
// by S2 and `lost_breakdown`; it must agree with the module's own table or the probe stops.
//
// Plan D D2b (PD-26): the module's paired ratios read each fit's aperture sum `S` (the dump's `apSum` column, `star_snr`'s sum at
// max(1.5 F, 3) px about the fit, ring sky) and no longer the model flux times `c`; the anchor's whole-set sums, hence the weight's
// absolute scale, stay model x `c`. So `w_product`, the ranks and every D1A line are the S-based weight; `rf_noise_*` reads
// sigma_i = S_i / snr_i (`star_snr`'s own noise). Beside them, per frame and plane, the S-based ratios and the model x `c` ones D1w
// had (`rf_s`/`rf_model`, `qf_s`/`qf_model`, new columns after `rf_noise`), the weight the model ratios give (`w_model`, `rank_model`),
// and the free-form line `model-weight` (its Spearman against the S-based weight). The model ratios are taken over every pair whose
// model flux is usable; the S ratios over the pairs whose `S` is too, so the two differ by exactly the stars the module drops for
// a missing `S`. A dump without the `apSum` column predates D2b and is refused.
mod common {
    use athenaeum_core::geometry::{KdTree2, PixelMap};
    use athenaeum_core::stacking::common_set::{self, Candidate, CommonOutcome, MemberPlane, Usable};
    use athenaeum_core::stacking::measure::ADU_SCALE;
    use athenaeum_core::stacking::psf_signal::psf_signal_weight;
    use athenaeum_core::star_measure::photometry::{
        CORRECTION_ISOLATION_PER_FIELD_FWHM, CORRECTION_MIN_SNR, POINT_SOURCE_MATCH_PER_FIELD_FWHM, POINT_SOURCE_MATCH_PX,
    };
    use athenaeum_core::star_measure::{StarFit, StarFlags};
    use rusqlite::{Connection, OpenFlags};
    use serde_json::Value;
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};

    const ADU: f64 = ADU_SCALE as f64;
    const SAT: u8 = 1;
    /// D0's SNR floor of (b), the dump's own SNR column.
    const B_MIN_SNR: f64 = 20.0;
    /// D0's (b) match tolerance (`analyse.py`): max(`B_MATCH_PX`, `B_MATCH_PER_FWHM` x the median field FWHM). The module's rule has
    /// the same two numbers by value (`POINT_SOURCE_MATCH_*`) and a different shape (the wider of two fields); (b)'s are D0's own.
    const B_MATCH_PX: f64 = 1.5;
    const B_MATCH_PER_FWHM: f64 = 0.5;
    /// hc11 L's eight C10 r1 frames and the tflux ratio C10 r1 gave each (lab `c10-r1-v6` weights.txt), as D0's replay.
    const C10_TARGET: [(i64, f64); 8] = [
        (69705, 0.7511),
        (69859, 0.7718),
        (69860, 0.6857),
        (69865, 0.6947),
        (69947, 0.6909),
        (69949, 0.7225),
        (69976, 0.6861),
        (70072, 0.7623),
    ];
    const C10_DS: &str = "hc11";
    const C10_GROUP: &str = "mono__L__bin1__180s";

    fn die(msg: impl std::fmt::Display) -> ! {
        eprintln!("d0_weight_probe common: {msg}");
        std::process::exit(1)
    }

    // ---------------------------------------------------------------------------------------------- the dump

    /// One (frame, plane) of D0's dump: the channel numbers the weight reads and the plane's fits.
    struct DumpPlane {
        base: String,
        beta: f64,
        ap_radius: f64,
        fwhm: f64,
        noise: f64,
        m_star: f64,
        unsaturated: usize,
        psf_signal_weight: f64,
        fits: Vec<StarFit>,
        ap_fixed: Vec<f64>,
    }

    /// `(ds, group)` -> plane -> file path -> the plane.
    type Dump = BTreeMap<(String, String), BTreeMap<usize, BTreeMap<String, DumpPlane>>>;

    fn num(v: &Value, key: &str) -> f64 {
        v.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
    }

    /// `rescue` off clears the dump's SIBLING_POINT bit on every fit (`--set-rescue off`): the set then reads what D2b's did,
    /// no extended fit. The dump's `apNoise` (the noise of the 3 F sum, PD-30) rides in the fit's `hfr` slot, which a dump has
    /// no HFR for and the probe reads nowhere else.
    fn read_stars(path: &Path, rescue: bool) -> (Vec<StarFit>, Vec<f64>) {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap_or_else(|| die(format!("{}: empty", path.display()))).split('\t').collect();
        let col = |name: &str| header.iter().position(|h| *h == name).unwrap_or_else(|| die(format!("{}: no column {name}", path.display())));
        let (cx, cy, cs, ca, cn, cf, cap) = (col("x"), col("y"), col("signal"), col("area"), col("snr"), col("flags"), col("apFixed"));
        // Plan D D2b / D2c: the aperture sum the weight reads and its noise. A dump from before D2c has no `apNoise` (and one from
        // before D2b no `apSum`; a D2b dump's `apSum` is the sum at 1.5 F, not this module's): measure it again with the main arm.
        let csum = header
            .iter()
            .position(|h| *h == "apSum")
            .unwrap_or_else(|| die(format!("{}: no column apSum: the dump predates Plan D D2b; measure it again with the main arm", path.display())));
        let cnoise = header
            .iter()
            .position(|h| *h == "apNoise")
            .unwrap_or_else(|| die(format!("{}: no column apNoise: the dump predates Plan D D2c (its apSum is not the 3 F sum); measure it again with the main arm", path.display())));
        let mut fits = Vec::new();
        let mut ap = Vec::new();
        for (n, line) in lines.enumerate() {
            let f: Vec<&str> = line.split('\t').collect();
            let get = |c: usize| -> f64 { f.get(c).and_then(|s| s.parse::<f64>().ok()).unwrap_or(f64::NAN) };
            if f.len() <= cap.max(csum).max(cnoise) {
                die(format!("{}: line {} has {} columns", path.display(), n + 2, f.len()));
            }
            let flags = f[cf].parse::<u8>().unwrap_or_else(|_| die(format!("{}: line {}: flags {:?}", path.display(), n + 2, f[cf])));
            let flags = if rescue { flags } else { flags & !StarFlags::SIBLING_POINT.bits() };
            fits.push(StarFit {
                x: get(cx),
                y: get(cy),
                background: 0.0,
                amplitude: f64::NAN,
                fwhm_x: f64::NAN,
                fwhm_y: f64::NAN,
                fwtm_x: f64::NAN,
                fwtm_y: f64::NAN,
                theta: 0.0,
                beta: f64::NAN,
                residual: f64::NAN,
                signal: get(cs) * ADU,
                area: get(ca),
                peak5: f64::NAN,
                sky_ring: f64::NAN,
                sky_ring_sigma: f64::NAN,
                sky_ring_n: 0,
                ap_sum: get(csum) * ADU,
                snr: get(cn),
                hfr: get(cnoise) * ADU,
                flags: StarFlags(flags),
            });
            ap.push(get(cap));
        }
        (fits, ap)
    }

    fn load_dump(dir: &Path, rescue: bool) -> Dump {
        let text = std::fs::read_to_string(dir.join("frames.jsonl")).unwrap_or_else(|e| die(format!("{}/frames.jsonl: {e}", dir.display())));
        let mut dump = Dump::new();
        for (n, line) in text.lines().enumerate() {
            let v: Value = serde_json::from_str(line).unwrap_or_else(|e| die(format!("frames.jsonl line {}: {e}", n + 1)));
            let tag = v["tag"].as_str().unwrap_or_else(|| die(format!("frames.jsonl line {}: no tag", n + 1)));
            let (ds, group) = tag.split_once(':').unwrap_or_else(|| die(format!("frames.jsonl line {}: tag {tag:?} is not ds:group", n + 1)));
            let plane = v["plane"].as_u64().unwrap_or_else(|| die("frames.jsonl: no plane")) as usize;
            let file = v["file"].as_str().unwrap_or_else(|| die("frames.jsonl: no file")).to_string();
            let stem = v["stem"].as_str().unwrap_or_else(|| die("frames.jsonl: no stem"));
            let ch = &v["channel"];
            let (fits, ap_fixed) = read_stars(&dir.join("stars").join(format!("{stem}.p{plane}.tsv")), rescue);
            let base = Path::new(&file).file_name().and_then(|s| s.to_str()).unwrap_or(&file).to_string();
            let p = DumpPlane {
                base,
                beta: num(&v, "beta"),
                ap_radius: num(&v, "apRadius"),
                fwhm: num(ch, "fwhmPx"),
                noise: num(ch, "noise"),
                m_star: num(ch, "mStar"),
                unsaturated: ch.get("starsUnsaturated").and_then(Value::as_u64).unwrap_or(0) as usize,
                psf_signal_weight: num(ch, "psfSignalWeight"),
                fits,
                ap_fixed,
            };
            dump.entry((ds.to_string(), group.to_string())).or_default().entry(plane).or_default().insert(file, p);
        }
        dump
    }

    // ---------------------------------------------------------------------------------------------- the clone

    struct CloneFrame {
        key: i64,
        included: bool,
        c_group: Vec<Option<f64>>,
        group_beta: Option<f64>,
    }

    struct Reg {
        reference: Option<i64>,
        map: Option<PixelMap>,
    }

    struct CloneDb {
        /// `(group key, calibrated file name)` -> index into `frames`.
        by_file: HashMap<(String, String), usize>,
        frames: Vec<CloneFrame>,
        regs: HashMap<i64, Reg>,
    }

    fn load_clone(dir: &Path) -> CloneDb {
        let db = dir.join("athenaeum.db");
        let uri = format!("file:{}?immutable=1", db.display());
        let con = Connection::open_with_flags(&uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
            .unwrap_or_else(|e| die(format!("{}: {e}", db.display())));
        let (run, set, project): (i64, Option<i64>, Option<String>) = con
            .query_row("select id, frames_set_id, project_id from stacking_runs order by id desc limit 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap_or_else(|e| die(format!("{}: no stacking run: {e}", db.display())));
        let key = if project.is_some() { "project_frame" } else { "frame_id" };
        eprintln!("d0_weight_probe common: clone {} run {run} ({})", dir.display(), if project.is_some() { "project" } else { "set" });
        let mut frames = Vec::new();
        let mut index: HashMap<(i64, String), usize> = HashMap::new();
        {
            let sql = format!(
                "select rf.{key}, g.group_key, rf.included, rf.metrics_json, g.beta from stacking_run_frames rf \
                 join stacking_run_groups g on g.id = rf.group_id where rf.run_id = ?1"
            );
            let mut st = con.prepare(&sql).unwrap_or_else(|e| die(format!("prepare frames: {e}")));
            let rows = st
                .query_map([run], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<f64>>(4)?))
                })
                .unwrap_or_else(|e| die(format!("query frames: {e}")));
            for row in rows {
                let (k, group, included, metrics, beta) = row.unwrap_or_else(|e| die(format!("frames row: {e}")));
                let c_group = metrics
                    .and_then(|m| match serde_json::from_str::<Value>(&m) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            eprintln!("d0_weight_probe common: frame {k} ({group}): unreadable metrics_json, its stand-in c reads 1: {e}");
                            None
                        }
                    })
                    .and_then(|m| m.get("channels").and_then(Value::as_array).cloned())
                    .map(|chs| chs.iter().map(|c| c.get("apertureCorrectionGroup").and_then(Value::as_f64)).collect())
                    .unwrap_or_default();
                index.insert((k, group), frames.len());
                frames.push(CloneFrame { key: k, included: included != 0, c_group, group_beta: beta });
            }
        }
        let mut by_file = HashMap::new();
        {
            let sql = format!("select {key}, group_key, path from stacking_artifacts where kind = 'calibrated'");
            let mut st = con.prepare(&sql).unwrap_or_else(|e| die(format!("prepare artifacts: {e}")));
            let rows = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))
                .unwrap_or_else(|e| die(format!("query artifacts: {e}")));
            for row in rows {
                let (k, group, path) = row.unwrap_or_else(|e| die(format!("artifacts row: {e}")));
                let Some(path) = path else { continue };
                let base = Path::new(&path).file_name().and_then(|s| s.to_str()).unwrap_or(&path).to_string();
                if let Some(&i) = index.get(&(k, group.clone())) {
                    by_file.insert((group, base), i);
                }
            }
        }
        let mut regs: HashMap<i64, Reg> = HashMap::new();
        let mut parse = |k: i64, reference: Option<i64>, json: Option<String>| {
            let map = json.and_then(|j| match PixelMap::from_json(&j) {
                Ok(m) => Some(m),
                Err(e) => {
                    eprintln!("d0_weight_probe common: frame {k}: unreadable registration map: {e}");
                    None
                }
            });
            regs.insert(k, Reg { reference, map });
        };
        if project.is_some() {
            let mut st = con
                .prepare("select project_frame, payload_json from stacking_artifacts where kind = 'registration'")
                .unwrap_or_else(|e| die(format!("prepare registration: {e}")));
            let rows = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
                .unwrap_or_else(|e| die(format!("query registration: {e}")));
            for row in rows {
                let (k, payload) = row.unwrap_or_else(|e| die(format!("registration row: {e}")));
                let v: Value = match payload {
                    Some(p) => serde_json::from_str(&p).unwrap_or_else(|e| {
                        eprintln!("d0_weight_probe common: frame {k}: unreadable registration payload, the frame has no map: {e}");
                        Value::Null
                    }),
                    None => Value::Null,
                };
                parse(
                    k,
                    v.get("referenceFrameId").and_then(Value::as_i64),
                    v.get("transformJson").and_then(Value::as_str).map(str::to_string),
                );
            }
        } else {
            let mut st = con
                .prepare("select frame_id, reference_frame_id, transform_json from registration_results where frames_set_id = ?1")
                .unwrap_or_else(|e| die(format!("prepare registration: {e}")));
            let rows = st
                .query_map([set.unwrap_or(-1)], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<String>>(2)?)))
                .unwrap_or_else(|e| die(format!("query registration: {e}")));
            for row in rows {
                let (k, reference, json) = row.unwrap_or_else(|e| die(format!("registration row: {e}")));
                parse(k, reference, json);
            }
        }
        CloneDb { by_file, frames, regs }
    }

    // ---------------------------------------------------------------------------------------------- statistics

    /// Ranks of `w`, 1 = the largest, ties averaged; a non-finite weight ranks as 0.
    fn rank_desc(w: &[f64]) -> Vec<f64> {
        let v: Vec<f64> = w.iter().map(|x| if x.is_finite() { *x } else { 0.0 }).collect();
        let mut order: Vec<usize> = (0..v.len()).collect();
        order.sort_by(|&a, &b| v[b].total_cmp(&v[a]));
        let mut r = vec![0.0; v.len()];
        let mut i = 0;
        while i < order.len() {
            let mut j = i;
            while j + 1 < order.len() && v[order[j + 1]] == v[order[i]] {
                j += 1;
            }
            let avg = (i + j) as f64 / 2.0 + 1.0;
            for &o in &order[i..=j] {
                r[o] = avg;
            }
            i = j + 1;
        }
        r
    }

    /// Spearman rho of two weight vectors.
    fn spearman(a: &[f64], b: &[f64]) -> f64 {
        let (ra, rb) = (rank_desc(a), rank_desc(b));
        let n = ra.len() as f64;
        let (ma, mb) = (ra.iter().sum::<f64>() / n, rb.iter().sum::<f64>() / n);
        let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
        for (x, y) in ra.iter().zip(&rb) {
            sab += (x - ma) * (y - mb);
            saa += (x - ma).powi(2);
            sbb += (y - mb).powi(2);
        }
        if saa <= 0.0 || sbb <= 0.0 {
            f64::NAN
        } else {
            sab / (saa * sbb).sqrt()
        }
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.retain(|x| x.is_finite());
        if v.is_empty() {
            return f64::NAN;
        }
        v.sort_by(f64::total_cmp);
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        }
    }

    /// The sharpest decile's ranks, ascending: the `round(0.1 n)` (at least 1) members of the lowest mean FWHM, rounded as D0's
    /// `analyse.py` did (ties to even: 105 frames give 10).
    fn decile_ranks(fwhm: &[f64], rank: &[f64]) -> Vec<f64> {
        let n = fwhm.len();
        let k = ((0.1 * n as f64).round_ties_even() as usize).max(1);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| fwhm[a].total_cmp(&fwhm[b]).then(a.cmp(&b)));
        let mut r: Vec<f64> = order[..k.min(n)].iter().map(|&i| rank[i]).collect();
        r.sort_by(f64::total_cmp);
        r
    }

    fn worst(v: &[f64]) -> f64 {
        v.iter().cloned().fold(f64::NAN, f64::max)
    }

    fn join(v: &[f64]) -> String {
        v.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(",")
    }

    /// The `q`-quantile (0..=1) of the finite values by linear interpolation between order statistics (numpy's default); NaN when
    /// none is finite.
    fn quantile(v: &[f64], q: f64) -> f64 {
        let mut s: Vec<f64> = v.iter().cloned().filter(|x| x.is_finite()).collect();
        if s.is_empty() {
            return f64::NAN;
        }
        s.sort_by(f64::total_cmp);
        let pos = q * (s.len() - 1) as f64;
        let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
        s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
    }

    /// The largest finite value, NaN when none is finite.
    fn max_finite(v: &[f64]) -> f64 {
        v.iter().cloned().filter(|x| x.is_finite()).fold(f64::NAN, f64::max)
    }

    /// Ordinary least squares y = a + b x: `(b, SE of b)`; NaN without three points or without any spread in x.
    fn ols(x: &[f64], y: &[f64]) -> (f64, f64) {
        let n = x.len();
        if n < 3 || n != y.len() {
            return (f64::NAN, f64::NAN);
        }
        let (mx, my) = (x.iter().sum::<f64>() / n as f64, y.iter().sum::<f64>() / n as f64);
        let sxx: f64 = x.iter().map(|v| (v - mx).powi(2)).sum();
        if !(sxx > 0.0) {
            return (f64::NAN, f64::NAN);
        }
        let b = x.iter().zip(y).map(|(vx, vy)| (vx - mx) * (vy - my)).sum::<f64>() / sxx;
        let rss: f64 = x.iter().zip(y).map(|(vx, vy)| (vy - (my + b * (vx - mx))).powi(2)).sum();
        (b, (rss / (n - 2) as f64 / sxx).sqrt())
    }

    /// S1 (PD-16): the drift of the product's frame weight against D0's (b) with seeing, `(slope, SE)` in %/px: the OLS fit of
    /// ln(w_product / w_b) on the frame FWHM over the members with both weights finite and positive, as 100 (e^b - 1) and
    /// 100 SE(b) (`fable-d1a/t1_drift.py`).
    fn drift(fwhm: &[f64], w_prod: &[f64], w_b: &[f64]) -> (f64, f64) {
        let ok = |i: usize| fwhm[i].is_finite() && w_prod[i].is_finite() && w_prod[i] > 0.0 && w_b[i].is_finite() && w_b[i] > 0.0;
        let keep: Vec<usize> = (0..fwhm.len()).filter(|&i| ok(i)).collect();
        let x: Vec<f64> = keep.iter().map(|&i| fwhm[i]).collect();
        let y: Vec<f64> = keep.iter().map(|&i| (w_prod[i] / w_b[i]).ln()).collect();
        let (b, se) = ols(&x, &y);
        (100.0 * (b.exp() - 1.0), 100.0 * se)
    }

    // ---------------------------------------------------------------------------------------------- the product

    #[derive(Clone)]
    struct Member {
        key: i64,
        reference: Option<i64>,
        map: PixelMap,
        /// Mean of the planes' FWHM.
        fwhm: f64,
        unsaturated: usize,
        planes: Vec<DumpPlaneRef>,
    }

    /// A plane of a member: the dump's numbers, the fits as the member currently holds them (the C10 replay trims them).
    #[derive(Clone)]
    struct DumpPlaneRef {
        fits: Vec<StarFit>,
        ap_fixed: Vec<f64>,
        ap_radius: f64,
        fwhm: f64,
        noise: f64,
        m_star: f64,
        psf_signal_weight: f64,
        c_stand_in: f64,
    }

    impl Member {
        fn plane(&self, p: usize) -> MemberPlane<'_> {
            let d = &self.planes[p];
            MemberPlane { fits: &d.fits, map: &self.map, fwhm_px: d.fwhm, noise: d.noise, c_stand_in: d.c_stand_in }
        }
    }

    /// What a member holds at one of the anchor's candidates under the module's match rule.
    enum Match<'a> {
        /// No fit within the radius.
        None,
        /// Two or more fits within the radius: the candidate is unmatched in this member.
        Ambiguous,
        /// Exactly one fit, whatever its flags (`common_set::match_member` then also asks it to be usable).
        One(&'a StarFit),
    }

    /// One member's plane as a tree over its fits' positions in group coordinates: the ONE place the probe redoes
    /// `common_set::match_member`'s search, on the module's own constants, for what `Usable` does not say: WHICH fit paired (S2 reads
    /// its signal and SNR) and why a candidate went unmatched (`lost_breakdown`).
    struct MemberIndex<'a> {
        tree: KdTree2,
        /// Tree point -> index into `fits`.
        fit_of_point: Vec<usize>,
        fits: &'a [StarFit],
        fwhm: f64,
    }

    impl<'a> MemberIndex<'a> {
        /// The module's own point list: a fit with a finite position whose mapped position is finite too.
        fn new(m: &'a Member, plane: usize) -> Self {
            let d = &m.planes[plane];
            let mut fit_of_point = Vec::with_capacity(d.fits.len());
            let mut pts = Vec::with_capacity(d.fits.len());
            for (i, f) in d.fits.iter().enumerate() {
                if !(f.x.is_finite() && f.y.is_finite()) {
                    continue;
                }
                let (gx, gy) = m.map.forward_exact(f.x, f.y);
                if gx.is_finite() && gy.is_finite() {
                    fit_of_point.push(i);
                    pts.push((gx, gy));
                }
            }
            MemberIndex { tree: KdTree2::build(&pts), fit_of_point, fits: &d.fits, fwhm: d.fwhm }
        }

        /// The fits within max(`POINT_SOURCE_MATCH_PX`, `POINT_SOURCE_MATCH_PER_FIELD_FWHM` x max(F_anchor, F_member)) of `c`.
        fn at(&self, c: &Candidate) -> Match<'a> {
            let radius = POINT_SOURCE_MATCH_PX.max(POINT_SOURCE_MATCH_PER_FIELD_FWHM * c.anchor_fwhm_px.max(self.fwhm));
            let hits = self.tree.within(c.x, c.y, radius);
            match hits.len() {
                0 => Match::None,
                1 => Match::One(&self.fits[self.fit_of_point[hits[0].0]]),
                _ => Match::Ambiguous,
            }
        }
    }

    /// What the product made of one plane.
    struct PlaneResult {
        /// Per member, in member order; the dump's Measure-time weight (times the stand-in `c` squared, as `take_group_c`) where the
        /// plane has no common set.
        weights: Vec<f64>,
        cands: Vec<Candidate>,
        /// The set's stars in group coordinates (empty without a set).
        positions: Vec<(f64, f64)>,
        /// The set's stars as indices into `cands` (empty without a set).
        set_idx: Vec<usize>,
        /// The module's own match table: `usable[k][j]` is member k's fit at candidate j when the module may read its flux.
        usable: Vec<Vec<Option<Usable>>>,
        too_few: Option<usize>,
        paired: Vec<usize>,
    }

    struct Product {
        anchor: usize,
        planes: Vec<PlaneResult>,
    }

    /// The product pipeline over a group's members; `None` when no anchor can be picked. `proxy_floor` above the module's
    /// own 20 keeps only the candidates whose proxy SNR reaches it (the lab's what-if for "a higher proxy floor"; the module's
    /// floor is not a parameter).
    fn product(members: &[Member], planes: usize, proxy_floor: f64) -> Option<Product> {
        let fwhm: Vec<f64> = members.iter().map(|m| m.fwhm).collect();
        let unsat: Vec<usize> = members.iter().map(|m| m.unsaturated).collect();
        let ids: Vec<i64> = members.iter().map(|m| m.key).collect();
        let anchor = common_set::pick_anchor(&fwhm, &unsat, &ids)?;
        let mut out = Vec::new();
        for p in 0..planes {
            let mut cands = common_set::candidates(&members[anchor].plane(p));
            if proxy_floor > CORRECTION_MIN_SNR {
                let a = &members[anchor];
                let d = &a.planes[p];
                let proxy: HashMap<(u64, u64), f64> = d
                    .fits
                    .iter()
                    .map(|f| {
                        let (gx, gy) = a.map.forward_exact(f.x, f.y);
                        ((gx.to_bits(), gy.to_bits()), f.signal / ADU / (d.noise * f.area.sqrt()))
                    })
                    .collect();
                cands.retain(|c| proxy.get(&(c.x.to_bits(), c.y.to_bits())).map_or(false, |&v| v >= proxy_floor));
            }
            let usable: Vec<Vec<Option<Usable>>> = members.iter().map(|m| common_set::match_member(&cands, &m.plane(p))).collect();
            let (weights, positions, too_few, paired) = match common_set::common_flux(&cands, &usable, anchor) {
                CommonOutcome::Measured { positions, per_member } => (
                    members
                        .iter()
                        .zip(&per_member)
                        .map(|(m, c)| psf_signal_weight(c.tflux, c.tmean_flux, m.planes[p].noise, m.planes[p].m_star))
                        .collect(),
                    positions,
                    None,
                    per_member.iter().map(|c| c.paired).collect(),
                ),
                CommonOutcome::TooFewStars { stars } | CommonOutcome::NoAnchorFlux { stars } => {
                    eprintln!("d0_weight_probe common: plane {p}: {stars} common stars (too few, or the anchor's flux over them not positive), the Measure-time weights stand");
                    // A plane at floor (i) carries no `c` in the dump; the run's `take_group_c` scales its weight by c^2.
                    (
                        members.iter().map(|m| m.planes[p].psf_signal_weight * m.planes[p].c_stand_in.powi(2)).collect(),
                        Vec::new(),
                        Some(stars),
                        vec![0; members.len()],
                    )
                }
                CommonOutcome::MalformedTable => die(format!("plane {p}: the module reports a malformed match table (a bug of this probe)")),
            };
            let cand_of: HashMap<(u64, u64), usize> = cands.iter().enumerate().map(|(j, c)| ((c.x.to_bits(), c.y.to_bits()), j)).collect();
            let set_idx: Vec<usize> = positions
                .iter()
                .map(|q| {
                    *cand_of
                        .get(&(q.0.to_bits(), q.1.to_bits()))
                        .unwrap_or_else(|| die(format!("plane {p}: a set star at ({}, {}) is not one of the candidates", q.0, q.1)))
                })
                .collect();
            out.push(PlaneResult { weights, cands, positions, set_idx, usable, too_few, paired });
        }
        Some(Product { anchor, planes: out })
    }

    /// The frame weight: a mono plane's weight; a colour image's planes each over their maximum, averaged (the run's rule).
    fn frame_weights(per_plane: &[Vec<f64>]) -> Vec<f64> {
        if per_plane.len() == 1 {
            return per_plane[0].clone();
        }
        let n = per_plane[0].len();
        let maxes: Vec<f64> = per_plane.iter().map(|w| w.iter().cloned().filter(|x| x.is_finite()).fold(0.0, f64::max)).collect();
        (0..n)
            .map(|i| per_plane.iter().zip(&maxes).map(|(w, m)| if *m > 0.0 { w[i] / m } else { 0.0 }).sum::<f64>() / per_plane.len() as f64)
            .collect()
    }

    /// S2 (PD-16): per member, the relative noise of its paired flux ratio r_f from the dump's per-star SNR (sigma_i = flux_i / snr_i):
    /// sqrt(sum sigma_i^2) / sum flux_i over the set stars the member pairs, plus the anchor's same term over the same stars, in quadrature.
    /// NaN for a member with no pair (and for every member of a plane without a set). The pairs are the module's own (`pr.usable`), and the
    /// fits come from `MemberIndex`: a disagreement between the two is a bug of this probe and stops it.
    ///
    /// Plan D D2b: `s_based` reads the aperture sum `S` as the flux (then `snr` is `S / sigma_S` by construction: `star_snr`'s own noise) and
    /// pairs the stars whose `S` is usable, the pairs the S-based ratio is taken over; otherwise the model flux `signal` and every
    /// signal-usable pair (D1w's r_f, whose noise the D1a bars first judged).
    fn rf_noise(idx: &[MemberIndex<'_>], anchor: usize, pr: &PlaneResult, s_based: bool) -> Vec<f64> {
        let flux = |f: &StarFit| if s_based { f.ap_sum } else { f.signal };
        // Plan D D2c (PD-30): the S-based sigma_i is the dump's `apNoise` (the fit's `hfr` slot here): `star_snr`'s sigma of the 1.5 F sum,
        // S_1.5 / snr, scaled by sqrt(n_3F / n_1.5F) = 3 ring F / max(1.5 F, 3) for the 3 F sum. The model-flux variant keeps signal / snr.
        let sigma = |f: &StarFit| if s_based { f.hfr } else { f.signal / f.snr };
        (0..idx.len())
            .map(|k| {
                let (mut sf, mut vf, mut sa, mut va, mut pairs) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0usize);
                for &j in &pr.set_idx {
                    let Some(u) = pr.usable[k][j] else { continue };
                    if s_based && !u.ap_sum.is_finite() {
                        continue;
                    }
                    pairs += 1;
                    let cand = &pr.cands[j];
                    let (Match::One(f), Match::One(g)) = (idx[k].at(cand), idx[anchor].at(cand)) else {
                        die(format!("member {k}, candidate {j}: the module pairs a fit that the probe's re-match does not find"));
                    };
                    let good = |snr: f64| snr.is_finite() && snr > 0.0;
                    if !(good(f.snr) && good(g.snr)) || !(sigma(f).is_finite() && sigma(g).is_finite()) {
                        continue;
                    }
                    sf += flux(f);
                    vf += sigma(f).powi(2);
                    sa += flux(g);
                    va += sigma(g).powi(2);
                }
                if s_based && pairs != pr.paired[k] {
                    die(format!("member {k}: {pairs} pairs in the re-match, {} in the module", pr.paired[k]));
                }
                if sf > 0.0 && sa > 0.0 {
                    (vf / (sf * sf) + va / (sa * sa)).sqrt()
                } else {
                    f64::NAN
                }
            })
            .collect()
    }

    /// Plan D D2b: per member, `[r_s, r_model, q_s, q_model]` - the member's flux over the anchor's on the set stars it pairs: the aperture
    /// sums `S` (flux and mean flux `S / area`, the pairs whose `S` is usable: the module's weight) and the model flux times `c` (D1w's
    /// ratios, every pair whose model flux is usable). NaN for a member with no such pair and for every member of a plane without a set.
    fn paired_ratios(pr: &PlaneResult) -> Vec<[f64; 4]> {
        let ratio = |n: f64, d: f64| if d > 0.0 && n.is_finite() { n / d } else { f64::NAN };
        pr.usable
            .iter()
            .map(|u| {
                if pr.set_idx.is_empty() {
                    return [f64::NAN; 4];
                }
                let (mut s, mut sa, mut qs, mut qsa) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                let (mut m, mut ma, mut qm, mut qma) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for &j in &pr.set_idx {
                    let Some(f) = u[j] else { continue };
                    let c = &pr.cands[j];
                    m += f.signal;
                    ma += c.signal;
                    qm += f.mean;
                    qma += c.mean;
                    if f.ap_sum.is_finite() {
                        s += f.ap_sum;
                        sa += c.ap_sum;
                        qs += f.ap_mean;
                        qsa += c.ap_mean;
                    }
                }
                [ratio(s, sa), ratio(m, ma), ratio(qs, qsa), ratio(qm, qma)]
            })
            .collect()
    }

    /// The plane's weights under D1w's model x `c` ratios (the weight before D2b): the anchor's whole-set sums times each member's model
    /// ratios, through the same `psf_signal_weight`; a member with no pair reads 0 flux as the module's does. Without a set, the product's
    /// own (Measure-time) weights.
    fn model_weights(members: &[Member], p: usize, pr: &PlaneResult, anchor: usize, ratios: &[[f64; 4]]) -> Vec<f64> {
        if pr.set_idx.is_empty() {
            return pr.weights.clone();
        }
        let asig: f64 = pr.set_idx.iter().map(|&j| pr.cands[j].signal).sum();
        let amean: f64 = pr.set_idx.iter().map(|&j| pr.cands[j].mean).sum();
        members
            .iter()
            .enumerate()
            .map(|(k, m)| {
                let (t, q) = if k == anchor {
                    (asig, amean)
                } else if ratios[k][1].is_finite() && ratios[k][3].is_finite() {
                    (ratios[k][1] * asig, ratios[k][3] * amean)
                } else {
                    (0.0, 0.0)
                };
                psf_signal_weight(t, q, m.planes[p].noise, m.planes[p].m_star)
            })
            .collect()
    }

    // ---------------------------------------------------------------------------------------------- D0's (b)

    struct BResult {
        weights: Vec<f64>,
        weights_ap: Vec<f64>,
        /// The set in the reference grid.
        positions: Vec<(f64, f64)>,
        reference: usize,
    }

    /// D0's (b) match tolerance on `plane`: max(`B_MATCH_PX`, `B_MATCH_PER_FWHM` x the median field FWHM of the members).
    fn b_tolerance(members: &[Member], plane: usize) -> f64 {
        B_MATCH_PX.max(B_MATCH_PER_FWHM * median(members.iter().map(|m| m.planes[plane].fwhm).collect()))
    }

    /// D0's offline set (b) and (b-ap) on `plane`, as `analyse.py` builds them, over `members`.
    fn d0_b(members: &[Member], plane: usize) -> BResult {
        let n = members.len();
        let mut votes: HashMap<i64, usize> = HashMap::new();
        for m in members {
            if let Some(r) = m.reference {
                *votes.entry(r).or_default() += 1;
            }
        }
        let ref_key = votes.iter().max_by_key(|(k, v)| (**v, -**k)).map(|(k, _)| *k);
        let reference = ref_key
            .and_then(|k| members.iter().position(|m| m.key == k))
            .unwrap_or_else(|| (0..n).max_by_key(|&i| members[i].planes[plane].fits.len()).unwrap_or(0));
        let r = &members[reference].planes[plane];
        let rmap = &members[reference].map;
        let rpos: Vec<(f64, f64)> = r.fits.iter().map(|f| rmap.forward_exact(f.x, f.y)).collect();
        let pix: Vec<(f64, f64)> = r.fits.iter().map(|f| (f.x, f.y)).collect();
        let tree_pix = KdTree2::build(&pix);
        let iso: Vec<bool> = pix.iter().map(|&(x, y)| tree_pix.within(x, y, 2.0 * r.ap_radius).len() == 1).collect();
        let cand: Vec<usize> = (0..r.fits.len())
            .filter(|&i| r.fits[i].flags.bits() & SAT == 0 && r.fits[i].snr >= B_MIN_SNR && iso[i] && r.ap_fixed[i].is_finite())
            .collect();
        let tol = b_tolerance(members, plane);
        // matched[k][c] = member k's fit nearest to candidate c within tol (any flag).
        let mut matched: Vec<Vec<Option<usize>>> = Vec::with_capacity(n);
        for m in members {
            let d = &m.planes[plane];
            let mut idx = Vec::new();
            let mut pts = Vec::new();
            for (i, f) in d.fits.iter().enumerate() {
                let (gx, gy) = m.map.forward_exact(f.x, f.y);
                if gx.is_finite() && gy.is_finite() {
                    idx.push(i);
                    pts.push((gx, gy));
                }
            }
            let tree = KdTree2::build(&pts);
            matched.push(cand.iter().map(|&i| tree.nearest_within(rpos[i].0, rpos[i].1, tol).map(|(j, _)| idx[j])).collect());
        }
        let common: Vec<usize> = (0..cand.len())
            .filter(|&c| {
                let present = (0..n)
                    .filter(|&k| matched[k][c].map_or(false, |j| members[k].planes[plane].fits[j].flags.bits() & SAT == 0 && members[k].planes[plane].ap_fixed[j].is_finite()))
                    .count();
                present as f64 / n as f64 >= 0.9
            })
            .collect();
        let sig = |f: &StarFit| f.signal / ADU;
        let mean = |f: &StarFit| f.signal / f.area / ADU;
        let com_ref_sig: f64 = common.iter().map(|&c| sig(&r.fits[cand[c]])).filter(|x| x.is_finite()).sum();
        let com_ref_mean: f64 = common.iter().map(|&c| mean(&r.fits[cand[c]])).filter(|x| x.is_finite()).sum();
        let (mut w, mut wap) = (Vec::new(), Vec::new());
        for (k, m) in members.iter().enumerate() {
            let d = &m.planes[plane];
            // Ratios are over the stars matched in this member with both values finite.
            let pair = |col: usize| -> f64 {
                let (mut num, mut den) = (0.0, 0.0);
                for &c in &common {
                    let Some(j) = matched[k][c] else { continue };
                    let rf = &r.fits[cand[c]];
                    let (vf, vr) = match col {
                        0 => (sig(&d.fits[j]), sig(rf)),
                        1 => (mean(&d.fits[j]), mean(rf)),
                        _ => (d.ap_fixed[j], r.ap_fixed[cand[c]]),
                    };
                    if vf.is_finite() && vr.is_finite() {
                        num += vf;
                        den += vr;
                    }
                }
                if den > 0.0 {
                    num / den
                } else {
                    f64::NAN
                }
            };
            let (r_sig, r_mean, r_ap) = (pair(0), pair(1), pair(2));
            w.push(psf_signal_weight(r_sig * com_ref_sig, r_mean * com_ref_mean, d.noise, d.m_star));
            wap.push(psf_signal_weight(r_ap * com_ref_sig, r_mean * r_ap / r_sig * com_ref_mean, d.noise, d.m_star));
        }
        BResult { weights: w, weights_ap: wap, positions: common.iter().map(|&c| rpos[cand[c]]).collect(), reference }
    }

    // ---------------------------------------------------------------------------------------------- the C10 r1 replay

    /// D0's replay of the C10 r1 Blend refusals on one frame: the brightest saturated fits go until the removed signal is
    /// `1 - ratio` of the frame's total.
    fn trim_c10(fits: &[StarFit], ratio: f64) -> Vec<StarFit> {
        let total: f64 = fits.iter().map(|f| f.signal).filter(|x| x.is_finite()).sum();
        let goal = (1.0 - ratio) * total;
        let mut order: Vec<usize> = (0..fits.len()).filter(|&i| fits[i].flags.bits() & SAT != 0).collect();
        order.sort_by(|&a, &b| fits[b].signal.total_cmp(&fits[a].signal));
        let mut drop = vec![false; fits.len()];
        let mut removed = 0.0;
        for i in order {
            if removed >= goal {
                break;
            }
            removed += fits[i].signal;
            drop[i] = true;
        }
        fits.iter().zip(&drop).filter(|(_, d)| !**d).map(|(f, _)| *f).collect()
    }

    /// Why the product's set lacks the stars of (b)'s: the first reason that fits, in the order the product decides them
    /// (a star near no anchor fit; a clipped, an extended, a crowded (another anchor fit within `CORRECTION_ISOLATION_PER_FIELD_FWHM`
    /// F) or a faint (proxy SNR below `CORRECTION_MIN_SNR`) anchor fit; a candidate the presence rule dropped). `idx` is every member's
    /// `MemberIndex` on `plane`, in member order.
    fn lost_breakdown(idx: &[MemberIndex<'_>], anchor: &Member, plane: usize, pr: &PlaneResult, b_positions: &[(f64, f64)], tol: f64) -> String {
        let d = &anchor.planes[plane];
        let apos: Vec<(f64, f64)> = d.fits.iter().map(|f| anchor.map.forward_exact(f.x, f.y)).collect();
        let atree = KdTree2::build(&apos);
        let set_tree = KdTree2::build(&pr.positions);
        let cand_tree = KdTree2::build(&pr.cands.iter().map(|c| (c.x, c.y)).collect::<Vec<_>>());
        let pix: Vec<(f64, f64)> = d.fits.iter().map(|f| (f.x, f.y)).collect();
        let pix_tree = KdTree2::build(&pix);
        let (mut kept, mut presence, mut no_fit, mut clipped, mut extended, mut crowded, mut faint) = (0, 0, 0, 0, 0, 0, 0);
        // For the stars the presence rule dropped: why each member could not use them (member-star pairs).
        let (mut none, mut ambiguous, mut sat, mut ext) = (0usize, 0usize, 0usize, 0usize);
        for q in b_positions {
            if set_tree.nearest_within(q.0, q.1, tol).is_some() {
                kept += 1;
            } else if let Some((ci, _)) = cand_tree.nearest_within(q.0, q.1, tol) {
                presence += 1;
                let c = &pr.cands[ci];
                for t in idx {
                    match t.at(c) {
                        Match::None => none += 1,
                        Match::One(f) => {
                            if f.flags.saturated() {
                                sat += 1;
                            } else if !f.flags.in_common_set() {
                                ext += 1;
                            }
                        }
                        Match::Ambiguous => ambiguous += 1,
                    }
                }
            } else if let Some((i, _)) = atree.nearest_within(q.0, q.1, tol) {
                let f = &d.fits[i];
                if f.flags.saturated() {
                    clipped += 1;
                } else if !f.flags.in_common_set() {
                    extended += 1;
                } else if pix_tree.within(f.x, f.y, CORRECTION_ISOLATION_PER_FIELD_FWHM * d.fwhm).len() != 1 {
                    crowded += 1;
                } else {
                    faint += 1;
                }
            } else {
                no_fit += 1;
            }
        }
        format!("kept {kept} | dropped by the 90 % presence rule {presence} (member-star pairs unusable: no fit {none}, ambiguous {ambiguous}, clipped {sat}, extended {ext}) | no anchor fit {no_fit} | clipped in the anchor {clipped} | extended in the anchor {extended} | crowded {crowded} | faint {faint}")
    }

    // ---------------------------------------------------------------------------------------------- one group

    fn build_members(ds: &str, group: &str, planes: &BTreeMap<usize, BTreeMap<String, DumpPlane>>, clone: &CloneDb) -> (Vec<Member>, usize) {
        let n_planes = planes.keys().max().map_or(0, |m| m + 1);
        if n_planes == 0 || (0..n_planes).any(|p| !planes.contains_key(&p)) {
            die(format!("{ds} {group}: planes {:?} are not contiguous from 0", planes.keys().collect::<Vec<_>>()));
        }
        let mut skipped = (0usize, 0usize, 0usize); // not in the clone, not included, no map
        let mut members = Vec::new();
        for (file, p0) in &planes[&0] {
            let Some(&ci) = clone.by_file.get(&(group.to_string(), p0.base.clone())) else {
                skipped.0 += 1;
                continue;
            };
            let cf = &clone.frames[ci];
            if !cf.included {
                skipped.1 += 1;
                continue;
            }
            let reg = clone.regs.get(&cf.key);
            let Some(map) = reg.and_then(|r| r.map.clone()) else {
                skipped.2 += 1;
                continue;
            };
            let Some(all): Option<Vec<&DumpPlane>> = (0..n_planes).map(|p| planes[&p].get(file)).collect() else {
                skipped.0 += 1;
                continue;
            };
            let refs: Vec<DumpPlaneRef> = all
                .iter()
                .enumerate()
                .map(|(p, d)| DumpPlaneRef {
                    fits: d.fits.clone(),
                    ap_fixed: d.ap_fixed.clone(),
                    ap_radius: d.ap_radius,
                    fwhm: d.fwhm,
                    noise: d.noise,
                    m_star: d.m_star,
                    psf_signal_weight: d.psf_signal_weight,
                    c_stand_in: cf.c_group.get(p).copied().flatten().filter(|c| c.is_finite() && *c > 0.0).unwrap_or(1.0),
                })
                .collect();
            let fwhm = refs.iter().map(|d| d.fwhm).sum::<f64>() / refs.len() as f64;
            let unsaturated = all.iter().map(|d| d.unsaturated).sum();
            members.push(Member { key: cf.key, reference: reg.and_then(|r| r.reference), map, fwhm, unsaturated, planes: refs });
        }
        eprintln!(
            "d0_weight_probe common: {ds} {group}: {} members ({} dump rows not in the clone run or a plane short, {} not included, {} without a map)",
            members.len(),
            skipped.0,
            skipped.1,
            skipped.2
        );
        members.sort_by_key(|m| m.key);
        (members, n_planes)
    }

    fn analyse_group(ds: &str, group: &str, planes: &BTreeMap<usize, BTreeMap<String, DumpPlane>>, clone: &CloneDb, out_dir: Option<&Path>, proxy_floor: f64) {
        let (members, n_planes) = build_members(ds, group, planes, clone);
        if members.is_empty() {
            die(format!("{ds} {group}: no member (included, mapped, every plane in the dump)"));
        }
        let dump_beta = planes[&0].values().next().map_or(f64::NAN, |p| p.beta);
        let clone_beta = planes[&0]
            .values()
            .find_map(|p| clone.by_file.get(&(group.to_string(), p.base.clone())).and_then(|&i| clone.frames[i].group_beta));
        let override_beta = clone_beta.map_or(false, |b| (b - dump_beta).abs() > 1e-6 * b.abs().max(1.0));
        let Some(prod) = product(&members, n_planes, proxy_floor) else {
            die(format!("{ds} {group}: no anchor (no member with a finite FWHM at or above the median star count)"))
        };
        let anchor_key = members[prod.anchor].key;
        let frame_fwhm: Vec<f64> = members.iter().map(|m| m.fwhm).collect();
        let w_prod = frame_weights(&prod.planes.iter().map(|p| p.weights.clone()).collect::<Vec<_>>());
        let rank_prod = rank_desc(&w_prod);
        if override_beta {
            println!(
                "plane-info {ds} {group} beta override: dump beta {dump_beta:.5} vs the clone's group beta {:.5}; judged by the beta307 line only",
                clone_beta.unwrap_or(f64::NAN)
            );
            println!("D1A beta307 {ds} {group} n={} dec_ranks={}", members.len(), join(&decile_ranks(&frame_fwhm, &rank_prod)));
            for (p, pr) in prod.planes.iter().enumerate() {
                println!("plane-info {ds} {group} p{p} anchor={anchor_key} candidates={} set={} too_few={:?}", pr.cands.len(), pr.positions.len(), pr.too_few);
            }
            return;
        }
        // Every member's fits as a match index, per plane: S2's pairs and `lost_breakdown` read the same one.
        let indices: Vec<Vec<MemberIndex<'_>>> = (0..n_planes).map(|p| members.iter().map(|m| MemberIndex::new(m, p)).collect()).collect();
        let noise: Vec<Vec<f64>> = prod.planes.iter().enumerate().map(|(p, pr)| rf_noise(&indices[p], prod.anchor, pr, true)).collect();
        let noise_model: Vec<Vec<f64>> = prod.planes.iter().enumerate().map(|(p, pr)| rf_noise(&indices[p], prod.anchor, pr, false)).collect();
        // Plan D D2b: the S-based ratios beside D1w's model x c ones, and the weight those give.
        let ratios: Vec<Vec<[f64; 4]>> = prod.planes.iter().map(paired_ratios).collect();
        let w_model = frame_weights(
            &prod.planes.iter().enumerate().map(|(p, pr)| model_weights(&members, p, pr, prod.anchor, &ratios[p])).collect::<Vec<_>>(),
        );
        let rank_model = rank_desc(&w_model);
        // D0's (b) side by side, plane by plane.
        let bs: Vec<BResult> = (0..n_planes).map(|p| d0_b(&members, p)).collect();
        let w_b = frame_weights(&bs.iter().map(|b| b.weights.clone()).collect::<Vec<_>>());
        let w_bap = frame_weights(&bs.iter().map(|b| b.weights_ap.clone()).collect::<Vec<_>>());
        let (rank_b, rank_bap) = (rank_desc(&w_b), rank_desc(&w_bap));
        let rho = spearman(&w_prod, &w_b);
        let (dp, db, dap) = (decile_ranks(&frame_fwhm, &rank_prod), decile_ranks(&frame_fwhm, &rank_b), decile_ranks(&frame_fwhm, &rank_bap));
        // The C10 r1 replay (hc11 L only): trim the eight frames' fits as D0 did, run the product again. `replay_moves=NA` says it
        // could not run (a C10 frame is not a member, or the trimmed group has no anchor): the judge reads NA as a MISS.
        let mut replay: Option<String> = None;
        let c10_missing: Vec<i64> = C10_TARGET.iter().map(|(k, _)| *k).filter(|k| !members.iter().any(|m| m.key == *k)).collect();
        if ds == C10_DS && group == C10_GROUP && !c10_missing.is_empty() {
            eprintln!("d0_weight_probe common: {ds} {group}: the C10 r1 replay cannot run, frames {c10_missing:?} are not members");
            replay = Some("NA".to_string());
        } else if ds == C10_DS && group == C10_GROUP {
            let mut trimmed: Vec<Member> = members.clone();
            for (k, ratio) in C10_TARGET {
                if let Some(m) = trimmed.iter_mut().find(|m| m.key == k) {
                    for d in &mut m.planes {
                        d.fits = trim_c10(&d.fits, ratio);
                    }
                }
            }
            if let Some(after) = product(&trimmed, n_planes, proxy_floor) {
                let w_after = frame_weights(&after.planes.iter().map(|p| p.weights.clone()).collect::<Vec<_>>());
                let rank_after = rank_desc(&w_after);
                let moved: Vec<String> = members
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| rank_after[*i] != rank_prod[*i])
                    .map(|(i, m)| format!("{}:{}->{}", m.key, rank_prod[i], rank_after[i]))
                    .collect();
                println!(
                    "replay {ds} {group} C10 r1: anchor {} -> {}; rho(before, after) {:.6}; frames whose rank moved {} [{}]; the eight: {}",
                    anchor_key,
                    trimmed[after.anchor].key,
                    spearman(&w_prod, &w_after),
                    moved.len(),
                    moved.join(" "),
                    C10_TARGET
                        .iter()
                        .map(|(k, _)| {
                            let i = members.iter().position(|m| m.key == *k).unwrap_or(0);
                            format!("{k}:{}->{}", rank_prod[i], rank_after[i])
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                replay = Some(moved.len().to_string());
            } else {
                eprintln!("d0_weight_probe common: {ds} {group}: the C10 r1 replay cannot run, the trimmed group has no anchor");
                replay = Some("NA".to_string());
            }
        }
        let mut line = format!(
            "D1A group {ds} {group} n={} rho={rho:.4} dec_med={} dec_worst={} b_dec_med={} b_dec_worst={} bap_dec_worst={}",
            members.len(),
            median(dp.clone()),
            worst(&dp),
            median(db.clone()),
            worst(&db),
            worst(&dap)
        );
        if let Some(m) = replay {
            line.push_str(&format!(" replay_moves={m}"));
        }
        println!("{line}");
        let (slope, se) = drift(&frame_fwhm, &w_prod, &w_b);
        println!("D1A drift {ds} {group} slope_pct_per_px={slope:.3} se={se:.3}");
        let (slope_m, se_m) = drift(&frame_fwhm, &w_model, &w_b);
        println!(
            "model-weight {ds} {group} rho(w_product, w_model)={:.6} rho(w_model, w_b)={:.4} drift_model_vs_b_slope_pct_per_px={slope_m:.3} se={se_m:.3}",
            spearman(&w_prod, &w_model),
            spearman(&w_model, &w_b)
        );
        // Per plane: the set against (b)'s.
        let amap = &members[prod.anchor].map;
        for (p, pr) in prod.planes.iter().enumerate() {
            let b = &bs[p];
            let tol = b_tolerance(&members, p);
            let tree = KdTree2::build(&pr.positions);
            let recovered = if b.positions.is_empty() {
                f64::NAN
            } else {
                b.positions.iter().filter(|q| tree.nearest_within(q.0, q.1, tol).is_some()).count() as f64 / b.positions.len() as f64
            };
            let by_pos: HashMap<(u64, u64), usize> = members[prod.anchor].planes[p]
                .fits
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let (gx, gy) = amap.forward_exact(f.x, f.y);
                    ((gx.to_bits(), gy.to_bits()), i)
                })
                .collect();
            let proxy_only = pr
                .cands
                .iter()
                .filter(|c| by_pos.get(&(c.x.to_bits(), c.y.to_bits())).map_or(false, |&i| !(members[prod.anchor].planes[p].fits[i].snr >= B_MIN_SNR)))
                .count();
            let share = if pr.cands.is_empty() { f64::NAN } else { proxy_only as f64 / pr.cands.len() as f64 };
            println!(
                "D1A plane {ds} {group} p{p} set={} recovered={recovered:.4} proxy_only={share:.4} rf_noise_med={:.6} rf_noise_p90={:.6} rf_noise_max={:.6}",
                pr.positions.len(),
                quantile(&noise[p], 0.5),
                quantile(&noise[p], 0.9),
                max_finite(&noise[p])
            );
            println!(
                "plane-info {ds} {group} p{p} rf_noise_model_med={:.6} rf_noise_model_p90={:.6} (D1w's r_f on the model flux, beside the S-based rf_noise_* of the D1A line)",
                quantile(&noise_model[p], 0.5),
                quantile(&noise_model[p], 0.9)
            );
            // Plan D D2c (PD-30): of the set's member-star pairs (the module's own, a finite S) the share whose S is at or under the sky
            // (noise, summed), and how many set stars are rescued extended fits in the anchor (SIBLING_POINT).
            let (mut pairs, mut le0) = (0usize, 0usize);
            for u in &pr.usable {
                for &j in &pr.set_idx {
                    if let Some(f) = u[j].filter(|f| f.ap_sum.is_finite()) {
                        pairs += 1;
                        le0 += usize::from(f.ap_sum <= 0.0);
                    }
                }
            }
            let s_le0 = if pairs == 0 { f64::NAN } else { le0 as f64 / pairs as f64 };
            let set_rescued = pr
                .set_idx
                .iter()
                .filter(|&&j| matches!(indices[p][prod.anchor].at(&pr.cands[j]), Match::One(f) if f.flags.extended()))
                .count();
            println!(
                "plane-info {ds} {group} p{p} anchor={anchor_key} reference={} candidates={} set={} b_set={} proxy_only_stars={proxy_only} too_few={:?} set_rescued={set_rescued} pairs={pairs} s_le0_share={s_le0:.5}",
                members[b.reference].key,
                pr.cands.len(),
                pr.positions.len(),
                b.positions.len(),
                pr.too_few
            );
            println!(
                "plane-info {ds} {group} p{p} (b)'s stars the product's set lacks, by the first reason that fits: {}",
                lost_breakdown(&indices[p], &members[prod.anchor], p, pr, &b.positions, tol)
            );
        }
        // Per frame.
        // The columns after `rf_noise` are Plan D D2b's: per plane (joined by "/") the S-based and the model x c ratios, then the model weight.
        let mut table = String::from(
            "key\tfwhm\tw_product\tw_b\tw_bap\trank_product\trank_b\trank_bap\tpaired\trf_noise\trf_s\trf_model\tqf_s\tqf_model\tw_model\trank_model\n",
        );
        for (i, m) in members.iter().enumerate() {
            let paired: Vec<String> = prod.planes.iter().map(|p| p.paired[i].to_string()).collect();
            let rf: Vec<String> = noise.iter().map(|n| format!("{:.6}", n[i])).collect();
            let col = |c: usize| ratios.iter().map(|r| format!("{:.6}", r[i][c])).collect::<Vec<_>>().join("/");
            table.push_str(&format!(
                "{}\t{:.4}\t{:.6e}\t{:.6e}\t{:.6e}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.6e}\t{}\n",
                m.key,
                m.fwhm,
                w_prod[i],
                w_b[i],
                w_bap[i],
                rank_prod[i],
                rank_b[i],
                rank_bap[i],
                paired.join("/"),
                rf.join("/"),
                col(0),
                col(1),
                col(2),
                col(3),
                w_model[i],
                rank_model[i]
            ));
        }
        for row in table.lines().skip(1) {
            println!("frame {ds} {group} {row}");
        }
        if let Some(dir) = out_dir {
            let path = dir.join(format!("{ds}.{group}.frames.tsv"));
            std::fs::create_dir_all(dir).unwrap_or_else(|e| die(format!("{}: {e}", dir.display())));
            std::fs::write(&path, table).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
        }
    }

    pub fn run(args: &[String]) {
        let (mut dumps, mut clone, mut out_dir, mut proxy_floor) = (None::<PathBuf>, None::<PathBuf>, None::<PathBuf>, CORRECTION_MIN_SNR);
        let mut rescue = true;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut v = || it.next().unwrap_or_else(|| die(format!("{a} needs a value"))).clone();
            match a.as_str() {
                "--dumps" => dumps = Some(PathBuf::from(v())),
                "--clone" => clone = Some(PathBuf::from(v())),
                "--out-dir" => out_dir = Some(PathBuf::from(v())),
                "--proxy-floor" => proxy_floor = v().parse().unwrap_or_else(|_| die("--proxy-floor")),
                "--set-rescue" => {
                    rescue = match v().as_str() {
                        "on" => true,
                        "off" => false,
                        o => die(format!("--set-rescue takes on or off, not {o}")),
                    }
                }
                o => die(format!("unknown argument {o}")),
            }
        }
        let dumps = dumps.unwrap_or_else(|| die("--dumps is required"));
        let clone_dir = clone.unwrap_or_else(|| die("--clone is required"));
        println!("set-rescue {}", if rescue { "on" } else { "off" });
        let dump = load_dump(&dumps, rescue);
        let clone = load_clone(&clone_dir);
        if dump.is_empty() {
            die(format!("{}: no frames in frames.jsonl", dumps.display()));
        }
        for ((ds, group), planes) in &dump {
            analyse_group(ds, group, planes, &clone, out_dir.as_deref(), proxy_floor);
        }
    }
}
