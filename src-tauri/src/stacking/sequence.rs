//! Sequence management for batch FITS operations
//!
//! A sequence represents a collection of FITS images that will be processed
//! together, such as a set of bias frames to be stacked into a master bias.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi::{self, Sequence as FfiSequence};
use crate::stacking::fits::FitsImage;
use std::ffi::CString;
use std::path::{Path, PathBuf};

/// A managed sequence of FITS images
///
/// This struct provides a safe interface for working with collections of
/// images for stacking operations.
pub struct ImageSequence {
    /// Paths to the source FITS files
    paths: Vec<PathBuf>,
    /// Reference image index (for registration)
    reference_index: usize,
    /// Cached metadata from first frame
    dimensions: Option<(u32, u32, u32)>,
    /// Whether images are 32-bit float
    is_float: Option<bool>,
}

impl ImageSequence {
    /// Create a new sequence from a list of file paths
    ///
    /// # Arguments
    ///
    /// * `paths` - Paths to FITS files in the sequence
    ///
    /// # Returns
    ///
    /// A new ImageSequence or an error if paths are empty or invalid.
    pub fn from_paths<P: AsRef<Path>>(paths: &[P]) -> StackingResult<Self> {
        if paths.is_empty() {
            return Err(StackingError::EmptySequence);
        }

        let paths: Vec<PathBuf> = paths.iter().map(|p| p.as_ref().to_path_buf()).collect();

        // Verify all files exist
        for path in &paths {
            if !path.exists() {
                return Err(StackingError::FileNotFound(path.display().to_string()));
            }
        }

        Ok(ImageSequence {
            paths,
            reference_index: 0,
            dimensions: None,
            is_float: None,
        })
    }

    /// Get the number of images in the sequence
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Check if the sequence is empty
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Get the file paths in the sequence
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Set the reference image index
    ///
    /// The reference image is used as the base for registration alignment.
    pub fn set_reference(&mut self, index: usize) -> StackingResult<()> {
        if index >= self.paths.len() {
            return Err(StackingError::InvalidReferenceIndex {
                index,
                count: self.paths.len(),
            });
        }
        self.reference_index = index;
        Ok(())
    }

    /// Get the reference image index
    pub fn reference_index(&self) -> usize {
        self.reference_index
    }

    /// Load and return the image at the given index
    pub fn load_frame(&self, index: usize) -> StackingResult<FitsImage> {
        if index >= self.paths.len() {
            return Err(StackingError::InvalidReferenceIndex {
                index,
                count: self.paths.len(),
            });
        }
        FitsImage::open(&self.paths[index])
    }

    /// Get the dimensions of images in this sequence
    ///
    /// Loads the first image to determine dimensions if not already cached.
    pub fn dimensions(&mut self) -> StackingResult<(u32, u32, u32)> {
        if let Some(dims) = self.dimensions {
            return Ok(dims);
        }

        let first = self.load_frame(0)?;
        let dims = first.dimensions();
        self.dimensions = Some(dims);
        self.is_float = Some(first.is_float());
        Ok(dims)
    }

    /// Check if images in this sequence are 32-bit float
    pub fn is_float(&mut self) -> StackingResult<bool> {
        if let Some(is_float) = self.is_float {
            return Ok(is_float);
        }

        // Force dimension loading which also caches is_float
        self.dimensions()?;
        Ok(self.is_float.unwrap_or(false))
    }

    /// Validate that all images in the sequence have compatible dimensions
    ///
    /// This is important before stacking operations.
    pub fn validate_dimensions(&mut self) -> StackingResult<()> {
        let (ref_w, ref_h, ref_c) = self.dimensions()?;

        for (i, path) in self.paths.iter().enumerate().skip(1) {
            let img = FitsImage::open(path)?;
            let (w, h, c) = img.dimensions();

            if w != ref_w || h != ref_h {
                return Err(StackingError::DimensionsMismatch {
                    expected: format!("{}x{}", ref_w, ref_h),
                    actual: format!("{}x{} (frame {})", w, h, i),
                });
            }

            if c != ref_c {
                return Err(StackingError::DimensionsMismatch {
                    expected: format!("{} channels", ref_c),
                    actual: format!("{} channels (frame {})", c, i),
                });
            }
        }

        Ok(())
    }

    /// Get exposure times for all frames (if available)
    pub fn exposure_times(&self) -> Vec<Option<f64>> {
        self.paths
            .iter()
            .map(|p| FitsImage::open(p).ok().and_then(|f| f.exposure_time()))
            .collect()
    }

    /// Get a summary of the sequence
    pub fn summary(&mut self) -> SequenceSummary {
        let dims = self.dimensions().ok();
        let is_float = self.is_float().ok();
        let exposures = self.exposure_times();
        let total_exposure: f64 = exposures.iter().filter_map(|e| *e).sum();

        SequenceSummary {
            frame_count: self.paths.len(),
            dimensions: dims,
            is_float: is_float.unwrap_or(false),
            total_exposure_seconds: total_exposure,
            reference_index: self.reference_index,
        }
    }
}

/// Summary information about a sequence
#[derive(Debug, Clone)]
pub struct SequenceSummary {
    /// Number of frames in the sequence
    pub frame_count: usize,
    /// Image dimensions (width, height, channels) if known
    pub dimensions: Option<(u32, u32, u32)>,
    /// Whether images are 32-bit float
    pub is_float: bool,
    /// Total exposure time in seconds
    pub total_exposure_seconds: f64,
    /// Reference image index
    pub reference_index: usize,
}

impl std::fmt::Display for SequenceSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} frames",
            self.frame_count
        )?;
        if let Some((w, h, c)) = self.dimensions {
            write!(f, ", {}x{}x{}", w, h, c)?;
        }
        if self.total_exposure_seconds > 0.0 {
            write!(f, ", {:.1}s total", self.total_exposure_seconds)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_sequence() {
        let result = ImageSequence::from_paths::<PathBuf>(&[]);
        assert!(matches!(result, Err(StackingError::EmptySequence)));
    }

    #[test]
    fn test_invalid_reference() {
        let paths = vec![PathBuf::from("/nonexistent/file.fits")];
        // This will fail at path validation, not reference validation
        let result = ImageSequence::from_paths(&paths);
        assert!(result.is_err());
    }
}
