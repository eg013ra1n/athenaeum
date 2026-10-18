# Task 13c: stacking parallelism, each machine used fully, numbers unchanged (2026-10-18)

Plan A Task 13c (`docs/superpowers/plans/2026-10-07-one-star-path-plan-a.md`, "### Task 13c"); ruling R-OSP-13 in
`docs/stacking/README.md`. Branch `one-star-path-t13c`, final code head **23b81727** (L4b reverted in 28be0f3e). Lab
results are in athenaeum-lab `star-lab/results/plan-a/`: `L` = `t13c/` (dev Mac), `R` = `t13c-runner/` (runner). The
controller ledger and the phase reports, rulings and reviews (`task-13c-*.md`) are in the local SDD folder
`.superpowers/sdd/2026-10-07-one-star-path-plan-a/`. "ERP" = the reference product the owner measures against. Every
number is a measurement unless it says "projected".

## 1. The question and the bars

The owner (2026-10-17) asked for full use of each machine, sized to it ("подобрано под машину, но так чтобы
утилизировать все доступное"), with every result unchanged. The task asked where each stage waits (cores, P / E time,
memory, I/O, serial share); how admission and pools are sized; granularity and tails; overlap; and the hardware (E
cores, SIMD, the GPU as research only).
- **Identity.** Every artifact byte is equal at every thread count and admission level, under a band partition, and
  after a mid-run memory-cap drop (§5). Every bracket also compares its H and N artifacts.
- **Owner bar.** The WHOLE run (Masters → Output) is faster than the ERP's on the same frames, machine and work. A lever
  lands when it is bit-identical, swaps no more than H in any stage, is faster beyond its bracket's spread on one
  machine and does not regress on the other. A lever worth < 1 % of the run AND of medium or high cost is recorded. A
  bracket is H N H N H N on fresh clones on a quiet machine, the median of 3, spread = (max − min) / median, with
  stage walls from `stacking stage telemetry`.

## 2. The machines (`compute budget resolved`, `stacking budget`)

| | dev Mac | runner | container, dry run | container, Step 5 arm |
| ---- | ---- | ---- | ---- | ---- |
| CPUs (P + E) | 10 (4 + 6), Apple M4 | 16 (12 + 4), M4 Max | 8 (`cpu_quota` 8.0) of a 16-CPU VM | 8 (`cpu_quota` 8.0) |
| memory | 16 GiB | 128 GiB | `--memory 16g` ≥ the VM's 15.66 GiB → `mem_limit` −1 (correct) | `--memory 12g` → `mem_limit` 12 GiB |
| pool / memory budget (auto) | 10 / 4 GiB | 16 / 32 GiB | 8 / 3.92 GiB (total / 4) | 8 / 3.00 GiB (limit / 4) |
| r_E (E-core worth, P = 1) | 0.25 (ladder median) | 0.29 | | |
| P-equivalent ceiling | 4 + 6 × 0.25 = 5.5 | 12 + 4 × 0.29 ≈ 13.2 | | |

The pool is `available_parallelism`, or the cgroup / job quota when it binds; the 16-thread cap was removed in Step 1.
`RAYON_NUM_THREADS` overrides it on both hosts (`pool_why = "env"`). The budget is `compute.memory_budget_mb`: 0 = auto =
a quarter of the memory the process may use; a larger value is clamped to that memory (`setting-clamped`).

## 3. Baselines (Step 2, then b296dbc4) and the ERP's whole runs

Set 204 (197 lights, 92 mono + 105 OSC, LN, drizzle 2×), dev Mac, Step 2 at a7d18865 (`L/full-s204` at a07c87f), and
the full run at b296dbc4 (L1 + L17 + L4 + 4d, before L2 / L3; lab 7fe48a1, single run; its clone became the refreshed
before tree `tierA-lnB-f1-13c` run 6). The ERP run is the owner's, on the same machine and work (`L/erp-s204/verdict.txt`).

