# Fast-Measurement Cycle — Implementation Plan (2026-10-18)

> **For agentic workers:** REQUIRED SUB-SKILL: `lean-execution` (it overrides superpowers:subagent-driven-development
> wherever the two conflict): one phase per agent, a one-page handoff, a report of ≤ 80 lines, fix rounds to a fresh
> agent, the controller restarts at task boundaries. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal.** The WHOLE set 204 run on the dev Mac (Masters → Output) beats the reference product ("ERP") on the same frames,
machine and work: today **1081 s against 1005 s** (run 2: 968 s), and Measure is the gap: **≈ 477 s against the ERP's
≈ 72 s of star descriptors alone, or its warm-up-corrected 226.4 s** (calibration PSF signal + descriptors + LN fits —
the work our Measure serves; R-OSP-12's comparison, review M2). The whole-run bar is unaffected by which figure is read.
Two orthogonal routes, both in scope (owner order 2026-10-17: Task 13c → this cycle → Plan A Task 14):
1. **`measurement.fitStars`** — All | the K brightest seeds get the PSF fits; detection and `stars_detected` unchanged;
   Register and LN read the fitted list as today. A stacking-config field; a Settings → Stacking field, panel control and
   preset entry ONLY if a K passes (FM-19); **the default stays All**; K joins the "Fast preview" preset ONLY after it
   passes the truth bar. It changes numbers, so truth decides K.
2. **A GPU prototype** (wgpu + WGSL, f32, the CPU stays the reference and the fallback) for the fixed-β fits (Kernel 1)
   and the Integrate linear-fit combine (Kernel 2). It intends NO numeric change: tolerance-gated against the CPU, and
   productized only behind a cargo feature and a `compute.gpu` setting (`off` default | `auto`).

**Architecture.**
- **athenaeum-core**: `stacking/config.rs` (`MeasurementConfig::fit_stars`, the hash fold-ins, the preset), `star_measure/`
  (`Mode::Brightest { k }`), `stacking/measure.rs` (the adapter maps the config to the mode); a `gpu-proto` feature for
  the prototype example only, then (conditional, Task G6) a `gpu` feature and a `gpu/` module the Measure, Normalize and
  (if Kernel 2 passes) Integrate stages call through one engine switch.
- **athenaeum-lab** (`star-lab/`): the cut table's tools, a dense truth family whose seed filter-SNR distribution matches
  the real planes, the fitStars truth row (from `results/plan-a/t13b-truth/truth_row.py`), the `fm.sh` acceptance cycle
  (from `harness/acceptance/t13c.sh`). Lab tools are committed there; Rust probes stay in `crates/athenaeum-core/examples/`.
- **Frontend**: the Measure panel and Settings → Stacking (the fitStars field); Settings → Stacking → Compute (`compute.gpu`,
  desktop only) — only in the tasks that ship them.

**Tech stack.** Rust 1.96.1 (wgpu 30.0.1, MSRV 1.87, MIT/Apache; pollster; bytemuck 1.25.2 already in the lock), WGSL,
Python 3.14 + numpy 2.4 (lab, `unittest`), React/TS, `sqlite3` with `immutable=1` for acceptance copies.

**The design (read, never re-researched).**
- `.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-13c-report-research-starcount.md` — Q3 consumers, Q4 the truth
  bar for K, the verdict (no detection threshold; a fitted-star budget).
- `.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-13c-report-research-gpu.md` (+ its Linux / Vulkan amendment) and
  `task-13c-review-fable-gpu.md` (READY WITH FIXES: C1, C2, I1–I6, M1–M8 and its 8-point list — every item is folded below;
  the coverage table at the end maps each one).
- `.superpowers/sdd/2026-10-18-fast-measurement/fm-review-fable-plan.md` — the plan review (READY WITH FIXES: C1, I1–I6,
  M1–M8, rulings D1–D6, Q2, Q3; Q1 answered by the owner 2026-10-18, see A1) — folded as FM-13…FM-21 and in place; the second coverage
  table maps each item.
- `docs/superpowers/research/2026-10-18-task-13c-parallelism.md` §3 (baselines), §9 (Step 5), §10 (the gap), §11.
- `docs/stacking/README.md` R-OSP-12 (route A: every seed fitted, numbers = 5836a970; what was REFUSED and why) and
  R-OSP-13 (parallelism; the owner bar); Tier C spec §8 (`docs/superpowers/specs/2026-09-20-stacking-compute-tierC-design.md`).

**Places.** Code branch `one-star-path-fm` from `one-star-path` (78fdc673); one squash onto `one-star-path` at the end;
push only on the owner's word. Ledger, handoffs and reports: `.superpowers/sdd/2026-10-18-fast-measurement/`
(`fm-<task>-handoff-rK.md`, `fm-<task>-report-rK.md`). Lab results: `athenaeum-lab/star-lab/results/plan-a/fast-measure/<task>/`.
Before trees (dev Mac): set 204 `~/.athenaeum-acc/tierA-lnB-f1-13c`, run B `~/.athenaeum-acc/lnB-runB-f1`, Horse C11
`~/.athenaeum-acc/hc11B-f1` (106 lights); the runner holds Horse C11 (164 lights) only. Exchange:
`/Volumes/Universe/athenaeum-runner/{git,acc,handoffs}` and GitLab.

**Where the time goes today** (set 204, dev Mac, 197 lights = 92 mono + 105 OSC = 407 planes of 6248 × 4176; ours =
b296dbc4 single run, the final head's whole run 1081 s; ERP = the owner's run 1):

| stage | ours s | ERP s | | stage | ours s | ERP s |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| calibrate (+ cosmetic, debayer) | 120.3 | 156 | | normalize (LN) | 168.1 | 351 |
| **measure** | **476.5** | **72** (descriptors) / **226.4** (warm-up-corrected, R-OSP-12) | | **integrate** | **≈ 172** (L3) | **113** |
| register (+ reference) | 66.5 | 134 | | drizzle | ≈ 71 (L2) | 170 |

Projected (F0 recomputes them; never a bar): fitStars K = 5 000 → Measure ≈ 275–305 s (55–65 % of Measure is
count-driven; mono fits × 0.22, OSC × 0.41), whole run ≈ 880–910 s; Kernel 1 alone (fixed-β fits only, verdict on the CPU,
I1) → Measure ≈ 260–385 s. Kernel 2 has NO projection until G1 measures it (C1 d: Fable expects ≈ 1× on the dev Mac).

---

## Global constraints (owner, standing)

- **Plan by measured quantity.** One owner task per shared input (the cut table: F0; the dense truth family and the true
  weight (its definition ruled by FM-21): F2; the per-star GPU bars: G2; the post-K star count: F3/F4).
  Consumers read those files and never re-estimate.
- **A numeric change is judged on the WHOLE group at master level** (set 204, run B, Horse C11), never on a few-frame
  probe. **Quality over speed**: a speedup that misses a bar is reverted with its numbers, never tuned toward the bar.
  No K, threshold or tolerance is chosen after its numbers are seen; every bar below is fixed now.
- **Truth via athenaeum-lab** (star-lab, inject). The ERP is printed beside, never the referee. Report pooled rows
  (all rows vs truth, ERP beside), never a selected subset.
- **Results-only disk rule** (every machine): a job deletes its clones, worktrees, dumps and targets after its results
  are committed in the lab; before trees are never deleted. Dev Mac: one heavy job at a time, ≥ 100 GiB free floor
  (490 GiB free on 2026-10-18); a fresh core debug target ≈ 11 GB, a release probe build ≈ 6 GB; `df` before every build.
- **No third-party product or project names** anywhere in the repo (code, comments, docs, this plan, commit messages):
  "the reference product" / "ERP".
- **Bit-identity where claimed.** `fitStars = All` and `compute.gpu = off` (and every build without the feature) leave
  every artifact byte and every stage hash literal of 78fdc673 unchanged — proven, not assumed.
- **The GPU path is tolerance-gated**: behind a feature, `compute.gpu` off | auto, the resolved engine folded into the
  stage hashes, provenance with backend / adapter / driver / kernel version / reduction path.
- **Never swallow errors**: no adapter, a lost device, a kernel error or an unreadable dump → `warn!`/`error!` and the CPU,
  never a silent default. New log fields go into the logging spec's dictionary first.
- **Models** (lean-execution): `haiku` for a proof run whose gates are fixed in advance (exact commands, hard STOP, no
  destructive step); `sonnet` for scripted work, implementation and judgment runs; `opus` for numerics, design reads
  and reviews; Fable when an agent struggles or a STOP needs a ruling. Path-of-development questions go to a fresh Fable
  agent, not the owner; scope and cost questions go to the owner (notify, recommended option first).
- **Gates for every code task**: `cargo check -p athenaeum-core --all-targets`, `cargo check -p athenaeum-core
  --no-default-features`, the touched modules' tests filtered (`| grep -E '^test result|FAILED|panicked'`), both hosts
  (`cargo check -p athenaeum-tauri -p athenaeum-web`), `rustfmt <files>` by path (clippy is not a gate); UI tasks add
  `tsc`, vitest and the settings-registry coverage test. Before any push: the full `cargo test -p athenaeum-core
  --all-targets`.

## Machines

| machine | what it is | runs here |
| ---- | ---- | ---- |
| **dev Mac** | M4, 10 cores (4 P + 6 E), 10-core GPU, 16 GiB; the owner's bar machine | code, set 204 + run B checkpoints, G1–G3 GPU timing, the owner-bar run |
| **Mac runner** | M4 Max, 16 cores (12 P + 4 E), 40-core GPU, 128 GiB; Remote Control "Stopped backup"; ERP installed; Horse C11 (164) only | lab cycles (Python), the Horse C11 checkpoint, the second GPU machine |
| **Windows runner** | Remote Control "Athenaeum win-runner" (OBS-PC1): Core Ultra 7 258V, 8 cores, 32 GiB, Arc 140V (Vulkan 1.4), D: 479 GB free, **no Python**, `cargo -j 4`; Universe via `\\192.168.31.208\Universe` (read + write) | the Vulkan and DX12 arms, x86_64 CPU timings, the Windows desktop sanity run |

**Windows runner (filled by the controller 2026-10-18 from the runner's own inventory):**

```text
WIN_REACH        = Remote Control session "Athenaeum win-runner" (confirmed 2026-10-18; no ssh fallback)
WIN_WORK         = D:\fm-work\<task>-<yyyymmddHHMM>\               (every run creates and deletes ONE such dir)
WIN_TARGET       = %WIN_WORK%\target                                (CARGO_TARGET_DIR; never a shared target)
WIN_UNIVERSE     = \\192.168.31.208\Universe\athenaeum-runner\     (inputs: dumps, planes; outputs: jsonl + report)
WIN_FREE_FLOOR   = 60 GB on D: (479 GB free on 2026-10-18)         (checked BEFORE the run; below it = STOP, nothing created)
WIN_TOOLCHAIN    = rustc 1.96.1 x86_64-pc-windows-msvc (rust-toolchain.toml), VS 2022; cargo ALWAYS -j 4 (a full-parallel build OOMs)
WIN_DESKTOP_DEPS = node v24.18.0; WebView2 runtime unchecked       (only for G7's desktop sanity run)
WIN_GPU          = Arc 140V (integrated, shared memory), driver 32.0.101.6737, Vulkan 1.4.309, DX12 FL 12_2; `vulkaninfo --summary` saved to the report dir
                   (vulkaninfo ships with the SDK, not the driver: if absent, G0's own adapter report is the record)
Pre-run:  (Get-PSDrive D).Free / 1GB  >= WIN_FREE_FLOOR, else STOP
Pre-run:  the "Windows release runner (astro)" gitlab-runner service takes release/tag jobs (concurrent 1) — no job
          running, else wait; never stop the service. The old C:\...\athenaeum\target (110 GB) is not ours: never touch it.
No Python on this machine: Windows tasks are Rust-only (probe binaries write jsonl; the analysis runs on a Mac).
Post-run: Remove-Item -Recurse -Force %WIN_WORK%; print free space before / after in the report
```

Outputs from Windows are `jsonl` written by the Rust example (no Python there); every analysis runs on a Mac.

---

## Plan rulings (where this plan is more precise than the design)

- **FM-1 Order (Fable I6, amended).** F0 → F1 → F2 → F3 decides K → F4 real-data checkpoint → F5 product finish; G1
  (Kernel 2 op count + occupancy, 0.5 day, its ≥ 3× stop rule applied) runs only after F3 and before ANY Kernel 1 code;
  G2 dumps stamps only after K is known; G3 prototypes Kernel 1 at the post-K star count. G0 (the harness: feature,
  example skeleton, adapter / timestamp / two-sum checks, no kernel) may be written while the runner executes F2/F3, with
  the dev Mac builds serialized by the controller. 4d is already merged (d4535980), so G1's CPU oracle is post-4d.
- **FM-29 (controller, 2026-10-18) G1 on the dev Mac may start before F3 ends.** Kernel 2 is Integrate's linear-fit
  combine: it does not read the fitted-star list, so K cannot change its inputs, its CPU oracle (post-4d) or its stop
  rule; the dev Mac is quiet while F3 runs on the runner. What FM-1 protects stays: no Kernel 1 code before G1's verdict,
  G2 dumps only after K is known, and the runner's G1 numbers after F3. G1 is split by lean-execution into G1a (sonnet:
  the `gpu-proto` recorder, `--dump-stacks`, the dumps and the raw `rofunc` evaluation counts) and G1b (opus: the op
  model written BEFORE timing, the kernel, occupancy, timing, the SG1 verdict).
- **FM-30 (owner-approved 2026-10-18) Per-core efficiency before GPU: S1 + S2.** The set 204 gap sits in two stages
  only (Measure +405 s, Integrate +59 s; the other four stages are 1.3–2.4x faster than the ERP). Integrate runs the SAME
  algorithm as the ERP (the minimum-absolute-deviation line by slope bisection, a median of residuals per evaluation), but
  the ERP's is hand-vectorized (element-wise SIMD residuals, a branchless three-way partition for the selection, integer
  sums) with results pinned bit for bit; ours is scalar. 13c left SIMD as a note (L13) and G1 went to the GPU first —
  the wrong order. S1 = a bit-identical SIMD / branchless Integrate selection, written from first principles (the
  reference product's source is never copied or paraphrased into the repo); S2 = a per-unit cost audit of Measure (cost
  per fitted star and its hot loops vs the ERP's per-star cost from its logs), read-only first. Both bit-identical; S1's
  bar is §8 identity + a set 204 Integrate bracket.
- **FM-31 (controller, 2026-10-18) G5 decided by measured projection: SG6 fires, the GPU star-fit path ends.** After S1 +
  S2b the set 204 run is 847 s (lab 4b4fc4e), so SG6's bar is ≈ 127 s. The GPU ports only the fixed-β fits (FM-21 / Fable
  I1). Measured shares (lab `fast-measure/g5pre/`, ec1310b; `sample` on the 24-frame subset at 99f899ce): pass-2 fits ≈ 21 %
  of Measure (318 s), LN target refits ≈ 17 % of Normalize (132 s) → T_gpuable ≈ 89 s (81–96 s). The saving bound
  (1 − 1/s)·T is 59 / 71 / 80 / 89 s at s = 3 / 5 / 10 / ∞ — even an infinitely fast GPU stays at 10.5 % (upper range 11.3 %)
  < 15 %. G1 measured the M4 GPU at ≈ 30 % of nominal against a now NEON-vectorized CPU fit, so the credible s is 2–5.
  G2, G3, G4, G6 and G7 are not run; `gpu-proto` (feature-gated, never in a default build) stays as the measured-out
  record until A2 decides its fate. Verdict fits (48 % of Measure CPU) stay CPU by ruling — the remaining big lever is
  number-changing (vectorized `pow`, ≈ 31 % of the fit thread), a truth-gated item for a later cycle.
