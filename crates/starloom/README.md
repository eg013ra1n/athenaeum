# starloom

Starloom is the astrophotography stacking kernel behind Athenaeum: geometry and resampling, FITS/XISF writing, light calibration, star measurement, registration, outlier-rejecting integration, drizzle and frame weighting, as a library that runs on the caller's rayon pool. It is built in layers, and each layer is a cargo feature, so a consumer pays only for what it uses.

| Feature | Layer | Adds | Pulls in |
| ---- | ---- | ---- | ---- |
| none | base | geometry, resampling, the FITS/XISF writer core, the platform memory probe, light-calibration parameter types | nothing heavy |
| `engine` | 2 | integration, light calibration, cosmetic correction | `rustafits` |
| `stack` | 3 | registration, measurement, rejection, drizzle, frame weighting and the algorithm config | `engine`, `solvemyastro`, `postcard` |
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
| `MEASURE_STAGE_VERSION` (`stacking/measure.rs`) | 1 | `stack` | the measurement, registration and normalization hashes when not 1 (hash); a bump also raises `PASS1_ARTIFACT_VERSION` with it, because the pass-1 header checks only rustafits' version. In Athenaeum the pass-1 artifact already misses by key without that raise (its stage hash, `pass1_hash_for`, hashes the measurement subtree, which carries the fold); the raise is for a reader of the header alone (a caller with no hash). A bump also re-points two asserts in the application's `stacking/config.rs` tests ("the shipped helper is the versioned one…" and "the shipped measurement helper is the fits-versioned one…") |
| `INTEGRATE_VERSION` (`stacking/integrate.rs`) | 1 | `stack` | signal: enters no hash |
| `ENGINE_VERSION` (`integration/engine.rs`) | 1 | `engine` | signal: enters no hash |
| `MEASURE_VERSION` | 8 | lives in `rustafits` | star measurement; unchanged by starloom |

A change that moves a stage's bytes bumps the stage's version in the same change; the stage goldens below are what notices when it does not.

## Stage goldens

`tests/stage_goldens.rs` runs every stage (measure, pass-1 artifact, register, warp, normalization model, `.athln` format, integrate, SNR gain, drizzle, master bias, master flat, light calibration, cosmetic correction) on a fixed synthetic input on a 2-thread pool and checksums its output. A change that moves a stage's bytes while its version stays is red, and so is a version bump with no new row.

- **A row is keyed by** the stage name, the platform and the versions it was recorded at: the stage's own version first, then the upstream versions its hash folds (for example the registration row also carries `MEASURE_STAGE_VERSION` and rustafits' `MEASURE_VERSION`).
- **The platform key** is `<arch>-<os>`, because `exp` and `powf` round one ulp differently from one system math library to the next. Float rows exist for `aarch64-macos` only; the macOS version and the rustafits revision they were recorded with are in the comment above them. A platform with no rows checks determinism only (two runs agree) and prints the rows it would record. The one float-free stage, the `.athln` format, has the platform `"*"` and holds everywhere. A red float row when neither starloom nor rustafits changed points at the system math library first.
- **How a row is recorded.** Nothing writes the table. Run `cargo test -p starloom --test stage_goldens -- --nocapture`, copy the literal the failure message prints into the table by hand, and do it in the same change as the version bump. A change that moves no number records its row with the reason in its commit message.
- **The rustafits note.** A row is valid for the rustafits revision the workspace locks: the measure, pass-1, register, normalization and integrate stages run rustafits' star fit. A red row right after a rustafits bump means rustafits moved numbers; if its `MEASURE_VERSION` rose, record new rows, and if not, stop the bump.
- **The pass-1 hook.** To make a stored first pass byte-identical between runs, the test-only `FramePass1::fix_stamp_and_zero_timings` zeroes every wall-clock field. Two timing fields inside rustafits' `PlanePrep` have no public setter, so the hook rewrites the tail of the postcard bytes (the last two stored fields of a prep) and reads the prep back. If rustafits changes that layout, the hook fails loudly instead of recording a wrong row; a rustafits test hook replaces the rewrite at the next rustafits bump.

## Writers

Every writer family that reaches the XISF `CreatorApplication` has plain forms with defaults, which write the default creator `starloom <version>`, and one `_full` form that takes every parameter, the creator string last (`write_image_f32_full`, `write_xisf_f32_full`, `write_xisf_f32_to_full`).
