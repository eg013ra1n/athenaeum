//! Standards-compliant FITS writer (FITS 4.0): BITPIX=-32 primary HDU, typed keyword vocabulary.
pub mod card;
pub mod writer;
pub mod keywords;
pub use card::{Card, CardValue, FitsWriteError};
pub use writer::{write_fits_f32, write_fits_f32_to};
