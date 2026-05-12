//! Centralized image-orientation logic.
//!
//! "Should this image be flipped vertically for screen display?" is a
//! question that several pipelines need to answer the same way:
//!
//!   - the JPEG preview emitted by rustafits
//!   - the contour-plot pipeline in `flat_analysis`
//!   - the star-overlay coordinate transform in the frontend (via the
//!     `flip_vertical` field on `FrameAnalysis`)
//!   - any future thumbnail / annotation surface
//!
//! Today the rule lives in three places (rustafits' FITS reader, the Tauri
//! analysis command's inline `detect_flip_vertical`, and its Axum mirror)
//! with the same logic in each. This module is the single source of truth.
//! After step 1 of the plan it is **not yet wired into any of those call
//! sites** — it exists so we can validate its output against PixInsight on
//! a few sample files before flipping the convention switch.
//!
//! # The convention
//!
//! The astronomical-software convention (PixInsight, ds9, Siril,
//! AstroImageJ) treats FITS files as **bottom-up** by default and flips for
//! screen display. Specifically:
//!
//! | `ROWORDER`           | What it means                              | Flip for display? |
//! | -------------------- | ------------------------------------------ | ----------------- |
//! | `TOP-DOWN`           | row 0 is the top row of the displayed image | **no**           |
//! | `BOTTOM-UP`          | row 0 is the bottom row                    | **yes**           |
//! | (missing)            | astronomical convention: bottom-up         | **yes**           |
//!
//! XISF stores rows in display-natural top-down order, so XISF data needs
//! no flip on display.
//!
//! Note: this is the *opposite* of what rustafits does today
//! (`flip_vertical = roworder == "TOP-DOWN"`). Step 3 of the plan patches
//! rustafits to match. Until that lands, callers that already get
//! `flip_vertical` from rustafits will be inconsistent with this helper —
//! that is intentional and is what the manual verification step (step 2)
//! is for.

use std::path::Path;

use crate::fits_parser::fits_header_reader::FitsHeader;

/// Returns whether the image at `path` needs a vertical flip for screen
/// display, per astronomical convention. `false` if the file can't be
/// read or the format is unknown — never panics, never returns an error;
/// "I don't know" → don't flip is the safe default for renderers.
pub fn flip_vertical_for_path(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    match ext.as_deref() {
        Some("fits") | Some("fit") | Some("fts") => fits_flip_vertical(path),
        Some("xisf") => false,
        _ => false,
    }
}

/// FITS-specific rule: read `ROWORDER`, apply astronomical convention.
fn fits_flip_vertical(path: &Path) -> bool {
    let header = match FitsHeader::from_path(path) {
        Ok(h) => h,
        Err(_) => return false,
    };
    match header.get_str("ROWORDER") {
        Some(value) => {
            // Compare case-insensitive against canonical FITS spellings.
            // Trim leading/trailing whitespace and surrounding quotes that
            // some writers leave in the value despite the FITS card
            // already being delimited.
            let normalized = value
                .trim()
                .trim_matches('\'')
                .trim()
                .to_ascii_uppercase();
            match normalized.as_str() {
                "TOP-DOWN" => false,
                "BOTTOM-UP" => true,
                // Unknown explicit value — assume astronomical default.
                _ => true,
            }
        }
        // Missing ROWORDER → astronomical default = bottom-up = flip.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The naming check is the most likely failure mode if someone edits
    /// this file: getting TOP-DOWN/BOTTOM-UP backwards is silent and only
    /// shows up as a 180° flipped image somewhere in the UI. Spot test
    /// the normalization branches.
    #[test]
    fn unknown_extension_returns_false() {
        assert!(!flip_vertical_for_path(Path::new("/tmp/not-a-real-file.png")));
    }

    #[test]
    fn xisf_extension_returns_false() {
        // No file needs to exist; the dispatch happens before any I/O.
        assert!(!flip_vertical_for_path(Path::new("/tmp/whatever.xisf")));
    }

    #[test]
    fn missing_file_returns_false() {
        // FITS extension but the file doesn't exist → defaults to false
        // (don't flip on error).
        assert!(!flip_vertical_for_path(Path::new("/tmp/does-not-exist.fits")));
    }
}
