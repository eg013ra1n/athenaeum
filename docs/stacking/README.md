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
verbatim) → `Measure` → `Reference` (one frame for the whole set — highest
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
frames | unsupported`) — reusing the calibrated-export readiness gate
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

**19 commands** (`api/stacking.rs` + `commands/stacking.rs` +
`routes/stacking.rs`, all mirrored on both hosts): `get_stacking_plan`,
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
`<working_dir>/<set_slug>/{calibrated,registered,ln,runs,rej,previews}/…` —
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
  Plan C (the Analysis switch) is 5. `AnalysisConfig::config_hash` is Plan C's.
- **Risk.** The first run after Plan A re-measures, re-registers (mono) and re-normalizes every set; the `calib`
  column of the stage-hash golden literals is unchanged, every other column moved
  (`stage_hashes_are_pinned_to_golden_literals`).
- **Logging.** The moved measure-path warnings now log under the tracing target `star_measure` rather than
  `stacking`; their `clip` and `samples` fields joined the logging dictionary
  (`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`).
- **F-0 - every seed has an outcome (Task 4; numbers unchanged).** `star_measure::diagnostics` holds
  `RefusalReason` (eleven camelCase names, `outsideImage` … `duplicate`), `Outcome` (`Accepted { star }`,
  `Refused(reason)`, and the Budget mode's `NotSelected` / `Trimmed` from Task 12), the per-seed `FitDiagnostics`
  and `RefusalCounts`. The identity `accepted + refused + not_selected + trimmed = seeds` (ruling F13) is a
  `debug_assert!` in the fit and in `measure_plane`, a test, and the lab's `fit outcomes` row.
  - **Order (M-4).** `fit_one` returns `Result<StarFit, RefusalReason>` plus its trace. The outcomes come off the
    same `seeds.par_iter().map_init(FitScratch::default, …)` chain, collected per seed in seed order. The accepted
    fits are read off it sequentially and handed to `dedupe`, so its input, its stable sort and the Kahan sums see
    exactly the order they saw before. A fit the ±1 px drop removes is refused `duplicate`.
  - **Today's reasons.** The nominal stamp leaves the image → `outsideImage`; fewer than 10 finite region pixels →
    `tooFewPixels`; the fitter returns `None` → `fitFailed`. The first acceptance gate is still one condition; its
    reason is `nonFinite` (a parameter or the residual), else `residualTooLarge`, else `fitFailed` (`A ≤ 0` or an
    `α ≤ 0`). The centroid tolerance → `centreMoved`; the inner region → `centreAtRim`; half the FWTM beyond the
    region radius → `regionTooSmall`. `donut` and `narrowerThanPixel` are declared now and produced from Task 6b.
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

      Each group PASSES when its head pooled median (of |value| for a bias key) is no higher than the base's,
      and the count of (row, key) judgments worse than the base beyond SE exceeds the count better beyond SE by
      no more than 2·√(worse + better). The target is every group PASS by the end of Plan A, checked at Tasks
      6b, 9 and 14. The starting debt at t05 (`t05-f2/debt.txt`):

      | group | worse / better | pooled median t01c → t05 | verdict |
      | ---- | ---- | ---- | ---- |
      | fwhm bias (measure, pointfit) | 96 / 69 | 0.01045 → 0.00969 | OPEN (sign test) |
      | fwhm scatter (measure, pointfit) | 68 / 98 | 0.01552 → 0.01553 | OPEN (median) |
      | background bias (measure) | 141 / 36 | 0.0502 → 0.0563 | OPEN (median and sign test) |
      | centroid bias (measure) | 33 / 34 | 0.00175 → 0.00170 | PASS |
      | centroid scatter (measure) | 42 / 48 | 0.0237 → 0.0239 | OPEN (median) |
      | centroid bias (register) | 22 / 20 | 0.00128 → 0.00136 | OPEN (median) |
      | centroid scatter (register) | 40 / 33 | 0.0181 → 0.0179 | PASS |
