//! Master-frame integration engine. The engine lives in `starloom` (`starloom::integration`); this module
//! re-exports it under the old path and keeps the two modules that read Athenaeum's settings, plus `storage_class`
//! (which also carries the platform detection `classify*`).

pub use starloom::integration::*;

pub mod band_budget;
pub mod io_policy;
pub mod storage_class;
