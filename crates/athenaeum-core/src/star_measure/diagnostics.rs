//! Every seed's outcome (spec 2026-10-07 §3 F-0, ruling F13): a seed is either accepted as a star or refused for a
//! counted reason, never silently dropped. [`RefusalCounts`] is the always-on summary every plane carries
//! (`stars_refused` and its per-reason counts in the stacking adapter's `ChannelMeasurement`); [`FitDiagnostics`] is
//! the per-seed record a caller asks for through `MeasureOptions::diagnostics` (the lab's `star_probe
//! --diagnostics`) — the run never does, the vector costs memory on 24 576 seeds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Why a seed was not accepted. Serialized as its camelCase name ([`RefusalReason::name`]); the variants a later
/// task introduces are declared now so the names never move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefusalReason {
    /// The width-start box (radius `⌈2·F_field⌉`, Task 6b) leaves the image.
    OutsideImage,
    /// Fewer than 10 finite pixels in the region.
    TooFewPixels,
    /// The core is darker than the ring (Task 6b's donut test).
    Donut,
    /// The fitter returned `None` (α < 0.3, A ≤ 0, β out of bounds), or returned a fit with `A ≤ 0` or an axis
    /// `α ≤ 0`.
    FitFailed,
    /// A fitted parameter or the fit residual is not finite.
    NonFinite,
    /// The RMS residual reaches `FitParams::max_fit_residual` of the amplitude: the fit explains nothing.
    ResidualTooLarge,
    /// The fitted centre left the seed by more than `FitParams::centroid_tolerance_px` on an axis.
    CentreMoved,
    /// The fitted centre lies outside the sampling region shrunk by `FitParams::inner_margin` per side.
    CentreAtRim,
    /// The sampling region cannot hold the aperture: `0.5·k·max(FWTM) > r`.
    RegionTooSmall,
    /// `min(fwhm_x, fwhm_y) < 0.8` px (Task 6b; a hot pixel that got a seed).
    NarrowerThanPixel,
    /// A brighter accepted fit lies within ±1 px (the duplicate drop).
    Duplicate,
}

impl RefusalReason {
    /// Every reason, in declaration (and `Ord`) order.
    pub const ALL: [RefusalReason; 11] = [
        RefusalReason::OutsideImage,
        RefusalReason::TooFewPixels,
        RefusalReason::Donut,
        RefusalReason::FitFailed,
        RefusalReason::NonFinite,
        RefusalReason::ResidualTooLarge,
        RefusalReason::CentreMoved,
        RefusalReason::CentreAtRim,
        RefusalReason::RegionTooSmall,
        RefusalReason::NarrowerThanPixel,
        RefusalReason::Duplicate,
    ];

    /// The serialized (camelCase) name: the key of `ChannelMeasurement::refused_by_reason` and the `reason` of a
    /// probe `refused` line.
    pub fn name(self) -> &'static str {
        match self {
            RefusalReason::OutsideImage => "outsideImage",
            RefusalReason::TooFewPixels => "tooFewPixels",
            RefusalReason::Donut => "donut",
            RefusalReason::FitFailed => "fitFailed",
            RefusalReason::NonFinite => "nonFinite",
            RefusalReason::ResidualTooLarge => "residualTooLarge",
            RefusalReason::CentreMoved => "centreMoved",
            RefusalReason::CentreAtRim => "centreAtRim",
            RefusalReason::RegionTooSmall => "regionTooSmall",
            RefusalReason::NarrowerThanPixel => "narrowerThanPixel",
            RefusalReason::Duplicate => "duplicate",
        }
    }
}

/// Every seed's outcome (ruling F13): `NotSelected` and `Trimmed` are Budget-mode outcomes (Task 12), never seen
/// in `Mode::All`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Accepted: `star` indexes the plane's accepted fits (`PlaneMeasurement::stars`, after the duplicate drop).
    Accepted {
        star: usize,
    },
    Refused(RefusalReason),
    NotSelected,
    Trimmed,
}

