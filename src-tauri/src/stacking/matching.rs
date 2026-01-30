//! Star pattern matching for image registration
//!
//! This module provides triangle-based star matching for aligning images
//! without requiring plate solving (WCS).

use crate::stacking::stars::Star;
use std::collections::HashMap;

/// Triangle formed by three stars
#[derive(Debug, Clone)]
pub struct Triangle {
    /// Indices of the three stars forming this triangle
    pub indices: [usize; 3],
    /// Normalized side ratios for matching
    pub ratios: [f64; 2],
    /// Longest side length (for scale reference)
    pub longest_side: f64,
}

/// A matched pair of star indices (reference, target)
#[derive(Debug, Clone, Copy)]
pub struct StarMatch {
    pub ref_idx: usize,
    pub target_idx: usize,
}

/// Build triangles from a set of stars
///
/// Creates triangles from star triplets for pattern matching.
/// Only uses the brightest stars for efficiency.
pub fn build_triangles(stars: &[Star], max_triangles: usize) -> Vec<Triangle> {
    let n = stars.len().min(30); // Limit to brightest 30 stars
    if n < 3 {
        return vec![];
    }

    let mut triangles = Vec::new();

    // Build triangles from all combinations of 3 stars
    for i in 0..n {
        for j in (i + 1)..n {
            for k in (j + 1)..n {
                if triangles.len() >= max_triangles {
                    return triangles;
                }

                // Compute side lengths
                let mut sides = [
                    distance(&stars[i], &stars[j]),
                    distance(&stars[j], &stars[k]),
                    distance(&stars[k], &stars[i]),
                ];

                // Sort sides so longest is first
                sides.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

                // Skip degenerate triangles
                if sides[0] < 10.0 || sides[2] < 1.0 {
                    continue;
                }

                // Normalize ratios (side2/side1, side3/side1)
                let ratios = [sides[1] / sides[0], sides[2] / sides[0]];

                triangles.push(Triangle {
                    indices: [i, j, k],
                    ratios,
                    longest_side: sides[0],
                });
            }
        }
    }

    triangles
}

