# Stacking

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


In-app light stacking — the frame set's own master light(s), built from its
matched calibration and a chosen reference, with no external stacker. Spec:
`docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`; M1 plans in
`docs/superpowers/plans/`: `2026-09-08-stacking-m1-plan1-pixel-path.md`,
`2026-09-09-stacking-m1-plan{2-measurement,3-registration,4-integration,
5a-orchestration,5b-stacking-tab}.md`. Retired
2026-09-09: the plate-solve-era registration flow (`register_frame_set` /
`get_frame_set_registration` / `cancel_frame_set_registration`, dev-only
`StackingPrepTab`) this feature replaces — `registration::db` and
`set/get_frame_set_reference` stay (the stacking run writes/reads
`registration_results`; the Analysis tab's "Set as reference" star still
calls them).

**Pipeline stages** (`stacking::plan::Stage`, spec §2): `Masters` (0.5 —
build/rebuild whatever the export-readiness gate found missing, including the
pre-calibration master a missing flat master's rebuild reads, dependency
order bias/darkflat → dark → flat, so a run never blocks on a master it can
build itself) → `Calibrate` (reuses the calibrated-lights export engine
verbatim) → `Measure` (the shared `star_measure` path, `MEASURE_VERSION` 7,
the aperture correction `c` kept per plane — see "One star-measurement
path, Plan A" and R-OSP-16) → `Reference` (one frame for the whole set — highest
weight, or the user's pinned choice via `set_frame_set_reference`; this
frame REGISTERS every group — since ruling R-M3-17 (2026-09-10) it no
longer anchors normalization, which every group instead picks sky-
penalized: `s = weight / sqrt(background)`, so a dark-sky frame outranks a
brighter-sky one of similar weight. With `reference.twoPass` (default
true, M4a) an Auto reference first registers its OWN group in a dry pass
(no rows, no artifacts), takes the median rotation/translation of the
successful alignments, scores the top `TWO_PASS_CANDIDATES = 10` frames by
weight by their corner displacement from that median (`hypot(Δθ_rad·D/2,
|Δt|)`, `D` the reference diagonal), and switches to the closest one when
the current reference is worse by ≥ `TWO_PASS_MIN_GAIN_PX = 4.0` px
(`stacking/weights.rs::two_pass_pick`) — a switch updates the run row, the
summary's `reference.switchedFrom` and adds one run warning; Manual
references never move and the `FastPreview` preset turns it off
(R-M4a-18); the plan-time stale check keys on the last run's recorded
reference so a switch doesn't make Register stale next time (R-M4a-6),
but the dry pass itself is uncached and runs every time) → `Register`
(registration v2:
quad-seeded RANSAC + distortion) → `Normalize`
(local normalization, M2 — see below; a no-op when
`normalization.local.enabled` is off and `normalization.rejection` isn't
`"local"`) → `Integrate` (banded, weighted, Auto rejection) → `Drizzle` (M3
— see below; a no-op unless `drizzle.enabled`) → `Output` (master-light
header/naming/writers + WCS/SIP from the stored solve).
`Calibrate`/`Measure`/`Register`/`Normalize` are the only
cacheable per-frame stages (`stacking_artifacts`, keyed by a per-stage
config hash — spec §9.3; `StackingPlan.stale_stages` lists which of the
four a fresh run would have to redo).

**The plan gate** (`stacking::plan::build_plan`, DB + cheap FS probes, no
pixel I/O) returns a `StackingPlan`: groups (`stacking::groups`, camera-
agnostic since owner decision 2026-09-10 — catalog grouping by colour mode/
filter/binning/exposure cluster; camera and native geometry are display
facts only, `PlanGroup.cameras`/`instrume`, never keys), the resolved
config + its hash, the reference, folder/space state, and ordered blockers
(`code` ∈ `masters | links | masterFiles | reference | folders | space |
frames | unsupported`; a project plan adds `grid` while the project's grid
is unconfirmed) — reusing the calibrated-export readiness gate
(`ExportReadiness`) for the masters/links checks, so the two features can
never disagree about what "ready to calibrate" means. A blocking `code` at
the front of the list stops `start_stacking` cold; anything past it is
informational (e.g. a stale-stage note).

**The run** (`stacking::run`): `start_stacking` validates the plan, inserts
the `stacking_runs` + group rows, registers a cancel handle on
`ServiceContext::active_stacks` (gated `#[cfg(all(feature = "render",
feature = "solver"))]`, matching `stacking`'s own home — absent in a headless
build), and spawns a dedicated `stacking-run-<id>` thread admitted through
the shared `ComputeQueue` (`ComputeJobKind::Stacking`, label `"Stacking ·
<set name>"` — no separate queue widget, see below). `cancel_stacking` flips
the cancel flag; the thread notices it between frames/groups and unwinds
cleanly. `heal_interrupted_runs` runs on demand (not host-startup-driven)
from every `api::stacking` entry point, finishing any run row a crashed
process left stuck as `"failed"` with `error = "interrupted by a restart"`.
Progress rides `stacking-progress` (per stage/group/frame, throttled 300 ms)
and exactly one `stacking-complete` fires from the run's single exit path
regardless of success/cancel/failure/panic.

**26 commands** (`api/stacking.rs` + `commands/stacking.rs` +
`routes/stacking.rs`, all mirrored on both hosts) — the frame-set path's 19
listed here plus the seven project-path twins (`*_project_stacking_*`, see
"Commands (D12)" under the project path): `get_stacking_plan`,
`start_stacking`, `cancel_stacking`, `get_stacking_runs`, `get_stacking_run`,
`get_stacking_config`, `set_stacking_config`, `get_stacking_presets` (Default
/ Fast preview / Maximum quality, a single Rust source of truth so the tab
never re-implements the transforms), `get_stacking_defaults`,
`set_stacking_defaults`, `reset_stacking_defaults`, `get_stacking_paths`,
`set_stacking_paths`, `get_stacking_work_usage`, `cleanup_stacking_work`,
`get_master_light_preview` (M4d Task 3 — JPEG bytes for one written master
light, `maxPx` clamped to `[64, 2048]` and cached per resolved render step;
the web host answers it at `POST /api/get_master_light_preview`, the mirror
`api.invoke` uses on both targets, AND at
`GET /api/stacking/master-preview?runId=…` for direct browser access when no
API key is configured), `list_stacking_presets`, `save_stacking_preset`,
`delete_stacking_preset` (M4d Task 4, ruling R-M4d-6 — the user's OWN
presets, ONE settings row `stacking.presets` holding a JSON array of
`{ name, config }`: max 50, names 1–60 chars trimmed and unique
case-insensitively (an upsert keeps the NEW spelling), `config.paths`
stripped on save so a preset never carries folders, and all three return the
full list sorted by name case-insensitively. The row is decoded ENTRY BY
ENTRY, so one undecodable entry costs only itself — dropped with one `warn!`
carrying its `count`, and the next write rewrites the row without it; only a
document that is not a JSON array at all reads as empty on `list` and is
refused with a `Conflict` naming the key on either write, since that is the
one case where overwriting destroys something unknowable. Both writes are
ONE `BEGIN IMMEDIATE` read-modify-write — the whole list is a single
settings value, so two interleaved saves would otherwise drop a sibling
preset. In the tab, a run disables APPLY only: the menu still opens and
Save-as / delete stay live, because a run is exactly when a user wants to
save the settings they just launched with).

**The tab** (`src/components/stacking/`, mounted from `FrameSetDetail.tsx` as
the **Stacking** tab): `StackingTab` (toolbar, run/cancel) →
`PipelineBoard`/`StageRow` (the board is TEN rows, `0 · Masters` … `9 ·
Output` — nine backend stages plus the display-only Debayer row;
`stageSummary.ts` — a pure function shared with `StageInspector`, never reads
run state) + `GroupsTable`; `StageInspector` + one config panel per stage
(`panels/{Masters,Calibrate,Debayer,Measure,Reference,Register,Normalize,
Integrate,Drizzle,Output}Panel.tsx`) for configuration; `FramesTable` (manual
exclusion is the ONE frame-level write from the tab — everything else is
read-only run output) and `ResultsPanel` + `ProvenanceModal`. **No
`StackingQueueIndicator`**: the sidebar's existing `ComputeQueueIndicator`
already lists every queue entry including a running stack, with cancel — a
second widget for the same job would duplicate it (plan 5b ruling 2). The
tab is enabled for every build since the 2026-09-10 acceptance run
(`docs/superpowers/research/2026-09-09-m1-acceptance-run.md`), gated only on
the set having light frames.

**Settings → Stacking** (`src/components/settings/StackingSection.tsx`):
global config defaults (`get/set/reset_stacking_defaults`, the same
`StackingConfig` tree a set can override) and the working/output folders
(`get/set_stacking_paths` — `stacking::paths`, on-disk layout
`<working_dir>/<set_slug>/{calibrated,measured,registered,ln,runs,rej,previews}/…` —
`previews/run-<id>/<group>_<kind>_<step>.jpg` is the M4d master-light
thumbnail cache (`step` = the RESOLVED render step, `thumbnail | preview |
full`, never the caller's raw `maxPx`), swept only by `CleanupWhat::All`;
the web folder
picker's `browse_directories` scope `"stacking"` resolves against the same
roots as `"scan"`, plan 5b ruling 6). Per-set override lives in
`stacking_set_config`; precedence is WHOLE-CONFIG (spec §9.2, `resolve_config`)
— a stored per-set document, when present, IS the run's config with no
field-level merge against the global default; only with no per-set override
does the global default JSON apply the same way, and with neither, the
built-in default. M4a added `measurement.detectionSigma` and
`reference.twoPass` to `StackingConfig` as `#[serde(default)]` fields with
no `STACKING_CONFIG_VERSION` bump (both decode from every stored
document); both fold into the per-stage `config_hash` above, so the
Measure stage's cache invalidated for every set on the first M4a run
(R-M4a-9).

**Beyond M1** (spec §14): **M2** — local normalization (MMT background
models, PSF-flux scale with RCR, `.athln` sidecars, `NormalizePanel`'s LN
block goes live) — SHIPPED, see below. **M3** — drizzle (exact clipping,
forward mapping, M1's rejection bitmaps turned on, `DrizzlePanel` live) —
SHIPPED (Tasks 1-6, see below, plus a whole-branch final fix wave closing
three review findings before merge: a group whose members' native geometry
differs from the run's reference was refused outright rather than
drizzled, the Output stage's own timing double-counted drizzle's whole
duration, and a `.rej` write fault mid-integration failed the group
instead of degrading to "drizzle skipped"); acceptance run 2026-09-10 on
LDN 1272 — `docs/superpowers/research/2026-09-10-m3-acceptance-run.md`:
drizzle 2× on both groups, the mono drizzled/undrizzled FWHM ratio equal to
the external reference's to 0.05 %, the OSC G/B ratio ≈ 10 % broader (an M4
item), and the OSC master matching the external one in level and background
shape after Task 8's sky-penalized normalization anchor (ruling R-M3-17).
**M4** — polish:
thin-plate-spline distortion, ESD/RCR/min-max/large-scale rejection,
Bayer drizzle, XISF output, cataloging masters, preset management, and
**mixed pixel scales in one set** (owner requirement
2026-09-09) — that last item SHIPPED as M4b, see its own paragraph below:
the plan-time scale WARNING, the per-frame scale gate, the WCS seed and the
co-registered / native modes all landed together, so the M2-era correction
that used to stand here ("the only defence is registration's fixed
`[0.8, 1.25]` gate, no plan-time signal names the group") no longer
describes the code. Bayer drizzle, XISF output, cataloging masters and
preset management SHIPPED as M4d — Tasks 1–4 code-complete, see its own
paragraph below; accepted 2026-09-14 on LDN 1272 (runs 34–36,
`docs/superpowers/research/2026-09-14-m4d-acceptance-run.md`).

**M2 — local normalization** (spec §5.2, executed 2026-09-10 alongside Task
10's camera-agnostic grouping rule — see "The plan gate" above, same
cycle): stage 6 (`Stage::Normalize`) stops being a no-op the moment
`normalization.local.enabled` is on or `normalization.rejection == "local"`.
Per group it ranks included members by weight, integrates the best
`referenceFrames` of them
(default 20) into an in-RAM LN reference (linear-fit rejection, plain
global normalization — never the group's own configured normalization),
models that reference's background on the `scale/8` node mesh (default
scale 1024 → stride 128), then fans out per included member: warp into the
reference geometry, model the target's own background the same way (a
looser deviation threshold), take the PSF-flux relative scale against the
reference by RCR over matched-star fits, and write `A = s`,
`B = B_ref − s·B_tgt` on the stride grid as a `.athln` sidecar
(`stacking::ln`). Sidecars are cached as `stacking_artifacts` rows exactly
like every other per-frame stage — `kind = "ln"` per frame, `kind =
"ln_reference"` per group (`frame_id` `NULL`) — keyed by a config hash that
folds in the reference member list and the reference's own hash, so
re-registering ONE reference member correctly invalidates every other
member's sidecar too, not just its own.

`stacking::run` reads the cached sidecars back into `GroupInput.ln`
(indexed like the group's own frame list, `None` for a frame with no
grid) and forwards it to `integrate_planes`, which wraps each channel's
`LnGrid` as an opaque row-evaluator factory so `integration/engine.rs`
never depends on the `stacking` tree directly (`StackParams.local`, a
factory called once per rayon worker — never shared behind a lock). The
engine's band loop applies `v' = A·v + B` (the SAME bicubic B-spline
reconstruction of the coarse grid the acceptance probe below uses) in
place of a frame's global `(offset, scale)` pair for OUTPUT normalization
when `normalization.local.enabled`, and for REJECTION normalization when
`normalization.rejection == "local"` — independently gated, so a group can
use one without the other; a frame with no grid always falls back to its
own global pair, keeping the M1 byte-identical pins intact when `local` is
off entirely. A frame whose own relative scale can't be measured (fewer
than 20 matched stars) or whose sidecar can't be read back (corruption, a
stale cache hit) is excluded from the group with a reason, never silently
degraded, when LN drives OUTPUT normalization; it keeps global
normalization with a warning when LN drives rejection only.

`normalization.local` config (spec §9.2): `enabled`, `scale` (the tile
size in px — 256–4096, step 256 in the UI), `referenceFrames` (3–50),
`psfModel`, `localScale` (still disabled — a per-cell local scale spline
is M4). On-disk layout: `<working_dir>/<set_slug>/ln/<group>/reference.fits`
+ `ln/<group>/<calibrated-stem>.athln`.

**LN runs end to end through `start_stacking`.** The two M1-era guards that
used to block it — `build_plan`'s Gate 6 (`plan.rs`, code `"unsupported"`)
refusing any plan with `normalization.local.enabled = true`, and
`integrate_group` (`stacking/integrate.rs`) refusing
`normalization.rejection == "local"` with a `BadInput` before it ever
reached the engine — were both lifted in the same cycle; nothing routes
around `start_stacking` to exercise LN any more. **Acceptance run
2026-09-10** (`docs/superpowers/research/2026-09-10-m2-acceptance-run.md`):
LN end to end on the real LDN 1272 catalog (368 frames across a mono and an
OSC group), reference build + per-frame fan-out both verified against the
external baseline (master noise 0.89–1.13× it, no mesh imprint at the grid
stride); the residual rejected-fraction gap versus that baseline is
attributed to the M4 robust-line-fit-dispersion calibration item (spec §14
M4), not a defect in LN itself — see `docs/superpowers/open-items.md`'s
Stacking M2 subsection for the full attribution and the owner smokes still
owed.

**M3 — drizzle** (spec §7, executed 2026-09-10): stage 7 (`Integrate`) grows
an optional sink — when `drizzle.enabled && drizzle.useRejection`, every
band's per-frame rejected bits are written to `rej/run-<id>/<group>/
<stem>.rej` (`stacking::rej`, one bit per pixel per channel), sized and
ordered to the SAME included-frame set `integrate_group` computes via the
extracted `stacking::integrate::included_after_min_weight` rule — the run
(`stacking::run`) creates the set before calling `integrate_group`, never
duplicating the rule. Stage 8 (`Drizzle`) then runs per group right after
the master is written, in the same `process_group_output` call: per plane
it deposits every included frame's calibrated pixel onto a 1×/2×/3× output
grid through the frame's `PixelMap`, with the run's weights and (when on)
local normalization, skipping rejected pixels per the `.rej` bitmap.
`I / W` where `W > 0` is level-preserving — a uniform field comes out at
the input level for every scale/dropShrink (ruling R-M3-2, spec's
Implementation notes). Output `<master stem>_drizzle<s>x.fits`
(+ `..._weight.fits` when `writeWeightMap`) with the reference's WCS
scaled and the `ATH_DRZ`/`ATH_DRZP`/`ATH_DRZK` cards
(`stacking::master_cards`, `fits_writer::wcs::scale_plate_solve`). A
drizzle failure (`Memory`/`Io`/`BadInput`, or any error past
`drizzle_group` itself) NEVER fails the group — the master is already
written and good, so the group stays `done` with `drizzle_path` `NULL`, a
`warn!` and a run warning; only `Cancelled` propagates, as the run's own
cancel. The `.rej` bitmaps are per-run temporaries: removed at the run's
single exit path (`run_thread`, every outcome — success, cancel, failure,
panic-recovery) unless `output.cleanup = keepAll`. `rerunFrom: "drizzle"`
is clamped to `"integrate"` (`api::stacking::start_stacking`, ruling
R-M3-9) — drizzle has no cache of its own. The plan gate's old "Drizzle
arrives in M3" blocker is gone; the only thing gate 6 still blocks on is
an out-of-range `scale` (`∉ {1, 2, 3}`, ruling R-M3-10); the byte-footprint
estimate grows by the `.rej` bitmap and drizzled-output terms when drizzle
is on. `MaximumQuality` now turns on drizzle 2× AND local normalization
(spec §9.2) — both hidden in M1/M2 only because neither stage existed yet.
Full ruling list: spec §7's "Implementation notes (M3)". `DrizzlePanel`,
the tab's drizzle rows/summary and the board/`ResultsPanel` polish shipped
in Task 6. A whole-branch review before Task 7's acceptance run found the driver
conflated a frame's own SOURCE geometry with the run's REFERENCE
geometry — `drizzle_group` refused any frame whose native size differed
from the reference outright, which the project's own acceptance set (an
OSC group natively 6248×4176 registered onto a 6224×4168 reference) would
have tripped on the first run — fixed by splitting `FrameDepositCtx` into
`src_width`/`src_height` (source-plane indexing and the band's source
window) and `ref_width`/`ref_height` (the `.rej` bitmap lookup and the LN
grid index, both always reference-geometry); the up-front check is now
`channels` only. The same review closed two more: the `Output` stage
timer used to include the whole drizzle duration (Drizzle and Output are
meant to be disjoint spans), and a `.rej` write fault mid-integration
(`ENOSPC`/`EACCES`/an SMB hiccup) used to fail the group outright instead
of degrading to "drizzle skipped, master kept" the way a bitmap-set
`create` failure already did — `RejBitmapSet` now latches its first write
failure and every later `record_band` call for that set becomes a no-op.
Acceptance (2026-09-10, `docs/superpowers/research/2026-09-10-m3-acceptance-run.md`):
drizzle 2× on both LDN 1272 groups is level-preserving (0.9987–0.99999 of
the master), seam-free at the 512-row band period, fully covered, with sane
weight maps; the mono drizzled/undrizzled FWHM ratio matches the external
reference's (0.925 vs 0.926, both through our estimator on FITS
conversions of the raw attachments — `measure_probe`'s XISF branch is not
trustworthy, an M4 item); the OSC G/B ratio is ≈ 10 % broader than the
external one for a reason that is neither rejection strength, level
conventions nor registration distortion (M4 item, together with the OSC
PSF-weight sky-penalty audit). Drizzle time on this 16 GB Mac: mono 4.2 min,
OSC 13.5 min (three planes). Plan:
`docs/superpowers/plans/2026-09-10-stacking-m3-plan-drizzle.md`.

**M4a — quality** (`docs/superpowers/plans/2026-09-10-stacking-m4a-plan-quality.md`,
closes the quality items the M2/M3 acceptance runs left open): measurement
(stage 3) seeds are now noise-relative instead of rank-budgeted. The root
cause of the OSC bright-sky/dark-sky weight inversion was
`ImageAnalyzer::detect_fast_data`'s `star_levels` picking its two
detection levels by a fixed bright-pixel HISTOGRAM RANK budget
(`6·maxStars`/`24·maxStars`), which a sharp night fills for free and a
bright sky pays nothing extra for (ruling R-M4a-1); rustafits'
`DetectionLevels::{RankBudget, NoiseRelative, Absolute}` hook
(`with_detection_levels`) now lets the pipeline pass `NoiseRelative { k1:
σ, k2: σ/2 }`, keyed by `measurement.detectionSigma` (default 20, clamped
to `[1, 100]` in `resolve_config`), and stops the detector's ladder there.
The PSF fit grows the external tool's adaptive sampling region (start
`max(nominal/2, 3)`, grow while the median drops ≥ 1 %, cap
`min(2·nominal, 48)`) and inner-region acceptance (`inner_margin` 0.15);
`psf_signal::PSF_FIT_VERSION` (= 2) folds into both the measurement and
the LN artifact hashes (R-M4a-15), so a fitter change recomputes cached
metrics AND `.athln` sidecars together. `measurement.seedPrefilter`
(`none` default | `median3`, a 3×3 median on the DETECTION copy only,
thresholds from the unfiltered noise) shipped as an option after two
calibration rounds showed it depletes the star population (R-M4a-13/14);
the residual OSC sharp-night excess is M4c Task 0 (a structure-map
detector, R-M4c-11). Calibration on the external tool's 368 calibrated
LDN 1272 frames (`examples/weight_audit.rs` +
athenaeum-lab `harness/weight_audit_compare.py`): mono
per-night fit ratios 1.01/1.05/0.82, PSFSW Spearman 0.92, top-20 18/20;
OSC 0.91/0.80/0.68, top-20 14/20 (baseline: mono 1.11–1.21 / 0.924 / 18;
OSC 1.6–7.6× / 0.93/0.86/0.44 / 12); 10 PASS / 12 MISS of the R-M4a-2
targets vs 7/15 before. Rejection (stage 7) `LinearFitClip` now fits the
sorted stack against rank with the minimum-absolute-deviation line
(`integration/combine.rs::medfit_line`: intercept = median of `y − b·x`,
slope bracketed and bisected on the sign of `Σ x·sgn(residual)`,
warm-started from the previous iteration, exact-root early return,
`select_nth` median) instead of the least-squares one; dispersion `s =
LINEAR_FIT_SIGMA_SCALE · 2 · adev` with `LINEAR_FIT_SIGMA_SCALE = 1.0`
— calibrated by the acceptance run (2.985 / 2.733 % rejected at the Auto
5.0/3.5, inside the 2.3–3.3 % target; M2 measured 0.83/0.74 % with the
least-squares line), so it stays 1.0; cost ≈
8.5× the old line at n = 200 end to end (≈ 16 µs per pixel stack, ≈ 40 s
per 26 Mpx plane on this Mac), accepted by R-M4a-17. Reference resolution
now includes the two-pass dry-run pick described under the `Reference`
stage above (`reference.twoPass`, default true, rulings R-M4a-5/6/18).
The XISF reader (rustafits `formats/xisf.rs`) now picks the largest
`<Image>` (ties keep the first — the external tool's masters carry a
same-size weight-map image after the data) and honours `byteOrder="big"`
and `bounds="lo:hi"`; the u16-domain float convention (samples × 65535)
stays a cross-crate contract (`integration/banded.rs::spill_via_read_raw`,
`analysis/analyzer.rs`, R-M4a-11) — the M3 "XISF branch untrustworthy"
finding was the two probes' own `Float32` arm never dividing by 65535,
fixed in `examples/measure_probe.rs` and `examples/weight_audit.rs`. Two
measured LN hot spots (Task 5) came out without changing any output
number: `LnScratch::for_grid` now precomputes one `wx_table` of 4-tap
B-spline weights per `stride` value once per grid, and
`grid.rs::evaluate_row_into` indexes it instead of recomputing
`BicubicBSpline::weights()` for every pixel — a call-count reduction from
`ref_height·ref_width` to `stride` per plane, amortized across every row
and every channel sharing one grid; and `LnReferenceForDetection::build`
(`ln/mod.rs`) borrows an all-finite reference plane as `Cow::Borrowed`
instead of always cloning it, paying the sanitizing copy only when a
plane genuinely carries a non-finite pixel. Ruling R-M4a-19 accepted the
table's `fx = r/stride` differing from the old per-pixel `fx = tx −
tx.floor()` by up to 8.1e-5 at non-power-of-two strides (evaluated row
values drifting up to 6.3e-6) — the table's formula is the MORE accurate
of the two (the old one's f32 rounding error grows with `x`), pinned at a
widened 1e-4 tolerance with the reasoning attached rather than silently
loosened; the power-of-two case (the default scale 1024 → stride 128) is
bit-identical and its pin tightened to 1e-9. The run thread de-registers
its cancel handle through an RAII guard as the LAST thing it does (Task 4
fix round — the old early removal raced tests waiting on
`stacking-complete`/`rej/` cleanup). Rulings R-M4a-1…R-M4a-19 live in the
plan's header (`docs/superpowers/plans/2026-09-10-stacking-m4a-plan-quality.md`);
cite it. **Acceptance run 2026-09-11** (`docs/superpowers/research/2026-09-11-m4a-acceptance-run.md`, run 13 on LDN 1272, 56 min end to end): the two-pass pick switched the reference from the best-weighted `_0080` (36 px off the median framing) to `_0073` — the frame the owner had pinned by hand in M2/M3; rejected fractions 2.985 % (mono) / 2.733 % (OSC) at the Auto 5.0/3.5 with `LINEAR_FIT_SIGMA_SCALE` left at 1.0 (M2: 0.83/0.74 %; the external tool 2.5–2.8 %); per-frame weights against the external log: mono ρ 0.92 / top-20 18/20, OSC ρ 0.93/0.82/0.68 / top-20 15/20 with the bright night no longer monopolising the top; the mono drizzled/undrizzled FWHM ratio equals the external tool's to 0.6 % under the new estimator, the OSC G/B ratio stays +11–12 % over it (the M3 residual, unchanged in kind — M4c Task 0); LN 368/368, measure −20 %, LN −7 %, drizzle −16 % vs M3.

**M4b — mixed pixel scales** (spec §3.8, plan
`docs/superpowers/plans/2026-09-10-stacking-m4b-plan-mixed-pixel-scales.md`,
rulings R-M4b-1…9): one set may hold groups — or members of one group —
shot at different pixel scales (a bin-2 group, a second telescope, another
camera), and the pipeline integrates all of them in one of two modes the
owner picks per set through `registration.geometry`
(`"coRegistered" | "native"`, default co-registered, `#[serde(default)]`,
no `STACKING_CONFIG_VERSION` bump). **Co-registered** is M1–M4a's
behaviour: ONE run-wide reference, every group resampled into its geometry.
**Native** gives each group its own reference — the group's best-weighted
member, two-pass re-picked per group (a Manual pin applies to its OWN group
only) — and its own geometry for local normalization, integration,
drizzle, the master's WCS and the `.rej` bitmaps, with no cross-group
registration at all. `RunContext.group_geometry` (`GroupGeometry`, resolved
at the end of stage 4 by `resolve_group_geometry`) is the one place that
knows; every former reader of `rc.reference_width/height` now reads
`rc.geometry_of(&group.key)`, which in co-registered mode holds the
run-wide value for every group, so the M1–M4a pins keep passing with the
default config. The run-level `stacking_runs.reference_frame_id` stays the
largest group's reference in `Auto` mode and IS the pin in `Manual` mode
(ruling R-T3-2 — what the plan gate and the results header show);
`SummaryGroup.reference_frame_id` carries each group's own, and is `Some`
ONLY for a group whose master was actually WRITTEN — `None` for a group
skipped, failed, or dropped below the member floor at any stage (ruling
R-T3-3); every master and drizzled master (and a drizzle weight map)
carries `ATH_RGEO = 'coRegistered' | 'native'`, and in native mode the
master's WCS is the GROUP reference's solve. `registration.geometry` rides
`registration_subtree`, so flipping it re-registers every set on purpose.
Two supporting mechanisms ship in the same plan: every `GroupFrame` carries
`pixel_scale_arcsec`/`scale_source` (the stored plate solve when the frame
is solved, else `206.2648 · XPIXSZ / FOCALLEN` — no binning factor,
R-M4b-1) and the plan gate turns a scale spread into a named WARNING,
never a blocker (R-M4b-7); and registration's scale gate is per frame,
centred on the frame's own implied ratio to its reference (`[r/1.25,
r·1.25]`, R-M4b-2), with the alignment SEEDED from the two WCS solutions
when both frames are solved (`register/wcs_seed.rs`, the `+wcs` model
suffix on the row, R-M4b-3) and the quad seed whenever a solve is missing.
The plan gate's per-group staleness in native mode follows each group's own
reference as the last run recorded it in its `summary_json`
(`plan.rs::summary_group_references`). The plan-time warning surfaces in the
tab as `GroupsTable.tsx`'s `Scale` column: the group's own measured or
header-implied scale (a `~` prefix marks a header-only member) plus a `×r`
ratio badge whenever the group sits outside `[0.8, 1.25]` of the resolved
reference (ruling R-M4b-7, `text-warning`, mirroring the backend's
`SCALE_TOLERANCE` client-side); in `Auto` reference mode the plan resolves
that reference scale from the LAST run's own recorded reference frame
when one exists (`list_runs(_, _, 1)`, status-unfiltered), else the median
scale of the largest group by INCLUDED frame count
(ruling R-T1-1) — the same fallback order `compute_register_stale` already
uses for staleness. The WCS-seed trigger (R-M4b-3) is ratio-based, not
window-based: `wcs_seed::WCS_SEED_RATIO_EPS = 0.05` (rulings
R-T2-1/R-T6-4) — on real catalog data one rig's own solve-to-solve scale
jitter reaches 0.8–1.6 %, so a same-rig frame's implied ratio never crosses
the 5 % floor and takes the quad seed FIRST, while a genuine 5–25 %
optical step (a different focal length or binning) leads with the WCS
hint; a leading hint that confirms fewer than `MIN_INLIERS` pairs at its
own `WCS_SEED_RADIUS_PX` (≥ 8 px) is discarded with a warning and falls
back to the quad seed. The trigger decides only the ORDER — below the
floor the hint is still built whenever both frames are solved, and serves
as the quad seed's own fallback (R-T6-9, below); neither seed ever
bypasses the star-based confirmation.
The frames table (`FramesTable.tsx`) renders the
`+wcs` suffix on a row's `regModel` as a small muted `WCS` chip (title
"seeded from the plate solves") next to the plain model text, rather than
as part of the string. In native mode a Manual reference pin IS the
run-level `stacking_runs.reference_frame_id` (ruling R-T3-2); every other
group still auto-picks and two-pass-refines its own best-weighted member
independently of the pin. **Acceptance run 2026-09-11** (`docs/superpowers/research/2026-09-11-m4b-acceptance-run.md`, three real mixed-scale sets of the owner's catalog at 30 best frames per group, both modes, 7 runs): the cross-scale groups registered through the plate-solve seed at the expected scale — ×0.207 / ×1.457 (ASI6200MM and ASI294MM-bin2 onto an OSC reference, rms ≤ 0.61 px), ×0.502 (bin 1 onto bin 2, same-star centroids 0.15 px between the masters), ×1.285 (two focal lengths inside one narrowband group) — and native mode gave each group its own reference, geometry and WCS; the reference's own master is bit-identical between the modes on every set. Two rulings came out of it: `WCS_SEED_RATIO_EPS = 0.05` (R-T6-4, from the measured within-rig solve jitter of up to 1.6 %) and the plate-solve seed as the FALLBACK after a quad-seed failure (R-T6-9 — an H-alpha field against an O-filter reference of the same rig lost 14 of 30 frames to quads and 0 with the fallback); and the plan gate now evaluates masters/links readiness and its scale statistics over the frames that will actually run, after manual exclusions (R-T6-6/7).

**M4c — algorithms** (spec §3.3, §5.1, §5.2, §6.2, §6.3, plan
`docs/superpowers/plans/2026-09-10-stacking-m4c-plan-algorithms.md`,
rulings R-M4c-1…11 plus the fix-round rulings R-T0-1/2, R-T2-1, R-T3-1,
R-T4-1…5 and R-T5-1/2): the algorithm half of §14's M4 — six items, every
one an added arm on an existing enum or an optional pass inside a stage
that already runs, so no table changed and no command was added.

**The structure-map seed detector** (`stacking/structure.rs`, ruling
R-M4c-11) lets stage 3 seed its PSF fits from the math reference's §5.1
structure map instead of the peak threshold —
`measurement.seedDetector = "peak" | "structure"`, **default `peak`**. The
map is an optional 3×3 median on its own copy → a high-pass subtracting a
separable Gaussian of size `1 + 2^structure_layers` (5 → 33 px) truncated
at 0 → 3×3 dilation → adaptive binarization at `median(dilated) +
3·σ_noise` → 3×3 erosion → two-pass union-find labelling → the reference's
per-candidate rules in its own order (border, size floor, ring background,
significant pixels and their maxima, barycentre, `upper_limit`, coverage,
detection SNR, kurtosis). Two deviations are measured rather than assumed:
the binarization LEVEL comes from the dilated map but the SCALE from the
caller's UNFILTERED plane (anchoring both on the map costs most of the
sharpness behaviour the detector exists for — at the shipped sensitivity an
undersampled fixture field yields 0.56× the well-sampled field's star count
with the plane anchor and 0.91× with the map anchor), and `σ_noise` is the
module's own second-à-trous-layer K-sigma estimator ÷ `B3_LAYER2_GAIN =
0.2007` rather than rustafits' `noise_mrs` — ruling R-T0-2 ran both on the
same unfiltered plane over 176 real planes: they agree to 11 % on mono but
diverge 1.67 / 2.20 / 2.04× on the debayered OSC R/G/B, 10.8 % of planes
inside the ruling's 10 % bar against its 90 % re-use rule, so the private
estimator stays. `min_structure_size = 0` is an AUTOMATIC floor (the
detected sizes clustered by their own increasing-gap statistic, the first
cluster dropped when it is a minority beside the second) — documented on
the field, in the panel help and the spec, unit-pinned, and re-measured
with the floor forced off: at the shipped `DEFAULT_SENSITIVITY = 0.7` it
evaluates to 3 px on both fixture fields and removes NOTHING (ruling
R-T0-1); it bites only at the reference's own 0.5. `peak` stays the default
because R-M4c-11's bar is ALL 22 of the R-M4a-2 targets and the full
368-frame run passes 12 where the shipped peak threshold passes 10: mono
improves across the board (PSFSW ρ 0.918 → 0.977, fit-count ρ 0.94 → 0.96)
but the OSC bright night still yields 2.46–3.86× the external tool's fits
and OSC blue REGRESSES (PSFSW ρ 0.683 → 0.423, top-20 14/20 → 11/20). That
run is also the OSC residual's **second signature**: two unrelated noise
estimators disagree by 1.6–2.2× on exactly the debayered planes where both
detectors overshoot, so the next investigation belongs on the VNG planes or
the PSF fitter's acceptance, not on a third detector.

*Superseded for the default by R-OSP-2 (2026-10):* `measurement.seedSource =
matchedFilter` is the default (the star-measurement module's two-pass matched
filter at `measurement.seedThreshold`, default `seeds::DEFAULT_THRESHOLD`,
clamped to [2, 20]); `peak` (reads `detectionSigma` and `seedPrefilter`) and
`structure` remain explicit choices with their own dials; a stored document
without `seedSource` decodes per spec §2.2 (`legacy_seed_source`: structure
stays structure, a `median3` prefilter or a `detectionSigma` other than 20
stays peak, everything else becomes the matched filter; a stored value is
never reinterpreted in other units). `seedDetector` stays on the wire for
round-trips and mirrors the source. A plane with no noise estimate (a
noiseless synthetic) seeds nothing under the matched filter, by design.

**Three more rejection algorithms** (ruling R-M4c-1): `Rejection::{MinMax,
Esd, Rcr}` appended to the engine enum (`integration/combine.rs`) and
`RejectionChoice::{minMax, esd, rcr}` to the config
(`stacking/integrate.rs`), defaults `MinMax { low: 1, high: 1 }` / `Esd {
outliersFraction: 0.3, alpha: 0.05, lowRelaxation: 1.5 }` / `Rcr { limit:
0.5 }`. **The Auto ladder is byte-for-byte unchanged** (`n < 8` percentile
0.2/0.1, `8 ≤ n < 20` Winsorized 4.0/3.0, `n ≥ 20` linear fit 5.0/3.5) and
never resolves to any of the three — they are user choices only, so no
existing group's rejection moved because they exist.
`integration/student_t.rs` (new leaf) carries the numerics with no new
dependency:
`erfc`/`erfinv` (bit-identical copies of `stacking::robust`'s, which is
unreachable from `integration/`), `ln_gamma`, the regularized incomplete
beta by Lentz's continued fraction, `t_quantile` by bisection, and
`esd_critical(n, i, alpha)` = ESD's `λ_i`; `with_esd_lambdas` memoizes the
`λ` vector per `(n, alpha)` in a thread-local map (ruling R-M4c-2) so the
incomplete beta is evaluated at most `k` times per distinct stack size
instead of 26 M times per plane. Both new routines are **allocation-free
per pixel**: ESD and RCR only ever remove one of the two ENDS of the sorted
stack, so the survivors are a contiguous range finished by one
`copy_within`, and RCR's deviation scratch and half-normal abscissae table
are thread-local. Measured: `esd_critical(50, 0, 0.05) = 3.128247` against
Rosner's published 3.128 (and `λ` SHRINKS with `i`); ESD rejects exactly
the four planted +6σ samples of a 60-sample Gaussian and keeps all 60 of a
clean one; RCR rejects the same four plus one genuine tail sample and keeps
57 of a clean 60. `stacking::robust`'s own RCR is cross-checked
bit-for-bit against the integration copy over 50 contaminated samples.
Throughput on a real plane is an ESTIMATE, never measured (order 50 s per
26 Mpx plane for RCR) — Task 7's job.

**Winsorized sigma clipping now runs the reference loop** (ruling
R-M4c-3): `winsorized_location_scale` in `integration/combine.rs` replaces
the retired fixed point — `μ = median`, `σ = WINSORIZE_MAD_TO_SIGMA
(1.4826) · MAD`, then `t = μ ± WINSORIZE_CLAMP_SIGMA (1.5) · σ` with a
first-pass `WINSORIZE_CUTOFF_SIGMA (5.0)` mapping a gross outlier to the
CENTRE rather than to the bound, `σ = WINSORIZE_SCALE_CORRECTION (1.134) ·
stddev`, `μ = mean`, stop at `|Δσ| < WINSORIZE_CONVERGENCE (0.0005) · σ`
after ≥ 2 passes, cap `WINSORIZE_MAX_PASSES = 20`. Two documented
deviations: the MAD seed instead of `1.1926·Sn` (O(n²) per pixel stack),
and — ruling R-T2-1 — a **stddev-about-the-median fallback when `MAD ==
0`**, because a majority-tied stack (every integer-ADU bias master) seeded
`σ = 0`, i.e. "nothing to reject", and a cosmic ray survived: `15 × 500 +
1 × 5000` went from 0 rejected / combined 781.25 to 1 rejected / 500.0. A
stack with a non-zero MAD cannot reach that branch, so no non-degenerate
output moved. **No fixture fingerprint pin moved** — on the master-build
fixtures both fixed points reject the same single outlier, a coincidence of
the fixtures rather than agreement between the estimators (the surviving
legacy pin now asserts they disagree) — so the real move was measured on
real data instead: 21 calibrated LDN 1272 mono frames through
`integrate_probe --rejection winsorized`, rejected fraction 0.380 % →
0.963 % (×2.54), master median −0.024 %, MAD +0.50 %, noise +2.0 %, PSF SNR
−3.9 %, 6.59 % of 25.9 M pixels changed, combine time +20 %. **Every master
built with Winsorized now differs from its pre-M4c self** (the Auto
ladder's `8 ≤ n < 20`, plus master builds at n ≥ 15) — a `rebuild_master`
produces different pixels than the original build; provenance shape and the
`ATH_REJ` text are unchanged, so nothing migrates.

**Large-scale rejection** (ruling R-M4c-4) is an optional SECOND
integration pass that removes what the per-pixel algorithms leave as
speckle — a satellite trail's faint shoulders. `integration/source.rs::
RejectionBitSource` is the read-side mirror of M3's `RejectionBitSink`,
ROW-based and plane-bound (`words_per_row`/`frames`/`forced_row`) because
`StackParams` is per-plane and a `&dyn` call per SAMPLE would be ≈ 5×10⁹
virtual calls per plane; `StackParams.forced_rejection` makes a forced
sample `present` (the side attribution still judges it against the
survivors' median) but never lets it enter `work`, counts it in
`rejected_per_frame`, the low/high maps and the bitmaps, and NOT in
`base.rejected_fraction` (the algorithm-only convention range rejection
already had). `integration.largeScale { enabled: false, protectedLayers: 2,
growth: 2 }` (clamped 1–6 / 0–4 in `resolve_config`) turns it on: pass 1
writes the `.rej` bitmaps, `stacking::rej::process_large_scale` filters each
frame's bitmap into a `.rejl` sibling, pass 2 re-integrates with those bits
forced. The filter is an **MMT-shaped median cascade** — majority medians
of windows 3, 5, …, `2^layers + 1` — not the single wide median the ruling
first named: no majority median of window 9 can keep a 3-px band (27 of 81
set is a minority), so the single-median reading would erase exactly the
thin trails the stage exists for. `protectedLayers` is therefore a SCALE
SELECTOR, not a strength knob — a band needs 3 px at 2, 5 px at 3, 9 px at
4, and a compact blob survives only while it is larger than the widest
window; `growth` dilates with a disc, not a square. Ruling R-T3-1: the
second pass runs WITH a sink of its OWN into a fresh set at
`rej/run-<id>/<group>/pass2/<stem>.rej` (`SECOND_PASS_DIR = "pass2"`), and
drizzle reads THAT set when `GroupOutput.second_pass_rej_ok` — forced
structures ∪ pass 2's own algorithmic and range rejections, i.e. exactly
the set the master was built from; `.rejl` stays the intermediate. A second
pass that ran WITHOUT a usable set of its own skips drizzle for the group
rather than being handed pass 1's bits, which describe the integration pass
2 replaced (the master is already written and untouched).
`GroupStats.large_scale_rejected_fraction` reports the forced fraction, the
plan-time footprint counts THREE bitmap copies per frame when large-scale
is on, and everything under `rej/run-<id>` rides the run's single-exit
cleanup. **No preset enables it** — turning it on in `MaximumQuality` would
double every such run's integration time, which the ruling does not ask
for. Run pin: 6 frames, a flat-topped trail whose 2-px shoulders survive
the per-pixel clip — the shoulder band reads +11.70 % of a control row
without large-scale and +0.40 % with it.

**Thin-plate-spline distortion** (rulings R-M4c-5/6/7): `geometry/tps.rs`
(ungated, no `stacking` dependency) holds `ThinPlateSpline` — the classic
`φ(r) = r² ln r` with an affine part, two scalar splines (x and y) sharing
nodes, Bookstein's bordered system with both right-hand sides on ONE
factorization, coordinates normalized to the node cloud's bounding-box
diagonal (`center`/`scale` stored so `displacement` stays a self-contained
function of pixel coordinates) — plus `TPS_MAX_NODES = 600`, `TPS_GRID_PX =
8`, `TPS_MIN_NODES = 4` and `select_nodes`, which grid-stratifies over a
30×20 cell grid (best combined-σ pair per cell, then round-robin), never
the first N. The solver is **dense Gaussian elimination with partial
pivoting, not a Cholesky** (ruling R-T4-2): `φ(0) = 0` makes `trace(K) = 0`
exactly, so the plan's suggested `1e-9·trace/n` ridge is literally zero, and
no diagonal ridge can make a merely conditionally-positive-definite kernel
Cholesky-able. `PixelMap.distortion` became `Option<DistortionModel>` —
`Polynomial(Distortion)` | `Tps { forward, inverse, domain, grids }` — with
explicit `kind` tagging and a hand-written `Deserialize` that reads a
missing tag as polynomial, so every M1–M4b `transform_json` decodes
unchanged and the polynomial arm is byte-identical. `DistortionChoice::Tps`
is a user choice; `Auto` never picks it. Ruling R-T4-3 draws the line
between the two evaluation paths: `forward_exact`/`inverse_exact`
(`O(nodes)`) serve every NON-pixel caller — registration QA, the local
loop's re-pairing, `weights::reference_coverage`,
`drizzle::band_source_window`, the probes — while `TpsGrids { forward:
RwLock<Option<Arc<TpsGrid>>>, inverse: … }` lives behind an `Arc` so every
clone of a map shares ONE allocation and each direction is built only when
that direction's pixel path first asks for it. Both slots are `RwLock`s
rather than `OnceLock`s because ruling R-T4-6 made a built grid
RELEASABLE: `PixelMap::release_grids` (and its scope guard
`release_grids_on_drop` → `GridRelease`) empties the slots and reports the
bytes handed back, and every stage calls it when its per-frame work ends —
the registration writer after it writes, `RegisteredSource`'s `Drop` for
integration and LN, drizzle after each frame's deposit. The `Arc` inside is
what keeps the per-pixel path lock-free: a pixel loop takes ONE handle per
band (`forward_eval`/`inverse_eval`/`inverse_burst`), so a release on
another thread frees the slot while that burst's own handle keeps its grid
alive to the end. Ruling R-T4-4 makes the reported RMS honest: at `λ = 0`
every inlier is a node and the spline INTERPOLATES, so the in-sample
residual (0.0013 px on the synthetic
scene against a real 0.108 px off-inlier error) would leave `maxRmsPx`
toothless — above the node cap the non-node inliers ARE the hold-out, below
it `TPS_HOLDOUT_STRIDE = 5` fits a second spline on 80 % of the stratified
node order and measures the held-back 20 %, while the SHIPPED model stays
the one fitted on all nodes. **A TPS row's hold-out rms is not comparable
with a polynomial row's in-sample rms**: on the same scene the spline
reports 1.47 px against the cubic's 0.93 px while being 11× more accurate
against the truth field. `dedupe_nodes` +
`TPS_MIN_NODE_SEPARATION_PX = 0.05` closes a real defect found while
pinning that hold-out — `pair_through` gives each subject star its own
nearest reference star independently, so two subjects can claim ONE
reference, the node set carries that position twice and Bookstein's system
is exactly singular; roughly one synthetic seed in three fell back to its
linear model before the fix (the correspondence ambiguity itself stands for
every model). The local distortion loop lives in
`stacking/register/local_loop.rs` (extracted from `align`): with
`registration.localDistortion` and any distortion model, up to
`LOCAL_DISTORTION_ROUNDS = 3` rounds of re-pair through the whole current
map at `ransacTolerancePx · (1 + round)` → RANSAC a corrector homography →
stop at `‖H_c − I‖_F < LOCAL_DISTORTION_STOP = 1e-3` → else compose into
the linear part and refit the distortion; ruling R-T4-5 has the accept
guard evaluate the incumbent AND the candidate on ONE common pair set (the
incumbent's own inliers), so a round whose corrector kept an easier subset
cannot look better while being worse. `Alignment.local_rounds` is a field
of its own (ruling R-T4-1) — `refit_rounds` already means the σ-clip rounds
inside one `refit_weighted` call. `registration.tpsSmoothing` (clamped to
`[MIN_TPS_SMOOTHING, MAX_TPS_SMOOTHING] = [0, 10]`) defaults to `0.5` —
ruling R-T7-1's own measurement, not the interpolating `0.0`: at the
600-node cap on real 26 Mpx frames the hold-out rms was 0.145 / 0.203 px
(mono / OSC) at λ = 0, 0.099 / 0.156 at 0.5 and 0.102 / 0.165 at 2. Both
new fields ride `registration_subtree` (`cfg.registration` is serialized
whole — `tpsSmoothing` is in every set's registration hash whatever the
`distortion`), so the first run after M4c re-registers every set once on
purpose; the later default-VALUE change costs no second invalidation, and
reaches only documents that OMIT the field. `model_name` yields
`homography+tps`, with `+wcs` still LAST (`homography+tps+wcs`)
because `FramesTable.tsx::splitRegModel` strips the trailing suffix for its
`WCS` chip.

**LN local scale and the barycentre pass** (rulings R-M4c-8/9):
`ScaleResult` grew `pass: u8` and `local: Option<ThinPlateSpline>` (and
lost `Copy`). With `normalization.local.localScale`,
`ln::scale::fit_local_scale` fits an approximating spline on the RCR
survivors' residuals `z_k − scale` at their reference positions with
`λ = LN_LOCAL_SCALE_SMOOTHING_SIGMAS (5.0) · σ_z`, and `ln::a_grid` samples
`A(node) = s + spline(node).0` on the stride mesh in place of the constant
`s`, with `B = B_ref − A·B_tgt` following; with no spline `a_grid` returns
`vec![s; gw·gh]` before touching a position, so every M2/M3/M4a LN
byte-identity pin holds. `LN_LOCAL_SCALE_MIN_STARS = 40` counts DISTINCT
reference stars and is applied AFTER the reference-index dedupe the one-way
match makes necessary, so 40 pairs collapsing onto 5 stars fit nothing;
`LN_LOCAL_SCALE_MAX_DEVIATION = 0.25` (in `ln/mod.rs`, beside its only
consumer) refuses a sampled surface that moves by more than a quarter of
`s` across the frame AS A WHOLE, loudly — never a partial clamp. Ruling
R-T5-1 leaves the math reference's surface-simplification step deliberately
unimplemented: because λ scales with the dispersion, a PURE-NOISE ratio
sample still yields a smooth spurious surface — measured end to end through
the real detector, fitter, RCR and grid sampling over 10 seeds at
σ_z ∈ [0.031, 0.038], peak-to-peak **0.92–2.18·σ_z** — so a no-gradient
control pin records it at `3.0·σ_z` as a number to beat (the gradient pin's
own REAL structure runs at ≈ 5·σ_z), and Task 7 variant E decides on real
data. The barycentre second matching pass re-matches on the DETECTION
barycentres (the seeds' positions before the PSF fit) when pass 1 covered
less than `LN_BARYCENTRE_PASS_THRESHOLD = 0.8` of the **TARGET's** own
accepted fits — not the reference's (ruling R-T5-2): the LN reference is an
integration of the group's best 20 frames and therefore deeper than any
single target, so against ITS fit count "matched under 80 %" is the
ordinary case, the pass would run on nearly every real frame and its lazy
preparation would be defeated. The larger pairing wins, a tie keeps pass 1,
and the pass-2 tree arrives as a `FnOnce` so the common path pays nothing.

Two process facts this cycle established, both worth remembering: **the
headless check does not exercise `integration/`** — `cargo check -p
athenaeum-core --no-default-features` passes while `lib.rs` carries
`#[cfg(feature = "render")] pub mod integration;`, so the module is not
compiled at all (verified in Task 2 by appending a deliberate type error to
`combine.rs` and watching the check stay green); of the trees this cycle
touched only `geometry/` is genuinely ungated, so the headless gate is not
coverage for anything under `integration/`. And **`cargo test --lib` hides
example breakage** — a public-signature change that breaks
`examples/*_probe.rs` surfaces only under
`cargo check -p athenaeum-core --all-targets`, which is the re-gate to run
after one. **Acceptance run 2026-09-12**
(`docs/superpowers/research/2026-09-12-m4c-acceptance-run.md`, runs 22–33 on
LDN 1272, the M4a acceptance config as the baseline): the rejection variants
from Integrate — ESD 0.150 / 0.138 % rejected (by design: only genuine single
extremes; the set's one satellite trail still rejected), RCR 1.81 / 2.25 %,
min/max exactly 2/n per stack, Winsorized 4/3 0.68 / 0.71 % — all with
7–13 % LOWER master noise than the 5.0/3.5 linear fit's 3 %; RCR (strongly:
faint-star peak p10 0.67×, width +3.4 %) and Winsorized 4/3 (mildly: p10
0.73×) clip medium/faint star cores on the OSC RED plane (the skewed
per-pixel distribution of VNG-interpolated cores under seeing), mono and the
G/B planes untouched, the brightest stars untouched — the Auto ladder never
selects them at n ≥ 20, so a caution ships instead of a default change; the
Winsorized before/after on a REAL master dark (set 1763, 100 × 180 s bin 2,
rebuilt through the M4c server) median −0.002 %, MAD +0.010 %, hot pixels
−0.855 % (targets ± 0.1 / 2 / 1 %). Large-scale rejection: every target
met (`large_scale_rejected_fraction` 0.137 / 0.049 %, the master unchanged
within noise, the real trail's sparse pass-1 mask survives the cascade and
is densified — Task 3's m5 closed) at 3.5× the integration time. TPS: the
FIRST attempt (run 27) stalled the 16 GB machine in the mono drizzle (0 %
CPU, 11.3 GB swap) because every frame's grids stayed alive for the run —
ruling R-T4-6 (a release per stage, `8f7f1242`) fixed it and the three
re-runs took 49 min each without a stall; hold-out rms λ = 0 / 0.5 / 2:
0.145 / 0.099 / 0.102 px mono, 0.203 / 0.156 / 0.165 OSC — **ruling
R-T7-1: `tpsSmoothing` default 0.5**; no measurable FWHM gain on this
cubic-corrected field and the M3 drizzled OSC G/B residual is unchanged
(not registration distortion — M4d's Bayer-drizzle item). LN local scale:
corner/centre change < 0.1 % on every plane, LN stage time within noise
(12.06 vs 12.6 min in a clean window), a spline on all 688 channel-frames
(σ_z ≈ 0.10) whose per-frame ripple costs +3.1 / +5.4 % master noise on
G/B — stays off by default, the reference's simplification step is the
follow-up; the barycentre pass won on 6.2 % of channel-frames with the
target-side denominator. The desktop click-through of the four panels is
owed to the owner (two Chrome instances were connected; a static check of
the served bundle stands in).

**M4d — outputs** (spec §6.4/§7/§9.1/§9.2/§10.1, plan
`docs/superpowers/plans/2026-09-10-stacking-m4d-plan-outputs.md`, rulings
R-M4d-1…7 plus the fix-round ruling R-T2-1): Tasks 1–4 landed 2026-09-12
(Bayer drizzle, XISF output) through 2026-09-14 (`master_lights` + preview,
user presets) — `4748f91c`..`7d5e4780`, code-complete with green gates and
clean reviews; **accepted 2026-09-14** on LDN 1272 (runs 34–36,
`docs/superpowers/research/2026-09-14-m4d-acceptance-run.md`): Bayer drizzle
level within 0.1 % and coverage 1.0 on every plane, the G plane 3 % sharper
than the debayered drizzle, and the M3/M4a "OSC drizzled G/B ≈ 10–12 %
broader than the external tool's" residual CLOSED — G +0.5 %, B +0.2 %
against the external CFA drizzle of the same frames (R +4 %); the R−G/B−G
colour offsets (0.36/0.23 px) and the broader R plane are in the DATA — the
external CFA drizzle shows the same offsets (0.28/0.21 px) and the same R/G
ratio — so ruling R-T6-1 re-states the plan's unattainable absolute
"≤ 0.1 px" as "within 25 % of the external CFA drizzle's, same directions";
the 160 mosaics are re-used on a re-run from Register (Calibrate 0.00 min);
the XISF masters read back through the harness identical to the FITS ones
(mono every term, OSC ≤ 1.9e-10); `master_lights` 6 rows per run; previews
6/6 from FITS and from XISF; presets round-trip through the API. Owed to the
owner: the external-tool open of an Athenaeum `.xisf`, and the desktop
click-through of the preset menu and the results-card thumbnails.
**The calibrated CFA mosaic and Bayer drizzle** (rulings R-M4d-1/2):
`execute_generation` gains an optional second write — beside the debayered
`c_<stem>_d.fits` it writes the corrected, pre-debayer CFA mosaic
`c_<stem>.fits` from the SAME in-memory frame (one read, one calibration,
two writes) whenever `CalibratedLightOptions.keep_mosaic` is set (a
run-internal `#[serde(skip)]` flag, never on the wire). `wants_cfa_mosaic`
(`stacking/plan.rs`, read by the plan gate, stage 1 and stage 8 alike)
requires `drizzle.enabled && drizzle.bayer && calibration.debayer_osc` for
an OSC group — turning `bayer` on with the debayer off costs one run-level
warning instead of a permanent recalibration loop (fix round 1, I1). The
mosaic is its own `stacking_artifacts` row (`kind = "calibrated_mosaic"`)
under the SAME calibration hash as the debayered artifact — `drizzle.bayer`
enters the whole-config run fingerprint but no per-stage hash, and the plan
gate's Calibrate staleness follows the pair (I2): a cached calibrated frame
whose mosaic row is missing regenerates BOTH files in one generation.
`drizzle.bayer` (default false) makes stage 8's deposit colour-pure:
`cfa_plane_of(pattern, x, y)` (`stacking/drizzle/geom.rs`, four const
tables keyed on `(y&1)*2+(x&1)`) routes every output plane's source read to
the mosaic's own colour sites — R and B each cover a quarter of the pixels,
G half — while everything else (the `.rej` lookup at the reference
coordinate, honouring whichever bitmap set is live including M4c's
`pass2/` large-scale set, the plane weight, the LN grid, the output pair)
stays the DEBAYERED run's, read through the SAME `ForwardEval` handle the
debayered deposit uses (no second map path — the M4c grid-release rule
R-T4-6 is untouched). `DrizzleStats` carries no Bayer flag — coverage is
read honestly off the mask, the level-preserving `I/W` is unchanged, and a
mono group ignores `bayer` (a `CfaSource` is refused for a non-3-channel
group at the engine boundary); a frame whose own `BAYERPAT` the catalog
cannot parse falls back to depositing its debayered planes, counted and
warned once per group.

**XISF output** (`output.format`, ruling R-M4d-3): a new
`fits_writer/xisf_writer.rs` (ungated, like `writer.rs`) writes monolithic
XISF 1.0 — signature block, one `<Image>` element with every master card as
a `<FITSKeyword>` (values formatted and sanitized through the SAME
`card::fmt_real`/`sanitize_text` the FITS writer uses, so a master's two
containers never disagree about what a card says), padded to a 4096-byte
boundary, then uncompressed little-endian Float32 planar samples.
`stacking/master_cards.rs::write_master_light`/`write_drizzled_master`
branch on `output.format` at the ONE point that decides a master's
extension — the drizzled master and its weight map follow the master's
format, the rejection maps stay FITS always. Row order is NOT flipped
(ruling R-T2-1): flipping would have to transform the master's WCS/SIP
cards too (`CRPIX2`, the CD matrix, the odd-`v` SIP terms), which is its
own follow-up (open-items). Instead the XISF keyword list always states the
EFFECTIVE order explicitly — the source's own `ROWORDER`, copied through
calibration → registration → master via `calibration_library::light_headers`'
`COPY_THROUGH_KEYWORDS`, or the synthesized `'BOTTOM-UP'` when the source
carries no card at all — and the run pushes ONE warning per GROUP (not per
output file) when the effective order is bottom-up. Fix round 2 found that
row-order copy-through is real end to end; fix round 1 had believed
otherwise because the M4d Task 2 test fixture wrote no `fits_header` row at
all, a state no scanned file is ever in — `stacking::test_fixtures::
insert_light_row` now inserts a real header (and `frames.roworder`) so
every run test exercises the same copy-through path a scanned file does.
XISF values are byte-identical to what the FITS cards say
(`card_grammar_parity_with_the_fits_writer` pins full grammar parity,
comment-length rule included); `XISF:CreationTime` is the wall clock, so an
XISF master is NOT byte-reproducible across two runs of the same input —
every M1–M4c byte-identity pin stays on `format = fits`.

**`master_lights` and the preview** (rulings R-M4d-4/5, amended by Task 3
fix round 1): stage 9 writes one `master_lights` row per WRITTEN output
(`master | drizzle | weight_map`, `UNIQUE(run_id, group_key, kind)`) in the
same connection scope as the `update_group` call that records the same
path — not a real `rusqlite` transaction (the pre-existing stage-9 shape
was one pooled connection, not a transaction; a crash between the writes is
an open item, not a regression this task introduced). Geometry is per
GROUP — the master's row is the group's reference geometry and plane
count, the drizzle/weight-map rows are the writer's actual output grid.
`get_master_light_preview(run_id, group_key, kind, maxPx)` renders through
the SAME format-aware path a catalog frame's preview uses
(`api::files::render_preview_from_path` → `rustafits_processor::
process_fits_to_jpeg` — `PlaneReader` cannot read `.xisf`, a Task 2
finding). Fix round 1 clamped `maxPx` to `[64, 2048]`
(`MIN_MASTER_PREVIEW_MAX_PX`/`MAX_MASTER_PREVIEW_MAX_PX`) at the API
boundary and made the cache key the RESOLVED RENDER STEP (`thumbnail |
preview | full`, `api::files::preview_step`) instead of the raw number — an
unclamped `maxPx` was itself the cache-file key, so a caller could force a
native-resolution render of a ~100 Mpx drizzled master and mint `2^32`
distinct cache files; the ceiling at 2048 keeps `Resolution::Full`
unreachable from this command entirely, not merely bounded. The web host
answers both `POST /api/get_master_light_preview` (the `api.invoke` mirror
both hosts use) and `GET /api/stacking/master-preview?…` (browser-friendly
only when no API key is configured — the router sits behind
`require_api_key`, which an `<img src>` cannot satisfy). The path takes no
`heal_interrupted_runs` pass (a written master's file cannot be made wrong
by a stuck run row) and no `image_semaphore` permit (the clamp keeps the
expensive case unreachable; the fan-out is a run's group count, once per
panel mount). Cached under `previews/run-<id>/<group>_<kind>_<step>.jpg`,
reported in `WorkUsage.previews_bytes` and swept only by `CleanupWhat::All`
(a preview describes a WRITTEN master, not an intermediate).

**User presets** (ruling R-M4d-6): `stacking.presets` is one settings row —
a JSON array of `{ name, config }`, `config.paths` stripped on save so a
preset never carries folders — with `list_stacking_presets`/
`save_stacking_preset`/`delete_stacking_preset` (the 16th–19th stacking
commands) returning the full list sorted by name case-insensitively after
every change. All three validation failures (name length, too many
presets, unknown name on delete) answer `ApiError::Invalid` (400) with
fixed strings; a document that fails to decode as a JSON array at all
answers `ApiError::Conflict` (409) on either write (naming the settings key,
so a client can tell a state problem from a validation problem) while
`list` reads it as an empty list with a `warn!`. Fix round 1 made the read
ENTRY-WISE (`decode_presets_entrywise`) — one undecodable element inside an
otherwise-good array no longer hides its siblings or blocks a write, it is
dropped with one `warn!(key, count)` and the next write rewrites the row
without it — and wrapped both writes in one `BEGIN IMMEDIATE` transaction
(`begin_presets_write`) so a read-modify-write cannot lose a concurrent
saver's entry. Logging dictionary: `preset_name` (the trimmed name an event
is about) and the formalized `key` (the `settings` row key, in informal use
since T1) are both in
`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`. In the tab,
a run disables APPLY only (fix round 1, I1) — the menu, Save current as…,
and delete stay live during a run.

**v0.6.3 — fix round (2026-09-15)**, four owner-reported bugs after
v0.6.2 plus the red CI, no command surface changed: (1) **XISF `bounds` is
mandatory** — XISF 1.0 §11.5.1 has NO default representable range for a
Float32 image and the external tool refuses a file without it; the writer
stamps `bounds="0:1"` (every master is u16-domain/65535, `ATH_CSCL`; a
representable range, not a clip — saturated cores above 1.0 are legal) and
`imageType` (`MasterLight`/`WeightMap`, nothing guessed for other
`IMAGETYP`s). (2) **"Re-run from" follows spec §8**: the menu lists the
CACHEABLE stages (Calibrate/Measure/Register, Normalize when LN is on) plus
Integrate — each forcing itself and everything after it fresh, reusing what
is before — with stale entries visible but inert; Plan 5b had built it as
`staleStages ∪ {integrate}`, disabled when nothing was stale, so no entry
ever differed from ▶ Run. The plan gate's `cleanup_stale_note` adds a
warning naming the last `done` run and its `output.cleanup` policy when
that policy is why nothing is cached ("Run #N deleted its calibrated and
registered frames afterwards … the next run starts from Calibrate whichever
stage it is re-run from"); `paths::INTERMEDIATE_ARTIFACT_KINDS` no longer
lists `metrics` — a file-less payload row whose hash keys on the
calibration CONFIG hash (`measurement_hash_for`), so it stays a valid cache
hit after the calibrated frame is regenerated; only `CleanupWhat::All`
drops it. (3) **Per-frame progress from inside the fan-out stages**:
`run.rs::FanOutTicker` (thread-safe, stage-wide `current`/`total`
continuing from the cached count, the 300 ms throttle with the tick on
`total` always emitted, the gate held ACROSS the emit so racing workers
never deliver ticks backwards) is what Measure, Register and Normalize tick
from their `fan_out` closures — they used to report only when a whole
group's fan-out returned; `register_group_pass` adopts the ticker's count
after the fan-out instead of re-emitting in its results loop; the LN
reference build emits per-plane messages; `emit_integrate_tick` carries a
`message` (`integrate_tick_message`: `plane i/n [· pass 2] · band b/B |
combining | reading`); `StageRow.tsx::progressText` renders every running
row as `count · percent · bytes · group · message`, Integrate's plane index
dropped as a count. (4) **The R-T4-6d grid-residency pin** asserts `peak <=
tps_frames`, not `peak <= threads + 4`: `integrate_planes` opens ONE
`RegisteredSource` per group (R-T4-7) and every band reads every included
frame through its inverse grid, so the spline frames' grids are all
resident for the whole integration by design (~3 MB each at the 8-px grid
step), independent of the pool — the old bound held on the 10-worker Mac
only because 11 ≤ 14 and was red on every 4-worker GitHub runner from
v0.6.1 through v0.6.2. The stacking measurement (`stacking::measure`)
shares rustafits' primitives (fast detector, Moffat fit, background mesh,
MRS noise) with the Analysis tab but reads NOTHING from its tables: it
measures the CALIBRATED frame from scratch (PSF Signal Weight, PSF SNR,
normalization stats — the weighting the external tool's is calibrated
against, M4a) and caches the result as the per-frame `metrics` artifact.

**Perf tier A — compute, bit-identical (2026-09-19/20)** (audit
`docs/superpowers/research/2026-09-19-stacking-compute-audit.md`, plan
`docs/superpowers/plans/2026-09-19-stacking-compute-tierA-plan.md`, acceptance
`docs/superpowers/research/2026-09-20-stacking-compute-tierA-acceptance.md`,
rulings R-TA-1…11 in the plan's ledger): fourteen tasks, every checkpoint
byte-identical to the baseline on the four masters, 565 registration rows,
197 `.athln` sidecars and 302 calibrated frames; the reduced set
`LDN1272-test` (prod set 204, 92 mono + 105 OSC 180 s lights) 27.0 → 23.4 min
against a same-evening baseline re-run (−13 %; 26.6 → 22.3 against the
morning's cool-machine baseline). **The registered frame is a REQUIRED artifact** (Task 6a,
owner decision 2026-09-19 — "disk as storage so nothing is computed twice"):
`stacking_artifacts.kind = "registered"`, written by Register inside its own
fan-out (plane at a time, `Durability::Volatile`), keyed on the registration
config hash; Normalize and Integrate read it verbatim through
`RegisteredSource::open_materialized` instead of re-warping per band (LN warp
3.3 s → 0.15 s per frame, Integrate read 26 → 7 s per plane), a
missing/unreadable/wrong-size file falls back PER FRAME to the on-the-fly warp
with one `warn!(frame_id, path, src)`; drizzle still reads the calibrated frame
through the forward map. Disk roughly doubles (`registered/` ≈ the
`calibrated/` footprint, 41 GB beside 50 GB on the reduced set) and the plan
gate counts it at REFERENCE geometry, so the `space` blocker refuses runs that
used to start; the gate's Register staleness folds in the artifact's freshness
(`find_artifact` + `is_fresh` per frame), so an existing catalog's first run
and every run after `deleteRegistered`/`deleteIntermediates` reads as
rewriting, not cached. `registration.writeRegisteredFrames` is INERT (kept
for compat; the panel shows a note, `dc9f6287`). **Admission**:
`REGISTER_PLANES_RESIDENT = 4` / `REGISTER_PLANES_RESIDENT_OSC = 6`, keyed on
the batch's measured plane count (OSC register admission 6 on 16 GB);
`MEASURE_PLANES_RESIDENT`/`LN_PLANES_RESIDENT` STAY 8 — ruling R-TA-3: the
Measure peak (≈ 7 planes) is intrinsic to `noise_mrs`'s à-trous layers, the
audit's "admission 4 → 6–8" is withdrawn. **New log fields** (dictionary):
`ln_detect_ms`/`ln_fit_ms`/`ln_match_ms` on `ln frame normalized` (detection
IS the LN scale cost — 4.8 of 6.5 s per frame before Tier C),
`combine_cpu_ms`/`rejection_iters_mean`/`medfit_evals_mean` on `plane
integrated`. **Measured OUT and reverted, numbers in the doc comments** (do
not re-propose): Task 9's cached `b·i` (1.4–1.6× slower), four integer
accumulators (1.05–1.09× slower), `sort_unstable_by` + tiebreak (1.36× slower
than the std stable sort at n ≈ 208); Task 10's `with_max_len(4)` (+0.8 %);
Task 3b's bit-packed `sig_mask` (CPU for memory that bought no admission);
Task 11's mosaic cache (blows the R-M3-7 ceiling). What paid: the detector
(rustafits `perf/stacking-kernels`: the caller's bg/noise pair, the dead
noise map, `hfd_at`'s scratch window + static r² table + selection medians,
parallel histograms — Register detect −45 %, LN detect −30 %), the warp's
interior fast path (2×, all seven kernels pinned), the f32 band lane, the
no-division `Linear::apply` for affine maps, the incremental
`sampling_radius` ring, the parallel LN background, the drizzle band skip
(a SAMPLED-max bound + 1 px margin — R-TA-10: a homography's Jacobian is
position-dependent, so the origin drop alone is not a bound). **Method rules**
(R-TA-8/9): this Mac drifts ±10–15 % across a build-heavy session — only
INTERLEAVED before/after brackets are comparable, the product-build checkpoint
is the arbiter (a probe under thin-LTO swung +21 % on an untouched function),
and the acceptance total is read against a same-evening baseline re-run. The
release probes carry `RUST_LOG`-gated subscribers (R-TA-1). **Harness**
(athenaeum-lab `harness/acceptance/` since 2026-10-06, reuse it — owner
2026-09-18): `prepare-catalog.sh <work> [fmt] [set-id]` (`ATH_ACC_DB`, copies
set 109's config, forces FITS/keepAll, REMOVES `paths`), `server.sh`
(`ATH_ACC_EXTRA_PATHS`), `tier1/tier1-run.sh … SET_ID` (exits non-zero on a
refused `start_stacking`), `tier1/checkpoint.sh <name> [base]` (refuses on
Time Machine/cargo, bare-word names, a `paths` override; needs ≈ 135 GB
free), `tier1-extract.py`/`tier1-compare.py`; `checkpoint.sh` re-execs under
zsh from any shell and refuses `name == base` (a `bash` invocation once shifted
its positional parse and overwrote the ruler tree).

**Perf tier C — numeric, gated by spec §8 (2026-09-20)** (spec
`docs/superpowers/specs/2026-09-20-stacking-compute-tierC-design.md`, plan
`docs/superpowers/plans/2026-09-20-stacking-compute-tierC-plan.md`, acceptance
`docs/superpowers/research/2026-09-20-stacking-compute-tierC-acceptance.md`,
rulings C-1…C-29 in the spec's §9): Tier C changes outputs on purpose, so its
gate is the §8 tolerance table against the Tier A ruler
(`.athenaeum-acc/tierA-baseline`, byte-identical to Tier A's own tree on every
compared artifact — ruling C-19), read by `checkpoint.sh <name> baseline
--numeric`. Reduced set: **21.73 → 14.47 min against a Tier A build re-run back to
back (−33 %; 23.44 → 14.47 = −38 % against Tier A's acceptance run on a
hotter evening; 27.0 → 14.5 = −46 % against the pre-audit baseline)**, every gated row PASS (masters within 0.1 %
median / 1 % MAD / 2 % noise / 1 % FWHM, rejected fraction +0.002 pp,
per-frame weights ρ = 1.0, LN scale −0.035 %), the external-masters gate PASS.
**What shipped.** (1) **Measure persists its PSF fits** as the per-plane `fits`
artifact (`stacking/fits_artifact.rs`, `FITS_ARTIFACT_VERSION = 1`, kind
`fits.<plane>`), and the group's β is the LOWER median of the members' Auto β
(`psf_signal::group_beta`, `PsfModel::Fixed(β)`, `PSF_FIT_VERSION = 3`, since replaced by `star_measure::MEASURE_VERSION` — see "One star-measurement path, Plan A";
`stacking_run_groups.beta`). (2) **LN's relative scale comes from those fits
as SEEDS, not fluxes** (ruling C-12 — H2 finding: a Moffat fit's integrated
`signal` is NOT warp-invariant, the B-spline-warped frame differs from the
native one by an FWHM-dependent factor up to 20 %): the seed positions are
mapped through `forward_exact`, re-fitted on the warped plane at the group β,
and a per-channel calibration `k = median(s_detected / s_seeds)` over a
weight-STRATIFIED 7-frame sample (the reference + one member per weight
sextile, `ln/calibration.rs`, rulings C-14/C-15 — the best-weighted sample was
one contiguous half-hour and left a +0.44 % systematic; stratified −0.05 %)
is refused outside `[0.97, 1.03]`, applied only to seeds-path channels, folded
into the `.athln` hash and recorded in the run summary (`seedsCalibration`).
The forced-detection calibration arm is silent and not a fallback
(`ln_scale_source = seeds | detected`, dictionaried). LN scale 6.5 → 0.64 s per
frame; Normalize 5.70 → 2.01 min; zero fallbacks on the acceptance set. (3)
**Register on a mono frame reuses the fits** (ruling C-6; OSC keeps luminance
detection): `Star.flux = fit.signal / ADU_SCALE`, `passes_register_cuts`, a
`MIN_INLIERS` floor falls back to detection with one `warn!`
(`star_source = fits | detected`); the frame is opened header-only
(`PlaneReader::open`, no plane read) and `star_source = "fits"` on 92/92
mono frames of the acceptance run, whose `frame stars detected` median
detect went 730 → 236 ms — and Register no longer fires that event on a
mono frame at all. NOT the `frame registered` row's `read/detect`: those
have been hard-coded `0` since perf tier 1 Task 9 split detection out of
alignment (`register_detected`'s own doc says so), so they were already
`0/0` before this task and are no evidence for it. **Both the LN hashes and the registration hash now fold the
per-frame MEASUREMENT hash** (rulings C-17/C-18) — the seed population moves
with `measurement.detectionSigma`/`seedDetector`/`seedPrefilter`/`maxStars`,
so a Measure config change re-normalizes and re-registers instead of reusing
stale sidecars/rows. (4) **The drizzle deposit goes through a per-phase
overlap table** (`stacking/drizzle/phase_table.rs`, `PHASES = 64` — ruling
C-21, the plan's 2 % per-pixel bound against the exact clip is unreachable at
32; residual ≤ 1.0–1.6 %, level 1 ± 8e-6, `Σ area` per phase = the drop's
area to 1e-6): one 212 KB table per (frame, plane) for a linear map, per
256-px tile from the local Jacobian under distortion, `SquareOverlapPlan::
resolve` the ONE dispatch (`scale == 1 && dropShrink == 1.0` keeps the exact
clip, C-3), `DRIZZLE_KERNEL_VERSION = 2` in the whole-config fingerprint only;
deposit −85 % per plane, Drizzle 4.26 → 1.18 min. (5) **rustafits' Moffat LM
arithmetic** (`perf/stacking-kernels` `18377bd`/`d98629e`): one
transcendental per sample-iteration (`power = base^-β` once, `dpower = -β·
power/base`, `powi` when β is integral), residual reuse from the last ACCEPTED
Jacobian pass, thread-local Cholesky scratch — control flow untouched (ruling
C-4: iteration counts identical 180/180, ≤ 7e-15 px), `PSF_FIT_VERSION` NOT
bumped (C-24: `fwhm_px` moved 1 ulp); fit −30…−49 %, Measure 4.48 → 3.67.
**Measured out and reverted, numbers in the doc comments (do not re-propose):**
`medfit_line`'s warm bracket (0.966× combine, 4 % of stacks to a different,
equally valid MAD root — the exit-on-zero-rejection it was paired with has
existed since the first clipper and is now pinned; ruling C-26) and the LN
background on a 4×4-binned plane (−80 % on a 3-frame probe, but on the full
group the `B` grid moved 2.4e-3 of sky at the MEDIAN node and the masters
failed §8 — mono MAD +1.9 %, OSC blue FWHM +2.7 %; ruling C-29). **Two absolute
§8 rows read FAIL on this set for data reasons the baseline shares** (C-20/
C-23): the OSC-red drizzle level sits at the 0.998 bound in the baseline
itself (0.998007 vs 0.997995), and coverage is 0.9935 on mono (the rotated
mono footprint does not reach the OSC reference geometry's corners) and short
by 2 edge pixels on OSC blue — reported, never re-sized. **Method rules added**
(memory + ledger): a per-frame numeric change is judged on the WHOLE group at
master level (a full checkpoint) before its review closes, never on a
few-frame probe; a small gain (≈ 3 % of the run) that misses any §8 row is
reverted with its numbers. Harness: `tier1-compare.py --numeric`, the
`tierC-external.sh` gate (C-8/C-9), `drizzle_probe`, `ln_probe --diag /
--seeds-calibration / --measure-calibration`, `register_probe --fits`.

**Run subjects (wave 6b-1, 2026-10-02)** (plan
`2026-10-02-collab-v3-wave6b1-stacking-foundation-plan.md`, spec
`2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §18 steps 0-2): the
behaviour-preserving seam under project stacking. Nothing a frame-set user can
see moved; the wire shapes (`StackingRunRow.framesSetId`, events, `RunSummary`,
`StackingPlan.setId`, filled by `run::wire_set_id`) are unchanged (6b-2 changed them; see below).

- **`StackSubject::{FrameSet(i64), Project(String)}`** (`db::stacking`,
  ungated, re-exported as `stacking::StackSubject`; its `Display` prints
  `frame set {id}` so frame-set log fields and error strings are
  byte-identical) keys runs, run-frames, artifacts, `master_lights`, the
  admission check, work usage / cleanup and the heal sweep. A frame id is read
  in the subject's namespace: a catalog `frames.id` for a frame set, a
  `stack_project_frames.id` handle for a project; the two never meet in one
  map or query. `StackHandle.subject` replaces the set id.
- **The four rebuilt tables** (`db/schema.rs::rebuild_stacking_tables_for_subjects`):

| Table | Subject pair | Frame pair | Constraints |
| ---- | ---- | ---- | ---- |
| `stacking_runs` | `frames_set_id` / `project_id` | none | exactly one of the subject pair |
| `stacking_run_frames` | via the run | `frame_id` / `project_frame` | exactly one of the frame pair; `UNIQUE(run_id, frame_id)` + `UNIQUE(run_id, project_frame)` |
| `stacking_artifacts` | `frames_set_id` / `project_id` | `frame_id` / `project_frame` | exactly one subject; at most one frame; `frame_id` needs a set, `project_frame` needs a project |
| `master_lights` | `frames_set_id` / `project_id` | none | exactly one subject; `UNIQUE(run_id, group_key, kind)` |

  The run-frame uniques are table-level, not partial indexes:
  `upsert_frame_row`'s `ON CONFLICT(run_id, frame_id)` cannot match a partial
  index, and NULLs are distinct in a UNIQUE, so the two kinds of row never
  collide. The artifacts' unique key is `stacking_artifacts_subject_key` on six
  expressions, `COALESCE(frames_set_id, 0), COALESCE(project_id, ''),
  group_key, kind, COALESCE(frame_id, 0), COALESCE(project_frame, 0)`;
  `upsert_artifact`'s `ON CONFLICT` repeats them token for token, and
  `find_artifact` / `delete_artifacts` / `delete_all_artifacts` filter on the
  same COALESCE expressions so a per-frame lookup is a point lookup on that
  index (pinned by `EXPLAIN QUERY PLAN` tests; an `IS` pair there degrades to
  a scan of every artifact of the set). `list_runs`, `active_run_for_subject`
  and `list_artifacts` use `IS` pairs (`list_unfinished_runs` has no subject
  filter). `stacking_run_groups` gains a nullable `exposure_class` column in
  the same rebuild (it is also a target of the in-transaction FK check).
- **The rebuild** (detected by `stacking_runs` lacking `project_id`, so it
  runs once): `PRAGMA foreign_keys = OFF` before `BEGIN` (inside a transaction
  the pragma is a silent no-op, and with it ON dropping `stacking_runs`
  would cascade-delete every group, run-frame and master-light row), one
  IMMEDIATE transaction that creates `*_new`, copies with the same ids,
  drops and renames, re-probes under the lock, and runs `foreign_key_check`
  INSIDE the transaction on the rebuilt tables plus `stacking_run_groups`
  before COMMIT. A violation rolls everything back, foreign keys go back ON,
  and startup refuses until the rows are repaired; the error names the table,
  `PRAGMA foreign_key_check('<table>')`, and that deleting those orphan rows
  is safe (cache and run-history rows). A fresh database is created in the new
  shape from the same DDL constants (`STACKING_RUNS_BODY` and siblings), so the
  two shapes cannot drift
  (`fresh_and_migrated_stacking_tables_have_the_same_shape`). Every stacking
  index is created by `create_stacking_indexes` AFTER the rebuild point
  (outside its transaction), which first drops the pre-6b
  `stacking_artifacts_key` so a later start never re-creates it on the new
  table. Also created now for 6b-2 to fill: `stack_project_frames`
  (`INTEGER PRIMARY KEY AUTOINCREMENT`, so a handle is never re-used; no
  foreign key to `project_frames_local`, whose rows exchange writes delete and
  re-insert; minted only by `db::stacking::project_frame_handles`, one
  IMMEDIATE transaction, called only from tests in 6b-1),
  `stacking_project_config`, `collab_projects.canonical_grid_json`,
  `stacking_run_groups.exposure_class`.
- **Per-frame catalog reads go through `stacking::origin`.** `FrameOrigin`
  had one variant in 6b-1, `Catalog { file_id }` (6b-2's two are below); `origin_of` refuses a frame with no
  origin (an error, never a silent catalog read); `stored_solve` and
  `cfa_geometry` are the dispatchers. `HashMemo` is built with the run's
  origin map and dispatches the stage-1 hash by origin, so its callers did not
  change. Calibrate's generation read dispatches the same way: a missing
  origin is a per-frame exclusion in Calibrate and fails the run in later
  stages. 6b-2 adds `Project { .. }` and every `match` then fails to compile at
  exactly the sites that need a project arm. Subject-level reads have a
  dispatcher too: `paths::subject_layout` (`<working>/<set_slug(name)>` for a
  frame set, `<working>/_projects/<set_slug(slug)>` for a project; 6b-1 wrote `projects/`).
- **The config pin.** `calibration_subtree` writes grouping as
  `{"exposureToleranceSec": x}` only, pinned by
  `stage_hashes_are_pinned_to_golden_literals` (pure helpers, literal inputs).
  A new grouping or output field must leave those literals unchanged, so an
  existing frame set keeps its caches across an upgrade; only
  `config_hash_is_canonical_and_pinned` moves.
- **For future editors**: ts-rs copies `///` docs of exported fields into
  `src/types`; exported fields carry `//` comments so the generated files stay
  byte-identical (`git diff --exit-code src/types`).
- **Resolved by 6b-2.**
  - (1) The namespace overlap: a project lives under `<working>/_projects/`, and no frame-set slug can start with `_`.
  - (3) The funnels: a grep test now enforces them, and `last_run_reference_scale` / `cleanup_stale_note` take `&StackSubject`.
  - (2), the project's `RegistrationRecord` shape, belongs to registration onto the grid (6b-3).

**The project path (wave 6b-2, 2026-10-03)** (plan
`2026-10-03-collab-v3-wave6b2-project-path-plan.md`, spec
`2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §18 steps 3-7 and 13).
A collaboration project is a stacking subject up to, but not including,
registration onto its canonical grid. `start_stacking` still refuses a project
(`PROJECT_STACKING_UNAVAILABLE`). Project stages run only through
`run::project_test_context` in tests until 6b-3.

- **The wire is subject-aware.** The two events, `RunSummary`, `StackingRunRow`
  and `StackingPlan` carry `setId | projectId`, exactly one set
  (`run::wire_subject`). `RunSummary.projectId` is `#[serde(default)]`.
  `useStackingRuns` drops an event without a `setId` until 6c.
- **Four config fields.**
  - `grouping.exposureMode` (`seconds | stops | equivalent | none`, default `seconds`).
  - `grouping.stopsWidth` (default 1, clamped to `MIN_STOPS_WIDTH` = 0.1).
  - `output.coverageMap` and `output.cropToCoverage`.

  No stage hash moved: the golden literals hold with non-default values.
  `config_hash` was re-pinned once. A project's first plan seeds its stored
  document (`stacking_project_config`) with `exposureMode = stops`.
- **Registered frames** (post-acceptance fix, plan 6b-2). The `ATH_REGR` card
  carries the reference frame's file stem and keeps its " / reference frame"
  comment only when the whole card fits in 80 columns (`register::writer::
  reference_card`); a long stem (an attested Prepare stem, or a long
  frame-set calibrated stem of about 47 characters or more) is written whole
  and without the comment. `fits_writer::card` stays strict. A failed
  registered-frame write keeps its per-frame `warn!` and adds ONE run warning
  per group ("registered frames could not be written for N frames of <group>:
  <first error>; they are warped on the fly").
- **Exposure classes** (`groups::classify_exposures`, pure, shared by both frame sources).
  - `seconds` is the old clustering, byte for byte.
  - `stops` and `equivalent` anchor at the shortest remaining exposure and take
    everything within ×2^W. `equivalent` works on t/N², N = `meta.focalLen` /
    `meta.aperture`; without both it falls back to seconds, with a warning.
  - `none` is one class, `all`.
  - Frames without EXPTIME keep their own `unknown` class in every mode.
  - Tokens are `<t>s`, `st<num>`, `eq<num>`, `all` and `unknown`. The number has
    four significant digits, so two anchors (at least ×1.07 apart at the
    `stopsWidth` floor) never print alike.
  - `stacking_run_groups.exposure_class` holds the token; `exposure` stays seconds.
  - In the equivalent and none modes two classes may share the shortest member's
    seconds, so their masters differ only by a `_2` suffix (`resolve_collision`).
- **`FrameOrigin`** is `Catalog { file_id, attested }` or `Project { project_id,
  frame_uuid, publisher, landed_path, blake3, byte_size, size_mtime, meta }`.
  `meta` is the manifest's `collab::frame_meta::ProjectFrameMeta`, parsed once;
  `meta.wcs` becomes a `PlateSolveRecord` with crpix − 1. This is not
  `db::collab_frames::FrameOrigin` (`own | replica`).
- **The project frame source** (`stacking::project_source`) reads
  `project_frames_local` and the landed files' headers, never the catalog.
  - A frame is stackable when it is exportable, servable, not `own_staged`, and
    has a recorded `size_mtime_seen` that parses (one that does not is
    "unreadable size:mtime stamp", never "waiting"). Every other published,
    accepted frame is a Frames row with its reason.
  - Groups are colour × canonical filter × class, keyed
    `<mono|osc|cfa>__<filter>__<class>`, with no binning and no camera. `cfa`
    appears only when `debayerOsc` is off and the file is a one-plane mosaic,
    or when its shape cannot be read (a warning is logged; Prepare then
    excludes the unreadable file).
  - **The colour rule** (`project_source::color_token`), the same as the
    attested rule below: the manifest's `bayerpat` decides colour when it is
    present; otherwise the file's own plane count decides (three planes →
    `osc`, anything else → `mono`; an unreadable file is `mono`, with a
    warning). An externally debayered contributor light has no `bayerpat` and
    three planes, so it groups as `osc` and Prepare uses it in place. A
    Frames row's colour mode comes from the same token as its group.
  - Handles are read first and take no write lock; one IMMEDIATE transaction
    mints only the missing uuids.
- **Funnels.** `origin::tests::subject_and_origin_variants_stay_in_their_funnels`
  keeps every `StackSubject::` / `FrameOrigin::` path in the production half of
  `run.rs` and `plan.rs` inside the named funnels, or in an allowlist where each
  entry has its reason. The per-frame dispatch points the run calls live in
  `stacking::origin`, which the test does not scan: `stored_solve`,
  `cfa_geometry`, `landed_unchanged`, `needs_prepare`, `prepare_input`,
  `prepare_stem_needs_frame_id`, `solve_plan`, `wcs_tier` and
  `local_solve_outcome`. A third subject or origin touches exactly these and the
  named funnels.
- **The identity check.** `origin::landed_unchanged` compares a landed file with
  its row's `size:mtime` (2 s tolerance). It runs at Prepare, Solve, Measure,
  the Register candidates and the Output member snapshot. A changed file
  excludes the frame "changed during the run"; one that cannot be read excludes
  it with "landed file unreadable: …" (more specific than spec §5.1's single
  reason). A changed reference fails the run ("the reference frame changed
  during the run"); Register checks the reference BEFORE its star read (the
  co-registered path once, the native path per group), so that read never sees
  new bytes. A catalog origin always passes with no I/O.
- **The plan** (`build_plan`'s project arm).
  - It seeds the config and mints handles: the documented exception to "the plan
    reads only". Readiness is empty and there are no masters.
  - Blockers, in order: `grid`, `reference`, `folders`, `space`, `frames`,
    `unsupported`. `folders` also fires when the project's working root
    (`<working>/_projects/<slug>`) overlaps any scan root; the Collaboration root
    is one.
  - Warnings (spec §7): frames not on this device, other reasons a frame is left
    out, frames to be solved locally or that could not be, frames outside the
    grid, unsupported files, and unknown exposure equivalence.
  - Unsupported files also leave every group's included count and the ≥ 3
    rule, so a group Prepare would leave short is blocked by `frames` at plan
    time; they are not manual exclusions.
  - It carries `grid` (state, current, proposal, framesInside, canEdit) and
    `projectFrames`.
  - It does not yet know the WCS reference rule (below): a manual pin without
    any WCS passes the plan and fails at the Reference stage (a note for
    6b-3/6c).
- **R3 (attested sets), live for frame sets in 6b-2.** A frame set with
  `calibrated_externally` needs no masters: no masters / links / masterFiles
  gate and nothing to build, so `start_stacking` runs it with no master at all.
  Its lights are prepared, never calibrated; the stage-1 hash is
  `prepare_hash_for_catalog`.
  - The colour token follows the project rule (`project_source::attested_color_token`):
    the catalog `BAYERPAT` when present, otherwise a header-only plane probe
    (three planes → `osc`, else `mono`; an unreadable file falls back to the
    catalog rule, with a warning). `groups::group_frames_with` takes this only
    for an attested set; a non-attested set is grouped exactly as before, with
    no probe. The plan and the run share `frame_source`, so their group keys
    cannot differ.
- **Prepare** is stage 1 for a project and for an attested set. It reports under
  `Stage::Calibrate`, the stage-1 slot. A run never mixes prepared and
  calibrated frames: `stage_one_kind` refuses it.
  - **In place:** a float FITS (BITPIX −32/−64) that needs no debayer is used as it
    is. The `calibrated` artifact is the landed file, which no cleanup removes.
  - **Converted:** an integer FITS (÷ `scale_divisor_for_bitpix`) or an XISF
    (`read_raw` ÷ 65535) becomes float32 FITS `prepared/<group>/p_<stem>.fits`.
    An integer FITS is never used in place, although spec §5.1 row 1 allows
    it (narrowed to float, review I2): converting gives an attested set's 16-bit
    lights and a published replica the calibrated-light generator's one unit
    domain.
  - **Debayered:** a CFA mosaic, when `debayerOsc` is on, goes through the
    generator's own VNG (`debayer_plane`) into `p_<stem>_d.fits`. The
    pre-debayer mosaic is the `calibrated_mosaic` for Bayer drizzle.
  - **Names:** a project frame keeps the replica's stem (`p_c_<uuid>.fits`); an
    attested light's prepared name carries its catalog id
    (`p_<stem>_f<frame_id>.fits`, decided by `origin::prepare_stem_needs_frame_id`),
    because two files of one set may share a stem. A prepared output equal to
    its source is refused (error log, frame excluded).
  - **Excluded:** a tile-compressed FITS, an extension image, a distributed
    XISF or a file that cannot be opened, as "unsupported file: …" (the
    decoder's reason reads "not a 1- or 3-plane 8/16/32-bit or float primary
    image"); and a frame whose plane count after Prepare contradicts its
    group's colour, as "plane count {n} disagrees with the group's colour
    mode" (the group's colour comes from the manifest `bayerpat`, or, without one, the
    file's own plane count (an attested set follows the catalog BAYERPAT or the
    same plane-count rule); the debayer comes from the file's own header).
  - Prepared files carry the copy-through cards, `ATH_STK`, `ATH_STKV` and
    `ATH_PREP`, and never `CALSTAT`, `ATH_CSRC`, `ATH_PRJ` or `ATH_FILT`, so the
    scanner never catalogs one.
  - The hash is `prepare_subtree` (`debayerOsc`, `debayerMethod: "VNG"`,
    `prepareVersion`) over `blake3` and byte size (a project) or the catalog
    identity (an attested light).
  - `prepared/` counts as calibrated bytes and goes with `Intermediates`.
- **Solve** is the first thing Measure does. A project frame without `meta.wcs`
  is solved by `rc.solver`:
  - `stacking::solve::FrameSolver`; `CatalogSolver` opens the catalog lazily
    through `plate_solve::open_catalog_layers`, the opener both hosts now call
    (`ready()` opens it once, before the first uncached solve).
  - It solves the prepared file, with the project target and the manifest scale
    as hints, through `solve_frame_with_hints` and the extracted gate
    `acceptance_refusal` at the manifest scale.
  - **Domain order** (`solve::solve_in_domains`, the one attempt sequence, also
    used by the probe). A unit-domain frame (the median of a strided sample of
    at most 65,536 finite values is ≤ 1.5) is solved on an ADU-scaled copy
    (× 65535, `<scratch>/adu_<handle>.fits`) FIRST, and on the original only
    after a `Failed`, never after a `Refused`. A float file already in ADU is
    solved as is, with no copy. With no scratch folder the original is solved
    once. The copy is deleted right after the first call. Two failures report
    "<first>; on the unscaled pixels: <second>".
  - **Nothing is attempted, nothing cached** in two cases, both through
    `solve_unavailable` (one run warning counting only the frames without a
    cached `Solved`, frames retried next run):
    - the solver's catalog cannot open: `ready()` fails, and it is called only
      once a frame actually needs a solve (a run whose frames are all cached
      never asks), so an open failure cannot reach a frame's outcome;
    - the scratch folder cannot be created.
  - **`SolveOutcome::Unavailable`** is the per-frame case: the ADU copy cannot
    be written. It is never cached (`write_outcome` refuses it, and it
    serializes to an error), and the frame is retried next run.
  - Every other outcome (solved, failed, refused) is a `solve` artifact: group
    key `""`, payload `{blake3, app_version, outcome}`, hash over blake3 and
    the solver identity (`solver_config_hash:catalog_identity`). This departs
    from spec §5.2: the payload is `{blake3, app_version, outcome}`, failures
    are cached per app version, and the first attempt is the ADU-scaled one.
  - A frame whose manifest has no `pixelScaleArcsec` is solved with no scale
    hint and no scale check in the acceptance gate; the run logs a warning
    per frame before solving it. A missing project row is a warning too (the
    frames are solved without a position hint), never a silent blind solve; the
    cache write then fails on the artifact foreign key and nothing is cached.
  - **What is not in the key.** Per-frame hints (project target, manifest scale)
    are not hashed: a Solved outcome is hint-independent and the manifest scale
    is frozen per content, so only a Failed/Refused cached under an older
    project target misses a retry. A cached Failed/Refused from another app
    version is tried once more (review I7); a cached solve stays. Bump
    `plate_solve::config::SOLVE_RECIPE_VERSION` when a solver change should
    re-solve frames cached as solved. `rerun_from = Measure` does not force a
    re-solve.
  - `solve` is not an intermediate: `CleanupWhat::Intermediates` keeps it, and
    only `CleanupWhat::All` (every artifact row) deletes the solve cache.
  - `plate_solves` and `frames` are never written. `stored_solve`'s project arm
    reads the manifest WCS, then the run's solve of this content.
  - For 6c: the sub-step's own progress runs `Measure` to 100 % with the
    message `"solving"`, and Measure then restarts at 0, so a project run's
    Measure bar goes back once.
  - **Real-data probe** on the `.dev` LDN1272-test replicas, 2026-10-03
    (mono and OSC sub-rates, 10 frames each). Before the domain order was
    flipped: 0/20 solved on unit-domain pixels ("too few stars detected"), and
    the ADU-scaled copy rescued 20/20. After: `solve probe: mono 10/10 solved on
    the first attempt, 0 needed the fallback; osc 10/10 solved on the first
    attempt, 0 needed the fallback; unsupported 0, not on disk 0, unreadable
    manifests 0, with a manifest wcs 20; wall 187.2s` (≈ 221 s before).
- **The reference rule** (spec §5.4). On a project run the reference must carry a WCS.
  - Candidates are the manifest-WCS frames, or the locally solved ones only when
    no included frame has a manifest WCS.
  - Auto takes the largest group that holds a candidate, and first looks at
    groups with ≥ 3 included frames, then any group. A native group without a
    candidate takes the run reference (`group_can_switch`).
  - The two-pass re-pick never lets a non-candidate win (`two_pass_pick_among`).
  - A manual pin without any WCS (manifest or local) fails the stage; a pin that
    stage 1 excluded fails with its own reason ("the manual reference frame is
    excluded: …").
  - With no candidate, the stage fails with "no frame of this project could be
    plate-solved; the grid needs one".
- **The canonical grid** is described in `docs/transfers/README.md` (wave 6b-2).
  **Orientation:** the grid is sky-standard (north up, east left, det(CD) < 0).
  Every solve in the owner's catalog has det(CD) > 0, so a master on the grid is
  the mirror image of a raw frame, which is correct on the sky. 6b-3 must not
  copy the reference's `ROWORDER` onto a grid master.
- **Open for 6b-3:** all landed; see "The project path on the grid" below.
  The `expected_ln_channels` disagreement is gone: `plan::stage_one_planes`
  is the one plane rule for stage 1 and LN. An attested frame set of raw
  mosaics with the debayer off is keyed `osc__…` and counted as three planes
  by the plan and the run alike, while Prepare keeps its files as one-plane
  mosaics: both exclude such frames at stage 1, consistently. Mosaics stay
  mosaics only under `cfa__` keys, which frame sets never produce (review
  M7).

**The project path on the grid (wave 6b-3, 2026-10-03)** (plan
`docs/superpowers/plans/2026-10-03-collab-v3-wave6b3-project-grid-stacking-plan.md`,
rulings D1–D23; spec §5.5–§5.9, §9, §15). A project run starts, stacks every
group onto the coordinator-confirmed grid and writes grid masters.

- **One composed map per frame (D1).** `stacking::grid_map::compose_to_grid`
  fits an affine to the exact chain — star map, then the reference's WCS with
  SIP, then the grid's TAN — over a 32-px lattice (`COMPOSE_SAMPLE_PX`), and
  keeps the rest as a `DistortionModel::Sampled` layer: a `ComposedField`
  evaluated exactly, read through lazy bilinear 8-px grids (`TpsGrids`, built
  and released like a spline's). The inverse is Newton steps on the forward
  chain; a point that does not converge reads NaN, never an unconverged
  iterate. The map is never serialized; it is re-derived from (star map,
  reference WCS, grid) whenever a cached star map is reused. Pinned within
  0.01 px of the chain for identity, homography, polynomial and spline star
  maps, at 0° and 4° from the tangent point.
- **Two caches (D2).** The star map is a payload-only `registration` artifact
  (group key `""`) outside `INTERMEDIATE_ARTIFACT_KINDS`, so only `All` drops
  it. The registered frame's hash is `grid_registered_hash(registration
  hash, reference WCS hash, grid hash)`: a grid change re-composes and
  rewrites the registered frames with no star detection. A frame set's
  registered hash is its registration hash, as before.
- **Row order (D3).** Everything written on the grid states `ROWORDER =
  'BOTTOM-UP'`: the grid is sky-standard (det CD < 0). A grid master is the
  mirror image of a raw frame from the owner's rigs (det CD > 0) and is right
  on the sky. An XISF grid master fires the existing bottom-up warning.
- **Coverage (D4, D9).** The engine counts, per output pixel, the frames
  whose sample is finite (before rejection), in a post-pass over the band
  already in memory; the combine loop is untouched. `output.coverageMap`
  writes `<stem>_coverage.fits` (`BUNIT = 'frames'`, the master's WCS, kind
  `coverage`) and drizzle's own `<drizzle stem>_coverage.fits` (kind
  `drizzle_coverage`, counted from a `W` snapshot around each deposit; a drop
  below `W`'s f32 resolution is not counted). A frame set's master is
  bit-identical with the map on and off. A project's seed turns it on.
- **Statistics on the covered rectangle (review I1, final review A1).** On a
  grid every master's statistics (`GroupStats`: location, scale, noise, PSF
  SNR, SNR gain, FWHM, eccentricity) are measured on
  `crop::largest_covered_rect` at depth `k = ceil(n / 2)`, `n` the group's
  included frames: the largest rectangle every pixel of which at least half
  the frames cover, with or without a coverage map. The deep centre of a
  stack is what the statistics describe, not the thin one-frame fringe a wide
  offset frame adds (a one-frame rectangle would read mostly shallow, noisy
  pixels and understate the master's depth). When no pixel reaches `k`, or
  that rectangle is under `MIN_STATS_RECT_PX` (16) on a side, the statistics
  fall back to `k = 1` with a warning naming both rectangles; then the 16 px
  floor, and "no frame covers the grid", fail the group with a named error as
  before. The masters themselves never change, only the measured rectangle.
  A drizzle master's statistics are measured on the chosen rectangle scaled
  by the drizzle factor. Frame sets measure the whole plane, as before. The
  debug event `"master statistics on the covered rectangle"` (inside the
  `integrate_group` span, with `group_key`) names the rectangle and its
  depth; `grid-report.py` prints it beside each group's lines.
- **LN on the overlap (D5–D8).** A project's LN reference leaves uncovered
  pixels NaN, and each member is fitted on member ∩ reference: detection
  planes fill the rest with 0.0 (the detector ignores it), seeds stay 48 px
  (`LN_MASK_MARGIN_PX`) inside the eroded overlap, and both backgrounds come
  from the overlap. A member under 25 % overlap (`LN_MIN_OVERLAP`, measured
  on channel 0) keeps its global pair, gets no sidecar and an `ln_global`
  marker (a cache hit on the next run and in the plan), and the group warns
  once with the count. Admissibility on a grid uses the group's median
  footprint, not the whole grid. Frame sets keep the legacy LN; a project's
  LN hash folds `lnmask:1`. The field (review I6) is built from the in-scope
  frames' WCS footprints on a lattice of 32 probes per median frame side,
  forgives one lost probe, and its hash (the frames that pass the 0.97 rule
  against it) folds into every member's LN hash; the plan builds the same
  field (`origin::group_ln_field`), so its Normalize view moves with the
  field, never a run later.
- **Crop (D10).** With `output.cropToCoverage = K > 0` every output is
  written as FITS into `runs/run-<id>-uncropped/`; after the last group,
  `finish_crop` crops a co-registered run to ONE rectangle (the union of the
  groups' `coverage ≥ K` boxes) and a native run per group, writes the final
  files in the configured format, moves the rows, group paths and summary
  onto them, and removes the scratch. CRPIX moves by the crop origin (× the
  scale for drizzle outputs). A group with no rectangle, or whose crop fails,
  is finalized uncropped with a warning; one that cannot be finalized keeps
  its scratch files, which its rows name. `K = 0` is the old path; a
  full-rectangle finalization writes exactly what it writes (review I7). A
  crop does not re-measure `GroupStats`, and the plan's space estimate
  counts the scratch (review M6).
  - **A run that ends early.** On Cancelled or Err (including a panic) every
    group already written is finalized uncropped into the output folder, like
    a `K = 0` run, with one warning. A group that cannot be finalized, and a
    drizzle family whose finalization failed, keep their files in the
    scratch folder, named in a warning. A drizzle crop failure keeps only the
    drizzle family there.
  - **Healed rows may name the scratch.** A run healed after a kill (or one
    whose group could not be finalized) can leave `master_lights` rows that
    name `runs/run-<id>-uncropped/` paths. That is the scratch kept on
    purpose: it is the master's only copy until a manual action moves it.
  - **The crop pass reports progress and ignores a cancel.** Each group's
    finalization emits an Output-stage `stacking-progress` tick (throttled
    like every stage); a cancel that arrives during the pass is logged once
    (`"crop pass finished after a cancel"`) and the outputs are final.
  - **A record failure never leaves an orphan.** When a group's rows cannot
    be updated after its final files were written, those final files are
    removed again (the warning says so); the scratch stays and the rows keep
    naming it.
  - **The scratch folder is never cleaned.** `cleanup_work` removes every
    `runs/` entry under `All` except `run-*-uncropped`, and no level touches
    that folder. `heal_interrupted_runs` warns when a killed run left one.
    Only a manual delete removes it.
- **Output on the grid (D11).** The master's WCS is the grid's TAN
  (`grid_plate_solve`: the 0-based centre as `crpix`, no SIP).
  `stamp_grid_cards` writes the canonical `FILTER` (no card at all for a
  group whose canonical filter is empty: the reference's raw name must not
  leak, and the run warns once), `ROWORDER`, `ATH_RGEO = 'grid'`, `ATH_SPRJ`
  (with a comment only when it fits), `ATH_GRID` (16 hex), and drops
  `PLTSOLVD`; `ATH_PRJ` is never written. Masters land in
  `<output>/<sanitized title>/`. A project's output folder follows the
  frame-set rule: the Save-time overlap warning only, and `resolve_collision`
  means a master never overwrites a file. The plan's `folders` blocker covers
  the working root, not the output folder, so a master may land inside the
  Collaboration folder (a blocker can be added in 6c).
- **Starting (D13, D14, D16, D17).** `start_stacking` takes a project. The
  run's grid is the plan's confirmed one, recorded in `RunSummary.grid`.
  - The plan blocks a run certain to fail at Reference — a manual pin, or
    every in-scope frame, that will have no usable WCS — and warns when the
    pin's WCS waits on a solve. A steady-state plan writes nothing.
  - One predicate decides "usable WCS" for the plan and the run's tiers
    alike: a WCS whose SIP `ReferenceWcs::from_record` refuses counts as
    none, so a frame never reads as anchored in the plan and unanchored in
    the run. A separate warning names a WCS that cannot be used.
  - The plan reads the installed solver's identity ONCE and uses it for the
    solve counters and for that predicate, so a cached failure blocks only
    while it stands for this solver and catalog, and one plan never says
    both "could not be plate-solved" and "will be plate-solved". With no
    catalog installed an unsolved frame reads "could not be plate-solved".
  - `Intermediates` and `All` sweep `tmp/` for every subject, frame sets
    included (a directory of crash leftovers, no artifact rows). The
    working-usage figure does not count `tmp/`, so a cleanup can free more
    than the Settings readout shows (open-items).
  - `heal_interrupted_runs` runs from every project entry point (plan, runs,
    cleanup) as it does from the frame-set ones.
- **Commands (D12).** Seven on both hosts (strict count 306):
  `get_project_stacking_plan`, `start_project_stacking`,
  `get_project_stacking_runs` (NotFound for an unknown project),
  `get_project_stacking_config` (read only: the seed, `isDefault`, while
  nothing is stored), `set_project_stacking_config` (sorted unique trimmed
  exclusions; an absent `referenceFrameUuid` clears the pin; refuses a
  project this device left), `get_project_stacking_work_usage`,
  `cleanup_project_stacking_work`. Run-history frame rows carry `frameUuid`.
  - Cleanup heals, refuses with Conflict while a run is active, reads the
    stored config (an invalid one fails) and refuses a layout that overlaps a
    scan root (the Collaboration root is one). Every refusal comes before
    every write. Only then does it sweep the rows of frames that left the
    project, at every level and even with no working folder (then `Ok(0)`).
- **After a plate-solve or catalog change the plan is identity-blind.** The
  plan cannot predict a re-solve's WCS: Register may re-run while the plan
  shows it cached, and the "will be plate-solved" warning still appears.
  Only the Reference check judges with the installed identity.
- **Tests.** `api/stacking_project_e2e.rs` stacks a fake-hub project — two
  publishers, two canonical filters, a coordinator exclusion, a frame the run
  solves, a pair under 25 % overlap — twice. Every project run test holds
  `grid_counters::exclusive()`.
- **Open for 6c:** the project page's Stacking tab (the wire types are final),
  `/projects/<id>?tab=stacking` for the completion notification. The 25 %
  threshold stays synthetic until a real partial-overlap project exists
  (open-items).

**LN model fixes (2026-10-04 … 2026-10-06)** (spec
`docs/superpowers/specs/2026-10-04-ln-model-fixes-design.md`, plan
`docs/superpowers/plans/2026-10-05-ln-model-fixes-plan.md`, acceptance
`docs/superpowers/research/2026-10-05-ln-model-fixes-acceptance.md`, rulings
L-1…L-26 and P-1…P-21). One cycle, five items: the background model at the
scale, a scale derived from the image, a star selection for the scale, a
common-aperture scale estimator (F1) and a calibration pedestal warning. No
migration: stored runs, rows and `master_lights` are untouched; a set reaches
the new model through the normal cache invalidation (below).

- **The background model at the LN scale (item 1, L-1…L-6, P-1…P-3, P-9,
  P-19…P-21).** `stacking/ln/background.rs` no longer holds a robust level per
  128-px cell with a validity gate and a neighbour fill (retired, with
  `rejection_limit`, the 3.0σ/3.2σ deviation thresholds and the full-plane
  cleaned copy). One procedure, for the reference and every target alike:
  - **Tiles (L-2).** Non-overlapping 32-px tiles, every pixel used, cleaned
    tile by tile (hot-pixel pass and the `[4.5e-5, 0.85 × full scale]` clip
    unchanged, a 2-px apron so the result equals the full-plane pass). A tile
    with at least 256 present pixels is a sample: its value is the median of
    the pixels kept by `median ± 3.0 · 1.4826 · MAD` (at most 5 rounds), with
    its own σ.
  - **The surface (L-3, P-20).** A tensor cubic B-spline with knot spacing
    equal to the scale, penalized by a first-difference (membrane) penalty,
    `MEMBRANE_PENALTY_REL = 1e-2` times the median non-zero diagonal of
    `BᵀB`. The spec's `1e-4` was replaced at plan review (owner-confirmed
    2026-10-05): at `1e-4` a weakly supported coefficient amplifies sample
    noise to up to 6.8 % of sky in a gap, against 0.37 % for the old neighbour
    fill, so a project member's pixels outside the reference would have
    regressed. At `1e-2` every analytic node-noise column is at or below the
    old model (gap anywhere 0.37 %). The price is corner flattening (P-1): a
    10 %-of-sky gradient difference gains at most 0.13 % of sky at a corner
    node, so the linear-gradient pin asserts what the model can meet (every
    node within 2e-2 of the amplitude, nodes one knot interval inside within
    5e-4) rather than the spec's 1e-5. Accumulated in `f64` in tile order and
    solved by dense Cholesky, so it is bit-identical with and without a pool.
  - **Rejection rounds and the floor (L-4, P-9, P-21).** Two rounds: fit,
    reject samples beyond `3.0 · σ_e` of the residual MAD (two-sided, every
    sample re-judged each round), refit; the third fit is final. A residual
    MAD at the rounding level (`σ_e ≤ 1e-6 × |median|`) rejects nothing, and a
    round that would keep no sample rejects nothing. A knot cell is supported
    at 8 accepted samples; a plane is refused only when no cell is supported
    (a refusal is logged, never swallowed). There is no rejected-fraction
    gate.
  - **The shared refit (L-6).** After both own-threshold fits, the reference
    and target surfaces are refitted per pair on the intersection of their
    accepted samples: one design matrix, one `λ`, one factorization, two
    right-hand sides, no rejection round. It is symmetric, not the rejected
    "one mask from the reference". A pair with no supported cell on the
    intersection keeps its own-threshold surfaces with one `warn!`. Measured
    on IC 434: the surface difference of one frame against the reference fell
    from 1.66 % / 1.31 % of background (28- / 5-frame reference) to 0.23 % /
    0.20 %.
  - **Gaps and the project grid (L-5, P-3, P-19).** No fill step: the
    membrane continues the surface over uncovered tiles (the data's local
    trend for about one knot interval, then the edge level). A project
    member's reference model reuses the group reference's tiles when the tile
    and its apron lie inside the overlap, recomputes cut tiles and drops
    outside ones; the result equals recomputing every tile.
  - **Hashes.** `LN_MODEL_VERSION` (`ln/mod.rs`, folded into
    `normalization_subtree`) is 2 after this item; see the selection and the
    map for 3 and 4. The `.athln` format, `grid.rs` and `B = B_R − A·B_T` are
    unchanged. `LnArtifactPayload.cells_rejected` became `samples`,
    `samples_rejected`, `samples_shared`, `cells_unsupported`.
  - **Checkpoint A (2026-10-05, accepted by the owner after an independent
    review), Horse C11 G, scale 1536.** The halo rings at 60–70 / 80–90 /
    100–120 px read +0.07 / −0.04 / −0.03 points against the no-LN control
    (normalized read; `main` −11.4 / −9.1 / −6.2, and its raw 15.58 / 4.09 /
    1.09 % reproduces run 8's 15.5 / 4.1 / 1.1). B nodes within 600 px of the
    star: 0 of 840 out of line, worst 0.017 % (`main`: 252 of 1 932, worst
    35 %). The three IC 434 boxes agree with the control to +0.040 / −0.028 /
    +0.078 points. The scale path is bit-identical to `main` on set 204
    (197/197 sidecars) and run B (184/184). Normalize grew 2.21 against 1.88
    min on set 204 (+20 s; spec estimate +10 s). Three §11.3 rows missed as
    written (master median ±0.1 % of the LN reference, whole-frame MAD,
    FWHM on OSC blue) and the owner ruled them an effect of the item, a
    harness artifact and a pre-existing offset (see the integration-side
    open item in `open-items.md`); the median row reads branch against base
    from checkpoint B on.
- **The derived scale (item 2, L-7…L-10).** `scale = snap(min(W, H) /
  gridSize)` on the geometry the LN models live in (the registration
  reference, the group's own reference in native mode, the grid for a
  project). `normalization.local` gains `scaleMode` (`auto` | `fixed`,
  decoded as `Option`) and `gridSize` (default 4, 2–8); `scale` stays the
  fixed value. The supported list is `256, 384, 512, 768, 1024, 1536, 2048,
  3072, 4096, 6144, 8192`; `snap` takes the nearest, ties to the smaller.
  Horse C11 (9576 × 6388) resolves to 1536; set 204 (6248 × 4176) and run B
  (4480 × 5760) both stay at 1024.
  - **Legacy rule (L-9).** A stored document without `scaleMode` reads as
    **auto** when `scale == 1024` and **fixed** (snapped) otherwise, resolved
    at read time in `resolve_config`; nothing is rewritten.
  - **One function for plan and run (L-10).** `ln::resolve_scale` (`ln/
    scale_rule.rs`). The plan's geometry is the grid, the stored
    `ln_reference` artifact's own `width`/`height`, or a marked estimate
    (`lnScaleEstimated`); a run whose scale differs from the plan's estimate
    adds one run warning. The resolved scale is the hash token
    `ln_scale_px:<n>` on the per-frame `ln` and `ln_calibration` hashes only;
    the `ln_reference` hash carries none, and `scale`/`scaleMode`/`gridSize`
    left the subtree, so a scale change never rebuilds the reference image.
- **The star selection (item 3, L-11, L-12, L-14, L-23).** `ln/selection.rs`,
  applied to pairs on both estimators, both scale paths, both arms of the
  seeds calibration and the local-scale input:
  - **Relative saturation.** A pair fails if the largest pixel within
    `max(2, FWHM)` px of either centroid exceeds `saturation` (0.75) times the
    hot-pixel-corrected plane maximum.
  - **Peak SNR.** `(peak − b) / σ_local ≥ minPeakSnr` (40), `peak` the mean of
    the five brightest pixels in the fit's sampling region.
  - **The brightest.** Ranked by the reference fit's signal; the first
    `maxStars` (500) valid pairs are kept. The cap counts matched pairs, a
    deliberate deviation from the ERP's per-image cap.
  - **Fallback.** Fewer than `MIN_MATCHES` (20) pairs passing both tests: the
    brightest `max(⌈n/4⌉, 20)` of those passing saturation, logged
    `brightQuarter`.
  - **The reference-frames saturation map (L-23).** An integrated reference
    averages a core clipped in some frames with unclipped ones, so its own
    peak stays under the cut while its flux is short. The reference build
    records, per channel, `R(x) = max_k (p_k(x) / M_k)` over the integrated
    frames (`M_k` each frame's own hot-pixel-corrected maximum), and a pair
    also fails when `R > saturation` within `max(2, FWHM)` px of the
    reference centroid. It is written beside the reference as
    `ln/<group>/reference-saturation.fits` (float32), belongs to the
    `ln_reference` artifact, and the run rebuilds a reference whose map is
    missing, unreadable or of another shape (with a `warn!`); the plan stats
    neither file. In RAM only the 1-bit mask is kept, so a `saturation`
    change never rebuilds the float map. Measured by the Task 16 pre-check
    (set 204 mono): the integrated reference's plane maximum is 0.75 against
    0.84–0.86 for its 20 frames; 243 stars fail on the reference itself and
    374 in at least one frame; the median brightness-tercile trend of the
    selected 500 moved from −1.70 % to −0.52 %. Raising `maxStars` to 1000 or
    2000 left the trend at 1.05–1.60 %; the cause was the saturation, not the
    cap.
  - **`LN_MODEL_VERSION` is 4** (3 for the selection, 4 for the map); it moves
    the `ln`, `ln_reference` and `ln_calibration` hashes together, so every
    LN set re-normalizes once (reference, seeds calibration, every sidecar).
    `PSF_FIT_VERSION` stays 3.
- **F1 and the default (item 4, L-13, L-15, L-16, L-24).**
  - **The estimator.** `fluxEstimator: commonAperture`: both frames are
    measured in one circle of radius `a·F` (`F` the larger of the two median
    fitted FWHMs, whole pixels, each frame's own centroid) with sky the median
    of the present pixels in `[6F, 8F]`; a pair is valid when its circle is
    entirely present, at least half of each annulus is present and both fluxes
    are positive. The growth-curve aperture (`aperture: growthCurve`) takes the
    ratio at `a = 2` and multiplies it by `c`, the median over the first 100
    valid pairs of the 5F-to-2F growth ratio of the reference over the
    target's (`c = 1` with a `warn!` when no pair qualifies, P-10). The Moffat
    fit stays the finder and the centroider; β does not enter the flux.
  - **Checkpoint B (2026-10-06), per member against the photometric truth,
    on each run's own LN reference** (acceptance §8.0). Median gap / median
    |gap| / slope, F1 at the growth curve against PSF signal with selection:

    | Dataset | F1 | PSF signal with selection |
    | ---- | ---- | ---- |
    | set 204 mono (91) | −0.10 % / 0.27 % / −0.11 %/px² | −5.98 % / 6.09 % / −3.29 %/px² |
    | set 204 OSC (13 × 3) | −0.19 % / 0.44 % / −0.49 %/px² | −0.49 % / 1.13 % / −4.41 %/px² |
    | run B mono (82) | +0.08 % / 0.33 % / −0.07 %/px² | −7.47 % / 7.47 % / −3.41 %/px² |
    | run B OSC (101 × 3) | −0.13 % / 0.39 % / +0.58 %/px² | +0.03 % / 0.96 % / −4.85 %/px² |
    | Horse C11 G (28) | −0.07 % / 0.32 % / −0.04 %/px² | −2.68 % / 2.68 % / −0.58 %/px² |

  - **At master level** (set 204 mono, `--stack-sim`, no rejection): F1's
    master star flux is −0.17 % from the truth-corrected arm, PSF signal
    −4.16 %. Master SNR is higher under F1 on every plane of every dataset
    (set 204 +0.39 / +0.57 / +0.20 / +0.09 % against `lnA`; run B +2.21 /
    +0.42 / +0.09 / +0.08 %); rejected fraction moves −0.047 / −0.021 pp and
    the weights stay at ρ 1.0 (set 204) and bit-identical (run B). The seeds
    calibration `k` is 1.0001 on set 204 mono and 1.0000 / 1.0000 / 1.0002 on
    OSC (`lnA`: 0.9916; 0.9965 / 0.9969 / 0.9925).
  - **L-16 as written** (spec §4.4) held on items 2 and 4 and missed on items
    1 (run B OSC slope pooled over the planes, +0.58 %/px²) and 3 (the
    whole-frame MAD on mono, −2.37 % and −4.56 % scale-normalized).
  - **The ruling (owner, 2026-10-06, after an independent review, "да,
    согласен, погнали"; L-24).** F1 at the growth curve is the **default**
    (`LocalNormalizationConfig::default` and the two enums' `#[default]`);
    PSF signal stays a choice. `maxStars` stays 500. Math reference §4.3 step 3
    (the PSF-flux ratio) is overturned for the default path; the owner
    accepted that masters may differ from the ERP by a few percent in level
    where F1 is measurably closer to the truth.
  - **Kept by Plan B B3 (2026-10-23, R-PB3-1; R-OSP-16 B3).** Re-read
    against B2's referee on Plan A's fits and B4's `c`, F1 as is stays the
    default (767 real cells: median |gap| 0.40 % against psfSel's 1.32 %);
    the F1 v2 sky is out by its entry test. Nothing in the product changed.
  - **Stored documents.** A document without `fluxEstimator` or `aperture`
    reads the defaults and runs F1, so its sets re-normalize once (the default
    config's hash moved); a document naming `psfSignal` keeps it.
    `LN_MODEL_VERSION` stays 4. The subtree carries `aperture` only while
    `fluxEstimator = commonAperture`.
  - **What a user sees.** An LN master's star signal rises with the group's
    weighted-mean gap: +4.3 % on set 204 mono and +6.9 % on run B mono
    against PSF signal with selection; the OSC planes move by up to 0.6 %, so
    colour balance moves slightly.
- **The re-defined acceptance rows (L-25, owner 2026-10-06).** Read as
  written, three §11.3 rows cannot measure a change of the LN scale. No bar
  is re-sized.
  - **R1, master MAD.** The block-median field at the LN scale (blocks of
    `scale` px), read raw, must lie within ±1 % of the comparison arm: above
    the LN scale the background comes from the LN reference through `B`
    whatever the arm's `A`, so dividing it by a larger `k` shrinks it (block
    field raw +0.15 % set 204 mono, +0.12 % run B mono; normalized −3.39 % /
    −6.38 %). Noise per unit signal is the master-SNR row; the whole-frame MAD
    is reported, raw and normalized.
  - **R2, FWHM.** The yardstick is the no-rejection mixture of the run's own
    registered frames at their own scales; the master's star width must not
    exceed it by more than 1 % on the same stars. The Gaussian variance-mixture
    formula over-predicts the change about three-fold on real profiles and is
    reported only; a fit whose β differs between the two masters is a harness
    flag, not a reading (production `fwhmPx` jumped −6.3…+6.7 % on OSC planes
    where β flipped, the second-moment read was flat within ±0.02 %).
  - **R3, OSC slope.** Judged within each plane, centred, with its 90 %
    bootstrap interval; a plane whose half-max ΔS² lever arm is under 1 px² is
    "unresolved at this lever arm". Pooling is dropped: it adds the colour
    offset times the plane-width term (run B OSC pooled +0.58 %/px²,
    within-plane +0.21 %/px² [−0.10, +0.49]).
- **F1's known limitation (L-26).** Its `[6F, 8F]` median sky reads neighbour
  light on a dense field: the lab measured +0.9 %/px² at about 600 stars/Mpx
  and +1.6…+2.2 %/px² on crowded fields. This cycle's fields hold 5–40
  stars/Mpx and read at most +0.1 %/px². `ln_annulus_crowding` (the median,
  over the pairs the ratio uses, of the larger of the two frames' fractions of
  annulus pixels above the annulus median + 3σ; log-only, no hash) makes a
  dense field visible on the `"ln relative scale"` event. The masked far-annulus
  sky that would remove the effect is the measurement cycle's (open-items).
- **The pedestal warning (item 5, L-17, L-18, P-11, P-12).** A light of a
  non-attested frame set whose links include a Flat and neither a Dark nor a
  Bias gets `CALSTAT = F`; dividing `P + V·S` by a flat `V` yields `P/V + S`,
  so the offset pedestal grows where the optics vignette. One core function
  (`api::lights::flat_only_lights`) serves three callers: the stacking plan
  (one warning per group in `StackingPlan.warnings`), export/send readiness
  (`ExportReadiness.calibrationWarnings`, listed on the Export tab while the
  mode is Calibrated lights) and the run (one `RunSummary.warnings` line per
  group). Only the run measures the corner ratio `V`, once per distinct flat
  master: the median of each `c × c` corner patch (`c = max(16,
  round(0.05 · min(W, H)))`) over the flat's normalization divisor, the
  smallest of the four. Real data: the Horse C11 copy shows it in the plan
  ("OFFSET 1, GAIN 100"), the Export readiness (106 of 106 lights) and the run
  summary (L 50 %, G 51 %, so 2.0×). It never blocks, never writes, never
  changes an auto-link pick (pinned by a test that runs auto-link with the
  warning path on and off), enters no hash and moves no pixel; attested sets
  and project frames are out of its reach, and collab publish receives the
  field without showing it.
- **Not re-proposed / measured out.**
  - C-29 (LN background on a 4×4-binned plane) still applies: the tiles take a
    robust median of full-resolution pixels.
  - The plain tile median, without the in-tile clip, is the recorded second
    lever (it removes the noise-dependent bound from the tile statistic); not
    built because the IC 434 rows passed.
  - `λ = 1e-4`'s gap noise (P-20): do not return to the spec's constant.
  - An exact copy of the ERP's PSF-signal estimator was considered and
    declined by the owner (2026-10-06): the ERP is a comparison, never the
    reference and never a label.
  - Raising `maxStars` does not fix the brightness trend (L-23 does); the top
    is never capped.
- **The L-20 ruling.** The Horse C11 ERP-slope row (the per-frame scale over
  the ERP's, against PSF variance) is reported, not gated: its premise was
  ERP faithfulness, and F1 is not an imitation of the ERP. Measured: PSF
  signal with selection −0.48 %/px² (r −0.85), F1 +0.07 %/px² (r +0.06; the
  truth over the ERP +0.11).
- **The harness additions** (athenaeum-lab `harness/` and `examples/`;
  reuse, never rebuild).
  - **`ln_scale_probe`** calls the production selection and both production
    estimators and writes per-member scales for five arms (`psfOff`,
    `psfSel`, `f1_2`, `f1_3`, `f1_gc`) with the gap by brightness tercile and
    `annulusCrowding`. `--ref-saturation <fits>` reads a reference's map;
    `--build-ref-saturation <out.fits> <registered.fits>...` builds one with the
    library function the run uses; `--stack-sim <config.json>` combines the
    whole group's LN-normalized members, one arm per estimator and variant,
    plus the truth-corrected arm and every arm without rejection.
  - **Scripts:** `ln-model/aperture_choice.py` (probe bars, `--sim`),
    `ln-model/nebula_boxes.py` (fixed boxes, chosen on the no-LN control
    before any LN master is read), `ln-model/star_blocks.py`,
    `ln-scale-audit/master_phot.py --noise` (star flux in LN-reference units
    `k` and MRS σ, one script for the normalized read and the SNR row), and
    `tier1-compare.py --ln-grids` (the B move and the `A` / `global_scale`
    bit-identity), with a `beta` line beside the FWHM so a β flip is visible.
  - **The probe's arms model only the detection path.** A run normally takes
    the seeds path with the seeds-calibration `k`, so read `k` (≈ 1 under F1,
    acceptance §8.2) as the probe-to-run link: on the same reference and
    frames the probe's per-member scale equalled the run's `.athln` scale to
    0.03–0.05 % (set 204 mono), ≤ 0.02 % median (run B) and +0.07 % median
    (Horse C11 G).
- **Tests.** The pre-checkpoint-B full gate at `d5f26b7e` was green: core
  all targets 3737 passed / 0 failed, web 51, plus tauri, headless,
  `src/types`, tsc and vitest (acceptance §8). The final full core run
  (`cargo test -p athenaeum-core --all-targets`) is owed before any push.

**Key files**: `crates/athenaeum-core/src/stacking/{config,groups,paths,
plan,run,provenance,measure,weights,psf_signal,prefilter,robust,structure,
integrate,master_cards}.rs`,
`stacking/register/{mod,detect,align,frame,wcs_seed,local_loop,writer}.rs`,
`stacking/ln/{mod,grid,background,scale,reference,selection,aperture,scale_rule,calibration}.rs`,
`stacking/drizzle/{mod,geom}.rs`, `stacking/rej.rs`,
`stacking/{origin,project_source,prepare,solve,grid_map,crop}.rs`,
`collab/{grid,grid_geometry,frame_meta}.rs`, `api/collab_grid.rs`,
`plate_solve/{service,layers}.rs`,
`crates/athenaeum-core/src/integration/student_t.rs`,
`crates/athenaeum-core/src/geometry/tps.rs`,
`crates/athenaeum-core/src/api/stacking.rs` (also `list_stacking_presets`/
`save_stacking_preset`/`delete_stacking_preset`, M4d Task 4),
`crates/athenaeum-core/src/api/files.rs` (`render_preview_from_path`/
`preview_step`, M4d Task 3), `crates/athenaeum-core/src/db/stacking.rs`
(`master_lights` rows, M4d Task 3), `crates/athenaeum-core/src/
fits_writer/{wcs,xisf_writer}.rs`; dev probes
`examples/{measure,register,integrate,ln}_probe.rs` and the weight-audit
harness `examples/weight_audit.rs` (+ athenaeum-lab `harness/
weight_audit_compare.py`).
Frontend: `src/components/stacking/` (above),
`src/hooks/useStackingRuns.ts`, `src/contexts/StackingContext.tsx`.

## One star-measurement path, Plan A

Spec `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md`; plan `docs/superpowers/plans/` (Plan A).

- **R-OSP-1 - `star_measure::MEASURE_VERSION = 4` replaces `psf_signal::PSF_FIT_VERSION` (3).** The constant lives
  in `crates/athenaeum-core/src/star_measure/mod.rs` with versions 1-3 recorded as the old constant's history. It is
  folded as `"measureVersion"` (formerly `"psfFitVersion"`) into the three fit-reading stage subtrees only:
  `measurement_subtree`, `registration_subtree` and `normalization_subtree` (helpers `*_with_measure_version`).
  `calibration_subtree` and the prepare subtree do not carry it, so a bump never recalibrates.
- **Policy.** ONE bump per merged plan that changes what a fit accepts or measures, never one per task. Plan A is 4;
  Plan B is 5 (B4: `c`'s point-source screen and SNR floor, R-OSP-16 B4); Plan C (the Analysis switch) is 7 (6, Blend on every
  path, was burnt by R-PC-10b: its stacking artifacts carry Blend numbers, never released).
  `AnalysisConfig::config_hash` is Plan C's. The `MEASURE_VERSION` doc in `star_measure/mod.rs` states the same.
- **Risk.** The first run after Plan A re-measures, re-registers (mono) and re-normalizes every set; the `calib`
  column of the stage-hash golden literals is unchanged, every other column moved
  (`stage_hashes_are_pinned_to_golden_literals`). Task 11b adds a once-only ripple of its own: every `Auto` set
  re-measures, re-registers and re-normalizes once on its first run after the group β lands (R-OSP-9, review I3).
- **Logging.** The moved measure-path warnings now log under the tracing target `star_measure` rather than
  `stacking`; their `clip` and `samples` fields joined the logging dictionary
  (`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`).
- **F-0 - every seed has an outcome (Task 4; numbers unchanged).** `star_measure::diagnostics` holds
  `RefusalReason` (twelve camelCase names with `defocused`, `outsideImage` … `duplicate`), `Outcome` (`Accepted { star }`,
  `Refused(reason)`, and the Budget mode's `NotSelected` / `Trimmed` from Task 12), the per-seed `FitDiagnostics`
  and `RefusalCounts`. The identity `accepted + refused + not_selected + trimmed = seeds` (ruling F13) is a
  `debug_assert!` in the fit and in `measure_plane`, a test, and the lab's `fit outcomes` row.
  - **Order (M-4).** `fit_one` returns `Result<StarFit, RefusalReason>` plus its trace. The outcomes come off the
    same `seeds.par_iter().map_init(FitScratch::default, …)` chain, collected per seed in seed order. The accepted
    fits are read off it sequentially and handed to `dedupe`, so its input, its stable sort and the Kahan sums see
    exactly the order they saw before. A fit the ±1 px drop removes is refused `duplicate`.
  - **Today's reasons.** `outsideImage`; fewer than 10 finite region pixels →
    `tooFewPixels`; the fitter returns `None` → `fitFailed`. The first acceptance gate is still one condition; its
    reason is `nonFinite` (a parameter or the residual), else `residualTooLarge`, else `fitFailed` (`A ≤ 0` or an
    `α ≤ 0`). The seed outside the fit's half-maximum contour (ruling A4-2; before it a fixed 1.5 px centroid tolerance) → `centreMoved`; the inner region → `centreAtRim`; half the FWTM beyond the
    region radius → `regionTooSmall`. `donut`, `narrowerThanPixel` and `defocused` are produced from Task 6b (R-OSP-4).
  - **Where the counts land.** `FitOutcome.refused` and `PlaneMeasurement.refused` always carry the counts.
    `ChannelMeasurement` gains `stars_refused` and `refused_by_reason` (keys = the camelCase names), so the
    `metrics` payload grows two keys; both are serde defaults, so an older payload decodes as `0` / `{}`. The
    per-seed vector is kept only when `star_measure::MeasureOptions.diagnostics` is set: the run never sets it (it
    costs memory on 24 576 seeds). The adapter's `measure_plane_diagnosed` hands it to `star_probe --diagnostics`,
    which writes one `refused` line per refused seed; every measure and betas frame line carries `refused` counts.
  - **The `.athf` flags byte.** The reserved byte is now `StarFit.flags: StarFlags` (bit 0 `SATURATED`, bit 1
    `BETA_AT_CAP`; set from Tasks 8 / 9, so every fit reads none today). A file written before this task holds 0
    there and reads as no flags; the layout and `FITS_ARTIFACT_VERSION` are unchanged.
  - **Acceptance.** The lab step `t04-f0`, recorded in athenaeum-lab `star-lab/results/plan-a/t04-f0/`. The bar:
    `fit outcomes` ok on every image, and 0 moved rows against `t02-extract` (`noreg.txt`) and against
    `t01c-baseline` (`noreg-cumulative.txt`). `fit outcomes` was ok on all 885 rows (189 of 189 images per path).
    - **The donut ruling.** The `donut outcome` row (row 1, path `measure`, `donut/r6` and `donut/r10`) moved by
      design: seeds that were unaccounted are now refused with a reason. On `donut/r10` that is 56 `centreMoved` +
      13 `regionTooSmall`; on `donut/r6`, 61 `regionTooSmall`. The accepted counts are unchanged (0 / 69 and
      24 / 85).
    - By controller ruling the row is a `reported` design entry,
      `{"row":1,"metric":"donut outcome","family":"donut/*","path":"measure","kind":"reported"}`; it is judged
      at Task 6b. With it both diffs PASS (0 moved, 0 lost, control 0 mismatch). Without it, they read FAIL on
      exactly those two rows.
- **F-2 - canonical axes and a conditioned fit (Task 5; numbers move by design).**
  - **Canonical axes.** `accept` reports the major axis on `fwhm_x`/`fwtm_x` and `theta` on that axis, in
    `[-π/2, π/2)` (`canonical_theta`). The fitter's first axis was whichever its start made it, so the same star
    could read (major, minor, θ) or (minor, major, θ ± 90°). Both forms are the same ellipse. The gates, the
    aperture, `fwhm()`, `eccentricity()` and every reader outside the module are symmetric in the swap:
    - the register σ pairs, which every consumer reduces to `σx² + σy²`;
    - LN `to_seed`'s `σx·σy`;
    - the `ln_scale_probe` ellipse.
    An `.athf` written before keeps the fitter's order; it is stale by `MEASURE_VERSION`.
  - **The conditioned fit.** `fit_one` hands the fitter the stamp centred on `(cx, cy)`, with values
    `(v − b0)/a0`. The fit starts at `B = 0`, `A = 1`, `x0 = seed.x − cx`, and the result is mapped back
    (`x0 + cx`, `B·a0 + b0`, `A·a0`). `fit_residual` was amplitude-normalised already. `conv_tol` and the cost
    test keep their values; the step-size test now sees O(1) parameters on every star instead of a norm
    dominated by the position or the sky level.
  - **R-OSP-3 - the rustafits change fired on unit-level numbers (controller ruling, Task 5).** The Moffat
    solver's Jacobian built its θ column as `2uv·(1/αy² − 1/αx²)`. That is the negative of
    `∂Q/∂θ = 2uv·(1/αx² − 1/αy²)` (`∂u/∂θ = v`, `∂v/∂θ = −u`). Its θ steps went uphill: once the other
    parameters had compensated, every step was rejected and the fit stopped away from the minimum. The deciding
    numbers, noiseless and measured before any lab run:
    - **The canonical-axes fixture** (αx 3, αy 6, θ 20°, β 4): θ −73.83° against the true −70° on every star,
      not converged, residual 7.2e-3. Corrected: −70.0000°, αx/αy 6.00000/3.00000, residual 2.8e-7 (the f32
      floor), converged in 5 iterations.
    - **A 31×31 stamp** (αx 2.5, αy 4, θ 30°, β 4) started at θ 35°: 44.84°, not converged; the fit walks away
      from the truth. Corrected: 30.0000°.
    - **The remedy is the column.** Tighter tolerances, 1000 iterations or 50 rejects end at the same wrong θ,
      so the plan's per-parameter convergence test was not added. It becomes a second, recorded change only if
      the lab bar misses with the corrected column.
    - **The rustafits change.** It is on branch `one-star-path-beta` (from 8321412, commit 197fd93). The new
      public `fit_moffat_2d_fixed_beta_conditioned` takes the corrected column through a private `ThetaColumn`
      option. The option is threaded into the shared solver and the frozen C3 reference; the solver is not
      duplicated.
    - **The legacy entry points.** `fit_moffat_2d` and `fit_moffat_2d_fixed_beta` keep the legacy column and
      are output-identical to 1.3.0, pinned bit for bit (`the_legacy_entry_points_are_output_identical_to_1_3_0`).
      Ruling F7: the legacy analysis path, rustafits `analysis::metrics`, is the lab control through Plan A.
      `fit_one` calls the new entry point.
    - **Push order (M-7).** rustafits `one-star-path-beta` goes to both remotes before athenaeum `main`.
  - **R-OSP-4 - the Task 6b fit rules (controller and owner rulings, 2026-10-08; fix wave A after the task review).**
    - **The A-g width start (R1).** A star starts from its own half-maximum width (`own_halfmax_width`) when it is
      readable, else from the field width, clamped to [0.6, 40]. Readable = the width-start box maximum minus the rim median is at least 10 x 1.4826 MAD of the width-start box's
      rim; the gate reads the box maximum, kept by measurement (t06b-m2-check, 2026-10-08), and the own-peak alternative
      (the maximum inside the `HALFMAX_PEAK_SEARCH_PX` window) was measured worse: 547 fits lost, same-star fwhm +0.3 % worse. Chosen
      on a truth criterion, before any bar was read.
    - **The PA-12 region and rim b0.** The width-start box has radius ceil(2 x F_field); b0 is the median of its
      outer 1-px rim, and the fit's B is read relative to it.
    - **The PA-18 box admission.** A star whose width-start box leaves the image is refused `outsideImage`.
    - **The D start shape (R9).** The start axis ratio and angle come from the second moments inside PA-12's
      starting square, radius max(3, ceil(1.5 x F_start)), above b0.
    - **The donut test (R7, R8).** Centred on a local iterated centroid of max(v - b0, 0) (window max(1.5, F_start),
      at most 3 iterations, stop under 0.1 px); ring radius max(1.5, sqrt(sigma_major x sigma_minor)) from the box
      moments about that centre; ring (R - 1/2, R + 1/2], core 3 x 3; refused when core < 0.8 x ring. At a pixel
      centre the cross neighbours (d = 1) are not in the ring; an offset of 1e-12 px puts one in (the decision is
      unharmed: every ratio is well clear of 0.8).
    - **`NarrowerThanPixel`.** A fit narrower than a pixel is refused with that reason.
    - **The defocused-frame verdict, measure path only (owner ruling 2026-10-08: "defocused stars are not stars").**
      Rule: with `n_read` the plane's readable seeds (the width start's gate passed) and `n_donut` those refused
      `Donut`, `n_donut >= 5` and `n_donut >= 0.25 * n_read` make the plane DEFOCUSED: every fit it would have accepted
      is refused `Defocused`, the per-seed `Donut` refusals keep theirs, `FitOutcome.defocused` /
      `PlaneMeasurement.defocused` are set and the plane has no fits (the existing zero-star path).
      Measure path only: `FitStarsOptions.defocus_verdict` (default `false`) is set by `measure_plane` alone. LN's
      refits (`fit_stars_with_beta`, `fit_stars_at`, `fit_stars`) keep the per-star `Donut` refusal and never get the
      plane rule, because LN sees at most 200 seeds on a warped plane and could condemn a frame Measure keeps.
      One `warn!` per plane from the adapter (`outcome = "defocused"`, `count = n_donut`, `stars_detected`, `path`,
      `plane`). Checked on the round-3b probe outputs before it was coded: `donut/r6` 82 of 87 readable seeds
      (94.3 %), `donut/r10` 61 of 61; every other lab frame 0.1 % or less (the worst, `density/d1000`, 2 of 2741).
    - **Owner rulings of 2026-10-08.** (a) Raw-mode completeness is judged on NON-edge injected stars with Fisher SNR
      >= 20; edge stars are listed beside it as a record (refused `outsideImage`). (b) `background/neb3_4096` moves to
      Tasks 8/9 (frame beta 10 on Moffat-4 stars; a perfect flat sky model would carry +0.005 sigma) and is not a 6b
      STOP. (c) Defocused stars are not stars (the verdict above).
    - **Carried.**
      - The drizzle model bias, 5-8 % low, which predates 6b: to Task 9 and the real-data checkpoint.
      - psfSel's auto-beta drift, 1.25 -> 1.30 %: to Tasks 8/9 and Plan B.
      - PA-12's bright-star region, which stops at about 2 FWHM with 0.5-3.8 sigma of wing at the rim: to Task 8.
      - Same-star at t06b vs t06a: fwhm at Fisher 40-100 0.03179 -> 0.03300, to Tasks 8/9.
      - **Owner ruling 2026-10-08: ecc above Fisher 300 is moved to Task 8**, which must bring same-star ecc >300
        back to <= t06a (sign rule). Measure 0.08979 -> 0.09084 (up 28691 / down 27304, 2 sqrt 473); betas@10
        0.09024 -> 0.09114 and betas@6 0.09219 -> 0.09293 (betas@2.5 at Fisher 20-40 0.35746 -> 0.35696); every
        other bin NOT WORSE. The row 4 ecc design entries (measure and betas@*) stay set aside for this reason. Row 4
        angle stays set aside on the lab's `angle-same-star.txt` (24 shape families, same in every Fisher bin; the
        measure code is unchanged since round 3); row 2 centroid (measure and register) is re-listed for Task 8 (same-star NOT WORSE in every bin).
      - Parked 6a review minors: M1 and M2 (mask coverage) to Plan C; M5, M7, M8 and M9 to the final review.
      - M7 (task review): the defocused rule's false-positive rate on real frames is unmeasured; count `defocused`
        frames on real data at Task 13.
    - **Acceptance: t06b-f1 final run (athenaeum 1100f0f5, athenaeum-lab 9d893a3a; results in `star-lab/results/plan-a/t06b-f1/`).**
      697/697 probes proven, 53 790 scorecard rows, 0 missing; inject 90/90 proven (2 826 rows). Bars PASS, to the
      owner as ruled: R2 F-3 cells (`sharp/m4` f2.0 +2.96 %, f12.0 +1.21 / +1.14 / +1.20 %, f16.0 +1.76 / +1.47 %;
      unsaturated-only +2.62 % and within 0.04 % for the rest), `sharp/gauss` F-4 (-1.4 to -2.6 %), `neb3_4096`
      bias +1.889 sigma to Tasks 8/9; raw mode 26 edge stars recorded, not counted. With the final 32-entry design
      list (owner rulings above; axis order, false detections and bright coverage set aside by population, +15 383
      truth-matched stars): noreg vs t06a-seeds PASS and noreg-cumulative vs t01c-baseline PASS, both 0 moved, 0
      one-sided missed, 0 lost, control 0. Same-star (measure, pooled): fwhm BETTER, ecc BETTER, centroid BETTER;
      WORSE bins fwhm 40-100 and ecc >300, both carried above.
    - **Earlier lab cycle (2026-10-08, athenaeum c2a7eb60), before fix wave A.**
      Results in athenaeum-lab `star-lab/results/plan-a/t06b-f1/`: 697/697 probes proven, 53 712 scorecard rows, 0 missing; inject 90/90 proven.
      `lab_cycle` bars line PASS (with the owner and moved cells below); noreg vs t06a PASS and noreg-cumulative vs
      t01c PASS (0 moved, 0 lost, control 0) with the 32-entry design list (R4's population and fit-change groups
      named with their mechanism). Numbers: fitted / seeded at FWHM 8 = 95.6 / 96.0 = 99.6 % (baseline 89.9 %); donut
      0 accepted on `donut/r6` and `donut/r10` (defocused 50 / 11, donut 86 / 77) and no plane outside `donut/*` is
      defocused (956 frame lines); F-7 background bias neb0.1 -0.087, neb0.5 -0.211, neb3 +0.146 sigma, flat -0.054
      sigma; raw mode 100.0 % of the non-edge Fisher >= 20 injected stars on every `inject/raw/*` and `inject/mosaic/*`
      plane (18 planes); frame FWHM within 1 % on every `sharp/m4` cell except the owner cells below.
      To the owner / later tasks: R2's cells go to F-3 (Task 8): `f2.0` i0 (+2.96 %), `f12.0` i0-2 (+1.14 to
      +1.21 %), `f16.0` i0-1 (+1.47, +1.76 %), unsaturated-only +2.62 % and within 0.04 % for the rest;
      `sharp/gauss/*` (-1.4 to -2.6 %) go to F-4.
  - **Moved pins (by design).** Each literal carries its previous value.
    - `the_shipped_none_path_measures_exactly_these_numbers`: `psf_signal_weight` and `fwhm_px`.
    - `measure_plane_with_seeds_pinned_before_task_3b_rewrite`: `fwhm_px`, `eccentricity`, `tflux`,
      `tmean_flux`, `psf_signal_weight` and `psf_snr`.
    - The counts, β and every noise/sky field are unchanged. The module-vs-adapter equality pins stay
      equalities.
  - **Acceptance.** The lab step `t05-f2` is recorded in athenaeum-lab `star-lab/results/plan-a/t05-f2/` (commit
    62dacd9).
    - **The bar holds.** Row 4 `angle` `eff` against the prepixel CRLB is 0.82–1.31 on all 24 `shape` families
      with e ≥ 0.3 (t04: 3.40–9.76; `shape/f2.6/e0.3/t30` rms 44.56° → 25.61°, `e0.5/t30` 32.34° → 11.88°).
      `axis order` is 100 % on every family, path `measure` (77 978 fits, 0 minor-first). Path-compare part 2
      reads `fwhm_x < fwhm_y` on 0 of N on every shape family (baseline 71–145 on e0.3). The legacy analysis
      control is bit-identical.
    - **Attribution.** Two diagnostic arms ran on local scratch branches from 7658d698 and were never merged:
      - the conditioning with the legacy column: 307 moved, 35 missed, every one a last-bit change (≤ 1.1e-13)
        but one `betas@10` image;
      - the corrected column without the conditioning: 11 185 moved, 1 523 missed;
      - full F-2 for comparison: 11 076 moved, 1 521 missed.

      The corrected θ column therefore owns every moved and missed group. The conditioning is a near no-op on
      today's fits, because the legacy fits stopped on the stall, not on the step-size test. The canonical axes
      only relabel; they account for the first part of the angle gain, 3.40–9.76 → 2.37–7.95.
    - **Owner decision, 2026-10-08:** the corrected column is accepted. Its degradations are a tracked debt.
    - **The design list.** It was amended by controller ruling (`design.json`, 42 entries, each new one noted as
      the corrected θ column's). It sets aside row 4 `ecc` (every family) and `floor`, rows 3 and 5, row 2
      `centroid` (measure and register), row 6 `captured fraction`, row 8, and `kernel growth` on
      `resampled/f3.0`/betas@10. It reports the row 1 and row 10 counts. With it both no-regression diffs PASS
      against t04 and t01c: 0 moved, 0 missed, 0 lost, control 0.
    - **The round-star ecc floor** is accepted as the free-θ noise floor. On `background/flat`, `median_e` rises
      from 0.1107 to 0.1255, beside the ERP's free-θ Moffat value of 0.1279.
    - **The debt.** athenaeum-lab `bin/debt_check.py` checks `results/plan-a/debt.json`, groups that are judged
      against t01c every time:
      - row 3 `fwhm` `bias` and `scatter`, on `pointfit` only (spec D-2; prepixel and pixbox are printed, never
        judged);
      - row 5 `background` `bias`;
      - row 2 `centroid` `bias_x`/`bias_y` and `scatter_x`/`scatter_y`, on measure and on register.

      Each group is judged by the final rule. Values are |value| for a bias key. Over every (row, key) of the
      group, the rows where t05 is worse than t01c by any amount (up, exact ties excluded) may exceed the rows
      where it is better (down) by no more than 2·√(up + down). The same two-sigma sign test then applies to the
      judgments beyond the row's SE (worse against better). The pooled medians are printed, never judged. The
      target is every group PASS by the end of Plan A, checked at Tasks 6b, 9 and 14. The starting debt at t05
      (`t05-f2/debt.txt`, athenaeum-lab 4a35b05):

      | group | up / down | worse / better | pooled median t01c → t05 | verdict |
      | ---- | ---- | ---- | ---- | ---- |
      | fwhm bias (measure, pointfit) | 446 / 483 | 96 / 69 | 0.01045 → 0.00969 | OPEN (beyond SE: 27 > 25.7) |
      | fwhm scatter (measure, pointfit) | 434 / 473 | 68 / 98 | 0.01552 → 0.01553 | PASS |
      | background bias (measure) | 574 / 351 | 141 / 36 | 0.0502 → 0.0563 | OPEN (both: 223 > 60.8, 105 > 26.6) |
      | centroid bias (measure) | 943 / 932 | 33 / 34 | 0.00175 → 0.00170 | PASS |
      | centroid scatter (measure) | 921 / 913 | 42 / 48 | 0.0237 → 0.0239 | PASS |
      | centroid bias (register) | 650 / 656 | 22 / 20 | 0.00128 → 0.00136 | PASS |
      | centroid scatter (register) | 618 / 676 | 40 / 33 | 0.0181 → 0.0179 | PASS |
- **F-1 seeds - the matched filter, the mask, the field width (Task 6a; no fit change, ruling F16).**
  - **The detector** (`star_measure::seeds`). A two-pass Gaussian matched filter on the plane minus its mesh
    background (the 128-px model). Every absent pixel (non-finite, or `false` in the pixel mask) counts zero and leaves
    the normalisation: `f = Σk·d/√Σk²` is the filter's signal in units of the pixel σ, with the same noise wherever data
    is present, so a registered frame's NaN border neither seeds nor dims its neighbours; `flux = 2πσ_F²·Σk·d/Σk²` is
    the least-squares Gaussian flux, the selection key. Candidates are the local maxima of `f` above `K·σ_f` (σ_f =
    1.4826·MAD over the stratified sample), placed by their flux-weighted centroid, sorted by flux.
  - **The field width (PA-17).** The first pass (FWHM 3 px) gives its 100 brightest candidates; the median of their
    azimuthal half-maximum widths (rings of 0.5 px, the box rim's median as background, every candidate, no
    saturation test) clamped to [1.5, 24] px is the field width; the second pass runs at it, on the 2×2 super-pixel
    plane from 6 px on. `field_width_of` gives seeds handed in by another detector the same width. No fit reads it
    before Task 6b; it is reported as `PlaneMeasurement.field_width` and `FitOutcome.field_width`.
    *Plan A Task 10b phase A2 (F-10, ruling A5; 2026-10-11):* the field width is the HALF-SAMPLE MODE of the
    half-maximum reads of EVERY readable candidate (`field_width_of`: every given seed, unranked) — readable = the
    read's brightest pixel at least 10 σ_rim above the read's own box rim, the width start's level (Task 6b R1) —
    clamped to [1.5, 24]; no count, no saturation test (F25 stands). Every contaminant of the read is wider than the
    PSF (a clipped core is read at half the clip; a blend, a knot or a sky bump is a broader profile) while the stars
    share one width, so the PSF is the reads' mode. Design run `t10b-a2-design` (reference: the frame's good stars'
    reads at read SNR ≥ 50): the median of the 100 brightest read 1.04–4.43× (M42 4.0–4.4, LDN 1272 3.3, L 0000 2.0;
    lab up to 1.48, 12 cells beyond 10 %), a near-clip exclusion 1.02–1.24, the mode 0.92–1.02 (none). Recorded cost:
    the SNR 10–20 tier reads narrow, so the mode sits 1–8 % under the bright reads on real frames. PA-17 in the plan
    carries the full table. Proof (dev Mac, A1 head vs A2, task-10b-report-A2): F_field M42 8.2–8.8 → 1.7–1.95, LDN
    1272 mono 6.6 → 2.0, L 0000 10.8 → 5.1; M42 injected found 46/49/50 → 57/62/60, L 0000 51 → 59 of 61; the lab
    families' β and frame FWHM unchanged, `density/d1000` and `doubles/f2.6` recall up. Open for the controller: at an
    accurate width A1's `extended` test flags far more real stars (LDN mono 128 → 6381 seeds) and LDN's frame β falls
    to the 1.5 floor (2.00 → 1.50; still 1.50 with the width forced to the fit's 2.23 or 2.45).
    *Plan A Task 10b phase A2b (F-10, rulings A2-1 to A2-3; 2026-10-11):* the knot tests' bounds sit at the field's
    star-width ENVELOPE F_env = F_field + 3.09·s, not at F_field (the mode: half the real stars are broader). `s` is the
    TRUE spread of the stars' widths (`WidthEnvelope`, E4n): 1.4826 × the median of F_field − w over the readable reads
    with 0.715·F_field ≤ w ≤ F_field (the lower half only, because every contaminant is wider; the floor excludes
    single-pixel events, which read about 1.3 px), then the reads' own error removed in quadrature, s² = max(0,
    s_measured² − mean (F_field·e(ν_i))²), e(ν) = √((0.899/ν)² + 0.0208²) (fitted on 12 constant-width lab families).
    The knot test's 3.09·σ_r already carries each seed's noise, so the envelope must be the true spread: on stars of one
    width E4n reads F_env/F = 1.00, where the uncorrected spread invented 1.08–1.11. No readable read in the band: s = 0
    with one `warn!`. `measure` frame lines carry `fieldWidth`, `fieldWidthSpread` (s) and `fieldWidthEnvelope` (F_env).
    Proof (release probe, HEAD vs E4n): F_env/F G 1.41/1.36/1.29, D0099 1.35, L 0000/0008 1.21/1.30, LDN mono 1.32, M42
    1.15/1.12/1.07; extended fits LDN mono 6152 → 227, L 0000 257 → 48, M 0000 1687 → 210; β-sample stars kept (of 64)
    LDN mono 35 → 50, L 0000 40 → 60, M 0000 52 → 58; 22 clean lab families 0 move > 1 SE; the knot fixtures' bins
    over the allowance and pooled r ≥ 2 passes equal HEAD's (F_env/F 1.00 on constant-width knots). LDN OSC plane 1
    (F at its 1.5 px clamp, reads compressed by the clamp, so the read law does not hold) keeps s = 0. LDN mono's β stays
    1.50 (its β pool holds the near-clip stars: Task 10c's class).
  - **The pixel mask.** `MeasureOptions.mask` / `FitStarsOptions.mask` (`false` = absent, like a non-finite pixel) in
    the detector, the field width, the fit's region and aperture (`FitDiagnostics.pixels_used` counts the region's
    present pixels). The noise model and the sky statistic read the whole plane (a Plan C note). A plane with no
    noise estimate (no MRS, no positive `N*`) reports `NoiseSource::None` (`none` in the `metrics` payload, F43) and
    the detector seeds nothing, with one `warn!`.
  - **The hot-pixel rule's defect and its ruling.** The plan's rule asked a candidate for ≥ 2 of 4 cross-neighbours
    above `background + K·σ_plane` on the unfiltered plane. A matched-filter detection at `K·σ_f` puts only
    ≈ 0.16–0.27·K σ on each unfiltered pixel, so that rule passed a star only near a Fisher SNR of 3K–6K, refused the
    noise peaks for the same reason, and became the threshold in place of K (PA-2's sweep would have read 0 false
    detections at every K). Measured at K 4 (path `seeds`, expected set Fisher SNR ≥ 4):

    | family | completeness, the plan's rule | completeness, capped | `nothing` / Mpx, capped |
    | ---- | ---- | ---- | ---- |
    | `twins/sharp/m4/f2.6` | 84.7 % of 1789 | 98.8 % | 9.06 |
    | `twins/sharp/m4/f8.0` | 74.2 % of 1803 | 94.6 % | 0.00 |
    | `artefacts` | 80.9 % of 580 | 95.2 % | 10.01 |

    Hot pixels (0.24 per Mpx) and cosmic rays (10.01 per Mpx) are seeded at the same rate under both. Controller
    ruling: the level is `background + min(K·σ_plane, 0.15·(c − background))`, `c` the candidate's own pixel; 0.15 is
    geometry (the cross-neighbour/peak ratio of a pixel-integrated Gaussian of FWHM 1.0 px is 0.157), fitted to no bar,
    and a bright candidate (`c ≥ K·σ_plane/0.15`) sees the plan's rule unchanged. Under a checkerboard mask the
    diagonal neighbours stand in. Pinned by `a_broad_star_at_fisher_snr_20_passes_the_neighbour_test` (FWHM 8, Fisher
    SNR 20: 6 of 20 passed the plan's level, 20 of 20 the capped one) and `hot_pixel_is_not_seeded`.
  - **R-OSP-2 - the threshold, `seeds::DEFAULT_THRESHOLD = 6.0`** (PA-2, ruling F37), chosen before any fitted bar
    was read: the smallest K of {3.0, 3.5, 4.0, 4.5, 5.0, 6.0} at which every `twins/*` family (46, run whole) and
    `artefacts` read ≤ 0.5 false detections per Mpx of cause `nothing` on the `seeds` path. The twins enter row 1
    through the scorecard's opt-in `--twins-detection` (controller ruling; off by default, a scorecard without it is
    byte-identical to one before it). The data: athenaeum-lab `star-lab/results/plan-a/t06a-seeds/threshold-choice.txt`
    (lab c02e39e; star_probe 7e3289d7 built from athenaeum 71cac35c; 307 invocations per K, 307/307 proven).

    | K | deciding cells over 0.5 (of 47) | worst deciding cell, `nothing` / Mpx | `background/flat` (reported) |
    | ---- | ---- | ---- | ---- |
    | 3.0 | 45 | `twins/sharp/gauss/f1.2` 1001.8 | 258.9 |
    | 3.5 | 41 | `twins/sharp/m10/f1.2` 196.8 | 47.9 |
    | 4.0 | 40 | `twins/sharp/m10/f1.2` 32.2 | 4.77 |
    | 4.5 | 29 | `twins/sharp/blur1/f1.2` 3.42 | 0.48 |
    | 5.0 | 1 | `twins/sharp/blur2.5/f1.2` 0.56 | 0.24 |
    | 6.0 | 0 | 0.00 | 0.00 |

    The narrow fields decide it: at a field width clamped to 1.5 px the filtered noise has the most maxima per area.

    *Plan A Task 10b phase A1 (F-10, rulings A1-A4, Q4; 2026-10-11):* K stays 6.0 but its σ_f is PROPAGATED, no longer
    measured on the filtered plane: σ_f(x) = the plane's one 128-px mesh's per-cell clipped σ (its noise map) × the
    noise gain, the filtered-to-pixel σ ratio of the noise at each pass's kernel and grid. The caller states the
    noise's spatial correlation (`star_measure::PixelNoise`, A1c, ruling C-10): `White` (sensor pixels, raw or
    calibrated; mono, undebayered CFA, super-pixel) makes the gain 1 by derivation (A1b, ruling C-7: shot, read and
    dark noise are per pixel and calibration is per-pixel arithmetic, so every lag-≠0 covariance is 0) and nothing is
    read; `Mixed` (debayered, resampled, warped, integrated) and `Unknown` (the default; a debayered
    `MeasureOptions.colour` plane is `Mixed`) read it. The Measure stage states `White` for a calibrated mono plane
    and `Mixed` for a debayered one; master statistics (integrate, drizzle) state `Mixed`. The read comes from the
    noise's own autocovariance (reach 2 px), read once per plane on the 5×5 high-pass residual (sources beyond 5
    robust σ and 4 px about them left out, anchors in the quieter half of the noise map) and solved by least squares
    (white noise reads 1.00, a 3×3 binomial smoothing 2.32 at the 3-px kernel); that read leaks pixel-scale nebula
    texture, a recorded limit owned by Task 11. The mesh is computed once per plane and shared by `N*` and the filter
    (Q4: seeds and every probe line bit-identical to fee44d50 on 5 real frames and 12 v2 families). The shared mesh is
    the masked one, so on a MASKED plane `N*` reads the masked mesh (an absent pixel - Plan C's other-colour CFA
    site - is not sky; ruling D-1 records the change, no production caller passes a mask today;
    `on_a_masked_plane_n_star_reads_the_masked_mesh`). Two knot tests at the field width, thresholds derived from a
    Moffat β 1.5 star and the ratio's noise at z 3.09 (D2), drop structure seeds and flag extended ones
    (`StarFlags::EXTENDED`: fitted, but out of the β sample, `c`'s qualifiers and the frame FWHM/ecc, P3); a seed
    within the plateau-clip band of the clip is exempt. The β sample takes only verdicts that pass the fit's own
    centre and region tests (`SaturationVerdict::centred`, A4). Ruling A1-1 (closes A1's STOP): the plan test
    "propagated = measured on a clean field" failed on its premise - on the 150-star fixture the MEASURED σ_f reads
    1.121× the true noise (the bright stars' filtered wings, already so before A1), the propagated one 0.997× - so the
    test (`propagated_detection_noise_is_the_true_noise_on_a_clean_field`) judges the propagated σ_f against the
    fixture's TRUE noise within 4 % and reports the measured one.
  - **The surfaces.** The stacking adapter's probe override gains `SeedSource::MatchedFilter { threshold }` (the
    config's default stays the peak detector until Task 7, which renames the enum `SeedOverride`, ruling F26).
    `star_probe` gains `--seeds matched|peak|structure` (default `matched`) and `--seed-threshold K`; every frame line's
    `options.measure` names the effective `seedSource` and `seedThreshold` (ruling F3), which the scorecard's
    expected set reads (`matchedFilter` → Fisher SNR ≥ K).
  - **Acceptance.** The lab step `t06a-seeds`, recorded in athenaeum-lab `star-lab/results/plan-a/t06a-seeds/`
    (lab 1270f68). The cycle ran at K 6.0 on star_probe d06cab40, built from athenaeum f5bbc822; all 697 outputs are
    proven.
    - **Population.** The matched filter seeds 32 % more stars: 79 269 → 104 475 seeds and 77 978 → 101 097
      accepted fits on the measure path. `fit outcomes` is ok on all 177 rows.
    - **The design list.** The brief's 22-entry list left the rows this population moves unnamed (26 762
      quantities, 163 one-sided misses against t05). By controller ruling the list was amended to 34 entries,
      with the reason "a new seed population, the matched filter, +32 % seeds; same-star test vs truth: unchanged
      where β held". The amended list:
      - `fit outcomes` on `betas@*`: reported;
      - row 2 `centroid` on `measure` and `register`: set aside (on `register` for this step only; Task 8 restores
        the F10 one-sided entry);
      - row 4 `ecc` and `angle` on `measure` and `betas@*`: set aside;
      - row 4 `axis order`: reported;
      - row 8: set aside;
      - row 10 `auto beta vs saturated`, `peak5` and `saturation flag`: reported.

      With it both no-regression diffs PASS: 0 moved, 0 missed, 0 lost, control 0 against t05 (34 entries) and
      against t01c (the 77-entry union).
    - **The β flip, owned by F-3.** athenaeum-lab `bin/same_star.py` compares the same truth star in both runs. On
      68 of 376 planes the auto β changed (66 upward, e.g. 4 → 10 on 25), because the matched filter puts saturated
      stars into the auto-β top-64 sample (row 10 `n_sat64` median 0 → 31). There the same stars' FWHM error
      against the pointfit truth reads 2.77 % → 5.72 % (24 365 worse, 4 765 better, worse in every Fisher bin).
      Row 3's set-aside note records that the fits are not unchanged on those planes. F-3 (Task 8, the β sample on
      unsaturated stars) owns the cure: on the planes in `beta-flips.tsv`, t08's β must equal t05's.
    - **Where β held, the same stars measure the same.** FWHM error 2.508 % → 2.505 %; eccentricity unchanged;
      the bright centroid improves (Fisher SNR > 300: 0.0133 → 0.0097 px). The register centroid improves
      (0.0294 → 0.0290 px).
    - **The F-2 rows at t06a (reported).** Angle `eff` is 0.75–1.28 on the 24 shape cells (t05: 0.82–1.31). Axis
      order is 100 % on every family and path.
- **F-3 - the per-seed saturation verdict and the unsaturated statistics (Task 8; numbers move by design).**
  - **R-OSP-5 - one saturation test per seed, after the field width: an established stacking program's method,
    adopted as is (owner ruling 2026-10-09, spec §2.1, PA-23).** The flat-top wing test (PA-8) failed on lab truth
    and is retired; its constants were never measured here and neither are these.
    - **The plane's inputs** (`star_measure::photometry::SaturationPlane`, working units): bg = the module's sky
      median, σ = the noise model's σ, threshold = bg + **5σ**, norm = the plane's full scale (`ADU_SCALE` for a
      calibrated plane; for raw ADU the stored integer type's, `Domain::RawAdu.full_scale`, never DATAMAX), range =
      min(plane max, norm) − bg. The cap keeps the range at full scale where a flat division lifts a plateau above
      1.0.
    - **The plateau test at the seed pixel** (`plateau_test`): of pixel0's 8 neighbours, those ≥ threshold must
      number **≥ 3 of 8** on a mono plane or **8 of 8** on a debayered colour plane (`MeasureOptions.colour`: the
      stacking adapter's 3-plane OSC frames, the probe's 3-plane input), with meanhigh − bg ≥ **0.7**·range and
      pixel0 − minhigh ≤ **0.1**·range. The level is min(pixel0, norm) − 0.1·range when it passes, else norm.
    - **The verdict fit** (`saturation_verdict`): a free-β Moffat (`fit_moffat_2d`; the bounded fit from Task 9) on
      the seed's own region (F-1) without its pixels ≥ the level, started at the region's rim median, the seed and
      the star's own F_start, round; the iteration limit **×3** when any pixel is excluded. **n ≤ 8** pixels left
      (the fit's 8 parameters) refuses the seed `mostlySaturated`. A fit that fails otherwise gives no flag and no
      refusal (`fit = false`, counted as `verdict_fit_failed`) and keeps the seed out of the β sample (Q4).
    - **The flag** is the verdict fit's amplitude above the range (`by_amplitude`) or the plateau-clip clause below
      (`by_plateau_clip`); the plateau test otherwise only sets the level.
    - **R2, the plateau-clip clause (owner extension 2026-10-09, after research R-S).** A seed whose plateau test
      passed and whose pixel0 − bg ≥ **0.9**·range (pixel0 within the test's own 0.1·range band of the clip) is
      flagged saturated whatever its fitted amplitude: `flag = by_amplitude || by_plateau_clip`
      (`SaturationVerdict`, both reported; the probe's `--diagnostics` verdict carries `byAmplitude` and
      `byPlateauClip`). The clause needs no fit, so a verdict whose fit failed with a passed plateau at the clip is
      flagged too. It catches seeds inside large saturated areas (a nebula core: the fit sees no peak above the rim,
      A/range 0.002-0.05) and bloomed stars whose region holds a neighbour's core. R-S on the lab: flag FP +5
      (0.149 -> 0.156 %), FN 6 -> 4 (0.147 -> 0.098 %); every blob seed on the real M42 frames flagged. Unchanged:
      the cross-plane spread, the `mostlySaturated` refusals, the at-beta exclusion of flagged stars. Known limit: the
      plane's brightest flat-topped star is flagged by construction (it sets the range, so pixel0 - bg = range).
    - **The three substitutes** for the reference method's own inputs: bg is the module's sampled, clipped sky median
      (PA-21), σ is the MRS σ (`N*` when MRS is refused; 0 under `NoiseSource::None`, one `warn!` per plane), and
      pixel0 is the pixel at the rounded seed centre (`fit_one`'s own `(cx, cy)`).
    - **The order** (`fit::verdict_pass`, then `fit::fit_after_passes`): seeds → field width → verdicts → (on a
      multi-plane image every plane's verdicts, then the cross-plane rule) → β sample → fits at β. The verdict
      pass runs `fit_one`'s steps 1–4 (admission, width start, region, donut test) per seed, so a seed refused there
      gets no verdict. The `Auto` β sample is the 64 brightest seeds whose verdict fit succeeded unflagged. Every
      star is then fitted at the frame β and keeps its flag in the `.athf` flags byte; only a flagged star is fitted
      with its verdict's exclusion (Q6 as revised, below).
      The frame FWHM and eccentricity are the residual-weighted means over the accepted unflagged fits
      (`FrameAggregates.stars_unsaturated`; `None`, and the adapter's `fwhm_px = 0.0`, when there is none).
      `stars_fitted` and the PSF-signal totals still count every accepted fit (M-4).
    - **The measure path and the master statistics** (Q7; review I-1 ruled 2026-10-09). `measure_plane` runs the
      verdicts, and so do the master statistics (`integrate`, `drizzle`, their covered-rectangle crops included),
      which measure a 3-plane master as a debayered colour image; LN's refits (`fit_stars_at`, `fit_stars`,
      `fit_stars_with_beta`) and the probe's betas arm run none: no flag, the brightest 64 seeds. The exemptions are
      LN's reference (its own L-23 mask) and registration's own cuts.
    - **The cross-plane rule (owner ruling 2026-10-09).** On an image of more than one plane, a star flagged on any
      plane is flagged on every plane at that position: a star saturated in any channel is saturated in all,
      because the debayer mixes the channels (the reference measures green only, so this goes beyond it). The match
      is seed position ≤ 1 px (`CROSS_PLANE_MATCH_PX`, R3's); only a plane's own flags spread
      (`SaturationVerdict.cross` marks the rest). The union is formed after every plane's verdict pass and before any
      plane's β sample, frame statistic or fits at β, in ONE multi-plane entry,
      `stacking::measure::measure_planes_diagnosed` (`star_measure::{prepare_plane, cross_plane_flags,
      finish_plane}`), which the frame adapter, the master statistics and the probe call; one plane is
      `measure_plane_diagnosed` unchanged. The entry takes its planes through a reader called once per half, so the
      frame adapter re-reads a plane from disk instead of holding every plane (`MEASURE_PLANES_RESIDENT` unchanged).
      R3's mechanism: all 52 OSC false negatives of t08-f3 were G-saturated stars with no plateau at their R or B
      seed pixel, each flagged on G. Lab (`t08-f3`, round 3): false negatives mono 3/4089 = 0.073 %, OSC 0/1229 (was 52);
      false positives mono 0.294 %, OSC 0.411 % (5 own + 90 cross, every cross flag a star saturated in another
      channel). A `MostlySaturated` refusal on one plane counts as an own flag in the union (the Task 8 close; the
      lab's OSC `MostlySaturated` count is 0, so no lab number moved). The union is idempotent: only own flags
      (`!cross`) spread.
    - **Q6 as revised (controller ruling from R3, 2026-10-09).** The fit at β leaves out the pixels ≥ the star's own
      level (norm when its own plateau test failed), ×3 iterations, ONLY for a flagged star, own or cross. An
      unflagged star's fit at β is byte-identical to a fit with no verdict. R3: Q6 as first written censored 218
      truth-unsaturated plateau stars at β (the held-β FWHM 163/55 moves). Lab: 37 278 / 37 278 unflagged measure fits
      equal the held-β fits (the β-change control).
    - **Formula weighting (review M-3).** A channel with `stars_unsaturated == 0` (no accepted fit, or every one
      flagged) has no FWHM or eccentricity: it is left out of the group's `fwhm` and `ecc` ranges and those two terms
      give it zero credit (`stacking::weights::formula_weight`); SNR, stars and the pedestal are unchanged.
    - **The field width runs no saturation test** (ruling F25 as folded): PA-17's median reads every candidate,
      because the verdict's region needs it. Cost if wrong: a field width biased on frames dominated by saturated
      stars. *Task 10b A2:* that cost was real (M42 8.2–8.8 px against stars of ~2; L 0000 10.8 against 5.6); F25
      stands — still no saturation test — and the estimator became the mode of every readable read, which a clipped
      core (read wider than the PSF) no longer moves.
    - **Known limit, three consumers.** A star whose clipped core is small (3–5 px) fails the full-3×3 plateau test,
      so its verdict fit runs uncensored: it is still flagged, but its own FWHM is not trusted. No consumer reads it:
      the frame statistics exclude flagged stars, the Analysis tab (Plan C) takes unsaturated stars, and registration
      takes centroids. Consumers: the frame statistic excludes them; Analysis takes unsaturated stars; registration takes
      centroids.
    - **The row and the payoff.** Row 10 `saturation flag vs truth`: false negatives ≤ 0.5 % among truth-saturated
      stars with ≥ 3 clipped core pixels, false positives reported (a plane's brightest star sets its own range by
      design). The payoff is the censored verdict fit's FWHM error where the plateau test passed, beside the
      failed ones (|ΔFWHM|/FWHM, median / p90): mono 2.85 / 8.51 % where the plateau test passed, 34.82 / 61.39 % where it
      failed; OSC 7.93 / 20.95 % and 32.81 / 50.45 %.
    - **Measure wall time** (one real calibrated mono frame, LDN_1272, release probe): 1.19 → 1.69 s (c59f4385); the
      verdict pass's region reuse changes nothing. **The real-frame note:** LDN_1272 β 10 → 2.5, frame FWHM
      2.722 → 2.325 px.
  - **The surfaces.** `RefusalReason::MostlySaturated` (`mostlySaturated`, declared last); `FitDiagnostics.verdict`;
    `ChannelMeasurement.stars_unsaturated` (camelCase `starsUnsaturated`, 0 in a payload written before Task 8); the
    adapter's `PlaneMeasurement.verdict_fit_failed` and `cross_flags` (reported, not persisted);
    `SaturationVerdict.cross`; the probe's measure frame line `starsUnsaturated`, `mostlySaturated`,
    `verdictFitFailed`, `crossFlags`, `colour`, and under `--diagnostics` each star and refused line's `verdict` with
    its `flagSource` (`own` | `cross` | null). The probe measures every plane of a multi-plane input through the
    entry, even under `--plane`; a plane's `wallMs` is its own share of the call.
  - **Moved pins (by design: the statistic's population, the β sample, the stored censored width).** Each literal
    carries its previous value. On the 150-star fixture fields 9 of 150 fits are flagged (the brightest stars' verdict
    fits top the plane's range, the by-design false positive); the fits, β and the PSF-signal totals do not move, the
    frame FWHM and eccentricity move by 4–7e-5 relative (`stacking::measure` tests: the Peak and matched-filter
    records, `the_shipped_none_path_…`, `the_matched_filter_default_…`). The drizzle sharpening test's lone star is
    flagged the same way, so `stats.fwhm_px` reads 0.0 there and the test reads the star's own fit (unchanged).
  - **Acceptance.** The lab step `t08-f3` (round 3, closed by the design entries of the C3 report): FN as above; twins
    44 of 46 families ≤ 1 %, and `twins/sharp/m2.5/f1.6` and `twins/sharp/m4/f2.0` move to Task 9 (discrete β near-ties
    on undersampled stars; at held β they read ≤ 0.10 %); R2 f12.0 / f16.0 ≤ 0.04 %, and `sharp/m4/f2.0` i0 moves to
    Task 9 (+2.62 % at β 6, +0.27 % at the truth β 4); β-change control 37 278 / 37 278.
- **F-4 - the continuous per-frame β (Task 9; numbers move by design).**
  - **R-OSP-6 - `PsfModel::Auto` is a continuous per-frame β, the median of pixel-integrated bounded free-β fits
    of the sample (spec §3 F-4, as revised by the owner's ruling E5, 2026-10-09).** The discrete search over
    `AUTO_BETAS` is retired; the ladder stays as the probe's betas arm (row 3's β sensitivity), which no production
    path reads. The Gaussian is the large-β limit: there is no separate Gaussian model, and β stays a finite number
    everywhere (`.athf` f32, `metrics`, `stacking_run_groups.beta`, `group_beta`, `PsfModel::Fixed`);
    `FITS_ARTIFACT_VERSION` and `MEASURE_VERSION` (4, one bump per plan) are unchanged.
    - **Why the sample fit is pixel-integrated (the cause).** Task 9's first build read the frame β as the median of
      the bounded free-β verdict fits, a POINT model. A point Moffat fitted to stars whose pixels integrate their
      light reads β high, more so as the star's FWHM in pixels falls, because the integrated core is flatter: on the
      lab's m2.5 stars it read 3.47 / 3.17 / 2.93 / 2.76 at FWHM 1.2 / 1.6 / 2.0 / 2.6 px, and the frame FWHM read
      1.4–6 % high on undersampled low-β stars (the lab step `t09-f4` stopped on it; research R-F4: the noiseless
      sampling term explains +11.65 of the +13.40 % mean β gap over 28 planes, and no point-model statistic — the
      median, the sample, the region, the background — fixes it). A pixel-integrated model returns the truth β within
      0.1–0.7 % under every region and background variant, and the point-model FWHM at that β equals the one at the
      truth β within 0.1 %. **Owner ruling E5 (2026-10-09):** the frame β comes from a pixel-integrated fit of the
      same sample; the verdict fit and every fit at β stay the point model.
    - **The fit** (rustafits `fit_moffat_2d_bounded`, branch `one-star-path-beta`): a free-β Moffat whose solver
      carries **ln β** — the Gaussian limit is a flat ridge in (α, β), which a step in ln β crosses in a few
      iterations — with the Jacobian column `A·(1+Q)^−β·(−ln(1+Q))·β` (the chain rule) and each step's ln β clamped
      to the caller's bounds. A fit that ends at a bound is returned with β exactly that bound; only a non-finite
      parameter, α < 0.3, A ≤ 0 or a solve that never left its start (fix round A3, below) refuses it. It carries the
      corrected θ column (R-OSP-3). Its `subsample` (k): k = 1 is the point model, byte-identical to the entry
      point's first build (cceab37) for every solve that accepted a step (the golden pin
      `the_bounded_fit_at_one_sample_per_pixel_is_byte_identical_to_before`, recorded before the change, every field
      and the iteration count on three stamps); k ≥ 2 makes the model and its Jacobian at each pixel the mean over
      k × k midpoint sub-samples at offsets `(i + 0.5)/k − 0.5` px (the Jacobian is pinned against central
      differences of the integrated model at k = 3, every column). `fit_moffat_2d` (β linear, [1, 20], the legacy θ
      column) is output-identical: the solver's `BetaParam::FreeLinear` arm is its code, and
      `fit_moffat_2d_is_byte_identical_to_before` pins it bit for bit on a C3 stamp (recorded at 197fd93). Unit: a
      noiseless Gaussian (σ 2) reaches β 100 in 39 iterations at −0.32 % FWHM; Moffat β 4 and 2.5 are recovered to
      1e-8; an integrated m2.5 stamp reads β 2.5098 / 2.5052 at FWHM 1.2 / 2.0 px at k = 7, where k = 1 reads
      3.127 / 2.814.
    - **A solve that never left its start is not a fit** (fix round A3, rustafits e9cfdb1). On 6 of the 33 t09-f4
      inject planes some sample stars read β exactly 3 (the start), 57 of their 2112 sample fits — 15–17 of 64 on
      the mono2 L 300 s frames, where the median landed on it (frame β 3.0000000000000004) — and each of those stars'
      verdict fits had stopped on its start too. The cause: the solver refuses a step that moves the centroid more
      than 2 px or takes α under 0.5, and counted each refusal against `max_rejects` (5) like a step whose cost rose.
      A star whose top is censored and whose fit starts under it (A = 1, the clipped maximum, many times under the
      truth; a start width under the censored core) overshoots that guard on every first step while λ is still
      under 1, so five refusals ended the solve on its start, which the bounded entry point returned as a fit; the
      verdict then read the clipped maximum as the amplitude, so a saturated star went unflagged into the sample.
      Now, in the bounded entry point only (`GuardedStep::UncountedBeforeFirstStep`), a refusal before the first
      accepted step raises λ without spending the budget (after one, refusals count as before), and a solve that
      accepted no step and did not converge is refused (`None`): the verdict reads `fit = false`, the sample fit is
      counted out (`sample_median`'s `debug!` names how many). The legacy entry points keep the counted refusal,
      output-identical; the fit at β kept it until fix round A4 (next item). Unit: a censored integrated m2.5 star (50 above a 1.0 clip, 50 pixels
      excluded, started at F 2.5) is flagged (verdict amplitude 56.98) and its integrated fit reads β 2.5007 (before:
      the start, β 3 and the clipped amplitude 0.96). On the six planes the stuck sample fits go 57 → 0: the real saturated stars among them
      are flagged now; frame β 6.390 → 11.598, 3.000 → 3.551 and 3.000 → 3.666 on the three mono2 frames, 3.687 →
      3.733 on one 30 s frame, unchanged on the other two; LDN_1272 mono and OSC are identical but for one non-sample
      verdict that had been stuck; the wall moves −1 … +10 % (3 interleaved runs). Two things it does not fix,
      both older than it: seeds inside saturated blobs (a rim at or near the clip, thousands of pixels excluded)
      entered the sample unflagged, then with β 2–5 or 27–100 instead of exactly 3 (11–12 of 64 on the mono2
      frames; superseded by the plateau-clip clause of R-OSP-5, owner 2026-10-09: they are flagged now); and a region whose rim median equals its maximum (`a0` at its 1e-9 floor) is conditioned to ±1e8,
      where the shape steps fall under one ulp — one such sample star still reads exactly 3.
    - **The fit at β never returns its start either** (fix round A4, rustafits, the same branch). The conditioned
      fixed-β entry point every star is fitted at still counted each guard refusal, so 80 of its fits stopped on
      their start (B, A, the centre bit for bit) on 10 of the 21 planes surveyed — LDN_1272 mono and OSC, the six
      inject frames, eleven saturated/density/clipped-mixture lab frames (none on the lab frames): 45 unflagged
      accepted, 22 flagged accepted, 13 then refused by acceptance. Two causes: 43 had counted guard refusals (22 of
      them flagged censored stars, the A3 mechanism), 37 unflagged ones five cost rejections from their start (small
      undersampled stars, 32 of them on the LDN OSC planes). The entry point now takes `fit_moffat_2d_bounded`'s rule
      (`GuardedStep::UncountedBeforeFirstStep`: an uncounted guard refusal before the first accepted step, and a
      solve that accepted no step and did not converge refused); the caller counts it `fitFailed` and a plane-level
      `debug!` names how many fits failed. Only the stuck runs change (the two rules part only in a solve the counted
      one ended before its first step). After: 0 stuck; the 43 now move — 3 stay flagged fits, 23 are refused
      `centreMoved` (19 of them the flagged ones, in saturated blobs on the mono2 frames), 16 `narrowerThanPixel`,
      `residualTooLarge` or `regionTooSmall`, 1 `fitFailed` — and the 37 are refused `fitFailed`. 79 records change,
      nothing else: the frame β and its sample are identical everywhere; on LDN_1272 mono 1 accepted fit fewer (FWHM
      +5e-6 relative), on the OSC planes 9 / 10 / 14 fewer (FWHM −5e-5 … −9e-5). Unit: the censored star of A3 at
      its own β 2.5 left its start (before: A 0.96, the start FWHM 2.5, the seed's centre) and converges at FWHM
      2.125, centre within 5e-5 px (amplitude 59.8: the point model on integrated pixels); with 3 iterations, or at
      the 1.3.0 entry point, it does not move: refused here, returned there as it always was.
    - **The fit at β has no β ceiling** (rustafits, the same branch, 0ebc618). The shared Moffat acceptance refused
      any β > 20, the fixed arm included, so every star of a frame whose β rose above 20 was refused `fitFailed`. The
      conditioned fixed-β entry point (R-OSP-3, the one the fits at β use) now fits any fixed β ≥ 1; below 20 its
      output is what it was, and the two 1.3.0 entry points keep their [1, 20]. A Phase A deviation from the plan's
      "legacy entry points identical", ACCEPTED by the controller (ledger, 2026-10-09) as the spec's "bound lifted to
      the cap" for the fit at β.
    - **The constants** (`star_measure::beta`): `BETA_BOUNDS = (1.5, 100)` (at β 100 a Gaussian star reads −0.32 %,
      β 30 −1.04 %, spec re-review N-1); `BETA_START = 3.0` (the verdict fit's start β, the free arm's through Task
      8, and the sample fit's); `AUTO_SAMPLE = 64`; `BETA_MIN_FITS = 8`; `BETA_FALLBACK = 4.0`;
      `SAMPLE_SUBSAMPLE_PX = 10` and `SAMPLE_SUBSAMPLE_RANGE = (3, 9)`: a sample star is fitted at
      `k = clamp(⌈10 / F_start⌉, 3, 9)` (`sample_subsample`), about 10 sub-samples across its FWHM.
    - **The measure path (C-b, PA-23, E5).** The verdict fit of R-OSP-5 IS the bounded fit at k = 1 (`BETA_BOUNDS`,
      start 3.0; its region, starts, exclusion and ×3 iterations unchanged), and a β at a bound is a successful
      verdict fit. The sample is the 64 brightest seeds whose verdict fit succeeded unflagged (own and cross-plane
      flags united first; `sample_seeds`). Each is re-fitted pixel-integrated on the verdict fit's own problem
      (`photometry::integrated_beta`): its region (re-gathered from the seed's record; steps 1–4 are pure) minus the
      pixels its level excluded, conditioned the same way, from the same starts (b0, the seed, the star's `F_start`,
      β 3.0), in `BETA_BOUNDS`, with the verdict's iteration budget, at the star's k. β is the median of the accepted
      integrated fits (the mean of the two middle ones for an even count), clamped to the bounds. At least 8:
      `BetaSource::Fitted { sample }`; fewer: β 4.0, `BetaSource::Fallback`, one `warn!` per plane. Every star is
      then fitted at that β with the point model (the fixed-β conditioned fit of R-OSP-3, unchanged). Unit
      (`star_measure::beta` tests, noiseless fields of 70 off-grid stars, each pixel integrated over 15 × 15
      sub-samples): `frame_beta` reads m2.5 / m4 / m10 at +0.11…+0.62 / +0.12…+0.61 / +0.14…+0.79 % of the truth β
      from FWHM 1.2 to 8 px, an integrated Gaussian at 99.9992 (FWHM 1.6) and 100 (FWHM 4); the measure path reads an
      integrated m2.5 field at FWHM 2 px at 2.5038.
    - **Spec delta (§3 F-4; spec §2.1's "the unflagged bright seeds' verdict fits are the β sample's free-β fits").**
      "Fits nothing twice" no longer holds: each plane runs up to 64 extra fits (the integrated sample fits, up to
      k² = 81 model evaluations per pixel). Measure wall on LDN_1272 mono, Task 9's first build (1d2d40cb) → E5:
      1.81 → 1.84 s (+2.0 %, 3 interleaved runs; the owner-item line is +15 %); the OSC `_d` frame's planes +0.9 /
      +1.5 / +2.6 %.
    - **Q7 - a caller that runs no verdict.** `fit_stars` under `Auto` and `star_measure::frame_beta` (Plan C's green
      sites, ruling F45: it takes `FitStarsOptions`, so the mask and the field width reach the sample) fit their own
      sample the same way: the pixel-integrated fit of the 64 brightest seeds from the verdict fit's starts, with no
      saturation test and no exclusion (the level of `SaturationPlane::UNTESTED`); the same median, minimum and
      fallback. Q7 reaches tests and examples only (review M-2): in a run, LN's `Auto` is
      `PsfModel::Fixed(group β)` (`run.rs`, the grid path included), so `ln/scale.rs`'s reference fit never samples
      there and the Normalize wall is unaffected; the direct `fit_stars(Auto)` callers are tests and the `ln_*` /
      `weight_audit` examples.
    - **`BETA_AT_CAP`.** A fitted frame β at a bound (exactly 1.5 or 100) flags every star fitted at it (the
      `.athf` flags byte, bit 2; the probe's `betaAtCap`); a caller's fixed β (`Moffat4`, `Fixed`, `fit_stars_at`)
      never does. `group_beta` is unchanged code: the lower median of its members' continuous βs, so it is always
      one of their own.
  - **The surfaces.** `star_measure::{BetaSource, BETA_BOUNDS, frame_beta}`; `PlaneMeasurement.beta_source` (the
    module's and the adapter's; reported, not persisted) and `FitOutcome.beta_source`; `StarFlags::beta_at_cap()`;
    `SaturationVerdict.iterations`, which fills `FitDiagnostics.iterations` with the bounded verdict fit's LM
    iterations (`None` where no verdict fit succeeded: the fixed-β fit reports none); `FitDiagnostics.sample_beta`,
    a sample star's integrated β (`None` for every other seed); the probe's measure frame line `betaSource`
    (`fitted` | `fallback` | `fixed`) and `betaSample`, and with `--diagnostics` `sampleBeta` on a sample star's star
    or refused line. The Measure panel labels the model "Auto (per-frame β)"; the Normalize panel labels it "Auto
    (group β: lower median of the frames' β)", because in a run local normalization fits every frame of a group at
    the group's one β (review M-1).
  - **Moved pins (by design: the continuous β).** Each literal carries its previous value. Task 9's first build:
    the three earlier tests ruling F14 names (`group_beta`'s, renamed `…_always_one_of_the_members_betas`;
    `auto_model_prefers_the_generating_beta`, Moffat-4 within 0.2 of 4 and a Gaussian field ≥ 60;
    `beta_sample_skips_saturated_stars`, within 0.2 of 4), LN's `target_is_fitted_with_the_reference_beta` (3.933 on
    its Moffat-4 field, no longer a ladder value), and the four `stacking::measure` fixture pins: β 10 → ≈ 99.3 on
    the Gaussian fixture, frame FWHM 4.129 → 4.229 px against the Gaussian truth 4.2387 (−2.59 % → −0.22 %), tflux
    −1.5 % (the FWTM aperture follows β); 6 of 150 flagged (was 9) on the two field-7 pins (the Peak and the
    matched-filter scalar pins: 144 unflagged), 1 of 150 (was 9) on the two `2026_09_19` records (149 unflagged)
    (review M-6) — the bounded verdict fit's amplitude on a Gaussian star no longer overshoots the range the way the
    free fit stopped at β 20 did. At a non-integral β the fits run on `powf` throughout, so those records pin β and
    every fit-derived field to a few ulp (the counts, noise and sky stay exact). E5 moved β on the four
    `stacking::measure` pins (99.29 / 99.25 / 99.56 / 99.52 → 99.51 / 99.36 / 99.49 / 99.54, still near the cap:
    point-sampled stars at FWHM 4.24 px, k = 3) and the fit-derived fields with it (≤ 6e-6 relative; the Peak
    record's eccentricity −0.44 %), the counts and flags unchanged; and LN's `target_is_fitted_with_the_reference_beta`
    3.933 → 3.506 (that pin was a known-wrong value of a point-sampled fixture, and is restored in the Task 9 close:
    the test now renders its Moffat-4 pair pixel-integrated, `test_support::integrated_field`, and pins
    `|β − 4| < 0.2` again; the reference-β equality holds exactly; the other tests that share the point-sampled pair
    read no frame β). The drizzle
    sharpening test's four widths do not move (its lone, flagged star leaves the sample empty: β 4.0 before and
    after).
  - **The real frame** (LDN_1272, release probe, `--paths measure`, 3 interleaved runs, deterministic), Task 9's
    first build (1d2d40cb) → E5: mono β 2.108 → 2.000 (`fitted`, 64), frame FWHM 2.251 → 2.226 px (−1.1 %),
    12 767 → 12 749 fits (12 452 → 12 446 unflagged); the OSC `_d` frame's planes β 2.271 / 5.407 / 6.249 → 1.948 /
    5.292 / 5.192, frame FWHM 1.471 / 1.539 / 1.524 → 1.420 / 1.537 / 1.503 px; verdict fits failed unchanged (mono
    68, OSC 119 / 94 / 45), no `mostlySaturated` seed. The mono sample's point verdict β reads a median 3.5 % above
    its integrated β (p90 10.5 %). Before Task 9 (5bd5c926): mono β 2.5, FWHM 2.325 px; OSC β 2.5 / 4 / 2.5, FWHM
    1.499 / 1.499 / 1.363 px.
  - **R2 on the real frames** (A5, release probe, `--paths measure`): LDN mono β 1.9997 and FWHM 2.2262 px unchanged;
    OSC R / G / B β 1.9483 / 5.2918 / 5.1925 → 1.8616 / 5.0372 / 5.1925, FWHM 1.4194 / 1.5365 / 1.5029 →
    1.4037 / 1.5308 / 1.5029 px (one seed per plane leaves the sample); the three M42 mono2 inject frames flag 11-12 more
    sample seeds each (clip-only), frame β 11.598 / 3.551 / 3.666 → 13.957 / 3.142 / 3.342. R2 does not steady the M42
    frame β (the rest of its sample is nebulosity and near-clip stars whose clip on the calibrated plane is under 1.0;
    t08 read 2.5 / 2.5 / 6.0 on the same frames, so the instability predates F-4): a MUST-RESOLVE Task 13 item.
  - **Acceptance (lab step `t09-f4`, final: E5 + A3/A4 + R2, athenaeum 61838081 / rustafits c02fa93, lab bd2a059;
    `star-lab/results/plan-a/t09-f4/{run-log.txt,bars.txt}`).** Cycle 697/697 probes and 90/90 inject proven, 53 789
    scorecard rows, 0 missing. **BAR: PASS.**
    - **Control** (a gate): the fixed-β arms' star lines are bit for bit t08-f3's but for those A4 removed: 71 `betas`
      lines disappear on v2 and 527 on inject (a stuck fit refused), one CHANGES (inject corpus `…_30.00s_0015`,
      plane 0, β 2.5: FWHM 0.984 → 1.824, ecc 0.553 → 0.930; a stuck base fit that moved to a real minimum), none is
      new; legacy `analysis` rows 11 736 + 370 bit for bit; cumulative vs t01c 0 / 0 / 0.
    - Item 1 frame FWHM (C6: converged pointfit truth; C7-2: mixture vs `pixbox`): 0 of 58 images over the bar.
      Item 2 twins: 0 of 46 families over 1 %. Item 3 inject mono at its own β 4: 0 of 3 cells over 1 %. C7-1 pooled
      centroid efficiency vs t08-f3: measure x1.0018, register x1.0021 (bar 1.01; F ≤ 2 split x1.0136 / x1.0113, the
      debt line). Item 4 saturation flag vs truth with R2: FN mono 4 of 4090 = 0.098 % (r3 0.147 %), OSC 0 (bar 0.5 %);
      FP mono 116 of 74 284 = 0.156 % (t08 0.294 %; the R-S prediction), OSC 0.411 %. Item 5 neb3_4096 background
      −0.253 σ (bar 0.3). Item 6 same-star FWHM vs truth pooled BETTER (up 87 118 / down 104 168). Item 8 angle
      efficiency max 1.25 (bar 1.5).
    - Reported, not judged: owner item 6 pooled ecc > 300 WORSE (up 30 964 / down 27 172; mean |e| 0.0919 → 0.0933) and
      item 7 WORSE on betas@10 / @6 / @2.5 (PA-12 under a held β, the recorded trade-off, ruling C6-2). Measure-path wall
      (row 9, sum of 145 family medians) 29 618 → 30 804 ms (+4.0 % vs t08-f3, +2.0 % vs round 3). 30 s inject frame
      β 4.039 / 3.733 / 2.952 → 4.067 / 3.716 / 2.860; raw OSC planes read the bound 1.5 (VNG item, Task 13).
    - **Owner rulings of 2026-10-09:** E5 (the frame β from a pixel-integrated fit of the same sample); C6 1-4 (E5;
      PA-12's ecc trade-off under a held β recorded; the twin β part void where both twins exceed β 10, ΔFWHM always
      judged; debayered inject planes to the VNG item, mono cells judged, real OSC β 1.5 to Task 13); C7 1-4
      (small-star centroids accepted and recorded, the per-frame pixel-phase correction a later item; mixture judged vs
      `pixbox`; inject mono at β 4; twins β part void where EITHER twin exceeds 10); R2 (the plateau-clip clause);
      the ERP rule (re-run only when the synthetic frames change; "accepted no star" labelled, not "not run"). Fix
      rounds A3 / A4 (a solve that never left its start is refused and counted) are the controller's, ruled the
      same day. Debt vs t01c stays OPEN on FWHM scatter (measure) and centroid scatter (measure, register).
    - **Review round 2 (Task 9 close):** four minors fixed (doc lines in `photometry.rs` / `fit.rs` / rustafits
      `fitting.rs` and the lines above; `plateau_level` test-only; the plateau-clip tests; LN's restored pin); the
      logging dictionary notes the reused `count` / `failed` / `beta` fields and a per-plane `debug!` now names the
      verdict-fit failures. A near-start stall (a first accepted step at a huge λ) would pass the exact-start check; the
      probe's accepted star lines carry neither the iteration count nor the conditioned amplitude, so the lab count
      the review asked for is a proxy: of 8 476 censored accepted verdicts in the t09-f4 probe outputs, 0 read β within
      1e-3 of the start 3.0.

  - **R-OSP-7 - the star flux is the model's flux in its FWTM ellipse times a per-plane aperture correction (F-5, Plan A
    Task 10; ruling F14, no `MEASURE_VERSION` bump: one bump per merged plan, PA-4).**
    - **The signal.** `StarFit::signal` = `photometry::model_flux_in_fwtm` (the analytic `A·π·αx·αy/(β−1)·(1 −
      10^{(1−β)/β})`, no pixel read; the limit `ln 10` at β = 1) × `c`, the plane's aperture correction; `area` keeps
      its meaning. `c` is measured after the accepted fits and the ±1 px duplicate drop and before `FrameAggregates::of`,
      so `tflux`, `tmean_flux`, `psf_signal_weight`, `psf_snr` and every consumer of `signal` read the new flux. The
      old ellipse sum (`fit.rs::aperture`) is deleted: no production caller and no test needed it.
    - **`c`** = the median over the 64 brightest qualifying fits (by model flux, ties by seed index) of
      `growth(star) / model_flux`: `growth` = the sum of `v − b` over the pixels with `r ≤ 5F`, `b` = the PLAIN MEAN
      of the present pixels at `10F ≤ d ≤ 12F` that lie outside every seed's `3F` disc (owner ruling R6, round 3; until
      round 2 the median of `[6F, 8F]`); `F` = the plane's frame FWHM (`frame_shape` over the fits neither saturated
      nor extended, P3). A ratio only when the model flux is finite and > 0.
    - **Qualifying star** (brief C0): an accepted fit after the duplicate drop; not `flags.saturated()` (own or
      cross-plane); its fitted amplitude not above the plane's range (the clip screen, R6); no other seed handed to the
      fitter (accepted or not) within `6·F_field` px of its fitted centre; its whole `12F` disc inside the plane and
      inside the caller's footprint (round 4); no absent pixel (non-finite or masked) within `5F`; a rim pixel left. Constants: `GROWTH_RADIUS_PER_FWHM = 5`,
      `GROWTH_RIM_INNER/OUTER_PER_FWHM = 10 / 12`, `GROWTH_RIM_SEED_MASK_PER_FWHM = 3`, `CORRECTION_STARS = 64`,
      `CORRECTION_MIN_RATIOS = 5`, `CORRECTION_ISOLATION_PER_FIELD_FWHM = 6.0`.
    - **R6 (owner ruling 2026-10-11, brief C6; after the lab's RF5 / RF5b / RF5c research).** Why: on the Measure path
      `c` worked (t10-f5 r1, C1.1 0.88 / 1.61 %), but LN's `c` was wrong because (1) LN's fits run no saturation verdict,
      so clipped stars filled the 64 (c −8 to −11 %), and (2) the `[6F, 8F]` median read the neighbours' light
      non-linearly (part 1's L3 sky, f1v2-findings mechanism 3 - the basis of the ruling, not the bars). The rim is the
      far ring `[10F, 12F]`, the seed discs (radius `3F`, seed positions, the star's own included, one plane-wide bit
      mask per call: `photometry::SeedMask`) are left out, `b` is their plain mean. **The clip screen** (F-3's amplitude
      clause on every path): a candidate whose fitted amplitude > `range = min(plane max, norm) − sky median` does not
      qualify - on the measure path the plane's own `SaturationPlane`; on a path without a verdict (`fit_stars_at`, its
      wrappers, LN) `photometry::unverdicted_clip_range` of the fitted plane: plane max over its present pixels,
      `norm` = the caller's full scale (round 4, below), the sky median of the module's stratified sample clipped to `(0, plane max)` (`SkyLevel`'s
      clipped median without its one-count margins, which need the plane's units: LN hands native `[0, 1]` planes, the
      probe ADU-scaled ones). The screened count rides on the per-plane events as `skipped`. The plateau clause is NOT
      in the screen (RF5b: it adds 0 on both paths and needs a noise σ the refit paths do not have). Predicted by RF5c
      (non-`c` parts held): every LN bar passes (psfSel max 1.72 %), C1.1 0.76 / 1.38 %. Known costs, recorded: absolute
      flux on nebula / crowded wide frames (neb3 0.92 → 0.88, hc11B G `_0003` −5.9 %, crowded ±4 %); Task 13 (M42) reads
      the weights.
    - **Round 4 (controller rulings, 2026-10-11).** (1) **The footprint**: LN's planes are zero outside its `allowed`
      region (the eroded coverage / overlap, `ln/mod.rs`) and LN seeds only inside it, so a far-ring mean reaching past
      it read zero fill and unseeded stars. `FitStarsOptions::footprint` (borrowed `&[bool]`, never copied): a star
      qualifies for `c` only when every pixel centre within `12F` of it has `footprint[i]`, exactly like the plane-edge
      rule; read by the aperture correction alone (the fits do not see it). LN passes `allowed` on both sides - the
      reference's (`PreparedReferenceChannel::build_with`, through the new `star_measure::fit_stars_with_model`) and the
      target's (`relative_scale_against_core` / `relative_scale_from_seeds_core`, through `fit_stars_at`); legacy mode
      and every other caller pass none and behave as in round 3. Cost: a star within `12F` of the region's edge does not
      qualify; a small overlap may fall under 5 (`c` = 1, one `warn!` on that side). (2) **The screen's full scale**
      is the caller's, in the plane's own units: `FitStarsOptions::full_scale` (`None` = uncapped, `range = plane max −
      sky median`); LN passes `1.0` (`ln/scale.rs::LN_PLANE_FULL_SCALE`: its registered frames and reference are the
      unit-domain calibrated planes the measure path reads as `Domain::Calibrated`), the probe's betas path `ADU_SCALE`
      for its ADU-scaled plane; the measure path keeps its `SaturationPlane`. (3) Plan tests 1 and 2 assume isolated
      stars and R6's footprint is `12F`, so they run on a 140-px grid on an 841 × 701 plane (a 70-px margin = ⌈12 × 5.78⌉,
      the widest profile's measured frame FWHM, + 1 px for the last pixel centre; the range-setting star at a cell
      centre, 99 px from its four stars); the 70-px number stays as a labelled pin (F28,
      `r6_far_ring_takes_in_the_neighbours_wings_on_a_70_px_grid`: 0.98164, R6's neighbour-wing cost). (4)
      `ln_refit_takes_a_given_correction` carries round 2's range-setting star (1.5×, unseeded) 140 px below its grid
      on a 480 × 560 plane. (5) **EXPTIME's one ulp** was serde_json's float parse: the cached `FrameMeasurement`
      JSON carried ryu's shortest strings (e.g. `96.26476737863949`, bits `…379`) that serde_json without
      `float_roundtrip` read one ulp low (`…378`; 4 of 4 cached frames had such a field), so the cached run's weights
      and `weighted_exposure_s` moved an ulp; `athenaeum-core` now enables serde_json's `float_roundtrip` feature (no
      version change, `Cargo.lock` unchanged) and the crop test passes unedited. (6) The four `stacking::measure`
      literal pins re-pinned to R6's values (psf_signal_weight 1.6976e-3 → 1.7025e-3 / 1.7024e-3; the `2026_09_19`
      records' tflux 620.46 → 626.22 / 626.69); no count, β, width or noise/sky field moved. (7) **The wall time** is
      code layout, not work: on the mono frame (5 rotations) base reads 1 740-1 761 ms, round 3 1 856-1 879, round 4
      1 726-1 835, and round 4 with a five-line timer added inside `aperture_correction` 1 659-1 721 - faster than base
      while doing the same work; the `c` computation itself times 5.4-5.7 ms. Nothing to fix.
    - **Fallbacks.** Fewer than 5 ratios (floor (i)): the module's `c = 1`, `aperture_correction = None`, ONE `warn!` per
      plane. In a stacking group (Plan B B4, R-PB4-5, R-OSP-16 B4) such a plane then takes the group channel's median kept
      `c` in the weights when at least 2 peers carry one (`tflux`, `tmean_flux` × m; `psf_signal_weight`, `psf_snr` × m²;
      recorded in `aperture_correction_group`), in `measure_group_finish` only; under 2 peers `c = 1` stays; the module, the
      `.athf` fits and the `metrics` payload keep `None`. No fit on the plane (defocused verdict, starless): `None`,
      `debug!` only, never substituted. A CFA-masked plane (Plan C, no production caller today) gets `c = 1` by these
      rules: **a Plan C carry**. Plan B B4 also narrows the qualifying stars: on a 3-plane colour image the point-source
      screen (PB-12', R-PB4-2), and on every plane `StarFit.snr` ≥ 20 (PB-13'); the top 64 by model flux stands.
    - **Surfaces.** `FluxCorrection::{FromPlane (default), Given(f64)}` on `FitStarsOptions` (LN: `FromPlane` on each
      side), with `full_scale` and the borrowed `footprint` (round 4; `FitStarsOptions<'a>`), and
      `star_measure::fit_stars_with_model` (`fit_stars_at` with β from a `PsfModel`, LN's reference side); `FitOutcome` and both `PlaneMeasurement`s carry `aperture_correction` and `aperture_stars` (the qualifying
      count, at most 64; 0 for a caller's `Given`); the probe's measure and betas frame lines carry `apertureCorrection`
      and `apertureStars`. New log field `aperture_correction` (logging dictionary note, Task 10).
    - **Moved pins (by design, F14).** The four `stacking::measure` fixture pins (tflux 562.457 → 620.462, tmean_flux
      11.956 → 13.189, psf_signal_weight 1.468e-3 → 1.786e-3 on the `2026_09_19` records and 1.387e-3 → 1.698e-3 on the
      field-7 pins; counts, β, fwhm_px, eccentricity, noise and sky unchanged); `fixed_beta_fit_recovers_…_aperture_signal`
      (3 stars, fewer than 5 qualify, so `c` is None and `signal` is the bare model flux, asserted); the stale message
      of `aperture_follows_the_fitted_ellipse_orientation`. The plan's two captured-flux tests render one range-setting
      star of 1.5× the grid amplitude at (25, 25), not seeded: F-3's verdict flags a star whose fitted amplitude tops
      the plane's range, so a field of equal brightest stars has no unsaturated star and `c` would be None (round 1: test 1
      0.74880 uncorrected; with the star 0.99169; test 2 spread 0.77 %, all medians within 1 %).
    - **STOP 2 (round 2) - both tests PASS under R6 (round 3), unedited.** Round 2's record: two LN truth tests failed: `matched_flux_ratio_recovers_a_uniform_scale`
      reads scale 1.23832 against 1.25 ± 0.01, `the_local_spline_follows_a_scale_gradient` reads A(511) 0.81156 against
      0.80013 ± 0.01. Mechanism (round 1, LN fixture, Gaussian FWHM 4.2, 60 stars, true seeds, β 4): noiseless `c` =
      1.0555 at every amplitude; at noise 0.002 `c` = 1.077 / 1.070 / 1.060 / 1.057 / 1.051 at ×0.5 / 0.8 / 1 / 1.2 / 2
      (the fitted model flux reads low at low SNR and each frame's `c` absorbs its own bias), so `c_ref/c_tgt` between
      frames of different SNR is off by 1-2 % and LN's scale inherits it. A shared `c` (`Given(c_ref)` on the target)
      reads ≈ +0.24 % against ≈ −1.36 % per side (rough). The controller rules after the lab step.
    - **STOP 3 (round 3) - closed by the round-4 rulings but (c)'s first test, STOP 4 below.** Round 3's record: five truth tests failed under R6 and stayed as they were:
      (a) `captured_flux_is_the_total_flux_on_a_moffat_2_5_field` 0.98164 and the Moffat-2.5 median of
      `captured_flux_spread_across_profiles_is_within_two_percent` 0.98243 (bar 1 %; spread 1.79 % passes): the 70-px
      grid is 12.1 F at FWHM 5.78, so the neighbours' β-2.5 wings beyond their `3F` discs sit in the far ring and the
      plain mean takes them (`b` +8.5e-5 native); the lab's own `growth_mean_rim` / `union_mask` give 0.98164 on the
      same field, and 0.9946 on a 140-px grid. (b) `ln_refit_takes_a_given_correction`: `c` None - its 30 equal stars
      fit amplitude 19 660.5009 against the range 19 660.4949 (`plane max − sky median`, the median lifted by the
      wings), so every one is screened; with round 2's range-setting star (1.5×, unseeded) `c` = 1.21603 over 30 and
      the assertions hold. (c) `masked_ln_half_coverage_agrees_with_legacy_full_coverage_under_the_production_kernel`
      (half 1.29403 vs full 1.25211, 3.35 % against 0.4 %) and `masked_ln_recovers_the_half_covered_scale_from_measured_seeds`
      (1.30677 vs 1.25 ± 0.0125): the target's `c` 1.0226 (20 ratios) against the reference's 1.0662 (30) - LN keeps
      only the overlap's seeds, the stars beyond it still light the target plane, they sit 46 px (≈ 11 F) from the
      overlap-edge column, in its far ring, unmasked; a noiseless replica reads those 5 ratios 0.28-0.60 against the rest's 1.111,
      a quarter of a small sample. Also moved: `stacking::run::a_full_rectangle_crop_writes_what_the_zero_crop_path_writes`
      (EXPTIME 224.06130914091074 vs 224.0613091409108, one ulp, deterministic; the second run reads cached
      measurements - plausibly serde_json's float parsing, no `float_roundtrip`) and the four `stacking::measure`
      literal pins (re-pin owed after the ruling).
    - **STOP 4 (round 4; closed below).** With the footprint, `masked_ln_recovers_the_half_covered_scale_from_measured_seeds`
      passes unedited (1.25060), `masked_ln_half_coverage_agrees_with_legacy_full_coverage_under_the_production_kernel`
      does not: half 1.23724 vs full 1.25211 (1.19 % against 0.4 %; |half − 1.25| 0.0128 passes 0.0331). Per side: the
      reference's `c` 1.06911 over 26 (full: 1.06908 over 40), the target's 1.07248 over 12 (full: 1.06014 over 40) -
      the whole move is the target's `c` on 12 qualifiers. A same-plane check reproduces both target values to 7
      digits on the fixture's target smoothed by the B-spline kernel at integer positions ([1, 4, 1]/6, no prefilter):
      there the per-star growth/model ratios spread 0.96-1.15 and the 12 inside the overlap footprint have a median 1.2 %
      above the 40; on the raw (unsmoothed) plane the 12 and the 40 agree within 0.02 % (1.06891 / 1.06908).
    - **Real frames** (LDN_1272 `…_0000` mono and `…_0000_d` OSC, release probe, `--paths measure`, base = Task 9 close
      → F-5, 3 interleaved runs; stars fitted, β and frame FWHM identical): mono β 2.000, FWHM 2.226 px, 12 748 fits,
      `c` 1.3967 (64 qualify), sum of star signal 8 263 → 14 841, wall 1 715-1 870 → 1 723-1 768 ms; OSC R / G / B `c`
      1.3384 / 1.2529 / 1.2246 (64 each), sum of signal 3 675 / 4 269 / 3 783 → 5 427 / 6 050 / 4 910, wall unchanged
      within noise. **Round 3 (R6)**, same frames and base: fits, β, FWHM and every star line but `signal` identical;
      mono `c` 1.3967 → 1.3999 (64, 21 candidates screened), tflux 14 876; OSC R / G / B `c` 1.3579 / 1.2747 / 1.2286
      (64 each), tflux 5 506 / 6 155 / 4 926; the `c` computation times 5.5 ms on the mono plane, yet the mono wall reads
      1 817-1 856 ms against base 1 682-1 809 (6 interleaved pairs; OSC +3-5 %) - not explained by `c` (round 4: code
      layout, above). **Round 4**, same frames and base: the measure path is unchanged from round 3 (the footprint and
      the full scale are not on it) - fits, β, FWHM identical to base, mono `c` 1.39994 (64), OSC R / G / B 1.35791 /
      1.27471 / 1.22859 (64 each); mono wall base 1 688-1 757 vs round 4 1 780-1 837 (5 interleaved pairs).
    - **STOP 4 closed (RF7, controller ruling 2026-10-11).** `masked_ln_half_coverage_agrees_with_legacy_full_coverage_under_the_production_kernel`:
      both truth assertions stay as they were; only the half-vs-full consistency bound moved 0.004 -> 0.030. F-5 gives
      each LN side its own aperture correction; with 12 footprint qualifiers on the target the half/full ratio has sd
      0.99 % over 30 noise seeds (passes 0.4 % in 7 of 30), so 0.030 = 3 sigma; the truth assertions pass on 30 of 30
      seeds. Real LDN_1272 (RF7): registered planes give the best `c` (per-star MAD 3.6 % vs 8.1 % calibrated, bootstrap
      SE 0.59 % vs 1.09 %, frame-to-frame spread 1.41 % vs 2.02 %); the pixel-phase effect is real only on undersampled
      calibrated planes (14 % swing) and the B-spline warp flattens it (+-0.5 %); an LN-like check on registered targets
      reads the c-corrected model ratio against the model-free growth ratio at +0.18 % (sd 1.08 %). A pixel-integrated
      denominator gains nothing, a shared star set is worse on real frames (1.41 -> 1.64 %).
    - **Why a plain mean in the far ring.** Neighbour light is linear, so the mean takes it in proportion and the
      seed-disc mask removes the neighbours' cores; a median reads it non-linearly (part 1's L3 sky mechanism). The
      price is paid where the sky is structured (below), and is recorded, not patched (owner rule: no second pass).
    - **Acceptance (lab step `t10-f5`, r2 at e3312b03, lab 39bb670): BAR PASS**, every number equal to the RF5c
      prediction to the printed digit. C1.1 spread at F 2.6 / F 8.0 **0.76 % / 1.38 %** (bar 2 %); C1.2 psfSel
      `pairs/iso/blur25mix` t60 **+1.72 %**, `pairs/iso/blur4` t60 **+1.19 %** (bar +-2 %); C1.3 pure families **0 / 70**
      over 1.2 % (worst -1.09 %); C1.4 mixtures **0 / 20** miss (worst +1.72 %); control, register guard and both
      no-regression diffs PASS. r1 was read and is the reason for R6 (C1.1 0.88 / 1.61 %, LN `c` wrong).
    - **Known limits (recorded, not judged; owner rulings C6, C7, RF8: KEEP R6, no per-star residual screen).**
      (1) Nebula structure: `c` is biased where the sky has structure the plain mean cannot model - `neb3` row 6 0.8802
      (c off -11.5 % +- 5.6 % SE), `neb3_4096` 1.1287 (+13.9 % +- 2.3 %; stars selected on bumps, p 0.002), `neb0.5`
      0.9885, `neb0.1` 0.9964; against t09 the move is `c` itself (+0.05 to +0.30, 10-400 SE). Only a background model
      fixes it (out of scope). (2) Crowded wide frames +-4 % (`pairs/main` row 6 0.945-1.029, worst 5.48 %).
      (3) The injected-star rows (calibrated inject +4 to 5.5 %; raw mono2 OSC planes 1.47 / 1.74 / 1.70) are an
      injection artefact: `c` is set by the frame's own stars (real PSF, beta at the 1.5 cap on raw OSC) and the injected
      stars' profile is not the frame's PSF; their 5F sums are right. (4) hc11B L 0010: `c` 1.137 +- 25 % (17 of 63
      ratios > 5) - a nebula SEED FLOOD (the one frame at the 24 576-seed cap; field width 5.14 vs F 7.48, so isolation
      leaves junk qualifiers), the same mechanism as Task 13's M42 frame-beta MUST-RESOLVE; fixed once, at the seeds.
    - **Carries.** Task 13: re-read hc11B L 0010's `c` after the seed fix; the nebula-background `c` bias (numbers
      above); the OSC LN reference R-plane `c` SE 2.19 %. Plan B: growth/model falls for fainter stars (all-pairs
      LN-like -4.7 %). Plan C: a CFA-masked plane gets `c` = 1 (no qualifier).
  - **R-OSP-7b - one background: the aperture-scale sky B_i and noise σ_i per star (F-10, Plan A Task 10b, rulings
    A1-A6, B1-B5, P1-P5, R1-R3, Q1-Q7 and the cycle's A1-1 … D-1 below; no `MEASURE_VERSION` bump, PA-4; B_i, σ_i
    and n_i are not persisted, like `peak5`).** Detection (A1, A1b, A1c) is recorded under R-OSP-2 and the field width
    (A2, A2b) under F-1's field width; this entry carries the per-star sky, the trails, every ruling and the lab cycle.
    *Phase A3 (2026-10-11):* every accepted fit carries `sky_ring` (B_i: the plain mean of its present pixels at
    `10F ≤ d ≤ 12F` outside every seed's `3F` disc, R6's ring, row-major sum), `sky_ring_sigma` (σ_i: the RMS of
    `v − B_i` over the same pixels) and `sky_ring_n`, read ONCE per fit after the duplicate drop
    (`photometry::ring_skies` / `ring_sky`); `F` = the plane's frame FWHM, or `FitStarsOptions::ring_fwhm`. An edge
    star reads its partial ring; under 8 pixels (or no frame FWHM) the ring is missing (NaN, NaN, 0) and counted in
    `ring_sky_missing`. `c` reads the stored B_i (`growth_curve` takes it; the qualifier rule is `c`'s own, plus "B_i
    present"), so `c` is R6's by construction. The fit's `b` stays free (B1); `StarFit::sky_offset` = `b − B_i` is the
    near/far diagnostic. `annulus_sigma` is test-only (`#[cfg(test)]`, D-1; B4: Task 11 reads σ_i). LN:
    `ln::scale::RingGeometry::{PerSide (default), PairCommon}`; pair-common (B3's arm, `ln_scale_probe --ln-ring
    pair-common`, `PreparedReferenceChannel::build_pair_common`) reads both sides at `max(F_R, F_T)` after both are
    fitted: the side whose own F is smaller re-reads its rings and `c` through `star_measure::correct_at_ring`
    (`FitOutcome::fit_seeds` keeps each fit's seed), so nothing is fitted twice; the reference keeps a copy of its
    plane, seeds and footprint only under that arm. **The `c` gate:** release `star_probe` at A2b vs A3 over v2's quick
    families, the LDN pair and the 19 inject.json measure frames (49 planes): `apertureCorrection` and `apertureStars`
    byte-identical on every plane, and every other line identical once the new keys are removed. Proof (σ_i/σ_MRS
    median): clean lab families 1.00-1.01, real mono frames 1.00-1.08, debayered colour planes 1.24-1.58 (every
    one of them, G highest; not examined in A3 - an interpolated plane's pixel scatter against the MRS's finest
    scale is the candidate, Task 11 reads σ_i), `neb0.5` 1.31, `neb3` 4.27 (the ring
    spans the nebula's structure); `skyOffset/σ_i` median -0.15…+0.30, p90 0.03-1.9 (`doubles/f2.6` 7.0: a neighbour
    inside the fit region); `ringSkyMissing` 0 on every plane.
    *Phase A4 (2026-10-11): the trailed-frame verdict, from the refusal structure (rulings A6, Q1, Q5, Q6).* No
    per-star streak refusal (an elongated star is a real star, measured as such; ecc is never a gate). A trail longer
    than its fit region cannot be contained, so its seed ends `centreMoved` / `regionTooSmall` / `centreAtRim` (A4-2):
    `fit::trail_share` = those three over the judged seeds (every seed the fit call gave an outcome), and a plane with
    at least `fit::TRAIL_VERDICT_MIN_SEEDS` (100, the plate solver's own minimum, quoted) judged seeds and a share
    above `fit::TRAIL_REFUSAL_SHARE` (0.5, the majority rule: most detected sources cannot be contained) is TRAILED,
    as the defocused verdict: every fit it would have accepted is refused `Trailed` (`RefusalReason::Trailed`, serde
    `"trailed"`, declared last; `ALL` 13 -> 14), `stars` empty, `c` None, the zero-star path gives PSF signal
    weight 0. Measure path only (`FitStarsOptions::trail_verdict`, default `false`; LN's refits never see it); a
    defocused plane keeps its own verdict. `FitOutcome` / `PlaneMeasurement` (both) report `trailed` and `trail_share`
    (on every plane); one adapter `warn!` per trailed plane; `star_probe`'s measure frame line carries `trailed` and
    `trailShare`. Step 0 proved LN's default ring arm unchanged by A3 (`ln_scale_probe` A2b vs A3, two pairs, 19 670
    lines identical). Proof (release `star_probe` at A3 head vs A4, measure path, matched seeds at 6): the 10 clean
    hc11B L frames of 2024-10-06 share 0.023-0.136, no verdict, fits identical; L 0010 / L 0011 0.914 / 0.933,
    verdict; the `trails/` frames OSC 0022 / 0032 (every plane 0.910-0.962), L 0028 / 0032 (0.880 / 0.883) verdict, G
    0009 (`elongated`) 0.277 and O 0004 (`doubled`) 0.021 none, H 0029 0.419 none (Q6's recorded miss). **Open (STOP
    to the controller): G 0003 (`elongated`) reads 0.598 (inject) / 0.623 (plain) at A3's head - 0.389 / 0.394 at
    fee44d50, A1 0.472 / 0.507, A2 0.625 / 0.648 - so the majority rule fires on it**; the plate solver's own rule
    calls it trailed too (367 of 402 detections streaks).
    *Phase A4b (2026-10-11, ruling A4-2): the centre test is the fit's own half-maximum contour; the share counts every
    containment refusal.* `CentreMoved` is now "the seed lies outside `(u/α_x)^2 + (v/α_y)^2 <= 2^(1/β) - 1` in the
    fit's axes at θ" (`fit::inside_half_max`, read by `fit::placement` for `accept` and the verdict fit's `centred`):
    a round FWHM-3 star keeps today's 1.5 px, an elongated star owns the seeds along its major axis. The fixed
    `FitParams::centroid_tolerance_px` was a per-star streak refusal by the back door (Q1) and is removed. The trail
    share is `centreMoved + regionTooSmall + centreAtRim` over the judged seeds (a streak fit no longer caught by the
    fixed test reaches the rim test). LN's seed link (`ln::scale::link_barycentres`) reads the same predicate: a fit
    links to the nearest seed inside its own half-maximum contour (every seed within `fwhm_x / 2`, nearest first,
    `KdTree2::within`; the first that passes the ellipse test, none otherwise - D-1 M-4).
    Numbers: `.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-10b-report-A4b.md`.
    - **The design rulings (2026-10-11), one line each.** A1 σ_f propagated (the mesh's noise map × a gain), K 6.0
      kept (clean field measured / propagated 1.00-1.04). A2 the 128-px mesh stays the plane's one map. A3 two knot
      tests from a Moffat β 1.5 star's ratios and the noise at z 3.09 (cost: doubles / crowded −2.7 pp at most). A4
      the β sample only from verdict fits that pass the centre and region tests. A5 the field width is biased (M42 8.5
      px against fitted 2.4) and 10b owns it. A6 trail segments are not stars (L 0010: 947 accepted fits at the
      24 576-seed cap against 2 458 clean, `c` 4.48, weight 0.024 of the group max). B1 the fit's b stays free (fixing
      it triples the nebula FWHM scatter); one sky per scale. B2 B_i on R6's ring (the only geometry passing every F-5
      bar). B3 LN's pair-common ring only if it passes the nebula-pair bar. B4 σ_i = the RMS of (v − B_i) over B_i's
      ring (smooth sky 1.00); the [3F, 4F) annulus MAD retired for SNR. B5 the nebula `c` bias recorded (`neb3` 0.882
      ± 5.6 %, `neb3_4096` 1.128 ± 2.3 %). P1 the spec row is F-10. P2 the M42 β is reported; its bar moves to Task
      10c (the near-clip class). P3 `extended` leaves the β sample, `c`'s qualifiers and the frame statistic. P4
      recall floors reconciled before the run, never a bar move. P5 a test whose premise fails is a STOP with numbers.
      R1 frame classes `trailed` / `elongated` / clean. R2 a bar not derivable before the cycle is REPORTED. R3 the
      runner's baselines re-measured before C. Q1 no per-star streak refusal (an ecc line at 0.8 refuses 2-32 % of
      clean real fits). Q2 G 0003 is measured as is. Q3 D.1's floor = the arm − 2 SE. Q4 one mesh per plane, shared.
      Q5 the trailed line is the majority rule, share > 0.5 (the line derived from the clean frames fired on G 0003 at
      0.389). Q6 H 0029 (0.414, fittable dwell cores) is a recorded miss. Q7 K.1's allowance is the one-sided 97.7 %
      binomial quantile at max(p_exp, 1/n).
    - **The cycle's rulings (2026-10-11 to 2026-10-12), one line each: what was decided, why, the number.**
      - **A1-1** the clean-field test judges the PROPAGATED σ_f against the fixture's true noise (4 %): the measured
        σ_f reads 1.121× the true noise (the bright stars' filtered wings), the propagated one 0.997×.
      - **A1b (C-7)** gain := 1 on a plane whose pixels were never mixed: the read leaked nebula texture (1.81 on
        `neb3`, 1.07 on `neb0.5`, 1.48 on `neb3_4096` against 1.003-1.038 on real mono; truth 1.000), and every one of
        the 38 stars D.1 missed clears K at gain 1 (`neb3` 301 → 339 of 629). A structure term instead was rejected
        (debayered OSC 1.36 / 1.74 → 0.93 / 1.09).
      - **A1c (C-10)** the caller states the noise's spatial correlation (`PixelNoise` White / Mixed / Unknown,
        default Unknown = read): A1b keyed on `colour` and missed resampled mono planes (`resampled/f2.0`, `/f3.0`
        gain 1.88 / 1.95 → 1, false seeds 0 → 1 147 / 1 047, about 270 per Mpx); at A1c 0 / 0 again.
      - **A2-1** the knot bounds move to the star-width envelope F_env = F_field + 3.09·s: at the mode half the real
        stars are broader (LDN mono `extended` seeds 128 → 6 381 at A2).
      - **A2-2** s = E4, the mirrored lower-half spread of the reads ≥ 0.715 F (= 1 − 3.09·e(10)), e(ν) =
        √((0.899/ν)² + 0.0208²) fitted on 12 constant-width lab families: single-pixel events (1.3 px reads, 25-29 %
        of G's readable reads) set the plain lower half (G F_env 1.8-2.8 F, the test dormant). LDN mono's β 2.0 → 1.5
        is its whole β pool at the full well - Task 10c's class, reported, not a bar.
      - **A2-3** s = E4n, E4 with the reads' own error removed in quadrature: on constant-width knot fixtures E4
        invented F_env / F 1.08-1.11 (flat knots passing 33 → 54), E4n reads 1.00. Known cost: extended fits L 0000 20
        → 48, M 0000 86 → 211, LDN mono 164 → 251; LDN OSC plane 1 at the 1.5 px clamp falls back to F_env = F.
      - **L3-1** C's pooled r ≥ 2 knot bar is Q7's allowance per sky (3 or 4 of 216); K.1 / K.3 read the product's
        `fieldWidth` / `fieldWidthEnvelope`; pre-A1 builds are judged pooled only (fee44d50 / t10-f5-r2: 185 / 103,
        the before).
      - **A4-1** the majority rule stands and G 0003 stays `elongated` when A4 fired on it (0.598 / 0.623, 0.389 at
        fee44d50): the rise was the seed position under A2's field width (672 of 1 213 `centreMoved` seeds > 1 px from
        every A1 seed) against a fixed 1.5 px centre tolerance - a streak refusal by the back door, so the fix belongs
        to the centre test, not to the rule.
      - **A4-2** the centre test is the fit's own half-maximum contour (96 % of G 0003's `centreMoved` seeds lie
        inside their fit's ellipse, the fit 0.15 px from the truth); the share counts `centreAtRim` too (L 0010's rim
        refusals 0 → 3 571 once no fixed test catches the streak first); LN links by the same predicate. Shares then:
        trailed 0.882-0.92, G 0009 0.326, H 0029 0.447, G 0003 0.175 / 0.176, O 0004 0.015, hc11B L ≤ 0.15; G 0003 β
        1.75 → ≈ 52 and fits 180 → 880, reported.
      - **C-1** D.1 is judged on D2's own quantity, found over all 629 truth stars (D2's HEAD 0.591 × 629 = 372 = the
        scorecard's found): floors 0.743 / 0.499; the scorecard's found / expected (532 / 511) printed beside.
      - **C-2** the seed set moves on every family by design (σ_f about 3 % lower on flat, D.6 0.969): a population
        move is set aside only where the same truth stars measure better or unchanged within SE in every bin.
      - **C-3** D.3's `false ≤ base` at base 0 is not derived: it is read at D.4's 0.5 per Mpx (one `nothing` seed on
        `shape/f2.6/e0.3/t60`, 0.24 per Mpx, 30.5 px from a star).
      - **C-4** D.1 `neb3` 301 of 629 against 314: diagnosed before any ruling (→ C-7).
      - **C-5** (a) same-star materiality: unchanged within max(2 SE, 1 % of the base mean) (1 % is the spec's
        tightest accuracy tolerance; at 10⁵ pairs the sign test flags 1e-12 drifts); (b) the seeds-path bright
        centroid (Fisher > 300: 0.0384 → 0.0435 px) is the seed's ±⌈F_field⌉ box at the corrected width (⌈F⌉ 4 → 3, 5
        → 4; a sawtooth in sub-pixel phase): accepted, no consumer reads a seed at 0.05 px (the fit recovers it,
        0.0176 → 0.0173).
      - **C-6** the 19 faint-bin WORSE same-star cells sit on planes where A2's field width came down toward the truth
        (`saturated/f2.6/n60` 3.99 → 2.89, `density/d600` 4.83 → 3.31; truth 2.6-3.0): accepted as the corrected
        width's cost.
      - **C-6b** C-6's check over the 85 β-changed planes (76 pass, 8 within 0.2-1.5 % of the base F, 1 n/a): the
        cells are re-split by plane.
      - **C-6c** the 9 planes' 5 WORSE cells exceed their lines by ≤ 1.2× (≤ 0.003) where β moved 0.001-0.004, and 645
        cells give about 15 WORSE by noise alone (allowance about 22): accepted; from here on the WORSE count prints
        beside its binomial allowance and one cell beyond 2× its line is a STOP.
      - **C-7** = A1b. Recorded limits: debayered colour planes keep the leak (Task 11 owns their noise correlation);
        the mesh noise map reads ≈ 6× the true noise on `neb3`, so heavy-nebula recall stays ≈ 0.54 until a later
        owner.
      - **C-8** row 7's frame ecc / FWHM moved on 604 rows (median +0.22 %, p90 +1.1 %); with the same stars unchanged
        only the population can move the frame statistic → checked on the base's own stars.
      - **C-8b** that frame SE (about 0.01 % of the value) is no materiality line: C-5 (a) applies to row 7 and D.8
        (ecc beyond 2 SE on 46 of 142 planes, median 0.00 %, max +2.4 %).
      - **C-9** P.3, below.
      - **C-10** = A1c.
      - **C-11** on a lab family the truth, not the base, referees row 7: a plane beyond materiality that sits closer
        to the truth counts BETTER (A1b run: ecc beyond materiality on 34 of 331 planes, 13 beyond 2×, doubles −7.4 /
        −8.3 %).
      - **C-12** C-11's result: 37 cells, 24 BETTER (the blend families: `doubles/f4.0` FWHM 4.777 → 4.457, truth
        4.062), 13 WORSE (ecc +0.002…+0.007 on round truth, allowance 3), 4 beyond 2× → a mechanism first.
      - **C-12b** the 4 move at A2b and stay; no region-size dose-response (Spearman 0.10); 82-85 % of the OSC planes'
        bright / mid stars shift one way, a frame-β signature → split on the fixed-β arms.
      - **C-12c** `osc/f2.6/m4` p2 and `osc_pairs/f2.6` p1: no shift at fixed β, the frame β moved (3.482 → 3.397,
        4.661 → 4.456; truth 4.0) - accepted, F-3 / F-4 judge β; `twins/sharp/blur1/f8.0`: the weights only (fits 587
        → 526) - C-8; `twins/sharp/m2.5/f3.5` persists at fixed β (+0.0046 ± 0.0017) → the pinned-width check.
      - **C-12d** forcing that plane's F_field back to the base's 5.304 reproduces the base (frame ecc 0.1096 against
        0.1094, head 0.1128; β 2.5022; the common stars' ecc per SNR bin equal to 4 digits): the whole shift is A2's
        corrected width (5.30 → 3.78, truth 3.5), accepted. A later fit-design task owns whether shape wants a region
        wider than the F-sized windows (β 2.5: +0.003 ecc on bright round stars, +0.02 at SNR < 20 at the true width).
      - **D-1** the Phase D review (0 Critical / 3 Important / 10 Minor) accepted whole: the trail share's
        documentation; this record; the masked-plane `N*` recorded, not reverted (R-OSP-2); LN's link takes, among the
        seeds within the radius, the nearest that passes the contour (M-4); `FitOutcome.ring_fwhm` carries the ring
        width `ring_skies` chose and LN reads it (M-5, byte-identical); `annulus_sigma` test-only; serde_json's
        `float_roundtrip` kept and named in the F-5 commit (M-6); LN's half / full bound 0.004 → 0.030 named as RF7's
        ruled re-pin in the F-10 commit (M-13). `star_probe` and LN's library outputs byte-identical to 4c95b6b4 (the
        LN probe's replication arm moved by design, M-12).
    - **P.3 = per-side (ruling C-9).** B3's rule: pair-common iff P.1 passes under it and C1.1-C1.4 hold. It does not:
      pair-common clears a few cells and adds new misses (`neb0.5` m4 t60 +9.5 pp, `neb3` g t25 +7.7 pp), 15 of 34
      cells over against per-side's 13. Per-side's misses (worst `neb3` g t45 −15.9 pp, `neb3` b25mix t25 −12.3 pp,
      `neb0.5` m4 / g +1.5…+3.8 pp; shifted / warped t25 ≈ +1.9 pp against ±1.2) equal the 10b-L before cell for cell:
      10b does not touch LN's per-side sky, the error lives in LN's own tile sky on nebula (`ln/background.rs`, Plan
      B) and in sub-10F nebula structure (B5). A recorded limit, reported to the owner; the P.1 / P.2 tables are Plan
      B's before. No switch to K7. (Plan B B5, R-OSP-16 B5: the cause is not `ln/background.rs`; psfSel's cells are
      `c`'s on nebula sky, owner F-10 / B.3, and F1's own four are its median sky's limit, R-PB5-1 / -3.)
    - **Phase C - the joint cycle `t10b-onebg` (runner; athenaeum-lab `star-lab/results/plan-a/t10b-onebg/`).** First
      run at 6a1addc6 (lab 895136a): STOPs on D.1 `neb3` (→ C-7, A1b), P.1 / P.2 (→ C-9) and row 7 (→ C-8b). Second at
      7fd911ab (lab 07cd2cb): STOP-A, resampled mono planes flooded (→ C-10, A1c); STOP-B, row 7 on the base's stars
      (→ C-11 … C-12d). **Third at 4c95b6b4 (lab 4f9e21e, star_probe ea7faf29d0bc; base t10-f5-r2, the F-5 cycle):
      every bar met or reported, no STOP open**, lab suite 724 OK. The images that interpolate (`resampled/f2.0`,
      `/f3.0` and the 18 shifted / warped LN targets) run with `--pixel-noise mixed` (C-10); the rest on the probe
      default.

      | Cell | Bar | Value (third run) | Verdict |
      | ---- | ---- | ---- | ---- |
      | C1.1 | F-5 spread at F 2.6 / 8.0 ≤ 2 % | 0.76 % / 1.41 % | ok |
      | C1.2 | psfSel `pairs/iso/blur25mix` / `blur4` t60 within ±2 % | +1.72 % / +1.19 % | ok |
      | C1.3 | pure pair targets within ±1.2 % | 0 of 70 over (worst +1.07 %) | ok |
      | C1.4 | mixture pairs no worse than t01c + SE | 0 of 20 over | ok |
      | F.0 | accepted + refused = seeds on every `fit outcomes` row | 885 of 885 | ok |
      | no-regression | v2 + inject against t10-f5-r2 (final design list); cumulative against t01c; legacy control | 0 moved, 0 missed, 0 lost; PASS; 0 mismatches | ok |
      | D.1 | `neb0.5` / `neb3` found of 629 ≥ 0.743 / 0.499 (C-1); false ≤ 3 per Mpx | 489 = 0.777 / 339 = 0.539 (base 0.591 / 0.343); 1.67 / 2.38 | ok |
      | D.2 | `halo` false seeds ≤ 5 | 2 (base 256) | ok |
      | D.3 | completeness ≥ base − 0.005, false ≤ base (C-3), the flat families with `resampled/*` added | all 93 completeness rows in line; `resampled/f2.0`, `/f3.0` 0.986 / 0.985, false 0 / 0 | ok |
      | D.4 | twins and artefacts `nothing` ≤ 0.5 per Mpx | 0.000 | ok |
      | D.5 | doubles, `d1000` ≥ base − 0.01; density pairs ≥ base − 0.03 | `doubles/f2.6` 0.772 (0.734), `/f4.0` 0.629 (0.632), `d1000` 0.892 (0.837); pairs 0.865-0.971 (0.852-0.977) | ok |
      | D.6 | flat σ_f propagated / measured 1.00 ± 0.04 | 0.973 | ok |
      | D.7 | `neb3_4096` (reported) | completeness 0.719 (0.447), false 30 (0); D2's quantity 1 475 of 2 517 (arm 1 421) | reported |
      | D.8 | frame FWHM / ecc / `c` on the clean families | in the no-regression diff; row 7 on the base's stars: ecc beyond max(2 SE, 1 %) on 34 of 331 planes, 13 beyond 2×; against the truth (C-11) 24 BETTER / 13 WORSE, the 4 beyond 2× closed by C-12b (2), C-8 weights (1), C-12d (1) | ok (C-8b, C-11, C-12d) |
      | K.1 | accepted unflagged knots per bin of FWHM ≥ 2F ≤ Q7's allowance; pooled ≤ Q7 per sky (L3-1) | no bin over on any sky; pooled 1 / 1 / 2 of 216 on flat / `neb0.5` / `neb3` (allowance 4 / 3 / 4); the 1.5F row reported (0 / 1 / 2 of 6 bins over) | ok |
      | K.2 | truth-star completeness ≥ base − 0.01 | 0.987 / 0.928 / 0.655 (0.978 / 0.714 / 0.424) | ok |
      | K.3 | knots dropped / extended by bin (reported) | of 576 knots accepted: flat 466 (227 extended), `neb0.5` 382 (188), `neb3` 73 (34); truth stars flagged extended 0 | reported |
      | N.1 | density × nebula completeness ≥ base − 0.03; `c` from ≥ 5 per plane | 0.607-0.862 on the six planes (0.367-0.681); 64 qualifiers each | ok |
      | W.1 | M42 0000 / 0008 / 0010 injected above threshold found ≥ 99 % | 45 of 45, 41 of 41, 45 of 45 (plain 57 / 60, 59 / 62, 58 / 62) | ok |
      | W.2 | clean G / L inject frames found ≥ base | G 0001 60 / 61 (60), G 0002 60 / 61 (60), L 0000 60 / 61 (51), L 0008 59 / 60 (51) | ok |
      | W.3 | F_field / frame FWHM (reported) | M42 inject F_field 1.95 / 1.74 / 1.90 px, frame FWHM 2.34 / 1.64 / 1.89 (F_field before 8.2-8.8) | reported |
      | T.1 | L 0010 / L 0011 accepted fits ≤ 24 | 0 / 0 (share 0.916 / 0.933) | ok |
      | T.2 | their PSF signal weight ≤ 0.00182 | no fit, so no weight inputs (the zero-star path) | ok |
      | T.3 | their `c` None | None / None | ok |
      | T.4 | the 10 clean L siblings: no verdict; fits A3 → head within √A3 | shares 0.005-0.111; −8…+23 (√A3 45-47) | ok |
      | T.5 | the 7 `trails/` frames: verdict = rendered class | OSC 0022 / 0032 (0.908-0.962 per plane), L 0028 / 0032 (0.880 / 0.883) trailed; G 0009 `elongated` 0.326 and O 0004 `doubled` 0.017 none; H 0029 0.445 none (Q6's recorded miss) | ok |
      | T.6 | every other frame: no verdict | G 0003 0.176; inject frames 0.020-0.203; none fires | ok |
      | T.7 | long-trail family (reported) | the share crosses 0.5 at L / F 5.70 / 5.58 / 5.53 (F 2.6 / 4 / 6), about the fit region's radius (diameter ≈ 12 F); lab `trails` 0.299, no verdict | reported |
      | T.8 | G 0003, `elongated` (reported) | no verdict, share 0.176, 907 fits, frame ecc 0.646, `c` 1.177, β 49.6 | reported |
      | B.1 | σ_i / truth noise median 1.00 ± 0.05 on smooth sky | every row within (55 at the first run; ok at the third) | ok |
      | B.2 | B_i − `sky_local`, b − B_i (reported) | not printed by the cycle; A3's proof table: `skyOffset / σ_i` median −0.15…+0.30 | reported |
      | B.3 | nebula `c` (recorded limit) | density × `neb3` row 6 d200 / d600 / d1000 1.058 / 1.133 / 1.087 (before 1.058 / 1.135 / 1.069); `neb3` 0.880 beside | recorded |
      | P.1 | psfSel on nebula-sky pairs, Δ against flat, both ring arms | per-side 13 of 34 cells over (= the before), pair-common 15 | reported (C-9) |
      | P.2 | shifted / warped targets within the unwarped twin's tolerance | 3 cells at t25, +1.91…+1.96 pp (= the before) | reported (C-9) |
      | P.3 | B3's decision | per-side | C-9 |
      | I.1 | inject row 6 Δ ≤ 2 % + 2 SE_c per clean frame | −0.025…+0.009 on 7 frames (allowances 2.80-4.74 %); G 0003 and L 0010 out | ok |
      | S.1 | M42's 12 calibrated frames, `c` sd ≤ 1.46 % | 0.67 % (base 1.02 %) | ok |
      | S.2 | seeds per Mpx per clean frame (reported) | max / median 1.04 (M42, 583-675 per Mpx), 1.19 (hc11B G), 1.06 (hc11B L) | reported |
      | S.3 | M42 frame β 0000 / 0008 / 0010 (reported; Task 10c's bar) | 31.8 / 2.81 / 3.38 (t10-f5-r2 13.96 / 3.14 / 3.34) | reported |
      | M.1 | m42 corpus Measure batch ≤ 159.8 s | 74.7 s (base, interleaved, 133.5 s) | ok |
      | M.2 | LDN 1272 (dev Mac, reported) | `c` sd 3.30 → 3.56 % (measure), 2.02 → 2.44 % (betas); mono β 2.0 → 1.5 (Task 10c) | reported |
      | R.1 | fits wider than 2F (reported) | `neb3` 0 → 5; density × `neb3` d1000 1 → 25; M42 inject 174 → 145, 130 → 64, 119 → 74 | reported |
      | R.2 | saturated share of Σ signal (reported, Task 10c) | M42 mono2 0.79-0.90 (base 0.82-0.91); hc11B G 0.62-0.73 | reported |
      | same-star | C-5 (a), against t10-f5-r2 (1.31 M paired stars) | 645 cells: 46 better, 580 unchanged, 19 worse (allowance 23) | ok (C-6c) |

      Also reported: M42 mono2 0000's seeds 7 717 → 15 272 (9 221 added: 8 008 fitted, FWHM / F p50 1.18 against the
      base's own 1.21 - faint real stars; 1 213 unfitted, 653 of them on the clipped Trapezium core plateau - Task 10c);
      `neb0.1` false seeds 0 → 13 (3.1 per Mpx); `donut/r10` `nothing` seeds 14 → 315 (since A1; donut accepted 0); the
      debt check against t01c OPEN on three scatter rows (Task 14 judges).
  - **R-OSP-7c - the saturation flag's near-clip class: clause 3, read against a clip floor derived per plane (F-3,
    Plan A Task 10c, rulings R10c-0 … R10c-11 below; no `MEASURE_VERSION` bump, PA-4; the floor is not persisted).**
    *As built (9f2cafc7 the class at the fixed 0.9·range band; 7933f582 the floor derived per plane).* Clause 3
    `by_near_clip`: the plateau test did not pass AND the 3×3 maximum about the seed pixel (`SaturationVerdict.top3x3`,
    the region's present pixels) ≥ the plane's clip floor; `saturated` = clause 1 (A > range) ∨ clause 2 (plateau ∧
    pixel0 − bg ≥ 0.9·range) ∨ clause 3; it holds when the verdict fit fails; level and fit unchanged. The floor
    (`photometry::derive_clip_floor`, once per plane after the verdict pass, before the multi-plane rule;
    `mark_near_clip` re-reads clause 3): evidence = the plane max when ≥ 2 present pixels equal it (`max_count`, from
    the plane-max reduce, no extra pass) ∪ pixel0 of every seed clauses 1 AND 2 both flag; floor = min, no margin;
    none → `clip_floor = None` → no class B. Floor ≥ bg + 0.9·range always, so class B ⊆ the knot exemption's
    `SaturationPlane::near_clip`, which keeps the fixed band (it runs before any verdict) — R10c-1's "one function"
    is now a documented superset; detection unchanged. Probe: frame `clipFloor` (range units, null = no evidence) and
    `clipEvidence`, verdict `byNearClip` and `top3x3` (both `PlaneMeasurement`s carry `clip_floor` / `clip_evidence`).
    Spec §2.1 (the clip floor, limits (a)–(g)) and §3 F-3 "as built" follow it. The fixed band's F14 fixture changes
    and the four `stacking/measure.rs` re-pins (149 → 122, 144 → 125) were reverted at 7933f582: a clip-free plane has
    no class B, so they hold at 3deb6cb5's numbers again; the β test adds noise before the clip (an exact tie).
    - **R10c-0 the bar (fixed before code).** The stub's min–max form fails for reasons outside 10c (0002's own PSF,
      β 4.9–5.6 in every bin), so the robust form (`bar3.py`): M = the 11 other M42 mono2 L calibrated frames, C = the
      hc11B L replica 0000–0009, s = 1.4826·MAD, SE_j the 2000× bootstrap SE of frame j's median; **B1** z₀ =
      |β₀₀₀₀ − med M| / √(s_M² + SE₀²) ≤ k (k = the worst clean frame left out of its own set); **B2** SE₀/β₀₀₀₀ ≤ the
      clean frames' max SE_j/β_j; **B3** s_M/med M ≤ s_C/med C. Head 31.777 (SE 15.89): B1 1.79 ≤ 3.11, B2 0.500 >
      0.144, B3 0.088 ≤ 0.207 → FAIL.
    - **R10c-1 the class (arm a3).** Plateau failed ∧ 3×3 max ≥ bg + 0.9·range (the knot exemption's own reading);
      censoring (b) rejected (an unconstrained undersampled core reads β 98 on 0000). Costs recorded then: FP mono 0.23 →
      1.02 %, OSC 0.40 → 0.54 % (974 of +1 213 at 0.90–0.98 of an exact clip, 266 the brightest stars of clip-free
      planes); 19 β / 9 F lab cells moved; real `c` −1.7 … +2.9 %. Superseded in its level by R10c-10.
    - **R10c-2 β 1.5: the clamp stays** (`BETA_AT_CAP`). A post-fit reject is a truncated median biasing β up (LDN
      mono 1.5 → 2.68 at head, 4.0 fallback after a3); 0 of 337 lab planes move. LDN mono's 1.5 is a whole-frame
      property → the later fit-design task (RB3; a 3F region floor 1.50 → 1.78).
    - **R10c-3 plateau seeds: not this class.** With the class, the 683 / 855 plateau seeds on 0000 / 0008 move no β,
      F or `c`; dropping them loses 16 flagged big-star records. If wanted: one seed per clipped component (F-1's seeds).
    - **R10c-4 PSFSW → Task 13.** `frame.rs::signal_totals` sums every accepted fit, reads no flag; saturated share of
      TFlux 0.79–0.90 (M42), 0.77–0.82 (hc11B L); relative M42 weights 0.40–1.00 → 0.82–1.00 with flagged fits out.
    - **R10c-5 stale premise.** LDN 1272 0099 reads β 99.96 at every brightness (5 near-clip of 64, not 63; F 4.1 vs
      2.0) → Task 13's frame classes (defocused / flat-topped).
    - **R10c-6 the 0000 remainder, corrected by the owner.** The diag D-M00 read 0000 as a "doubled frame" (ePSF lobe
      at 1.58 px, 0.28 of peak; asymmetry 0.211 vs 0.136–0.188); **REJECTED**: frames 0000 / 0001 are visually
      identical and the comparison program's ecc differs by 0.001 (ours 0.656 / 0.652). 0000 is clean; its β 7.6 is
      OUR frame-β estimator's error (between-pixel cores read point β 9.4 vs 3.8 centred), not a frame property.
    - **R10c-7 the order (owner, after Fable's plan review).** 10c ships; B2 is a known miss owned by the new Task 11b
      (F-4b, a frame-level β; truth = the lab's `ComaPsf` extended to F 1.6–2.4, gate zero = HEAD fails it as 0000
      does; arms joint 64-star fit / growth-curve β / flux-ranked sample, the 3F floor beside); order 10c → 11 → 11b
      → 12. Fable's mechanism: a one-sided shoulder in 0000's mean profile (ePSF +1 / +1.5 / +2 px 0.53 / 0.45 / 0.26
      vs 0001 0.59 / 0.33 / 0.16) a single Moffat cannot represent × the between-pixel flat top × the peak-ranked
      sample (53 % split vs 32 % in the population); ≈ 1 % of 0000's FWHM.
    - **R10c-8 the fixed band's FP has a truth cost → a fix round.** L2: the C-11 frame-ecc WORSE (68 planes) is the
      SET change — 871 newly flagged stars, 0 truth-saturated, 843 truth-unsaturated, the brightest (filterSnr 3–15×
      the rest); truthcheck +0.03…+0.07, `neb3` +0.025, `neb3_4096` +0.024, twins +0.002 (refit Δ ≤ 0.0004). Not
      accepted as a limit (raw frames clip exactly, like truthcheck): the floor derived per plane; bars FN ≤ 0.5 %, FP
      ≈ base, C-11 ≈ a1c's, M42 B1 / B3 pass.
    - **R10c-9 trails wording.** `trails_i0` F +1.43 % (4.3468 → 4.4091) moves toward the pixbox and prepixel truths
      (−5.34 → −3.98 %, −3.55 → −2.16 %) and away from pointfit (+1.30 → +2.78 %); `trails` is in no bar and has no
      set referee (its pointfit and pixbox differ by 7 %). No "toward truth" claim either way. At 7933f582 0 lab
      planes move vs base, so the move is gone with the fixed band.
    - **R10c-10 the derived floor (design round D2, arm cc).** As built above. Arms measured out: pixel0 of clause-2
      seeds alone (truthcheck t4 +0.050 ecc), the pixel-histogram pile-up edge (t1 +0.070), ties only (no tie on any
      calibrated frame: 0000 stays 31.777), any-pixel0 plateau ∧ clause 1 (OSC FP 1.421 %), the 3×3 max as evidence
      (cc3: LDN OSC G loses its gain), a 5σ margin (changes nothing), the fixed band as no-evidence fallback (FP 0.402 %,
      truthcheck back to +0.071). Dev-Mac numbers: FN 4 / 12 169 · 0 / 1 229, FP 0.235 / 0.410 %, 0 lab planes moved,
      ecc ≤ +0.0014; LDN OSC G 3.811 → 2.917 (a3 3.043). Limits below.
    - **R10c-11 L3's letter-STOP, accepted.** One new C-11 WORSE plane beyond a1c's set, `osc_pairs/f3.5` t3 p0, ecc
      0.1454 → 0.1469 vs t10-f5-r2 (1× the line, not a STOP-cell; vs a1c +0.0012, inside +0.002), set change only (2
      newly flagged, refit 0), predicted +0.001 on the dev Mac; table 38 / 24 / 14 / 4 vs a1c's 37 / 24 / 13 / 4.
    - **The floor's limits (recorded, not bars; (a)–(e) R10c-10, (f)–(g) the final review `review-t10c-2.md`).** (a) A
      clipped plane whose clipped stars are all one-pixel clips (no tie, no certified core) gets no class B; resampled
      `f2.0` keeps its 4 FN, as at base; no real frame tested qualifies (7–321 certified cores per plane). (b) The
      floor never goes below the plateau band: LDN mono's clip at 0.86–0.90 stays unflagged (its β is the clamp,
      R10c-2). (c) The floor is the frame-wide lowest clipped value: an unclipped star between it and a vignetted
      corner's clip (about 1.03 on M42) is flagged. (d) A single certified star can set the floor. (e) Clauses 1 and 2
      keep their own clip-free FP (base 0.23 %). (f) A `MostlySaturated` refusal (the heaviest clipped core) has no
      verdict, so its pixel0 is never evidence: on a flat-divided clip the floor can only sit too high (FN side) when
      the lowest-clipped stars are all refused. (g) On an integer plane a chance tie at a clip-free maximum sets a
      floor at the plane max: class B then adds only the star(s) holding that pixel, which clause 1 flags anyway (e).
    - **The lab cycle (runner; athenaeum-lab `star-lab/results/plan-a/t10c-nearclip/` at 9f2cafc7, lab 2971210, with
      L2's split `l2-ecc-split.txt`; `t10c-floor/` at 7933f582, lab 7eb48fb; base = the a1c outputs).** Lab pools
      11 778 clipped mono (the dev Mac's 12 169 includes alias families) and 333 planes (dev 337).

      | Cell | Bar | `t10c-nearclip` (fixed band) | `t10c-floor` (derived floor) | Verdict |
      | ---- | ---- | ---- | ---- | ---- |
      | B1 | z₀ ≤ k | 3.69 ≤ 4.31 | 3.60 ≤ 5.39 | ok |
      | B2 | SE₀/β₀ ≤ max clean | 0.149 > 0.059 | 0.155 > 0.046 | known miss → Task 11b |
      | B3 | w_M ≤ w_C | 0.071 ≤ 0.210 | 0.051 ≤ 0.207 | ok |
      | β₀₀₀₀ (SE) | reported (base 31.777 (15.89)) | 7.568 (1.125) | 7.568 (1.173) | reported |
      | per-frame β / F / unsat | = the dev Mac's column (22 frames) | 0 mismatches | 0 mismatches | ok |
      | F-3 twins | 48 / 48, F ≤ 1 % | F worst 0.69 %, β worst 7.7 % (24 void) | F worst 0.69 %, β worst 6.8 % (24 void) | ok |
      | row 10 FN mono / OSC | ≤ 0.5 % | 0 / 11 778 · 0 / 1 229 | 4 / 11 778 (= base) · 0 / 1 229 | ok |
      | row 10 FP mono / OSC | reported; floor: mono ≤ base + 0.02 pp | 1.047 % · 0.543 % | 0.244 % · 0.410 % (base 0.242 · 0.397) | ok |
      | detection control | seed sets identical | 391 / 391 | 391 / 391 | ok |
      | planes moved vs base (β > 1 % / F > 0.1 %) | the design list only | 19 / 9 (flagged set changed on 231) | 0 / 0 of 333 | ok |
      | C-11 frame ecc vs a1c | floor: every plane ≤ +0.002 | 68 WORSE, worst truthcheck t3 +0.071, `neb3` +0.0255 | max +0.00145 (`neb3_4096`), 0 over | ok (R10c-8) |
      | C-11 table vs t10-f5-r2 | ≈ a1c's 37 / 24 / 13 / 4 | 88 cells, 68 WORSE, 29 STOP-cells | 38 / 24 / 14 / 4 | R10c-11 |
      | cumulative vs t01c | PASS | moved 0 | moved 0 | ok |
      | F-4 item 1 | ≥ 56 / 58 | 58 / 58 | 58 / 58 | ok |
      | lab_cycle (C1.1–C1.4, F.0, D.1) | pass | 0 MISS in 70 + 20, F.0 885 / 885, D.1 0.777 / 0.539 | same | ok |
      | `c_judge` (K.1, N, W, T.4–T.8, I, P) | allowance | 82 ok, 0 MISS | 82 ok, 0 MISS | ok |
      | S.1 / M.1 | ≤ 1.46 % / ≤ 159.8 s | 0.79 % / 76.7 s | 0.81 % / 76.8 s | ok |
      | same-star vs a1c | allowance | 0 WORSE, 5 BETTER | 0 WORSE, 6 BETTER | ok |
      | class-B flags (reported) | — | +1 183 FP mono, +35 OSC | 71: 66 truth-saturated (38 ≥ 3 clipped core px, 28 with 1–2), 5 FP | reported |
      | `clipFloor` (reported) | — | — | 308 of 333 planes: 272 ties (floor 1.0), 36 certified cores (0.9007–0.9997); 25 none | reported |

    - **Per-frame β (release `star_probe`; base 3deb6cb5 → fixed band 9f2cafc7 → floor 7933f582; floor in range
      units / evidence count / class-B stars; the dev Mac's `perframe_base_t10c_d2cc.tsv`, which both runner cycles
      reproduce on the 22 M42 / hc11B L frames with 0 mismatches).**

      | Frame | base | fixed band | floor | floor / evidence / class B |
      | ---- | ---- | ---- | ---- | ---- |
      | M42 0000 | 31.777 | 7.568 | 7.568 | 0.925 / 319 / 59 |
      | M42 0001 | 4.185 | 4.073 | 3.952 | 0.940 / 297 / 23 |
      | M42 0002 | 5.384 | 4.764 | 4.764 | 0.942 / 310 / 19 |
      | M42 0003 | 3.395 | 3.324 | 3.307 | 0.928 / 309 / 21 |
      | M42 0004 | 3.285 | 2.961 | 2.987 | 0.921 / 309 / 31 |
      | M42 0005 | 3.440 | 3.413 | 3.413 | 0.924 / 308 / 25 |
      | M42 0006 | 4.108 | 3.313 | 3.230 | 0.927 / 297 / 40 |
      | M42 0007 | 3.334 | 3.165 | 3.152 | 0.939 / 309 / 24 |
      | M42 0008 | 2.810 | 2.645 | 2.645 | 0.946 / 321 / 18 |
      | M42 0009 | 3.700 | 3.379 | 3.383 | 0.944 / 318 / 20 |
      | M42 0010 | 3.343 | 3.336 | 3.336 | 0.937 / 317 / 17 |
      | M42 0011 | 3.193 | 3.160 | 3.193 | 0.934 / 315 / 15 |
      | hc11B L 0000 / 0001 / 0003 / 0007 / 0009 | 3.594 / 4.722 / 3.621 / 4.201 / 5.061 | equal | equal | 0.900–0.908 / 18–22 / 0–1 |
      | hc11B L 0002 / 0004 / 0005 / 0006 | 3.002 / 4.794 / 4.567 / 3.261 | 3.017 / 4.902 / 4.590 / 3.279 | = fixed band | 0.902–0.910 / 20–22 / 1–3 |
      | hc11B L 0008 | 4.184 | 4.201 | 4.184 | 0.905 / 19 / 0 |
      | LDN 1272 mono | 1.500 | 1.500 | 1.500 | 0.900 / 134 / 57 |
      | LDN 1272 0099 | 99.956 | 99.964 | 99.964 | 0.900 / 114 / 8 |
      | LDN 1272 OSC R / G / B | 1.590 / 3.811 / 1.848 | 1.634 / 3.043 / 1.749 | 1.590 / 2.917 / 1.754 | 0.936 / 12 / 7 · 0.994 / 12 / 38 · 0.912 / 7 / 11 |

      M42 0000's F 2.342 → 2.267 → 2.267. B3's M spread s_M/med M: 0.088 → 0.071 → 0.051.
    - **Carries.** **Task 11b:** B2 (0000's β 7.568, SE 1.173; B2 0.155 > 0.046), with its truth family, gate zero and
      arms (R10c-7); Fable's two ledger items — (1) on a clip-free plane `range = min(plane max, norm) − bg` puts the
      brightest CLEAN star at the range: the derived floor took it out of class B (0 lab planes moved, FP back to
      base), but clauses 1 and 2 keep it (limit (e)), and 11b's sample rule must not inherit it; (2) the β sample's
      split share per frame before / after the class (D-M00 at the fixed band: 0000 53 % vs 32 % in the population,
      0001 53 % vs 39 %) is 11b's premise, to be read at 7933f582. **Task 13:** PSFSW (R10c-4) and LDN 1272 0099
      (R10c-5), written into its text. **The later fit-design task:** LDN mono's β 1.5 (R10c-2). **F-1's seeds, if
      wanted:** one seed per clipped component (R10c-3).
  - **R-OSP-8 - the per-star SNR and HFR: S / σ_S on the star's own ring sky and noise, the plane's noise
    correlation and the header gain, HFR on the F-5 flux (F-6, Plan A Task 11, rulings R11-1 … R11-10 below; no
    `MEASURE_VERSION` bump, PA-4; `snr` and `hfr` are not persisted, like `peak5`).** *As built (1224718a; 12706d6c
    the review round: a given-seed `Mixed` / `Unknown` plane reads its noise correlation off the prep's one mesh, no
    second mesh build; one `warn!` per failed read; doc wording; the probe output byte-identical on the five Phase A
    inputs).*
    `photometry::star_snr` = S / σ_S over the present pixels whose centres lie within max(1.5·FWHM, 3) px of the
    fitted centre (the fit's own FWHM), S = Σ(v − B_i) on the star's stored `sky_ring` (R-OSP-7b), σ_S² =
    s·max(S, 0)/g + κ(A)·N_ap·σ_i²·(1 + N_ap/n_i) on its stored `sky_ring_sigma` / `sky_ring_n`; it estimates no sky
    and no noise. A negative S (the star reads below its ring sky) gives a negative finite SNR, which enters the
    medians. κ(A) = Σ_{i,j∈A} ρ(i − j) / N_ap over the aperture's present pixels, ρ = C/C(0) from the plane's one
    noise read (`seeds::NoiseCorrelation`, exposed as `rho()` and `DetectionReport.noise_correlation`; the read itself
    unchanged); κ ≡ 1 on a `White` plane. s (`ApertureNoise.shot_spread`) = 1 / the CFA site share on a debayered
    plane (the adapter states 0.25 / 0.5 / 0.25 on a colour group's planes, so 4 / 2 / 4), else 1.
    `photometry::hfr` = the linear half-signal radius on (v − B_i): the present pixels within 5 × the ring width,
    taken by distance until their sum reaches half the star's F-5 flux, interpolated by the last pixel's share; NaN
    without a ring, signal or width, or when that circle holds less than half. Both are computed after `c`, in the
    fit's tail and in `correct_at_ring` (B_i and `signal` move there). The gain: `stacking::measure::gain_from_header`
    (FITS and XISF) = EGAIN × `ATH_CSCL`, `ATH_CFNM` never read, read errors `warn!`ed, non-positive → None. Frame:
    `snr_median` / `hfr_median`; both `PlaneMeasurement`s carry `snr_gain`, `snr_missing`, `snr_noise` (`white` |
    `read` | `unread`) and `shot_spread`; probe star `snr`, `hfr`, `apSum`, `apN`, frame `snrGain`, `snrMissing`,
    `snrMedian`, `hfrMedian`, `snrNoise`, `shotSpread`. Spec §3 F-6 "as built", §4.2's HFR line and the
    owned-quantities table (the noise-correlation row) follow it.
    - **R11-1 σ_S (arm c).** σ_S² = s·S/g + κ(A)·N_ap·σ_i²·(1 + N_ap/n_i), κ(A) from the plane's ONE noise read (10b's
      `seeds::NoiseCorrelation`, reach 2; its owner unchanged, Task 11 only reads it); a `White` plane ⇒ κ ≡ 1, no
      read (C-7's argument, no number chosen). s = 1 / the plane's CFA site share on a debayered plane (R 4, G 2, B 4,
      derived from a flux-preserving interpolation spreading each site's shot noise), else 1. Given-seed `Mixed` /
      `Unknown` planes run the same read once; LN's refits and the probe's `betas` arm pass no correlation (κ ≡ 1).
      Pull classic 1.52–2.12 (`osc`), 1.63 (`resampled`) → (c) 0.92–1.17 / 1.08. Cost if wrong: colour SNR over-read
      1.5–2.1× (classic) or 1.2–1.6× on bright colour stars (κ without s).
    - **R11-2 the R/B κ under-read (recorded limit).** The 10b read under-reads κ on R and B by 12–21 % (pull
      1.16–1.28 on R / B at F 5). Fixing it moves colour seeds (injected LDN OSC R recall −5…−7 pp with the VNG
      white-noise table) → a later detection-owner item, not Task 11. C-7 (a)'s leak is not visible on real OSC nebula
      (read / true 0.80–1.06).
    - **R11-3 the structure term: not in F-6.** F-6 is the noise SNR on σ_i. The RB4 structure term breaks 4 clean
      cells (−7…−25 %) and its c = 0.8 is circular (fitted on the two nebulae it is judged on). Recorded for Plan B /
      the later nebula-photometry owner: on nebula sky the real flux error is 3.6–6× σ_S (pull `neb0.5` 3.63, `neb3`
      6.04: the local sky mismatch under the disc, the annulus method's own limit). Plan B B5 (owner, 2026-10-23): a
      recorded limit with no owner, re-read 3.64 / 5.98 at the B5 binary (R-OSP-16 B5).
    - **R11-4 the bar.** Row 6 median bias ≤ 3 % on 50 cells: the 46 `sharp/*` and `background/` flat, grad10, neb0.1,
      vig20. `neb0.5` / `neb3` / `neb3_4096` are REPORTED with their predicted values (−16.5 / −75.5 / −60.7 % against
      the white truth): there the white-noise truth is not the star's real error, and any noise SNR misses it. Spec
      F-6 bar amended (owner informed 2026-10-12; reopen if the owner objects). New row "star SNR noise pull" on
      `osc/*`, `osc_pairs/*`, `resampled/*`: the robust std of (S_meas − S0)/σ_S, S0 = the generator's noiseless frame
      through the same debayer / warp in the same aperture (1.00 = σ_S is the real error), per plane within [0.85,
      1.30]; row 6 on the Mixed families reported with "white truth on a Mixed plane".
    - **R11-5 populations.** Every accepted fit carries SNR and HFR (saturated and extended included, finite). NaN
      only without a ring (B_i missing) or, HFR, under half the signal inside 5F. `snr_median` / `hfr_median` =
      medians over the fits `in_statistic()` (neither saturated nor extended) with finite values; this amends the
      plan's "finite values only" (the flagged stars lift the frame SNR +10…+24 %). HFR has no truth row; reported
      only.
    - **R11-6 the gain: header only, no camera table.** EGAIN × `ATH_CSCL` e⁻ per calibrated unit (PA-22, `ATH_CFNM`
      ignored), raw domain EGAIN. No gain ⇒ the shot term is omitted and `snrGain` is null on the frame line.
    - **R11-7 additive.** `StarFit.snr` / `.hfr` `#[serde(skip)]` like `peak5`; `.athf`, `ChannelMeasurement`, the DB,
      the weights, LN and `MEASURE_VERSION` unchanged; the new frame values ride on the adapter's `PlaneMeasurement`
      plain fields. Probe seed / star / frame lines byte-identical to 35870dbf after stripping the new keys.
    - **R11-8 the pull referee is clipped.** The noiseless reference is clipped at sat like the noisy frame (OSC and
      resampled): an unclipped S0 adds clip loss to (S_meas − S0) that the pull must not charge to σ_S. `resampled`
      f2.0 classic pull 1.59 (unclipped 1.63); 4 of 525 stars carry a neighbour's clipped core.
    - **R11-9 the design list.** The pull row is `"kind":"reported"` (`scorecard_diff` knows set-aside / one-sided /
      reported).
    - **R11-10 the control STOP (runner Part 2).** The PA-15 control mismatches (143 at the step, 138 cumulative
      against t01c) were all row 6 `star SNR vs truth` on path `analysis`: the truth `snr_ap` moved onto the 10b ring,
      the legacy analysis estimator did not. `scorecard_diff` skipped a hard-coded `t09truth`; fixed in the lab: a SET
      of truth steps ({t09truth, t11truth}), the union gains the t11 truth entries, 4 tests; both diffs re-run without
      a probe re-run → control 0 (the first-run verdicts kept as `*.pre-R11-10.txt`). Supporting: the rescored-base
      diff PASS (11 736 analysis rows). No product line moved. R11-8's predicted ≈ 1.04 withdrawn: the head reads 1.07
      (unclipped 1.08).
    - **The recorded limits (not bars).** (a) R11-2: the R / B κ under-read of 12–21 %, the detection owner's. (b)
      R11-3: on nebula sky the real flux error is 3.6–6× σ_S (Plan B / the nebula-photometry owner). (c) R11-6: the
      gain comes from the header only; the owner's ATR2600M and QHY268M calibrated lights carry no EGAIN, so their
      SNR has no shot term (`snrGain` null); a ×18 wrong EGAIN moves the median star's SNR +4…+10 %, bright stars up
      to +64 %. (d) A flagged-only plane (every fit saturated or extended): the rings read no F (the frame FWHM is
      the unflagged statistic, 10b's rule), so no B_i and every SNR / HFR is NaN, `snrMissing` = all, both medians
      None. (e) `Unread`: a `Mixed` / `Unknown` plane whose correlation could not be read (or with no seed) gets κ 1
      and one `warn!`, the frame line says `unread`, and its SNR is the white-pixel value, which over-reads on a
      really correlated plane.
    - **The lab cycle `t11-f6` (runner; athenaeum-lab `star-lab/results/plan-a/t11-f6/` at 1224718a, rustafits
      67c180f, solvemyastro c1c633c; lab 70b116e (Part 1: the ring truth, the pull row, the Mixed note), b3d8434
      (design list, bars), 1cd8704 (R11-10's tool), 60bbf27 (scripts), 8532f01 (results); base = t10c-floor).**
      Part 1 re-derived the truth `snr_ap` on the star's own ring (`families._v2_cols`: N_sky = the centres in
      [10F_t, 12F_t] outside every other truth star's 3F_t disc; S, N_ap and the per-pixel variance unchanged): 301 v2
      truth files refreshed, 299 changed, only `snr_ap` moved (on the old annulus truth `sharp/*` read +2.4…+9.7 %,
      emulated: the move was required). **Every bar passes; one STOP (the PA-15 control), resolved by R11-10.**

      | Cell | Bar | Value (`t11-f6`) | Verdict |
      | ---- | ---- | ---- | ---- |
      | lab suite / truth sanity | ok | 765 OK / 3 cells, 3 explained, 0 new | ok |
      | row 6 vs the ring truth, 50 cells | ≤ 3 % | worst 0.97 % (`neb0.1`), `sharp/*` −0.90…+0.51 %; `n_no_gain` 0 | ok |
      | row 6, `neb0.5` / `neb3` / `neb3_4096` | reported (predicted −16.5 / −75.5 / −60.7 %) | −16.54 / −75.51 / −60.65 % | as predicted |
      | noise pull, 50 planes (`osc` 24, `osc_pairs` 24, `resampled` 2) | per plane within [0.85, 1.30] | 0.92…1.25; `osc` f2.6 m4 R / G / B 0.92 / 1.04 / 1.06, f5.0 m4 1.16 / 1.01 / 1.17; `resampled` f2.0 1.07 (unclipped 1.08), f3.0 1.04; 0 `apN` mismatches | ok |
      | row 6 on Mixed (reported, white truth on a Mixed plane) | predicted −25…−41 %, −6 / −2 % | `osc` −41.5…−24.4 %, `resampled` −5.9 / −1.9 % | as predicted |
      | lines vs t10c-floor | identical after the new keys (and the clocks `wallMs` / `ms` / `analyzed_at`) | 3.84 M lines (v2, inject, L, replica) identical; row 1 register 0 of 1 431 cells differ | ok |
      | no-regression / cumulative vs t01c | PASS | moved 0, one-sided 0, lost 0, control 0 on both (after R11-10; the rescored base PASS too) | ok |
      | R10c-0 (22 frames) | B1, B3 pass; B2 the known miss | β₀₀₀₀ 7.568 (SE 1.173); B1 3.60 ≤ 5.39; B3 0.051 ≤ 0.207; B2 0.155 > 0.046 | = t10c-floor |
      | twins / row 10 | 48 / 48; FN ≤ 0.5 % | 48 / 48 (F 0.69 %, β 6.8 %, 24 void); FN 4 of 11 778 · 0 of 1 229; FP 0.244 % / 0.410 % | = t10c-floor |
      | C-11 / F-4 item 1 / C1.1–C1.4 | ≤ +0.002; ≥ 56 / 58; pass | frame ecc Δ 0 (frame lines identical), table = t10c-floor's (14 WORSE, 4 STOP-cells); 58 / 58 (worst 2.23 %); ok, control PASS | ok |
      | S.1 / M.1 / same-star / `c_judge` | ≤ 1.46 % / ≤ 159.8 s / allowance | 0.81 % / 76.9 s (t10c-floor 76.8 s) / 1 333 667 stars, β changed on 0, 0 WORSE / 82 ok, 0 MISS | ok |
      | HFR vs analytic, Fisher < 20 / 20–100 / ≥ 100 (reported) | — | m4 −0.33 / +0.34 / +0.95 %, m2.5 −1.78 / −1.51 / −1.08 %, m10 −1.23 / −1.15 / −0.66 % (= the dev Mac); gauss −1.05 / −0.45 / −0.10 % (analytic width = the pre-pixel truth FWHM, the Moffat rows' reference; the design read's −4.2 / −2.2 / −1.3 % took the point-fit FWHM for gauss, which reproduces −4.26 / −2.25 / −1.28 %: a reference difference on the same 9 families, runner L3) | reported |
      | `snrNoise` / `snrMissing` / null gain (reported) | — | read 50, white 139, unread 0 / 0 / 0; per-family medians in `reported.txt` | reported |

    - **Cost.** Measured as an interleaved bracket on the runner (L3, lab 7a70351): A = 35870dbf, B = 12706d6c, rounds
      A B B A A B, one process at a time. B / A is the summed wall per round, median (min–max):
      - LN on the product's per-side ring: 1.022 (1.006–1.027);
      - LN on the pair-common lab arm: 1.043 (1.038–1.049);
      - the stack invocation on `osc/*` + `resampled/*` + `sharp/m4/*`: 1.104 (1.078–1.132);
      - the analysis path, which Task 11 does not touch (null control): 1.009 (1.001–1.011).

      Over the null that is +1.3 pp (LN), +3.4 pp (the lab-only arm) and +9.5 pp (stack on small synthetic frames). On
      the real M42 frames M.1 reads 76.9 s against 76.8 s. Probe lines: B equals 1224718a on the subset (205 436 v2
      lines plus the LN arms) and equals A once the new keys are stripped. **No skip flag on the LN refits**
      (`Correction.photometry` is not built): the product's LN path reads +1.3 pp, under the +3 % line, so one SNR /
      HFR definition holds everywhere. The small-frame stack cost (+9.5 pp, not split between the per-star SNR / HFR
      and the given-seed noise read on Mixed planes) goes to Task 12's cost bars.
    - **Carries.** **The detection owner (a later item):** R11-2's R / B κ under-read (fixing it moves colour seeds).
      **Plan B / the later nebula-photometry owner:** R11-3's structure term (the nebula flux error 3.6–6× σ_S); Plan
      B B5 kept it a recorded limit with no owner (R-OSP-16 B5).
      **Plan C:** the later readers of `snr` / `hfr` (the analyzer's star metrics and `median_snr`, the `star_metrics`
      columns, the collab gate's `median_snr` rules and the hub's mirror, the Analysis and frame-info views; listed in
      plan Task 11's Outcome).
  - **R-OSP-9 - the stacking group β: under `Auto` every member of a group is fitted at ONE β, `psf_signal::group_beta`
    of the members' own βs; the frame's own β is a diagnostic (F-4b, Plan A Task 11b, owner decision R11b-12; rulings
    R11b-0 … R11b-13 below; no `MEASURE_VERSION` bump, PA-4).** *As built (0f70e3c0; 3af1fd8d the review round: pass 1
    ticks progress, `beta_own_at_cap`, the fan-out length check an `error!` + `Err`, doc notes — review I1, I2, M1–M4).*
    Stage 3 under `measurement.psfModel = Auto` (`run.rs::measure_group_two_pass`) is two-pass over `PlanePrep`: pass 1
    per frame in parallel (`measure_frame_pass1`: read, seeds, verdict passes, `star_measure::plane_beta` = F-4's E5
    estimator, unchanged → each plane's own β); the group β resolved once = `psf_signal::group_beta` (the lower median,
    always one of the members' own βs) over every measured plane of every measured member — the value LN already fitted
    its reference and every member at (`PsfModel::Fixed`, perf tier C Task 1); pass 2 per frame (`measure_frame_pass2`)
    at `PsfModel::Fixed(group β)` (`star_measure::fit::resolve_beta`, the one place a β comes from a `PsfModel`). Every
    at-β fit, F, `.athf`, `c` and F-6 is made at the group β; `ChannelMeasurement.beta` = the β the fits were made at =
    `stacking_run_groups.beta` = LN's reference β (pinned by `measure_writes_fits_artifacts_and_the_group_beta`).
    `ChannelMeasurement.beta_own` (serde default, skipped when None) and `beta_own_at_cap` (skipped when false;
    `beta::at_bound(beta_own)`) carry the own β; `SummaryFrame.beta_own` / `beta_own_at_cap` in the provenance; the
    probe's measure frame line `betaOwn` / `betaOwnAtCap`, and `--psf-beta B` runs pass 1 then pass 2 at `Fixed(B)` on
    every plane. A group of one sits at its own β (an OSC frame at the lower median of its planes' own βs, so the cache
    key stays unambiguous). `Moffat4` / `Fixed`: one pass, byte-identical, the old hash
    (`moffat4_measure_is_one_pass_and_unchanged`). The single-frame path (`star_probe`'s default, Analysis / Plan C)
    keeps its own β. A member whose pass 1 passed and pass 2 failed still counts in the lower median (M2; every `.athf`
    is still at it). `BETA_AT_CAP` is never set at the group β (a fixed β); the own β's bound is `beta_own_at_cap`.
    Progress: pass 1 and pass 2 each tick half a frame (`FanOutTicker::with_scale(2)`), the stage lands on total (I1).
    Logging: one `info!` per group (`run_id`, `group_key`, `beta`, `count`, `beta_own_min`, `beta_own_max`), a `debug!`
    per frame plane (`frame_id`, `plane`, `beta_own`, `beta`), `beta_own` on `"frame plane measured"` (the logging
    dictionary has the three new names).
    - **The cache.** `plan::measurement_hash_for(cfg, calib, group β)` folds `measure_beta:{:.6}` under `Auto` only
      (`unresolved` for None; no production row carries it). A stored row's first pass is valid iff its key equals the
      hash at `group_beta` of its own channels' β (`pass1_own_betas`, one rule for the run and the gate), so a valid row
      gives the own β without a frame read; the gate rebuilds the same group β from the stored rows of the non-excluded
      members (`gate_measure_betas`) and marks the group's Measure stale when any member lacks a valid first pass
      (conservative, as HEAD's missing-row rule). Register and LN fold the group-β'd hash (`group_measurement_hash`).
      Unchanged group β: a full hit, nothing read. Moved: every member misses pass 2 (I2 below).
    - **The mechanism (D0, R11b-2, R11b-7; the second design ruling).** M42 0000 is a composite PSF: 5 % wider than 0001
      (held β 4 1.044–1.047, the ERP 1.050 / 1.053, HFR 1.050; free β 1.088) with a weak frame-wide +x skew (skew x z
      19.6 > k 10.9), the same shape star by star (the owner's eye; the ERP's β 4.35 vs 4.55 on 0001). A single Moffat
      fitted to such a star has two basins on a shallow ridge — narrow with heavy wings (β < 3.5, FWHM 1.90 px: the
      shoulder as wing) or wide with light wings (β ≥ 7, 2.32 px: the shoulder as core) — so the per-star β is BIMODAL
      (0000's point β over peak 0.1–0.6 of range: < 3.5 / 3.5–7 / ≥ 7 = 0.41 / 0.27 / 0.32; 0001 0.19 / 0.68 / 0.13)
      and any median over the sample is a basin-mix statistic; the frame-summed χ²(β) is unimodal at a model-mismatch
      value (0000 5–7, 0001 4). F rides the (α, β) ridge identically on every frame at +1.8 % per unit β (D0 S3: 0000
      2.199 / 2.240 / 2.279 at β 3.3 / 4 / 5), so 3c is a bar on Δβ between the frames, not on any β's value: HEAD's
      1.088 is the ridge at Δβ 3.6. B2 and 3c contradict each other BY THE MODEL, not by an estimator, and every arm
      that read 0000 better read the clean frames worse. The consumers never needed a per-frame β: the weights compare
      F across the group, `c` is per plane and absorbs the model, and LN already fitted every member at one group β —
      so before this task Measure's F and LN's F sat at two different βs.
    - **The decision and the rule (R11b-12, the owner, 2026-10-13: the group β for stacking, Moffat 4 by default — the
      default clarified as Analysis / Blink, Plan C).** In stacking Measure every at-β fit, F, `.athf`, `c` and F-6 are
      made at the GROUP β; a group of one at its own β; the own β is `beta_own`, reported, never consumed — a frame whose
      own β sits far from the group β gets no flag and no exclusion; `Moffat4` / `Fixed` unchanged; the single-frame
      path reads its own β. **Plan C:** the Analysis tab / Blink default PSF is Moffat 4 (the reference frame
      analyser's default), the frame's own β shown as a diagnostic, Auto selectable. This amends F-4's "every star is
      then fitted at it" (the frame's β) for stacking; F-4's lab bar is unchanged; B2 is not a bar.
    - **R11b-0 the order (owner).** 0000 and 0001 look identical star by star → a localization step D0 first (S1 the
      ERP, S2 model-free pixels, S3 our fit at a held β, S4 our free β), each by the R10c-0 B1 z / leave-one-out k rule.
    - **R11b-1 an ePSF bar candidate** (0000's F / ePSF half-max +5.97 % vs −2.0…+3.24 %), dropped by R11b-2 (the ePSF
      is built at our centres).
    - **R11b-2 D0's outcome.** First separation at S1 / S2 (width z 2.34 > 1.91, skew x 19.6 > 10.9), NOT the shape and
      NOT our fit at a held β; β 7.568 lives only in S4 — our estimator's error on a 5 % wider, weakly skewed frame.
    - **R11b-3 gate zero** (β / β(control) ≥ 1.8 AND ΔF over the control ≥ +2 pp on a skew cell), fixed before the run.
    - **R11b-4 gate zero passes; the arms' bars.** `skew/f1.6/s1` reproduces 0000's pattern (ours +2.88 pp vs the ERP's
      −0.14 pp, β 1.94×); bars B-2 (family ΔF err ≤ max(the ERP's, 0) + 0.5 pp), B-3 (M42 B1–B3 with B2 required, 3b,
      3c ∈ [1.044, 1.062]), B-4 (no regression); phase invariance reported, β itself not a bar on the family.
    - **Owner rule (2026-10-12).** Mechanism / algorithm questions inside the task go to a fresh design reviewer; its
      answer, truth-checked by the controller, is the ruling; the owner is informed of outcomes.
    - **R11b-5 a fifth arm `band`** (owner: the brightest stars carry aberrations more strongly): the sample = eligible
      seeds at peak 0.05–0.6 of range, ≤ 256, stratified over 4 log-peak bins × a 4 × 4 grid.
    - **R11b-6 `band` read** (0000 β 8.23, 0001 4.17): its truth check had read the POINT β, the frame β is the
      integrated one — corrected by R11b-7.
    - **R11b-7 the bimodal per-star β** (the mechanism above); path (d) amended to a joint sample fit with a
      frame-shared displaced-copy shoulder; fallback the summed-profile β.
    - **R11b-8 `shoulder` read** (β 2.17–14.46 over the 12 frames, 3c 1.0735; summed profile 0000 6.80 vs 3.36): both
      fail B-3 by construction; the ninth estimator → "stop patching, re-audit the model".
    - **R11b-9 the second design ruling** — the group β as the consumed quantity, the per-frame β a diagnostic, B2 not a
      bar; truth-checked (the ridge +1.8 % per unit β; clean-frame s_M 0.168); it amends the owner's F-4, so the owner
      was asked before code.
    - **R11b-10 owner challenge** (the rationale was unclear; the reference programs do not do this): the group β
      withdrawn as the recommendation; a reference-style frame FWHM (median of per-star own-free-β point-fit FWHM) read
      0000 / 0001 1.0524 (ours 1.0883).
    - **R11b-11 the L4 read** — F_ref on the point fits (measured out, below); the pixel-integrated `fref` arm was built
      (A4) and never judged.
    - **Owner fact (2026-10-13).** The reference frame analyser's default PSF is Moffat 4 (one fixed β for every frame);
      its M42 ratio 0000 / 0001 1.050; ours at a held β 4 1.044–1.047, at the per-frame β 1.088.
    - **R11b-12 the owner's decision** (above).
    - **R11b-13 the L5 read** (the table below): no further estimator; the group β is kept; 3c and 3b FWHM_m2 are
      recorded as the composite-PSF definition limit; Task 13 judges at master level.
    - **The lab read L5 (runner; athenaeum-lab 1e52339 at 0f70e3c0, `star-lab/results/plan-a/t11b-l5/`).** Step 0 = the
      default (single-frame) path against L2's `head`; then the 12 M42 frames replayed at `--psf-beta` = the group β.

      | Cell | Bar | Value (L5) | Verdict |
      | ---- | ---- | ---- | ---- |
      | Step 0: single-frame path vs `head` (24 M42 / L1 planes + the 463-invocation quick manifest: 487 files, 1 849 458 lines) | identical | identical but for the additive `betaOwn` (== `beta`) on 255 measure frame lines | ok (accepted) |
      | M42 group β | — | 3.3066 (0003's own β; 0000's own 7.568) | reported |
      | 3c F(0000) / F(0001) | ∈ [1.044, 1.062] | 1.0431 ± 0.0013 (own β 1.0883) | miss by 0.0009 (0.7 SE): the definition limit |
      | 3b ours / HFR | z₀ ≤ k | 1.01 ≤ 2.66 (own β 4.63 > 3.74) | ok |
      | 3b ours / FWHM_m2 | z₀ ≤ k | 5.16 > 2.16, sign flipped: 0000 0.7246 vs the others' 0.7425 (own β 5.47 > 5.16) | miss: the definition limit |
      | B1 / B3 on `beta_own` | = Task 10c | 3.60 ≤ 5.39 / 0.051 ≤ 0.207 (`betaOwn` == the default run's β on all 12) | ok |
      | B2 | not a bar | — | resolved by definition |
      | F / the ERP's free-Moffat F (informational) | — | 0000 0.964, the 11 others 0.973–1.010 (own β 1.022 vs 0.965–1.004) | reported |
      | F at the group β / F at the own β | — | 0000 0.943, 0008 1.032 (own β 2.645), the rest 0.968–1.013 | reported |
      | accepted fits at the group β | — | 0000 13 843 → 13 654, 0008 14 908 → 15 089, 0003 unchanged (`centreMoved` / `narrowerThanPixel` / `regionTooSmall`) | reported |
      | probe wall, `--psf-beta` / default (ABBAAB, median) | information | 0000 0.992, 0008 1.024 | reported; M.1 is Task 13's |

      3c's lower edge was D0 S3's own held-β reading (1.044–1.047 at β 3.3–5); the independent widths span 1.043–1.062
      on 0000 (ours 1.043, HFR 1.050, the ERP 1.050 / 1.053, FWHM_m2 1.062). 3b's FWHM_m2 k fell 5.16 → 2.16 because
      the 11 clean frames tightened under one β.
    - **Measured out — never re-propose (nine estimators and the reference-style F; archive: the WIP branch
      `one-star-path-t11b-wip` and its bundles, none merged).**
      - *joint* (β, α_x, α_y, θ shared over the 64-star sample, block-diagonal LM): 0000 β 6.14, 3c 1.069, B2 miss,
        B-2 f1.6 s1 2.52 pp vs 0.50.
      - *growth* (the β whose model growth curve matches the sample's F-5 5F sums): 3c 1.057, but β 2.754 is
        sky-coupled; item 1 27 of 58 over, twins 33 / 46, C-11 Δecc up to 0.111.
      - *flux* (rank by F-5 flux below 0.9·range): 0000 β 6.77, 3c 1.081, 3b HFR miss.
      - *± floor* (Task 10b's 3F region floor on the sample fits, beside head / joint / flux): 0000 β 5.7–6.0, but B1
        misses (the clean frames' k shrinks to 2.8–4.5); 3c 1.071–1.078.
      - *band* (R11b-5): 0000 β 8.23; fails B2 by construction.
      - *shoulder* (a frame-shared displaced-copy shoulder in a joint fit): not identifiable on real data — β 2.17–14.46,
        f at its 0.5 bound for a 0.25 % χ² gain; 3c 1.0735.
      - *summed profile*: 0000 6.80 vs the others' median 3.36 (0003 6.60); a clean-frame spread 10× HEAD's (±4 % F
        noise in every clean group).
      - *moment ratio* (HFR / FWHM against the Moffat's own ratio): skew-insensitive (0000 2.35, 0001 2.21) but
        sky-coupled through the 5F total — drags β from ≈ 4 to 2.3 on every M42 frame.
      - *the Auto ladder* (a fixed β chosen by summed χ²): the discretised profile — 6 on 0000, 4 on 0001 (F +3.5 %),
        plus the 9.6 % β-alone jumps F-4 removed.
      - *multi-start* (repair the integrated fit): only picks the global of two basins 3.9 % apart; the sum still says
        5–7.
      - *F_ref on the point fits* (the frame F = the median of each star's own free-β point-fit FWHM): passes M42 (3c
        1.0455 / 1.0524, 3b both, twins 46 / 46) but misses item 1 (51 / 58: +1.0 % at F 1.2, worst m2.5 f1.2 +6.3 %)
        and B-2 f1.6 s1 (1.11 / 1.62 vs 0.50) — a point model on pixel-integrated undersampled stars reads wide (E5's
        own reason); trimmed means read low.
    - **The upgrade ripple (review I3).** Every pre-11b `Auto` row is keyed without a β, so it is stale on the first plan
      build after this lands: every existing `Auto` set re-measures, re-registers and re-normalizes ONCE on its first
      run (`.athf` files overwritten in place, no orphans); `Moffat4` / `Fixed` sets keep their hashes. Named in the
      release notes. Task 11c halves it (its pass-1 artifact is keyed without the group β).
    - **I2 — a group-β move re-runs pass 1 today.** The pass-1 records are not stored, so when an exclusion moves the
      lower median every member re-reads and re-detects, not only re-fits (HEAD's Measure × (3n − 1)/(2n − 1) reads for
      the group). Not a correctness bug; the doc comment on `measure_group_two_pass` says it; **Task 11c** owns it.
    - **Memory (the review's ruling).** A freshly measured member holds its pass-1 records until pass 2 (≈ 250 B per
      seed, ≤ 6 MB per plane at the detector's 24 576 cap): 6–15 MB on M42; a 300-frame OSC group ≈ 0.7 GB typical,
      5.4 GB worst, uncounted by admission. Kept in 11b; a bound (drop and recompute) computes twice, against the
      owner's disk-as-cache rule → the spill, Task 11c.
    - **Cost.** A mono plane is read twice, an OSC frame 3n − 1 times (2n − 1 before); the verdict pass is not
      repeated. The probe-level bracket above is information only; the stage-level Measure wall (M.1, ≤ 1.2×) is Task
      13's, whose bar now covers the two-pass Measure.
    - **Recorded limits (not bars).** (a) 3c (1.0431 vs 1.044) and 3b FWHM_m2 on M42: the composite-PSF definition
      limit — the referees themselves disagree by 1.9 % on 0000. (b) The single-frame path over-reads a skewed frame's
      half-max by the ridge (+1.8 % per unit β; 0000 +4 %), as every standalone tool does: there is no second frame to
      share with (a Task 14 debt line). (c) A mixed-profile group reads every F at one β: comparability over per-frame
      truth (F-4's lab bar is per family).
    - **Carries.** **Task 11c:** spill pass 1 to `<working_dir>/<set_slug>/measured/` (I2, the memory, half the
      ripple). **Task 12:** the analysis mode's default PSF is Moffat 4 (R11b-12). **Task 13:** its M42 frame-β line is
      closed by this definition; M.1 covers the two-pass Measure; master-level judgment of the group β. **Task 14:**
      limit (b) as a debt line. **Plan C:** Moffat 4 by default, the own β shown, Auto selectable; the Measure panel
      still labels the model "Auto (per-frame β)" (`stageSummary.ts`), which now names the diagnostic, not the β the
      fits are made at — a label line owed with the next frontend touch.
    - **The 11c amendment: the pass-1 spill (Plan A Task 11c; rulings R11c-1 … R11c-7; `PASS1_ARTIFACT_VERSION` = 1).**
      Under `Auto`, pass 1 writes one artifact per member, `<working_dir>/<set_slug>/measured/<group>/<stem>.athm`
      (a project's under `_projects/<slug>/`), the new `stacking_artifacts` kind `measured`; pass 2 reads it back and a
      valid artifact skips every pass-1 plane read, detection, verdict pass and own-β sample. Pass 2 still reads the
      frame (its fits need the pixels).
      - R11c-1 stored vs derived: the `PlanePrep` verbatim, the head's timings, `w`, `h`, gain, `beta_own` and the
        source stamp are stored; the detector and module options are rebuilt through ONE builder (`module_options`)
        that `adapter_head` itself calls, so there is no second construction.
      - R11c-2 format: postcard behind a hand-rolled header (`b"ATHM"`, the artifact version, `MEASURE_VERSION`, head
        length + xxh3, body length, a whole-file xxh3); JSON refused (the records hold NaN and ±inf). Any refusal is a
        `warn!` and pass 1 re-runs, never a mis-decode.
      - R11c-3 key: `pass1_hash_for(cfg, calib)` = the measurement subtree + the calib hash + `measure_pass1:v<N>`,
        independent of the group β, so a β move re-runs pass 2 only; `Moffat4` / `Fixed` write nothing.
      - R11c-4 identity: the stamp is the read guard's size + mtime at pass 1's first plane read; a mismatch re-runs
        pass 1 (never an exclusion).
      - R11c-5 flow: validate on the controller, pass 1 fanned out and written inside the worker (records dropped
        there), the group β, then pass 2 from the artifact or, with none, pass 1 in the worker (the upgrade ripple).
        A write failure is a `warn!`, no row, pass 1 again in pass 2's worker: nothing is ever held.
      - R11c-6 lifecycle: `measured/` goes with `Intermediates` and `All`, an excluded frame's artifact stays;
        `WorkUsage.measuredBytes`; no command changes on either host.
      - R11c-7 (owner): bar 1 on real colour data, set 204's OSC and mono groups.
      - **Bars (lab c4d9b92, `star-lab/results/plan-a/t11c-wall/bars.txt`; A = ec39507e, B = 89958c3f).**

        | Bar | Rule | Value | Verdict |
        | ---- | ---- | ---- | ---- |
        | 4a M42 fresh Measure, B / A | median ≤ 1.2 | 0.9885 (pairs 1.0542 / 0.9685 / 0.9885) | PASS |
        | 4b M42 exclusion re-run, B / A | median ≤ 1 − s/2 = 0.6181 | 0.2700 (0.2622 / 0.2700 / 0.2823); s = 0.7638, expected 1 − s = 0.2362 | PASS |
        | 1 M42, `--measure-identity` | 0 differences | (i) 0 / (ii) 0 | PASS |
        | 1 set 204 (OSC 105 + mono 92) | 0 differences | (i) 0 / (ii) 0 | PASS |

        Set 204, information: fresh Measure A 678.7 s / B 643.0 s; exclusion re-run Measure A 396.9 s → B 83.0 s (B / A
        0.209), run wall 1021 → 691 s; the OSC pass-1 share of the stage 0.533. `.athm` bytes per plane: OSC mean
        1.94 MB, mono 3.63 MB (max 3.82 MB), M42 2.55 MB. `measured/` 0.94 GB beside `calibrated/` 50 GB = about 1.9 %
        (the design read's "about 1 %" was low). Attempt 1 of the driver stopped on a lab-tool bug (a moved beta
        required set 107's unmeasured groups to move; fixed in the lab, 6a8d4bb).
      - **Review** (`review-t11c.md`, 6abffeb0): Ready with fixes, 0 Critical / 2 Important / 3 Minor, all fixed in
        a6a28f39 (I1 a log field dropped, I2 measured rows kept on cancel, M1 a cache-hit member does not validate its
        artifact, M2 pass-2 identity over both seed specs, M3 a typed `Mismatch`); 89958c3f tightened M1's test.
        Gates at 89958c3f: `stacking::` 853 / 0 / 2 ignored, `star_measure::` 173, `check --all-targets` clean,
        `ts_contract` pass.
      - **Limits and later items.** (C2) Pass 2 still holds every member's `(FrameMeasurement, fits)` until they are
        persisted (0.1–0.3 GB on a 300-frame OSC group); bar 3 covered pass 1 only. (C3) The postcard golden pin is the
        only layout guard, and `RefusalReason`'s variant order is part of the format; the bump is
        `PASS1_ARTIFACT_VERSION`, and a `PlanePrep` field change fails the pin until it is bumped (Task 12). (C4) The
        plan gate reads `metrics` only, so a cancelled run reads conservatively stale (11b's rule). (C5) The space
        estimate has no term for `measured/` (about 1.9 % of the calibrated term). (C6) Bar 4b's `1 − s/2` is lenient
        and kept; `1 − s` is printed as the expected ratio. The Measure stage wall on real data is Task 13's M.2.

  - **R-OSP-10 - the Analysis mode: `Mode::Budget { budget }` fits at most N good unsaturated stars spread over a 4 × 4
    grid, with no own-β work at the default Moffat 4 (spec D-3, PA-10, ruling F40; Plan A Task 12; rulings R12-0 …
    R12-9; `MEASURE_VERSION` and `PASS1_ARTIFACT_VERSION` unchanged).** *As built (a7ac7659; a00238f7 the review round:
    the amode `snrGain` in the lab unit, a saturating target, the renumbering pin, refusal and `raw_scales` tests, the
    BZERO wording, the selection wall in `fit_ms`).* `star_measure::Mode` (`All` by default = the stacking path, op for op
    unchanged; `Budget { budget }`) lives in `MeasureOptions.mode`; `star_measure/budget.rs` holds `DEFAULT_BUDGET` = 500,
    `ANALYSIS_PSF_MODEL` = `Moffat4`, `CANDIDATE_FACTOR` = 1.25, `cell_of`, `select_candidates`, `trim`, `report` and the
    serialized `BudgetReport { cellsWithStars, cellsCovered, candidates, kept, notSelected, trimmed }`
    (`PlaneMeasurement.budget`, `Some` exactly under `Mode::Budget`).
    - **R12-2 the budget lives in `measure_plane` only.** `prepare_working` was split into `prepare_head` (noise, mesh,
      sky, saturation plane, detection, noise correlation) and `prepare_passes` (verdict pass, clip floor, near-clip
      class), a pure restructuring. The selection runs after detection and before the verdict pass, so both per-seed
      fits run on the candidates only. `PlanePrep` is untouched, so `PASS1_ARTIFACT_VERSION` stays 1
      (`the_plane_prep_encoding_is_pinned` unchanged is the proof); `prepare_plane`, `finish_plane` and `plane_beta` stay
      `Mode::All` only (`prepare_plane` under `Budget` logs a `warn!` and measures every seed).
    - **R12-3 the selection.** The cell is `cell_of` of the SEED's position (`4·cy + cx`, each clamped to [0, 3]) — for
      the selection, the trim and the report alike, so coverage can never exceed 1. Rank = seed index (the detector's
      brightest-first order). Quota `q = ⌈1.25·N/16⌉`, target `16·q` (640 at N = 500); a water-fill round-robin (key =
      rank within the seed's own cell, then index): every cell that still holds seeds gives its next brightest, a short
      cell's unused share passes to the others. Returned ascending (= rank order). N ≥ seeds selects every seed (=
      `Mode::All`'s fit set); N = 0 selects nothing (every seed `NotSelected`, one `warn!`).
    - **R12-4 the trim.** Tier 0 = `flags.in_statistic()` (neither saturated nor extended), tier 1 = the rest; round r =
      the fit's rank within (cell, tier) by SNR descending (NaN last, then fit index); the first N of (tier, r, SNR, fit
      index) are kept. Flagged fits fill only the slots no unflagged fit wants (a plain SNR round-robin would keep the
      saturated stars first). The removed fits become `Outcome::Trimmed` and keep their diagnostics records;
      `Accepted { star }` is renumbered after the trim (pinned: RED on a broken counter).
    - **R12-5 neighbours.** The ring sky masks the 3F disc of EVERY detected seed and `c`'s isolation tests every seed
      (`fit::Neighbours`, `aperture_correction(.., own)`), not only the fitted ones; so a candidate's B_i, σ_i, SNR and
      HFR are what `Mode::All` reads at the same ring width. Every other caller (LN's refits) passes `None`.
    - **R12-6 the report.** `cellsWithStars` = the cells holding an accepted fit before the trim, plus the cells holding
      a `NotSelected` seed and no accepted candidate; `cellsCovered` = the cells holding a kept fit; a plane whose
      verdict is defocused or trailed is 0 / 0 and the coverage bar skips it. (The literal "any seed above threshold"
      would score the module's own refusals — an edge star, a blend — and a defocused donut image 0 / 16 as budget
      misses.) `candidates` = |C|, `kept` = |stars|, `notSelected` = seeds − |C|, `trimmed` = accepted − kept; one
      `debug!` ("budget applied") carries them.
    - **R12-1 no own-β work at Moffat 4 (owner, 2026-10-15).** Under the default `PsfModel::Moffat4` the mode makes no β
      sample, no extra fit and not even the verdict fit's free point-model β (it reads undersampled stars high, E5);
      `beta` = 4, `beta_source` = `Fixed`. The 64 pixel-integrated free-β fits (k² = 9 … 81 sub-samples a pixel) would by
      estimate cost at least the 500-star fit itself. The frame's own β exists only when the caller selects `Auto` (the
      verdict sample then runs over the budget's candidates); `Fixed(b)` is b. A diagnostic β at Moffat 4 is Plan C's
      opt-in, if wanted.
    - **R12-7 the probe.** `star_probe`'s `analysis-mode` path (with `--budget N`, `--clip C`, `--psf moffat4|auto`)
      runs alone on the RAW file in ADU, writes no seed lines, and the cost walls are read without `--diagnostics`; the
      frame line carries `timings` (background / noise / detect / fit_ms, the selection wall inside `fit_ms`), the
      budget report and `snrGain` in the lab unit.
    - **R12-0 and R12-9 (the lab).** No runner for this task (owner): every run is dev-Mac, base `t12-base-pn` at
      8bbcc135 (the pre-Task-12 probe, a lab worktree before the amode switch). The interpolated-image pixel-noise
      statement is the lab default since 92fbded (resampled and warp pair targets ran white without it: T11.5 pull
      2.26 / 2.52 instead of 1.07 / 1.04). With it the dev-Mac base reproduces the runner's t11-f6 cumulative report line
      for line except the row-9 walls. Head `t12-amode-pn` (a00238f7, lab 92fbded): noreg vs the base PASS, 0 moved /
      control 0; cumulative vs t01c PASS; the held rows (D, T11) PASS.
    - **Bars (athenaeum-lab `star-lab/results/plan-a/t12-amode*/bars.txt`; read with `grep` / `sed -n`).**

      | Bar | Rule | Value | Verdict |
      | ---- | ---- | ---- | ---- |
      | B1 budget coverage, worst image | ≥ 0.9 of the cells that hold measurable stars | 100 % worst (141 planes scored, 0 below; 2 planes 0 / 0 skipped: donut r10 / r6 plane 0) | PASS |
      | B2 analysis-mode fit outcomes identity | ok on every family | 143 / 143 rows | PASS |
      | B3 m42 analysis-mode mean per frame | ≤ 0.6 s | **0.70 s** (rounds 0.705 / 0.701 / 0.702, 77 frames; r1 0.72; max 0.92); legacy the same day 0.316 s (2026-10-07: 0.33) | **MISS, recorded (R12-8)** |
      | B4 m42 `measure` batch, B (head) sum | ≤ 1.2 × 223.1 = 267.7 s | 111.9 s (A = 8bbcc135 108.9 s; B / A 1.028 / 1.023 / 1.037, median 1.028) | PASS |

      B4: the 2026-10-07 223.1 s was the OLD measure path (the corpus batch is 2× faster now; M.2 is the stacking fit, not
      this). B / A 1.028 is small, real, beyond both spreads, and unexplained (`Mode::All` got only the structural split).
    - **R12-8 B3 is a recorded miss (owner, 2026-10-16).** The cost is the three whole-plane passes, the stacking path's
      own code: corpus phase medians background 178 / noise 234 / detect 141 ms (= 553 of ~630 ms) against the budget's
      fit 65 ms (max 265); `background/neb3_4096` 0.46 s against legacy 0.20. Their speed-up goes to the Measure-perf work
      beside Task 13's M.2, judged at master level; there is no Analysis-only shortcut (one path, one code).
    - **Recorded rows (information).** `_DSC5767.fit` `measure` seeds per plane 15041 / 15786 / 9039 (baseline 0;
      `Mode::All` is unchanged); `sharp/m4/f2.6` bright coverage 100 % (analysis-mode) against 7.3 % (legacy analysis),
      completeness 92.6 % of 537 against 21.5 % of 554; corpus analysis-mode coverage 99.9 % (worst 93.8 %), corpus
      identity ok.
    - **Limits.** (a) The per-plane values taken from the fits (aperture correction and its stars, trail share, defocus donuts,
      verdict-fit failures, the near-clip floor and its evidence) are read over the candidates only, not every seed; the
      ring sky and `c`'s isolation still read every seed (R12-5). (b) A raw OSC input is ONE plane (no per-colour split): the mode measures the mosaic as a mono plane; the CFA rule is
      Plan C's. (c) The raw-mode completeness bar (≥ 99 % of the injected stars above threshold) is not met by the 500
      cap on a dense field and is Plan C's at N ≥ seeds.
    - **Carries.** **Task 13:** the B3 whole-plane cost beside M.2 (R12-8). **Plan C:** the Analysis tab / Blink switch
      (`analysis/analyzer.rs map_result`), XISF / `read_raw`, the CFA rule for raw OSC, the completeness bar at N ≥ seeds,
      the optional own-β diagnostic at Moffat 4. Logging: the dictionary has the new names (`budget`, `candidates`,
      `kept`, `not_selected`, `trimmed`, `cells_with_stars`, `cells_covered`). No command, type or `ts_export` change.

  - **R-OSP-11 - the real-data checkpoint at master level (Plan A Task 13; rulings R13-0 ... R13-4; read with `grep` /
    `sed -n` from athenaeum-lab `star-lab/results/plan-a/{t13-real,t13-attrib}/*/verdict.txt`, `t13-real/{m2,p0,erp,
    ln-truth}`; lab d43e019 ... c7f512f, pushed).** *The design.* Each dataset is a clone of its d5f26b7e before tree
    (Horse C11 `hc11B-f1` run 5, run B `lnB-runB-f1` run 6, set 204 `tierA-lnB-f1` run 5) run from Measure with the
    before run's reference pinned `manual` (69845 / 123 / 28934) and `twoPass = false`; the after server is b6c5d5b3 (the
    13-P build for P0 and the M.2 B arm). **The attribution:** the same three runs by ed5fa8de (A; it holds the LN model
    fixes 42d4f338, 61e9608b, 002a1e08 that d5f26b7e does not) reproduce the before trees EXACTLY (masters, weights rho
    1.0000, LN s ratio 1.000000, Measure 1.000 +- 0.007 on all three), so **every delta below is Plan A's own**.
    - **Rows (B = b6c5d5b3 against the before run; PA-13 Measure equivalence equal on all three).**

      | Row | Horse C11 | run B | set 204 |
      | ---- | ---- | ---- | ---- |
      | master median / MAD | G +0.03 / -0.18 %; L +10.32 / +2.01 % (1/6 pass) | L +0.22 / +2.12 %, OSC p0 +0.36 % (0/4) | mono +0.147 / +3.54 %, drizzled +0.131 %, OSC +0.38 / -0.22 / -0.31 % (0/8) |
      | master noise | G -6.48 %, L -10.52 % (less noise) | L +0.62 %, OSC p0 +1.68 % (within) | mono +3.83 %, drizzle2x +4.60 %, OSC p0 +1.97 % (5/8) |
      | master FWHM | G +0.185 %, L -0.42 % (beta 3.30 vs 3.39) | L -1.82 %, OSC p0 -1.17 % (sharper) | mono -2.04 %, drizzle2x -2.61 %, OSC p0 -3.37 % (5/8) |
      | weights rho (bar >= 0.9) | **0.4642** (G -0.170, L 0.520; 78 movers, each with a scorecard row) | 0.9553 (L 0.942, OSC 0.814; 63 movers) | 0.9595 (mono 0.936, OSC 0.833; 63 movers) |
      | LN s median (bar +-0.5 %), scatter | +3.00 %, 0.20 % | -0.95 %, 0.08 % | -0.97 %, 0.08 % |
      | Measure, one run (before) | 2.21 (1.65) min = 1.338, drift-ambiguous | 9.78 (3.55) = 2.752 | 9.88 (3.62) = 2.726 |
      | Normalize (before) | 1.22 (1.19) | 3.19 (2.66) | 3.10 (2.46) |
      | total (before) | 10.4 (11.0) | 18.5 (13.5) | 20.0 (15.0) |

      set 204 also: drizzle level 3/4 (OSC p0 0.9978), coverage 2/4, rejected fraction PASS. Run B's masters were
      unreadable in the first pass (tool defects below) and read from the attribution's B re-run.
    - **M.2 (set 204, interleaved Measure-only bracket A1 B1 A2 B2 A3 B3).** A = ed5fa8de **3.61 min** (3.61 / 3.61 /
      3.57; = the 2026-10-06 3.62, no drift); B = 13-P **9.88 min** (10.08 / 9.78 / 9.88): **B / A 2.738, bar 1.2 MISS;
      the owner's 4.34 min MISS.** The ERP's own subframe measurement on the same calibrated files: **4.00 min**
      in-instance (mono 92 files 116.1 s = 1.26 s / frame, peak 8.7 GB; OSC 105 files 124.2 s = 1.18 s / frame in
      chunks of 3, an upper bound; Horse C11 106 files 252.7 s = 2.38 s / frame; run B derived 3.74 min): **ours / ERP
      2.47x**, A ~ ERP. Plane medians A / B (ms), mono: read 36 / 76, background 327 / 412, noise 346 / 298, detect 172 /
      171, **fit 185 / 3552**, total 1652 / 7510; OSC: fit 200 / 1557, total 1551 / 4313.
    - **The fit profile (13-P, P0 identity 0 differences against the set 204 run).** Mono, 92 planes: the seed cap
      (24 576) is hit on **50 / 92** planes; **22 961 stars fitted per plane against A 6 884 and the ERP 8 272** (3.3x);
      fit 3.55 s / plane = the verdict pass 1.21 s (15.75 LM iterations per fit, share 0.157) + the fit at the group
      beta 2.33 s = **0.102 ms per star against A's 0.027** (185 ms / 6 884, 4x); the beta sample 38 ms (0.005); pass 1 =
      0.757 of the Measure wall; the whole-plane passes (read 73 / background 374 / noise 297 / detect 216) barely
      moved. OSC, 315 planes: 12 235 seeds, no cap hit, fit 1.65 s (verdict 0.72 + at beta 0.93). The pre-Plan-A 0.21 s
      fit was cheap by COUNT (6 884 stars), not by kernel.
    - **R13-3 (Fable, standing in for the owner, 2026-10-16 night).** (Q3 / Q4) Plan A's weights and masters are
      ACCEPTED: tonight's ERP rows say the weights track the ERP better, set 204 mono Spearman(ERP PSFSW, weight) 0.931
      -> 0.956, OSC 0.903 -> 0.936, OSC plane-0 FWHM A 3.60 / B 3.07 / ERP 3.06; the sharper, noisier master is PSFSW on
      better inputs (mono Neff 84 -> 71 = the +3.8 % noise; a weight-mode choice, not a code fix). Horse C11 rho 0.46 is
      the old fixed-ellipse flux (x6 - 22 per frame, enclosed fraction 0.83 - 0.93) being the artefact; on the 78 movers
      Spearman(ERP PSFSW, weight) is G -0.16 -> +0.72, L +0.26 -> +0.81. No F-3 / F-4 / F-5 round. (Q1) The master
      level shifts are a re-definition (the LN offset term under F-10's ring sky), report-only; Horse C11's L +10.3 % is
      pedestal data (OFFSET 1, no dark or bias, 2x in the corners), where two sky definitions differ most.
    - **R13-4 and the LN verdict (Fable final, `ln-truth/`).** The LN reference is NOT pinned: `build_reference`
      integrates the top-N frames by sky-penalized weight, and Plan A's weights changed its composition (set 204 top-20
      shared 16 / 20 mono, 11 / 20 OSC; OSC anchor 29034 -> 28949). The row s_B / s_A therefore read the reference's own
      star-flux change (phot_B / phot_A 0.990 mono, 0.994 / 0.991 / 0.987 OSC); reference-corrected, the estimator's
      move is **-0.21 % mono, -0.06 / -0.06 / +0.13 % OSC**, inside +-0.5 %. Against aperture photometry on the registered
      planes (gap = s / phot - 1): median |gap| A -> B mono 0.270 -> 0.313 % (n 91, a 0.04 pp difference under the ~0.1 pp
      noise), OSC 0.401 -> 0.196, 0.326 -> 0.247, 0.780 -> 0.683 %, pooled 0.348 -> 0.339 %. Master / reference star
      flux A -> B -0.56 % mono, -0.27 / -0.13 / -0.02 % OSC, within the combined noise (0.6 - 1.3 sigma). **RULING:
      re-definition, no Task 10b / Task 10 fix round; the harness row is re-based to the reference-corrected ratio
      (bar +-0.5 %), with the raw ratio, the top-N overlap and the anchors printed beside (a pair without truth prints
      "reported, n/a").** Horse C11 closed by the controller: its `referenceFrames` is 5 and the top-5-by-weight sets of
      A and B are disjoint in both groups. The harness re-base itself is a lab item for Task 14's notes.
    - **Routing.** (1) The **Measure-performance task**, scoped by the OWNER (Fable's proposal, five lines): first
      the budgeted population (R-OSP-10's `Mode::Budget` for what the stacking Measure consumes: weights, F, beta, `c`,
      F-6), judged on the whole group at master level since it moves `.athf` and flattens the depth tilt; second the
      per-star kernel, every `.athf` bit-identical, then the verdict pass skipped for a seed whose peak cannot reach the
      clip floor; third the whole-plane passes (R12-8's family); out of bounds: the 6 sigma threshold (R-OSP-2), the
      Moffat conditioning (R-OSP-3), the verdict rule (R-OSP-5), the beta estimator (R-OSP-6), the group-beta two-pass
      (R-OSP-9). Bars: set 204 Measure <= 4.34 min interleaved, Horse C11 <= 1.2 x A, the ERP's 1.2 s / frame printed
      beside; acceptance = this checkpoint's compare re-run. (2) The LN item: closed (above). (3) Nothing else
      re-runs: no weight, master or LN fix round opens from this checkpoint.
    - **Tool defects found and fixed tonight.** Lab f819e12 (project masters sit one level down in `stackout/<project>/`
      and rejection / coverage maps were classified as masters), 2260ab7 (`ATH_ACC_STOP_AFTER` polled for the
      `stacking stage finished` line, which the server writes only at the run's end: a stage now ends at the later
      stage's `fan-out admitted` line), 88f6cb2 (`erp_group`'s file array under zsh `set -u`). **Plan defect:** Step 1's
      `cargo build -p athenaeum-web -p athenaeum-core --example ...` builds only the examples, so the server needs its own
      `-p athenaeum-web` call (B's server was a stale binary until rebuilt).
    - **Limits and carries.** (a) Horse C11 Measure 1.338 and the single-run walls are one-night, not interleaved; the
      attribution's same-night B / A (hc11 1.327, runB 2.696, set 204 2.729) supersede them. (b) The ERP OSC time is an
      upper bound. (c) Horse C11's B tree is deleted; only `starA-s204` stays for Task 14 (`probe_parity`). (d) 13-P
      is additive debug lines only (`PlanePrep` and `.athm` untouched); its review round (0 / 1 / 4, `measure_planes_
      diagnosed` multi-plane `verdict_ms`) folds into the 13-P commit. **Task 14:** the LN-row re-base, the Measure
      cost as the Measure-performance task leaves it.

  - **R-OSP-12 - Measure performance, route A: the stacking Measure's numbers are 5836a970's, only faster (Plan A Task
    13b; rulings R13b-1 ... R13b-12; branch `one-star-path-t13b`, 3b8b1e41; lab 591c556, 4b18f85, 086309c, 2b89a75).**
    *The bar and how it ended.* The owner asked for a Measure faster than the ERP. Set 204 Measure 9.90 min (5836a970,
    "H") against the ERP's warm-up-corrected 3.77 min (226.4 s); Horse C11 2.22 against 3.71 min. The first design
    (fit a budgeted population, estimate the weight's totals over the rest) reached the bar on paper and was REFUSED by
    synthetic truth; the owner chose **route A, 2026-10-17 ("A: как раньше")**: every seed is fitted and the weight is
    exactly 5836a970's. **The stacking Measure's numbers equal 5836a970 bit for bit** (`MEASURE_VERSION` 4 and
    `PASS1_ARTIFACT_VERSION` 1 restored, so artifacts measured before stay valid); only the bit-identical speedups
    remain. "Faster than the ERP" on set 204 is NOT met and goes to its own cycle (below).
    - **The speedups kept (all bit-identical).** rustafits (07b8287, branch `one-star-path-13b`): background mesh **1.16
      -> 0.58 s** and MRS **0.62 -> 0.11 s** per plane (B3 smoothers as contiguous slice passes in the same add order,
      bicubic per-column tables, per-cell MAD median by integer select on f32 bits, branch-free masks). Athenaeum: `rcr`
      by **verified decisions, x4.5** (phase 0 decides each rejection from a 2^-16 table of sqrt(2)*erfinv with a rigorous
      interval, |X~ - X| <= 1.76e-10; undecided iterations, the ending iteration and every returned value are recomputed
      exactly, so the kept set and the location / scale bits are identical; the proof is in the module comment; signal
      totals 0.705 -> 0.154 s on 0058, 0.905 -> 0.197 on 0075, 0.797 -> 0.178 on OSC; assumption libm ln / exp within
      1e-12 / 1e-11 relative); `hfr` by lazy chunked selection (SNR / HFR -45...-60 %); ring sky per-row range. Ideas
      skipped as under 1 % of a plane on the 13b-P timers: read->working fusion, `d` fusion, buffer reuse.
    - **Brackets** (Measure-only, interleaved H N H N H N, clones of the before trees, reference pinned `manual`,
      `twoPass = false`; H = 5836a970, N = 3f275540). **Set 204 9.90 -> 7.95 min (9.82 / 9.97 / 9.90 against 7.97 / 7.95 /
      7.93; N / H 0.803; ERP 3.77 min -> 2.11x, bar MISSED). Horse C11 2.22 -> 1.43 min (0.647; ERP 3.71 min -> 0.39x, bar
      MET, faster than the ERP).** CPU-s per plane H -> N: set 204 12.65 -> 10.55 (cores busy 8.66 -> 9.00), Horse C11
      8.76 -> 5.38. Run B was not run. The review fix round (3b8b1e41) did not re-run the brackets; its single-thread wall
      against 5836a970: 0058 12.90 -> 11.09 s, 0075 6.60 -> 4.76, OSC x3 20.86 -> 17.14 (the bracket's N was measured
      before the timer gating below; the gated head is no slower in detect, so the bracket is a slight over-estimate of
      N's wall). **Analysis B3** (Task 12 bracket (b), m42, 3 rounds, 3b8b1e41): analysis-mode 0.464 s per frame — PASS
      (bar 0.6; Task 12 0.723); legacy analysis 0.240 (0.339).
    - **Proofs.** Lab `t13b-id` (591c556) at the Step 1 head against the base, 67 658 noreg rows: moved 0 / missed 0 / lost
      0 / control 0, max |delta| 0, raw trees 2 590 files with 0 different once clock content is dropped; lab `t13b-final`
      (4b18f85) at 3f275540: measure / betas / register / seeds / legacy analysis 0 moved (11 736 legacy rows), inject
      measure rows 0, probe census 0 changed values of any existing key; only the analysis-mode rows move (1 537 v2 / 755
      inject, below). **Set 204 (Step 5a, route A before the shape revert): registration 565 / 565, group and plane
      beta, 197 registered frames byte-identical, PSFSW / totals / noise / counts bit-equal on every channel, weights and
      `weight_channels` identical on 197 frames, 199 LN files identical, every master / drizzle / weight / rejection image
      HDU identical** (the Measure stage ran 477 s against 593 s). Final probe identity 3b8b1e41 against 5836a970
      (`star_probe measure --paths measure,betas,register --diagnostics`, wall fields and the probe-only keys dropped):
      0058 123 717 lines, 0075 146 068, OSC 0163 225 442 = 495 k lines, **0 differing**; debug on against off, 0 differing.
    - **Tried and REFUSED (measured out; do not re-propose without a truth-validated weight).** The stacking weight
      (PSFSW, TMeanFlux) estimated over a budgeted population (a) of N = 1 000 plus a list (b) of M = 4 000 mono / 2 000
      OSC, the rest imputed. (1) **Imputed totals** (R13b-1 / 8 / 11): with 20 % of the seeds unfitted (the small-M row,
      139 mono frames) the truth row gave a **depth tilt -5.5 % (point-fit -4.0 %) on N against -0.5 % (-0.9 %) on H,
      per-frame tails p5 -31 % / p95 +10 % and extremes -42 % / +46 %, 35 / 139 frames more than 10 % off H, H nearer
      truth on 82 / 139**; production would impute 75-90 %. Mechanism: the mixed vector's `rcr` collapses the winsor bounds
      onto the fitted stars (P_Nfit / P_N median 1.96, p90 16.5). At the default M the corpus imputed nothing (0 / 187
      frames), so the first truth row was no verdict. (2) **rcr bounds from the fitted vector alone, imputed means
      clipped** (test C): rho 0.56 against H 0.97, depth tilt +39.6 %, spread 699 % - H's and the truth's own `rcr` run
      over ALL stars; DEAD. (3) The per-class ERP join (R13b-8) could not resolve its own -0.01 slack (P(delta rho < -0.01)
      0.60-0.73 by paired bootstrap), so R13b-9 moved the referee to synthetic truth AFTER the 3c miss - stated here; the
      owner may overturn. (4) **The (a) frame shape** (frame FWHM / ecc over population (a)): vs truth MIXED (FWHM toward
      9 / away 16, ecc toward 101 / away 78; nebulae ecc +0.017 / +0.020, seven wide-PSF sharp rows +0.011...+0.013,
      fieldpsf FWHM +1.0 %), inject hc11B G image 2 FWHM 9.87 -> 12.85 px (**+30 %**), tierA mosaic p0 -16 %; with every
      seed fitted it buys no speed, so the controller reverted it to all fits (3f275540). Also measured: set 204 weights
      rho(N, H) 0.991 / 0.985 under the refused totals - it was the truth row, not rho against H, that refused them.
      Population c from a stratum sample had SE 2.1-4.7 % (R13b-6 reverted c to the 64 brightest of (a); the final c is
      5836a970's again).
    - **Kept in the tree, UNUSED by the stacking Measure.** `Mode::Population`, `population::select`, `StarPopulation::
      probe_default`, the probe's `--paths population`, the lab's `t13b-pop` family: the probe / lab read them; the
      stacking adapter does not pass them. `Mode::Population` reports its totals over the fitted set (documented: NOT the
      stacking weight, R13b-11); `write_pass1` refuses (error logged) a record whose prep is not storable.
      `signal_totals_complete` and the imputation were removed in the fix round. **`Mode::Budget`** (the Analysis path)
      takes the shared cell-first, rank-spread selection and a stratified trim: cell-first water-fill, then (cell,
      SNR-quartile) classes rank-spread, the flagged tier last and given the cells the unflagged tier left uncovered
      (the R12 coverage contract holds, 12 / 12); it moves the Analysis rows (1 537 v2 / 755 inject; B1 100 %, B2 143 /
      143), re-based by a runner cycle (`t13b-final2`).
    - **The pre-fit clip evidence (R13b-10 / 12).** The population's pre-saturation screen anchored both clauses on
      min(plane max, norm) - bg, so a clip-free plane flagged every star within 10 % of its brightest (truthcheck frame
      ecc +0.014...+0.088 against truth 0). The first fix (range = norm - bg when `max_count < 2`) failed on calibrated
      frames (set 204 mono has `max_count` 1, plane max 0.862 / 0.868 < norm); R13b-12's evidence is `max_count >= 2 ||
      a seed's full-scale 3x3 plateau core`: real clipped planes keep the old screen bit for bit (plateau cores 145 / 152
      / 37-52-24 on 0058 / 0075 / OSC 0163), a clip-free plane screens at full scale. Truthcheck frame ecc head - base
      -0.0015...-0.0020 (PASS); 169 / 189 planes with evidence 0 moved. Bounded concern: a clip-free plane of FWHM >~ 8.5
      px with a star >= 0.7 of full scale gives false evidence. Only the Population mode reads it.
    - **13b-P timers (debug-only).** `SubSteps` wall + thread-CPU timers for every sub-step ("frame plane substeps",
      "... fit", "fits artifact write timed"); after the review they exist only when `stacking::measure` is at debug
      (`enabled!` at creation: no clock read otherwise), `RcrTimer` reads clocks only inside `rcr_timing(true)`, and a
      `clock_gettime` failure warns once. The always-on version made detect 192 -> 406 ms on a mono plane in the bracket;
      gated, detect is within 1-2 % of 5836a970. Spec dictionary: `aperture_se`, `statistic`, `list`, `fitted`,
      `pre_saturated`, `plateau_cores`, and the clock-failure `error` field.
    - **Handed on.** (1) The **weight-redesign cycle, "faster than the ERP"**: a truth-validated weight that does not need
      every faint star fitted (the winsorized TMeanFlux is fragile in every variant: H's own fitted-only / all ratio is
      1.88); the (a) frame shape returns there with a truth gate. (2) **Task 13c** (parallelism research, per-machine
      budget, dynamic scaling, results identical at every thread count). (3) `c` against `c_true` on undersampled OSC,
      doubles and nebulae - already off on the base: doubles/f4.0 +16.1 %, sharp/m4/f16.0 +7-8 %, OSC +6...+10 %,
      nebulae +-11-12 %; not 13b's. (4) The **knot exemption** in `seeds.rs` reads the same plane-max band pre-fit
      (pre-existing, Task 10c). (5) A not-bit-identical rustafits item (the per-cell quickselect permutation feeds the
      mean sum, about -0.15 s per plane). (6) `minStars` reads `stars_fitted` as before; no settings change.

  - **R-OSP-13 - parallelism: each machine used to the full of what it has, every number unchanged (Plan A Task 13c;
    branch `one-star-path-t13c`, final code head 23b81727; research note
    `docs/superpowers/research/2026-10-18-task-13c-parallelism.md`; lab `star-lab/results/plan-a/{t13c,t13c-runner}/`).**
    *The bar and how it ended.* The owner asked for full use of every machine ("подобрано под машину, но так чтобы
    утилизировать все доступное") and set the bar as the WHOLE run faster than the ERP's whole run on the same frames,
    machine and work. **Met on the runner** (M4 Max, 12 P + 4 E, 128 GiB): Horse C11, 164 lights, 317 s against the ERP's
    2 787.6 s = 8.8× (Step 5, H a7d18865 377 s). **Not met on the dev Mac** (M4, 4 P + 6 E, 16 GiB): set 204 1237.7 →
    1105.5 s at b296dbc4 and **1081 s at the final head** (Step 5, H a7d18865 1170 s, N/H 0.923) against the ERP's 1005 s
    (+76 s: Measure +404 s is the gap). Dev Mac Horse C11 535 → 388 s (0.725). Step 5's two set 204 stages above 1
    (calibrate 1.029, register 1.018, 2 rounds) are bracket noise by Fable's ruling (`task-13c-ruling-fable-step5.md`); the
    Calibrate peak (3.88 GiB on a 4 GiB budget, swap 0) is L1's admission by construction. The
    rest of the gap goes to the fast-measurement cycle, below. Every lever is bit-identical by construction or by proof,
    and no artifact layout, cache hash, command or `ts_export` field changed.
    - **The rulings.**
      1. **Results land by index.** `fan_out` workers pull from one FIFO; a result lands at its item's index and commits
         in today's group-then-frame order. Nothing on the stacking path reads the thread count. The `AdmissionGate`
         retires a worker before its next pull and only ever lowers the count. `fan_out_groups` orders a merged queue by
         (working set desc, group, item) and scatters results back per group in item order. L17's jobs land at
         2 · slot + arm.
      2. **Admission comes from measured working sets**: `clamp((budget − shared) / working_set, 1, pool)`. In planes
         of one f32 channel: Calibrate mono 3 / debayered OSC 5 (`CALIBRATE_PLANES_RESIDENT_*`, measured, was 8), Measure
         8, Register 4 mono / 6 OSC, Normalize 8 (+ 3 masked). The L17 seeds arms use the Normalize working set.
         Integrate and Drizzle keep their own budgets (`integration.band_budget_mb`; Drizzle total / 2 plus L2's one
         prefetch plane).
      3. **Groups share one queue only when it lowers nobody.** `merged_admission` merges when at least two groups
         have jobs and the one queue's admission (largest working set, summed shared bytes) is ≥ every group's own;
         otherwise the per-group queues stay. Merged: Calibrate and Measure pass 1. Measure pass 2 stays per group,
         because the group β is a real barrier.
      4. **The budget is read per fan-out** (`fan_budget` before each one, not once per stage). The gate's live read is
         rate-limited to 1 s and goes through `try_conn`; an unreadable budget keeps the current target and never guesses.
      5. **Band prefetch for stacking only.** `IoPolicy::prefetch_local` is set at the one stacking resolve, which feeds
         Integrate and the LN reference build: prefetch plus a halved band on every storage class. Master builds keep
         network-only prefetch (unchanged, unmeasured). The Drizzle read-ahead (L2) falls back to serial when its extra
         plane would cross the R-M3-7 ceiling or a thread cannot be spawned.
      6. **The owner bar (2026-10-17).** A lever lands when it is bit-identical, swaps no more than H in any stage, and
         is faster beyond its interleaved bracket's spread on one machine with no regression on the other. Under 1 % of
         the run at medium or high cost → recorded, not merged (L4b).
      7. **The system allocator stays** (owner: «оставь системные контроли памяти, ос лучше знает»). The stage-to-stage
         carry-over is freed-but-retained heap (`Malloc Large (empty)`), which the OS reclaims under pressure. jemalloc
         removed it and every swap, and was reverted (afb87500). The relief call (`malloc_zone_pressure_relief` on macOS,
         `malloc_trim` on Linux) and the 4a-0 memory telemetry stay.
    - **Discovery and the budget** (`services/machine.rs`, probed once). Logical CPUs, P / E cores (macOS perflevels,
      Linux `cpu_core` / `cpu_atom` or capacity, Windows EfficiencyClass), the cgroup v1 / v2 or Windows job CPU quota,
      memory total / limit, and the available memory read live. The image pool is `RAYON_NUM_THREADS`, else the logical
      CPUs, or the quota when it binds; the 16-thread cap is gone. The budget is the KV `compute.memory_budget_mb`
      (Settings → Stacking → **Compute**, "Frame memory budget (MB)"): 0 = auto = a quarter of the memory the process may
      use (the cgroup / job limit when below physical memory). A larger value is clamped to that memory
      (`budget_why = "setting-clamped"`, one `warn!` per value), and `1` is valid (one frame in flight). Logged at info
      as `compute budget resolved` (each host, at start) and `stacking budget` (each run).
    - **Environment.** `RAYON_NUM_THREADS=<n>` sizes the image pool on both hosts (and rayon's global pool) and
      overrides discovery (`pool_why = "env"`; anything but a positive integer warns and sizes automatically).
      `ATHENAEUM_TOKEN_STORE=file` keeps a release build's hub token in the 0600 file, off the OS keychain. It is logged
      once at the first account call ("token store: file only"); any other value warns and is ignored. Debug builds are
      always file-only. The lab's `server.sh` and `star-lab-mono-run.sh` set it, so a rebuilt ad-hoc-signed server never
      raises a keychain prompt. `ATH_VMMAP_DIAG=1` (macOS, dev only) writes `vmmap --summary` at every stage start
      (`docs/logging/README.md`).
    - **Telemetry** (timing only; fields in the logging spec's Task 13c entries). `stacking stage started` /
      `stacking stage telemetry` (wall, CPU, P-CPU, runnable, cores busy, serial / full / gap ms, peaks, min available,
      swap delta, disk bytes; Linux PSI), a debug `stacking telemetry sample` every second, `fan-out finished` (items,
      admission, workers, tail, idle workers), `fan-out admission lowered`, the debug `cross-group queue decision`,
      `memory relief` and `memory checkpoint`. Every `Option` is logged as `-1`. `read_ms` changed meaning in L2 / L3
      (spec note).
    - **The measured gains** (median of 3, interleaved). **L1**, Calibrate working sets + a parallel distinct-master
      decode (13c11124): hc11 dev Mac 184.8 → 79.8 s (admission 1 → 4); 12 GiB container 23.3 → 12.8 s; set 204 and the
      runner level (disk-bound mono; admission already 16). **L17**, the seeds-calibration arms through one fan-out
      (ec547c3a): runner group L Normalize 27.9 → 22.8 s; dev Mac set 204 136.8 → 125.8 s. **L4a / L4c**, one queue across
      groups (2c5bd1ba, r1 da0837ed): runner Calibrate + Measure + Register 154.7 → 149.2 s (tails 31 / 27 → 6–8 %); dev Mac
      level. **4d**, the windowed residual median in `medfit_line`'s `rofunc` (d4535980, r1 b296dbc4; proof and
      measured-out variants in `combine.rs`): runner Integrate 98.6 → 79.2 s, CPU 1 504 → 1 194 s. **L3**, Integrate band
      prefetch on local storage (f3859de6, scoped in 23b81727): set 204 189.6 → 172.2 s, peak 6.79 → 5.89 GiB. **L2**,
      Drizzle reads frame i+1 while frame i deposits (bb240c05): set 204 86.4 → 70.8 s. **Whole runs:** runner Horse C11
      377 → 317 s (0.841); the container subset 117.6 → 105.0 s with no OOM. Identity PASS in every bracket.
    - **Determinism.** The matrix (pool 1 / 4 / 10 / 16 / 24, admission 1, band 256 MiB, a mid-run cap drop, pool 16 vs 16
      across processes) passed on both machines at a7d18865 and on the runner at b296dbc4 (the dev Mac at the final
      head: PASS (lab d7a5029: pool1, pool16, adm1, capdrop — admission lowered seen —, band256, pool10r vs pool10, 791 files each, 0 differing / 0 missing)). Its one finding, D1, was
      pre-existing: `source_cards_for_file` copied calibrated-header cards in `HashMap` order. They are now keyword-sorted
      (9486bf48). `.athm` is exempt from the byte compare by design (source mtime, pass-1 timings).
    - **Recorded, not landed** (numbers in the note; do not re-propose without new ones). jemalloc (owner). M2 / M3: the
      LN shared set is +83…+295 MiB once the retained heap is gone. L4b, one Register align queue: −1.2 s = 0.3 % of the
      runner run, dev Mac falls back; reverted 28be0f3e. L15, admission above the pool: ≤ 2 s after L4. L5, drizzle bands
      ≥ 4 × pool: 0.4 % of the run, needs an R-TA-10 proof. L6 (subsumed). L7, the Settings thread cap: deferred, no
      machine has > 16 CPUs. L8 – L13: no concurrent job, Measure 8 planes, CPU-bound reads, keep the E cores (r_E 0.25 /
      0.29), SIMD a note. L16: +1.25 GB on a 6.62 GiB Drizzle peak, refused on 16 GB. 4d's variants: an insertion-
      repaired permutation 1.27–4.57×, `total_cmp` alone 0.95–0.99×, partition bounds 1.04–1.18×.
    - **Carries.** (1) The **fast-measurement cycle** (owner order: 13c → it → Task 14): `measurement.fitStars` (All | K
      brightest, default All) shipped only past a truth bar, beside a GPU prototype per `task-13c-report-research-gpu.md`
      and its Fable review (Kernel 2 measured first; the verdict fits stay on the CPU). (2) The Normalize swap watch: one
      b296dbc4 run swapped 911 MB there, and Step 5's interleaved H / N decides. (3) The master-build prefetch,
      unmeasured. (4) The Linux, macOS 11–12 and Windows discovery paths are tested only through their parsers; no x86
      SIMD run.
  - **R-OSP-14 - fast measurement: All stays, the GPU path ends, per-core SIMD lands bit-identical (Plan A fast-measurement
    cycle; branch `one-star-path-fm`, final code head 61e526d6 (squashed onto `one-star-path` at the close); research note
    `docs/superpowers/research/2026-10-18-fast-measurement.md`; plan `docs/superpowers/plans/2026-10-18-fast-measurement-plan.md`;
    lab `star-lab/results/plan-a/fast-measure/`).**
    *The bar and how it ended.* Set 204 on the dev Mac 1 090.7 -> 847.1 s (0.777, interleaved bracket H ff3c0a83 / N db762cd9)
    against the ERP's 1 005 s: **met at the defaults** (All, GPU off), which also satisfies the owner's Q1 ("with the fast
    mode"). Final-head figures: whole run 1082.2 → 794.1 s (N/H 0.734; N runs 794.1 / 796.8 / 793.6), against the ERP's 1005 s (run 2: 968 s) — 21 % faster at defaults; Measure 0.624, Normalize 0.761, Integrate 0.699, Drizzle 0.844, Register 0.966, Calibrate 0.981; identity H3 / N3 1310 files + 197 rows, 0 differing; the per-stage swap max-rule misses come from single N runs and are the OS's response to read rate (Normalize reads ≈ 58 GB per run, now 25 % faster) — malloc at every stage start N ≤ H (lab `fast-measure/a1-mem/vmmap-compare.txt`); lab 1910574 (set 204), 164 lights, 3 pairs from Calibrate: whole run 305 → 267 s (N/H 0.876; the ERP's 2788 s → 10.4x), Measure 0.885, Integrate 0.667; identity 3 / 3 pairs (1962 files); swap 0 in all six runs; lab ae93eac (Horse C11 runner), MSVC / AVX2 on OBS-PC1, vector gate on ("S1 stress … vector == scalar"); `star_probe` probe-diff 0 / 0 / 0 on 0058 / 0075 / 0163_d; the only failing tests are the pre-existing Windows pins (open-items); lab 9d3035e (Windows identity).
    - **`measurement.fitStars` (All | K) is kept as a config field and a probe flag only.** F3's truth run (SK2) found no K
      that passes: K 4 000 / 5 000 / 8 000 miss K1 (estimator tilt), K3 (families worse than All) and K4 (14-21 of 32
      injected real planes move more than 1 %, worst -20.8 %: bright stars are not representative on sparse frames), though
      K2 (weight ranking) passes. No Settings UI, no preset, no panel (FM-19). The master is always measured at All (FM-28).
    - **The GPU path ends.** Two-sum fails in plain WGSL on Metal, Vulkan and DX12 (only MSL `math_mode(safe)` passes);
      Kernel 2 is 1.08x / 0.99x end to end (issue-bound, about 30 % of nominal); the GPU-able share of the run is about 89 s,
      10.5 % even at infinite speed, below SG6's 15 % (FM-31). No `gpu` feature, `compute.gpu` or module exists.
    - **Per-core levers, bit-identical per platform, scalar oracle kept:** S1 (Integrate selection, NEON / AVX2; combine
      0.655x / 0.633x), S1c (Median combination 0.68-0.71), S2b-1 (rustafits LM pass monomorphized + NEON; 1-thread 0.57-0.69x),
      S2b-2 (point-model columns two per vector; 0.95-0.97x). `ATHENAEUM_SCALAR_SELECTION` forces the scalar kernel and
      `ATHENAEUM_REQUIRE_VECTOR_KERNEL` makes the tests fail when no vector kernel ran (`docs/logging/README.md`).
    - **Measured out, do not re-propose:** K at 4 000 / 5 000 / 8 000; Kernel 2 on the GPU; the GPU star-fit path at All;
      percentile / sigma / winsorized internals on the vector selection; caching u / v / base from the cost pass; the
      target-rank pivot. **Next levers** (number-changing, truth-gated): a vectorized `pow` (about 31 % of the fit thread),
      Normalize's LN reference build (about 34 %) and detection (about 20 %). **Rule:** per-core efficiency before threads or
      GPU; compare work units with the ERP, not stage walls.

  - **R-OSP-15 - a field width robust to single-pixel events: route E, a 3x3 median on the detection plane of the field-width
    pass (Plan A Task 14b; branch `one-star-path-t14b`, code 5cf5f09b + review fix c8cdc1b3; plan
    `docs/superpowers/plans/2026-10-07-one-star-path-plan-a.md` `### Task 14b`; design `.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-14b-erp-design.md`;
    lab `star-lab/results/plan-a/t14b/`).**
    *The defect.* Since Task 10b (ruling A5) the field width is the half-sample mode of every readable pass-1 read, on the
    premise that every contaminant is WIDER than the PSF. A hot / warm pixel is narrower: raw m101's pools are mostly
    single-pixel events, the mode sat at the 1.5 px clamp, every gate scaled to a 1.5-px star, and nothing said so. At 69b4710b
    the stacking `measure` path read 0.379 (2025) / 0.094 (2023) of the reference product on m101 (29 of 30 frames at the floor)
    and 0.094 on the raw M51 Halpha single; despiked copies read 0.962. Calibrated broad sets were never affected.
    *The rule.* On a plane its caller states White (`PixelNoise::White`: raw / calibrated mono, the Analysis mode's 2x2
    super-pixel plane), pass 1 (the 3-px kernel whose readable candidates give the field width and the knot envelope) searches
    d_M, the 3x3 median of the background-subtracted plane d. Everything else reads d: the 6a neighbour rule (kept in both
    passes), centroid, peak, the half-maximum reads, pass 2, the super-pixel plane, the knot tests, the fits. d_M is dropped
    right after pass 1. A window holds its present pixels (finite, unmasked, inside the image); n >= 3 gives their median
    (even n: the mean of the middle two), n <= 2 gives an absent cell, and a cell whose OWN pixel is absent is absent on d_M
    (a masked centre never becomes a candidate). Noise: the noise map x `MEDIAN3_WHITE_COV`-derived gain G_M(F, step) (F 1.5
    0.666 ... F 24 1.119), K = 6 unchanged. Mixed / Unknown (debayered) planes run no median. If no cell of a masked plane has a
    median (an R/B-site mask) pass 1 falls back to d, as it did before route E. `from_stars == 0` pass 2 runs on d at 3 px.
    Cost: column triples sorted once, med3(max of mins, med of meds, min of maxs), NEON / AVX2 lanes equal to the sort oracle
    (`median3_cell`); MEASURE_VERSION stays 4 (one bump per merged plan) and staleness is proven by the runs' own logs.
    *Never silent.* `FieldWidth::at_floor()` (a method: `from_stars > 0 && fwhm_px < 1.607042`, so it survives the stored
    first-pass round trip) raises one `warn!` "field width at the clamp floor" per detected plane (`fwhm_px`, `mode_px`,
    `from_stars`, `width`, `height`) and sets `channel.fieldWidthAtFloor` (skip-if-false; summary and TS too). **It fires** on a
    sub-floor star field (all 12 lab `f1.2` families; ASI533MC `...0023`; the Gamma Scuti single `..._0010`, Step 5 ruling; R3
    unmoved) and on the Markarian raw CFA-site MEASURE planes p0 / p2 (field width 1.52-1.56, S3-R3: a mixed plane whose pool a
    narrow-read cluster wins, where design 3 had expected silence), and on a White pool still won by narrow reads (clusters of
    >= 5 pixels, a dense jump path). **Two silent limits** (the `at_floor` doc comment in `star_measure/seeds.rs` is the wording): (1) a debayered (Mixed / Unknown)
    plane with warm pixels - no median, so no flag unless the pool itself lands under the floor; triggers: a refused cosmetic
    map on an OSC stacking frame (it stamps no `ATH_CHPX`) and a debayered Analysis plane; (2) on a masked plane whose median
    has some present cell, a mask that thins the windows. The R/B-site CFA mask (no median cell at all) is NOT silent: it falls
    back to the first pass on d with one warn, and `at_floor` still fires on d. No production caller sets a mask today. Callers
    of given seeds (`field_width_of`: Peak / Structure seeds, LN refits, Plan C sites) carry `fieldWidthAtFloor` WITHOUT a
    warn - the warn lives only in the detection path. Owner of the limits: Plan C's per-site table (R-OSP-17).
    *Three recorded limits, with owners.* (a) Events inside a broad star's fit window: lab L3 completeness deficits against the
    twin (`spikes/a` -1.54, `b` -5.88, `c0000` -1.49, `c0010` -4.17 pp), identical under `off` (the bar is pass1 >= off; about
    one star: centreMoved / donut) - owner: a later fit-robustness cycle. (b) Half-sample-mode fragility on trail, flat-top
    and wind-shaken pools (E3c): Horse C11 14 of 106 planes move > 2 % under pass1 and none is median-specific - 761 L 0009
    (two satellite trails = 57 % of the pool: `off` 12.09 on the trail, pass1 6.86 on the stars, clean-star HSM 6.84, a near-tie),
    837 G 0004 (wind-shaken, no single width: 9.87 -> 8.68), 842 G 0004 (pass1 corrects `off`: events pulled it 11 % low,
    4.66 -> 5.25), 756 / 760 (flat tops, HSM noise); removing as many reads at random moves the same planes (median probability
    0.44 vs 0.01 elsewhere), the HSM bootstrap sd is 2.0 % = the R4 bar - owner: its own cycle. Horse C11 L 0010 / L 0011
    (2024-10-06) are untracked frames (every star a long streak) refused as trailed with "measure: the plane is trailed; no
    star is measured"; run 6 integrated 104 of 106. (c) Debayered planes: the median is off there (silent limit 1) - owner: Plan C.
    *Measured out, do not re-propose.* **`both`** (the reference product's whole model, pass 2 on d_M too): fails R1 `0015`
    (0.994), R3 m42 (|d| 1.12 / 1.47 %), singles (1.016, 4 frames > 2 %) and L5' completeness (10 of 11 families, worst
    `neb0.5` -2.44 pp): faint stars drop and the FWHM skews +1-2.5 %; nu50 at F 1.5 goes 5.9 -> ~10.5, broad +12-17 %. **The
    KNOT_Z veto** (keep a d candidate when K o M >= 3.09 G_M): keeps 6-10 % of 3-in-a-line events and 34 % of 2x2 events, and
    50 % / 38 % of m101's event seeds. **A d_M peak search** in `halfmax_read`: removes the ten ±2-px jumps onto an event but
    widens reads 3-7 %. **A box-mean background** for the median. **The synthetic census collapse:** the lab families
    `spikes/c0000` and `spikes/c0010` (star density 11.7 / 4.05 reads / Mpx, ln-sigma 0.145 / 0.048) did NOT reproduce the
    collapse (RED 4.885 / 4.885 and 11.951 / 11.951 against twins; event : star reads 21 : 49 vs real 440 : 728), so the RED
    proof rests on real m101 (R1, 0.379 / 0.094 under `off`). Route S (a neighbour-share screen below the floor read, plan text
    in 2da08d82) is the fallback on paper only; on Horse C11 it would move none (it engages below 1.607 px only).
    *Numbers.* Step 5: E head star_probe a445e109c2f2 differs from 69b4710b and the prototype; identity with the prototype's
    `pass1` 961 / 961 once the new `fieldWidthAtFloor` key is dropped (without it 20 files differ: 2 corpus singles and 18 lab
    `f1.2` planes; STOP P ruling); corpus R1 median 0.9469 (2025 0.947 / 2023 0.957, min 0.8705, 30 / 30 off the floor), `0015`
    0.9505, R2 M51 Halpha 0.9911, R3 m42 0.9942 (max gap 0.23 %), singles 0.9973; set 204 R4 PASS (407 planes, frame FWHM max
    0.05 %); Horse C11 R4 PASS by Q7's reading (14 moved, all explained: random band 14 / clean-star bracket 9 / reference 4 /
    nearer 3; median ÷ reference 0.9592 -> 0.9590); registration 100 % sharp and broad; determinism identical. Step 6: probes
    1041 / 1041 (inject 109 / 109); L1 0.9903-1.0000, L2 0.9968-0.9998, L3 pass1 >= off, L4, L5' 1523 / 1523 cells in 137
    families, F-10 recall `neb0.5` 0.777 / `neb3` 0.539, PA-2 0 / Mpx; the 18 appended set-aside families (12 lab incl. 3 twins,
    6 inject; S3 ruling) take `design.json` to 396 entries and the noreg diffs read 0 moved outside the list; the two OSC
    inject families move in `analysis-mode` only (its 2x2 super-pixel plane is White). Median-noise gain ratio +0.3 % / -2.7 % (E3's Q5 measurement on the prototype, carried to the E head by Step 5's identity - not a Step 6 number).
    Step 7 (lab 2739765): set 204 run 7 and Horse C11 run 6 succeed at their pinned references; "detection median plane built"
    92 / 106 (= the White planes), "frame plane first pass" 407 / 106, Measure rows = measured frames (no stale reuse), floor
    warns 0 / 0, gates 13 / 13, probe_parity 20 010 fits / 200 100 fields bit-equal. Step 8 (lab bc68fb9): median pass 0.262
    ms / Mpx (m101 `0000`, 61.17 Mpx) and 0.193 (set 204 `0058`, 25.94 Mpx) against <= 0.35; B3 m42 0.500 s per frame, 0.937 x
    69b4710b; 100 % of interior rows on the fast path; peak RSS delta vs 69b4710b +0.00 planes (star_probe 9.08 vs 9.07, m101
    9.46 vs 9.47). **Open item (pre-existing):** the mono Measure peak is 8.82-8.83 planes (`measure_probe`, `0058`), the same at
    69b4710b, against `MEASURE_PLANES_RESIDENT = 8` (fitted on an OSC frame): the working-set estimate is about 10 % low for
    mono frames (`docs/superpowers/open-items.md`).

  - **R-OSP-16 - Plan B: the Measure aperture correction `c` is kept, and the four before-runs (Task B0; branch `plan-b`, code
    c04ef5df + review fold 4566d628 + span fix 20697493; plan `docs/superpowers/plans/2026-10-20-one-star-path-plan-b.md` `### Task B0`;
    lab `star-lab/results/plan-b/b0/`). Later Plan B blocks append here; B6 closes it.**
    *Placement (additive: no measured value, hash or `.athf` moves).* `ChannelMeasurement` keeps `aperture_correction`, `aperture_se`,
    `aperture_stars` (`Option`, skip-if-None; payload `apertureCorrection` / `apertureSe` / `apertureStars`), copied from the module's
    report at the one channel build: all three or none. Floor (i) (under 5 ratios, `c = 1` applied) keeps none; a plane with no fit
    (`stars_fitted == 0`) keeps none and is NOT floor (i) (R-PB0-4). `MEASURE_VERSION` stays 4, no stage hash moves (the three fit-reading
    hashes of the default config are pinned in `config.rs`), an old payload decodes with `None`. One info per (group, channel),
    `"measure: aperture correction resolved per channel"` (`measure_group_finish`, pure `measure::channel_c_summary`): `plane`, `planes`
    (planes with a measured `c`), `count` (floor (i) planes), `aperture_correction` (median), `aperture_correction_min` / `_max` (new
    dictionary names); the per-plane line stays `debug`. LN, on the run summary: `SummaryFrame.ln_growth_factor` (mean),
    `ln_growth_pairs` (MINIMUM over planes, so a floor-(ii) channel shows), `ln_aperture_correction` (target `c`; `Some` only when every
    target channel has it; pure `ln::fold_plane_readings`); `SummaryGroup.ln_reference_aperture_correction: Option<Vec<Option<f64>>>`
    (`null` at floor (i)). Plain `Option`s (`null` in the payload), `src/types/stacking.ts` regenerated; an LN cache hit leaves them `null`.
    *Identity chain (PB-3: no pre-B0 run of a real set exists, so each link has its own fixed answer).* (a) Runner lab identity at
    4566d628 (`b0-ident`, lab cdaa835) and at 20697493 (`b0-ident2`, lab ce9c5af): probes 1041 / 1041 proven, 73 403 scorecard rows /
    0 missing, noreg vs t14-final 0 moved, cumulative vs t01c 0, inject 109 / 109 (3 328 / 0); `probe_identity` vs t14b: probe 1033 same,
    inject 109 same, 0 diff; channel-c 396 / 396 and 33 / 33, differ 0. (b) The three stage-hash pins pass unchanged. (c) Set 204 gates
    13 / 13 (`probe_parity`: 20 010 fits, 0 mismatches). (d) The kept values equal the debug `"aperture correction measured"` values
    plane by plane (`c_identity`): Horse C11 106 / 106, run B 386 / 386, set 204 407 / 407, differ 0, 0 unkeyed.
    *The four before-runs (B4's baseline).* Fresh acceptance clones re-measured from Measure (R-B15), reference mode as stored (R-PB0-1),
    staleness proven (`frame plane measured` = included planes: 106 / 386 / 407); lab commits 9fc40db, 55b7b64, 9368902 (lab main
    05500b9). Their LN reference md5s differ from the old trees' (pre-Plan A): B4 compares against THESE. One pick per run row:
    - Horse C11 (set 182): manual 69845, SAME as the old run; LN reference md5 G 7da6f7a1e8c2f639a9a2e8ce66c0396c, L a51e3c819654f0918b0c181008d6f13e.
    - Run B: manual 123, SAME; LN reference md5 mono 83d6efb4cf09d03f9a03271764c7fc2a, OSC f92ca7da8fd9ab52689fe4c86744d1d0.
    - Set 204: auto (twoPass) 28949, MOVED (old 28934; cause not established); LN reference md5 mono e0774e3502e6eee3c49800434bc4208e, OSC d96186f2b2230591d3bc27c81b10066e.

    | group | channel | planes | median | min | max | at floor | no fit |
    | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
    | Horse C11 G | 0 | 30 | 1.273 | 1.179 | 1.366 | 0 | 0 |
    | Horse C11 L | 0 | 76 | 1.288 | 1.215 | 1.427 | 0 | 2 |
    | run B mono L | 0 | 83 | 1.130 | 1.015 | 1.349 | 0 | 0 |
    | run B OSC | 0 / 1 / 2 | 101 each | 1.823 / 1.194 / 1.217 | 1.215 / 1.142 / 1.130 | 2.277 / 1.484 / 1.410 | 0 | 0 |
    | set 204 mono | 0 | 92 | 1.132 | 1.010 | 1.349 | 0 | 0 |
    | set 204 OSC | 0 / 1 / 2 | 105 each | 1.815 / 1.193 / 1.214 | 1.176 / 1.103 / 1.117 | 2.277 / 1.485 / 1.410 | 0 | 0 |

    The two Horse C11 L no-fit planes are the trailed frames 69990 / 69996. The OSC red channel's large `c` (median 1.8, max 2.28, against
    about 1.2 on G / B) is the undersampled-plane `c` of B4's F-9 / F-12. Truth re-read on each run's own LN reference: 101, 385 / 386
    (h152 plane 0, 9 stars) and 399 / 407 (8 mono plane-0 refusals of 4-9 stars) member-planes; the refusals are accepted (R-PB0-14).
    *The span fix (R-PB0-13, 20697493).* The first Horse C11 run failed `c_identity` (`planes 106 equal 2 differ 104`) though the payload
    equalled the log on 106 / 106 planes: the log lines had no span. The tracing span is thread-local and `ThreadPool::install` runs its
    closure on an image-pool thread when the caller is not a worker, so the Measure stage's per-plane `star_measure` events (ring sky,
    aperture correction, seeds, fit and verdict lines, two warns) were unattributed. `star_measure::install_in_span` enters the caller's
    span on the pool thread at all 17 `install` sites; the LN reference build gets `ln_reference{group_key}` / `ln_reference_plane{plane}`.
    Logging only (the probe has no pool; the runner identity was repeated at 20697493). An event from a `par_iter` leaf is spanless again.
    *Rulings.*
    - R-PB0-1: the reference mode stays as stored (set 204 auto; run B and Horse C11 manual); B4 compares the recorded pick.
    - R-PB0-2: `planb.sh truth` takes the HEAVY.lock.
    - R-PB0-3: the identity drops the three channel `c` keys and checks `channel.X == top-level X` (`--drop-path`, `--check-channel-c`).
    - R-PB0-4: a plane with `c = None` and `stars_fitted == 0` is left out of the summary; `count` is the planes whose under-5 warn fired.
    - R-PB0-5: `SummaryFrame.ln_aperture_correction` is `Some` only when every target channel carries `c`.
    - R-PB0-6: the per-frame LN fold is one pure, pinned function (pairs MINIMUM, factor mean).
    - R-PB0-7: `ln_reference_aperture_correction` is `Option<Vec<Option<f64>>>`, `null` at floor (i); old `[1.0, ...]` rows still decode.
    - R-PB0-8: B0 must not reach main without B4's `MEASURE_VERSION` 5 (one merge, at B6); see the open items below.
    - R-PB0-9 (amended by R-PB0-13): `c_identity` leaves out only lines under a non-empty span list without `measure_frame*`; spanless fails.
    - R-PB0-10: B0's identity lives at `star-lab-a/b0-ident*`; `--bars` is omitted; `--prev t14-final`; the runner fast-forwards from a bundle.
    - R-PB0-11: every Plan B `lab_cycle` passes `--design results/plan-b/<stage>/design.json` (B0's is `[]`; cumulative union: Plan A's 701).
    - R-PB0-12: an identity across machines also drops `options.analysis.batch_concurrency` (a runtime knob outside the config hash).
    - R-PB0-13: fix the log's attribution, not the copy site: `install_in_span` plus the `ln_reference` spans; no by-value join.
    - R-PB0-14: a truth run whose every bad row is the harness's own "only N usable stars" refusal is accepted; any other bad row is a STOP.
    - R-PB1-1: B1 uses the B0 build in `target-acc` (identity proven); its sha256 check reads B0's build record.
    *Open items carried.* (1) R-PB0-8: a pre-B0 cached `metrics` row carries no `c` and would log `planes 0`, so B0 stays off main until
    B4's `MEASURE_VERSION` 5 (`docs/superpowers/open-items.md`). (2) Scorecard format (B4): B1 adds an `erp` cell to every LN row, so a noreg
    against a scorecard built before it (t14-final) lists every LN row `changed`; re-score `b0-ident2`'s probe with the new scorecard
    (`--reuse-probe`) for a same-format `--prev`. (3) `integrate.rs` has the same pool hop (the master's `c` line lacks `integrate_group`):
    `install_in_span` if B4 wants it, out of B0's scope. (4) `planb.sh truth` exits 4 on refusals only (R-PB0-14): print the count, exit 0 (B6).
    *Suites (B0 Step 6, 2026-10-23, at 20697493):* `cargo test -p athenaeum-core --all-targets --no-fail-fast` 28 binaries 4105 passed /
    0 failed / 20 ignored (Plan A's 4091 + 14 new; no flake this run); `tsc --noEmit` clean after the `stacking.ts` regeneration. B0 closed.
  - **R-OSP-16 - Plan B B1: the LN re-baseline on Plan A's fits, the ERP beside every arm (Task B1; branch `plan-b`, no product code; plan
    `### Task B1`; lab `star-lab/results/plan-b/b1/`, its `README.md` names the machine and volume of every table). A baseline: no bar.**
    *Runs* (all at the B0 build, 20697493; dev Mac data on BigMac unless noted). Lab: truth rebuild + ERP-beside code 6c848ed, synthetic table and
    audit fa7785a, l10b on the runner 5a10fe5, Steps 3-5 tools 9385e6c, ln_real + F1 v2 read 685cb7f, ERP on the real sets + inject e428e5d,
    package 7d31ece. `ln_real`: 23 run / 23 proven, 543 member-planes, 2077.5 s. ERP LN on the real sets: 515 member-planes, 0 refusals, scale
    1597 from the owner's batch record (8 instances, all 1597). Gap = s / truth - 1, pooled median / median |.|, %:

    | table (n) | f1_gc | f1_3 | f1_2 | psfSel | psfOff | ERP |
    | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
    | synthetic, 24 families, beta auto (133) | +1.274 / 1.274 | +1.007 / 1.014 | +0.954 / 1.209 | +0.038 / 0.203 | +0.137 / 0.287 | +0.430 / 1.533 |
    | l10b nebula-sky, 15 groups, beta auto (43) | +0.345 / 0.624 | +0.195 / 0.266 | +0.164 / 0.339 | +0.238 / 0.687 | +0.166 / 0.543 | -0.239 / 1.772 |

    The l10b a1c (earlier-binary) columns equal ours in every pooled row; 2 of 45 ERP members refused (neb3/g t3 at 36 stars, neb3/m4 t3 at 19).
    *Real sets* (f1_gc median gap / median |.|; ERP median gap and slope in %/px2; the 3 % flag = an arm and the ERP both miss truth by more
    than 3 % the same way; `.athln` parity = the probe's f1_gc over the run's sidecar):

    | set (n) | f1_gc | ERP gap / slope | 3 % flags | parity median / max |
    | ---- | ---- | ---- | ---- | ---- |
    | set 204 mono (91) | -0.108 / 0.280 | -2.78 / -0.29 | 39 of 91 | +0.0077 / 0.194 |
    | set 204 OSC (39) | -0.179 / 0.433 | +4.45 / -0.60 | 2 of 39 | +0.0034 / 0.236 |
    | run B mono (82) | +0.092 / 0.327 | -3.64 / -0.30 | 52 of 82 | +0.0057 / 0.236 |
    | run B OSC (303) | -0.166 / 0.389 | +3.54 / -0.63 | 15 of 303 | +0.0051 / 0.212 |
    | Horse C11 G (28) | -0.114 / 0.296 | not re-run (2026-10-06: -2.44 / -0.06) | 0 of 0 | -0.0465 / 1.226 |

    Pooled parity n 543: median +0.0050 % (2026-10-07: +0.0097), max 1.226 %. The flags are `psfSel` / `psfOff` only, never f1_gc, f1_2, f1_3
    or sLib; carried to B2, not resolved here. F1 v2 port, real read on 58 re-dumped members (median gap, 2026-10-06 beside, never a bar): set 204
    V0-gc5 -0.05 % (-0.64), L3-gc5 -0.68 (-1.01); Horse C11 G V0-gc5 -0.11 (-0.13), L3-gc5 +0.12 (+0.08); the port against f1_gc on the same
    dumps at most 0.10 %. Injected pairs: the ERP's LN is the whole-frame ratio 1.02-1.27 against s_inj 0.8, our arms read 0.79-0.80.
    *Audit and truth.* LN freshness audit: 24 `ln.csv` on disk, 24 audited, 19 CURRENT hash-identical + 5 header-only, 0 STALE (first run audited
    21: the family filter took exact names, `erp_v2` takes `/` prefixes); l10b 15 / 15 CURRENT. Truth: s204's two CSVs rebuild byte for byte, run B
    (82 + 303) and Horse C11 G (28) rebuilt with recorded sha256, the five reference md5s equal the pins; `check-truth` NOT RUN.
    *Rulings.*
    - R-PB1-2: `ln_real.py check-truth` is not run (its three input trees were deleted before Plan B); the rebuild's byte-identity and the md5s stand.
    - R-PB1-3: an ERP member refusal is a recorded cell ("ERP refused (N stars)"), not a failed run; any other missing scale stays a failure.
    - R-PB1-4: the injected pairs' ERP LN runs at scale 1597 (full 6248x4176 frames), `ATH_ERP_LN_SCALE`; the synthetic default 512 is unchanged.
    - R-PB1-5: on the injected pairs the ERP's scale is a whole-frame quantity, printed with that label, never compared with s_inj or flagged.
    - The ERP's real-set gaps are a column, not a referee (owner rule). Context (B2, R-PB2-2): the real cells' truth is not yet refereed (normal
      mode plus a per-cell far read), so every gap above is "against the truth rows as they stand".
    *Open items carried.* (1) Horse C11 G parity max 1.226 % (range -0.395 .. +1.226, median -0.0465) is 5x the other trees' 0.19-0.24 %: B3 looks
    at it. [B6: closed, verdict (a), a probe re-computation artefact.] (2) The ERP's gap (mono -2.8 / -3.6 %, OSC +4.4 / +3.5 %, slope -0.3 to -0.6 %/px2) and its mono / OSC sign flip: cause not
    investigated, B6's report. [B6: a recorded limit, not investigated; the ERP is a column, never the referee.] (3) `ln-real-verdict.txt` predates the ERP runs and only the heavy `planb.sh b1 ln-real` rewrites it: it carries a
    dated note pointing to `ln-real-table.md`; B6 hygiene is `planb.sh b1 table` regenerating it. [B6: stays stale with its dated NOTE; `b1 table` refreshed the rest (e428e5d).] (4) Lab hygiene (B6): `planb.sh b1 audit`
    exits 0 on an UNAUDITED file (the coverage line is informational; `l10b_b1.py audit-gate` reads only the summary line); `planb.sh truth`
    exits 4 on refusals only (R-PB0-14); the ERP writes a stray display-profile file under `star-lab/true/` on the runner (its working
    directory at launch): `run_job.sh` should `cd` to a scratch directory first. [B6: all settled, lab 4e4e757 / fc0a3e2 / ecddc4f, see the B6 block.]
  - **R-OSP-16 - Plan B B2: the referee, measured; the field realism; the seeing-lever pairs (Task B2; branch `plan-b`, no product code; plan
    `### Task B2`; lab `star-lab/results/plan-b/b2/`, its `referee-verdict.md` is B3's input; lab package commit 03e300d). A measurement: no bar moved.**
    *Both-modes read (Steps 1-2; lab 8ca5ebd merged at 05500b9, read on the runner at 39c82ca).* The harness truth with its mode forced
    (`force_mode`, default byte-identical) over 115 cells x 2 modes: normal 29 read / 19 miss / 86 n/a, crowded 73 / 12 / 42; no crowded miss
    beyond FWHM 5 because nothing beyond 5 reads (the 14F / 9F isolation on 50-px grids, not a defect). The tool is right (truthcheck t4 and
    d600 t3 equal the 2026-10-07 gates digit for digit). 28 of the 31 misses are resolution limits (one-unit stored cells, SE 0.5-1.9 % against
    +-0.3 %); the three `iso/m4w10` crowded cells (t25 -1.32, t35 +0.93, t45 +2.06 %) are a real curve-of-growth bias proportional to dF.
    *WO-7: the growth-corrected crowded mode is measured out (Step 2b).* Built (`crowded_growth`: r_ap 5F, annulus [9F, 11F], c = flux(5F) /
    flux(8F) per frame; the sparse winged family `truthcheck_w10`; planb-b2b a70f01e) and read on the runner (WO-3, e44bf6f, 120 cells): the
    corrected mode reads 78 cells with 60 misses against the plain mode's 73 with 12 (iso/m4w10 t45 +15.6 %, d600 t3 +7.3 %), adds +1.5 to
    +3.7 % on resolved core cells and on `truthcheck_w10` only equals the normal mode. That family also shows the NORMAL mode biased on winged
    stars, proportional to dF (t1..t5 at dF -1 / 0 / +1.5 / +3 / +4.5: -1.01 / -0.31 / +0.86 / +1.57 / +2.86 %, k 0.6-1.0 %/px). The option is
    pinned OFF (the code stays, default byte-identical), its drivers are retired, WO-4 (its real re-read) is cancelled, and it is never re-proposed.
    *The far read (WO-5 e791b2d, WO-5b bf917bb, WO-6 621d2ac).* The same bright stars read again at twice the radii; a seeing-scaled wing makes
    Delta (x2 minus x1) opposite to dF. The runner validation was withdrawn (R-PB2-3: 28F isolation, 126 px, does not fit the 120-px lattice).
    Validated instead by unit renders on a 300-px lattice against the enclosed-flux algebra: x1 / x2 / Delta within max(0.15 %, 3 SE) at four
    FWHM pairs (x1 -0.84 / +0.83 / +1.23 / +1.43 % against -0.88 / +0.89 / +1.23 / +1.40), five mutations (ring, aperture, isolation, peak,
    growth radius) each fail the suite. **Real data (WO-6, standard rows asserted identical to B0): UNREADABLE.** Far n per member is 2-4 and
    no member reads at both scales with n >= 10, so k_far is n/a everywhere and every counted cell takes the w10 ceiling allowance:

    | set (B0 before-run) | rows | std (normal, counted) | crowded-dF (flagged) | n/a | far n median | members n >= 10 | abs(dF) median / max (px) |
    | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
    | set 204 (mono 31 + OSC 315 counted) | 407 | 346 | 53 | 8 | 2 | 0 | 0.60 / 2.48 |
    | run B (mono 27 + OSC 303) | 386 | 330 | 55 | 1 | 4 | 0 | 0.67 / 1.93 |
    | Horse C11 (G 28 + L 64) | 101 | 92 | 9 | 0 | 2 | 0 | 0.80 / 2.90 |

    *Field realism (Step 3; `realism_table`, lab f10585a, opus verdict).* Mono is realistic: 19 of 22 families inside the real range on both
    quantities like-for-like (crowding 0.0053-0.0989, c - 1 0.0096-0.3661), e.g. pairs/main/m4 0.0597 / 0.2302. OSC is not: 0 of 6 `osc_pairs`
    plane-rows inside (crowding 4-13x too sparse; plane-0 c - 1 0.20 against a real median 0.82). Step 3b is NOT triggered. Beside it, F1's
    synthetic over-read (+1.27 % pooled, +2.06 % on realistic mono targets, +1.9 % at the real abs(dF)) sits on realistic fields; the real
    f1_gc gaps are -0.18 to +0.09 %, at most about 0.9 % with the whole allowance granted: the synthetic size does not carry over.
    *The seeing-lever pairs (Step 4; lab bbc6a29, dev Mac).* Six pairs on set 204 mono, s_inj 0.8, empirical B-spline stamp; injected aperture
    sums 0.997-1.005 (gate 0.95-1.03), 12 / 12 probe outputs proven by the B0 build, the ERP's whole-frame scale beside (R-PB1-5). Gap %, beta auto:

    | dF (pair) | -0.742 (f264) | -0.400 (f300) | -0.097 (f340) | +0.760 (f400) | +1.597 (f500) | +2.532 (f592) | slope %/px |
    | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
    | PSF signal | -4.19 | -2.53 | -2.16 | -6.61 | -6.25 | -7.76 | -1.54 |
    | 2F circle | -0.84 | +0.54 | -0.54 | +0.32 | +2.01 | +1.60 | +0.76 |
    | 3F circle | +0.18 | +1.24 | +0.52 | +1.03 | +2.54 | +2.90 | +0.79 |

    The PSF-signal arm reads 2-8 % low at every dF and is flagged (3 % rule) on 4 of 6 pairs (5 at beta 4); the circles stay inside -0.8 to +2.9 %.
    **Not a product finding (owner, 2026-10-23: the offset unlinked in the settings on purpose, no other lights).** The set 204 mono tree was calibrated
    with an OFFSET-200 master dark on OFFSET-30 lights (170-206 ADU over-subtracted, 198 by the pooled fit; the camera gain holds, free fit 1.067).
    Plan B is blind to it (LN scale and `c` are local-sky-subtracted); the lever gate became the six-source pooled free fit. Standing decision in `open-items.md`.
    *Rulings.*
    - R-PB2-1: the tool is right; B3 counts normal-mode rows only (interim, superseded by R-PB2-2 and R-PB2-6).
    - R-PB2-2: the growth-corrected crowded mode is dead and pinned OFF; the referee is the normal mode plus a far read.
    - R-PB2-3: the runner validation of the far read is withdrawn; unit renders against the algebra; k_far is per dataset, an allowance, not a per-member band.
    - R-PB2-4: the empirical stamp defaults to the cubic B-spline (bilinear fails the 1 % width test: +7.1 / +4.3 / +1.7 % wide).
    - R-PB2-5: lever placement is >= 8F from real peaks of >= 20 sigma (12F from every peak placed 1-19 of 300 stars at FWHM >= 3.4).
    - R-PB2-6: the far read is unreadable on the real sets: the referee is the normal-mode truth with the ceiling allowance 1 %/px x abs(dF) per cell, crowded rows
      flagged `crowded-dF` and never counted, refusals n/a; B3 decides on the referee-free pairwise and slope comparisons; no further referee round.
    - R-PB2-7: mono fields are realistic, OSC fields are not; Step 3b not triggered; B3 reads the referee-free comparisons.
    - R-PB2-8: the lever gain gate is the free fit pooled over the six sources (bounds 0.9-1.1 unchanged), the pedestal recorded; pixels and s_inj untouched.
    *Open items carried.* (1) The OFFSET mismatch above: a MANUAL dark override (`is_manual_override = 1`, 2026-08-28), the owner's deliberate link (2026-10-23) - closed, nothing to build. (2) The halo's dependence on F (seeing-scaled
    wing against a fixed-pixel optical halo) cannot be sized on real pairs; the candidate cause of the F1 size gap, unmeasured. (3) No synthetic OSC
    family represents real OSC crowding; no mono family reaches set 204 / run B crowding (max 0.0597 against 0.0753 / 0.0849). (4) Lab hygiene (B6):
    `planb.sh truth` exits 4 on refusals only; `ln_truth_run --far` prints an intercept beyond 2 SE without a usability rule (moot: nothing reads).
    No product code changed in B2 (B0's core line, 4105 / 0 / 20, stands); lab suites at bbc6a29: star-lab 1335 OK, harness 349 OK (2 skipped).
  - **R-OSP-16 - Plan B B4: `c`'s screen and sample, the two floor verdicts, `MEASURE_VERSION` 5 (Task B4; branch `plan-b`, code
    21c151a0 + review fix 71a4a738 + floor (i) 13aee664; plan `### Task B4`; lab `star-lab/results/plan-b/b4/`, its `verdict.md` the
    package, lab e3e85ec). `c` moves by design (R-B11); on the three real trees nothing moved.**
    *The point-source screen (PB-12', R-PB4-2, R-PB4-3).* On a debayered colour image (adapter colour, exactly 3 planes) a `c`
    candidate (after `in_statistic`, a finite model and the clip screen) with no SEED of another plane within
    max(`POINT_SOURCE_MATCH_PX` = 1.5 px, 0.5 × the plane's field FWHM) of its fitted centre leaves the list before the sort. The
    siblings are the other planes' `prep.seeds`, gathered after `cross_plane_flags` in both multi-plane entries and handed to the
    correction through `finish_plane`: a sibling's at-β fits do not exist yet when a plane's `c` is computed, so PB-12's "no fit on
    any sibling" was not buildable, and the seeds refuse the same stars in the dumps. Mono, one-plane images and LN's per-plane refits
    are not screened. The radius grows with F because the R–G / G–B centroid offset (atmospheric dispersion, lateral colour) scales
    with the sampling as the seeing does (the review's Important 1); set 204 OSC (F ≈ 1.3–1.6 px) stays at 1.5 px. Logged
    `point_source` on the debug `"aperture correction measured"` line and on the under-5 warn (`skipped` stays the clip count);
    `PlaneMeasurement.point_source_screened` (reported, not persisted); probe `pointSourceScreened`; dictionary `point_source`.
    *The sample (PB-13').* `c` = the median of the 64 brightest candidates by model flux (ties by seed index; R13b-6 stands) among those
    passing the screen with `StarFit.snr` ≥ `CORRECTION_MIN_SNR` = 20 (NaN fails); no strata; `CORRECTION_MIN_RATIOS` stays 5. `star_snr`
    runs before `aperture_correction` in `fit_at_width` and `correct_at_ring` (it never reads `c`); HFR stays after. Logged `below_snr`
    beside `point_source`; probe `apertureBelowSnr`, `apertureTerciles` (the terciles of the ≤ 64 sample, never read) and
    `--dump-ratios`; dictionary `below_snr`. F-12 asked for a brightness-stratified sample; Step 0b (opus, existing frames only)
    refused it: on 344 lab planes with truth every stratified or mid-band option moved some family's `c` away from truth by more
    than 2 SE (4 × 16 brightest per quartile 47 planes, rank-spread 74, mid-band 85, no cap 33), the top 64 at SNR ≥ 20 after the
    screen 0. Lab truth is brightness-flat to < 1 % at SNR ≥ 20; the raw 0004 p0 trend (aperture / model 1.57 → about 1.2) is a
    β-bound fit bias (the fitted FWHM grows with faintness, β at the 1.5 floor), not a profile property. Step 0b's screen reads: lab
    truth 0 of 15 715 refused at SNR ≥ 20, raw real stars 0 of 6620, 0013 p0 hot pixels 35 / 35, set 204 OSC ≤ 0.15 % per plane.
    *`MEASURE_VERSION` 5 and its ripple.* 4 → 5, Plan B's one bump (R-OSP-1's policy). `"measureVersion"` moves the three fit-reading
    stage hashes (`measurement_subtree`, `registration_subtree`, `normalization_subtree`; pin
    `measure_version_five_moves_the_three_fit_reading_subtrees_only`); `calibration_subtree` is unchanged; the `plan.rs` goldens are
    re-recorded. The first stack after the upgrade re-measures, re-registers and re-normalizes every set once and never re-calibrates.
    R-PB0-8's condition (B0 reaches main only with version 5) is met.
    *Floor (i): the product change (R-PB4-5, 13aee664).* At the end of Measure for a group (after pass 2, BEFORE the weights), per
    channel, m = the median kept `c` over the group's members carrying one (`channel_c_summary`'s median, the value of the per-channel
    info). Every member plane at floor (i) (`aperture_correction` None AND `stars_fitted` > 0, R-PB0-4) is rescaled when at least 2
    peers carry `c`: `tflux`, `tmean_flux` × m; `psf_signal_weight`, `psf_snr` × m² (exactly `c`'s effect, no refit). Under 2 peers c = 1
    stays and the module's warn is the record. `aperture_correction` / `_se` / `_stars` stay None; the additive
    `ChannelMeasurement.aperture_correction_group: Option<f64>` records m. The substitution lives in the fold (`measure_group_finish`;
    `take_group_c`, `at_floor`), never in `aperture_correction`, the `.athf` fits or the `metrics` payload, and m is re-derived from the
    group's current members every run, cache hits included. Logs: info `"measure: floored planes took the group channel's median
    aperture correction"` (`count`, `aperture_correction`) per (group, channel) with a substituted plane, and the per-plane debug; no
    new field name. LN is untouched (F1's scale is `c`-free). `MEASURE_VERSION` stays 5, no stage hash moves; pin
    `fit_call_floor_behaviour_matches_the_owner_ruling`.
    *Floor (ii): kept (R-PB4-6).* Not reached in the product; the code stays as built (1–4 growth pairs keep the factor with a warn,
    0 pairs factor 1 with a warn), nothing removed. The LN model fixes cycle's owed question ("fall back to 1 below 5 pairs?") closes
    as moot. Pins: `a_growth_curve_over_a_few_pairs_reports_the_count_and_keeps_c`, `a_growth_curve_without_a_qualifying_pair_keeps_c_one`.
    *The verdicts and the numbers (`verdict.md`; every value copied from the lab files).* Both floors by the rule (R-B13 / R-B16);
    the owner's STOP (PB-18) delegated to fable:

    | floor | counted | median abs(error): c = 1 / own 1–4 ratios / group's median kept c | paired difference | verdict |
    | ---- | ---- | ---- | ---- | ---- |
    | (i) Measure's fit-call floor | 183 planes, 2 datasets (lab-d200, s204-osc) | 19.079 % / 14.726 % / 2.540 % | c = 1 − arm 3 +16.539 ± 0.654 %; own − arm 3 +12.185 ± 1.020 % | favours arm 3 → R-PB4-5 |
    | (ii) LN F1's growth floor | 0 of 504 cases / 72 windows | — | — | not reached (lowest 14 growth pairs) → R-PB4-6 |

    Floor (ii): 193 cases excluded by `MIN_MATCHES` 20 first, 294 kept ≥ 20 selected pairs with ≥ 14 growth pairs; fewer than 5 of
    ≥ 20 needs ≥ 75 % of the matched stars within 5F of an edge, which no crop, 2 × 2 binning or masked corner produced. Floor (i):
    a floored R plane (f28889) weighs 0.303 of its group at c = 1 and 0.888 at its reference `c`, psfSel's s 0.586 / 1.064 (arm 3) /
    1.003 (uncropped); on the after-run DBs a random member sits |c / median − 1| p50 / p90 8.2 / 21.8 % (set 204 OSC p0), 8.2 / 18.5 % (run B p0), 1.5 / 5 %
    (p1), 2.2 / 7.5 % (p2), 2.4–2.8 / 11.5 % (mono), 1.4–2.2 / 6 % (Horse C11), where c = 1 errs by 45 % (R), 16 % (G / B), 12 % (mono).
    *The × 0.40 R case (raw 0004 → 0013, debayered).* X1 PASS: 0013 p0 `pointSourceScreened` 1984 (bar ≥ 36), 0 hot pixels among
    the product's 33 qualifiers. X2 as written MISSED (R signal ratio 0.9208 against the G / B mean 0.9531, −3.39 % vs 3 %), and
    R-PB4-4 re-reads it on `c`: R's `c` ratio 0.8615 against the G / B mean 0.8601, +0.17 % (2 SE 7.86 %) PASS; the −3.39 % is the
    c-free model-flux part (−3.56 %: R at the β 1.5 floor, F 1.43 → 1.47 against G 1.76 → 2.04), out of any `c`'s reach; Plan A's
    hot-pixel sample reads 0.634, −26.3 %. Sharp R at the group β 2.2436 (calibrated 0176 → 0206): B4's `c` equals B0's kept `c`
    (p0 1.934 / 1.210), signal ratio p0 0.6801 against 0.9695 / 0.9761 on G / B (reported).
    *The lab cycle (runner, `b4-c2` at 71a4a738; lab db9003e, d04546b).* 1041 / 1041 probes and 109 / 109 inject outputs proven,
    73 403 scorecard rows. BAR PASS: F5.1 spread +0.76 % (f2.6) / +1.41 % (f8.0) against ≤ 2 %; psfSel blur pairs +1.72 / +1.19 %
    against ± 2 %, 60 Moffat / Gaussian and wing cells 0 miss against ± 1.2 %; R13 0 of 130 row-6 cells beyond 2 SE of Plan A; `halo`
    0.9984 (Plan A 0.9984); the screen refused 0 truth stars on the 10 clean colour families and screened nothing on mono; floor (i)
    planes 0 of 396 before and after. noreg FAILED first on 11 analysis-mode row-6 cells (mono plane 0: density/d5, spikes/c0010/m4/
    f12.0 ± clean, spikes/b/m4/f12.0/clean): R-PB4-7 accepts them as PB-13' on the shared `aperture_correction` (the SNR floor runs on
    every plane), mirroring the measure path's set-aside cells (d5 +0.55 % on both; pooled analysis-mode median 0.9980, mean |bias|
    0.0124 before and after); with that design entry the re-diff on the stored scorecards PASSES (0 moved); cumulative vs t01c 0 moved;
    same-star WORSE bins none. The inject per-frame row-6 table printed 0 frames (a lab reader gap, fixed in lab 5b01e0a after the
    probe dirs were deleted), so B4's table is not printed; the inject scorecard is proven and judged by noreg.
    *Master level (the after-runs at 71a4a738 against B0's, picks pinned to B0's, R-B11).*

    | tree | staleness: measured / registered / normalized | weights ρ per group | movers | spec §8 summary | LN vs truth, re-based |
    | ---- | ---- | ---- | ---- | ---- | ---- |
    | Horse C11 (69845) | 106 / 101 / 101 | G 1.0000, L 1.0000 | 0 | 6 / 6 PASS | +0.00 % |
    | run B (123) | 386 / 184 / 184 | mono 1.0000, OSC 0.9999 | 0 | 6 / 6 PASS | +0.00 % |
    | set 204 (28949) | 407 / 197 / 197 | mono 1.0000, OSC 0.9999 | 0 | 6 / 6 PASS | ± 0.00 % (B0 within 0.01 %) |

    Master median / MAD / noise within 0.05 %, FWHM within 0.36 % (set 204 OSC drizzle 2× p0, a measured-β difference), rejected
    fractions identical; set 204's absolute drizzle level 3 / 4 and coverage 2 / 4 read as at the base. The `c` tables equal B0's,
    at_floor 0 everywhere: no real plane reaches floor (i), and the screen and the SNR floor leave the 64-star samples unchanged.
    Floor (i) proof at 13aee664 (lab dcde2e0): the set 204 re-run against the B4 after-run, ρ 1.0000, 0 movers, 0 substitution lines.
    *Rulings.*
    - PB-12': the point-source screen (sibling seeds, colour only); PB-13': the top 64 by model flux at SNR ≥ 20, no strata.
    - R-PB4-1: B4 Step 0's runs use the B0 build (identity proven, R-PB1-1).
    - R-PB4-2: the screen's radius is max(1.5 px, 0.5 × the field FWHM).
    - R-PB4-3: the screen only on a 3-plane colour image; `StarFit::snr`'s doc (read before `c`); `apertureTerciles` stays the sample's.
    - R-PB4-4: the × 0.40 bar reads the `c` ratio within max(3 %, 2 SE) of the G / B mean; the signal and c-free ratios are reported.
    - R-PB4-5: a floor-(i) plane takes its group channel's median kept `c` in the fold (≥ 2 peers), never in the payload.
    - R-PB4-6: floor (ii) not reached in the product; the code stays; the LN question closes.
    - R-PB4-7: the 11 analysis-mode row-6 moves are PB-13' by design; design.json sets aside analysis-mode row 6.
    *Open items carried.* (1) Plan C: on raw debayered planes with β at the 1.5 floor the `extended` classifier removes 50–95 % of
    the bright stars from `c`'s sample frame-dependently and `c` follows by ± 20 % (R-PB4-4); the group-β path is not shown to share it.
    (2) Plan C: the crowded-field `c` bias on density/d5, +0.8 % (R-PB4-7). (3) Plan C: a seeing-conditioned stand-in for floor (i)
    (nearest peers by `fwhm_px`), unmeasured; the stand-in's R error is seeing-sized (p90 22 %). (4) PB-13''s known limits: OSC R / B
    `c` +4–7 % above truth on 44 / 48 lab rows whatever the sample; cost if wrong about 8 % over-correction of faint stars on β-bound
    undersampled R. (5) The `metrics` payload shows a floored plane's module `c` (None) while its weight is substituted; an additive
    per-frame field would make it visible (optional). (6) If B3 adopts psfSel, LN's own floor (i) takes the same m through
    `FluxCorrection::Given` (a B3 / B5 line); B3 kept F1 (R-PB3-1), so it binds only if psfSel is ever the default. (7) The ERP's 15 "no file" units are Task 14b's families (14 `spikes/*`,
    `under/gauss/f1.5/i0`), a Plan A debt; B3's tables are unaffected. (8) Lab hygiene (B6): the harness flake
    `test_planb_b2 …lock_that_stays_held_exits_75…` (full suite only); the runner's stray `star-lab/true/` profile file.
    *Suites.* `cargo test -p athenaeum-core --all-targets --no-fail-fast` at 13aee664: 28 binaries 4115 passed / 0 failed / 20 ignored
    (B0's 4105 + 10). Lab at the tools commit: star-lab 1408 OK, harness 388 with that one flake.
  - **R-OSP-16 - Plan B B3: the LN estimator stays F1 as is (Task B3; branch `plan-b`, no product code: the code stays 13aee664; plan
    `### Task B3`; lab `star-lab/results/plan-b/b3/`, its `verdict.md` the package, lab 7e291fb). The owner's choice (PB-18), delegated to
    fable: R-PB3-1. Nothing in the product moved.**
    *The tables (every value copied from the lab files).* `synthetic-table.md` (133 cells, 28 slope rows, 21 injected rows; after = the B4
    binary's cycle `b4-c2` re-scored on the runner, no probe run, lab fd54b5f; before = B1's rows at Plan A's `c`, lab aedfe27);
    `real-table.md` + `real-members.csv` (`ln_real` at the B4 binary on the B4 after-run clones `planb-<ds>-b4`, the LN reference pinned to
    B0's pick, the reference B2's referee cells were measured on, so not the plan's "three trees", whose references differ; Horse C11 L
    added; 885 member-planes, 34 / 34 proven, 767 counted, 117 crowded-dF never counted; lab b1af182); `entry-test.md` + `entry/` (lab
    d3bd6f2); `verdict-inputs.md` (lab ddd7da7). Lab tools 9f74ee5 (`b3_table.py`, `f1v2/entry_test.py`, `planb.sh b3 <job>`).
    *The candidates, on the same rows, β auto.* F1 as is (`f1_gc`, L-24's default; Measure's `c` does not enter it), psfSel on B4's `c`
    (its s moves 1 : 1 with `c`, F-9), and the F1 v2 sky (port L3-gc5) only under its entry test. Synthetic, cells within their bar, after
    (before = after in every class verdict: B4's `c` moved none):

    | class (bar) | F1 as is | psfSel on B4's `c` |
    | ---- | ---- | ---- |
    | 19 `pairs`, Moffat / Gaussian (± 1.2 %) | 24 / 70, worst +11.97 % (`pairs/main/m4w10` t60) | 70 / 70, worst 1.06 % |
    | 19 `pairs`, blur (± 2 %) | 27 / 35, worst 11.14 % (`pairs/main/m4mix` t60) | 35 / 35, worst 1.72 % |
    | `truthcheck` / `density_pairs` (the reading mode's ± 0.3 / ± 0.8 %) | 2 / 4, 1 / 6 (d600 t3 4.38 %) | 4 / 4, 6 / 6 |
    | `osc_pairs` (± 2 % per plane; unrepresentative, R-PB2-7) | 14 / 18 | 17 / 18 |
    | injected pairs (± 2 % of s / s_inj) | 14 / 14 | 6 / 7 (star-lab-mono2 OSC p1 −2.36 %) |

    Real, counted cells against B2's referee (median abs(gap) / slope in %/px of ΔF, the referee's own ceiling being ± 1 %/px), and the
    referee-free pairwise psfSel / F1 − 1 over every member-plane (median / slope):

    | dataset (counted) | F1 as is | psfSel on B4's `c` | pairwise |
    | ---- | ---- | ---- | ---- |
    | set 204 mono (31) | 1.17 % / +0.871 | 1.79 % / +0.722 | −0.50 % / −2.281 |
    | set 204 OSC (315) | 0.39 % / +0.273 | 1.46 % / −4.573 | −0.76 % / −4.853 |
    | run B mono (27) | 0.65 % / +0.263 | 3.72 % / −3.911 | −6.82 % / −3.186 |
    | run B OSC (303) | 0.40 % / +0.652 | 1.28 % / −4.281 | −0.70 % / −4.932 |
    | Horse C11 G (28) | 0.25 % / −0.163 | 2.02 % / +0.635 | +2.03 % / +0.802 |
    | Horse C11 L (63) | 0.17 % / −0.140 | 0.41 % / +0.432 | +0.15 % / +0.932 |
    | pooled (767) | 0.40 % / +0.121; 4 cells over 3 % | 1.32 % / −0.812; 167 cells over 3 % | — |

    On members 1–2 px worse than the reference psfSel reads −6.65 / −7.13 / −5.34 % (set 204 OSC / run B mono / run B OSC), F1 −0.5 … +0.3 %. The seeing
    lever (B2, truth by construction, s_inj 0.8): F1's 2F circle −0.84 … +2.01 %, +0.76 %/px, none flagged; PSF signal −2.2 … −7.8 %,
    −1.54 %/px, 4 of 6 flagged. B1's columns printed beside are another reference (set 204 mono: the tree's F 3.385 px against B0's pick,
    ≈ 5.1 px) and another truth (B1's `phot`), never a before → after (R-PB3-2): set 204 mono F1 0.21 % there against 1.17 % here is not a
    move. That referee is thin (n_std 10–11 on every counted cell, SE median 1.19 % against allowances of 0.05–0.78 %), so its "23 / 31
    outside" is printed, not read; the sound mono referees read F1 −0.56 % (run B, SE 0.53 %), −0.08 / −0.02 % (Horse C11 G / L). On OSC,
    F1's four 3 % cells (plane 2, ΔF ≈ −1.25) and −0.8 % plane-2 medians are shared by psfSel (pairwise −0.90 / −0.65 %): the plane or the
    referee, not the estimator. Horse C11 L f70062 p0 has a referee cell and no scale (the verdict line's 768 counted against 767).
    *The F1 v2 entry test: OUT (recorded, not a STOP).* (1) The port's pooled median abs(gap) over 58 real members 0.582 % against F1's
    0.296 % (re-dumped at the B4 binary; both 0.0000 pp from B1's read); (2) the cosmic-ray cell `main/m4w10` t60 moves +1.351 / +0.132 /
    +0.637 pp on seeds 1–3 against ± 0.53. Step 2b skipped: `ring_sky` takes no caller F or union mask.
    *The decision.* F1 as is stays the default (`LocalNormalizationConfig::default`, `fluxEstimator: commonAperture`, `aperture:
    growthCurve`; L-24 stands, PSF signal stays a choice). Steps 4–5 skipped: `integrate.rs` untouched, no masters run, B3's after-runs are
    B4's. Recorded beside R-PB2-7: "F1 misses the synthetic `pairs` bar (24 / 70, +11.97 % at `pairs/main/m4w10` t60) and is kept: on 767
    real counted cells it reads median |gap| 0.40 % with every dataset's slope inside the referee's ±1 %/px ceiling, while psfSel, exact on
    synthetic, reads 1.32 % with −3.9…−4.6 %/px on real OSC and run B mono and −2…−8 % on the lever."
    *B5's branch.* F1 as is is not module-fed: its seeds are `register::detect::detect_stars` on the integrated reference and Measure's fits
    (or detection) on the target, the mismatch corrected by the seeds calibration `k` (C-14, `stacking/ln/calibration.rs`), and its flux
    reads its own a·F circles and [6F, 8F] sky, not the module's ring or `c`. B5 Step 0 therefore takes the "F1 as is" branch: Steps 1–2
    (module seeds and their guards) are skipped, `k` and `unverdicted_clip_range` are kept and recorded beside LN's other screens (Register's
    0.95, L-14's 0.75, L-23's map); Steps 3 (C-9, the nebula sky; P.1 reads psfSel and F1) and 4 (R11-3, the package) run, against B4's
    after-runs wherever B5 names B3's.
    *Rulings.*
    - R-PB3-1: F1 as is stays the default; psfSel on B4's `c` is rejected; F1 v2 is out by its entry test. Cost if wrong: F1 over-reads the
      LN scale of members worse than the reference by ≤ ≈ 0.8–0.9 %/px of ΔF (the lever's +0.76), on the lowest-weight members; psfSel
      instead would have put its −5 … −7 % on ΔF > 1 px members into every OSC and run B master.
    - R-PB3-2: B1's before columns are another reference and another truth, never a before → after of the estimator; the set 204 mono referee
      (n 10–11) sets no level. Cost if wrong: a real ≈ 1 % F1 under-read on crowded set 204 mono goes unflagged; psfSel is worse there (−1.79 %).
    *Open items carried.* (1) Plan C: F1's synthetic-vs-real size gap, the halo's scaling with F (B2's item 2); a synthetic family with a
    fixed-pixel halo would settle it. (2) Plan C: psfSel is no default candidate again until it passes the lever pairs (abs(gap) ≤ 3 %,
    abs(slope) ≤ 1 %/px). (3) The set 204 mono referee at n 10: recorded only. (4) The tools' concerns, open: the lever pairs and the
    realistic family have no B4-binary row (the lever reads the B0 build; `c` on calibrated set 204 is unchanged by B4, so identical by
    construction; the family was never built); the `b4-c2` probes are 71a4a738's, not the 13aee664 binary's (floor (i) is stacking-fold
    only and the cycle read 0 floor-(i) planes; unverified). (5) B1's item (1), Horse C11 G parity max 1.226 %, was not examined in B3: B6.
    (6) B4's item (6) binds only if psfSel is ever the default. No product code changed in B3 (B4's core line, 4115 / 0 / 20, stands); lab
    suites at 9f74ee5: star-lab 1436 OK, harness 403 OK (2 skipped).
  - **R-OSP-16 - Plan B B5: LN's own estimates - the F1 branch, C-9 no change, R11-3 and B4's `c` on nebula recorded (Task B5; branch
    `plan-b`, no product code: the code stays 13aee664; plan `### Task B5`; lab `star-lab/results/plan-b/b5/`, its `verdict.md` the package,
    lab ff6070e). Rulings R-PB5-1 / -2 / -3 (fable) and the owner's word on R11-3. Nothing in the product moved.**
    *The results (every value copied from the lab files).* `l10b-{verdict.txt, p1p2-table.md, cells.json, provenance.txt}`: the nebula-sky
    P.1 / P.2 table at the B5 binary (the runner's own build of 32f61897 = the 13aee664 code; 60 frames sha256-equal, 174 / 174 outputs proven;
    the ERP column is B1's committed `ln.csv`, no ERP run; lab 6324185). `r11-{verdict.txt, pull.txt, provenance.txt}`: R11-3's pulls (dev Mac,
    the build equal to B4's b4f record, 6 / 6 proven; lab a03ce38). `ctable-{verdict.txt, table.md, cells.json}`: Measure's `c` per l10b plane
    at B1 and B5 against the truth's `c*` (runner, lab 75f4fcd). Tools: lab 4994f54 (`planb.sh b5 l10b / r11 / clean`, `b5_l10b.py`,
    `r11_pull.py`, `l10b_report.py --arm`) and a49b1e0 (`bin/b5_c_table.py`, `planb.sh b5 ctable`).
    *The scope (Step 0, the F1 branch).* B3 kept F1 as is (R-PB3-1), which is not module-fed, so Steps 1–2 (module seeds and their guards)
    were skipped and LN keeps its seeds path: `register::detect::detect_stars` on the integrated reference, Measure's fits on the target, the
    seeds calibration `k` (C-14) between them, F1's own a·F circles and [6F, 8F] sky. `k` and `unverdicted_clip_range` are kept, recorded beside
    LN's other screens (Register's 0.95, L-14's 0.75, L-23's map) in the spec's owned-quantities table: `k` still corrects the seeds-path
    mismatch (+0.666 % median, +1.401 % max against detection), and LN's refits still run on a path without a saturation verdict, so they
    still need the clip screen. Retiring both is R-B9's module-seeds swap, in scope only for a module-fed default. Steps 3–4 ran at B4's
    binary; B5's "B3's after-runs" read B4's (B3 ran none).
    *C-9, the nebula sky: no code change (R-PB5-1).* The research put the cause outside `ln/background.rs`: the tile grid enters only the
    selection and the log-only L-26, and psfOff (no selection, no tile σ) misses the same cells. psfSel's misses are `c`'s; F1's scale with
    the aperture area (neb0.5 g t60: 4F² −0.87, 9F² −1.34, 25F² −3.06 pp), a median sky on structured nebula when the two sides'
    noise-to-structure differs. Its proposal, the [6F, 8F] annulus MEAN, is the F1 v2 study's L1-gc5, measured out on the real members: set 204
    mono 30 of 30 worse, −0.40 pp median (SE 0.03), median |gap| 0.64 → 1.04 %; Horse C11 G +0.21 pp, 0.20 → 0.25 %, a wash. The cosmic-ray
    cell does not separate the estimators: F1 as shipped moves −1.42 / −0.14 / −1.29 pp on `pairs/main/m4w10` t60 and −2.36 / −1.71 / −2.52
    on `pairs/main/m4` t60 against ±0.53 (the mean +0.23 / +2.46 / +3.13 there); the 5F growth circle is the fragile part, recorded, not
    proposed. A 3σ-clipped mean is a wash on real data. The plan's C-9 STOP is void (no change).
    *The P.1 / P.2 table at the B5 binary (R-PB5-2): `B5 L10B: PASS`, 0 worse, 0 lost.* F1, the default and the one judged, reads D5 = D1
    bit for bit in all 34 cells (B4's `c` does not reach it); neb3 m4 t60 and neb3 g t60 are n/a in both (6 / 11 matched stars, < 20). Over
    the F-13 bar (±1.2 pp m4 / g, ±2 pp b25mix, plus 2 SE on neb3), 4 of 34 at both binaries, all at t60:

    | cell | F1 Δ against flat, pp (SE) | bar |
    | ---- | ---- | ---- |
    | P.1 neb0.5 m4 t60 | −1.67 (0.20) | 1.20 |
    | P.1 neb0.5 g t60 | −3.07 (0.20) | 1.20 |
    | P.2 shift m4 t60 | −1.59 (0.12) | 1.20 |
    | P.2 warp m4 t60 | −2.18 (0.12) | 1.20 |

    R-PB5-1 / -2 quote 5 of 34: the research's fifth cell, neb0.5 g t45, is −1.193 against 1.20 (rounded to −1.20 there), inside. psfSel,
    printed with its owner and never judged: 15 of 34 over (13 at B1), worst neb3 g t45 −15.89 and neb3 b25mix t25 −12.28 pp; B4's `c` took
    neb3 m4 t25 +0.26 → +6.14 (SE 0.17, bar 1.46) and neb3 g t25 +1.18 → −2.57 pp (SE 0.14, bar 1.58) over, every other cell ≤ 0.15 pp. The
    "not worse" rule (|D5| − |D1| ≤ 2·√(SE5² + SE1²), the tools' choice) is moot at D5 = D1.
    *R11-3 at the B5 binary.* Pulls `background/neb0.5` 3.64 (n 457) and `background/neb3` 5.98 (n 301) against the Task 11 record 3.63 /
    6.04: SAME within ±0.25. The pull is reconstructed (robust std of (apSum − S0) / (apSum / snr) over unflagged measure stars, S0 the true
    star flux in the same aperture; nothing committed produced the record) and reads 3.64 / 5.98 on Plan A's t14b outputs too, so "the same"
    is at a re-run's precision. The owner's word (2026-10-23, PB-18): a recorded limit, no owner and no bar in Plan B; re-opened only by a
    master-level miss on a real nebula, with an owner and a bar declared before measuring (the ERP has no per-star flux error either; RB4's
    structure term broke 4 clean cells, −7…−25 %).
    *B4's `c` on the nebula sky (R-PB5-3; the owner chose "check, then fable").* Measure's frame `c`, the weight path (`tmean_flux`,
    `psf_signal_weight`, `psf_snr`), on the 60 l10b planes: 59 same, 1 closer (neb3/m4/t1 −3.72 → −0.78 %), 0 farther; median |c − c*|
    0.36 → 0.36 % (neb3 5.10 → 4.85 %). psfSel's move is LN's own per-side `c`: its s is `rf.signal / tf.signal` (`ln/scale.rs` l.606–617)
    on LN's refits, whose `c` is `photometry::aperture_correction` on LN's fit list with `siblings: None` (`fit.rs` l.2406: the point-source
    screen does not run, only B4's SNR floor and the brightest 64 act); neb3 g t25's Measure pair is unchanged (−1.61 %) while psfSel moved
    3.75 pp. psfSel is `LnFluxEstimator::PsfSignal`, reachable as Normalize → "Star flux" → "PSF signal" (non-default; the default
    CommonAperture is F1).
    *Rulings.*
    - R-PB5-1: F1's [6F, 8F] annulus sky stays the median; no C-9 code change; F1's P.1 limit recorded with its numbers. Cost if wrong: F1
      keeps −1.6…−3.1 pp on single-frame nebula t60 cells; the real nebula set (Horse C11 G) reads 0.20 % median |gap|, no master-level loss
      is known; a later real nebula miss re-opens it with a per-side, structure-aware estimator, never a plain or clipped mean.
    - R-PB5-2: P.1 is judged on F1's 34 cells against B1's (not worse beyond 2 SE); psfSel's count is printed with its owner (F-10 / B.3; it is
      LN's non-default "PSF signal", its `c` from `star_measure`), never a STOP for an LN change. The controller's note: 4 of 34, not 5. Cost
      if wrong: an F1 nebula regression is caught only at 2 SE on single-frame cells; the real guard (`ln_real`) stays the arbiter.
    - R-PB5-3: (a) a recorded limit, no code change, `MEASURE_VERSION` stays 5; `planb.sh b5 ctable` is the standing check for any later `c`
      change. Cost if wrong: a user who picks PSF signal on a deep-nebula field gets a ≈ 4–6 % LN scale error on some pairs (13 of 34 cells
      before B4).
    *Open items carried.* (1) B6 hygiene: a `cRef` / `cTgt` per-side field on `ln_scale_probe`'s per-arm line
    (`ReferenceFits::aperture_correction`, `target_aperture_correction`), so LN's own `c` is read, not inferred; the runner's B5 l-probe can
    be cleaned, the numbers being committed. (2) PSF signal on deep nebula, 15 of 34 l10b cells over, owner F-10 / B.3; if it is ever to be
    supported there, the measured lever is the pair-common ring (neb3 g t45 −15.88 → −8.42), not a Measure-side screen: recorded, not
    proposed. (3) B1's Horse C11 G parity max 1.226 %: B6. No product code changed in B5 (B4's core line, 4115 / 0 / 20, stands); lab
    suites at a49b1e0: star-lab 1462 OK, harness 415 OK.
  - **R-OSP-16 - Plan B B6: the checkpoint, Plan B closed (Task B6; branch `plan-b`, final sha 5103310e; lab `star-lab/results/plan-b/b6/`, its
    `after-table.md` the package, lab 60a5fc3; index `star-lab/results/plan-b/README.md`). Rulings R-PB6-1 (withdrawn) and R-PB6-2.**
    After report: `docs/superpowers/research/2026-10-24-star-measurement-after-plan-b.md` (every bar as judged, the owned quantities at close,
    the recorded limits with owners, what Plan C needs). Nothing in the product moved after 13aee664 except the fold below.
    *The runs (every value from the lab files).*
    - `b6 cycle` (runner, 5103310e): `B6 CYCLE: PASS (every bar)` - probe 1041/1041, inject 109/109, noreg and cumulative moved 0, B4 bars equal, F-11 PASS, HELD PASS (lab 2366176).
    - Scorecard identity b4-c2 vs b6-final (runner): moved 0, max |delta| 0 over 73 403 rows (lab d44e3bb).
    - `b6 l10b` (runner): `B6 L10B: PASS` - 60 frames, 174/174, same as B5, F1 4/34, psfSel 15/34 (lab e63c4fc).
    - `b6 gates` (dev Mac, planb-s204-b6): `B6 GATES: PASS (13 / 13)` - determinism 185/185, probe_parity 0 mismatches over 200 100 fields (lab 8569c8b; r1 11/13 was R-PB6-2).
    - `b6 ln-real` (dev Mac): `B6 LN-REAL: PASS (a read)` - 23/23 trees, every tree's F1 gap equal to B1 (lab ec97596).
    - `b6 ln-truth` (dev Mac, all three trees): `B6 LN-TRUTH: PASS` - measure-equal to B4's after-runs (durationMs ignored), weights rho 1.0000, 0 movers, masters ratio 1.000000 (lab 8e9f305).
    - `b6 report`: the after-table (lab 60a5fc3). Build record: star_probe and measure_probe byte-identical to b4f; ln_scale_probe differs by the `cRef` / `cTgt` keys.
    *Rulings.*
    - R-PB6-1 WITHDRAWN: it proposed ln-truth on run B only, reading s204 / hc11 from B4's after-runs by binary equivalence. Its premise was false:
      B4's hc11 / runB / s204 after-runs ran at 71a4a738, before floor (i) (13aee664); only b4f ran at 13aee664. ln-truth ran all three at 5103310e (the plan as written), B4's after-runs the comparator. Caught before any run.
    - R-PB6-2: the determinism gate's 185/186 was a lab registry defect (`truthcheck_w10`, B2's in-memory-only family, never in the v2 manifest), not a regeneration miss; fixed in the lab, 13/13 after.
    *Product.* hygiene 4437eda8: `ln_scale_probe` prints LN's own per-side `c` (`cRef` = `aperture_correction()`, `cTgt` = `target_aperture_correction`); no product behavior.
    Review fold 5103310e: the screen-radius phrase, the "metrics artifact" phrase, and a run test pinning floor (i)'s wiring (`measure_folds_a_floored_member_to_the_group_channel_median_c_before_the_weights`).
    *Review.* The final whole-branch review (fable, at 4437eda8, before the runs): READY, 0 critical / 0 important / 4 minor, 51 targeted tests passed; minors 1-3 folded at 5103310e, the 4th deliberate.
    *Suites.* Core all targets 4116 passed / 0 failed / 20 ignored (runner 3684 / 0 / 19 with the three network modules skipped + dev Mac `--lib` network modules 432 / 0 / 1; the runner box fails the localhost-QUIC tests, an environment item); web 51 / 0; `--no-default-features` check Finished; vitest 82 files / 898 tests; lab star-lab 1491 OK, harness 439 OK.
    *Lab hygiene, settled.* exits_75 flake deterministic and `star-lab/true/` gitignored (lab 4e4e757); `planb.sh truth` refusal exit (lab fc0a3e2); `planb.sh b1 audit` stops (exit 4) on an UNAUDITED file, B6 stage labels fixed (lab ecddc4f); the `ln_truth_run --far` intercept rule is moot (nothing reads it); B1's `ln-real-verdict.txt` stays stale with its dated NOTE (only the heavy `b1 ln-real` rewrites it; `b1 table` refreshed the rest in e428e5d).
    *B1 open items at close.* (1) Horse C11 G parity 1.226 %: closed, verdict (a), a probe re-computation artefact (the `.athln` side is the 2026-10-06 before-run's measurement, f70055: 384 matches vs the probe's 523; hc11's members are the noisy ones, per-star sigma 0.042-0.049 vs 0.009-0.018); B6's hc11 ln-truth row agrees. (2) The ERP's real-set gap and mono / OSC sign flip: not investigated, a recorded limit (the ERP is a column, never the referee; our F1 meets truth on those sets).
    **R-OSP-16 CLOSED (2026-10-24): Plan B complete; merge / push on the owner's word.**
- **Plan A closed (Task 14, 2026-10-20).** Note: `docs/superpowers/research/2026-10-20-star-measurement-after-plan-a.md`,
  every Plan A bar against its baseline cell in `2026-10-07-star-measurement-baseline.md`.
  - *The run.* One uniform lab run at c10be63b (`star_probe` 695bb4935f67; lab cf07135) wrote v2 1041, inject 109 and
    corpus 420 outputs, one binary each, identical to Task 14b's own. Every bar is met or carries its ruling. 13 gates
    PASS on set 204 run 7. No row moved outside the 701-entry design list against the 2026-10-07 baseline.
  - *The headline.* The captured-fraction spread went from 18.73 / 22.16 % to 0.77 / 1.41 %. A 2× flux twin moves the
    frame FWHM 0.06 % (9.99 % before, `sharp/m4/f2.0`). The Gaussian frame FWHM reads −0.28 % (−2.58 %, f8.0). m101
    `measure` ÷ the ERP is 0.947 / 0.957 (0.379 / 0.094 before Task 14b). Set 204's whole run takes 794 s against the
    ERP's 1005 s.
  - *Debt carried* (owner 2026-10-19, «Закрыть, долг перенести»). The debt check against t01c stays OPEN on fwhm
    scatter (measure, pointfit), centroid scatter (measure) and centroid scatter (register). The centroid goes to the
    owed per-frame pixel-phase correction, the FWHM scatter to research. The Task 14 debt lines fall under the same
    ruling: the centroid F ≤ 2 split (re-read at c10be63b: measure ×1.0140, register ×1.0112), the owner ecc items 6 / 7 (WORSE, 0.09187 → 0.09339) and the
    11b single-frame ridge.
  - *Open items.*
    - The VNG per-plane widths and the resampling broadening (out of scope).
    - The injected OSC captured fraction, 1.81–2.34: explained, a scorecard / truth artefact (I.1 extended, sized by the Measure aperture correction `c`).
    - The Measure aperture correction `c` on undersampled OSC planes: not persisted or logged, hot pixels feed it on an uncorrected R plane, one `c` per plane overstates faint stars (note section 7; first Plan B item: log `c` per plane - done by B0, R-OSP-16).
    - R-OSP-15's two silent limits and three recorded limits, with their owners.
    - `MEASURE_PLANES_RESIDENT` (8 against 8.82 mono).
    - The ERP refresh for Task 14b's 15 new units.
    - F1's over-read and the nebula-sky LN pairs go to Plan B; the Analysis switch (`analysis-mode`, 0.500 s per m42
      frame) goes to Plan C.
- **R-OSP-17 - Plan C: the Analysis tab on the one star path, `MEASURE_VERSION` 7 (Tasks C0-C10; branch `plan-c` from `main` 4255a3f5,
  plan `docs/superpowers/plans/2026-10-24-one-star-path-plan-c.md`, ledger `.superpowers/sdd/2026-10-24-plan-c/progress.md`, rulings
  `.superpowers/sdd/2026-10-24-plan-c/rulings.md`; spec `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` §4; lab
  `star-lab/results/plan-c/`).** The Analysis tab, Blink's star overlay and the plate-solve input gate read the stacking Measure's
  `star_measure` module instead of the legacy `ImageAnalyzer`; the rustafits analyzer stays in rustafits, unused by the app (owner
  2026-10-24, option A; its switch is the owner's rustafits track). Plan C is the Analysis switch only; Plan B's `c` items and the rest go to
  Plan D (`docs/superpowers/open-items.md`). Code: C1 6f0dcc8d / e93c7e92 (`star_measure` seams), C2 0043b7b2 / a1b79699 (raw input),
  C3 65ad923d / 3d756181 (the CFA green-site path), C4 3d8a745f (trail R²), C5 d66f7f88 / 056c2772 / 6ee0d9f5 (the analyzer switch, the config,
  the probe arms), BUMP (rustafits 1.6) 08446186 / 2764e12b, C6 595ef880 / eaa1d88a / ec928042 / 127226cb (persistence, compatibility, gate trust,
  frame meta), C7 62e24c48 / 365cc9f9 (the gate), C8 4497f5d5 / 0c59fad1 (frontend), C9 0b3a281b / 0872d6e5 / d811c054 / 16c0a2ec / 41ca7bad (app look fixes, Blend), rustafits #7 follow-up 47c33f73, C10B 16c8e2aa
  (Blend on the Analysis path only, `MEASURE_VERSION` 7; R-PC-10b). Final code sha **16c8e2aa**.
  *Planned rulings PC-1 … PC-15 (one line each; evidence: the plan and the task reports under the ledger folder).*
  - PC-1 `MEASURE_VERSION` stays 5 unless a stacking-path number moves - **SUPERSEDED by R-PC-10**: the Blend refusal moves what a fit
    accepts, so 5 → 6 (Plan C's one bump, R-OSP-1's policy; `AnalysisConfig::config_hash` folds it; Analysis rows store the version that
    measured them); **then by R-PC-10b**: 6 → 7 (6 burnt), the stacking path back to v5's numbers, every Analysis row recomputes. Evidence:
    R-PC-10, R-PC-10b, `c9-report.md`, `task-C10F-verdict-r1.md`.
  - PC-2 raw input scales: an integer FITS takes its full scale from BITPIX/BZERO, XISF UInt16 / Float32 take 65535, a float FITS with
    maximum ≤ 1 is ×65535, `clip` = DATAMAX else the full scale, gain = EGAIN else none (the SNR omits its shot term, stated). Amended by
    R-PC-2. Evidence: `task-C2-report-r1.md`.
  - PC-3 BITPIX 16 / BZERO 0 is read signed; a file with > 1 % negatives is reported, never guessed. Amended by R-PC-1 / R-PC-1a. Evidence:
    C0 census (`star-lab/results/plan-c/before/census.tsv`).
  - PC-4 a Bayer mosaic is measured on its green sites: detection on a 2×2 super-pixel sum, seeds mapped to native pixels (offset 0.5, pinned
    at seed level), fit on the native mosaic with the green-site mask, sky and noise from one green phase, `c` = 1 (Analysis star fluxes on
    OSC are uncorrected, a recorded limit). Detection source amended by R-PC-3. Evidence: `task-C3-report-r1.md`, `task-C3-review-r1.md`.
  - PC-5 a debayered 3-plane file is measured on its G plane (`colour: true`, `PixelNoise::Mixed`, `cfa_site_share: Some(0.5)`); R-OSP-15's
    silent limit 1 (no field-width median on a debayered plane) stays recorded. Evidence: `analysis/analyzer.rs`.
  - PC-6 a frame the module judges trailed or defocused has no measured star; `possibly_trailed` = the trailed verdict; trail R² = the Rayleigh
    R² over the measured stars' canonical θ, stored for compatibility. Evidence: `task-C4-report-r1.md`.
  - PC-7 a frame with no fit in its statistic writes 0.0 for `median_fwhm` / `median_eccentricity` / `median_snr` / `median_hfr`. Evidence:
    `analysis::analyzer::map_measurement`.
  - PC-8 `star_metrics.snr` / `hfr` are nullable; a NaN is stored NULL (`Option<f64>`). Evidence: `task-C6-report-r1.md`.
  - PC-9 `median_beta` is NULL under Moffat 4 and the fitted β under Auto when its source is `Fitted`. Evidence: `analysis/analyzer.rs`.
  - PC-10 Settings → Analysis keeps `detection_threshold` (σ, default 6.0, range 2-20), `psf_model` (`moffat4` default, `auto`), `max_stars`
    (default 500; `measure_cap` folds in) and `batch_concurrency`; the detection sigma, star-area, saturation fraction, MRS layers, PSF-fit knobs
    and Trail Threshold settings are removed. Evidence: `task-C5-report-r1.md` (the config), `task-C8-report-r1.md` (the panel).
  - PC-11 `frame_snr` is background / noise, a ratio: no "dB" label anywhere, the per-filter stacked value is √Σ frame_snr². Evidence:
    `task-C8-review-r1.md` (I1, I2).
  - PC-12 the gate's ecc AND R² pair stays only if C7 measures it refusing a hopeless frame the verdict lets through without refusing a
    solvable one. **Ruled REMOVE** (C7D): no pair on 95 frames; the gate is the verdict alone; the two settings and the stale-default
    migration go. Evidence: lab `star-lab/results/plan-c/c7/verdict.txt`, `task-C7D-report-r1.md`.
  - PC-13 the frame verdict is stored (`frame_analysis.frame_verdict`: `trailed` / `defocused` / `no_stars` / NULL); the input gate refuses
    a current row whose verdict is `trailed` on that alone; `defocused` and `no_stars` never refuse. Evidence: `task-C6-report-r1.md`,
    `plate_solve/service.rs::input_gate_reason`.
  - PC-14 every frame's solve wall time and outcome recorded in C7; the hopeless frames the gate still lets through are listed with their
    numbers and go to Plan D, no rule invented (see "C7" below). Evidence: lab `star-lab/results/plan-c/c7/gate-frames.tsv`.
  - PC-15 the Verdict column replaces Trail R² (Analysis table, Blink frame panel, Black Hole section, CSV; the R² chart metric goes) and
    "Reject trailed" becomes "Reject unusable frames" (any verdict, or a legacy row by its old `possibly_trailed` until re-measured); the
    stored threshold key `trail` keeps its name. Evidence: `task-C8-report-r1.md`, `task-C8-report-r2.md`.
  *Rulings made during execution, R-PC-1 … R-PC-11.*
  - R-PC-1 (owner, «Сдвиг +32768», amends PC-3): BITPIX 16 / BZERO 0 with > 1 % of the finite stored values negative is read as BZERO 32768
    with one `warn!` per file naming the path; otherwise signed. The corpus's only such file is `singles/_DSC5767.fit` (99.8 % negative).
    **R-PC-1a** (after rustafits 1.6): the rule lives in the 1.6 reader; a standard signed file has its negatives clamped to 0, Analysis logs
    one `warn!` when the read plane's maximum exceeds 32767. Evidence: C0 census, `task-C5-review-r1.md` (I3, M4).
  - R-PC-2 (amends PC-2): 8-bit FITS is read in its own ADU (full scale 255); a BITPIX 32 FITS takes the float rule; DATAMAX is scaled ×65535
    only when the plane was scaled and 0 < DATAMAX ≤ 1. Evidence: `task-C2-review-r1.md` (I1, I2, M1, M2).
  - R-PC-3 (amends PC-4's detection source): a mosaic's seeds come from the matched filter on the sum of its TWO GREEN sites (G1 + G2), not
    R + G + G + B; offset 0.5 and the ×2 map stay; the SNR cost (6-22 %) sits at the faint end the 500-star budget never reaches. Evidence:
    `task-C3-review-r1.md` Q3.
  - R-PC-4: `RawPlanes::Cfa`'s pattern carries the file's XBAYROFF / YBAYROFF folded in (mod 2). Evidence: `task-C3-review-r1.md` (I3); the
    reader's own fold since rustafits #7 (47c33f73: the FITS fold lives in `processing::cfa`, `analysis/input.rs` folds XISF only).
  - R-PC-5 / R-PC-8: `stars_detected` = the matched filter's point-like seeds (not flagged `extended`), one rule for every layout (3-9× the
    legacy counts, a release note); on a mosaic the extended-flagged seeds are HANDED to the fit flagged, fitted and kept out of the frame
    statistic, the mono path's P3 treatment. Evidence: `task-C5-review-r1.md` Q2, truth check 2.
  - R-PC-6 / R-PC-7 (bar definitions): "injected stars above the threshold" = Fisher SNR ON THE MEASURED PLANE ≥ the threshold (CFA: the green
    `fisher_snr` / √2); stars the injector flags `edge` are outside the completeness set. Evidence: lab `star-lab/results/plan-c/c5-r2/`.
  - R-PC-9 (recorded limit): at native F < 3 px the mosaic's sum plane is under the width floor (1.5 super-px); the structure test drops
    17-20 % of the seeds and takes isolated ≤ 0.90 F stars with them. **R-PC-9a (owner, «Записать как ограничение»): C5's raw-mode completeness
    bar is a RECORDED MISS** - 6 of 10 entries short by 1-3 of 45-60 stars, all in R-PC-9's cells; 0206 / 0013 / 0004 / 0015 pass; the FWHM bar
    passes 10 / 10; no Plan D item. Evidence: lab 8fa3ff8 `star-lab/results/plan-c/c5-r2/` (`raw-bars-misses.txt`), `task-C5L-report-r2.md`.
  - R-PC-10 (owner at the C9 app look, «я бы сделал отказ от blend»): an otherwise-accepted fit is refused `Blend` when another seed of the
    plane's detection lies inside the fit's half-maximum ellipse and at least 0.5 `F_field` from the fit's own seed; applied before the duplicate
    drop in the shared fit path (every mode - **SUPERSEDED by R-PC-10b**: the Analysis path only), declared last in `RefusalReason`;
    `MEASURE_VERSION` 5 → 6 (burnt, now 7). Evidence: owner screenshot (two stars,
    one elongated ellipse); lab `doubles/f4.0` FWHM 4.457 vs truth 4.062; 16c0a2ec / 41ca7bad, `task-C9B-review-r1.md`.
    **R-PC-10a (owner, «Оставляем Blend как есть, близкие неразделённые пары - в Plan D»):** ships as built; it refuses resolved pairs (both
    seeded, ~1.7 F); close pairs whose companion is no seed (95 of 132 on `doubles/f4.0`, FWHM bias +7…+14 % at 0.5-1.25 F) are a recorded
    limit for Plan D (a second-peak test in the fit residual). Evidence: lab 425db82 `star-lab/results/plan-c/c9b/`, `task-C9BL-report-r1.md`.
  - R-PC-11 (controller 2026-10-25, amends C10 Step 1's identity bar): C10 runs on the dev Mac (owner 2026-10-24); R-PC-10 moved the stacking
    path, so "moved 0, max |Δ| 0 vs b6-final" is void. The bars: (a) the full cycle at the plan-c head, `--prev t14-final`, every truth bar of
    `planb_b6.py` PASS, every moved scorecard row in a family whose frames carry ≥ 1 `Blend` refusal (a move without one = STOP); (b) LN at
    master level on hc11 / runB / s204 against the B6 clones, the b3 compare bars, measure-equal REPORTED (Blend refusals per group), never a
    STOP; (c) gates 13 / 13 on the s204 c10 clone; (d) raw-mode bars at v6, no entry below its C5L r2 count except by a `Blend` refusal.
    Evidence: `rulings.md` R-PC-11; the B4 comparator clones were deleted 2026-10-25 (owner), B6's stand in. After R-PC-10b the identity is
    back: (a)'s "moved rows in Blend families" clause is void again (r2 read moved 0), (b)'s comparison expected EQUAL to B6.
  - R-PC-10b (fable C10F verdict B, 2026-10-25; the controller checked the call path; owner informed at wake; supersedes R-PC-10's "every
    mode"): the `Blend` refusal applies on the Analysis path only (`Mode::Budget`: the Analysis tab, Blink, the CFA green-site path through
    `measure_plane`); the stacking Measure (`Mode::All` / `Brightest`), LN's refits and registration (`fit_stars` / `fit_stars_with_beta`) fit
    as at v5, byte-identical to B6. `fit::Correction` carries `blend_refusal` (default false), set only by `measure_budget`.
    `MEASURE_VERSION` 6 → 7: 6 is burnt (every v6 stacking artifact since 16c0a2ec carries Blend numbers under the v6 hash); 7 re-measures
    each cached tree once to v5-identical numbers and recomputes every Analysis row. Why: C10 r1's ln-truth FAILED - Blend refuses ~1.6 % of
    stacking fits (resolved pairs, not a misfire), but `tflux` sums every accepted fit, saturated included, and saturated stars hold 53-64 % of
    a frame's accepted signal, so a few saturated refusals re-rank the weights against seeing (hc11: the sharpest frames demoted, weights ρ
    0.891, master noise +1.59 %); head and B6 equidistant from the photometric truth. Cost, recorded: stacking keeps the blends' 0.1-0.4 %
    FWHM / crowded-field ecc bias until Plan D; the Analysis star list is up to 2.3 % shorter than the stack's. Evidence:
    `task-C10F-verdict-r1.md`, lab 67f13ad (`star-lab/results/plan-c/c10-r1-v6/`), 16c8e2aa, `task-C10B-report-r1.md`.
  *`fieldWidthAtFloor` per site (R-OSP-15's per-site table; code read at the plan-c head).* `FieldWidth::at_floor()` is read by two things only:
  the detection warn (`star_measure/seeds.rs`, "field width at the clamp floor") and the stacking channel flag
  (`stacking/measure.rs`, `field_width_at_floor: m.field_width.at_floor()`); no Analysis consumer reads it (`ChannelMeasurement::at_floor` is a
  different notion: `c` not measured).

  | call site | field width from | warn | flag |
  | ---- | ---- | ---- | ---- |
  | stacking Measure, every detected plane (`seeds::detect` via `detect_on`) | the matched filter's own half-sample mode | one per plane | `channel.fieldWidthAtFloor` |
  | Analysis mono plane (`measure_plane`, `SeedSpec::MatchedFilter`, White) | the same detection path | one per plane | none read |
  | Analysis debayered G plane (Mixed noise) | the same path, no median (silent limit 1, PC-5) | only when the pool itself lands under the floor | none read |
  | Analysis CFA path (`analysis/cfa.rs`, `seeds::detect` on the G1 + G2 sum) | the detection, in SUPER-PIXEL units | yes, in super-pixel units | no: the native fit is handed `2 × fwhm_px`, which cannot read under 1.607, so the handed width carries no flag (C3-review Q2 / M1; mapping the floor to native units would shrink the fit box, a flag field would move the stored `PlanePrep` bytes) |
  | given seeds without a width (`measure_plane` `SeedSpec::Given` / `None` through `seeds::field_width_of`; LN refits via `fit_stars_at`; Peak / Structure seeds) | `field_width_of`, the same rule on the given seeds | no (the warn lives only in the detection path) | carried by a caller that reads it, without a warn |

  *C3-review notes (`task-C3-review-r1.md`), all recorded, none a code change.* **M2** the ×2 width of the CFA path reads wide because of the
  2×2 box blur (a Gaussian estimate √(F² + 8 ln2/3): +6 % at F 4, +10 % at F 3); it is read only by the box radius ⌈2F⌉, the start width of an
  unreadable star and the merged-pair bar 1.3 F, all tolerant - recorded, no fix. **M3** `c` is None on a mosaic by construction (a
  qualifier needs no absent pixel within 5F, which a checkerboard mask never allows), so `photometry.rs` raises its one `warn!` per OSC plane on
  every Analysis frame - expected, not a fault. **M7** the noise from one green phase leaves out the G1 / G2 sky imbalance (the fit's single
  background sees a checkerboard of two green skies) and on the phase the noise mesh spans 256 native px; only `sigma` feeds the fit (saturation,
  the SNR) - raw-corpus limits; `green_sum` reads `BayerPattern::None` as the GRBG phase without the warn `green_mask` gives. (I1 knots → R-PC-8,
  I2 → `native_seed` pinned at seed level, I3 → R-PC-4, all folded by `3d756181`.)
  *C7 (plate-solve gate; lab 28130b2).* 95 frames (trails 7, m101 30, singles 38, the first 20 of m42), one frame per batch: 86 solvable, 9 hopeless;
  4 of the 9 carry `frame_verdict = trailed` and fail in 0.8-1.3 s on the solver's own refusal; STOP check passed (no solvable frame carries the
  verdict). No (ecc, R²) pair refuses a candidate without refusing a solvable frame (`ldn621`, ecc 0.883 / R² 0.865, solves) → REMOVE. Let
  through, 455 s of solve wall (Plan D, PC-14): M101 `…_020058_0002` (89.1 s, ecc 0.767), `…_020531_0004` (86.0 s, 0.798), `…_021623_0009`
  (80.1 s, 0.384), `_VSH8902.xisf` (122.8 s; no Analysis row, no IMAGETYP), H 0029 (77.0 s, tracking jumps, point-like fake stars).
  *Lab lines.* C0 before (lab 73cdce2; athenaeum 78e37a87): 152 files, the BITPIX census, 147 images × 2 arms read (5 XISF the probe could not read
  then); C5L (lab b69a28a / 4ce220f, r2 8fa3ff8): raw-mode bars, r1 MISS, r2 MISS 6/10 by 1-3 stars (R-PC-9a), FWHM bar 10/10; C7D (lab 28130b2): the
  gate data above; C9L (lab c54afc9, athenaeum 0c59fad1): `PLANC CORPUS: PASS` - 152 images × 2 arms, 304 / 304 read, FWHM new / legacy 0.996
  (m101), 1.019 (m42), 1.006 (singles), 0.999 (trails), ecc ratios 1.019-1.099, 27 outliers each explained, legacy drift only `_DSC5767.fit` (the
  XBAYROFF fold); C9BL (lab 425db82): Blend - synthetic judgement STOP (4.356 → 4.353 vs truth 4.062), corpus PASS (ratios 1.000, no verdict
  change, Blend ≤ 3 % of fits), isolated families identical.
  *Cost (C9).* `PLANC COST: MET` - m42 measure median 0.387 s per frame (p90 0.409) against the 0.6 s bar; OSC singles 0.279 s; Blink m42 call
  0.404 s (measure 0.387 + read / persist 0.018, warm cache); after Blend (C9BL) 0.385 s. Dev profile: rustafits at opt-level 2 (d811c054) took the
  dev Analysis from 6.82 to 0.36 s per 3124×2088 frame.
  *The app (C9).* Owner, `npm run tauri dev` on the dev catalog: «в целом все устраивает»; startup rebuilt `star_metrics` (510 501 stars, 2.25 s, no
  orphans); keep BOTH the Verdict and Refused columns; Blink ellipses sized by brightness (0b3a281b); the settings reset button beside its label
  (0872d6e5).
  - C10 runs (dev Mac, R-PC-11; `planc.sh c10 build|cycle|inject-raw|ln-truth|gates|report`, lab tools 9ccf343, every run with
    `PLANB_GUARD_SUBMODULES=all` for someone else's `solvemyastro` fixture commit). Final review C10R (fable, before the runs, at 41ca7bad):
    READY, 0 critical / 0 important / 5 minor; m1-m4 folded in e2ae53ff, m5 is this entry.
    - **r1 at 41ca7bad (`MEASURE_VERSION` 6, Blend on every path): FAIL** (lab 67f13ad, kept as `star-lab/results/plan-c/c10-r1-v6/` at
      804c771). Build PASS; cycle PASS under R-PC-11 (a) (probe 1041 / 1041, inject 109 / 109, 73 437 scorecard rows; noreg vs t14-final
      moved 1012 rows in 14 families, every one with Blend refusals; isolated families unmoved; cumulative vs t01c moved 0; B1 coverage
      100 %, 156 planes); inject-raw PASS; gates 13 / 13; **ln-truth FAIL**: hc11 weights ρ 0.891 < 0.9 with 19 unexplained movers, one L
      master plane median −2.48 % / MAD −4.09 %, GROUPS WORSE 1; runB OSC plane 2 MAD −1.17 %; s204 masters within, GROUPS WORSE 3. Blend
      refusals on the stacking path: hc11 391 in 37 frames, runB 68 361 in 184, s204 72 951 in 197 (C9BL had never measured stacking's
      all-stars mode on a real tree). → C10F verdict B → R-PC-10b.
    - **r2 at 16c8e2aa (`MEASURE_VERSION` 7): every bar PASS** (lab 05e46a8, `star-lab/results/plan-c/c10/`, index `after-table.md`:
      `C10 REPORT: COMPLETE`, all jobs PASS). Build PASS (star_probe 24ae9644375f); cycle PASS (probe 1041 / 1041, inject 109 / 109, 73 437
      scorecard rows, noreg vs t14-final **moved 0**, cumulative vs t01c moved 0, Plan B's B4 bars as at B6, B1 coverage 100 %, 156 planes, 0
      below; Blend refusals only in the analysis-mode rows, 100 in 6 of 183 families); inject-raw PASS (10 entries, none below C5L r2, refused
      Blend 0, FWHM bar 10 / 10; the completeness bar is R-PC-9a's recorded miss, unchanged); ln-truth PASS, **EQUAL to B6** on hc11 / runB /
      s204 (measure-equal 0 of 106 / 184 / 197 differ, Blend 0, weights ρ 1.0000, movers 0, masters PASS, GROUPS WORSE 0); gates 13 / 13 on
      the s204 c10 clone.
    - Suites (controller, dev Mac, single runs): core all targets `--no-fail-fast` **4201 passed / 0 failed / 17 ignored** at 16c8e2aa
      (4197 / 0 / 17 at 41ca7bad; the dev Mac runs the network modules too, no split); at 41ca7bad: `cargo test -p athenaeum-web` 53 / 0,
      `cargo check -p athenaeum-core --no-default-features` Finished, vitest 88 files / 930 passed. Lab (C10T): star-lab 1556 OK, harness
      468 OK (2 skipped).
    - After note: `docs/superpowers/research/2026-10-25-star-measurement-after-plan-c.md`.
  - **R-OSP-17 CLOSED (2026-10-25): Plan C complete; merge into local main; push on the owner's word.**

## Detail moved from CLAUDE.md (2026-10-20) - Stacking rules

### Stacking

In-app light stacking — the frame set's own master light(s) from its matched calibration and a chosen reference, no external stacker. Spec `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`. **`docs/stacking/README.md` is the reference for everything here — the M1–M4d milestones, perf tiers A and C, every ruling (R-M…, R-T…, C-…), every acceptance run and every measured-out idea. Read it before touching `stacking/`.**

- **Stages** (`stacking::plan::Stage`): Masters → Calibrate (the calibrated-lights engine verbatim) → Measure → Reference (one frame per run, or per group in `registration.geometry = "native"`; two-pass re-pick) → Register (quad-seeded RANSAC + polynomial or TPS distortion, WCS-seeded across pixel scales) → Normalize (local normalization, `.athln` sidecars) → Integrate (banded, weighted; the Auto rejection ladder is unchanged since M1, the other algorithms are user choices) → Drizzle (incl. Bayer) → Output (FITS or XISF master + WCS/SIP, `master_lights` rows, previews). Calibrate/Measure/Register/Normalize are the cacheable per-frame stages (`stacking_artifacts`, per-stage config hashes, spec §9.3); **the registered frame is a required artifact** (owner rule: disk as storage so nothing is computed twice).
- **The plan gate** (`stacking::plan::build_plan`, DB + cheap FS probes, no pixel I/O) returns groups (camera-agnostic: colour mode/filter/binning/exposure), the resolved config + hash, the reference, folder/space state and ordered blockers (`masters | links | masterFiles | reference | folders | space | frames | unsupported`); it reuses the calibrated-export readiness gate so the two features can never disagree. A pixel-scale spread is a WARNING, never a blocker.
- **The run** (`stacking::run`): one `stacking-run-<id>` thread admitted through the shared `ComputeQueue` (the sidebar's `ComputeQueueIndicator` lists it — no separate widget), cooperative cancel via `ServiceContext::active_stacks`, `heal_interrupted_runs` from every API entry point, `stacking-progress` throttled 300 ms (`FanOutTicker` inside the fan-out stages), exactly one `stacking-complete` from the single exit path.
- **Config precedence is WHOLE-CONFIG** (`resolve_config`): a stored per-set document IS the run's config, no field-level merge; new fields are `#[serde(default)]` without a version bump. Presets (Default / Fast preview / Maximum quality) are a single Rust source of truth; user presets are one `stacking.presets` settings row, entry-wise decoded, written under `BEGIN IMMEDIATE`.
- **26 commands** in `api/stacking.rs`, mirrored on both hosts: plan/start/cancel, runs, config/defaults/presets, paths/work-usage/cleanup, `get_master_light_preview` (`maxPx` clamped to `[64, 2048]`), `list/save/delete_stacking_preset`, and the seven project twins (wave 6b-3). A project run stacks onto the coordinator's canonical grid: one composed map per frame, LN on the overlap, a coverage map and an optional crop (`docs/stacking/README.md`, The project path on the grid).
- **Tab** (`src/components/stacking/`): `StackingTab` → `PipelineBoard`/`StageRow` (ten rows: nine stages + the display-only Debayer) + `GroupsTable`, `StageInspector` + one panel per stage, `FramesTable` (manual exclusion is the ONE frame-level write), `ResultsPanel`/`ProvenanceModal`. Settings → Stacking: global defaults + working/output folders (`<working_dir>/<set_slug>/{calibrated,measured,registered,ln,runs,rej,previews}/`) + a Compute section (`compute.memory_budget_mb`, the frame memory budget; 0 = auto = a quarter of the effective memory, clamped above it; R-OSP-13).
- **Method rules**: a numeric change is judged on the WHOLE group at master level (a full checkpoint), never on a few-frame probe; a speedup that misses a spec §8 tolerance row is reverted with its numbers; only interleaved before/after brackets are comparable on this Mac; `cargo check --no-default-features` does not compile `integration/`, and `cargo test --lib` hides example breakage (`--all-targets`). Per-core efficiency (SIMD / branchless, bit-identical, scalar oracle kept) comes before threads or GPU, and work units are compared with the ERP, not stage walls (R-OSP-14). Measured-out ideas are recorded in the reference and in doc comments — do not re-propose them. Acceptance harness and the synthetic star lab: the private `athenaeum-lab` repository (`harness/acceptance/`, `star-lab/`; reuse, never rebuild; `ATHENAEUM_REPO` names this checkout). Lab and measurement tools are committed there, never here; the Rust probes stay in `examples/`.

Key files: `crates/athenaeum-core/src/stacking/{config,groups,paths,plan,run,provenance,measure,weights,psf_signal,structure,integrate,master_cards,rej,fits_artifact}.rs`, `stacking/register/`, `stacking/ln/`, `stacking/drizzle/`, `integration/` (engine, combiners, `student_t.rs`), `geometry/tps.rs`, `fits_writer/{wcs,xisf_writer}.rs`, `api/stacking.rs`, `db/stacking.rs`; probes `examples/{measure,register,integrate,ln,drizzle}_probe.rs`, `examples/weight_audit.rs`. Frontend: `src/hooks/useStackingRuns.ts`, `src/contexts/StackingContext.tsx`.

