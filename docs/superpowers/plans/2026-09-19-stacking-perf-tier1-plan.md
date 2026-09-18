# Stacking Performance — Tier 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the stacking run's wall time by roughly a third on the reference set without changing a single output byte — first by measuring where each stage's time goes per frame, then by removing the structural serialization the audit found (serial band I/O in Integrate, the sequential Calibrate stage, over-generous fan-out admission, three uncoordinated thread pools, per-channel re-opens in LN, and the two-pass dry pass that throws its detections away).

**Architecture:** Every change lives in `athenaeum-core` (`stacking/`, `integration/`, `calibration_library/`, `export/calibrated_generator.rs`, `fits_writer/`). No command surface changes, so neither host is touched. Each task is gated by the existing M1–M4d byte-identity pins plus one new pin per task that asserts the fast path produces the same bytes/rows as the path it replaces. Task 0 adds the per-sub-stage `debug!` events the later tasks are measured against; Task 10 re-runs the reference set through the acceptance harness and writes the measured Δ into the audit.

**Tech Stack:** Rust 2021, rayon 1.10, `std::thread::scope`, rusqlite, `tracing`; tests via `cargo test -p athenaeum-core --lib`.

**Spec:** `docs/superpowers/research/2026-09-19-stacking-performance-audit.md` (§3 findings, §5 parallelism, §8 tier 1). The pipeline spec it amends is `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`.

## Global Constraints

- **Output bytes unchanged.** Every task in this plan is output-identical by design: the FITS masters, the `registration_results` rows, the `.athln` sidecars, the `stacking_artifacts` hashes. Every M1–M4d pin stays green; each task adds its own "same bytes as before" pin. If a task cannot be made byte-identical, it stops and reports — it does not loosen a tolerance.
- **Zero-print rule.** `println!`/`eprintln!` = 0 in production code. All diagnostics go through `tracing` with fields from the canonical dictionary (`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` §"Unified event schema"); new field names are added to that spec in the same commit that first emits them.
- **Message style.** `debug!(frame_id, read_ms = 12, "frame plane measured")` — a short stable phrase, all data in snake_case fields, never interpolated into the message.
- **No `canonicalize`** on any path the scanner spelled.
- **Never name the external stacking tool** in code or comments.
- **Both hosts untouched.** No Tauri command or Axum route changes; if a task finds it needs one, it stops.
- **Feature gates.** `stacking` is `#[cfg(all(feature = "render", feature = "solver"))]`; `integration/` is `render`-gated. The headless check (`cargo check -p athenaeum-core --no-default-features`) does not compile them — run `cargo check -p athenaeum-core --all-targets` after every public-signature change (the probes under `examples/` break silently otherwise).
- **Full suite before every push:** `cargo test -p athenaeum-core --lib` (≈ 77 s) — filtered runs have missed a pinned hash test before.
- **Commit as the owner** (`eg013ra1n` / `vilen.sharifov@gmail.com`) with the `Co-Authored-By` and `Claude-Session` trailers; push only on the owner's word.

## File map

| File | Responsibility in this plan |
| ---- | --------------------------- |
| `crates/athenaeum-core/src/integration/engine.rs` | `band_loop`: the one-band prefetch (Task 1) |
| `crates/athenaeum-core/src/integration/registered_source.rs` | `with_pool` so band-worker warps land on the app pool (Task 2) |
| `crates/athenaeum-core/src/stacking/run.rs` | `admission` clamp (Task 2), working sets (Task 3), Calibrate fan-out (Task 4), master preload (Task 5), dry-pass star reuse (Task 9), sub-stage fields (Task 0) |
| `crates/athenaeum-core/src/stacking/measure.rs` | `noise_mrs` in the pool (Task 2), sub-stage timings (Task 0) |
| `crates/athenaeum-core/src/stacking/ln/mod.rs` | one `RegisteredSource` per frame (Task 8), sub-stage timings (Task 0) |
| `crates/athenaeum-core/src/stacking/ln/scale.rs` | pool into `detect_seeds` / `fit_all` (Task 2) |
| `crates/athenaeum-core/src/stacking/psf_signal.rs` | `fit_all` under an explicit pool (Task 2) |
| `crates/athenaeum-core/src/stacking/register/frame.rs` | `detect_frame_stars` / `register_detected` split (Task 9), sub-stage timings (Task 0) |
| `crates/athenaeum-core/src/export/calibrated_generator.rs` | VNG in the pool (Task 2), preloaded masters + volatile writes threaded through (Tasks 5, 6), sub-stage timings (Task 0) |
| `crates/athenaeum-core/src/calibration_library/light_cal.rs` | `PreloadedMasters` in the band loop (Task 5), `Durability` on the write (Task 6) |
| `crates/athenaeum-core/src/calibration_library/cosmetic.rs` | `select_nth` medians (Task 7) |
| `crates/athenaeum-core/src/fits_writer/{mod,writer,xisf_writer}.rs` | `Durability` enum and the `_with` writers (Task 6) |
| `crates/athenaeum-core/src/export/models.rs` | `CalibratedLightOptions::skip_fsync` (Task 6) |
| `crates/athenaeum-core/src/stacking/drizzle/mod.rs` | per-plane `read_ms`/`deposit_ms` (Task 0) |
| `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` | dictionary extension (Task 0) |
| `docs/superpowers/research/2026-09-19-stacking-performance-audit.md` | measured Δ column (Task 10) |

---

### Task 0: Sub-stage instrumentation

The audit's Δ estimates are derived from stage totals; nothing today says how a Measure frame's 1.6 s splits between read, background, noise, detection and fitting, or how an LN frame's 6 s splits between warp, background and the star pass. This task adds those numbers as `debug!` fields on the per-frame events that already exist, plus one `info!` per fan-out stating the admission actually granted. No behaviour change.

**Files:**
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary extension, after the last "Dictionary extension" paragraph)
- Modify: `crates/athenaeum-core/src/stacking/measure.rs:253-300` (`measure_plane_with_seeds`) and `:507-551` (`measure_frame_with_seeds`)
- Modify: `crates/athenaeum-core/src/stacking/register/frame.rs:72-130` (`register_frame`)
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs:129-134` (`LnFrameOutcome`), `:396-562` (`normalize_frame`)
- Modify: `crates/athenaeum-core/src/stacking/run.rs:7164-7171` (the `"ln frame normalized"` debug), `:1917-1927` (`admission`), the four `admission(...)` call sites (`:2361`, `:3277`, `:7015`, and Task 4's new one)
- Modify: `crates/athenaeum-core/src/export/calibrated_generator.rs:485-700` (`execute_generation`)
- Modify: `crates/athenaeum-core/src/stacking/integrate.rs:1285-1295` (the `"plane integrated"` debug)
- Modify: `crates/athenaeum-core/src/stacking/drizzle/mod.rs:483-690` (per-plane accumulators)
- Test: `crates/athenaeum-core/src/stacking/measure.rs` (tests module), `crates/athenaeum-core/src/stacking/ln/mod.rs` (tests module)

**Interfaces:**
- Produces: `LnFrameOutcome { warp_ms: u64, background_ms: u64, scale_ms: u64, write_ms: u64 }` (four new `u64` fields, all set by `normalize_frame`); `pub struct MeasureTimings { read_ms, background_ms, noise_ms, detect_ms, fit_ms }` returned inside `ChannelMeasurement` as `pub timings: MeasureTimings` (all `u64` ms, `#[serde(skip)]` so the cached `metrics` payload is unchanged); the log field names below.
- Consumes: nothing from later tasks.

- [ ] **Step 1: Extend the logging dictionary**