/// Match stars between two images using triangle matching
///
/// Returns pairs of matched star indices (ref_idx, target_idx).
pub fn match_stars(
    ref_stars: &[Star],
    target_stars: &[Star],
) -> Vec<StarMatch> {
    const RATIO_TOLERANCE: f64 = 0.05; // 5% tolerance on ratios
    const MIN_MATCHES: usize = 3;
    const MAX_TRIANGLES: usize = 1000;

    // Build triangles for both star sets
    let ref_triangles = build_triangles(ref_stars, MAX_TRIANGLES);
    let target_triangles = build_triangles(target_stars, MAX_TRIANGLES);

    if ref_triangles.is_empty() || target_triangles.is_empty() {
        return vec![];
    }

    // Vote for star correspondences
    let mut votes: HashMap<(usize, usize), i32> = HashMap::new();

    for ref_tri in &ref_triangles {
        for target_tri in &target_triangles {
            // Check if triangles have similar shape
            let ratio_match = (ref_tri.ratios[0] - target_tri.ratios[0]).abs() < RATIO_TOLERANCE
                && (ref_tri.ratios[1] - target_tri.ratios[1]).abs() < RATIO_TOLERANCE;

            if ratio_match {
                // Vote for all 6 possible correspondences (3! = 6)
                let perms = [
                    [0, 1, 2],
                    [0, 2, 1],
                    [1, 0, 2],
                    [1, 2, 0],
                    [2, 0, 1],
                    [2, 1, 0],
                ];

                // Find best permutation based on scale consistency
                let scale_ref = ref_tri.longest_side;
                let scale_target = target_tri.longest_side;
                let scale_ratio = scale_ref / scale_target;

                for perm in &perms {
                    // This permutation maps ref_tri.indices to target_tri.indices[perm]
                    for i in 0..3 {
                        let ref_idx = ref_tri.indices[i];
                        let target_idx = target_tri.indices[perm[i]];
                        *votes.entry((ref_idx, target_idx)).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Convert votes to matches (filter by minimum votes)
    let min_votes = 3;
    let mut matches: Vec<StarMatch> = Vec::new();
    let mut used_ref: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut used_target: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // Sort by vote count (descending)
    let mut sorted_votes: Vec<_> = votes.into_iter().collect();
    sorted_votes.sort_by(|a, b| b.1.cmp(&a.1));

    for ((ref_idx, target_idx), vote_count) in sorted_votes {
        if vote_count >= min_votes
            && !used_ref.contains(&ref_idx)
            && !used_target.contains(&target_idx)
        {
            matches.push(StarMatch { ref_idx, target_idx });
            used_ref.insert(ref_idx);
            used_target.insert(target_idx);
        }
    }

    // Verify matches with RANSAC-like outlier rejection
    if matches.len() >= MIN_MATCHES {
        refine_matches(&matches, ref_stars, target_stars)
    } else {
        matches
    }
}

/// Refine matches by removing outliers
fn refine_matches(
    matches: &[StarMatch],
    ref_stars: &[Star],
    target_stars: &[Star],
) -> Vec<StarMatch> {
    if matches.len() < 4 {
        return matches.to_vec();
    }

    // Compute scale and rotation from matches
    let mut scales: Vec<f64> = Vec::new();
    let mut rotations: Vec<f64> = Vec::new();

    for i in 0..matches.len() {
        for j in (i + 1)..matches.len() {
            let ref_i = &ref_stars[matches[i].ref_idx];
            let ref_j = &ref_stars[matches[j].ref_idx];
            let tgt_i = &target_stars[matches[i].target_idx];
            let tgt_j = &target_stars[matches[j].target_idx];

            let d_ref = distance(ref_i, ref_j);
            let d_tgt = distance(tgt_i, tgt_j);

            if d_ref > 10.0 && d_tgt > 10.0 {
                scales.push(d_ref / d_tgt);

                let angle_ref = (ref_j.y - ref_i.y).atan2(ref_j.x - ref_i.x);
                let angle_tgt = (tgt_j.y - tgt_i.y).atan2(tgt_j.x - tgt_i.x);
                rotations.push(angle_ref - angle_tgt);
            }
        }
    }

    if scales.is_empty() {
        return matches.to_vec();
    }

    // Use median scale and rotation
    scales.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    rotations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let median_scale = scales[scales.len() / 2];
    let median_rotation = rotations[rotations.len() / 2];

    // Filter matches that are consistent with median transform
    let tolerance_scale = 0.1; // 10% scale tolerance
    let tolerance_angle = 0.05; // ~3 degrees

    let mut refined: Vec<StarMatch> = Vec::new();

    for m in matches {
        let ref_star = &ref_stars[m.ref_idx];
        let tgt_star = &target_stars[m.target_idx];

        // Check consistency with other matches
        let mut consistent_count = 0;
        for other in matches {
            if other.ref_idx == m.ref_idx {
                continue;
            }

            let ref_other = &ref_stars[other.ref_idx];
            let tgt_other = &target_stars[other.target_idx];

            let d_ref = distance(ref_star, ref_other);
            let d_tgt = distance(tgt_star, tgt_other);

            if d_ref > 10.0 && d_tgt > 10.0 {
                let scale = d_ref / d_tgt;
                if (scale - median_scale).abs() < tolerance_scale * median_scale {
                    consistent_count += 1;
                }
            }
        }

        if consistent_count >= 2 {
            refined.push(*m);
        }
    }

    if refined.len() >= 3 {
        refined
    } else {
        matches.to_vec()
    }
}

/// Compute distance between two stars
fn distance(a: &Star, b: &Star) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_stars() -> Vec<Star> {
        vec![
            Star { x: 100.0, y: 100.0, flux: 1000.0, peak: 0.8, fwhm: 3.0, fwhm_x: 3.0, fwhm_y: 3.0, roundness: 1.0, snr: 50.0, background: 0.1 },
            Star { x: 200.0, y: 100.0, flux: 800.0, peak: 0.7, fwhm: 3.0, fwhm_x: 3.0, fwhm_y: 3.0, roundness: 1.0, snr: 40.0, background: 0.1 },
            Star { x: 150.0, y: 200.0, flux: 600.0, peak: 0.6, fwhm: 3.0, fwhm_x: 3.0, fwhm_y: 3.0, roundness: 1.0, snr: 30.0, background: 0.1 },
            Star { x: 300.0, y: 150.0, flux: 500.0, peak: 0.5, fwhm: 3.0, fwhm_x: 3.0, fwhm_y: 3.0, roundness: 1.0, snr: 25.0, background: 0.1 },
        ]
    }

    fn create_shifted_stars(stars: &[Star], dx: f64, dy: f64) -> Vec<Star> {
        stars.iter().map(|s| Star {
            x: s.x + dx,
            y: s.y + dy,
            flux: s.flux,
            peak: s.peak,
            fwhm: s.fwhm,
            fwhm_x: s.fwhm_x,
            fwhm_y: s.fwhm_y,
            roundness: s.roundness,
            snr: s.snr,
            background: s.background,
        }).collect()
    }

    #[test]
    fn test_build_triangles() {
        let stars = create_test_stars();
        let triangles = build_triangles(&stars, 100);

        // With 4 stars, we should get C(4,3) = 4 triangles
        assert_eq!(triangles.len(), 4);

        for tri in &triangles {
            // Ratios should be <= 1 (normalized by longest side)
            assert!(tri.ratios[0] <= 1.0);
            assert!(tri.ratios[1] <= 1.0);
        }
    }

    #[test]
    fn test_match_stars_identical() {
        let ref_stars = create_test_stars();
        let target_stars = ref_stars.clone();

        let matches = match_stars(&ref_stars, &target_stars);

        // Should match all stars
        assert!(matches.len() >= 3);
    }

    #[test]
    fn test_match_stars_shifted() {
        let ref_stars = create_test_stars();
        let target_stars = create_shifted_stars(&ref_stars, 50.0, 30.0);

        let matches = match_stars(&ref_stars, &target_stars);

        // Should still match (triangles are scale/position invariant)
        assert!(matches.len() >= 3);
    }

    #[test]
    fn test_distance() {
        let a = Star { x: 0.0, y: 0.0, flux: 0.0, peak: 0.0, fwhm: 0.0, fwhm_x: 0.0, fwhm_y: 0.0, roundness: 0.0, snr: 0.0, background: 0.0 };
        let b = Star { x: 3.0, y: 4.0, flux: 0.0, peak: 0.0, fwhm: 0.0, fwhm_x: 0.0, fwhm_y: 0.0, roundness: 0.0, snr: 0.0, background: 0.0 };

        assert!((distance(&a, &b) - 5.0).abs() < 1e-6);
    }
}
