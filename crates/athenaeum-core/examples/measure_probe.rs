//! Dev probe: measure one calibrated FITS frame and print its
//! `FrameMeasurement` as JSON.
//!
//! `cargo run --release -p athenaeum-core --example measure_probe -- <file.fits> [auto|moffat4]`

use std::path::Path;
use std::sync::atomic::AtomicBool;

use athenaeum_core::stacking::measure::{measure_frame, MeasureOptions};
use athenaeum_core::stacking::psf_signal::PsfModel;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: measure_probe <file.fits> [auto|moffat4]");
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
    match measure_frame(Path::new(&path), &opts, None, &AtomicBool::new(false)) {
        Ok(m) => println!(
            "{}",
            serde_json::to_string_pretty(&m).expect("serializable")
        ),
        Err(e) => {
            eprintln!("measure failed: {e}");
            std::process::exit(1);
        }
    }
}
