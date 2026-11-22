# starloom

Starloom is the astrophotography stacking kernel behind Athenaeum: geometry and resampling, FITS/XISF writing, light calibration, star measurement, registration, outlier-rejecting integration, drizzle and frame weighting, as a library that runs on the caller's rayon pool. It is built in layers, and each layer is a cargo feature, so a consumer pays only for what it uses.

| Feature | Layer | Adds | Pulls in |
| ---- | ---- | ---- | ---- |
| none | base | geometry, resampling, the FITS/XISF writer core, the platform memory probe, light-calibration parameter types | nothing heavy |
| `engine` | 2 | integration, light calibration, cosmetic correction | `rustafits` |
| `stack` | 3 | registration, measurement, rejection, drizzle, frame weighting and the algorithm config | `engine`, `solvemyastro` |
| `ts` | — | ts-rs type derives (on by default) | `ts-rs` |
| `test-support` | — | test hooks and fixtures that dependent crates' tests read; never in default | nothing |

`default = ["stack", "ts"]`. The crate is standalone: its manifest has no `[workspace]` table. Licence: Apache-2.0.

## Versions

A stage version is a public constant. A change to a stage's output bits bumps that stage's version in the same change. The values below are the ones the dependent application's tripwire test asserts; the ones marked "hash" enter a stage hash or a file header, so a bump invalidates the matching cached results.

| Constant | Value | Layer | Enters |
| ---- | ---- | ---- | ---- |
| `ALIGN_VERSION` (`stacking/register/align.rs`) | 1 | `stack` | the registration stage hash and the two-pass key (hash) |
| `LN_MODEL_VERSION` (`stacking/ln/mod.rs`) | 4 | `stack` | the normalization stage hash and the seeds-calibration hash (hash) |
| `SNR_GAIN_VERSION` (`stacking/integrate.rs`) | 2 | `stack` | the group statistics and the run summary |
| `DRIZZLE_KERNEL_VERSION` (`stacking/config.rs`) | 2 | `stack` | the run-level config hash (hash) |
| `PASS1_ARTIFACT_VERSION` (`stacking/pass1_artifact.rs`) | 1 | `stack` | the pass-1 artifact file header and its stage hash (hash) |
| `LN_GRID_VERSION` (`stacking/ln/grid.rs`, the `.athln` `VERSION`) | 1 | `stack` | the `.athln` sidecar header |
| `LIGHT_CAL_VERSION` (`calibration_library/mod.rs`) | 1 | base | nothing inside starloom; the caller marks its calibrated lights with it |
| `MEASURE_VERSION` | 8 | lives in `rustafits` | star measurement; unchanged by starloom |
