//! Re-export shim (wave-3 fold-in, collab Task 8 fix round 1): the
//! implementation moved to the ungated `crate::storage_class`, because
//! `collab::storage::sweep` needs it and `integration` is behind the
//! `render` feature. Kept here, under the old path, so every existing
//! `integration::storage_class::…` reference — including the ones in
//! `examples/` and `tests/` that name it through the crate's public API —
//! keeps compiling unchanged.

pub use crate::storage_class::{
    classify, classify_all, read_concurrency, StorageClass, READ_CONCURRENCY_MAX,
};
