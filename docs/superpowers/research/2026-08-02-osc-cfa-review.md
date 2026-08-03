# OSC / CFA Calibration Review — 2026-08-02

Focused follow-up to the 2026-08-02 calibration audit: does the pipeline handle
one-shot-color (Bayer CFA) data correctly? One deep code pass (every CFA
touchpoint traced with file:line) + an industry-norms research pass, verified
against each other. Companion plan:
`docs/superpowers/plans/2026-08-02-osc-cfa-hardening.md`.

## Verdict

**The pixel math is CFA-safe — no data corruption.** No debayer exists on any
calibration read path (debayer lives only in analysis/preview, provably walled
off); all combining/rejection is strictly per-(x,y); 1-channel input is
enforced at the reader; outputs are genuine 1-channel mosaics; the light-cal
Bayer copy-through works. The problems are (a) one norm gap — flat
normalization is a global channel-mixed scalar where PixInsight/Siril use
per-CFA-channel handling to avoid tinting OSC lights — and (b) a cluster of
real Bayer-metadata defects on outputs, plus zero CFA-specific math tests.

## Industry norms (researched)

- Calibrate in the CFA domain, debayer after — standard. We comply.
- **Per-channel flat scaling for OSC**: PixInsight ImageCalibration computes 3
  separate CFA flat scaling factors (single factor "causes a color shift" —
  their wording; WBPP enables it for OSC); Siril's `equalize_cfa` equalizes the
  master flat's channel means "to avoid tinting". PI notes the effect is
  "mainly cosmetic" (later color calibration removes casts) — a parity gap,
  not corruption.
- **Bayer cards on master files are a real interop requirement**: documented
  WBPP breakage when a master flat lacks BAYERPAT (CFA mismatch / treated as
  mono). `ROWORDER` also matters — CFA pattern interpretation is row-order
  dependent (PI's FITS default is BOTTOM-UP).

## Key findings (verified by hand where marked ✓)

**Important — real defects, norm-independent:**

1. ✓ Masters fabricate `XBAYROFF=0`/`YBAYROFF=0` (`calibration_library/headers.rs`
   hard-codes `None` then `.unwrap_or(0)`); source offsets are never read
   anywhere (`fits_parser` has zero XBAYROFF hits). Tests pin only `.is_some()`.
2. ✓ `ROWORDER` is emitted by nothing: `HeaderBuilder::roworder_top_down` is
   dead code; absent from the light-cal copy-through whitelist; the Phase 2
   spec explicitly lists it; `orientation.rs` knows about ROWORDER flips for
   display but calibration outputs drop the declaration.
3. XISF files carrying only the native `<ColorFilterArray>` element (no
   BAYERPAT FITSKeyword) are treated as MONO end-to-end — PixInsight-written
   XISF is the obvious exposure.
4. Global-only flat normalization: ATH_FNRM (build), CentralThird recompute
   and PixinsightTrimmed (light-cal), AND the per-frame flat-integration
   scales are all channel-mixed scalars → OSC lights carry a flat-color cast;
   cross-frame color drift in sky flats blends untracked. Central-third pixel
   COUNTS are balanced (G exactly 50%, R−B skew <0.1%) — it is a level blend,
   not a sampling bias.
5. No CFA compatibility validation anywhere: a mono master flat divides an OSC
   light silently; a CFA phase shift (ROI at odd origin, same dimensions) is
   structurally invisible (offsets never parsed, no matching/grouping key).

**Minor:** three silent Bayer-card drop paths (unknown pattern arm, LIMIT 1
lookup miss, missing `fits_header` blob → whole copy-through dropped) — all
without a warn; blob↔frames drift (master rebuild never re-parses; scanner
reparse updates `frames.bayerpat` but NOT the `fits_header` blob, while
copy-through reads the BLOB — note CLAUDE.md documents reparse as updating
fits_header, so this is also a doc-vs-code mismatch to resolve); master
BAYERPAT chosen by unordered `LIMIT 1` from one arbitrary member with no
member-consistency check; the 3-channel rejection message calls a LIGHT a
"calibration frame"; Flat Analysis contour plot block-averages mixed CFA
pixels (display-only); ✓ false comment "frames has no bayerpat column" (the
column exists and is live); zero CFA math tests (no mosaic fixture anywhere).

**Verified clean:** no debayer on calibration paths (both readers); per-pixel
combine/rejection; 1-channel enforcement both at master build and light-cal;
un-debayered 1-channel outputs; light copy-through of BAYERPAT/XBAYROFF/
YBAYROFF with correct card typing; master registration round-trips CFA
metadata (new builds); foreign CFA master ingestion preserves everything;
synthetic bias CFA-neutral; analysis debayer correctly isolated; central-third
sampling balance; `frames.bayerpat` live (export OSC detection uses it).

## Ratified direction (this review, owner-vetoable at plan review)

PI-style **per-CFA-channel flat scaling at light-cal time** (option, default ON
when the light carries a Bayer pattern; CentralThird mode v1, PixinsightTrimmed
stays global with a documented follow-up), per-channel constants stamped on
CFA master flats at build; masters emit real offsets + ROWORDER with a
deterministic member-consensus rule; XISF ColorFilterArray support; advisory
(never blocking) CFA-compat warnings in matching and light-cal; CFA mosaic
test fixtures.

## Status: cycle complete (2026-08-03)

Every finding above is closed or deferred with owner visibility — nothing is
left silently open.

**Closed in code:** CFA phase offsets and `ROWORDER` are parsed into the
catalog (FITS + XISF) and reach both master files and calibrated lights, at
their REAL values or not at all; master Bayer cards come from a deterministic
member consensus instead of an unordered `LIMIT 1`; per-CFA-channel flat
scaling ships (default ON for colour lights, `CentralThird`), with the
constants stamped on CFA master flats and reused only when the flat's phase
matches the light's; the XISF `<ColorFilterArray>` element is adopted; every
silent Bayer-drop path now warns; the blob↔columns drift is gone (scanner
re-parse and master rebuild both rewrite `fits_header`, and copy-through falls
back to the columns when the blob is silent or blank); CFA compatibility is
checked and surfaced as an advisory in readiness and at calibrate time; the
3-channel rejection message no longer calls a LIGHT a "calibration frame"; and
the "zero CFA math tests" gap is closed — mosaic fixtures now run from the
channel-geometry unit level through three end-to-end tests that assert the
cards on files actually written to disk.

**Deferred, all recorded** in the companion audit's "Deferred follow-ups":
`pixinsightTrimmed` per-channel variant; the Flat Analysis contour plot on CFA
data (display-only); a matcher-level `bayerpat` parameter; whether a mono flat
on OSC lights should ever hard block (currently advisory); a per-batch divisor
memo (perf only); and phase-class canonicalization, so `GRBG` at (0, 0) and
`RGGB` at (1, 0) stop reading as different mosaics.
