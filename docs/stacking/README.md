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
`docs/superpowers/research/scripts/weight_audit_compare.py`): mono
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
(`docs/superpowers/research/scripts/acceptance/`, reuse it — owner
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
(`psf_signal::group_beta`, `PsfModel::Fixed(β)`, `PSF_FIT_VERSION = 3`;
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

**Key files**: `crates/athenaeum-core/src/stacking/{config,groups,paths,
plan,run,provenance,measure,weights,psf_signal,prefilter,robust,structure,
integrate,master_cards}.rs`,
`stacking/register/{mod,detect,align,frame,wcs_seed,local_loop,writer}.rs`,
`stacking/ln/{mod,grid,background,scale,reference}.rs`,
`stacking/drizzle/{mod,geom}.rs`, `stacking/rej.rs`,
`crates/athenaeum-core/src/integration/student_t.rs`,
`crates/athenaeum-core/src/geometry/tps.rs`,
`crates/athenaeum-core/src/api/stacking.rs` (also `list_stacking_presets`/
`save_stacking_preset`/`delete_stacking_preset`, M4d Task 4),
`crates/athenaeum-core/src/api/files.rs` (`render_preview_from_path`/
`preview_step`, M4d Task 3), `crates/athenaeum-core/src/db/stacking.rs`
(`master_lights` rows, M4d Task 3), `crates/athenaeum-core/src/
fits_writer/{wcs,xisf_writer}.rs`; dev probes
`examples/{measure,register,integrate,ln}_probe.rs` and the weight-audit
harness `examples/weight_audit.rs` (+ `docs/superpowers/research/scripts/
weight_audit_compare.py`).
Frontend: `src/components/stacking/` (above),
`src/hooks/useStackingRuns.ts`, `src/contexts/StackingContext.tsx`.

