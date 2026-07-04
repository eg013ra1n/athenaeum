//! Standards-compliant FITS writer (FITS 4.0): BITPIX=-32 primary HDU, typed keyword vocabulary.
pub mod card;
// pub mod writer;    // Task 14
// pub mod keywords;  // Task 15
pub use card::{Card, CardValue, FitsWriteError};
// pub use writer::{write_fits_f32, write_fits_f32_to};
