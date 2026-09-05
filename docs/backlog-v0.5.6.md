# Backlog — v0.5.6

Opened 2026-09-06, the day v0.5.5 was tagged. Items 2–4 are carried over from
`docs/backlog-v0.5.5.md` by owner decision — they were deferred, not dropped.
Item 1 came out of the v0.5.5 release itself.

Each entry states what is already *known* — the code that owns the behaviour,
what was measured — and the question that has to be answered before it can be
planned.

## 1. The plate-solve gate thresholds need controls

The v0.5.5 release notes promised "the thresholds are in Settings if you want
them looser". They were not, and the sentence has been retracted from the notes,
the blog post and the download-page row. Owner decision the same day: expose
them for real.

**Why it matters.** The input gate refuses a frame whose own analysis shows
`median_eccentricity >= input_max_eccentricity` AND
`trail_r_squared >= input_min_trail_r2`. On a rig that trails a little — an
unguided mount, a windy site — the defaults may refuse frames the solver would
have handled. Today the only way to loosen them is to edit JSON in the settings
table by hand.

**Where everything already is:**

- `crates/athenaeum-core/src/plate_solve/config.rs` — `input_gate_enabled`
  (true), `input_max_eccentricity` (0.85), `input_min_trail_r2` (0.65), stored
  with the rest of `PlateSolveConfig` as one JSON blob under the settings key
  `plate_solve.config`.
- `get_plate_solve_config` / `set_plate_solve_config` /
  `reset_plate_solve_config` exist and are registered on **both** backends, and
  the panel writes the whole struct back on save.
- `src/components/plate-solve/PlateSolveSettingsPanel.tsx` renders exactly three
  fields — `base_verification_tolerance_arcsec`, `sip_order`,
  `autofind_tolerance_deg`. Its `DEFAULT_CONFIG` is a hand-written mirror of the
  whole struct (there is a comment saying so), which is why every other field
  round-trips through the panel untouched instead of being lost.

**So this is a frontend-only task**: no Rust change, no new command, no
migration. Three controls and their copy.

**Also unexposed, and part of the same decision:** the whole acceptance-gate
group — `blind_gate_enabled`, `blind_rms_max_px_mult`, `blind_min_inlier_ratio`,
`blind_inlier_floor`, `blind_scale_sanity_min` / `_max`,
`blind_scale_header_tol` — plus `batch_concurrency`.

**Open:** expose only the input gate, or the acceptance gate too? Proposed:
**only the input gate**, plus `batch_concurrency` if it is wanted. The
acceptance gate is what stopped v0.5.5's false 16–193× solutions from reaching
the catalog; a number a user can loosen without understanding it is a number
that will be loosened. If the acceptance gate ever does surface, it belongs
behind an "Advanced" disclosure with the failure it prevents named in the copy.

**Read this together with item 4.** The input gate reads the FULL analysis
path's eccentricity, which under-reports on exactly the frames the gate exists
for. Loosening the threshold is honest; tightening it will not catch what item 4
describes, and the copy should not imply that it will.

**Acceptance:** the toggle and both numbers persist across a restart and
round-trip through the panel; turning the gate off makes a frame it previously
refused attempt a solve again; the three fields already on the tab still save.

## 2. VNG debayer in the blink preview at full resolution

Carried over from `docs/backlog-v0.5.5.md` item 4 — **the research there is
done, start from it, do not re-measure.** Summary: the render path uses
`super_pixel_debayer_f32`, which halves both axes, so Blink's "Full Resolution"
gives an OSC frame half its native size with no warning anywhere. VNG is wired
only into the export generator. Measured on a real 26 MP OSC frame: super-pixel
3.6 ms for 6 MP out, VNG 683 ms for 26 MP out (~191× the wall clock for 4× the
pixels, ~313 MB for one planar f32 RGB frame). The stretch is size-agnostic and
not a blocker.

**Open, unchanged:** always-on for `full`, or a separate "1:1 / high quality"
mode? The preview cache is keyed per resolution, so a new mode needs a cache-key
decision too.

## 3. Navigation memory ("backspace browsing")

Carried over from `docs/backlog-v0.5.5.md` item 1, minus the frame-card fields,
which shipped in v0.5.5. Every main screen should remember its last browsing
state; explicitly **not** pop-up modals and **not** blink. The calendar screen
resets entirely today.

**Open, unchanged:** is "back" the browser/history gesture, an in-app
affordance, or both? Does the state survive a full restart or only navigation
within a session? Scope is every page under `src/pages/`, so the answer decides
whether this is one shared hook or per-page work — the widest regression surface
of anything on this list.

## 4. The full analysis path under-reports eccentricity on trailed frames

Found while building the v0.5.5 plate-solve gates and deliberately left open
there, because the fix changes every stored metric.

The full detector measures shape over a stamp sized at `1.5 × field FWHM`. A
streak's bright head has a small FWHM, so the stamp is narrower than the object
and reports it rounder than it is — self-reinforcing. Measured on the owner's
real frames: **0.56 where the fast path, whose stamp follows the star's own size
(2 × HFD), sees 0.88.**

Consequences today: the Analysis table rates badly trailed frames well, and the
input gate of item 1 — which reads this number — misses them. Plate solving is
not fooled: it measures shape again per detection at its own scale, so such a
frame is refused rather than mis-solved.

**Open:** re-analysis story. Changing the stamp changes `median_eccentricity`
for every frame already in the catalog, so either every library needs a
re-analysis pass or the two measurements have to coexist for a while. That
decision, not the maths, is what makes this its own cycle.
