# Open items

What is finished in code but not yet confirmed by hand, and what has already been
decided and should not be reopened. Everything here sits on `main` — the development
trunk since 2026-08-24, when the project went open source and version branches were
retired — and covers the eleven completed cycles on top of the last tag.

Plans, specs and audits live in `plans/`, `specs/` and `research/` beside this file
and hold the detail. This file holds only the residue: the checks nobody has run and
the calls that have already been made.

**Keeping it current.** A cycle that lands adds its unverified checks here. A check
that passes is deleted, not ticked — with the date and the measurement, if there was
one, moved into the cycle's own doc. A decision that gets ratified moves from
"awaiting a call" into "standing".

---

## Standing decisions — do not re-flag these

Each of these has been raised by an audit or a review at least once and answered.
They read like bugs; they are not. Re-proposing them costs a cycle every time.

| Decision | Where it bites | Why |
| ---- | ---- | ---- |
| **Missing files are not orphans.** No pass anywhere may delete catalog rows because a path is absent from disk. | Scanner, any "garbage collect" proposal | Astro archives live on detachable storage. A file absent during a scan usually means the volume is not mounted, not that it was deleted. Auto-cleaning destroys the catalog the first time someone scans without the archive drive. If explicitly asked for, it is a manual opt-in operation with a preview — never part of scanning. |
| **A file in the Black Hole still counts as present** for transfer dedup, on both the pre-handshake Offer/Want path and at ingest. | `sync/responder.rs`, `sync/ingest.rs`, `find_files_by_content_hashes` | The bin is still the file being on the machine, so calling it a duplicate is honest. To re-receive, the user purges the Black Hole first — the hard delete drops the `files` row and dedup wants it again. An audit proposed excluding blackholed rows from the "have" set; rejected. |
| **One physical camera per scan root.** | Anything that would disambiguate two cameras sharing an `INSTRUME` label | Athenaeum does not read camera serials, and the owner already separates identical bodies by folder. Serial numbers, readout mode and other hardware fingerprinting are not needed — the scan-root path is the answer. |
| **Flats are shot after the session.** An equidistant tie-break prefers `FlatTiming::After`. | `flat_matcher.rs::apply_pattern_selection` and any flat tie-break | A post-session flat captures the optical state the lights were actually taken under: dust drift and dewing accumulated during the night. Do not reverse this to "prefer before" on the strength of literature that assumes a different workflow. |
| **The sampling xxHash used to verify a cross-volume move is intentional.** | `duplicates::compute_xxhash`, `file_op/executor.rs` | It reads first/middle/last 512 KB, so corruption in the unsampled remainder passes verification before the source is deleted. Speed was chosen over exhaustive verification; `verify_byte_identical` exists for callers that want the full check. Flagged as a bug in the 2026-06-10 audit and confirmed intentional. |
| **A NULL CCD-TEMP matches in `Warning` mode**, bypassing the threshold. | `calibration/configurable_matcher.rs` | Uncooled cameras write no temperature keyword; those frames should match rather than be excluded. Flagged in the same audit and confirmed intentional. |
| **The preview stretch is scale-adaptive.** | rustafits `stretch.rs` / `pipeline.rs` | The midtones parameter adapts to the normalized median, so `[0,1]` float FITS render almost identically to the same data in u16 ADU despite the hardcoded `max_input = 65536.0` — pinned by `unit_range_floats_stretch_like_u16`. A "missing float normalization" finding was raised and retracted. The real float bug was NaN poisoning the sample statistics, and it is fixed. |
| **The Folders deep-link token is deliberately not monotonic** (raise → consume → lower → re-arm). | Folders workspace deep-linking | Monotonicity was the replay bug. Restoring it reintroduces it. |
| **The calibration-set empty-prune trigger and `prune_orphaned_calibration_sets` are deliberately not identical.** | `db/schema.rs` | The per-row trigger exempts master-library sets because a master legitimately loses and regains its sole member during a re-import and the trigger fires inside that window. The whole-table prune runs only at quiescent points, where a member-less unreferenced master really is garbage. Both doc comments say so at length. |
| **The XISF parser drops the `comment` attribute of `FITSKeyword`, and fixing it is not a duplicate-detection fix.** | `fits_parser/mod.rs`, stored `fits_header.header` blobs | PixInsight writes history as `value="" comment="ImageIntegration.rejectedHigh_32: …"`, so our blob holds 364 empty `HISTORY =` lines. Including `comment` separates only 4 of 30 master groups — the other 26 share every keyword and property and differ only in pixels, so masters stay excluded from the header key either way. Processed **Light**-derivatives are a different case and the reason this is not free: a GraXpert/ABE output keeps `IMAGETYP = 'Light'` and `is_master = 0`, so the header key admits it and the erased history makes it identical to its source — they are shielded by the header key's `files.filename` component (spec D8), not by the master exclusion. Worth fixing for the metadata pane's per-field revert and light calibration's Bayer copy-through, which read that blob — but NOT in the same release as the duplicate-key change: a changed blob changes the fingerprint, so a re-scanned file stops matching its not-yet-re-scanned copy until both are scanned. |
| **The three-part sampling hash is not a better default key than the header.** | Any "just use `compute_xxhash`" proposal | It IS `files.content_hash`, so the proposal is the existing `Content` branch. Measured: identical answer to the header key on raw frames (40/40 vs 80/80 against full SHA-256) for 61.4 GiB of reads and ~19 min; and on masters it is wrong in the DELETING direction — three of thirty groups are `..._DBE_WCS.xisf` / `_f.xisf` pairs differing by 3-4 bytes at 0.5-0.9 MiB, past the first sample and nowhere near the middle or end. Spec §2.5. |

