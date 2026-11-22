//! The version tripwire (spec SL-19): the stage versions that enter a hash or a file header are starloom
//! constants. A starloom bump changes output bits, so Athenaeum accepts it on purpose: this test names each value
//! the application was last checked against.

use starloom::calibration_library::LIGHT_CAL_VERSION;
#[cfg(feature = "solver")]
use starloom::stacking::config::DRIZZLE_KERNEL_VERSION;
#[cfg(feature = "solver")]
use starloom::integration::engine::ENGINE_VERSION;
#[cfg(feature = "solver")]
use starloom::stacking::integrate::{INTEGRATE_VERSION, SNR_GAIN_VERSION};
#[cfg(feature = "solver")]
use starloom::stacking::measure::MEASURE_STAGE_VERSION;
#[cfg(feature = "solver")]
use starloom::stacking::ln::grid::LN_GRID_VERSION;
#[cfg(feature = "solver")]
use starloom::stacking::ln::LN_MODEL_VERSION;
#[cfg(feature = "solver")]
use starloom::stacking::pass1_artifact::PASS1_ARTIFACT_VERSION;
#[cfg(feature = "solver")]
use starloom::stacking::register::align::ALIGN_VERSION;

#[cfg(feature = "solver")]
#[test]
fn starloom_versions_are_the_accepted_ones() {
    let accepted = "a starloom bump changed this version: accept it on purpose (spec SL-19)";
    assert_eq!(ALIGN_VERSION, 1, "ALIGN_VERSION: {accepted}");
    assert_eq!(LN_MODEL_VERSION, 4, "LN_MODEL_VERSION: {accepted}");
    assert_eq!(SNR_GAIN_VERSION, 2, "SNR_GAIN_VERSION: {accepted}");
    assert_eq!(DRIZZLE_KERNEL_VERSION, 2, "DRIZZLE_KERNEL_VERSION: {accepted}");
    assert_eq!(PASS1_ARTIFACT_VERSION, 1, "PASS1_ARTIFACT_VERSION: {accepted}");
    assert_eq!(LN_GRID_VERSION, 1, "LN_GRID_VERSION (the .athln version): {accepted}");
}

/// The two version-less stages' signal versions enter no hash: a bump recomputes nothing, so it is accepted here.
#[cfg(feature = "solver")]
#[test]
fn starloom_signal_versions_are_the_accepted_ones() {
    let accepted =
        "a starloom bump changed this signal version: accept it on purpose (spec SL-19); no cache keys this stage";
    assert_eq!(INTEGRATE_VERSION, 1, "INTEGRATE_VERSION: {accepted}");
    assert_eq!(ENGINE_VERSION, 1, "ENGINE_VERSION: {accepted}");
}

#[cfg(feature = "solver")]
#[test]
fn starloom_measure_stage_version_is_the_accepted_one() {
    assert_eq!(
        MEASURE_STAGE_VERSION, 1,
        "MEASURE_STAGE_VERSION: caches key this stage: a bump recomputes measurement, registration and \
         normalization artifacts; raise PASS1_ARTIFACT_VERSION with it \u{2014} the pass-1 header checks only \
         rustafits' version, pass1_artifact.rs:140,163"
    );
}

// `LIGHT_CAL_VERSION` is a base-layer starloom constant and `LIGHT_CAL_ENGINE_VERSION` is ungated in core, so these
// two are checked in every build, headless included.
#[test]
fn starloom_light_cal_versions_are_the_accepted_ones() {
    let accepted = "a starloom bump changed this version: accept it on purpose (spec SL-19)";
    assert_eq!(LIGHT_CAL_VERSION, 1, "LIGHT_CAL_VERSION: {accepted}");
    assert_eq!(
        athenaeum_core::models::LIGHT_CAL_ENGINE_VERSION,
        3,
        "raise LIGHT_CAL_ENGINE_VERSION together with starloom's LIGHT_CAL_VERSION"
    );
}
