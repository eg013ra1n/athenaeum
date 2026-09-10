//! Dev probe: measure one calibrated frame and print its measurement as
//! JSON.
//!
//! FITS inputs go through `measure_frame` unchanged (the full `FrameMeasurement`
//! — header-aware, channel-count-from-header). Non-FITS inputs (M3 Task 5,
//! Task 7: the external 2x drizzled XISF masters) are read plane-by-plane
//! via `astroimage::ImageConverter::read_raw` (mirroring
//! `integrate_probe.rs::read_planes_any`) and measured one plane at a time
//! with `stacking::measure::measure_plane` — printed as a JSON ARRAY of
//! `ChannelMeasurement`s, since there is no frame-level header to build a
//! `FrameMeasurement` from. The XISF branch's "not trustworthy" numbers in
//! `docs/superpowers/research/2026-09-10-m3-acceptance-run.md` finding 1
//! (M4a Task 1) were never a reader bug: rustafits returns a calibrated
//! XISF light's Float32 samples in the u16-like ADU domain (a production
//! contract other `athenaeum-core` readers depend on — controller ruling
//! R-M4a-11), and this probe passed them straight into `measure_plane`,
//! which assumes `[0, 1]` and applies its OWN ADU scale on top — a
//! double-scale. `read_planes_any` below now divides by 65535 for exactly
//! that reason, mirroring the `Uint16` arm's own normalization. For
//! measuring many files at once (and comparing against an external
//! per-frame log) use `examples/weight_audit.rs` instead.
//!
//! `cargo run --release -p athenaeum-core --example measure_probe -- <file.fits|file.xisf> [auto|moffat4]`

use std::path::Path;
use std::sync::atomic::AtomicBool;

use athenaeum_core::stacking::measure::{measure_frame, measure_plane, MeasureOptions};
use athenaeum_core::stacking::psf_signal::PsfModel;

/// As `integrate_probe.rs::read_planes_any`: FITS via `PlaneReader`,
/// anything else via `astroimage::ImageConverter::read_raw` (XISF, the
/// probe's own reason for existing — `PixelData::Float32` divided by
/// 65535 down into `[0, 1]` just like `PixelData::Uint16`, since rustafits
/// returns XISF float samples in the u16-like ADU domain), returning
/// planar planes.
fn read_planes_any(path: &Path) -> Result<(Vec<Vec<f32>>, usize, usize), String> {
    if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("fits") || e.eq_ignore_ascii_case("fit"))
        .unwrap_or(false)
    {
        let r = athenaeum_core::integration::plane_reader::PlaneReader::open(path)
            .map_err(|e| e.to_string())?;
        let planes: Vec<Vec<f32>> = (0..r.channels())
            .map(|p| r.read_plane(p))
            .collect::<Result<_, _>>()
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
    let planes: Vec<Vec<f32>> = (0..meta.channels)
        .map(|c| data[c * n..(c + 1) * n].to_vec())
        .collect();
    Ok((planes, meta.width, meta.height))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: measure_probe <file.fits|file.xisf> [auto|moffat4]");
            std::process::exit(2);
        }
    };
    let model = match args.next().as_deref() {
        Some("moffat4") => PsfModel::Moffat4,
        _ => PsfModel::Auto,
    };
    let opts = MeasureOptions {
        psf_model: model,
        ..MeasureOptions::default()
    };

    let path = Path::new(&path);
    let is_fits = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("fits") || e.eq_ignore_ascii_case("fit"))
        .unwrap_or(false);

    if is_fits {
        match measure_frame(path, &opts, None, &AtomicBool::new(false)) {
            Ok(m) => println!(
                "{}",
                serde_json::to_string_pretty(&m).expect("serializable")
            ),
            Err(e) => {
                eprintln!("measure failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let (planes, width, height) = match read_planes_any(path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("read failed: {e}");
            std::process::exit(1);
        }
    };
    let measurements: Vec<_> = planes
        .iter()
        .map(|plane| measure_plane(plane, width, height, &opts, None))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&measurements).expect("serializable")
    );
}