| stage | Step 2 wall s | cores busy / 10 | P-eq / 5.5 | serial % | peak GiB | swap MB | b296dbc4 s (swap MB) | ERP s |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| calibrate (+ cosmetic, debayer) | 127.3 | 5.45 | 3.18 | 39 | 2.10 | −42 | 120.3 (0) | 84 + 72 = 156 |
| measure | 511.4 | 8.57 | 4.86 | 0 | 4.38 | 0 | 476.5 (0) | 72 |
| register (+ reference) | 69.4 | 7.59 | 4.35 | 4 | 3.45 | 0 | 66.5 (0) | 134 |
| normalize (LN) | 194.0 | 7.49 | 4.36 | 3 | 8.26 | +761 | 168.1 (**911**, peak 9.20 GiB) | 351 |
| integrate | 244.8 | 8.34 | 4.72 | 9 | 6.42 | −25 | 188.4 (0) | 113 |
| drizzle | 90.8 | 5.18 | 3.13 | 3 | 6.62 | 0 | 85.7 (0) | 170 |
| **whole run** | **1237.7** | | | | | | **1105.5 (−10.7 %)** | **1005** (run 2: 968) |

At b296dbc4 set 204 is still 100 s above the ERP: Measure +404 s, Integrate +75 s. With the L2 / L3 bracket deltas
(−15.6, −17.4 s, §6) it projects to ≈ 1072 s, ≈ 67 s above.

ERP run 2 used the "max stars" mode (the batch script's Maximum-quality preset): all 309 parameter lines and its PSF fits
(5 095 mean / 16 717 max per plane) are run 1's. Horse C11 on the dev Mac (106 lights L + G, `L/full-hc11`): **551.3 s**;
Calibrate takes 183.9 s at 0.72 cores, admission 1, 100 % serial (33 % of the run), and Integrate swaps +450 MB. Horse
C11 on the runner (164 lights L / R / G / B, flats only, LN, no drizzle, b21c2c9b, `R/report.md`): **399.9 s** against
the **ERP's 2 787.6 s (46:27.62)** on the same machine. Ours / ERP: calibrate 43.5 / 546, measure 78.3 / 174, register
64.4 / 264, LN 92.1 / 1 236, integrate 121.7 / 426 s; the ERP's flats took 138 s (ours links library masters). Cores
busy / 16: 6.54, 9.44, 13.07, 8.44, 13.87; no swap.

What Step 2 showed:
- **The losses sit inside stages.** Every inter-stage gap is 0. Dev Mac fan-out tails are 2–4 % and idle workers 0–1 %;
  the runner's tails are 23–28 % in Calibrate, Measure, Register and LN (the barriers between groups).
- **Calibrate's 8-plane working set was too large** (one frame in flight on hc11 dev Mac), and its master preload ran on
  one core (3–4 s per group). **Normalize's serial part was the seeds calibration** (7 frames × 2 arms in sequence,
  ≈ 11 s per group on the runner).
- **One frame does not fill a machine.** Probe ladders (`star_probe measure`, identical outputs at every p): dev Mac
  S(10) = 4.27 / 3.89 / 4.10 on 0058 / 0075 / 0163, Karp–Flatt e rising from 0.04–0.11 to 0.15–0.18; runner S(12) 4.80
  / 3.76, S(16) 4.94 / 3.84, S(20) 4.88 / 3.82, e 0.12–0.22. Filling the machine takes frames in flight, not threads.
- **Working sets at admission 1 (system allocator).** Measure 8.7 planes (code 8) and Register OSC 5.7 (code 6) hold;
  Normalize OSC read ≈ 40 planes and the runner's LN L 36.7 planes (see §4).

## 4. Memory (4a): the carry-over is the system allocator's retained heap

- **4a-0** (247fdfde: `memory relief` / `memory checkpoint`, `ATH_VMMAP_DIAG`). `malloc_zone_pressure_relief`
  released **0 bytes at all 28 stage starts** (STOP M0). `vmmap --summary` at each boundary showed the footprint is
  `Malloc Large (empty)`: freed large blocks the allocator keeps, resident or swapped. Set 204 mono Normalize starts at
  6.6 GiB and hc11 Integrate at 7.6 / 7.1 GiB, while live data is small. Peaks are 9.1–9.7 GiB; the walls match Step 2
  (1215 vs 1238 s, 546 vs 551 s).
