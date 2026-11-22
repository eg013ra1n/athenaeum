//! The LN scale (spec 2026-10-04 §2, rulings L-7…L-10): one function for
//! the plan and the run.

use crate::stacking::integrate::{LnScaleMode, LocalNormalizationConfig};

/// The supported LN scales (L-7): the reference pipeline's list from 256 up.
/// Below 256 a knot interval would hold fewer than 8 tiles.
pub const LN_SCALES: [u32; 11] = [256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192];
pub const MIN_GRID_SIZE: u32 = 2;
pub const MAX_GRID_SIZE: u32 = 8;
/// The pre-2026-10 default `scale`; a legacy document carrying it reads as
/// auto (L-9).
pub const LEGACY_DEFAULT_LN_SCALE: u32 = 1024;

/// The nearest list value, ties to the smaller (L-8); below the list 256,
/// above it 8192.
pub fn snap_ln_scale(v: f64) -> u32 {
    let mut best = LN_SCALES[0];
    for &s in &LN_SCALES[1..] {
        if (s as f64 - v).abs() < (best as f64 - v).abs() {
            best = s;
        }
    }
    best
}

/// The mode a config means: its own, or L-9's reading of a document that
/// never went through `resolve_config` (a plan override, a decoded preset).
pub fn effective_scale_mode(local: &LocalNormalizationConfig) -> LnScaleMode {
    local
        .scale_mode
        .unwrap_or(if local.scale == LEGACY_DEFAULT_LN_SCALE {
            LnScaleMode::Auto
        } else {
            LnScaleMode::Fixed
        })
}

/// The LN scale for a `width × height` LN geometry (L-10). Every consumer
/// reads the value this returns; nothing else reads `local.scale`.
pub fn resolve_scale(local: &LocalNormalizationConfig, width: usize, height: usize) -> u32 {
    match effective_scale_mode(local) {
        LnScaleMode::Fixed => snap_ln_scale(local.scale as f64),
        LnScaleMode::Auto => {
            let grid = local.grid_size.clamp(MIN_GRID_SIZE, MAX_GRID_SIZE) as f64;
            snap_ln_scale(width.min(height) as f64 / grid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::integrate::{LnScaleMode, LocalNormalizationConfig};

    #[test]
    fn snap_follows_the_nearest_rule_with_ties_to_the_smaller() {
        for (v, want) in [
            (1597.0, 1536),
            (1044.0, 1024),
            (1120.0, 1024),
            (880.0, 768),
            (752.0, 768),
            (1280.0, 1024),
            (640.0, 512),
            (100.0, 256),
            (256.0, 256),
            (9000.0, 8192),
            (8192.0, 8192),
            (1300.0, 1536),
        ] {
            assert_eq!(snap_ln_scale(v), want, "{v}");
        }
    }

    /// Spec §2.1's table.
    #[test]
    fn resolve_scale_matches_the_spec_table() {
        let auto = LocalNormalizationConfig {
            scale_mode: Some(LnScaleMode::Auto),
            grid_size: 4,
            ..Default::default()
        };
        for ((w, h), want) in [
            ((9576, 6388), 1536),
            ((6248, 4176), 1024),
            ((4480, 5760), 1024),
            ((4656, 3520), 768),
            ((3008, 3008), 768),
        ] {
            assert_eq!(resolve_scale(&auto, w, h), want, "{w} × {h}");
        }
    }

    #[test]
    fn fixed_mode_snaps_and_grid_size_is_clamped() {
        let fixed = LocalNormalizationConfig {
            scale_mode: Some(LnScaleMode::Fixed),
            scale: 2000,
            ..Default::default()
        };
        assert_eq!(resolve_scale(&fixed, 9576, 6388), 2048);
        let wide = LocalNormalizationConfig {
            scale_mode: Some(LnScaleMode::Auto),
            grid_size: 50,
            ..Default::default()
        };
        assert_eq!(
            resolve_scale(&wide, 9576, 6388),
            snap_ln_scale(6388.0 / 8.0)
        );
    }

    /// L-9 for a config that never went through `resolve_config` (a plan
    /// override, a test literal): `None` reads 1024 as auto, anything else
    /// as fixed.
    #[test]
    fn an_unresolved_mode_follows_the_legacy_rule() {
        let legacy_default = LocalNormalizationConfig {
            scale_mode: None,
            scale: 1024,
            ..Default::default()
        };
        assert_eq!(effective_scale_mode(&legacy_default), LnScaleMode::Auto);
        let legacy_custom = LocalNormalizationConfig {
            scale_mode: None,
            scale: 2048,
            ..Default::default()
        };
        assert_eq!(effective_scale_mode(&legacy_custom), LnScaleMode::Fixed);
        assert_eq!(resolve_scale(&legacy_custom, 9576, 6388), 2048);
    }
}
