# CLAUDE.md

Guidance for Claude Code working in `starloom`, the stacking kernel crate. The crate is standalone-publishable: keep it free of Athenaeum items (no catalog or Settings; it writes and reads no `ATH_*` card of its own; the only Athenaeum card names left, outside test fixtures, are in three `light_cal` debug messages).

## Layers

Base (no feature), `engine` (needs `rustafits`), `stack` (needs `engine` and `solvemyastro`), plus `ts` and `test-support`. A gate that names a feature the crate does not have fails the build (`unexpected_cfgs = "deny"`).

## Rules

- **Naming.** Never name another code base consulted (solver, stacker or processing program) in code, comments, docs or commit messages; the comparison program is "the ERP". Never name the reference implementation of the debayer.
- **Versions.** Stage versions are public starloom constants (SL-7): a change to a stage's output bits bumps that stage's version in the same PR, and `tests/stage_goldens.rs` enforces it (a stage whose bytes move while its version stays is red). `MEASURE_STAGE_VERSION` also enters the measurement, registration and normalization hashes when not 1, and a bump raises `PASS1_ARTIFACT_VERSION` with it. The versions that enter a hash or a file header are `MEASURE_STAGE_VERSION`, `ALIGN_VERSION`, `LN_MODEL_VERSION`, `SNR_GAIN_VERSION`, `DRIZZLE_KERNEL_VERSION`, `PASS1_ARTIFACT_VERSION`, the `.athln` `VERSION` and `LIGHT_CAL_VERSION`; `MEASURE_VERSION` lives in rustafits. Athenaeum's core tests (`starloom_versions.rs`) pin them and `INTEGRATE_VERSION` / `ENGINE_VERSION`. Section 5 of [Athenaeum's starloom extraction design](https://github.com/eg013ra1n/athenaeum/blob/ff25561c/docs/superpowers/specs/2026-11-22-starloom-extraction-design.md) is the list.
- **Measurement doctrine.** A numeric change is judged on a whole group at master level. Measured-out ideas are not re-proposed; the list lives in [Athenaeum's stacking reference](https://github.com/eg013ra1n/athenaeum/blob/ff25561c/docs/stacking/README.md).
- **Calibration rulings.**
  - Raw-master-dark, no dark scaling. Proposals ship off by default and pass the corpus.
  - Hot pixels are mapped from the master dark only, never from the light.
  - A MAD of exactly 0 gives no hot-pixel map; over-flagging is refused.
- **Contracts.**
  - Every public item has a doc (`#![deny(missing_docs)]`), and a `#[doc(hidden)]` item carries its reason in a comment.
  - SL-8: starloom writes and reads no `ATH_*` card of its own. The header facts Athenaeum owns arrive as parameters.
  - SL-9: starloom never builds a thread pool outside tests; it runs on the caller's rayon pool.
  - SL-12: `Cargo.toml` is standalone: no `[workspace]` table, no `workspace = true` keys, Apache-2.0. `serde_json` enables only `float_roundtrip`; `rusqlite` is never a dependency.
  - SL-17: test hooks that tests in other crates read exist only under `test-support` (or `cfg(test)`).
- **Releases** come from `main` only; `CHANGELOG.md` covers every commit since the last tag, the tag `vX.Y.Z` equals the `Cargo.toml` version, and `Cargo.lock` is committed (the goldens are valid for the rustafits it locks).
- **Environment variables** are `STARLOOM_SCALAR_SELECTION` and `STARLOOM_REQUIRE_VECTOR_KERNEL`. The default XISF creator is `starloom <version>`.
- **Logging.** `tracing` only; no `println!` or `eprintln!` in production code.
- **Tests.** Every test passes on macOS, Linux and Windows; test names use letters, digits, `_` and `-` only.
