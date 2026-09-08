//! A static 2-D KD-tree for nearest-neighbour lookups within a radius —
//! replaces the O(N·M) scan in frame-to-frame correspondence. Built once
//! per reference star list (≤ a few thousand points); queries are O(log n).

pub struct KdTree2 {
    /// Points in tree order; `idx[i]` is the original index of `pts[i]`.
    pts: Vec<(f64, f64)>,
    idx: Vec<usize>,
}

impl KdTree2 {
    pub fn build(points: &[(f64, f64)]) -> KdTree2 {
        let mut idx: Vec<usize> = (0..points.len()).collect();
        let mut pts: Vec<(f64, f64)> = points.to_vec();
        Self::build_rec(&mut pts, &mut idx, 0);
        KdTree2 { pts, idx }
    }

    pub fn len(&self) -> usize {
        self.pts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }

    /// Recursively arrange `[lo, hi)` so the median on the split axis sits
    /// in the middle, alternating axes by depth. Implicit balanced tree:
    /// node = middle index of its range.
    fn build_rec(pts: &mut [(f64, f64)], idx: &mut [usize], depth: usize) {
        let n = pts.len();
        if n <= 1 {
            return;
        }
        let axis = depth % 2;
        let mid = n / 2;
        // Selection by axis: sort the pair arrays together (n log n per
        // level is fine at these sizes and keeps the code obvious).
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            let ka = if axis == 0 { pts[a].0 } else { pts[a].1 };
            let kb = if axis == 0 { pts[b].0 } else { pts[b].1 };
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
        let sorted_pts: Vec<_> = order.iter().map(|&o| pts[o]).collect();
        let sorted_idx: Vec<_> = order.iter().map(|&o| idx[o]).collect();
        pts.copy_from_slice(&sorted_pts);
        idx.copy_from_slice(&sorted_idx);
        let (left_p, right_p) = pts.split_at_mut(mid);
        let (left_i, right_i) = idx.split_at_mut(mid);
        Self::build_rec(left_p, left_i, depth + 1);
        Self::build_rec(&mut right_p[1..], &mut right_i[1..], depth + 1);
    }

    /// Nearest point within `radius` (inclusive), as `(original index,
    /// distance)`.
    pub fn nearest_within(&self, x: f64, y: f64, radius: f64) -> Option<(usize, f64)> {
        if self.pts.is_empty() {
            return None;
        }
        let mut best: Option<(usize, f64)> = None;
        let mut best_d2 = radius * radius;
        self.search(0, self.pts.len(), 0, x, y, &mut best, &mut best_d2);
        best.map(|(i, d2)| (i, d2.sqrt()))
    }

    #[allow(clippy::too_many_arguments)]
    fn search(
        &self,
        lo: usize,
        hi: usize,
        depth: usize,
        x: f64,
        y: f64,
        best: &mut Option<(usize, f64)>,
        best_d2: &mut f64,
    ) {
        if lo >= hi {
            return;
        }
        let mid = lo + (hi - lo) / 2;
        let (px, py) = self.pts[mid];
        let d2 = (px - x).powi(2) + (py - y).powi(2);
        if d2 <= *best_d2 && best.map(|b| d2 < b.1).unwrap_or(true) {
            *best = Some((self.idx[mid], d2));
            *best_d2 = d2;
        }
        let axis = depth % 2;
        let diff = if axis == 0 { x - px } else { y - py };
        let (near, far) = if diff < 0.0 {
            ((lo, mid), (mid + 1, hi))
        } else {
            ((mid + 1, hi), (lo, mid))
        };
        self.search(near.0, near.1, depth + 1, x, y, best, best_d2);
        if diff * diff <= *best_d2 {
            self.search(far.0, far.1, depth + 1, x, y, best, best_d2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut s = seed;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % 1_000_000) as f64 / 1_000_000.0
        };
        (0..n).map(|_| (next() * 6000.0, next() * 4000.0)).collect()
    }

    fn brute(pts: &[(f64, f64)], x: f64, y: f64, r: f64) -> Option<(usize, f64)> {
        let mut best: Option<(usize, f64)> = None;
        for (i, (px, py)) in pts.iter().enumerate() {
            let d = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            if d <= r && best.map(|b| d < b.1).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        best
    }

    #[test]
    fn nearest_within_matches_brute_force() {
        let pts = points(2000, 77);
        let tree = KdTree2::build(&pts);
        assert_eq!(tree.len(), 2000);
        for (qx, qy) in points(500, 99) {
            let a = tree.nearest_within(qx, qy, 40.0);
            let b = brute(&pts, qx, qy, 40.0);
            match (a, b) {
                (None, None) => {}
                (Some((ia, da)), Some((ib, db))) => {
                    assert!((da - db).abs() < 1e-9, "distance mismatch");
                    // Ties at the same distance are legal either way.
                    if ia != ib {
                        assert!((da - db).abs() < 1e-9);
                    }
                }
                other => panic!("mismatch at ({qx},{qy}): {other:?}"),
            }
        }
    }

    #[test]
    fn empty_tree_returns_none() {
        let tree = KdTree2::build(&[]);
        assert!(tree.nearest_within(1.0, 1.0, 10.0).is_none());
    }

    #[test]
    fn radius_is_inclusive_and_exact() {
        let tree = KdTree2::build(&[(0.0, 0.0), (10.0, 0.0)]);
        assert_eq!(tree.nearest_within(4.0, 0.0, 4.0).unwrap().0, 0);
        assert!(tree.nearest_within(5.0, 0.0, 4.9).is_none());
        assert_eq!(tree.nearest_within(7.0, 0.0, 4.0).unwrap().0, 1);
    }
}
