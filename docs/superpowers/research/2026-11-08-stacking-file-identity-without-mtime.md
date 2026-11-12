# Stacking file identity without the modification time — backlog (2026-11-08)

**Status: backlog.** Scheduled by the owner for the week of 2026-11-09 («сделаем задачу на следующей неделе»).
Not specced, not planned, not started. Size: M–L (a spec, then a plan). This note is the trace and the rulings the
spec starts from.

## Why

Owner, 2026-11-08: «я считаю что это неправильно завязываться на время изменения, есть хеш, файлы не меняются если
изменилось лишь время изменения».

The stacking cache treats a modification-time difference as a content change. Size equal, time different by 100 ns:
the file is "another file", and the whole chain behind it recomputes (Calibrate, Measure, Register, LN, Integrate).
It never produces a wrong stack — every mismatch errs towards recompute — but moving or copying lights, masters or a
working folder to a NAS or an exFAT drive costs hours of recompute on bytes that did not change.

## Evidence (dev Mac, 2026-11-08)

A probe wrote a file the way the stacker does (tmp, rename, stat), stat-ed it again 3 s later, then copied a local
APFS file in with its time kept (`shutil.copy2`, the semantics of `cp -p`, Finder and a NAS mover).

| Volume | Written in place, stat 3 s later | Copied in with the time kept |
| ---- | ---- | ---- |
| APFS | same | `…47.917852109` → `…47.917852109`, same |
| exFAT (macOS-made image) | same | → `…47.910`, **differs** (10 ms steps) |
| SMB (Universe) | same | → `…47.917852100`, **differs** (100 ns steps) |

The owner's exFAT drive (Fotobase) shows whole seconds on its existing files (written by another tool). The identity
check compares the RFC 3339 rendering with nanoseconds as a string, so every copy across these volumes reads as a
change. The in-place write stayed stable on all three in this probe: the "SMB reports another time after close" loop
the frame-level cache feared did not show here (not ruled out across a remount).

## Where the time decides today

| Site | What it decides | On a moved time |
| ---- | ---- | ---- |
| `stacking/plan.rs` `master_identity`: `path\|size\|mtime` of a live stat, folded in the stage-1 hash | the key of the whole chain | a master moved or copied: every light calibrated with it recomputes everything |
| `stacking/plan.rs` `calibration_hash_for` / `prepare_hash_for_catalog`: the light's `SourceIdentity { file_id, size, modified_at }` from `files` | the key of the whole chain | the scanner sees a drift or a move (`scanner/mod.rs` writes the new `modified_at`): the frame recomputes everything |
| `stacking/plan.rs` `file_matches_identity` / `is_fresh` | a cached file of ours is still the one its row recorded | a working folder moved or copied: every cached file is stale |
| `stacking/measure.rs` `check_stamp_len` under `strict_mtime` (fa50a60a) | pass 1 of an attested light or a project replica | pass 1 reruns (24–34 min on set 270) |
| `stacking/measure.rs` `ReadGuard` | the file did not change between two plane reads of one measurement | a refusal; within one run, one mount, no rounding |
| `stacking/origin.rs` `landed_unchanged` | a landed project file did not change during the run | already tolerant (2 s) |

Project frames already do it right: `prepare_hash_for_project` folds `blake3` and the size, no time.

## Owner rulings (2026-11-08)

1. **No full-file hash.** «пересчет полного хеша опять хрень какая-то, давай половинчатые (как при скане) этого
   достаточно». The content identity is the sampled hash `files.content_hash` (`duplicates::compute_xxhash`: first,
   middle and last 512 KiB). Note: the scan itself does not compute it; the content-index job does, and only when
   sync or content grouping is on.
2. **Athenaeum's own working files need no content check.** «никто не будет менять временные файлы в здравом уме».

## Proposed design (the spec's starting point; calls marked as proposed)

1. **Own files** (`calibrated/`, `prepared/`, `measured/`, registered): fresh = key + size; `file_matches_identity`
   drops the time. The cancelled-rewrite hole the time closed (ruling R7 b: one row and one path per frame for any
   config, so a run cancelled after replacing the file but before the upsert leaves an old row over a new file of the
   same size) is closed by write order instead (proposed): delete the row, replace the file, upsert the new row. No
   extra stat or read.
2. **Foreign inputs** (lights, catalog masters, attested lights, project replicas): the stage-1 hash folds the size and
   the sampled hash, never the time.
   - Light: `SourceIdentity` carries `content_hash` instead of `modified_at`.
   - Catalog master: `size|sample:<content_hash>` instead of `path|size|mtime`; the path leaves the key so a moved
     master recomputes nothing (proposed). A zip master keeps `size|zip:<crc32>`, without the path.
   - Project frame: unchanged.
   - Where the hash comes from: `files.content_hash` when `disk_matches_row` accepts the row; otherwise
     `compute_xxhash` now (1.5 MiB: about 0.13 s on the owner's SMB share at 11.7 MB/s, about 40 s for 300 frames
     once), banked when the row matches. One helper for the plan and the run, so both derive the same key. The time is
     left only as the trigger for re-reading 1.5 MiB, never as a verdict.
3. **`strict_mtime` goes** (fa50a60a): pass 1 is keyed on the calibration / prepare hash, which changes when an
   attested light's sample does.
4. **Untouched:** `ReadGuard` and `landed_unchanged` (checks within one run).
5. **Hash shape:** stage 1 changes, so `stage_hashes_are_pinned_to_golden_literals` is re-pinned and every existing
   set recomputes once after the update. No migration (proposed). It should ship in the same release as the
   frame-level cache (`main` is 40 commits past v0.7.1, nothing released since).

## Accepted risk

A same-size pixel edit outside the three samples, with an unchanged header, is not seen (ruling 1). The
duplicate-detection spec (§2.5) records that sampling cannot tell apart two ERP masters differing by one pixel; for
this cache that case needs a master rewritten in place with an identical header. The spec checks that a master rebuilt
in place rewrites its header (a creation date in the first 512 KiB); if it does not, the master keeps the time as a
trigger for a full re-read and this risk is stated per kind.

## Open for the spec

- `content_hash` gets a second producer (stacking, for its own frames only); the catalog README says "ONE bulk
  producer" and is updated.
- The open-items "known gap" (a landed file edited under a cached first pass, before a rescan) is partly closed by the
  stat plus re-sample at plan time; the spec says how far.
- The scanner's own `(size, modified_at)` contract (`reparse_and_update_in_place`, `disk_matches_row`) is out of scope:
  after this change a scanner re-parse no longer reaches the stacking keys unless the samples change.

## Acceptance

The owed open-items smoke becomes this item's acceptance: a second run on an SMB folder and on an exFAT drive reuses
everything; so does a run after the lights, a master and the working folder are copied there with their times kept.
