# Plate-solve input and acceptance gates

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


Three defences, added 2026-09-05 after wind-shaken frames were found being
"solved" at 16-193x their true pixel scale and written into the catalog
(measured on the owner's real files; spec-less, the reasoning lives in the
commits and in this file).

- **Shape reaches the fast path** (`rustafits`): `detect_fast` used to build
  every `FastStar` with `eccentricity: 0.0` — shape was computed only by the
  full analysis. It is now measured for every detection, over a stamp that
  follows the star's own size (2 x HFD): a window narrower than the object
  reports it round, which is what happens on frames whose stars are 13 px
  across. **`sx`/`sy` are NOT a substitute** — the PSF fit declines almost
  everything on exactly those frames, leaving them zero.
- **Streaks are not quad material** (`solvemyastro::select`): detections with
  eccentricity > `MAX_ECCENTRICITY` (0.8) are dropped before SNR ranking, in
  all three selectors (their equal-length-and-order contract). Healthy frames
  and trailed-but-solvable ones carry 8-10 % above that line, hopeless ones
  98-99 %.
- **And a frame the cut emptied is refused at once** (`orchestrate`):
  `looks_trailed` — 90 % or more of at least 100 detections removed — bails
  before the FOV ladder. Without it such a frame still cleared the four-star
  minimum (14 survivors of 600 on a real one) and spent minutes walking the
  ladder twice, counting the density-balanced retry, to reach the same
  refusal.
- **Two gates in the app** (`athenaeum-core/src/plate_solve/service.rs`): the
  input gate (rule below: 2026-09-05 pair, replaced 2026-10-24 by the verdict);
  the acceptance gate finally receives the header's pixel scale, which
  `blind_gate_ok` has always compared against via `blind_scale_header_tol`
  but was given `None`. **The input gate has a Settings section** (Plate
  Solving → Input Gate, `plateSolving.inputGate` in `src/settings/registry.ts`,
  rendered by `PlateSolveSettingsPanel.tsx`): the on/off switch
  `input_gate_enabled` ("Refuse trailed frames before solving"; off, the gate
  returns no refusal). (Plan C C7 removed the two number fields from the
  config; their controls leave the panel in C8.) **The
  acceptance gate has none**: its `blind_*` fields live only in the stored
  `plate_solve.config` JSON and round-trip without controls. (Before the input
  gate's section existed, the v0.5.5 release notes claimed both gates were
  configurable; that was corrected in the notes and on the docs site.)

## The input gate's rule

**Now (Plan C C7, 2026-10-24): the `trailed` verdict alone.** `input_gate_reason`
reads the frame's stored analysis; a row written by the current measurement
(`measure_version == MEASURE_VERSION`; a legacy row or another version is not
trusted and never refuses) whose `frame_verdict` is `trailed` is refused
("Stars are trails the measurement could not fit"). Every other row passes:
`defocused`, `no_stars`, a measured frame of any eccentricity or trail R². A
frame with no analysis is never refused (the answer is unknown; the solver's
own refusal is the backstop). `input_max_eccentricity` and `input_min_trail_r2`
are gone from `PlateSolveConfig`; a stored config that carries them decodes and
ignores them (`a_stored_config_with_the_removed_gate_fields_decodes`).

**History: the 2026-09-05 calibration (kept).** The gate refused a frame whose
`median_eccentricity >= input_max_eccentricity` AND `trail_r_squared >=
input_min_trail_r2` (0.85 / 0.65, both required: either alone refused frames
that solve fine). Measured on real files, frames that solve correctly sat at
eccentricity 0.62-0.72 with trail R² 0.30-0.57 and hopeless ones at 0.90-0.96
with 0.73-0.93. That calibration used the old analysis path, which
under-reported eccentricity on trailed frames (0.56 where the fast path saw
0.88); the one star path (Plan C) replaced that path and the numbers with it.

