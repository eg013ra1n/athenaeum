//! The version tripwire (spec SL-19): the stage versions that enter a hash or a file header are starloom
//! constants. A starloom bump changes output bits, so Athenaeum accepts it on purpose: this test names each value
//! the application was last checked against.
#![cfg(feature = "solver")]

use starloom::calibration_library::LIGHT_CAL_VERSION;
use starloom::stacking::config::DRIZZLE_KERNEL_VERSION;
use starloom::stacking::integrate::SNR_GAIN_VERSION;
use starloom::stacking::ln::grid::LN_GRID_VERSION;
use starloom::stacking::ln::LN_MODEL_VERSION;
use starloom::stacking::pass1_artifact::PASS1_ARTIFACT_VERSION;
use starloom::stacking::register::align::ALIGN_VERSION;

#[test]
fn starloom_versions_are_the_accepted_ones() {
    let accepted = "a starloom bump changed this version: accept it on purpose (spec SL-19)";
    assert_eq!(ALIGN_VERSION, 1, "ALIGN_VERSION: {accepted}");
    assert_eq!(LN_MODEL_VERSION, 4, "LN_MODEL_VERSION: {accepted}");
    assert_eq!(SNR_GAIN_VERSION, 2, "SNR_GAIN_VERSION: {accepted}");
    assert_eq!(DRIZZLE_KERNEL_VERSION, 2, "DRIZZLE_KERNEL_VERSION: {accepted}");
    assert_eq!(PASS1_ARTIFACT_VERSION, 1, "PASS1_ARTIFACT_VERSION: {accepted}");
    assert_eq!(LN_GRID_VERSION, 1, "LN_GRID_VERSION (the .athln version): {accepted}");
    assert_eq!(LIGHT_CAL_VERSION, 1, "LIGHT_CAL_VERSION: {accepted}");
    assert_eq!(
        athenaeum_core::models::LIGHT_CAL_ENGINE_VERSION,
        3,
        "raise LIGHT_CAL_ENGINE_VERSION together with starloom's LIGHT_CAL_VERSION"
    );
}
