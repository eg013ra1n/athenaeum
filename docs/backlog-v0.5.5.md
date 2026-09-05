# Backlog — v0.5.5

Owner findings, captured 2026-09-05. Raw intake: none of this is planned yet, and
several items are design questions rather than bugs. Each entry keeps the owner's
wording as the requirement and adds only what is already *known* — measured
numbers, the code that owns the behaviour, and the question that has to be
answered before the item can be planned.

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
- Rename the `Calib` column to **Calibrated** and show the status as an icon.
- `WCS` column shows **Header** or **ATH**, without an icon.
- `Reference` column shows only the star, without the set.

Smallest, best-specified item in this list; a good first slice.

## 3. Night detection splits a night that should be one

LDN 1272, the night of 18–19, is split into two nights although there is no large
gap between frames in the session. The app should understand night logic rather
than gap-only clustering.

**Owner rule this touches:** frame sets are clustered by sky coordinates, but
nights come from `imaging_nights` / `sessions`. Needs a look at what actually
defines a night boundary today before proposing anything — and a real-data
reproduction on the LDN 1272 set, which is on this machine.

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

---

## Suggested order

2 (self-contained UI) → 6 (unify + scoring; largest user-visible win) → 3
(needs a real-data reproduction) → 5 → 4 (needs a mode decision) → 1 (broadest,
touches every page).
