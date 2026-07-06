_The calibration release: build master darks, flats and bias in-app, calibrate your lights with Athenaeum's own rigorously tested math, and export stacker-ready data — plus a major star-measurement accuracy overhaul._

## What's New

- **Master Calibration Library.** Athenaeum now builds master calibration files itself — no external stacker needed. Select any matched calibration set and create a master dark, flat, bias or dark-flat with configurable integration recipes: Average / Median / Winsorized combination crossed with sigma-clip, percentile-clip or linear-fit rejection — or leave it on Auto. Masters are written into a dedicated Calibration Library folder, registered in the catalog exactly as if the scanner had found them, and every light frame and sub-calibration that used the raw set is relinked to the new master automatically. The raw set is marked as superseded — and can be archived to ZIP in one step, right from the build dialog.
- **Batch master building.** "Create all masters" builds an entire equipment profile's worth of masters in dependency order (bias and dark-flats first, then darks, then flats — each flat pre-calibrated with the best available master), with a build-order preview, live per-set status, and in-batch dependency awareness.
- **Master provenance and rebuild.** Every in-app master records exactly which frames and which recipe produced it. Rebuild a master in place after its source frames change — or restore its archived originals first if they were zipped.
- **In-app light calibration.** A new **Calibrate Lights** action applies your master dark/bias and flat to a frame set's light frames, producing 32-bit float FITS ready for your stacking software with its own calibration steps disabled. The math follows the raw-master-dark convention (one subtraction removes bias and dark together), preserves negatives, keeps OSC frames un-debayered, and labels every output honestly (`CALSTAT`, full master provenance in the header). Flat normalization is selectable between two statistics; a side-by-side comparison with PixInsight-calibrated output of the same data agreed to 0.3% rms — Athenaeum keeps its own math, the comparison is a cross-check, not a target. Advanced parameters (trim fraction, output pedestal, bias-fallback policy) are there when you need them.
- **Calibration coverage, visualized.** The Coverage tab shows per-set calibration lights with recipe badges, per-frame calibration details, and one-click master creation for anything missing.
- **Export modes.** Export now offers three modes: calibrated lights only (with a strict readiness gate), raw lights + master calibration, or raw lights + full calibration sets.
- **Compute queue.** Heavy jobs — analysis, master builds, light calibration — now run through a global admission queue with a sidebar indicator, so parallel work cannot oversubscribe the machine. Concurrency is configurable.
- **Calibration Library folder management.** Designate the library folder in Settings → Calibration or the File Manager; the folder is scanned like any other root, so masters built elsewhere are picked up too.

## Changes

- **Star measurement accuracy overhaul.** The analysis engine was compared star-by-star with PixInsight's measurements on three datasets — not to chase its numbers, but to hunt down real defects. Several long-standing problems were found and fixed:
  - Defocused frames now measure correctly. Previously a strongly defocused frame could report absurd sub-pixel FWHM values (stars "1 px wide"); such frames now land within a few percent of the cross-check values.
  - Eccentricity and orientation are now measured reliably. The PSF fit could settle on a too-round solution for elongated stars when a neighbouring star or stamp corner skewed its starting point; per-star eccentricity now agrees closely in the cross-check, and elongation direction overlays align with what you see in the image.
  - Close star pairs (blends) are excluded from measurement instead of being reported as single elongated pseudo-stars.
  - Star overlay ellipses are drawn at 1.2× FWHM (was 2.5×) and the scale is now a setting — annotations hug the stars they belong to.
- **Plate-solving star detection covers the whole frame.** On dense star fields the fast detector could fill its star budget from the top of the image and ignore the rest; it now selects the brightest stars frame-wide. Verified against the full solver corpus.
- **Cleaner builds.** Type-generation warnings about serde attributes are gone.

## Bug Fixes

- Defocused-frame star analysis no longer collapses to sub-pixel FWHM values (the "1 px stars" bug).
- Elongated stars near neighbours no longer measure as nearly round; orientation overlays no longer disagree with the visible elongation.
- Analysis of 3-channel (RGB) XISF files no longer crashes the diagnostic pipeline.
- Export mode is passed explicitly to the backend — a stale persisted setting can no longer silently change what gets exported.
- Master build failures always release their queue slot and report completion, even on internal errors.
- Restoring archived originals detects missing archive ZIPs and offers to forget the archive reference.
- Calibration-set refresh is supersede-aware and prunes emptied sets; scan-root deletion is guarded against removing an active calibration folder.
- Numerous smaller fixes across the master-build preview, batch dialog notes, FITS writer hardening, and the compute queue's admission race.
