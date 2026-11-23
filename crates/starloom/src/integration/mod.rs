//! Master-frame integration engine (spec §4): banded streaming reads,
//! per-pixel robust combination, recipe orchestration. Never holds
//! N full frames in RAM — the working set is N × one band.

pub mod band_budget;
pub mod banded;
pub mod cfa;
pub mod combine;
pub mod engine;
pub mod io_policy;
pub mod plane_reader;
pub mod registered_source;
pub mod source;
pub mod stats;
pub mod storage_class;
pub mod student_t;

/// Why an integration or calibration read failed.
#[derive(Debug)]
pub enum IntegrationError {
    /// A file-system error while opening or reading a frame.
    Io(std::io::Error),
    /// The inputs cannot be combined as given (mixed frame sizes, a plane that does not exist, bad parameters);
    /// carries the reason.
    BadInput(String),
    /// A frame file could not be decoded; carries the reason.
    Decode(String),
    /// The caller raised its cancel flag; no output was produced.
    Cancelled,
}

impl std::fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::BadInput(m) => write!(f, "bad input: {m}"),
            Self::Decode(m) => write!(f, "decode: {m}"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}
impl std::error::Error for IntegrationError {}
impl From<std::io::Error> for IntegrationError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
