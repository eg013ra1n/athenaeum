//! Star detection and measurement
//!
//! This module provides pure Rust implementations for detecting stars
//! in astronomical images and measuring their properties.

use crate::stacking::fits::FitsImage;
use crate::stacking::stats::{mean, median, std_dev, mad};
use rayon::prelude::*;
use std::collections::BinaryHeap;
use std::cmp::Ordering;

/// Detected star with measured properties
#[derive(Debug, Clone)]
pub struct Star {
    /// X position (sub-pixel)
    pub x: f64,
    /// Y position (sub-pixel)
    pub y: f64,
    /// Total flux (integrated brightness)
    pub flux: f64,
    /// Peak value
    pub peak: f64,
    /// Full Width at Half Maximum (pixels)
    pub fwhm: f64,
    /// FWHM in X direction
    pub fwhm_x: f64,
    /// FWHM in Y direction
    pub fwhm_y: f64,
    /// Roundness (1.0 = perfect circle)
    pub roundness: f64,
    /// Signal-to-noise ratio
    pub snr: f64,
    /// Background level at this star
    pub background: f64,
}

/// Configuration for star detection
#[derive(Debug, Clone)]
pub struct StarFinderConfig {
    /// Detection threshold in sigma above background
    pub sigma_threshold: f64,
    /// Maximum number of stars to return
    pub max_stars: usize,
    /// Minimum FWHM (to reject hot pixels)
    pub min_fwhm: f64,
    /// Maximum FWHM (to reject extended objects)
    pub max_fwhm: f64,
    /// Minimum roundness (0-1)
    pub min_roundness: f64,
    /// Minimum SNR
    pub min_snr: f64,
    /// Search radius for local maximum (pixels)
    pub search_radius: usize,
}

impl Default for StarFinderConfig {
    fn default() -> Self {
        StarFinderConfig {
            sigma_threshold: 3.0,
            max_stars: 500,
            min_fwhm: 1.0,
            max_fwhm: 20.0,
            min_roundness: 0.3,
            min_snr: 5.0,
            search_radius: 3,
        }
    }
}

// Helper struct for priority queue (max-heap by flux)
#[derive(Clone)]
struct StarCandidate {
    x: usize,
    y: usize,
    value: f32,
}

impl PartialEq for StarCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for StarCandidate {}

impl PartialOrd for StarCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.value.partial_cmp(&other.value)
    }
}

impl Ord for StarCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.partial_cmp(&other.value).unwrap_or(Ordering::Equal)
    }
}

// =============================================================================
// Star Detection
// =============================================================================

/// Find stars in an image
///
/// Uses a peak-finding algorithm with local maximum detection.
pub fn find_stars(
    img: &FitsImage,
    config: &StarFinderConfig,
) -> Vec<Star> {
    let width = img.width as usize;
    let height = img.height as usize;

    // Use first channel (or luminance for RGB)
    let data = if img.nchannels == 1 {
        img.data.clone()
    } else {
        // Convert RGB to luminance
        let pixels_per_channel = width * height;
        (0..pixels_per_channel)
            .map(|i| {
                let r = img.data[i];
                let g = img.data[pixels_per_channel + i];
                let b = img.data[2 * pixels_per_channel + i];
                0.299 * r + 0.587 * g + 0.114 * b
            })
            .collect()
    };

    // Compute background and noise
    let background = median(&data);
    let noise = mad(&data) * 1.4826; // Scale MAD to approximate sigma

    let threshold = background + config.sigma_threshold as f32 * noise;

    // Find local maxima above threshold
    let radius = config.search_radius;
    let mut candidates: BinaryHeap<StarCandidate> = BinaryHeap::new();

    for y in radius..(height - radius) {
        for x in radius..(width - radius) {
            let idx = y * width + x;
            let value = data[idx];

            if value < threshold {
                continue;
            }

            // Check if this is a local maximum
            let mut is_max = true;
            'outer: for dy in -(radius as i32)..=(radius as i32) {
                for dx in -(radius as i32)..=(radius as i32) {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = (x as i32 + dx) as usize;
                    let ny = (y as i32 + dy) as usize;
                    if data[ny * width + nx] >= value {
                        is_max = false;
                        break 'outer;
                    }
                }
            }

            if is_max {
                candidates.push(StarCandidate { x, y, value });
            }
        }
    }

    // Measure properties of top candidates
    let mut stars: Vec<Star> = Vec::new();

    while let Some(candidate) = candidates.pop() {
        if stars.len() >= config.max_stars {
            break;
        }

        // Skip if too close to an already detected star
        let too_close = stars.iter().any(|s| {
            let dx = s.x - candidate.x as f64;
            let dy = s.y - candidate.y as f64;
            (dx * dx + dy * dy).sqrt() < config.min_fwhm * 2.0
        });

        if too_close {
            continue;
        }

        // Measure star properties
        if let Some(star) = measure_star(&data, width, height, candidate.x, candidate.y, background, noise) {
            // Apply filters
            if star.fwhm >= config.min_fwhm
                && star.fwhm <= config.max_fwhm
                && star.roundness >= config.min_roundness
                && star.snr >= config.min_snr
            {
                stars.push(star);
            }
        }
    }

    stars
}

