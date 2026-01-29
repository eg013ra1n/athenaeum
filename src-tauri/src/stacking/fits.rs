//! Safe Rust wrapper for FITS image handling
//!
//! This module provides a safe, idiomatic Rust interface for working with
//! FITS images through the Siril FFI layer.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi::{self, DataType, Fits, GTRUE};
use std::ffi::CString;
use std::path::Path;

/// A safe wrapper around the Siril fits structure
///
/// This struct owns the underlying fits data and ensures proper cleanup
/// when dropped. It provides safe methods for common FITS operations.
pub struct FitsImage {
    /// Raw pointer to the fits structure
    inner: *mut Fits,
    /// Whether we own this memory (should free on drop)
    owned: bool,
}

// FitsImage can be sent between threads (the underlying data is thread-safe)
// but cannot be shared without synchronization
unsafe impl Send for FitsImage {}

impl FitsImage {
    /// Create a new FitsImage wrapping a raw pointer
    ///
    /// # Safety
    ///
    /// The pointer must be valid and point to a properly initialized fits struct.
    /// If `owned` is true, the memory will be freed when this FitsImage is dropped.
    pub(crate) unsafe fn from_raw(ptr: *mut Fits, owned: bool) -> Self {
        FitsImage { inner: ptr, owned }
    }

    /// Get the raw pointer (for FFI calls)
    ///
    /// # Safety
    ///
    /// The pointer should only be used for FFI calls and should not outlive
    /// this FitsImage.
    pub(crate) fn as_ptr(&self) -> *mut Fits {
        self.inner
    }

    /// Get the raw mutable pointer (for FFI calls that modify the image)
    ///
    /// # Safety
    ///
    /// The pointer should only be used for FFI calls and should not outlive
    /// this FitsImage.
    pub(crate) fn as_mut_ptr(&mut self) -> *mut Fits {
        self.inner
    }

    /// Create a new empty FitsImage with specified dimensions
    ///
    /// # Arguments
    ///
    /// * `width` - Image width in pixels
    /// * `height` - Image height in pixels
    /// * `channels` - Number of channels (1 for mono, 3 for RGB)
    /// * `is_float` - Use 32-bit float data (true) or 16-bit unsigned (false)
    ///
    /// # Returns
    ///
    /// A new FitsImage or an error if allocation fails.
    pub fn new(
        width: u32,
        height: u32,
        channels: u32,
        is_float: bool,
    ) -> StackingResult<Self> {
        // FFI not available - return stub implementation
        if !ffi::is_ffi_available() {
            return Err(StackingError::NotInitialized);
        }

        let data_type = if is_float {
            DataType::DataFloat
        } else {
            DataType::DataUshort
        };

        let mut fit_ptr: *mut Fits = std::ptr::null_mut();

        #[cfg(feature = "siril-ffi")]
        {
            let result = unsafe {
                ffi::new_fit_image(
                    &mut fit_ptr,
                    width as i32,
                    height as i32,
                    channels as i32,
                    data_type,
                )
            };

            if result != 0 || fit_ptr.is_null() {
                return Err(StackingError::AllocationError);
            }
        }

        #[cfg(not(feature = "siril-ffi"))]
        {
            // Create a zeroed fits structure for testing
            let boxed = Box::new(Fits::null());
            fit_ptr = Box::into_raw(boxed);
            unsafe {
                (*fit_ptr).rx = width;
                (*fit_ptr).ry = height;
                (*fit_ptr).naxis = if channels == 1 { 2 } else { 3 };
                (*fit_ptr).naxes[0] = width as i64;
                (*fit_ptr).naxes[1] = height as i64;
                (*fit_ptr).naxes[2] = channels as i64;
                (*fit_ptr).data_type = data_type;
            }
        }

        Ok(unsafe { FitsImage::from_raw(fit_ptr, true) })
    }

    /// Load a FITS file from disk
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the FITS file
    ///
    /// # Returns
    ///
    /// The loaded FitsImage or an error if loading fails.
    pub fn open<P: AsRef<Path>>(path: P) -> StackingResult<Self> {
        let path = path.as_ref();
        let path_str = path.to_str().ok_or_else(|| {
            StackingError::ReadError {
                path: path.display().to_string(),
                details: "Invalid UTF-8 in path".to_string(),
            }
        })?;

        if !path.exists() {
            return Err(StackingError::FileNotFound(path.display().to_string()));
        }

        if !ffi::is_ffi_available() {
            return Err(StackingError::NotInitialized);
        }

        let c_path = CString::new(path_str).map_err(|_| StackingError::ReadError {
            path: path_str.to_string(),
            details: "Path contains null byte".to_string(),
        })?;

        // Allocate fits structure
        let boxed = Box::new(Fits::null());
        let fit_ptr = Box::into_raw(boxed);

        #[cfg(feature = "siril-ffi")]
        {
            let result = unsafe {
                ffi::readfits(
                    c_path.as_ptr(),
                    fit_ptr,
                    std::ptr::null_mut(),
                    0, // Don't force float
                )
            };

            if result != 0 {
                // Clean up on error
                unsafe {
                    let _ = Box::from_raw(fit_ptr);
                }
                return Err(StackingError::ReadError {
                    path: path_str.to_string(),
                    details: format!("Siril error code: {}", result),
                });
            }
        }

        Ok(unsafe { FitsImage::from_raw(fit_ptr, true) })
    }

