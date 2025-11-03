// AutoSTF (Automatic Screen Transfer Function) implementation
// Based on PixInsight's algorithm for automatic histogram stretching

use anyhow::Result;
use libvips::VipsImage;
use super::models::AutoSTFResult;
use std::sync::Mutex;

// Global mutex to serialize LibVIPS histogram operations (not thread-safe)
static HISTOGRAM_LOCK: Mutex<()> = Mutex::new(());

/// Parameters for AutoSTF calculation
#[derive(Debug, Clone)]
pub struct AutoSTFParams {
    /// Shadows clipping in MAD units (default: -2.8)
    pub shadows_clipping: f32,
    /// Target median after stretch (default: 0.25)
    pub target_median: f32,
    /// Use RGB linking for color images (default: true)
    pub use_rgb_linking: bool,
}

impl Default for AutoSTFParams {
    fn default() -> Self {
        Self {
            shadows_clipping: -1.5,  // Less aggressive for dim astro images (was -2.8)
            target_median: 0.35,      // Brighter output, 35% gray (was 0.25)
            use_rgb_linking: true,
        }
    }
}

/// Calculate AutoSTF parameters using PixInsight's algorithm
pub fn calculate_autostf(image: &VipsImage, params: &AutoSTFParams) -> Result<AutoSTFResult> {
    println!("  📊 Calculating AutoSTF statistics...");

    // Get image statistics
    let stats = calculate_statistics(image)?;

    println!("    Median: {:.4}, MAD: {:.4}", stats.median, stats.mad);

    // Normalize statistics to [0,1] range (16-bit → float)
    let median_norm = stats.median / 65535.0;
    let mad_norm = stats.mad / 65535.0;

    // Calculate black point using MAD (PixInsight formula on normalized data)
    // c0 = median + shadows_clipping * MAD * 1.4826
    let black_norm = (median_norm + params.shadows_clipping as f64 * mad_norm * 1.4826)
        .max(0.0)
        .min(median_norm);

    // Convert back to 16-bit range
    let black_point = black_norm * 65535.0;

    // White point is typically the 99.9th percentile
    let white_point = stats.percentile_999;

    // Calculate midtones transfer function parameter
    // This ensures the median maps to target_median after stretch
    // Normalize median position in the stretched range
    let normalized_median = (stats.median - black_point) / (white_point - black_point);
    let midtones = calculate_mtf_parameter(
        normalized_median,
        params.target_median as f64,
    );

    println!("    Black: {:.4}, White: {:.4}, Midtones: {:.4}",
             black_point, white_point, midtones);

    Ok(AutoSTFResult {
        black_point,
        white_point,
        midtones,
        median: stats.median,
        mad: stats.mad,
    })
}