- **Allocator experiment** (scratchpad, our allocation pattern). The system allocator keeps 8.6 GiB after the drops,
  relief returns 0, and `man malloc` has no knob. jemalloc at its defaults returns everything (4–7 MB left per drop)
  and is faster: big phases 513 / 562 / 616 / 522 ms against 527 / 591 / 879 / 703, small allocations 85 against
  270–282 ms. mimalloc keeps 2–4 GiB; jemalloc with `dirty_decay_ms:0` takes 3.3 s on the small-allocation stress.
- **jemalloc in both host binaries** (aa311100, `L/m1`): stage-boundary footprints 80–490 MiB, swap 0 in every stage of
  three jobs, set 204 lifetime max 9.12 → 5.98 GiB; single-run walls 1259.2 / 564.8 s (+3.5 %, inside the run-to-run
  spread). The LN shared set is small: OSC +238 to +295 MiB, mono +83 to +237 MiB, seeds +12 to +34 MiB. The "8 GiB LN"
  and the "40-plane Normalize" were the retained heap.
- **Owner ruling (2026-10-17):** «оставь системные контроли памяти, ос лучше знает». jemalloc was reverted (afb87500)
  and the system allocator stays; the relief call and the 4a-0 telemetry stay. **M2 / M3** (band budgets minus shared,
  LN admission with shared) lost their premise and were not built. The memory bar became "no more swap than H in any
  stage", and a lever that raises the number of frames in flight is bracketed for swap too.

## 5. Determinism matrices

| when (head) | machine, input | arms against the reference | result |
| ---- | ---- | ---- | ---- |
| Step 3a (a7d18865) | dev Mac probe 0058 / 0075 / 0163 | threads 1 / 4 / 10 / 16 / 24 + P-only / E-only; tripwire vs 4ce275bf | identical |
| Step 3, first pass (b21c2c9b) | runner Horse C11 12-light subset | pool1 vs pool16 | **STOP D1**: 12 recalibrated `c_*.fits` with the same cards in a different ORDER |
| Step 3 (a7d18865) | dev Mac set 204 subset, 12 mono + 12 OSC, LN + drizzle 2× | pool10r, pool4, adm1, band256, capdrop (admission 5 → 1 seen), pool16, pool24, pool1 vs pool10 | PASS, 791 files, 0 differing / 0 missing each |
| Step 3 (a7d18865) | runner Horse C11 subset | pool16r, pool1, pool24, adm1, capdrop vs pool16 | PASS, 260 files + 164 metrics rows |
| after 4b (663595ac, L17 head) | dev Mac subset | pool1 vs pool10 | PASS, 791 files, 197 rows; pool16 / adm1 / capdrop not run (Time Machine, then a compile: HARD STOP held) |
| after 4b (b296dbc4) | runner Horse C11 subset | pool1, pool24, adm1, capdrop, pool16r vs pool16 | PASS, 260 files + 164 rows; capdrop "measure 16->1 (setting)" seen |
| final (23b81727) | dev Mac subset | pool1, pool16, adm1, capdrop, band256, pool10r vs pool10 | PASS (lab d7a5029: pool1, pool16, adm1, capdrop — admission lowered seen —, band256, pool10r vs pool10, 791 files each, 0 differing / 0 missing) |

D1 was pre-existing and independent of the thread count (two 16-thread runs differed too): `source_cards_for_file`
copied cards through a `HashMap`. The fix (9486bf48, Fable ruling) sorts them by keyword at both exits, as
`copy_through_cards` already did. Subset walls: pool1 963.5 s against pool10 207.9 s (4.6×); pool 16 / 24
(oversubscribed) 215.8 / 213.5 s; admission 1 241.3 s. `.athm` is exempt from the byte compare by design (it carries the
source mtime and the pass-1 timings); `.athf`, the metrics rows and every downstream artifact cover it.

## 6. The levers that landed (brackets)

