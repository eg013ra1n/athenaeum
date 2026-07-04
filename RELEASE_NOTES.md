_Foundation release: catalog identity, generated frontend types, a unified command layer, and a standards-compliant FITS writing engine — the groundwork for in-app master calibration, mosaics, and multi-user collaboration in the upcoming releases._

## What's New

- **Your catalog now has a permanent identity.** The catalog itself and every file, frame, frame set, session, calibration set, tag and export template carry a globally unique ID and a last-modified timestamp, maintained automatically by the database. This is the identity layer that upcoming releases build on — sharing frame sets between machines, master-file provenance, and group imaging projects. Existing catalogs migrate automatically and instantly on first launch; nothing to do.
- **FITS writing engine (under the hood).** Athenaeum can now *write* FITS files, not just read them: a strict FITS 4.0 writer (validated headers, long-string support, atomic file writes) plus a typed keyword vocabulary following the conventions your capture software already uses (SBFITSEXT, NINA-style keywords, WBPP-recognizable `IMAGETYP` values, `ATH_*` provenance namespace). Not user-visible yet — it is the prerequisite for the next release's in-app master calibration library.
- **Exposure time from `EXPOSURE` keyword.** FITS files whose capture software records the exposure only as `EXPOSURE` (instead of `EXPTIME`) now have their exposure time recognized by the scanner.

## Changes

- **Desktop and web now share one backend implementation.** About 70 commands across scanning, file management, calibration and analysis were consolidated so both platforms run literally the same code — the recurring class of "works on desktop, subtly broken on web" bugs can no longer drift apart. Several such drift bugs were found and fixed during the consolidation (see Bug Fixes).
- **Frontend types are generated from the backend.** The UI's data types are produced directly from the Rust models and checked by a test on every build — desktop app, web app and UI can no longer disagree about data shapes silently.

## Bug Fixes

- **Web: path traversal in folder creation blocked.** Creating a folder with a crafted path (`../`) could escape the configured allowed directories on web/Docker deployments; folder creation now resolves paths against the real filesystem and fails closed. Folder handling inside symlinked scan roots is also more reliable.
- **Web: errors are reported instead of silently swallowed.** Rebuilding calibration sets after a scan-root change, renaming catalog folders, and bulk-restoring calibration metadata now surface their failures on web exactly as they do on desktop (previously some reported success while partially failing).
- **Web: file-move operations now announce completion** to the UI the same way desktop does.
- **Calibration metadata edits keep their intent.** The "is master" flag in bulk metadata edits is correctly optional again in the desktop/web API contract.
