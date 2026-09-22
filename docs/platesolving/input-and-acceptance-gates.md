# Plate-solve input and acceptance gates

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


Three defences, added 2026-09-05 after wind-shaken frames were found being
"solved" at 16-193x their true pixel scale and written into the catalog
(measured on the owner's real files; spec-less, the reasoning lives in the
commits and in `docs/backlog-v0.5.5.md` item 5).

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
  input gate refuses a frame whose own analysis shows `median_eccentricity >=
  input_max_eccentricity` AND `trail_r_squared >= input_min_trail_r2` (0.85 /
  0.65, both required — either alone refuses frames that solve fine); the
  acceptance gate finally receives the header's pixel scale, which
  `blind_gate_ok` has always compared against via `blind_scale_header_tol`
  but was given `None`. **Neither gate has a Settings UI**: both live in the
  stored `plate_solve.config` JSON, and `PlateSolveSettingsPanel.tsx` renders
  only `base_verification_tolerance_arcsec`, `sip_order` and
  `autofind_tolerance_deg` — its `DEFAULT_CONFIG` is a hand-written mirror of
  the whole struct, which is why the other fields round-trip without controls.
  The v0.5.5 release notes claimed they were configurable; that was wrong and
  has been corrected in the notes and on the docs site.

**Known gap, deliberately not fixed here**: the FULL analysis path
under-reports eccentricity on trailed frames (0.56 where the fast path sees
0.88) because its stamp is `1.5 x field FWHM` and the FWHM of a streak's
bright head is small — a self-reinforcing measurement. The Analysis table
therefore still shows such frames as good, and the input gate above misses
them. Fixing it changes every stored metric, so it is its own cycle.

**Object-name fallback** (`plate_solve::hints::apply_object_name_fallback`):
when a header carries no usable RA/Dec, the frame's OBJECT name is resolved
against the bundled DSO catalog (`dso_lookup`, name index + `Messier`/
`Caldwell`/`Barnard` synonyms) and used as the position hint. A recorded
position always wins. The metadata editor confirms a typed name live via
`resolve_object_name` (both backends), so naming a target is a usable repair
for coordinate-less frames.

