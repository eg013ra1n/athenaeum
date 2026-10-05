# Local normalization model fixes and a calibration pedestal warning — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** LN fits its background as a smooth surface at the LN scale (no cell gate), derives that scale from the image size, selects the stars its scale estimator reads, offers a common-aperture photometric estimator beside the PSF-signal one, and warns (never blocks) when a light has a flat but no dark or bias. Every LN master's sky stops losing star halos and nebula rims, and the measured PSF-width bias of the scale is removed or reduced.

**Architecture:**
- **One background procedure for reference and target** (`stacking/ln/background.rs`). 32-px tiles give robust samples (`TileTable`). A penalized tensor cubic B-spline with knot spacing = scale is fitted to them (`Surface`), with two rejection rounds and a sample-count floor (`BackgroundModel::fit`). Per reference–target pair, both surfaces are refitted once on the samples both kept (`shared_refit`). The mesh nodes the `.athln` sidecar stores are the refitted surfaces sampled at the node pixels. `grid.rs`, the `.athln` format and `v′ = A·v + B` do not change.
- **One scale rule** (`ln::resolve_scale`), called by the run per group and by the plan per group (its twin). The resolved scale is a hash token on the per-frame `ln` and the `ln_calibration` hashes, never on the `ln_reference` hash. `scale`, `scaleMode` and `gridSize` leave the normalization subtree.
- **Star selection** (`stacking/ln/selection.rs`) sits between pairing and the flux ratio inside the two private `_core` bodies of `scale.rs`. So it covers the seeds path, the detection path, both seeds-calibration arms, the masked variants, the local-scale input and the probes with no other call site. **The common-aperture estimator** (`stacking/ln/aperture.rs`) is a second flux definition over the same selected pairs.
- **The pedestal warning** is one core function in `api::lights` (`flat_only_lights`). Export readiness, the stacking plan (per group) and the run (per group, at stage 1, with a measured flat-corner ratio) call it.
- **Versioning:** `LN_MODEL_VERSION` (new, in the normalization subtree) is 2 after the background model and 3 after the selection. No `STACKING_CONFIG_VERSION` bump, no migration (owner rule).

**Tech Stack:** Rust (`athenaeum-core`): serde, rayon, xxhash, rusqlite 0.40, ts-rs (`tests/ts_contract.rs`). React/TS (`NormalizePanel`, `stageSummary`, `registry.ts`, `ExportTab`), vitest. Python 3 + numpy for the acceptance scripts. No new crate dependency (the 1148×1148 Cholesky is written by hand in `background.rs`).

**Spec:** `docs/superpowers/specs/2026-10-04-ln-model-fixes-design.md` (owner-approved 2026-10-04, rulings L-1…L-22). Read it whole before Task 1. Every task cites the spec sections it implements. Where this plan refines the spec, it says so as a ruling **P-n** (next section); P-1 and P-20 need the owner's confirmation at plan review.

**Review folded in:** `.superpowers/sdd/ln-model-fixes/plan-review-fable.md` (verdict "ready after fixes": 3 Critical, 6 Important, 10 Minor). Every fix names its finding (C-n, I-n, M-n) where it lands; C-1 became ruling P-21. The reviewer independently confirmed P-20 and P-1.

**Research behind the spec** (read the parts each task names):
- `docs/superpowers/research/2026-10-04-ln-scale-psf-width-audit.md` (the scale bias; §5 `--stack-sim`, §7 validation at master level);
- `docs/superpowers/research/2026-10-04-ln-math-review.md` (§5 the halo, §8 the ranked fixes).

**Prototype:** before this plan was written, the background procedure of spec §1.2 was prototyped in numpy (`docs/superpowers/research/scripts/ln-model/bg_prototype.py`, committed with this plan; `bg_prototype_pins.py` beside it reproduces every number of P-1, P-2, P-19 and P-20). The prototype sized the unit pins of spec §1.6 and found P-1, P-2, P-19 and P-20.

**Execution order:** Tasks 1 → 18 as numbered. Tasks 9, 16 and 17 are the controller's own (real-data runs and the default decision). Every other task is one implementer plus one reviewer. Tasks 5–7 (the warning) run before checkpoint A so that the Horse C11 runs of checkpoint A also serve as the warning's real-data acceptance (spec §11.5).

| # | Task | Kind | Spec |
| ---- | ---- | ---- | ---- |
| 1 | Background model core | Rust (new code beside the old) | §1.2–§1.6 |
| 2 | Background model wiring, old model retired, `LN_MODEL_VERSION = 2` | Rust | §1, §7, §10 |
| 3 | Scale rule: config, `resolve_scale`, hash token, run and plan twin | Rust + TS types | §2, §6, §7, §8 |
| 4 | Normalize panel: scale controls and the resolved line | Frontend | §9 |
| 5 | Pedestal warning: core function, readiness, plan | Rust + TS types | §5.1, §5.2, §5.4 |
| 6 | Pedestal warning in the run: stage 1, corner ratio | Rust | §5.2, §5.3 |
| 7 | Export tab warning list | Frontend | §5.2, §9 |
| 8 | Checkpoint-A harness | Python + one probe mode | §11.2 |
| 9 | Checkpoint A | Controller | §11.1, §11.3, §11.4 |
| 10 | Star selection core | Rust | §3 |
| 11 | Star selection wiring, `LN_MODEL_VERSION = 3` | Rust + TS types | §3, §6, §7, §10 |
| 12 | Common-aperture photometry core | Rust | §4.1 |
| 13 | Common-aperture wiring | Rust + TS types | §4.1, §4.2, §7, §8, §10 |
| 14 | Normalize panel: star selection and star flux fields | Frontend | §9 |
| 15 | Probe and `--stack-sim` arms | Rust example + Python | §11.2 |
| 16 | Pre-checks, aperture choice, checkpoint B | Controller | §4.3, §11.1, §11.3 |
| 17 | The default, per §4.4 | Controller (+ a one-line change) | §4.4 |
| 18 | Docs | Docs | §12 item 9 |

## Global Constraints

- **Branch:** `ln-model-fixes` (cut from `main` 60df59c4; the research and spec commits are already on it). Merge and push only on the owner's word.
- **No migration (owner, 2026-10-04).** No code rewrites a stored run, run row, provenance document or `master_lights` row. The new model reaches existing sets only through the hash changes this plan names. The legacy config rule (L-9) is a read-time resolution in `resolve_config`. No one-off scripts.
- **Commits** are made as the configured user (`eg013ra1n` / `vilen.sharifov@gmail.com`; the repo config has them — pass no `-c user.*`). Every message ends with exactly these two lines (use your own model name):
  ```
  Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg
  ```
  Commit by explicit `git add <files>` after checking `git status` for foreign work in progress.
- **Copy rules.** Comments, docs, help text and commit messages are in English. **Never name a third-party program or project anywhere in the repo** — not in code, comments, docs, plan-ledger files or commit messages. The spec calls the external pipeline "the external reference pipeline (ERP)"; use that or "the reference pipeline". UI labels are in sentence case.
- **What must not move.**
  - **LN off** (`local.enabled = false` and `normalization.rejection ≠ local`): Normalize stays a no-op and every written pixel, card and sidecar is byte-identical to `main`. Stage hashes other than the three LN ones (`ln_reference`, `ln`, `ln_calibration`) keep their golden literals in every task (`stage_hashes_are_pinned_to_golden_literals`: `calib`, `meas`, `reg` never change).
  - The run fingerprint `config_hash` changes in Task 3 (new `StackingConfig` fields), again in Tasks 11 and 13, and in Task 17 if a default moves. Each change updates `config_hash_is_canonical_and_pinned`'s literal with a dated paragraph naming the task.
  - `grid.rs`, the `.athln` binary format (`VERSION = 1`), `PSF_FIT_VERSION` (3), the `fits` artifact codec (`FITS_ARTIFACT_VERSION`), every upstream stage (calibrate, measure, register) and their cache rows.
  - **Until Task 10 lands, the scale path is untouched.** At the head of Task 9 (checkpoint A) every `A` grid and `global_scale` on set 204 and run B is bit-identical to `main` (spec §1.6).
- **Golden pins.** A task that changes the normalization subtree or the config shape updates exactly these and names the cause in a dated comment: `stage_hashes_are_pinned_to_golden_literals` (`plan.rs:~7525`, the `ln_ref`/`ln`/`ln_cal` literals only), `config_hash_is_canonical_and_pinned` (`config.rs:~1610`), `serde_names_follow_the_spec` (`config.rs:~1115`), `config_serde_names_match_the_spec` (`integrate.rs:~1770`). Any other changed assertion is named by its task.
- **Tests: never loosen a tolerance to make a test pass.** When a pre-existing synthetic test fails because of a deliberate model change, the task names the rule that applies (P-13). Anything else that fails is a stop-and-report.
- **TypeScript:** `src/types/` changes only in Tasks 3, 5, 11, 13 (and 17 if a default moves), and each lists what moves. Every other task ends with `git diff --exit-code src/types`. An exported Rust field added by this plan carries a `//` comment, never `///` (ts-rs copies `///` into the generated files). Regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.
- **The headless build stays green:** `cargo check -p athenaeum-core --no-default-features`. `stacking` (and so `stacking::ln`) is behind `render + solver`; `api::lights` is not — the pedestal functions in `api::lights` name no `stacking` type.
- **Errors and logging.**
  - Never swallow an error: log at `warn!`/`error!` before degrading. Every refusal (a plane with no supported knot cell, a pair with no shared supported cell, a flat whose corners cannot be read) is logged before it returns.
  - `tracing` only, short stable message, snake_case fields; zero `println!` outside tests and the example probes' stdout.
  - New field names are only those of spec §10 (Task 18 registers them in the logging dictionary). `ln_cells_rejected` is retired.
- **Build discipline (this Mac).**
  - `CARGO_BUILD_JOBS=4`, `--test-threads=4`, one compile at a time, nothing heavy beside a checkpoint run (memory note 2026-09-26).
  - **Never run `rustfmt` on `stacking/run.rs`, `stacking/plan.rs`, `stacking/ln/mod.rs`, `stacking/ln/scale.rs` or `calibration_library/light_cal.rs`** (not rustfmt-clean upstream, and `ln/mod.rs` pulls its submodules in). Format your hunks there by hand to rustfmt style. Every other touched Rust file: `rustfmt --edition 2021 <file>`, then revert any sibling reformatting it drags in.
  - `cargo check -p athenaeum-core --lib` stays warning-free. Clippy is not a gate.
  - Review packages use `git diff --diff-algorithm=histogram`.
- **Two backends.** No command is added or removed (spec §0). New fields ride existing payloads; both hosts return the same core structs, so neither host needs code beyond what the compiler asks for.
- **Gate per task** (the controller re-runs it; IDE diagnostics mid-edit are stale):
  ```bash
  CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked'
  CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --lib 2>&1 | grep -E '^warning|^error' ; echo "lib check done"
  CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
  CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
  CARGO_BUILD_JOBS=4 cargo check -p athenaeum-tauri
  TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract && git diff --stat src/types
  npx tsc --noEmit -p . && npx vitest run
  ```
  - The headless check keeps exactly its pre-existing dead-code warnings (`exchange_meter`, `sharing/iroh/node.rs`); a task adds none.
  - Known flakes under full-suite load: the wall-clock collab live tests (`edited_touched_and_deleted_replicas`, `a_publishing_switch_returns_at_once_while_the_reannounce_retries`, `unmapped_filter_then_mapped_and_an_attested_set`), `landing_free_bytes_is_bounded_by_the_collaboration_root`, `concurrent_gets_for_an_unreachable_provider_share_one_dial`, `local_normalization_sidecars_are_cached_on_the_second_run`. If one fails, re-run it alone and report both runs.
- **Acceptance discipline (Tasks 9, 16, 17).** Rows are never re-sized and nothing is tuned toward a bar (owner rule). A miss stops the work and goes to the owner with its numbers. A numeric change is judged on the whole group at master level. Interleaved before/after brackets and same-evening `main` re-runs only (R-TA-8/9). Never read or write the production catalog; every run works on a `.backup` copy.

## Review Focus

Five inputs the spec implies, which no task's original tests would exercise, and which are the most likely to bite a person using this:

1. **A frame that covers only part of the reference canvas** (a large dither or a meridian flip leaves a wide NaN strip; a project member covers a corner of the grid). Expected: the covered part is modelled as if the strip were not there, and the surface over the strip stays bounded (it continues the local trend for about one knot interval and then levels, P-19). It never oscillates or blows up. Pinned by `an_off_canvas_strip_stays_bounded_and_leaves_the_covered_part_exact` (Task 1) and `restricted_tiles_equal_measuring_the_masked_plane` (Task 1).
2. **A stored config from before this cycle** (a per-set document, the global default, a user preset, a project document) with `scale: 2048` and no `scaleMode`. Expected: it stays fixed at 2048 everywhere (plan, run, Settings, the Stacking tab), and `scale: 1024` becomes auto. Pinned by `a_legacy_document_reads_scale_1024_as_auto_and_any_other_as_fixed` and `every_stored_config_reader_resolves_the_legacy_scale` (Task 3).
3. **The plan's estimated scale disagrees with the run's** (Auto pick, first run). Expected: one run warning. The second plan then reads Normalize as cached, never stale forever. Pinned by `a_run_warns_once_when_its_ln_scale_differs_from_the_plan` and `the_plan_reads_normalize_cached_after_a_run_with_a_derived_scale` (Task 3).
4. **One hot pixel or cosmic ray far above every star.** Expected: it does not set the plane maximum the relative-saturation step compares with, so the bright unsaturated stars still pass. Pinned by `a_hot_pixel_does_not_set_the_plane_maximum` (Task 10).
5. **A set with mixed links and missing cards** (some lights flat-only, some flat+dark; OFFSET recorded on some frames only; an attested set). Expected: the warning counts exactly the flat-only lights, formats the cards honestly, never blocks export, send or a run, and never changes a link. Pinned by `flat_only_lights_counts_exactly_the_lights_without_a_dark_or_bias`, `offset_gain_text_covers_one_value_a_range_and_not_recorded` and `the_warning_never_changes_an_auto_link_pick` (Task 5).

## Plan rulings (refinements of the spec)

Numbered P-1…P-21. Each says what it decides and why. **P-1 and P-20 need the owner's confirmation at plan review.** The rest are writer's decisions on points the spec leaves open.