/// Calculate image statistics for AutoSTF
fn calculate_statistics(image: &VipsImage) -> Result<ImageStatistics> {
    // Get image dimensions
    let width = image.get_width() as usize;
    let height = image.get_height() as usize;
    let bands = image.get_bands() as usize;

    println!("    Image: {}x{}, {} bands", width, height, bands);

    // OPTIMIZATION: Subsample image for statistics calculation (10-50x faster!)
    // Sample every 4th pixel in both dimensions = 16x fewer pixels
    // This gives excellent statistical accuracy while being much faster
    let sample_factor = 4;
    let sampled_width = width / sample_factor;
    let sampled_height = height / sample_factor;

    println!("    Sampling {}x{} → {}x{} ({:.1}x faster)",
             width, height, sampled_width, sampled_height,
             (sample_factor * sample_factor) as f64);

    // CRITICAL: LibVIPS operations are NOT thread-safe!
    // Lock to prevent concurrent LibVIPS operations that crash the app
    let _lock = HISTOGRAM_LOCK.lock().unwrap();
    println!("    🔒 Histogram lock acquired");

    println!("    [1/5] Reading pixel data from image and subsampling...");
    // WORKAROUND: Avoid LibVIPS ops::extract_band which causes GLib-GObject errors
    // Read full image buffer and manually extract first band + subsample in pure Rust
    let buffer = image.image_write_to_memory();

    // Read pixel values (16-bit unsigned, interleaved if multi-band)
    let full_pixels: &[u16] = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr() as *const u16,
            width * height * bands
        )
    };

    // Manually subsample and extract first band if multi-band
    // For multi-band: pixels are interleaved [R0,G0,B0, R1,G1,B1, ...]
    let mut sampled_pixels: Vec<u16> = Vec::with_capacity(sampled_width * sampled_height);
    for y in (0..height).step_by(sample_factor) {
        for x in (0..width).step_by(sample_factor) {
            let pixel_idx = y * width + x;
            let buffer_idx = pixel_idx * bands;  // First band of this pixel
            sampled_pixels.push(full_pixels[buffer_idx]);
        }
    }

    let pixels = sampled_pixels.as_slice();
    let total_pixels = pixels.len() as f64;

    println!("    Sampled {} pixels from {} total", pixels.len(), full_pixels.len());

    println!("    [2/5] Computing basic statistics (min, max, mean)...");
    // Calculate min, max, and mean directly
    let mut min = 65535u16;
    let mut max = 0u16;
    let mut sum = 0u64;

    for &pixel in pixels {
        if pixel < min {
            min = pixel;
        }
        if pixel > max {
            max = pixel;
        }
        sum += pixel as u64;
    }

    let avg = sum as f64 / total_pixels;

    println!("    Min: {:.6}, Max: {:.6}, Avg: {:.6}", min as f64, max as f64, avg);

    println!("    [3/5] Building histogram from {} pixels...", pixels.len());
    // Build histogram: count pixels in each bin (0-65535)
    let mut histogram: Vec<u32> = vec![0; 65536];
    for &pixel in pixels {
        histogram[pixel as usize] += 1;
    }

    // Find median (50th percentile)
    let median = find_percentile_from_rust_histogram(&histogram, total_pixels as usize, 0.5);
    println!("    Median found: {:.6}", median);

    // Find 99.9th percentile for white point
    let percentile_999 = find_percentile_from_rust_histogram(&histogram, total_pixels as usize, 0.999);
    println!("    P99.9 found: {:.6}", percentile_999);

    println!("    [4/5] Computing MAD (Median Absolute Deviation)...");
    // Calculate MAD from pixel data directly (avoid buggy LibVIPS histogram ops)
    let mut mad_histogram: Vec<u32> = vec![0; 65536];
    for &pixel in pixels {
        let deviation = (pixel as f64 - median).abs() as u16;
        mad_histogram[deviation as usize] += 1;
    }

    let mad = find_percentile_from_rust_histogram(&mad_histogram, total_pixels as usize, 0.5);
    println!("    MAD computed: {:.6}", mad);

    println!("    [5/5] Statistics complete - Median: {:.6}, MAD: {:.6}, P99.9: {:.6}", median, mad, percentile_999);

    // Release lock
    drop(_lock);
    println!("    🔓 Histogram lock released");

    Ok(ImageStatistics {
        min: min as f64,
        max: max as f64,
        median,
        mad,
        percentile_999,
    })
}

/// Find a percentile value from a Rust histogram (pure Rust implementation)
fn find_percentile_from_rust_histogram(
    histogram: &[u32],
    total_pixels: usize,
    percentile: f64,
) -> f64 {
    let target_count = (total_pixels as f64 * percentile) as usize;
    let mut cumulative = 0usize;

    for (bin, &count) in histogram.iter().enumerate() {
        cumulative += count as usize;
        if cumulative >= target_count {
            return bin as f64;
        }
    }

    // If we get here, return the max value
    65535.0
}

/// Find a percentile value from a cumulative histogram (DEPRECATED - causes crashes)
fn _find_percentile_from_histogram(
    cum_hist: &VipsImage,
    total_pixels: f64,
    percentile: f64,
    min_val: f64,
    max_val: f64,
) -> Result<f64> {
    let hist_width = cum_hist.get_width() as usize;

    // Target count for this percentile
    let target_count = total_pixels * percentile;

    // OPTIMIZED: Read entire histogram buffer at once (10-50x faster than 65K FFI calls!)
    // Write histogram data to memory buffer
    let buffer = cum_hist.image_write_to_memory();

    // Debug: Check histogram properties
    println!("    Histogram buffer size: {} bytes", buffer.len());
    println!("    Histogram width: {}, format: {:?}, bands: {}",
             cum_hist.get_width(), cum_hist.get_format(), cum_hist.get_bands());

    // Histogram data is stored as u32 values (4 bytes each) in Uint format
    // Safety: We trust LibVIPS to give us properly aligned u32 data
    let counts_u32: &[u32] = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr() as *const u32,
            buffer.len() / 4  // 4 bytes per u32
        )
    };

    println!("    Counts array length: {}, first few: [{}, {}, {}, {}]",
             counts_u32.len(),
             counts_u32.get(0).unwrap_or(&0),
             counts_u32.get(1).unwrap_or(&0),
             counts_u32.get(2).unwrap_or(&0),
             counts_u32.get(3).unwrap_or(&0));

    // Find the bin where cumulative count exceeds target
    let mut result_bin = 0;
    for (i, &count_u32) in counts_u32.iter().enumerate() {
        let count = count_u32 as f64;
        if count >= target_count {
            result_bin = i;
            break;
        }
    }

    // Map bin index back to pixel value
    let value_range = max_val - min_val;
    let value = min_val + (result_bin as f64 / hist_width as f64) * value_range;

    Ok(value)
}

