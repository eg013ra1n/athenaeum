//! Dev probe for Checkpoint B: register + measure + weigh + integrate a
//! folder of calibrated `c_*.fits` (or `c_*_d.fits` for `--osc`) frames onto
//! a reference, write the resulting master light, and print the run as
//! JSON; optionally cross-check the two measurement-seed detectors on the
//! first few frames, or compare the written master against an externally
//! produced one.
//!
//! A probe has no `ServiceContext`/DB, so `IoPolicy` cannot go through
//! `integration::io_policy::resolve` (which needs a `Connection` and a
//! `SettingsManager`) — it is built directly here with fixed values
//! (`band_budget_bytes: 1 GiB`, `read_concurrency: 4`, `storage: Local`).
//!
//! When the reference file is not among `<dir>`'s own `--limit`-truncated
//! `c_*.fits` slice (the common case — the reference is usually a later
//! frame than an alphabetically-sorted, size-limited slice reaches) AND its
//! plane count matches the group's, it is prepended as an extra frame ahead
//! of the slice instead of being dropped, so `frameCount`/`frames.length`
//! in the JSON output is `N + 1`, not `N`. When the plane counts differ
//! (e.g. a mono reference over an OSC group), the reference is used for
//! registration only (never prepended) and the group's own best-weighted
//! frame anchors normalization instead — see `referenceInGroup` /
//! `normalizationReference` in the JSON.
//!
//! cargo run --release -p athenaeum-core --example integrate_probe -- \
//!   <reference.fits> <dir-with-c_*.fits> --out <dir> [--limit N] [--osc] \
//!   [--distortion off|auto] [--rejection auto|linearFit|winsorized|sigma] \
//!   [--maps] [--seeds fast|full] [--seeds-report N] \
//!   [--interpolation nearest|bilinear|bicubicSpline|bicubicBSpline|lanczos3|lanczos4|mitchellNetravali] \
//!   [--compare <external-master.xisf>] [--json <path>] \
//!   [--set-name <str>] [--threads N]

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use astroimage::ImageAnalyzer;

use athenaeum_core::fits_parser::FitsHeader;
use athenaeum_core::geometry::PixelMap;
use athenaeum_core::integration::engine::EngineProgress;
use athenaeum_core::integration::io_policy::IoPolicy;
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::integration::stats;
use athenaeum_core::integration::storage_class::StorageClass;
use athenaeum_core::stacking::groups::ColorMode;
use athenaeum_core::stacking::integrate::{
    integrate_group, GroupInput, GroupProgress, GroupStats, IntegrationConfig, NormalizationConfig,
    RejectionChoice, StackFrame,
};
use athenaeum_core::stacking::master_cards::{
    build_master_light_cards, master_file_name, write_master_light, MasterCardInputs,
};
use athenaeum_core::stacking::measure::{
    measure_frame_with_seeds, measure_plane, measure_plane_with_seeds, MeasureOptions, NoiseSource,
    SeedSource, ADU_SCALE,
};
use athenaeum_core::stacking::register::align::model_name;
use athenaeum_core::stacking::register::detect::{detect_stars, luminance, Star};
use athenaeum_core::stacking::register::frame::{
    identity_registration, reference_stars, register_frame, FrameRegistration,
};
use athenaeum_core::stacking::register::writer::source_cards_from_file;
use athenaeum_core::stacking::register::{DetectionConfig, RegistrationConfig};
use athenaeum_core::stacking::weights::{
    best_by_weight, compute_weights, FormulaWeights, WeightInput, WeightMode,
};

fn usage() -> ! {
    eprintln!(
        "usage: integrate_probe <reference.fits> <dir> --out <dir> [--limit N] [--osc] \
[--distortion off|auto] [--rejection auto|linearFit|winsorized|sigma] [--maps] \
[--seeds fast|full] [--seeds-report N] [--interpolation nearest|bilinear|bicubicSpline| \
bicubicBSpline|lanczos3|lanczos4|mitchellNetravali] [--compare master.xisf] [--json path] \
[--set-name str] [--threads N]\n\
(the reference is prepended as an extra frame when --limit's slice of <dir> doesn't already \
contain it AND its plane count matches the group's; otherwise it is used for registration only)"
    );
    std::process::exit(2);
}

struct Args {
    reference: PathBuf,
    dir: PathBuf,
    out: PathBuf,
    limit: Option<usize>,
    osc: bool,
    reg: RegistrationConfig,
    rejection: RejectionChoice,
    maps: bool,
    seeds: SeedSource,
    seeds_report: usize,
    compare: Option<PathBuf>,
    json: Option<PathBuf>,
    set_name: Option<String>,
    threads: Option<usize>,
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let reference = PathBuf::from(it.next().unwrap_or_else(|| usage()));
    let dir = PathBuf::from(it.next().unwrap_or_else(|| usage()));
    let mut a = Args {
        reference,
        dir,
        out: PathBuf::new(),
        limit: None,
        osc: false,
        reg: RegistrationConfig::default(),
        rejection: RejectionChoice::Auto,
        maps: false,
        seeds: SeedSource::Fast,
        seeds_report: 0,
        compare: None,
        json: None,
        set_name: None,
        threads: None,
    };
    let mut out_set = false;
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--osc" => {
                a.osc = true;
                continue;
            }
            "--maps" => {
                a.maps = true;
                continue;
            }
            _ => {}
        }
        let value = it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--out" => {
                a.out = value.into();
                out_set = true;
            }
            "--limit" => a.limit = Some(value.parse().unwrap_or_else(|_| usage())),
            "--distortion" => {
                a.reg.distortion = serde_json::from_value(serde_json::Value::String(value))
                    .unwrap_or_else(|_| usage())
            }
            "--interpolation" => {
                a.reg.interpolation = serde_json::from_value(serde_json::Value::String(value))
                    .unwrap_or_else(|_| usage())
            }
            "--rejection" => {
                a.rejection = match value.as_str() {
                    "auto" => RejectionChoice::Auto,
                    "linearFit" => RejectionChoice::LinearFitClip {
                        sigma_low: 5.0,
                        sigma_high: 3.5,
                    },
                    "winsorized" => RejectionChoice::WinsorizedSigma {
                        sigma_low: 4.0,
                        sigma_high: 3.0,
                    },
                    "sigma" => RejectionChoice::SigmaClip {
                        sigma_low: 4.0,
                        sigma_high: 3.0,
                    },
                    _ => usage(),
                }
            }
            "--seeds" => {
                a.seeds = match value.as_str() {
                    "fast" => SeedSource::Fast,
                    "full" => SeedSource::Full,
                    _ => usage(),
                }
            }
            "--seeds-report" => a.seeds_report = value.parse().unwrap_or_else(|_| usage()),
            "--compare" => a.compare = Some(value.into()),
            "--json" => a.json = Some(value.into()),
            "--set-name" => a.set_name = Some(value),
            "--threads" => a.threads = Some(value.parse().unwrap_or_else(|_| usage())),
            _ => usage(),
        }
    }
    if !out_set {
        usage();
    }
    a
}

