//! Dev harness (M4a Task 1): measure a list of calibrated frames — FITS via
//! `PlaneReader`, anything else via `astroimage::ImageConverter::read_raw`
//! — through the production estimator and print one JSON line per file with
//! every per-plane `ChannelMeasurement`, so per-frame quality terms can be
//! compared against an external per-frame log by stem
//! (`docs/superpowers/research/scripts/weight_audit_compare.py`).
//!
//! rustafits returns a calibrated XISF light's Float32 samples in the
//! u16-like ADU domain (`bounds`-normalized × 65535 — a production
//! contract other `athenaeum-core` readers depend on, see
//! `read_xisf_image`'s doc comment; M4a Task 1 controller ruling
//! R-M4a-11), while `measure_plane_with_seeds` assumes calibrated data is
//! already float32 in `[0, 1]` and applies its OWN ADU scale on top. This
//! was the actual M3-acceptance-note "XISF branch not trustworthy" bug
//! (`docs/superpowers/research/2026-09-10-m3-acceptance-run.md` finding
//! 1): the reader was never wrong, the probe never divided its Float32
//! samples back down. `read_planes` below divides by 65535 for exactly
//! that reason, mirroring the `Uint16` arm's own normalization —
//! `--dump-planes` therefore writes the DIVIDED-BACK `[0, 1]`-domain
//! samples, not the file's raw attachment bytes (they round-trip
//! bit-exact for Float32 on every real file checked so far, since the
//! reader's own `* 65535.0` and this division are exact inverses in
//! practice, but the dump is not literally "no conversion" any more).
//!
//! `--out` APPENDS to an existing file (so a large file list can be split
//! across several invocations without clobbering earlier chunks, e.g.
//! Task 2's calibration grid); pass `--truncate` to empty it first
//! instead.
//!
//! `cargo run --release -p athenaeum-core --example weight_audit -- \
//!     [--seed fast|full] [--sigma <k>] [--psf auto|moffat4] [--max-stars <n>] \
//!     [--min-snr <x>] [--out <file.jsonl>] [--truncate] \
//!     [--dump-planes <file.bin>] <file>…`
//!
//! `--sigma` is `MeasureOptions::detection_sigma` — the seed-detection
//! threshold in σ above the local background; `--min-snr` is
//! `MeasureOptions::min_snr`, the flux-SNR floor a detection must clear to
//! become a fit seed. Both default to the production values.`
use athenaeum_core::stacking::measure::{
    measure_plane_with_seeds, ChannelMeasurement, MeasureOptions, SeedSource,
};
use athenaeum_core::stacking::psf_signal::PsfModel;
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

#[derive(Serialize)]
struct Line<'a> {
    stem: &'a str,
    width: usize,
    height: usize,
    channels: Vec<ChannelMeasurement>,
    duration_ms: u64,
}

/// FITS via `PlaneReader`, anything else (XISF) via
/// `astroimage::ImageConverter::read_raw` — `PixelData::Float32` divided
/// by 65535 down into `[0, 1]` just like `PixelData::Uint16`, since
/// rustafits returns XISF float samples in the u16-like ADU domain.
fn read_planes(path: &Path) -> Result<(Vec<Vec<f32>>, usize, usize), String> {
    let is_fits = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("fits") || e.eq_ignore_ascii_case("fit"))
        .unwrap_or(false);
    if is_fits {
        let r = athenaeum_core::integration::plane_reader::PlaneReader::open(path)
            .map_err(|e| e.to_string())?;
        let planes = (0..r.channels())
            .map(|p| r.read_plane(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        return Ok((planes, r.width(), r.height()));
    }
    let (meta, pixels) = astroimage::ImageConverter::read_raw(path).map_err(|e| e.to_string())?;
    let data: Vec<f32> = match pixels {
        // rustafits returns XISF float samples in the u16-like ADU domain
        // (R-M4a-11); the estimator wants native [0, 1].
        astroimage::PixelData::Float32(v) => v.into_iter().map(|x| x / 65535.0).collect(),
        astroimage::PixelData::Uint16(v) => v.iter().map(|&u| u as f32 / 65535.0).collect(),
    };
    let n = meta.width * meta.height;
    Ok((
        (0..meta.channels)
            .map(|c| data[c * n..(c + 1) * n].to_vec())
            .collect(),
        meta.width,
        meta.height,
    ))
}

struct Args {
    seed: SeedSource,
    sigma: Option<f32>,
    psf: PsfModel,
    max_stars: Option<usize>,
    min_snr: Option<f32>,
    out: Option<String>,
    truncate: bool,
    dump_planes: Option<String>,
    files: Vec<String>,
}

const USAGE: &str = "usage: weight_audit [--seed fast|full] [--sigma <k>] [--psf auto|moffat4] \
[--max-stars <n>] [--min-snr <x>] [--out <file.jsonl>] [--truncate] \
[--dump-planes <file.bin>] <file>…";

/// Prints `usage:` context plus `msg` and exits 2 — used for every
/// argument-parsing failure so a typo'd flag or a missing/non-numeric
/// value is never silently swallowed into the file list.
fn bad_arg(msg: &str) -> ! {
    eprintln!("weight_audit: {msg}");
    eprintln!("{USAGE}");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut seed = SeedSource::Fast;
    let mut sigma = None;
    let mut psf = PsfModel::Auto;
    let mut max_stars = None;
    let mut min_snr = None;
    let mut out = None;
    let mut truncate = false;
    let mut dump_planes = None;
    let mut files = Vec::new();

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seed" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| bad_arg("--seed needs a value (fast|full)"));
                seed = match v.as_str() {
                    "full" => SeedSource::Full,
                    "fast" => SeedSource::Fast,
                    other => bad_arg(&format!("unknown --seed value '{other}' (want fast|full)")),
                };
            }
            "--sigma" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| bad_arg("--sigma needs a numeric value"));
                sigma =
                    Some(v.parse::<f32>().unwrap_or_else(|_| {
                        bad_arg(&format!("--sigma value '{v}' is not a number"))
                    }));
            }
            "--psf" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| bad_arg("--psf needs a value (auto|moffat4)"));
                psf = match v.as_str() {
                    "moffat4" => PsfModel::Moffat4,
                    "auto" => PsfModel::Auto,
                    other => bad_arg(&format!(
                        "unknown --psf value '{other}' (want auto|moffat4)"
                    )),
                };
            }
            "--max-stars" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| bad_arg("--max-stars needs a numeric value"));
                max_stars = Some(v.parse::<usize>().unwrap_or_else(|_| {
                    bad_arg(&format!("--max-stars value '{v}' is not a number"))
                }));
            }
            "--min-snr" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| bad_arg("--min-snr needs a numeric value"));
                min_snr = Some(v.parse::<f32>().unwrap_or_else(|_| {
                    bad_arg(&format!("--min-snr value '{v}' is not a number"))
                }));
            }
            "--out" => {
                out = Some(
                    it.next()
                        .unwrap_or_else(|| bad_arg("--out needs a file path")),
                );
            }
            "--truncate" => {
                truncate = true;
            }
            "--dump-planes" => {
                dump_planes = Some(
                    it.next()
                        .unwrap_or_else(|| bad_arg("--dump-planes needs a file path")),
                );
            }
            other if other.starts_with("--") => {
                bad_arg(&format!("unrecognized flag '{other}'"));
            }
            other => files.push(other.to_string()),
        }
    }

    Args {
        seed,
        sigma,
        psf,
        max_stars,
        min_snr,
        out,
        truncate,
        dump_planes,
        files,
    }
}

