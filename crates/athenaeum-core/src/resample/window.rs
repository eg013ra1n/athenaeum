//! The source rows a band of output rows needs. The band's boundary is
//! mapped densely through the inverse map (every 32 px along its four
//! edges), the source row span is expanded by the kernel radius plus one,
//! and clamped to the frame. A span longer than `whole_threshold` of the
//! frame height (a rotation near a quarter turn) reads the whole plane.

use crate::geometry::InverseMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceWindow {
    /// Source rows `[y0, y1)`; empty (`y0 == y1`) when the band maps
    /// entirely outside the source.
    Rows {
        y0: usize,
        y1: usize,
    },
    Whole,
}

/// `radius` must be `interp.radius()` of the kernel the band will be warped
/// with — a smaller value produces a window the warp will silently clamp
/// into (caught by a debug assertion in `Plane::at`).
#[allow(clippy::too_many_arguments)]
pub fn source_window(
    map: &dyn InverseMap,
    out_width: usize,
    y0: usize,
    rows: usize,
    src_width: usize,
    src_height: usize,
    radius: usize,
    whole_threshold: f64,
) -> SourceWindow {
    const STEP: usize = 32;
    let x_max = out_width.saturating_sub(1) as f64;
    let top = y0 as f64 - 0.5;
    let bottom = (y0 + rows) as f64 - 0.5;
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    // Ruling R-T4-6b: one evaluator for the whole probe, not one cache
    // lock per sample.
    let at = map.inverse_burst();
    let mut visit = |x: f64, y: f64| {
        let (_, sy) = at(x, y);
        if sy.is_finite() {
            ymin = ymin.min(sy);
            ymax = ymax.max(sy);
        }
    };
    let mut x = 0.0;
    while x <= x_max {
        visit(x, top);
        visit(x, bottom);
        x += STEP as f64;
    }
    visit(x_max, top);
    visit(x_max, bottom);
    let mut y = top;
    while y <= bottom {
        visit(0.0, y);
        visit(x_max, y);
        y += STEP as f64;
    }
    visit(0.0, bottom);
    visit(x_max, bottom);
    let _ = src_width;
    if !ymin.is_finite() || !ymax.is_finite() {
        return SourceWindow::Whole;
    }
    let margin = radius as f64 + 1.0;
    let lo = (ymin - margin).floor();
    let hi = (ymax + margin).ceil() + 1.0;
    if (hi - lo) > whole_threshold * src_height as f64 {
        return SourceWindow::Whole;
    }
    let y0s = lo.clamp(0.0, src_height as f64) as usize;
    let y1s = hi.clamp(0.0, src_height as f64) as usize;
    SourceWindow::Rows {
        y0: y0s.min(y1s),
        y1: y1s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Linear, LinearKind};

    #[test]
    fn identity_window_is_the_band_plus_margin() {
        let id = Linear::identity();
        let w = source_window(&id, 100, 200, 50, 100, 1000, 2, 0.6);
        // top edge 199.5, bottom 249.5, margin radius+1 = 3: floor(196.5)=196, ceil(252.5)+1=254
        assert_eq!(w, SourceWindow::Rows { y0: 196, y1: 254 });
    }

    #[test]
    fn window_clamps_to_the_frame() {
        let id = Linear::identity();
        assert_eq!(
            source_window(&id, 100, 0, 10, 100, 1000, 3, 0.6),
            SourceWindow::Rows { y0: 0, y1: 15 }
        );
        assert_eq!(
            source_window(&id, 100, 995, 5, 100, 1000, 3, 0.6),
            SourceWindow::Rows { y0: 990, y1: 1000 }
        );
    }

    #[test]
    fn shift_moves_the_window() {
        // inverse map: reference (x, y) → subject (x, y + 40)
        let shift = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 40.0], [0.0, 0.0, 1.0]],
        };
        assert_eq!(
            source_window(&shift, 100, 200, 50, 100, 1000, 0, 0.6),
            SourceWindow::Rows { y0: 238, y1: 292 }
        );
    }

    #[test]
    fn a_quarter_turn_reads_the_whole_frame() {
        // 90° rotation about the centre of a square frame: a band of output
        // rows maps to a band of source COLUMNS, i.e. every source row.
        let (cx, cy) = (500.0, 500.0);
        let rot = Linear {
            kind: LinearKind::Affine,
            m: [[0.0, -1.0, cx + cy], [1.0, 0.0, cy - cx], [0.0, 0.0, 1.0]],
        };
        assert_eq!(
            source_window(&rot, 1000, 400, 50, 1000, 1000, 2, 0.6),
            SourceWindow::Whole
        );
    }

    #[test]
    fn a_band_entirely_outside_the_source_is_empty() {
        let shift = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 5000.0], [0.0, 0.0, 1.0]],
        };
        assert_eq!(
            source_window(&shift, 100, 0, 50, 100, 1000, 2, 0.6),
            SourceWindow::Rows { y0: 1000, y1: 1000 }
        );
    }
}
