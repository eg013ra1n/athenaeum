# Open items

What is finished in code but not yet confirmed by hand, and what has already been
decided and should not be reopened. Everything here sits on `main` — the development
trunk since 2026-08-24, when the project went open source and version branches were
retired — and covers the ten completed cycles on top of the last tag.

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

Newest first. Every cycle below is code-complete with green gates and a clean final
review; what is missing is a human running the flow on real data.

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

- Duplicate detection now recognises copies whose timestamps changed in
  transit — moving a night between drives no longer hides its duplicates.
- Deep verify remembers its work: files proven byte-identical keep their
  full-content hash, so re-verifying them is instant — and changing the keep
  rules mid-verify now actually stops the run.
  Masters and processed files are compared by their full contents, so two
  different stacks that share a header are never mistaken for copies.
- Rebuild master from the library: masters built in Athenaeum can be re-integrated
  in place from their original source frames (Equipment → expanded master row).
- Calibration sets and sessions no longer keep counting frames that were deleted;
  existing catalogs are corrected once on startup.
- The catalog drops a duplicate-detection column it no longer uses; existing
  catalogs are migrated once on startup.
- Transfers remember the files they hash: every full read a send, a receive
  or a deep verify pays is kept, so masters that travelled between devices
  show up in duplicate detection without another read, and re-sending files
  a device already confirmed costs no disk I/O on the receiver.
- Scans read headers only. The content index that powers transfer dedup and
  content-based duplicate grouping is built by its own background job — after
  every scan when sync is set up or content grouping is on, and from a new
  "Build content index" button on the Folders page. Settings gained one
  "Content index" card in place of two.

(v0.5.1 lines were paid in full on 2026-08-24.)