**The 2026-10-24 measurement (why REMOVE, PC-12).** 95 frames (trails 7, m101
30, singles 38, the first 20 of m42), the input gate off, one frame per batch;
`analyze_frame_set` (force) on every set wrote 87 Analysis rows at
`measure_version` 5. 86 frames solve and 9 are hopeless (all 9 fail to solve;
none solves off-scale). 4 of the 9 carry `frame_verdict = trailed` (trails
2024-07-07 0022 and 0032, 2025-06-29 L 0028 and 0032): they fail in 0.8-1.3 s
on the solver's own "stars are trailed" refusal, so the verdict gate saves
4.2 s in all. No solvable frame carries the verdict (the STOP check passed;
the two trails frames that solve, O 0004 and G 0009, have none). Over the
hopeless frames without the verdict, no pair (ecc, R²) under the AND rule
refuses one of them and no solvable frame: refusing M101 0002 / 0004 (ecc
0.767 / 0.798, R² 0.601 / 0.574) needs ecc below those values, and the solvable
`ldn621.fits` (ecc 0.883, R² 0.865) sits above M101 0002 / 0004 / 0009 and
H 0029 on both axes, so every such pair refuses it; M101 0009 and H 0029 sit at
or below 44 and 7 solvable frames on both axes. The pair adds nothing beyond
the verdict, so it went. (One solvable frame, `ldn621`, decides the corner: the
ruling is "no pair on this data", not a proof. The verdict frames carry ecc
0 / R² 0 in the Analysis row, so a pair could never have seen them.)

**Hopeless frames the gate still lets through (PC-14).** Total solve wall
455.0 s on a shared, loaded dev Mac (relative sizes are safe, exact seconds
are not):

| frame | outcome | solve wall s | ecc / R² |
| ---- | ---- | ---- | ---- |
| M101 `..._020058_0002.fit` | no catalog quads matched | 89.1 | 0.767 / 0.601 |
| M101 `..._020531_0004.fit` | no catalog quads matched | 86.0 | 0.798 / 0.574 |
| M101 `..._021623_0009.fit` | 1 of 6 needed matches | 80.1 | 0.384 / 0.099 |
| `_VSH8902.xisf` (no Analysis row) | matched, no fit | 122.8 | none |
| H 0029 (2025-09-14_04-14-30) | matched, no fit | 77.0 | 0.678 / 0.000 |

Each walks the whole FOV ladder (14-18 rungs). The three M101 frames are one
night (2023-06-18, ASI6200 at 1960 mm, about 1300-1500 detections,
`stars_refused` 317-524) and look point-like by the numbers; H 0029 is the
known tracking-jump class (R² 0.000, `stars_refused` 330). These go to Plan D.
Eight singles files carry no IMAGETYP (`_VSH8902.xisf`, `_DSC2522.xisf`,
`_DSC5767.fit`, the `*_P.fit`, two `Light_Pane*.xisf`): `auto_generate_frame_sets`
leaves them out, so they never get an Analysis row and the gate can never see
them.

**Known gap, deliberately not fixed here**: the gate decides from what the
measurement can say about the stars, and the failures that remain are not
visible in it: point-like frames whose stars are fine by the numbers but whose
field does not match (the M101 night), a tracking jump that leaves a normal
eccentricity (H 0029) and a frame with no Analysis row. They cost minutes per
frame because the ladder is walked to its end (455 s over the five above).
The measurement offers no column that refuses them without also refusing a
solvable frame (above), so they are Plan D's.

**Object-name fallback** (`plate_solve::hints::apply_object_name_fallback`):
when a header carries no usable RA/Dec, the frame's OBJECT name is resolved
against the bundled DSO catalog (`dso_lookup`, name index + `Messier`/
`Caldwell`/`Barnard` synonyms) and used as the position hint. A recorded
position always wins. The metadata editor confirms a typed name live via
`resolve_object_name` (both backends), so naming a target is a usable repair
for coordinate-less frames.

