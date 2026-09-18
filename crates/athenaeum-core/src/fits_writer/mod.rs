//! Standards-compliant FITS writer (FITS 4.0): BITPIX=-32 primary HDU, typed keyword vocabulary.
pub mod card;
pub mod keywords;
mod stamp;
pub mod writer;
pub mod xisf_writer;
// Consumes plate_solve::storage::PlateSolveRecord, which is itself gated —
// plate_solve builds on astroimage (render) and solvemyastro (solver).
#[cfg(all(feature = "render", feature = "solver"))]
pub mod wcs;
pub use card::{Card, CardValue, FitsWriteError};
pub use stamp::stamp_extra_card;
pub use writer::{write_fits_f32, write_fits_f32_to};
pub use xisf_writer::{write_xisf_f32, write_xisf_f32_to, xisf_keyword_value};

/// The container an output image is written in. One enum for every writer
/// in the app — stacking masters, calibration masters, calibrated lights —
/// so no two features can disagree about what "xisf" means.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default, ts_rs::TS,
)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    #[default]
    Fits,
    /// Monolithic XISF 1.0, one uncompressed Float32 image, the same cards
    /// as the FITS file (`xisf_writer`).
    Xisf,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Fits => "fits",
            OutputFormat::Xisf => "xisf",
        }
    }

    /// The container a file on disk already has — a rebuild keeps it.
    pub fn from_path(path: &std::path::Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("xisf") => OutputFormat::Xisf,
            _ => OutputFormat::Fits,
        }
    }
}

/// The representable range an XISF Float32 image declares (XISF 1.0
/// §11.5.1, mandatory). Which one applies is the CALLER's knowledge: a
/// stacking master and a calibrated light are unit-scaled (`ATH_CSCL`), a
/// calibration master is raw ADU. rustafits' reader normalizes by these
/// bounds and multiplies by 65535, so both come back in the ADU domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XisfBounds {
    Unit,
    Adu16,
}

impl XisfBounds {
    pub fn attr(self) -> &'static str {
        match self {
            XisfBounds::Unit => "0:1",
            XisfBounds::Adu16 => "0:65535",
        }
    }
}

/// One image in the chosen container — the ONE place the two writers are
/// chosen between. `bounds` only matters for XISF.
pub fn write_image_f32(
    path: &std::path::Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[card::Card],
    format: OutputFormat,
    bounds: XisfBounds,
) -> Result<(), card::FitsWriteError> {
    match format {
        OutputFormat::Fits => writer::write_fits_f32(path, width, height, channels, data, cards),
        OutputFormat::Xisf => {
            xisf_writer::write_xisf_f32_with(path, width, height, channels, data, cards, bounds)
        }
    }
}
