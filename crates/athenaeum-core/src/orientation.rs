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

/// The canonical `ROWORDER` spellings, so a writer that has to state the
/// order explicitly (the XISF writer — see [`row_order_is_bottom_up`])
/// spells it the same way this rule reads it.
pub const ROW_ORDER_TOP_DOWN: &str = "TOP-DOWN";
pub const ROW_ORDER_BOTTOM_UP: &str = "BOTTOM-UP";

/// The row-order rule over an already-read `ROWORDER` value — the ONE
/// place the table in the module docs is encoded (M4d Task 2 fix round 1,
/// ruling R-T2-1: the XISF writer and the stacking run both need this
/// answer over a card they already hold, and a second copy of the rule is
/// exactly how a 180° flip goes unnoticed).
///
/// `TOP-DOWN` → `false`; `BOTTOM-UP`, an unrecognized explicit value, and a
/// missing card → `true`, the astronomical default. Case-insensitive, and
/// tolerant of the surrounding quotes some writers leave in the value
/// despite the FITS card already delimiting it.
pub fn row_order_is_bottom_up(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value
                .trim()
                .trim_matches('\'')
                .trim()
                .to_ascii_uppercase();
            match normalized.as_str() {
                s if s == ROW_ORDER_TOP_DOWN => false,
                s if s == ROW_ORDER_BOTTOM_UP => true,
                // Unknown explicit value — assume astronomical default.
                _ => true,
            }
        }
        // Missing ROWORDER → astronomical default = bottom-up = flip.
        None => true,
    }
}

/// FITS-specific rule: read `ROWORDER`, apply astronomical convention.
fn fits_flip_vertical(path: &Path) -> bool {
    let header = match FitsHeader::from_path(path) {
        Ok(h) => h,
        Err(_) => return false,
    };
    row_order_is_bottom_up(header.get_str("ROWORDER").as_deref())
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

    /// The rule itself, over the values callers actually hold (M4d Task 2
    /// fix round 1): the two canonical spellings, case and quote noise, an
    /// unrecognized value and a missing card.
    #[test]
    fn row_order_rule_covers_every_value_shape() {
        assert!(!row_order_is_bottom_up(Some(ROW_ORDER_TOP_DOWN)));
        assert!(row_order_is_bottom_up(Some(ROW_ORDER_BOTTOM_UP)));
        assert!(!row_order_is_bottom_up(Some(" 'top-down' ")));
        assert!(row_order_is_bottom_up(Some("bottom-up")));
        assert!(
            row_order_is_bottom_up(Some("sideways")),
            "an unrecognized value takes the astronomical default"
        );
        assert!(
            row_order_is_bottom_up(None),
            "a missing card takes the astronomical default"
        );
    }

    #[test]
    fn missing_file_returns_false() {
        // FITS extension but the file doesn't exist → defaults to false
        // (don't flip on error).
        assert!(!flip_vertical_for_path(Path::new("/tmp/does-not-exist.fits")));
    }
}