| lever (commit) | machine, dataset, stage | H median | N median | N/H | spread H / N | swap H / N | identity |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| L1 (13c11124) | dev Mac hc11, Calibrate | 184.8 s | 79.8 s | 0.432 | 0 / 4 % | 0 / 0 | 106 calibrated FITS |
| L1 | dev Mac s204, Calibrate | 124.2 | 122.4 | 0.983 | 7 / 6 % | 0 / 0 | 302 FITS |
| L1 | runner hc11, Calibrate | 37.4 | 38.1 | 1.018 | 1.9 / 1.3 % | 0 / 0 | 164 FITS |
| L17 (ec547c3a) | runner hc11, Normalize group L | 27.9 | 22.8 | 0.82 | 3 / 1 % | 0 / 0 | 393 files + 164 rows |
| L17 | dev Mac s204, Normalize (OSC) | 136.8 | 125.8 | 0.920 | 5 / 0.2 % | N ≤ H | 606 files |
| L4a + L4c (2c5bd1ba) | runner hc11, Calibrate + Measure + Register | 154.7 | 149.2 | 0.964 | 2 / 5 % | 0 / 0 | 317 files + 164 rows |
| L4a + L4c | dev Mac s204, the same window | 700.2 | 701.4 | 1.002 | 3 / 4 % | 0 / 0 | 499 files |
| 4d (28305a26 → d4535980) | runner hc11, Integrate | 98.6 | 79.2 | 0.803 | 25 / 6 % | 0 / 0 | 654 files + 164 rows |
| L3 (f3859de6) | dev Mac s204, Integrate | 189.6 | 172.2 | 0.909 | 1 / 0 % | 0 / 0 | 1 310 files |
| L2 (bb240c05) | dev Mac s204, Drizzle | 86.4 | 70.8 | 0.820 | 3 / 0 % | 0 / 0 | (same run) |

- **L1: Calibrate admission from measured working sets, and a parallel master decode.** The jemalloc run measured hc11
  mono at ≈ 460 MiB per frame (1.9 planes beyond 933 MiB shared) and set 204 OSC at ≈ 3.8 planes. The constants are now
  mono 3 and debayered OSC 5 planes (were 8): admission 1 → 4 on hc11, 4 / 4 → OSC 7 / mono 10 on set 204. Distinct
  masters decode through `fan_out` (hot maps deduped first, warnings in group order). Set 204 mono stays disk-bound and
  the runner was already at 16, so neither gains. It missed the plan's ≤ 70 s by 10 s and was accepted under the owner
  bar; in the 12 GiB container it is the largest gain (§9).
- **L17: the seeds-calibration arms in parallel.** The 2 × N (frame, arm) jobs go through `fan_out`, results by job
  index, at the Normalize fan-out's own working set. Seeds span per group: runner 10.3 → 3.8 s, dev Mac OSC 59 → 50 s.