- **P-1 — The linear-gradient pin (spec §1.6) as the model can meet it.** *(Owner to confirm, together with P-20.)*
  - The spec asks that "a linear gradient is reproduced at every node to 1e-5 of its amplitude". No first-difference (membrane) penalty can meet that at the edges.
  - The cause is the membrane itself. Its null space is the constants, so near an edge it pulls the surface toward the edge level. That is the documented property the spec relies on for gaps ("beyond the last data it continues at the edge level").
  - Prototype on 2048 × 1536 at scale 512, a 0.02 gradient on a 0.1 sky, error as a fraction of the gradient's amplitude:

    | λ (relative) | Every node | Nodes ≥ one knot interval inside |
    | ---- | ---- | ---- |
    | 1e-4 (L-3 as written) | 1.38e-3 | 6.1e-6 |
    | 3e-3 | 9.3e-3 | 8.0e-5 |
    | 1e-2 (P-20) | 1.34e-2 | 2.8e-4 |
    | 0 | 6e-13 | 1e-13 |

  - On real data only the **difference** of the reference's and the target's gradients reaches `B` (each surface's flattening cancels where the two agree). With P-20, a 10 %-of-sky gradient difference gains at most 0.13 % of sky at a corner node, and 0.003 % one knot interval inside.
  - **Decision:**
    - The pin asserts the measured property at the chosen λ: under P-20, every node within 2e-2 of the amplitude and every node one knot interval inside within 5e-4.
    - If the owner keeps λ = 1e-4, the bars are 2e-3 and 1e-5 (the spec's number, where it is meaningful).
- **P-2 — The halo and broad-feature pins (spec §1.6) are measured where the feature is, and the broad feature is sized like the spec's case.**
  - Fixture: 2048 × 1536 at scale 512, sky 0.1, target noise σ_T = 0.188 × sky (the math review's single-frame σ/background), reference σ_T/√n for n = 1, 5, 28.
  - **Halo pin.** A halo star `0.6·(1 + (r/15)²)^−1.5` (sky units: +6 × sky at the core, +2 % at 100 px, +0.25 % at 200 px). The spec's "nodes within 0.5 % of background of each other" is read on the nodes within 600 px of the star — the same region as acceptance row "B nodes" (§11.3).
    - Over the whole plane the max is noise-limited at the plane's edges (prototype 0.9–1.2 %, for n = 1 as much as n = 28), so it would not test the model.
    - Within 600 px the prototype gives at most 0.33 %, against the bar of 0.5 %.
  - **Broad-feature pin.** A Gaussian with FWHM = 1.3 × scale (667 px) at +30 % of sky. 1.3 × scale is the spec's own ratio (2000 px at scale 1536); at 3.9 × scale (2000 px at 512) the surface follows the feature and nothing is rejected differently, so no refit effect exists to pin.
    - It is read on the nodes within one FWHM of the feature's centre.
    - The prototype gives the following, without the refit and with it:

      | n | Without the refit | With the refit |
      | ---- | ---- | ---- |
      | 1 | 0.32 % | 0.32 % |
      | 5 | 0.96 % | 0.20 % |
      | 28 | 0.93 % | 0.16 % |

    - At n = 1 there is no noise asymmetry, so nothing for the refit to remove. The pin therefore asserts "fails without the refit" at n = 5 and n = 28 only, and "passes with it" at all three.
  - If the Rust fixture (its own RNG) misses a bar that the prototype met, the implementer stops and reports both numbers. The fixture is never tuned.
- **P-3 — "A tile wholly inside the member's overlap" includes the hot-pixel apron (L-5).** A tile is reused bit for bit only when the tile **and its 2-px apron** (clipped to the plane) lie inside the overlap. The hot-pixel window reads the apron, so a tile whose apron crosses the overlap edge must be recomputed for the spec's own claim to hold ("the result is identical to recomputing every tile"). Pinned by `restricted_tiles_equal_measuring_the_masked_plane`.
- **P-4 — `normalize_frame` and `measure_seeds_calibration` take the resolved scale as an explicit `scale_px: u32`.** They never read `cfg.scale`. The run passes `ln_cfg.scale` in Task 2 and `ln::resolve_scale(..)` from Task 3 on. `local.scale` is then read by `resolve_scale` alone (spec §2.3), plus the probe's `--scale` override.
- **P-5 — `PlanGroup` also carries `ln_width`/`ln_height` (`Option<u32>`).** The "Resolved scale" line of spec §9 shows "1536 px (9576 × 6388)", and the anchor dims (`anchor_width`/`anchor_height`) are not the LN geometry on a grid or in co-registered mode. These are two more optional fields on an existing payload; no command changes.
- **P-6 — The B-move read lives in `tier1-compare.py --ln-grids`, not `ln_probe --diag`.**
  - `ln_probe --diag` already prints the seed-filter breakdown (Tier C), and `tier1-compare.py` already parses `.athln`.
  - The new mode pairs every sidecar of two working trees by relative path. It reports the B move (median, p99, nodes past 1 % of sky) and the bit-identity of `A` and `global_scale`. Spec §11.2's "ln_probe --diag reports the B move" is satisfied by this mode.
- **P-7 — On a project member, the reference side of the selection uses the member's restricted reference model** (its plane maximum and its tile σ). That is the model the member's background is fitted against. A frame set uses the group reference model.
- **P-8 — Presence masks for the common aperture.**
  - Target: finite before sanitizing; in masked mode that is exactly the overlap, because the target is masked first.
  - Reference: the raw reference plane's finite pixels (legacy, built once per group in `LnReferenceForDetection`), or the member's overlap (masked).
- **P-9 — A rejection round that would keep no sample rejects nothing** (spec §1.2 step 5 is silent). It only happens with residuals all far from their MAD; the round keeps every sample, as for `σ_e ≤ 0`.
- **P-10 — The growth-curve correction with no qualifying pair is `c = 1`, with one `warn!` per plane** (spec §4.1 is silent). The ratio sample is unaffected, and the log says why the variant degraded to 2F. A pair qualifies when `circle_flux` at 5F is valid on both frames: entirely present, as the spec says, and also a half-present annulus and a positive flux. That is stricter than the spec and harmless (review M-8).
- **P-11 — The pedestal `warn!` is emitted by the run (per group) and by `get_export_readiness` (per set), and by nothing else.**
  - `readiness_from_data` fills the field silently. It also serves the collab per-frame verdict (once per light), where a `warn!` per call would flood the log.
  - The plan carries the warning in its payload and does not log it (it is rebuilt on every debounced edit).
- **P-12 — The card text with partial records.**
  - Distinct recorded values are shown as one value (`OFFSET 1`) or a range (`OFFSET 1–5`, en dash).
  - No recorded value reads `OFFSET not recorded`.
  - Recorded on some affected lights only appends ` (not recorded on some)`.
  - Values print as integers when whole, else with up to two decimals, trimmed.
- **P-13 — Pre-existing synthetic LN tests and the selection.**
  - Synthetic fixtures have no saturated star, so the plane maximum IS the brightest star. Relative saturation at 0.75 then removes the top quarter of stars, which no real frame would see.
  - A pre-existing test whose subject is not the selection (pairing, β, RCR, the local spline, the sidecar, masked coefficients) runs with `LnStarSelection::OFF` and a one-line comment naming this ruling.
  - The same holds for the `run.rs` LN tests built on `seed_ln_star_group` (`run.rs:~17448` and its users — `local_normalization_writes_one_sidecar_per_included_frame_and_a_reference`, `…falls_back_to_detection…`, `…sidecars_are_cached_on_the_second_run`, the rejection-only tests ~18566/~18738 — and Tasks 2 and 3's new run tests). Their subject is the LN run, never the selection. Under the default selection a frame can drop below 20 matches once the top quarter is cut (review C-3). From Task 11 on they set `cfg.normalization.local.star_selection = LnStarSelection::OFF` with the P-13 comment when they fail for that reason.
  - Selection itself is pinned by Task 10's new tests. No tolerance changes.
- **P-14 — The run learns the plan's per-group estimate from the plan `start_stacking` already builds** (`run.rs:~897`). It is stored as `RunContext.planned_ln_scales`, so the mismatch warning of spec §2.3 needs no second plan build.
- **P-15 — `ScaleResult.matches` keeps its meaning (valid PSF-signal pairs before selection, `ln_matches`); `ScaleResult.selected` is new (`ln_selected`).** `diag` and RCR read the selected sample. Under `LnStarSelection::OFF` with PSF signal, `selected == matches`, which keeps every pre-existing count assertion's meaning.
- **P-16 — `--stack-sim` stays plane 0** (its dumped warps are single-plane). Spec §4.3 runs it on set 204 mono only, and the arms of Task 15 are per-member scale maps.
- **P-17 — The master SNR row reads σ from `measure_probe <master>` `.noise`** — the same MRS estimate `tier1-compare.py`'s masters section reads — and `k` from `master_phot.py`. One script prints both for every arm.
- **P-18 — `σ_local ≤ 0` (a noise-free synthetic plane) makes the SNR test pass** when `peak − b > 0`. There is no noise information, so there is no SNR cut. Real frames always have tile σ > 0.
- **P-21 — A residual MAD at the rounding level is σ_e ≤ 0** (spec §1.2 step 5; review C-1).
  - Tile values are f32. On a constant or noise-free plane the penalized solve leaves residuals at the f64 rounding level, and their MAD is still > 0.
  - The prototype rejected 158 of 3072 samples of a constant plane at σ_e = 4.9e-16. That makes the constant-plane pin fail and the 8-sample floor fixture machine-dependent.
  - **Decision:** σ_e ≤ 1e-6 × |median of the accepted samples' values| (≈ 8 f32 ulps of the level) counts as σ_e ≤ 0: the round rejects nothing.
  - Real tile σ is 1e-3–1e-1 of the level, four orders above the floor.
  - Mirrored in `bg_prototype.py`. P-1's numbers are unchanged with it: the gradient fixture's residual MAD lies above the floor.
- **P-19 — What the surface does over a gap, measured** (spec §1.3 states it loosely).
  - The spec says that beyond the last data the surface "continues at the edge level". That holds only where the coefficients have no data in their support, i.e. more than about two knot intervals from the data.
  - Closer than that, the coefficients are still fitted to the data, so the surface continues the data's local trend and carries more noise than over the data.
  - Prototype, 2048 × 1536 at scale 512, NaN for x ≥ 1400, sky 0.1 with 2 % pixel noise. Farthest strip node from the edge level, over three seeds:

    | Sky | λ = 1e-4 | λ = 1e-2 (P-20) |
    | ---- | ---- | ---- |
    | 10 %-of-sky gradient | 2.8–3.7 % | 2.0–2.6 % |
    | Flat | 0.6–1.3 % | 0.13–0.61 % |

    The covered part stays within 0.09 % of sky in every case.
  - On a frame set, a target's strip pixels are NaN and are never normalized. Only the nodes within two strides of the edge reach covered pixels, and their noise equals the old cell model's (below, P-20).
  - On a project member, the member's pixels outside the reference ARE normalized from these extrapolated nodes. That is why P-20 matters.
  - The pin (Task 1) guards against oscillation or blow-up:
    - ≤ 5 % of sky with the gradient;
    - ≤ 2 % flat;
    - the covered part within 0.3 % of sky.
- **P-20 — The membrane penalty is λ = 1e-2 × the median non-zero diagonal of BᵀB, not 1e-4 (L-3's constant).** *(Owner to confirm.)*
  - **Found by the prototype after the spec was approved.** A coefficient whose support touches the data only through its B-spline tail is weakly determined. At λ = 1e-4 the penalty is too weak to hold it, so the sample noise is amplified there.
  - Analytic node noise for a single frame (σ/sky = 0.188, the set-204 geometry 6248 × 4176 at scale 1024), in % of sky. The old cell model's node noise is 0.37 % everywhere.

    | λ | Interior | Plane edge (max) | Gap, ≤ 2 strides from the data | Gap, anywhere (30 %-wide gap) |
    | ---- | ---- | ---- | ---- | ---- |
    | 1e-4 | 0.034 | 0.18 | 0.36 | **6.8** |
    | 1e-3 | 0.033 | 0.16 | 0.24 | 0.98 |
    | 3e-3 | 0.031 | 0.14 | 0.21 | 0.50 |
    | 1e-2 | 0.030 | 0.11 | 0.18 | **0.37** |

  - At λ = 1e-4, a project member's pixels outside the LN reference (normalized by design, 6b-3 D6) would carry up to 6.8 % of sky of background noise per frame, against 0.37 % from today's neighbour fill. That is a regression.
  - At λ = 1e-2 every column is at or below the old model. The halo and broad-feature pins pass alike: halo ≤ 0.27 % with the refit; broad feature 0.88 / 0.97 % without the refit and ≤ 0.20 % with it.
  - The cost is the corner flattening of P-1.
  - **Мой вывод (writer): take λ = 1e-2.** It removes a regression the spec did not see and keeps every benefit the spec measured. The corner cost (0.13 % of sky per 10 % of gradient difference) is under every acceptance bar. Task 1 writes `MEMBRANE_PENALTY_REL = 1e-2`; if the owner keeps 1e-4, it is a one-line change plus the two P-1 bars.
  - Checkpoint A judges it on real data either way (the halo rings, the IC 434 boxes, the B move).

---

### Task 1: Background model core

Spec §1.2 (steps 1–7), §1.3, §1.5, §1.6; rulings L-1…L-6, P-1, P-2, P-3, P-9, P-19, P-20, P-21.

The new model lands **beside** the old one in the same file, with every pin of spec §1.6. Nothing calls it yet; Task 2 switches every caller and deletes the old model. This split lets a reviewer judge the numerics on their own.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/ln/background.rs` (append the new model after `fill_invalid_cells`, before `#[cfg(test)] mod tests`; append the new tests inside `mod tests`)
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs:54-56` (re-exports)

**Interfaces:**
- Consumes: `crate::integration::stats::{mad_about, median_in_place, MAD_TO_SIGMA}`, `super::grid::LnGrid::grid_dims` (`pub(crate)`).
- Produces (all `pub`, re-exported from `stacking::ln`):
  - constants `TILE_PX = 32`, `MIN_TILE_PRESENT = 256`, `TILE_CLIP_SIGMA: f32 = 3.0`, `SAMPLE_CLIP_SIGMA: f64 = 3.0`, `SAMPLE_REJECTION_ROUNDS = 2`, `MIN_SAMPLES_PER_KNOT_CELL = 8`, `MEMBRANE_PENALTY_REL: f64 = 1e-2` (P-20);
  - `struct CleaningParams { hot_radius: usize, low_clip: f32, high_clip_rel: f32 }` and `const CLEANING: CleaningParams` (2, 4.5e-5, 0.85);
  - `struct TileStat { x0, y0, x1, y1, present: u32, value: f32, sigma: f32, max: f32 }` with `is_sample()`, `centre() -> (f64, f64)`;
  - `struct TileTable { width, height, tw, th: usize, tiles: Vec<TileStat> }` with `measure(plane, w, h, &CleaningParams, pool)`, `measure_masked(plane, w, h, &CleaningParams, pool, mask: &[bool])`, `restricted(&self, plane, mask: &[bool], &CleaningParams, pool) -> TileTable`, `plane_max() -> f32`;
  - `struct Surface { width, height: usize, knot: f64, nx, ny: usize, coeffs: Vec<f64> }` with `eval(x, y) -> f64`, `nodes(stride) -> Vec<f32>`;
  - `enum BackgroundRefusal { NoSamples, NoSupportedCell, Singular }` (`Display`, `std::error::Error`);
  - `struct BackgroundModel { table: TileTable, scale: u32, surface: Surface, accepted: Vec<bool>, samples: usize, samples_rejected: usize, cells_unsupported: usize, cells: usize }` with `BackgroundModel::fit(table: TileTable, scale: u32) -> Result<BackgroundModel, BackgroundRefusal>`;
  - `struct PairSurfaces<'a> { reference: Cow<'a, Surface>, target: Cow<'a, Surface>, shared: usize, refit: bool }` and `fn shared_refit<'a>(reference: &'a BackgroundModel, target: &'a BackgroundModel) -> PairSurfaces<'a>`.

- [ ] **Step 1: Write the failing tests** (append inside `background.rs`'s `mod tests`; they reference names that do not exist yet).

```rust
    // ---- Spec 2026-10-04 §1.6 pins for the model at the scale (Task 1) ----

    /// Deterministic Gaussian noise (xorshift64 + Box–Muller), so every pin
    /// is reproducible without a `rand` dependency.
    fn noisy(
        width: usize,
        height: usize,
        sky: impl Fn(f64, f64) -> f64,
        sigma: f64,
        seed: u64,
    ) -> Vec<f32> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        let mut spare: Option<f64> = None;
        let mut out = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let g = match spare.take() {
                    Some(g) => g,
                    None => {
                        let (u1, u2) = (uniform(), uniform());
                        let r = (-2.0 * u1.ln()).sqrt();
                        let t = std::f64::consts::TAU * u2;
                        spare = Some(r * t.sin());
                        r * t.cos()
                    }
                };
                out.push((sky(x as f64, y as f64) + sigma * g) as f32);
            }
        }
        out
    }

    fn model(plane: &[f32], w: usize, h: usize, scale: u32) -> BackgroundModel {
        BackgroundModel::fit(TileTable::measure(plane, w, h, &CLEANING, None), scale)
            .expect("the fixture has samples")
    }

    fn node_xy(i: usize, j: usize, stride: usize, w: usize, h: usize) -> (f64, f64) {
        ((i * stride).min(w - 1) as f64, (j * stride).min(h - 1) as f64)
    }

    /// (max |S_ref − S_tgt| with the shared refit, the same without it) over
    /// the nodes within `radius` px of `centre` (P-2).
    fn pair_difference_near(
        rf: &[f32],
        tgt: &[f32],
        w: usize,
        h: usize,
        scale: u32,
        centre: (f64, f64),
        radius: f64,
    ) -> (f64, f64) {
        let (rm, tm) = (model(rf, w, h, scale), model(tgt, w, h, scale));
        let pair = shared_refit(&rm, &tm);
        assert!(pair.refit, "the fixture always has shared samples");
        let stride = (scale / 8) as usize;
        let (gw, gh) = LnGrid::grid_dims(w, h, stride);
        let (rs, ts) = (pair.reference.nodes(stride), pair.target.nodes(stride));
        let (ro, to) = (rm.surface.nodes(stride), tm.surface.nodes(stride));
        let (mut shared, mut own) = (0f64, 0f64);
        for j in 0..gh {
            for i in 0..gw {
                let (x, y) = node_xy(i, j, stride, w, h);
                if (x - centre.0).hypot(y - centre.1) > radius {
                    continue;
                }
                let k = j * gw + i;
                shared = shared.max((rs[k] - ts[k]).abs() as f64);
                own = own.max((ro[k] - to[k]).abs() as f64);
            }
        }
        (shared, own)
    }

    /// P-21: no sample is rejected on rounding-level residuals.
    #[test]
    fn a_constant_plane_gives_constant_nodes() {
        let (w, h) = (1024usize, 768usize);
        let m = model(&vec![0.1f32; w * h], w, h, 256);
        for v in m.surface.nodes(32) {
            assert!(((v as f64) / 0.1 - 1.0).abs() < 1e-7, "{v}");
        }
        assert_eq!(m.samples_rejected, 0);
        assert_eq!(m.cells_unsupported, 0);
    }

    /// P-1 at λ = 1e-2 (P-20): 5e-4 of the amplitude one knot interval
    /// inside, 2e-2 at the edges (the membrane's documented pull toward the
    /// edge level). Prototype: 2.8e-4 inside, 1.34e-2 at the worst corner
    /// node. (At λ = 1e-4 the bars would be 1e-5 and 2e-3.)
    #[test]
    fn a_linear_gradient_is_reproduced_inside_and_flattened_only_at_the_edges() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let amp = 0.02;
        let sky = |x: f64, y: f64| {
            0.1 + amp * (0.6 * x / (w - 1) as f64 + 0.4 * y / (h - 1) as f64)
        };
        let plane: Vec<f32> = (0..w * h)
            .map(|k| sky((k % w) as f64, (k / w) as f64) as f32)
            .collect();
        let m = model(&plane, w, h, scale);
        let stride = 64;
        let (gw, gh) = LnGrid::grid_dims(w, h, stride);
        let nodes = m.surface.nodes(stride);
        let s = scale as f64;
        for j in 0..gh {
            for i in 0..gw {
                let (x, y) = node_xy(i, j, stride, w, h);
                let err = (nodes[j * gw + i] as f64 - sky(x, y)).abs() / amp;
                assert!(err <= 2e-2, "node ({i},{j}): {err:e} of the amplitude");
                let inside = x >= s && x <= (w - 1) as f64 - s && y >= s && y <= (h - 1) as f64 - s;
                if inside {
                    assert!(err <= 5e-4, "inside node ({i},{j}): {err:e} of the amplitude");
                }
            }
        }
    }

    /// P-2 halo pin. Prototype (3 seeds, λ = 1e-2): ≤ 0.27 % of sky with
    /// the refit.
    #[test]
    fn a_halo_star_moves_reference_and_target_alike_at_three_noise_ratios() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let (cx, cy) = (0.55 * w as f64, 0.45 * h as f64);
        let sky = |x: f64, y: f64| {
            let r2 = (x - cx).powi(2) + (y - cy).powi(2);
            0.1 + 0.6 * (1.0 + r2 / 225.0).powf(-1.5)
        };
        let sigma_t = 0.188 * 0.1;
        for (k, n) in [1.0f64, 5.0, 28.0].into_iter().enumerate() {
            let tgt = noisy(w, h, sky, sigma_t, 11 + k as u64);
            let rf = noisy(w, h, sky, sigma_t / n.sqrt(), 101 + k as u64);
            let (shared, own) = pair_difference_near(&rf, &tgt, w, h, scale, (cx, cy), 600.0);
            assert!(
                shared <= 0.005 * 0.1,
                "n = {n}: {:.3} % of sky with the refit ({:.3} % without)",
                1e3 * shared,
                1e3 * own
            );
        }
    }

    /// P-2 broad-feature pin: FWHM = 1.3 × scale at +30 % of sky. Without
    /// the shared refit (L-6) the low-noise reference rejects more of the
    /// feature than the target does; with it, both surfaces agree.
    /// Prototype (λ = 1e-2): without 0.88 / 0.97 % at n = 5 / 28, with
    /// ≤ 0.20 %.
    #[test]
    fn a_broad_feature_needs_the_shared_refit() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let (cx, cy) = (0.55 * w as f64, 0.45 * h as f64);
        let fwhm = 1.3 * scale as f64;
        let s = fwhm / 2.354_820_045;
        let sky = |x: f64, y: f64| {
            let r2 = (x - cx).powi(2) + (y - cy).powi(2);
            0.1 * (1.0 + 0.30 * (-0.5 * r2 / (s * s)).exp())
        };
        let sigma_t = 0.188 * 0.1;
        for (k, n) in [1.0f64, 5.0, 28.0].into_iter().enumerate() {
            let tgt = noisy(w, h, sky, sigma_t, 21 + k as u64);
            let rf = noisy(w, h, sky, sigma_t / n.sqrt(), 201 + k as u64);
            let (shared, own) = pair_difference_near(&rf, &tgt, w, h, scale, (cx, cy), fwhm);
            assert!(
                shared <= 0.005 * 0.1,
                "n = {n}: {:.3} % of sky with the refit",
                1e3 * shared
            );
            if n >= 5.0 {
                assert!(
                    own > 0.005 * 0.1,
                    "n = {n}: the fixture no longer shows the noise asymmetry the refit removes \
                     ({:.3} % without the refit)",
                    1e3 * own
                );
            }
        }
    }

    /// Review Focus 1 / P-19: the covered part is right and the strip stays
    /// bounded (a blow-up guard, not a fidelity bar). Prototype (λ = 1e-2,
    /// 3 seeds): covered part ≤ 0.09 % of sky; strip ≤ 2.6 % (a 10 %-of-sky
    /// gradient) and ≤ 0.61 % (flat).
    #[test]
    fn an_off_canvas_strip_stays_bounded_and_leaves_the_covered_part_exact() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let stride = 64;
        for (grad, strip_bar) in [(0.01f64, 0.05 * 0.1), (0.0, 0.02 * 0.1)] {
            let sky = |x: f64, _y: f64| 0.1 + grad * x / (w - 1) as f64;
            let mut plane = noisy(w, h, sky, 0.002, 3);
            for y in 0..h {
                for x in 1400..w {
                    plane[y * w + x] = f32::NAN;
                }
            }
            let m = model(&plane, w, h, scale);
            let (gw, gh) = LnGrid::grid_dims(w, h, stride);
            let nodes = m.surface.nodes(stride);
            let edge_level = sky(1400.0, 0.0);
            for j in 0..gh {
                for i in 0..gw {
                    let (x, y) = node_xy(i, j, stride, w, h);
                    let v = nodes[j * gw + i] as f64;
                    if x >= 1400.0 {
                        assert!(
                            (v - edge_level).abs() <= strip_bar,
                            "grad {grad}: strip node ({i},{j}) {v} vs edge level {edge_level}"
                        );
                    } else if x <= 1400.0 - scale as f64 {
                        assert!(
                            (v - sky(x, y)).abs() <= 0.003 * 0.1,
                            "grad {grad}: covered node ({i},{j}) {v} vs {}",
                            sky(x, y)
                        );
                    }
                }
            }
        }
    }

    /// The pre-2026-10 full-plane cleaning, re-implemented naïvely so this
    /// oracle survives Task 2's deletion of `clean_plane`.
    fn naive_tile_table(plane: &[f32], w: usize, h: usize) -> TileTable {
        let p = CLEANING;
        let mut hot = plane.to_vec();
        for y in 0..h {
            for x in 0..w {
                let v = plane[y * w + x];
                if !(v.is_finite() && v > p.high_clip_rel) {
                    continue;
                }
                let mut win = Vec::new();
                for wy in y.saturating_sub(2)..=(y + 2).min(h - 1) {
                    for wx in x.saturating_sub(2)..=(x + 2).min(w - 1) {
                        let wv = plane[wy * w + wx];
                        if wv.is_finite() {
                            win.push(wv);
                        }
                    }
                }
                let med = median_in_place(&mut win);
                let mad = mad_about(&win, med);
                if v > med + 5.0 * MAD_TO_SIGMA * mad {
                    hot[y * w + x] = med;
                }
            }
        }
        let (tw, th) = (w.div_ceil(TILE_PX), h.div_ceil(TILE_PX));
        let mut tiles = Vec::new();
        for v in 0..th {
            for u in 0..tw {
                let (x0, y0) = (u * TILE_PX, v * TILE_PX);
                let (x1, y1) = ((x0 + TILE_PX).min(w), (y0 + TILE_PX).min(h));
                let mut vals = Vec::new();
                let mut max = f32::NEG_INFINITY;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let c = hot[y * w + x];
                        if !c.is_finite() {
                            continue;
                        }
                        max = max.max(c);
                        if c >= p.low_clip && c <= p.high_clip_rel {
                            vals.push(c);
                        }
                    }
                }
                let max = if max == f32::NEG_INFINITY { f32::NAN } else { max };
                let present = vals.len() as u32;
                let (value, sigma) = if vals.len() < MIN_TILE_PRESENT {
                    (f32::NAN, f32::NAN)
                } else {
                    for _ in 0..5 {
                        let m = crate::integration::stats::median_of(&vals);
                        let mad = mad_about(&vals, m);
                        if mad <= 0.0 {
                            break;
                        }
                        let next: Vec<f32> = vals
                            .iter()
                            .copied()
                            .filter(|&c| (c - m).abs() <= 3.0 * MAD_TO_SIGMA * mad)
                            .collect();
                        let done = next.len() == vals.len();
                        vals = next;
                        if done {
                            break;
                        }
                    }
                    let m = crate::integration::stats::median_of(&vals);
                    (m, MAD_TO_SIGMA * mad_about(&vals, m))
                };
                tiles.push(TileStat {
                    x0: x0 as u32,
                    y0: y0 as u32,
                    x1: x1 as u32,
                    y1: y1 as u32,
                    present,
                    value,
                    sigma,
                    max,
                });
            }
        }
        TileTable { width: w, height: h, tw, th, tiles }
    }

    fn assert_tables_bit_identical(a: &TileTable, b: &TileTable) {
        assert_eq!((a.width, a.height, a.tw, a.th), (b.width, b.height, b.tw, b.th));
        assert_eq!(a.tiles.len(), b.tiles.len());
        for (k, (s, t)) in a.tiles.iter().zip(&b.tiles).enumerate() {
            assert_eq!((s.x0, s.y0, s.x1, s.y1, s.present), (t.x0, t.y0, t.x1, t.y1, t.present), "tile {k}");
            assert_eq!(s.value.to_bits(), t.value.to_bits(), "tile {k} value {} vs {}", s.value, t.value);
            assert_eq!(s.sigma.to_bits(), t.sigma.to_bits(), "tile {k} sigma");
            assert_eq!(s.max.to_bits(), t.max.to_bits(), "tile {k} max");
        }
    }

    /// A field with hot pixels (above the 0.85 clip), saturated stars, a NaN
    /// and an Inf, over a partial last tile on both axes (530 × 400).
    fn dirty_field() -> (Vec<f32>, usize, usize) {
        let (w, h) = (530usize, 400usize);
        let mut plane = noisy(w, h, |x, y| 0.08 + 2e-5 * x + 1e-5 * y, 0.003, 7);
        for k in 0..300 {
            let (x, y) = ((k * 53) % w, (k * 97) % h);
            plane[y * w + x] = 0.95;
        }
        for (cx, cy) in [(100usize, 100usize), (400, 300)] {
            for y in cy - 3..=cy + 3 {
                for x in cx - 3..=cx + 3 {
                    plane[y * w + x] = 0.97;
                }
            }
        }
        plane[5 * w + 5] = f32::NAN;
        plane[9 * w + 9] = f32::INFINITY;
        (plane, w, h)
    }

    #[test]
    fn tile_statistics_match_the_full_plane_cleaning() {
        let (plane, w, h) = dirty_field();
        assert_tables_bit_identical(
            &TileTable::measure(&plane, w, h, &CLEANING, None),
            &naive_tile_table(&plane, w, h),
        );
    }

    /// P-3 / L-5: reuse whole-overlap tiles (apron included), recompute the
    /// edge tiles — bit-identical to measuring the masked plane outright.
    #[test]
    fn restricted_tiles_equal_measuring_the_masked_plane() {
        let (plane, w, h) = dirty_field();
        let mask: Vec<bool> = (0..w * h)
            .map(|k| {
                let (x, y) = ((k % w) as f64, (k / w) as f64);
                x + 0.7 * y > 150.0 && (x - 260.0).hypot(y - 200.0) < 230.0
            })
            .collect();
        let group = TileTable::measure(&plane, w, h, &CLEANING, None);
        let restricted = group.restricted(&plane, &mask, &CLEANING, None);
        let masked_plane: Vec<f32> = plane
            .iter()
            .zip(&mask)
            .map(|(&v, &m)| if m { v } else { f32::NAN })
            .collect();
        assert_tables_bit_identical(&restricted, &TileTable::measure(&masked_plane, w, h, &CLEANING, None));
        assert_tables_bit_identical(&restricted, &TileTable::measure_masked(&plane, w, h, &CLEANING, None, &mask));
    }

    #[test]
    fn the_model_is_bit_identical_with_and_without_a_pool_and_run_to_run() {
        let (plane, w, h) = dirty_field();
        let serial = BackgroundModel::fit(TileTable::measure(&plane, w, h, &CLEANING, None), 256).unwrap();
        let again = BackgroundModel::fit(TileTable::measure(&plane, w, h, &CLEANING, None), 256).unwrap();
        let pooled =
            BackgroundModel::fit(TileTable::measure(&plane, w, h, &CLEANING, Some(&pool())), 256).unwrap();
        for other in [&again, &pooled] {
            assert_tables_bit_identical(&serial.table, &other.table);
            assert_eq!(serial.accepted, other.accepted);
            let bits = |m: &BackgroundModel| m.surface.coeffs.iter().map(|c| c.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&serial), bits(other));
        }
    }

    #[test]
    fn a_plane_with_no_present_pixel_is_refused() {
        let (w, h) = (256usize, 256usize);
        let r = BackgroundModel::fit(TileTable::measure(&vec![0.0f32; w * h], w, h, &CLEANING, None), 256);
        assert_eq!(r.err(), Some(BackgroundRefusal::NoSamples));
    }

    /// L-4: a knot cell needs 8 accepted samples. Seven sample tiles in one
    /// cell of a 1024 × 768 plane at scale 256 refuse; eight fit.
    #[test]
    fn the_sample_floor_refuses_a_plane_with_no_supported_cell() {
        let (w, h) = (1024usize, 768usize);
        for (tiles, ok) in [(7usize, false), (8, true)] {
            let mut plane = vec![f32::NAN; w * h];
            for t in 0..tiles {
                let (x0, y0) = ((t % 4) * TILE_PX, (t / 4) * TILE_PX);
                for y in y0..y0 + TILE_PX {
                    for x in x0..x0 + TILE_PX {
                        plane[y * w + x] = 0.1;
                    }
                }
            }
            let r = BackgroundModel::fit(TileTable::measure(&plane, w, h, &CLEANING, None), 256);
            assert_eq!(r.is_ok(), ok, "{tiles} sample tiles");
            if !ok {
                assert_eq!(r.err(), Some(BackgroundRefusal::NoSupportedCell));
            }
        }
    }

    #[test]
    fn shared_refit_with_no_common_supported_cell_keeps_the_own_surfaces() {
        let (w, h) = (1024usize, 768usize);
        let half = |left: bool| -> Vec<f32> {
            (0..w * h)
                .map(|k| if ((k % w) < w / 2) == left { 0.1 } else { f32::NAN })
                .collect()
        };
        let (rm, tm) = (model(&half(true), w, h, 256), model(&half(false), w, h, 256));
        let pair = shared_refit(&rm, &tm);
        assert!(!pair.refit);
        assert_eq!(pair.shared, 0);
        assert_eq!(*pair.reference, rm.surface);
        assert_eq!(*pair.target, tm.surface);
    }

    /// Step 5: a tile-aligned 3 × 3 block offset by +20 % of sky is
    /// rejected and the surface stays on the sky. Prototype (an unaligned
    /// block, λ = 1e-2): 9–10 rejected, max node error ≤ 0.13 % of sky.
    #[test]
    fn the_rejection_rounds_remove_an_outlier_block() {
        let (w, h) = (1024usize, 768usize);
        let mut plane = noisy(w, h, |_, _| 0.1, 0.002, 5);
        for y in 320..416 {
            for x in 512..608 {
                plane[y * w + x] += 0.02;
            }
        }
        let m = model(&plane, w, h, 256);
        assert!(m.samples_rejected >= 9, "{} rejected", m.samples_rejected);
        for v in m.surface.nodes(32) {
            assert!((v as f64 - 0.1).abs() <= 0.005 * 0.1, "{v}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib stacking::ln::background -- --test-threads=4 2>&1 | tail -5`
Expected: compile errors naming `TileTable`, `BackgroundModel`, `CLEANING`, `shared_refit`.

- [ ] **Step 3: Implement the model** (append to `background.rs` after `fill_invalid_cells`, before `mod tests`; add `use std::borrow::Cow;` to the imports).

```rust
// ===================================================================
// The background model at the scale (spec 2026-10-04 §1.2, rulings
// L-1…L-6). One procedure for the LN reference and every target, each in
// its own noise units: 32-px tiles give robust samples (steps 1–3), a
// penalized tensor cubic B-spline with knot spacing = scale is fitted to
// them with two rejection rounds (steps 4–5) and a sample-count floor
// (step 6), and each reference–target pair is refitted once on the
// samples both kept (step 7, `shared_refit`). There is no cell validity
// gate and no neighbour fill: a tile with too few present pixels is simply
// not a sample, and the membrane penalty continues the surface over it.
// ===================================================================

/// Tile edge in pixels (L-2): non-overlapping, anchored at (0, 0).
pub const TILE_PX: usize = 32;
/// A tile is a sample when at least this many of its pixels are present
/// (a quarter of a full tile).
pub const MIN_TILE_PRESENT: usize = 256;
/// In-tile clip, in MAD-sigma of the tile (L-1).
pub const TILE_CLIP_SIGMA: f32 = 3.0;
/// Sample rejection against the fitted surface, in MAD-sigma of the
/// residuals (L-1).
pub const SAMPLE_CLIP_SIGMA: f64 = 3.0;
/// Rejection rounds; the fit after the last round is final (step 5).
pub const SAMPLE_REJECTION_ROUNDS: usize = 2;
/// A knot cell (scale × scale) is supported at this many accepted samples
/// (L-4). A plane is refused only when no cell is supported.
pub const MIN_SAMPLES_PER_KNOT_CELL: usize = 8;
/// λ = this × the median non-zero diagonal of BᵀB (L-3 as amended by
/// P-20: 1e-2, not 1e-4 — at 1e-4 a coefficient the data reach only through
/// a B-spline tail is too weakly held and amplifies sample noise into gaps,
/// up to 6.8 % of sky per frame on a project member's uncovered pixels).
pub const MEMBRANE_PENALTY_REL: f64 = 1e-2;
const MAD_TO_SIGMA_F64: f64 = 1.4826;

/// The pixel cleaning both models share (unchanged from the cell model):
/// the hot-pixel pass and the `[low_clip, high_clip_rel]` clip, with the
/// 2026-09-10 ruling that 0.85 is 0.85 of full scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CleaningParams {
    pub hot_radius: usize,
    pub low_clip: f32,
    pub high_clip_rel: f32,
}

pub const CLEANING: CleaningParams = CleaningParams {
    hot_radius: 2,
    low_clip: 4.5e-5,
    high_clip_rel: 0.85,
};

/// One tile's statistic (spec §1.2 step 3). `value`/`sigma` are NaN when
/// the tile is not a sample. `max` is the largest hot-pixel-corrected
/// finite pixel taken BEFORE the clip (NaN with none) — the plane maximum
/// the star selection compares with (L-14).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileStat {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub present: u32,
    pub value: f32,
    pub sigma: f32,
    pub max: f32,
}

impl TileStat {
    pub fn is_sample(&self) -> bool {
        self.present as usize >= MIN_TILE_PRESENT
    }

    /// The centre of the tile's pixel extent (not of its present pixels).
    pub fn centre(&self) -> (f64, f64) {
        (
            (self.x0 + self.x1 - 1) as f64 * 0.5,
            (self.y0 + self.y1 - 1) as f64 * 0.5,
        )
    }
}

/// Every tile of one plane, row-major (`tiles[v * tw + u]`).
#[derive(Debug, Clone, PartialEq)]
pub struct TileTable {
    pub width: usize,
    pub height: usize,
    pub tw: usize,
    pub th: usize,
    pub tiles: Vec<TileStat>,
}

#[derive(Default)]
struct TileScratch {
    vals: Vec<f32>,
    dev: Vec<f32>,
    window: Vec<f32>,
}

impl TileTable {
    /// Steps 1–3 over the whole plane. Each tile cleans its own pixels,
    /// reading a `hot_radius` apron from `plane` for the hot-pixel window,
    /// so the result equals the old full-plane pass pixel for pixel
    /// (`tile_statistics_match_the_full_plane_cleaning`) with no full-plane
    /// copy. A tile is a pure function of `plane`, so the parallel collect
    /// (index order) is bit-identical to a serial loop.
    pub fn measure(
        plane: &[f32],
        width: usize,
        height: usize,
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> TileTable {
        Self::measure_where(plane, width, height, p, pool, None)
    }

    /// As [`Self::measure`], with every pixel outside `mask` absent —
    /// including inside the hot-pixel window. Equal to `measure` on a copy
    /// of `plane` with NaN outside `mask`.
    pub fn measure_masked(
        plane: &[f32],
        width: usize,
        height: usize,
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
        mask: &[bool],
    ) -> TileTable {
        Self::measure_where(plane, width, height, p, pool, Some(mask))
    }

    fn measure_where(
        plane: &[f32],
        width: usize,
        height: usize,
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
        mask: Option<&[bool]>,
    ) -> TileTable {
        if width == 0
            || height == 0
            || plane.len() < width.saturating_mul(height)
            || mask.is_some_and(|m| m.len() < width * height)
        {
            return TileTable { width, height, tw: 0, th: 0, tiles: Vec::new() };
        }
        let plane = &plane[..width * height];
        let (tw, th) = (width.div_ceil(TILE_PX), height.div_ceil(TILE_PX));
        let body = || {
            (0..tw * th)
                .into_par_iter()
                .map_init(TileScratch::default, |s, idx| {
                    let (x0, y0) = ((idx % tw) * TILE_PX, (idx / tw) * TILE_PX);
                    let (x1, y1) = ((x0 + TILE_PX).min(width), (y0 + TILE_PX).min(height));
                    measure_tile(plane, width, height, (x0, y0, x1, y1), p, mask, s)
                })
                .collect::<Vec<TileStat>>()
        };
        let tiles = match pool {
            Some(pl) => pl.install(body),
            None => body(),
        };
        TileTable { width, height, tw, th, tiles }
    }

    /// A project member's reference tiles (L-5, P-3): a tile whose extent
    /// AND hot-pixel apron lie inside `mask` keeps this table's statistic
    /// bit for bit; a tile with no pixel inside is absent; any other tile is
    /// recomputed on its masked pixels. Identical to
    /// [`Self::measure_masked`] (`restricted_tiles_equal_measuring_the_masked_plane`).
    pub fn restricted(
        &self,
        plane: &[f32],
        mask: &[bool],
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> TileTable {
        let (w, h) = (self.width, self.height);
        if self.tiles.is_empty() || plane.len() < w * h || mask.len() < w * h {
            return TileTable { width: w, height: h, tw: 0, th: 0, tiles: Vec::new() };
        }
        let r = p.hot_radius;
        let body = || {
            self.tiles
                .par_iter()
                .map_init(TileScratch::default, |s, t| {
                    let (x0, y0, x1, y1) = (t.x0 as usize, t.y0 as usize, t.x1 as usize, t.y1 as usize);
                    let (ax0, ay0) = (x0.saturating_sub(r), y0.saturating_sub(r));
                    let (ax1, ay1) = ((x1 + r).min(w), (y1 + r).min(h));
                    let (mut any_in_tile, mut all_in_apron) = (false, true);
                    for y in ay0..ay1 {
                        for x in ax0..ax1 {
                            let m = mask[y * w + x];
                            all_in_apron &= m;
                            if m && (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                                any_in_tile = true;
                            }
                        }
                    }
                    if all_in_apron {
                        *t
                    } else if !any_in_tile {
                        TileStat { present: 0, value: f32::NAN, sigma: f32::NAN, max: f32::NAN, ..*t }
                    } else {
                        measure_tile(plane, w, h, (x0, y0, x1, y1), p, Some(mask), s)
                    }
                })
                .collect::<Vec<TileStat>>()
        };
        let tiles = match pool {
            Some(pl) => pl.install(body),
            None => body(),
        };
        TileTable { width: w, height: h, tw: self.tw, th: self.th, tiles }
    }

    /// The plane maximum for relative saturation (L-14): the largest
    /// hot-pixel-corrected finite pixel, NaN when the plane has none.
    pub fn plane_max(&self) -> f32 {
        self.tiles
            .iter()
            .map(|t| t.max)
            .filter(|m| m.is_finite())
            .fold(f32::NAN, |acc, m| if acc.is_nan() || m > acc { m } else { acc })
    }
}

/// Steps 2–3 for one tile. `mask` (when given) makes every pixel outside it
/// absent, inside the hot-pixel window too.
fn measure_tile(
    plane: &[f32],
    width: usize,
    height: usize,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
    p: &CleaningParams,
    mask: Option<&[bool]>,
    s: &mut TileScratch,
) -> TileStat {
    let at = |k: usize| -> f32 {
        match mask {
            Some(m) if !m[k] => f32::NAN,
            _ => plane[k],
        }
    };
    let high = p.high_clip_rel;
    s.vals.clear();
    let mut max = f32::NEG_INFINITY;
    for y in y0..y1 {
        for x in x0..x1 {
            let mut v = at(y * width + x);
            if !v.is_finite() {
                continue;
            }
            if v > high {
                s.window.clear();
                for wy in y.saturating_sub(p.hot_radius)..=(y + p.hot_radius).min(height - 1) {
                    for wx in x.saturating_sub(p.hot_radius)..=(x + p.hot_radius).min(width - 1) {
                        let wv = at(wy * width + wx);
                        if wv.is_finite() {
                            s.window.push(wv);
                        }
                    }
                }
                let med = median_in_place(&mut s.window);
                let mad = mad_about(&s.window, med);
                if v > med + HOT_PIXEL_SIGMA * MAD_TO_SIGMA * mad {
                    v = med;
                }
            }
            max = max.max(v);
            if v >= p.low_clip && v <= high {
                s.vals.push(v);
            }
        }
    }
    let max = if max == f32::NEG_INFINITY { f32::NAN } else { max };
    let present = s.vals.len() as u32;
    let (x0, y0, x1, y1) = (x0 as u32, y0 as u32, x1 as u32, y1 as u32);
    if s.vals.len() < MIN_TILE_PRESENT {
        return TileStat { x0, y0, x1, y1, present, value: f32::NAN, sigma: f32::NAN, max };
    }
    for _ in 0..MAX_SIGMA_CLIP_ROUNDS {
        let med = median_in_place(&mut s.vals);
        s.dev.clear();
        s.dev.extend(s.vals.iter().map(|&v| (v - med).abs()));
        let mad = median_in_place(&mut s.dev);
        if mad <= 0.0 {
            break;
        }
        let bound = TILE_CLIP_SIGMA * MAD_TO_SIGMA * mad;
        let before = s.vals.len();
        s.vals.retain(|&v| (v - med).abs() <= bound);
        if s.vals.len() == before {
            break;
        }
    }
    let value = median_in_place(&mut s.vals);
    s.dev.clear();
    s.dev.extend(s.vals.iter().map(|&v| (v - value).abs()));
    let sigma = MAD_TO_SIGMA * median_in_place(&mut s.dev);
    TileStat { x0, y0, x1, y1, present, value, sigma, max }
}

/// A smooth background surface: a tensor-product uniform cubic B-spline on
/// knots `t_k = (k − 3)·knot` along each axis (L-3), `nx × ny` coefficients
/// row-major (`coeffs[j * nx + i]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    pub width: usize,
    pub height: usize,
    pub knot: f64,
    pub nx: usize,
    pub ny: usize,
    pub coeffs: Vec<f64>,
}

/// The four uniform cubic B-spline weights at local position `t ∈ [0, 1]`.
fn cubic_weights(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        (1.0 - t).powi(3) / 6.0,
        (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0,
        (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0,
        t3 / 6.0,
    ]
}

/// The span (first of the four active basis functions) and local `t` of a
/// coordinate. The last pixel of an extent that is an exact multiple of the
/// knot spacing lands at `t = 1` of the last span.
fn span(v: f64, knot: f64, n: usize) -> (usize, f64) {
    let u = v / knot;
    let m = (u.floor().max(0.0) as usize).min(n - 4);
    (m, u - m as f64)
}

impl Surface {
    /// `(nx, ny)` for a plane at a knot spacing: `ceil((extent − 1)/knot) + 3`.
    fn dims(width: usize, height: usize, knot: usize) -> (usize, usize) {
        (
            (width - 1).div_ceil(knot) + 3,
            (height - 1).div_ceil(knot) + 3,
        )
    }

    pub fn eval(&self, x: f64, y: f64) -> f64 {
        let x = x.clamp(0.0, (self.width - 1) as f64);
        let y = y.clamp(0.0, (self.height - 1) as f64);
        let (mx, tx) = span(x, self.knot, self.nx);
        let (my, ty) = span(y, self.knot, self.ny);
        let (wx, wy) = (cubic_weights(tx), cubic_weights(ty));
        let mut s = 0.0;
        for (b, wyb) in wy.iter().enumerate() {
            let row = (my + b) * self.nx + mx;
            for (a, wxa) in wx.iter().enumerate() {
                s += wyb * wxa * self.coeffs[row + a];
            }
        }
        s
    }

    /// The surface at every LN mesh node (spec §1.2 step 8): node `(i, j)`
    /// at pixel `(i·stride, j·stride)`, clamped into the plane, row-major —
    /// the layout `BackgroundGrid::cells` had and `LnGrid::b` uses.
    pub fn nodes(&self, stride: usize) -> Vec<f32> {
        let (gw, gh) = LnGrid::grid_dims(self.width, self.height, stride);
        let mut out = Vec::with_capacity(gw * gh);
        for j in 0..gh {
            let y = (j * stride).min(self.height - 1) as f64;
            for i in 0..gw {
                let x = (i * stride).min(self.width - 1) as f64;
                out.push(self.eval(x, y) as f32);
            }
        }
        out
    }
}

/// Why a plane has no background model (spec §1.2 step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundRefusal {
    /// No tile has [`MIN_TILE_PRESENT`] present pixels.
    NoSamples,
    /// No knot cell holds [`MIN_SAMPLES_PER_KNOT_CELL`] accepted samples.
    NoSupportedCell,
    /// The normal matrix was not positive definite (never seen; kept so a
    /// numeric failure is an error, not a NaN surface).
    Singular,
}

impl std::fmt::Display for BackgroundRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BackgroundRefusal::NoSamples => "no background sample",
            BackgroundRefusal::NoSupportedCell => "no supported knot cell",
            BackgroundRefusal::Singular => "the background fit is singular",
        })
    }
}

impl std::error::Error for BackgroundRefusal {}

/// One plane's own-threshold model (spec §1.2 steps 4–6). `accepted[k]` is
/// tile `k`'s membership in the final fit; `samples` counts them,
/// `samples_rejected` the samples the rounds left out, `cells` the knot
/// cells and `cells_unsupported` those under the floor.
#[derive(Debug, Clone)]
pub struct BackgroundModel {
    pub table: TileTable,
    pub scale: u32,
    pub surface: Surface,
    pub accepted: Vec<bool>,
    pub samples: usize,
    pub samples_rejected: usize,
    pub cells_unsupported: usize,
    pub cells: usize,
}

impl BackgroundModel {
    pub fn fit(table: TileTable, scale: u32) -> Result<BackgroundModel, BackgroundRefusal> {
        let (width, height) = (table.width, table.height);
        if width < 2 || height < 2 || table.tiles.is_empty() {
            return Err(BackgroundRefusal::NoSamples);
        }
        let knot = scale.max(1) as usize;
        let (nx, ny) = Surface::dims(width, height, knot);
        let values: Vec<f32> = table.tiles.iter().map(|t| t.value).collect();
        let all: Vec<usize> = (0..table.tiles.len()).filter(|&k| table.tiles[k].is_sample()).collect();
        if all.is_empty() {
            return Err(BackgroundRefusal::NoSamples);
        }
        let solve = |ids: &[usize]| -> Result<Vec<f64>, BackgroundRefusal> {
            fit_coeffs(&table, knot as f64, nx, ny, ids, &[&values])
                .map(|mut c| c.remove(0))
                .ok_or(BackgroundRefusal::Singular)
        };
        let mut accepted_ids = all.clone();
        let mut surface = Surface { width, height, knot: knot as f64, nx, ny, coeffs: solve(&accepted_ids)? };
        let mut resid = vec![0f64; table.tiles.len()];
        for _ in 0..SAMPLE_REJECTION_ROUNDS {
            for &k in &all {
                let (x, y) = table.tiles[k].centre();
                resid[k] = values[k] as f64 - surface.eval(x, y);
            }
            let sigma_e = MAD_TO_SIGMA_F64 * mad_f64(accepted_ids.iter().map(|&k| resid[k]));
            // P-21: tile values are f32, so a residual MAD below a few f32
            // ulps of the level is rounding, not noise — it is σ_e ≤ 0 here.
            let level = median_f64_sorted(&mut accepted_ids.iter().map(|&k| values[k] as f64).collect::<Vec<_>>()).abs();
            let next: Vec<usize> = if sigma_e > 1e-6 * level {
                all.iter()
                    .copied()
                    .filter(|&k| resid[k].abs() <= SAMPLE_CLIP_SIGMA * sigma_e)
                    .collect()
            } else {
                all.clone()
            };
            // P-9: a round that would keep nothing rejects nothing.
            accepted_ids = if next.is_empty() { all.clone() } else { next };
            surface.coeffs = solve(&accepted_ids)?;
        }
        let (supported, cells) = supported_cells(&table, knot as f64, nx, ny, &accepted_ids);
        if supported == 0 {
            return Err(BackgroundRefusal::NoSupportedCell);
        }
        let mut accepted = vec![false; table.tiles.len()];
        for &k in &accepted_ids {
            accepted[k] = true;
        }
        Ok(BackgroundModel {
            samples: accepted_ids.len(),
            samples_rejected: all.len() - accepted_ids.len(),
            cells_unsupported: cells - supported,
            cells,
            accepted,
            surface,
            scale,
            table,
        })
    }
}

/// `(supported, total)` knot cells for the samples `ids` (L-4).
fn supported_cells(table: &TileTable, knot: f64, nx: usize, ny: usize, ids: &[usize]) -> (usize, usize) {
    let (cx, cy) = (nx - 3, ny - 3);
    let mut counts = vec![0usize; cx * cy];
    for &k in ids {
        let (x, y) = table.tiles[k].centre();
        let (mx, _) = span(x, knot, nx);
        let (my, _) = span(y, knot, ny);
        counts[my * cx + mx] += 1;
    }
    (counts.iter().filter(|&&c| c >= MIN_SAMPLES_PER_KNOT_CELL).count(), cx * cy)
}

/// `median |e − median(e)|` in f64 (even counts: the mean of the middle two).
fn mad_f64(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return 0.0;
    }
    let med = median_f64_sorted(&mut v);
    let mut dev: Vec<f64> = v.iter().map(|e| (e - med).abs()).collect();
    median_f64_sorted(&mut dev)
}

