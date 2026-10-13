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
    - **Fallbacks.** Fewer than 5 ratios: `c = 1`, `aperture_correction = None`, ONE `warn!` per plane. No fit on the
      plane (defocused verdict, starless): `None`, `debug!` only. A CFA-masked plane (Plan C, no production caller today)
      gets `c = 1` by these rules: **a Plan C carry**.
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
      B's before. No switch to K7.
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
      6.04: the local sky mismatch under the disc, the annulus method's own limit).
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
      **Plan B / the later nebula-photometry owner:** R11-3's structure term (the nebula flux error 3.6–6× σ_S).
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
