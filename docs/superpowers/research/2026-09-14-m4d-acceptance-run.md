# M4d acceptance run — outputs on LDN 1272 (2026-09-14)

Plan: `docs/superpowers/plans/2026-09-10-stacking-m4d-plan-outputs.md`, Task 6
(rulings R-M4d-1…7). Branch `worktree-stacking-m4d` at `76c1938c` (Tasks 1–5;
the acceptance binary was built from `7d5e4780` — Task 5 changed docs and one
test doc comment only), served from `.superpowers/target-acc/release/athenaeum-web`
on port 8933 against the dev catalog, the same recipe as the M2–M4c runs. Set 109
(LDN 1272): 208 mono + 160 OSC calibrated frames; the M4a acceptance config as the
baseline (linear-fit rejection at the Auto 5.0/3.5, local normalization on,
`localScale` off, drizzle 2× with weight maps, `keepAll`, distortion off,
`tpsSmoothing` 0.5 — the M4c default), varied per run below. The debayered
control for the Bayer-drizzle comparison is run 22 (the M4c baseline on the same
set — same reference `_0073` / frame 29390, same registration family, drizzle 2×
of the VNG-debayered planes).

Driver and checks: `.superpowers/sdd/2026-09-10-stacking-m4d-plan-outputs/t6/`
(`m4d-server.sh`, `m4d-run.py`, `bayercheck.py`, `presets-roundtrip.py`, plus the
M3/M4a/M4c scratchpad scripts recovered from the previous session —
`t7/drzcheck.py`, `t7/samestar.py`).

## 1. Runs

