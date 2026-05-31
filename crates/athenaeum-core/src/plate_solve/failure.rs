//! Map a plate-solve failure (the `anyhow::Error` from `solvemyastro::solve`)
//! to a human-readable reason + a stable code, for display in the UI.
//!
//! `solvemyastro` returns a structured [`solvemyastro::SolveFailure`] carrying
//! a [`solvemyastro::FailureClass`]; we downcast it and translate each class
//! into actionable text. Errors that don't carry one (early bails like "too
//! few stars", cancellation) fall back to their own message.

use solvemyastro::{FailureClass, SolveFailure};

/// A user-facing description of why a solve failed.
pub struct SolveFailureInfo {
    /// Stable machine code (the solvemyastro `FailureClass`, e.g. `VERIFY_GAP`)
    /// when available, for grouping/styling in the UI. `None` for errors that
    /// don't carry a structured `SolveFailure` (early bails, cancellation).
    pub code: Option<String>,
    /// Human-readable, actionable message.
    pub message: String,
}

/// Inspect a plate-solve error and produce a [`SolveFailureInfo`]. Downcasts to
/// the structured [`solvemyastro::SolveFailure`] when present; otherwise falls
/// back to the error's own message.
pub fn describe_solve_failure(err: &anyhow::Error) -> SolveFailureInfo {
    if let Some(f) = err.downcast_ref::<SolveFailure>() {
        let message = match &f.class {
            FailureClass::VerifyGap { best, required } => format!(
                "Found {best} of {required} needed matches — the field is too sparse \
                 or needs a deeper catalog."
            ),
            FailureClass::AffineNeverFit => {
                "Matched stars but couldn't fit a solution (field geometry or distortion)."
                    .to_string()
            }
            FailureClass::MatchStarved => {
                "No catalog quads matched — the catalog is too shallow for this field, \
                 or the image is heavily distorted."
                    .to_string()
            }
            FailureClass::VerifyZeroInliers => {
                "A candidate position was found but matched no catalog stars (wrong sky region)."
                    .to_string()
            }
            FailureClass::DetectTooFew | FailureClass::ImgQuadsEmpty => {
                "Too few stars detected in the image to attempt a solve.".to_string()
            }
            FailureClass::NoVerifyAttempted => {
                "A candidate was rejected before verification (implausible pixel scale)."
                    .to_string()
            }
            // Solved shouldn't appear on the error path; Unclassified has no
            // better text than the raw message.
            FailureClass::Solved | FailureClass::Unclassified => f.message.clone(),
        };
        SolveFailureInfo {
            code: Some(f.class.as_str().to_string()),
            message,
        }
    } else {
        SolveFailureInfo {
            code: None,
            message: err.to_string(),
        }
    }
}