/// One seed's record (`MeasureOptions::diagnostics`). The fields a refusal leaves unmeasured read their "not
/// reached" value: `region_radius` the width-start box's radius when the seed was refused before its region grew
/// (`OutsideImage`), `width_start` and `background_start` NaN before they were measured, `growth_steps` 0,
/// `converged` false when the fitter never returned a fit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitDiagnostics {
    /// The seed's index in the seed list the fitter was handed (the probe's seed line `i`).
    pub seed: usize,
    pub outcome: Outcome,
    /// The sampling region's radius, px (the square is `(2r+1)²`).
    pub region_radius: u16,
    /// The background the fit started from: the median of the sampling region's outer 1-px rim (Task 6b, F-7),
    /// working units.
    pub background_start: f64,
    /// The FWHM (px) the fit started from: the star's own `F_start`, from its second moments in the width-start box
    /// (Task 6b, PA-12).
    pub width_start: f64,
    /// Steps the sampling region grew from its start radius.
    pub growth_steps: u8,
    /// The region's present pixels — finite and unmasked — the fit was handed (Task 6a); 0 when the seed was refused
    /// before its region was gathered (`OutsideImage`).
    pub pixels_used: u32,
    /// `None` until the rustafits entry point reports it (Task 9).
    pub iterations: Option<u16>,
    /// The fitter's own convergence flag (not an acceptance criterion; reported only).
    pub converged: bool,
}

/// The refused seeds of one fit call, by reason, and the two Budget-mode outcomes (Task 12; 0 in `Mode::All`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RefusalCounts {
    pub by_reason: BTreeMap<RefusalReason, usize>,
    pub not_selected: usize,
    pub trimmed: usize,
}

impl RefusalCounts {
    /// Refused seeds only (the F-0 count).
    pub fn total(&self) -> usize {
        self.by_reason.values().sum()
    }

    pub fn add(&mut self, r: RefusalReason) {
        *self.by_reason.entry(r).or_insert(0) += 1;
    }

    /// `accepted + refused + not_selected + trimmed` — the identity's left-hand side (ruling F13).
    pub fn accounted(&self, accepted: usize) -> usize {
        accepted + self.total() + self.not_selected + self.trimmed
    }

    /// `by_reason` keyed by [`RefusalReason::name`] (the stacking adapter's `refused_by_reason`).
    pub fn by_name(&self) -> BTreeMap<String, usize> {
        self.by_reason
            .iter()
            .map(|(r, n)| (r.name().to_string(), *n))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pin (ruling F28): `name` is the serde name of every reason, so the adapter's string keys and the probe's
    /// `reason` field read exactly what serde writes.
    #[test]
    fn reason_names_are_the_serde_names() {
        for r in RefusalReason::ALL {
            assert_eq!(
                serde_json::to_value(r).unwrap(),
                serde_json::Value::String(r.name().to_string())
            );
            let back: RefusalReason = serde_json::from_str(&format!("\"{}\"", r.name())).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn counts_total_accounted_and_round_trip() {
        let mut c = RefusalCounts::default();
        assert_eq!((c.total(), c.accounted(7)), (0, 7));
        c.add(RefusalReason::RegionTooSmall);
        c.add(RefusalReason::RegionTooSmall);
        c.add(RefusalReason::Duplicate);
        c.not_selected = 4;
        c.trimmed = 1;
        assert_eq!(c.total(), 3);
        assert_eq!(c.accounted(10), 10 + 3 + 4 + 1);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(
            json,
            "{\"byReason\":{\"regionTooSmall\":2,\"duplicate\":1},\"notSelected\":4,\"trimmed\":1}"
        );
        assert_eq!(serde_json::from_str::<RefusalCounts>(&json).unwrap(), c);
        assert_eq!(
            serde_json::from_str::<RefusalCounts>("{}").unwrap(),
            RefusalCounts::default()
        );
        let names = c.by_name();
        assert_eq!(names.get("regionTooSmall"), Some(&2));
        assert_eq!(names.get("duplicate"), Some(&1));
    }
}
