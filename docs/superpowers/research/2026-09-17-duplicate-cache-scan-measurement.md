# Duplicate cache rebuild and the slow scan — measured, re-diagnosed, fixed (2026-09-17)

> Moved verbatim from `docs/backlog-v0.5.6.md` item 9 when that backlog was retired (2026-10-24).
> The standing decision in `docs/superpowers/open-items.md` and `docs/catalog/README.md` cite it.

Raised by the owner 2026-09-16, asking why the duplicate cache takes so long to
build in the production build. The 2026-09-16 diagnosis below was written from
the code alone; the 2026-09-17 measurement on the owner's production catalog
(40,456 files, 711 folders, a `.backup` copy) overturned its two central
claims, and the fix that shipped is for what the numbers actually showed.

**What was measured (2026-09-17, `examples/phase4_probe.rs`, release build):**

| Phase 4 step | Time on the production copy |
| ---- | ---- |
| Master strong-hash pending SELECT | 0.81 s (0 pending) |
| `rebuild_duplicate_groups_cache(Header)` | 0.10 s (1 group) |
| `rebuild_duplicate_groups_cache(Master)` | 0.012 s |
| `find_duplicate_folders` (the O(folders²) pass, 711 folders) | 1.4 s |
| `rebuild_folder_similarity_cache` | 1.26 s |
| **Whole DB half of Phase 4** | **~3.6 s** |
| Master strong-hash pass, 61 shortlisted local masters (hashes cleared on the copy) | 67.5 s |
| One 156 MB read from the owner's SMB share (`/Volumes/Universe`) | 13.4 s = **11.7 MB/s** |

And from the production logs of 2026-09-16, root 7 (`/Volumes/Universe/Astrophotography`,
an SMB share, 5,611 files):

| Scan | Total | Where it went |
| ---- | ---- | ---- |
| 20:38 — first scan after a re-ingest, 5,410 processed | 997 s | discovery 19 s, parse 109 s, calibration sets 1 s, **master strong-hash 853 s (103 files)**, cache rebuild ~2.8 s |
| 21:08 — nothing new (183 vanished files, 0 processed) | 897 s | discovery **276 s**, unchanged-check **4 min**, Phase 4 **6 min** — all three while the **content-index job** the first scan had autostarted (5,410 files × 3 × 512 KB over the same share, 20:54 → 21:23) was saturating it |
| 21:57 — one new file | 16 s | Phase 4 2.1 s |

**What the numbers say:**

- The duplicate-cache DB rebuild is **not** the slow part: ~3.6 s on this
  catalog, the folder pass 1.4 s. Quadratic in folders, yes, but 711 folders is
  a quarter of a million cheap pair checks; it would need several thousand
  folders before it registered.
- It does **not** run "on every scan": a scan that finds nothing new or
  modified returns at `new_files.is_empty()` before Phase 3/4 ever start. The
  monitor's "effectively free" premise holds for an unchanged folder.
- The 14 minutes labelled **"Building duplicate cache…"** in the UI were the
  **master strong-hash pass** — `fill_master_strong_hashes` reading every
  header-shortlisted master in full at the volume's speed (local masters at
  ~150 MB/s, the share at 11.7 MB/s) — shown as an indeterminate bar with no
  file name, inside the blocking scan modal. One-time per master row (only
  NULL-hash rows are visited), but a re-ingest, or a master whose bytes change
  (the in-place re-parse clears both hashes), pays it again.
- The 15-minute "nothing new" scan was the **content index** competing with
  the scan for the same SMB share. Its 50 ms nap per 64-file chunk protects
  the app's IO in general and does nothing against a scan walking the same
  volume.

**What shipped (2026-09-17):**

- The master-hash pass reports as its own scan phase, `"hashing"`, on
  `scan-progress` — a tick before the first read and one per file with the
  file's path — and the UI labels it **Verifying master duplicates** with
  `N / M files` and the current file. `"caching"` now names only the ~3 s
  cache rebuild that follows. The pass logs its `duration_ms`.
- The content index yields to any active scan: before each chunk, while
  `ServiceContext::active_scans` is non-empty, it parks in 500 ms steps
  (`run_content_index_yielding`, cancel honoured while parked), logging
  `content index paused: scan active` / `content index resumed` with the
  pause's `duration_ms`. Both hosts pass the registry in.

**Still open, deliberately not done:** moving the master-hash pass off the
scan's thread altogether (a background job like the content index, with the
Master cache rebuilt when it lands). It would unblock the scan modal by the
whole hash time at the cost of the Master half of the Duplicates view lagging
the scan. Worth it only if the honest phase above is not enough — the owner's
call after seeing it on a real scan.
