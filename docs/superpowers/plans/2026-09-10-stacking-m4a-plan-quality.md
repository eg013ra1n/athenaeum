# Stacking M4a — Quality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the quality items the M2/M3 acceptance runs left open (spec §14 M4 "polish", `docs/superpowers/open-items.md` Stacking M2/M3 subsections): the OSC PSF Signal Weight ranking that inverts the bright-sky/dark-sky night order against the external per-frame weights, the linear-fit rejection that rejects ≈ 3× less than the external tool at the same nominal thresholds, a two-pass registration-reference pick that keeps a rotated/offset best frame from rotating the whole master, an XISF reader that measures the external tool's own frames on identical pixels, and two measured local-normalization hot spots — then the LDN 1272 acceptance re-run that re-measures every M2/M3 target (including the OSC drizzle +10 % residual) on the changed weights.

**Architecture:** M4a changes no stage boundary and no table. The measurement stage (`stacking::measure`) keeps its estimators and constants but changes WHICH stars feed them: the PSF-fit seed population becomes noise-relative instead of rank-budgeted, and the fit acceptance grows the external tool's adaptive sampling region and inner-region rules (math reference §1.4), so per-frame star counts track the external tool's per channel and per night. The integration engine's linear-fit rejection gets the robust minimum-absolute-deviation line the math reference describes plus a named dispersion constant calibrated on the acceptance set. The register stage runs a dry first pass over the reference's own group when the reference is automatic, re-picks the reference among the top-weighted frames closest to the set's median transform, then runs the persisting pass everyone already knows. A dev harness (`examples/weight_audit.rs` + a comparison script) measures any list of FITS/XISF frames through the production estimator and compares per-channel terms with the external tool's log, which is the acceptance instrument for the weight work.

**Tech Stack:** Rust (athenaeum-core `stacking/{measure,psf_signal,run,weights,config}.rs`, `integration/combine.rs`, `stacking/ln/{grid,mod}.rs`, `examples/weight_audit.rs`; rustafits `src/formats/xisf.rs`, `src/analysis/adaptive_detection.rs`), Python 3 (the comparison script, stdlib only), React/TS (`MeasurePanel.tsx`, `ReferencePanel.tsx`, `stageSummary.ts`), ts-rs regeneration, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §4 (measurement, selection, reference), §6.3 (rejection, the Auto ladder), §9.2 (`measurement.*`, `reference.*`), §13 (acceptance targets), §14 M4, §15 · **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §1.1–1.6 (PSF Signal Weight, the detector and acceptance rules, selection/reference rules), §2.3 (MRS), §3.4 (linear fit clipping, the robust line) · **Findings this plan answers:** `docs/superpowers/research/2026-09-10-m3-acceptance-run.md` findings 1, 7, 8; `docs/superpowers/research/2026-09-10-m2-acceptance-run.md` (rejection fraction 0.83/0.74 % vs 2.5–2.8 %) · **External ground truth:** the external tool's run log `~/Pictures/Calibration Test/LDN1272-Output/logs/20260908121336.log` (per frame and per channel: `TFlux`, `TMeanFlux`, `M*`, `N*`, `PSF fits`, `sigma_n`) and its calibrated/debayered frames `~/Pictures/Calibration Test/LDN1272-Output/{calibrated,debayered}/…/*.xisf` (371 lights: 208 mono `_c.xisf` Gray Float32, 160 OSC `_c_d.xisf` RGB Float32, plus 3 that the catalog excludes).

## Global Constraints

- No new crate dependencies; `tracing` only (no `println!`/`eprintln!` outside tests/examples); never name other software in code or comments (the math reference and the acceptance notes are the only places that do).
- Two backends in sync: no new commands are expected; if one is added, Tauri + Axum + `invoke_handler` + `build_router` + `ts_export.rs` in the same task.
- Headless build: `cargo check -p athenaeum-core --no-default-features` must stay clean — everything under `stacking/` is gated `#[cfg(all(feature = "render", feature = "solver"))]` like its siblings.
- New log field names go into the "Unified event schema" dictionary of `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in the same task.
- rustfmt only on new leaf files and files that are rustfmt-clean today (`rustfmt --check` first); never `integration/engine.rs`, `integration/combine.rs`, `integrate.rs`, `stats.rs`, `weights.rs`, `run.rs`, `plan.rs`, `measure.rs`, `mod.rs`, `ts_export.rs`, `schema.rs`.
- Serde names are spec §9.2's verbatim; new config fields are `#[serde(default)]` with a `fn default_*` and appear in `src/types/stacking.ts` through ts-rs regeneration (`cargo test -p athenaeum-core --test ts_contract` or the crate's export test — whichever the repo uses today, check `ts_export.rs`'s own doc), never hand-edited.
- Every pixel path runs on the units the engine uses (float32 in [0, 1] as read from the calibrated artifacts); the PSF estimators keep working on the ADU-scaled copy (`measure::ADU_SCALE`) exactly as today.
- The M1 byte-identical engine pins (Plan 4), the M2 no-grid pins and the M3 drizzle pins must keep passing; a task that changes a pinned number on purpose says so in its commit message and updates the pin in the same commit.
- The rustafits submodule is a separate repository: a change there is its own commit inside `rustafits/` (same author/trailers), then the gitlink bump is part of the athenaeum commit that needs it. Never leave the gitlink pointing at an unpushed-and-uncommitted state; never push either repository.
- Commit as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit …` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY` trailers.

## Rulings made while planning (binding unless the spec says otherwise)

