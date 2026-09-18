# Stacking pipeline — performance audit (2026-09-19)

Scope: every stage of the stacking run (`stacking::run`) plus the primitives
it rides on (`integration/`, `resample/`, `geometry/`, `calibration_library/
light_cal.rs`, `export/calibrated_generator.rs`, the rustafits detector /
fitter / convolutions). Read-only: nothing was changed. The question was
"where does the time go, and what would make it go faster — more
parallelism, SIMD, or the GPU". Every claim below cites the code; the
numbers come from the acceptance-run reports of M1–M4d (all on the same
Mac mini, 10 cores, 16 GB, 208 mono + 160 OSC lights of LDN 1272, 26 Mpx
each) unless a line says otherwise.

## 0. Verdict in one paragraph

The run is **not compute-bound on any single hot loop**. It is bound by
three structural things: (1) the same calibrated pixels are read from disk
and decoded four to six times per frame across the stages, and warped at
full resolution two to three times; (2) three independent concurrency
domains — the `fan_out` OS threads, `ServiceContext::image_pool`, and
rayon's *global* pool — share the cores without knowing about each other,
while the memory-budgeted `admission()` uses an over-generous working-set
formula that starves the OSC group of parallelism on 16 GB; (3) inside the
two heaviest stages the I/O and the compute run strictly one after the
other (Integrate's band loop) or the whole stage is single-threaded across
frames by ruling (Calibrate). A GPU would accelerate the stages that are
already the *smallest* fraction of the run once these are fixed, and would
need a CPU twin for the Docker/web build anyway. **Recommendation: a CPU
cycle first (tiers 1–2 below, no output-changing numerics), which the
measurements say is worth roughly 2× on the reference set; a GPU spike
only after that, scoped to the one shared primitive (`resample::warp_rows`)
that three stages pay for.**

## 1. Where the time goes

Full runs with the shipped defaults + LN + drizzle 2× (the only config the
acceptance runs measured stage by stage). Minutes, wall clock, 16 GB Mac.

| Stage | run 22 (M4c baseline, debayered drizzle) | run 34 (M4d, Bayer drizzle) | Notes |
| ----- | ---- | ---- | ----- |
| Calibrate | 5.1 (run 13) | 3.7 | 368 frames ≈ 0.8 s/frame; OSC writes the mosaic too |
| Measure | 10.1 | (cached) | memory-bound admission — see §3 |
| Register (incl. two-pass dry pass) | 1.9 | 1.8 | dry pass ≈ 0.55 of it |
| Normalize (LN) | 12.6 | 12.2 | 368 sidecars; mono ≈ 19/min, OSC ≈ 10/min |
| Integrate | 10.7 | 10.3 | M1 without LN/bitmaps: 4.3 |
| Drizzle | 14.8 | 8.3 | deposit is 97 % of it; reads 3 % |
| Output | 0.0 | 0.0 | fast on local SSD |
| **Total** | **≈ 50** | **≈ 36** | spec target ≤ 60 |

Sources: `docs/superpowers/research/2026-09-1{0,1,2,4}-m{3,4a,4c,4d}-acceptance-run.md`.
Register once measured 5.4 Gpx × 16 taps ≈ 87 G MACs ≈ 5–30 s of compute
against ≈ 100–120 s of reads at the disk's 400 MB/s — the pass is
disk-bound (spec §3.7). The Integrate stage in M1 read 21.6 GB mono in
50.5 s + combined in 19.5 s, and 49.8 GB OSC in 132 s + 44 s combine —
**read time ≈ 2.5 × combine time before M4a**, ≈ 1 : 1 after M4a's
`medfit_line` (≈ 16 µs per pixel stack at n = 200, ≈ 40 s per 26 Mpx
plane).

**The I/O floor.** The calibrated intermediates of this set are ≈ 88 GB
(208 × 104 MB mono + 160 × (312 + 104) MB OSC). Every stage after Calibrate
reads them in full: Measure, Register (twice for the reference's own group),
Normalize, Integrate, Drizzle — five to six passes ≈ 440–530 GB at 400 MB/s
≈ **18–22 min of pure reading** inside a 50-min run, on top of the 88 GB
written. Nothing overlaps most of it. That is the first number to attack.

## 2. The concurrency model as it stands

Three domains, sized independently, sharing ten cores:

| Domain | Where | Size | Who uses it |
| ------ | ----- | ---- | ----------- |
| `fan_out` OS threads | `stacking/run.rs:1940` | `admission(working_set) = clamp(RAM/4 ÷ working_set, 1, cores)` (`run.rs:1917`) | Measure, Register, Normalize fan-outs (Calibrate: none) |
| `ServiceContext::image_pool` (rayon) | `athenaeum-tauri/src/lib.rs:81`, `athenaeum-web/src/main.rs:145` | `min(available_parallelism, 16)` | Measure's background/fit/structure via `pool.install`, Register's detector, Integrate/LN-reference band combine, drizzle deposit |
| rayon **global** pool | implicit | `available_parallelism` | VNG debayer (`rustafits/.../vng.rs:188`), `noise_mrs` (`stacking/measure.rs:280`, not wrapped), LN's PSF fits (`psf_signal.rs:492` from a plain thread), `warp_rows` inside `RegisteredSource` band workers (`resample/warp.rs:150` from `std::thread::scope` threads) |

Plus `ComputeQueue` (`compute.max_concurrent`, default 1) at the top, and
the Integrate band budget (`RAM/4 ÷ max_concurrent`, `band_budget.rs:148`)
which shrinks per job when the queue allows more jobs.

**What is wrong with it.** Not oversubscription in the crash sense — each
pool is bounded — but (a) `admission()` uses raw `cores`, not the pool's
thread count, so on a > 16-core machine it admits more frame workers than
the pixel pool can serve; (b) work submitted to the global pool is invisible
to the settings, the budget and the docs (the rustafits convention says
"always pass the pool" and three call sites in this pipeline don't); (c) the
per-stage working-set formulas are guesses that under-admit:

| Stage | Formula (`run.rs`) | OSC 26 Mpx | admission on 16 GB | What is actually resident |
| ----- | ------- | ---------- | ---------- | ----- |
| Measure | `8 · planes · w · h · 4` (`:2361`) | 2.5 GB | **1** | one plane + scaled copy + background + noise maps ≈ 3–4 planes of ONE channel at a time |
| Register | `4 · ref_w · ref_h · 4` (`:3277`) | 0.42 GB | 9 | 3 planes + luminance — accurate |
| Normalize | `channels · w · h · 4 · 2` (`:7015`) | 0.62 GB | 6 | one channel's warp + clean copy at a time |

M1's own finding 2 (`2026-09-09-m1-acceptance-run.md:92`) already called
the Measure formula "over-generous … worth revisiting in M4's performance
pass". It was never revisited. The OSC group — the slower half of every
stage — measures **one frame at a time** on this machine.

## 3. Findings per stage

Each finding: what the code does → cost → the lever → does it change output
bytes (the M1–M4d byte-identity pins are the constraint on every change).

### 3.1 Calibrate (stage 1) — 5 min, sequential by ruling

- **Frames calibrate one at a time** — `stage_calibrate` is a plain nested
  `for group { for frame { calibrate_one_frame(rc, …)? } }`
  (`run.rs:1705-1819`, "ruling 4 — exactly as the calibrated-lights export
  does"). No `fan_out`, no pool. Per frame: read light (52 MB u16) + master
  dark + master flat (104 MB f32 each), a scalar f64 formula loop
  (`light_cal.rs:408-458`), VNG (row-parallel on the global pool), then
  write 104 MB (mono) or 312 + 104 MB (OSC + mosaic) through `BufWriter`
  → `flush` → **`sync_all`** → rename (`fits_writer/writer.rs:83-120`).
  That is ≈ 0.4–0.8 GB of I/O per frame with an fsync in the middle, on one
  thread, ≈ 0.8 s/frame.
- **The master dark and flat are re-opened, re-probed and re-band-read for
  every light** (`light_cal.rs:384-398` opens a fresh `BandSource` over
  `[light, dark, flat]`). Only the derived values are cached: the flat-norm
  divisor (`DivisorCache`, `calibrated_generator.rs:212-272`) and the
  hot-pixel map per dark (`rc.hot_maps`, `run.rs:251`). ≈ 208 MB of extra
  reads per frame that the page cache may or may not absorb.
- `resolve_generation_cached` runs **twice per frame** — once for the
  cache-freshness hash, once for the real spec (`run.rs:1490-1500` and
  `:1558-1567`, acknowledged at `plan.rs:417-428`): two header opens and two
  rounds of SQL per frame.
- Hot-pixel map: two full `sort_unstable_by` passes over the dark plane
  (`cosmetic.rs:148,153`) where `select_nth_unstable` would do — once per
  distinct dark, single-threaded (≈ 2–3 s per 26 Mpx dark).

**Levers.** (1) Fan the frames out through the SAME `fan_out`/admission
mechanism the other stages use — the per-frame unit is already
self-contained; shared state is `hot_maps`, `memo` and the warnings vector.
Working set ≈ light + 2 masters + out (+ 3-plane VNG out for OSC) ≈ 0.5–
1.2 GB → 3–4 frames in flight on 16 GB, which hides each frame's fsync and
read latency behind its neighbours. (2) Hold the group's decoded master
dark/flat planes in RAM for the stage (2 × 104 MB per group) and let the
`BandSource` read only the light. (3) Drop `sync_all` for the calibrated
INTERMEDIATE (keep tmp + rename for atomicity; the artifact row is written
after the rename, so a crash leaves either no file or a complete one — the
fsync buys durability the cache row does not need). (4) `select_nth` for
the map. **Expected: 5 → ≈ 1.5–2 min. Output bytes unchanged** (same
formula, same order per frame). Risk: the ruling-4 mirror with export is
lost — export would keep its sequential loop unless it adopts the same
fan-out.

### 3.2 Measure (stage 3) — 10 min, memory-starved and doing more than it needs

- Per plane, unconditionally: background mesh (`estimate_background_mesh`,
  two full-res bicubic interpolations — background AND noise map,
  `rustafits/src/analysis/background.rs:143,164`), MRS noise (4 à-trous
  layers, full-frame separable convolutions), detection, PSF fits. Under
  `PsfModel::Auto` the brightest 64 seeds are fitted **up to 5 ×** (4 β
  candidates + the final pass, `psf_signal.rs:503-566`).
- `sampling_radius` grows the fit region 1 px at a time and **rescans the
  whole `(2r+1)²` square on every step** (`psf_signal.rs:207-271`) — cubic
  in the stamp radius per star; worst on the defocused/trailed frames.
- `noise_mrs` is the one call in `measure_plane_with_seeds` NOT wrapped in
  `pool.install` (`measure.rs:280`) — it runs on the global pool.
- Admission: 1 OSC frame at a time (§2). M1 measured the fan-out saving only
  20 % over a fully sequential probe, all of it on the mono group, because
  the inner `pool.install` calls already use the pool — so the stage's
  serial parts (the sequential mesh fill, labelling, the fit tails, the
  full-frame read) are what a higher admission would overlap.

**Levers.** (1) Fix the working-set formula to what is resident (≈ 3–4
planes of one channel) → OSC admission 3–4. (2) Incremental ring update in
`sampling_radius` (quadratic instead of cubic per star). (3) One
interpolation pass producing both background and noise maps. (4) Wrap
`noise_mrs` in the pool. **Expected: 10 → ≈ 6 min.** Output: (1), (4)
unchanged; (2), (3) change floating-point order — the metrics artifact hash
folds `PSF_FIT_VERSION`, so a bump recomputes cached metrics AND LN
sidecars once (R-M4a-15), which is the accepted mechanism.

### 3.3 Register (stage 5) — 2 min, half of it thrown away

- With the defaults (`Auto` + `twoPass`), `stage_register` first runs the
  reference's OWN group through `register_group_pass(persist = false)`
  (`run.rs:4045-4110`, `:3541-3564`): full read, full detection
  (`detect_fast_data` with centroid refine), quad seed, RANSAC, refit — for
  **every included frame of the group** — and keeps only
  `(rotation_deg, translation)` per frame. Then the persisting pass
  re-reads and re-detects the same group from scratch. The doc comment says
  so (`run.rs:3117-3126`, "pass 1 always registers afresh"). In native mode
  this repeats per group. Detection depends only on the frame's pixels and
  the detection config, never on the reference — the dry pass's `Vec<Star>`
  per frame is exactly what the real pass needs again.
- Register detects stars a frame Measure already detected two stages
  earlier (different thresholds, different purpose, nothing shared —
  `register/frame.rs:86`, `measure.rs:349`).
- `align()` is single-threaded (RANSAC ≤ 2000 iterations over ≤ 2000
  correspondences — milliseconds; not the cost).
- The registered-frame writer is OFF by default and nothing downstream reads
  it (`register/writer.rs:1-4`) — already the fast path; no action.
- Lanczos kernels evaluate `sin()` per tap per pixel (`resample/kernels.rs:
  177-189`, up to 32 `sin` per output pixel at Lanczos4). The default
  `BicubicBSpline` is polynomial and cheap; this matters only when Lanczos
  is chosen, and then in LN/Integrate/Drizzle too since they share the
  kernel.

**Levers.** (1) Keep the dry pass's per-frame star lists (and the decoded
luminance if memory allows) in the run for the persisting pass, re-running
only `align()` — removes a full read + detection of the reference group per
run (≈ 0.5 min here, ≈ 1/3 of Register on a one-group set). (2) A
fixed-phase weight table for Lanczos (the `wx_table` trick LN's `grid.rs`
already uses). **Output unchanged for (1)** — the same detections feed the
same `align`; (2) quantizes the kernel phase and is a numeric change.

### 3.4 Normalize / LN (stage 6) — 12.6 min, the largest redundancy in the run

Per included frame, per channel (`ln/mod.rs:396-562`):

- A **fresh single-frame `RegisteredSource` per channel** (`mod.rs:401-413`)
  → a fresh `PlaneReader::open` + a fresh distortion grid build per channel
  (3 × per OSC frame) — the exact thing ruling R-T4-7 fixed for Integrate by
  reusing one source via `set_plane` (`stacking/integrate.rs:622-635`). LN
  did not adopt it.
- The full-resolution warp of the whole frame runs with `concurrency = 1`
  (`mod.rs:417`) — `read_band_with_progress(0, height, …, 1, …)` takes the
  single-worker branch, so one thread warps 26 Mpx per channel (the
  `warp_rows` row-parallelism inside it lands on the global pool).
- `detect_seeds` calls `detect_stars(…, None)` — **no pool** (`scale.rs:115`)
  — with `DetectionConfig::default()`, not the run's `measurement.*`
  settings, so LN's detector is a third, separately configured detection
  pass over the same frame. The PSF fits right after it (`fit_stars_with_beta`
  → `fit_all` `par_iter`) go to the global pool.
- `background_grid` is a sequential double loop over ≈ 1600 cells
  (`background.rs:127-147`), each up to 5 median/MAD rounds allocating fresh
  `Vec`s; `clean_plane` clones the plane; `median_of_finite` does a second
  full-plane filter+collect (`mod.rs:159-165`).
- The warped pixels are then discarded; Integrate re-warps the same frame
  band by band.

**Levers.** (1) Reuse Measure's fitted star list (positions mapped through
the frame's own forward transform, flux measured natively — the PSF-flux
ratio is invariant under resampling to first order) instead of re-detecting
on the warped plane: removes a full detection + fit per frame per channel,
the single largest line item of the stage. This IS a numeric change (the
sample of stars differs) and needs the R-M4a-15 hash bump plus an
acceptance re-run against the M2 baseline (master noise 0.89–1.13 × the
external tool's). (2) One `RegisteredSource` per frame + `set_plane` —
mechanical, output-identical. (3) Pass the pool into `detect_stars` and
scope `fit_all` into it — output-identical. (4) The stride-128 background
model does not need a full-resolution warp: warp a 4 × 4-binned plane (or
sample the warp on the node mesh's cell centres only) and detect stars on
the full plane only when (1) is not adopted. (5) Tighten the working set to
one channel (admission 6 → 10 on OSC). **Expected: 12.6 → ≈ 6–7 min with
(1)+(2)+(3)+(5); (4) on top if (1) is deferred.**

### 3.5 Integrate (stage 7) — 10.7 min, read and combine strictly serial

- `band_loop` (`integration/engine.rs:170-305`) reads band N fully (`:244`),
  then combines it fully under `pool.install` (`:285`), then reads band N+1.
  `read_duration` and `combine_duration` are two disjoint sums by
  construction. The band read is itself CPU work — `RegisteredSource::
  fill_frame` warps the source window through the inverse map on scoped OS
  threads (`registered_source.rs:154-205`, workers = `read_concurrency`,
  which is the pool width on local storage) — so during "read" the pool
  sits idle and during "combine" the disk sits idle. M1 measured read ≈
  2.5 × combine; after M4a's robust line they are of the same order. **A
  one-band prefetch (double-buffered `BandPlanes`) turns
  `read + combine` into `max(read, combine)`** — the reads already run on
  their own threads, so nothing about the combine's parallelism changes.
- Per pixel: a **stable** `sort_by` (`combine.rs:224-229`, load-bearing for
  the tie-order contract) then the algorithm; `medfit_line` is O(n)
  selection per bracket evaluation, warm-started — already tight for its
  family. Per-pixel `sample()` re-decodes raw bytes (`banded.rs:806-810`),
  N × width × height decode calls per plane; row-local scratch is
  allocated per row inside `process_row` rather than kept in the
  `for_each_init` `RowState` (`engine.rs:810-822`).
- A frame whose source window exceeds `whole_threshold` (0.6 of the height —
  near-quarter-turn rotations) **re-reads its whole plane on every band**
  (`registered_source.rs:142-146`) with no cache; not budgeted anywhere.
- `band_bits` for the `.rej` sink is re-allocated per band (≈ 16 MB × 40
  bands per plane, `engine.rs:1128-1142`, self-documented as unbudgeted).
- Large-scale rejection doubles the stage (two `integrate_planes` passes) —
  off in every preset; fine.

**Levers.** (1) Prefetch the next band — output-identical, the biggest
single win in this stage (≈ 30–45 % by the M1/M4a split). (2) Keep the row
scratch in `RowState`; reuse `band_bits`. (3) Cache the decoded whole plane
for `Whole`-window frames across bands. (4) Bulk-decode a band's row into a
per-worker f32 scratch once (the `decode_row_into` path exists,
`banded.rs:814-826`) instead of per-sample `match` — a modest CPU win, and
it is what makes the gather SIMD-shaped. **Expected: 10.7 → ≈ 6–7 min.**

### 3.6 Drizzle (stage 8) — 8–15 min, compute-bound in `deposit_band`

- Groups sequential; frames sequential; **only the 17 output bands of one
  frame's one plane run in parallel** (`DRIZZLE_BAND_ROWS = 512`,
  `drizzle/mod.rs:49,624-632`). Per source pixel with the square kernel:
  ≈ 5 `ForwardEval::at()` calls (four drop corners + the rejection/LN
  index) and up to 4 Sutherland–Hodgman `clip_area` calls in f64
  (`geom.rs:103-258`). Measured 1.1–1.3 s per 26 Mpx frame-plane at 2 ×;
  deposit is 97 % of the stage, reads 3 %.
- The circle/Gaussian kernels call `fwd.at()` **once per micro-drop table
  entry — 256 per source pixel** (`mod.rs:979`) — ≈ 50 × the square
  kernel's inner loop; never benchmarked on real data.
- OSC re-opens and re-reads the calibrated file per channel; Bayer drizzle
  re-reads the ONE mosaic three times (`mod.rs:515-549`). Cheap today (3 %)
  but triples the decode.
- Accumulators are full output resolution, one `I`/`W` pair live per
  channel (≈ 1.25 GB for mono at 2 ×, 3–4 GB OSC with the weight map);
  frame-level parallelism would multiply that — the R-M3-7/16 refusal gate
  exists for exactly this.

**Levers.** (1) **For a map with no distortion (the default — `distortion:
Off`), the mapped drop is the same parallelogram for every source pixel;
only its sub-pixel phase varies.** The overlap areas can be tabulated once
per frame over a 16 × 16 (or 32 × 32) phase grid, turning ≈ 5 evaluations +
4 polygon clips per pixel into one table row lookup. With a polynomial or
TPS map the shape varies slowly — per-tile tables (e.g. 256 px) keep the
error below the drop's own quantization. This is the same idea the
circle kernel's `kernel_table` already uses for the DROP, applied to the
OVERLAP. Expected 2–4 × on deposit; numeric change of order the phase
quantization (measurable against the current exact clip: the M3 acceptance
pins level to 0.9987–0.99999). (2) Reuse `at()` results between the four
corners of neighbouring pixels (each corner is shared by four drops — a
row-of-corners cache cuts the evaluations by ≈ 4 ×); output-identical.
(3) One read per frame across channels (plane-major loop → frame-major with
three accumulator pairs live, +2 planes of RAM — or keep plane-major and
cache the decoded mosaic under Bayer). (4) Smaller bands for > 16-core
machines. **Expected: 8–15 → ≈ 4–6 min.**

### 3.7 Output (stage 9) — negligible today, latent on slow storage

`sync_all` + a scalar big-endian byte loop per master (`writer.rs:159-167`,
`xisf_writer.rs:458-466`); `cleanup_work` walks every intermediate twice
(`dir_size_bytes` then `remove_dir_all`, `paths.rs:750-758`) with no
elapsed-time log. Not a target on local SSD; log the cleanup duration.

## 4. Cross-cutting redundancy (the real budget)

Per frame of the reference set, with the defaults:

| Work | Times done | Where |
| ---- | ---------- | ----- |
| Full read + decode of the calibrated frame | 5 (6 for the reference group) | Measure, Register (+ dry), LN, Integrate, Drizzle |
| Star detection + centroid/PSF fit | 3 (4 for the reference group) | Measure, Register (+ dry), LN |
| Full-resolution warp through the `PixelMap` | 2–3 | LN (whole frame), Integrate (band windows), + the LN reference subset |
| Distortion grid build (TPS only) | ≈ 4 mono / 9 OSC | writer, LN per channel, integrate, drizzle per channel (spec §5 note: ≈ 25 min of grid building on the OSC group) |
| Master dark/flat read | once per LIGHT | Calibrate |

The "transforms persist, pixels do not" architecture (`registered_source.rs:
1-5`) is a deliberate disk-vs-CPU trade and this audit does not propose to
reverse it — a materialized registered set would be another 88 GB written
and read. What the table says is that the *detections* and the *first
decode* are the things worth carrying across stages, not the pixels.

## 5. Parallelism — what to change

1. **One pool, one budget.** Every `par_iter`/`par_chunks_mut` in the
   pipeline runs inside `image_pool.install` (VNG, `noise_mrs`, LN's
   `detect_stars` and `fit_all`, `warp_rows` from the band workers), and
   `admission()`'s upper clamp is `image_pool.current_num_threads()`, not
   raw `cores`. Output-identical.
2. **Working sets measured, not guessed** (§2 table). Measure and LN are
   under-admitted on 16 GB by 3–4 × for OSC. Output-identical.
3. **Calibrate joins the fan-out** (§3.1). Output-identical.
4. **Integrate prefetches one band** (§3.5). Output-identical.
5. **Cross-group work queue** — Register/LN drain one group's tail before
   the next group starts (`run.rs:4118-4132`); a queue spanning the run's
   groups keeps the cores busy on multi-group (native-mode, multi-filter)
   sets. Output-identical; bookkeeping only.
6. **Stage pipelining is NOT worth it** as a general mechanism: Measure needs
   every frame calibrated before the reference is chosen, Register needs the
   reference, LN needs registration — the dependencies are real. The one
   fusion that pays is **Calibrate + Measure in one pass over the frame
   while it is still in RAM** (saves one full read + decode per frame and
   lets the measurement start the moment the calibrated buffer exists) —
   an architectural change to the artifact model (one generation producing
   two artifact rows), best done as its own plan after the tier-1 wins.

## 6. SIMD

rustafits already carries NEON/AVX2 paths for its own hot loops
(`convolution.rs`, `binning.rs`, `debayer.rs`, `stretch.rs`) under the
default `simd` feature. In the stacking pipeline nothing is hand-vectorized;
the scalar loops that matter are:

- the calibrate formula (`light_cal.rs:408-458`, f64 with per-pixel
  `Option` branches and a CFA-channel lookup — restructure per row per CFA
  phase and it auto-vectorizes; today it will not);
- the FITS/XISF byte-swap serializers (`writer.rs:159`, `xisf_writer.rs:458`)
  and `banded.rs`'s per-sample decode — trivially vectorizable, memory-bound
  once they are;
- the `warp_rows` tap gather (`warp.rs:35-47` clamps per tap) — vectorizes
  across output x once the taps are hoisted per row;
- the per-pixel rejection stack (`combine.rs`) — **not** vectorizable in its
  current form (data-dependent iteration counts, selection, a stable sort);
  the only SIMD there is the bulk decode into the stack.

Expected gain from SIMD alone: 10–20 % on Calibrate and the Integrate read
side, nothing on the two heaviest loops (`deposit_band`, `medfit_line`).
Recommendation: profile first (`cargo flamegraph` on `integrate_probe` and
`ln_probe`), then vectorize the loops the profile names. Do not spend the
cycle on SIMD before the structural items above — they are 5–10 × bigger.

## 7. GPU — assessment and decision

**What is GPU-shaped.** Ranked by (share of run × how well it maps):

| Candidate | Share of a 50-min run | GPU fit | Blocker |
| --------- | ------ | ------- | ------- |
| `resample::warp_rows` (LN + Integrate + drizzle's forward map) | ≈ 30 % of the run is warping or reading-to-warp | excellent — texture sampling with a bicubic/Lanczos kernel, per-frame inverse grid as a second texture | the band model streams N frames per band; the GPU version wants the whole source plane resident per frame (104 MB — fine on unified memory, a PCIe copy per frame elsewhere) |
| Drizzle deposit | 15–30 % | good — scatter with atomics, or a gather formulation per output pixel | exact f64 polygon clipping must be re-expressed in f32; the level-preservation pin (R-M3-2) would need a tolerance; the CPU path must stay for headless |
| Measure's full-frame passes (background interpolation × 2, 4 à-trous layers, the structure cascade) | ≈ 8 % | excellent — separable convolutions and elementwise passes | rustafits primitives, shared with the Analysis tab — the GPU twin lives in rustafits |
| VNG debayer | ≈ 2 % | excellent | already parallel, small share |
| Per-pixel rejection + combine | ≈ 10 % | **poor** — warp divergence on `medfit_line`'s data-dependent bracketing/bisection and on Winsorized/RCR iterations; a 200-sample sort per thread | would need a fixed-iteration formulation (min/max, a fixed-pass sigma clip) to be worth it — i.e. a different algorithm, not a port |
| PSF fits (LM per star) | ≈ 10 % | poor — variable stamp size, data-dependent iterations, branchy accept gates | batched LM with padded stamps is a redesign |

**What a GPU path costs.** A `wgpu` dependency (Metal on macOS, Vulkan/DX12
elsewhere; WGSL shaders) — a large new subsystem: device selection and
fallback, buffer lifetime, a CPU twin kept byte-close for the Docker/web
build and for CI runners that have no GPU (the whole M1–M4d pin suite runs
on GitHub's 4-worker runners), float32-only on most consumer GPUs (the
drizzle clipper and the calibrate formula are f64 today), and a
tolerance-based acceptance instead of byte identity for every kernel that
moves. On this Mac the transfer question is moot (unified memory); on a
discrete-GPU Windows box each 26 Mpx plane is ≈ 10 ms over PCIe — not the
issue; the issue is that after tier 1–2 the GPU-shaped stages are ≈ 40 % of
a run that is otherwise I/O, SQLite and per-star fitting.

**Decision.** Not now. Amdahl says the CPU cycle (§8 tiers 1–2) is worth
≈ 2 × on the reference set with no output change; a GPU cycle after it is
worth at most another ≈ 1.5 × on the same set, at several times the
engineering cost and a permanent two-implementation burden. When the CPU
cycle has landed and been measured, run a **one-week spike** scoped to
exactly one primitive — `warp_rows` on `wgpu` — through `ln_probe` on the
mono group: if the warp per frame drops from the measured ≈ 1–2 s to
< 100 ms AND the LN/Integrate stages move by more than 25 %, the second
primitive (drizzle deposit) gets a plan; otherwise the GPU stays out of the
tree. The spike's code is throwaway by construction.

## 8. Proposed plan — tiers

Effort in engineer-days on this codebase; "Δ" is the expected change on the
50-min reference run (16 GB / 10 cores); "bytes" says whether the M1–M4d
byte-identity pins survive.

**Tier 1 — structural, output-identical (≈ 5–7 days, Δ ≈ −18 min)**

| # | Item | Stage | Δ (min) | bytes |
| - | ---- | ----- | ------- | ----- |
| 1 | Prefetch the next band in `band_loop` (double-buffered `BandPlanes`) | Integrate, LN reference | −3 to −4 | identical |
| 2 | Calibrate through `fan_out`; masters decoded once per group; no `sync_all` on intermediates; `select_nth` hot map | Calibrate | −3 | identical |
| 3 | Working-set formulas from measured residency (Measure ≈ 4 planes of one channel, LN one channel) | Measure, LN | −3 to −4 | identical |
| 4 | One pool: `install` around VNG / `noise_mrs` / LN detect+fit / band-worker `warp_rows`; admission clamped to the pool | all | −1 (and predictability) | identical |
| 5 | LN: one `RegisteredSource` per frame via `set_plane`; pool into `detect_stars` | LN | −1 to −2 (−more with TPS) | identical |
| 6 | Register: carry the dry pass's star lists into the persisting pass | Register | −0.5 | identical |
| 7 | Drizzle: corner-sharing cache for `fwd.at()`; one read per frame across channels | Drizzle | −2 to −3 | identical |

**Tier 2 — algorithmic, numeric change under the existing hash-bump
mechanism (≈ 5–8 days, Δ ≈ −8 to −10 min more)**

| # | Item | Stage | Δ (min) | bytes |
| - | ---- | ----- | ------- | ----- |
| 8 | Phase-tabulated overlap areas for the square kernel (per frame for linear maps, per tile with distortion) | Drizzle | −4 to −6 | tolerance (pin vs exact clip) |
| 9 | LN takes Measure's star fits instead of re-detecting on the warp | LN | −3 to −4 | R-M4a-15 hash bump + M2 acceptance re-run |
| 10 | `sampling_radius` incremental ring; one interpolation pass for background + noise; Auto-β on fewer refits | Measure | −2 | `PSF_FIT_VERSION` bump |
| 11 | LN background model on a binned plane | LN | −1 | tolerance |

**Tier 3 — architectural (own plans, after measurement)**

- Calibrate + Measure fused in one pass over the in-RAM frame (§5.6).
- A cross-group work queue for the fan-out stages (§5.5).
- The `warp_rows`-on-`wgpu` spike (§7), gated on tier 1–2 numbers.

**Instrumentation first (½ day, before tier 1):** the stage timings exist
(`stage_timings`), but the sub-stage split does not — read vs warp vs
detect vs fit vs write per frame, read vs combine per band, deposit vs read
per frame-plane. Add `debug!` events with `duration_ms` per sub-stage on
the existing `FanOutTicker` path and re-run the reference set once, so the
Δ column above is measured, not estimated, before any item is started. The
acceptance harness (`docs/superpowers/research/scripts/acceptance/`) is the
tool; the reference set is LDN 1272 as in every M-run.

## 9. Things this audit deliberately does not propose

- Materializing registered frames (another 88 GB per run; "transforms
  persist, pixels do not" stands).
- Replacing the stable `sort_by` in rejection with an unstable sort (the
  tie-order contract is load-bearing for the weighted-sum reproducibility).
- Changing the Auto rejection ladder or the robust line (calibrated in
  M4a/M4c against the external tool; a faster estimator is a quality
  decision, not a performance one).
- mmap for the band reads (the byte-accounting/progress contract of
  `FrameSource::read_band_with_progress` would go, for a syscall-count win
  that the prefetch already dwarfs).
- Compressing the calibrated intermediates (f32 is required by every
  downstream pin; a lossless codec costs more CPU than the reads it saves at
  400 MB/s).