/// Calculate MTF parameter to map median_position to target
///
/// Given:
/// - median_position: where the image median sits in normalized [0,1] range after stretch
/// - target: where we want the median to appear in output (e.g., 0.25 for 25% gray)
///
/// Solves the inverse MTF formula: y = ((m - 1) * x) / ((2m - 1) * x - m)
/// for m such that MTF(median_position) = target
fn calculate_mtf_parameter(median_position: f64, target: f64) -> f64 {
    if target <= 0.0 || target >= 1.0 || median_position <= 0.0 || median_position >= 1.0 {
        return 0.5;  // Default midtones (no curve)
    }

    // Inverse MTF formula:
    // m = x(target - 1) / (x(2*target - 1) - target)
    // where x = median_position

    let numerator = median_position * (target - 1.0);
    let denominator = median_position * (2.0 * target - 1.0) - target;

    if denominator.abs() < 1e-10 {
        return 0.5;  // Avoid division by zero
    }

    let midtones = numerator / denominator;
    midtones.clamp(0.01, 0.99)
}

/// Statistics for an image
#[derive(Debug)]
struct ImageStatistics {
    pub min: f64,
    pub max: f64,
    pub median: f64,
    pub mad: f64,  // Median Absolute Deviation
    pub percentile_999: f64,
}

/// Calculate AutoSTF for RGB image with linked channels
pub fn calculate_autostf_rgb_linked(
    r_channel: &VipsImage,
    g_channel: &VipsImage,
    b_channel: &VipsImage,
    params: &AutoSTFParams,
) -> Result<AutoSTFResult> {
    println!("  📊 Calculating RGB-linked AutoSTF...");

    // Calculate statistics for each channel
    let r_stats = calculate_statistics(r_channel)?;
    let g_stats = calculate_statistics(g_channel)?;
    let b_stats = calculate_statistics(b_channel)?;

    // Combine statistics (use weighted average favoring green channel)
    let combined_median = (r_stats.median + 2.0 * g_stats.median + b_stats.median) / 4.0;
    let combined_mad = (r_stats.mad + 2.0 * g_stats.mad + b_stats.mad) / 4.0;

    // Normalize statistics to [0,1] range (16-bit → float)
    let median_norm = combined_median / 65535.0;
    let mad_norm = combined_mad / 65535.0;

    // Calculate black point using MAD (PixInsight formula on normalized data)
    let black_norm = (median_norm + params.shadows_clipping as f64 * mad_norm * 1.4826)
        .max(0.0)
        .min(median_norm);

    // Convert back to 16-bit range
    let black_point = black_norm * 65535.0;

    // Use maximum white point to avoid clipping any channel
    let white_point = r_stats.percentile_999
        .max(g_stats.percentile_999)
        .max(b_stats.percentile_999);

    // Calculate midtones for target median
    // Normalize median position in the stretched range
    let normalized_median = (combined_median - black_point) / (white_point - black_point);
    let midtones = calculate_mtf_parameter(
        normalized_median,
        params.target_median as f64,
    );

    println!("    RGB Black: {:.4}, White: {:.4}, Midtones: {:.4}",
             black_point, white_point, midtones);

    Ok(AutoSTFResult {
        black_point,
        white_point,
        midtones,
        median: combined_median,
        mad: combined_mad,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mtf_parameter_calculation() {
        // Test that MTF parameter maps median_position to target correctly
        let median_position = 0.5;  // Median at center
        let target = 0.25;          // Want it to appear at 25%

        let mtf = calculate_mtf_parameter(median_position, target);

        // Verify mtf is in valid range
        assert!(mtf > 0.0 && mtf < 1.0);

        // Apply MTF and check result is close to target
        let result = apply_mtf_forward(median_position, mtf);

        assert!((result - target).abs() < 0.01, "MTF({}) with m={} should give {}, got {}",
                median_position, mtf, target, result);
    }

    fn apply_mtf_forward(x: f64, m: f64) -> f64 {
        if x <= 0.0 { return 0.0; }
        if x >= 1.0 { return 1.0; }

        let numerator = (m - 1.0) * x;
        let denominator = (2.0 * m - 1.0) * x - m;

        if denominator.abs() < 1e-10 {
            x
        } else {
            (numerator / denominator).clamp(0.0, 1.0)
        }
    }
}