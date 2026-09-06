//! Master-frame integration engine (spec §4): banded streaming reads,
//! per-pixel robust combination, recipe orchestration. Never holds
//! N full frames in RAM — the working set is N × one band.

pub mod band_budget;
pub mod banded;
pub mod cfa;
pub mod combine;
pub mod engine;
pub mod io_policy;
pub mod storage_class;

#[derive(Debug)]
pub enum IntegrationError {
    Io(std::io::Error),
    BadInput(String),
    Decode(String),
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
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}