- **L4a / L4c: one queue across groups** for Calibrate and Measure pass 1, longest first, results scattered back per
  group. It merges only when the merged admission lowers no group's own; Measure pass 2 stays per group (the group β is
  a real barrier). Runner: both merged at 16 / 16 (budget 32 GiB; L4a's assumed 8 GiB was wrong), tails 31 → 6–8 % and
  27 → 7–8 %, Calibrate peak 10.96 → 13.69 GiB (every group's masters resident). Dev Mac set 204: Calibrate falls back
  (merged 7 vs per-group 7 / 10), Measure merges. Fix round r1 (da0837ed) reads the budget per fan-out.
- **L3: the Integrate band prefetch on local storage**, the existing Network shape (prefetch plus a halved band), for
  stacking only (Fable ruling). Band reads of the registered artifacts were I/O waits (1 s dips to 0.5–0.8 cores at each
  band boundary). Peaks: Integrate 6.79 → 5.89, Normalize 9.09 → 8.53 GiB. band256 covers the halved partition. Master
  builds keep network-only prefetch (final review B-I1, `IoPolicy::prefetch_local`, 23b81727).
- **L2: Drizzle reads frame i+1 while frame i deposits** (a scoped `FrameFeed`, errors held to their frame's turn), with
  one more source plane (≈ 104–114 MB on the set 204 OSC group) in `estimate_memory_bytes`; if that crosses the R-M3-7
  ceiling, Drizzle runs serially (final review B). L3 and L2 shared one bracket (`--from integrate`, H b296dbc4 / N
  b8ab0768): Register 0.981 and Normalize 0.997 level, window 485.4 → 451.2 s (0.929). Horse C11 has no drizzle; the
  runner's Step 5 (a) carries 4d + L3 together.

## 7. The 4d-0 profile and 4d

`sample` on the runner's full Horse C11 (`R/4d-profile/profile-summary.txt`; the sampler slows the walls, so read the
shares only). Integrate's non-idle samples: `partition_at_index` (the `select_nth` in `medfit_line`'s `rofunc`) **48 %**,
the `medfit_line` closure itself **31 %**, the `integrate_stack` closure (f32 ↔ f64, masks) 6 %, small sorts 3.8 %,
`combine_pixel_weighted` 3.5 %, band gather 1.6 %, LN evaluate 1.2 %, band read 0.2 %, allocation 0.2 %; `medfit_line`
is 81 % inclusive, with all 16 workers busy. The LN reference build is an Integrate on all 16 threads.

**4d** (28305a26, cherry-picked as d4535980; r1 b296dbc4). Each `rofunc` evaluation after the first takes a window from
the previous evaluation's order statistics (the median moves by at most |Δb|·(m − 1), one way). The residual build
counts `below` / `le`, and that count, not the bounds, decides exactness. NaN-capable input, a non-finite slope, a missed
window and any zero median take today's full selection, which reproduces ±0. Proof: 120 000 stacks × 10 sizes × 10
shapes, cold and warm, `to_bits` against the oracle; branch counters show every window branch runs at both parities; an
opus review proved bit-identity independently. Micro-bench new / old per call at m = 105: 0.766 cold, 0.648 warm. Runner
Integrate CPU 1 504 → 1 194 s. **Measured out** (in `combine.rs`, do not re-propose): an insertion-repaired rank
permutation (1.27–4.57× cold), the full selection under `total_cmp` alone (0.95–0.99×), a two-sided bound from the
bracket's endpoints (no narrower), bounds from the previous median's partition (1.04–1.18×), a separate count pass.

## 8. Recorded, not landed

| lever | the numbers | why |
| ---- | ---- | ---- |
| jemalloc (M1) | §4: swap 0, boundary 80–490 MiB, lifetime max 5.98 GiB, walls +3.5 % (single runs) | owner: the system allocator stays (reverted afb87500) |
| M2 / M3 | LN shared +83…+295 MiB once the retained heap is gone | premise gone |
| L4b, one Register align queue (d87c54a1) | runner Register 50.0 → 48.8 s (0.976, spread 5 / 1 %, paired −3.3 / −1.4 / −1.2 s; merged 16 / 16, tail 22–23 → 6 %); dev Mac s204 falls back (0); identity PASS | −1.2 s = 0.3 % of the run at medium cost (projected ≈ 8 s); reverted 28be0f3e |
| L15, admission above the pool (runner Measure) | after L4 the Measure tail is 7–8 % and idle workers 3–4 % → ≤ 2 s of 64 s | inside the spread; the ≥ 2 % rule cannot be met |
| L5, drizzle bands ≥ 4 × pool | ≈ −5 % of set 204 Drizzle ≈ 4.5 s = 0.4 % of the run | needs a band knob and an R-TA-10 proof |
| L6, available-memory admission with back-off | the swap was retained heap (§4); the gate keeps the capdrop path | subsumed; see the Normalize watch (§11) |
| L7, pool > 16, a Settings thread cap | 16 cap removed in Step 1; pool 24 passes on both; no machine has > 16 logical CPUs | the thread cap (a lazy pool, 74 sites) stays deferred |
| L8 / L9 / L10 / L11 | no concurrent job (`compute.max_concurrent` 1); Measure 8 planes confirmed, P-eq 4.86 of 5.5; set 204 Measure reads at 169 MB/s, CPU-bound | nothing to share; memory-bound; streaming / mmap stay notes |
| L12 / L13 / L14 | r_E 0.25 dev Mac / 0.29 runner; aarch64 NEON baseline, no x86 run | keep the E cores (L4 took the tails); SIMD a note; GPU → its own design (§11) |
| L16, drizzle frame-outer, three accumulator pairs | ≈ +1.25 GB at 2× on a 6248 × 4176 OSC group on top of a 6.62 GiB Drizzle peak | refused on 16 GB; the runner's work has no drizzle |
| Group pipelining in the output stages | after L17 no low-use phase is left to fill | no gain |

## 9. Step 5: acceptance (H = a7d18865 + the token switch, N = the final head)

Runner (a), the full Horse C11 pipeline (164 lights), H 6522d30b / N b8ab0768 (N still carries L4b, worth ≈ 1 s),
`R/step5a/notes.md`:

| stage s | calibrate | measure | register | normalize | integrate | **whole run** |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| H median (spread) | 37.6 (2 %) | 64.4 (5 %) | 50.5 (5 %) | 90.6 (1 %) | 90.7 (1 %) | **377 (1 %)** |
| N median (spread) | 34.5 (10 %) | 62.2 (1 %) | 48.7 (1 %) | 66.6 (2 %) | 77.2 (2 %) | **317 (1 %)** |
| N/H | 0.918 | 0.966 | 0.964 | 0.735 | 0.851 | **0.841** |

N's slowest round is 58 s under H's fastest. Swap 0 / 0; peaks within 32 GiB (Calibrate 11.0 → 13.7 GiB, the merged
queue). Identity PASS: 654 files + 164 rows. **Against the ERP's 2 787.6 s on the same work: 8.8× faster.**

Runner (b), the container (`--memory 12g --cpus 8`, the Horse C11 subset), H N H N H N, `R/container-arm/notes.md`:
subset total 117.6 → 105.0 s (0.893; N's slowest under H's fastest). Calibrate 23.3 → 12.8 s (L1: the 3 GiB budget
admits 1 frame with H's 8 planes and 4 with N's 3); the other stages ≈ 1.0. No OOM in 6 of 6 runs (`memory.peak` = the
limit, page cache reclaimed). The budget line shows `mem_limit` 12 GiB and `cpu_quota` 8. N's `memory relief` (Linux
`malloc_trim`) releases 0–36.8 MB per call, 153–176 MB per run. Identity PASS: 260 files + 164 rows.

