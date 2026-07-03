_Stable release combining the 0.2.2 data-safety/web-hardening cycle and the 0.2.3 logging overhaul._

## What's New

- **Structured logging across the whole app.** Every operation now logs through one leveled pipeline (error / warn / info / debug) into rotating JSONL files — desktop: `<app data>/logs/`, Docker: `/data/logs/` plus JSON on stdout. A new **Settings → Logging** section switches the global level and per-module verbosity (scanner, solver, calibration, archive) live, without restart; `ATHENAEUM_LOG` overrides everything for power users (full filter syntax, including per-item `trace` detail). Every command records its duration and outcome; scans, solves, archive/file operations, exports, and registration runs are correlated end-to-end, so "show me everything about that scan" is one query. "Open log folder" in Settings gets you straight to the files — perfect for support bundles.
- **Web app: optional API-key protection.** Set `ATHENAEUM_API_KEY` on the server and the web UI asks for the key before loading; every API call and the live event stream authenticate with it. Unset = open as before for trusted-LAN setups.
- **Web app: relink scan roots** — now available in web mode with a server-side directory picker (was desktop-only).
- **Empty Black Hole button.** One click permanently deletes everything staged in the Black Hole, with a confirmation showing the live file count and total size.
- **Calibration: "Reset to automatic".** Manual calibration assignments are no longer a one-way door — clear the override and the frame set returns to automatic matching.
- **Registration guards.** Meridian-flipped (mirrored) frames are detected and flagged with a badge instead of passing silently; frame sets mixing binning modes or focal lengths are rejected up front with a message naming each group; match tolerance now adapts to pixel scale instead of one hardcoded threshold.

## Changes

- **Leaner internals**: 35 unused backend commands and the superseded star-catalog query engine removed from both backends — no user-facing feature was removed. All ~950 legacy print statements were audited and replaced by the structured logging pipeline (or deleted where they carried no diagnostic value).
- **Docker Hub images now identify themselves**: the repository description carries a live tag → version table, and images ship standard OCI version/revision labels (`docker inspect` shows exactly what you're running). A running container also prints its version as the first log line.
- **Export documentation rewritten** to describe the real WBPP folder/keyword export, including symlink behavior and platform caveats.

## Bug Fixes

- **Web app: saving settings works again.** Calibration-matching, analysis, plate-solve, and export configurations could not be saved in web/Docker mode (the logging config would silently reset instead of saving) — all five config endpoints now accept the frontend's payload correctly, with regression tests.
- **Web app: multiple tabs no longer block each other.** All live updates in a tab share a single event stream; previously each listener opened its own connection, exhausting the browser's per-server limit — a second tab could hang entirely.
- **Archive restore verifies file content.** The skip-if-existing path hash-verifies the on-disk file; mismatches surface as conflicts — never silently accepted, never overwritten — and a conflicted frame set stays archived so the restore can be retried.
- **Interrupted cross-volume moves self-heal.** A move that copied and verified but failed to delete its source is reconciled automatically at startup; the leftover source is removed only after the destination re-verifies by content hash.
- **Scanner: volume-aware move detection.** Files on offline or unmounted volumes are no longer mistaken for moved files, and previously swallowed scan errors now show up in the scan summary.
- **Scanner: post-scan database checkpoint actually runs now** — it had been failing silently since it was written (caught by the new logging on its first real scan), so post-scan disk activity settles faster.
- **Exact path matching in the catalog.** Renames, relinking, and scan-root queries use exact byte-range matching instead of SQL pattern matching — folder names containing `%`, `_`, or unusual characters behave precisely.
- **Nested database transactions are nest-safe** (savepoints replace the last raw transaction sites).
- **Images with unsupported channel counts** are rejected with a clear error instead of being mis-rendered.
- **Plate-solve stability**: a crash in one parallel search worker no longer cascades across the whole solve.
- **Docker builds again** — two build regressions fixed; multi-arch publishing to Docker Hub resumes with this release.
