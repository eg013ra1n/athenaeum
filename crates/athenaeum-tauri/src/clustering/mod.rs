// Frame set clustering by sky coordinates

use crate::coordinates::{
    angular_distance, format_dec_sexagesimal, format_ra_sexagesimal,
    parse_dec_sexagesimal, parse_ra_sexagesimal, spherical_mean,
};
use crate::models::{Frame, ImageType};
use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// Frame with normalized coordinates ready for clustering
#[derive(Debug, Clone)]
pub struct ClusterableFrame {
    pub frame_id: i64,
    pub ra_deg: f64,      // Normalized to [0, 360)
    pub dec_deg: f64,     // Normalized to [-90, 90]
    pub date_obs: Option<String>,
    #[allow(dead_code)]
    pub object: Option<String>,
}

/// A frame set cluster with computed center
#[derive(Debug, Clone)]
pub struct FrameCluster {
    pub name: Option<String>,
    pub center_ra_deg: f64,
    pub center_dec_deg: f64,
    pub objctra: String,     // Sexagesimal format
    pub objctdec: String,    // Sexagesimal format
    pub date_obs: Option<String>,  // Earliest date among members
    pub total_exp_time: Option<f64>,
    pub member_frame_ids: Vec<i64>,
    member_coords: Vec<(f64, f64)>,  // (ra_deg, dec_deg) for spherical mean
}

impl FrameCluster {
    /// Create a new cluster with a single seed frame
    pub fn new_with_seed(seed: &ClusterableFrame) -> Self {
        Self {
            name: None,
            center_ra_deg: seed.ra_deg,
            center_dec_deg: seed.dec_deg,
            objctra: format_ra_sexagesimal(seed.ra_deg),
            objctdec: format_dec_sexagesimal(seed.dec_deg),
            date_obs: seed.date_obs.clone(),
            total_exp_time: None,
            member_frame_ids: vec![seed.frame_id],
            member_coords: vec![(seed.ra_deg, seed.dec_deg)],
        }
    }

    /// Add a frame to this cluster and update the center using spherical mean
    pub fn add_member(&mut self, frame: &ClusterableFrame) {
        self.member_frame_ids.push(frame.frame_id);
        self.member_coords.push((frame.ra_deg, frame.dec_deg));

        // Recompute center as proper spherical mean (handles RA wraparound correctly)
        match spherical_mean(&self.member_coords) {
            Ok((mean_ra, mean_dec)) => {
                self.center_ra_deg = mean_ra;
                self.center_dec_deg = mean_dec;
            }
            Err(_) => {
                // Fallback: keep previous center (extremely unlikely — only if coords are antipodal)
                eprintln!("Warning: spherical_mean failed for cluster, keeping previous center");
            }
        }

        self.objctra = format_ra_sexagesimal(self.center_ra_deg);
        self.objctdec = format_dec_sexagesimal(self.center_dec_deg);

        // Update earliest date
        if let Some(new_date) = &frame.date_obs {
            self.date_obs = match &self.date_obs {
                Some(existing) => {
                    if new_date < existing {
                        Some(new_date.clone())
                    } else {
                        Some(existing.clone())
                    }
                }
                None => Some(new_date.clone()),
            };
        }
    }

    /// Check if a frame is within the threshold distance from this cluster's center
    pub fn is_within_threshold(&self, frame: &ClusterableFrame, threshold_deg: f64) -> bool {
        let distance = angular_distance(
            self.center_ra_deg,
            self.center_dec_deg,
            frame.ra_deg,
            frame.dec_deg,
        );
        distance <= threshold_deg
    }

