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
//! cargo run --release -p athenaeum-core --example integrate_probe -- \
//!   <reference.fits> <dir-with-c_*.fits> --out <dir> [--limit N] [--osc] \
//!   [--distortion off|auto] [--rejection auto|linearFit|winsorized|sigma] \
//!   [--maps] [--seeds fast|full] [--seeds-report N] \
//!   [--compare <external-master.xisf>] [--json <path>] \
//!   [--set-name <str>] [--threads N]

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
use athenaeum_core::integration::stats::{self, ScaleEstimator};
use athenaeum_core::integration::storage_class::StorageClass;
use athenaeum_core::stacking::integrate::{
    integrate_group, GroupInput, GroupProgress, GroupStats, IntegrationConfig, NormalizationConfig,
    RejectionChoice, StackFrame,
};
use athenaeum_core::stacking::master_cards::{
    build_master_light_cards, master_file_name, write_master_light, MasterCardInputs,
};
use athenaeum_core::stacking::measure::{
    measure_frame_with_seeds, MeasureOptions, SeedSource, ADU_SCALE,
};
use athenaeum_core::stacking::psf_signal::noise_mrs;
use athenaeum_core::stacking::register::detect::{detect_stars, luminance, Star};
use athenaeum_core::stacking::register::frame::{
    identity_registration, reference_stars, register_frame,
};
use athenaeum_core::stacking::register::writer::source_cards_from_file;
use athenaeum_core::stacking::register::{DetectionConfig, RegistrationConfig};
use athenaeum_core::stacking::weights::{compute_weights, FormulaWeights, WeightInput, WeightMode};

