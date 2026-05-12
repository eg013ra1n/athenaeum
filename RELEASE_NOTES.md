## What's New

The first stable release since v0.1.0 — nine months of beta work landing together.

- **Frame analysis pipeline** — FWHM, HFR, eccentricity, median + frame SNR, SNR weight, PSF signal, background, noise, trail R², and Moffat β measured automatically by the embedded rustafits engine. Inline table with sortable columns, unit-aware px ↔ arcsec toggle, rejection thresholds, FWHM auto-suggestion, and a background analysis queue with a global sidebar progress indicator
- **Inline chart view** on the Analysis tab — Table | Chart toggle, configurable grid of small-multiple scatter plots sharing selection, thresholds, and time axis with the table. Drag to zoom (synced across the grid), shift-drag box-brush select, click to toggle, threshold reference lines with directional arrows
- **Blink viewer** — side-by-side frame review with client-side star annotation overlays drawn from per-star metrics persisted in the catalog. Resizable tabbed sidebar, ROWORDER-aware geometry, Flat Contour Plot mode for diagnosing vignetting in master flats
- **Plate solving** — astrometric WCS storage backed by rustafits v1.0.0 SVD affine fitting. Queue + context layer caches results so multiple views read WCS without re-running the solver
- **Far-Manager dual-pane file browser** — `File Manager → Browse Files` with hot-synced Move / Delete / Rename / Mkdir (single SQL transaction per file, cross-volume moves verify with xxHash before deleting source), catalog search across filename/path/OBJECT/FILTER/IMAGETYP/INSTRUME/TELESCOP, metadata side-panel with bulk edit + automatic calibration-set cascade pruning, Black Hole staging, F3 reveal-in-OS
- **Archive feature** — package finished frame sets into one zip per type in multi-folder destinations, per-calibration disposition (Move / Copy / Skip), hash-verified reconcile-based restore that skips files already on disk, cancel/rollback/resume across crashes, pre-flight free-space and zip-integrity checks. Dedicated Archive page with restore + delete actions
- **Background folder monitoring** — long-running service auto-rescans every registered scan root; non-destructive UPDATE-in-place re-parse preserves `files.id` and junction-table rows across in-place modifications, archive→restore round-trips, and mtime drift
- **Interactive sky chart** — FOV indicators, clickable footprints with labels, kbd-chip overlay, partial-date filter
- **Equipment + Calibration Coverage** — two-way set-ID navigation between chips, camera-filtered dual-pane file browser per hardware row, pre/post-calibration states with consumer chips
- **Duplicates rule chain** — configurable ordered picker (Master root / Path contains / Oldest mtime / Shortest path), live per-rule coverage counts, opt-in byte-by-byte deep verify before any destructive operation
- **Web / Docker build** — Axum HTTP/SSE server with full feature parity with the desktop app via shared `athenaeum-core`. Ship as `vsharifov/athenaeum:0.2.0`
- **Release notifications** — Discord and Telegram posts on tag push

## Changes

- **Frame set clustering** — switched from DBSCAN to deterministic seed-and-grow single-link with spherical-mean re-centering. Dithered/mosaicked fields now collapse into one frame set even when the dither span exceeds the threshold; great-circle distance handles RA wraparound and high-Dec compression correctly. If you previously widened the threshold to catch dithered targets, tighten it now
- **Calibration matching** — manual selection now runs through the same auto-link engine as automatic matching, with housekeeping triggers on edit. Matching rules are fully configurable under Settings → Calibration Matching (per-pair Exact / Warning / Ignore, time-clustering windows, scoring weights, master preferences) as a single JSON config
- **Excluded Frames + Missing Metadata** unified around a single repair shell with a side-panel editor — edit in place, see what's still wrong, move on
- **Reveal buttons** in Blink and Lights Analysis now navigate to the dual-pane file browser instead of the OS file manager. F3 in the dual-pane reveals in OS
- **Grouping settings** split into `grouping.threshold.value` + `grouping.threshold.unit` (deg / arcmin / arcsec)
- **Scanner UTF-8 path hardening** — rejects non-UTF-8 paths with a clear error instead of silently producing replacement-character corrupted strings
- **Sidebar collapsible**, Stage / WIP / Archive tabs on Objects page, standalone Export page collapsed into a tab on Object detail, About page Community section with Discord + Telegram links

## Bug Fixes

- Restored frame sets no longer empty themselves on the next monitor scan tick (two-layer fix: restore syncs catalog mtime + size, scanner's modified-file branch is non-destructive UPDATE-in-place)
- Archived files no longer flagged as "missing" by the scanner
- `get_files` row mapping off-by-one (introduced when adding archive columns) — fixed before any user-visible regression shipped
- Update checker now uses the `semver` crate; beta-to-beta upgrades like `beta.7 → beta.8` are detected correctly
- ROWORDER-flipped FITS frames annotate at correct star coordinates in Blink
- Symbolic links are now followed during scanning and relinking
- Plate-solving pre-check rejects frames missing usable headers instead of falling over mid-solve
- Many smaller fixes across analysis UI, archive flow, and dual-pane browser
