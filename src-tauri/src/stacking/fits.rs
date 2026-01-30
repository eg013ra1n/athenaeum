//! Safe Rust wrapper for FITS image handling
//!
//! This module provides a safe, idiomatic Rust interface for working with
//! FITS images. It uses the fitsio crate for I/O and stores pixel data
//! in Rust-native data structures.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi::DataType;
use std::path::Path;

/// A FITS image with pixel data stored in Rust
///
/// This struct holds image metadata and pixel data for processing.
/// Data can be either 16-bit unsigned or 32-bit float.
#[derive(Debug, Clone)]
pub struct FitsImage {
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
    /// Number of channels (1 for mono, 3 for RGB)
    pub nchannels: u32,
    /// Data type
    pub data_type: DataType,
    /// Pixel data (stored as f32 for processing flexibility)
    pub data: Vec<f32>,
    /// Exposure time in seconds
    pub exposure: Option<f64>,
    /// Filter name
    pub filter: Option<String>,
    /// Object name
    pub object: Option<String>,
    /// Instrument/camera name
    pub instrument: Option<String>,
    /// Bayer pattern (if CFA)
    pub bayer_pattern: Option<String>,
    /// Pixel size X in microns
    pub pixel_size_x: Option<f64>,
    /// Pixel size Y in microns
    pub pixel_size_y: Option<f64>,
    /// Focal length in mm
    pub focal_length: Option<f64>,
    /// CCD temperature
    pub ccd_temp: Option<f64>,
    /// Gain
    pub gain: Option<i32>,
    /// Offset
    pub offset: Option<i32>,
    /// Binning X
    pub binning_x: Option<u32>,
    /// Binning Y
    pub binning_y: Option<u32>,
}