fn main() {
    let args = parse_args();

    if args.files.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }

    let mut opts = MeasureOptions {
        psf_model: args.psf,
        ..MeasureOptions::default()
    };
    if let Some(max_stars) = args.max_stars {
        opts.max_stars = max_stars;
    }
    if let Some(sigma) = args.sigma {
        opts.detection_sigma = sigma;
    }
    if let Some(min_snr) = args.min_snr {
        opts.min_snr = min_snr;
    }
    eprintln!(
        "weight_audit: detection_sigma {} min_snr {} max_stars {} psf {:?}",
        opts.detection_sigma, opts.min_snr, opts.max_stars, opts.psf_model
    );

    if let Some(dump_path) = &args.dump_planes {
        // Writes `read_planes`'s OWN output — for XISF, the ADU-domain
        // Float32 samples already divided back by 65535 into `[0, 1]`,
        // not the file's raw attachment bytes (see the module doc
        // comment). For the default bounds `0:1` that round trip is
        // bit-exact in practice, so this is still the right pairing for
        // `weight_audit_compare.py --dump-first-image`'s byte diff.
        let Some(first) = args.files.first() else {
            eprintln!("weight_audit: --dump-planes needs a file");
            std::process::exit(2);
        };
        if args.files.len() > 1 {
            eprintln!("weight_audit: --dump-planes only reads the first file, ignoring the rest");
        }
        match read_planes(Path::new(first)) {
            Ok((planes, _w, _h)) => {
                let mut f = match std::fs::File::create(dump_path) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("weight_audit: cannot create {dump_path}: {e}");
                        std::process::exit(1);
                    }
                };
                for plane in &planes {
                    let bytes: Vec<u8> = plane.iter().flat_map(|v| v.to_le_bytes()).collect();
                    if let Err(e) = f.write_all(&bytes) {
                        eprintln!("weight_audit: write failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("weight_audit: read failed for {first}: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let mut out_writer: Box<dyn Write> = match &args.out {
        Some(path) => {
            // APPENDS by default (a large file list can be split across
            // several invocations, e.g. Task 2's calibration grid runs);
            // `--truncate` empties the file first instead.
            let mut open_opts = std::fs::OpenOptions::new();
            open_opts.create(true);
            if args.truncate {
                open_opts.write(true).truncate(true);
            } else {
                open_opts.append(true);
            }
            match open_opts.open(path) {
                Ok(f) => Box::new(f),
                Err(e) => {
                    eprintln!("weight_audit: cannot open {path}: {e}");
                    std::process::exit(1);
                }
            }
        }
        None => Box::new(std::io::stdout()),
    };

    for file in &args.files {
        let path = Path::new(file);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(file.as_str());

        let start = Instant::now();
        let (planes, width, height) = match read_planes(path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("weight_audit: skipping {file}: {e}");
                continue;
            }
        };

        let channels: Vec<ChannelMeasurement> = planes
            .iter()
            .map(|plane| measure_plane_with_seeds(plane, width, height, &opts, None, args.seed))
            .collect();
        let duration_ms = start.elapsed().as_millis() as u64;

        let line = Line {
            stem,
            width,
            height,
            channels,
            duration_ms,
        };
        match serde_json::to_string(&line) {
            Ok(json) => {
                if let Err(e) = writeln!(out_writer, "{json}") {
                    eprintln!("weight_audit: write failed: {e}");
                    std::process::exit(1);
                }
            }
            Err(e) => eprintln!("weight_audit: serialize failed for {file}: {e}"),
        }
    }
}