- **FM-2 What `fitStars = K` means.** The first K seeds of the detection order (filter flux descending, `seeds.rs:586` —
  the order the 24 576 cap already truncates by, so `K ≥ maxStars` IS All) get the verdict fit and the pass-2 fit; the
  rest keep their detection record with `Outcome::NotSelected` (the budget's existing outcome). `stars_detected` is
  unchanged; `stars_fitted` ≤ K. **No imputation** (R-OSP-12 refused it): TFlux, TMeanFlux, PSFSW, frame FWHM / ecc,
  `c` and the F-6 medians are over the fitted set. The Auto β sample (the 64 brightest unflagged verdict fits) lies inside
  K on every real plane by F0's table, so plane β and group β are byte-identical (a gate, not an assumption). `.athf`
  holds the accepted fits within K, so mono Register and LN read the shorter list with their algorithms unchanged. The
  Analysis path (`Mode::Budget`) is untouched. **The order is ruled (FM-16): flux order**, not a filter-SNR order. The
  filter SNR is `f / sigma_at(cx, cy)` (`seeds.rs:571`), a LOCAL σ, so the two orders differ wherever the sky varies
  (vignetted corners, nebula cores); a filter-SNR order would un-fit stars over bright nebula first, exactly where LN
  and the weight care. F0 reports the share of the first K a filter-SNR order would swap as a column of the cut table —
  a reported number, not a STOP (SK0 retired).
- **FM-3 Hashes.** `fit_stars: Option<u32>`, `None` = All, `#[serde(default, skip_serializing_if = "Option::is_none")]`,
  so a stored or default document at All serializes exactly as today. `resolve_config` clamps K up to the **floor =
  `max(MIN_FIT_STARS, registration.max_stars)`** (= 2 000 at the defaults) with a `warn!` (as
  `seed_threshold` is clamped, `config.rs:619, 730`), then canonicalizes K ≥ `measurement.max_stars` to `None`
  (equivalent configs share hashes). The floor exists because mono Register takes Measure's fits whenever they exist
  (`register/frame.rs:128–146`), sorts by the fitted `signal`, truncates to `registration.max_stars` and never falls back
  to detection (`register/detect.rs:228–252`): a K below its budget would starve Register with no log (review C1 b). The
  probe stays unclamped (the lab's K_lab). `resolve_config` also warns when `selection.min_stars > fit_stars`
  (`selection` and `integrate.rs:1187` read `stars_fitted`, R-OSP-12 (6): a stored `minStars` above K excludes every
  frame; review M4).
  K ≠ None folds into every subtree that reads Measure's output: `measurement_subtree` (automatically, it serializes
  `cfg.measurement`), and, through the upstream `measure:<hash>` of `registration_hash_for` / `normalization_hash_for`, Register and
  Normalize — no explicit fold (an explicit fold would duplicate the key and, serializing `None`, move the All literals), and any other subtree F0's grep of `MEASURE_VERSION` / `measurement` fold-ins finds. `MEASURE_VERSION`
  stays 4. A test pins every stage-hash literal at All against 78fdc673.
- **FM-4 The cut is a measured quantity (the corpus trap).** The v2 corpus holds at most **4 194 stars per frame**
  (`density/d1000`, 2048²; 170 families, default 629): K = 5 000 is a no-op on every lab frame, K = 4 000 cuts only
  d1000 and K = 2 000 only d600 / d1000. This is 13b's trap again ("at the default M the corpus imputed nothing
  (0 / 187 frames), so the first truth row was no verdict"). So: F0 owns the **cut table** (per real plane class — set
  204 mono wide / mono sharp / OSC, run B mono / OSC, Horse C11 L and colour planes: N_seeds, the filter SNR at rank K
  — **the median filter SNR over ranks K ± 1 % (a band), its spread printed** (FM-16) —, q = K / N_seeds, H's TMeanFlux share beyond rank K, Register's
  usable stars at K); F2 owns a **dense truth family** whose seed filter-SNR distribution matches each class; F3 applies the
  bar at the **filter-SNR-matched cut** per lab frame (primary), with the q-matched cut printed beside (not a bar). Every
  lab frame (dense or ordinary) is mapped to one real class by F2a (FWHM tier, mono / OSC); its cut is that class's filter
  SNR at rank K, passed to the probe as a per-frame count K_lab = the seeds at or above that SNR (from the H arm's
  diagnostics). The dense family carries K1 / K2 (a realistic faint population); every family carries K3.
