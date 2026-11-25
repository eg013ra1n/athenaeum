//! On-disk record of the star fits Measure's PSF fitter accepted for one frame PLANE (`.athf`). The format lives in
//! `starloom` (`starloom::stacking::fits_artifact`); this module re-exports it under the old path and keeps the one
//! test that names the catalog's artifact kind.

pub use starloom::stacking::fits_artifact::*;

#[cfg(test)]
mod tests {
    /// A plane's fits file has its own `stacking_artifacts` kind (`plane` 0-based, the index
    /// [`crate::integration::plane_reader::PlaneReader`] uses): each plane owns its own `.athf` file and its own row.
    #[test]
    fn artifact_kind_is_distinct_per_plane() {
        use crate::db::stacking::ArtifactKind;
        assert_eq!(ArtifactKind::Fits(0).as_db_str(), "fits.0");
        assert_eq!(ArtifactKind::Fits(1).as_db_str(), "fits.1");
        assert_ne!(ArtifactKind::Fits(0), ArtifactKind::Fits(1));
    }
}
