_First beta of the 0.2.2 cycle — a data-safety, web-hardening, and cleanup release._

## What's New

- **Web app: optional API-key protection.** Set `ATHENAEUM_API_KEY` on the server and the web UI asks for the key before loading; every API call and the live event stream authenticate with it (header or one-time query parameter for the stream). Leave it unset and the server stays open as before — nothing changes for trusted-LAN setups. Example compose files document both modes.
- **Web app: relink scan roots.** Relinking a scan root to a new location now works in web mode with a server-side directory picker — previously desktop-only.
- **Empty Black Hole button.** One click permanently deletes everything staged in the Black Hole. The confirmation shows the live file count and total size, and the button stays disabled while the deletion runs.
- **Calibration: "Reset to automatic".** Manual calibration assignments are no longer a one-way door — a new action in the manual-assignment dialog clears the override and returns the frame set to automatic matching.
- **Registration guards.** Meridian-flipped (mirrored) frames are detected during registration and flagged with a badge instead of passing silently, and frame sets that mix binning modes or focal lengths are rejected up front with a message naming each group and its frame count.

## Changes

- **Leaner internals: 35 unused backend commands removed** from both the desktop and web backends, along with the superseded star-catalog query engine. No user-facing feature was removed — every deletion was verified against the UI first. (File deletion flows through the Black Hole by design.)
- **Registration tolerance now adapts to pixel scale** instead of using one hardcoded pixel threshold, improving match quality at both short and long focal lengths.
- **Export documentation rewritten** to describe the real WBPP folder/keyword export, including its symlink behavior and platform caveats.

## Bug Fixes

- **Web app: multiple tabs no longer block each other.** All live updates in a tab now share a single event stream. Previously each listener opened its own connection, exhausting the browser's per-server connection limit — a second tab could hang entirely and later listeners silently missed events.
- **Archive restore verifies file content.** The skip-if-existing path now compares the on-disk file's hash against the archived one. Mismatches surface as conflicts — never silently accepted, never overwritten — and a conflicted frame set stays archived so the restore can be retried after you resolve it.
- **Interrupted cross-volume moves self-heal.** A move that copied and verified but failed to delete the source is now reconciled automatically at startup: the leftover source is removed only after the destination re-verifies by content hash. No more phantom "moved back" catalog paths with an orphan copy left on the destination volume.
- **Scanner: volume-aware move detection.** Files whose volume is offline or unmounted are no longer mistaken for moved files, and scan errors that were previously swallowed are now reported in the scan summary.
- **Exact path matching in the catalog.** Path-prefix queries (renames, relinking, scan-root listings) use exact byte-range matching instead of SQL pattern matching, so folder names containing `%`, `_`, or unusual characters are handled precisely.
- **Nested database transactions are now nest-safe** — the remaining raw transaction sites were migrated to savepoints, removing a class of "cannot start a transaction within a transaction" edge cases.
- **Images with unsupported channel counts** (anything other than mono or 3-channel) are rejected with a clear error instead of being mis-rendered.
- **Plate-solve stability:** a crash in one parallel search worker no longer cascades across the whole solve.
- **Missing-files panel reports failures** as notifications instead of logging silently to the console.
- **Docker: the image builds again** — two build regressions fixed (a missing submodule in the build context and a newer Rust toolchain required by a dependency). Multi-arch publishing resumes with this release.
