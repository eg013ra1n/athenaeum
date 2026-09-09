# M1 acceptance run — LDN 1272 on the real catalog

**Date:** 2026-09-10 · **Plan:** `docs/superpowers/plans/2026-09-09-stacking-m1-plan5b-stacking-tab.md` (Task 7) · **Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §2, §13 · **Baseline:** `2026-09-09-checkpoint-b-integration.md` (Plan 4's probe on the same set) · **Machine:** Mac mini, 10 cores, 16 GB RAM, macOS 26.5.2; lights on `/Volumes/bigbase3` (external), work/output on `/Volumes/BigMac`.

## 1. Purpose

The first end-to-end run of the stacking pipeline from the product itself — the Stacking tab, the run thread, the tables — on the owner's real dev catalog: set 109 "LDN 1272", 208 mono lights (ATR2600M) + 160 OSC lights (ZWO ASI2600MC Duo), 180 s each, against the targets Checkpoint B pinned and the external stacker's masters. Everything the run needed and did not find on disk, the run had to build itself (owner requirement 2026-09-09; spec §2 stage 0.5).

## 2. Setup

- **Build:** `athenaeum-web` release from branch `worktree-stacking-m1-plan5b` — commit `31a02733` for the runs (Plan 5b Tasks 1–8b landed; the later `32ba14cb` is a robustness wrap in the same gate that cannot change this catalog's plan) — built into a separate target dir. Frontend built with `NODE_ENV=development` (the tab is behind `STACKING_ENABLED = import.meta.env.DEV`) and served by the web server itself (`ATHENAEUM_STATIC_DIR`) on `http://localhost:8931`, i.e. the shipped same-origin shape. The Vite dev server could not be pointed at a separately running API: the web server has no CORS layer and the Vite config no proxy (a dev-workflow note, not a product defect).
- **Catalog:** the dev catalog (`com.vsharifov.athenaeum.dev/athenaeum.db`), `ATHENAEUM_ALLOWED_PATHS` unset (AllowAll), no API key, `ATHENAEUM_LOG=info`, `compute.max_concurrent = 1`.
- **Folders (Settings → Stacking):** working `/Volumes/BigMac/Users/astrobureau/Pictures/Astro/Stacking/work`, output `…/Stacking/output` — inside the `Pictures/Astro` scan root; the validator warned (scan-root overlap, artifacts carry the scanner-skip card) and accepted. 768.8 GB free, plan estimate 67.1 GB.
- **Calibration library:** `/Volumes/bigbase3/Calibration/` was empty — the 11 master files the catalog links have been gone since 2026-09-08. Nothing was rebuilt by hand; stage 0.5 did it.
- **Step 0 — the lights had moved.** On 2026-09-07 the owner moved the whole `Pictures/Astro/Unsorted/edph` tree (1563 files: 1392 lights, 171 flats) to `/Volumes/bigbase3/Astrolib/Darks/Unsorted/edph`. The dev catalog still pointed at the old paths (0 of 368 lights on disk; the prod catalog already knew the new ones). One **Find new images** on scan root 8 (`start_scan`, 69 s, 6703 files found) re-pointed all 1563 rows in place through the scanner's header-fingerprint move detection — ids preserved, set 109 membership intact, 368/368 lights and 50/50 raw flats on disk afterwards; 37 unrelated new files were cataloged, 4 pre-existing corrupt-header files under `Astrolib/Darks` were reported.

## 3. The plan before the run — and the defect it exposed

`get_stacking_plan(109)`: no blockers, 208 + 160, `configHash 268adcec1face673` (the Default preset), `staleStages = [calibrate, measure, register]`, `missingMasterFiles = 7`, seven rebuild items (2 darks 180 s, 5 flats).

**Finding 1 (blocking, fixed before the run — Task 8b, commits `31a02733` + `32ba14cb`).** Eleven master files were missing, the plan listed seven. The readiness walk covered only the lights' direct Dark/Flat/Bias links; a missing master FLAT is rebuilt through `select_flat_precal` over the raw flat set's own sub-calibration links (829/911 → the 1 s master darks 1750/1747, 713 → master bias 1746 — all missing too), and `load_precal_pixels` fails on a missing file ("pre-cal master unreadable") with no fallback. The run would have rebuilt the two 180 s darks and failed on the first flat. Fix: the gate asks the build's own selector which pre-calibration master a missing flat master's rebuild would read and lists it when its file is missing; `type_build_rank` already orders it first. After the fix the plan lists **10** masters in build order — bias 1746; darks 1747 (1 s Duo), 1748, 1749, 1750 (1 s ATR); flats 1751–1755 — and correctly leaves out the Duo bias 1745, which no chain reads.

## 4. Run 1 — the full pipeline (auto reference)

Started from the tab's **Run stacking** at 22:34:43 Z, Default preset, run id 1; the sidebar's compute-queue entry read "Stacking · LDN 1272 running"; one completion notification arrived (bell badge 1).

| Stage | Wall | Notes |
| ----- | ---- | ----- |
| 0.5 Masters | 2 min 08 s | 10 rebuilt at their exact cataloged paths: bias 31.9 s (100 frames, 226 MB/s), 180 s darks 20–28 s, 1 s darks 12–13 s, flats 4.3–5.1 s each |
| Calibrate (+ debayer) | 6 min 37 s | 368 frames, 66.8 GB of float32 artifacts |
| Measure & select | 10 min 22 s | fan-out under the memory budget (see the miss below) |
| Reference | 0 | auto → `2025-10-18_02-18-35_0077` (weight 1.000) |
| Register | 1 min 15 s | both groups; homography 207/207 + 160/160, rms median 0.14 / 0.21 px |
| Integrate | 4 min 15 s | both groups, Average · linear fit clip 5.0/3.5 (Auto rule, n ≥ 20) |
| Output | 0.3 s | two masters, WCS from the stored solve |
| **Total** | **24 min 38 s** | no warnings |

**Mono** (208/208 included, 0 below the weight floor): rejected 0.092 % low / 0.561 % high; master MRS noise 1.7794e-05; FWHM 2.706 px; ecc 0.304; SNR gain 816; weights min 0.138 / median 0.370 / max 1.000; 21.6 GB read in 50.5 s, combine 19.5 s.
**OSC** (160/160): rejected 0.089 % / 0.544 %; noise [1.6555e-05, 1.9691e-05, 1.9472e-05]; FWHM [2.97, 2.87, 2.69]; normalization reference `…_0019` (0.980) as in Checkpoint B; 49.8 GB read in 132 s, combine 44 s.

Two direct reproducibility checks against Checkpoint B, whose inputs were calibrated with the ORIGINAL masters (the app's calibrated export of 2026-09-09):

- a calibrated mono frame and a calibrated OSC frame from run 1 are **bit-identical** to their Checkpoint B counterparts (`fitsdiff`: equal 1.0000, max|d| 0, all planes) — the ten rebuilt masters reproduce the originals exactly;
- the per-frame weights are identical (`weightedExposureS` 16193.6675029098 in both, to every digit) and the rejected fractions match to four significant figures.

The master itself differed (pixel rms/MAD 134 against Checkpoint B's master) for one reason: **the reference**. Checkpoint B registered onto the external stacker's reference frame `2025-10-18_02-02-02_0073` for its like-for-like comparison; run 1's auto mode chose the best-weighted frame `_0077` (`_0073` ranks 6th at 0.929). The noise pin (1.758e-05 to 3 s.f.) is defined on the `_0073` geometry, so run 2 pins it.

## 5. Run 2 — the like-for-like re-run (manual reference, re-run from Register)

`set_frame_set_reference(109, 29390)` (the Analysis tab's "Set as reference" feature) + a per-set config with `reference.mode = manual` (the tab shows the preset chip as **Custom**; config hash `fd2c9a6cc7b019c4`); the plan then reported `staleStages = [register]` only, 208 + 160 calibrated and measured artifacts cached, nothing to build. **Re-run from Register**, run id 2:

| Stage | Wall |
| ----- | ---- |
| Calibrate / Measure | 32 ms / 86 ms — `cached_calibrated` 368/368, `cached_metrics` 368/368 |
| Register | 1 min 17 s |
| Integrate | 4 min 13 s |
| Output | 0.3 s (`_2` masters — the collision suffix) |
| **Total** | **5 min 30 s** |

**Both masters of run 2 are bit-identical to Checkpoint B's masters** (`fitsdiff` equal 1.0000, max|d| 0 — mono, and all three OSC planes). Mono: noise **1.758051e-05**, rejected **0.0920 % / 0.5610 %** (identical to Checkpoint B to every digit), SNR gain 831.1, FWHM 2.707. OSC: noise [1.6514e-05, 1.9753e-05, 1.9379e-05] = 0.99 / 1.19 / 1.29 of the external log's [1.6716e-05, 1.6631e-05, 1.4975e-05] (Checkpoint B §6: 1.00 / 1.18 / 1.27, the per-channel input-gain picture), rejected 0.089 % / 0.544 %.

Consequence: the product pipeline — tab → run thread → stage 0.5 → cached calibrate/measure → register → normalize → integrate → output — reproduces the Plan 4 probe's checkpoint exactly, so every number in Checkpoint B §4–§9 against the external masters (mono noise ratio 0.90 by the log / 0.87 by our estimator, FWHM 2.71 vs 2.67, 299/300 star centroids within 0.03 px, rejection 0.65 % vs 2.83 % attributed to local normalization = M2) holds for these masters verbatim.

## 6. Targets (spec §13 + ruling 9)

| Target | Result | Verdict |
| ------ | ------ | ------- |
| Frames registered | 208/208 + 160/160, both runs; homography everywhere, rms median 0.13–0.21 px | pass |
| Mono master MRS noise = 1.758e-05 (3 s.f.), identical numeric path to Checkpoint B | run 2: 1.75805e-05, master bit-identical | pass |
| Rejected 0.092 % / 0.561 % (2 s.f.) | 0.0920 % / 0.5610 % (runs 1 and 2) | pass |
| OSC per plane as Checkpoint B §6 | run 2 OSC master bit-identical; 0.99 / 1.19 / 1.29 of the external noise | pass |
| Whole set wall time ≤ 60 min | 24 min 38 s including 2 min of master rebuilds | pass |
| Measure under 5 min with the fan-out (Checkpoint B: 12.9 min sequential) | 10 min 22 s | **miss** — attributed, §8 |
| Disk ≤ 85 GB in the working folder | 66.76 GB (all of it calibrated frames; registered frames not written by default) | pass |
| Artifacts: no residual trail at 400 % around (5760, 1700) | clean on run 1's and run 2's mono master (`assets/2026-09-10-m1-acceptance/run{1,2}-mono-master-trail-region-x4.png`) | pass |
| Re-run from Integrate: every `cached*` flag true, only integrate/output take time, a `_3` master | run 3: 368/368 cached on all three flags, calibrate + measure + register 0.6 s together, integrate 4 min 14 s, `_3` masters | pass |
| Cancel mid-measure → `cancelled`, artifacts kept, no master | run 4: `cancelled` after 16 s, 368 calibrated artifacts kept, no master | pass |
| Delete intermediates → usage drops to the runs folder | 71.68 GB → 929.5 KB of run manifests, masters kept | pass |
| Stage timings, compute-queue entry, notification, master names, `GroupStats` per group recorded | §4, §5, `run-1.json` / `run-2.json` in the plan workspace | done |

## 7. Re-run from Integrate, cancel mid-measure, delete intermediates

**Run 3 — re-run from Integrate** (23:08:53 Z → 23:13:08 Z, **4 min 15 s**): calibrate 31 ms, measure 86 ms, register 456 ms (`cached_registration` 368/368 — the stage only re-reads its rows), integrate 4 min 14 s, output 0.3 s; `_3` masters written; mono noise 1.758051e-05, i.e. the same master again. Every `cached*` flag true for every frame. Pass.

**Run 4 — cancel mid-measure.** `measurement.maxStars` 24576 → 20000 in the set's config makes Measure stale (`staleStages = [measure]`, calibrate still cached); **Run stacking**, then the tab's **Cancel** while Measure showed 0/368. The run ended `cancelled` 16 s after it started (23:14:43 Z → 23:14:59 Z), `error = null`, its manifest lists the one stage that completed (calibrate, 31 ms, from cache); no `_4` master; the 368 calibrated artifacts stayed on disk (usage 71.68 GB calibrated / 0.95 MB run manifests). The board showed every stage as "Skipped" afterwards (finding, §8). Pass.

**Delete intermediates** (the results panel's button → the in-app confirmation "Delete stacking intermediates? Removes this set's registered, calibrated and local-normalization working files … Run manifests and master lights are kept."): usage dropped from 71.68 GB to **0 B calibrated · 0 B registered · 0 B local-norm · 929.5 KB run archives**; `work/LDN_1272/` holds `runs/` (four manifests) and an empty `tmp/`; the six masters in the output folder are untouched. Pass.

## 8. Findings and rulings

1. **Stage 0.5 missed the pre-calibration masters** (Finding 1, §3) — fixed as Task 8b before the run; the run then rebuilt all ten in dependency order.
2. **Measure 10.4 min vs the 5 min target — attributed to the memory budget on this machine, not to the driver.** The fan-out admits `clamp(RAM/4 ÷ working set, 1, cores)` frames at once; with 16 GB of RAM the budget is 4 GB, the mono working set is 0.83 GB (4 frames at a time) and the OSC working set 2.5 GB (**one** frame at a time). Checkpoint B's sequential probe took 12.9 min; the fan-out saved 20 %, all of it on the mono group. A 64 GB machine gets the intended parallelism. Ruling: recorded, no code change in M1; the per-frame working-set formula (8 planes × W × H × 4 B) is worth revisiting in M4's performance pass — the measurement needs far fewer than eight full planes resident.
3. **The reference decides the pin.** Auto mode picks the best-weighted frame; Checkpoint B's numbers are defined on the external stacker's reference. Both are correct behaviour; the note pins run 2 as the like-for-like number and keeps run 1 as the product's default answer.
4. **Web host, outside stacking:** opening Settings → General wedged the whole server — `api::account::build_status → TokenStore::load → SecKeychainFindGenericPassword` blocked in a `mach_msg` to `securityd` (a macOS Keychain access prompt for the unsigned acceptance binary) and every other request queued behind it (the stacking entry points heal interrupted runs on the DB first). Recorded in open-items: the account status must not block the runtime (`spawn_blocking`, no shared lock across the keychain call).
5. **Minor UI findings** (for the branch's final fix wave): the Integrate summary shows "min weight 0.01" for `minWeight = 0.005` (rounding in `stageSummary.ts`); master labels format the exposure with 0 decimals ("Flat 0s 0°C ATR2600M" for the 0.39 s flat; `calibration_set_label`); the Integrate row's progress text reads "0 / 1 · 100 %" (per-group count next to the current group's band percentage); after closing the provenance modal the main pane sat scrolled ~200 px right (the frames table is wider than the pane); the frames table shows "—" in the Group column before any run.
6. **Not verifiable from this harness:** the narrow-layout check (ruling 5) — the browser window ignored resize requests (full-screen window); it stays on the owner's click-through list in open-items.
7. **Dev workflow:** the Vite dev server cannot call a separately running web API (no CORS layer, no proxy) — use the same-origin static build, as here; noted in open-items, not a product defect.

## 9. Verdict

M1 is **green** on every target except the measurement time, which is a machine-memory attribution with a named follow-up, and the tab is enabled for release (`STACKING_ENABLED` removed). The pipeline built its own calibration masters from an empty library, calibrated bit-identically to the previous masters, and reproduced the Plan 4 checkpoint masters bit for bit through the product's own run path.

## 10. Release-note lines (draft, English)

- **Stacking.** A frame set's new **Stacking** tab builds its master light(s) in-app: calibration, measurement and weighting, reference selection, registration, integration with automatic outlier rejection, and a FITS master with WCS — one run, a pipeline board with per-stage configuration and presets, cached stages for fast re-runs, and full provenance per run.
- The stacking run builds any calibration master it needs and cannot find — including the pre-calibration masters a flat's rebuild reads — before calibrating, so an empty library is never a blocker.
- **Settings → Stacking:** pipeline defaults (the same tree a frame set can override) and the working/output folders.
- The web build's folder picker now answers 400 to an unknown scope instead of falling back to the scan roots.
- The plate-solve-based registration preview (dev-only) is gone; registration lives inside a stacking run.