/// Same helper as `register_probe.rs`/`measure_probe.rs` — examples do not
/// share code. Collapses every plane to one luminance plane in native units.
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

/// As `read_luminance_any`, but keeps every plane separate (for `--compare`,
/// which needs to line up our own planar master against the external one
/// channel by channel rather than a fused luminance).
fn read_planes_any(path: &Path) -> Result<(Vec<Vec<f32>>, usize, usize), String> {
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
        return Ok((planes, r.width(), r.height()));
    }
    let (meta, pixels) = astroimage::ImageConverter::read_raw(path).map_err(|e| e.to_string())?;
    let data: Vec<f32> = match pixels {
        astroimage::PixelData::Float32(v) => v,
        astroimage::PixelData::Uint16(v) => v.iter().map(|&u| u as f32 / 65535.0).collect(),
    };
    let n = meta.width * meta.height;
    let planes: Vec<Vec<f32>> = (0..meta.channels)
        .map(|c| data[c * n..(c + 1) * n].to_vec())
        .collect();
    Ok((planes, meta.width, meta.height))
}

/// The first 1 MB of the file as lossy UTF-8 — enough to hold a written
/// master's XML header (well under the 100 MB the XISF reader itself
/// tolerates), used only to spot which `<Image id="…">` elements the file
/// lists, not to decode their pixels.
fn read_header_text(path: &Path) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.take(1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// The ` id="…"` attribute (leading space, so it can't match inside a
/// longer attribute name such as `ref_id`) of the first `<Image` element in
/// `text`, searched only within that element's own tag text (up to its
/// closing `>`) so a later, unrelated element's `id` can't be picked up.
fn first_image_id(text: &str) -> Option<String> {
    let img = text.find("<Image")?;
    let tag_end = text[img..].find('>')? + img;
    let tag = &text[img..=tag_end];
    let key = " id=\"";
    let start = tag.find(key)? + key.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

fn median(v: &mut Vec<f64>) -> f64 {
    v.retain(|x| x.is_finite());
    v.sort_by(|a, b| a.total_cmp(b));
    if v.is_empty() {
        f64::NAN
    } else {
        v[v.len() / 2]
    }
}

fn mean_f32(v: &[f32]) -> f64 {
    if v.is_empty() {
        f64::NAN
    } else {
        v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64
    }
}

/// `a / b`, `None` when `b` is not positive or the result isn't finite.
fn ratio(a: f64, b: f64) -> Option<f64> {
    if b > 0.0 {
        let r = a / b;
        r.is_finite().then_some(r)
    } else {
        None
    }
}

/// The file stem without the leading `c_` and a trailing `_d` (the OSC
/// debayer suffix), e.g. `c_2025-09-14_..._0000_d.fits` -> `2025-09-14_...`.
fn frame_stem(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("frame");
    let stem = stem.strip_prefix("c_").unwrap_or(stem);
    let stem = stem.strip_suffix("_d").unwrap_or(stem);
    stem.to_string()
}

/// Wall-clock gate so a progress callback fired from inside the engine
/// prints at most once every `interval`.
struct Throttle {
    last: Mutex<Instant>,
    interval: Duration,
}

impl Throttle {
    fn new(interval: Duration) -> Self {
        Throttle {
            last: Mutex::new(Instant::now() - interval),
            interval,
        }
    }
    fn tick(&self) -> bool {
        let mut last = self.last.lock().expect("throttle mutex poisoned");
        if last.elapsed() >= self.interval {
            *last = Instant::now();
            true
        } else {
            false
        }
    }
}

/// Detector seed positions for `--seeds-report`, independent of
/// `measure_plane_with_seeds` (which reports only counts, not positions) —
/// duplicated here on purpose, same as every other copied probe helper.
fn fast_seed_positions(
    scaled: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: &Arc<rayon::ThreadPool>,
) -> (Vec<(f64, f64)>, u64) {
    let t = Instant::now();
    let analyzer = ImageAnalyzer::new()
        .with_max_stars(opts.max_stars.max(8))
        .with_centroid_refine(false)
        .with_thread_pool(Arc::clone(pool));
    let seeds = match analyzer.detect_fast_data(scaled, w, h, 1) {
        Ok(r) => r
            .stars
            .iter()
            .filter(|s| s.snr >= opts.min_snr && s.peak > 0.0)
            .map(|s| (s.x as f64, s.y as f64))
            .collect(),
        Err(e) => {
            eprintln!("[integrate_probe] fast seed detection failed: {e}");
            Vec::new()
        }
    };
    (seeds, t.elapsed().as_millis() as u64)
}

/// As `fast_seed_positions`, but through the full two-pass detector. Also
/// returns `(stars_detected, stars_measured)` straight off `AnalysisResult`
/// for the seed report's `fullStarsDetected`/`fullStarsMeasured` — raw
/// pipeline counts, ahead of the `min_snr`/`peak` gate this fn applies to
/// the positions it returns. `with_measure_cap(0)` matters: the default cap
/// (500) truncates AFTER measurement, throttling the full arm's fitted-star
/// count (and therefore PSFSW/PSF SNR, both quadratic in it) far below the
/// fast arm's regardless of `with_max_stars`.
fn full_seed_positions(
    scaled: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: &Arc<rayon::ThreadPool>,
) -> (Vec<(f64, f64)>, u64, usize, usize) {
    let t = Instant::now();
    let analyzer = ImageAnalyzer::new()
        .with_max_stars(opts.max_stars)
        .with_measure_cap(0)
        .with_thread_pool(Arc::clone(pool));
    let (seeds, detected, measured) = match analyzer.analyze_data(scaled, w, h, 1) {
        Ok(r) => (
            r.stars
                .iter()
                .filter(|s| s.snr >= opts.min_snr && s.peak > 0.0)
                .map(|s| (s.x as f64, s.y as f64))
                .collect(),
            r.stars_detected,
            r.stars_measured,
        ),
        Err(e) => {
            eprintln!("[integrate_probe] full seed detection failed: {e}");
            (Vec::new(), 0, 0)
        }
    };
    (seeds, t.elapsed().as_millis() as u64, detected, measured)
}

/// Fraction of `a` within 0.5 px of the nearest point in `b`.
fn within_half_px_fraction(a: &[(f64, f64)], b: &[(f64, f64)]) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    let hits = a
        .iter()
        .filter(|&&(x, y)| {
            b.iter()
                .any(|&(bx, by)| ((x - bx).powi(2) + (y - by).powi(2)).sqrt() < 0.5)
        })
        .count();
    hits as f64 / a.len() as f64
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn frame_paths(dir: &Path, osc: bool, limit: Option<usize>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| {
            eprintln!("[integrate_probe] cannot read {}: {e}", dir.display());
            std::process::exit(1);
        })
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with("c_") {
                return false;
            }
            if osc {
                name.ends_with("_d.fits")
            } else {
                name.ends_with(".fits") && !name.ends_with("_d.fits")
            }
        })
        .collect();
    paths.sort();
    if let Some(n) = limit {
        paths.truncate(n);
    }
    paths
}

