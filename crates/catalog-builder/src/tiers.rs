//! Density-limited tier table and per-cell rank-band slicing.

/// Cumulative density targets (stars/deg²) for the 4 additive tiers.
pub const TIER_DENSITIES: [u32; 4] = [500, 2000, 5000, 8000];

/// Full-sphere area / HEALPix-6 cell count = per-cell solid angle in deg².
const CELL_AREA_DEG2: f64 = 41_252.961_25 / 49_152.0;

/// Cumulative per-cell record counts `[0, b1, b2, b3, b4]`. Tier `k` owns the
/// rank band `[counts[k], counts[k+1])`.
pub fn cell_cum_counts() -> [usize; 5] {
    let mut out = [0usize; 5];
    for (i, d) in TIER_DENSITIES.iter().enumerate() {
        out[i + 1] = (*d as f64 * CELL_AREA_DEG2).round() as usize;
    }
    out
}

/// The slice of `records` for rank band `[lo, hi)`, clamped to the data length.
/// Records must be mag-sorted ascending; bands are disjoint and tile the cell.
pub fn slice_select<T>(records: &[T], lo: usize, hi: usize) -> &[T] {
    let end = hi.min(records.len());
    let start = lo.min(end);
    &records[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_match_density_times_cell_area() {
        // cell area = 41252.96125 / 49152 = 0.8392876 deg²
        let c = cell_cum_counts();
        assert_eq!(c, [0, 420, 1679, 4196, 6714]);
    }

    #[test]
    fn slice_select_returns_the_band() {
        let v: Vec<u32> = (0..100).collect();
        assert_eq!(slice_select(&v, 10, 30), &v[10..30]);
    }

    #[test]
    fn slice_select_clamps_to_len_and_is_disjoint() {
        let v: Vec<u32> = (0..15).collect();
        // band beyond the data → empty; bands tile without overlap
        assert_eq!(slice_select(&v, 20, 40), &[] as &[u32]);
        let a = slice_select(&v, 0, 10);
        let b = slice_select(&v, 10, 40); // clamps hi to 15
        assert_eq!(a.len() + b.len(), v.len());
        assert_eq!(b, &v[10..15]);
    }
}
