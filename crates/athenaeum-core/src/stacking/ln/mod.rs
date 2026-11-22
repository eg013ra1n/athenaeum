//! The local-normalization (LN) model and its reference. The kernel — grids, background model, star selection,
//! calibration, the relative-scale fit and the reference build — comes from starloom, under its old paths; this module
//! keeps the reference's artifact writers (`reference`), which stamp Athenaeum's scanner-skip cards.

pub mod reference;

pub use starloom::stacking::ln::*;

// The shim rule: a glob import is shadowed only by an item of the same name in this module, and the kernel's `ln`
// re-exports its own `reference::{write_reference, write_reference_saturation}`. These two names are Athenaeum's
// (`reference.rs`), so they are named explicitly here; an explicit import beats the glob.
pub use reference::{write_reference, write_reference_saturation};