/// EXPTIME (default 0.0, warned) and DATE-OBS text.
fn frame_meta(path: &Path) -> (f64, Option<String>) {
    match FitsHeader::from_path(path) {
        Ok(h) => {
            let exptime = h.get_f64("EXPTIME").unwrap_or_else(|| {
                eprintln!(
                    "[integrate_probe] {} has no EXPTIME; using 0.0",
                    path.display()
                );
                0.0
            });
            (exptime, h.get_str("DATE-OBS"))
        }
        Err(e) => {
            eprintln!(
                "[integrate_probe] {} header unreadable: {e}; EXPTIME 0.0",
                path.display()
            );
            (0.0, None)
        }
    }
}

fn normalization_string(n: &NormalizationConfig) -> String {
    let output = serde_json::to_value(n.output).unwrap_or_default();
    let rejection = serde_json::to_value(n.rejection).unwrap_or_default();
    format!(
        "{}/{}",
        output.as_str().unwrap_or("?"),
        rejection.as_str().unwrap_or("?")
    )
}

struct ProcessedFrame {
    path: PathBuf,
    map: PixelMap,
    measurement: athenaeum_core::stacking::measure::FrameMeasurement,
    exposure_s: f64,
    date_obs: Option<String>,
    /// Index into `frame_entries` (and `all_paths`) this frame came from.
    entry_idx: usize,
}

/// The JSON shape for a frame that never reached measurement (registration
/// I/O error, alignment failure) or reached alignment only (measurement
/// failure) — `align = Some((rms_px, inliers, pairs, model))` for the
/// latter, `None` for the former. Patched afterwards: `weight` once
/// `compute_weights` runs, `isNormalizationReference` once the group's
/// anchor frame is chosen, `included`/`rejectedFraction` once
/// `integrate_group` returns — none of that exists yet for a frame that
/// stops here, so they stay at their fixed defaults below.
fn frame_entry_shell(
    stem: &str,
    path: &Path,
    is_reference: bool,
    registered: bool,
    align: Option<(f64, usize, usize, String)>,
    exposure_s: f64,
    date_obs: &Option<String>,
) -> serde_json::Value {
    let (rms_px, inliers, pairs, model) = match align {
        Some((r, i, p, m)) => (
            serde_json::json!(r),
            serde_json::json!(i),
            serde_json::json!(p),
            serde_json::json!(m),
        ),
        None => (
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::Null,
        ),
    };
    serde_json::json!({
        "stem": stem,
        "path": path.display().to_string(),
        "isReference": is_reference,
        "isNormalizationReference": false,
        "registered": registered,
        "rmsPx": rms_px,
        "inliers": inliers,
        "pairs": pairs,
        "model": model,
        "psfsw": serde_json::Value::Null,
        "psfSnr": serde_json::Value::Null,
        "fwhmPx": serde_json::Value::Null,
        "eccentricity": serde_json::Value::Null,
        "stars": serde_json::Value::Null,
        "noise": serde_json::Value::Null,
        "location": serde_json::Value::Null,
        "scale": serde_json::Value::Null,
        "weight": serde_json::Value::Null,
        "included": false,
        "rejectedFraction": serde_json::Value::Null,
        "exposureS": exposure_s,
        "dateObs": date_obs,
    })
}

