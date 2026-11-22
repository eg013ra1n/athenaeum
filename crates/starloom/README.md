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
