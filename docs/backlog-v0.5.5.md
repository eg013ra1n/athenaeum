# Backlog — v0.5.5

Owner findings, captured 2026-09-05 and staged the same day (see **Stages** at
the end). Each entry keeps the owner's wording as the requirement and adds only
what is already *known* — measured numbers, the code that owns the behaviour, and
the question that has to be answered before the item can be planned. Items 4 and
1 are **out of v0.5.5** — they go to their own cycle after the tag.

## 1. Navigation memory ("backspace browsing")

Every main screen should remember its last browsing state so the user can go back.
Explicitly **not** pop-up modals, and **not** blink.

- The calendar screen currently resets its state entirely.
- Frame cards should carry more: camera, telescope, timings — and suggest more.

**Open:** is "back" the browser/history back gesture, an in-app affordance, or
both? Does state survive a full app restart or only navigation within a session?
Scope is every page under `src/pages/`, so the answer decides whether this is one
shared hook or per-page work.

## 2. Analysis tab

- Sorting on **all** columns (today only some sort).
- ~~Rename the `Calib` column to **Calibrated** and show the status as an icon.~~
  Dropped (owner, 2026-09-05): the column belonged to the retired standalone
  Calibrate Lights flow and is already gone on `main` — v2 has no per-frame
  "calibrated" state to show. Not coming back.
- `WCS` column shows **Header** or **ATH**, without an icon.
- `Reference` column shows only the star, without the set.

Smallest, best-specified item in this list; a good first slice.

## 3. Night detection splits a night that should be one

LDN 1272, the night of 18–19, is split into two nights although there is no large
gap between frames in the session. The app should understand night logic rather
than gap-only clustering.

**Owner rule this touches:** frame sets are clustered by sky coordinates, but
nights come from `imaging_nights` / `sessions`.

**Spike done 2026-09-05 (dev catalog, set 109 "LDN 1272", 4 night rows).** The
night of 13→14 Sep is stored as TWO rows — `486` 21:55–23:59 (33 frames) and
`489` 22:36–01:59 (73 frames): overlapping ranges, and no gap over 30 min
anywhere between 21:55 and 01:59 against the 6 h threshold. One night, two rows.
(17→18 and 18→19 Oct are two real nights in the catalog, 15.5 h apart; if the
UI doubles 18–19 too, the mechanism is the same — confirm at acceptance.)

Cause: a merge does not re-derive nights, it stitches the rows. The set was
assembled from merged frame sets (a post-flip pointing shift clustered the
second half of the night separately). `frames_set_merge::nights_match` = same
UTC calendar date AND range overlap; the post-flip cluster's night did not
overlap the target's at merge time, so it came in as a separate row, and later
range unions (`calculate_time_range_union`) made them overlap after the fact —
nothing re-checks. The same rule runs in the manual merge (duplicated in
`athenaeum-tauri/commands/frame_sets.rs::merge_frame_sets` and the web mirror —
the logic sits in the shell crates, not in core) and in `auto_merge`.

Decision (owner + assistant, 2026-09-05): both.
- Automatic: every merge (manual and "Find new images") re-derives the set's
  nights/sessions from the UNION of member frames via the existing
  `sessions::detect_sessions`. A night is derived data with one definition (the
  gap rule); derived data is recomputed, never stitched. `imaging_nights` and
  `sessions` are referenced only by `session_members` (+ the session-stat
  triggers), so delete + re-insert per set is safe.
- Manual: a "Recalculate nights" action on a frame set — the same core
  function behind one button and one command in both backends — because LDN 1272
  is already wrong in the catalog and nothing else repairs it.
- The merge night logic moves into `athenaeum-core` on the way.

Acceptance: set 109 shows three nights after Recalculate; merging the two halves
of a night produces one row without the button.

**Second cause, found by the owner's smoke 2026-09-05 — and the one the report
was actually about.** After the re-derivation the UI still showed five nights
for LDN 1272 (Oct 19 / Oct 18 / Oct 17 / Sep 14 / Sep 13). The tree does not
read `imaging_nights` at all: `get_calibration_hierarchy_for_frame_set` joined
the nights only to filter by set and grouped by `DATE(f.date_obs)` — the
frame's UTC calendar date — so every night that runs through midnight was two
groups. 54 + 52 = 106, 29 + 36 = 65, 142 + 55 = 197: the three real nights,
split across five dates. Fixed by grouping on `imaging_nights.id`, keying the
group on the night's start (UTC RFC3339, so the frontend's lexicographic sort
stays chronological) and labelling the span (`October 18–19, 2025`). Verified
on the real catalog: set 109 → 3 groups, 368 frames.

**Calendar, same rule (owner request, 2026-09-05).** `get_calendar_month_data`
grouped both of its queries by `DATE(fr.date_obs)`, so one night occupied two
day cells. A day now keys on the night that STARTED there:
`DATE(imaging_nights.start_time, '-12 hours')` for organized frames, and the
same noon-to-noon rule over `date_obs` for loose frames (they have no stored
night, and the shift gives the answer the gap rule would have). Verified on
the real catalog: LDN 1272's 106 frames sit in the 13 Sep cell (was 54 + 52),
65 in 17 Oct, 197 in 18 Oct; the 14 Sep and 19 Oct cells are gone.

## 4. VNG debayer in the blink preview at full resolution

**Already researched in the 2026-09-05 session — start from here, do not
re-measure.**

Today the render path debayers with `super_pixel_debayer_f32`
(`rustafits/src/pipeline.rs`), which halves both axes. Consequences:

