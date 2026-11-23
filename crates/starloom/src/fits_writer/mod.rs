//! The FITS/XISF writer core: `Durability`, `XisfBounds`, `OutputFormat` and `write_image_f32(_with)`.
//!
//! Every writer family that reaches the XISF `CreatorApplication` has plain forms with defaults, which write
//! [`STARLOOM_CREATOR`], and one `_full` form that takes every parameter, the creator string last.

pub mod bayer;
pub mod card;
pub mod writer;
pub mod xisf_writer;
pub use card::{Card, CardValue, FitsWriteError};
pub use writer::{
    write_fits_f32, write_fits_f32_streaming_to, write_fits_f32_streaming_with, write_fits_f32_to,
    write_fits_f32_with,
};
pub use xisf_writer::{
    write_xisf_f32, write_xisf_f32_full, write_xisf_f32_to, write_xisf_f32_to_full, xisf_keyword_value,
};

/// The default `XISF:CreatorApplication` of a file this crate writes (ruling R-M4d-3). A caller that has its own
/// application name passes it to a `_full` writer instead.
pub const STARLOOM_CREATOR: &str = concat!("starloom ", env!("CARGO_PKG_VERSION"));

/// Whether a write must survive power loss before its rename makes it
/// visible (perf tier 1 Task 6). `Durable` = `sync_all` before the rename —
/// every master, every export. `Volatile` = flush only: the run's own
/// calibrated intermediates, which the artifact row keys on a hash and a
/// stat, so a file a crash loses or truncates is a cache miss — not a claim
/// that it can never read back as a WRONG answer in principle: on a
/// filesystem without ordered metadata a rename that landed before its data
/// was flushed can, in theory, reappear at the recorded size with garbage
/// inside it. APFS and ext4 (`data=ordered`) — the filesystems this app
/// ships on — make that unreachable, which is what the "cache miss, not a
/// wrong answer" claim actually rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Fsyncs the temporary file (`sync_all`) before the rename makes the write visible. Used for every master and
    /// every export.
    Durable,
    /// Flushes only and skips the fsync; the rename is still atomic. Used for the run's own regenerable
    /// intermediates.
    Volatile,
}

/// The container an output image is written in. One enum for every writer
/// in the app — stacking masters, calibration masters, calibrated lights —
/// so no two features can disagree about what "xisf" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    /// FITS, one primary HDU with 32-bit IEEE float samples (BITPIX -32), written by [`writer`].
    #[default]
    Fits,
    /// Monolithic XISF 1.0, one uncompressed Float32 image, the same cards
    /// as the FITS file (`xisf_writer`).
    Xisf,
}

impl OutputFormat {
    /// The file extension for this container, without the dot: `"fits"` or `"xisf"`.
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
/// stacking master and a calibrated light are unit-scaled (normalized), a
/// calibration master is raw ADU. rustafits' reader normalizes by these
/// bounds and multiplies by 65535, so both come back in the ADU domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XisfBounds {
    /// Declares the range `0:1`: samples are normalized (DN divided by the unit divisor), as in a stacking master or
    /// a calibrated light.
    Unit,
    /// Declares the range `0:65535`: samples are raw sensor DN, as in a calibration master.
    Adu16,
}

impl XisfBounds {
    /// The XISF `bounds` attribute text for this range: `"0:1"` for [`XisfBounds::Unit`], `"0:65535"` for
    /// [`XisfBounds::Adu16`].
    pub fn attr(self) -> &'static str {
        match self {
            XisfBounds::Unit => "0:1",
            XisfBounds::Adu16 => "0:65535",
        }
    }
}

/// One image in the chosen container — the ONE place the two writers are
/// chosen between. `bounds` only matters for XISF. Always
/// [`Durability::Durable`] — see [`write_image_f32_with`] for a caller that
/// can afford to skip the fsync.
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
    write_image_f32_with(
        path,
        width,
        height,
        channels,
        data,
        cards,
        format,
        bounds,
        Durability::Durable,
    )
}

/// [`write_image_f32`] with the durability as a parameter (perf tier 1 Task
/// 6) — the stacking run's regenerable calibrated intermediates pass
/// [`Durability::Volatile`]; every other caller (masters, exports, sends,
/// Perseus) passes [`Durability::Durable`].
pub fn write_image_f32_with(
    path: &std::path::Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[card::Card],
    format: OutputFormat,
    bounds: XisfBounds,
    durability: Durability,
) -> Result<(), card::FitsWriteError> {
    write_image_f32_full(
        path,
        width,
        height,
        channels,
        data,
        cards,
        format,
        bounds,
        durability,
        STARLOOM_CREATOR,
    )
}

/// The one full form of the container switch: [`write_image_f32_with`] with the `XISF:CreatorApplication` string as a
/// parameter (a FITS file has no such field).
pub fn write_image_f32_full(
    path: &std::path::Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[card::Card],
    format: OutputFormat,
    bounds: XisfBounds,
    durability: Durability,
    creator: &str,
) -> Result<(), card::FitsWriteError> {
    match format {
        OutputFormat::Fits => {
            writer::write_fits_f32_with(path, width, height, channels, data, cards, durability)
        }
        OutputFormat::Xisf => xisf_writer::write_xisf_f32_full(
            path, width, height, channels, data, cards, bounds, durability, creator,
        ),
    }
}
