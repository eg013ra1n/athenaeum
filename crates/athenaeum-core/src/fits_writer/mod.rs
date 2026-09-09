//! Standards-compliant FITS writer (FITS 4.0): BITPIX=-32 primary HDU, typed keyword vocabulary.
pub mod card;
pub mod writer;
pub mod keywords;
mod stamp;
// Consumes plate_solve::storage::PlateSolveRecord, which is itself gated —
// plate_solve builds on astroimage (render) and solvemyastro (solver).
#[cfg(all(feature = "render", feature = "solver"))]
pub mod wcs;
pub use card::{Card, CardValue, FitsWriteError};
pub use stamp::stamp_extra_card;
pub use writer::{write_fits_f32, write_fits_f32_to};