    /// Determine the name for this cluster based on member objects
    pub fn determine_name(&mut self, frames_map: &HashMap<i64, &Frame>) {
        // Count occurrences of each OBJECT value
        let mut object_counts: HashMap<String, usize> = HashMap::new();

        for frame_id in &self.member_frame_ids {
            if let Some(frame) = frames_map.get(frame_id) {
                if let Some(obj) = &frame.object {
                    *object_counts.entry(obj.clone()).or_insert(0) += 1;
                }
            }
        }

        // Check if any object has a strict majority (> 50%)
        let total = self.member_frame_ids.len();
        let majority_threshold = (total + 1) / 2; // Ceiling of 50%

        let mut max_count = 0;
        let mut majority_object: Option<String> = None;

        for (obj, count) in object_counts {
            if count > max_count {
                max_count = count;
                if count >= majority_threshold {
                    majority_object = Some(obj.clone());
                }
            }
        }

        // Use majority object name or fall back to coordinates
        self.name = majority_object.or_else(|| {
            Some(format!(
                "Unknown @ RA={}, Dec={}",
                format_ra_sexagesimal(self.center_ra_deg),
                format_dec_sexagesimal(self.center_dec_deg)
            ))
        });
    }

    /// Calculate total exposure time from frames
    pub fn calculate_total_exp_time(&mut self, frames_map: &HashMap<i64, &Frame>) {
        let mut total = 0.0;

        for frame_id in &self.member_frame_ids {
            if let Some(frame) = frames_map.get(frame_id) {
                if let Some(exptime) = frame.exptime {
                    total += exptime;
                }
            }
        }

        self.total_exp_time = if total > 0.0 { Some(total) } else { None };
    }
}

/// Normalize coordinates from a Frame to ClusterableFrame
pub fn normalize_frame_coordinates(frame: &Frame) -> Result<ClusterableFrame> {
    // Try to get numeric RA/Dec first
    let (ra_deg, dec_deg) = if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
        (ra, dec)
    } else if let (Some(objctra), Some(objctdec)) = (&frame.objctra, &frame.objctdec) {
        // Parse sexagesimal strings
        let ra = parse_ra_sexagesimal(objctra)
            .map_err(|e| anyhow!("Failed to parse OBJCTRA '{}': {}", objctra, e))?;
        let dec = parse_dec_sexagesimal(objctdec)
            .map_err(|e| anyhow!("Failed to parse OBJCTDEC '{}': {}", objctdec, e))?;
        (ra, dec)
    } else {
        return Err(anyhow!(
            "Frame {} has no valid coordinates (RA/DEC or OBJCTRA/OBJCTDEC)",
            frame.id.unwrap_or(-1)
        ));
    };

    Ok(ClusterableFrame {
        frame_id: frame.id.ok_or_else(|| anyhow!("Frame has no ID"))?,
        ra_deg,
        dec_deg,
        date_obs: frame.date_obs.as_ref().map(|dt| dt.to_rfc3339()),
        object: frame.object.clone(),
    })
}

/// Sort frames deterministically by RA → Dec → DATE-OBS
pub fn sort_frames_deterministic(frames: &mut [ClusterableFrame]) {
    frames.sort_by(|a, b| {
        // First by RA
        match a.ra_deg.partial_cmp(&b.ra_deg) {
            Some(std::cmp::Ordering::Equal) => {
                // Then by Dec
                match a.dec_deg.partial_cmp(&b.dec_deg) {
                    Some(std::cmp::Ordering::Equal) => {
                        // Finally by DATE-OBS
                        a.date_obs.cmp(&b.date_obs)
                    }
                    other => other.unwrap_or(std::cmp::Ordering::Equal),
                }
            }
            other => other.unwrap_or(std::cmp::Ordering::Equal),
        }
    });
}