fn main() {
    let program_start = Instant::now();
    let args = parse_args();

    let threads = args.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let pool: Arc<rayon::ThreadPool> = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap_or_else(|e| {
                eprintln!("[integrate_probe] failed to build thread pool: {e}");
                std::process::exit(1);
            }),
    );
    let cancel = AtomicBool::new(false);
    let measure_opts = MeasureOptions::default();

    let reference = reference_stars(&args.reference, &args.reg, Some(&pool)).unwrap_or_else(|e| {
        eprintln!("[integrate_probe] reference: {e}");
        std::process::exit(1);
    });

    let mut candidates = frame_paths(&args.dir, args.osc, args.limit);
    if candidates.is_empty() {
        eprintln!(
            "[integrate_probe] no frames matching the glob found in {}",
            args.dir.display()
        );
        std::process::exit(1);
    }

    // spec §2: a group is registered onto the global reference regardless of
    // its own plane count (registration works on collapsed luminance), but
    // only joins the group's OWN combine as a member when its plane count
    // matches — an OSC (3-plane) group given a mono (1-plane) reference
    // would otherwise fail `integrate_group`'s per-frame channel-count check
    // the instant the reference was prepended as if it were a same-shaped
    // member.
    let reference_channels = match PlaneReader::open(&args.reference) {
        Ok(r) => r.channels(),
        Err(e) => {
            eprintln!("[integrate_probe] cannot read reference geometry: {e}");
            std::process::exit(1);
        }
    };
    let group_channels = match PlaneReader::open(&candidates[0]) {
        Ok(r) => r.channels(),
        Err(e) => {
            eprintln!(
                "[integrate_probe] cannot read group geometry from {}: {e}",
                candidates[0].display()
            );
            std::process::exit(1);
        }
    };
    let reference_in_group = reference_channels == group_channels;
    if reference_in_group {
        eprintln!(
            "[integrate_probe] reference has {reference_channels} plane(s), matching the group's {group_channels} — it is a member of the group and anchors normalization"
        );
    } else {
        eprintln!(
            "[integrate_probe] reference has {reference_channels} plane(s), the group has {group_channels} — used for registration only; the group's own best-weighted frame anchors normalization"
        );
    }

    let reference_present = candidates.iter().any(|p| same_file(p, &args.reference));
    let mut all_paths = Vec::with_capacity(candidates.len() + 1);
    if reference_in_group && !reference_present {
        all_paths.push(args.reference.clone());
    }
    all_paths.append(&mut candidates);
    let total = all_paths.len();

    let mut processed: Vec<ProcessedFrame> = Vec::with_capacity(total);
    let mut frame_entries: Vec<serde_json::Value> = Vec::with_capacity(total);
    let mut failed = 0usize;
    let mut register_ms_total = 0u64;
    let mut measure_ms_total = 0u64;
    let mut seed_report: Vec<serde_json::Value> = Vec::new();

    for (idx, path) in all_paths.iter().enumerate() {
        if (idx + 1) % 10 == 0 || idx + 1 == total {
            eprintln!(
                "[integrate_probe] {}/{} frames processed, {:.1}s elapsed",
                idx + 1,
                total,
                program_start.elapsed().as_secs_f64()
            );
        }

        let is_reference = same_file(path, &args.reference);
        let stem = frame_stem(path);
        let (exposure_s, date_obs) = frame_meta(path);

        let reg_result: Result<FrameRegistration, String> = if is_reference {
            Ok(identity_registration(&reference))
        } else {
            register_frame(&reference, path, &args.reg, Some(&pool), &cancel)
                .map_err(|e| e.to_string())
        };
        let reg = match reg_result {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "[integrate_probe] register I/O error for {}: {e}",
                    path.display()
                );
                failed += 1;
                frame_entries.push(frame_entry_shell(
                    &stem,
                    path,
                    is_reference,
                    false,
                    None,
                    exposure_s,
                    &date_obs,
                ));
                continue;
            }
        };
        register_ms_total += reg.duration_ms;
        let a = match reg.outcome {
            Ok(a) => a,
            Err(e) => {
                eprintln!(
                    "[integrate_probe] alignment failed for {}: {e}",
                    path.display()
                );
                failed += 1;
                frame_entries.push(frame_entry_shell(
                    &stem,
                    path,
                    is_reference,
                    false,
                    None,
                    exposure_s,
                    &date_obs,
                ));
                continue;
            }
        };
        let (rms_px, inliers, pairs) = (a.rms_px, a.inliers, a.pairs);
        let model_str = model_name(a.model, a.distortion_order);
        let map = a.map;

        let measurement =
            match measure_frame_with_seeds(path, &measure_opts, Some(&pool), &cancel, args.seeds) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "[integrate_probe] measure failed for {}: {e}",
                        path.display()
                    );
                    failed += 1;
                    frame_entries.push(frame_entry_shell(
                        &stem,
                        path,
                        is_reference,
                        true,
                        Some((rms_px, inliers, pairs, model_str)),
                        exposure_s,
                        &date_obs,
                    ));
                    continue;
                }
            };
        measure_ms_total += measurement.duration_ms;

        let mut seed_entry: Option<serde_json::Value> = None;
        if seed_report.len() < args.seeds_report {
            match read_luminance_any(path) {
                Ok((lum, w, h)) => {
                    let scaled: Vec<f32> = lum.iter().map(|v| v * ADU_SCALE).collect();
                    let (fast_xy, fast_ms) =
                        fast_seed_positions(&scaled, w, h, &measure_opts, &pool);
                    let (full_xy, full_ms, full_detected, full_measured) =
                        full_seed_positions(&scaled, w, h, &measure_opts, &pool);
                    let fast_m = measure_plane_with_seeds(
                        &lum,
                        w,
                        h,
                        &measure_opts,
                        Some(&pool),
                        SeedSource::Fast,
                    );
                    let full_m = measure_plane_with_seeds(
                        &lum,
                        w,
                        h,
                        &measure_opts,
                        Some(&pool),
                        SeedSource::Full,
                    );
                    let fast_within = within_half_px_fraction(&fast_xy, &full_xy);
                    let full_within = within_half_px_fraction(&full_xy, &fast_xy);
                    eprintln!(
                        "[integrate_probe] seeds-report {}: fast {} stars ({:.0}% within 0.5px), full {} stars ({:.0}% within 0.5px, {} detected / {} measured)",
                        path.display(),
                        fast_xy.len(),
                        fast_within * 100.0,
                        full_xy.len(),
                        full_within * 100.0,
                        full_detected,
                        full_measured
                    );
                    let entry = serde_json::json!({
                        "path": path.display().to_string(),
                        "fastStars": fast_xy.len(),
                        "fullStars": full_xy.len(),
                        "fastWithinHalfPx": fast_within,
                        "fullWithinHalfPx": full_within,
                        "fastPsfsw": fast_m.psf_signal_weight,
                        "fullPsfsw": full_m.psf_signal_weight,
                        "fastPsfSnr": fast_m.psf_snr,
                        "fullPsfSnr": full_m.psf_snr,
                        "fastMs": fast_ms,
                        "fullMs": full_ms,
                        "fullStarsDetected": full_detected,
                        "fullStarsMeasured": full_measured,
                    });
                    seed_report.push(entry.clone());
                    seed_entry = Some(entry);
                }
                Err(e) => eprintln!(
                    "[integrate_probe] seeds-report read failed for {}: {e}",
                    path.display()
                ),
            }
        }

        let ch0 = &measurement.channels[0];
        let psfsw = measurement.mean_psf_signal_weight();
        let psf_snr_mean = measurement.channels.iter().map(|c| c.psf_snr).sum::<f64>()
            / measurement.channels.len().max(1) as f64;
        let stars = measurement.min_stars();

        let mut entry = serde_json::json!({
            "stem": stem,
            "path": path.display().to_string(),
            "isReference": is_reference,
            "isNormalizationReference": false,
            "registered": true,
            "rmsPx": rms_px,
            "inliers": inliers,
            "pairs": pairs,
            "model": model_str,
            "psfsw": psfsw,
            "psfSnr": psf_snr_mean,
            "fwhmPx": ch0.fwhm_px,
            "eccentricity": ch0.eccentricity,
            "stars": stars,
            "noise": ch0.noise,
            "location": ch0.location,
            "scale": ch0.scale,
            "weight": serde_json::Value::Null,
            "included": false,
            "rejectedFraction": serde_json::Value::Null,
            "exposureS": exposure_s,
            "dateObs": date_obs,
        });
        if let Some(se) = seed_entry {
            entry["seedReport"] = se;
        }
        frame_entries.push(entry);

        processed.push(ProcessedFrame {
            path: path.clone(),
            map,
            measurement,
            exposure_s,
            date_obs,
            entry_idx: idx,
        });
    }

    let registered = processed.len();
    if registered == 0 {
        eprintln!("[integrate_probe] no frame registered and measured; nothing to integrate");
        std::process::exit(1);
    }

    let weight_inputs: Vec<WeightInput> = processed
        .iter()
        .map(|f| WeightInput {
            measurement: &f.measurement,
            exposure_s: Some(f.exposure_s),
            keyword_value: None,
        })
        .collect();
    let excluded = vec![false; processed.len()];
    let weights = compute_weights(
        &weight_inputs,
        WeightMode::PsfSignalWeight,
        &FormulaWeights::default(),
        &excluded,
    );
    for (f, w) in processed.iter().zip(weights.iter()) {
        frame_entries[f.entry_idx]["weight"] = serde_json::json!(w.normalized_mean);
    }

    // spec §2: when the reference isn't a group member, the group's own
    // best-weighted frame anchors normalization instead (`best_by_weight`,
    // same helper Plan 2's selection stage uses, over every processed frame
    // — none of them excluded here).
    let reference_index = if reference_in_group {
        processed
            .iter()
            .position(|f| same_file(&f.path, &args.reference))
            .unwrap_or(0)
    } else {
        let included_mask = vec![true; processed.len()];
        let star_counts: Vec<usize> = processed
            .iter()
            .map(|f| f.measurement.min_stars())
            .collect();
        best_by_weight(&weights, &included_mask, &star_counts).unwrap_or(0)
    };
    frame_entries[processed[reference_index].entry_idx]["isNormalizationReference"] =
        serde_json::json!(true);
    let normalization_reference_stem = frame_stem(&processed[reference_index].path);

    let stack_frames: Vec<StackFrame> = processed
        .iter()
        .zip(weights)
        .map(|(f, w)| StackFrame {
            path: f.path.clone(),
            map: f.map.clone(),
            measurement: f.measurement.clone(),
            weight: w,
            exposure_s: f.exposure_s,
            date_obs: f.date_obs.clone(),
        })
        .collect();

    let (width, height) = (reference.width, reference.height);
    let channels = stack_frames[reference_index].measurement.channels.len();

    let integration = IntegrationConfig {
        rejection: args.rejection,
        write_rejection_maps: args.maps,
        ..IntegrationConfig::default()
    };
    let normalization = NormalizationConfig::default();

    let group_input = GroupInput {
        frames: &stack_frames,
        reference: reference_index,
        width,
        height,
        channels,
        interpolation: args.reg.interpolation,
        clamping: args.reg.clamping_threshold,
        integration: &integration,
        normalization: &normalization,
        ln: None,
        rej: None,
    };

    let band_throttle = Throttle::new(Duration::from_secs(2));
    let combine_throttle = Throttle::new(Duration::from_secs(2));
    let on_plane = |p: usize, total: usize| {
        eprintln!("[integrate_probe] plane {}/{}", p + 1, total);
    };
    let on_band = |band: usize, bands: usize, done: u64, total: u64| {
        if band_throttle.tick() {
            eprintln!(
                "[integrate_probe] band {}/{} — {}/{} bytes read",
                band, bands, done, total
            );
        }
    };
    let on_combine = |rows: usize, total_rows: usize, done: u64, total: u64| {
        if combine_throttle.tick() {
            eprintln!(
                "[integrate_probe] combine {}/{} rows — {}/{} bytes",
                rows, total_rows, done, total
            );
        }
    };
    let progress = GroupProgress {
        on_plane: &on_plane,
        engine: EngineProgress {
            on_band: &on_band,
            on_combine: &on_combine,
        },
    };

    let io = IoPolicy {
        band_budget_bytes: 1024 * 1024 * 1024,
        read_concurrency: 4,
        storage: StorageClass::Local,
    };

    let output = match integrate_group(&group_input, &measure_opts, &pool, &cancel, &progress, io) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[integrate_probe] integration failed: {e}");
            let out = serde_json::json!({
                "reference": args.reference.display().to_string(),
                "frameCount": total,
                "registered": registered,
                "failed": failed,
                "error": e.to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
            std::process::exit(1);
        }
    };
    let stats: &GroupStats = &output.stats;

    let included_position: HashMap<usize, usize> = output
        .included
        .iter()
        .enumerate()
        .map(|(k, &i)| (i, k))
        .collect();
    for (processed_idx, f) in processed.iter().enumerate() {
        if let Some(&k) = included_position.get(&processed_idx) {
            frame_entries[f.entry_idx]["included"] = serde_json::json!(true);
            frame_entries[f.entry_idx]["rejectedFraction"] =
                serde_json::json!(stats.rejected_fraction_per_frame.get(k));
        }
    }

    // Header/naming/write (spec §6.4/§9.5).
    // FILTER/INSTRUME name the actual group's camera, not necessarily the
    // global reference's — when `reference_in_group` they're the same file
    // (`reference_index` resolves to the reference's own entry), and when
    // they're not, naming the master after the reference's camera would be
    // wrong (an OSC master stamped/named as the mono reference's camera).
    let group_header = FitsHeader::from_path(&processed[reference_index].path).ok();
    let filter = group_header.as_ref().and_then(|h| h.get_str("FILTER"));
    let instrume = group_header.as_ref().and_then(|h| h.get_str("INSTRUME"));
    let set_name = args.set_name.clone().unwrap_or_else(|| {
        args.dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("stack")
            .to_string()
    });
    let included_exposures: Vec<f64> = output
        .included
        .iter()
        .map(|&i| stack_frames[i].exposure_s)
        .collect();
    // Owner decision 2026-09-10: no camera token in the filename any more,
    // and the naming input is one exposure (the group's own cluster label)
    // rather than the whole per-frame list — this probe has no real
    // cluster label of its own, so it reads the first included frame's
    // exposure as a stand-in. Fix round 1 ruling: the colour-mode token is
    // always present — this probe already carries `args.osc`.
    let name = master_file_name(
        &set_name,
        filter.as_deref(),
        if args.osc {
            ColorMode::Osc
        } else {
            ColorMode::Mono
        },
        // M11 (final fix wave): this probe has no real binning input of its
        // own — 1 keeps the pre-M11 filename shape (no `bin<n>` token).
        1,
        included_exposures.first().copied(),
        stats.included,
    );

    let reference_cards = match source_cards_from_file(&processed[reference_index].path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[integrate_probe] reading reference cards failed: {e}");
            std::process::exit(1);
        }
    };
    let date_obs_first = output
        .included
        .iter()
        .filter_map(|&i| stack_frames[i].date_obs.as_deref())
        .min();
    let date_obs_last = output
        .included
        .iter()
        .filter_map(|&i| stack_frames[i].date_obs.as_deref())
        .max();
    // `ATH_STKF` names whichever frame actually anchored normalization
    // (`GroupInput.reference`) — the literal `--reference` argument only
    // when it was a group member; otherwise it isn't even part of the
    // stack, so naming it here would misdescribe the master's own header.
    let reference_id = normalization_reference_stem.clone();
    let mono_or_osc = if args.osc { "osc" } else { "mono" };
    // Owner decision 2026-09-10: `<mono|osc>__<filter>__bin<n>__<exposure
    // cluster>` — camera and geometry are no longer part of the key. This
    // probe's own formatter for the exposure token, since the crate's own
    // (`calibration_library::paths::fmt_num`) is `pub(crate)`, not visible
    // from an example binary — display text only, never asserted anywhere.
    let exposure_token = included_exposures.first().map_or_else(
        || "unknown".to_string(),
        |&e| {
            if e == e.trunc() {
                format!("{e:.0}s")
            } else {
                format!("{e}s")
            }
        },
    );
    let group_key = format!(
        "{}__{}__bin1__{}",
        mono_or_osc,
        filter.as_deref().unwrap_or("NoFilter"),
        exposure_token,
    );
    let cameras: Vec<String> = instrume
        .as_deref()
        .map(|s| vec![s.to_string()])
        .unwrap_or_default();
    let normalization_str = normalization_string(&normalization);

    let cards = match build_master_light_cards(&MasterCardInputs {
        reference_cards: &reference_cards,
        wcs: None,
        frames: stats.included,
        weighted_exposure_s: stats.weighted_exposure_s,
        date_obs_first,
        date_obs_last,
        recipe: &stats.recipe,
        weight_mode: "psfSignalWeight",
        normalization: &normalization_str,
        reference_id: &reference_id,
        group_key: &group_key,
        cameras: &cameras,
        run_id: "probe",
        app_version: env!("CARGO_PKG_VERSION"),
    }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[integrate_probe] building master cards failed: {e}");
            std::process::exit(1);
        }
    };

    let write_start = Instant::now();
    let written = match write_master_light(&args.out, &name, &output, &cards) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[integrate_probe] writing master failed: {e:#}");
            std::process::exit(1);
        }
    };
    let write_ms = write_start.elapsed().as_millis() as u64;

    let mut planes = Vec::with_capacity(channels);
    for p in 0..channels {
        planes.push(serde_json::json!({
            "masterNoise": stats.master_noise.get(p),
            "masterLocation": stats.master_location.get(p),
            "masterScale": stats.master_scale.get(p),
            "masterPsfSnr": stats.master_psf_snr.get(p),
            "bestSubNoise": stats.best_sub_noise.get(p),
            "bestSubPsfSnr": stats.best_sub_psf_snr.get(p),
            "snrGain": stats.snr_gain.get(p),
            "fwhmPx": stats.master_fwhm_px.get(p),
            "eccentricity": stats.master_eccentricity.get(p),
        }));
    }

    let mut out = serde_json::json!({
        "reference": args.reference.display().to_string(),
        "referenceInGroup": reference_in_group,
        "normalizationReference": normalization_reference_stem,
        "interpolation": serde_json::to_value(args.reg.interpolation).unwrap_or_default(),
        "frameCount": total,
        "registered": registered,
        "failed": failed,
        "included": stats.included,
        "droppedBelowMinWeight": stats.dropped_below_min_weight,
        "recipe": stats.recipe,
        "planes": planes,
        "rejectedLowFraction": stats.rejected_low_fraction,
        "rejectedHighFraction": stats.rejected_high_fraction,
        "weightedExposureS": stats.weighted_exposure_s,
        "totalExposureS": stats.total_exposure_s,
        "timing": {
            "registerMs": register_ms_total,
            "measureMs": measure_ms_total,
            "readMs": stats.read_ms,
            "combineMs": stats.combine_ms,
            "writeMs": write_ms,
            "totalMs": program_start.elapsed().as_millis() as u64,
        },
        "bytesRead": stats.bytes_read,
        "written": {
            "master": written.master.display().to_string(),
            "rejectionLow": written.rejection_low.as_ref().map(|p| p.display().to_string()),
            "rejectionHigh": written.rejection_high.as_ref().map(|p| p.display().to_string()),
        },
    });
    out["frames"] = serde_json::json!(frame_entries);
    if !seed_report.is_empty() {
        out["seedReport"] = serde_json::json!(seed_report);
    }

    if let Some(compare_path) = &args.compare {
        out["compare"] = run_compare(compare_path, &output, args.maps, stats.included, &pool);
    }

    let out_text = serde_json::to_string_pretty(&out).unwrap();
    match &args.json {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &out_text) {
                eprintln!("[integrate_probe] writing --json {}: {e}", path.display());
                std::process::exit(1);
            }
        }
        None => println!("{out_text}"),
    }

    let failed_run = out.get("compare").and_then(|c| c.get("error")).is_some();
    std::process::exit(if failed_run { 1 } else { 0 });
}