/// Measure star properties at a given position
fn measure_star(
    data: &[f32],
    width: usize,
    height: usize,
    cx: usize,
    cy: usize,
    background: f32,
    noise: f32,
) -> Option<Star> {
    // Measurement window size (should cover the star)
    let window = 7;
    let half = window / 2;

    // Check bounds
    if cx < half || cy < half || cx + half >= width || cy + half >= height {
        return None;
    }

    // Extract star region
    let mut region: Vec<f32> = Vec::with_capacity(window * window);
    for dy in 0..window {
        for dx in 0..window {
            let x = cx - half + dx;
            let y = cy - half + dy;
            region.push(data[y * width + x] - background);
        }
    }

    // Compute centroid (center of mass)
    let mut sum_x = 0.0_f64;
    let mut sum_y = 0.0_f64;
    let mut sum_flux = 0.0_f64;

    for dy in 0..window {
        for dx in 0..window {
            let val = region[dy * window + dx].max(0.0) as f64;
            sum_x += dx as f64 * val;
            sum_y += dy as f64 * val;
            sum_flux += val;
        }
    }

    if sum_flux < 1e-10 {
        return None;
    }

    let x_centroid = sum_x / sum_flux;
    let y_centroid = sum_y / sum_flux;

    // Compute second moments for FWHM estimation
    let mut sum_xx = 0.0_f64;
    let mut sum_yy = 0.0_f64;
    let mut sum_xy = 0.0_f64;

    for dy in 0..window {
        for dx in 0..window {
            let val = region[dy * window + dx].max(0.0) as f64;
            let x_diff = dx as f64 - x_centroid;
            let y_diff = dy as f64 - y_centroid;
            sum_xx += x_diff * x_diff * val;
            sum_yy += y_diff * y_diff * val;
            sum_xy += x_diff * y_diff * val;
        }
    }

    // Standard deviations
    let sigma_x = (sum_xx / sum_flux).sqrt();
    let sigma_y = (sum_yy / sum_flux).sqrt();

    // Convert sigma to FWHM (FWHM = 2.355 * sigma for Gaussian)
    let fwhm_x = 2.355 * sigma_x;
    let fwhm_y = 2.355 * sigma_y;
    let fwhm = (fwhm_x * fwhm_y).sqrt();

    // Roundness
    let roundness = if fwhm_x > fwhm_y {
        fwhm_y / fwhm_x
    } else {
        fwhm_x / fwhm_y
    };

    // Peak value and SNR
    let peak = region.iter().cloned().fold(f32::MIN, f32::max) + background;
    let snr = if noise > 1e-10 {
        (peak - background) / noise
    } else {
        1000.0
    };

    // Convert centroid to image coordinates
    let x = (cx - half) as f64 + x_centroid;
    let y = (cy - half) as f64 + y_centroid;

    Some(Star {
        x,
        y,
        flux: sum_flux,
        peak: peak as f64,
        fwhm,
        fwhm_x,
        fwhm_y,
        roundness,
        snr: snr as f64,
        background: background as f64,
    })
}

// =============================================================================
// FWHM Measurement
// =============================================================================

/// Measure FWHM at a specific star location
pub fn measure_fwhm(img: &FitsImage, star: &Star) -> f64 {
    // Already computed during star detection
    star.fwhm
}

/// Compute average FWHM from detected stars
pub fn compute_average_fwhm(stars: &[Star]) -> f64 {
    if stars.is_empty() {
        return 0.0;
    }

    let fwhms: Vec<f64> = stars.iter().map(|s| s.fwhm).collect();
    let sum: f64 = fwhms.iter().sum();
    sum / fwhms.len() as f64
}

/// Compute median FWHM from detected stars
pub fn compute_median_fwhm(stars: &[Star]) -> f64 {
    if stars.is_empty() {
        return 0.0;
    }

    let mut fwhms: Vec<f64> = stars.iter().map(|s| s.fwhm).collect();
    fwhms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

    let mid = fwhms.len() / 2;
    if fwhms.len() % 2 == 0 {
        (fwhms[mid - 1] + fwhms[mid]) / 2.0
    } else {
        fwhms[mid]
    }
}

