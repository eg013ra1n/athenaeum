//! The local-normalization (LN) model and its reference. The kernel — grids, background model, star selection,
//! calibration, the relative-scale fit and the reference build — comes from starloom, under its old paths; this module
//! keeps the reference's artifact writers (`reference`), which stamp Athenaeum's scanner-skip cards.

pub mod reference;

// By name: the kernel's own `write_reference` and `write_reference_saturation` write no Athenaeum cards and stay out; the
// two wrappers in `reference` carry them. The SL-17 hook module sits on its own line.
pub use starloom::stacking::ln::{
    accumulate_reference_saturation, aperture, aperture_multiple, background, build_reference,
    build_reference_saturation, calibration, calibration_k, circle_flux, effective_scale_mode, eroded_mask, grid,
    ln_fit_options, measure_seeds_calibration, normalize_frame, rank_candidates, read_reference,
    read_reference_saturation, read_reference_saturation_masks, reference_saturation_mask, relative_scale,
    relative_scale_against, relative_scale_against_with_diag, relative_scale_from_seeds,
    relative_scale_from_seeds_with_breakdown, relative_scale_from_seeds_with_diag, resolve_scale, run_jobs_serial,
    scale, scale_rule, select_calibration_frames, selection, shared_refit, snap_ln_scale, ApertureSide,
    BackgroundModel, BackgroundRefusal, Candidates, CalibrationMeasurement, ChannelCalibration, ChannelOutcome,
    CleaningParams, FiniteMask, LnError, LnFrameGrids, LnFrameOutcome, LnGrid, LnOverlap, LnReference,
    LnReferenceForDetection, LnScaleSeeds, PairSurfaces, PreparedReferenceChannel, RunJobs, SaturationMask,
    ScaleInputs, ScaleMatchDiag, ScaleResult, SeedFilterBreakdown, SeedsCalibration, SelectionRule, SelectionSide,
    Surface, TileNoise, TileStat, TileTable, CLEANING, GROWTH_OUTER_FWHM, GROWTH_PAIRS, LEGACY_DEFAULT_LN_SCALE,
    LN_BARYCENTRE_PASS_THRESHOLD, LN_LOCAL_SCALE_MAX_DEVIATION, LN_LOCAL_SCALE_MIN_STARS,
    LN_LOCAL_SCALE_SMOOTHING_SIGMAS, LN_MASK_MARGIN_PX, LN_MIN_OVERLAP, LN_MODEL_VERSION, LN_SCALES, MAX_GRID_SIZE,
    MIN_GRID_SIZE, SEEDS_CALIBRATION_BAND, SEEDS_CALIBRATION_FRAMES, SEEDS_CALIBRATION_MIN_RATIOS, SKY_INNER_FWHM,
    SKY_OUTER_FWHM,
};
#[cfg(feature = "test-support")]
pub use starloom::stacking::ln::fallback_counters;

// The shim rule: a glob import is shadowed only by an item of the same name in this module, and the kernel's `ln`
// re-exports its own `reference::{write_reference, write_reference_saturation}`. These two names are Athenaeum's
// (`reference.rs`), so they are named explicitly here; an explicit import beats the glob.
pub use reference::{write_reference, write_reference_saturation};