/// `--compare`: cross-check the just-written master against an externally
/// produced one (Checkpoint B step, spec §6.4). Best-effort on rejection
/// maps: the XISF reader only decodes the FIRST embedded `<Image>`, so a
/// second/third image (`rejection_low`/`rejection_high`) can be spotted by
/// name in the header text but not decoded — that half is reported as
/// `null` rather than fabricated.
fn run_compare(
    compare_path: &Path,
    output: &athenaeum_core::stacking::integrate::GroupOutput,
    maps: bool,
    included: usize,
    pool: &Arc<rayon::ThreadPool>,
) -> serde_json::Value {
    let header_text = match read_header_text(compare_path) {
        Ok(t) => t,
        Err(e) => return serde_json::json!({ "error": e }),
    };
    let image_id = match first_image_id(&header_text) {
        Some(id) => id,
        None => {
            return serde_json::json!({ "error": "no <Image id=\"…\"> element found in the first 1 MB" })
        }
    };
    if image_id != "integration" {
        return serde_json::json!({
            "error": format!("first <Image> id is '{image_id}', expected 'integration'"),
            "referenceImageId": image_id,
        });
    }

    let (mut their_planes, tw, th) = match read_planes_any(compare_path) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({ "referenceImageId": image_id, "error": e }),
    };
    let their_max = their_planes
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0f32, f32::max);
    let theirs_scaled = their_max > 1.5;
    if theirs_scaled {
        for plane in their_planes.iter_mut() {
            for v in plane.iter_mut() {
                *v /= 65535.0;
            }
        }
    }

    if (tw, th) != (output.width, output.height) {
        return serde_json::json!({
            "referenceImageId": image_id,
            "theirsScaled": theirs_scaled,
            "error": format!(
                "geometry mismatch: ours {}x{}, theirs {}x{}",
                output.width, output.height, tw, th
            ),
        });
    }
    if their_planes.len() != output.channels {
        return serde_json::json!({
            "referenceImageId": image_id,
            "theirsScaled": theirs_scaled,
            "error": format!(
                "channel mismatch: ours {} planes, theirs {} planes",
                output.channels,
                their_planes.len()
            ),
        });
    }

    // Both sides get the SAME measurement (spec-consistent FWHM/eccentricity/
    // PSFSW/PSF SNR/noise/location/scale), rather than reading some of them
    // off a log — `measure_plane` already tells us when MRS noise wasn't
    // available (`NoiseSource::BackgroundResidual`), so that signal drives
    // item 6's null-out instead of a second raw `noise_mrs` call.
    let pixels = output.width * output.height;
    let mut plane_stats = Vec::with_capacity(output.channels);
    let compare_opts = MeasureOptions::default();
    for p in 0..output.channels {
        let ours = &output.data[p * pixels..(p + 1) * pixels];
        let theirs = &their_planes[p];

        let ours_m = measure_plane(ours, output.width, output.height, &compare_opts, Some(pool));
        let theirs_m = measure_plane(
            theirs,
            output.width,
            output.height,
            &compare_opts,
            Some(pool),
        );
        let pixel_diff_skipped = theirs_m.noise_source == NoiseSource::BackgroundResidual;

        let (median_diff, median_rel_diff, pixels_compared) = if pixel_diff_skipped {
            (None, None, None)
        } else {
            let their_noise_val = theirs_m.noise;
            let mut diffs = Vec::new();
            let mut rel_diffs = Vec::new();
            stats::for_each_stratified(output.width, output.height, |i| {
                let o = ours[i];
                let t = theirs[i];
                if o.is_finite() && t.is_finite() && (t as f64) > 2.0 * their_noise_val {
                    diffs.push((o - t) as f64);
                    rel_diffs.push(((o - t).abs() as f64) / (t as f64));
                }
            });
            let n = diffs.len();
            (
                Some(median(&mut diffs)),
                Some(median(&mut rel_diffs)),
                Some(n),
            )
        };

        let mut plane_json = serde_json::json!({
            "oursNoiseMrs": ours_m.noise,
            "theirsNoiseMrs": theirs_m.noise,
            "noiseRatio": ratio(ours_m.noise, theirs_m.noise),
            "oursLocation": ours_m.location,
            "theirsLocation": theirs_m.location,
            "oursScale": ours_m.scale,
            "theirsScale": theirs_m.scale,
            "scaleRatio": ratio(ours_m.scale, theirs_m.scale),
            "oursPsfSnr": ours_m.psf_snr,
            "theirsPsfSnr": theirs_m.psf_snr,
            "oursPsfsw": ours_m.psf_signal_weight,
            "theirsPsfsw": theirs_m.psf_signal_weight,
            "theirsFwhmPx": theirs_m.fwhm_px,
            "theirsEccentricity": theirs_m.eccentricity,
            "theirsStars": theirs_m.stars_fitted,
            "medianDiff": median_diff,
            "medianRelDiff": median_rel_diff,
            "pixelsCompared": pixels_compared,
        });
        if pixel_diff_skipped {
            plane_json["pixelDiffSkipped"] = serde_json::json!("theirs noise unavailable");
        }
        plane_stats.push(plane_json);
    }

    // Star-level: one comparison on the luminance-combined planes, not
    // per-channel (registration-style stars are always found on luminance).
    let ours_refs: Vec<&[f32]> = (0..output.channels)
        .map(|p| &output.data[p * pixels..(p + 1) * pixels])
        .collect();
    let ours_lum = luminance(&ours_refs);
    let theirs_refs: Vec<&[f32]> = their_planes.iter().map(Vec::as_slice).collect();
    let theirs_lum = luminance(&theirs_refs);
    // Common-bright-subset match: our top 300 against THEIR top 600 (and,
    // separately, their top 300 against OUR top 600) — 300-vs-300 nearest-
    // neighbour matching missed pairs whenever the two detectors didn't rank
    // the same stars into their respective top 300, which a wider opposite-
    // side pool corrects for. `medianFluxRatio`/`medianCentroidDeltaPx` are
    // computed over the first (ours-in-theirs) pairing only.
    let detection = DetectionConfig::default();
    let ours_stars_300: Vec<Star> = detect_stars(
        &ours_lum,
        output.width,
        output.height,
        &detection,
        300,
        None,
    );
    let theirs_stars_300: Vec<Star> = detect_stars(&theirs_lum, tw, th, &detection, 300, None);
    let ours_stars_600: Vec<Star> = detect_stars(
        &ours_lum,
        output.width,
        output.height,
        &detection,
        600,
        None,
    );
    let theirs_stars_600: Vec<Star> = detect_stars(&theirs_lum, tw, th, &detection, 600, None);

    fn match_against(a: &[Star], b: &[Star], max_px: f64) -> (usize, Vec<f64>, Vec<f64>) {
        let mut flux_ratio = Vec::new();
        let mut centroid_delta = Vec::new();
        for s in a {
            if let Some(t) = b.iter().min_by(|p, q| {
                let d = |t: &Star| (t.x - s.x).powi(2) + (t.y - s.y).powi(2);
                d(p).total_cmp(&d(q))
            }) {
                let d = ((t.x - s.x).powi(2) + (t.y - s.y).powi(2)).sqrt();
                if d < max_px {
                    centroid_delta.push(d);
                    if t.flux > 0.0 && s.flux > 0.0 {
                        flux_ratio.push(s.flux / t.flux);
                    }
                }
            }
        }
        (centroid_delta.len(), flux_ratio, centroid_delta)
    }

    let (matched_ours_in_theirs, mut flux_ratio, mut centroid_delta) =
        match_against(&ours_stars_300, &theirs_stars_600, 1.5);
    let (matched_theirs_in_ours, _, _) = match_against(&theirs_stars_300, &ours_stars_600, 1.5);

    let star_summary = serde_json::json!({
        "oursStars": ours_stars_300.len(),
        "theirsStars": theirs_stars_300.len(),
        "matchedOursInTheirs": matched_ours_in_theirs,
        "matchedTheirsInOurs": matched_theirs_in_ours,
        "medianFluxRatio": median(&mut flux_ratio),
        "medianCentroidDeltaPx": median(&mut centroid_delta),
    });

    let mut compare = serde_json::json!({
        "referenceImageId": image_id,
        "theirsScaled": theirs_scaled,
        "planes": plane_stats,
        "stars": star_summary,
    });

    if maps {
        let has_low = header_text.contains("id=\"rejection_low\"");
        let has_high = header_text.contains("id=\"rejection_high\"");
        if has_low || has_high {
            let n = included.max(1) as f64;
            let ours_low_mean = output.rejection_low.as_deref().map(|m| mean_f32(m) / n);
            let ours_high_mean = output.rejection_high.as_deref().map(|m| mean_f32(m) / n);
            compare["rejectionMaps"] = serde_json::json!({
                "listed": { "low": has_low, "high": has_high },
                "oursMeanPerFrame": { "low": ours_low_mean, "high": ours_high_mean },
                "theirsMean": { "low": serde_json::Value::Null, "high": serde_json::Value::Null },
                "note": "listed in the header but not decoded: the XISF reader only exposes the first embedded <Image>",
            });
        }
    }

    compare
}