impl FitsImage {
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
    /// A new FitsImage initialized with zeros.
    pub fn new(
        width: u32,
        height: u32,
        channels: u32,
        is_float: bool,
    ) -> StackingResult<Self> {
        let total_pixels = width as usize * height as usize * channels as usize;

        Ok(FitsImage {
            width,
            height,
            nchannels: channels,
            data_type: if is_float { DataType::DataFloat } else { DataType::DataUshort },
            data: vec![0.0; total_pixels],
            exposure: None,
            filter: None,
            object: None,
            instrument: None,
            bayer_pattern: None,
            pixel_size_x: None,
            pixel_size_y: None,
            focal_length: None,
            ccd_temp: None,
            gain: None,
            offset: None,
            binning_x: None,
            binning_y: None,
        })
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
        use fitsio::FitsFile;
        use fitsio::images::ImageType;

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

        let mut fptr = FitsFile::open(path_str).map_err(|e| StackingError::ReadError {
            path: path_str.to_string(),
            details: e.to_string(),
        })?;

        // Get primary HDU
        let hdu = fptr.primary_hdu().map_err(|e| StackingError::ReadError {
            path: path_str.to_string(),
            details: format!("Failed to read primary HDU: {}", e),
        })?;

        // Read image dimensions from NAXIS keywords
        let naxis: i64 = hdu.read_key(&mut fptr, "NAXIS").unwrap_or(0);
        if naxis < 2 {
            return Err(StackingError::ReadError {
                path: path_str.to_string(),
                details: format!("Invalid NAXIS value: {}", naxis),
            });
        }

        let width: i64 = hdu.read_key(&mut fptr, "NAXIS1").unwrap_or(0);
        let height: i64 = hdu.read_key(&mut fptr, "NAXIS2").unwrap_or(0);
        let channels: i64 = if naxis >= 3 {
            hdu.read_key(&mut fptr, "NAXIS3").unwrap_or(1)
        } else {
            1
        };

        let bitpix: i64 = hdu.read_key(&mut fptr, "BITPIX").unwrap_or(16);
        let is_float = bitpix < 0; // Negative BITPIX means floating point

        // Read image data
        let total_pixels = (width * height * channels) as usize;
        let data: Vec<f32> = if is_float {
            // Read as float directly
            hdu.read_image(&mut fptr).map_err(|e| StackingError::ReadError {
                path: path_str.to_string(),
                details: format!("Failed to read image data: {}", e),
            })?
        } else {
            // Read as integers and convert to float
            let int_data: Vec<u16> = hdu.read_image(&mut fptr).map_err(|e| StackingError::ReadError {
                path: path_str.to_string(),
                details: format!("Failed to read image data: {}", e),
            })?;
            // Normalize 16-bit to 0-1 range
            int_data.iter().map(|&v| v as f32 / 65535.0).collect()
        };

        // Read optional keywords
        let exposure: Option<f64> = hdu.read_key(&mut fptr, "EXPTIME").ok()
            .or_else(|| hdu.read_key(&mut fptr, "EXPOSURE").ok());
        let filter: Option<String> = hdu.read_key(&mut fptr, "FILTER").ok();
        let object: Option<String> = hdu.read_key(&mut fptr, "OBJECT").ok();
        let instrument: Option<String> = hdu.read_key(&mut fptr, "INSTRUME").ok();
        let bayer_pattern: Option<String> = hdu.read_key(&mut fptr, "BAYERPAT").ok()
            .or_else(|| hdu.read_key::<String>(&mut fptr, "COLORTYP").ok());
        let pixel_size_x: Option<f64> = hdu.read_key(&mut fptr, "XPIXSZ").ok()
            .or_else(|| hdu.read_key(&mut fptr, "PIXSIZE1").ok());
        let pixel_size_y: Option<f64> = hdu.read_key(&mut fptr, "YPIXSZ").ok()
            .or_else(|| hdu.read_key(&mut fptr, "PIXSIZE2").ok());
        let focal_length: Option<f64> = hdu.read_key(&mut fptr, "FOCALLEN").ok();
        let ccd_temp: Option<f64> = hdu.read_key(&mut fptr, "CCD-TEMP").ok();
        let gain: Option<i32> = hdu.read_key(&mut fptr, "GAIN").ok()
            .or_else(|| hdu.read_key::<i64>(&mut fptr, "GAIN").ok().map(|v| v as i32));
        let offset: Option<i32> = hdu.read_key(&mut fptr, "OFFSET").ok()
            .or_else(|| hdu.read_key::<i64>(&mut fptr, "OFFSET").ok().map(|v| v as i32));
        let binning_x: Option<u32> = hdu.read_key::<i64>(&mut fptr, "XBINNING").ok().map(|v| v as u32);
        let binning_y: Option<u32> = hdu.read_key::<i64>(&mut fptr, "YBINNING").ok().map(|v| v as u32);

        Ok(FitsImage {
            width: width as u32,
            height: height as u32,
            nchannels: channels as u32,
            data_type: if is_float { DataType::DataFloat } else { DataType::DataUshort },
            data,
            exposure,
            filter,
            object,
            instrument,
            bayer_pattern,
            pixel_size_x,
            pixel_size_y,
            focal_length,
            ccd_temp,
            gain,
            offset,
            binning_x,
            binning_y,
        })
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
        use fitsio::FitsFile;
        use fitsio::images::{ImageDescription, ImageType};

        let path = path.as_ref();
        let path_str = path.to_str().ok_or_else(|| {
            StackingError::WriteError {
                path: path.display().to_string(),
                details: "Invalid UTF-8 in path".to_string(),
            }
        })?;

        // Delete existing file if present (FITS library doesn't overwrite)
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| StackingError::WriteError {
                path: path_str.to_string(),
                details: format!("Failed to remove existing file: {}", e),
            })?;
        }

        // Determine image type and dimensions
        let image_type = if self.data_type == DataType::DataFloat {
            ImageType::Float
        } else {
            ImageType::UnsignedShort
        };

        let dimensions: Vec<usize> = if self.nchannels == 1 {
            vec![self.width as usize, self.height as usize]
        } else {
            vec![self.width as usize, self.height as usize, self.nchannels as usize]
        };

        let description = ImageDescription {
            data_type: image_type,
            dimensions: &dimensions,
        };

        // Create the FITS file
        let mut fptr = FitsFile::create(path_str)
            .with_custom_primary(&description)
            .open()
            .map_err(|e| StackingError::WriteError {
                path: path_str.to_string(),
                details: format!("Failed to create FITS file: {}", e),
            })?;

        let hdu = fptr.primary_hdu().map_err(|e| StackingError::WriteError {
            path: path_str.to_string(),
            details: format!("Failed to access primary HDU: {}", e),
        })?;

        // Write image data
        if self.data_type == DataType::DataFloat {
            hdu.write_image(&mut fptr, &self.data).map_err(|e| StackingError::WriteError {
                path: path_str.to_string(),
                details: format!("Failed to write image data: {}", e),
            })?;
        } else {
            // Convert from float (0-1) to 16-bit
            let int_data: Vec<u16> = self.data.iter()
                .map(|&v| (v.clamp(0.0, 1.0) * 65535.0) as u16)
                .collect();
            hdu.write_image(&mut fptr, &int_data).map_err(|e| StackingError::WriteError {
                path: path_str.to_string(),
                details: format!("Failed to write image data: {}", e),
            })?;
        }

        // Write keywords
        if let Some(exp) = self.exposure {
            let _ = hdu.write_key(&mut fptr, "EXPTIME", exp);
        }
        if let Some(ref flt) = self.filter {
            let _ = hdu.write_key(&mut fptr, "FILTER", flt.as_str());
        }
        if let Some(ref obj) = self.object {
            let _ = hdu.write_key(&mut fptr, "OBJECT", obj.as_str());
        }
        if let Some(ref inst) = self.instrument {
            let _ = hdu.write_key(&mut fptr, "INSTRUME", inst.as_str());
        }
        if let Some(ref bayer) = self.bayer_pattern {
            let _ = hdu.write_key(&mut fptr, "BAYERPAT", bayer.as_str());
        }
        if let Some(px) = self.pixel_size_x {
            let _ = hdu.write_key(&mut fptr, "XPIXSZ", px);
        }
        if let Some(py) = self.pixel_size_y {
            let _ = hdu.write_key(&mut fptr, "YPIXSZ", py);
        }
        if let Some(fl) = self.focal_length {
            let _ = hdu.write_key(&mut fptr, "FOCALLEN", fl);
        }
        if let Some(temp) = self.ccd_temp {
            let _ = hdu.write_key(&mut fptr, "CCD-TEMP", temp);
        }

        Ok(())
    }

    /// Get image dimensions (width, height, channels)
    pub fn dimensions(&self) -> (u32, u32, u32) {
        (self.width, self.height, self.nchannels)
    }

    /// Get number of channels
    pub fn channels(&self) -> u32 {
        self.nchannels
    }

    /// Check if image uses 32-bit float data
    pub fn is_float(&self) -> bool {
        self.data_type == DataType::DataFloat
    }

    /// Get exposure time from header (if available)
    pub fn exposure_time(&self) -> Option<f64> {
        self.exposure
    }

    /// Get filter name from header (if available)
    pub fn filter_name(&self) -> Option<String> {
        self.filter.clone()
    }

    /// Alias for filter_name
    pub fn filter(&self) -> Option<String> {
        self.filter_name()
    }

    /// Get object name from header (if available)
    pub fn object_name(&self) -> Option<String> {
        self.object.clone()
    }

    /// Get camera/instrument name from header (if available)
    pub fn instrument(&self) -> Option<String> {
        self.instrument.clone()
    }

    /// Get Bayer pattern (if CFA)
    pub fn bayer_pattern(&self) -> Option<String> {
        self.bayer_pattern.clone()
    }

    /// Check if this is a CFA (Color Filter Array) image
    pub fn is_cfa(&self) -> bool {
        self.bayer_pattern.is_some() && self.nchannels == 1
    }

    /// Check if this is a color (RGB) image
    pub fn is_color(&self) -> bool {
        self.nchannels >= 3
    }

    /// Get pixel size in microns (if available)
    pub fn pixel_size(&self) -> Option<(f64, f64)> {
        match (self.pixel_size_x, self.pixel_size_y) {
            (Some(x), Some(y)) => Some((x, y)),
            _ => None,
        }
    }

    /// Get pixel scale in arcsec/pixel (if focal length and pixel size known)
    pub fn pixel_scale(&self) -> Option<f64> {
        match (self.pixel_size_x, self.focal_length) {
            (Some(pixel_size), Some(focal_length)) if focal_length > 0.0 => {
                // Scale = 206.265 * pixel_size_um / focal_length_mm (arcsec/pixel)
                Some(206.265 * pixel_size / focal_length)
            }
            _ => None,
        }
    }

    /// Get CCD temperature (if available)
    pub fn ccd_temp(&self) -> Option<f64> {
        self.ccd_temp
    }

    /// Get gain value (if available)
    pub fn gain(&self) -> Option<i32> {
        self.gain
    }

    /// Get offset value (if available)
    pub fn offset(&self) -> Option<i32> {
        self.offset
    }

    /// Get binning (if available)
    pub fn binning(&self) -> Option<(u32, u32)> {
        match (self.binning_x, self.binning_y) {
            (Some(x), Some(y)) => Some((x, y)),
            _ => None,
        }
    }

    /// Get pixel value at (x, y, channel)
    pub fn get_pixel(&self, x: u32, y: u32, channel: u32) -> f32 {
        if x >= self.width || y >= self.height || channel >= self.nchannels {
            return 0.0;
        }
        let idx = (channel as usize * self.width as usize * self.height as usize)
            + (y as usize * self.width as usize)
            + x as usize;
        self.data.get(idx).copied().unwrap_or(0.0)
    }

    /// Set pixel value at (x, y, channel)
    pub fn set_pixel(&mut self, x: u32, y: u32, channel: u32, value: f32) {
        if x >= self.width || y >= self.height || channel >= self.nchannels {
            return;
        }
        let idx = (channel as usize * self.width as usize * self.height as usize)
            + (y as usize * self.width as usize)
            + x as usize;
        if idx < self.data.len() {
            self.data[idx] = value;
        }
    }

    /// Get a slice of pixel data for a channel
    pub fn channel_data(&self, channel: u32) -> &[f32] {
        let pixels_per_channel = self.width as usize * self.height as usize;
        let start = channel as usize * pixels_per_channel;
        let end = start + pixels_per_channel;
        &self.data[start..end.min(self.data.len())]
    }

    /// Get a mutable slice of pixel data for a channel
    pub fn channel_data_mut(&mut self, channel: u32) -> &mut [f32] {
        let pixels_per_channel = self.width as usize * self.height as usize;
        let start = channel as usize * pixels_per_channel;
        let end = start + pixels_per_channel;
        let len = self.data.len();
        &mut self.data[start..end.min(len)]
    }

    /// Copy this image
    pub fn clone_image(&self) -> StackingResult<Self> {
        Ok(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_image() {
        let result = FitsImage::new(100, 100, 1, false);
        assert!(result.is_ok());
        let img = result.unwrap();
        assert_eq!(img.width, 100);
        assert_eq!(img.height, 100);
        assert_eq!(img.nchannels, 1);
        assert_eq!(img.data.len(), 10000);
    }

    #[test]
    fn test_pixel_access() {
        let mut img = FitsImage::new(10, 10, 1, true).unwrap();
        img.set_pixel(5, 5, 0, 0.5);
        assert_eq!(img.get_pixel(5, 5, 0), 0.5);
        assert_eq!(img.get_pixel(0, 0, 0), 0.0);
    }

    #[test]
    fn test_rgb_image() {
        let img = FitsImage::new(100, 100, 3, true).unwrap();
        assert_eq!(img.dimensions(), (100, 100, 3));
        assert_eq!(img.data.len(), 30000);
        assert!(img.is_color());
        assert!(!img.is_cfa());
    }
}