- Blink's **"Full Resolution"** setting (Settings → Image Resolution, described as
  "Full shows maximum detail") gives a mono frame its native size but an OSC frame
  **half** of it: a 6248×4176 sensor renders 3124×2088. There is no warning
  anywhere; the promise and the behaviour disagree.
- VNG is wired only into `export/calibrated_generator.rs`. Nothing in the viewer
  path calls it.
- The stretch is **not** a blocker: `apply_stretch_and_finalize` runs after the
  debayer on planar RGB and is size-agnostic, so VNG output stretches identically.

Measured on a real 26 MP OSC frame (LDN 1272, ZWO ASI2600MC Duo):

| | time | output | per output pixel |
| ---- | ---- | ---- | ---- |
| super-pixel 2×2 | 3.6 ms | 3124×2088×3 (6 MP) | 0.55 ns |
| VNG | 683 ms | 6248×4176×3 (26 MP) | 26.2 ns |

So VNG is ~191× the wall clock for 4× the pixels, plus ~313 MB for the planar
f32 RGB of one frame. Fine for an explicit single-frame 1:1 view; expensive for
blink, which flips frames; out of the question for thumbnails.

**Open:** always-on for `full`, or a separate "1:1 / high quality" mode? The
preview cache is already keyed per resolution, so a new mode needs a cache key
decision too.

Note star metrics are unaffected either way — analysis uses
`interpolate_green_f32` at native resolution, never the super-pixel path.

## 5. Plate-solving gates

Do not plate-solve a frame with no stars, or one showing strong trailing —
especially do not let such a frame fall through to a blind solve.

**Related prior work:** the defocus persist gate (floor 12→6, shipped) and the
"slow solve = wrong scale hint" chain (focal-length-as-aperture → blind
fallback). This item adds an *input-quality* gate in front of those, which is a
different axis from the existing stage gates.

## 6. Manual calibration assignment

- The filter in the modal should always be visible (today it is not).
- Add dropdowns for the available cameras and exposure times.
- Clicking a date in the left info tab should fill the date filter; same for
  camera.
- **Sub-calibration assignment and calibration-to-lights assignment are two
  different interfaces and should be one.**
- Scoring must not read 0 when the camera matches and the dates are close: with
  everything shown, the list should still sort usefully by score so the right
  frames surface instead of being buried.

The scoring point is the substantive one — it changes what the matcher reports,
not just the UI. `calibration/configurable_matcher.rs` owns the score.

## 7. Export tab — default mode and the folder tree

- **Calibrated lights** is preselected even when it is unavailable. That must
  not happen.
- The export tree must show exactly how the export will be performed: with no
  calibration frames, or with masters, the tree has to be drawn correctly by
  count.

**Known:** `ExportTab.tsx::readExportModePref` restores the LAST chosen mode
from localStorage on every set without consulting readiness, so a mode chosen
once where it was ready stays preselected everywhere, blocked or not. The tree
comes from `get_export_summary` → `collect_export_summary`, which builds
`build_folder_preview` BEFORE `apply_export_mode` — the mode is never passed —
and counts the raw frames of every linked set. So the tree is truthful only for
`rawWithCalibrationSets`: masters should be one file per set, lights-only has no
calibration folders, calibrated lights has `c_*` names under `lights/` and no
calibration folders either.

Fix shape: a persisted choice never beats readiness (blocked → the documented
default `rawWithCalibrationSets`, then list order; the preference itself is not
rewritten); with NOTHING linked the two raw modes are blocked too (they would
land exactly what Lights only lands — the masters rule was vacuously true with
no sets at all; found on C/2025 A6, 104 lights, 0 links); calibration warnings
describe the links, not the mode, and lights-only carries none; a missing-
calibration warning names the camera (two groups can share a filter); `get_export_summary(set_id, mode)` in both backends,
mode applied before tree/totals/size, and the tab re-fetches the summary when
the mode changes.

---

## Stages

Decided 2026-09-05. Items are independent subsystems — the order is by risk and
by how many unknowns have to be removed before code. v0.5.5 = the work already
on `main` since v0.5.4 (transfer preparation, calibrated-export v2, rustafits
1.1.0 — release-note lines in `docs/superpowers/open-items.md`) + stages 1–4.
The tag goes after stage 4.

| # | Stage | Path |
| ---- | ---- | ---- |
| 1 | **Quick fixes**: Analysis tab (item 2) · Export default + tree (item 7) · frame cards carry camera / telescope / timings (the tail of item 1) | bounded, one commit per slice |
| 2 | **Nights** (item 3): re-derive on merge + Recalculate nights — DONE 2026-09-05, accepted on a copy of the real catalog (set 109: 4 rows → 3 nights, 106/65/197 frames) | bounded |
| 3 | **Manual calibration assignment** (item 6): scoring first, then the unified modal | architectural → spec → plan |
| 4 | **Plate-solve input gate** (item 5) | bounded–medium |

Inside stage 3 the scoring change goes first — it is core, testable against
this catalog, and independent of the UI; polishing two modals that are about
to become one is thrown-away work.

**Deferred to their own cycle after the v0.5.5 tag** (owner call, 2026-09-05):

- **VNG in the blink preview** (item 4) — needs the mode decision (always-on for
  `full` vs a separate "1:1" mode) and a preview-cache key decision first.
- **Navigation memory** (item 1, minus the frame-card fields) — touches every
  page under `src/pages/`, the widest regression surface in the list; needs the
  back-gesture and persistence decisions first.

## Superseded: suggested order from intake

2 (self-contained UI) → 6 (unify + scoring; largest user-visible win) → 3
(needs a real-data reproduction) → 5 → 4 (needs a mode decision) → 1 (broadest,
touches every page).