fn median_f64_sorted(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Penalized least squares (L-3) on the samples `ids` (ascending tile
/// order), one solution per right-hand side `rhs[r][tile]`. The normal
/// matrix and right-hand sides accumulate in f64 in `ids` order and the
/// solve is a serial dense Cholesky, so the fit is deterministic. `None`
/// only when the matrix is not positive definite.
fn fit_coeffs(
    table: &TileTable,
    knot: f64,
    nx: usize,
    ny: usize,
    ids: &[usize],
    rhs: &[&[f32]],
) -> Option<Vec<Vec<f64>>> {
    let n = nx * ny;
    let mut ata = vec![0f64; n * n];
    let mut atb = vec![vec![0f64; n]; rhs.len()];
    let (mut idx, mut w) = ([0usize; 16], [0f64; 16]);
    for &k in ids {
        let (x, y) = table.tiles[k].centre();
        let (mx, tx) = span(x, knot, nx);
        let (my, ty) = span(y, knot, ny);
        let (wx, wy) = (cubic_weights(tx), cubic_weights(ty));
        for b in 0..4 {
            for a in 0..4 {
                idx[b * 4 + a] = (my + b) * nx + mx + a;
                w[b * 4 + a] = wy[b] * wx[a];
            }
        }
        for p in 0..16 {
            let row = idx[p] * n;
            for q in 0..16 {
                ata[row + idx[q]] += w[p] * w[q];
            }
            for (r, vals) in rhs.iter().enumerate() {
                atb[r][idx[p]] += w[p] * vals[k] as f64;
            }
        }
    }
    let mut diag: Vec<f64> = (0..n).map(|k| ata[k * n + k]).filter(|&d| d > 0.0).collect();
    if diag.is_empty() {
        return None;
    }
    let lambda = MEMBRANE_PENALTY_REL * median_f64_sorted(&mut diag);
    let mut couple = |a: usize, b: usize| {
        ata[a * n + a] += lambda;
        ata[b * n + b] += lambda;
        ata[a * n + b] -= lambda;
        ata[b * n + a] -= lambda;
    };
    for j in 0..ny {
        for i in 0..nx {
            let a = j * nx + i;
            if i + 1 < nx {
                couple(a, a + 1);
            }
            if j + 1 < ny {
                couple(a, a + nx);
            }
        }
    }
    cholesky_in_place(&mut ata, n)?;
    Some(atb.into_iter().map(|b| cholesky_solve(&ata, n, b)).collect())
}

/// Lower Cholesky factor in place (the upper triangle is left stale).
/// `None` on a non-positive pivot.
fn cholesky_in_place(m: &mut [f64], n: usize) -> Option<()> {
    for j in 0..n {
        let mut d = m[j * n + j];
        for k in 0..j {
            d -= m[j * n + k] * m[j * n + k];
        }
        if !(d > 0.0) {
            return None;
        }
        let d = d.sqrt();
        m[j * n + j] = d;
        for i in j + 1..n {
            let mut s = m[i * n + j];
            for k in 0..j {
                s -= m[i * n + k] * m[j * n + k];
            }
            m[i * n + j] = s / d;
        }
    }
    Some(())
}

fn cholesky_solve(l: &[f64], n: usize, mut b: Vec<f64>) -> Vec<f64> {
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= l[i * n + k] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in i + 1..n {
            s -= l[k * n + i] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
    b
}

/// The two surfaces `B = B_R − A·B_T` is formed from (spec §1.2 step 7,
/// L-6). `refit` is false when the pair had no supported knot cell in
/// common (or, unreachably, different geometry): the two own-threshold
/// surfaces are then kept and the caller logs one `warn!` per frame.
#[derive(Debug, Clone)]
pub struct PairSurfaces<'a> {
    pub reference: Cow<'a, Surface>,
    pub target: Cow<'a, Surface>,
    pub shared: usize,
    pub refit: bool,
}

/// The symmetric shared-acceptance refit: both surfaces refitted once, with
/// one factorization and two right-hand sides, on the tiles BOTH final fits
/// kept. No rejection round; each side keeps its own sample values.
pub fn shared_refit<'a>(reference: &'a BackgroundModel, target: &'a BackgroundModel) -> PairSurfaces<'a> {
    let own = |shared: usize| PairSurfaces {
        reference: Cow::Borrowed(&reference.surface),
        target: Cow::Borrowed(&target.surface),
        shared,
        refit: false,
    };
    let (rt, tt) = (&reference.table, &target.table);
    if (rt.width, rt.height, rt.tw, rt.th, reference.scale) != (tt.width, tt.height, tt.tw, tt.th, target.scale) {
        debug_assert!(false, "shared_refit on two geometries");
        return own(0);
    }
    let ids: Vec<usize> = (0..rt.tiles.len())
        .filter(|&k| reference.accepted[k] && target.accepted[k])
        .collect();
    let s = &reference.surface;
    if supported_cells(rt, s.knot, s.nx, s.ny, &ids).0 == 0 {
        return own(ids.len());
    }
    let rv: Vec<f32> = rt.tiles.iter().map(|t| t.value).collect();
    let tv: Vec<f32> = tt.tiles.iter().map(|t| t.value).collect();
    match fit_coeffs(rt, s.knot, s.nx, s.ny, &ids, &[&rv, &tv]) {
        Some(mut c) => {
            let tgt = c.pop().expect("two right-hand sides");
            let rf = c.pop().expect("two right-hand sides");
            PairSurfaces {
                reference: Cow::Owned(Surface { coeffs: rf, ..s.clone() }),
                target: Cow::Owned(Surface { coeffs: tgt, ..target.surface.clone() }),
                shared: ids.len(),
                refit: true,
            }
        }
        None => own(ids.len()),
    }
}
```

In `stacking/ln/mod.rs`, extend the `pub use background::{…}` list (lines 54–56) with `shared_refit, BackgroundModel, BackgroundRefusal, CleaningParams, PairSurfaces, Surface, TileStat, TileTable, CLEANING` (keep the old names; Task 2 removes them).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib stacking::ln::background -- --test-threads=4 2>&1 | grep -E '^test |test result'`
Expected: every old and new test in the module passes. If a P-2 pin misses its bar, STOP and report the numbers beside the prototype's (P-2). Never change the fixture or the bar.

- [ ] **Step 5: Cross-check against the prototype.** `docs/superpowers/research/scripts/ln-model/bg_prototype.py` (committed with this plan) is the numpy oracle of the same procedure; `python3 bg_prototype_pins.py` prints the numbers the pins' doc comments quote. Report the Rust pins' measured values beside them (they differ by RNG only).

- [ ] **Step 6: Run the gate** (Global Constraints). `git diff --exit-code src/types`.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/stacking/ln/background.rs crates/athenaeum-core/src/stacking/ln/mod.rs
git commit -m "feat(ln): the background model at the scale — tiles, penalized B-spline, rejection rounds, shared refit (not wired yet)"
```

---

### Task 2: Background model wiring; the cell model retired; `LN_MODEL_VERSION = 2`

Spec §1.1, §1.2 step 8, §1.3 (masked members), §1.6, §7 (`LN_MODEL_VERSION`, `LnArtifactPayload`), §10; rulings L-5, L-19, P-3, P-4.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/ln/background.rs` (delete the cell model and its tests)
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs` (re-exports; `LN_MODEL_VERSION`; `LnFrameOutcome`; `target_background_params` deleted; `normalize_frame`; tests at ~1597, ~1744, ~1985, ~2141, ~2312)
- Modify: `crates/athenaeum-core/src/stacking/ln/calibration.rs` (`measure_seeds_calibration`, `run_arm`)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (imports 79–81; `LnArtifactPayload` 10152–10170; reference models 10448–10502; seeds calibration 10726–10742 and 11224–11311; fan-out 10967–10981; payload 11060; log 11097–11112)
- Modify: `crates/athenaeum-core/src/stacking/config.rs:811-829` (`lnModelVersion` in the subtree; a new test)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs:~7582-7585` (golden `ln_ref`/`ln`/`ln_cal` literals)
- Modify: `crates/athenaeum-core/examples/ln_probe.rs` (port to the new API)

**Interfaces:**
- Consumes (Task 1): `TileTable::{measure, restricted}`, `BackgroundModel::fit`, `shared_refit`, `Surface::nodes`, `CLEANING`, `BackgroundRefusal`.
- Produces:
  - `pub const LN_MODEL_VERSION: u32 = 2;` in `stacking::ln` (Task 11 sets 3);
  - `normalize_frame(reference, reference_for_detection, ref_models: &[BackgroundModel], frame, seeds, cfg, scale_px: u32, measure, interpolation, clamping, sidecar, seeds_calibration, pool, cancel) -> Result<LnFrameOutcome, LnError>` (P-4: `scale_px` follows `cfg`);
  - `measure_seeds_calibration(reference, reference_for_detection, ref_models: &[BackgroundModel], frames, indices, load_fits, cfg, scale_px: u32, measure, interpolation, clamping, scratch_dir, pool, cancel, on_progress)`;
  - `LnFrameOutcome { …, samples: usize, samples_rejected: usize, samples_shared: usize, cells_unsupported: usize, … }` (`cells_rejected` removed; all four are sums over channels).

- [ ] **Step 1: Write the failing tests.**

In `config.rs`'s `mod tests`:
```rust
    #[test]
    fn the_ln_model_version_rides_the_normalization_subtree_only() {
        let cfg = StackingConfig::default();
        assert_eq!(
            normalization_subtree(&cfg)["lnModelVersion"],
            serde_json::json!(crate::stacking::ln::LN_MODEL_VERSION)
        );
        for other in [measurement_subtree(&cfg), registration_subtree(&cfg)] {
            assert!(other.get("lnModelVersion").is_none(), "{other}");
        }
    }
```
In `run.rs`'s test module, beside `local_normalization_falls_back_to_detection_when_a_fits_artifact_is_missing` (~17608), a pin on the new payload fields (reuse `seed_ln_star_group` and the run helper that test uses):
```rust
    #[test]
    fn the_ln_payload_records_the_background_sample_counts() {
        // Same fixture and run as `local_normalization_writes_one_sidecar_per_included_frame_and_a_reference`.
        // For every `ln` row: deserialize `LnArtifactPayload` and assert
        // `samples > 0`, `samples_shared > 0`, `samples_shared <= samples`,
        // and that the JSON has no `cells_rejected` key.
    }
```
(Write the body by copying the setup of `local_normalization_writes_one_sidecar_per_included_frame_and_a_reference` verbatim and replacing its assertions with the four above.)

- [ ] **Step 2: Run them to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib the_ln_model_version_rides the_ln_payload_records -- --test-threads=4 2>&1 | tail -5`
Expected: compile errors (`LN_MODEL_VERSION`, `samples` on the payload).

- [ ] **Step 3: `LN_MODEL_VERSION` and the subtree.** In `stacking/ln/mod.rs`, after the `pub use` block:
```rust
/// The LN model's own version (spec 2026-10-04 §7, L-19), folded into the
/// normalization subtree so a change in what an existing config produces
/// moves the `ln`, `ln_reference` and `ln_calibration` hashes together.
/// 1 = the cell model (implicit, never written); 2 = the background model
/// at the scale; bump it again whenever an existing config's output moves.
pub const LN_MODEL_VERSION: u32 = 2;
```
In `config.rs` `normalization_subtree_with_fit_version`, add `"lnModelVersion": crate::stacking::ln::LN_MODEL_VERSION,` to the `json!` object, and extend the function's doc with one sentence naming it.

- [ ] **Step 4: `normalize_frame`.** Edit `stacking/ln/mod.rs`:
  1. Delete `target_background_params` (345–351).
  2. Signature: replace `ref_backgrounds: &[BackgroundGrid]` with `ref_models: &[BackgroundModel]`, and add `scale_px: u32,` right after `cfg: &LocalNormalizationConfig,`. Update the doc paragraph that names `background_grid`/`TARGET_DEVIATION_SIGMA`: the target is modelled by the same procedure as the reference (L-1), and the two surfaces come from `shared_refit` (L-6).
  3. The `ref_backgrounds.len() != channels` check becomes `ref_models.len() != channels` with the message `"{} reference background models for a {channels}-channel LN reference"`.
  4. `let stride = (scale_px / 8).max(2) as usize;` and delete `let target_params = …`.
  5. Counters: replace `let mut cells_rejected_total = 0usize;` with
     ```rust
     let (mut samples_total, mut samples_rejected_total) = (0usize, 0usize);
     let (mut samples_shared_total, mut cells_unsupported_total) = (0usize, 0usize);
     let mut warned_no_shared = false;
     ```
  6. Masked block (830–879): keep the overlap computation, the `LowOverlap` check and the target masking verbatim. Replace the `ref_masked` + `background_grid` build with the restricted model (L-5, P-3):
     ```rust
            let restricted = ref_models[p].table.restricted(&reference.planes[p], &overlap, &CLEANING, pool);
            member_ref_model = Some(BackgroundModel::fit(restricted, scale_px).map_err(|r| {
                tracing::warn!(
                    frame_id = frame.frame_id,
                    plane = p,
                    error = %r,
                    "ln: the LN reference's background model on this frame's overlap is refused"
                );
                LnError::Other(format!("background model of the LN reference on the overlap: {r}"))
            })?);
     ```
     Rename `member_ref_bg: Option<BackgroundGrid>` to `member_ref_model: Option<BackgroundModel>`. Keep `allowed` as is.
  7. Target model (replace 881–912's `background_grid` + refusal + `cells_rejected_total` lines):
     ```rust
        let t = Instant::now();
        let target_model = BackgroundModel::fit(
            TileTable::measure(&target, reference.width, reference.height, &CLEANING, pool),
            scale_px,
        )
        .map_err(|r| {
            tracing::warn!(frame_id = frame.frame_id, plane = p, error = %r, "ln: the frame's background model is refused");
            LnError::Other(format!("background model: {r}"))
        })?;
        let ref_model = member_ref_model.as_ref().unwrap_or(&ref_models[p]);
        let pair = background::shared_refit(ref_model, &target_model);
        if !pair.refit && !warned_no_shared {
            tracing::warn!(
                frame_id = frame.frame_id,
                plane = p,
                "no background sample in common with the LN reference; own-threshold backgrounds kept"
            );
            warned_no_shared = true;
        }
        let (expected_gw, expected_gh) = LnGrid::grid_dims(reference.width, reference.height, stride);
        let ref_nodes = pair.reference.nodes(stride);
        let tgt_nodes = pair.target.nodes(stride);
        samples_total += target_model.samples;
        samples_rejected_total += target_model.samples_rejected;
        samples_shared_total += pair.shared;
        cells_unsupported_total += target_model.cells_unsupported;
        let location_tgt = median_of_finite(&target);
        background_ms += t.elapsed().as_millis() as u64;
     ```
     (`pair` borrows `ref_model`/`target_model`; compute the two node vectors here, then let `pair` drop before `target` is sanitized in place — it does not borrow `target`, so no reordering is needed beyond keeping `location_tgt` where it is.)
  8. Delete the old `let ref_bg = member_ref_bg…` line (1074). The length check (1086–1093) compares `ref_nodes.len()`, `tgt_nodes.len()` and `expected_cells`, with the same message shape (`"background grid size mismatch: reference has {} nodes, target has {}, the stride mesh has {expected_cells}"`).
  9. `b` is formed from `ref_nodes`/`tgt_nodes` (`br - ak * bt`, same zip order).
  10. `LnGrid { …, scale: scale_px, … }`.
  11. The outcome: replace `cells_rejected: cells_rejected_total,` with the four new fields.
  12. `LnFrameOutcome`: replace `pub cells_rejected: usize,` with
      ```rust
      // Background samples of the frame's own final fit, the samples its
      // rejection rounds left out, the samples of the shared refit, and the
      // knot cells under the floor — each summed over channels (spec §10).
      pub samples: usize,
      pub samples_rejected: usize,
      pub samples_shared: usize,
      pub cells_unsupported: usize,
      ```
      and rewrite the struct doc's "`matches`/`cells_rejected` are the SUM" sentence accordingly.
  13. Module doc (lines 15–29): replace the "(just a looser deviation threshold — `TARGET_DEVIATION_SIGMA`)" phrase with "by the same procedure and thresholds (L-1), then both surfaces are refitted on the samples both kept (L-6)".

- [ ] **Step 5: Retire the cell model.** In `background.rs` delete `CELL_SUBSAMPLE_THRESHOLD`, `BackgroundParams`, `DEFAULT_PARAMS`, `TARGET_DEVIATION_SIGMA`, `BackgroundGrid`, `CellScratch`, `background_grid` (with its long doc, but KEEP the C-29 "measured out" paragraph: move it verbatim into the new module doc under a heading line "Measured out (do not re-propose)"), `high_clip_threshold`, `clean_plane`, `gather_cell`, `robust_cell_level`, `fill_invalid_cells`, and these tests: `flat_plane_with_stars_recovers_the_flat_level`, `vertical_gradient_is_tracked_per_cell`, `a_cell_that_is_mostly_star_is_invalid_and_filled_from_neighbours`, `trailing_node_overshoot_gets_a_real_window`, `robust_cell_level_over_rejected_by_the_iterative_clip_is_invalid`, `robust_cell_level_on_scratch_matches_a_fresh_allocation_every_round`, `a_plane_entirely_below_the_low_clip_is_fully_invalid`, `background_grid_is_bit_identical_with_and_without_a_pool`. **Keep the `fn pool()` test helper**: Task 1's `the_model_is_bit_identical_with_and_without_a_pool_and_run_to_run` uses it (review I-6). Rewrite the module doc (lines 1–30) to describe the model at the scale (spec §1.2 in five sentences, the cleaning ruling of 2026-09-10 kept verbatim, then the C-29 paragraph). In `mod.rs` remove the old names from the `pub use background::{…}` list.

  Add one trailing-node pin to replace `trailing_node_overshoot_gets_a_real_window`:
  ```rust
    /// The trailing node overshoots the plane by design and is clamped into
    /// it: on 1026 × 514 at scale 256 every node of a flat plane is the sky.
    #[test]
    fn trailing_nodes_are_clamped_into_the_plane() {
        let (w, h) = (1026usize, 514usize);
        let m = model(&vec![0.1f32; w * h], w, h, 256);
        let nodes = m.surface.nodes(32);
        assert_eq!(nodes.len(), { let (gw, gh) = LnGrid::grid_dims(w, h, 32); gw * gh });
        assert!(nodes.iter().all(|v| (v - 0.1).abs() < 1e-6), "{nodes:?}");
    }
  ```

- [ ] **Step 6: The seeds calibration.** In `calibration.rs`, `measure_seeds_calibration` and `run_arm` take `ref_models: &[BackgroundModel]` in place of `ref_backgrounds: &[BackgroundGrid]` and `scale_px: u32` after `cfg`, and pass both to `normalize_frame` unchanged. Update the imports.

- [ ] **Step 7: The run** (`run.rs`; hand-format, never rustfmt).
  1. Imports (79–81): replace `background_grid, … BackgroundGrid, BackgroundParams, … DEFAULT_PARAMS` with `BackgroundModel, TileTable, CLEANING`.
  2. Reference models (10448–10502): replace `ref_params` + `ref_backgrounds` + the "no measurable cell" refusal with:
     ```rust
    let scale_px = ln_cfg.scale;
    let bg_pool = Some(&rc.ctx.image_pool);
    let mut ref_models: Vec<BackgroundModel> = Vec::with_capacity(ln_reference.planes.len());
    for (p, plane) in ln_reference.planes.iter().enumerate() {
        let table = TileTable::measure(plane, ln_reference.width, ln_reference.height, &CLEANING, bg_pool);
        match BackgroundModel::fit(table, scale_px) {
            Ok(m) => {
                tracing::debug!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    channel = p,
                    ln_samples = m.samples,
                    ln_samples_rejected = m.samples_rejected,
                    ln_cells_unsupported = m.cells_unsupported,
                    "ln reference background modelled"
                );
                ref_models.push(m);
            }
            Err(r) => {
                // keep the existing warn!/rc.warnings.push/`return Ok(LnGroupOutcome { reference_path: None, .. })`
                // shape of the old "no measurable cell" branch (10483–10502), with
                // the message "local normalization off for this group: the LN reference's background model is refused"
                // (fields run_id, group_key, channel = p, error = %r) and the run warning
                // "group {}: local normalization off — the LN reference's background model is refused ({r}); using global normalization".
            }
        }
    }
     ```
     Write the `Err` arm out in full by adapting lines 10483–10502. Grep the tests for the old warning text (`"no measurable cell"`). If one asserts it, update that assertion to the new text and name it in the report.
  3. Every later use of `ref_backgrounds` (the seeds calibration call ~10726–10742, the `measure_ln_seeds_calibration` wrapper signature 11224–11239 and its inner call 11296–11311, the fan-out's captured `ref_backgrounds_ref`) becomes `ref_models` / `&ref_models`, and `scale_px` is passed next to `&ln_cfg`.
  4. `LnArtifactPayload` (10152–10170): replace `cells_rejected: usize,` with
     ```rust
    #[serde(default)]
    samples: usize,
    #[serde(default)]
    samples_rejected: usize,
    #[serde(default)]
    samples_shared: usize,
    #[serde(default)]
    cells_unsupported: usize,
     ```
     and fill them at 11060 from `outcome`.
  5. The `"ln frame normalized"` debug (11097–11112): replace `ln_cells_rejected = outcome.cells_rejected,` with `ln_samples = outcome.samples, ln_samples_rejected = outcome.samples_rejected, ln_samples_shared = outcome.samples_shared, ln_cells_unsupported = outcome.cells_unsupported,`.

- [ ] **Step 8: The `ln/mod.rs` tests.** At ~1641, ~1796, ~2047, ~2208 and ~2377 the tests build `ref_bg = vec![background_grid(…)]`. Add one helper in that test module and use it at all five sites (and pass `cfg.scale` as the new `scale_px` argument of `normalize_frame`):
  ```rust
    fn ref_models(reference: &LnReference, scale: u32) -> Vec<BackgroundModel> {
        reference
            .planes
            .iter()
            .map(|p| {
                BackgroundModel::fit(TileTable::measure(p, reference.width, reference.height, &CLEANING, None), scale)
                    .expect("the fixture reference has a background")
            })
            .collect()
    }
  ```
  These tests' numeric bars stay as they are. If one fails, stop and report: Task 2 changes `B` only, and these tests pin `A` or finiteness.

- [ ] **Step 9: The golden literals.** Run `stage_hashes_are_pinned_to_golden_literals`. Only `ln_ref`, `ln` and `ln_cal` may differ, for every preset row. Replace those literals with the new values and add above the golden block: `// 2026-10-05 LN model fixes Task 2: lnModelVersion 2 joins the normalization subtree (spec §7, L-19); calib/meas/reg unchanged.` If `calib`, `meas` or `reg` moved, STOP.

- [ ] **Step 10: `ln_probe`.** In `examples/ln_probe.rs`:
  - imports: drop `background_grid, BackgroundParams, DEFAULT_PARAMS, TARGET_DEVIATION_SIGMA`; add `BackgroundModel, TileTable, CLEANING`.
  - 490–496: `LocalNormalizationConfig { enabled: true, scale: args.scale, reference_frames: args.frames, ..LocalNormalizationConfig::default() }`.
  - 576–597: the reference models, built as in the run (Step 7.2, `.expect` with a message is acceptable in an example).
  - 846–857: the target model on `target_plane` (`TileTable::measure` + `BackgroundModel::fit(.., args.scale)`), and `let pair = shared_refit(&ref_models[p], &target_model);` with `ref_nodes = pair.reference.nodes(stride)` and `tgt_nodes = pair.target.nodes(stride)` replacing `ref_bg.cells`/`target_bg.cells` in the residual loop (884–896). The loop iterates `LnGrid::grid_dims(w, h, stride)` (the example can call `athenaeum_core::stacking::ln::LnGrid`; if `grid_dims` is `pub(crate)`, use `grid.gw`/`grid.gh` from the sidecar it reads, which are the same mesh).
  - JSON: `"cellsRejected"` (911, 945) becomes `"samples"`, `"samplesRejected"`, `"samplesShared"`, `"cellsUnsupported"`.
  - Pass `args.scale` as `scale_px` to `normalize_frame` and `measure_seeds_calibration`.
  - Fix the stale doc comment's usage line (20–22) to the real usage string (57–60).

- [ ] **Step 11: Run the targeted tests, then the gate.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib stacking::ln -- --test-threads=4 2>&1 | grep -E 'test result|FAILED'` then `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib local_normalization -- --test-threads=4 2>&1 | grep -E 'test result|FAILED'`, then the full gate including `cargo check -p athenaeum-core --all-targets` (the examples). Expected: all pass; `git diff --exit-code src/types` (no exported type changed).

Also confirm the LN-off invariant: `local_normalization_off_leaves_no_ln_files`, `local_rejection_normalization_group_fallback_matches_global_scale_zero_offset` and `local_normalization_rejection_only_frame_failure_keeps_global_normalization` pass unchanged.

- [ ] **Step 12: Commit**

```bash
git add crates/athenaeum-core/src/stacking/ln/background.rs crates/athenaeum-core/src/stacking/ln/mod.rs \
  crates/athenaeum-core/src/stacking/ln/calibration.rs crates/athenaeum-core/src/stacking/run.rs \
  crates/athenaeum-core/src/stacking/config.rs crates/athenaeum-core/src/stacking/plan.rs \
  crates/athenaeum-core/examples/ln_probe.rs
git commit -m "feat(ln): LN normalizes against the background model at the scale; the cell model is retired (LN_MODEL_VERSION 2)"
```

---

### Task 3: Scale rule — config, `resolve_scale`, hash token, run and plan twin

Spec §2 (all), §6 (`scaleMode`, `gridSize`, `scale`), §7 (token, subtree canonicalization, `LnReferencePayload`), §8; rulings L-7…L-10, L-19, P-4, P-5, P-14.

**Files:**
- Create: `crates/athenaeum-core/src/stacking/ln/scale_rule.rs`
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs` (`pub mod scale_rule;` + re-exports)
- Modify: `crates/athenaeum-core/src/stacking/integrate.rs:145-166` (`LnScaleMode`, two fields, `Default`; test literal ~1834)
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (`resolve_ln_fields`, called from `resolve_config`; subtree canonicalization; tests; golden literals)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (hash functions; `LnReferencePayload`; `PlanGroup`; the plan's scale twin; config override resolution; tests; golden literals)
- Modify: `crates/athenaeum-core/src/api/stacking.rs:445-470` (`decode_presets_entrywise` resolves each preset)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (`RunContext.planned_ln_scales`; `run_group_normalization`; `build_and_write_ln_reference`; `LnGroupOutcome`; `push_summary_group`; the test-context helper ~12206)
- Modify: `crates/athenaeum-core/src/stacking/provenance.rs:142-219` (`SummaryGroup.ln_scale_px`)
- Modify: `crates/athenaeum-core/src/ts_export.rs:~395` (register `LnScaleMode` before `LocalNormalizationConfig`)
- Regenerate: `src/types/stacking.ts` (`LnScaleMode`; `LocalNormalizationConfig.scaleMode/gridSize`; `PlanGroup.lnScalePx/lnScaleEstimated/lnWidth/lnHeight`; `SummaryGroup.lnScalePx`). Fix any TS object literal `tsc` then flags (none known).

**Interfaces:**
- Consumes (Task 2): `normalize_frame(.., cfg, scale_px, ..)`, `measure_seeds_calibration(.., cfg, scale_px, ..)`, `LN_MODEL_VERSION`.
- Produces:
  - `stacking::integrate::LnScaleMode { Auto, Fixed }` (serde `"auto"`/`"fixed"`, `Copy`); `LocalNormalizationConfig { enabled, scale, scale_mode: Option<LnScaleMode>, grid_size: u32, reference_frames, psf_model, local_scale }` (still `Copy`);
  - `stacking::ln::{LN_SCALES: [u32; 11], snap_ln_scale(f64) -> u32, effective_scale_mode(&LocalNormalizationConfig) -> LnScaleMode, resolve_scale(&LocalNormalizationConfig, width: usize, height: usize) -> u32, MIN_GRID_SIZE: u32 = 2, MAX_GRID_SIZE: u32 = 8, LEGACY_DEFAULT_LN_SCALE: u32 = 1024}`;
  - `stacking::config::resolve_ln_fields(&mut StackingConfig)` (`pub(crate)`);
  - `normalization_hash_for(cfg, registration_hash, measurement_hash, reference_member_ids, reference_hash, calibration, beta, ln_scale_px: Option<u32>)` and `seeds_calibration_hash_for(cfg, reference_hash, calibration_frames, beta, ln_scale_px: u32)`;
  - `LnReferencePayload { reference_member_ids, reference_hash, width: usize, height: usize, scale_px: u32 }` (the three new fields `#[serde(default)]`);
  - `PlanGroup { …, ln_scale_px: Option<u32>, ln_scale_estimated: bool, ln_width: Option<u32>, ln_height: Option<u32> }`;
  - `SummaryGroup { …, ln_scale_px: Option<u32> }` (`#[serde(default)]`);
  - `RunContext { …, planned_ln_scales: HashMap<String, u32> }`.

- [ ] **Step 1: Write the failing tests.**

`stacking/ln/scale_rule.rs` (`mod tests` at its foot):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::integrate::{LnScaleMode, LocalNormalizationConfig};

    #[test]
    fn snap_follows_the_nearest_rule_with_ties_to_the_smaller() {
        for (v, want) in [
            (1597.0, 1536), (1044.0, 1024), (1120.0, 1024), (880.0, 768), (752.0, 768),
            (1280.0, 1024), (640.0, 512), (100.0, 256), (256.0, 256), (9000.0, 8192),
            (8192.0, 8192), (1300.0, 1536),
        ] {
            assert_eq!(snap_ln_scale(v), want, "{v}");
        }
    }

    /// Spec §2.1's table.
    #[test]
    fn resolve_scale_matches_the_spec_table() {
        let auto = LocalNormalizationConfig { scale_mode: Some(LnScaleMode::Auto), grid_size: 4, ..Default::default() };
        for ((w, h), want) in [
            ((9576, 6388), 1536), ((6248, 4176), 1024), ((4480, 5760), 1024),
            ((4656, 3520), 768), ((3008, 3008), 768),
        ] {
            assert_eq!(resolve_scale(&auto, w, h), want, "{w} × {h}");
        }
    }

    #[test]
    fn fixed_mode_snaps_and_grid_size_is_clamped() {
        let fixed = LocalNormalizationConfig { scale_mode: Some(LnScaleMode::Fixed), scale: 2000, ..Default::default() };
        assert_eq!(resolve_scale(&fixed, 9576, 6388), 2048);
        let wide = LocalNormalizationConfig { scale_mode: Some(LnScaleMode::Auto), grid_size: 50, ..Default::default() };
        assert_eq!(resolve_scale(&wide, 9576, 6388), snap_ln_scale(6388.0 / 8.0));
    }

    /// L-9 for a config that never went through `resolve_config` (a plan
    /// override, a test literal): `None` reads 1024 as auto, anything else
    /// as fixed.
    #[test]
    fn an_unresolved_mode_follows_the_legacy_rule() {
        let legacy_default = LocalNormalizationConfig { scale_mode: None, scale: 1024, ..Default::default() };
        assert_eq!(effective_scale_mode(&legacy_default), LnScaleMode::Auto);
        let legacy_custom = LocalNormalizationConfig { scale_mode: None, scale: 2048, ..Default::default() };
        assert_eq!(effective_scale_mode(&legacy_custom), LnScaleMode::Fixed);
        assert_eq!(resolve_scale(&legacy_custom, 9576, 6388), 2048);
    }
}
```

`config.rs` `mod tests`:
```rust
    #[test]
    fn a_legacy_document_reads_scale_1024_as_auto_and_any_other_as_fixed() {
        use crate::stacking::integrate::LnScaleMode;
        let read = |local: &str| {
            resolve_config(Some(&format!(r#"{{"normalization":{{"local":{local}}}}}"#)), None)
                .unwrap()
                .normalization
                .local
        };
        let a = read(r#"{"enabled":true,"scale":1024}"#);
        assert_eq!((a.scale_mode, a.scale), (Some(LnScaleMode::Auto), 1024));
        let b = read(r#"{"enabled":true,"scale":2048}"#);
        assert_eq!((b.scale_mode, b.scale), (Some(LnScaleMode::Fixed), 2048));
        let c = read(r#"{"enabled":true,"scale":1300}"#);
        assert_eq!((c.scale_mode, c.scale), (Some(LnScaleMode::Fixed), 1536), "fixed is snapped");
        let d = read(r#"{"enabled":true,"scale":2048,"scaleMode":"auto"}"#);
        assert_eq!(d.scale_mode, Some(LnScaleMode::Auto), "a stored mode is never overridden");
        assert_eq!(read(r#"{"gridSize":1}"#).grid_size, 2);
        assert_eq!(read(r#"{"gridSize":9}"#).grid_size, 8);
    }

    #[test]
    fn a_stored_document_without_scale_mode_decodes_to_none_before_resolution() {
        let c: StackingConfig =
            serde_json::from_str(r#"{"normalization":{"local":{"scale":2048}}}"#).unwrap();
        assert_eq!(c.normalization.local.scale_mode, None);
        assert_eq!(StackingConfig::default().normalization.local.scale_mode, Some(crate::stacking::integrate::LnScaleMode::Auto));
    }

    /// L-19: scale, scaleMode and gridSize leave the subtree; the resolved
    /// scale reaches the hashes as a token instead.
    #[test]
    fn auto_and_fixed_at_the_same_resolved_scale_share_the_normalization_subtree() {
        use crate::stacking::integrate::LnScaleMode;
        let mut auto = StackingConfig::default();
        auto.normalization.local.enabled = true;
        let mut fixed = auto.clone();
        fixed.normalization.local.scale_mode = Some(LnScaleMode::Fixed);
        fixed.normalization.local.scale = 1536;
        fixed.normalization.local.grid_size = 6;
        assert_eq!(normalization_subtree(&auto), normalization_subtree(&fixed));
        let local = &normalization_subtree(&auto)["normalization"]["local"];
        for key in ["scale", "scaleMode", "gridSize"] {
            assert!(local.get(key).is_none(), "{key} is still in the subtree: {local}");
        }
    }
```

`plan.rs` `mod tests` (beside `the_ln_hashes_follow_the_group_beta`, ~6060):
```rust
    #[test]
    fn the_ln_reference_hash_ignores_the_scale_and_the_frame_hashes_follow_it() {
        let cfg = StackingConfig::default();
        let reference = |s: Option<u32>| normalization_hash_for(&cfg, "r", "m", &[1, 2], "", None, Some(4.0), s);
        // The run passes no token for the reference (L-19); a token would move it.
        assert_ne!(reference(None), reference(Some(1024)));
        let frame = |s: u32| normalization_hash_for(&cfg, "r", "m", &[1, 2], "ref", None, Some(4.0), Some(s));
        assert_ne!(frame(1024), frame(1536));
        let cal = |s: u32| seeds_calibration_hash_for(&cfg, "ref", &[(1, "r", "m")], Some(4.0), s);
        assert_ne!(cal(1024), cal(1536));
    }
```
Plus, in `run.rs`'s test module (copy the setup of `local_normalization_sidecars_are_cached_on_the_second_run`, ~17732):
```rust
    /// Spec §2.3 + Review Focus 3: the plan reports the scale it will use,
    /// marked estimated before any run; the run resolves the same value; the
    /// next plan reads it exactly from the reference payload and Normalize
    /// is cached.
    #[test]
    fn the_plan_reads_normalize_cached_after_a_run_with_a_derived_scale() {
        // seed_ln_star_group(...) as in the cached-second-run test; LN on, scale mode auto.
        // 1. plan_before = build_plan(..): every group has ln_scale_px == Some(resolve_scale(&cfg.normalization.local, w, h))
        //    for the fixture's frame geometry, and ln_scale_estimated == true.
        // 2. Run (the same helper that test uses).
        // 3. The run summary's group carries ln_scale_px == plan_before's value; the ln_reference
        //    row's payload has width/height == the fixture geometry and scale_px == that value.
        // 4. plan_after: ln_scale_estimated == false, ln_scale_px unchanged,
        //    ln_width/ln_height == the fixture geometry, and !stale_stages.contains(&Stage::Normalize).
    }

    /// Review Focus 3: a run whose scale differs from the plan's estimate
    /// warns once.
    #[test]
    fn a_run_warns_once_when_its_ln_scale_differs_from_the_plan() {
        // Same fixture. Build the run context the LN run tests build, then
        // `rc.planned_ln_scales.insert(<group key>.into(), 8192)` (a value the
        // fixture geometry cannot resolve to), run, and assert exactly one
        // entry of `rc.warnings` (or the summary's `warnings`) contains
        // "the plan estimated 8192 px".
    }
```
Write the two bodies out fully, adapting the named test's setup verbatim. Do not invent a new fixture. (From Task 11 on, these two tests fall under P-13's `run.rs` clause, review C-3.)

And in `api/stacking.rs`'s test module:
```rust
    /// Review Focus 2: every reader of a stored config resolves L-9.
    #[test]
    fn every_stored_config_reader_resolves_the_legacy_scale() {
        // A legacy document: {"normalization":{"local":{"enabled":true,"scale":2048}}}.
        // 1. stored as frame set 1's config (`set_set_config` with the raw JSON) → get_stacking_config
        //    returns scale_mode Some(Fixed), scale 2048;
        // 2. stored as the global default row (`set_stacking_defaults` would re-serialize, so write
        //    the settings key directly the way `read_global_config_json` reads it) → get_stacking_defaults
        //    returns Some(Fixed);
        // 3. stored as one entry of the `stacking.presets` row → list_stacking_presets returns it with Some(Fixed);
        // 4. build_plan with `config_override` = that document decoded WITHOUT resolution
        //    (serde_json::from_str) → plan.config.normalization.local.scale_mode == Some(Fixed).
    }
```

- [ ] **Step 2: Run them to verify they fail to compile**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib scale_rule legacy_document auto_and_fixed -- --test-threads=4 2>&1 | tail -5`
Expected: errors naming `LnScaleMode`, `scale_mode`, `resolve_scale`.

- [ ] **Step 3: The config type** (`integrate.rs`, rustfmt-clean):
```rust
/// How the LN scale is chosen (spec 2026-10-04 §2): derived from the LN
/// geometry (`auto`, the default) or the stored `scale` (`fixed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LnScaleMode {
    Auto,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalNormalizationConfig {
    pub enabled: bool,
    // The LN scale in pixels when `scaleMode` is `fixed`; snapped to the
    // supported list by `resolve_config`. Read only by `ln::resolve_scale`.
    pub scale: u32,
    // `None` only on a document stored before 2026-10 (field-level default,
    // so the struct's own default cannot fill it): `resolve_config` turns it
    // into `Some` by the legacy rule L-9.
    #[serde(default)]
    pub scale_mode: Option<LnScaleMode>,
    // Auto scale = the short side of the LN geometry / gridSize, snapped (2–8).
    pub grid_size: u32,
    pub reference_frames: u32,
    pub psf_model: crate::stacking::psf_signal::PsfModel,
    pub local_scale: bool,
}
```
`Default`: `scale: 1024, scale_mode: Some(LnScaleMode::Auto), grid_size: 4`, the rest unchanged. Fix the literal in `config_serde_names_match_the_spec` (~1834) by adding the two fields, and add `"scaleMode":"auto","gridSize":4` to its expected JSON in serde order (`scale` then `scaleMode` then `gridSize`).

- [ ] **Step 4: The scale rule** (`stacking/ln/scale_rule.rs`, rustfmt-clean):
```rust
//! The LN scale (spec 2026-10-04 §2, rulings L-7…L-10): one function for
//! the plan and the run.

use crate::stacking::integrate::{LnScaleMode, LocalNormalizationConfig};

/// The supported LN scales (L-7): the reference pipeline's list from 256 up.
/// Below 256 a knot interval would hold fewer than 8 tiles.
pub const LN_SCALES: [u32; 11] = [256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192];
pub const MIN_GRID_SIZE: u32 = 2;
pub const MAX_GRID_SIZE: u32 = 8;
/// The pre-2026-10 default `scale`; a legacy document carrying it reads as
/// auto (L-9).
pub const LEGACY_DEFAULT_LN_SCALE: u32 = 1024;

/// The nearest list value, ties to the smaller (L-8); below the list 256,
/// above it 8192.
pub fn snap_ln_scale(v: f64) -> u32 {
    let mut best = LN_SCALES[0];
    for &s in &LN_SCALES[1..] {
        if (s as f64 - v).abs() < (best as f64 - v).abs() {
            best = s;
        }
    }
    best
}

/// The mode a config means: its own, or L-9's reading of a document that
/// never went through `resolve_config` (a plan override, a decoded preset).
pub fn effective_scale_mode(local: &LocalNormalizationConfig) -> LnScaleMode {
    local.scale_mode.unwrap_or(if local.scale == LEGACY_DEFAULT_LN_SCALE {
        LnScaleMode::Auto
    } else {
        LnScaleMode::Fixed
    })
}

/// The LN scale for a `width × height` LN geometry (L-10). Every consumer
/// reads the value this returns; nothing else reads `local.scale`.
pub fn resolve_scale(local: &LocalNormalizationConfig, width: usize, height: usize) -> u32 {
    match effective_scale_mode(local) {
        LnScaleMode::Fixed => snap_ln_scale(local.scale as f64),
        LnScaleMode::Auto => {
            let grid = local.grid_size.clamp(MIN_GRID_SIZE, MAX_GRID_SIZE) as f64;
            snap_ln_scale(width.min(height) as f64 / grid)
        }
    }
}

impl LnScaleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LnScaleMode::Auto => "auto",
            LnScaleMode::Fixed => "fixed",
        }
    }
}
```
(`impl LnScaleMode` may live here or beside the enum in `integrate.rs`; keep it beside the enum if the orphan rule complains — it will not, same crate.) In `ln/mod.rs`: `pub mod scale_rule;` and `pub use scale_rule::{effective_scale_mode, resolve_scale, snap_ln_scale, LEGACY_DEFAULT_LN_SCALE, LN_SCALES, MAX_GRID_SIZE, MIN_GRID_SIZE};`.

- [ ] **Step 5: `resolve_config` and the subtree** (`config.rs`):
```rust
/// L-9 and the §6 clamps for the LN fields, on any config that came from
/// storage or from a caller (resolve_config, a plan override, a decoded user
/// preset). Idempotent.
pub(crate) fn resolve_ln_fields(config: &mut StackingConfig) {
    use crate::stacking::integrate::LnScaleMode;
    use crate::stacking::ln::{effective_scale_mode, snap_ln_scale, MAX_GRID_SIZE, MIN_GRID_SIZE};
    let local = &mut config.normalization.local;
    if local.scale_mode.is_none() {
        let mode = effective_scale_mode(local);
        tracing::debug!(scale = local.scale, ln_scale_mode = mode.as_str(), "stacking config: legacy LN scale resolved");
        local.scale_mode = Some(mode);
    }
    if local.scale_mode == Some(LnScaleMode::Fixed) {
        let snapped = snap_ln_scale(local.scale as f64);
        if snapped != local.scale {
            warn!(value = local.scale, clamped = snapped, "stacking config: LN scale is not a supported size; snapped");
            local.scale = snapped;
        }
    }
    if !(MIN_GRID_SIZE..=MAX_GRID_SIZE).contains(&local.grid_size) {
        let clamped = local.grid_size.clamp(MIN_GRID_SIZE, MAX_GRID_SIZE);
        warn!(value = local.grid_size, clamped, "stacking config: LN gridSize out of range; clamped");
        local.grid_size = clamped;
    }
}
```
Call it in `resolve_config` right before `Ok(config)`. Canonicalize the subtree:
```rust
/// The normalization block as the LN hashes see it (L-19): `scale`,
/// `scaleMode` and `gridSize` leave it — their effect reaches the hashes as
/// the resolved-scale token — so auto ↔ fixed at the same resolved value
/// invalidates nothing.
fn canonical_normalization(cfg: &StackingConfig) -> serde_json::Value {
    let mut v = serde_json::to_value(&cfg.normalization).expect("NormalizationConfig serializes");
    if let Some(local) = v.get_mut("local").and_then(|l| l.as_object_mut()) {
        for key in ["scale", "scaleMode", "gridSize"] {
            local.remove(key);
        }
    }
    v
}
```
and use `"normalization": canonical_normalization(cfg),` in `normalization_subtree_with_fit_version`.

- [ ] **Step 6: Every other reader of a caller-supplied config.**
  - `plan.rs` `resolve_config_and_exclusions` (~1113) and the project twin (~1181): `Some(mut cfg) => { crate::stacking::config::resolve_ln_fields(&mut cfg); cfg }`.
  - `api/stacking.rs` `decode_presets_entrywise`: after `Ok(p) =>`, call `crate::stacking::config::resolve_ln_fields(&mut p.config)` before pushing (bind `Ok(mut p)`). Check the `NamedPreset` field name (`config`) when editing.

- [ ] **Step 7: The hash functions** (`plan.rs`, hand-format):
  - `normalization_hash_for` gains a last parameter `ln_scale_px: Option<u32>` and, after the β push, `if let Some(s) = ln_scale_px { upstream.push(format!("ln_scale_px:{s}")); }`. Doc: the reference hash passes `None` (L-19); every per-frame hash passes the resolved scale.
  - `seeds_calibration_hash_for` gains `ln_scale_px: u32` and pushes the same token unconditionally.
  - Update every call (production: `run.rs` ~10369 with `None`, ~10680 and ~10703 with the scale, `plan.rs` ~3100 with the plan's value; tests at ~5989, 6000, 6071, 6082, 6327, 6353, 6638, 6665, 7559, 7560, 7569, 8981, 9003 — pass `Some(1024)` / `1024` where the test builds per-frame or calibration hashes, `None` where it builds a reference hash, and keep each test's own comparison meaning).

- [ ] **Step 8: `LnReferencePayload`** (`plan.rs:624-628`):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LnReferencePayload {
    pub reference_member_ids: Vec<i64>,
    pub reference_hash: String,
    // The LN geometry the reference (and every sidecar against it) lives in,
    // and the scale of the run that built it. 0 on a payload written before
    // 2026-10 (the plan then estimates, spec §2.3 case 3).
    #[serde(default)]
    pub width: usize,
    #[serde(default)]
    pub height: usize,
    #[serde(default)]
    pub scale_px: u32,
}
```
Fill the three test literals (`plan.rs` ~6328, ~6639, ~8982) with the fixture's geometry (or `..` the values the run would write; the tests compare hashes only, so `width: 0, height: 0, scale_px: 0` keeps their meaning — use that).

- [ ] **Step 9: The run** (`run.rs`, hand-format).
  1. `RunContext` gains `pub(crate) planned_ln_scales: HashMap<String, u32>,` (doc: "Per group, the LN scale the start plan reported (spec §2.3, P-14); a run whose own resolution differs warns once."). In `start_stacking_with_solver`'s literal (~1091) fill it from the plan before `plan` is moved: `planned_ln_scales: plan.groups.iter().filter_map(|g| g.ln_scale_px.map(|s| (g.key.clone(), s))).collect(),` — compute it into a local right after `build_plan` (~897), since later lines move fields out of `plan`. The test helper's literal (~12255) gets `planned_ln_scales: HashMap::new(),`.
  2. In `run_group_normalization`, right after `(ref_width, ref_height)` (~10251):
     ```rust
    // Spec §2.3 (L-10): the one scale rule, on this group's LN geometry.
    let scale_px = ln::resolve_scale(&ln_cfg, ref_width, ref_height);
    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_scale_px = scale_px,
        ln_scale_mode = ln::effective_scale_mode(&ln_cfg).as_str(),
        width = ref_width,
        height = ref_height,
        "ln scale resolved"
    );
    if let Some(&planned) = rc.planned_ln_scales.get(&group.key) {
        if planned != scale_px {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                ln_scale_px = scale_px,
                expected = planned,
                "ln scale differs from the plan's estimate"
            );
            rc.warnings.push(format!(
                "group {}: local normalization scale resolved to {scale_px} px; the plan estimated {planned} px",
                group.key
            ));
        }
    }
     ```
     Delete Task 2's `let scale_px = ln_cfg.scale;` line. Every later `scale_px` use stays.
  3. The three hash calls take the token (Step 7).
  4. `build_and_write_ln_reference` gains `scale_px: u32` (last parameter) and writes `LnReferencePayload { reference_member_ids, reference_hash, width: built.width, height: built.height, scale_px }`. Its `"ln reference built"` `info!` (~11481) gains `ln_scale_px = scale_px,` (spec §10; review M-1).
  5. `LnGroupOutcome` gains `scale_px: Option<u32>` (`Some(scale_px)` on every return after it is resolved, `None` on the early returns before it); the caller at ~8742 passes it to `push_summary_group`, which gains `ln_scale_px: Option<u32>` and sets the new `SummaryGroup.ln_scale_px`. Every other `push_summary_group` call passes `None`.
- [ ] **Step 10: `SummaryGroup`** (`provenance.rs`): add `#[serde(default)] pub ln_scale_px: Option<u32>,` with a `//` comment ("the LN scale this group ran at, spec §8"). Fix any `SummaryGroup { .. }` literal the compiler flags with `ln_scale_px: None`.

- [ ] **Step 11: The plan twin** (`plan.rs`, hand-format).
  1. `PlanGroup` gains, after `ln_cached`:
     ```rust
    // The LN scale this group will run at (spec §2.3), `None` while LN is
    // inactive; `ln_scale_estimated` when the geometry is not yet the one
    // the sidecars were written in (no stored LN reference). `ln_width` /
    // `ln_height` are that geometry (P-5).
    pub ln_scale_px: Option<u32>,
    pub ln_scale_estimated: bool,
    pub ln_width: Option<u32>,
    pub ln_height: Option<u32>,
     ```
  2. A helper above `build_plan`:
     ```rust
/// The LN geometry the plan expects for `g` and whether it is an estimate
/// (spec §2.3, L-10): a project's grid (exact once confirmed); the stored
/// LN reference's own dims (exact); otherwise the run's reference frame in
/// co-registered mode (a manual pin, else the last run's reference), the
/// group's per-group reference from the last run in native mode, else the
/// group's anchor, else the largest group's anchor — all estimates.
#[allow(clippy::too_many_arguments)]
fn plan_ln_geometry(
    g: &IntegrationGroup,
    groups: &[IntegrationGroup],
    excluded: &HashSet<i64>,
    ln_grid_dims: Option<(u64, u64, bool)>,
    stored_reference: Option<&LnReferencePayload>,
    native: bool,
    run_reference: Option<i64>,
    native_references: &HashMap<String, i64>,
) -> Option<(usize, usize, bool)> {
    if let Some((w, h, confirmed)) = ln_grid_dims {
        return Some((w as usize, h as usize, !confirmed));
    }
    if let Some(p) = stored_reference.filter(|p| p.width > 0 && p.height > 0) {
        return Some((p.width, p.height, false));
    }
    let dims_of = |id: i64| {
        find_group_frame(groups, id).and_then(|f| {
            (f.width > 0 && f.height > 0).then_some((f.width as usize, f.height as usize, true))
        })
    };
    let pick = if native { native_references.get(&g.key).copied() } else { run_reference };
    if let Some(found) = pick.and_then(dims_of) {
        return Some(found);
    }
    let anchor = |grp: &IntegrationGroup| match group_anchor_geometry(grp) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w as usize, h as usize, true)),
        _ => None,
    };
    if native {
        if let Some(found) = anchor(g) {
            return Some(found);
        }
    }
    largest_group(groups, excluded).and_then(anchor)
}
     ```
     Extract `largest_group(groups, excluded) -> Option<&IntegrationGroup>` from `largest_group_scale` (same selection loop) and make `largest_group_scale` call it. Check the exact type of `GroupFrame.width/height` (`i64`) and adapt the casts.
  3. In `build_plan`, before the per-group loop, when `local_normalization_active`:
     - `run_reference = reference.frame_id.or(last_run_reference_id)` (read the last run once with `list_runs(conn, subject, 1)?`; reuse a value the function already holds if there is one);
     - `native_references = summary_group_references(last_run.summary_json.as_deref())` when `cfg.registration.geometry == RegistrationGeometry::Native`, else empty;
     - `ln_grid_dims = project.as_ref().and_then(|p| p.grid_dims.map(|(w, h)| (w, h, p.grid.current.is_some())))` — a new name: `build_plan` already binds `project_grid: Option<CanonicalGrid>` at ~2481 and reads it at ~2914/~3090/~3234 (review I-1). `ProjectInputs.grid_dims: Option<(u64, u64)>` (~1954) and `PlanGrid.current` (~242) are as written.
     Inside the per-group loop, after `ln_reference_info` is read (~2905–2911), compute
     ```rust
            let ln_geometry = plan_ln_geometry(g, &groups, &excluded_set, ln_grid_dims, ln_reference_info.as_ref(), native, run_reference, &native_references);
            let ln_scale_px = ln_geometry.map(|(w, h, _)| crate::stacking::ln::resolve_scale(&cfg.normalization.local, w, h));
     ```
     (adapt `ln_reference_info.as_ref()` to however that payload is held), and pass `ln_scale_px` into the per-frame `normalization_hash_for` call (~3100). Store `ln_scale_px`, `ln_geometry.map_or(false, |g| g.2)`, `ln_geometry.map(|g| g.0 as u32)`, `ln_geometry.map(|g| g.1 as u32)` into the `PlanGroup` literal (~3161); `None`/`false` while LN is inactive.
     If LN is active but `ln_geometry` is `None` (no dims anywhere), the per-frame hash gets `None`, which never matches a stored row, so Normalize reads stale. That is the honest answer.
  4. The golden literals: `stage_hashes_are_pinned_to_golden_literals` builds its hashes through these functions. Pass `Some(1024)` for its per-frame/calibration calls (the default config's resolved scale on any geometry with a short side of 3584–5119 px) and `None` for the reference call. Then update `ln_ref`, `ln` and `ln_cal` to the new values with the dated comment `// 2026-10-05 LN model fixes Task 3: scale/scaleMode/gridSize leave the subtree, ln_scale_px token (L-19).`. `config_hash_is_canonical_and_pinned` and `serde_names_follow_the_spec` (the `"local":{…}` needle) take the new shape; update both with the same dated comment. `calib`, `meas`, `reg` must not move.

- [ ] **Step 12: TS export.** In `ts_export.rs`, add `crate::stacking::integrate::LnScaleMode,` immediately before `crate::stacking::integrate::LocalNormalizationConfig,`. Regenerate: `TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract`, then `npx tsc --noEmit -p .` — fix any object literal it flags (`scaleMode: 'auto', gridSize: 4` in a `LocalNormalizationConfig`, the four `PlanGroup` fields, `lnScalePx: null` in a `SummaryGroup`).

- [ ] **Step 13: Run the new tests, then the gate.** All of Step 1's tests pass; `local_normalization_reacts_to_a_config_change` (`plan.rs` ~6193) still expects Normalize stale after `cfg2.normalization.local.scale += 256`. Under auto mode that edit no longer changes the resolved scale. Change the edit to `cfg2.normalization.local.scale_mode = Some(LnScaleMode::Fixed); cfg2.normalization.local.scale = 2048;` (still a real scale change) and name this in the report. `src/types/stacking.ts` changes exactly as listed under Files.

- [ ] **Step 14: Commit**

```bash
git add crates/athenaeum-core/src/stacking/ln/scale_rule.rs crates/athenaeum-core/src/stacking/ln/mod.rs \
  crates/athenaeum-core/src/stacking/integrate.rs crates/athenaeum-core/src/stacking/config.rs \
  crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/api/stacking.rs \
  crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/provenance.rs \
  crates/athenaeum-core/src/ts_export.rs src/types/stacking.ts
git commit -m "feat(ln): the LN scale is derived from the image (short side / gridSize, snapped); plan and run share one rule"
```

---

### Task 4: Normalize panel — scale controls and the resolved line

Spec §9 (the Scale, Grid size and Resolved scale rows; board summary; registry keywords); P-5. Agent: frontend.

**Files:**
- Create: `src/components/stacking/panels/lnScale.ts` (pure helpers) and `src/components/stacking/panels/lnScale.test.ts`
- Modify: `src/components/stacking/panels/NormalizePanel.tsx` (LN block; `plan` prop)
- Modify: `src/components/stacking/StageInspector.tsx:95-97` (pass `plan`)
- Modify: `src/components/stacking/stageSummary.ts:298-307` (normalize summary) and create `src/components/stacking/stageSummary.test.ts` if none exists (append if it does)
- Modify: `src/settings/registry.ts:686-703` (`normalize` keywords)

**Interfaces:**
- Consumes (Task 3 TS types): `LocalNormalizationConfig.scaleMode: LnScaleMode | null`, `.gridSize`, `PlanGroup.lnScalePx/lnScaleEstimated/lnWidth/lnHeight`.
- Produces: `LN_SCALES`, `effectiveScaleMode(local)`, `resolvedScaleLines(plan)` in `lnScale.ts`; `NormalizePanelProps.plan?: StackingPlan | null`.

- [ ] **Step 1: Write the failing tests** (`lnScale.test.ts`):
```ts
import { describe, expect, it } from 'vitest';
import { effectiveScaleMode, resolvedScaleLines } from './lnScale';
import type { PlanGroup, StackingPlan } from '../../../types/stacking';

const group = (over: Partial<PlanGroup>): PlanGroup => ({
  key: 'mono__L__bin1__180s', lnScalePx: 1536, lnScaleEstimated: false, lnWidth: 9576, lnHeight: 6388,
  ...over,
} as PlanGroup);
const planOf = (groups: PlanGroup[]) => ({ groups } as unknown as StackingPlan);

describe('effectiveScaleMode', () => {
  it('reads a stored mode as is', () => {
    expect(effectiveScaleMode({ scaleMode: 'fixed', scale: 1024 } as never)).toBe('fixed');
  });
  it('reads a legacy document by the L-9 rule', () => {
    expect(effectiveScaleMode({ scaleMode: null, scale: 1024 } as never)).toBe('auto');
    expect(effectiveScaleMode({ scaleMode: null, scale: 2048 } as never)).toBe('fixed');
  });
});

describe('resolvedScaleLines', () => {
  it('points Settings at the Stacking tab', () => {
    expect(resolvedScaleLines(null)).toEqual(["A frame set's Stacking tab shows the resolved value."]);
  });
  it('shows one line when every group agrees', () => {
    expect(resolvedScaleLines(planOf([group({}), group({ key: 'b' })]))).toEqual(['1536 px (9576 × 6388)']);
  });
  it('marks an estimate', () => {
    expect(resolvedScaleLines(planOf([group({ lnScaleEstimated: true })]))).toEqual([
      '1536 px (9576 × 6388) — estimated, the run confirms it',
    ]);
  });
  it('lists groups when they differ', () => {
    expect(
      resolvedScaleLines(planOf([group({}), group({ key: 'b', lnScalePx: 1024, lnWidth: 6248, lnHeight: 4176 })])),
    ).toEqual(['mono__L__bin1__180s: 1536 px (9576 × 6388)', 'b: 1024 px (6248 × 4176)']);
  });
});
```
and in `stageSummary.test.ts`:
```ts
import { describe, expect, it } from 'vitest';
import { stageSummary } from './stageSummary';
import type { StackingConfig } from '../../types/stacking';

const withLocal = (local: Partial<StackingConfig['normalization']['local']>) =>
  ({ normalization: { output: 'additiveWithScaling', rejection: 'scaleZeroOffset', scaleEstimator: 'mad',
    local: { enabled: true, scale: 1024, scaleMode: 'auto', gridSize: 4, referenceFrames: 20, psfModel: 'auto', localScale: false, ...local } } } as unknown as StackingConfig);

describe('stageSummary normalize', () => {
  it('auto', () => expect(stageSummary('normalize', withLocal({}))).toBe('Local · scale auto (÷4) · ref 20'));
  it('fixed', () => expect(stageSummary('normalize', withLocal({ scaleMode: 'fixed', scale: 2048 }))).toBe('Local · scale 2048 · ref 20'));
  it('local scale', () => expect(stageSummary('normalize', withLocal({ localScale: true }))).toBe('Local · scale auto (÷4) · ref 20 · local scale'));
});
```
(If `stageSummary`'s first argument type rejects `'normalize'`, cast through the existing `BoardStage` type exactly as the file's own callers do.)

- [ ] **Step 2: Run them to verify they fail.** `npx vitest run src/components/stacking` → FAIL (module `./lnScale` missing; summary strings differ).

- [ ] **Step 3: Implement.**

`lnScale.ts`:
```ts
import type { LocalNormalizationConfig, LnScaleMode, StackingPlan } from '../../../types/stacking';

// Spec 2026-10-04 §2.1 (L-7): the supported LN scales.
export const LN_SCALES = [256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192] as const;

// L-9, mirrored for a config the backend has not resolved yet.
export function effectiveScaleMode(local: Pick<LocalNormalizationConfig, 'scaleMode' | 'scale'>): LnScaleMode {
  return local.scaleMode ?? (local.scale === 1024 ? 'auto' : 'fixed');
}

export function resolvedScaleLines(plan: StackingPlan | null | undefined): string[] {
  if (!plan) return ["A frame set's Stacking tab shows the resolved value."];
  const groups = plan.groups.filter((g) => g.lnScalePx != null);
  if (groups.length === 0) return [];
  const text = (g: (typeof groups)[number]) =>
    `${g.lnScalePx} px${g.lnWidth != null && g.lnHeight != null ? ` (${g.lnWidth} × ${g.lnHeight})` : ''}` +
    (g.lnScaleEstimated ? ' — estimated, the run confirms it' : '');
  const distinct = new Set(groups.map(text));
  return distinct.size === 1 ? [text(groups[0])] : groups.map((g) => `${g.key}: ${text(g)}`);
}
```
`NormalizePanel.tsx`:
- props gain `plan?: StackingPlan | null` (destructure it);
- replace the `ParamPair` (Scale / Reference frames) with: a `<select>` "Scale" (value `auto` or the fixed number; options `Auto` then `LN_SCALES` as `"{n} px"`; onChange → `patchLocal({ scaleMode: 'auto' })` or `patchLocal({ scaleMode: 'fixed', scale: n })`); help `The background model follows structures larger than this and treats smaller ones as signal.`; while `effectiveScaleMode(n.local) === 'auto'`, a `NumericField` "Grid size" (2–8, step 1, commit `patchLocal({ gridSize: Math.round(v) })`, help `Auto scale is the frames' short side divided by this, snapped to a supported size.`) and a read-only "Resolved scale" block rendering `resolvedScaleLines(plan)` one `<p className="text-xs text-content-muted">` each; then a `NumericField` "Reference frames" (3–50, as before). Keep each control's existing "default …" help line pattern (`defaults.normalization.local.*`). Reuse the select's existing class names from the PSF-model select. No Save button, no banner.
- `StageInspector.tsx`: `<NormalizePanel … plan={plan} />` (it is `null` in Settings already).

`stageSummary.ts` normalize branch:
```ts
    case 'normalize': {
      const local = config.normalization.local;
      if (local.enabled) {
        const scale = effectiveScaleMode(local) === 'auto' ? `scale auto (÷${local.gridSize})` : `scale ${local.scale}`;
        const localScale = local.localScale ? ' · local scale' : '';
        return `Local · ${scale} · ref ${local.referenceFrames}${localScale}`;
      }
      return `Global · ${outputNormLabel(config.normalization.output)} · ${config.normalization.scaleEstimator.toUpperCase()}`;
    }
```
(import `effectiveScaleMode` from `./panels/lnScale`). Task 14 appends the estimator part.

`registry.ts`: the `normalize` field becomes `{ id: 'normalize', label: 'Local normalization', keywords: ['normalization.local', 'scale', 'grid size'] }` (Task 14 adds the rest).

- [ ] **Step 4: Run the tests and the type check.** `npx vitest run src/components/stacking src/settings` → PASS; `npx tsc --noEmit -p .` → clean; `git diff --exit-code src/types`.

- [ ] **Step 5: Look at it** (`npm run ui:harness` or `npm run dev:web` against a scratch copy): the Stacking tab shows Scale = Auto, Grid size 4, and a resolved line; switching to a number hides Grid size and the resolved line; Settings → Stacking shows the Settings sentence. Report a screenshot or a short description.

- [ ] **Step 6: Commit**
```bash
git add src/components/stacking/panels/lnScale.ts src/components/stacking/panels/lnScale.test.ts \
  src/components/stacking/panels/NormalizePanel.tsx src/components/stacking/StageInspector.tsx \
  src/components/stacking/stageSummary.ts src/components/stacking/stageSummary.test.ts src/settings/registry.ts
git commit -m "feat(stacking-ui): LN scale is Auto by default with a grid size and the resolved value per group"
```

---

### Task 5: Pedestal warning — core function, export readiness, stacking plan

Spec §5.1, §5.2 (plan and readiness texts), §5.4, §11.5 (unit tests); rulings L-17, P-11, P-12.

**Files:**
- Modify: `crates/athenaeum-core/src/api/lights.rs` (`FlatOnlyTally`, `flat_only_lights`, text helpers, `ExportReadiness.calibration_warnings`, `readiness_from_data`, `get_export_readiness`'s `warn!`; tests)
- Modify: `crates/athenaeum-core/src/collab/gate.rs:~896` and `lights.rs:~1093` (struct literals: `calibration_warnings: Vec::new()`)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (per-group plan warning for a frame set; test)
- Regenerate: `src/types/models.ts` (`ExportReadiness.calibrationWarnings`); fix `src/components/export/modeBlocker.test.ts`'s literal (`calibrationWarnings: []`)

**Interfaces:**
- Produces (in `api::lights`, all `pub`):
  - `struct FlatOnlyTally { frame_ids: Vec<i64>, offsets: Vec<Option<f64>>, gains: Vec<Option<f64>>, flat_set_ids: Vec<i64> }` (`offsets`/`gains` aligned with `frame_ids`; `flat_set_ids` distinct ascending);
  - `fn flat_only_lights(conn: &Connection, light_ids: &[i64]) -> Result<FlatOnlyTally, ApiError>`;
  - `fn offset_gain_text(t: &FlatOnlyTally) -> String`;
  - `fn pedestal_readiness_sentence(n: usize, m: usize, offset_gain: &str) -> String`, `fn pedestal_plan_sentence(group_key: &str, n: usize, m: usize, offset_gain: &str) -> String`, `fn pedestal_run_sentence(group_key: &str, n: usize, offset_gain: &str, corner_ratio: Option<f64>) -> String`;
  - `ExportReadiness { …, calibration_warnings: Vec<String> }`.

- [ ] **Step 1: Write the failing tests** (`lights.rs` `mod tests`, reusing `seed_db`, `seed_frame_set`, `seed_light`, `seed_set`, `add_link`, `seed_masters` — Dark 100, Flat 101, Bias 102):
```rust
    fn set_cards(conn: &Connection, frame_id: i64, offset: Option<f64>, gain: Option<f64>) {
        conn.execute("UPDATE frames SET offset = ?1, gain = ?2 WHERE id = ?3", rusqlite::params![offset, gain, frame_id]).unwrap();
    }

    #[test]
    fn flat_only_lights_counts_exactly_the_lights_without_a_dark_or_bias() {
        let conn = seed_db();
        let s = seed_frame_set(&conn, 1);
        seed_masters(&conn);
        // 1: F only (warn); 2: F+D; 3: F+B; 4: D; 5: B; 6: none.
        for id in 1..=6 { seed_light(&conn, id, s); }
        add_link(&conn, 1, 101, "Flat");
        add_link(&conn, 2, 101, "Flat"); add_link(&conn, 2, 100, "Dark");
        add_link(&conn, 3, 101, "Flat"); add_link(&conn, 3, 102, "Bias");
        add_link(&conn, 4, 100, "Dark");
        add_link(&conn, 5, 102, "Bias");
        set_cards(&conn, 1, Some(1.0), Some(100.0));
        let t = flat_only_lights(&conn, &[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(t.frame_ids, vec![1]);
        assert_eq!(t.flat_set_ids, vec![101]);
        assert_eq!(offset_gain_text(&t), "OFFSET 1, GAIN 100");
        // Excluded lights are not counted: the caller passes the included ones.
        assert!(flat_only_lights(&conn, &[2, 3]).unwrap().frame_ids.is_empty());
    }

    #[test]
    fn offset_gain_text_covers_one_value_a_range_and_not_recorded() {
        let t = |offsets: Vec<Option<f64>>, gains: Vec<Option<f64>>| {
            offset_gain_text(&FlatOnlyTally { frame_ids: vec![0; offsets.len()], offsets, gains, flat_set_ids: vec![] })
        };
        assert_eq!(t(vec![Some(1.0), Some(1.0)], vec![Some(100.0), Some(100.0)]), "OFFSET 1, GAIN 100");
        assert_eq!(t(vec![Some(1.0), Some(5.0)], vec![Some(100.0), Some(100.0)]), "OFFSET 1–5, GAIN 100");
        assert_eq!(t(vec![None, None], vec![None, None]), "OFFSET not recorded, GAIN not recorded");
        assert_eq!(t(vec![Some(1.0), None], vec![Some(0.5), Some(0.5)]), "OFFSET 1 (not recorded on some), GAIN 0.5");
        assert_eq!(t(vec![Some(12.25)], vec![Some(139.0)]), "OFFSET 12.25, GAIN 139");
    }

    #[test]
    fn the_run_sentence_states_the_corner_ratio_or_leaves_it_out() {
        assert_eq!(
            pedestal_run_sentence("mono__G", 28, "OFFSET 1, GAIN 100", Some(0.41)),
            "group mono__G: 28 lights were calibrated with a flat but no dark or bias (OFFSET 1, GAIN 100). \
             The flat's darkest corner reads 41 % of its centre, so the offset pedestal is 2.4× larger there. \
             Link a master dark or bias with the same gain and offset."
        );
        assert_eq!(
            pedestal_run_sentence("mono__G", 28, "OFFSET 1, GAIN 100", None),
            "group mono__G: 28 lights were calibrated with a flat but no dark or bias (OFFSET 1, GAIN 100). \
             Link a master dark or bias with the same gain and offset."
        );
        assert!(pedestal_run_sentence("g", 1, "x", Some(0.083)).contains("8 % of its centre, so the offset pedestal is 12× larger"));
    }

    #[test]
    fn readiness_lists_the_pedestal_warning_and_never_blocks() {
        let conn = seed_db();
        let s = seed_frame_set(&conn, 1);
        seed_masters(&conn);
        seed_light(&conn, 1, s);
        add_link(&conn, 1, 101, "Flat");
        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(
            r.calibration_warnings,
            vec!["1 of 1 lights have a flat but no dark or bias (OFFSET not recorded, GAIN not recorded). \
                  Dividing by the flat also divides the camera's offset pedestal, which brightens vignetted \
                  corners. Link a master dark or bias with the same gain and offset.".to_string()]
        );
        // The warning is advisory: the gate's verdict is what it was without it.
        let without = ExportReadiness { calibration_warnings: Vec::new(), ..r.clone() };
        assert_eq!(check_mode_ready(&r, ExportMode::CalibratedLights), check_mode_ready(&without, ExportMode::CalibratedLights));
    }

    #[test]
    fn an_attested_set_has_no_pedestal_warning() {
        let conn = seed_db();
        let s = seed_frame_set(&conn, 1);
        seed_masters(&conn);
        seed_light(&conn, 1, s);
        add_link(&conn, 1, 101, "Flat");
        crate::db::collab::set_frames_set_attestation(&conn, 1, true).unwrap();
        assert!(compute_export_readiness(&conn, 1).unwrap().calibration_warnings.is_empty());
    }

    /// §5.4: the warning path writes nothing and never changes an auto-link pick.
    #[test]
    fn the_warning_never_changes_an_auto_link_pick() {
        // Seed a frame set with flat-only lights and a matching master dark the
        // matcher can pick (reuse the fixtures of `api::calibration`'s
        // `find_calibration_for_frame_set` tests; that function is the UI's
        // auto-link). Snapshot `SELECT * FROM calibration_set_to_frames ORDER BY id`.
        // Run auto-link → snapshot A. Compute readiness and `flat_only_lights` →
        // snapshot B == A. Run auto-link again → snapshot C == A.
    }
```
Write the last body out against the real fixture the `find_calibration_for_frame_set` tests use (grep its test module). In `plan.rs`'s test module:
```rust
    /// Spec §5.2: one plan warning per affected group, counted over the
    /// group's included lights; the readiness sentence is not copied in.
    #[test]
    fn the_plan_warns_per_group_for_flat_only_lights() {
        // A frame set with two groups (two filters) — reuse the fixture of
        // `plan_ready_with_masters_and_folders` (~3835) and add a second filter group.
        // Group A's lights: Flat only; group B's: Flat + Dark. One light of A excluded.
        // Assert: exactly one `plan.warnings` entry starts with "group <A key>: {n} of {m} lights have a flat"
        // where m = A's included count and n = its flat-only included count; none names B;
        // and `plan.readiness.calibration_warnings` has its one set-level sentence that
        // `plan.warnings` does not contain.
    }
```

- [ ] **Step 2: Run them to verify they fail to compile.** `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib flat_only offset_gain pedestal readiness_lists attested_set_has_no the_plan_warns_per_group -- --test-threads=4 2>&1 | tail -5`.

- [ ] **Step 3: Implement** in `api/lights.rs` (rustfmt-clean):
```rust
/// The lights a flat will calibrate with no dark and no bias (spec
/// 2026-10-04 §5.1, L-17), judged on their calibration links — the same
/// `get_links_for_frame` stage 1 and the export generator resolve, so it
/// names exactly the frames that will get `CALSTAT = F`. Dividing `P + V·S`
/// by a flat `V` gives `P/V + S`: the camera's offset pedestal `P` grows by
/// `1/V` where the optics vignette. Advisory only: it never blocks, never
/// writes, never changes a link (§5.4).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlatOnlyTally {
    pub frame_ids: Vec<i64>,
    pub offsets: Vec<Option<f64>>,
    pub gains: Vec<Option<f64>>,
    pub flat_set_ids: Vec<i64>,
}

pub fn flat_only_lights(conn: &Connection, light_ids: &[i64]) -> Result<FlatOnlyTally, ApiError> {
    let mut tally = FlatOnlyTally::default();
    let mut flats = std::collections::BTreeSet::new();
    for &id in light_ids {
        let links = get_links_for_frame(conn, id)?;
        let has = |t: &str| links.iter().any(|l| l.calibration_type == t);
        if has("Flat") && !has("Dark") && !has("Bias") {
            tally.frame_ids.push(id);
            flats.extend(links.iter().filter(|l| l.calibration_type == "Flat").map(|l| l.calibration_set_id));
        }
    }
    let mut stmt = conn.prepare("SELECT offset, gain FROM frames WHERE id = ?1")?;
    for &id in &tally.frame_ids {
        let (offset, gain): (Option<f64>, Option<f64>) = stmt
            .query_row(rusqlite::params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .unwrap_or((None, None));
        tally.offsets.push(offset);
        tally.gains.push(gain);
    }
    tally.flat_set_ids = flats.into_iter().collect();
    Ok(tally)
}

/// `"OFFSET 1, GAIN 100"` (P-12): one value, a range, or not recorded.
pub fn offset_gain_text(t: &FlatOnlyTally) -> String {
    format!("{}, {}", card_text("OFFSET", &t.offsets), card_text("GAIN", &t.gains))
}

pub(crate) fn card_text(name: &str, values: &[Option<f64>]) -> String {
    let mut recorded: Vec<f64> = values.iter().flatten().copied().filter(|v| v.is_finite()).collect();
    recorded.sort_by(f64::total_cmp);
    recorded.dedup();
    let partial = if values.iter().any(|v| v.is_none()) { " (not recorded on some)" } else { "" };
    match recorded.as_slice() {
        [] => format!("{name} not recorded"),
        [v] => format!("{name} {}{partial}", card_number(*v)),
        [lo, .., hi] => format!("{name} {}–{}{partial}", card_number(*lo), card_number(*hi)),
    }
}

fn card_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Two significant figures: 2.4, 12, 120.
fn two_significant(x: f64) -> String {
    if !(x.is_finite() && x > 0.0) {
        return format!("{x}");
    }
    let d = 1 - x.log10().floor() as i32;
    let p = 10f64.powi(d);
    let r = (x * p).round() / p;
    if d > 0 { format!("{r:.*}", d as usize) } else { format!("{r:.0}") }
}

const PEDESTAL_WHY: &str = "Dividing by the flat also divides the camera's offset pedestal, which brightens vignetted corners.";
const PEDESTAL_FIX: &str = "Link a master dark or bias with the same gain and offset.";

pub fn pedestal_readiness_sentence(n: usize, m: usize, offset_gain: &str) -> String {
    format!("{n} of {m} lights have a flat but no dark or bias ({offset_gain}). {PEDESTAL_WHY} {PEDESTAL_FIX}")
}

pub fn pedestal_plan_sentence(group_key: &str, n: usize, m: usize, offset_gain: &str) -> String {
    format!("group {group_key}: {}", pedestal_readiness_sentence(n, m, offset_gain))
}

pub fn pedestal_run_sentence(group_key: &str, n: usize, offset_gain: &str, corner_ratio: Option<f64>) -> String {
    let corner = match corner_ratio {
        Some(v) if v.is_finite() && v > 0.0 => format!(
            " The flat's darkest corner reads {} % of its centre, so the offset pedestal is {}× larger there.",
            (100.0 * v).round() as i64,
            two_significant(1.0 / v)
        ),
        _ => String::new(),
    };
    format!(
        "group {group_key}: {n} lights were calibrated with a flat but no dark or bias ({offset_gain}).{corner} {PEDESTAL_FIX}"
    )
}
```
(check `two_significant(1.0 / 0.083)` = 12 and `two_significant(1.0 / 0.41)` = 2.4 in the test). Add `use rusqlite::OptionalExtension;` if not present. In `ExportReadiness` after `attested`:
```rust
    // Advisory calibration sentences (spec §5.2): today only the pedestal
    // warning. Never a blocker; empty for an attested set.
    #[serde(default)]
    pub calibration_warnings: Vec<String>,
```
In `readiness_from_data`, after `attested` (~520): 
```rust
    let calibration_warnings = if attested {
        Vec::new()
    } else {
        let t = flat_only_lights(conn, light_frame_ids)?;
        if t.frame_ids.is_empty() {
            Vec::new()
        } else {
            vec![pedestal_readiness_sentence(t.frame_ids.len(), light_frame_ids.len(), &offset_gain_text(&t))]
        }
    };
```
and add the field to the literal. In `get_export_readiness` (~748), after computing `r`, when `!r.calibration_warnings.is_empty()`: recompute the tally (cheap) and log once (P-11):
```rust
        tracing::warn!(
            set_id,
            count = tally.frame_ids.len(),
            light_offset = %card_text("OFFSET", &tally.offsets),
            light_gain = %card_text("GAIN", &tally.gains),
            flat_set_ids = %tally.flat_set_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","),
            "flat without a dark or bias: the offset pedestal is amplified in vignetted corners"
        );
```
(the light ids for the tally are the same `load_light_members` set `compute_export_readiness` uses).

In `plan.rs`, in the frame-set branch of the plan (not a project, not `readiness.attested`), per group after the groups are built: collect the group's included light ids (`g.frames` minus `excluded_set`), call `crate::api::lights::flat_only_lights(conn, &ids)?`, and when non-empty push `pedestal_plan_sentence(&g.key, t.frame_ids.len(), ids.len(), &offset_gain_text(&t))` into `warnings`. No log line (P-11). Never copy `readiness.calibration_warnings` into `warnings`.

- [ ] **Step 4: Run the tests, regenerate TS, run the gate.** `src/types/models.ts` gains `calibrationWarnings: Array<string>`; fix `modeBlocker.test.ts`'s literal. Write the auto-link test body before the gate (no placeholder).

- [ ] **Step 5: Commit**
```bash
git add crates/athenaeum-core/src/api/lights.rs crates/athenaeum-core/src/collab/gate.rs \
  crates/athenaeum-core/src/stacking/plan.rs src/types/models.ts src/components/export/modeBlocker.test.ts
git commit -m "feat(calibration): warn when a light has a flat but no dark or bias (readiness and stacking plan; never a gate)"
```

---

### Task 6: Pedestal warning in the run — stage 1 and the flat-corner ratio

Spec §5.1 (the run evaluates per group at stage 1, so cache hits are covered), §5.2 (run text), §5.3; rulings L-18, P-11.

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs` (new `flat_corner_ratio`; hand-format, never rustfmt)
- Modify: `crates/athenaeum-core/src/export/calibrated_generator.rs` (one `pub(crate)` seam, `flat_norm_divisor_cached`, extracted from ~353–378 and used by `resolve_generation` itself — review I-2)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (`stage_calibrate`, per group after the resolve loop, ~2803)
- Test: `light_cal.rs` `mod tests`; `run.rs` test module

**Interfaces:**
- Consumes (Task 5): `flat_only_lights`, `offset_gain_text`, `pedestal_run_sentence`, `card_text` (`pub(crate)`; the run's `light_offset`/`light_gain` log fields are `card_text("OFFSET", &t.offsets)` / `card_text("GAIN", &t.gains)`).
- Produces: `pub fn flat_corner_ratio(flat_path: &Path, scratch_dir: &Path, divisor: f64) -> Result<f64, IntegrationError>`.

- [ ] **Step 1: Write the failing tests.**
```rust
    /// Spec §5.3: the smallest of the four corner-patch medians over the
    /// normalization divisor; c = max(16, round(0.05·min(W, H))).
    #[test]
    fn flat_corner_ratio_reads_the_darkest_corner_over_the_divisor() {
        // Write a 400 × 300 float32 FITS flat (use the module's existing test
        // writer helper or `crate::fits_writer::write_fits_f32`) whose value is
        // 1.0 in the centre, 0.5 in the top-left 20 × 20 corner, 0.7 in the
        // other three corners (c = max(16, 15) = 16 px patches sit inside them).
        // flat_corner_ratio(path, tmp, 1.0) == 0.5; with divisor 0.8 → 0.625.
    }
```
and in `run.rs` (reuse `add_master_dark_and_flat` and the stage-1 test helpers; a set whose lights link to the master flat only):
```rust
    #[test]
    fn a_flat_only_group_gets_one_run_warning_with_the_corner_ratio() {
        // Fixture (review I-3): `add_master_dark_and_flat`, then on `fixture.conn`
        // `DELETE FROM calibration_set_to_frames WHERE source_type = 'frame' AND calibration_type = 'Dark'`
        // so the lights link the master flat only. The fixture flat is a constant
        // plane, so its corners equal its centre: with flat normalization on, the
        // sentence reads "100 % of its centre, so the offset pedestal is 1.0× larger"
        // (with it off, the divisor is 1.0 and the ratio is the flat's own level —
        // assert the exact text for whichever the fixture's config uses).
        // Run. Exactly one `summary.warnings` entry starts with "group <key>: <n> lights were
        // calibrated with a flat but no dark or bias" and contains that corner text.
        // Run again (stage 1 fully cached): the warning is still there (§5.1).
        // A second fixture with the dark links kept: no such warning.
    }
```
Write both bodies fully from the named fixtures.

- [ ] **Step 2: Run to verify they fail.**

- [ ] **Step 3: Implement `flat_corner_ratio`** (beside `read_full_flat_plane`):
```rust
/// The flat's darkest corner over its normalization divisor (spec
/// 2026-10-04 §5.3, L-18): the median of each `c × c` corner patch,
/// `c = max(16, round(0.05·min(W, H)))`, the smallest of the four, divided
/// by `divisor` (the generator's `FlatNormDivisor::global_value`). Reads
/// only the top and bottom `c` rows, band by band.
pub fn flat_corner_ratio(flat_path: &Path, scratch_dir: &Path, divisor: f64) -> Result<f64, IntegrationError> {
    let src = BandSource::open(&[flat_path.to_path_buf()], scratch_dir, 1)?;
    let (w, h) = (src.width(), src.height());
    let c = 16usize.max((0.05 * w.min(h) as f64).round() as usize).min(w.min(h) / 2).max(1);
    let band_rows = src.band_rows_for_budget(MIN_BUDGET_BYTES).max(1);
    let mut planes = BandPlanes::new(&src);
    let mut read_rows = |y0: usize| -> Result<Vec<f32>, IntegrationError> {
        let mut out = vec![0f32; c * w];
        let mut y = 0;
        while y < c {
            let rows = band_rows.min(c - y);
            src.read_band(y0 + y, rows, &mut planes, 1)?;
            planes.decode_frame_into(0, &mut out[y * w..(y + rows) * w]);
            y += rows;
        }
        Ok(out)
    };
    let (top, bottom) = (read_rows(0)?, read_rows(h - c)?);
    let patch = |rows: &[f32], x0: usize| -> f32 {
        let mut v: Vec<f32> = (0..c)
            .flat_map(|r| rows[r * w + x0..r * w + x0 + c].iter().copied())
            .filter(|p| p.is_finite())
            .collect();
        if v.is_empty() { f32::NAN } else { crate::integration::stats::median_in_place(&mut v) }
    };
    let darkest = [patch(&top, 0), patch(&top, w - c), patch(&bottom, 0), patch(&bottom, w - c)]
        .into_iter()
        .filter(|m| m.is_finite())
        .fold(f32::INFINITY, f32::min);
    if !darkest.is_finite() || !(divisor.is_finite() && divisor > 0.0) {
        return Err(IntegrationError::BadInput(format!(
            "flat corners unreadable or divisor {divisor} unusable ({})",
            flat_path.display()
        )));
    }
    Ok(darkest as f64 / divisor)
}
```
(If the borrow checker rejects the `read_rows` closure capturing `planes` mutably while `src` is borrowed, write it as a small `fn` taking `&src`, `&mut planes`.)

- [ ] **Step 4: The run** (`stage_calibrate`, inside `for group in &groups`, after the resolve loop and before `if jobs.is_empty() { continue; }` so a fully cached group is covered):
  1. `ids` = the group's frames this loop visited that were not excluded (the same iteration's `frame.frame_id`s minus the runtime exclusions pushed in this loop).
  2. `let tally = crate::api::lights::flat_only_lights(&conn, &ids)` — on `Err`, `warn!` with `error` and continue the stage (advisory; never fails the run).
  3. When `!tally.frame_ids.is_empty()`: for each distinct flat master path among the affected lights (`crate::calibration_library::light_resolve::resolve_frame_inputs(&conn, id, cfg.calibration.flat_norm)` → `.flat`), resolve the divisor exactly as the generator does. `divisor_key` and `DivisorCache.entries` are private (`calibrated_generator.rs:~248/~278`), so first extract this seam from ~353–378 and make `resolve_generation_cached` call it (review I-2):
     ```rust
pub(crate) fn flat_norm_divisor_cached(
    divisors: &mut DivisorCache,
    flat_path: &str,
    scratch_dir: &Path,
    mode: FlatNormMode,
    params: &LightCalParams,
    cfa: Option<CfaGeometry>,
) -> anyhow::Result<FlatNormDivisor> {
    let key = divisor_key(flat_path, cfa);
    if let Some(d) = divisors.entries.get(&key) {
        return Ok(*d);
    }
    let d = resolve_flat_norm_divisor(Path::new(flat_path), scratch_dir, mode, params, cfa)?;
    divisors.entries.insert(key, d);
    Ok(d)
}
     ```
     The generator's `(Some(m), true)` arm becomes `flat_norm_divisor_cached(divisors, &m.path, scratch_dir, opts.flat_norm_mode, &opts.params, resolved.cfa_geometry)?`; its output is byte-identical (the calibrated-export tests prove it). In the run: `let inputs = resolve_frame_inputs(&conn, id, cfg.calibration.flat_norm)?;` gives `inputs.flat` (`ResolvedMaster { path: String, .. }`) and `inputs.cfa_geometry`. The divisor is `flat_norm_divisor_cached(rc.memo.divisors_mut(), &m.path, &scratch, cfg.calibration.flat_norm_mode, &cfg.calibration.params, inputs.cfa_geometry)?.global_value()` when `cfg.calibration.flat_norm`, else `1.0` (the generator's `Global(1.0)` arm). Check the field names on `CalibratedLightOptions` (`export/models.rs:~320`). Then call `flat_corner_ratio(Path::new(&m.path), &scratch, divisor)`. Keep the smallest `V`. A flat whose ratio fails: `warn!(path, error, "flat corner ratio unreadable; the pedestal warning omits it")` and skip it.
  4. One `warn!`:
     ```rust
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                count = tally.frame_ids.len(),
                light_offset = %offset_text,
                light_gain = %gain_text,
                flat_set_ids = %flat_ids_text,
                flat_corner_ratio = corner.unwrap_or(f64::NAN),
                "flat without a dark or bias: the offset pedestal is amplified in vignetted corners"
            );
     ```
     and `rc.warnings.push(pedestal_run_sentence(&group.key, tally.frame_ids.len(), &offset_gain_text(&tally), corner))`.
  Read the stage's real variable names (`cfg`, `scratch`) at `run.rs:2672-2810` before editing. Stage 1 has no `conn` binding: take one with `let db_handle = db(&rc.ctx)?; let conn = db_handle.conn();` as `resolve_calibrate_job` does (~1780), scoped so it is dropped before the stage's fan-out. A project subject never reaches `stage_calibrate` (it goes to `stage_prepare`), and neither does an attested set; do not add a check for them.

- [ ] **Step 5: Run the tests and the gate.** `git diff --exit-code src/types`.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/calibration_library/light_cal.rs crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/api/lights.rs crates/athenaeum-core/src/export/calibrated_generator.rs
git commit -m "feat(stacking): the run names flat-only groups and their flat's corner ratio (pedestal warning, advisory)"
```

---

### Task 7: Export tab — the calibration warning list

Spec §5.2 (export/send text), §9 (the Export tab list, `text-warning`, design tokens). Agent: frontend.

**Files:**
- Modify: `src/components/export/ExportTab.tsx` (at the top of the `exportMode === 'calibratedLights'` block, ~561)
- Test: `src/components/export/calibrationWarnings.test.tsx` (new)

- [ ] **Step 1: Extract and test.** Create `src/components/export/CalibrationWarnings.tsx`:
```tsx
import { AlertTriangle } from 'lucide-react';

// Spec 2026-10-04 §5.2: advisory calibration sentences from export readiness;
// shown while the mode is Calibrated lights (export and frame-set send).
export function CalibrationWarnings({ warnings }: { warnings: string[] }) {
  if (warnings.length === 0) return null;
  return (
    <ul className="space-y-1" aria-label="Calibration warnings">
      {warnings.map((w, i) => (
        <li key={i} className="flex items-start gap-2 text-xs text-warning">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" /> <span>{w}</span>
        </li>
      ))}
    </ul>
  );
}
```
Test (`calibrationWarnings.test.tsx`, RTL as the repo's other component tests use):
```tsx
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { CalibrationWarnings } from './CalibrationWarnings';

describe('CalibrationWarnings', () => {
  it('renders nothing without warnings', () => {
    const { container } = render(<CalibrationWarnings warnings={[]} />);
    expect(container).toBeEmptyDOMElement();
  });
  it('lists each sentence', () => {
    render(<CalibrationWarnings warnings={['3 of 30 lights have a flat but no dark or bias (OFFSET 1, GAIN 100).']} />);
    expect(screen.getByRole('list', { name: 'Calibration warnings' })).toHaveTextContent('3 of 30 lights');
  });
});
```
(If the repo's tests do not use `@testing-library/jest-dom` matchers, use `container.innerHTML === ''` and `textContent` instead — match `src/hooks/useStackingRuns.test.tsx`'s style.)

- [ ] **Step 2: Wire it** in `ExportTab.tsx`: inside `{exportMode === 'calibratedLights' && (<div className="mt-3 pt-3 border-t border-border space-y-3">`, as its first child: `<CalibrationWarnings warnings={readiness?.calibrationWarnings ?? []} />`. Never block a button on it.

- [ ] **Step 3:** `npx vitest run src/components/export` → PASS; `npx tsc --noEmit -p .`; `git diff --exit-code src/types`. Look at it on a scratch copy with a flat-only set and report.

- [ ] **Step 4: Commit**
```bash
git add src/components/export/CalibrationWarnings.tsx src/components/export/calibrationWarnings.test.tsx src/components/export/ExportTab.tsx
git commit -m "feat(export-ui): list calibration warnings under Calibrated lights"
```

---

### Task 8: Checkpoint-A harness

Spec §11.2 (the harness rows checkpoint A needs), §11.3 (the scale-normalized read, master SNR), §11.4 (Horse C11 preparation); rulings L-21, P-6, P-17. Reuse, never rebuild: every change extends an existing script, except two new small ones the spec names.

**Files:**
- Modify: `docs/superpowers/research/scripts/acceptance/tier1/tier1-compare.py` (new mode `--ln-grids`)
- Modify: `docs/superpowers/research/scripts/ln-math/halo_profile.py` (`--search R`, `--k`, `--bg0`)
- Modify: `docs/superpowers/research/scripts/ln-scale-audit/master_phot.py` (`--y0`, `--noise`)
- Create: `docs/superpowers/research/scripts/ln-model/nebula_boxes.py`
- Create: `docs/superpowers/research/scripts/ln-model/horse-c11-prep.sh` and `horse-c11-config.py`
- Modify: `docs/superpowers/research/scripts/acceptance/README.md` (one item per new mode or script)

- [ ] **Step 1: `tier1-compare.py --ln-grids <W_head> <W_base>`** (P-6). Reuse the module's `.athln` layout (`ATHLN_MAGIC`, the header/channel layout `read_athln_channel_means` documents). Add `read_athln(path) -> (scale, [(gw, gh, global_scale, location_ref, a: list[float], b: list[float])])` beside it, and a mode that:
  - pairs every `*.athln` under `<W>/stackwork/**` by relative path (`collect_athln_scales`'s walk);
  - per pair: if `(scale, gw, gh)` differ on any channel → counts it as `mesh differs` and skips it; else
    - `a_identical` when every `a` float and `global_scale` is bit-identical (compare the raw bytes, `struct.pack`), and
    - per node `|b_head − b_base| / location_ref_base` (B is in pixel units; `location_ref` is the LN reference's median, "sky");
  - prints, in this format:
    ```
    == LN grids: HEAD vs BASE ==
    sidecars paired=N missing_in_head=M mesh_differs=K
    A and global_scale bit-identical: X / N
    B move (|dB| / sky): median=… p99=… max=… nodes>1%=… of …
    ```
  - exits 0 (reported, not gated). Add `--ln-grids` to the usage string and `main()`.
  Self-check: run it on any working tree against itself → `X / N` with `X == N`, every B number 0.

- [ ] **Step 2: `halo_profile.py`.**
  - `--search R` (default 0): before reading rings, refine `(x, y)` in each master by the centroid of the 7 × 7 box around the brightest pixel of a 5 × 5 box-smoothed copy within `R` px. Print the refined position per master.
  - `--k K_LN,K_NOLN,K_EXT` (default `1,1,1`) and `--bg0 BG` (default: the no-LN master's own background annulus median): print each ring excess both raw (today's definition) and scale-normalized, `(median(X/k) − background(X/k)) / BG` (L-21).
  - `--labels A,B,C` (default `LN,noLN,external`) names the three slots in the output, so a table built from it can read `main` instead of "external" (review M-4).
  - Keep today's output lines and add the new ones after them, so the math review's numbers still reproduce.

- [ ] **Step 3: `master_phot.py`.** Make its last three positionals (`summ key dec`: the run summary, group key and the audit's decomposition CSV) optional, and print the `predicted` block only when all three are given — Horse C11 has no decomposition CSV (review I-5). Add `--y0` (default 0; today it is fixed at 0) and `--noise`. With `--noise` it runs `measure_probe` on the master (`.superpowers/target-acc/release/examples/measure_probe <master>`, the same binary `tier1-compare.py` uses), reads the plane's `.noise`, and adds `noise`, `k` (`1 / ratio_ref_over_master`) and `snr` (`k / noise`) to its JSON line (P-17).

- [ ] **Step 4: `nebula_boxes.py`** (new, numpy + `imgcmp.read`):
```
nebula_boxes.py <boxes.json> <control.fits> <master>... [--k K ...] [--size 100]
```
  - `boxes.json` is `{"nebula": [[x, y], …], "background": [[x, y], …]}` (box top-left corners). It is written and committed BEFORE the LN master is read (spec §11.3).
  - For each master `X` (with its `k`, default 1):
    - `bg(X) = median` over the background boxes' pixels;
    - for each nebula box: `excess = (median(box of X/k) − bg(X/k)) / bg0`, where `bg0 = bg(control)` (the no-LN control's raw background, one fixed denominator);
    - the raw read is the same with `k = 1` and `bg(X)` as the denominator.
  - Print one line per box per master, with both reads, and the difference from the control's excess in points of background.

- [ ] **Step 5: Horse C11 preparation** (spec §11.4). `horse-c11-prep.sh <acc-dir> <port>`:
  1. refuses unless `<acc-dir>` is under `~/.athenaeum-acc/`;
  2. builds the copy with `acceptance/prepare-catalog.sh <acc-dir>` (the dev catalog, read-only `.backup`);
  3. on the COPY only (review M-2: lights reach a set through `session_members → sessions → imaging_nights`):
     ```sql
     DELETE FROM calibration_set_to_frames
     WHERE source_type = 'frame' AND calibration_type = 'Dark' AND calibration_set_id IN (1242, 1315)
       AND source_id IN (SELECT sm.frame_id FROM session_members sm
                         JOIN sessions s ON s.id = sm.session_id
                         JOIN imaging_nights n ON n.id = s.imaging_night_id
                         WHERE n.frames_set_id = 182);
     ```
     Run it with `sqlite3 <acc-dir>/athenaeum.db`, then print `changes()`. Confirm the column names against `db/schema.rs` (~319–402, ~1067) first;
  4. prints the next steps.

  `horse-c11-config.py <run-8.json> <acc-dir/athenaeum.db>` prints the `set_stacking_config` JSON body: `{"setId": 182, "config": <run-8.json's config>, "excludedFrameIds": [<set 182 lights whose FILTER is R or B>]}`. The driver then sends it with `acceptance/api.sh set_stacking_config '<json>' <port>`. Pin the reference with the `set_frame_set_reference` command (read its arguments from `crates/athenaeum-web/src/routes/registration.rs`) to frame 10666. For the optional fixed-1024 run, the script takes `--fixed-1024` and sets `normalization.local.scaleMode = "fixed"`, `scale = 1024`.

  Never read or write the production catalog; never touch the dev catalog except through `prepare-catalog.sh`'s read-only backup.

- [ ] **Step 6: Self-checks.** Run each script's `--help`/usage. Run `tier1-compare.py --ln-grids` on an existing tree against itself, and `nebula_boxes.py` on two copies of one master (every difference 0). Run `master_phot.py` once with only its first four positionals plus `--noise` on an existing master (review I-5). Run `horse-c11-prep.sh` on a scratch `~/.athenaeum-acc/horse-c11-dry` with the server stopped, report the deleted-link count, then delete the scratch dir.

- [ ] **Step 7: Commit**
```bash
git add docs/superpowers/research/scripts/acceptance/tier1/tier1-compare.py \
  docs/superpowers/research/scripts/ln-math/halo_profile.py docs/superpowers/research/scripts/ln-scale-audit/master_phot.py \
  docs/superpowers/research/scripts/ln-model/nebula_boxes.py docs/superpowers/research/scripts/ln-model/horse-c11-prep.sh \
  docs/superpowers/research/scripts/ln-model/horse-c11-config.py docs/superpowers/research/scripts/acceptance/README.md
git commit -m "test(acceptance): LN grid compare, halo/nebula reads on the scale-normalized master, Horse C11 preparation"
```

---

### Task 9: Checkpoint A (controller)

Spec §11.1 (checkpoint A), §11.3 (the rows), §11.4, §11.5 (the warning on real data); rulings L-21, R-TA-8/9. **The controller runs this itself.** One compile at a time; nothing else heavy beside a run; Time Machine off (it is on manual since 2026-10-04 — confirm before starting).

**Pre-flight.**
- [ ] Branch head = Task 8's commit. The full core suite (`cargo test -p athenaeum-core --all-targets`) is green at that head.
- [ ] Free space: `df -h ~` ≥ 2 × 135 GB plus Horse C11 (~3 × 60 GB). Ask the owner before deleting `~/.athenaeum-acc/acc6b3-runA` (145 GB) or `acc6b3-runB` (70 GB); they are the owner's to keep.
- [ ] The acceptance build is built once: `server.sh` builds `.superpowers/target-acc` from this worktree.

**Set 204** (the `tierA-baseline` ruler plus a same-evening `main` re-run):
- [ ] `tier1/checkpoint.sh lnA baseline --numeric`, then immediately `tier1/rerun-build.sh 60df59c4 lnA-main`.
- [ ] `tier1-compare.py --numeric ~/.athenaeum-acc/tierA-lnA ~/.athenaeum-acc/tierA-lnA-main` → the §8 rows against the same-evening ruler.
- [ ] `tier1-compare.py --ln-grids ~/.athenaeum-acc/tierA-lnA ~/.athenaeum-acc/tierA-lnA-main` → **`A and global_scale bit-identical: N / N` is a gate** (spec §1.6: set 204 resolves to 1024 at this head). The B move is reported.

**Run B** (the 6b-3 project on the 4480 × 5760 grid):
- [ ] On the 6b-3 run-B tree (`~/.athenaeum-acc/acc6b3-runB`; the tree name is in the audit `2026-10-04-ln-scale-psf-width-audit.md` §1 and the 6b-3 acceptance record; read the project id from the copy's own catalog, read-only — review M-3): the branch's `project-run.sh`, then the same on a `main` worktree build, back to back.
- [ ] `grid-report.py <tree> <project> --ln-truth` on both; `tier1-compare.py --ln-grids` between the two trees (a bit-identical `A` gate, as for set 204).

**Horse C11** (L + G, rows on G):
- [ ] `horse-c11-prep.sh ~/.athenaeum-acc/horse-c11 <port>`, the config from `horse-c11-config.py` (run 8's config: LN on, 5 reference frames; its `scale: 1024` reads as auto → 1536), the reference pinned to frame 10666. Confirm in the plan:
  - groups L and G only;
  - the derived scale is 1536 (`PlanGroup.lnScalePx`);
  - the pedestal warning is in the plan and in the Export tab's readiness for set 182 (§11.5).
- [ ] Three runs on three copies:
  - (a) branch, LN on;
  - (b) branch, LN off (`normalization.local.enabled = false`, rejection normalization `scaleZeroOffset`) — the no-LN control;
  - (c) `main`, LN on — run 8's reproduction.
  - Optionally (d) branch with `--fixed-1024`.
- [ ] Before reading ANY LN master: choose the three IC 434 rim boxes and three background boxes (100 × 100 px, background within 1500 px, off the nebula) on (b)'s G master, write `boxes.json`, and commit it under `docs/superpowers/research/ln-model-fixes/`.
- [ ] Rows on the G masters:
  - `master_phot.py --noise` for each arm (gives `k`, σ, SNR);
  - `halo_profile.py (a) (b) (c) 7556 3212 --search 40 --k k_a,k_b,k_c --labels branch-LN,branch-noLN,main-LN` → rings at 60–70 / 80–90 / 100–120 px;
  - `nebula_boxes.py boxes.json (b) (a) (c) --k …`;
  - the B-node row: every member sidecar of (a), the B nodes within 600 px of the star versus the mean of their 8 neighbours (a short numpy read of the `.athln` with Task 8's reader);
  - `scale_pairs.py <the external log> 28516 29100 <(a)'s run json> G` → the ERP-slope row.
  - The run summary of (a) and (b) carries the pedestal warning for the L and G groups, with a corner ratio (§11.5).

**Rows and bars** (spec §11.3, checkpoint A column; Horse C11 rows):

| Row | Bar |
| ---- | ---- |
| Master median | within ±0.1 % of the LN reference's own median, same rectangle |
| Master MAD | ±1 % vs baseline, scale-normalized (raw beside it) |
| Master noise (MRS σ) | ±2 % vs baseline |
| FWHM | ±1 % vs baseline |
| Rejected fraction | ±0.3 pp |
| Weights | ρ ≥ 0.99, top-20 ≥ 18/20 |
| `A` grids and `global_scale` | bit-identical to `main`, set 204 and run B |
| B move | reported |
| Wall | reported, against the same-evening re-run |
| Horse C11 halo rings | within ±1 % of background of the no-LN control (scale-normalized) |
| Horse C11 B nodes | none within 600 px differs from its 8-neighbour mean by > 2 % of background |
| Horse C11 IC 434 boxes | each within ±1 point of the control's excess (scale-normalized) |
| `main` LN on reproduces run 8's deficit | ≈ −8 % of background at 60–90 px |

- [ ] **Record** in `docs/superpowers/research/2026-10-0x-ln-model-fixes-acceptance.md` (sections as the Tier C acceptance doc: Verdict, method, the tables, per-dataset notes, what is owed). Keep the per-arm working trees until Task 16 is done.
- [ ] **A miss stops the cycle** and goes to the owner with its numbers (owner rule): no re-sizing, no tuning, no second attempt with a changed constant. The one exception the spec allows: if only the IC 434 rows miss, propose the recorded second lever (a plain tile median, §1.4) to the owner — do not build it unasked.

---

### Task 10: Star selection core

Spec §3 steps 1–4, L-11, L-12, L-14; P-13, P-18. Pure functions and types; nothing is wired. No config field yet (Task 11).

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/psf_signal.rs` (`StarFit.peak5`; `fit_one`; `accept`'s literal; test literal ~1517)
- Modify: the other `StarFit { .. }` literals: `stacking/fits_artifact.rs:~115` (`decode_fit`: `peak5: f64::NAN`) and `~235` (test), `stacking/register/detect.rs:~528`, `stacking/register/frame.rs:~915`, `stacking/ln/scale.rs:~1915`, `stacking/run.rs:~15830` (tests: `peak5: f64::NAN`)
- Modify: `crates/athenaeum-core/src/stacking/ln/background.rs` (`TileNoise`)
- Modify: `crates/athenaeum-core/src/stacking/integrate.rs` (`LnStarSelection`, not yet a field)
- Create: `crates/athenaeum-core/src/stacking/ln/selection.rs`; `ln/mod.rs` (`pub mod selection;` + re-exports)

**Interfaces:**
- Produces:
  - `StarFit.peak5: f64` — the mean of the five brightest finite pixels of the fit's sampling region (L-12). It is not persisted (`#[serde(skip, default = "not_measured")]`; the `fits` codec writes 13 fields as before and decodes NaN). `PSF_FIT_VERSION` and `FITS_ARTIFACT_VERSION` do not move.
  - `stacking::integrate::LnStarSelection { min_peak_snr: f64, max_stars: u32, saturation: f64 }` (camelCase, `Copy`, `Default` 40 / 500 / 0.75), with `pub const OFF` (`NEG_INFINITY`, `u32::MAX`, `INFINITY`; tests and the probe's legacy arm only — never stored).
  - `background::TileNoise` with `from_model(&BackgroundModel)`, `from_table(&TileTable)`, `uniform(sigma: f64, plane_max: f32)`, `sigma_local(x, y) -> f64`, `plane_max() -> f32`.
  - `selection::{SelectionSide<'a> { plane: &'a [f32], width: usize, height: usize, noise: &'a TileNoise }, SelectionRule { Snr, BrightQuarter } (+ as_str "snr"/"brightQuarter"), Candidates { ranked: Vec<(usize, usize)>, quota: usize, rule: SelectionRule, fwhm_set: usize }, rank_candidates(pairs, ref_fits, tgt_fits, &SelectionSide, &SelectionSide, &LnStarSelection) -> Result<Candidates, LnError>}`.

- [ ] **Step 1: Write the failing tests.**

`psf_signal.rs` `mod tests`:
```rust
    /// L-12: `peak5` is the mean of the five brightest finite pixels of the
    /// square `fit_one` sampled — independent of the fitted model.
    #[test]
    fn peak5_is_the_mean_of_the_five_brightest_pixels_in_the_fit_region() {
        // Review I-4: plant nothing (hand-set outliers can make `accept` refuse the fit).
        // A 64 × 64 plane at 0.1, one rendered Gaussian star (FWHM 3 px, amplitude 0.4,
        // no noise) at (32.3, 31.7). Fit with `fit_stars` from one seed at the star.
        // expected = the mean of the five largest pixels within ±3 px of (32, 32):
        // every square `fit_one` can sample has r ≥ 3, and a FWHM-3 star's five
        // brightest pixels lie within 1.5 px of its centre, so the set is the same
        // whatever r it grew to.
        // Assert fits.len() == 1 and (fits[0].peak5 - expected).abs() < 1e-9.
    }
```
(Write it with the module's existing plane/seed helpers.)

`background.rs` `mod tests`:
```rust
    /// Review Focus 4 / L-14: a lone hot pixel does not set the plane
    /// maximum; a real saturated star does.
    #[test]
    fn a_hot_pixel_does_not_set_the_plane_maximum() {
        let (w, h) = (256usize, 256usize);
        let mut plane = noisy(w, h, |_, _| 0.1, 0.002, 9);
        for y in 100..105 {
            for x in 100..105 {
                plane[y * w + x] = 0.98; // a saturated 5 × 5 core
            }
        }
        plane[40 * w + 200] = 1.0; // a lone hot pixel
        let t = TileTable::measure(&plane, w, h, &CLEANING, None);
        assert_eq!(t.plane_max(), 0.98);
    }

    #[test]
    fn sigma_local_reads_the_three_by_three_block_or_the_plane_median() {
        let (w, h) = (512usize, 256usize);
        // Left half σ = 0.001, right half σ = 0.01.
        let mut plane = noisy(w, h, |_, _| 0.1, 0.001, 4);
        let right = noisy(w, h, |_, _| 0.1, 0.01, 5);
        for y in 0..h {
            for x in w / 2..w {
                plane[y * w + x] = right[y * w + x];
            }
        }
        let m = model(&plane, w, h, 256);
        let n = TileNoise::from_model(&m);
        assert!((n.sigma_local(64.0, 128.0) / 0.001 - 1.0).abs() < 0.15);
        assert!((n.sigma_local(448.0, 128.0) / 0.01 - 1.0).abs() < 0.15);
        // No accepted sample nearby: the plane median.
        let u = TileNoise::uniform(0.003, 0.9);
        assert_eq!(u.sigma_local(10.0, 10.0), 0.003);
    }
```

`selection.rs` `mod tests` (build `StarFit`s by hand; a helper `fit(x, y, fwhm, background, signal, peak5)` filling the rest with plausible values):
```rust
    // Planes are flat 0.1 except where a test paints a star's pixels.
    #[test]
    fn a_saturated_star_is_left_out_on_either_side() { /* 25 clean pairs + pair 0 whose reference
       pixel within max(2, FWHM) of its centroid reads 0.9 with plane_max 1.0 (0.9 > 0.75) and pair 1
       likewise on the target: ranked has 25 entries and contains neither (0, 0) nor (1, 1). */ }

    #[test]
    fn peak_snr_is_judged_on_both_sides_with_the_local_noise() { /* TileNoise::uniform(0.001, 1.0):
       a pair whose target peak5 − background is 0.03 (SNR 30) is out; at 0.05 it is in (rule Snr). */ }

    #[test]
    fn pairs_are_ranked_by_the_reference_signal_and_capped_at_max_stars() { /* 30 passing pairs with
       distinct reference signals in shuffled order and max_stars 25: ranked is the 30 in descending
       reference signal, quota 25, fwhm_set 30. Two pairs sharing one reference fit with equal signal
       order by target index. */ }

    #[test]
    fn fewer_than_twenty_snr_pairs_fall_back_to_the_bright_quarter() { /* 100 pairs pass saturation,
       10 pass SNR: rule BrightQuarter, ranked = the 100 by reference signal, quota = max(25, 20) = 25,
       fwhm_set = 25. With 60 passing saturation and 0 SNR: quota = max(15, 20) = 20. */ }

    #[test]
    fn fewer_than_twenty_after_saturation_is_too_few_matches() { /* 19 pairs pass saturation:
       Err(LnError::TooFewMatches { matches: 19 }). */ }

    #[test]
    fn off_keeps_every_pair_in_rank_order() { /* LnStarSelection::OFF on 40 pairs, one saturated and
       one faint: ranked has all 40, rule Snr, quota u32::MAX as usize. */ }
```
Write each body out fully; they are small hand-built fixtures.

- [ ] **Step 2: Run to verify they fail to compile.**

- [ ] **Step 3: Implement.**

`psf_signal.rs`:
```rust
fn not_measured() -> f64 {
    f64::NAN
}
```
and on `StarFit`, after `area`:
```rust
    /// Mean of the five brightest finite pixels in the fit's sampling
    /// region (spec 2026-10-04 L-12) — the peak the LN star selection
    /// judges SNR on. Not persisted: NaN on a fit decoded from the `fits`
    /// artifact.
    #[serde(skip, default = "not_measured")]
    pub peak5: f64,
```
In `fit_one`, inside the gather loop (389–403), keep the five largest values in a small ascending array:
```rust
    let mut top = [f64::NEG_INFINITY; 5];
    // ... inside the loop, after `peak = peak.max(v as f64);`:
            let v64 = v as f64;
            if v64 > top[0] {
                top[0] = v64;
                top.sort_by(f64::total_cmp);
            }
```
After `accept` returns the fit (`let mut f = accept(...)?;` — adapt to the current shape at ~433), set `f.peak5 = { let got: Vec<f64> = top.iter().copied().filter(|v| v.is_finite()).collect(); got.iter().sum::<f64>() / got.len() as f64 };` (there are ≥ 10 pixels, so `got.len() == 5`). `accept`'s literal sets `peak5: f64::NAN`.

`integrate.rs`:
```rust
/// The star selection for the LN scale (spec 2026-10-04 §3, L-11).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct LnStarSelection {
    // Both sides' (peak − background) / σ_local must reach this (5–200).
    pub min_peak_snr: f64,
    // At most this many matched PAIRS, brightest first (20–5000).
    pub max_stars: u32,
    // A pixel within max(2, FWHM) of either centroid above this fraction of
    // that plane's maximum leaves the pair out (0.5–1.0).
    pub saturation: f64,
}

impl Default for LnStarSelection {
    fn default() -> Self {
        LnStarSelection { min_peak_snr: 40.0, max_stars: 500, saturation: 0.75 }
    }
}

impl LnStarSelection {
    /// No selection at all: every matched pair, in rank order. Tests and the
    /// probe's legacy arm only (P-13); never stored — it is not valid JSON.
    pub const OFF: LnStarSelection = LnStarSelection {
        min_peak_snr: f64::NEG_INFINITY,
        max_stars: u32::MAX,
        saturation: f64::INFINITY,
    };
}
```

`background.rs`:
```rust
/// What the star selection reads from one side's background model (spec
/// §3 steps 1–2): each tile's σ where the final fit accepted it, the plane
/// median of those, and the hot-pixel-corrected plane maximum (L-14).
#[derive(Debug, Clone)]
pub struct TileNoise {
    tw: usize,
    th: usize,
    sigma: Vec<f32>,
    plane_sigma: f64,
    plane_max: f32,
}

impl TileNoise {
    pub fn from_model(m: &BackgroundModel) -> TileNoise {
        Self::build(&m.table, |k| m.accepted[k])
    }

    /// Every sample counts as accepted (probes and the test wrapper, which
    /// fit no surface).
    pub fn from_table(t: &TileTable) -> TileNoise {
        Self::build(t, |k| t.tiles[k].is_sample())
    }

    pub fn uniform(sigma: f64, plane_max: f32) -> TileNoise {
        TileNoise { tw: 0, th: 0, sigma: Vec::new(), plane_sigma: sigma, plane_max }
    }

    fn build(t: &TileTable, accepted: impl Fn(usize) -> bool) -> TileNoise {
        let sigma: Vec<f32> = (0..t.tiles.len())
            .map(|k| if accepted(k) { t.tiles[k].sigma } else { f32::NAN })
            .collect();
        let mut finite: Vec<f64> = sigma.iter().filter(|s| s.is_finite()).map(|&s| s as f64).collect();
        let plane_sigma = if finite.is_empty() { f64::NAN } else { median_f64_sorted(&mut finite) };
        TileNoise { tw: t.tw, th: t.th, sigma, plane_sigma, plane_max: t.plane_max() }
    }

    /// L-12: the median σ of the accepted samples in the 3 × 3 tile block
    /// around the tile holding (x, y); the plane median when that block has
    /// none.
    pub fn sigma_local(&self, x: f64, y: f64) -> f64 {
        if self.tw == 0 || self.th == 0 || !(x.is_finite() && y.is_finite()) {
            return self.plane_sigma;
        }
        let u = ((x.max(0.0) as usize) / TILE_PX).min(self.tw - 1);
        let v = ((y.max(0.0) as usize) / TILE_PX).min(self.th - 1);
        let mut block = Vec::with_capacity(9);
        for vv in v.saturating_sub(1)..=(v + 1).min(self.th - 1) {
            for uu in u.saturating_sub(1)..=(u + 1).min(self.tw - 1) {
                let s = self.sigma[vv * self.tw + uu];
                if s.is_finite() {
                    block.push(s as f64);
                }
            }
        }
        if block.is_empty() { self.plane_sigma } else { median_f64_sorted(&mut block) }
    }

    pub fn plane_max(&self) -> f32 {
        self.plane_max
    }
}
```

`selection.rs`:
```rust
//! The star selection for the LN scale (spec 2026-10-04 §3, L-11, L-12,
//! L-14): relative saturation, peak SNR, the brightest by the reference's
//! flux, capped on PAIRS, and the bright-quarter fallback. It runs after
//! pairing and before the flux ratio, on both scale paths, both
//! estimators, both seeds-calibration arms and the local-scale input — all
//! through `scale.rs`'s two `_core` bodies.

use super::background::TileNoise;
use super::scale::MIN_MATCHES;
use super::LnError;
use crate::stacking::integrate::LnStarSelection;
use crate::stacking::psf_signal::StarFit;

/// One side of a pair: the plane the scale reads (sanitized for detection),
/// and its background model's noise and maximum.
#[derive(Clone, Copy)]
pub struct SelectionSide<'a> {
    pub plane: &'a [f32],
    pub width: usize,
    pub height: usize,
    pub noise: &'a TileNoise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRule {
    Snr,
    BrightQuarter,
}

impl SelectionRule {
    pub fn as_str(self) -> &'static str {
        match self {
            SelectionRule::Snr => "snr",
            SelectionRule::BrightQuarter => "brightQuarter",
        }
    }
}

/// The pairs an estimator may use, brightest first, and how many of the
/// valid ones it keeps. `fwhm_set` is how many of the first `ranked`
/// entries define the common FWHM (all SNR passers, or the fallback's
/// quota) — spec §4.1's "pairs that passed steps 1–2 (or the fallback set)".
#[derive(Debug, Clone, PartialEq)]
pub struct Candidates {
    pub ranked: Vec<(usize, usize)>,
    pub quota: usize,
    pub rule: SelectionRule,
    pub fwhm_set: usize,
}

/// The largest pixel within `max(2, FWHM)` px of the fit's centroid
/// (pixel centres, inside the plane).
fn local_peak(side: &SelectionSide<'_>, f: &StarFit) -> f64 {
    let r = f.fwhm().max(2.0);
    let (x0, x1) = ((f.x - r).floor().max(0.0) as usize, ((f.x + r).ceil() as usize).min(side.width - 1));
    let (y0, y1) = ((f.y - r).floor().max(0.0) as usize, ((f.y + r).ceil() as usize).min(side.height - 1));
    let mut best = f64::NEG_INFINITY;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f64 - f.x, y as f64 - f.y);
            if dx * dx + dy * dy <= r * r {
                let v = side.plane[y * side.width + x];
                if v.is_finite() {
                    best = best.max(v as f64);
                }
            }
        }
    }
    best
}

/// L-14: relative saturation against this side's hot-pixel-corrected max.
fn saturated(side: &SelectionSide<'_>, f: &StarFit, sel: &LnStarSelection) -> bool {
    let max = side.noise.plane_max() as f64;
    max.is_finite() && local_peak(side, f) > sel.saturation * max
}

/// L-12 with P-18: no noise information (σ ≤ 0 or non-finite) is no cut.
fn snr_passes(side: &SelectionSide<'_>, f: &StarFit, sel: &LnStarSelection) -> bool {
    if sel.min_peak_snr == f64::NEG_INFINITY {
        return true; // OFF means off, even for a fit with no peak (review M-6)
    }
    let signal = f.peak5 - f.background;
    let sigma = side.noise.sigma_local(f.x, f.y);
    if !(sigma.is_finite() && sigma > 0.0) {
        return signal > 0.0;
    }
    signal / sigma >= sel.min_peak_snr
}

pub fn rank_candidates(
    pairs: &[(usize, usize)],
    ref_fits: &[StarFit],
    tgt_fits: &[StarFit],
    reference: &SelectionSide<'_>,
    target: &SelectionSide<'_>,
    sel: &LnStarSelection,
) -> Result<Candidates, LnError> {
    let rank = |v: &mut Vec<(usize, usize)>| {
        v.sort_by(|&(ra, ta), &(rb, tb)| {
            ref_fits[rb]
                .signal
                .total_cmp(&ref_fits[ra].signal)
                .then(ra.cmp(&rb))
                .then(ta.cmp(&tb))
        });
    };
    let mut unsaturated: Vec<(usize, usize)> = pairs
        .iter()
        .copied()
        .filter(|&(r, t)| !saturated(reference, &ref_fits[r], sel) && !saturated(target, &tgt_fits[t], sel))
        .collect();
    let mut snr: Vec<(usize, usize)> = unsaturated
        .iter()
        .copied()
        .filter(|&(r, t)| snr_passes(reference, &ref_fits[r], sel) && snr_passes(target, &tgt_fits[t], sel))
        .collect();
    let cap = sel.max_stars as usize;
    if snr.len() >= MIN_MATCHES {
        rank(&mut snr);
        let fwhm_set = snr.len();
        return Ok(Candidates { ranked: snr, quota: cap, rule: SelectionRule::Snr, fwhm_set });
    }
    let n = unsaturated.len();
    if n < MIN_MATCHES {
        return Err(LnError::TooFewMatches { matches: n });
    }
    rank(&mut unsaturated);
    let quota = n.div_ceil(4).max(MIN_MATCHES).min(cap);
    Ok(Candidates { ranked: unsaturated, quota, rule: SelectionRule::BrightQuarter, fwhm_set: quota })
}
```
(`MIN_MATCHES` must be `pub(crate)` or `pub` in `scale.rs`; it is `pub` today. Check for a circular-import complaint between `scale` and `selection`; there is none in Rust within one crate.)

- [ ] **Step 4: Run the tests and the gate.** `git diff --exit-code src/types` (no type is exported yet).

- [ ] **Step 5: Commit**
```bash
git add crates/athenaeum-core/src/stacking/psf_signal.rs crates/athenaeum-core/src/stacking/fits_artifact.rs \
  crates/athenaeum-core/src/stacking/register/detect.rs crates/athenaeum-core/src/stacking/register/frame.rs \
  crates/athenaeum-core/src/stacking/ln/scale.rs crates/athenaeum-core/src/stacking/run.rs \
  crates/athenaeum-core/src/stacking/ln/background.rs crates/athenaeum-core/src/stacking/integrate.rs \
  crates/athenaeum-core/src/stacking/ln/selection.rs crates/athenaeum-core/src/stacking/ln/mod.rs
git commit -m "feat(ln): star selection for the LN scale — relative saturation, peak SNR, brightest pairs, bright-quarter fallback (not wired yet)"
```

---

### Task 11: Star selection wiring; `LN_MODEL_VERSION = 3`

Spec §3 (wired everywhere), §6 (`starSelection`), §7, §10 (`ln_selected`, `ln_selection`); L-11, L-19, P-13, P-15.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/integrate.rs` (`LocalNormalizationConfig.star_selection`)
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (`resolve_ln_fields` clamps; goldens)
- Modify: `crates/athenaeum-core/src/stacking/ln/scale.rs` (`ScaleInputs`; every public entry point; the two `_core` bodies; `ScaleResult`; the debug event; tests per P-13)
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs` (`LN_MODEL_VERSION = 3`; `normalize_frame` builds the inputs; tests per P-13)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (golden `ln_*` literals)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `LnStarSelection`), regenerate `src/types/stacking.ts`
- Modify: `crates/athenaeum-core/examples/ln_probe.rs`, `examples/ln_scale_probe.rs` (compile against the new signatures with the production default selection)

**Interfaces:**
- Consumes (Task 10): `rank_candidates`, `SelectionSide`, `TileNoise`, `LnStarSelection`.
- Produces:
  - `scale::ScaleInputs<'a> { selection: LnStarSelection, reference: SelectionSide<'a>, target: SelectionSide<'a> }` (Task 13 extends it);
  - every public `relative_scale_*` entry point takes `inputs: &ScaleInputs<'_>` as its LAST parameter (`relative_scale(…)`, the test/probe wrapper, takes `selection: LnStarSelection` instead and builds its own sides from `TileNoise::from_table`);
  - `ScaleResult { …, matches (valid PSF-signal pairs before selection, unchanged meaning), selected: usize, selection: SelectionRule }` — `PartialEq` compares both new fields;
  - `LocalNormalizationConfig.star_selection: LnStarSelection`;
  - `LN_MODEL_VERSION = 3`.

- [ ] **Step 1: Write the failing tests** (`scale.rs` `mod tests`):
```rust
    /// L-11: the selection runs on both scale paths. A field with saturated
    /// stars: the production selection drops exactly the saturated pairs,
    /// and both paths agree.
    #[test]
    fn the_selection_runs_on_the_detection_and_the_seeds_path() {
        // `star_grid`'s field plus one 0.98-peak saturated 5 × 5 core per 10th star
        // (paint them on both planes), so plane_max = 0.98 and the 0.08–0.38
        // stars pass relative saturation at 0.75.
        // inputs: LnStarSelection::default(), TileNoise::from_table(&TileTable::measure(plane, ..)) per side.
        // detection: r.selection == SelectionRule::Snr, r.selected < r.matches, (r.scale - 1.25).abs() < 0.01.
        // seeds (relative_scale_from_seeds with the reference's fits as seeds through an identity map):
        // the same r.selected and r.scale within 0.5 % of the detection path's.
    }

    #[test]
    fn matches_counts_pairs_before_selection_and_selected_after() {
        // Same field with LnStarSelection::OFF: selected == matches.
        // With max_stars = 25: selected == 25, matches unchanged.
    }
```
and in `config.rs`:
```rust
    #[test]
    fn star_selection_is_clamped_to_its_ranges() {
        let read = |sel: &str| {
            resolve_config(Some(&format!(r#"{{"normalization":{{"local":{{"starSelection":{sel}}}}}}}"#)), None)
                .unwrap()
                .normalization
                .local
                .star_selection
        };
        let s = read(r#"{"minPeakSnr":1,"maxStars":5,"saturation":0.2}"#);
        assert_eq!((s.min_peak_snr, s.max_stars, s.saturation), (5.0, 20, 0.5));
        let s = read(r#"{"minPeakSnr":900,"maxStars":90000,"saturation":2}"#);
        assert_eq!((s.min_peak_snr, s.max_stars, s.saturation), (200.0, 5000, 1.0));
        assert_eq!(read("{}"), crate::stacking::integrate::LnStarSelection::default());
    }
```

- [ ] **Step 2: Run to verify they fail.**

- [ ] **Step 3: Config.** `LocalNormalizationConfig` gains, after `local_scale`:
```rust
    // Which matched stars set the scale (spec §3).
    pub star_selection: LnStarSelection,
```
`Default` adds `star_selection: LnStarSelection::default()`. `resolve_ln_fields` adds three clamps in the `!(x >= MIN)` style (NaN clamps to the minimum), `warn!` on a move: `minPeakSnr` 5–200, `maxStars` 20–5000, `saturation` 0.5–1.0. The struct stays `Copy`.

- [ ] **Step 4: `scale.rs`.**
  1. Add, near `ScaleResult`:
     ```rust
/// What the scale estimator needs beyond the two fit lists (spec §3): the
/// selection and both sides' planes and noise. Built per frame and channel
/// by `normalize_frame` (the reference side from the group's, or on a
/// project member the restricted, background model — P-7).
pub struct ScaleInputs<'a> {
    pub selection: LnStarSelection,
    pub reference: SelectionSide<'a>,
    pub target: SelectionSide<'a>,
}
     ```
  2. `ScaleResult` gains `pub selected: usize,` and `pub selection: SelectionRule,` (doc: P-15). Update the hand-written `PartialEq` and both construction sites.
  3. Add the shared middle step and use it in both `_core` bodies instead of `ratio_sample` + the `MIN_MATCHES` check (scale.rs ~835–840 and ~1292–1298):
     ```rust
/// Selection, then the flux ratios of the kept pairs (spec §3, L-11).
/// `matches` keeps its meaning — valid PSF-signal pairs BEFORE selection
/// (P-15); the sample is the selected pairs in rank order.
fn measured_sample(
    ref_fits: &[StarFit],
    tgt_fits: &[StarFit],
    pairs: &[(usize, usize)],
    inputs: &ScaleInputs<'_>,
) -> Result<(RatioSample, usize, SelectionRule), LnError> {
    let valid = |r: usize, t: usize| ref_fits[r].signal > 0.0 && tgt_fits[t].signal > 0.0;
    let matches = pairs.iter().filter(|&&(r, t)| valid(r, t)).count();
    let c = selection::rank_candidates(pairs, ref_fits, tgt_fits, &inputs.reference, &inputs.target, &inputs.selection)?;
    let mut sample = RatioSample::default();
    for &(r, t) in &c.ranked {
        if sample.ratios.len() >= c.quota {
            break;
        }
        if valid(r, t) {
            let (rf, tf) = (&ref_fits[r], &tgt_fits[t]);
            sample.ratios.push(rf.signal / tf.signal);
            sample.positions.push((rf.x, rf.y));
            sample.ref_idx.push(r);
            sample.ref_signal.push(rf.signal);
            sample.tgt_fwhm.push(tf.fwhm());
        }
    }
    if sample.ratios.len() < MIN_MATCHES {
        return Err(LnError::TooFewMatches { matches: sample.ratios.len() });
    }
    Ok((sample, matches, c.rule))
}
     ```
     (derive `Default` on `RatioSample`; delete `ratio_sample` if nothing else uses it — the `the_local_scale_floor_counts_distinct_reference_stars` test builds `RatioSample` literals, which still compile.) In the seeds path keep `breakdown.matched = matches;`.
  4. The `debug!("ln relative scale")` in both bodies gains `ln_selected = sample.ratios.len(), ln_selection = rule.as_str(),` and keeps `ln_matches = matches`.
  5. Thread `inputs: &ScaleInputs<'_>` as the last parameter through every public entry point and both `_core` bodies (`relative_scale_against`, `_masked`, `_with_diag`, `relative_scale_from_seeds`, `_masked`, `_with_breakdown`, `_with_diag`). `relative_scale(reference, target, width, height, psf, max_stars, match_radius_px, rcr_limit, local_scale, selection: LnStarSelection)` builds `TileNoise::from_table(&TileTable::measure(reference, …, &CLEANING, None))` and the same for `target`, then the two sides, then calls `relative_scale_against`.
  6. P-13 for this file's tests: every pre-existing test passes `LnStarSelection::OFF` (through a test helper `fn off_inputs<'a>(r: &'a [f32], t: &'a [f32], w: usize, h: usize, noise: &'a TileNoise) -> ScaleInputs<'a>` with `noise = TileNoise::uniform(0.0, f32::NAN)`), each with the comment `// P-13: pre-selection pin; selection is pinned by the_selection_runs_on_….`. The `with_diag_variants_match_the_plain_ones_and_the_diag_vec_is_self_consistent` test asserts `diag.len() == plain.matches`; under OFF `selected == matches`, so it holds. Change it to `plain.selected` anyway (the sample the diag describes) and say so in the report.

- [ ] **Step 5: `normalize_frame`.** After `target` is sanitized and before the scale call:
```rust
        let ref_model_for_scale = member_ref_model.as_ref().unwrap_or(&ref_models[p]);
        let (ref_noise, tgt_noise) = (TileNoise::from_model(ref_model_for_scale), TileNoise::from_model(&target_model));
        let inputs = scale::ScaleInputs {
            selection: cfg.star_selection,
            reference: SelectionSide {
                plane: &reference_for_detection.sanitized_planes[p],
                width: reference.width,
                height: reference.height,
                noise: &ref_noise,
            },
            target: SelectionSide { plane: &target, width: reference.width, height: reference.height, noise: &tgt_noise },
        };
```
and pass `&inputs` to all four scale calls (the two in `fall_back_to_detection` and the two seeds calls). `target_model` must stay alive until here (it is computed in the background block; keep the binding). Bump `LN_MODEL_VERSION` to 3 with a doc line "3 = the star selection (spec §3)".

  P-13 for `ln/mod.rs`'s tests: run them. Set `star_selection: LnStarSelection::OFF` in a test's `cfg`, with the comment, only if it fails and its subject is not the selection: `the_scale_event_carries_…`, `the_written_sidecar_carries_…`, `masked_ln_coefficients_are_finite_…`, `masked_probe`. If `masked_ln_recovers_a_half_covered_members_scale` or `masked_ln_recovers_the_half_covered_scale_from_measured_seeds` fails, report it before changing it: their subject is the scale.

  P-13 for `run.rs`'s LN tests (review C-3): every test built on `seed_ln_star_group` (and Tasks 2/3's run tests) that fails because a frame dropped under 20 matches sets `cfg.normalization.local.star_selection = LnStarSelection::OFF` with the P-13 comment. Name each one in the report. Never change a count or a tolerance instead.

- [ ] **Step 6: Goldens, TS, probes.** Register `LnStarSelection` in `ts_export.rs` before `LocalNormalizationConfig`; regenerate; `tsc` (add `starSelection` to any TS literal it flags — the Task 4 test's `withLocal` helper). Update `stage_hashes_are_pinned_to_golden_literals` (`ln_*` only), `config_hash_is_canonical_and_pinned` and `serde_names_follow_the_spec` with `// 2026-10-05 LN model fixes Task 11: starSelection + lnModelVersion 3`. The probes: pass `LnStarSelection::default()` and sides built from `TileNoise::from_table` (Task 15 adds the arms).

- [ ] **Step 7: Gate; commit.**
```bash
git add crates/athenaeum-core/src/stacking/integrate.rs crates/athenaeum-core/src/stacking/config.rs \
  crates/athenaeum-core/src/stacking/ln/scale.rs crates/athenaeum-core/src/stacking/ln/mod.rs \
  crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/ts_export.rs src/types/stacking.ts \
  crates/athenaeum-core/examples/ln_probe.rs crates/athenaeum-core/examples/ln_scale_probe.rs
git commit -m "feat(ln): the LN scale reads only selected stars on every path (LN_MODEL_VERSION 3)"
```

---

### Task 12: Common-aperture photometry core

Spec §4.1, L-13, P-8, P-10. Pure functions and types; nothing is wired, no config field (Task 13).

**Files:**
- Create: `crates/athenaeum-core/src/stacking/ln/aperture.rs`; `ln/mod.rs` (`pub mod aperture;` + re-exports)
- Modify: `crates/athenaeum-core/src/stacking/integrate.rs` (`LnFluxEstimator`, `LnAperture` — types only)

**Interfaces:**
- Produces:
  - `integrate::LnFluxEstimator { PsfSignal, CommonAperture }` (serde `"psfSignal"`/`"commonAperture"`, `Default = PsfSignal`), `integrate::LnAperture { Fwhm2, Fwhm3, GrowthCurve }` (`"fwhm2"`/`"fwhm3"`/`"growthCurve"`, `Default = Fwhm3`), both `Copy`, each with `as_str()`;
  - `aperture::FiniteMask` (`from_plane(&[f32], w, h)`, `from_bools(&[bool], w, h)`, `all(w, h)`, `get(x, y) -> bool`, `width()`, `height()`);
  - `aperture::ApertureSide<'a> { plane: &'a [f32], present: &'a FiniteMask }`;
  - `aperture::circle_flux(side, cx, cy, radius, sky_inner, sky_outer, scratch: &mut Vec<f32>) -> Option<f64>`;
  - `aperture::SKY_INNER_FWHM = 6.0`, `SKY_OUTER_FWHM = 8.0`, `GROWTH_OUTER_FWHM = 5.0`, `GROWTH_PAIRS = 100`;
  - `aperture::aperture_multiple(LnAperture) -> f64` (2, 3, 2).

- [ ] **Step 1: Write the failing tests** (`aperture.rs` `mod tests`; a helper renders Gaussian or Moffat stars on a flat sky into a `Vec<f32>`):
```rust
    #[test]
    fn circle_flux_recovers_a_gaussian_star() {
        // 128 × 128 at sky 0.1, one Gaussian star (FWHM 4 px, total flux 50.0) at (64.3, 63.6).
        // circle_flux at r = 3 × 4 px with the sky annulus [24, 32] px:
        // within 0.2 % of 50.0 · (1 − exp(−r² / (2σ²))).
    }

    #[test]
    fn a_circle_touching_an_absent_or_outside_pixel_is_invalid() {
        // The same star; mark one pixel inside the circle absent → None.
        // A star whose circle crosses the image edge → None.
    }

    #[test]
    fn an_annulus_under_half_present_is_invalid() {
        // Mark 51 % of the annulus pixels absent → None; 49 % → Some.
    }

    #[test]
    fn a_non_positive_flux_is_invalid() {
        // No star (flat sky + one negative dip inside the circle) → None.
    }

    /// The point of F1: one circle sized to the broader star measures both
    /// frames alike, whatever the seeing. Reference FWHM 3 px, target FWHM
    /// 4.5 px, target = 0.8 × reference: z = 1.25 on every star.
    #[test]
    fn a_common_circle_reads_the_same_ratio_whatever_the_seeing() {
        // 30 isolated Gaussian stars on each plane (no noise). F = 4.5, r = 3F, the
        // annulus reaches 8F = 36 px: use a 768 × 512 plane, stars on a 6 × 5 lattice
        // with 110-px spacing and a ≥ 60-px border (review M-7).
        // Median of flux_R / flux_T within 1e-3 of 1.25.
    }

    /// The growth-curve correction recovers the light beyond 2F: Moffat
    /// β = 4, FWHM 3 px vs 4.5 px, scale 1.25. Closed form (review C-2):
    /// z(2F) misses by 1.34 %; c · z(2F) = 1.25 × 1.00011. (At β = 2.5 the
    /// wings beyond 5F leave +0.37 %, so that profile cannot meet 0.3 %.)
    #[test]
    fn the_growth_correction_recovers_the_light_beyond_two_fwhm() {
        // Same lattice as above. z at 2F alone misses 1.25 by more than 1 %;
        // c · z(2F) is within 0.3 %, with c = median over stars of
        // (f_R(5F)/f_R(2F)) / (f_T(5F)/f_T(2F)).
    }
```
Write each body fully.

- [ ] **Step 2: Run to verify they fail.**

- [ ] **Step 3: Implement** (`aperture.rs`, rustfmt-clean):
```rust
//! The common-aperture photometry of the LN scale's second estimator (spec
//! 2026-10-04 §4.1, L-13): a whole-pixel circle of radius `a·F` at each
//! frame's own fitted centroid, `F` the larger of the two frames' median
//! fitted FWHMs, and a local sky from the `[6F, 8F]` annulus median. A pair
//! is valid when its circle is entirely present on both frames, at least
//! half of each annulus is present, and both fluxes are positive.

use crate::stacking::integrate::LnAperture;
use crate::integration::stats::median_in_place;

pub const SKY_INNER_FWHM: f64 = 6.0;
pub const SKY_OUTER_FWHM: f64 = 8.0;
pub const GROWTH_OUTER_FWHM: f64 = 5.0;
pub const GROWTH_PAIRS: usize = 100;

pub fn aperture_multiple(a: LnAperture) -> f64 {
    match a {
        LnAperture::Fwhm2 | LnAperture::GrowthCurve => 2.0,
        LnAperture::Fwhm3 => 3.0,
    }
}

/// Which pixels of a plane are data (1 bit per pixel; 7.6 MB at 61 Mpx).
#[derive(Debug, Clone, PartialEq)]
pub struct FiniteMask {
    width: usize,
    height: usize,
    bits: Vec<u64>,
}

impl FiniteMask {
    pub fn from_plane(plane: &[f32], width: usize, height: usize) -> FiniteMask {
        Self::from_fn(width, height, |k| plane[k].is_finite())
    }

    pub fn from_bools(mask: &[bool], width: usize, height: usize) -> FiniteMask {
        Self::from_fn(width, height, |k| mask[k])
    }

    pub fn all(width: usize, height: usize) -> FiniteMask {
        Self::from_fn(width, height, |_| true)
    }

    fn from_fn(width: usize, height: usize, f: impl Fn(usize) -> bool) -> FiniteMask {
        let n = width * height;
        let mut bits = vec![0u64; n.div_ceil(64)];
        for k in 0..n {
            if f(k) {
                bits[k / 64] |= 1 << (k % 64);
            }
        }
        FiniteMask { width, height, bits }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// `false` outside the plane.
    pub fn get(&self, x: isize, y: isize) -> bool {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return false;
        }
        let k = y as usize * self.width + x as usize;
        self.bits[k / 64] >> (k % 64) & 1 == 1
    }
}

/// One frame's plane (values are read where `present` is set) and its mask.
#[derive(Clone, Copy)]
pub struct ApertureSide<'a> {
    pub plane: &'a [f32],
    pub present: &'a FiniteMask,
}

/// `Σ_circle − N·sky` (spec §4.1), `None` when invalid: a circle pixel
/// absent or outside the plane, fewer than half of the annulus pixels
/// present, or a flux ≤ 0. Pixels whose centre lies within `radius` count
/// whole; the annulus is `sky_inner ≤ d ≤ sky_outer`.
pub fn circle_flux(
    side: &ApertureSide<'_>,
    cx: f64,
    cy: f64,
    radius: f64,
    sky_inner: f64,
    sky_outer: f64,
    scratch: &mut Vec<f32>,
) -> Option<f64> {
    let w = side.present.width();
    let reach = sky_outer.max(radius).ceil() as isize;
    let (ix, iy) = (cx.round() as isize, cy.round() as isize);
    let (r2, in2, out2) = (radius * radius, sky_inner * sky_inner, sky_outer * sky_outer);
    let (mut sum, mut n) = (0f64, 0usize);
    let mut annulus_total = 0usize;
    scratch.clear();
    for y in iy - reach..=iy + reach {
        for x in ix - reach..=ix + reach {
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            let d2 = dx * dx + dy * dy;
            let present = side.present.get(x, y);
            if d2 <= r2 {
                if !present {
                    return None;
                }
                sum += side.plane[y as usize * w + x as usize] as f64;
                n += 1;
            } else if d2 >= in2 && d2 <= out2 {
                annulus_total += 1;
                if present {
                    scratch.push(side.plane[y as usize * w + x as usize]);
                }
            }
        }
    }
    if n == 0 || scratch.is_empty() || 2 * scratch.len() < annulus_total {
        return None;
    }
    let sky = median_in_place(scratch) as f64;
    let flux = sum - n as f64 * sky;
    (flux > 0.0).then_some(flux)
}
```
`integrate.rs`:
```rust
/// The LN scale's flux definition (spec §4): the PSF fit's own signal, or a
/// common circular aperture (F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LnFluxEstimator {
    #[default]
    PsfSignal,
    CommonAperture,
}

/// F1's circle (spec §4.1): 2 or 3 × FWHM, or 2 × FWHM with the growth-curve
/// correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LnAperture {
    Fwhm2,
    #[default]
    Fwhm3,
    GrowthCurve,
}
```
with `as_str()` on both (`"psfSignal"`, `"commonAperture"`, `"fwhm2"`, `"fwhm3"`, `"growthCurve"`).

- [ ] **Step 4: Run the tests and the gate.** `git diff --exit-code src/types`.

- [ ] **Step 5: Commit**
```bash
git add crates/athenaeum-core/src/stacking/ln/aperture.rs crates/athenaeum-core/src/stacking/ln/mod.rs crates/athenaeum-core/src/stacking/integrate.rs
git commit -m "feat(ln): common-aperture photometry for the LN scale (not wired yet)"
```

---

### Task 13: Common-aperture wiring

Spec §4.1, §4.2, §6 (`fluxEstimator`, `aperture`), §7 (aperture in the subtree only under common aperture), §8 (`SummaryGroup`), §10; L-13, L-19, P-8, P-10.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/integrate.rs` (two config fields)
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (subtree: `aperture` only under `commonAperture`; goldens; test)
- Modify: `crates/athenaeum-core/src/stacking/ln/scale.rs` (`ScaleInputs` gains the estimator and the presence masks; `measured_sample`'s F1 arm and the growth curve; `ScaleResult` fields; the debug event)
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs` (`LnReferenceForDetection.present`; `normalize_frame`'s target mask before sanitizing; the inputs)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (`"ln scale resolved"` gains `ln_flux_estimator`; `SummaryGroup` fields at the push site)
- Modify: `crates/athenaeum-core/src/stacking/provenance.rs` (`SummaryGroup.ln_flux_estimator`, `.ln_aperture`)
- Modify: `crates/athenaeum-core/src/ts_export.rs`, regenerate `src/types/stacking.ts`
- Modify: the two probes (compile; PSF signal default)

**Interfaces:**
- Consumes (Task 12): `FiniteMask`, `ApertureSide`, `circle_flux`, the constants, `LnFluxEstimator`, `LnAperture`.
- Produces:
  - `LocalNormalizationConfig.flux_estimator: LnFluxEstimator` (default `PsfSignal` until Task 17), `.aperture: LnAperture` (default `Fwhm3` until Task 17);
  - `ScaleInputs { selection, estimator: LnFluxEstimator, aperture: LnAperture, reference: SelectionSide, target: SelectionSide, reference_present: &'a FiniteMask, target_present: &'a FiniteMask }`;
  - `ScaleResult { …, aperture_px: Option<f64>, common_fwhm_px: Option<f64>, growth_factor: Option<f64> }`;
  - `LnReferenceForDetection.present: Vec<FiniteMask>` (legacy: the raw reference plane's finite pixels; masked: empty — the member's overlap is used, P-8);
  - `SummaryGroup { …, ln_flux_estimator: Option<LnFluxEstimator>, ln_aperture: Option<LnAperture> }` (`#[serde(default)]`; `ln_aperture` only under common aperture).

- [ ] **Step 1: Write the failing tests.**

`scale.rs`:
```rust
    /// The point of F1, end to end: a target with broader seeing, scaled by
    /// 0.8, through `relative_scale_against` with the common aperture at 3F
    /// recovers 1.25 within 0.5 %. PSF signal's value is printed beside it.
    #[test]
    fn the_common_aperture_scale_is_independent_of_seeing() {
        // Reference: `star_grid`-style field, FWHM 3 px, plus a saturated anchor star
        // so relative saturation behaves as on a real frame. Target: same stars,
        // FWHM 4.5 px, flux × 0.8, sky unchanged, light noise.
        // Inputs: production selection, estimator CommonAperture, aperture Fwhm3,
        // presence masks FiniteMask::all.
        // assert!((r.scale - 1.25).abs() / 1.25 < 0.005, "F1 {} vs PSF signal {}", r.scale, psf.scale);
        // r.aperture_px == Some(3.0 * r.common_fwhm_px.unwrap()); r.growth_factor == None.
    }

    /// P-10: no pair has its 5F circle on both frames → c = 1, and the scale
    /// is the plain 2F ratio.
    #[test]
    fn a_growth_curve_without_a_qualifying_pair_keeps_c_one() {
        // A field whose stars all sit within 5F of an absent band (mark a grid of
        // absent pixel rows every 12 px on the target's presence mask, F ≈ 3):
        // GrowthCurve gives growth_factor == Some(1.0) and the same scale as Fwhm2.
    }
```
`config.rs`:
```rust
    #[test]
    fn the_aperture_is_in_the_subtree_only_under_the_common_aperture() {
        use crate::stacking::integrate::{LnAperture, LnFluxEstimator};
        let mut a = StackingConfig::default();
        let mut b = a.clone();
        b.normalization.local.aperture = LnAperture::Fwhm2;
        assert_eq!(normalization_subtree(&a), normalization_subtree(&b), "PSF signal ignores the aperture");
        a.normalization.local.flux_estimator = LnFluxEstimator::CommonAperture;
        b.normalization.local.flux_estimator = LnFluxEstimator::CommonAperture;
        assert_ne!(normalization_subtree(&a), normalization_subtree(&b));
        let mut c = StackingConfig::default();
        c.normalization.local.flux_estimator = LnFluxEstimator::CommonAperture;
        assert_ne!(normalization_subtree(&StackingConfig::default()), normalization_subtree(&c), "the estimator is always in");
    }
```

- [ ] **Step 2: Run to verify they fail.**

- [ ] **Step 3: Config and subtree.** `LocalNormalizationConfig` gains `pub flux_estimator: LnFluxEstimator,` and `pub aperture: LnAperture,` (`//` comments naming spec §4); `Default` adds both defaults. `canonical_normalization` (Task 3) adds: if `cfg.normalization.local.flux_estimator != LnFluxEstimator::CommonAperture` → `local.remove("aperture")`. No `LN_MODEL_VERSION` bump: the default (`psfSignal`) produces what Task 11 produced, and the subtree change moves the hashes once anyway.

- [ ] **Step 4: `scale.rs`.**
  1. `ScaleInputs` gains `estimator`, `aperture`, `reference_present`, `target_present` (field docs: P-8).
  2. `measured_sample`, after `rank_candidates`, branches on `inputs.estimator`:
     - `PsfSignal`: Task 11's loop, unchanged.
     - `CommonAperture`:
       ```rust
            let fwhm_of = |fits: &[StarFit], pick: &dyn Fn(&(usize, usize)) -> usize| {
                let mut v: Vec<f64> = c.ranked[..c.fwhm_set.min(c.ranked.len())]
                    .iter()
                    .map(|p| fits[pick(p)].fwhm())
                    .filter(|f| f.is_finite() && *f > 0.0)
                    .collect();
                v.sort_by(f64::total_cmp);
                v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
            };
            let f = fwhm_of(ref_fits, &|p| p.0).max(fwhm_of(tgt_fits, &|p| p.1));
            if !f.is_finite() {
                return Err(LnError::TooFewMatches { matches: 0 });
            }
            let radius = aperture::aperture_multiple(inputs.aperture) * f;
            let (sky_in, sky_out) = (aperture::SKY_INNER_FWHM * f, aperture::SKY_OUTER_FWHM * f);
            let rs = ApertureSide { plane: inputs.reference.plane, present: inputs.reference_present };
            let ts = ApertureSide { plane: inputs.target.plane, present: inputs.target_present };
            let mut scratch = Vec::new();
            let mut kept_pairs = Vec::new();
            for &(r, t) in &c.ranked {
                if sample.ratios.len() >= c.quota {
                    break;
                }
                let (rf, tf) = (&ref_fits[r], &tgt_fits[t]);
                let (Some(fr), Some(ft)) = (
                    aperture::circle_flux(&rs, rf.x, rf.y, radius, sky_in, sky_out, &mut scratch),
                    aperture::circle_flux(&ts, tf.x, tf.y, radius, sky_in, sky_out, &mut scratch),
                ) else {
                    continue;
                };
                sample.ratios.push(fr / ft);
                sample.positions.push((rf.x, rf.y));
                sample.ref_idx.push(r);
                sample.ref_signal.push(rf.signal);
                sample.tgt_fwhm.push(tf.fwhm());
                kept_pairs.push((r, t, fr, ft));
            }
       ```
       Then, for `GrowthCurve` (L-13, P-10), over `kept_pairs` in order, keep the first `GROWTH_PAIRS` whose `circle_flux` at `GROWTH_OUTER_FWHM · f` is `Some` on both sides, collect `(f5r / fr) / (f5t / ft)` and take the median as `c`. With none, use `c = 1.0` with `warn!(ln_common_fwhm_px = f, "ln growth curve: no pair has its outer circle on both frames; the 2 × FWHM ratio is kept")`. Then multiply every `sample.ratios[k]` by `c`. RCR is scale-equivariant, so this equals `c · RCR_loc(z)` up to rounding, and keeps the local-scale spline consistent.
     - Return `aperture_px = Some(radius)`, `common_fwhm_px = Some(f)` and `growth_factor = Some(c)` (growth only) alongside the sample. Extend `measured_sample`'s return to a small struct `MeasuredSample { sample, matches, rule, aperture_px, common_fwhm_px, growth_factor }`.
  3. `ScaleResult` gains the three `Option<f64>` fields; `PartialEq` compares them; both constructors fill them.
  4. The `"ln relative scale"` debug: under PSF signal it is Task 11's line plus `ln_flux_estimator = "psfSignal"`. Under the common aperture, it is a second `debug!` with the same message and fields, plus `ln_flux_estimator = "commonAperture"`, `ln_aperture_px` and `ln_common_fwhm_px`, and `ln_growth_factor` only in a third arm for the growth curve. Write the three macro calls out; tracing has no optional field.
  5. P-13 helper `off_inputs` sets `estimator: PsfSignal, aperture: Fwhm3` and `FiniteMask::all` for both masks (two `FiniteMask` values owned by the test).

- [ ] **Step 5: `normalize_frame` and `LnReferenceForDetection`.**
  - `LnReferenceForDetection` gains `pub present: Vec<FiniteMask>`: `build` fills `FiniteMask::from_plane(plane, w, h)` per reference plane (the raw plane, not the sanitized copy); `build_masked` leaves it empty (P-8).
  - In `normalize_frame`, after the masked block and the target model, and BEFORE the in-place sanitizing: `let tgt_present = FiniteMask::from_plane(&target, reference.width, reference.height);` (masked mode: the target is already NaN outside the overlap, so this IS the overlap).
  - `let ref_present = if masked { &tgt_present } else { &reference_for_detection.present[p] };` and extend the inputs with `estimator: cfg.flux_estimator, aperture: cfg.aperture, reference_present: ref_present, target_present: &tgt_present`.
  - The channel-count check includes `present.len()` for `Legacy`.

- [ ] **Step 6: The run and provenance.** `"ln scale resolved"` gains `ln_flux_estimator = ln_cfg.flux_estimator.as_str(),`. `SummaryGroup` gains `ln_flux_estimator` and `ln_aperture`; the push site fills `Some(ln_cfg.flux_estimator)` and `(ln_cfg.flux_estimator == CommonAperture).then_some(ln_cfg.aperture)` when LN ran for the group, `None` otherwise.

- [ ] **Step 7: Goldens, TS, probes.** Register `LnFluxEstimator` and `LnAperture` in `ts_export.rs` before `LocalNormalizationConfig`. Regenerate; `tsc`. Add `fluxEstimator: 'psfSignal', aperture: 'fwhm3'` to `stageSummary.test.ts`'s `withLocal` helper (review M-10). Goldens as in Task 11, with a dated comment naming Task 13. The probes compile with `estimator: PsfSignal`. Run the gate.

- [ ] **Step 8: Commit**
```bash
git add crates/athenaeum-core/src/stacking/integrate.rs crates/athenaeum-core/src/stacking/config.rs \
  crates/athenaeum-core/src/stacking/ln/scale.rs crates/athenaeum-core/src/stacking/ln/mod.rs \
  crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/provenance.rs \
  crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/ts_export.rs src/types/stacking.ts \
  crates/athenaeum-core/examples/ln_probe.rs crates/athenaeum-core/examples/ln_scale_probe.rs
git commit -m "feat(ln): the common-aperture estimator for the LN scale, beside PSF signal (default unchanged)"
```

---

### Task 14: Normalize panel — star selection and star flux fields

Spec §9 (the Star flux, Aperture, Minimum peak SNR / Brightest stars and Saturation limit rows; the board summary's estimator part; registry keywords). Agent: frontend.

**Files:**
- Modify: `src/components/stacking/panels/NormalizePanel.tsx`, `src/components/stacking/panels/lnScale.ts` (labels), `src/components/stacking/stageSummary.ts`, `src/components/stacking/stageSummary.test.ts`, `src/components/stacking/panels/lnScale.test.ts`, `src/settings/registry.ts`

- [ ] **Step 1: Failing tests.** `lnScale.test.ts`:
```ts
import { apertureLabel, fluxEstimatorLabel } from './lnScale';
describe('labels', () => {
  it('names the estimators', () => {
    expect(fluxEstimatorLabel('psfSignal')).toBe('PSF signal');
    expect(fluxEstimatorLabel('commonAperture')).toBe('Common aperture');
  });
  it('names the apertures', () => {
    expect(apertureLabel('fwhm2')).toBe('2 × FWHM');
    expect(apertureLabel('fwhm3')).toBe('3 × FWHM');
    expect(apertureLabel('growthCurve')).toBe('2 × FWHM + growth curve');
  });
});
```
`stageSummary.test.ts` (extend `withLocal` with `fluxEstimator: 'psfSignal', aperture: 'fwhm3', starSelection: { minPeakSnr: 40, maxStars: 500, saturation: 0.75 }`):
```ts
  it('common aperture', () =>
    expect(stageSummary('normalize', withLocal({ fluxEstimator: 'commonAperture', aperture: 'fwhm3' }))).toBe(
      'Local · scale auto (÷4) · ref 20 · common aperture 3×FWHM',
    ));
  it('growth curve', () =>
    expect(stageSummary('normalize', withLocal({ fluxEstimator: 'commonAperture', aperture: 'growthCurve' }))).toBe(
      'Local · scale auto (÷4) · ref 20 · common aperture 2×FWHM + growth',
    ));
```

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement.**
  - `lnScale.ts`: `fluxEstimatorLabel`, `apertureLabel` (for the panel), and `apertureShort` for the summary (`fwhm2` → `2×FWHM`, `fwhm3` → `3×FWHM`, `growthCurve` → `2×FWHM + growth`).
  - `NormalizePanel.tsx`, after the Reference frames field and before PSF model, in this order:
    - **Star flux** — a select over PSF signal / Common aperture. Help: `PSF signal measures each star inside its fitted profile; common aperture measures both frames in one circle sized to the broader star, independent of seeing.`
    - **Aperture** — a select over the three labels, shown only under Common aperture. Help: `The circle radius in units of the broader frame's FWHM; the growth curve adds the light beyond it from the brightest stars.`
    - **Minimum peak SNR / Brightest stars** — a `ParamPair` (5–200 step 1 / 20–5000 step 10). Help: `Only stars this far above the local noise, and at most this many of the brightest, set the scale.` It patches `starSelection` as a whole object: `patchLocal({ starSelection: { ...n.local.starSelection, minPeakSnr: v } })`.
    - **Saturation limit (× plane maximum)** — a `NumericField`, 0.5–1.0, step 0.05. Help: `Stars with a pixel above this fraction of the brightest pixel in the frame are left out.`
    - Each control keeps the "default …" help pattern.
  - `stageSummary.ts`: after the ref part, append `` ` · common aperture ${apertureShort(local.aperture)}` `` when `local.fluxEstimator === 'commonAperture'`, before the local-scale part.
  - `registry.ts`: extend the `normalize` field's keywords with `'star flux', 'common aperture', 'psf signal', 'peak snr', 'saturation'`.

- [ ] **Step 4:** `npx vitest run src/components/stacking src/settings` → PASS; `npx tsc --noEmit -p .`; `git diff --exit-code src/types`. Look at both places the panel renders and report.

- [ ] **Step 5: Commit**
```bash
git add src/components/stacking/panels/NormalizePanel.tsx src/components/stacking/panels/lnScale.ts \
  src/components/stacking/panels/lnScale.test.ts src/components/stacking/stageSummary.ts \
  src/components/stacking/stageSummary.test.ts src/settings/registry.ts
git commit -m "feat(stacking-ui): LN star flux, aperture and star selection settings"
```

---

### Task 15: Probe and `--stack-sim` arms

Spec §4.3 (the probe bars, `--stack-sim` as the discriminator), §11.2 (the probe calls the production selection and both estimators; per-member scales per variant; the gap by brightness tercile; `--stack-sim` arms); L-15, P-16. Reuse: extend `ln_scale_probe`, `sim_config.py`, `evaluate.py`; one new decision script.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/ln/scale.rs` (one public wrapper: `relative_scale_against_masked_with_diag`)
- Modify: `crates/athenaeum-core/examples/ln_scale_probe.rs` (main mode: production arms; `--stack-sim`: arms)
- Modify: `docs/superpowers/research/scripts/ln-scale-audit/sim_config.py` (`--arms`), `sim_report.py` (per-arm columns)
- Create: `docs/superpowers/research/scripts/ln-model/aperture_choice.py`
- Modify: `docs/superpowers/research/scripts/acceptance/tier1/tier1-run.sh` and `project-run.sh` (`ATH_ACC_SET_CONFIG` / `ATH_ACC_PROJECT_CONFIG`), `acceptance/README.md`

- [ ] **Step 1: The masked diag wrapper.** `relative_scale_against_masked_with_diag(…same as relative_scale_against_masked…, inputs) -> Result<(ScaleResult, Vec<ScaleMatchDiag>), LnError>` = `relative_scale_against_core(.., Some(allowed), inputs)`. A one-line test: it equals `relative_scale_against_masked` on Task 11's selection fixture.

- [ ] **Step 2: Probe main mode.** Per member, beside today's `sLib`/`sRep`/photometry output, which stays as is:
  - Build both sides' `BackgroundModel`. The reference plane is the raw one; the target is the warped plane before sanitizing, masked to the overlap under `--masked`. The scale is `--ln-scale N`, defaulting to `ln::resolve_scale(&LocalNormalizationConfig::default(), w, h)`. From each model take `TileNoise::from_model`.
  - Build the presence masks as `normalize_frame` does (P-8).
  - Run five arms through `relative_scale_against_with_diag` (or the masked variant), each with the production pairing constants (4.0, 0.3):
    - `psfOff` — `LnStarSelection::OFF`, PSF signal;
    - `psfSel` — the default selection, PSF signal;
    - `f1_2`, `f1_3`, `f1_gc` — the default selection, common aperture, each aperture.
  - Add `--max-stars-sel N` to override `maxStars` (the tercile-trend check of Task 16).
  - Per arm, emit `{"s", "sigma", "matches", "selected", "rule", "aperturePx", "commonFwhmPx", "growth", "terciles": [s1, s2, s3]}`, where `terciles` is `robust::rcr(ratios, 0.3).location` over the diag's ratios split into three equal-count groups by `ref_signal` (descending: bright, middle, faint).
  - Print them under a new `"arms"` key of the member's JSON line.
  - Update the doc comment and `usage()` (which also gain the three modes today's usage omits).

- [ ] **Step 3: `--stack-sim` arms** (P-16, plane 0 as today).
  - `SimMember` gains `#[serde(default)] arms: std::collections::BTreeMap<String, f64>` (arm → correction `s_arm / s_applied`). The truth arm stays `correction`.
  - The window simulation keeps `cur`/`cor` and their no-rejection twins. It adds, for each arm name, a window with and without rejection, built by the same `v'' = v' + (c − 1)·(v' − sky_ref)` bookkeeping the truth arm uses.
  - `outStars` gains `sim_<arm>,sim_<arm>_norej` columns (arm names in `BTreeMap` order). Validation is unchanged (variant 0 against the master).

- [ ] **Step 4: Scripts.**
  - `sim_config.py --arms <probe.jsonl>`: for each member label, `arms[name] = s_arm / applied` for every arm in the probe line. `applied` is the value `sim_config.py` already uses for the truth correction.
  - `sim_report.py`: prints each arm's median ± bootstrap SE, with and without rejection, next to `sim_cor_norej` (the truth-corrected no-rejection arm). Also prints the ratio `sim_<arm>_norej / sim_cor_norej` (L-22) and the ratio to the LN reference (reported, L-22).
  - `aperture_choice.py <dataset>=<probe.jsonl>:<truth.csv> … [--sim sim-stars.csv]` decides with spec §4.3:
    - It imports `ln-scale-audit/evaluate.py`'s gap statistics, with nothing re-implemented: gap = `s_arm / s_truth − 1`, and slope vs `Δσ² = (F_T² − F_R²)/(8 ln 2)` from `refFwhmFit`/`tgtFwhmFit`.
    - Per arm and dataset it prints the median |gap|, the slope, the mono-vs-OSC median offset (where a dataset has both), and the median per-member `sigma` (σ_z).
    - It prints the brightness-tercile trend: the median over members of `(t1 − t3)/s`, flagged above 1 %.
    - It prints which F1 variants pass step 1 on every dataset. With `--sim`, step 2 gives each survivor's no-rejection master star flux against the truth arm, and step 3 the tie-break.
    - It ends with one line: `aperture choice: <variant>` (or `none passes`).
  - `tier1-run.sh`: if `ATH_ACC_SET_CONFIG` names a file, then once the server answers and before `start_stacking`, send it with `api.sh set_stacking_config "$(cat "$ATH_ACC_SET_CONFIG")" "$PORT"`, echo the response, and refuse on an error. `project-run.sh` does the same with `ATH_ACC_PROJECT_CONFIG` and `set_project_stacking_config`. This is how checkpoint B's F1 arm is run on the same template. Document both in the README.

- [ ] **Step 5: Self-checks.**
  - Build the probe (`cargo build --release -p athenaeum-core --example ln_scale_probe` into `.superpowers/target-acc`).
  - Run it on two set-204 members from the audit's `tierA-warped` dump (if the dump is gone, regenerate it with the audit's §8 reproduction commands), and check:
    - `psfOff.s` equals today's `sLib` within 1e-12 (same pairs, same path);
    - `psfOff.selected == psfOff.matches`.
  - Run `--stack-sim` on the audit's `sim.json` with two arms and check that `validationMaxAbsDev` is unchanged (0.0).
  - Report all three.

- [ ] **Step 6: Gate; commit.**
```bash
git add crates/athenaeum-core/src/stacking/ln/scale.rs crates/athenaeum-core/examples/ln_scale_probe.rs \
  docs/superpowers/research/scripts/ln-scale-audit/sim_config.py docs/superpowers/research/scripts/ln-scale-audit/sim_report.py \
  docs/superpowers/research/scripts/ln-model/aperture_choice.py docs/superpowers/research/scripts/acceptance/tier1/tier1-run.sh \
  docs/superpowers/research/scripts/acceptance/tier1/project-run.sh docs/superpowers/research/scripts/acceptance/README.md
git commit -m "test(acceptance): LN scale probe and stack-sim arms per estimator, the aperture-choice script"
```

---

### Task 16: Pre-checks, the aperture choice, checkpoint B (controller)

Spec §3 (the brightness-tercile risk; owner ruling: raise `maxStars`, never cap the top), §4.3, §11.1 (checkpoint B), §11.3 (checkpoint-B column); L-15, L-16, L-20, L-21, L-22. **The controller runs this itself**, with the same discipline as Task 9.

- [ ] **Pre-checks: the probe on every member.**
  - Run B: mono and the three OSC planes.
  - Set 204: mono and OSC (its warps from the Task 9 `lnA` tree).
  - Horse C11 G: Task 9's branch LN-on tree (`prepare_frameset.py` on the Horse C11 copy, group G, every member).
  - Truth for every member: run B from `grid-report.py --ln-truth` (via `parse_report.py`), the frame sets from `truth.py` on the dumped warps (the audit's §8 recipe).
- [ ] **Tercile trend** (§3 risk, the owner's point 11). If `aperture_choice.py` flags a trend above 1 % for any arm on a mono group, re-run the probe with `--max-stars-sel 1000`, then 2000, and report each. If the larger cap brings the trend under 1 %, the default `maxStars` becomes that value (a one-line `LnStarSelection::default` change, Task 17's commit). The top is never capped.
- [ ] **Step 1 bars** (§4.3): `aperture_choice.py` across all five datasets → the F1 variants passing median |gap| ≤ 0.5 %, |slope| ≤ 0.5 %/px², and mono/OSC within 0.5 %. PSF signal with selection is reported against the same bars.
- [ ] **Step 2–3:**
  - `sim_config.py --arms` on set 204 mono (the audit's 400 stars), then `ln_scale_probe --stack-sim`, then `sim_report.py`, then `aperture_choice.py --sim`.
  - The chosen variant is the one closest to the truth-corrected no-rejection arm. Ties (within 0.3 %) go to the smaller σ_z, then to the smaller aperture.
  - Record the choice with its numbers.
- [ ] **Checkpoint B** (two arms, back to back, each against Task 9's `lnA` tree and a same-evening `main` re-run):
  - PSF-signal arm: `tier1/checkpoint.sh lnB-psf lnA --numeric`.
  - F1 arm: `ATH_ACC_SET_CONFIG=<set 204's config with fluxEstimator commonAperture and the chosen aperture> tier1/checkpoint.sh lnB-f1 lnA --numeric`. A runner-up within 0.3 % also runs on set 204 mono only (`lnB-f1b`).
  - Run B: both arms with `ATH_ACC_PROJECT_CONFIG`.
  - Horse C11 (L + G, rows on G): both arms.
- [ ] **Rows** (spec §11.3, checkpoint-B column):

  | Row | Bar |
  | ---- | ---- |
  | Master median | within ±0.1 % of the LN reference's own median, same rectangle |
  | Master MAD | ±1 % vs the PSF-signal arm, scale-normalized (raw beside it) |
  | Master SNR (`k/σ`) | not below the baseline's by more than 1 % |
  | FWHM | within ±1 % of the signal-weighted variance-mixture prediction from the new per-member scales (audit §5.3) |
  | Rejected fraction | ±0.3 pp |
  | Weights | ρ ≥ 0.99, top-20 ≥ 18/20 |
  | Per-member scale vs truth (F1) | median \|gap\| ≤ 0.5 %, \|slope\| ≤ 0.5 %/px², no instrument offset |
  | `--stack-sim` set 204 mono, no rejection (F1) | within ±0.5 % of the truth-corrected no-rejection arm |
  | Seeds calibration `k` per channel | reported (≈ 1 expected under F1) — read from the run log's `"ln seeds calibration"` lines |
  | Drizzle level and coverage | as before |
  | Horse C11 ERP slope (PSF signal with selection) | within ±0.2 %/px²; F1 reported (L-20) |
  | Wall | reported |

  Also re-read the Horse C11 halo, B-node and IC 434 rows for both arms (they should hold from checkpoint A).
- [ ] **Record** the checkpoint-B section of the acceptance doc. A miss stops the cycle and goes to the owner with its numbers.

---

### Task 17: The default, per §4.4 (controller, with a one-line change)

Spec §4.4 (L-16, decided in advance); §3's tercile ruling (from Task 16).

- [ ] **Apply §4.4 to Task 16's numbers**, item by item, in the acceptance doc:
  1. Truth, per member: F1 passes the step-1 bars on the four audited groups and Horse C11 G, and PSF signal with selection fails at least one of them on a mono group.
  2. Master level, scale only: F1's no-rejection master star flux is within ±0.5 % of the truth-corrected no-rejection arm (L-22); its ratio to the LN reference is reported.
  3. Master level, full runs (set 204, run B) — all four must hold:
     - F1's SNR is at most 1 % below PSF signal's;
     - rejected fraction within ±0.3 pp;
     - MAD within ±1 % (scale-normalized);
     - weights ρ ≥ 0.99.
  4. Margin: F1 beats PSF signal with selection by ≥ 1 point of median |gap| on a mono group, AND is ≥ 1 % of master star flux closer to the truth-corrected arm.
- [ ] **The outcome:**
  - **All four hold** → F1 becomes the default. In `integrate.rs`'s `LocalNormalizationConfig::default`, set `flux_estimator: LnFluxEstimator::CommonAperture` and `aperture` to the chosen variant. Update the dated comments in `config_hash_is_canonical_and_pinned` / `serde_names_follow_the_spec` / `stage_hashes_are_pinned_to_golden_literals` (ln_* only), and the frontend defaults that come from `get_stacking_defaults` (nothing to edit — the panel reads them). Math reference §4.3 is overturned for the default path (Task 18).
  - **1, 2 and 4 hold, 3 fails** → PSF signal stays the default; the numbers go to the owner.
  - **4 fails** → PSF signal stays (the ERP-faithful estimator).
  - In every case F1 ships as a choice. If Task 16's tercile check raised `maxStars`, change `LnStarSelection::default`'s `max_stars` in the same commit.
- [ ] **Gate; commit** (only if a default changed):
```bash
git add crates/athenaeum-core/src/stacking/integrate.rs crates/athenaeum-core/src/stacking/config.rs \
  crates/athenaeum-core/src/stacking/plan.rs src/types/stacking.ts
git commit -m "feat(ln): <the estimator / maxStars> default per the checkpoint-B ruling (spec §4.4)"
```
  Record the ruling (L-16's outcome, with its numbers) in the acceptance doc and in `docs/stacking/README.md` (Task 18).

---

### Task 18: Docs

Spec §12 item 9, §10 (the dictionary), §13 (what changes for users). Agent: the controller or a docs subagent with the acceptance doc in hand.

**Files:**
- Modify: `docs/stacking/README.md` (a new bold-lead section after the 6b-3 one, in its style: `**LN model fixes (2026-10-0x)** (spec …, plan …, acceptance …, rulings L-1…L-22 and P-1…P-20).` then bullets: the model at the scale; the scale rule; the selection; F1 and the default ruling; the pedestal warning; measured-out and not-re-proposed items (C-29 still, the plain tile median as the recorded second lever, λ = 1e-4's gap noise from P-20); the harness additions; tests)
- Modify: `docs/superpowers/specs/2026-09-08-stacking-math-reference.md` §4.2 (the background model: tiles, the penalized B-spline at the scale with λ per P-20, rejection rounds, the floor, the shared refit), and §4.3 when F1 became the default (Task 17)
- Modify: `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §5.2 and §9.2 (one pointer line each to the LN model fixes spec)
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (a "Dictionary extension (LN model fixes, 2026-10-04)" paragraph with every field of spec §10's table; `ln_cells_rejected` marked retired)
- Modify: `docs/superpowers/open-items.md` (unverified smokes: the Settings → Stacking panel and the Export tab warning on the desktop build; the release-note lines owed, from spec §13; the owner decisions taken at plan review, P-1 and P-20)
- Modify: `CLAUDE.md` only if a rule changed (none is expected; the Stacking summary already points to `docs/stacking/README.md`)

- [ ] **Step 1:** Write each section from the acceptance doc and the merged code. Every number is a measured one (no "about" where a number was measured). Never name a third-party program.
- [ ] **Step 2:** `git diff --stat` touches only `docs/` (and `CLAUDE.md` if a rule changed). Proof-read the dictionary paragraph against the code's actual field names (`grep -rn 'ln_selected\|ln_samples' crates/athenaeum-core/src`).
- [ ] **Step 3: Commit**
```bash
git add docs/stacking/README.md docs/superpowers/specs/2026-09-08-stacking-math-reference.md \
  docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md docs/superpowers/specs/2026-07-03-logging-overhaul-design.md \
  docs/superpowers/open-items.md
git commit -m "docs(stacking): LN model fixes — the model at the scale, the derived scale, star selection, F1, the pedestal warning"
```

The full core suite (`cargo test -p athenaeum-core --all-targets`) runs once more before any push. Push only on the owner's word.

---

## Self-review (writer)

**Spec coverage.**

| Spec | Tasks |
| ---- | ---- |
| §1.1–§1.6 | 1, 2 |
| §2 | 3, 4 |
| §3 | 10, 11, 14, 16 |
| §4.1–§4.2 | 12, 13, 14 |
| §4.3–§4.4 | 15, 16, 17 |
| §5 | 5, 6, 7 |
| §6 | 3, 11, 13 |
| §7 | 2, 3, 11, 13 |
| §8 | 3, 13 |
| §9 | 4, 7, 14 |
| §10 | the tasks that emit each field; dictionary in 18 |
| §11 | 8, 9, 15, 16 |
| §12 | the order above |
| §13 | 18 (release-note lines) |

**Out of scope, as the spec says:**
- interpolation, hot pixels, the reference-count rule;
- rejection clipping sharp cores;
- run B's unexplained 4.5 %;
- the matcher's geometry check (worked around on the acceptance copy);
- a dark-brighter-than-lights guard.

**Type consistency checked across tasks:**
- `BackgroundModel::fit(TileTable, u32)`;
- `shared_refit(&BackgroundModel, &BackgroundModel) -> PairSurfaces`;
- `normalize_frame(.., cfg, scale_px, ..)`;
- `resolve_scale(&LocalNormalizationConfig, usize, usize) -> u32`;
- `normalization_hash_for(.., beta, Option<u32>)`, `seeds_calibration_hash_for(.., beta, u32)`;
- `TileNoise::{from_model, from_table, uniform, sigma_local, plane_max}`;
- `rank_candidates(..) -> Result<Candidates, LnError>`;
- `ScaleInputs` (Task 11 fields, extended in Task 13);
- `ScaleResult.{matches, selected, selection, aperture_px, common_fwhm_px, growth_factor}`;
- `FiniteMask::get(isize, isize)`;
- `circle_flux(.., &mut Vec<f32>) -> Option<f64>`;
- `flat_only_lights(&Connection, &[i64]) -> Result<FlatOnlyTally, ApiError>`.

**Decisions this plan takes that the owner should see:**
- **P-1 and P-20** (the membrane penalty and the gradient pin) — confirmation requested.
- P-21 (a rounding-level residual MAD counts as σ_e ≤ 0; review C-1).
- P-2 (how the halo/feature pins are read), P-5 (two extra `PlanGroup` fields), P-6 (the B move in `tier1-compare.py`), P-11 (where the pedestal `warn!` is emitted), P-13 (OFF for pre-selection synthetic tests), P-19 (the spec's gap sentence is loose) — writer's decisions, open to the reviewer.
