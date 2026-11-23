//! The light-calibration parameter types: `LightCalParams`, `FlatNormMode`, `BiasFallback` and
//! `TRIMMED_MEAN_TAIL_FRACTION`. Plain data, ungated; the calibration math that reads them is the `engine` layer
//! (`calibration_library::light_cal`). Every type serializes to the camelCase JSON the caller stores and sends (one variant keeps its pre-rename wire
//! string).

/// Fraction of pixels discarded from EACH tail by the whole-frame trimmed mean
/// (`FlatNormMode::TrimmedMeanWhole`). Matches the ERP's image-calibration flat
/// scale clipping factor of 0.05, identified empirically against the ERP's own
/// arithmetic to 1.7e-6 relative (spec §2). The trim indices
/// are `lo = floor(n * TRIMMED_MEAN_TAIL_FRACTION)`, `hi = n − floor(n * TRIMMED_MEAN_TAIL_FRACTION)`
/// and the statistic is the f64 mean of `sorted[lo..hi]` — exactness is the
/// point, so this lives in exactly one place.
pub const TRIMMED_MEAN_TAIL_FRACTION: f64 = 0.05;

/// Which statistic normalizes the master flat when normalization is ON
/// (spec §2, "Normalization statistic is selectable"). Applies only when
/// `flat_norm` is on; recorded in the tracking row so a mode change makes a
/// flat-applied frame stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum FlatNormMode {
    /// The default: the mean of the flat's central third, read from the normalization constant stamped on the
    /// master flat and recomputed on the fly when no constant is stamped.
    #[default]
    CentralThird,
    /// ERP-compatible: a two-sided trimmed mean over the WHOLE frame,
    /// discarding [`TRIMMED_MEAN_TAIL_FRACTION`] of the pixels from each tail.
    /// Always computed from the flat file; a stamped normalization constant is ignored.
    ///
    /// Wire/storage compatibility: the persisted string stays the pre-rename
    /// one (stacking configs in the catalog, the `flat_norm_mode` DB column,
    /// the frontend's saved preference, the sync request), so an older app
    /// version still reads what this one writes. The new name
    /// (`trimmedMeanWhole`) is accepted on read as an alias.
    #[serde(rename = "pixinsightTrimmed", alias = "trimmedMeanWhole")]
    TrimmedMeanWhole,
}

impl FlatNormMode {
    /// The over-the-wire / stored string for this mode — identical to the serde
    /// camelCase representation and to the `flat_norm_mode` DB column values.
    /// Kept as a `&'static str` so a consumer can compare a stored/stamped mode
    /// against a wanted mode without a serde round-trip.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            FlatNormMode::CentralThird => "centralThird",
            FlatNormMode::TrimmedMeanWhole => "pixinsightTrimmed",
        }
    }
}

/// What to do for a LIGHT frame that has NO dark master (spec §2 "Advanced
/// parameters"). Default = current behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum BiasFallback {
    /// Current behavior: subtract the linked master bias (`(L − B)`).
    #[default]
    SubtractBias,
    /// Refuse bias-only calibration — a light with no dark master is a per-frame
    /// failure ("no dark master (bias fallback disabled)"), no output written.
    SkipFrame,
}

/// serde default for [`LightCalParams::trim_fraction`] — the current per-tail
/// discard fraction ([`TRIMMED_MEAN_TAIL_FRACTION`] = 0.05), so an omitted wire field or a
/// stored `'{}'` decodes to today's behavior.
fn default_trim_fraction() -> f64 {
    TRIMMED_MEAN_TAIL_FRACTION
}

/// serde default for [`LightCalParams::cfa_flat_scaling`] — ON, so an omitted
/// wire field decodes to the recommended behavior for colour sensors. NOTE the
/// asymmetry with the other defaults: a stored/stamped `'{}'` written before
/// this field existed also decodes to `true`, which is NOT what it was
/// calibrated with. `cal_params` therefore records what was REQUESTED; what was
/// APPLIED is reported separately (a stamp on the calibrated output).
fn default_true() -> bool {
    true
}

/// Advanced per-run light-calibration parameters (spec §2 "Advanced
/// parameters"). Every field is optional on the wire (`#[serde(default)]`) with
/// a default equal to the current behavior, so an omitted field — or a
/// pre-feature `'{}'` — decodes to [`LightCalParams::default`].
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LightCalParams {
    /// Per-tail discard fraction for the `TrimmedMeanWhole` statistic
    /// (default [`TRIMMED_MEAN_TAIL_FRACTION`] = 0.05). Only meaningful when the flat is
    /// normalized in `TrimmedMeanWhole` mode; the caller stamps it on the output then.
    #[serde(default = "default_trim_fraction")]
    pub trim_fraction: f64,
    /// DN added to the output AFTER the scale divide (`out += pedestal_dn /
    /// OUTPUT_SCALE_DIVISOR`, default 0 = off), for consumers that clip
    /// negatives. The caller always stamps the DN value on the output; the calibration status
    /// is unchanged — a pedestal is not a calibration step.
    #[serde(default)]
    pub pedestal_dn: f64,
    /// What to do for a light with no dark master (default `subtractBias`).
    #[serde(default)]
    pub bias_fallback: BiasFallback,
    /// Normalize a master flat per CFA channel instead of by one whole-frame
    /// constant (default ON). Only ever acts on a light that declares a CFA
    /// pattern: a mono light has no channels to separate, so the flag is
    /// inert there and the frame is calibrated exactly as before.
    ///
    /// Ignored in [`FlatNormMode::TrimmedMeanWhole`], which is a
    /// tool-parity statistic over the whole frame — splitting it per channel
    /// would no longer be the statistic it promises. That combination logs a
    /// `debug!` and normalizes globally.
    #[serde(default = "default_true")]
    pub cfa_flat_scaling: bool,
}

impl Default for LightCalParams {
    fn default() -> Self {
        Self {
            trim_fraction: TRIMMED_MEAN_TAIL_FRACTION,
            pedestal_dn: 0.0,
            bias_fallback: BiasFallback::SubtractBias,
            cfa_flat_scaling: true,
        }
    }
}