Dev Mac (H acc-base 52350b03, N 23b81727, `--rounds 2`; `task-13c-report-step5-devmac.md`; beside set 204: the ERP's
1005 s, run 2 968 s, and Step 2's 1237.7 s):

| dataset | stage | H ×2 | N ×2 | N/H | spread H / N | swap H / N MB | peak H / N GiB |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| set 204 | calibrate … drizzle, whole run | 1166 / 1174 s | 1078 / 1083 s | **0.923** (1170 → 1081 s) | 0.6 % / 0.4 % | 104 / 4 (max stage) | 9.12 / 8.66 |
| set 204 | normalize (the swap watch) | 3.04 min | 2.78 min | 0.917 | 0 % / 2 % | 104 / 0 | 9.12 / 8.66 |
| Horse C11 | calibrate … integrate, whole run | 537 / 533 s | 387 / 388 s | **0.725** (535 → 388 s) | 1 % / 0 % | 788 / 300 (integrate) | 10.26 / 10.34 |
| identity | both datasets | PASS: set 204 1310 files + 197 rows, Horse C11 426 files + 106 rows, 0 differing | | | | | |

Verdicts: whole run N < H on both datasets; swap N ≤ H in every stage; identity PASS. Two set 204 stages printed
N/H above 1 outside a 2-round spread — calibrate 1.029 and register 1.018; **Fable ruling** (`task-13c-ruling-fable-step5.md`):
PASS, bracket noise — calibrate's +2.9 % is the OSC group at admission 7, inside the same code's 92.3–101.1 s range
across the earlier 3-round brackets (pooled admission 4 vs 7: 0.98); register has no code behind it (identical to
b296dbc4). Pre-registered flip: three interleaved pairs with Calibrate N slower in every pair beyond a printed spread
≥ 3 % → cap OSC admission by write bandwidth as a next-cycle lever, never a revert. The Calibrate peak (N 3.88 vs H
2.10 GiB on set 204; 3.52 vs 1.62 Horse C11) is L1's admission formula by construction (7 × 497 MB + shared ≈ the 4 GiB
budget, peak / budget 0.97, swap 0, identical in the L1 / L4 brackets) — not a leak.

