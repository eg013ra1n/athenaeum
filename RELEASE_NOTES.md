## What's New

- **New star catalog — downloadable Gaia DR3 density tiers.** The plate solver now runs against a Gaia DR3 catalog organised into additive density tiers (500 / 2 000 / 5 000 / 8 000 stars/deg²) that you download on demand. The app looks at your LIGHT frames' fields of view, recommends the right tier, and lets you fetch only the tiers your equipment needs — each is a separate resumable, checksum-verified download. Deeper coverage (down to ~G21 in star-poor sky) solves fields the previous catalog could not. Managed from **Settings → Plate Solving → Star Catalog**, with per-tier install status and a one-click "download the recommended set".
- **Plate solving solves more fields.** Blind solving without RA/Dec hints, long focal lengths, SNR-ranked star selection for galaxy- and nebula-contaminated fields, scale recovery when the focal-length hint is wrong, and a calibrated confidence gate before any WCS is written. A runtime CRVAL fallback reads coordinates from legacy headers without a rescan.
- **Signed & notarized macOS builds.** macOS `.dmg` downloads are now code-signed with a Developer ID certificate and notarized by Apple. Just open the disk image and drag Athenaeum to Applications — no more `xattr` quarantine workaround.
- **Metadata editor upgrades in Browse Files.** Double-click an image file to open its metadata editor in the opposite pane. Side-by-side WCS comparison (current vs FITS header) with a one-click **Revert WCS to FITS header**. Editable XPIXSZ on any frame, per-INSTRUME/TELESCOP XPIXSZ defaults for sparse headers, and inline focal-length recalculation from the plate-solve result.
- **Notification center.** A global notification hub with persistent history, opened from the sidebar bell — scans, exports, analysis, plate-solves, archives, and file operations all report their outcomes in one place.

## Changes

- **Star catalog is now download-on-demand by field of view.** The old bundled catalog and the manual focal-length / sensor calculator are gone; the app recommends and installs the right density tier from the frames you already have.
- **macOS install no longer requires a security workaround** — signed + notarized builds launch straight from Applications.
- **UI polish aligned to the Nord palette** across the Plate Solving settings and the Browse Files metadata panel: a consistent "Saved" confirmation, correct accent-button contrast, and a tidier catalog table.
- **Older databases self-heal on launch** — catalogs created before header fingerprinting fill in their fingerprints automatically, so file relinking works with no manual step (the manual "Backfill" maintenance button has been removed).

## Bug Fixes

- Scanner in-place re-parse now works correctly inside the parallel scan and respects user metadata overrides.
- Preview cache is keyed by modification time in both the desktop and web backends; image stretch is NaN-safe.
- FITS headers dumped as ASIAIR "XISF FITS Keywords" text blocks are now parsed.
- Black Hole staging is idempotent — bulk moves no longer create duplicate rows.
- Blink keeps deleted, selected, and current frame rows visually distinct.
- Metadata WCS revert is one-shot (the button commits, with no separate Apply step); revert-to-NULL now clears every nullable field, including XPIXSZ.
- Plate solving confidence-gates the WCS / focal-length write-back and threads mid-solve cancellation through to the solver.
- Many smaller fixes across the plate-solve UI, catalog panel, and Browse Files metadata editor.
