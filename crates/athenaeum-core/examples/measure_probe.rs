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
//! `FrameMeasurement` from.
//!
//! `cargo run --release -p athenaeum-core --example measure_probe -- <file.fits|file.xisf> [auto|moffat4]`

use std::path::Path;
use std::sync::atomic::AtomicBool;

use athenaeum_core::stacking::measure::{measure_frame, measure_plane, MeasureOptions};
use athenaeum_core::stacking::psf_signal::PsfModel;

/// As `integrate_probe.rs::read_planes_any`: FITS via `PlaneReader`,
/// anything else via `astroimage::ImageConverter::read_raw` (XISF, the
/// probe's own reason for existing — `PixelData::Float32` used as-is,
/// `PixelData::Uint16` divided down to `[0, 1]`), returning planar planes.
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
        astroimage::PixelData::Float32(v) => v,
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