- **FM-5 The K grid is pre-registered: 4 000, 5 000, 8 000 (review C1).** K = 2 000 is gone: K7 holds only if every one
  of H's `registration.max_stars` (2 000) Register stars lies inside the first K seeds, and they sit at seed ranks
  3 270–3 575 on set 204 mono (research starcount:48), so K = 2 000 fails K7 by construction and its F3 arm would buy a
  trend line and nothing shippable. **The lowest point is lifted by F0's bound** B_reg = the max seed rank of H's Register
  selection over the mono planes of set 204, run B and Horse C11 (1-based; a K holds K7 only if K ≥ B_reg): if B_reg
  exceeds 4 000, the lowest point becomes ⌈B_reg / 1 000⌉ × 1 000 (a structural number read from F0 before F3 — not
  tuning); any other grid point below B_reg is lifted the same way and duplicates collapse; if B_reg exceeds 8 000, no
  pre-registered K can hold K7 and F3 is not run (SK2 with F0's numbers). All remaining points run in F3. The shipped K
  is the smallest that passes every F3 row AND every F4 row; F4 runs the smallest F3-passing K first and moves to the
  next F3-passing K only if a real-data row misses. No other K is ever tried in this cycle. None passes → nothing user-facing ships (FM-19) and the GPU is the speed path.
- **FM-6 The referee for the weight is truth.** §8's per-frame weight row (Spearman ρ ≥ 0.99 vs H, top-20 ≥ 18/20) is
  reported in F4; a miss there with every truth row passing goes to Fable, not to an automatic fail (R-OSP-12: "it was
  the truth row, not ρ against H, that refused them"; FM-17). The truth referee is `w_true` (FM-21). The real-data
  per-frame shape row K11 follows the same pattern (a median beyond ± 1 % with K3 / K4 passing → Fable, never silently
  past). Every other §8 row (master median / MAD / noise / FWHM / rejected fraction, LN `s` median AND scatter — K8) is
  hard.
- **FM-7 `compute.gpu` is a KV setting, not a stacking-config field** (deviates from the research's §4 on purpose):
  whether this machine's GPU runs is a machine capability, like `compute.memory_budget_mb`, not a recipe that travels
  with a frame set or a collab project's stacking config. Settings → Stacking → **Compute**, desktop only (hidden on the
  web build), `off` (default) | `auto`. The run resolves the engine once at start (`cpu` | `gpu:<kernel_version>:
  <reduction_path>`) and folds it into the Measure and Normalize subtrees (and Integrate if Kernel 2 ships) ONLY when it
  is a GPU engine, so `cpu` leaves every hash byte-identical. Confirmed at plan review (FM-13).
- **FM-8 GPU failure is per stage, never mixed (amended by FM-14).** Adapter-level refusals — no adapter, an adapter
  outside the validated list (FM-9) — resolve at run start (engine `cpu` for the whole run, `info` / `warn!` with the
  cause) and are never a mid-stage event. A lost device or a kernel error mid-stage → `warn!` with the cause and the stage
  re-runs wholly on the CPU with engine `cpu`; **the demotion holds for the REMAINDER of the run** (a device lost in
  Measure makes Normalize and Integrate `cpu` too: one `resolve_engine` per run, then demoted, no per-stage re-try).
  GPU artifacts of a stage that failed over are discarded and **never reach `stacking_artifacts` under their `gpu:`
  hash** (G6 either deletes the stage's per-frame rows on fail-over or commits the stage's rows only at stage end; the
  test pins the outcome), so one stage hash never covers two engines and a later `auto` run never reuses half a stage.
- **FM-9 `auto` uses only validated adapters.** A compiled-in list of (OS, backend, vendor family) that passed G3/G4's
  paired bars and G7's checkpoint on real hardware: candidates Metal / Apple (dev Mac + runner) and Vulkan / Intel on
  Windows (the Windows runner). **Vendor family = the wgpu adapter `vendor` id (+ `device_type`)**; the list and the
  provenance record the validated driver version. An unvalidated model of a validated vendor is admitted by construction
  (any subgroup width, determinism per device) — only its bytes were never measured (review M6). Everything else
  (Linux — no GPU machine exists —, DX12, other vendors) resolves to `cpu` with an `info` line `gpu adapter not
  validated` (FM-15). Backends: Vulkan first on Windows and Linux, DX12 without subgroups as the fallback, never GL
  (Fable I4).
- **FM-10 Kernel scope.** Kernel 1 = the fixed-β 7-parameter conditioned Moffat LM (`fit_moffat_2d_fixed_beta_conditioned`:
  Measure pass 2 + LN's refits). The verdict pass (`fit_moffat_2d_bounded`, `photometry.rs:464`) stays on the CPU for good
  (Fable I1), so β, the β-sample membership and the SATURATED flags stay byte-identical. Kernel 2 = the Auto linear-fit
  combine (n ≥ 20) only. Every sum is an explicit `subgroupShuffleXor` butterfly; the workgroup-memory path is REQUIRED
  (any subgroup width 8–64) and mirrors the same tree; the reduction path is part of the kernel version (Fable I3).
- **FM-11 The owner-bar run.** Config = the ERP-equivalent work (the owner's: same masters, manual reference, LN, drizzle
  2×) plus the shipped `fitStars` K and / or `compute.gpu = auto`; defaults stay All / off. Whether the owner accepts a
  bar met with a non-default setting was the owner's call (Q1), answered 2026-10-18 (A1): the bar is met at the owner's configuration — the Fast-preview K if K ships and / or `compute.gpu = auto` — with the defaults All / off; do not re-ask.

### Plan-review rulings (source: Fable, `.superpowers/sdd/2026-10-18-fast-measurement/fm-review-fable-plan.md`, 2026-10-18)

- **FM-13 (D1 confirmed) `compute.gpu` is a KV machine setting.** A stacking config travels with a frame set and a
  collab project (the project twins); a GPU is a property of the machine, not of the recipe; the hash folds the RESOLVED
  engine, so cache correctness is untouched (FM-7 stands).
- **FM-14 (D2 confirmed, amended by review I5).** The whole stage re-runs on the CPU, the demotion holds for the rest of
  the run, and the GPU artifacts of the failed stage never persist under their hash (FM-8 as amended; test
  `demotion_holds_for_the_run`, G6).
- **FM-15 (D3 confirmed).** `auto` is a promise of validated numerics; GPU bytes differ per device and are
  tolerance-gated only where measured, so an unvalidated adapter resolving to `cpu` with an `info` line is the honest
  default. The owner is told plainly (A2's summary): Linux builds carry the feature and run `cpu` until a Linux GPU is
  validated on real hardware.
- **FM-16 (D4 confirmed, SK0 demoted, review I2).** Flux order — the cap's order (`K ≥ maxStars` = All holds),
  position-neutral; the SNR / flux difference is a reported column of F0's cut table, not a STOP; the matched cut is a
  rank band's (K ± 1 %) median filter SNR, its spread printed (FM-2, FM-4).
- **FM-17 (D5 confirmed).** K10 is report-only with every rank move listed; R-OSP-12's precedent stands ("it was the
  truth row, not ρ against H"); truth (K1–K3 with FM-21) is the referee, a miss goes to Fable (FM-6).
- **FM-18 (D6 confirmed).** G-e is 19/20 with every rank move listed and ρ ≥ 0.999 (one saturation-edge flip moves a
  frame 0.1–1 %, GPU review M7).
- **FM-19 (Q2) If no K passes, nothing user-facing ships.** `fit_stars` stays in the config (decoded, clamped, for the
  probe and the lab's `--fit-stars`); no Settings field, no panel control, no preset, no help text — a field whose every
  measured value failed truth is a lever against the weight's quality ("quality over speed — revert"; research
  starcount:80 says the same). F5 is then skipped.
- **FM-20 (Q3) SG6's basis is All only: 15 % of the whole set 204 run (≈ 160 s).** The GPU's reason to exist is All's
  numbers, faster; `auto` is opt-in and a Fast-preview user chose speed already. The shipped-K saving is printed beside,
  never the basis.
- **FM-21 (review I1) The weight's truth referee is fixed now, not by F2a.** `w_true = (1 / s_true)² / σ_true²` IS the
  K2 referee — the inverse-variance optimum for extended signal, which is what a stacking weight optimises; sharpness is
  Register's and Drizzle's axis. `w_pt = w_true / FWHM²` is printed beside, report-only. Because PSFSW rewards sharpness
  and K's known definitional effect is a wide / sharp tilt (research Q4: dropping filter SNR < 20 moves TMeanFlux 29 %
  wide vs 14 % sharp), F2's series draw FWHM independently of (s, σ) (never co-varying, else ρ is blind to the tilt),
  and **K2b (report)** prints the tilt itself: median PSFSW_K / PSFSW_H per FWHM tier at fixed (s, σ).

### F0 rulings (source: `.superpowers/sdd/2026-10-18-fast-measurement/fm-f0-rulings.md`, 2026-10-18)

- **FM-22 (R1, S3) Horse C11 is All-only; its F4 K arm is dropped.** Its stored config is the default sigma (`detectionSigma` 20); only `maxStars = 500` differs. With the cap lifted to 24 576 the L plane 0000 detects 1 890 seeds (1 693 fits), the G plane 0000 938, both below 4 000, so every grid K is All there. F4 = set 204 + run B; Horse C11's line is "K6/K7/K9/K11 identity by construction (F0: 500 capped, 1 890 / 938 uncapped)". K6, K11 and F4 are amended; Horse C11 stays in G7, where All is the basis (FM-20).
- **FM-23 (R2, S4) Set 204 + run B suffice; set 174 is a conditional third arm.** Every set of 40 or more lights but 109 / 174 / 204 has no calibration links (linking is owner work, outside this cycle). Set 174 "LDN 1217 / vdB 152" (ASI2600MC Duo OSC, 6248 × 4176, 525 lights over 7 nights, 180 s, all linked, `/Volumes/bigbase2`) has 24 576 seeds = the cap on all three planes, so K bites; OSC only (no K7, no mono K8), its value is an independent OSC K8 / K10 read. It runs ONLY if a set 204 OSC group puts K8 within 2× of a bar (median |r − 1| > 0.05 % or scatter > 0.5 %) or K10 moves a top-20 rank: one night (2024-10-05, 146 lights; or 10-04 + 10-06, 118) via `excludedFrameIds`, reference `manual`, under 30 min. Otherwise F4's report states the independence caveat verbatim.
- **FM-24 (R3, C2) c reads the fitted set on both sides; K8 guards LN with an attribution column.** `Mode::Brightest` reuses the budget seam: the ring-sky mask and c's isolation run over every detected seed, so B_i and isolation are H's; only the ring width F (over the K in-statistic fits) and c's 64 candidates can move (F1 writes this as a required line). Guard: K1 + K3, and F3's truth row prints c_K / c_H and F_K / F_H per class (report, no bar). LN target side (code unchanged) reads the `.athf` list truncated to `measurement.max_stars` as mask + isolation, so under K the list is the K-prefix, the ONE place K changes the isolation population (about 20 k to about 4 k). K8 stays hard; F4 adds per frame LN target-side c_N / c_H and F_N / F_H from the `ln scale` debug event (F1 adds the same two fields as one `debug!` on the PerSide target fit). Pre-registered fallback if K8 misses with the column pointing at c: `.athf` carries the NotSelected seeds' positions for LN's mask / isolation (`FITS_ARTIFACT_VERSION` 1 to 2), not a K change. No full-list refit, no imputation (FM-2).
- **FM-25 (R4, C1) TMeanFlux depth is F3's truth question, not a STOP; the family must carry the depth.** PSFSW = TFlux · TMeanFlux / (σ_N · M*); K cuts 62–72 % of TMeanFlux (mono, 4 000–5 000) and the remaining share's wide / sharp ratio is 1.08 at every K, predicting a K2b tilt of about +8 % toward wide from that factor alone. K2 and K2b see it; K1's tilt is the estimator row over the same stars and does not, by design. K10 on real data stays report-only (FM-17). F2 acceptance adds "H's TMeanFlux share beyond the matched cut within ± 5 pp of the class's F0 median (mono 69–72 / 62–65 / 40–44 % at 4 000 / 5 000 / 8 000; OSC 45 / 34 / 14 %)"; F3's K2b prints the family tilt beside F0's real 1.08, and outside 1.04–1.12 at the matched cut is an SK1 realism flag (Fable), not a K verdict.
- **FM-26 (R5, C3) Broad faint fits among the "brightest by signal": recorded, out of scope, no bar changes.** They are in H too (an H estimator artefact, not a frame defect); K removes them (filter SNR about 10 is below every cut of 11 or more), so under K they can only move c-64 / LN-500 membership on the 17–19 OSC planes with tails beyond 4 000, which is what K8's scatter and FM-24's column judge. F4's K8 output lists those planes by frame id (F0 `planes.tsv`); the item goes to `docs/superpowers/open-items.md` as a weight-estimator item (an extended / FWHM screen on the signal-ranked candidates of c and LN's pairs, judged by truth in a later cycle). Nothing is fixed inside this cycle.
- **FM-27 (R6, S2) FM-3's explicit fold-in is redundant: dropped.** `registration_hash_for` and `normalization_hash_for` push `measure:<measurement hash>`, `measurement_subtree` serializes the whole `cfg.measurement`, and the run passes the same hash; an explicit fold would duplicate the key and, if it serialized `None`, move the All literals. FM-3 is amended; the tests `fit_stars_k_moves_measure_register_normalize_hashes` and the All-literal pin stay; the empty-hash "not recorded" branch is untouched.
- **FM-28 (F1 review I1, controller 2026-10-18) The master measurement is always at All.** `integrate_group` measures the integrated master with `fit_stars` cleared (`measure_planes_diagnosed` gets `MeasureOptions { fit_stars: None, .. }`); `fitStars` is a per-frame speed control and the master is one high-SNR image, so its FWHM, eccentricity and star count stay comparable across K. K6's FWHM row therefore judges the stack, not the estimator. LN reads only `max_stars` of the frame options and is untouched. Pinned by `master_measurement_ignores_fit_stars`.

---

## Bars (fixed now)

### B-K — `fitStars = K` (truth first, then real data; research Q4 + FM-4/5/6)

| # | row | bar |
| ---- | ---- | ---- |
| K1 | estimator: P_K against P_T over the SAME fitted stars (dense family, matched cut) | ρ ≥ ρ_H − 0.01; wide / sharp and depth-tier tilt of median P_K / P_T ≤ 2 % AND ≤ H's; p10–p90 of P_K / P_T ≤ H's |
| K2 | definition: PSFSW against the frame's true S² / σ² (`w_true`, FM-21) per frame series | Spearman ρ ≥ H's; ρ against `w_pt = w_true / FWHM²` printed beside (report) |
| K2b | definition tilt (report, FM-21): median PSFSW_K / PSFSW_H per FWHM tier at fixed (s, σ) | reported beside K2's verdict, never a bar |
| K3 | frame FWHM / ecc error against truth, per family | ≤ H's error in every family (pooled rows printed) |
| K4 | inject families: frame FWHM / ecc vs H (the population route moved hc11B G image 2 by +30 %) | within §8's FWHM row (± 1 %) on every inject frame; ecc reported |
| K5 | identity at All | lab noreg 0 moved, max \|Δ\| 0; probe `cmp` 0058 / 0075 / OSC 0163 0 differing; a subset run every artifact byte-identical |
| K6 | real data, set 204 + run B (Horse C11: identity by construction, F0 / FM-22), master level vs H (= 78fdc673) | §8 hard rows: master median ± 0.1 %, MAD ± 1 %, noise ± 2 %, FWHM ± 1 % (master measured at All, FM-28), rejected ± 0.3 pp |
| K7 | Register | every mono registered frame byte-identical to H's (H's `registration.max_stars` = 2 000 Register stars must lie within the first K seeds: a K below F0's B_reg fails by construction and is never run, FM-5) |
| K8 | LN relative scale `s` per frame | median ratio 1 ± 0.10 % AND scatter ≤ 1 %, both hard (§8, review I3) |
| K9 | β, `stars_detected` | plane and group β byte-identical; `stars_detected` identical on every plane |
| K10 | per-frame weights vs H | reported: Spearman ρ, top-20, every frame whose rank moved (FM-6 rules a miss) |
| K11 | per-frame FWHM / ecc ratio N / H on set 204, run B (Horse C11: identity by construction, F0 / FM-22) (the brightest-K frame shape on real data; owner-visible in the Frames table, feeds the Auto reference pick; review I4) | median within ± 1 % per dataset; median, p10–p90 printed, every frame beyond ± 1 % listed; a median beyond ± 1 % with K3 / K4 passing → Fable (FM-6), never silently past |

### B-G — the GPU path (research §4 re-based by Fable C2, I1, M7)

| # | output | paired bar: GPU vs the CPU, same inputs | truth bar (G7) |
| ---- | ---- | ---- | ---- |
| G-a | per-star x, y, FWHM, ecc, θ, flux / signal, B, residual (Kernel 1) | p99 of \|Δ\| / σ_p ≤ 2 × the CPU-control's p99 for that parameter (G2 writes the numbers before any GPU run) | centroid, FWHM, ecc, flux, background, star-SNR rows ≤ CPU's |
| G-b | fit outcomes (accept / refuse, converged, `max_iter`, guard) | flips ≤ 0.05 % of seeds; no reason class + 0.02 pp | fit-outcome rows unchanged |
| G-c | β, β-sample membership, SATURATED flags | byte-identical (the verdict stays on the CPU) | — |
| G-d | frame FWHM / ecc medians, `c` | FWHM ± 0.05 %; `c` ± 0.02 % | rows ≤ CPU's |
| G-e | per-frame weights (PSFSW) | Spearman ρ ≥ 0.999; top-20 ≥ 19/20; every rank move listed (M7) | weight vs `w_true` ρ ≥ CPU's |
| G-f | Register (mono reuses fits), LN `s` | transform Δ ≤ 0.01 px at the corners; LN `s` ± 0.05 % | LN-injected rows ≤ CPU's |
| G-g | combine rejection decisions (Kernel 2) | flipped samples ≤ 1e-4 (C-26's warm bracket moved 7.7e-5) | inject-a-trail: same rejected pixels ± 1e-4 |
| G-h | master pixels / maps | p99.9 \|Δ\| ≤ 1e-3 σ_stack; median ± 0.01 %, MAD ± 0.1 %, noise ± 0.2 %, FWHM ± 0.2 %, rejected ± 0.02 pp | §8 on set 204, run B, Horse C11, both Macs |
| G-i | determinism per device | same bytes at any batch size, CPU pool width (1 / 10 / 16) and admission level; no float atomics | — |

### B-O — the owner bar

The set 204 whole run on the dev Mac, interleaved H N H N H N on fresh clones from Masters on a quiet machine, median of 3,
spread = (max − min) / median: **N < 1005 s** and N below H beyond the spread; swap N ≤ H in every stage; Horse C11 on the
runner no slower than H beyond its spread. ERP run 2 (968 s) printed beside, with **the ERP's own spread** (1005 / 968 =
3.7 % between two identical runs, research Q1) beside N's (review M8); the bar stays the owner's (N < 1005 s, N below H
beyond its spread). What counts as meeting it at which configuration is OWNER Q1 (A1).

## Stop rules (pre-registered; a STOP writes its evidence file and the report, then waits)

| id | trigger | goes to |
| ---- | ---- | ---- |
| SK0 | *retired at plan review (FM-16)*: the flux / filter-SNR order difference is a reported column of F0's cut table | — |
| SK1 | the dense family cannot match a real class's filter-SNR distribution within F2's tolerance | Fable: re-scope the truth bar before any K run |
| SK2 | no K passes F3 (or F4), or F0's B_reg exceeds 8 000 (FM-5) | owner informed; All stays; nothing user-facing ships, F5 skipped (FM-19); GPU path continues |
| SK3 | the CPU control flips > 0.05 % of fit outcomes itself (G-b unreachable) | Fable: the bar |
| SG1 | Kernel 2 < 3× the post-4d CPU, full pool, end-to-end on the same machine (dev Mac) | recorded with numbers; Kernel 2 ends; Integrate stays CPU |
| SG2 | Kernel 1 float-float < 5× the full-pool CPU end-to-end (dev Mac) | recorded; Kernel 1 ends |
| SG3 | a paired bar misses even with float-float | recorded; that kernel ends |
| SG4 | fast math breaks two-sum AND the safe-math passthrough (FM-12 below) is unavailable or fails | Fable: a second backend is a new decision |
| SG5 | clean release build > +90 s or the desktop binary > +8 MB with the feature | controller → owner |
| SG6 | projected GPU saving AT ALL < 15 % of the whole set 204 run (≈ 160 s), by G5's formula (FM-20; the shipped-K saving printed beside, never the basis) | controller → Fable: end the GPU path or not |
| SD1 | any byte differs in an identity proof (All, `off`, no feature) | Fable rules before anything lands |
| SW1 | Windows free space below `WIN_FREE_FLOOR` | nothing is created; owner |

- **FM-12 Fast math (Fable I2).** G0 runs an on-device two-sum exactness test on every adapter. If it fails on Metal, the
  named fallback is `PASSTHROUGH_SHADERS` + MSL (naga's output with `#pragma METAL fp math_mode(safe)` prepended; MSL 3.2 =
  macOS 15: the GPU is off below macOS 15 unless G0 proves the pragma on 11–14). Bit-cast barriers are not a fence (the
  platform shader compiler folds them). If the plain-f32 arm passes every paired bar (a pairwise f32 tree errs by
  ≈ log2(n)·ε ≈ 1e-6, 100× under the 1e-4 cost test), the question is retired — an explicit prototype outcome.

---

## File structure

| path | task | change |
| ---- | ---- | ---- |
| `crates/athenaeum-core/src/stacking/config.rs` | F1, F5 | `fit_stars`, wire decode, the floor clamp `max(MIN_FIT_STARS, registration.max_stars)` / canonicalize, the `selection.min_stars > fit_stars` warn, fold-ins, preset (F5, only if passed) |
| `crates/athenaeum-core/src/stacking/run.rs` | F1 | the one-line `warn!` on a fits-list read error in `measure_ln_seeds_calibration` (`:13396`, review M5) |
| `crates/athenaeum-core/src/star_measure/mod.rs` | F1 | `Mode::Brightest { k }`; the outcome bookkeeping |
| `crates/athenaeum-core/src/stacking/measure.rs` | F1 | the adapter maps `fit_stars` to the mode (today `:828–832`) |
| `crates/athenaeum-core/examples/star_probe.rs` | F1, G2 | `--fit-stars K`; `--dump-stamps <file>` (`gpu-proto` only) |
| `crates/athenaeum-core/examples/integrate_probe.rs` | G1 | `--dump-stacks <y0..y1> <file>` (`gpu-proto` only) |
| `crates/athenaeum-core/src/star_measure/fit.rs`, `integration/engine.rs` | G1, G2 | `#[cfg(feature = "gpu-proto")]` recorders at the two call sites, zero effect otherwise |
| `crates/athenaeum-core/Cargo.toml` | G0, G6 | `gpu-proto` (example only), then `gpu` (product); optional wgpu / pollster / bytemuck |
| `crates/athenaeum-core/examples/gpu_proto/{main.rs,moffat_fixed.wgsl,linfit_combine.wgsl,…}` | G0–G4 | the prototype |
| `crates/athenaeum-core/src/gpu/{mod,device,kernels,provenance}.rs` + kernels | G6 | product module (conditional) |
| `crates/athenaeum-core/src/stacking/{run,measure,ln/scale}.rs`, `integration/engine.rs` | G6 | the engine switch (conditional) |
| `crates/athenaeum-core/src/stacking/master_cards.rs` | G6 | one engine card on a GPU-made master (only when the engine is GPU; review M7) |
| `crates/athenaeum-core/src/settings/` | G6 | KV `compute.gpu` (key, default, `all()`) |
| `crates/athenaeum-tauri/Cargo.toml` | G6 | `athenaeum-core/gpu` on macOS, Windows, Linux targets; the web crate never |
| `src/types/stacking.ts` (+ ts_export), `src/components/stacking/panels/MeasurePanel.tsx`, `stageSummary.ts`, `ProvenanceModal.tsx` | F5, G6 | the field, the summary line, the engine line |
| `src/settings/registry.ts`, `src/components/settings/tabs/StackingTab.tsx` + section | F5, G6 | the fitStars default, `compute.gpu` (desktop only) |
| athenaeum-lab `star-lab/bin/fm_cut_table.py`, `starlab/families.py` (dense family), `bin/fm_truth_row.py`, `harness/acceptance/fm.sh` | F0, F2, F3, F4 | lab tools and cycles |
| `docs/stacking/README.md` (R-OSP-14), `docs/settings/README.md`, the logging spec's dictionary, `docs/superpowers/research/2026-10-XX-fast-measurement.md`, this plan's Outcome | A2 | the record |

## Interfaces

```rust
// stacking/config.rs
pub struct MeasurementConfig { /* … */
    /// The first K seeds in detection order get the PSF fits (FM-2); None = all. Clamped / canonicalized by resolve_config.
    #[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
    pub fit_stars: Option<u32>,
}
pub const MIN_FIT_STARS: u32 = 1_000;
// resolve_config: K < max(MIN_FIT_STARS, registration.max_stars) → that floor + warn!; then K ≥ measurement.max_stars → None;
// selection.min_stars > K → warn! (FM-3)

// star_measure/mod.rs
pub enum Mode { All, Budget { budget: usize }, Population { statistic: usize, list: usize },
    /// verdict + pass-2 fits for seeds[..k]; seeds[k..] keep their record as Outcome::NotSelected; no imputation
    Brightest { k: usize } }

// G6 (conditional): gpu/mod.rs
pub enum Engine { Cpu, Gpu { kernel_version: &'static str, reduction: Reduction, backend: Backend, adapter: String, driver: String } }
pub fn resolve_engine(setting: GpuSetting) -> Engine;                // one info line "stacking engine resolved"
pub fn fit_fixed_beta_batch(dev: &GpuDevice, plane: &[f32], w: usize, h: usize, stars: &[FixedBetaJob])
    -> Result<Vec<Option<Moffat2DResult>>, GpuError>;                // Err → warn! + the stage on the CPU (FM-8)
```

Unchanged: every command (`compute.gpu` rides `get_setting` / `set_setting`; `fitStars` rides the stacking config
commands, the collab project twins included), the artifact layouts, `MEASURE_VERSION` 4.

---

## Tasks

```text
F0 ─ F1 ─┬─ F2 ─ F3 (K decided) ─ F4 ─ F5
         └─ G0 (harness; builds serialized with F1/F4 on the dev Mac)
F3 ─ G1 (Kernel 2, SG1) ─ G2 (dumps at post-K + control → bars) ─ G3 (Kernel 1) ─ G4 (Windows) ─ G5 (verdict)
G5 ─ [G6 product ─ G7 checkpoint]  (only past G5)  ─ A1 owner bar ─ A2 review, record, squash
S1 (SIMD Integrate, dev Mac now) ─ S1b bracket;  S2 (Measure per-unit audit, read-only) ─ S2b levers (FM-30)
```

Controller restarts: after F3, after F5, after G5, after G7.

### Task F0: The cut table and the consumer map (design read; numbers only, no code)

**Machine · model · time.** Dev Mac (probes one at a time) · opus · ≈ 3 h.
**Budget.** ≤ 120 tool calls.
**Depends on.** Nothing (OWNER Q1 answered 2026-10-18 (A1): the bar is met at the owner's configuration — the Fast-preview K if K ships and / or `compute.gpu = auto` — with the defaults All / off).
**Files.** Lab `star-lab/bin/fm_cut_table.py` (new, committed), results `fast-measure/f0-cut/`; report
`fm-f0-report-r1.md` (the design sharpenings, marked (S)).
**Approach.**
1. Code map, by grep: every reader of Measure's fitted list (`.athf` readers `run.rs:7358` Register, `:13075`
   `run_group_normalization`, `:13396` `measure_ln_seeds_calibration`; `c`; `minStars` / `stars_fitted`; weights; the
   provenance) and every subtree that folds `measurement` or `MEASURE_VERSION` (FM-3's list).
2. Per real plane class (set 204 mono wide / sharp and OSC from `tierA-lnB-f1-13c`, run B mono / OSC from `lnB-runB-f1`,
   Horse C11 from `hc11B-f1`): N_seeds; the filter SNR at each grid K (4 000 / 5 000 / 8 000, or the points FM-5 lifts)
   as the median over ranks K ± 1 % with its spread (FM-16); q; H's TMeanFlux and TFlux share beyond each
   rank; the max seed rank of H's Register selection (mono planes) and **B_reg** = its max over the three datasets (FM-5:
   it fixes the grid's lowest point before F3); **Register's usable stars at K** per dataset (H's Register cuts applied
   to the first K seeds' fits, against `registration.max_stars`); the max seed rank of LN's 500 pairs and `c`'s 64 and
   of the Auto β sample; the share of the first K a filter-SNR order would swap (a reported column, FM-16).
   Source: `star_probe measure --diagnostics` on a stratified plane sample (every class, wide and sharp ends) plus the
   13c trees' `.athf` order for every plane (a bound).
3. Projection per K: fits per class × the measured per-fit cost (13b design read) → Measure s and the whole run (the
   count-driven share from `star_probe` sub-step timers at debug).
4. Pick G2's extra planes: the most faint-dominated plane (largest share of seeds under filter SNR 15) and the most
   saturation-heavy plane (most SATURATED flags / censoring iterations) across the three datasets.
**Acceptance.** `cut-table.tsv` + `cut-table.txt` (≤ 40 lines) with every column above for every class; B_reg and the
resulting F3 grid (FM-5); the predicted K7 verdict per K; the consumer map; G2's two plane ids.
**STOP.** None of its own (SK0 retired, FM-16); B_reg > 8 000 → SK2 (FM-5).
- [x] F0 run and report.
  F0 facts (lab commit 89acd1d, `star-lab/results/plan-a/fast-measure/f0-cut/`, report `fm-f0-report-r1.md`): B_reg 3 576; F3 grid 4 000 / 5 000 / 8 000, not lifted; G2 extra planes: faint = set 204 frame 28920 plane 0 `c_2025-10-19_03-54-27__-10.00_180.00s_0212_d` (OSC); saturated = set 204 frame 29053 plane 0 `c_2025-10-18_23-41-41__-9.90_180.00s_0076` (mono sharp). Rulings FM-22…FM-27.

### Task F1: `fitStars` in the core — behaviour-neutral at All

**Machine · model · time.** Dev Mac · rust-engineer sonnet ≈ 3 h; review opus ≈ 1 h; identity proof haiku ≈ 1 h.
**Budget.** Code ≤ 150 tool calls; review ≤ 60; proof ≤ 40.
**Depends on.** F0 (the subtree list, the consumer map).
**Files.** `stacking/config.rs`, `star_measure/mod.rs`, `stacking/measure.rs`, `stacking/run.rs` (the M5 `warn!`),
`examples/star_probe.rs` (`--fit-stars K`), the ts_export registry if a type changes.
**Approach.** FM-2 and FM-3 exactly. The probe's `--fit-stars K` passes `Mode::Brightest` unclamped (the lab's per-frame
K_lab may be below the floor); the floor clamp `max(MIN_FIT_STARS, registration.max_stars)` and the
`selection.min_stars > fit_stars` warn live in `resolve_config` only. Required line of F1's approach (FM-24): `Mode::Brightest` reuses the budget seam, so the ring-sky mask and c's isolation run over EVERY detected seed and B_i / isolation are H's; only the ring width F and c's 64 candidates can move. F1 also adds one `debug!` on the PerSide LN target fit carrying c and F (same two fields as the PairCommon arm's `ln scale` event, scale.rs:181) — a log line, zero numeric change. `run.rs:13396`'s `read_fits(p).unwrap_or_default()`
in `measure_ln_seeds_calibration` gets a one-line `warn!` (`path`, `error`) before the empty default (review M5 ruling:
zero numeric change on a readable tree; under K the list matters more).
`Mode::Brightest { k }` runs the verdict and pass-2 fits on `seeds[..k]` with the
same pool installs and indexed collects as `Mode::All` (nothing reads the thread count); `seeds[k..]` get
`Outcome::NotSelected` records. The adapter passes `Brightest` only when `fit_stars` is `Some`. Pass-1 spill
(`PASS1_ARTIFACT_VERSION`) needs no version bump: its hash folds the measurement subtree.
**Tests.**
- `fit_stars_none_leaves_every_stage_hash_literal_unchanged` (every subtree's hash at All equals its 78fdc673 literal).
- `fit_stars_k_moves_measure_register_normalize_hashes`; `fit_stars_at_or_above_max_stars_is_none`;
  `fit_stars_below_min_is_clamped_with_a_warning`; `fit_stars_below_register_max_stars_is_clamped_to_it_with_a_warning`;
  `min_stars_above_fit_stars_warns`; `document_without_fit_stars_decodes_all`; `all_serializes_without_the_key`.
- `brightest_k_at_seed_count_is_bit_identical_to_all`; `brightest_k_fits_exactly_the_first_k`;
  `brightest_k_keeps_every_detection_record`; `brightest_k_beta_sample_identical_when_inside_k`.
**Acceptance (haiku proof, exact commands in the handoff).** K5: probe `cmp` at All on 0058 / 0075 / OSC 0163, 0
differing (wall fields dropped); the set 204 subset (`t13c-subset`) from Measure at All against 78fdc673, every artifact
byte-identical (`.athm` exempt by design); the lab `fm-id` cycle on the runner: noreg 0 moved, max |Δ| 0.
**STOP.** SD1.
- [x] F1 code + tests (sonnet) · [x] review (opus) · [x] K5 proof (dev Mac + runner)

### Task F2: The dense truth family, the true weight and the fitStars truth row (lab)

**Machine · model · time.** Runner (Python) · F2a opus ≈ 2 h (family spec, the tolerance; `w_true` / `w_pt` are fixed by
FM-21 and implemented, never re-decided) · F2b sonnet ≈ 3 h (build, tests, generation).
**Budget.** F2a ≤ 60 tool calls; F2b ≤ 120.
**Depends on.** F0 (the cut table), F1 (`--fit-stars`).
**Files (athenaeum-lab).** `starlab/families.py` (a `dense/<class>` family per F0 class), the manifest entry,
`bin/fm_truth_row.py` (from `results/plan-a/t13b-truth/truth_row.py` and Fable's PSFSW port, unchanged), the probe-plan
arms `fm-H` (All) and `fm-K<k>` at the matched cut, tests.
**Approach.**
- The family reproduces each class's seed filter-SNR histogram (the faint population down to the 6 σ_f threshold and
  the luminosity-function slope), its FWHM tier (2.0 / 3.0 / 4.3 px), crowding (stars / Mpx) and saturation share, on
  2048² frames (K is mapped by the matched cut, FM-4), in frame SERIES that vary transparency (`flux_scale = 1 / s_true`),
  sky / noise σ and FWHM, so a weight ranking is non-trivial. **FWHM is drawn independently of (s, σ)** — never
  co-varying, else ρ is blind to a sharpness tilt (FM-21) — and each series renders a subset of its (s, σ) draws at the
  low, middle and high FWHM of its range, so K2b reads the tilt at fixed (s, σ).
- **`w_true`** (ruled by FM-21, the K2 referee): per frame `(1 / s_true)² / σ_true²` from the generator's cards — the
  frame's true S² / σ²; **`w_pt = w_true / FWHM²`** printed beside, report-only. No truth weight exists in the lab today
  (`t13b-truth/truth_row.py` has none): F2b adds both.
- The truth row prints K1–K3 per arm: P_K, P_H, P_T over the same fitted stars through Fable's RCR + winsor port, the
  tilts (wide / sharp, depth tiers), p10–p90, ρ against `w_true` (and `w_pt` beside), K2b (median PSFSW_K / PSFSW_H per
  FWHM tier at fixed (s, σ)), frame FWHM / ecc error per family, pooled rows, and per class c_K / c_H and F_K / F_H (report, no bar; FM-24).
**Acceptance.** The match tolerance is fixed by F2a BEFORE generation (suggested: the share of seeds under filter SNR
10 / 20 / 50 within ± 3 pp of the class, and the filter SNR at the matched rank band (FM-16) within ± 10 %); tests pass
(incl. `w_true` / `w_pt` on a generated series and FWHM independent of (s, σ) by construction); a dry run of
the row on the H arm reproduces H's P_H from the frame lines (equal or the frame listed); H's TMeanFlux share beyond the matched cut within ± 5 pp of the class's F0 median (mono 69–72 / 62–65 / 40–44 % at 4 000 / 5 000 / 8 000; OSC 45 / 34 / 14 %; FM-25).
**STOP.** SK1.
- [x] F2a spec (opus) · [x] F2b build + generate + tests (sonnet)

### Task F3: The truth run — truth decides K

**Machine · model · time.** Runner · sonnet judgment run ≈ 3 h.
**Budget.** ≤ 60 tool calls (one command, then reads).
**Depends on.** F2, F0's B_reg (the grid, FM-5). One command (`harness/acceptance/fm.sh truth`): build at F1's head, the
full manifest (K3 reads every family) for `fm-H` (All; noreg against the cycle base = K5) and one `fm-K<k>` arm per grid
point FM-5 leaves (`fm-K4000`, `fm-K5000`, `fm-K8000` unless F0's B_reg lifts them) at the matched cuts, the inject cycle
per arm (K4), scorecard, `fm_truth_row.py`, a ≤ 30-line verdict.
**Acceptance.** B-K rows K1–K5 per K (K2b and ρ against `w_pt` printed beside K2; K2b prints the family tilt beside F0's real 1.08 — outside 1.04–1.12 at the matched cut is an SK1 realism flag for Fable, not a K verdict, FM-25); the verdict names the F3-passing K
set. Results committed; clones deleted.
**STOP.** SK2 (the cycle continues on the GPU path).
- [x] F3 run, verdict (SK2: no K passes), results commit, cleanup.

### Task F4: The real-data checkpoint at master level

**Machine · model · time.** Dev Mac (set 204, then run B, one at a time; Horse C11 is not run: K6/K7/K9/K11 identity by
construction, F0 — 500 capped, 1 890 / 938 uncapped, FM-22) · sonnet judgment runs ≈ 4 h + 2 h.
**Budget.** ≤ 60 tool calls per run.
**Depends on.** F3 (the smallest F3-passing K). One run per dataset per arm (the stacking path is deterministic): H =
78fdc673 (13c's final-head trees are byte-identical to it; reuse them where they still exist), N = F1's head with
`fitStars = K`, both from Measure on `cp -Rc` clones of the before trees, reference pinned `manual`, `twoPass = false`.
**Acceptance.** K6–K11 on set 204 and run B (K11: the per-frame FWHM / ecc ratio N / H per dataset — median, p10–p90, every frame beyond
± 1 % listed). K8 also prints, per frame, LN target-side c_N / c_H and F_N / F_H from the `ln scale` debug event (FM-24) and lists by frame id the planes with a seed tail beyond 4 000 (F0 `planes.tsv`, FM-26). Set 204 and run B are one OSC-bearing pair; if a set 204 OSC group puts K8 within 2× of a bar (median |r − 1| > 0.05 % or scatter > 0.5 %) or K10 moves a top-20 rank, set 174 runs as a third arm (FM-23); otherwise the report states the independence caveat verbatim. Timing is printed (single runs; never the bar). Every clone deleted after its results commit.
**STOP.** A miss on K6–K9 → the next F3-passing K (FM-5); K10 with K1–K3 passing, or a K11 median beyond ± 1 % with K3 /
K4 passing → Fable (FM-6); no K left → SK2.
- not run (SK2): set 204 · run B · set 174 third arm · verdict

### Task F5: The product finish — the field, the preset, the panel

**Machine · model · time.** Dev Mac · frontend-dev sonnet ≈ 1.5 h; rust-engineer sonnet ≈ 30 min (preset); opus review.
**Budget.** Code ≤ 120 tool calls; review ≤ 60.
**Depends on.** F4 passed. **On SK2, F5 is skipped entirely** (FM-19: no Settings field, no panel control, no preset, no
help text; `fit_stars` stays in the config for the probe and the lab).
**Files.** `src/types/stacking.ts` (+ ts_export), `MeasurePanel.tsx` ("Fit stars": All / Brightest N, N ≥ the floor
`max(1 000, registration.max_stars)`), `stageSummary.ts`, `ProvenanceModal.tsx`, `src/settings/registry.ts` (ids, label,
help with the measured effect, keywords), the Stacking tab's Measure section, the registry coverage test;
`stacking/config.rs` `preset(FastPreview)` gets `fit_stars = Some(K)`. **The help text names** the floor, that mono
registration identity needs K above the bound F0 printed (B_reg), and that a `selection.minStars` above K excludes every
frame (review C1, M4).
**Tests.** vitest for the panel and the summary; `fast_preview_carries_the_shipped_k` (only if shipped); the registry
coverage test; `tsc`.
**Acceptance.** The settings contract (`docs/settings/README.md`): autosave, Escape restores, an invalid draft never writes,
per-field reset; launch the app on the dev DB and set the field (verify by launching, not by tests alone).
- not run (SK2): F5 code · review · app launch check

### Task G0: The GPU harness (no kernel)

**Machine · model · time.** Dev Mac · rust-engineer sonnet ≈ 3 h.
**Budget.** ≤ 120 tool calls.
**Depends on.** F1 merged (one build at a time on the dev Mac).
**Files.** `Cargo.toml`: `gpu-proto = ["dep:wgpu", "dep:pollster", "dep:bytemuck"]`, `[[example]] gpu_proto`
(`required-features = ["render", "solver", "gpu-proto"]`); `examples/gpu_proto/main.rs` (adapter enumeration with the
backend order of FM-9; the harness flags **`--backend metal|vulkan|dx12`** and **`--reduction subgroup|workgroup`**
(G4's arms need them; the Kernel 2 override constant is not that, review M3);
the feature / limit report — `SUBGROUP`, `TIMESTAMP_QUERY`, `max_compute_workgroup_storage_size`
(32 KB on Metal), subgroup size range —, one timestamp pair per compute pass (else submit → poll wall), the two-sum
exactness test in plain WGSL and, on Metal, through the FM-12 passthrough, `timing.jsonl` / `agreement.jsonl` writers);
the dump format spec in a module comment: stamps = value f32 + i16 offsets (6 B per pixel, M2) + the per-star header
(start, β, `max_iter`, `conv_tol`, `max_rejects`, seed rank, the CPU result); stacks = work values + frame indices +
weights + the recipe + the CPU result and mask.
**Gates.** `cargo test --workspace` and every default build never compile wgpu (`cargo tree -p athenaeum-core -e normal |
grep -c wgpu` = 0); the example builds on the dev Mac and the runner. **Measure** the clean release build-time and binary
delta of the example (SG5 applies to the desktop build in G6; here it is printed).
**Acceptance.** The adapter report and the two-sum verdict per adapter (dev Mac, runner) in `fast-measure/g0-harness/`;
both harness flags accepted and echoed in `timing.jsonl` (a forced `--reduction workgroup` runs the workgroup tree on an
adapter that has subgroups). The adapter report is also the Windows record when `vulkaninfo` is absent.
**STOP.** SG4 (if two-sum fails AND the passthrough fails).
- [x] G0 code · [x] runs on both Macs and Windows

### Task G1: Kernel 2 — op count and occupancy first, the stop rule before any Kernel 1 code (Fable C1, I5, M1)

**Machine · model · time.** Dev Mac (quiet), runner second · opus ≈ 4 h (0.5 day; numerics) with the recorder by sonnet.
**Budget.** ≤ 200 tool calls.
**Depends on.** F3 (FM-1), G0.
**Files.** `integration/engine.rs` recorder (`gpu-proto` only), `integrate_probe.rs --dump-stacks`,
`examples/gpu_proto/linfit_combine.wgsl`.
**Approach.**
- Count first: the CPU's real `rofunc` evaluation counts per stack on the dumped rows (a `gpu-proto`-only counter) →
  ops / sample for the GPU form, written in the report BEFORE timing (C1 a).
- The arm (C1 b): ONE subgroup per stack, values in registers (⌈n / 32⌉ per lane as a pipeline-overridable constant),
  the median and the sort ranks by per-lane compare + a subgroup count, the (value, index) order with −0 canonicalized to
  +0 before the key (M1); sums by the shuffle butterfly; float-float for Σi·y, Σy, χ², the weighted mean; a per-k table
  (k = 2..360) of float-float `del`, Σi, Σi² computed on the CPU in f64 and uploaded with the recipe (I5).
- Inputs: 64 rows of a set 204 mono group (n = 92), Horse C11 n = 164 with prefixes 20 / 74 / 105, and a timing-only
  n = 260 stack (occupancy; no agreement claim).
- Measure: occupancy at n = 92 / 164 / 260, ns / sample end-to-end (upload, kernel, readback) against `integrate_probe`'s
  post-4d CPU at the full pool on the same rows and machine.
**Acceptance.** ≥ 3× end-to-end on the dev Mac → the agreement smoke on the same rows (G-g, G-h per-pixel) and the runner's
numbers; else SG1: recorded with the numbers, Kernel 2 ends, Integrate stays on the CPU, and no Integrate saving enters
any projection (C1 d).
**Cleanup.** The release example target (≈ 6 GB on the dev Mac; `df` before the build) and the stack dumps are deleted
once the report and the jsonl are committed in the lab; the runner deletes its clone and target the same way.
- [x] counts · [x] kernel + timing · [x] verdict (SG1 fires) · [x] cleanup

### Task G2: Kernel 1 inputs and the bars — dumps at the post-K count and the CPU-control arm (Fable C2, M2, M3)

**Machine · model · time.** Dev Mac · sonnet ≈ 3 h (recorder, dumps, control arm); opus ≈ 1 h (reads the bars).
**Budget.** ≤ 120 tool calls.
**Depends on.** F3/F4 (K known, FM-1), G0.
**Files.** `star_measure/fit.rs` recorder at the `fit_moffat_2d_fixed_beta_conditioned` call (`gpu-proto` only),
`star_probe.rs --dump-stamps`, `examples/gpu_proto/control.rs`.
**Approach.** Dump the fixed-β fits (pass 2 at the group β) of planes 0058 (wide), 0075 (sharp), OSC 0163 (3 planes) and
F0's faint-dominated and saturation-heavy planes, with each star's seed rank, so G3 times both the first-K batch and All.
The control arm re-runs the CPU oracle (the public `fit_moffat_2d_fixed_beta_conditioned`) (a) with the stamp's pixel
order reversed and (b) with the start perturbed by 1 ulp; σ_p per parameter from the f64 JᵀJ at the CPU solution × the
residual variance. **The per-star bars** (G-a) = 2 × the control's p99 of |Δ| / σ_p, pooled over (a) and (b), per
parameter — written to `fast-measure/g2-bars/bars.json` before any GPU kernel runs. The control's own outcome-flip rate
is printed (SK3).
**Acceptance.** Dumps + `bars.json` + the CPU oracle timings at 1 thread and the full pool.
**Cleanup.** The release example target (≈ 6 GB on the dev Mac) is deleted once `bars.json` and the report are committed.
The stamp dumps are G3's and G4's inputs: they stay (dev Mac + `WIN_UNIVERSE` copy) until G3's and G4's reports are
committed, then G4's analysis step deletes them on every machine.
- not run (SK2 / SG6): dumps · control + bars · bars read · target cleanup

### Task G3: Kernel 1 prototype (fixed-β only; Fable I1–I3, M5, M6)

**Machine · model · time.** Dev Mac, then the runner · opus ≈ 1.5 days (G1–G3 total budget 2.5–3 agent-days, M4).
**Budget.** ≤ 200 tool calls.
**Depends on.** G1's verdict (whatever it is), G2's bars.
**Files.** `examples/gpu_proto/moffat_fixed.wgsl`, `main.rs` arms.
**Approach.** One star per subgroup (r ≤ 12, 32 lanes) or workgroup (64–256 lanes above), stars bucketed by region
radius; the GPU gathers stamps from the plane (one upload per plane; each in-flight plane is +99 MiB on the GPU, M5); the
36 sums (28 JᵀJ + 7 Jᵀr + cost) by the shuffle butterfly with the workgroup tree mirrored; the 7×7 damped Cholesky in
float-float on one lane, the step broadcast; λ / ν / guards / reject counting uniform per star. Two arms: plain f32 and
float-float (Jᵀr and Σr² compensated). `(1 + Q)^(−β)` as `exp2(−β·log2(1 + Q))`, `power / base` by the division as the
CPU does (never a second `exp2`), θ argument-reduced before the trig, denormal flushes harmless (M6). Submits capped at
50–100 ms (Windows TDR 2 s; a Linux GPU driver ≈ 10 s; Metal none).
**Acceptance.** G-a and G-b against G2's bars on every dumped plane, both arms, both Macs; G-i (batch size); speed
end-to-end against the CPU full pool at the first-K batch and at All; the FM-12 outcome (plain f32 passes → fast math
retired). Report `fast-measure/g3-kernel1/report.md` (≤ 40 lines): speedups per arm per machine, the bars, the projection.
**STOP.** SG2, SG3, SG4.
**Cleanup.** The release example target (≈ 6 GB on the dev Mac) and the runner's clone and target are deleted once the
report and the jsonl are committed; the stamp dumps follow G2's rule (deleted after G4's report, or now if G4 does not run).
- not run (SK2 / SG6): kernel · dev Mac arms · runner arms · cleanup

### Task G4: The Windows arms

**Machine · model · time.** Windows runner · sonnet ≈ 3 h · the fill-in block complete first.
**Budget.** ≤ 100 tool calls.
**Depends on.** G3 (and G1 if Kernel 2 passed).
**Approach.** SW1 check; fresh clone into `WIN_WORK`; release build of `gpu_proto`; inputs copied from `WIN_UNIVERSE`.
Arms (G0's flags): Vulkan with subgroups (`--backend vulkan --reduction subgroup`), Vulkan forced to the workgroup path
(`--backend vulkan --reduction workgroup`), DX12 (`--backend dx12 --reduction workgroup`, FXC, no subgroups); the paired
bars with G2's `bars.json`; the CPU oracle timings on x86_64 (1 thread and the full pool) and the `star_probe measure`
ladder for 0058 / 0075 / 0163 (no x86_64 run existed, R-OSP-13 carry 4). Outputs `jsonl` + the build time to
`WIN_UNIVERSE`; the analysis runs on a Mac. Post-run cleanup and free space printed; once the analysis is committed, the
stamp dumps are deleted on every machine (G2's rule).
**Acceptance.** Per arm: bars pass / miss, speed vs the x86_64 CPU; determinism within a device (two runs, same bytes).
- not run (SK2 / SG6): G4 run · analysis on the Mac · dumps cleanup (G0 Windows done)

### Task G5: The GPU verdict

**Machine · model · time.** Reads only · opus ≈ 1 h.
**Budget.** ≤ 60 tool calls.
**Approach.** Per kernel: the stop rules, the bars, the speed on each machine; the projected set 204 run at All (the
SG6 basis, FM-20) with the shipped-K projection printed beside (never the basis); Integrate only if G1 passed. **The
projection formula (fixed; no agent re-estimates it), per machine:** saving = fits_at_config × (per-fit CPU cost −
per-fit GPU cost incl. upload / readback, from G3's end-to-end) + LN refits × the same − the residual CPU of the stage,
from G3's measured numbers on the dumped planes only (review I6). SG6 at All: saving < 15 % of the whole run (≈ 160 s).
A go needs: no SG1–SG4 for that kernel and SG6 not triggered.
Proposal to the controller, who takes Fable's ruling before G6 (and the owner's word, if the
cost of G6 is the question).
- [x] verdict: G5-pre, SG6 fires at 89 s / 10.5 % (FM-31); the GPU path ends

### Task G6 (conditional on G5): The product path — `gpu` feature, `compute.gpu`, the engine switch

**Machine · model · time.** Dev Mac · rust-engineer sonnet ≈ 1–1.5 days; frontend-dev sonnet ≈ 1 h; opus review.
**Budget.** Core ≤ 200 tool calls; UI ≤ 100; review ≤ 60; identity proof ≤ 40.
**Files.** See File structure. Kernels move from the example into `src/gpu/` unchanged in arithmetic (the agreement
re-proved in G7).
**Approach.** FM-7, FM-8 (as amended by FM-14: adapter refusals at run start only; a mid-stage loss re-runs the stage on
the CPU and demotes the rest of the run; no `gpu:`-hashed row of the failed stage survives in `stacking_artifacts`),
FM-9 (vendor id + `device_type`, validated driver version in the list and the provenance), FM-10. A GPU-made master
carries one engine card (the resolved engine string) beside the existing master cards (`master_cards.rs`) — an exported
master is what a user shares (review M7); the card is written only when the engine is GPU, so a `cpu` master stays
byte-identical. One GPU service queue per process inside the `ComputeQueue` budget; GPU-resident
planes capped at 2 and charged in the Measure / Normalize admission (L1's working-set formula + 99 MiB each); `resolve_engine` once per
run, logged (`stacking engine resolved`: backend, adapter, driver, kernel version, reduction path — the
dictionary extended first); the run summary and `ProvenanceModal` show the engine; the web crate never enables the
feature (no probe, no GPU code); Tauri enables it on macOS, Windows and Linux (`libvulkan` loaded at run time, so the
AppImage needs no SDK). Measure the desktop release build time and binary size (SG5) on macOS and Windows.
**Tests.** `compute_gpu_off_and_no_feature_leave_every_hash_literal_unchanged`; `engine_folds_only_when_gpu`;
`gpu_error_reruns_the_stage_on_cpu_without_mixing` (a fault-injection test; also asserts no `gpu:`-hashed artifact row
of the failed stage remains); `demotion_holds_for_the_run` (a device lost in Measure → Normalize and Integrate run `cpu`);
`gpu_master_carries_the_engine_card` (and a `cpu` master none); `unvalidated_adapter_resolves_to_cpu`; the
settings registry coverage test; vitest for the Compute section (hidden on `VITE_TARGET=web`).
**Acceptance.** SD1 identity at `off` (subset, every artifact byte) and without the feature; the review.
- not run (SK2 / SG6): core · UI · review · identity proof

### Task G7 (conditional): The GPU checkpoint — tolerance and truth

**Machine · model · time.** Dev Mac + runner (sonnet judgment ≈ 1 day); Windows runner (sonnet ≈ 2 h).
**Budget.** ≤ 60 tool calls per run (one command, then reads).
**Approach.** `compute.gpu = auto` against `off` at the shipped `fitStars` and at All: set 204 + run B (dev Mac), Horse C11
(runner, and dev Mac `hc11B-f1`), every B-G row; the lab scorecard at `auto` (truth rows ≤ CPU's, no row worse beyond its
sampling noise); the determinism matrix G-i (pool 1 / 10 / 16, admission 1, two processes). Windows: the desktop build on
the Windows runner, a `t13c-subset` stacking run at `auto` and `off` (fallback lines, provenance, no TDR), cleanup.
**Acceptance.** Every B-G row on each validated platform; the validated list (FM-9) is exactly the platforms that passed.
**`auto` never becomes the default in this cycle** — that is the owner's call on this checkpoint's numbers.
- not run (SK2 / SG6): dev Mac · runner · Windows · verdict

### Task S1: Integrate's selection vectorized, bit for bit (FM-30)

**Machine · model · time.** Dev Mac (NEON) · opus ≈ 1 day (code + proof); Windows runner AVX2 build + identity; bracket
S1b sonnet.
**Budget.** Code ≤ 150 tool calls; S1b ≤ 60.
**Depends on.** Nothing (4d merged; G1 closed by SG1).
**Files.** `integration/combine.rs` (`medfit_line` / `rofunc` evaluation, the windowed selection of 4d, the sorts it feeds),
a `simd` submodule with `std::arch` NEON (aarch64) and AVX2 (x86_64, runtime-detected) paths plus the scalar path kept as
the oracle; no new dependency.
**Approach.** Vectorize only what cannot change rounding: element-wise residuals `y − b·i` (the same f64 expression per
element), comparisons, a branchless three-way partition for the median selection, integer counts / sign sums. Keep every
floating-point reduction in today's order. The 4d window logic stays as is.
**Tests.** The 4d oracle tests at every size / shape (cold and warm, NaN-carrying input, ±0) `to_bits` equal on the SIMD
path AND the scalar path; a forced-scalar switch for the proof; the existing `integration::` suite.
**Acceptance.** Bit identity (t13c-subset from Integrate: every artifact byte-identical vs the head before S1; the probe
`integrate_probe` on set 204 / Horse C11 rows equal); S1b: set 204 Integrate bracket H N H N H N on fresh clones, median of
3 — faster beyond the spread on the dev Mac and no slower on the runner. A miss is recorded and reverted (rule).
- [x] S1 code + proof · [x] Windows AVX2 identity · [x] S1b bracket (also done: S1c, S2b-1, S2b-2)

### Task S2: Measure per-unit cost audit (FM-30)

**Machine · model · time.** Dev Mac, read-only first · opus ≈ 3 h.
**Budget.** ≤ 100 tool calls.
**Approach.** Per fitted star: LM iterations, Jacobian / residual evaluations, pixels per window, time per stage of the
fit (13b's debug sub-step timers), verdict vs pass-2 vs photometry vs totals; the ERP's per-star cost from its set 204
logs (PSF fits per stage and their time, incl. the fits hidden in its calibration / debayer); the hot loops' vectorization
state. Output: a ranked list of bit-identical levers with projected seconds on set 204 (S2b executes the top ones as
their own tasks), and the share of the Measure gap that is count (K / GPU) vs per-unit cost.
- [x] S2 audit report (`fm-s2-report-r1.md`) · [x] S2b levers (S2b-1, S2b-2)

### Task A1: The owner bar

**Machine · model · time.** Dev Mac (quiet, nothing else running) · sonnet judgment run ≈ 4 h; runner ≈ 2 h.
**Budget.** ≤ 60 tool calls per run.

**OWNER Q1 — answered 2026-10-18:** "with the fast mode" — the set 204 bar is met at the owner's own configuration on this Mac (the Fast-preview preset's K, if K ships, and / or `compute.gpu = auto`), with the defaults staying All / off. A1 runs that configuration (interleaved ×3 against the ERP's 1 005 s) and reports the defaults (All / off) beside it, not as the bar — the defaults figure IS the H arm (N at All / off is byte-identical to H by K5 / SD1, its timing is H's); no third arm is run.

**Approach.** B-O with FM-11's config (the owner's answer recorded first); stage table from `stacking stage telemetry`;
identity of N against its own F4 / G7 artifacts; Horse C11 on the runner no slower than H.
**Acceptance.** B-O. A miss is reported with the per-stage gap and no lever is tuned toward it.
- [x] dev Mac set 204 whole run 1082.2 → 794.1 s (N/H 0.734; N runs 794.1 / 796.8 / 793.6), against the ERP's 1005 s (run 2: 968 s) — 21 % faster at defaults; Measure 0.624, Normalize 0.761, Integrate 0.699, Drizzle 0.844, Register 0.966, Calibrate 0.981; identity H3 / N3 1310 files + 197 rows, 0 differing; the per-stage swap max-rule misses come from single N runs and are the OS's response to read rate (Normalize reads ≈ 58 GB per run, now 25 % faster) — malloc at every stage start N ≤ H (lab `fast-measure/a1-mem/vmmap-compare.txt`); lab 1910574 · runner Horse C11 164 lights, 3 pairs from Calibrate: whole run 305 → 267 s (N/H 0.876; the ERP's 2788 s → 10.4x), Measure 0.885, Integrate 0.667; identity 3 / 3 pairs (1962 files); swap 0 in all six runs; lab ae93eac · verdict (met at defaults, see Outcome)

### Task A2: Review, record, squash

**Machine · model · time.** opus final review (Fable if it struggles) · sonnet docs ≈ 2 h.
**Budget.** ≤ 60 tool calls (review); docs ≤ 60.
**Files.** The research note `docs/superpowers/research/2026-10-XX-fast-measurement.md` (the cut table, the truth rows,
the GPU measurements, every measured-out item with its numbers); README ruling **R-OSP-14**; `docs/settings/README.md`
(`fitStars`, `compute.gpu`); the logging dictionary; this plan's Outcome; CLAUDE.md only if a rule, a command surface or a
path changed (the `gpu` feature and module are a new path); the uninstall scripts only if a data or cache location was
added (a pipeline cache would be). The owner summary in plain astrophotographer terms (what K does to star quality and
weights, then the numbers), stating plainly that Linux builds carry the GPU feature and run `cpu` until a Linux GPU is
validated on real hardware (FM-15).
**Gates.** The full `cargo test -p athenaeum-core --all-targets`; both hosts; `tsc` + vitest. Squash onto `one-star-path`;
push only on the owner's word.
- [x] review (opus, per-task) · [x] docs (A2, this pass) · [ ] squash + full tests

---

## Not in this cycle (measured out or out of scope; do not re-propose without new numbers)

- A detection threshold or a peak gate (research Q4: re-defines the weight, tilted by class); the 6 σ seed threshold
  (R-OSP-2); imputed totals and population-(a) frame shape (R-OSP-12, refused by truth).
- A `fitStars` K below F0's B_reg, K = 2 000 included (mono Register identity fails by construction; plan review C1);
  a filter-SNR seed order (FM-16).
- A bit-identical LN refit-list cut bounded by the 500 pairs (research verdict; LN is already faster than the ERP's) —
  recorded as a candidate for a later cycle.
- CPU Integrate levers beyond 4d (its measured-out variants in `combine.rs`); Docker GPU passthrough; a native Metal
  backend (only via SG4); the App Nap run (owner, 2026-10-18: dropped).

## Coverage: the Fable GPU review (`task-13c-review-fable-gpu.md`) → this plan

| review item | where |
| ---- | ---- |
| C1 Kernel 2 cost, layout, measure first, no Integrate projection | FM-1, G1, SG1, "Where the time goes" |
| C2 per-star bars from a CPU control, σ units; absolute bars on aggregates only | G2, B-G G-a / G-d…G-h, SK3 |
| I1 verdict fits stay on the CPU; projection 280–410 s (here 260–385 s on today's 477 s) | FM-10, B-G G-c |
| I2 fast-math fallback = MSL passthrough + safe math; plain f32 may retire it | FM-12, G0, G3, SG4 |
| I3 shuffle butterfly; mirrored workgroup tree; path in the kernel version | FM-10, G1, G3, FM-7 |
| I4 Vulkan first on Windows / Linux, DX12 without subgroups, no GL; TDR | FM-9, G3, G4 |
| I5 per-k float-float LUT | G1 |
| I6 order 4d → K → Kernel 2 → Kernel 1 at the post-K count; no dumps before K | FM-1, G2 |
| M1 −0 → +0 | G1 · M2 6-byte stamps: G0, G2 · M3 faint and saturated planes: F0, G2 · M4 budget + build thresholds: G3, SG5 |
| M5 +99 MiB per GPU plane, cap 2, in admission | G3, G6 · M6 f32 range: G3 · M7 rank moves: B-G G-e, K10 · M8 provenance incl. reduction path: FM-7, G6 |
| Linux amendment: Vulkan, any subgroup width, per-platform validation | FM-9, FM-10 |

## Coverage: the Fable plan review (`fm-review-fable-plan.md`, READY WITH FIXES) → this plan

| review item | where |
| ---- | ---- |
| C1 a: K = 2 000 cannot hold K7; grid 4 000 / 5 000 / 8 000, lowest lifted by F0's Register seed-rank bound | FM-5, B-K K7, F0 step 2 + acceptance (B_reg, the grid), F3 arms, SK2, Not in this cycle |
| C1 b: floor `max(MIN_FIT_STARS, registration.max_stars)` + `warn!` in `resolve_config`; probe unclamped | FM-3, Interfaces, File structure, F1 approach + tests |
| C1: F0 column "Register's usable stars at K"; panel help names the floor and B_reg | FM-4, F0 step 2, F5 files |
| I1 `w_true` the K2 referee, `w_pt` beside; FWHM independent of (s, σ); K2b tilt row | FM-21, FM-6, B-K K2 / K2b, F2 machine line + approach + acceptance, F3 acceptance |
| I2 flux order; SK0 a reported column; matched cut = median filter SNR over ranks K ± 1 % | FM-2, FM-4, FM-16, SK0 (retired), F0 step 2 + STOP, F2 acceptance |
| I3 K8 median 1 ± 0.10 % AND scatter ≤ 1 %, both hard | B-K K8, FM-6 |
| I4 K11 real-data per-frame FWHM / ecc ratio N / H, misses with K3 / K4 passing → Fable | B-K K11, FM-6, F4 acceptance + STOP |
| I5 demotion for the remainder of the run; no `gpu:`-hashed artifacts persist; adapter refusals at run start only | FM-8, FM-14, G6 approach + tests (`demotion_holds_for_the_run`) |
| I6 SG6 basis (Q3) and G5's projection formula named | FM-20, SG6, G5 approach |
| M1 per-task tool-call budgets; G1–G3 cleanup lines | **Budget.** line in every task; G1 / G2 / G3 **Cleanup.** |
| M2 ERP Measure: 72 s descriptors vs 226.4 s warm-up-corrected, both printed | Goal, "Where the time goes" |
| M3 `--backend` / `--reduction` flags on `gpu_proto`; `vulkaninfo` may be absent | G0 files + acceptance, G4 arms, Windows block (`WIN_GPU`) |
| M4 `selection.min_stars > fit_stars` warns; the panel help says it | FM-3, Interfaces, File structure, F1 approach + tests, F5 files |
| M5 `run.rs:13396` read error gets a one-line `warn!` (ruling) | File structure, F1 files + approach |
| M6 vendor family = wgpu `vendor` id (+ `device_type`); driver version recorded; unvalidated model of a validated vendor | FM-9, G6 approach |
| M7 a GPU-made master carries an engine card | File structure (`master_cards.rs`), G6 approach + tests |
| M8 B-O prints the ERP's own 3.7 % spread beside N's | B-O |
| D1 confirmed (`compute.gpu` KV) | FM-13, FM-7 |
| D2 confirmed, amended (I5) | FM-14, FM-8 |
| D3 confirmed; tell the owner Linux runs `cpu` | FM-15, FM-9, A2 files |
| D4 confirmed, SK0 demoted | FM-16, FM-2 |
| D5 confirmed (K10 report-only, truth is the referee) | FM-17, FM-6, B-K K10 |
| D6 confirmed (G-e 19/20, ρ ≥ 0.999) | FM-18, B-G G-e |
| Q1 owner's call (what meets the bar) — ANSWERED (owner's configuration, defaults All / off) | A1 `OWNER Q1` line, FM-11, B-O, F0 depends-on |
| Q2 no K passes → nothing user-facing ships | FM-19, FM-5, Goal, SK2, F5 depends-on |
| Q3 SG6 at All only, 15 % of the whole run (≈ 160 s) | FM-20, SG6, G5 |

## Outcome

- **Owner bar: met at the defaults (All, GPU off).** Set 204, dev Mac, whole run 1 090.7 -> 847.1 s (0.777) against the ERP's
  1 005 s (interleaved bracket H ff3c0a83 / N db762cd9). Final head 301a777f: whole run 1082.2 → 794.1 s (N/H 0.734; N runs 794.1 / 796.8 / 793.6), against the ERP's 1005 s (run 2: 968 s) — 21 % faster at defaults; Measure 0.624, Normalize 0.761, Integrate 0.699, Drizzle 0.844, Register 0.966, Calibrate 0.981; identity H3 / N3 1310 files + 197 rows, 0 differing; the per-stage swap max-rule misses come from single N runs and are the OS's response to read rate (Normalize reads ≈ 58 GB per run, now 25 % faster) — malloc at every stage start N ≤ H (lab `fast-measure/a1-mem/vmmap-compare.txt`); lab 1910574; Horse C11 on the runner:
  164 lights, 3 pairs from Calibrate: whole run 305 → 267 s (N/H 0.876; the ERP's 2788 s → 10.4x), Measure 0.885, Integrate 0.667; identity 3 / 3 pairs (1962 files); swap 0 in all six runs; lab ae93eac; Windows final identity: MSVC / AVX2 on OBS-PC1, vector gate on ("S1 stress … vector == scalar"); `star_probe` probe-diff 0 / 0 / 0 on 0058 / 0075 / 0163_d; the only failing tests are the pre-existing Windows pins (open-items); lab 9d3035e.
- **SK2 (F3): no `fitStars` K passes** (K1 / K3 / K4 miss at 4 000 / 5 000 / 8 000; K4 = bright stars not representative on
  sparse frames). All stays; the config field and the probe flag stay; no UI, no preset (FM-19). F4 and F5 not run.
- **SG6 (G5-pre): the GPU path ends** (GPU-able 89 s = 10.5 % of the run at infinite speed, bar 15 %); SG1: Kernel 2 is
  1.08x / 0.99x. G2-G4, G6, G7 not run; no `gpu` feature, no `compute.gpu`, no Linux GPU caveat to state (FM-15 moot).
- **What landed instead (FM-30):** S1 / S1c (Integrate selection and Median on NEON / AVX2), S2b-1 / S2b-2 (rustafits LM pass
  monomorphized + NEON). All bit-identical, scalar oracle kept, guarded by `ATHENAEUM_REQUIRE_VECTOR_KERNEL`.
- **Records:** `docs/superpowers/research/2026-10-18-fast-measurement.md`, README R-OSP-14, logging env table, CLAUDE.md
  Method rules (one clause), open-items (Windows rustafits pinned-bit tests). No settings, command or uninstall-script change.
- **Owed:** squash onto `one-star-path` + full `cargo test -p athenaeum-core --all-targets` (A2 gate); push only on the owner's word.