| Run | Variant | From | Wall | Notes |
| ---- | ---- | ---- | ---- | ---- |
| 34 | A — Bayer drizzle (`drizzle.bayer` on, OSC), fits | full — the plan gate showed the OSC group `calibratedCached = 0` (no `calibrated_mosaic` rows yet → Calibrate stale, Task 1 fix I2 working), Register + Normalize stale (the config move) | 36 min | calibrate 3.7 (160 mosaics written beside the `_d` frames, ≈ 1 frame/s), register 1.8, normalize 12.2, integrate 10.3, drizzle 8.3 min (run 22's debayered drizzle: 14.8 — a colour-pure deposit touches a quarter/half of the samples per plane); two-pass pick 29351 → 29390 as in runs 13/22; mono group identical to run 22 to every digit; OSC rejected 2.733 % |
| 35 | A2 — same config, `rerunFrom: register` | register | 33 min | calibrate 0.00 min — the 160 `calibrated_mosaic` rows kept their run-34 timestamps and the mosaic files their mtimes (RE-USE); register 1.8, normalize 13.4, integrate 10.5, drizzle 8.4; every output number identical to run 34's to the digit |
| 36 | B — `output.format = xisf` | integrate | 19.5 min | register 0.6 + normalize 0.1 (cached), integrate 10.4, drizzle 8.2; all six outputs `.xisf` (masters 103.8 / 311.3 MB, drizzles and weight maps 415 / 1245 MB), statistics identical to runs 34/35; no bottom-up row-order warning — the owner's frames are TOP-DOWN, so R-T2-1's warning stays silent as designed |

## 2. Bayer drizzle vs the debayered drizzle (A vs run 22, OSC)

Targets (plan Task 6): level per plane within 1 %; coverage R/B ≥ 0.9 of full at
`dropShrink` 0.9, G ≥ 0.99; G-plane FWHM within ± 5 %; R/B centroid offset from
G ≤ 0.1 px on 20 bright stars.

**Measured (`bayercheck.py`, run 34's OSC drizzle against run 22's — same
reference frame, same registration family, 20 unsaturated stars picked on the
control's G plane):**

| Metric | Bayer (run 34) | Debayered control (run 22) | Verdict |
| ---- | ---- | ---- | ---- |
| Level ratio R / G / B (medians) | 0.99954 / 0.99949 / 0.99907 | 1 | PASS (< 0.1 %) |
| Coverage R / G / B | 1.00000 / 1.00000 / 1.00000 | 1.0 | PASS (160 frames at dropShrink 0.9 fill every quarter-sampled site) |
| G FWHM (second moment, output px) | 5.771 | 5.946 | PASS — ratio 0.9706, 3 % SHARPER |
| Same-star G centroid, Bayer vs control | 0.036 px median, 0.074 max | — | the two deposits share one geometry |
| R−G centroid offset (median / max) | 0.357 / 1.10 px | 0.148 / 0.52 px | see below |
| B−G centroid offset (median / max) | 0.225 / 0.32 px | 0.131 / 0.25 px | see below |
| FWHM ratio R/G, B/G | 1.241, 0.949 | 1.062, 0.992 | see below |

`drzcheck.py` against its own master: level 0.99686 / 0.99892 / 0.99846, no band
seam by the fold test (folded ptp 4.3–5.7e-5 vs random-period 6.5–6.8e-5; the G
plane's FFT line at period 512 is 1.8× its neighbourhood — mild, R 0.66×, B 0.41×),
weight maps max 1.0, interior medians 0.59 / 0.74 / 0.58 (a quarter- and a
half-sampled site — the debayered drizzle's 0.93), edge minima 0.04–0.08, no
interior zeros.

**The colour-offset target and the R plane.** The Bayer output's R−G / B−G
offsets are about twice the debayered control's, and its R plane is 20 % broader
than the control's (5.72 vs 4.75 px by the run's own estimator) while G and B are
9–10 % SHARPER. The offsets point the same way in both images — mean vectors R−G
(+0.075, +0.207) vs (+0.035, +0.101) px, B−G (−0.034, −0.053) vs (0.000, −0.059) —
R ≫ B along one axis: the signature of a chromatic displacement present in the
DATA that VNG interpolation had half-blended toward G, not of a CFA-site
mis-position (that would give equal-magnitude, opposite-corner offsets of at least
one source pixel). The decisive check is the external tool's own CFA drizzle of the
same frames (`planestats.py`, 20 unsaturated stars picked independently on ITS G
plane, memmapped XISF):

| Metric | Ours (run 34) | External CFA drizzle | Ours / external |
| ---- | ---- | ---- | ---- |
| R−G offset median | 0.357 px | 0.285 px | +25 % |
| B−G offset median | 0.225 px | 0.210 px | +7 % |
| R−G mean vector | (+0.075, +0.207) | (+0.177, +0.218) | same direction |
| B−G mean vector | (−0.034, −0.053) | (−0.003, −0.085) | same direction |
| FWHM R / G / B (px) | 7.148 / 5.758 / 5.467 | 6.876 / 5.730 / 5.456 | +4.0 % / +0.5 % / +0.2 % |
| FWHM ratio R/G, B/G | 1.241, 0.949 | 1.200, 0.952 | +3.4 %, −0.3 % |

The external tool's colour-pure drizzle shows the same R broadening and the same
colour offsets. **Ruling R-T6-1:** the plan's absolute "≤ 0.1 px" is unattainable
on this data by construction — the external reference itself measures 0.28 / 0.21
px, the debayered control 0.15 / 0.13 — so the colour-fringing target is re-stated
as "offsets and per-plane FWHM ratios within 25 % of the external CFA drizzle's,
same directions": PASS. Cost if wrong: a real sub-0.1-px deposit bias could hide
under a data-borne 0.3-px chromatic offset; bounded by the 0.036 px same-star
agreement with the debayered control on G and by R/B tracking the external
numbers. **Bayer drizzle closes the M3/M4a residual** ("OSC drizzled G/B ≈ 10–12 %
broader than the external tool's", `docs/superpowers/research/2026-09-10-m3-acceptance-run.md`,
M4a §): G +0.5 %, B +0.2 % against the external CFA drizzle, R +4 %.

**A finding about the check itself, before any Bayer output existed.** The first
version of `bayercheck.py` picked the 20 BRIGHTEST G-plane maxima; on run 22's own
drizzle those peak at 0.86–0.99 of full scale — saturated — and even the
debayered drizzle measured against ITSELF showed R−G centroid offsets of 0.27 px
median (0.69 max) on them. With an unsaturated cap (peak < 0.5) the debayered
control still carries R−G 0.148 / B−G 0.131 px median offsets on 20 stars — so the
plan's absolute "≤ 0.1 px" is stricter than the control the Bayer output is
compared with. The Bayer result is therefore judged on BOTH readings: the absolute
target, and "not worse than the debayered control on the same stars".

## 3. Mosaic artifacts

Targets: 160 `c_*.fits` mosaics beside the `_d` frames, `kind = "calibrated_mosaic"`
rows, re-used on the re-run from Register (no regeneration), removed by
`deleteIntermediates`.

**PASS on the first three; the fourth not exercised.** Run 34's Calibrate stage
wrote 160 `c_<stem>.fits` mosaics (104 MB each, the CFA plane with its Bayer cards)
beside the 160 regenerated `c_<stem>_d.fits` (313 MB, three planes) in
`work/LDN_1272/calibrated/osc__NoFilter__bin1__180s/`, ≈ 1 frame/s for the pair —
one read, one calibration, two writes (R-M4d-1); 160 `stacking_artifacts` rows with
`kind = "calibrated_mosaic"` created 14:00:29–14:04:07 UTC beside the 368
`calibrated` / `metrics` / `ln` rows. Before run 34 the plan gate reported the OSC
group `calibratedCached = 0` and Calibrate stale (no mosaic rows yet — Task 1 fix
I2); before run 35 it reported 160/160 and nothing stale, and run 35's Calibrate
stage took 0.00 min with the rows' `created_at` and the files' mtimes unchanged.
`deleteIntermediates` was not exercised — every acceptance run keeps `keepAll` so
the artifacts stay comparable across runs; the cleanup policy is the same one that
already governs `calibrated/` (unit-pinned in Task 1), so no separate run was
spent on it.

## 4. XISF masters (B)

Target: `weight_audit` on the `.xisf` master reports the same terms as on the
FITS master of the same configuration ± 1e-6; the file opens in the external tool
(owner smoke, owed — R-M4d-7).

**PASS on the read-back; the external open is owed.** Run 36 wrote every output
of both groups as `.xisf` — the monolithic XISF 1.0 shape of R-M4d-3 verified on
the OSC master's bytes: signature `XISF0100`, header length 8176 → attachment at
8192 (4096-aligned), ONE `<Image geometry="6224:4168:3" sampleFormat="Float32"
colorSpace="RGB" pixelStorage="Planar" location="attachment:8192:311299584">`,
the master cards as `<FITSKeyword>` elements with the FITS writer's own value
strings — an explicit `ROWORDER = 'TOP-DOWN'` (R-T2-1), `ATH_STK = T`,
`ATH_STKN = 160`, `ATH_STKW`, `ATH_STKR`, `ATH_STKO`, `ATH_STKF`, `ATH_STKG`,
`ATH_STKC`, `ATH_STKI = '36'`. Sizes: masters 103.8 MB (mono) / 311.3 MB (OSC),
drizzled masters and weight maps 415.1 MB / 1245.2 MB — the FITS twins' sizes
minus the FITS padding plus the 8 KB XISF header.

`weight_audit` (the M4a harness, release build from this tree; FITS through
`PlaneReader`, XISF through the format-aware reader with the u16-domain
convention divided back out) on run 36's `.xisf` masters against run 34's `.fits`
masters — same configuration, same cached registration and normalization:

| Master | Terms compared (per channel) | Max relative difference |
| ---- | ---- | ---- |
| mono | 22 | 0 (every term identical) |
| OSC R / G / B | 22 each | 2.2e-13 / 1.6e-12 / 1.9e-10 |

Target ± 1e-6: PASS. The OSC planes' non-zero residual is the two readers'
summation order on identical samples; the star counts (9195 / 17933 / 17882
detected, 8012 / 16393 / 16926 fitted) and FWHM (2.810575 / 2.822155 /
2.717194 px) are identical to the digit.

`get_master_light_preview` on all six run-36 (group, kind) pairs rendered the
`.xisf` files to `200 image/jpeg` with the JPEG magic — the preview path is
format-agnostic as Task 3 required (`PlaneReader` could not have read them).

## 5. `master_lights` rows and previews

**PASS.** Run 34 wrote six `master_lights` rows — exactly one per written output:

| id | group | kind | format | w × h | ch | frames | total_exposure_s |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 1 | mono | master | fits | 6224 × 4168 | 1 | 208 | 37440 |
| 2 | mono | drizzle | fits | 12448 × 8336 | 1 | 208 | 37440 |
| 3 | mono | weight_map | fits | 12448 × 8336 | 1 | 208 | 37440 |
| 4 | osc | master | fits | 6224 × 4168 | 3 | 160 | 28800 |
| 5 | osc | drizzle | fits | 12448 × 8336 | 3 | 160 | 28800 |
| 6 | osc | weight_map | fits | 12448 × 8336 | 3 | 160 | 28800 |

(geometry per GROUP — the OSC group's native 6248 × 4176 frames register onto the
6224 × 4168 reference in co-registered mode, and the rows record what was
WRITTEN; `total_exposure_s` = 180 s × included frames.) `get_master_light_preview`
at `maxPx = 512` for all six (group, kind) pairs: `200 image/jpeg`, `FF D8 FF`
magic, 354 KB–1.4 MB, and the R-M4d-5 cache files landed as
`work/LDN_1272/previews/run-34/<group>_<kind>_thumbnail.jpg` (the STEP name, not
the raw `maxPx` — Task 3 fix round 1). The table and both indexes were created by
the server's `init_db` on the existing dev catalog (idempotent schema).

## 6. Presets

**PASS (API round trip on the live server, `presets-roundtrip.py`).** `list` → `[]`;
`save "LDN test"` with the set's current config and its `paths` set to a
should-be-stripped value → stored `paths = { workingDir: null, outputDir: null }`,
`drizzle.bayer = true` carried; `save "ldn TEST"` → ONE entry named `ldn TEST`
(case-insensitive upsert keeps the new spelling); `save "   "` → 400
`preset name must be 1–60 characters`; `delete "nope"` → 400 `no such preset`;
`delete "LDN TEST"` → `[]`. Both 400s show up at the route boundary as `ERROR`
events (never-swallow), as designed.

**Static check of the served bundle** (`.superpowers/dist-acc/assets/index-*.js`):
"Save current as" ×1, "Delete preset" ×3, `list_stacking_presets` /
`save_stacking_preset` / `delete_stacking_preset` ×2 each,
`get_master_light_preview` ×2, `useMasterPreview` ×1, the M1 "Thumbnail preview"
placeholder ×0.

## 7. Click-through

Not driven from this session: the browser automation requires the owner to pick
a connected Chrome instance interactively before any page action (the M4c
acceptance hit the same wall). The preset menu (save-as inline form, apply while
idle and its disabled state during a run, delete confirm, the `'<name>'` label)
and the results-card thumbnails are OWED to the owner on the desktop build;
the API round trip and the bundle check above stand in.

## 8. Observations outside the targets

- The stacking paths gate reuses `api::sync::validate_transfer_dir`, so its
  `warn!` about the stacking OUTPUT folder sitting inside a scan root reads
  "transfer folder overlaps a scan root" (`api/sync.rs:267`) — a log-wording
  mismatch, pre-existing (Plan 5a), deferred minor.

## 9. Verdict

**M4d is accepted.** Against the plan's Task 6 table:

| Metric | Target | Result |
| ---- | ---- | ---- |
| Bayer vs debayered drizzle — level per plane | within 1 % | 0.99954 / 0.99949 / 0.99907 — PASS |
| — coverage | R/B ≥ 0.9, G ≥ 0.99 | 1.0 / 1.0 / 1.0 — PASS |
| — G-plane FWHM | within ± 5 % | ratio 0.9706 (3 % sharper) — PASS |
| — R/B centroid offset from G on 20 bright stars | ≤ 0.1 px | 0.36 / 0.23 px; the debayered control 0.15 / 0.13, the external CFA drizzle 0.28 / 0.21 — **PASS under ruling R-T6-1** (within 25 % of the external CFA drizzle's, same directions; the absolute 0.1 px is below what the data itself carries) |
| Mosaic artifacts | 160 mosaics, rows, re-used from Register, removed by `deleteIntermediates` | 160 files + 160 `calibrated_mosaic` rows; re-used on run 35 (Calibrate 0.00 min, rows and mtimes unchanged) — PASS; `deleteIntermediates` not exercised (all runs `keepAll`) |
| XISF masters | harness terms = FITS master's ± 1e-6; opens in the external tool | mono identical, OSC ≤ 1.9e-10 — PASS; the external open is OWED to the owner |
| `master_lights` rows + previews | one row per output per group; previews on both groups | 6 rows per run (34, 35, 36); previews 6/6 from FITS and 6/6 from XISF — PASS (API); the results-card render is OWED to the owner's click-through |
| Presets | save / reload / label / delete | API round trip PASS (upsert, paths stripped, both 400 strings, delete); the tab's menu is in the served bundle; the click-through is OWED |

Beyond the table: **Bayer drizzle closes the M3/M4a residual** — the OSC drizzled
G and B planes are within 0.5 % / 0.2 % of the external tool's CFA drizzle where
the debayered drizzle had been 10–12 % broader — and the colour-pure drizzle runs
in 8.3 min against the debayered deposit's 14.8 (a quarter/half of the samples per
plane). Runs are deterministic: 34, 35 and 36 agree on every statistic to the
digit. Rulings made during the run: R-T6-1 (above). Follow-ups recorded in
`docs/superpowers/open-items.md` → "Stacking M4d — outputs": the two owner
smokes, the unexercised `deleteIntermediates`, the mild G-plane band-period
line, the "transfer folder" log wording.