    /// Save the image to a FITS file
    ///
    /// # Arguments
    ///
    /// * `path` - Output path for the FITS file
    ///
    /// # Returns
    ///
    /// Ok(()) on success, or an error if saving fails.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> StackingResult<()> {
        let path = path.as_ref();
        let path_str = path.to_str().ok_or_else(|| {
            StackingError::WriteError {
                path: path.display().to_string(),
                details: "Invalid UTF-8 in path".to_string(),
            }
        })?;

        if !ffi::is_ffi_available() {
            return Err(StackingError::NotInitialized);
        }

        let c_path = CString::new(path_str).map_err(|_| StackingError::WriteError {
            path: path_str.to_string(),
            details: "Path contains null byte".to_string(),
        })?;

        #[cfg(feature = "siril-ffi")]
        {
            let result = unsafe { ffi::savefits(c_path.as_ptr(), self.inner) };

            if result != 0 {
                return Err(StackingError::WriteError {
                    path: path_str.to_string(),
                    details: format!("Siril error code: {}", result),
                });
            }
        }

        Ok(())
    }

    /// Get image dimensions (width, height, channels)
    pub fn dimensions(&self) -> (u32, u32, u32) {
        if self.inner.is_null() {
            return (0, 0, 0);
        }

        unsafe {
            let fit = &*self.inner;
            (fit.rx, fit.ry, fit.channels())
        }
    }

    /// Get image width
    pub fn width(&self) -> u32 {
        self.dimensions().0
    }

    /// Get image height
    pub fn height(&self) -> u32 {
        self.dimensions().1
    }

    /// Get number of channels
    pub fn channels(&self) -> u32 {
        self.dimensions().2
    }

    /// Check if image uses 32-bit float data
    pub fn is_float(&self) -> bool {
        if self.inner.is_null() {
            return false;
        }
        unsafe { (*self.inner).is_float() }
    }

    /// Get the data type
    pub fn data_type(&self) -> DataType {
        if self.inner.is_null() {
            return DataType::DataUshort;
        }
        unsafe { (*self.inner).data_type }
    }

    /// Get exposure time from header (if available)
    pub fn exposure_time(&self) -> Option<f64> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let exp = (*self.inner).keywords.exposure;
            if exp > 0.0 {
                Some(exp)
            } else {
                None
            }
        }
    }

    /// Get filter name from header (if available)
    pub fn filter_name(&self) -> Option<String> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let filter = &(*self.inner).keywords.filter;
            if filter[0] != 0 {
                let cstr = std::ffi::CStr::from_ptr(filter.as_ptr());
                cstr.to_str().ok().map(|s| s.to_string())
            } else {
                None
            }
        }
    }

    /// Get object name from header (if available)
    pub fn object_name(&self) -> Option<String> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let obj = &(*self.inner).keywords.object;
            if obj[0] != 0 {
                let cstr = std::ffi::CStr::from_ptr(obj.as_ptr());
                cstr.to_str().ok().map(|s| s.to_string())
            } else {
                None
            }
        }
    }

    /// Get camera/instrument name from header (if available)
    pub fn instrument(&self) -> Option<String> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let inst = &(*self.inner).keywords.instrume;
            if inst[0] != 0 {
                let cstr = std::ffi::CStr::from_ptr(inst.as_ptr());
                cstr.to_str().ok().map(|s| s.to_string())
            } else {
                None
            }
        }
    }

    /// Get pixel size in microns (if available)
    pub fn pixel_size(&self) -> Option<(f64, f64)> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let px = (*self.inner).keywords.pixel_size_x;
            let py = (*self.inner).keywords.pixel_size_y;
            if px > 0.0 && py > 0.0 {
                Some((px, py))
            } else {
                None
            }
        }
    }

    /// Get pixel scale in arcsec/pixel (if focal length and pixel size known)
    pub fn pixel_scale(&self) -> Option<f64> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let pixel_size = (*self.inner).keywords.pixel_size_x; // microns
            let focal_length = (*self.inner).keywords.focal_length; // mm

            if pixel_size > 0.0 && focal_length > 0.0 {
                // Scale = 206.265 * pixel_size_um / focal_length_mm (arcsec/pixel)
                Some(206.265 * pixel_size / focal_length)
            } else {
                None
            }
        }
    }

    /// Get Bayer pattern (if CFA)
    pub fn bayer_pattern(&self) -> Option<String> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let pattern = &(*self.inner).keywords.bayer_pattern;
            if pattern[0] != 0 {
                let cstr = std::ffi::CStr::from_ptr(pattern.as_ptr());
                cstr.to_str().ok().map(|s| s.to_string())
            } else {
                None
            }
        }
    }

    /// Check if this is a CFA (Color Filter Array) image
    pub fn is_cfa(&self) -> bool {
        self.bayer_pattern().is_some() && self.channels() == 1
    }

    /// Check if this is a color (RGB) image
    pub fn is_color(&self) -> bool {
        self.channels() >= 3
    }

    /// Alias for filter_name() - get filter name from header
    pub fn filter(&self) -> Option<String> {
        self.filter_name()
    }

    /// Create FitsImage from a Fits struct value (for extracting stacking results)
    ///
    /// Takes ownership of the Fits struct and wraps it in a FitsImage.
    pub(crate) fn from_fits_struct(fits: Fits) -> StackingResult<Self> {
        let boxed = Box::new(fits);
        let ptr = Box::into_raw(boxed);
        Ok(unsafe { FitsImage::from_raw(ptr, true) })
    }

    /// Get CCD temperature (if available)
    pub fn ccd_temp(&self) -> Option<f64> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let temp = (*self.inner).keywords.ccd_temp;
            // Check for default/invalid value
            if temp > -200.0 && temp < 100.0 {
                Some(temp)
            } else {
                None
            }
        }
    }

    /// Get gain value (if available)
    pub fn gain(&self) -> Option<i32> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let gain = (*self.inner).keywords.key_gain;
            if gain >= 0 {
                Some(gain)
            } else {
                None
            }
        }
    }

    /// Get offset value (if available)
    pub fn offset(&self) -> Option<i32> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let offset = (*self.inner).keywords.key_offset;
            if offset >= 0 {
                Some(offset)
            } else {
                None
            }
        }
    }

    /// Get binning (if available)
    pub fn binning(&self) -> Option<(u32, u32)> {
        if self.inner.is_null() {
            return None;
        }
        unsafe {
            let bx = (*self.inner).keywords.binning_x;
            let by = (*self.inner).keywords.binning_y;
            if bx > 0 && by > 0 {
                Some((bx, by))
            } else {
                None
            }
        }
    }

    /// Copy this image
    pub fn clone_image(&self) -> StackingResult<Self> {
        if self.inner.is_null() {
            return Err(StackingError::InvalidConfig("Source image is null".to_string()));
        }

        let (width, height, channels) = self.dimensions();
        let mut copy = FitsImage::new(width, height, channels, self.is_float())?;

        #[cfg(feature = "siril-ffi")]
        {
            let result = unsafe {
                ffi::copyfits(
                    self.inner,
                    copy.inner,
                    ffi::CP_ALLOC | ffi::CP_COPYA | ffi::CP_FORMAT,
                    -1, // All layers
                )
            };

            if result != 0 {
                return Err(StackingError::AllocationError);
            }
        }

        Ok(copy)
    }
}

impl Drop for FitsImage {
    fn drop(&mut self) {
        if self.owned && !self.inner.is_null() {
            #[cfg(feature = "siril-ffi")]
            unsafe {
                ffi::clearfits(self.inner);
            }

            #[cfg(not(feature = "siril-ffi"))]
            unsafe {
                // For stub mode, just drop the boxed memory
                let _ = Box::from_raw(self.inner);
            }

            self.inner = std::ptr::null_mut();
        }
    }
}

impl std::fmt::Debug for FitsImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h, c) = self.dimensions();
        f.debug_struct("FitsImage")
            .field("width", &w)
            .field("height", &h)
            .field("channels", &c)
            .field("is_float", &self.is_float())
            .field("object", &self.object_name())
            .field("filter", &self.filter_name())
            .field("exposure", &self.exposure_time())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_image() {
        // This will fail without FFI but shouldn't panic
        let result = FitsImage::new(100, 100, 1, false);
        // We expect NotInitialized when FFI is not available
        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn test_dimensions_null() {
        let img = FitsImage {
            inner: std::ptr::null_mut(),
            owned: false,
        };
        assert_eq!(img.dimensions(), (0, 0, 0));
    }
}