// =============================================================================
// Roundness Computation
// =============================================================================

/// Compute roundness of a star
pub fn compute_roundness(img: &FitsImage, star: &Star) -> f64 {
    // Already computed during star detection
    star.roundness
}

/// Compute average roundness from detected stars
pub fn compute_average_roundness(stars: &[Star]) -> f64 {
    if stars.is_empty() {
        return 0.0;
    }

    let roundnesses: Vec<f64> = stars.iter().map(|s| s.roundness).collect();
    let sum: f64 = roundnesses.iter().sum();
    sum / roundnesses.len() as f64
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_star_image() -> FitsImage {
        // Create an image with a simulated star
        let mut img = FitsImage::new(50, 50, 1, true).unwrap();

        // Background
        let background = 0.1;
        img.data.iter_mut().for_each(|v| *v = background);

        // Add a Gaussian star at center
        let cx = 25.0;
        let cy = 25.0;
        let sigma = 2.0;
        let amplitude = 0.5;

        for y in 0..50 {
            for x in 0..50 {
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                let r2 = dx * dx + dy * dy;
                let value = amplitude * (-r2 / (2.0 * sigma * sigma)).exp();
                img.data[y * 50 + x] += value as f32;
            }
        }

        img
    }

    #[test]
    fn test_find_stars() {
        let img = create_star_image();
        let config = StarFinderConfig::default();

        let stars = find_stars(&img, &config);

        assert!(!stars.is_empty());
        assert!(stars.len() <= config.max_stars);

        // Star should be near center
        let star = &stars[0];
        assert!((star.x - 25.0).abs() < 1.0);
        assert!((star.y - 25.0).abs() < 1.0);
    }

    #[test]
    fn test_star_fwhm() {
        let img = create_star_image();
        let config = StarFinderConfig {
            sigma_threshold: 2.0,
            ..Default::default()
        };

        let stars = find_stars(&img, &config);
        assert!(!stars.is_empty());

        // FWHM should be reasonable for our simulated star
        // sigma=2 -> FWHM ≈ 4.7
        let fwhm = stars[0].fwhm;
        assert!(fwhm > 3.0 && fwhm < 8.0);
    }

    #[test]
    fn test_star_roundness() {
        let img = create_star_image();
        let config = StarFinderConfig::default();

        let stars = find_stars(&img, &config);
        assert!(!stars.is_empty());

        // Our Gaussian star should be very round
        let roundness = stars[0].roundness;
        assert!(roundness > 0.9);
    }

    #[test]
    fn test_average_fwhm() {
        let stars = vec![
            Star { x: 0.0, y: 0.0, flux: 100.0, peak: 1.0, fwhm: 3.0, fwhm_x: 3.0, fwhm_y: 3.0, roundness: 1.0, snr: 50.0, background: 0.1 },
            Star { x: 10.0, y: 10.0, flux: 100.0, peak: 1.0, fwhm: 4.0, fwhm_x: 4.0, fwhm_y: 4.0, roundness: 1.0, snr: 50.0, background: 0.1 },
            Star { x: 20.0, y: 20.0, flux: 100.0, peak: 1.0, fwhm: 5.0, fwhm_x: 5.0, fwhm_y: 5.0, roundness: 1.0, snr: 50.0, background: 0.1 },
        ];

        let avg_fwhm = compute_average_fwhm(&stars);
        assert!((avg_fwhm - 4.0).abs() < 1e-6);
    }

    #[test]
    fn test_median_fwhm() {
        let stars = vec![
            Star { x: 0.0, y: 0.0, flux: 100.0, peak: 1.0, fwhm: 3.0, fwhm_x: 3.0, fwhm_y: 3.0, roundness: 1.0, snr: 50.0, background: 0.1 },
            Star { x: 10.0, y: 10.0, flux: 100.0, peak: 1.0, fwhm: 4.0, fwhm_x: 4.0, fwhm_y: 4.0, roundness: 1.0, snr: 50.0, background: 0.1 },
            Star { x: 20.0, y: 20.0, flux: 100.0, peak: 1.0, fwhm: 10.0, fwhm_x: 10.0, fwhm_y: 10.0, roundness: 1.0, snr: 50.0, background: 0.1 },
        ];

        let med_fwhm = compute_median_fwhm(&stars);
        assert!((med_fwhm - 4.0).abs() < 1e-6);
    }
}
