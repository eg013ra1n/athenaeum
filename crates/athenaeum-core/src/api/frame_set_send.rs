//! Frame-set send (spec 2026-08-28): the export pipeline's file list as sync
//! payload entries. `PayloadEntry` is the currency between whoever decides
//! WHAT to send (a frame selection, or a frame set under an export mode) and
//! the one package builder in `api::sync` that writes it.
use std::path::PathBuf;

use crate::package::PayloadKind;

/// One file to put in a package: the catalog frame it is (or derives from —
/// a calibrated artifact points at its source light), the file to copy, its
/// path inside the package (WBPP dir + filename, forward slashes) and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadEntry {
    pub frame_id: i64,
    pub source_path: PathBuf,
    pub rel_path: String,
    pub kind: PayloadKind,
}