---

## Unverified by hand

### v0.5.5 stage 2 — nights (2026-09-05)

- **Recalculate nights** on the real LDN 1272 (set 109) in the desktop app →
  three nights (13→14 Sep, 17→18 Oct, 18→19 Oct). Verified headless on a copy
  of the dev catalog through the web backend the same day; the click and the
  page reload are the only unverified part.
- Merge two sets whose lights are the two halves of one night → one night on
  the target, source gone (core-tested; not yet clicked).

Newest first. Every cycle below is code-complete with green gates and a clean final
review; what is missing is a human running the flow on real data.

### Calibrated-export v2 (2026-08-31)

Spec `docs/superpowers/specs/2026-08-31-calibrated-export-v2-design.md`. The
standalone "Calibrate Lights" op is gone; the **Calibrated lights** export mode now
calibrates every LIGHT frame at export/send time from its linked masters, applies
hot-pixel cosmetic correction from the master dark, and VNG-debayers OSC lights
(`rustafits`'s `astroimage::processing::vng`), engine version bumped 2 → 3. Verified
so far only by the unit suite and a throwaway pixel-diff harness (spec §1/§6) — no
owner run through the shipped Export tab or Transfers yet.

- Real-data export: LDN 1272 (OSC ZWO ASI2600MC Duo + mono ATR2600M,
  `~/Pictures/LDN1272`) through the Export tab in **Calibrated lights** mode,
  spot-compared against the external-reference calibrated/debayered outputs already
  collected for spec §1/§6 under the same directory (the reference calibrated pair
  and the `debayered/` folder, log `20260830105156.log`): math stays at float32
  rounding (as the throwaway harness already found), the VNG output matches the
  reference debayer within tolerance (no checkerboard/color-swap, small p99.9
  residual on interior pixels — bitwise equality is not expected), and the
  `ATH_CHPX` hot-pixel counts land near the reference's ~0.24%-of-pixels figure.
  This is the first run through the real UI (gate sentences, the two toggles, the
  `"calibrating"` progress phase) rather than the unit/harness path.
- Two-instance smoke: frame-set send in **Calibrated lights** mode (Transfers) —
  the masters-built gate blocks/unblocks the same way the Export tab does,
  generation runs during the `preparing` phase (not a copy), the receiver lands
  `c_*.fits`/`c_*_d.fits` with no catalog row at all, and re-sending after a
  re-calibration lands `c_*_2.fits` beside the first copy instead of replacing it
  (expected — see the release-note caveat below, not a bug to file).

#### Follow-ups surfaced by review (not smokes)

- **Collab publish rework — its own cycle.** Publishing a device's own lights is
  currently blocked unconditionally (spec §8a, decision C: the project gate's
  `LightCalStatus` resolves to `NotCalibrated` for every frame, so
  `publish_collab_frames` always fails with "no publishable frames"). The rework
  is generate-at-publish with a masters-built gate, mirroring this cycle's export
  gate. It must also un-ignore the 9 collab tests `#[ignore]`d pending it
  (`api/collab.rs` ×7, `api/collab_e2e_tests.rs` ×2, all tagged "collab publish
  rework pending — calibrated-export-v2 spec §8a") — they were rewritten to assert
  the blocked behavior, not deleted, specifically so the rework has something to
  flip back.
- CFA-mismatch advisories (light vs. master Bayer phase) used to surface through
  the old standalone dialog's readiness call; that dialog is gone and nothing
  replaced the surface — the per-frame engine-side logging still runs, but the
  set-level warning reaches no UI. Follow-up: add a `cfa_warnings` field to
  `ExportReadiness` (or the export summary) so Coverage/Export can show it again.
- Old `c_*` trees left under the Calibration Library root by the retired
  standalone flow are uncataloged leftovers on disk — this cycle deliberately does
  not migrate or delete them (spec §2). Manual cleanup, at the owner's
  convenience, whenever the library root gets tidied.
- `sync_prepare::spawn_prepare` holds the ONE staging slot for the WHOLE
  generation phase of a calibrated-lights send (minutes, not seconds) — a
  second send meanwhile sits in `preparing` at 0 bytes with no progress event
  of its own (fixed in the review-fix wave: it now announces itself with the
  row's known byte total before trying for the slot, and again before trying
  for the compute slot, so the wait itself is visible). The model-level
  question — a separate generation slot, or resolving generation before the
  staging slot — is still open and is an owner call.
- A flat deleted between readiness and preparation (the race window C-2 does
  not close) fails inside `resolve_generation_cached`'s norm-divisor read with
  a generic decode error rather than the C-2 "master file missing on disk"
  sentence — the failure is still early and non-partial, and readiness
  catches the case up front, so this is a message downgrade in a narrow race
  window only. A per-frame stat between `resolve_frame_inputs` and the
  divisor computation would close it.
- `get_export_readiness` resolves masters per LIGHT with no per-set
  memoization (`lights.rs:186-200`): a 500-light set runs `resolve_master` up
  to ~1500 times for a handful of shared sets, on a UI path called on every
  dialog open and mode switch. A `HashMap<i64, Option<String>>` memo would
  cut it to one query per set. Deferred out of this wave by the minimal-scope
  constraint.
- The failed-regeneration sibling sweep (`file_organizer.rs`, review fix #5)
  only clears the OPPOSITE-toggle output (`c_x_d.fits` when this run just
  failed to write `c_x.fits`). A previous run's SAME-name output
  (`c_x.fits` already on disk, this run fails to regenerate `c_x.fits`) is
  left untouched, and WBPP would ingest that stale artifact. Pre-existing
  (not introduced by this wave), out of its scope; a fix would need the
  failure arm to also remove `dest` itself when it already exists from an
  earlier run.

### Transfer preparation + single-copy footprint (2026-08-30)

Spec `docs/superpowers/specs/2026-08-30-transfer-prepare-and-footprint-design.md`.

- Send a ≥ 20 GB object from this Mac: the dialog closes in < 1 s, the row reads
  `preparing · 300 files · X / Y · speed`, Cancel mid-way removes the row's dir,
  a fresh send prepares while a second one waits in `preparing` at 0 B.
- After confirm, `<packages>/<uuid>` is manifest-only and `blobs/` on the Mac
  stays in the tens of MB (outboards only).
- Receive on the pod (ext4): `du blobs/` drops to KB right after export, landed
  files share inodes with `staging/` until confirm, storage card matches `du`.
- Kill the app mid-preparation: on relaunch the row is `failed — preparation
  interrupted`, its dir is gone.
- Settings → Transfers: move both folders, restart, send + receive again;
  Storage shows the leftovers in the old folder and Clean up frees them.
- Perseus resend against the same receiver still lands (Copy path untouched).

#### Follow-ups surfaced by review (not smokes)

- `set_transfer_paths` has no "keep this one" value on the wire (`Option<String>`
  means set-or-reset), so the UI resends the other folder's configured value and
  the backend re-validates it (`create_dir_all` + write probe). An unreachable
  working folder therefore blocks an outgoing-only change — with an honest
  message, on the right card.
- `fetch_collection_multi`'s Copy-export loop has no stale-target guard (Task 7
  deferred it): a swarm retry into the same staging path could hit the self-copy
  truncation if a collab package ever shares a hash with a push-path package now
  referenced in place. The guard is `export_child`'s — delete the complete staged
  children on every retry.
- An `announced` row's bar sits at the 0.95 cap it inherited from the final
  indexing byte count; speed and ETA are correctly suppressed there.
- `cleanup_finished_transfers` (Settings → Transfers → Clean up) removes terminal
  payload dirs without a `protect_shared_before_cleanup` pass — under
  `TryReference` a live sibling package sharing a byte-identical child can
  transiently lose its winning external path (self-heals: the next serve's
  readability probe Copy-re-imports it; it takes a manual click while a
  shared-content transfer is live). Fast-follow: run the protect hook per
  terminal package before its dir is removed.

### Sender counter subtracts the peer's duplicates (2026-08-30)

`TransferFileCounts` grew `duplicate` / `duplicateBytes` (files settled
`done`/`duplicate` at negotiate — the §D4 want-subset exclusion — plus ack-time
ingest duplicates). The send-side row subtracts them from files AND bytes, so it
counts what the receiver counts. Verified against the live sender DB for the
LDN 1272 transfer (562 files, 262 already on the peer): the new `GROUP BY`
returns `duplicate=262`, `duplicate_bytes=13.65 GB`; the confirmed sibling
transfer reports its 8 ingest-time duplicates the same way. Not yet seen on a
running build:

- Re-send an object the peer partly holds: the Transfers row reads
  `N of M files · X / Y · K already on peer` where `M` and `Y` match the
  receiver's own `of M` and total; the progress bar reaches 95% at upload end,
  not ~53%.
- The sidebar Transfers panel shows the same `N of M`, with `K already on peer`
  in the tooltip.
- After confirm, the history row reads `M files · Y · K already on peer`; a fully
  duplicate send reads `0 files · 0 B · K already on peer` next to the existing
  "Peer already had every file" banner in the detail pane.
- Received rows are unchanged (`80 of 300` stays `80 of 300`).

### Frame-set send (2026-08-28) — two-instance smoke

Real object with a raw dark set, a master flat, and a few calibrated lights; second
instance on the same account as the receiver.

- Export tab shows four modes with file counts; `Lights + masters` is disabled with
  "Build masters first — 1 set without a master" and → Coverage lands on that set.
- Build the master on Coverage, return: the radio enables without a reload.
- Send each of the four modes; on the receiver the batch folder opens in WBPP as-is
  (`camera_<x>/BIAS_/DARKS_/FLAT_/lights`), calibrated batch = `camera_<x>/lights/c_*.fits`
  only.
- Receiver Equipment shows the received master as imported (no Rebuild), the raw dark
  set exists with all members.
- Receiver: with the source light also received earlier, the light shows *calibrated*;
  without it, only the file is on disk and the log says "deferred".
- Re-send the calibrated batch: all files report duplicate, no `_2` copies.
- Export to WBPP in `Lights + masters` with a raw set linked is refused with the same
  sentence the tab shows.
- The → Coverage link from a *flat* (or bias) set without a master lands on the matching
  library with that set highlighted, not the Dark library.
- Lights Analysis tab (2026-08-30): no *Send to…* button; with nothing selected the
  Blink button reads *Blink all* and blinks every displayed frame; Shift-click on a
  row (or its checkbox) selects the range from the last plain click in the displayed
  order, and a re-sort between the two clicks does not move the range.

#### Follow-ups surfaced by review (not smokes)

- Receiver dedup for calibrated artifacts is by `sync_receipts` content hash alone
  (spec §4.1), so a deleted received `c_*.fits` is never re-receivable on that
  receiver — a spec property to ratify or revisit.
- A resend after a failed post-ingest calibration-set integration does not retry the
  integration (the receipt-reuse short-circuit leaves the inserted-frames list empty;
  only the `sync_events` journal records it).
- Pre-existing, not this cycle: `cargo check -p athenaeum-core --no-default-features
  --tests` fails (~40 errors in integration tests that use `render`/`solver`-gated
  modules unconditionally); the lib target is green headless. Worth a separate ticket.
- A re-sent artifact whose identity is already tracked at another path is dropped as a
  duplicate — the receiver's existing artifact wins, so re-sending a RE-calibrated light
  does not replace it (spec §4.1; pinned by
  `calibrated_light_already_tracked_is_dropped_as_duplicate`).
- App-shell retention removed 2026-08-29 (owner ruling) — `sync_sources` table is
  vestigial; drop it in a future schema pass.

### Hash cleanup — 2026-08-28

The catalog carried four hashes with three producers for one of them. First task
landed: `files.metadata_hash` (`size + mtime + filename`, the pre-2026-08-27
duplicate key) was write-only since the header key replaced it — no SQL, no
frontend consumer, an index maintained on every insert for nothing — so column,
index, `File.metadata_hash`, `compute_metadata_hash` and the never-read setting
key `duplicates.content_hash_rescanned` are gone. Existing catalogs lose the
column on the next start (`DROP INDEX` then `DROP COLUMN`, guarded on
`pragma_table_info`, rows untouched). Spec D2 of
`2026-08-27-duplicate-detection-design.md` was corrected: its stated reader
(`has_duplicate`) never read the column.

- First launch on the production catalog: the log carries one
  `dropped write-only column` line, `SELECT COUNT(*) FROM pragma_table_info('files')
  WHERE name='metadata_hash'` is 0, and the second launch logs nothing.
- Files page, Missing-metadata view and the Duplicates view open and list
  frames — the shared file+frame projection lost a column and every index
  after it moved down by one.
- A scan of a root with changed files still re-parses them in place
  (`files.id` preserved) — the in-place UPDATE lost a parameter.

Then the rest of the cycle: one full-hash function (`package::xxh3_full_file`,
4 MiB reads; `duplicates::compute_full_xxhash` gone), sync banks every full
read into `strong_hash` (sender manifest, receiver confirm, ingest — via
`db::bank_strong_hash`, under `disk_matches_row`), and the scan never hashes
content any more: the content-index job is the one bulk producer, its autostart
gate is `sync_configured || use_content_hash`, and a USER-started scan clears
the cancel latch (the monitor cycle does not). Settings shows one "Content
index" card; the Folders rail has a "Build content index" button.

- Two-instance round trip: after a send lands, `SELECT COUNT(*) FROM files
  WHERE strong_hash IS NOT NULL` grew on BOTH sides by the number of files
  transferred; re-send the same selection — the receiver's log carries
  `dedup responder confirmed candidate full hashes` with `reused = N` and
  `banked = 0`, and no full reads happen.
- Content grouping ON, sync signed out: run a Rescan — the job card appears
  after the scan and the Duplicates view (content mode) shows the new files
  once it finishes. Cancel the job, Rescan again — it comes back; cancel,
  wait for a monitor cycle — it does not.
- A large re-scan on the NAS with content grouping ON is as fast as with it
  OFF (the scan reads headers only either way).
- Settings → General: a single "Content index" card; nothing on the page still
  claims scans hash files as they go. Folders → rail button shows the pending
  count, spins while indexing, reads "All files indexed" at zero.
- Web-backend parity: the web scan route re-arms the job the same way.

### Deep verify banks its reads, and survives a rules change — 2026-08-27

Two fixes in one pass. (1) The verify loop now carries a run-generation token:
changing the keep rules or refreshing the view mid-verify actually stops the
loop instead of hiding the UI while the disks kept grinding and a phantom
"done" summary resurfaced minutes later (reset also un-cancelled a pending
cancel). (2) `verify_duplicate_pair` (both backends) replaces the path-based
compare for the Duplicates view: an identical pair's full-content hash is
computed DURING the byte compare (zero extra I/O — one stream feeds xxh3, and
equal bytes share the digest) and banked into `files.strong_hash` for every
row still current on disk; the next verify of that pair is decided from the
stored hashes without reading either file. A mismatch still early-exits and
stores nothing. Staleness contract shared with the master-hash pass
(`disk_matches_row`); its unreachable-volume log dropped warn→debug.

- Start verify, change a keep rule mid-run: the progress UI disappears AND
  the logs go quiet (no further `verify_duplicate_pair` spans) — previously
  they streamed on for the whole queue.
- Verify a large selection twice: the second run finishes near-instantly
  (stored-hash path), and `SELECT COUNT(*) FROM files WHERE strong_hash IS
  NOT NULL` grows after the first run by roughly the verified-files count.
- Cancel mid-run still lands the `cancelled` summary with partial counts.
- A verified pair, then one file re-saved (mtime drift): the next verify of
  that pair falls back to reading bytes (stale row disqualifies the shortcut).

### Duplicate detection keyed on header identity — 2026-08-27

The cheap duplicate key stopped being `files.metadata_hash`
(`size + mtime + filename`, where mtime is a property of the copy) and became
`(fits_header.header_fingerprint, files.size, files.filename)` restricted to raw
sub-frames.
Spec: `specs/2026-08-27-duplicate-detection-design.md`. On the owner's
production catalog the view returned 0 groups while holding 2 750
(5 552 files, 170.5 GiB) across 33 calibration sets.

- Open the Duplicates view on the production catalog: ~2 750 groups appear,
  including the twenty pairs in calibration set 628.
- No group mixes two filters. Spot-check any `C_2022_E3_ZTF_Light_*` or
  `IC_2087_Light_*` name — those files repeat across filter folders with the
  same size and must NOT be grouped.
- Master groups contain only byte-identical files: `Pane_2_Sii.xisf` /
  `Pane_2_Ha.xisf` (identical headers, different filters) never group, and
  neither do the `masterDark_BIN-1_…` near-copies that differ by ~12 bytes of
  XML header. NB: on this catalog every sampled master pair proved
  byte-DIFFERENT, so ZERO master groups is a legitimate — and expected —
  outcome; the smoke is that no wrong group appears, not that groups do.
- After the scan, `SELECT COUNT(*) FROM files WHERE strong_hash IS NOT NULL`
  is ~61 (the header-shortlisted masters), not 381 and not 41 893. ~61 may read
  high: the shortlist query does not gate on the Black Hole or on
  `scan_roots.find_duplicates`, so it can hash a few masters the view would
  never offer. Order of magnitude is the check, not the exact number.
- A processed derivative is not offered as a copy of its source: `Lum.xisf` and
  `Lum_GraXpert.xisf` (same header, same size, different bytes) must NOT be a
  group, and `Find duplicate folders` must not score a folder of processed
  outputs as similar to the folder it came from.
- Run a scan: the post-scan rebuild fills `duplicate_groups` with
  `hash_type = 'header'` and the second open of the view is instant.
- Turn on content grouping with an empty content index: the view goes empty
  rather than erroring, and the Settings text points at the index.
- `Find duplicate folders` scores the two copies of a flats folder as similar.

### Membership counters follow deletions — 2026-08-27

`calibration_set.frame_count` and `sessions.frame_count`/`total_exp_time` were written
only where members are *added*; a deletion arrives by FK cascade, where there was no
call site to hook, so the counters froze at their creation-time value. Owner report:
sets 546/547 showing 20 with 10 members left, 628 showing 80 with 40. Fixed with
`AFTER INSERT`/`AFTER DELETE` triggers on both junction tables plus a startup resync
sweep in `init_db` (`db/schema.rs::create_membership_count_sync_triggers` /
`resync_membership_counts`). On the owner's production catalog the sweep corrects 868
calibration sets and 42 sessions — verified on a copy, not yet run in the app.

- Launch on the production catalog: Equipment page shows real member counts on
  546/547 (10) and 628 (40); the log line `membership counters resynced from junction
  tables` reports `calibration_sets=868 sessions=42`, and a second launch reports
  nothing (the sweep only rewrites rows that actually differ).
- Delete calibration frames through the Black Hole → void: the owning set's count
  drops immediately, and a set that loses its last member is still pruned.
- Object-set session rows show frame counts and integration times matching their
  members after a deletion.

### Code-debt cleanup — 2026-08-25

Dead enum variants deleted (`WarningType` ×3, `SkipReason` ×2, `StepStatus::RolledBack`
in both `file_op` and `archive` — git history confirms none was ever constructed, so no
DB row can hold the removed strings), `api/` rustfmt pass, and a **Rebuild master**
button in the provenance block of `CalibrationSetTable`.

- Rebuild a built master from the library UI: progress rides the raw set's building
  state, one "Master created" notification at the end, provenance block refreshes
  (new Created timestamp) without collapsing the row.
- The button is disabled with a "restore originals first" hint on a master whose
  originals are archived and gone from disk; Restore originals → Rebuild works.
- An imported master shows no Rebuild button at all (provenance block says
  "imported master").
- Export warnings panel still renders temperature/age/missing warnings after the
  `WarningType` trim.

### Big-catalog performance — committed 2026-08-22, authored 2026-08-11

Unindexed foreign-key child columns, `IN (…)` lists past SQLite's bind-parameter
ceiling, a WAL transaction that read before it wrote, ghost master sets, and a
Duplicates view that put every group in the DOM at once.

- ~~Remove a large scan root.~~ **Done 2026-08-22: 2.41 s, against 45.8 minutes before.** Three of the four defects sit on that one path.
- Duplicates screen on a catalog that mirrors one library across several drives — it opens instead of failing; "Show 500 more" grows the list; the Black Hole count covers every group, not just the visible ones.
- After removing a root, the Master Dark/Flat library and the calibration matcher hold no master whose file is gone (39 such ghosts survived the old delete).
- `PRAGMA integrity_check` after the removal, and masters still in use survived it.

### Content index — 2026-08-11

`content_hash` is built by one visible, cancellable background job gated on sync
being configured. *(Since the 2026-08-28 hash cleanup: the scan never hashes at
all — the opt-in scan-time path is gone — and the gate also opens when content
grouping is on; see that section for the re-arm rules.)*

- Scan the 18 946-file root signed **out** — seconds rather than ~40 s, `content_hash` stays NULL, no job card.
- Sign in and relaunch — the card appears, progresses, finishes with one notification, `pending` reaches 0.
- Press the card's X mid-pass, then let a **monitor cycle** scan — **it must not come back.** (A scan the user starts by hand DOES bring it back since 2026-08-28 — that is the hash-cleanup check, not this one.)
- A catalog holding an archived frame set — a permanent non-zero remainder that the card explains without inviting a pointless retry.
- Settings → "Build index now" while signed **out** — runs anyway (the manual path is ungated) and clears any suppression.
- Start a master build and "Build index now" together — they queue rather than run concurrently (`compute.max_concurrent` defaults to 1).
- Web-backend parity for both routes.

### Dev/prod data separation — 2026-08-09

Debug builds resolve a `.dev` app-data sibling; `npm run dev:db-refresh` snapshots the
production catalog into it.

- `npm run tauri dev` — About shows the `.dev` DB path, the app is signed out, and no sync-receiver startup events appear in log-mcp.
- The production app is untouched after a dev session.
- log-mcp shows both trees (needs a Claude session restart, since the server is `cargo run`).
- `ATHENAEUM_APP_DATA_DIR=<prod dir> npm run tauri dev` opens the production catalog.
- `dev:db-refresh` on a machine with a **real populated catalog**. **Caveat:** the production DB on the dev Mac is empty — 53 tables, `files = 0` — so every real-data check during that cycle was vacuous and the script's row semantics were proven only in a sandbox.

### CFA / sync / archive / web fixes — 2026-08-04

- Web archive — the widget auto-dismisses and the page reloads.
- The resume banner shows progress, and resuming an all-Done operation still retires the banner.
- Web sky-map rectangle select returns candidates (it returned 422 on every selection before).
- Two-device OSC sync — `bayerpat` is non-NULL on the receiver. Every synced frame previously landed with NULL CFA columns, silently disabling per-channel flat scaling on the peer.
- An existing catalog back-fills on first start — `query_logs` for `"cfa columns back-filled from stored headers"`.
- A cancelled resume reports `rolled_back`, not `failed`.

### Dead / duplicate code cleanup — 2026-08-04

- Rollback progress is visible from the archive resume banner.
- Calibration tables click-through: sort flip, the Bias table's reversed B, G, O order, hammer → "building…" pulse.
- Three queue indicators in the sidebar.

### DB hygiene and transaction discipline — 2026-08-03

Six Critical findings across the SQLite layer, including SQL injection in
`get_black_hole_files` reachable from the web backend. The five-item smoke list is in
`plans/2026-08-03-db-hygiene-hardening.md`; the audit is
`research/2026-08-03-db-layer-audit.md`.

Two facts from that audit are worth not re-deriving: foreign-key enforcement is on
everywhere, because the bundled `libsqlite3-sys` compiles with
`-DSQLITE_DEFAULT_FOREIGN_KEYS=1`; and CASCADE deletes do fire after `AFTER DELETE`
triggers, verified empirically.

### OSC / CFA hardening — 2026-08-03

- Real OSC subs → inspect the master's Bayer cards.
- Calibrate with per-channel flat scaling on versus off, compared visually.
- The mono-flat advisory appears.
- A rescan picks up an edited `BAYERPAT`. Rows written before the fix carry `Some("")` and self-heal on rescan.

### Calibration supersede-hardening — 2026-08-02

Consolidated list in the audit's "Post-cycle follow-ups"
(`research/2026-08-02-calibration-audit.md`). A Windows smoke is owed for the
filename-claim and rename path. Note that `LIGHT_CAL_ENGINE_VERSION` went 1 → 2, so
every previously calibrated light derives *stale* — intended, and a release note is
owed for it.

### Cross-platform path fixes — 2026-07-30

The compile gate cleared with the v0.5.1 tag (2026-08-24) — see the audit doc's
Status section. What remains is the manual smoke list:

- Restore-to-original, Windows, end to end.
- A move across a Docker bind mount (EXDEV), killed mid-copy, then resumed.
- Windows beta catalogs that ran a relink before the fix may hold `\\?\`-spelled `scan_roots` / `files` rows and need re-adding or relinking.
- `mt.exe` manifest validation, and a path longer than 260 characters.
- A UNC breadcrumb click.
- The new `delete_archive` failure message.
- A Linux archive plan over case-only-distinct roots is refused loudly.

### Folders screen redesign — 2026-07-30

The eleven-item checklist in `plans/2026-07-29-folders-screen-redesign.md`, plus three
from the minors fix wave: an unknown-kind row stays visible; a toggle failure raises a
dialog rather than a banner; the tinted checkbox renders. The artfrom.space guide still
describes the old tab name.

### Shipped in v0.5.0-beta.1, still unconfirmed

Transfers v2 and the batch model (U1–U8, two instances), D3 multi-source distribution
(three devices), transfer concurrency W1 + W2 (four scenarios), Perseus UI v2, the
Windows path fixes (five scenarios), and the mirror hierarchy (five). Checklists live
in the plans beside this file. The per-task `progress-*.md` execution ledgers are in
`.superpowers/sdd/`, which is gitignored — they exist only on the machine that ran the
cycle, so anything from them that matters later belongs here or in a plan.

---

## Awaiting a call from the owner

- **D1–D4 from the DB-layer audit.** Single process per catalog file (proposed: ratify); keep the pool-exhaustion panic (proposed: keep); executor per-stage connections (proposed: defer); same-volume rename reconcile (proposed: defer, since the scanner heals it in practice).
- **The archive banner wedge.** Banner buttons are disabled for as long as the resume/rollback widget lives, so a lost terminal event — a narrow listen-registration race, or a worker panic — wedges the banner until reload. No data is lost. Accept it, or add a bounded escape that re-enables the controls on a timeout.
- **Auto-link deselect semantics**, and whether a "manual block" concept is wanted.
- **`frames.is_master` without `is_master_library`** — what shape that takes in the archive.
- **Header fingerprint as the transfer-dedup Offer key** (hash-cleanup follow-up,
  2026-08-28, NOT started). The content index exists only to give the RECEIVER a
  cheap "do I have this?" membership key — the sender computes its sampling hash
  from the staged copy — and `fits_header.header_fingerprint` is already 100 %
  populated there at zero I/O. Carrying the fingerprint in the Offer (a new
  `Msg` variant; indices are frozen, append-only; Perseus already parses headers
  through `athenaeum-core`) would retire `content_hash`, the index job and the
  Settings card, with the full-hash confirm unchanged as the safety net. A
  mismatch (WCS written into one copy, parser drift between versions) costs a
  re-transfer, never a loss. A protocol cycle of its own; proposed: defer.

---

## Release notes owed at the next tag

(The v0.5.1–v0.5.4 lines were paid at their own tags.)

- Transfers: the sender's progress line now counts only the files that actually
  travel and says how many the receiver already had — "84 of 300 files · 262
  already on peer" instead of "346 of 562" — so both sides of a transfer show the
  same total and the progress bar no longer stalls at half when most of a set is
  already there.
- Transfers: sending returns instantly — the transfer appears right away as a
  *preparing* row with a live byte count, speed and a Cancel button while the
  files are staged in the background, instead of the dialog hanging until every
  file has been copied.
- Transfers: a transfer now costs one copy of its files on each machine instead
  of two — the sender serves the prepared package where it lies and the receiver
  links the downloaded files into place — so a 20 GB send no longer needs 40 GB
  of free space at either end.
- Settings has a new Transfers tab: the outgoing staging folder and the incoming
  working folder can each be pointed at any disk (with the upload limit,
  receiving limit and storage figures moved there too), and whatever the previous
  folders still hold can be cleaned up from the same page.
- Export: the Calibrated lights mode now calibrates lights on the spot from your
  built masters at export (or send) time instead of requiring a separate
  Calibrate Lights run first — the standalone flow, its dialog and its per-frame
  badge are gone.
- Export: Calibrated lights gained two toggles — hot-pixel correction (replaces
  known-defective pixels using the master dark) and full-resolution VNG debayer
  for one-shot-color cameras — both on by default.
- Collaboration: publishing your own lights to a project is temporarily disabled
  while the calibrated-export changes above are worked into it; receiving other
  members' contributions is unaffected.
- Known consequence: resending a Calibrated-lights transfer after re-calibrating
  (e.g. after rebuilding a master) now lands a second copy on the receiver
  instead of replacing the first one — there is no tracking table left to dedup
  against.
- Analysis tab: every column with data now sorts — WCS and Reference included.
  The WCS column reads Header / ATH / — instead of icon badges, and the
  Reference column is just the star: filled on the chosen frame, a star
  button on the rest.
- Export: the folder tree, file total and size estimate follow the selected
  export mode — no calibration folders in Lights only, `c_*` names for
  Calibrated lights, one file per master set — and a remembered mode that is
  not available for the current set no longer stays selected. A set with no
  calibration linked at all now offers only Lights only (the two raw modes
  would have landed the same files under a different name), Lights only shows
  no calibration warnings, and a missing-calibration warning names the camera
  when two groups share a filter. The size estimate now uses the set's real
  average file size instead of a fixed 50 MB per file.
- Calendar: a day's cards show the camera, the telescope and the first–last
  exposure time of that night.
- Plate solving no longer invents astrometry for frames whose stars are
  streaks. Wind-shaken frames used to come back "solved" at 16-193x their
  true pixel scale — a confident, entirely wrong position written into the
  catalog. Such detections are now excluded from matching, and any solve whose
  scale disagrees with the frame's own focal length and pixel size is refused.
- Plate solving: a frame whose analysis shows badly trailed stars is skipped
  with a plain reason instead of spending minutes failing, and the thresholds
  are in Settings if you want them looser. A frame that turns out to be
  nothing but streaks is refused within a second — naming how many of its
  detections were streaks — rather than searching for minutes.
- Plate solving: a frame with no coordinates in its header can be solved by
  naming its target — set OBJECT in the metadata editor and the editor tells
  you, as you type, whether the name is one the sky catalog knows.
- Calibration: the manual assignment list is usable again. Every candidate
  now carries a real closeness percentage instead of a flat 0 % for anything
  your matching rules refuse, the list is ordered by how near a miss each one
  is — same camera first, then by what each broken rule costs the calibration
  — and every card states why a set was refused ("Temperature: 19.4 vs -9.9 —
  off by 29.3, limit 5.0", "Gain: this set does not declare one") instead of
  showing an unexplained score.
- Calibration: "show only compatible" now means exactly that. It used to hide
  everything whenever no candidate was perfect, and it hid compatible-but-old
  sets as well.
- Calibration: choosing calibration by hand is one screen now, for light
  frames and for a calibration set's own sub-calibration alike. The camera,
  exposure and date filters are always visible, clicking a value in the left
  panel filters the list by it, and each card names the difference that
  matters — "Offset 30 → 200" — instead of listing parameters you had to
  compare yourself.
- Master libraries: a master with no GAIN or OFFSET in its header is flagged
  in the Dark/Flat library — such a set can never be matched automatically,
  and Edit Metadata fills the values in.
- Objects and Calendar: a night that runs past midnight is one night again.
  The night tree grouped by the frame's calendar date, so every session
  through midnight showed as two — "October 18" and "October 19" instead of
  "October 18–19, 2025" — and the Shoot Calendar split the same night across
  two day cells. Both now group by the imaging night, which lands on the day
  it started.
- Objects: merging frame sets (and Find new images) now recomputes the
  merged set's nights from all of its frames instead of stitching the two
  sets' night rows together — a night split by a meridian-flip re-pointing no
  longer shows up as two nights — and a new **Recalculate nights** button on
  the object page repairs sets that were merged before this release.