- **R-M4a-1 Root cause of the OSC weight inversion is the seed population, not the estimators.** Measured on run 12 against the external log, frame by frame (208 mono, 160 OSC): `M*` agrees to 0.97–1.00 (mono) / 0.92–1.16 (OSC), `σ_N` to 1.00 (mono) / 0.92–0.99 (OSC R) but 0.67–0.78 (OSC G/B); the number of accepted PSF fits is 1.1–1.2× the external tool's on mono and 1.6–1.8× (dark nights) to **4.1–7.8× (the bright-sky night)** on OSC, with a NEGATIVE per-frame count correlation on OSC (Spearman −0.37 R, −0.48 G, −0.58 B). `TFlux` follows (0.85–0.90 mono; 3.2–4.6× OSC). The reason: measurement seeds come from `ImageAnalyzer::detect_fast_data`, whose `star_levels` picks the two detection levels by HISTOGRAM RANK (the intensity below which `6·max_stars` and `24·max_stars` brightest pixels lie — a fixed bright-pixel budget of 147 k / 590 k px at `max_stars = 24576`), so a sharp night yields more distinct peaks inside the same budget and a bright sky costs nothing; the external tool's detector is noise-relative and its fit acceptance discards more. The fix is a noise-relative seed threshold plus the §1.4 acceptance rules (Task 2); the estimators, `PSFSW_NUM/DEN`, `PSFSNR_NUM/DEN`, `N_STAR_FROM_MAD` and the MRS wrapper are untouched.
- **R-M4a-2 Acceptance instrument for the weights.** The external tool's log carries per-frame, per-channel `TFlux`/`TMeanFlux`/`M*`/`N*`/`PSF fits`/`sigma_n`; PSFSW recomputed from those terms with our constants is the per-channel reference ranking. Targets (Task 2, verified again in Task 7): per-channel Spearman of PSFSW ≥ 0.90 on BOTH groups (run 12: mono 0.924; OSC 0.944 R, 0.872 G, 0.447 B); per-channel Spearman of accepted-fit counts ≥ 0.80; per-night median ratio of our fits to the external tool's inside `[0.7, 1.4]` on every channel and night (run 12: 1.1–1.2 mono; 1.6–7.8 OSC); top-20 overlap of frame-mean normalized weights ≥ 14/20 per group (run 12 by the harness's own definition — ours = mean over channels of `psfSignalWeight`, theirs = mean over channels of PSFSW recomputed from the log's per-channel terms: 18/20 mono, 12/20 OSC; the M3 note's 1/20 compared our production weights against the log's single per-frame summary value, a different instrument — AMENDED after Task 1's baseline, ruling R-M4a-12). Measured on the external tool's OWN calibrated/debayered frames so pixel data is identical — which is why the XISF reader fix is Task 1, not a leftover.
- **R-M4a-3 No fudge constant on the weights.** If the count targets cannot be met by the seed threshold and the §1.4 acceptance rules alone, Task 2 stops at the best rule set it found, records the residual in the ledger and the acceptance note, and does NOT rescale weights per night or per group.
- **R-M4a-4 Linear-fit rejection: the robust line first, then one calibration round.** `reject_linear_fit` fits the sorted values against rank with the minimum-absolute-deviation line (Numerical Recipes 15.7 `medfit`: for a slope `b` the intercept is `median(y − b·x)`, the slope is bracketed and bisected on the sign of `Σ x·sgn(y − a − b·x)`), dispersion `s = LINEAR_FIT_SIGMA_SCALE · 2 · adev` with `adev` the mean absolute residual over the current survivors and `LINEAR_FIT_SIGMA_SCALE = 1.0` at first; Task 7 measures the rejected fraction at the shipped 5.0/3.5 on the acceptance set and, if it is outside 2.3–3.3 %, ONE calibration round sets the constant so it lands there (recorded as a ruling with the measured number; the constant is documented as empirical). The `sqrt(1 + b²)` slope term stays omitted (dimensionally wrong on unnormalized stacks — the existing comment). The zero-dispersion guard, the `w == 0` survivor rule and the iteration cap are unchanged.
- **R-M4a-5 Two-pass reference is Auto-only and dry-first.** With `reference.mode = auto` and `reference.twoPass = true` (default true), stage 5 registers the reference's OWN group once WITHOUT writing `registration_results` rows or registered artifacts (pass 1), takes the median rotation and translation of the successful alignments, scores the top `TWO_PASS_CANDIDATES = 10` frames by normalized weight (the group's `best_by_weight` order — the registration reference is a geometry/quality choice, the sky-penalized order is for normalization only, ruling R-M3-17) by their corner displacement from the median transform `d = sqrt((Δθ_rad · D/2)² + |Δt|²)` (`D` the reference frame's diagonal in px), and switches the reference to the argmin when the current reference's own `d` exceeds the best candidate's by at least `TWO_PASS_MIN_GAIN_PX = 4.0` px; pass 2 is the existing persisting loop over every group with the final reference. Manual references never move. A switch updates `stacking_runs.reference_frame_id`, the summary's reference block, and adds one run warning `reference switched by the two-pass pick: <old> → <new> (corner displacement <d_old> → <d_new> px)`. Pass 1 is bounded by the reference group's size (≈ 1–2 min on the acceptance set); pass-1 star lists are NOT cached across passes (simplicity; the cost is bounded).
- **R-M4a-6 Stale check honours the switched reference.** `plan.rs::compute_register_stale` (the plan-time "would registration re-run?" check) treats the LAST done run's `stacking_runs.reference_frame_id` as the expected reference when `reference.twoPass` is on and the stored `registration_results` rows carry that id — otherwise every plan after a switch would call registration stale forever and every run would pay two passes again.
- **R-M4a-7 The plan-time pixel-scale warning moves to M4b.** Spec §15 names it the first item of the mixed-pixel-scale work; it lands as Task 1 of the M4b plan together with the pixel-size column it needs, not here.
- **R-M4a-8 `measure_probe`'s XISF branch is fixed in rustafits, not dropped.** The reader lives in `rustafits/src/formats/xisf.rs` (`read_xisf_image` → `parse_xisf_xml`, which stops at the FIRST `<Image>` element and ignores everything but geometry/sampleFormat/pixelStorage/location/compression); Task 1 characterises the disagreement against the raw first-`<Image>` attachment with a fixture and fixes whichever attribute or element selection is wrong. The probe's own code is untouched except for the usage line.
- **R-M4a-9 New config fields, no version bump.** `measurement.detectionSigma` (f64, default set by Task 2 from the harness, initial 5.0) and `reference.twoPass` (bool, default true) are `#[serde(default)]`; `STACKING_CONFIG_VERSION` is unchanged (both decode from every stored document). Both enter `config_hash`, so the Measure stage cache invalidates for every set on the first M4a run — accepted (Task 7 re-runs everything anyway).
- **R-M4a-11 The rustafits XISF float convention stays** (Task 1 fix round 1): XISF float samples come back bounds-normalized × 65535 — the u16-like ADU domain `integration/banded.rs::spill_via_read_raw` (master builds, light calibration) and `analysis/analyzer.rs::analyze_frame` (the ADU-domain detector) depend on; the M3 "XISF branch" finding was the two probes' own `Float32` arm never dividing by 65535. The reader's genuine fixes (largest `<Image>` wins, ties keep the first — the external masters carry a same-size weight-map image AFTER the data; `byteOrder`; `bounds`) ship; the probes divide.
- **R-M4a-12 The harness IS the instrument** (Task 1 baseline): every "before" number in R-M4a-1/R-M4a-2 is re-stated by the harness's definition where it differs from the M3 note's (only the OSC top-20 overlap does: 12/20, not 1/20); the targets are unchanged.
- **R-M4a-13 (Task 2 fix round 1) The 3×3 median seed pre-filter is an experiment with a bar.** `measurement.seedPrefilter = median3` applies the reference detector's radius-1 hot-pixel median to the DETECTION copy only (the PSF fit, the aperture flux, M*/N*, MRS all stay on the unfiltered plane); it becomes the default only if, at some σ, mono's per-night fit ratios stay in [0.7, 1.4], every OSC channel's bright-night ratio is ≤ 2× its dark nights' and the minimum per-channel PSFSW ρ is ≥ the shipped 0.683. Bar not met (the median also cut the noise the level was taken from) → shipped as an option, default `none`.
- **R-M4a-14 (Task 2 fix round 2) The pre-filter must not move the threshold.** With `median3` the detection levels are ABSOLUTE (ADU above background) from the UNFILTERED plane's MRS noise (`DetectionLevels::Absolute`); same bar; not met (R meets ≤ 2×, G/B 2.3/2.6, mono depleted except at σ 12, min ρ 0.37–0.46) → the calibration for M4a ends; the §5.1 structure-map front end is M4c Task 0 (R-M4c-11).
- **R-M4a-15 (Task 2 fix round 3) The PSF fitter is versioned.** `psf_signal::PSF_FIT_VERSION` (= 2) is folded into BOTH `measurement_subtree` and `normalization_subtree`, because `stacking/ln/scale.rs` fits its matched stars through the same `fit_one` — a fitter change recomputes cached metrics AND `.athln` sidecars exactly once. `detectionSigma` is clamped to [1, 100] in `resolve_config`.
- **R-M4a-16 (Task 3 fix round 1) medfit cost.** The median inside the bracket is `select_nth_unstable_by` (odd/even handled), the bracket is warm-started from the previous rejection iteration's converged slope, and the trailing re-evaluation is dropped; the bisection tolerance stays `1e-3·σ_b`; an exact root at the seed (`f == 0`) returns immediately (it was walked away from before — 2.8 % of n = 20 stacks under-rejected).
- **R-M4a-17 (Task 3 fix round 1) The cost bar is judged end to end.** `reject_linear_fit` measures 12.6× the least-squares routine at n = 20 and 8.5× at n = 200 (≈ 16 µs per pixel stack, ≈ 40 s per 26 Mpx plane); the isolated cold primitive reads 65–100× because ~14 evaluations × 2 O(n) passes replace one pass. Accepted; the acceptance run records the Integrate stage time (10.85 min for both groups vs run 11's 5.01 — an upper bound measured under concurrent load) and the hybrid option (the robust line on iteration 1, least squares afterwards) stays recorded in open-items for a clean re-measurement above 2×.
- **R-M4a-18 (Task 4 fix round 1) `FastPreview` keeps one pass.** The preset sets `reference.twoPass = false` — a preset named "fast" must not pay the dry pass. Also from that round: the run handle is de-registered through an RAII guard (panic-safe), the two-pass switch degrades to "keeping the current reference" on a failure instead of failing the run, and the brief's claim that flipping `twoPass` makes Register stale was wrong — the registration hash never folds `cfg.reference`, and NOT-stale is the desirable behaviour (a checkbox that says HOW a reference is chosen must not re-register every set).
- **R-M4a-19 (Task 5 fix round 1) The weight table's drift is accepted and pinned.** At a non-power-of-two stride (`scale` 768 → 96, 1280 → 160 — every LN scale step except 256/512/1024/2048/4096) the table's `fx = (x mod stride)/stride` differs from the old per-pixel `x/stride − floor` by ≤ 8.1e-5 (row values ≤ 6.3e-6) because the old formula's rounding error grew with `x`; the table is the more exact of the two. Pinned at 1e-9 for stride 128 and 1e-4 for strides 96/160, with the reason in the test.
- **R-M4a-10 Measurement seeds stay a fast detector.** Task 2 changes the detection LEVELS of `detect_stars_adaptive` (a rustafits hook that lets the caller replace the rank-based `star_levels` pair with `background + k·noise` / `background + k₂·noise` AND stops the retry ladder there — today's ladder falls through to a global `background + 30·noise` peak level and then a per-tile adaptive pass until `max_stars` is reached, which is where the sharp-night excess comes from; with `NoiseRelative` those two deeper arms do not run), not the detector. The per-star `snr` the seed filter compares with `min_snr` is the detector's aperture-flux SNR `flux / sqrt(flux + π·r_ap²·σ²)`, so `min_snr` is the second lever (flux-based, closer to the external tool's sensitivity than a peak level). `SeedSource::Full` (the full analyzer) stays available as the cross-check it already is; it is not the production path (it fits PSFs of its own and is 3–5× slower on 26 Mpx).

---

## File structure

- Create `crates/athenaeum-core/examples/weight_audit.rs` — measure a list of frames (FITS or XISF, mono or 3-plane) through `measure_plane_with_seeds` with the production options, one JSON line per file with every `ChannelMeasurement` field per plane plus the file stem; options for seed source, detection sigma, PSF model, max stars.
- Create `docs/superpowers/research/scripts/weight_audit_compare.py` — parse the external log's per-frame blocks into `wbpp-terms.json`, join with the harness JSONL by stem, print per-night ratio tables and per-channel Spearman/top-20 overlap (stdlib only).
- Modify `rustafits/src/formats/xisf.rs` — the `<Image>` selection / attribute fix (Task 1) + a fixture test; `rustafits/src/analysis/adaptive_detection.rs` + `analysis/mod.rs` — the caller-supplied detection levels hook (Task 2).
- Modify `crates/athenaeum-core/src/stacking/measure.rs` — `MeasureOptions.detection_sigma`, the noise-relative seed path (Task 2).
- Modify `crates/athenaeum-core/src/stacking/psf_signal.rs` — `FitParams.{inner_margin, region_growth}`, the adaptive sampling region and the inner-region acceptance (Task 2).
- Modify `crates/athenaeum-core/src/stacking/config.rs` — `MeasurementConfig.detection_sigma`, `ReferenceConfig.two_pass` (Tasks 2, 4); `crates/athenaeum-core/src/stacking/run.rs` — the seed options plumbing (Task 2), the two-pass register stage (Task 4); `crates/athenaeum-core/src/stacking/plan.rs` — the stale check (Task 4); `crates/athenaeum-core/src/stacking/weights.rs` — `two_pass_pick` (Task 4).
- Modify `crates/athenaeum-core/src/integration/combine.rs` — `reject_linear_fit` robust line + `LINEAR_FIT_SIGMA_SCALE` (Task 3).
- Modify `crates/athenaeum-core/src/stacking/ln/grid.rs` (`LnScratch` weight table) and `crates/athenaeum-core/src/stacking/ln/mod.rs` (`Cow` planes) (Task 5).
- Modify `src/components/stacking/panels/MeasurePanel.tsx` (detection sigma control), `src/components/stacking/panels/ReferencePanel.tsx` (two-pass checkbox), `src/components/stacking/stageSummary.ts` (reference row text), `src/types/stacking.ts` (regenerated) (Tasks 2, 4).
- Modify `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary), `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §4/§6.3/§9.2/§14 (the rulings, one line each), `CLAUDE.md` → Stacking (M4a paragraph), `docs/superpowers/open-items.md` (Task 7).
- Create `docs/superpowers/research/2026-09-1x-m4a-acceptance-run.md` (Task 7).

---

### Task 1: The weight-audit harness and the XISF reader fix

**Files:**
- Modify: `rustafits/src/formats/xisf.rs` (`parse_xisf_xml`, `read_xisf_image`, `convert_to_float32`)
- Create: `crates/athenaeum-core/examples/weight_audit.rs`
- Create: `docs/superpowers/research/scripts/weight_audit_compare.py`
- Modify: `crates/athenaeum-core/examples/measure_probe.rs` (usage line only)
- Test: `rustafits/src/formats/xisf.rs` (`#[cfg(test)]`), `crates/athenaeum-core/examples/weight_audit.rs` (a `--self-test` arm is not needed; the harness is exercised by Task 2)

**Interfaces:**
- Consumes: `athenaeum_core::stacking::measure::{measure_plane_with_seeds, MeasureOptions, SeedSource, ChannelMeasurement}`, `astroimage::ImageConverter::read_raw`, `athenaeum_core::integration::plane_reader::PlaneReader`.
- Produces: `examples/weight_audit.rs` CLI — `weight_audit [--seed fast|full] [--sigma <k>] [--psf auto|moffat4] [--max-stars <n>] [--out <file.jsonl>] <file>…` — one JSON object per line: `{"stem": "<file stem>", "width": w, "height": h, "channels": [<ChannelMeasurement as serde JSON>…], "duration_ms": n}`; `weight_audit_compare.py --log <external log> --ours <file.jsonl> [--terms-out wbpp-terms.json]` prints the tables of ruling R-M4a-2. `--sigma` is accepted from the start and forwarded to `MeasureOptions.detection_sigma` once Task 2 adds the field (until then the harness prints `detection sigma ignored: not implemented yet` on stderr and proceeds).

- [ ] **Step 1: Characterise the XISF disagreement with a fixture**

Write the failing test in `rustafits/src/formats/xisf.rs`'s test module. It builds an XISF file in a temp dir with a header that mirrors the external tool's masters — TWO `<Image>` elements are the first suspect (a thumbnail-like small `<Image>` before the real one), the second is a Float32 attachment whose `location` offset must be honoured exactly, the third is `pixelStorage` and `colorSpace` for a 3-plane RGB. The test asserts that `read_xisf_image` returns the LAST/largest full-geometry image's raw Float32 samples unchanged (write known values, read them back):

```rust
#[test]
fn reads_the_main_image_attachment_bit_exact_when_a_smaller_image_precedes_it() {
    // header: <Image geometry="4:2:1" ... location="attachment:A:32"/> (a 4x2 gray thumbnail)
    //         <Image geometry="8:4:3" sampleFormat="Float32" colorSpace="RGB" pixelStorage="Planar" location="attachment:B:384"/>
    // attachments: the thumbnail's 8 f32 zeros at A, then 96 f32 values 0.0, 0.001, … at B
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two-images.xisf");
    let main: Vec<f32> = (0..96).map(|i| i as f32 * 0.001).collect();
    write_two_image_fixture(&path, &main); // helper in the same test module: builds the 16-byte signature, the XML, pads, writes both attachments at the offsets the XML names
    let (meta, pixels) = read_xisf_image(&path).unwrap();
    assert_eq!((meta.width, meta.height, meta.channels), (8, 4, 3));
    match pixels {
        PixelData::Float32(v) => assert_eq!(v, main, "the main image's samples must come back bit-exact"),
        _ => panic!("Float32 expected"),
    }
}
```

Run: `cd rustafits && cargo test --lib formats::xisf::tests::reads_the_main_image_attachment_bit_exact_when_a_smaller_image_precedes_it`
Expected: FAIL (today `parse_xisf_xml` breaks on the first `<Image>` and reads the 4×2 thumbnail).

If this test PASSES on the first run, the first suspect is wrong: run the second characterisation before changing anything — convert one external file two ways and diff: `python3 docs/superpowers/research/scripts/weight_audit_compare.py --dump-first-image ~/Pictures/…/master/masterLight_…_mono_drizzle_2x.xisf /tmp/ref.bin` (the script's raw reader: signature, XML length, the first `<Image>`'s `location="attachment:pos:size"`, read `size` bytes at `pos`, no conversion) against `cargo run --release -p athenaeum-core --example weight_audit -- --dump-planes /tmp/ours.bin <same file>` (Step 3 adds `--dump-planes`, raw little-endian f32 planes); the first differing offset tells which attribute is mishandled (`byteOrder="big"` is the second suspect — XISF allows it and `convert_to_float32` must honour it; `bounds="lo:hi"` rescaling is the third — XISF says samples are stored in `[lo, hi]` and a reader must map them, which the current reader ignores; a `location="attachment:…"` with a `<Data>` embedded thumbnail is the fourth). Write the failing test for the one that differs instead of the two-images one, keep the same name pattern.

- [ ] **Step 2: Fix the reader minimally**

In `parse_xisf_xml`: iterate over EVERY `<Image>` element instead of breaking at the first; keep the one with the largest `width × height × channels` (ties → the last). Add `byte_order: XisfByteOrder` (`Little` default, `Big` when `byteOrder="big"`) to `XisfImageInfo`, honoured in `convert_to_float32` for every multi-byte sample format; parse `bounds="lo:hi"` (default `0:1`) and, when `(lo, hi) != (0, 1)` for a float format, map `v → (v − lo)/(hi − lo)` in `convert_to_float32`. Keep the permissive `allow_dangling_amp`. No other behaviour change.

Run: `cd rustafits && cargo test --lib formats::xisf`
Expected: PASS (the new test and the existing ones).

- [ ] **Step 3: The harness**

Create `crates/athenaeum-core/examples/weight_audit.rs`:

```rust
//! Dev harness (M4a Task 1): measure a list of calibrated frames — FITS via
//! `PlaneReader`, anything else via `astroimage::ImageConverter::read_raw` —
//! through the production estimator and print one JSON line per file with
//! every per-plane `ChannelMeasurement`, so per-frame quality terms can be
//! compared against an external per-frame log by stem
//! (`docs/superpowers/research/scripts/weight_audit_compare.py`).
//!
//! `cargo run --release -p athenaeum-core --example weight_audit -- \
//!     [--seed fast|full] [--sigma <k>] [--psf auto|moffat4] [--max-stars <n>] \
//!     [--out <file.jsonl>] [--dump-planes <file.bin>] <file>…`
use athenaeum_core::stacking::measure::{measure_plane_with_seeds, ChannelMeasurement, MeasureOptions, SeedSource};
use athenaeum_core::stacking::psf_signal::PsfModel;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

#[derive(Serialize)]
struct Line<'a> {
    stem: &'a str,
    width: usize,
    height: usize,
    channels: Vec<ChannelMeasurement>,
    duration_ms: u64,
}

fn read_planes(path: &Path) -> Result<(Vec<Vec<f32>>, usize, usize), String> {
    let is_fits = path.extension().map(|e| e.eq_ignore_ascii_case("fits") || e.eq_ignore_ascii_case("fit")).unwrap_or(false);
    if is_fits {
        let r = athenaeum_core::integration::plane_reader::PlaneReader::open(path).map_err(|e| e.to_string())?;
        let planes = (0..r.channels()).map(|p| r.read_plane(p)).collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        return Ok((planes, r.width(), r.height()));
    }
    let (meta, pixels) = astroimage::ImageConverter::read_raw(path).map_err(|e| e.to_string())?;
    let data: Vec<f32> = match pixels {
        astroimage::PixelData::Float32(v) => v,
        astroimage::PixelData::Uint16(v) => v.iter().map(|&u| u as f32 / 65535.0).collect(),
    };
    let n = meta.width * meta.height;
    Ok(((0..meta.channels).map(|c| data[c * n..(c + 1) * n].to_vec()).collect(), meta.width, meta.height))
}

fn main() {
    // parse args by hand (no clap): --seed, --sigma, --psf, --max-stars, --out, --dump-planes, then files
    // for each file: read_planes → for each plane measure_plane_with_seeds(plane, w, h, &opts, None, seed)
    // → serialize Line as one JSON line to --out (or stdout); errors go to stderr and the file is skipped
    // --dump-planes: write every plane as little-endian f32 to the named file and exit (Step 1's raw comparison)
}
```

Fill the argument parsing and the loop; time each file with `std::time::Instant`. `--sigma <k>` stores into `opts.detection_sigma` behind a `cfg`-free guard: until Task 2 adds the field, print `detection sigma ignored: not implemented yet` to stderr (the implementer of THIS task adds a `// TODO(M4a Task 2)`-free comment saying which field it will become — no TODO markers, say it plainly).

Run: `cargo run --release -p athenaeum-core --example weight_audit -- --out /tmp/wa.jsonl "<one external mono _c.xisf>" "<one external OSC _c_d.xisf>"`
Expected: two JSON lines, the OSC one with 3 channels; the `fwhmPx`/`starsFitted` of the mono line within 10 % of what `measure_probe` prints for a FITS conversion of the same file (Task 1's own consistency check — the reader fix is what makes them agree).

- [ ] **Step 4: The comparison script**

Create `docs/superpowers/research/scripts/weight_audit_compare.py` with three functions and a `main`: `parse_external_log(path) -> dict[stem, {"ch": {i: {tflux, tmean, mstar, nstar, fits, sigma}}}]` (the block after `Loading target calibration frame: <path>` / `Loading target file: <path>` — strip a trailing `_c` from the stem — collecting `ch N : TFlux = …, TMeanFlux = …, M* = …, N* = …, <n> PSF fits` and `ch N : sigma_n = …, <p>% pixels (<src>)` lines), `load_ours(jsonl) -> dict[stem, channels]`, and `report(ext, ours)`: for each group (mono = 1-channel stems, osc = 3-channel stems), per channel, per night (the stem's leading `YYYY-MM-DD`), the median ratio ours/external of `starsFitted/fits`, `tflux/tflux`, `tmeanFlux/tmean`, `mStar/mstar`, `nStar/nstar`, `noise/sigma`, then the per-channel Spearman of PSFSW (ours `psfSignalWeight` vs `5.326e-6·tflux·tmean/(9.0e6·sigma·mstar)` from the external terms), of `starsFitted` vs `fits`, and the top-20 overlap of the frame-mean normalized PSFSW (ours: mean over channels of `psfSignalWeight` divided by the group max; external: the same from the recomputed values). `--dump-first-image <xisf> <out.bin>` writes the first `<Image>`'s attachment bytes raw (Step 1). Print a final `PASS`/`MISS` line per ruling-R-M4a-2 target so Task 2 can grep it. stdlib only (`json`, `re`, `statistics`, `struct`).

Run: `python3 docs/superpowers/research/scripts/weight_audit_compare.py --log "$HOME/Pictures/Calibration Test/LDN1272-Output/logs/20260908121336.log" --ours /tmp/wa.jsonl`
Expected: a two-line table (one frame per group) and the Spearman lines reading `n/a (fewer than 3 frames)`.

- [ ] **Step 5: Measure the whole external set once (the Task 2 baseline)**

Run: `cargo run --release -p athenaeum-core --example weight_audit -- --out /tmp/wa-baseline.jsonl "$HOME/Pictures/Calibration Test/LDN1272-Output/calibrated/Light_BIN-1_6224x4168_"*"/*_c.xisf" "$HOME/Pictures/Calibration Test/LDN1272-Output/debayered/"*"/*_c_d.xisf"` (≈ 25–35 min at ≈ 5 s per frame; run it in the foreground, it is the deliverable's proof), then the compare script.
Expected: the compare script reproduces ruling R-M4a-1's numbers on identical pixels within a few percent (fits ratio 1.1–1.2 mono, 1.6–7.8 OSC; OSC fit-count Spearman negative; PSFSW Spearman 0.92 mono / 0.94 / 0.87 / 0.45 OSC R/G/B). Paste the table into the report; keep `/tmp/wa-baseline.jsonl` for Task 2 (copy it to `.superpowers/sdd/<plan>/wa-baseline.jsonl`).

- [ ] **Step 6: Commit** (rustafits first, then the gitlink + harness)

```bash
cd rustafits && git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -am "fix(xisf): read the main image of a multi-image header bit-exact; honour byteOrder and bounds" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
cd .. && git add rustafits crates/athenaeum-core/examples/weight_audit.rs docs/superpowers/research/scripts/weight_audit_compare.py crates/athenaeum-core/examples/measure_probe.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): weight-audit harness + external-log comparison; XISF reader fix (M4a Task 1)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 2: Noise-relative measurement seeds and the §1.4 fit acceptance

**Files:**
- Modify: `rustafits/src/analysis/adaptive_detection.rs` (`detect_stars_adaptive`, `star_levels`), `rustafits/src/analysis/mod.rs` (`ImageAnalyzer` builder + `run_fast_detection`)
- Modify: `crates/athenaeum-core/src/stacking/measure.rs` (`MeasureOptions`, `measure_plane_with_seeds`), `crates/athenaeum-core/src/stacking/psf_signal.rs` (`FitParams`, `fit_one`, `accept`), `crates/athenaeum-core/src/stacking/config.rs` (`MeasurementConfig`), `crates/athenaeum-core/src/stacking/run.rs` (`stage_measure`'s `MeasureOptions` construction — search `MeasureOptions {`), `crates/athenaeum-core/examples/weight_audit.rs` (`--sigma` goes live)
- Modify: `src/components/stacking/panels/MeasurePanel.tsx`, `src/types/stacking.ts` (regenerated), `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §9.2 (`measurement.detectionSigma`), `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`
- Test: `rustafits/src/analysis/adaptive_detection.rs` (`#[cfg(test)]`), `crates/athenaeum-core/src/stacking/psf_signal.rs` tests, `crates/athenaeum-core/src/stacking/measure.rs` tests, `crates/athenaeum-core/src/stacking/config.rs` tests

**Interfaces:**
- Consumes: Task 1's harness and baseline JSONL; `astroimage::ImageAnalyzer::detect_fast_data`.
- Produces: `ImageAnalyzer::with_detection_levels(DetectionLevels::NoiseRelative { k1: f32, k2: f32 })` (rustafits; `DetectionLevels::RankBudget` is today's behaviour and the default); `MeasureOptions.detection_sigma: f32` (default 5.0, `NoiseRelative { k1: detection_sigma, k2: detection_sigma / 2.0 }` — `k2` is the secondary/fainter level the adaptive scan retries at, half of the primary the way the rank budget's `24·max_stars` is four times the `6·max_stars`); `FitParams.{inner_margin: f64 = 0.15, region_growth_step_px: usize = 1, region_growth_min_drop: f64 = 0.01}`; `MeasurementConfig.detection_sigma: f64` (serde `detectionSigma`, default = the value Step 6 settles).

- [ ] **Step 1: rustafits — the detection-levels hook (failing test first)**

In `adaptive_detection.rs` add

```rust
/// How the two detection levels are chosen. `RankBudget` is the historical
/// rule (levels at the intensities below which `6·max_stars` and
/// `24·max_stars` brightest pixels lie — a fixed bright-pixel budget, blind
/// to sky brightness); `NoiseRelative` puts them at `background + k·noise`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DetectionLevels {
    RankBudget,
    NoiseRelative { k1: f32, k2: f32 },
}
```

and a `levels: DetectionLevels` parameter to `detect_stars_adaptive` (every existing caller passes `DetectionLevels::RankBudget`); inside, `let (l1, l2) = match levels { RankBudget => star_levels(lum, bg, noise, max_stars), NoiseRelative { k1, k2 } => (bg + k1 * noise, bg + k2 * noise) };` — nothing else changes. `ImageAnalyzer` gets `detection_levels: DetectionLevels` in its config (default `RankBudget`) and `pub fn with_detection_levels(mut self, levels: DetectionLevels) -> Self`; `run_fast_detection` forwards it.

Test (rustafits, `adaptive_detection.rs` tests): a synthetic 512×512 field with background 1000 ADU, noise 20 ADU, 200 stars of peak 300 ADU and 200 stars of peak 60 ADU: `NoiseRelative { k1: 5.0, k2: 2.5 }` (levels 1100 / 1050) finds ≥ 380 stars; `NoiseRelative { k1: 20.0, k2: 10.0 }` (levels 1400 / 1200) finds ≤ 220 (the faint half is below the levels); `RankBudget` with `max_stars = 24576` finds ≥ 380 (the budget is larger than the field). Use the crate's existing synthetic helpers if any (`grep -n "fn gaussian_field\|fn synth" rustafits/src/analysis/*.rs`), else a 20-line generator in the test.

Run: `cd rustafits && cargo test --lib analysis::adaptive_detection` → FAIL (no such variant) → implement → PASS. Commit inside `rustafits/` (`feat(analysis): caller-chosen detection levels — noise-relative or the rank budget`).

- [ ] **Step 2: `MeasureOptions.detection_sigma` and the seed path**

In `measure.rs`: `pub detection_sigma: f32` on `MeasureOptions` (default 5.0); in `measure_plane_with_seeds`'s `SeedSource::Fast` arm: `.with_detection_levels(DetectionLevels::NoiseRelative { k1: opts.detection_sigma, k2: opts.detection_sigma * 0.5 })`. Keep `.filter(|s| s.snr >= opts.min_snr && s.peak > 0.0)`. Log the seed count on the existing `"frame plane measured"` debug as the already-canonical `stars_detected`; add `detection_sigma` (f32) to that debug and to the dictionary.

Test (`measure.rs` tests): on the existing synthetic-field fixture the tests already use (`grep -n "fn measure_plane_finds" crates/athenaeum-core/src/stacking/measure.rs`), `detection_sigma = 5.0` fits ≥ the count the fixture pins today, `detection_sigma = 40.0` fits strictly fewer, and the PSFSW at 40.0 is within a factor 3 of the 5.0 value (the estimator's sensitivity to the population is bounded — this pins that the term scaling is not pathological).

Run: `cargo test -p athenaeum-core stacking::measure` → PASS.

- [ ] **Step 3: The §1.4 sampling region and inner-region acceptance**

In `psf_signal.rs`: `FitParams` gains `inner_margin: f64` (0.15), `region_growth_step_px: usize` (1), `region_growth_min_drop: f64` (0.01). `fit_one` replaces the fixed `stamp_radius(sigma0)` with the adaptive region: start at `r0 = max(stamp_radius(sigma0) / 2, 3)`, loop `{ r += step; median_r = median of the stamp's border ring? no — the region MEDIAN (all pixels in the (2r+1)² square); stop when median_{r} > (1 − min_drop)·median_{r−step} (it stopped dropping by ≥ 1 %) or r reaches stamp_radius(sigma0)·2 (clamped to 48) }`; the fit runs on the final square. `accept` adds: the fitted centre must lie inside the region shrunk by `inner_margin` per side (`|x0 − cx| ≤ (1 − 2·inner_margin)·r` and the same for y) — in ADDITION to the existing centroid tolerance and the `0.5·k·fwtm ≤ r` check. Existing tests keep passing (the fixtures' stars are centred); add one test: a seed placed `0.8·r` from a star's true centre (the fit walks to the star) is REJECTED by the inner-region rule and ACCEPTED when `inner_margin = 0.0`.

Run: `cargo test -p athenaeum-core stacking::psf_signal` → PASS.

- [ ] **Step 4: Config, run plumbing, panel**

`config.rs`: `MeasurementConfig.detection_sigma: f64` (`#[serde(default = "default_detection_sigma")]`, serde `detectionSigma`, ts-rs), spec §9.2 line `detectionSigma: 5.0 — star-detection threshold for the quality measurement in σ above the local background (noise-relative)`; the run's `MeasureOptions { … detection_sigma: cfg.measurement.detection_sigma as f32 }`; `weight_audit.rs` forwards `--sigma`. `MeasurePanel.tsx`: a numeric field "Detection threshold (σ)" beside "Max stars" using the panel's existing `NumberField` pattern (min 1, max 100, step 0.5, default badge). `stageSummary.ts` measure row appends ` · σ ${detectionSigma}`. Regenerate `src/types/stacking.ts`. Config-hash test: two configs differing only in `detectionSigma` hash differently (`config.rs` tests — follow the existing `config_hash_changes_when…` test).

Run: `cargo test -p athenaeum-core stacking::config && npx tsc --noEmit` → PASS.

- [ ] **Step 5: Calibrate on the external frames (the deliverable)**

Pick the iteration subset: every 4th frame of each night per group (`ls … | awk 'NR % 4 == 1'`, ≈ 95 frames, ≈ 8 min per run). Run the harness with `--sigma` ∈ {5, 8, 12, 16, 24, 32} and the compare script each time; pick the σ that (a) brings every per-night fits ratio into `[0.7, 1.4]` on both groups and (b) maximises the min per-channel PSFSW Spearman. If no σ satisfies (a) on OSC G/B while mono stays ≥ 0.9 (the bright night's 4–8× excess may be an ACCEPTANCE effect on undersampled stars, not a threshold effect): try `inner_margin` 0.15 → 0.25 and `min_snr` 5 → 10 at the best σ; record every run's four numbers in the report. Then ONE full run (all 371 frames) at the chosen settings; the compare script's PASS/MISS lines are the acceptance. Settle `default_detection_sigma()` to the chosen value.

Expected: PASS on every R-M4a-2 target; if a target is missed after the grid above, ruling R-M4a-3 applies — ship the best rule set, report the residual numbers, do not add a per-night scale.

- [ ] **Step 6: Commit**

```bash
git add rustafits crates/athenaeum-core/src/stacking/{measure,psf_signal,config,run}.rs crates/athenaeum-core/examples/weight_audit.rs src/components/stacking/panels/MeasurePanel.tsx src/components/stacking/stageSummary.ts src/types/stacking.ts docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md docs/superpowers/specs/2026-07-03-logging-overhaul-design.md
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): noise-relative measurement seeds and the adaptive-region fit acceptance (M4a Task 2, ruling R-M4a-1)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 3: Robust linear-fit rejection

**Files:**
- Modify: `crates/athenaeum-core/src/integration/combine.rs` (`reject_linear_fit`, new `fn medfit_line`, `pub const LINEAR_FIT_SIGMA_SCALE: f64`)
- Test: `crates/athenaeum-core/src/integration/combine.rs` tests

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub(crate) fn medfit_line(values: &[f64]) -> (f64, f64)` — `(a, b)` of the minimum-absolute-deviation line `y = a + b·i` over `values[i]`; `pub const LINEAR_FIT_SIGMA_SCALE: f64 = 1.0` (doc: empirical, calibrated by the M4a acceptance run — see ruling R-M4a-4); `reject_linear_fit`'s signature is unchanged.

- [ ] **Step 1: Failing tests**

Add to `combine.rs`'s test module:

```rust
#[test]
fn medfit_line_ignores_a_single_extreme_outlier_that_drags_least_squares() {
    // a clean ramp 10 + 0.5·i for i in 0..40, plus values[39] = 1000
    let mut v: Vec<f64> = (0..40).map(|i| 10.0 + 0.5 * i as f64).collect();
    v[39] = 1000.0;
    let (a, b) = medfit_line(&v);
    assert!((a - 10.0).abs() < 0.05 && (b - 0.5).abs() < 0.01, "medfit ({a}, {b}) must recover the ramp");
    // least squares on the same data does not: its slope is > 1.0 — the point of the test
}

#[test]
fn linear_fit_rejection_with_the_robust_line_rejects_the_contaminated_tail_ls_kept() {
    // 200 samples: N(0.1, 0.002) + 6 high outliers at 0.13..0.16 (a satellite trail's stack column)
    let mut vals = fixture_gaussian_stack(200, 0.1, 0.002, 12345);
    for (k, x) in vals.iter_mut().rev().take(6).enumerate() { *x = sample_from(0.13 + 0.005 * k as f32); }
    let (kept, _) = reject_linear_fit(&mut vals, 5.0, 3.5);
    assert!(kept <= 194, "all six outliers rejected at 5.0/3.5, kept {kept}");
}
```

(`fixture_gaussian_stack`/`sample_from` = the module's existing fixture helpers — read the tests above `reject_linear_fit`'s current tests first and reuse their names; the second test is the one that fails today because the least-squares line + inflated `adev` keep one or more of the six.)

Run: `cargo test -p athenaeum-core integration::combine::tests::medfit` → FAIL (no `medfit_line`).

- [ ] **Step 2: Implement**

`medfit_line`: least-squares `(a_ls, b_ls)` and `σ_b = sqrt(Σ(y − a − b·x)² / (n·Σ(x − x̄)²))` as the bracket scale; `f(b) = Σ x_i · sgn(y_i − median(y − b·x) − b·x_i)`; bracket `b1 = b_ls`, `b2 = b_ls + 3·σ_b·sgn(f(b1))` widening ×2 until `f(b1)·f(b2) ≤ 0` (cap 32 widenings; on failure return `(a_ls, b_ls)` — never panic on degenerate input), bisect to `|b2 − b1| < 1e-3·σ_b` or 60 iterations, `a = median(y − b·x)`. Then `reject_linear_fit` uses `(a, b) = medfit_line(&survivor values)` in place of the LS line each iteration, `s = LINEAR_FIT_SIGMA_SCALE * 2.0 * adev`, everything else verbatim. Keep the `T: Sample` generic; convert to `f64` once per iteration into a scratch `Vec<f64>` (allocation per pixel stack is not acceptable in the band loop — reuse: make the scratch a `thread_local!` or take it from the caller's existing per-worker scratch if one exists; read `combine_pixel`'s callers first).

Run: `cargo test -p athenaeum-core integration::combine` → PASS; the whole crate's pins: `cargo test -p athenaeum-core integration` → any byte-identical master pin that used linear fit with contaminated fixtures may move — inspect: a pin on a clean synthetic stack must NOT move (the robust line equals LS on symmetric noise within tolerance), a pin that encoded the LS under-rejection updates in this commit with a message line saying so.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/integration/combine.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(integration): linear-fit rejection on the minimum-absolute-deviation line, named dispersion scale (M4a Task 3, ruling R-M4a-4)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 4: Two-pass registration reference

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (`ReferenceConfig.two_pass`), `crates/athenaeum-core/src/stacking/weights.rs` (`two_pass_pick`), `crates/athenaeum-core/src/stacking/run.rs` (`stage_register` split into `register_group_pass` + the two-pass driver; `stacking_runs.reference_frame_id` update), `crates/athenaeum-core/src/stacking/plan.rs` (`compute_register_stale`), `crates/athenaeum-core/src/db/stacking.rs` (an `update_run_reference(conn, run_id, frame_id)` helper if none exists — check first), `crates/athenaeum-core/src/stacking/provenance.rs` (summary reference block gains `switched_from: Option<i64>`, `#[serde(default)]`)
- Modify: `src/components/stacking/panels/ReferencePanel.tsx`, `src/components/stacking/stageSummary.ts`, `src/components/stacking/ResultsPanel.tsx` (the reference line shows `switched from #<id>` when set), `src/types/stacking.ts` (regenerated), spec §4.4 + §9.2 (`reference.twoPass`), the logging dictionary
- Test: `weights.rs` tests (`two_pass_pick`), `run.rs` tests (drive `RunContext` the way the stage-6 driver's tests do — `grep -n "fn test_context" crates/athenaeum-core/src/stacking/run.rs`), `plan.rs` tests (stale check)

**Interfaces:**
- Consumes: `stacking::register::align::Alignment { rotation_deg, translation, scale, flipped }`, `stacking::weights::best_by_weight`, `RegisteredFrameOutcome`.
- Produces: `pub(crate) fn two_pass_pick(candidates: &[TwoPassCandidate], reference_idx: usize, diagonal_px: f64, min_gain_px: f64) -> Option<usize>` with `pub(crate) struct TwoPassCandidate { pub idx: usize, pub weight: f64, pub rotation_deg: f64, pub translation: (f64, f64) }` (the candidates are the reference group's included frames with a successful pass-1 alignment, the reference itself included with `(0.0, (0.0, 0.0))`; returns `Some(new_idx)` only when the switch rule of ruling R-M4a-5 fires); `pub const TWO_PASS_CANDIDATES: usize = 10; pub const TWO_PASS_MIN_GAIN_PX: f64 = 4.0;` in `weights.rs`; `ReferenceConfig.two_pass: bool` (serde `twoPass`, default true).

- [ ] **Step 1: `two_pass_pick` (failing test first)**

Tests in `weights.rs`: (a) 30 frames, rotations N(0, 0.05°), translations N(0, 3 px), the reference (idx 0) rotated 0.6° with translation (40, −25) — on a 6000×4000 frame the reference's corner displacement is ≈ 38 px + 47 px → `Some(k)` where `k` is the top-10-by-weight frame closest to the median; (b) the same set with the reference at 0.02° / (1, 1) → `None` (gain below 4 px); (c) the best candidate is chosen among the TOP-10 by weight only — a frame ranked 11th with a perfect transform is not picked; (d) a candidate with a non-finite rotation is skipped. The median is the coordinate-wise median of rotation and of each translation component over ALL candidates (not only the top 10).

Run: `cargo test -p athenaeum-core stacking::weights::tests::two_pass` → FAIL → implement (sort candidates by weight desc, take `TWO_PASS_CANDIDATES`, `d(c) = hypot(Δθ_rad · diagonal / 2, hypot(Δtx, Δty))`, switch iff `d(ref) − d(best) ≥ min_gain_px`) → PASS.

- [ ] **Step 2: The stage split**

In `run.rs` extract the body of the per-group loop of `stage_register` into `fn register_group_pass(rc: &mut RunContext, group: &IntegrationGroup, ref_stars: &ReferenceStars, reference_frame_id: i64, reference_hash: &str, by_frame: &HashMap<i64, RegistrationRecord>, force_fresh: bool, persist: bool, progress: &mut (usize, usize)) -> Result<Vec<PassResult>, RunError>` where `persist = false` skips `upsert_registration`, `write_registered_artifact` and the `entries[idx].registration = …` writes (it returns the alignments instead: `PassResult { idx, frame_id, outcome: Result<(f64, (f64, f64)), String> }`) and never consumes the cached-row branch (pass 1 always registers afresh — the cache belongs to the final reference). Then `stage_register`: resolve the reference as today; if `cfg.reference.mode == Auto && cfg.reference.two_pass`: pass 1 over the reference's group only (`persist = false`), build the candidates from the group's `measured` entries (weight = `normalized_mean`, the reference itself with identity), `two_pass_pick(…)`; on `Some(new_idx)`: set `rc.reference_frame_id`, `rc.reference_calibrated`, recompute `ref_stars`/`reference_hash`/`rc.reference_width/height`, `db::stacking::update_run_reference`, `rc.summary_reference_switched_from = Some(old)`, `tracing::info!(run_id, from = old, to = new, corner_px_before, corner_px_after, "reference switched by the two-pass pick")` (fields `from`/`to` (i64) and `corner_px_before`/`corner_px_after` (f64) go into the dictionary) and a run warning with the wording of ruling R-M4a-5; then the existing loop over every group with `persist = true`. The Register progress total counts both passes (`viable_total + reference group size` when two-pass is on). Timing: one `StageTiming { Register }` covering both passes.

Test (`run.rs`, `RunContext`-driven like the stage-6 tests): a synthetic set of 6 frames where frame A has the highest weight but a 0.8° rotation relative to the other five (generate the calibrated frames with `test_support::moffat_field` rotated; the existing registration tests show how) → after `stage_register`, `rc.reference_frame_id` is one of the five and `stacking_runs.reference_frame_id` matches; with `two_pass = false` it stays A; with `mode = Manual` pinned to A it stays A and no warning is added.

Run: `cargo test -p athenaeum-core stacking::run::tests::two_pass` → PASS.

- [ ] **Step 3: The stale check, the summary and the panel**

`plan.rs::compute_register_stale`: when `cfg.reference.two_pass && cfg.reference.mode == Auto`, the expected reference is the last DONE run's `stacking_runs.reference_frame_id` for this set if that row exists and the set's `registration_results` rows carry it, else today's rule. Test: after a switched run, a plan with the same config reports `register` NOT stale; with `two_pass` flipped off it IS stale (the config hash changed anyway). `provenance.rs`: `SummaryReference.switched_from: Option<i64>` (`#[serde(default)]`); `ResultsPanel.tsx` renders `· switched from #<id> (two-pass)` on the reference line when present. `ReferencePanel.tsx`: under the Auto radio a checkbox "Two-pass pick (re-choose the reference closest to the set's median transform)" bound to `reference.twoPass`, disabled in Manual mode; `stageSummary.ts` reference row: `Auto (highest-weight frame · two-pass)` / `Auto (highest-weight frame)` / `Manual selection`. Regenerate `src/types/stacking.ts`; spec §4.4 gains the two-pass paragraph (ruling R-M4a-5 verbatim) and §9.2 the `twoPass: true` line.

Run: `cargo test -p athenaeum-core stacking::plan && npx tsc --noEmit` → PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/athenaeum-core/src/stacking/{config,weights,run,plan,provenance}.rs crates/athenaeum-core/src/db/stacking.rs src/components/stacking/panels/ReferencePanel.tsx src/components/stacking/stageSummary.ts src/components/stacking/ResultsPanel.tsx src/types/stacking.ts docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md docs/superpowers/specs/2026-07-03-logging-overhaul-design.md
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): two-pass registration reference — dry first pass, median-transform re-pick (M4a Task 4, rulings R-M4a-5/6)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 5: Local-normalization hot spots

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/ln/grid.rs` (`LnScratch`, `LnGrid::evaluate_row_into`), `crates/athenaeum-core/src/stacking/ln/mod.rs` (`LnReferenceForDetection`)
- Test: `grid.rs` tests, `ln/mod.rs` tests

**Interfaces:**
- Consumes: `resample::Interpolation::BicubicBSpline::weights`.
- Produces: `LnScratch::for_grid` also builds `wx_table: Vec<[f32; 4]>` (one 4-tap weight row per `fx = i / stride`, `stride` entries) and `LnGrid::evaluate_row_into` reads `scratch.wx_table[x % stride]` instead of calling `weights(fx, …)` per pixel — same numbers; `LnReferenceForDetection.sanitized_planes: Vec<std::borrow::Cow<'a, [f32]>>` borrowing an all-finite reference plane and owning a sanitized copy otherwise (the struct gains a lifetime tied to the `LnReference`; `prepared` is built from `&*plane` as today).

- [ ] **Step 1: Pin, then change the evaluator**

Test (`grid.rs`): a random 7×5 node grid (stride 128, ref 800×600): `evaluate_row_into` for rows 0, 77, 599 with the table equals the pre-change implementation to `1e-6` — pin by keeping the old per-pixel path as a private `evaluate_row_reference` in the test module (copy the current loop verbatim there before editing). Then implement the table.

Run: `cargo test -p athenaeum-core stacking::ln::grid` → PASS.

- [ ] **Step 2: `Cow` planes**

`LnReferenceForDetection<'a>`; `build(reference: &'a LnReference, …)`: `Cow::Borrowed(plane.as_slice())` when every value is finite, else `Cow::Owned(…)`. Adjust the callers (`grep -rn "LnReferenceForDetection" crates/athenaeum-core/src`). Test: an all-finite reference yields `Cow::Borrowed` for every plane (`matches!(p, Cow::Borrowed(_))`), a plane with one NaN yields `Cow::Owned` with the NaN replaced by the location.

Run: `cargo test -p athenaeum-core stacking::ln` → PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/stacking/ln/grid.rs crates/athenaeum-core/src/stacking/ln/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "perf(stacking): LN row evaluator weight table; borrow all-finite reference planes (M4a Task 5)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 6: Docs — CLAUDE.md, spec §14, open-items pointers

**Files:**
- Modify: `CLAUDE.md` → Stacking (an "M4a — quality" paragraph after the M3 one: the seed-population root cause, the robust line, the two-pass pick, the harness, the acceptance pointer once Task 7 fills it), `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §14 (M4 split into M4a/M4b/M4c with this plan's file name), `docs/superpowers/open-items.md` (the M3 subsection's first three M4 items and the M2 subsection's line-fit item get a `→ M4a Task N` pointer; they are DELETED only by Task 7 when the acceptance run confirms).

- [ ] **Step 1: Write the three edits; no code.** Keep every number quoted from the rulings above. Commit `docs(stacking): M4a paragraph, spec §14 M4 split, open-items pointers`.

---

### Task 7 (controller-run): M4a acceptance run on LDN 1272

**Setup:** release `athenaeum-web` from the branch head (the M3 acceptance's `.superpowers/target-acc` + `dist-acc` recipe), dev catalog, set 109 (LDN 1272), reference Auto (two-pass on), LN on both sides, drizzle 2× (the run-12 config with `registration.distortion = off`, `measurement.detectionSigma` = Task 2's default), `cleanup keepAll`. Everything from Measure on re-runs (the config hash changed).

**Measurements (note `docs/superpowers/research/<date>-m4a-acceptance-run.md`):**

| Metric | Target | How |
| ---- | ---- | ---- |
| Per-channel PSFSW Spearman vs the external tool, both groups | ≥ 0.90 every channel (R-M4a-2) | `weight_audit_compare.py` on the run's `stacking_run_frames.metrics_json` exported to JSONL (a 10-line sqlite→jsonl script in the note) vs the external log |
| Fit-count ratio per night, per channel | inside [0.7, 1.4] | same script |
| Top-20 overlap of normalized weights per group | ≥ 14/20 | same script |
| Per-night mean normalized weight order (OSC) | the dark-sky nights outrank the bright one, as the external tool's do (0.73 / 0.82 / 0.65) | the note's per-night table |
| Rejected fraction at 5.0/3.5, both groups | 2.3–3.3 % | `stats_json.rejected_low_fraction + rejected_high_fraction`; ONE calibration round of `LINEAR_FIT_SIGMA_SCALE` allowed (ruling R-M4a-4), re-run from Integrate |
| Two-pass pick | the pick is logged; on this set the mono reference either stays `_0077`/`_0073`-class or switches with a stated corner gain ≥ 4 px; the master's corner coverage (weight map min at the corners) ≥ run 12's 0.0375 | run warnings + weight-map corners |
| Masters vs the external tool | level and background shape as in the M3 note (OSC 0.00134/0.00290/0.00233 ± 5 %, corners ± 0.02) — the weights changed, so re-verify | `fitsdiff.py` / the M3 scripts |
| Master FWHM / noise, both groups | within ± 5 % of run 12's (2.688 mono; noise 1.86e-05) | results card + `measure_probe` |
| OSC drizzle G/B FWHM ratio | within ± 5 % of the external's 0.747 / 0.741 (was 0.83 / 0.82) — if still outside: the star-by-star table (`samestar.py`) across three magnitude bins goes into the note and the item stays in open-items with the measured residual | `measure_probe` on the drizzled outputs (FITS path) |
| Measure stage time | ≤ 1.5× M3's measure time (the noise-relative seeds fit fewer stars — expect faster) | `stages[measure]` |
| LN stage time | recorded; ≤ M2's 28 min on the same machine | `stages[normalize]` |
| Click-through | Measure panel's σ field, Reference panel's two-pass checkbox, the results reference line | the LAN browser recipe |

**Docs:** the acceptance note; `docs/superpowers/open-items.md` (delete the items the run closes, keep the residuals with numbers, add the owed owner click-throughs and the Windows/Linux runs); `CLAUDE.md` Stacking (the M4a acceptance sentence); the memory file.

---

## Self-review (done while writing)

- **Spec coverage:** §14 M4 quality items — PSF weight audit (Tasks 1–2), rejection calibration (Task 3, Task 7 round), two-pass reference (Task 4), LN perf (Task 5), XISF probe (Task 1), OSC drizzle residual (Task 7); §15 pixel-scale warning → M4b by ruling R-M4a-7; TPS/ESD/RCR/min-max/large-scale/Bayer drizzle/XISF output/cataloging/presets → M4c; mixed pixel scales → M4b.
- **Placeholders:** the only open values are the two the plan says are empirical by design — `default_detection_sigma()` (Task 2 Step 5 settles it from the harness grid) and `LINEAR_FIT_SIGMA_SCALE` (Task 7's single calibration round) — both with a stated procedure and acceptance band.
- **Type consistency:** `DetectionLevels::NoiseRelative { k1, k2 }` (rustafits) ↔ `MeasureOptions.detection_sigma` (`k1 = σ, k2 = σ/2`) ↔ `MeasurementConfig.detection_sigma` (serde `detectionSigma`) ↔ `--sigma`; `TwoPassCandidate`/`two_pass_pick`/`TWO_PASS_CANDIDATES`/`TWO_PASS_MIN_GAIN_PX` in `weights.rs`, consumed by `run.rs`; `ReferenceConfig.two_pass` (serde `twoPass`) read by `run.rs` and `plan.rs`; `LnScratch.wx_table` private to `grid.rs`.