fn usage() -> ! {
    eprintln!(
        "usage: integrate_probe <reference.fits> <dir> --out <dir> [--limit N] [--osc] \
[--distortion off|auto] [--rejection auto|linearFit|winsorized|sigma] [--maps] \
[--seeds fast|full] [--seeds-report N] [--compare master.xisf] [--json path] \
[--set-name str] [--threads N]"
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
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1024 * 1024];
    let n = f.read(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// The `id="…"` attribute of the first `<Image` element in `text`.
fn first_image_id(text: &str) -> Option<String> {
    let img = text.find("<Image")?;
    let rest = &text[img..];
    let key = "id=\"";
    let start = rest.find(key)? + key.len();
    let end = rest[start..].find('"')?;
    Some(rest[start..start + end].to_string())
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
) -> (Vec<(f64, f64)>, u64) {
    let t = Instant::now();
    let analyzer = ImageAnalyzer::new()
        .with_max_stars(opts.max_stars.max(8))
        .with_centroid_refine(false);
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

fn full_seed_positions(
    scaled: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
) -> (Vec<(f64, f64)>, u64) {
    let t = Instant::now();
    let analyzer = ImageAnalyzer::new().with_max_stars(opts.max_stars);
    let seeds = match analyzer.analyze_data(scaled, w, h, 1) {
        Ok(r) => r.stars.iter().map(|s| (s.x as f64, s.y as f64)).collect(),
        Err(e) => {
            eprintln!("[integrate_probe] full seed detection failed: {e}");
            Vec::new()
        }
    };
    (seeds, t.elapsed().as_millis() as u64)
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
    let reference_present = candidates.iter().any(|p| same_file(p, &args.reference));
    let mut all_paths = Vec::with_capacity(candidates.len() + 1);
    if !reference_present {
        all_paths.push(args.reference.clone());
    }
    all_paths.append(&mut candidates);
    let total = all_paths.len();

    let mut processed: Vec<ProcessedFrame> = Vec::with_capacity(total);
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
        let reg = if is_reference {
            identity_registration(&reference)
        } else {
            match register_frame(&reference, path, &args.reg, Some(&pool), &cancel) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "[integrate_probe] register I/O error for {}: {e}",
                        path.display()
                    );
                    failed += 1;
                    continue;
                }
            }
        };
        register_ms_total += reg.duration_ms;
        let map = match reg.outcome {
            Ok(a) => a.map,
            Err(e) => {
                eprintln!(
                    "[integrate_probe] alignment failed for {}: {e}",
                    path.display()
                );
                failed += 1;
                continue;
            }
        };

        let measurement =
            match measure_frame_with_seeds(path, &measure_opts, Some(&pool), &cancel, args.seeds) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "[integrate_probe] measure failed for {}: {e}",
                        path.display()
                    );
                    failed += 1;
                    continue;
                }
            };
        measure_ms_total += measurement.duration_ms;

        if seed_report.len() < args.seeds_report {
            match read_luminance_any(path) {
                Ok((lum, w, h)) => {
                    let scaled: Vec<f32> = lum.iter().map(|v| v * ADU_SCALE).collect();
                    let (fast_xy, fast_ms) = fast_seed_positions(&scaled, w, h, &measure_opts);
                    let (full_xy, full_ms) = full_seed_positions(&scaled, w, h, &measure_opts);
                    let fast_m = athenaeum_core::stacking::measure::measure_plane_with_seeds(
                        &lum,
                        w,
                        h,
                        &measure_opts,
                        Some(&pool),
                        SeedSource::Fast,
                    );
                    let full_m = athenaeum_core::stacking::measure::measure_plane_with_seeds(
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
                        "[integrate_probe] seeds-report {}: fast {} stars ({:.0}% within 0.5px), full {} stars ({:.0}% within 0.5px)",
                        path.display(),
                        fast_xy.len(),
                        fast_within * 100.0,
                        full_xy.len(),
                        full_within * 100.0
                    );
                    seed_report.push(serde_json::json!({
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
                    }));
                }
                Err(e) => eprintln!(
                    "[integrate_probe] seeds-report read failed for {}: {e}",
                    path.display()
                ),
            }
        }

        let (exposure_s, date_obs) = frame_meta(path);
        processed.push(ProcessedFrame {
            path: path.clone(),
            map,
            measurement,
            exposure_s,
            date_obs,
        });
    }

    let registered = processed.len();
    if registered == 0 {
        eprintln!("[integrate_probe] no frame registered and measured; nothing to integrate");
        std::process::exit(1);
    }
    let reference_index = processed
        .iter()
        .position(|f| same_file(&f.path, &args.reference))
        .unwrap_or(0);

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
                "frames": total,
                "registered": registered,
                "failed": failed,
                "error": e.to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
            std::process::exit(1);
        }
    };
    let stats: &GroupStats = &output.stats;

    // Header/naming/write (spec §6.4/§9.5).
    let ref_header = FitsHeader::from_path(&args.reference).ok();
    let filter = ref_header.as_ref().and_then(|h| h.get_str("FILTER"));
    let instrume = ref_header.as_ref().and_then(|h| h.get_str("INSTRUME"));
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
    let name = master_file_name(
        &set_name,
        filter.as_deref(),
        instrume.as_deref(),
        &included_exposures,
    );

    let reference_cards = match source_cards_from_file(&args.reference) {
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
    let reference_id = args
        .reference
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("reference")
        .to_string();
    let mono_or_osc = if args.osc { "osc" } else { "mono" };
    let group_key = format!(
        "{}__{}__{}__bin1__{}x{}",
        instrume.as_deref().unwrap_or("unknown"),
        mono_or_osc,
        filter.as_deref().unwrap_or("NoFilter"),
        output.width,
        output.height
    );
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
        "frames": total,
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
    if !seed_report.is_empty() {
        out["seedReport"] = serde_json::json!(seed_report);
    }

    if let Some(compare_path) = &args.compare {
        out["compare"] = run_compare(compare_path, &output, args.maps, stats.included);
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

    let pixels = output.width * output.height;
    let mut plane_stats = Vec::with_capacity(output.channels);
    for p in 0..output.channels {
        let ours = &output.data[p * pixels..(p + 1) * pixels];
        let theirs = &their_planes[p];

        let ours_scaled: Vec<f32> = ours.iter().map(|v| v * ADU_SCALE).collect();
        let theirs_scaled_adu: Vec<f32> = theirs.iter().map(|v| v * ADU_SCALE).collect();
        let ours_noise = noise_mrs(&ours_scaled, output.width, output.height)
            .map(|n| n as f64 / ADU_SCALE as f64);
        let theirs_noise = noise_mrs(&theirs_scaled_adu, output.width, output.height)
            .map(|n| n as f64 / ADU_SCALE as f64);

        let ours_sample = stats::stratified_sample(ours, output.width, output.height);
        let theirs_sample = stats::stratified_sample(theirs, output.width, output.height);
        let ours_ls = stats::location_scale(&ours_sample, ScaleEstimator::Bwmv);
        let theirs_ls = stats::location_scale(&theirs_sample, ScaleEstimator::Bwmv);

        let their_noise_val = theirs_noise.unwrap_or(0.0);
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
        let pixels_compared = diffs.len();
        let median_diff = median(&mut diffs);
        let median_rel_diff = median(&mut rel_diffs);

        plane_stats.push(serde_json::json!({
            "oursNoiseMrs": ours_noise,
            "theirsNoiseMrs": theirs_noise,
            "noiseRatio": match (ours_noise, theirs_noise) {
                (Some(o), Some(t)) if t > 0.0 => Some(o / t),
                _ => None,
            },
            "oursLocation": ours_ls.map(|l| l.location as f64),
            "theirsLocation": theirs_ls.map(|l| l.location as f64),
            "oursScale": ours_ls.map(|l| l.scale as f64),
            "theirsScale": theirs_ls.map(|l| l.scale as f64),
            "scaleRatio": match (ours_ls, theirs_ls) {
                (Some(o), Some(t)) if t.scale > 0.0 => Some(o.scale as f64 / t.scale as f64),
                _ => None,
            },
            "medianDiff": median_diff,
            "medianRelDiff": median_rel_diff,
            "pixelsCompared": pixels_compared,
        }));
    }

    // Star-level: one comparison on the luminance-combined planes, not
    // per-channel (registration-style stars are always found on luminance).
    let ours_refs: Vec<&[f32]> = (0..output.channels)
        .map(|p| &output.data[p * pixels..(p + 1) * pixels])
        .collect();
    let ours_lum = luminance(&ours_refs);
    let theirs_refs: Vec<&[f32]> = their_planes.iter().map(Vec::as_slice).collect();
    let theirs_lum = luminance(&theirs_refs);
    let detection = DetectionConfig::default();
    let ours_stars: Vec<Star> = detect_stars(
        &ours_lum,
        output.width,
        output.height,
        &detection,
        300,
        None,
    );
    let theirs_stars: Vec<Star> = detect_stars(&theirs_lum, tw, th, &detection, 300, None);
    let mut flux_ratio = Vec::new();
    let mut centroid_delta = Vec::new();
    for s in &ours_stars {
        if let Some(t) = theirs_stars.iter().min_by(|p, q| {
            let d = |t: &Star| (t.x - s.x).powi(2) + (t.y - s.y).powi(2);
            d(p).total_cmp(&d(q))
        }) {
            let d = ((t.x - s.x).powi(2) + (t.y - s.y).powi(2)).sqrt();
            if d < 1.5 {
                centroid_delta.push(d);
                if t.flux > 0.0 && s.flux > 0.0 {
                    flux_ratio.push(s.flux / t.flux);
                }
            }
        }
    }
    let matched = centroid_delta.len();
    let star_summary = serde_json::json!({
        "oursStars": ours_stars.len(),
        "theirsStars": theirs_stars.len(),
        "matched": matched,
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
