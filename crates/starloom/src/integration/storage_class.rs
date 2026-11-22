//! The storage class and the read-concurrency rule an integration run sizes its readers from. They live in the
//! base layer ([`crate::storage_class`]); this path re-exports them so `integration::storage_class::…` names resolve.

pub use crate::storage_class::*;