/// Cluster frames using seed-and-grow algorithm
/// Returns vector of FrameCluster
pub fn cluster_frames(
    frames: Vec<ClusterableFrame>,
    threshold_deg: f64,
) -> Result<Vec<FrameCluster>> {
    if frames.is_empty() {
        return Ok(Vec::new());
    }

    let mut sorted_frames = frames;
    sort_frames_deterministic(&mut sorted_frames);

    let mut clusters: Vec<FrameCluster> = Vec::new();
    let mut assigned = vec![false; sorted_frames.len()];

    // Iterate through sorted frames to create clusters
    for (seed_idx, seed_frame) in sorted_frames.iter().enumerate() {
        if assigned[seed_idx] {
            continue;
        }

        // Create new cluster with this seed
        let mut cluster = FrameCluster::new_with_seed(seed_frame);
        assigned[seed_idx] = true;

        // Grow the cluster by finding nearby unassigned frames
        let mut added_any = true;
        while added_any {
            added_any = false;

            for (candidate_idx, candidate_frame) in sorted_frames.iter().enumerate() {
                if assigned[candidate_idx] {
                    continue;
                }

                if cluster.is_within_threshold(candidate_frame, threshold_deg) {
                    cluster.add_member(candidate_frame);
                    assigned[candidate_idx] = true;
                    added_any = true;
                }
            }
        }

        clusters.push(cluster);
    }

    Ok(clusters)
}

/// Filter frames for clustering: only LIGHT frames with valid coordinates
pub fn filter_frames_for_clustering(frames: Vec<(i64, Frame)>) -> (Vec<ClusterableFrame>, Vec<(i64, String)>) {
    let mut clusterable = Vec::new();
    let mut excluded = Vec::new();

    for (file_id, frame) in frames {
        // Check if LIGHT frame
        if frame.imagetyp != Some(ImageType::Light) {
            excluded.push((file_id, format!("Not a LIGHT frame (IMAGETYP: {:?})", frame.imagetyp)));
            continue;
        }

        // Try to normalize coordinates
        match normalize_frame_coordinates(&frame) {
            Ok(cf) => clusterable.push(cf),
            Err(e) => excluded.push((file_id, format!("Invalid coordinates: {}", e))),
        }
    }

    (clusterable, excluded)
}

