//! ASTAP autoFOV ladder.
//!
//! ASTAP, when no scale is known, sweeps the field-of-view coarse→fine,
//! largest first, dividing by 1.5 each rung from 9.5° down to ~0.38°
//! (`command-line_version/unit_command_line_solving.pas:2396` `fov_org:=9.5`,
//! `:2397` `fov_min:=0.38`, `:2405` `fov_org:=fov_org/1.5`; the exit check is
//! at loop *end* so the first rung ≤ fov_min is still tried). "FOV" is the
//! angular size along the image's long axis, so a rung's pixel scale is
//! `fov_deg * 3600 / long_px`.
//!
//! With a usable scale hint the ladder collapses to the implied rung plus
//! one ÷1.5 / ×1.5 neighbour (absorbs a modest scale-hint error, e.g. a
//! mis-binned XPIXSZ), hinted rung first so well-hinted frames stay fast.

/// Largest FOV tried when blind (degrees). ASTAP `:2396`.
const FOV_MAX_DEG: f64 = 9.5;
/// Smallest FOV tried when blind (degrees). ASTAP `:2397`.
const FOV_MIN_DEG: f64 = 0.38;
/// Per-rung divisor. ASTAP `:2405`.
const FOV_DIV: f64 = 1.5;

/// One ladder rung: the long-axis field of view and the pixel scale it
/// implies for this image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FovRung {
    pub fov_deg: f64,
    pub scale_arcsec: f64,
}

fn rung(fov_deg: f64, long_px: usize) -> FovRung {
    FovRung {
        fov_deg,
        scale_arcsec: fov_deg * 3600.0 / long_px.max(1) as f64,
    }
}

/// Generate the FOV rungs to try, in order.
///
/// * `long_px` — the image's longer pixel dimension.
/// * `hint_scale_arcsec` — `Some` when a trustworthy FOCALLEN+XPIXSZ scale
///   is available; the ladder then collapses to ~3 rungs around it.
pub fn fov_rungs(long_px: usize, hint_scale_arcsec: Option<f64>) -> Vec<FovRung> {
    if let Some(s) = hint_scale_arcsec {
        if s.is_finite() && s > 0.0 {
            let fov = s * long_px.max(1) as f64 / 3600.0;
            // Hinted rung first (fast path), then one neighbour each side
            // for scale-hint slop.
            return vec![
                rung(fov, long_px),
                rung(fov / FOV_DIV, long_px),
                rung(fov * FOV_DIV, long_px),
            ];
        }
    }
    // Blind: coarse→fine, push-then-check so the first rung ≤ fov_min is
    // still tried (matches ASTAP's loop-end exit condition).
    let mut rungs = Vec::new();
    let mut fov = FOV_MAX_DEG;
    loop {
        rungs.push(rung(fov, long_px));
        if fov <= FOV_MIN_DEG {
            break;
        }
        fov /= FOV_DIV;
    }
    rungs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blind_ladder_is_coarse_to_fine_div_1_5() {
        let r = fov_rungs(4000, None);
        // 9.5,6.33,4.22,2.81,1.88,1.25,0.83,0.56,0.37 → 9 rungs.
        assert_eq!(r.len(), 9);
        assert!((r[0].fov_deg - 9.5).abs() < 1e-9);
        for w in r.windows(2) {
            assert!((w[0].fov_deg / w[1].fov_deg - 1.5).abs() < 1e-6);
        }
        // Last rung is the first one ≤ fov_min (loop-end exit).
        assert!(r[r.len() - 1].fov_deg <= FOV_MIN_DEG);
        assert!(r[r.len() - 2].fov_deg > FOV_MIN_DEG);
        // scale = fov*3600/long_px.
        assert!((r[0].scale_arcsec - 9.5 * 3600.0 / 4000.0).abs() < 1e-9);
    }

    #[test]
    fn hint_collapses_to_three_rungs_hinted_first() {
        let r = fov_rungs(8288, Some(0.48));
        assert_eq!(r.len(), 3);
        assert!((r[0].scale_arcsec - 0.48).abs() < 1e-9); // hinted first
        let fov0 = r[0].fov_deg;
        assert!((r[1].fov_deg - fov0 / 1.5).abs() < 1e-9);
        assert!((r[2].fov_deg - fov0 * 1.5).abs() < 1e-9);
    }

    #[test]
    fn invalid_hint_falls_back_to_blind() {
        assert_eq!(fov_rungs(4000, Some(0.0)).len(), 9);
        assert_eq!(fov_rungs(4000, Some(f64::NAN)).len(), 9);
    }
}
