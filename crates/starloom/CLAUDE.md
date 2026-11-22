# CLAUDE.md

Guidance for Claude Code working in `starloom`, the stacking kernel crate. The crate is standalone-publishable: keep it free of Athenaeum items (no catalog, Settings or `ATH_*` vocabulary).

## Layers

Base (no feature), `engine` (needs `rustafits`), `stack` (needs `engine` and `solvemyastro`), plus `ts` and `test-support`. A gate that names a feature the crate does not have fails the build (`unexpected_cfgs = "deny"`).

## Rules

- **Naming.** Never name another code base consulted (solver, stacker or processing program) in code, comments, docs or commit messages; the comparison program is "the ERP". Never name the reference implementation of the debayer.
- **Versions.** Stage versions are public starloom constants (SL-7): a change to a stage's output bits bumps that stage's version in the same PR, and a golden guard enforces it. The versions that enter a hash or a file header are `ALIGN_VERSION`, `LN_MODEL_VERSION`, `SNR_GAIN_VERSION`, `DRIZZLE_KERNEL_VERSION`, `PASS1_ARTIFACT_VERSION`, the `.athln` `VERSION` and `LIGHT_CAL_VERSION`; `MEASURE_VERSION` lives in rustafits. Athenaeum's core test `starloom_versions_are_the_accepted_ones` pins them. Spec section 5 of `docs/superpowers/specs/2026-11-22-starloom-extraction-design.md` in the Athenaeum repository is the list.
- **Measurement doctrine.** A numeric change is judged on a whole group at master level. Measured-out ideas are not re-proposed; the list lives in Athenaeum's `docs/stacking/README.md`.
- **Calibration rulings.**
  - Raw-master-dark, no dark scaling. Proposals ship off by default and pass the corpus.
  - Hot pixels are mapped from the master dark only, never from the light.
  - A MAD of exactly 0 gives no hot-pixel map; over-flagging is refused.
- **Contracts.**
  - SL-8: starloom writes and reads no `ATH_*` card of its own. The header facts Athenaeum owns arrive as parameters.
  - SL-9: starloom never builds a thread pool outside tests; it runs on the caller's rayon pool.
  - SL-12: `Cargo.toml` is standalone: no `[workspace]` table, no `workspace = true` keys, Apache-2.0. `serde_json` enables only `float_roundtrip`; `rusqlite` is never a dependency.
  - SL-17: test hooks that tests in other crates read exist only under `test-support` (or `cfg(test)`).
- **Environment variables** are `STARLOOM_SCALAR_SELECTION` and `STARLOOM_REQUIRE_VECTOR_KERNEL`. The default XISF creator is `starloom <version>`.
- **Logging.** `tracing` only; no `println!` or `eprintln!` in production code.
- **Tests.** Every test passes on macOS, Linux and Windows; test names use letters, digits, `_` and `-` only.
