*Athenaeum stacks: a frame set becomes its master light in-app — calibration, weighting, registration, local normalization, integration and drizzle, with frames of different pixel scales in one set.*

## What's New

- **Stacking.** A frame set's new **Stacking** tab builds its master
  light(s) in-app: calibration from the set's matched masters, per-frame
  quality measurement and weighting, reference selection, registration,
  integration with automatic outlier rejection, and a FITS master carrying
  the reference's WCS. One run drives a pipeline board with a panel per
  stage, three presets (Default, Fast preview, Maximum quality), cached
  stages for fast re-runs, and full provenance per run. The run builds any
  calibration master it needs and cannot find — including the
  pre-calibration masters a flat's rebuild reads — so an empty library is
  never a blocker. **Settings → Stacking** holds the pipeline defaults (the
  same tree a set can override) and the working/output folders.
- **Local normalization.** The Normalize stage can build a per-group
  reference from the best frames and correct every frame's background and
  scale locally — a background model on a coarse mesh plus a PSF-flux scale
  — for the master and for outlier rejection. Sidecars are cached like every
  other stage.
- **Drizzle.** A run can produce a 1×/2×/3× drizzled master next to the
  regular one — square (exact clipping), circle or gaussian drops, using the
  run's frame weights, per-frame rejection and local normalization, with an
  optional weight map; the WCS scales with it. The Maximum quality preset
  turns on local normalization and 2× drizzle.
- **Mixed pixel scales in one set.** A set may hold groups — or members of
  one group — shot at different pixel scales: a bin-2 group, a second
  telescope, another camera. Every frame's pixel scale comes from its plate
  solve or its header; the plan warns (never blocks) on a scale spread and
  shows it in the groups table; registration's scale gate is per frame and,
  when both frames are plate-solved, the alignment is seeded from the two
  solutions — the way a coordinate-based aligner works — and refined with
  star pairs (the frames table marks such rows **WCS**). Two geometry modes
  in the Register panel: **co-registered** resamples every group into the
  set reference's geometry; **native** keeps a per-group reference and
  geometry with no cross-group registration.
- **Smarter frame weighting.** Star detection for the quality measurement
  is noise-relative (the *Detection threshold (σ)* setting in the Measure
  panel, default 20), so a bright-sky night no longer floods the ranking
  with faint stars and the weights follow the frames' real quality across
  nights and cameras.
- **Two-pass reference pick.** Registration first checks the best-weighted
  frame against the set's median framing and, when that frame is rotated or
  shifted against the rest, picks the closest top-weighted frame instead —
  the automatic choice now matches what an experienced user pins by hand.
  Off in the Fast preview preset.
- **Stronger outlier rejection.** Linear-fit clipping fits a robust line to
  the sorted stack; at the default thresholds it now rejects ≈ 3 % of
  samples (satellite trails, cosmic rays, frame edges) where it used to
  reject under 1 %.
- **WCS in written files.** Masters — and any FITS the app writes from a
  plate-solved frame — carry the solution's WCS and SIP distortion cards.

## Changes

- **Grouping by exposure, not by camera.** Frames from different cameras
  with the same colour mode, filter, binning and exposure (within the
  tolerance) integrate together; master names read
  `<object>_<filter>_<mono|osc>_<exposure>_<n>x.fits`.
- **Faster measurement and local normalization** on large frames
  (measurement ≈ 20 % faster, the per-frame normalization pass ≈ 7 %).
- The plate-solve-era registration preview (a development-only tab) is
  gone; registration lives inside a stacking run.
- The web build's folder picker answers 400 to an unknown scope instead of
  falling back to the scan roots.

## Bug Fixes

- **Export: "Lights + calibration sets" lands the raw originals.** After a
  master was built, that export mode collected the built master in place of
  the raw calibration frames the mode promises; it now swaps each built
  master back for the raw set it superseded and blocks the mode up front
  when those originals are not on disk.
- **Folders: role folders list their missing files too**, so a missing frame
  under a lights/darks/flats folder shows up in the missing-files panel like
  one under a plain folder.