## 10. The owner bar per machine

| machine, dataset | ours | ERP, same machine and work | standing |
| ---- | ---- | ---- | ---- |
| runner, Horse C11 (164 lights) | 317 s (Step 5 a, N) | 2 787.6 s | **MET, 8.8×** (Step 2: 7.0×) |
| dev Mac, set 204 (197 lights) | **1081 s at the final head** (Step 5 N; 1105.5 s at b296dbc4) | 1005 s (968 s) | **NOT MET**: +76 s (+7.6 %); Measure +404 s is the gap (Integrate 172 s vs 113 s) |
| dev Mac, Horse C11 (106 lights) | 551.3 s (Step 2); **388 s at the final head** (H 535 s) | no same-machine ERP run | no bar |

The next cycle (owner order, 2026-10-17: 13c, then a fast-measurement cycle, then Task 14) takes set 204's gap:
- **Measure's +404 s is a numbers decision.** We fit every seed (≈ 20–24 k per mono plane); the ERP's detector completes
  at a matched-filter SNR of ≈ 18–28 against our 6 and fits ≈ 5 000–8 600 stars (`task-13c-report-research-starcount.md`).
  A detection threshold was measured out (it re-defines the weight unevenly). The cycle builds `measurement.fitStars`
  (All | K brightest, default All), shipped only past a truth bar, beside a GPU prototype (§11). The Fable review puts
  Measure at 280–410 s with the verdict fits kept on the CPU.
- **Integrate's +75 s is CPU work left after 4d and L3.** The GPU's combine kernel is measured first; the review projects
  ≈ 1× the post-4d CPU on the dev Mac.

## 11. Open items

- **The Normalize swap watch.** The b296dbc4 single run swapped 911 MB in Normalize, against 91–761 MB in H-level single
  runs; L17's concurrent arms on the retained heap are a plausible cause. The combined bracket's Normalize swapped 0 / 0.
  Step 5's interleaved H / N decided it: set 204 Normalize swapped 104 MB (H) vs 0 MB (N) in both rounds — N ≤ H; the
  911 MB was a single-run draw. Closed.
- **The master-build prefetch is unmeasured.** Master builds keep network-only prefetch; a halved, prefetched band on
  local storage for `integrate_bias_like` / `integrate_flat` was never measured (raw sets often sit on USB / HDD).
  Measure a master-build H / N pair before extending L3 there.
- **The GPU design** for the fast-measurement cycle: `task-13c-report-research-gpu.md` (wgpu + WGSL behind a `gpu`
  feature, f32 with float-float sums; Kernel 1 = per-star fits, Kernel 2 = the Integrate combine; `compute.gpu` default
  off; the Linux desktop via Vulkan) and `task-13c-review-fable-gpu.md` (ready with fixes, 2 C / 6 I / 8 M): measure
  Kernel 2 first, one subgroup per stack (it is ≈ 4–7× more work than counted); per-star paired bars at 2× a CPU-control
  self-spread, in σ units; Metal's safe math mode; Vulkan first on Windows and Linux; order fitStars K → Kernel 2
  measurement → Kernel 1 at the post-K star count. Its plan goes to Fable before it runs.
- **Untested platform paths.** Linux, macOS 11–12 (the `RUSAGE_INFO_V4` fallback) and Windows discovery are tested only
  through their parsers. Windows `available_memory` ignores a job memory limit (telemetry only). No x86_64 SIMD run (L13).
- **Done (controller):** the final dev Mac matrix PASS (lab d7a5029: pool1, pool16, adm1, capdrop — admission lowered seen —, band256, pool10r vs pool10, 791 files each, 0 differing / 0 missing); the squash onto `one-star-path` with the full `--all-targets` before its push.
- **Owed (owner):** the Step 2 App Nap run (a Measure-only run of the DESKTOP build with its window hidden) was never
  run. Code check: the desktop host takes no App Nap / power assertion, so a hidden window may be napped (background QoS
  → E-cores; the ladder measured 6.3× slower per thread). Measure it in the desktop GUI; the fix, if needed, is an
  `NSProcessInfo` user-initiated activity held for a run's lifetime (macOS desktop only, numbers unchanged).