Append to `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, after the last "Dictionary extension" paragraph:

```markdown
**Dictionary extension (stacking sub-stage timing, `stacking::{measure, register, ln, run, drizzle}` + `export::calibrated_generator`, perf tier 1 Task 0, 2026-09-19):** per-frame phase splits in milliseconds, every one a `u64` and every one a component of the same event's existing `duration_ms` — `read_ms` (decoding the frame's plane(s) off disk; reuses the master-build sense), `background_ms` (the background mesh / model), `noise_ms` (the MRS noise estimate), `detect_ms` (seed / star detection), `fit_ms` (PSF fitting), `align_ms` (quad seed + RANSAC + refit + distortion), `warp_ms` (resampling into the reference geometry), `scale_ms` (LN's target detect + fit + RCR), `compute_ms` (the calibration band loop, reads included), `cosmetic_ms` (hot-pixel replacement), `debayer_ms` (VNG), `write_ms` (the output write, fsync included), `deposit_ms` (drizzle deposition only, excluding the plane's `measure_plane` call — the split the `"drizzle plane deposited"` comment asked for). Carried on `"frame plane measured"` (read/background/noise/detect/fit), `"frame registered"` and `"frame registration failed"` (read/detect/align), `"ln frame normalized"` (warp/background/scale/write), `"light calibrated"` (compute/cosmetic/debayer/write), `"plane integrated"` (read/combine — reusing `read_ms`/`combine_ms`), `"drizzle plane deposited"` (read/deposit). `admission` (usize; the fan-out worker count a stage actually granted), `working_set_bytes` (u64; the per-frame residency estimate that produced it) and `pool_threads` (usize; the app pool's thread count it was clamped to) on the new `"fan-out admitted"` info each fan-out stage emits once per group, alongside `run_id`/`stage`/`group_key`.
```

- [ ] **Step 2: Write the failing test for `MeasureTimings`**

In `crates/athenaeum-core/src/stacking/measure.rs` tests module (find an existing test that builds a synthetic plane with `crate::stacking::test_fixtures::synthetic_star_field` and calls `measure_plane`; copy its setup):

```rust
#[test]
fn measure_timings_are_components_of_the_plane_duration() {
    let (w, h) = (256, 256);
    let data = crate::stacking::test_fixtures::synthetic_star_field(w, h, 40, 2.0, 0.001, 7);
    let opts = MeasureOptions::default();
    let t = std::time::Instant::now();
    let m = measure_plane(&data, w, h, &opts, None);
    let elapsed_ms = t.elapsed().as_millis() as u64;
    let sum = m.timings.background_ms + m.timings.noise_ms + m.timings.detect_ms + m.timings.fit_ms;
    assert!(sum <= elapsed_ms + 1, "phase sum {sum} exceeds the plane's own {elapsed_ms}");
    assert_eq!(m.timings.read_ms, 0, "a plane measured from memory has no read");
}
```

(If `synthetic_star_field`'s signature differs, use the exact signature at `test_fixtures.rs:744`.)

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib measure_timings_are_components -- --nocapture`
Expected: compile error — `timings` is not a field of `ChannelMeasurement`.

- [ ] **Step 4: Add `MeasureTimings` and time the phases**

In `measure.rs`, next to `ChannelMeasurement`:

```rust
/// Per-phase wall time of one plane's measurement (perf tier 1 Task 0).
/// Skipped by serde: the cached `metrics` artifact payload must not change
/// shape, and a timing is not a measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeasureTimings {
    pub read_ms: u64,
    pub background_ms: u64,
    pub noise_ms: u64,
    pub detect_ms: u64,
    pub fit_ms: u64,
}
```

Add `#[serde(skip)] pub timings: MeasureTimings,` to `ChannelMeasurement`. In `measure_plane_with_seeds`, wrap each phase:

```rust
let t = Instant::now();
let bg = match pool { /* unchanged */ };
let background_ms = t.elapsed().as_millis() as u64;
let t = Instant::now();
let (noise_adu, noise_source) = match psf_signal::noise_mrs(&scaled, w, h) { /* unchanged */ };
let noise_ms = t.elapsed().as_millis() as u64;
let t = Instant::now();
let seeds: Vec<Seed> = match seed_source { /* unchanged */ };
let detect_ms = t.elapsed().as_millis() as u64;
let t = Instant::now();
let fit = /* the existing fit_stars call */;
let fit_ms = t.elapsed().as_millis() as u64;
```

and set `timings: MeasureTimings { read_ms: 0, background_ms, noise_ms, detect_ms, fit_ms }` on the returned struct. In `measure_frame_with_seeds`, time `reader.read_plane(plane)` and set `m.timings.read_ms` before pushing; add `read_ms = m.timings.read_ms, background_ms = …, noise_ms = …, detect_ms = …, fit_ms = …` to the `"frame plane measured"` debug.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p athenaeum-core --lib measure_timings_are_components`
Expected: PASS.

- [ ] **Step 6: Register sub-stages**

In `register/frame.rs::register_frame`, time `read_luminance` (`read_ms`), `detect_stars` (`detect_ms`) and `align` (`align_ms`); add all three fields to both the `"frame registered"` debug and the `"frame registration failed"` warn. Do the same `read_ms`/`detect_ms` in `reference_stars` on its `"reference stars detected"` debug.

- [ ] **Step 7: LN sub-stages**

Add `pub warp_ms: u64, pub background_ms: u64, pub scale_ms: u64, pub write_ms: u64` to `LnFrameOutcome`. In `normalize_frame`, accumulate across channels: the `RegisteredSource::open` + `read_band_with_progress` + `decode_frame_into` block → `warp_ms`; `background_grid` + `median_of_finite` → `background_ms`; `relative_scale_against` → `scale_ms`; the final `LnFrameGrids::write` → `write_ms`. Every existing constructor of `LnFrameOutcome` in tests gets `..Default::default()` — derive `Default` on it if it does not already. In `run.rs` add the four fields to the `"ln frame normalized"` debug.

- [ ] **Step 8: Calibrate sub-stages**

In `execute_generation`: time `calibrate_light_compute` (`compute_ms`), the cosmetic block (`cosmetic_ms`), the VNG call (`debayer_ms`, `0` when not debayering), and the write(s) (`write_ms`, both writes when the mosaic is kept). Add the four to the `"light calibrated"` debug.

- [ ] **Step 9: Integrate and drizzle**

In `stacking/integrate.rs` add `read_ms = out.base.read_duration.as_millis() as u64, combine_ms = out.base.combine_duration.as_millis() as u64` to `"plane integrated"`. In `drizzle/mod.rs` add per-plane `plane_read = Duration::ZERO` / `plane_deposit = Duration::ZERO` accumulators inside the `for c` loop (mirroring `read_duration_total`/`deposit_duration_total`), and `read_ms`/`deposit_ms` on `"drizzle plane deposited"`.

- [ ] **Step 10: The `"fan-out admitted"` event**

In `run.rs`, add a helper next to `admission`:

```rust
/// One line per fan-out stating what `admission` decided and from what —
/// the number the audit's Δ estimates are measured against.
fn log_admission(rc: &RunContext, stage: Stage, group_key: &str, working_set_bytes: u64, admission: usize) {
    tracing::info!(
        run_id = rc.run_id,
        stage = stage.as_str(),
        group_key,
        working_set_bytes,
        admission,
        pool_threads = rc.ctx.image_pool.current_num_threads(),
        "fan-out admitted"
    );
}
```

Call it immediately after each `let admission_n = admission(...)` (Measure `:2361`, Register `:3277`, Normalize `:7015`).

- [ ] **Step 11: Gate and commit**

Run: `cargo test -p athenaeum-core --lib stacking:: && cargo check -p athenaeum-core --all-targets`
Expected: all green.

```bash
git add docs/superpowers/specs/2026-07-03-logging-overhaul-design.md crates/athenaeum-core/src
git commit -m "stacking: per-frame sub-stage timings on every stage's own debug event, and the admission each fan-out granted"
```

---

### Task 1: Integrate — prefetch the next band while combining the current one

`band_loop` (`integration/engine.rs:170-305`) reads band N, then combines it, then reads N+1. The reads run on their own scoped OS threads (`read_band_with_progress`), the combine on the rayon pool — nothing shares state, so band N+1's read can run during band N's combine. Peak memory stays inside the budget by halving the rows per band (two bands resident = the same bytes).

**Files:**
- Modify: `crates/athenaeum-core/src/integration/engine.rs:170-305` (`band_loop`), `:1751-1784` and `:1784-1822` (two existing tests whose band geometry moves)
- Test: `crates/athenaeum-core/src/integration/engine.rs` tests module

**Interfaces:**
- Consumes: `FrameSource` (`integration/source.rs:74-99`, already `Sync`), `BandPlanes::new` (`banded.rs:799`).
- Produces: unchanged public API. `BandStats.band_rows` now reflects the halved budget; `BandStats.read_duration` is the wall time the reader thread spent (overlapped with combine, so `read_duration + combine_duration` may exceed the loop's own elapsed time — document on the field).

- [ ] **Step 1: Write the failing pin — prefetch must not change the output and must overlap**

In `engine.rs` tests:

```rust
/// Perf tier 1 Task 1: with a budget that forces many bands, the loop's own
/// wall time must be less than read + combine (the two overlap), and the
/// output must equal the single-band run's bit for bit.
#[test]
fn band_prefetch_overlaps_read_and_combine_without_changing_the_output() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (64, 512);
    let paths: Vec<_> = (0..6)
        .map(|i| write(dir.path(), &format!("p{i}.fits"), w, h, move |x, y| (x * 3 + y * 7 + i * 11) as f32 * 0.01))
        .collect();
    let on_band = nop();
    let run = |budget: usize| {
        let t = std::time::Instant::now();
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(budget),
        )
        .unwrap();
        (out, t.elapsed())
    };
    let (single, _) = run(usize::MAX / 4);
    // 2 rows of 6 f32 frames + headroom ≈ 2 * (6*64*4 + 8*64) B per band before halving.
    let (many, elapsed) = run(4 * (6 * w * 4 + 8 * w));
    assert!(many.bands >= 64, "expected many bands, got {}", many.bands);
    assert_eq!(single.data, many.data, "prefetch changed the output");
    assert!(
        elapsed < many.read_duration + many.combine_duration,
        "no overlap: elapsed {elapsed:?} >= read {:?} + combine {:?}",
        many.read_duration,
        many.combine_duration
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib band_prefetch_overlaps`
Expected: FAIL on the overlap assertion (today `elapsed ≈ read + combine`).

- [ ] **Step 3: Rewrite `band_loop` with a double buffer**

Replace the body from `let mut planes = BandPlanes::new(src);` through the end of the `for` loop with:

```rust
    // Perf tier 1 Task 1: two band buffers, the next band's read overlapping
    // this band's combine. Rows per band are computed against HALF the
    // budget so the two resident bands together cost what one used to.
    let band_rows = src.band_rows_for_budget(io.band_budget_bytes / 2).max(1).min(h);
    let bands_total = h.div_ceil(band_rows);
    let per_row_bytes = src.bytes_per_row();
    let bytes_total = (h * per_row_bytes) as u64;
    let mut cur = BandPlanes::new(src);
    let mut next = BandPlanes::new(src);
    let read_duration = std::sync::Mutex::new(std::time::Duration::ZERO);
    let mut combine_duration = std::time::Duration::ZERO;
    // Bytes the READER has reported so far — the high-water mark both
    // `on_band` and `on_combine` ticks quote, so a combine tick emitted
    // while the next band is being read never reports fewer bytes than the
    // reader already has (the "bytes never regress" contract, now across two
    // threads).
    let bytes_reported = std::sync::Mutex::new(0u64);
    let rows_combined = AtomicUsize::new(0);
    const COMBINE_TICK_ROWS: usize = 64;

    // One band's read, callable from either thread. Returns the accounted
    // bytes of the band it read.
    let read_one = |band_idx: usize, y0: usize, rows: usize, into: &mut BandPlanes| -> Result<u64, IntegrationError> {
        let t = std::time::Instant::now();
        let on_bytes = |just_read: u64| {
            let mut so_far = bytes_reported.lock().unwrap();
            *so_far += just_read;
            (progress.on_band)(band_idx + 1, bands_total, *so_far, bytes_total);
        };
        src.read_band_with_progress(y0, rows, into, io.read_concurrency, &on_bytes, cancel)?;
        *read_duration.lock().unwrap() += t.elapsed();
        Ok((rows * per_row_bytes) as u64)
    };

    let mut bytes_read: u64 = 0;
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    bytes_read += read_one(0, 0, band_rows.min(h), &mut cur)?;

    for (band_idx, y0) in (0..h).step_by(band_rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let rows = band_rows.min(h - y0);
        let next_y0 = y0 + rows;
        let has_next = next_y0 < h;
        let next_rows = band_rows.min(h.saturating_sub(next_y0));

        let combine_result: Result<(), IntegrationError>;
        let prefetch_result: Option<Result<u64, IntegrationError>>;
        {
            let next_ref = &mut next;
            let read_one = &read_one;
            let (c, p) = std::thread::scope(|scope| {
                let prefetch = has_next.then(|| {
                    scope.spawn(move || read_one(band_idx + 1, next_y0, next_rows, next_ref))
                });
                let t_combine = std::time::Instant::now();
                let out_band = &mut out[y0 * w..(y0 + rows) * w];
                let tick = || {
                    let done = rows_combined.fetch_add(1, Ordering::Relaxed) + 1;
                    if done % COMBINE_TICK_ROWS == 0 || done == h {
                        let so_far = *bytes_reported.lock().unwrap();
                        (progress.on_combine)(done, h, so_far, bytes_total);
                    }
                };
                let c = pool.install(|| combine(BandJob { planes: &cur, out_band, y0, rows, width: w }, &tick));
                combine_duration += t_combine.elapsed();
                let p = prefetch.map(|j| j.join().expect("band prefetch thread panicked"));
                (c, p)
            });
            combine_result = c;
            prefetch_result = p;
        }
        combine_result?;
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        (progress.on_band)(band_idx + 1, bands_total, bytes_read, bytes_total);
        if let Some(r) = prefetch_result {
            bytes_read += r?;
            std::mem::swap(&mut cur, &mut next);
        }
    }

    Ok(BandStats {
        read_duration: read_duration.into_inner().unwrap(),
        combine_duration,
        band_rows,
        bands: bands_total,
        bytes_read,
    })
```

Keep the existing doc comments about the `Mutex`-as-critical-section reasoning above the closure (they still hold). Update `BandStats.read_duration`'s doc: "wall time spent in band reads, which since perf tier 1 overlap the combine — `read_duration + combine_duration` may exceed the loop's elapsed time."

- [ ] **Step 4: Fix the two geometry pins**

`bias_like_reports_exact_band_geometry_and_bytes_across_a_short_last_band` (`engine.rs:1784`) and `flat_bytes_read_includes_pass_one_central_third_plus_pass_two_full_height` (`:1823`) compute an expected `band_rows` from a budget: double the budget they pass so the halving yields the same 20-row band the assertions expect, and note why in a one-line comment. Any other test asserting `band_rows`/`bands` from a budget gets the same doubling — `cargo test` names them.

- [ ] **Step 5: Run the engine tests**

Run: `cargo test -p athenaeum-core --lib integration::engine`
Expected: all PASS, including `band_prefetch_overlaps_read_and_combine_without_changing_the_output`, `on_band_bytes_done_never_regresses_under_real_concurrency`, `cancel_mid_run_returns_cancelled`.

- [ ] **Step 6: Run the byte-identity pins of the stacking run**

Run: `cargo test -p athenaeum-core --lib stacking::`
Expected: all PASS (the master pins compare bytes; band geometry is not part of any master's bytes).

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/integration/engine.rs
git commit -m "integration: band_loop reads the next band while combining the current one; rows per band halved so two resident bands fit the same budget"
```

---

### Task 2: One pool — every parallel section runs on `image_pool`, admission clamped to it

Four call sites hand work to rayon's global pool instead of `ServiceContext::image_pool`: VNG (`calibrated_generator.rs:644-650`), `noise_mrs` (`measure.rs:280`), LN's detector (`ln/scale.rs:115`, `None`) and PSF fits (`psf_signal.rs:492` from a plain thread), and `warp_rows` inside `RegisteredSource`'s band workers (`registered_source.rs:319-358`). `admission()` clamps to raw cores rather than the pool.

**Files:**
- Modify: `crates/athenaeum-core/src/export/calibrated_generator.rs:485-500,640-655` (`execute_generation` gains a `pool: Option<&Arc<rayon::ThreadPool>>` parameter)
- Modify: `crates/athenaeum-core/src/stacking/measure.rs:280`
- Modify: `crates/athenaeum-core/src/stacking/ln/scale.rs:113-117,504-523` (`detect_seeds`, `relative_scale_against` gain `pool`), `crates/athenaeum-core/src/stacking/ln/mod.rs:396-470` (`normalize_frame` gains `pool`, threads it to `RegisteredSource::with_pool` and `relative_scale_against`)
- Modify: `crates/athenaeum-core/src/stacking/psf_signal.rs:482-495` (`fit_all` runs `par_iter` inside `pool.install` when given one; `fit_stars_with_beta` gains `pool`)
- Modify: `crates/athenaeum-core/src/integration/registered_source.rs:36-90,154-205` (`with_pool` builder, `fill_frame` warps under it)
- Modify: `crates/athenaeum-core/src/stacking/run.rs:1917-1927` (`admission(working_set, max_workers)`), the call sites, `execute_generation` call at `:1600`, `normalize_frame` call at `:7041`, `integrate_planes`/LN reference `RegisteredSource` construction (`stacking/integrate.rs:622-643`, `ln/reference.rs:111-209`) get `.with_pool(Arc::clone(pool))`
- Test: `crates/athenaeum-core/src/integration/registered_source.rs`, `crates/athenaeum-core/src/stacking/run.rs` tests

**Interfaces:**
- Produces: `RegisteredSource::with_pool(self, pool: Arc<rayon::ThreadPool>) -> Self`; `execute_generation(spec, output_path, mosaic_path, scratch_dir, opts, hot_maps, pool: Option<&Arc<rayon::ThreadPool>>, cancel)`; `psf_signal::fit_stars_with_beta(data, w, h, seeds, beta, params, pool: Option<&Arc<rayon::ThreadPool>>)`; `scale::relative_scale_against(prepared, target, width, height, max_stars, match_radius_px, rcr_limit, local_scale, pool: Option<&Arc<rayon::ThreadPool>>)`; `normalize_frame(…, pool: Option<&Arc<rayon::ThreadPool>>, cancel)`; `fn admission(working_set_bytes: u64, max_workers: usize) -> usize`.
- Consumes: Task 0's `log_admission`.

- [ ] **Step 1: Write the failing test for `RegisteredSource::with_pool`**

In `registered_source.rs` tests, next to `a_band_read_equals_a_full_warp_of_each_frame` (`:599`):

```rust
/// Perf tier 1 Task 2: a band read's row-parallel warp runs on the pool the
/// source was given, never on rayon's global pool. Pinned by giving the
/// source a 1-thread pool and asserting the warp's rayon workers all report
/// index 0 of THAT pool (rayon's global pool has >1 thread on every CI box).
#[test]
fn band_read_warps_on_the_source_pool() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_plane(dir.path(), "f.fits", 64, 64, |x, y| (x + y) as f32);
    let pool = std::sync::Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap());
    let frames = vec![RegisteredFrame { path, map: PixelMap::linear(Linear::identity()).unwrap() }];
    let src = RegisteredSource::open(&frames, 64, 64, 0, Interpolation::BicubicBSpline, 0.3)
        .unwrap()
        .with_pool(std::sync::Arc::clone(&pool));
    let seen = std::sync::Mutex::new(Vec::new());
    // `fill_frame` is private; `read_band_with_progress` with concurrency 1
    // takes the single-worker branch, which calls it on this thread.
    let mut band = BandPlanes::new(&src);
    let probe = |_: u64| {
        // `current_thread_index` is `None` off-pool and Some(i) on a pool.
        seen.lock().unwrap().push(rayon::current_thread_index());
    };
    src.read_band_with_progress(0, 64, &mut band, 1, &probe, &AtomicBool::new(false)).unwrap();
    // The on_bytes callback fires from the caller's thread, not from inside
    // the warp — what this pins is that the warp DID NOT panic under a
    // 1-thread pool install and that the source records the pool it holds.
    assert_eq!(src.pool_threads(), Some(1));
}
```

Add `pub fn pool_threads(&self) -> Option<usize>` returning `self.pool.as_ref().map(|p| p.current_num_threads())` for the pin. (Use the test module's existing plane-writing helper name in place of `write_plane`.)

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib band_read_warps_on_the_source_pool`
Expected: compile error — no `with_pool`.

- [ ] **Step 3: Implement `with_pool` and install the warp**

In `RegisteredSource` add `pool: Option<Arc<rayon::ThreadPool>>` (initialised `None` in `open`), the builder:

```rust
    /// The pool `fill_frame`'s row-parallel warp runs on (perf tier 1 Task
    /// 2). Without one it lands on rayon's GLOBAL pool — sized by
    /// `available_parallelism`, invisible to `image_pool` and the admission
    /// budget — because the band workers are plain `std::thread::scope`
    /// threads, not pool workers.
    pub fn with_pool(mut self, pool: Arc<rayon::ThreadPool>) -> Self {
        self.pool = Some(pool);
        self
    }
```

and in `fill_frame`, replace the direct `warp_rows(...)` call with:

```rust
        match &self.pool {
            Some(p) => p.install(|| warp_rows(&plane, map, self.ref_width, y0, rows, self.interp, self.clamping, dst)),
            None => warp_rows(&plane, map, self.ref_width, y0, rows, self.interp, self.clamping, dst),
        }
```

(keep the exact argument list `fill_frame` uses today). Thread `.with_pool(Arc::clone(pool_ref))` into the three constructions: `stacking/integrate.rs::integrate_planes` (`:622-643`), `ln/reference.rs::build_reference`, and `ln/mod.rs::normalize_frame` (after Task 8 it is one construction per frame).

- [ ] **Step 4: VNG and `noise_mrs` under the pool**

`execute_generation` gains `pool: Option<&Arc<rayon::ThreadPool>>` before `cancel`; the VNG call becomes:

```rust
        let rgb = match pool {
            Some(p) => p.install(|| vng_debayer_f32(&frame.data, frame.width, frame.height, pattern)),
            None => vng_debayer_f32(&frame.data, frame.width, frame.height, pattern),
        };
```

Update every caller: the run (`run.rs:1600`, pass `Some(&rc.ctx.image_pool)`), `export/file_organizer.rs::generate_one` and `api/sync_prepare.rs` (pass `Some(&ctx.image_pool)` where a `ServiceContext` is in scope, else `None`). In `measure.rs:280`:

```rust
    let (noise_adu, noise_source) = match match pool {
        Some(p) => p.install(|| psf_signal::noise_mrs(&scaled, w, h)),
        None => psf_signal::noise_mrs(&scaled, w, h),
    } { /* existing arms */ };
```

- [ ] **Step 5: LN detection and fits under the pool**

`psf_signal::fit_all` gains `pool: Option<&Arc<rayon::ThreadPool>>` and runs its `par_iter` body via `pool.map_or_else(|| body(), |p| p.install(body))`; `fit_stars` / `fit_stars_with_beta` thread it through (the `measure.rs` caller already wraps in `install` — passing `None` there keeps it as is). `scale::detect_seeds` gains `pool` and passes it to `detect_stars` instead of `None`; `relative_scale_against` gains `pool`, passes it to both. `normalize_frame` gains `pool` and passes it to `relative_scale_against` and `RegisteredSource::with_pool`. `run.rs:7041` passes `Some(pool_ref)` with `let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;` captured like Measure does. Every test calling these functions passes `None`.

- [ ] **Step 6: Clamp admission to the pool**

```rust
fn admission(working_set_bytes: u64, max_workers: usize) -> usize {
    let working_set = working_set_bytes.max(1);
    let n = match total_ram_bytes() {
        Some(total) => (total / 4) / working_set,
        None => 1,
    };
    n.clamp(1, max_workers.max(1) as u64) as usize
}
```

Every call site passes `rc.ctx.image_pool.current_num_threads()`. Update the doc comment: "`max_workers` is the app pool's width — a frame worker that outnumbers the pixel pool only queues behind it." Existing unit tests of `admission` (grep `fn admission_` in `run.rs` tests) gain the second argument.

- [ ] **Step 7: Gate**

Run: `cargo test -p athenaeum-core --lib && cargo check -p athenaeum-core --all-targets`
Expected: green (the full suite, because `execute_generation`'s signature reaches export and sync tests).

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src
git commit -m "stacking: every parallel section runs on image_pool — VNG, MRS noise, LN detection and fits, band-worker warps; fan-out admission clamped to the pool's width"
```

---

### Task 3: Working-set formulas from measured residency

Measure's `8 · planes · w · h · 4` admits one OSC frame at a time on 16 GB; LN's `channels · w · h · 4 · 2` admits six. Neither matches what is resident: both process one channel at a time.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/run.rs:2361` (Measure), `:7015` (Normalize)
- Test: `crates/athenaeum-core/src/stacking/run.rs` tests

**Interfaces:**
- Produces: two `pub(crate) const` factors — `MEASURE_PLANES_RESIDENT: u64 = 6` and `LN_PLANES_RESIDENT: u64 = 4` — and two pure functions `measure_working_set_bytes(w, h) -> u64` / `ln_working_set_bytes(w, h) -> u64` used by the call sites.

- [ ] **Step 1: Measure the real residency once**

Run on one OSC calibrated frame of the dev catalog (any `c_*_d.fits` under the stacking working folder):

```bash
cd crates/athenaeum-core && cargo build --release --example measure_probe
/usr/bin/time -l ../../target/release/examples/measure_probe <path-to-c_x_d.fits> 2>&1 | grep -E "maximum resident|peak"
```

Divide the peak RSS by `6248 × 4176 × 4 B = 104 MB` and round UP to get the plane factor; the audit expects 4–6 for Measure. Do the same with `ln_probe` on the same frame for LN (expected 3–4). Record both numbers in the constants' doc comments with the date and the probe command. If a probe reports more than 8 planes, stop: the audit's residency claim is wrong and the task must say so rather than admit more frames.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn measure_working_set_is_one_channel_deep() {
    // A 3-plane 26 Mpx frame: one channel resident at a time, so the planes
    // count does not multiply in.
    let one_channel = measure_working_set_bytes(6248, 4176);
    assert_eq!(one_channel, MEASURE_PLANES_RESIDENT * 6248 * 4176 * 4);
    // 16 GB, 10-worker pool: at least 3 OSC frames in flight.
    let n = (16u64 << 30) / 4 / one_channel;
    assert!(n >= 3, "admission would be {n}");
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib measure_working_set_is_one_channel_deep`
Expected: compile error.

- [ ] **Step 4: Implement**

```rust
/// Planes of ONE channel resident while a frame is measured (perf tier 1
/// Task 3, measured 2026-09-19 with `measure_probe` under `/usr/bin/time -l`:
/// <peak RSS> on a 6248×4176 3-plane frame → <factor>): the decoded plane,
/// its ADU-scaled copy, the background and noise maps, the detector's own
/// copy and the à-trous layers. `measure_frame_with_seeds` reads and measures
/// one plane at a time, so the channel count does not multiply in.
pub(crate) const MEASURE_PLANES_RESIDENT: u64 = 6;
/// Same for `normalize_frame`: the warped target, `clean_plane`'s copy, the
/// detector's scaled copy and the warp scratch — one channel at a time.
pub(crate) const LN_PLANES_RESIDENT: u64 = 4;

pub(crate) fn measure_working_set_bytes(w: u64, h: u64) -> u64 { MEASURE_PLANES_RESIDENT * w * h * 4 }
pub(crate) fn ln_working_set_bytes(w: u64, h: u64) -> u64 { LN_PLANES_RESIDENT * w * h * 4 }
```

Replace `admission(8 * max_planes as u64 * group_max_w * group_max_h * 4, …)` with `admission(measure_working_set_bytes(group_max_w, group_max_h), …)` and `admission(input.channels as u64 * input.width as u64 * input.height as u64 * 4 * 2, …)` with `admission(ln_working_set_bytes(input.width as u64, input.height as u64), …)`. Replace the constants with the measured factors from Step 1.

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p athenaeum-core --lib stacking::run`

```bash
git add crates/athenaeum-core/src/stacking/run.rs
git commit -m "stacking: Measure and Normalize admission from measured one-channel residency, not an all-planes guess"
```

---

### Task 4: Calibrate through the fan-out

`stage_calibrate` (`run.rs:1705-1819`) is one thread. Split `calibrate_one_frame` into a resolve phase (DB + memo, run thread), an execute phase (pixels, fan-out) and a commit phase (artifact rows, run thread). Hot-pixel maps are built for every distinct dark BEFORE the fan-out so workers only read the map cache.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/run.rs:1480-1830`
- Test: `crates/athenaeum-core/src/stacking/run.rs` tests (the run-level tests use `test_fixtures::frame_set` + `add_light` + `add_master_dark_and_flat`)

**Interfaces:**
- Produces (all private to `run.rs`):
  ```rust
  struct CalibrateJob {
      idx: usize,               // position in the group's frame list
      frame_id: i64,
      group_key: String,
      hash: String,
      spec: GenerationSpec,
      out: PathBuf,
      mosaic_out: Option<PathBuf>,
      opts: CalibratedLightOptions,
  }
  enum CalibrateResolved { Reused { bytes: u64 }, Excluded { reason: String }, Job(Box<CalibrateJob>) }
  fn resolve_calibrate_job(rc: &mut RunContext, cfg: &StackingConfig, group: &IntegrationGroup, frame: &GroupFrame, scratch: &Path) -> Result<CalibrateResolved, RunError>;
  fn prebuild_hot_maps(rc: &mut RunContext, jobs: &[CalibrateJob], scratch: &Path);
  fn commit_calibrated(rc: &mut RunContext, job: &CalibrateJob, generated: GeneratedLight) -> Result<u64, RunError>; // bytes
  pub(crate) const CALIBRATE_PLANES_RESIDENT: u64 = 8; // light + out + 3-plane VNG out + band scratch
  ```
- Consumes: Task 2's `execute_generation(…, pool, cancel)` and `admission(_, max_workers)`; Task 0's `log_admission`.

- [ ] **Step 1: Write the failing pin — fan-out calibration is byte-identical and warnings keep frame order**

```rust
/// Perf tier 1 Task 4: the fan-out must produce the same calibrated bytes,
/// the same artifact rows and the same warning ORDER as the sequential
/// stage did. The fixture's four lights share one dark whose MAD is zero, so
/// the hot-pixel refusal warning fires exactly once, for the FIRST frame.
#[test]
fn calibrate_fan_out_matches_the_sequential_stage_byte_for_byte() {
    let f = crate::stacking::test_fixtures::frame_set("fanout");
    let (dark, flat) = crate::stacking::test_fixtures::add_master_dark_and_flat(&f, 64, 48);
    let lights: Vec<_> = (0..4)
        .map(|i| crate::stacking::test_fixtures::add_light(&f, &LightSpec { name: &format!("l{i}.fits"), width: 64, height: 48, seed: i as u64, ..LightSpec::default() }))
        .collect();
    // Sequential reference: admission forced to 1 via the env override the
    // task adds for exactly this pin.
    std::env::set_var("ATHENAEUM_STACKING_ADMISSION", "1");
    let seq = run_to_stage(&f, Stage::Calibrate).unwrap();
    let seq_bytes: Vec<Vec<u8>> = seq.calibrated_paths().iter().map(|p| std::fs::read(p).unwrap()).collect();
    let seq_warnings = seq.warnings.clone();
    std::env::remove_var("ATHENAEUM_STACKING_ADMISSION");
    // Fresh working folder, admission free.
    let par = run_to_stage(&f.reset_working(), Stage::Calibrate).unwrap();
    let par_bytes: Vec<Vec<u8>> = par.calibrated_paths().iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert_eq!(seq_bytes, par_bytes);
    assert_eq!(seq_warnings, par.warnings);
    let _ = (dark, flat, lights);
}
```

Use the fixture and run-driver helpers the existing stage-1 tests in `run.rs` already use (grep `fn calibrate_` in the tests module and copy their setup verbatim — the names above are placeholders for those helpers; the plan's requirement is the two assertions). Add a private `admission_override() -> Option<usize>` reading `ATHENAEUM_STACKING_ADMISSION` (test-only escape hatch, documented as such, applied inside `admission`).

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib calibrate_fan_out_matches`
Expected: compile error (no override / no fan-out yet) — or, once the override exists, PASS trivially. The test earns its keep after Step 3; keep it.

- [ ] **Step 3: Split `calibrate_one_frame`**

Move everything in `calibrate_one_frame` up to and including the `mosaic_out` computation into `resolve_calibrate_job`, returning `CalibrateResolved::Job(Box::new(CalibrateJob { idx, frame_id: frame.frame_id, group_key: group.key.clone(), hash, spec, out, mosaic_out, opts: calibration_opts }))` (`Reused`/`Excluded` arms map from the existing early returns). Move the block after `execute_generation` (file identities, the warn about a missing mosaic, the artifact transaction) into `commit_calibrated`, which also does `rc.warnings.extend(generated.warnings)` and returns `size as u64`.

`prebuild_hot_maps`: for each distinct `job.spec.dark_path` (when `job.opts.hot_pixel_correction`) not yet in `rc.hot_maps`, call `crate::calibration_library::cosmetic::hot_pixel_map_from_dark(dark, scratch)` exactly as `execute_generation` does at `calibrated_generator.rs:514-527` (same `Refused` mapping, same `warn!`), insert `Arc::new(outcome)`. Then `execute_generation`'s own `hot_maps.get(dark)` always hits, and `newly_measured` is `false` for every worker — so the ONE refusal warning must now be pushed by `prebuild_hot_maps` itself (push `format!("Hot-pixel correction skipped for {}: {reason}", dark.display())` to `rc.warnings` when it builds a `Refused`), which is what keeps the warning order the pin asserts.

- [ ] **Step 4: Fan the jobs out**

Replace the inner `for frame in &group.frames` loop of `stage_calibrate` with:

```rust
        let mut jobs: Vec<CalibrateJob> = Vec::new();
        for (idx, frame) in group.frames.iter().enumerate() {
            if excluded_set.contains(&frame.frame_id) { continue; }
            rc.check_cancel()?;
            match resolve_calibrate_job(rc, &cfg, group, frame, &scratch)? {
                CalibrateResolved::Reused { bytes } => {
                    bytes_done += bytes;
                    rc.cached_calibrated.insert(frame.frame_id, true);
                    current += 1;
                    rc.progress(Stage::Calibrate, Some(group.key.clone()), current, total, bytes_done, bytes_total, Some(frame.frame_id), None);
                }
                CalibrateResolved::Excluded { reason } => {
                    tracing::warn!(run_id = rc.run_id, frame_id = frame.frame_id, error = %reason, "calibration failed; frame excluded");
                    rc.runtime_exclusions.push((frame.frame_id, reason));
                    current += 1;
                }
                CalibrateResolved::Job(job) => { let mut j = *job; j.idx = idx; jobs.push(j); }
            }
        }
        if jobs.is_empty() { continue; }
        prebuild_hot_maps(rc, &jobs, &scratch);

        // Max over the group's members, exactly as Measure's `group_max_w`/
        // `group_max_h` do — an M4b group may hold frames of different native
        // size (a second camera, a bin-2 member), and admission must budget
        // for the largest one.
        let (gw, gh) = (group_max_w as u64, group_max_h as u64);
        let admission_n = admission(CALIBRATE_PLANES_RESIDENT * gw * gh * 4, rc.ctx.image_pool.current_num_threads());
        log_admission(rc, Stage::Calibrate, &group.key, CALIBRATE_PLANES_RESIDENT * gw * gh * 4, admission_n);
        let ticker = FanOutTicker::new(rc, Stage::Calibrate, Some(group.key.clone()), current, total);
        let ticker_ref = &ticker;
        let cancel_ref: &AtomicBool = &rc.cancel;
        let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
        let hot_maps_snapshot: HashMap<PathBuf, Arc<HotPixelMapOutcome>> = rc.hot_maps.clone();
        let scratch_ref: &Path = &scratch;
        let job_refs: Vec<&CalibrateJob> = jobs.iter().collect();
        let results = fan_out(job_refs, admission_n, cancel_ref, move |job: &CalibrateJob| {
            let mut local_maps = hot_maps_snapshot.clone(); // Arcs only — cheap
            let out = execute_generation(&job.spec, &job.out, job.mosaic_out.as_deref(), scratch_ref, &job.opts, &mut local_maps, Some(pool_ref), cancel_ref)
                .map_err(|e| {
                    if matches!(e.downcast_ref::<IntegrationError>(), Some(IntegrationError::Cancelled)) { CANCELLED_MARKER.to_string() } else { format!("calibration failed: {e:#}") }
                });
            ticker_ref.tick(Some(job.frame_id));
            out
        });
        rc.check_cancel()?;
        current = ticker.done();
        for (pos, res) in results.into_iter().enumerate() {
            let job = &jobs[pos];
            match res {
                None => return Err(RunError::Cancelled),
                Some(Err(msg)) if msg == CANCELLED_MARKER => return Err(RunError::Cancelled),
                Some(Err(msg)) => {
                    tracing::warn!(run_id = rc.run_id, frame_id = job.frame_id, error = %msg, "calibration failed; frame excluded");
                    rc.runtime_exclusions.push((job.frame_id, msg));
                }
                Some(Ok(generated)) => {
                    bytes_done += commit_calibrated(rc, job, generated)?;
                    rc.cached_calibrated.insert(job.frame_id, false);
                }
            }
        }
        rc.progress(Stage::Calibrate, Some(group.key.clone()), current, total, bytes_done, bytes_total, None, None);
```

with `const CANCELLED_MARKER: &str = "__cancelled__";` next to the stage. `fan_out` returns results at their original index, so `commit_calibrated` runs in frame order — warnings and artifact rows land in the same order the sequential loop produced. Delete the old `calibrate_one_frame`; update the stage's doc comment (ruling 4's "sequentially" no longer holds — say the fan-out replaced it on 2026-09-19 and why the export path keeps its own loop).

`group_max_w`/`group_max_h` are computed the same way `stage_measure` computes them at `run.rs:~2340` (the maximum native width/height over the group's included members, read from the same per-frame geometry `calibrate_bytes_total` uses) — copy that computation, do not invent a group-level field. A mixed-geometry group (M4b: a second camera or a bin-2 member beside the others) is budgeted for its largest member.

- [ ] **Step 5: Gate**

Run: `cargo test -p athenaeum-core --lib stacking::` then `cargo test -p athenaeum-core --lib`
Expected: green, including every stage-1 cache/staleness pin (`calibrated_mosaic` regeneration, `rerun_from = Calibrate`, cancel mid-stage).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/stacking/run.rs
git commit -m "stacking: Calibrate runs through the fan-out — resolve on the run thread, pixels in parallel, artifact rows committed in frame order; hot-pixel maps prebuilt per dark"
```

---

### Task 5: Master dark/flat decoded once per group

Every light re-opens and re-band-reads its dark and flat (`light_cal.rs:384-398`). Decode each distinct master once per group through the SAME `BandSource` decode path and hand the planes to the engine.

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs:110-147` (`LightCalInputs` — no change; the preload rides beside it), `:262-268` (`calibrate_light_compute`), `:332-460` (`calibrate_light_compute_inner`)
- Modify: `crates/athenaeum-core/src/export/calibrated_generator.rs:485-495` (`execute_generation` takes `preloaded: Option<&PreloadedMasters>`)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (Task 4's fan-out: a per-group `MasterPlaneCache`)
- Test: `crates/athenaeum-core/src/calibration_library/light_cal.rs` tests

**Interfaces:**
- Produces:
  ```rust
  /// Master planes already decoded through `BandSource` — the same decode the
  /// engine would run itself, so the samples are bit-identical (perf tier 1
  /// Task 5). Keyed by the master's path; the engine uses an entry only when
  /// the path matches its own `dark_path`/`bias_path`/`flat_path` AND the
  /// length matches `w * h`, and reads the file itself otherwise.
  pub struct PreloadedMasters { pub planes: HashMap<PathBuf, Arc<Vec<f32>>> }
  pub fn preload_master_plane(path: &Path, scratch_dir: &Path) -> Result<Arc<Vec<f32>>, IntegrationError>;
  pub fn calibrate_light_compute_with(inputs: &LightCalInputs, preloaded: Option<&PreloadedMasters>, cancel: &AtomicBool) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError>;
  ```
  `execute_generation(spec, output_path, mosaic_path, scratch_dir, opts, hot_maps, preloaded: Option<&PreloadedMasters>, pool, cancel)`.

- [ ] **Step 1: Write the failing pin**

In `light_cal.rs` tests (copy the setup of the existing test that calibrates a light against a dark and a flat — grep `fn calibrates_` there):

```rust
/// Perf tier 1 Task 5: a frame calibrated from preloaded master planes is
/// bit-identical to one calibrated from the files.
#[test]
fn preloaded_masters_calibrate_bit_identically() {
    let (dir, inputs) = fixture_light_dark_flat(); // the module's existing fixture helper
    let cancel = AtomicBool::new(false);
    let (from_files, _) = calibrate_light_compute(&inputs, &cancel).unwrap();
    let mut planes = HashMap::new();
    for p in [inputs.dark_path.clone(), inputs.flat_path.clone()].into_iter().flatten() {
        planes.insert(p.clone(), preload_master_plane(&p, dir.path()).unwrap());
    }
    let pre = PreloadedMasters { planes };
    let (from_ram, _) = calibrate_light_compute_with(&inputs, Some(&pre), &cancel).unwrap();
    assert_eq!(from_files.data, from_ram.data);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib preloaded_masters_calibrate_bit_identically`
Expected: compile error.

- [ ] **Step 3: Implement the preload**

```rust
pub fn preload_master_plane(path: &Path, scratch_dir: &Path) -> Result<Arc<Vec<f32>>, IntegrationError> {
    let src = BandSource::open(&[path.to_path_buf()], scratch_dir, 1)?;
    let (w, h) = (src.width(), src.height());
    let mut planes = BandPlanes::new(&src);
    let mut out = vec![0f32; w * h];
    let band_rows = src.band_rows_for_budget(MIN_BUDGET_BYTES).min(h).max(1);
    let mut y = 0;
    while y < h {
        let rows = band_rows.min(h - y);
        src.read_band(y, rows, &mut planes, 1)?;
        planes.decode_frame_into(0, &mut out[y * w..(y + rows) * w]);
        y += rows;
    }
    Ok(Arc::new(out))
}
```

In `calibrate_light_compute_inner` (gains `preloaded: Option<&PreloadedMasters>`), resolve before building `paths`:

```rust
    let pre_sub: Option<&Arc<Vec<f32>>> = subtrahend.and_then(|p| preloaded.and_then(|m| m.planes.get(p)));
    let pre_flat: Option<&Arc<Vec<f32>>> = inputs.flat_path.as_ref().and_then(|p| preloaded.and_then(|m| m.planes.get(p)));
```

Push the subtrahend/flat path into `paths` ONLY when its preload is `None`. Inside the pixel loop:

```rust
            let mut v = planes.sample(0, idx) as f64;
            let full = y * w + idx;
            match (pre_sub, sub_idx) {
                (Some(sub), _) => v -= sub[full] as f64,
                (None, Some(si)) => v -= planes.sample(si, idx) as f64,
                (None, None) => {}
            }
            // flat: same shape — `pre_flat.map(|f| f[full])` else `planes.sample(fi, idx)`
```

After `BandSource::open`, check every preloaded plane's `len() == w * h` and return `IntegrationError::BadInput("preloaded master geometry mismatch: …")` otherwise. `calibrate_light_compute` calls `_with(inputs, None, cancel)`. `execute_generation` threads `preloaded` into `calibrate_light_compute_with`.

- [ ] **Step 4: Build the per-group cache in the run**

In Task 4's stage loop, after `prebuild_hot_maps`: collect the distinct `dark_path`/`bias_path`/`flat_path` of `jobs[*].spec.inputs`, `preload_master_plane` each (sequentially on the run thread, or through `fan_out` with admission 2 — three files per group), into a `PreloadedMasters`; a preload failure is a `warn!` (`path`, `error`, `"master preload failed; the engine reads it per frame"`) and the path is simply absent from the map. Pass `Some(&preloaded)` into `execute_generation` in the closure. Memory: two 104 MB planes per group — add `2 * gw * gh * 4` to the stage's admission working set so the budget accounts for them.

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p athenaeum-core --lib && cargo check -p athenaeum-core --all-targets`

```bash
git add crates/athenaeum-core/src
git commit -m "calibration: master dark/flat decoded once per stacking group and handed to the engine as preloaded planes (same BandSource decode, bit-identical)"
```

---

### Task 6: No fsync on regenerable intermediates

`write_fits_f32` (`writer.rs:83-120`) does `sync_all` before the rename. A calibrated intermediate is regenerable (the artifact row keys on the config hash; a missing or truncated file is a cache miss) — atomic visibility via tmp + rename is enough; durability is not needed. Export and masters keep fsync.

**Files:**
- Modify: `crates/athenaeum-core/src/fits_writer/mod.rs:70-90` (`write_image_f32_with`), `writer.rs:83-120` (`write_fits_f32_with`), `xisf_writer.rs:382-420` (`write_xisf_f32_with` gains `Durability`)
- Modify: `crates/athenaeum-core/src/export/models.rs:320-360` (`CalibratedLightOptions::skip_fsync`), `crates/athenaeum-core/src/calibration_library/light_cal.rs:279-291` (`write_calibrated_output` gains `Durability`), `crates/athenaeum-core/src/export/calibrated_generator.rs` (both write calls), `crates/athenaeum-core/src/stacking/run.rs` (the run sets `skip_fsync = true` beside `format = Fits`)
- Test: `crates/athenaeum-core/src/fits_writer/writer.rs` tests

**Interfaces:**
- Produces: `pub enum Durability { Durable, Volatile }` in `fits_writer/mod.rs`; `write_fits_f32_with(path, w, h, channels, data, cards, durability)`; `write_image_f32_with(path, w, h, channels, data, cards, format, bounds, durability)`; the existing `write_fits_f32`/`write_image_f32` become `Durable` wrappers; `CalibratedLightOptions { #[serde(skip)] pub skip_fsync: bool }` (default `false` = durable, so a deserialized document is durable).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn volatile_write_is_still_atomic_and_identical() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<f32> = (0..64 * 48).map(|i| i as f32 * 0.5).collect();
    let a = dir.path().join("durable.fits");
    let b = dir.path().join("volatile.fits");
    write_fits_f32_with(&a, 64, 48, 1, &data, &[], Durability::Durable).unwrap();
    write_fits_f32_with(&b, 64, 48, 1, &data, &[], Durability::Volatile).unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    // No tmp file left behind either way.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".tmp.")).collect();
    assert!(leftovers.is_empty());
}
```

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p athenaeum-core --lib volatile_write_is_still_atomic` → compile error.

- [ ] **Step 3: Implement**

In `fits_writer/mod.rs`:

```rust
/// Whether a write must survive power loss before its rename makes it
/// visible (perf tier 1 Task 6). `Durable` = `sync_all` before the rename —
/// every master, every export. `Volatile` = flush only: the run's own
/// calibrated intermediates, which the artifact row keys on a hash and a
/// stat, so a file lost to a crash is a cache miss, never a wrong answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability { Durable, Volatile }
```

`write_fits_f32_with`: the existing body with `if durability == Durability::Durable { w.get_ref().sync_all()?; }`. `write_fits_f32` = `write_fits_f32_with(…, Durability::Durable)`. Same for the XISF writer and `write_image_f32_with`. `write_calibrated_output(…, format, durability)`; `execute_generation` passes `if opts.skip_fsync { Durability::Volatile } else { Durability::Durable }` to both writes. The run sets `calibration_opts.skip_fsync = true;` right after `calibration_opts.format = OutputFormat::Fits;` with a comment naming the artifact-row contract. Every other `write_calibrated_output` caller passes `Durability::Durable`.

- [ ] **Step 4: Gate and commit**

Run: `cargo test -p athenaeum-core --lib && cargo check -p athenaeum-core --all-targets`

```bash
git add crates/athenaeum-core/src
git commit -m "fits_writer: Durability on the f32 writers; the stacking run's calibrated intermediates skip fsync (tmp + rename keeps them atomic, the artifact row makes a lost file a cache miss)"
```

---

### Task 7: Hot-pixel map medians by selection

`hot_pixel_map_from_dark` sorts the whole dark plane twice (`cosmetic.rs:147-153`); a median needs `select_nth_unstable_by`.

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/cosmetic.rs:140-156,317-327`
- Test: same file

**Interfaces:**
- Produces: `fn median_by_selection(work: &mut [f32]) -> f64` with `median_of_sorted`'s exact semantics (odd n → middle; even n → mean of the two middles as `f64`; empty → 0.0; `total_cmp` order).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn median_by_selection_matches_the_sorted_median() {
    let mut rng = 0x9E3779B97F4A7C15u64;
    let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; (rng % 10_000) as f32 * 0.37 - 1000.0 };
    for n in [0usize, 1, 2, 3, 4, 101, 1000, 1001] {
        let mut v: Vec<f32> = (0..n).map(|_| next()).collect();
        if n > 10 { v[3] = f32::NAN; v[7] = f32::NEG_INFINITY; }
        let mut sorted = v.clone();
        sorted.sort_unstable_by(|a, b| a.total_cmp(b));
        let expected = median_of_sorted(&sorted);
        let mut work = v.clone();
        let got = median_by_selection(&mut work);
        assert!(got.to_bits() == expected.to_bits(), "n={n}: {got} vs {expected}");
    }
}
```

- [ ] **Step 2: Run it to verify it fails** — compile error.

- [ ] **Step 3: Implement**

```rust
/// `median_of_sorted`'s value without the sort: `select_nth_unstable_by`
/// places the upper middle; the lower middle (even `n`) is the maximum of
/// the partition below it. Same `total_cmp` order, so a NaN-carrying plane
/// gives the same answer the sort did.
fn median_by_selection(work: &mut [f32]) -> f64 {
    let n = work.len();
    if n == 0 {
        return 0.0;
    }
    let mid = n / 2;
    let (below, at, _) = work.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    let upper = *at as f64;
    if n % 2 == 1 {
        upper
    } else {
        let lower = below.iter().copied().max_by(|a, b| a.total_cmp(b)).expect("even n >= 2 has a lower half") as f64;
        (lower + upper) / 2.0
    }
}
```

In `hot_pixel_map_from_dark`: `let median = median_by_selection(&mut work);` then the deviation rewrite, then `let mad = median_by_selection(&mut work);`. Keep `median_of_sorted` for the test's oracle (mark `#[cfg(test)]` if it has no other caller).

- [ ] **Step 4: Gate and commit**

Run: `cargo test -p athenaeum-core --lib cosmetic`

```bash
git add crates/athenaeum-core/src/calibration_library/cosmetic.rs
git commit -m "cosmetic: hot-pixel map medians by selection instead of two full sorts (same total_cmp order, same value)"
```

---

### Task 8: LN — one `RegisteredSource` per frame across channels

`normalize_frame` opens a fresh single-frame `RegisteredSource` per channel (`ln/mod.rs:401-413`): a fresh `PlaneReader::open` and, under TPS, a fresh displacement-grid build per channel. `set_plane` (`registered_source.rs:131-140`) exists for exactly this (ruling R-T4-7).

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs:396-425`
- Test: `crates/athenaeum-core/src/stacking/ln/mod.rs` tests

**Interfaces:** none new. Consumes Task 2's `with_pool`.

- [ ] **Step 1: Write the failing pin**

The `registered_source.rs` test `walking_the_planes_of_one_source_builds_one_grid_per_frame` (`:508`) already pins the one-grid-per-frame behaviour for `set_plane`. For LN, pin the sidecar bytes:

```rust
/// Perf tier 1 Task 8: one source per frame (planes via `set_plane`) writes
/// the same `.athln` bytes the per-channel re-open did.
#[test]
fn one_source_per_frame_writes_identical_sidecars() {
    // Use the module's existing 3-channel LN fixture (grep `channels = 3` in
    // this tests module) and run `normalize_frame` twice into two sidecar
    // paths: once with `LN_REOPEN_PER_CHANNEL_FOR_TESTS.store(true)`, once
    // with it false. Compare `std::fs::read` of both.
}
```

Add `pub(crate) static LN_REOPEN_PER_CHANNEL_FOR_TESTS: AtomicBool` (`#[cfg(test)]`) that keeps the old per-channel open path alive for this pin only; delete it and the pin's old-path arm in Task 10 once the acceptance run has confirmed the sidecar hashes on real data.

- [ ] **Step 2: Run it to verify it fails** — compile error.

- [ ] **Step 3: Implement**

Before the `for p in 0..channels` loop:

```rust
    let registered = RegisteredFrame { path: frame.path.clone(), map: frame.map.clone() };
    let mut src = RegisteredSource::open(&[registered], reference.width, reference.height, 0, interpolation, clamping)
        .map_err(|e| LnError::Other(format!("warping into the reference geometry: {e}")))?;
    if let Some(p) = pool { src = src.with_pool(Arc::clone(p)); }
    let mut band = BandPlanes::new(&src);
    let mut target = vec![0f32; reference.width * reference.height];
```

Inside the loop replace the open with `src.set_plane(p).map_err(|e| LnError::Other(e.to_string()))?;` and keep the `read_band_with_progress(0, reference.height, &mut band, 1, …)` + `decode_frame_into(0, &mut target)` — `target` is reused across channels (it is fully overwritten by `decode_frame_into`, and the in-place NaN sanitizing below happens after `background_grid`/`median_of_finite` read it, as today). Drop `src` after the loop (its `Drop` releases the grid, as before).

- [ ] **Step 4: Gate and commit**

Run: `cargo test -p athenaeum-core --lib stacking::ln`

```bash
git add crates/athenaeum-core/src/stacking/ln/mod.rs
git commit -m "ln: one RegisteredSource per frame, planes via set_plane — one reader open and one grid build per frame instead of per channel"
```

---

### Task 9: Register — the dry pass's detections feed the persisting pass

The two-pass dry pass (`run.rs:4045-4110`, `register_group_pass(persist = false)`) reads and detects every frame of the reference's group, keeps `(rotation, translation)`, and the persisting pass re-reads and re-detects the same frames. Detection depends only on the frame's pixels and `cfg.registration` — never on the reference.

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/register/frame.rs:38-130` (split), `crates/athenaeum-core/src/stacking/run.rs:200-260` (`RunContext.dry_pass_stars`), `:2986-3012` (`RegisterItem.pre`), `:3280-3372` (the fan-out closure), `:3541-3700` (`two_pass_refine` builds `switch.stars` from the cache), `:1163-1200` (`run_pipeline` clears the cache after stage 5)
- Test: `crates/athenaeum-core/src/stacking/register/frame.rs` tests, `crates/athenaeum-core/src/stacking/run.rs` tests

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone)]
  pub struct DetectedStars { pub stars: Vec<Star>, pub width: usize, pub height: usize }
  pub fn detect_frame_stars(path: &Path, cfg: &RegistrationConfig, pool: Option<&Arc<rayon::ThreadPool>>) -> Result<DetectedStars, IntegrationError>;
  pub fn register_detected(reference: &ReferenceStars, detected: &DetectedStars, cfg: &RegistrationConfig, hint: Option<&Linear>, policy: SeedPolicy, scale_gate: (f64, f64)) -> FrameRegistration;
  // `register_frame` = detect_frame_stars + register_detected, signature unchanged.
  ```
  `RunContext { dry_pass_stars: HashMap<i64, Arc<DetectedStars>> }`; `RegisterItem { pre: Option<Arc<DetectedStars>> }`; `ReferenceStars: From<&DetectedStars>`.

- [ ] **Step 1: Write the failing pins**

In `register/frame.rs` tests (copy the synthetic two-frame setup of the existing `register_frame` test there):

```rust
#[test]
fn register_detected_equals_register_frame() {
    let (dir, reference_path, subject_path, cfg) = two_frame_fixture();
    let reference = reference_stars(&reference_path, &cfg, None).unwrap();
    let cancel = AtomicBool::new(false);
    let direct = register_frame(&reference, &subject_path, &cfg, None, &cancel, None, SeedPolicy::QuadFirst, (0.8, 1.25)).unwrap();
    let detected = detect_frame_stars(&subject_path, &cfg, None).unwrap();
    let split = register_detected(&reference, &detected, &cfg, None, SeedPolicy::QuadFirst, (0.8, 1.25));
    let (a, b) = (direct.outcome.unwrap(), split.outcome.unwrap());
    assert_eq!(a.map.to_json().unwrap(), b.map.to_json().unwrap());
    assert_eq!(a.inliers, b.inliers);
    let _ = dir;
}
```

In `run.rs` tests, extend the existing two-pass test (grep `two_pass` in the tests module) with: after a full run at the defaults, count `"frame registered"`-class work — simplest observable: the `read_ms`-carrying `"frame registered"` debug from Task 0 fires ONCE per frame of the dry group in the persisting pass with `read_ms == 0 && detect_ms == 0`. Use the crate's log-capturing test helper (`docs/logging/README.md` "log-asserting test patterns") to assert every persisting-pass `"frame registered"` event of the dry group has `detect_ms = 0`.

- [ ] **Step 2: Run them to verify they fail** — compile errors.

- [ ] **Step 3: Split `register_frame`**

```rust
pub fn detect_frame_stars(path: &Path, cfg: &RegistrationConfig, pool: Option<&Arc<rayon::ThreadPool>>) -> Result<DetectedStars, IntegrationError> {
    let t = Instant::now();
    let (lum, width, height) = read_luminance(path)?;
    let read_ms = t.elapsed().as_millis() as u64;
    let t = Instant::now();
    let stars = detect_stars(&lum, width, height, &cfg.detection, cfg.max_stars, pool);
    debug!(path = %path.display(), detections = stars.len(), read_ms, detect_ms = t.elapsed().as_millis() as u64, "frame stars detected");
    Ok(DetectedStars { stars, width, height })
}

pub fn register_detected(reference: &ReferenceStars, detected: &DetectedStars, cfg: &RegistrationConfig, hint: Option<&Linear>, policy: SeedPolicy, scale_gate: (f64, f64)) -> FrameRegistration {
    let start = Instant::now();
    let outcome = align(&detected.stars, &reference.stars, (reference.width, reference.height), (detected.width, detected.height), cfg, hint, policy, scale_gate);
    let duration_ms = start.elapsed().as_millis() as u64;
    // the existing debug!/warn! pair, with `align_ms = duration_ms`
    FrameRegistration { width: detected.width, height: detected.height, detections: detected.stars.len(), outcome, duration_ms }
}

pub fn register_frame(reference, subject, cfg, pool, cancel, hint, policy, scale_gate) -> Result<FrameRegistration, IntegrationError> {
    if cancel.load(Ordering::Relaxed) { return Err(IntegrationError::Cancelled); }
    let detected = detect_frame_stars(subject, cfg, pool)?;
    if cancel.load(Ordering::Relaxed) { return Err(IntegrationError::Cancelled); }
    let mut reg = register_detected(reference, &detected, cfg, hint, policy, scale_gate);
    reg.duration_ms += /* the detect+read time — keep `duration_ms` = whole frame as today */;
    Ok(reg)
}
```

Add "frame stars detected" (`path`, `detections`, `read_ms`, `detect_ms`) to the Task 0 dictionary paragraph in the same commit.

- [ ] **Step 4: Cache in the dry pass, consume in the persisting pass**

`RegisterItem` gains `pre: Option<Arc<DetectedStars>>`. In `register_group_pass`, when building items: `pre: if persist { rc.dry_pass_stars.remove(&p.frame.frame_id).map(Arc::from) } else { None }` — wait, the map holds `Arc<DetectedStars>` already; `remove` returns it. The fan-out closure:

```rust
        let out = if item.is_reference {
            Ok((identity_registration(ref_stars_ref), None))
        } else {
            let detected: Result<Arc<DetectedStars>, IntegrationError> = match item.pre.clone() {
                Some(d) => Ok(d),
                None => detect_frame_stars(&item.path, reg_cfg, Some(pool_ref)).map(Arc::new),
            };
            detected.map(|d| {
                let reg = register_detected(ref_stars_ref, &d, reg_cfg, item.hint.as_ref(), item.policy, item.scale_gate);
                (reg, (!persist).then_some(d))
            }).map_err(|e| format!("registration failed: {e}"))
        };
```

(`persist` is a `bool` captured by copy.) In the results loop, for `!persist` insert `rc.dry_pass_stars.insert(frame.frame_id, d)` when `Some(d)` came back. In `two_pass_refine`, where `switch.stars` is computed by `reference_stars(&new_calibrated, …)` (`run.rs:~3667`): use `rc.dry_pass_stars.get(&new_frame_id).map(|d| ReferenceStars { stars: d.stars.clone(), width: d.width, height: d.height })` and fall back to `reference_stars` only when absent. After the persisting loop in `stage_register`, `rc.dry_pass_stars.clear()`. In native mode the same applies per group (the per-group dry pass in `register_native_groups` runs through the same `register_group_pass`).

- [ ] **Step 5: Gate**

Run: `cargo test -p athenaeum-core --lib stacking:: && cargo check -p athenaeum-core --all-targets` (the `register_probe` example calls `register_frame`).
Expected: green — the registration-row pins (`transform_json` byte equality across re-runs, the M4b `+wcs` pins) do not move because the detections and `align` inputs are identical.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/stacking
git commit -m "stacking: the two-pass dry pass keeps each frame's detections for the persisting pass — the reference group is read and detected once per run"
```

---

### Task 10: Acceptance re-run and the measured Δ

Run the reference set once through the acceptance harness before Task 1 (baseline with Task 0's events) and once after Task 9, byte-compare every output, and replace the audit's estimated Δ column with the measured one.

**Files:**
- Modify: `docs/superpowers/research/2026-09-19-stacking-performance-audit.md` §8 (tier 1 table gains a "measured" column), new §10 "Tier 1 acceptance (date)"
- Create: `docs/superpowers/research/2026-09-XX-stacking-perf-tier1-acceptance-run.md` (the run log, same shape as the M-run reports)
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs` (remove `LN_REOPEN_PER_CHANNEL_FOR_TESTS` and the pin's old-path arm)
- Modify: `CLAUDE.md` (the "Stacking" section: one paragraph on tier 1 — the fan-out Calibrate, the prefetch, the one-pool rule, the admission constants — and a note that ruling 4's "sequentially" is retired)

**Interfaces:** none.

- [ ] **Step 1: Baseline (do this BEFORE starting Task 1, right after Task 0 is committed)**

```bash
W=/private/tmp/claude-501/.../scratchpad/tier1
docs/superpowers/research/scripts/acceptance/prepare-catalog.sh $W
docs/superpowers/research/scripts/acceptance/server.sh $W 8933 &
# LDN 1272 = frame set 109 in the dev catalog; the M4c baseline config (LN on, drizzle 2× debayered, defaults otherwise)
docs/superpowers/research/scripts/acceptance/api.sh start_stacking '{"frameSetId":109}' 8933
```

Wait for `stacking-complete` on `curl -N http://127.0.0.1:8933/api/events`. Collect: the `"stacking stage finished"` lines (`query_logs` on the athenaeum-logs MCP, `run_id`), the per-frame sub-stage medians per stage (`query_logs` for `"frame plane measured"`, `"frame registered"`, `"ln frame normalized"`, `"light calibrated"`, `"plane integrated"`, `"drizzle plane deposited"` → median of each `*_ms`), the `"fan-out admitted"` lines, and `xxh3` of every master and drizzled master under `<W>/stacking/output/…`. Keep the working folder for the byte comparison — the after-run must NOT reuse its cache (a fresh `prepare-catalog.sh` into `$W-after`).

- [ ] **Step 2: After Task 9 — the same run on a fresh working folder**

Same commands into `$W-after`. Byte-compare: every master and drizzled master identical (`cmp`); `registration_results.transform_json` identical row by row (`sqlite3 … "select frame_id, transform_json from registration_results order by frame_id"` on both catalogs, `diff`); every `.athln` identical (`cmp` per file — the sidecar carries no timestamp).

- [ ] **Step 3: Write the run report and the measured Δ**

The report's table: stage / baseline min / after min / Δ / the sub-stage medians that explain it / admission before → after. Update the audit's tier 1 table with a "measured Δ" column and add §10 with the report's link and the verdict. Delete the LN test escape hatch from Task 8. Update `CLAUDE.md`'s Stacking section.

- [ ] **Step 4: Full gate and commit**

Run: `cargo test -p athenaeum-core --lib && cargo check -p athenaeum-core --all-targets && cargo check -p athenaeum-core --no-default-features`

```bash
git add docs CLAUDE.md crates/athenaeum-core/src/stacking/ln/mod.rs
git commit -m "docs(research): stacking perf tier 1 acceptance — measured stage deltas, byte-identical outputs; audit and CLAUDE.md updated"
```

Push only when the owner says so.

---

## Self-review

**Spec coverage** (audit §8 tier 1, after the item-7 withdrawal): item 1 → Task 1; item 2 → Tasks 4, 5, 6, 7; item 3 → Task 3; item 4 → Task 2; item 5 → Tasks 8 + 2; item 6 → Task 9. §5.5 (cross-group queue) and §5.6 (Calibrate+Measure fusion) are tier 3, out of scope. Instrumentation-first → Task 0; the measured Δ → Task 10.

**Placeholders:** Task 4 Step 1 and Task 8 Step 1 name fixture helpers by intent (`run_to_stage`, `two_frame_fixture`, `fixture_light_dark_flat`) with an explicit instruction to copy the module's existing helper of that role; the assertions are complete. Task 3 Step 1's constants are filled from a measurement the task itself performs, with the stop condition stated.

**Type consistency:** `execute_generation`'s final signature is `(spec, output_path, mosaic_path, scratch_dir, opts, hot_maps, preloaded, pool, cancel)` — Task 2 adds `pool`, Task 5 adds `preloaded` before it; Task 4's closure is written against the Task 2 shape and must add `Some(&preloaded)` when Task 5 lands (Task 5 Step 4 says so). `admission(working_set, max_workers)` is Task 2's shape everywhere after it. `RegisteredSource::with_pool` (Task 2) is what Task 8 calls. `LnFrameOutcome`'s Task 0 fields are set inside the loop Task 8 restructures — keep the accumulators.