/// Complete clustering workflow: filter, cluster, name, and calculate metadata
pub fn auto_generate_frame_sets(
    frames_with_ids: Vec<(i64, Frame)>,
    threshold_deg: f64,
) -> Result<(Vec<FrameCluster>, Vec<(i64, String)>)> {
    // Filter eligible frames
    let (clusterable_frames, excluded) = filter_frames_for_clustering(frames_with_ids.clone());

    if clusterable_frames.is_empty() {
        return Ok((Vec::new(), excluded));
    }

    // Create a map for quick frame lookup
    let frames_map: HashMap<i64, &Frame> = frames_with_ids
        .iter()
        .map(|(id, frame)| (*id, frame))
        .collect();

    // Perform clustering
    let mut clusters = cluster_frames(clusterable_frames, threshold_deg)?;

    // Determine names and calculate metadata for each cluster
    for cluster in &mut clusters {
        cluster.determine_name(&frames_map);
        cluster.calculate_total_exp_time(&frames_map);
    }

    Ok((clusters, excluded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_test_frame(id: i64, ra: f64, dec: f64, object: Option<&str>) -> Frame {
        Frame {
            id: Some(id),
            file_id: id,
            object: object.map(String::from),
            date_obs: Some(Utc::now()),
            telescop: None,
            instrume: None,
            exptime: Some(60.0),
            filter: None,
            imagetyp: Some(ImageType::Light),
            is_master: false,
            gain: None,
            offset: None,
            binning: None,
            xbinning: None,
            ybinning: None,
            ccd_temp: None,
            set_temp: None,
            focallen: None,
            xpixsz: None,
            ypixsz: None,
            naxis1: None,
            naxis2: None,
            ra: Some(ra),
            dec: Some(dec),
            sitelat: None,
            lat_obs: None,
            sitelong: None,
            long_obs: None,
            objctra: None,
            objctdec: None,
            override_: false,
            swcreate: None,
            bayerpat: None,
            rotation: None,
        }
    }

    #[test]
    fn test_sort_frames_deterministic() {
        let mut frames = vec![
            ClusterableFrame {
                frame_id: 3,
                ra_deg: 180.0,
                dec_deg: 0.0,
                date_obs: Some("2024-01-03T00:00:00Z".to_string()),
                object: None,
            },
            ClusterableFrame {
                frame_id: 1,
                ra_deg: 0.0,
                dec_deg: 0.0,
                date_obs: Some("2024-01-01T00:00:00Z".to_string()),
                object: None,
            },
            ClusterableFrame {
                frame_id: 2,
                ra_deg: 0.0,
                dec_deg: 45.0,
                date_obs: Some("2024-01-02T00:00:00Z".to_string()),
                object: None,
            },
        ];

        sort_frames_deterministic(&mut frames);

        assert_eq!(frames[0].frame_id, 1); // RA=0, Dec=0
        assert_eq!(frames[1].frame_id, 2); // RA=0, Dec=45
        assert_eq!(frames[2].frame_id, 3); // RA=180
    }

    #[test]
    fn test_cluster_frames_simple() {
        // Two frames close together, one far away
        let frames = vec![
            ClusterableFrame {
                frame_id: 1,
                ra_deg: 180.0,
                dec_deg: 0.0,
                date_obs: None,
                object: Some("M31".to_string()),
            },
            ClusterableFrame {
                frame_id: 2,
                ra_deg: 180.05,
                dec_deg: 0.05,
                date_obs: None,
                object: Some("M31".to_string()),
            },
            ClusterableFrame {
                frame_id: 3,
                ra_deg: 0.0,
                dec_deg: 0.0,
                date_obs: None,
                object: Some("M33".to_string()),
            },
        ];

        let clusters = cluster_frames(frames, 0.1).unwrap();

        // Should have 2 clusters
        assert_eq!(clusters.len(), 2);

        // First cluster should have frames 3 (sorted first by RA)
        assert_eq!(clusters[0].member_frame_ids.len(), 1);
        assert_eq!(clusters[0].member_frame_ids[0], 3);

        // Second cluster should have frames 1 and 2
        assert_eq!(clusters[1].member_frame_ids.len(), 2);
        assert!(clusters[1].member_frame_ids.contains(&1));
        assert!(clusters[1].member_frame_ids.contains(&2));
    }

    #[test]
    fn test_cluster_naming() {
        let frames_with_ids = vec![
            (1, make_test_frame(1, 180.0, 0.0, Some("M31"))),
            (2, make_test_frame(2, 180.01, 0.01, Some("M31"))),
            (3, make_test_frame(3, 180.02, 0.02, Some("M31"))),
            (4, make_test_frame(4, 0.0, 0.0, Some("M33"))),
        ];

        let (clusters, _) = auto_generate_frame_sets(frames_with_ids, 0.1).unwrap();

        assert_eq!(clusters.len(), 2);

        // First cluster (M31) should have majority name
        let m31_cluster = &clusters.iter().find(|c| c.member_frame_ids.len() == 3).unwrap();
        assert_eq!(m31_cluster.name, Some("M31".to_string()));

        // Second cluster (M33) should have its name
        let m33_cluster = &clusters.iter().find(|c| c.member_frame_ids.len() == 1).unwrap();
        assert_eq!(m33_cluster.name, Some("M33".to_string()));
    }

    #[test]
    fn test_cluster_center_ra_wraparound() {
        // Two frames straddling RA=0h boundary — arithmetic mean would give ~180°
        let frames = vec![
            ClusterableFrame {
                frame_id: 1,
                ra_deg: 359.0,
                dec_deg: 45.0,
                date_obs: None,
                object: None,
            },
            ClusterableFrame {
                frame_id: 2,
                ra_deg: 1.0,
                dec_deg: 45.0,
                date_obs: None,
                object: None,
            },
        ];
        let clusters = cluster_frames(frames, 5.0).unwrap();
        assert_eq!(clusters.len(), 1);
        let center_ra = clusters[0].center_ra_deg;
        // Center should be near 0°, NOT near 180°
        assert!(
            center_ra > 355.0 || center_ra < 5.0,
            "Cluster center RA={} should be near 0°, not 180°",
            center_ra
        );
    }
}
