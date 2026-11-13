# Stacking file identity without the modification time: design

Date: 2026-11-12. Status: implemented and accepted on 2026-11-13 (acceptance: `docs/stacking/README.md`); FI-11 added on 2026-11-13. Approved by the owner on 2026-11-12 («одобряю спеку, перепривязку включай»). This spec starts from `docs/superpowers/research/2026-11-08-stacking-file-identity-without-mtime.md`. It changes the cache of `docs/superpowers/specs/2026-11-08-stacking-frame-level-cache-design.md`, including that cache's ruling R7 b. The rulings are FI-1 to FI-11 (§2).

Code paths are under `crates/athenaeum-core/src/` unless they start with `docs/`. Line numbers are taken from `main` at 484fa75f.

## 1. Problem

The owner, 2026-11-08: «я считаю что это неправильно завязываться на время изменения, есть хеш, файлы не меняются если изменилось лишь время изменения».

The stacking cache treats a change of modification time as a change of content. It compares the time as an RFC 3339 string with nanoseconds. On some volumes a copy that keeps the time does not keep that string. The probe of 2026-11-08 (research note) wrote a file, then copied it with `cp -p` semantics:

- APFS to APFS: `…47.917852109` stays `…47.917852109`.
- APFS to exFAT: becomes `…47.910` (10 ms steps). The owner's Fotobase drive shows whole seconds.
- APFS to SMB (Universe): becomes `…47.917852100` (100 ns steps).

Each such copy reads as a different file. The bytes are equal, but Calibrate, Measure, Register, LN and Integrate all recompute; on set 270, pass 1 alone takes 24 to 34 min. A file written in place kept its time on all three volumes.

The change: a file's identity is its size plus a content sample, never its time.

## 2. Rulings

| Id | Date | Ruling |
| ---- | ---- | ---- |
| FI-1 | 2026-11-08 | No full-file hash: «пересчет полного хеша опять хрень какая-то, давай половинчатые (как при скане) этого достаточно». The content identity is the sampled hash `files.content_hash` (`duplicates::compute_xxhash`, `duplicates/mod.rs:15-57`: the first, middle and last 512 KiB; a file under 1.5 MiB is read whole). |
| FI-2 | 2026-11-08 | Athenaeum's own working files need no content check: «никто не будет менять временные файлы в здравом уме». |
| FI-3 | 2026-11-12, «да на все три» | (a) A moved master recomputes nothing: the path leaves the master's key. |
| FI-4 | 2026-11-12, «да на все три» | (b) No migration: every existing set recomputes once after the update. |
| FI-5 | 2026-11-12, «да на все три» | (c) It ships in the same release as the frame-level cache. |
| FI-6 | 2026-11-12, «ок» | The reference's measurement is folded into the registration key for SAME-group mono frames too. Merging or splitting a group then does not re-register frames whose reference did not change. The price is one re-registration of every mono set, which FI-4 already pays. This closes the owner call in the open-items line "Finding, what a regroup still re-registers". |
| FI-7 | 2026-11-12, with the approval | The gate may read Calibrate stale after a scan re-parse until a run samples the inputs again; the run then reuses everything. The gate reads no file (§11, point 1). |
| FI-8 | 2026-11-12, with the approval | No full read of a master rewritten in place with its header kept; the risk of §6 is accepted (§11, point 2). |
| FI-9 | 2026-11-12, «перепривязку включай» | A working folder that now lives at another path reuses its cache: the path rebase is part of this cycle (§4.10; §11, point 3). |
| FI-10 | 2026-11-13, «Ок, начинай» (the plan's approval) | RB-1 of §4.10 stands: a copied or moved working folder is reused only when its files kept their times within 2 s. A content sample per working file is a follow-up only if a tool the owner uses drops times. |
| FI-11 | 2026-11-13, «Включай» | A file Athenaeum wrote under a working folder is the row's file when the key is equal, the size is equal AND the file's modification time is within 2 s of the row's `modified_at`, both at full precision. It amends FI-2 (key + size alone) and is the rule FI-10 already uses for a moved folder. A row that names the frame's INPUT in place (a file Athenaeum did not write) keeps key + size and no time check. |

The "accept ±2 s" remedy stays dropped (open-items, backlog line).

## 3. Today

These are every place where a modification time decides something in stacking or next to it.

| # | Site | What it compares | Kind of file | What the time protects against |
| ---- | ---- | ---- | ---- | ---- |
| 1 | `master_identity`, `stacking/plan.rs:410-428` | `<path>\|<size>\|<mtime>` from a live stat, folded into the stage-1 upstream | catalog master on disk | a master rebuilt or replaced in place |
| 2 | `master_identity_in`, `stacking/plan.rs:434-454` | `<path>\|<size>\|zip:<crc32>`; without a CRC, the row's `modified_at` (`:445`) | master inside a zip | a rewritten zip |
| 3 | `calibration_hash_for`, `stacking/plan.rs:505-533` (sources `:522-526`) | `SourceIdentity { file_id, size, modified_at }` from the `files` row (`stacking/groups.rs:324`), no stat | raw light (disk or zip) | a light the scanner saw change |
| 4 | `prepare_hash_for_catalog`, `stacking/plan.rs:459-469` | the same | attested light | the same |
| 5 | `prepare_hash_for_project`, `stacking/plan.rs:475-489` | blake3 + size; the sources entry carries `modified_at: ""` | project replica | nothing (no time). Listed because its bytes must not move |
| 6 | `file_matches_identity` via `is_fresh`, `stacking/plan.rs:1057-1098`, `:1119-1124` | live size and the RFC 3339 string equal the row's (written by `file_identity`, `stacking/run/mod.rs:615-622`) | own `calibrated/`, `prepared/`, `measured/`, `fits/`, `registered/`, `ln/`. Also the in-place `calibrated` row of an attested light or a project replica, which names the user's own file (`stacking/run/prepare.rs:332-345`) | R7 b, a cancelled rewrite (§4.2); an outside edit of a foreign file used in place |
| 7 | Blink `replaced`, `api/stacking_aligned.rs:240-245` | #6, for a row that has a `modified_at` | own `registered/` | showing pixels that a cancelled later run wrote |
| 8 | `check_stamp_len` under `strict_mtime`, `stacking/measure.rs:2135-2147`; set at `stacking/run/measure.rs:1326` by `owns_calibrated_file` (`:1147-1149`) | the `.athm` head's source time against the calibrated file now | attested light or project replica used in place | an outside edit under a cached pass 1 |
| 9 | `ReadGuard`, `stacking/measure.rs:1581-1640` | size + `SystemTime` between the two plane reads of one measurement | anything Measure reads | a rewrite inside one measurement |
| 10 | `landed_unchanged`, `stacking/origin.rs:172-202` (stamp: `stacking/project_source.rs:333`) | size + whole-second time against the landing stamp, 2 s tolerance (`collab/storage/sweep.rs:10`, `:52`) | project replica | the exchange replacing a file during a run |
| 11 | master-light preview cache, `api/stacking.rs:1287-1320` | `cached_at >= master_mtime` | the run's output master (not the frame cache) | a master rebuilt in place |
| 12 | `disk_matches_row`, `duplicates/backfill.rs:353-364` | the scanner's `(size, RFC 3339)` | every catalog file | the scanner's contract; reused below as a trigger |

What stays as it is:

- **#9 and #10 stay.** `ReadGuard` works within one measurement, on one mount, with no rounding. `landed_unchanged` belongs to collab storage, and its 2 s tolerance covers FAT and SMB rounding.
- **#11 stays.** Rounding moves both times in the same direction, so the `>=` comparison survives a copy.
- **#12 stays** as the scanner's contract (out of scope, §7).

Corrections to the research note, from the code:

- **A zip master's token holds the path today:** `<path>|<size>|zip:<crc>` (`stacking/plan.rs:446`). It also falls back to the entry's time when no CRC was recorded (`:445`).
- **A relocate changes only the path.** Missing files' relocate updates `path` and `filename` and nothing else (`api/missing_files.rs:263-266`); `modified_at` and `content_hash` keep the old values. The scanner's re-parse writes the new time and also NULLs `content_hash` and `strong_hash` (`scanner/mod.rs:1384-1396`). A rescan under another root makes a new row: a new `file_id` and a new frame.
- **`file_matches_identity` also judges foreign files** (#6, in-place rows), not only Athenaeum's own.
- **`content_hash` already has a second producer.** Zip extraction banks it (`zip_store/materialize.rs:360-366`). The catalog README's line 113 ("its ONE producer is the content-index job") is wrong today.
- **The project stage-1 hash serializes an empty `modified_at`** (`stacking/plan.rs:483-487`). Reshaping `SourceIdentity` would move every project's hash unless the old bytes are kept (§5).
- **Own files also include `fits/` (`.athf`) and `ln/`.** The `ln` sidecars and `ln_reference` are read through `is_fresh` (`stacking/run/normalize.rs:513`, `:751`, `:881`, `:985`; `stacking/run/reweigh.rs:99`).
- **Lights can live inside a zip.** They have no file to sample.
- **A FITS master carries no build date** (§6).

## 4. Design

### 4.1 Own files: key + size + a time within 2 s (FI-2, amended by FI-11)

- Signature: `file_matches_identity(path: &Path, size: Option<i64>, modified_at: Option<&str>) -> bool` (`stacking/plan.rs`). It is one stat: the length equals the row's size AND the file's time is within 2 s of the row's `modified_at`, both at full precision (`within_tolerance`, the function the rebase of §4.10 uses; `MTIME_TOLERANCE_SECS`). `NotFound` is the ordinary stale. Any other stat error stays a `warn!`. A row without a size, without a time, or with a time that does not parse never matches ("we do not know" is stale). A file whose time cannot be read is a `warn!` and does not match. A size that matches with a time outside the 2 s is a `debug!` ("cached file has the recorded size but another modification time; counted as changed", fields `path`, `recorded`, `current`).
- `is_fresh` passes the row's `modified_at`: the hash is equal, the row has a path, and the file matches.
- `file_identity` writes `modified_at` into the artifact row after the file's last write and rename; every writer does. No schema change.
- Blink (`api/stacking_aligned.rs`): `Replaced` when the row recorded a size and `!file_matches_identity(path, size, modified_at)`. A row without a size keeps the existence check.
- **Why the time came back (FI-11).** The dev build and the release build share one working folder on purpose and keep two catalogs. A writer retires only its own catalog's rows, so the other catalog's row, at its own key and the same size, read fresh over a file the first build rewrote under another config. The 2 s window costs nothing on the owner's disks: a copy that keeps its times moves a file's time by at most 9.57 ms (exFAT) and 62 ns (the SMB share).
- This covers `calibrated`, `calibrated_mosaic`, `measured`, `fits.<plane>`, `registered`, `ln` and `ln_reference`, for the gate and the run alike.
- An in-place foreign `calibrated` row (#6) is judged by key + size, with NO time check (`is_fresh_in_place`): the file is not Athenaeum's, its identity is in the key (size + content sample, §4.3), and a light copied with another time must recompute nothing. A row under one of the subject's own `prepared/` or `calibrated/` folders is an own file and gets the time.
- **An in-place row must name the frame's current input.** The key vouches for the file at `files.path` (or the replica's landed path), not for the row's path. After a relocate the row still names the old path (`stacking/run/prepare.rs:339`), and another file of the same size there (a capture program that restarts its counter writes `Light_0001.fits` again) would read fresh by key + size. So an in-place row is fresh only when its path equals the frame's current input path (`origin::prepare_input`, `stacking/origin.rs:79-91`), in the gate and in the run. A row that fails this is stale: the run uses the file in place again, with no write, and re-points the row; Measure and later stay fresh because their keys did not move.

### 4.2 The write order that replaces the time (R7 b)

**The hole.** Every frame-level kind has one row and one path per frame, whatever the config (`frame_file_stem`, `stacking/run/frames.rs:19-25`). Calibrate's commit loop returns `Cancelled` at the first unfinished job (`stacking/run/calibrate.rs:198-202`), so the files that later jobs of that fan-out already wrote get no row. The old row, with the old hash and the same size, stays over a new file. While the time was compared, that mismatch was caught. Without the time, a later run at the old config would read the new file as fresh.

**The new order, at every site that replaces a file:**

1. read the old row;
2. delete it, with `db::stacking::delete_artifact(conn, subject, group_key, kind, frame_id)` (new: a point delete on the same six-expression key as `find_artifact`, `db/stacking.rs:1034-1057`);
3. replace the file;
4. upsert the new row;
5. `replace_frame_level_file(old)`.

For a fan-out writer, steps 1 and 2 run on the run thread when the job is queued, because a worker holds no connection (`stacking/run/register.rs:2144-2147`). A cancel then leaves no row for any queued job, and every queued job was a miss anyway. No extra stat, no extra read.

| Kind | File written by | Old row read today | Deleted at |
| ---- | ---- | ---- | ---- |
| `calibrated`, `calibrated_mosaic` (`calibrated/`) | fan-out worker (tmp + rename) | `resolve_calibrate_job`, `stacking/run/calibrate.rs:262-284`, carried in the job (`:438-439`) | the same resolve, when it returns a job (before `fan_out`, `:498`) |
| the same under `prepared/`, or in place | fan-out worker; no write when used in place | `resolve_prepare_job`, carried (`stacking/run/prepare.rs:250-251`) | the same resolve |
| `measured` (`.athm`) | pass-1 worker (`stacking/run/measure.rs:1236`); pass-2 worker fallback | `upsert_pass1_row`, `:1837`, after the write | `measure_two_pass_prepare` (`:1306`), for every member owed pass 1 |
| `fits.<plane>` (`.athf`) | run thread, `write_fits` (`stacking/run/measure.rs:1954`) | `:1990`, after the write | just before `write_fits` |
| `registered` | fan-out worker (`write_one_registered_frame`, `stacking/run/register.rs:2102`) | `record_registered_artifact`, `:2158`, after | when queued (`:905`, `:967`), before the fan-outs at `:1090`, `:1243` |
| `ln` (`.athln`) | fan-out worker (`stacking/run/normalize.rs:1138`) | none | before that fan-out, for each member in `needs_normalize` |
| `ln_reference` | run thread (`stacking/run/normalize.rs:1836-1870`) | none | before the map is written |

Notes on the table:

- **Payload-only rows name no file and are unchanged:** `metrics`, `registration`, `solve`, `ln_calibration`, `ln_global`, `two_pass`.
- **`replace_frame_level_file` (`stacking/run/frames.rs:36-76`) works as before.** It is given the old row its caller read, and once that row is deleted it no longer counts as "a row still naming the old file".
- **The one write without a delete:** pass 2's in-worker pass-1 fallback after a refused read. It writes under the SAME key, at `layout_spill`, so it produces equal content or uses a different path, and there is no hole.
- **Pass 1 rows are upserted before a cancel is honoured** (`stacking/run/measure.rs:1508`), so pass 1 is exposed only to a kill. The same rule applies anyway.
- **The C-30 comment in `persist_measurement` changes** ("an existing row is left alone"): a failed `.athf` write now leaves no row, and the consumers fall back as they do for a missing row.
- **No cleanup race.** The `measured/` and `fits/` sweep skips while a run is active.
- **Blink.** A frame whose row a cancelled later run deleted reads `missing`, or `replaced` by the group rule. It never offers that run's pixels.

### 4.3 Foreign inputs: size + sample (FI-1)

| Kind | Token in the stage-1 hash |
| ---- | ---- |
| raw or attested light on disk | `SourceIdentity::Sampled { file_id, size, sample }` |
| catalog master on disk | `<size>\|sample:<sample>` (no path, FI-3) |
| light or master inside a zip | `<size>\|zip:<crc32 as 08x>`; with no CRC, `<size>\|zip:<entry modified_at>` (a time stored inside the zip, which a copy of the zip keeps) |
| project replica | unchanged: blake3 + size, in the old bytes |

**A trustworthy row (the run).** The row's `content_hash` is set, and `disk_matches_row` holds on one stat: the scanner's size and RFC 3339 time equal the file's now.

**The gate** reads no file and trusts the catalog (§4.4).

### 4.4 The helper: one rule for the gate and the run

New module `stacking/source_identity.rs`:

```rust
pub(crate) enum SampleRead { CatalogOnly /* gate: no stat, no read */, Disk /* run */ }
pub(crate) struct InputRow { file_id: i64, path: PathBuf, size: i64, modified_at: String,
                             content_hash: Option<String>, zip_entry: Option<ZipEntryStamp> }
pub(crate) enum InputIdentity { Sampled { size: i64, sample: String },
                                Zip { size: i64, token: String }, NotSampled(&'static str) }
pub(crate) struct Sampling { identity: InputIdentity, bank: Option<BankSample> }
pub(crate) fn input_identity(row: &InputRow, read: SampleRead, cancel: &AtomicBool) -> Sampling
```

`InputIdentity::master_token()` and `InputIdentity::light_source(file_id)` render the tokens of §4.3.

**`Disk` mode, in order:**

1. A zip row becomes `Zip`, with no stat.
2. Stat the file. `NotFound` gives the `CatalogOnly` answer: an offline input keeps its last vouched sample. Any other error gives `NotSampled`, with a `warn!`.
3. If the row is trustworthy, use `Sampled { row.size, content_hash }`.
4. If this session's memo holds `(path, live size, live time)`, use that sample.
5. Otherwise run `compute_xxhash` and stat again.
   - If the size or the time moved during the read: `NotSampled("changed while it was sampled")`.
   - Otherwise: `Sampled { live size, sample }`. Insert it into the memo. Set `bank` when the row matched at both stats and its `content_hash` is NULL.

**`CatalogOnly` mode, in order:**

1. A zip row becomes `Zip`.
2. If the memo holds `(path, row size, row time)`, use that sample.
3. If `content_hash` is set, use `Sampled { row.size, content_hash }`.
4. Otherwise `NotSampled("not sampled yet")`.

**The memo** is process-wide: a `static`, bounded at 65 536 entries and cleared when full, never persisted. An entry holds the path, the row's size and time, the live size and time, and the sample. It is keyed on the path and stamps, never on `file_id` alone, because tests open several catalogs in one process.

**Why the two modes derive the same key.** They agree whenever the catalog row still describes the file, or a run sampled the file in this session. They differ only for a file that changed after the catalog last saw it and before any run sampled it. Then the gate presumes the catalog and can say cached, while the run reads the file and recomputes that frame. That is never a false hit in the run (§7).

**The book: `SourceSamples`, a map from path to `InputIdentity` for one plan or one run.**

- **Gate:** `SourceSamples::catalog_only()` resolves each input lazily under the gate's connection: one `files` query per input, no file I/O. The gate keeps its "DB + cheap FS probes" contract.
- **Run:** `SourceSamples::resolve(ctx, inputs, cancel, progress)` works in three phases:
  1. read the rows under the connection;
  2. stat and read without it (the rule that no connection is held across slow I/O, as the content index does, `duplicates/backfill.rs:231`);
  3. bank under the connection, in one transaction.
  - `inputs` is every catalog light of `rc.plan_groups`, plus every master that `applied_master_paths` names for a non-attested one.
  - It runs once per run, at the top of `stage_calibrate_with` (`stacking/run/calibrate.rs:1004-1008`, before the Prepare dispatch). That is after `stage_masters`, so a master the run rebuilt has already been resynced (`scanner/mod.rs:1350-1356`) and is sampled as rebuilt.
  - It is idempotent, so a test that starts at a later stage still gets a book.
  - Progress: the message "checking the inputs" on the Calibrate row. Cancel is checked per file.
- **Use in the hash.** The book goes in through `HashMemo::new(origins, samples)`, and `calibration_hash_for` and `prepare_hash_for_catalog` take their tokens from it.
  - A `NotSampled` input is the frame's stage-1 error. In the gate it means "stale", logged at `debug!`, not `HashMemo`'s current `warn!` (`stacking/plan.rs:597-601`). In the run the frame is excluded, with the reason.
  - A run lookup outside the book is an error, never a silent read under the connection.
- **No API change.** No command and no TS type changes; `GroupFrame` keeps `modifiedAt`.

### 4.5 Banking: `content_hash` gets a second producer

New `db::bank_content_hash(conn, file_id, size, modified_at, hash) -> Result<bool>`, in `db/operations.rs` next to `bank_strong_hash` (`:7606`):

```sql
UPDATE files SET content_hash = ?4
 WHERE id = ?1 AND size = ?2 AND modified_at = ?3
   AND content_hash IS NULL AND archive_zip_path IS NULL
```

It is called only for a `Sampling` with `bank` set.

**Yes: the row must match the disk, with size and time as the scanner sees them.** The rest of the row (the header snapshot, the frame, `strong_hash`) describes the bytes the scanner parsed, and readers take `content_hash` to mean "these bytes". A wrong value would do damage in three places:

- Duplicates in content-hash mode groups by size + `content_hash` and offers to delete the others, so a unique file could be deleted.
- Sync dedup would answer "already on peer", and the file would never be sent.
- Our Move/ZIP checks an extraction against the sampled hash, so a correct move would fail.

**The guard is stricter than the content index's.** The content index checks before the read (`duplicates/backfill.rs:255`), and its UPDATE (`:276`) does not check again. This guard checks before the read, after it, and in the UPDATE's `WHERE`. If a scan re-parse lands between the read and the UPDATE, it changes `modified_at` and nothing is written. A sample that cannot be banked lives only in the memo.

**Catalog README.** Line 36 becomes: "`content_hash` has ONE bulk producer — the content-index job (…); stacking banks the samples of the lights and masters a run reads, and an extraction banks a zip entry's, both under the `disk_matches_row` contract; never the scan itself". Line 113 becomes: "`files.content_hash` (opt-in sampled; bulk producer the content-index job, also banked by a stacking run's inputs and by zip extraction; never the scan)".

### 4.6 Masters without the path (FI-3)

**Token:** `<size>|sample:<sample>`. A zip master's is `<size>|zip:<crc32>` (§4.3).

**What "moved" means for a catalog row:**

1. **Relocated through Missing files.** The row and its id stay, the path is new and the time is the old one. Until a rescan, the run reads 1.5 MiB once per app session because the time no longer matches. The token is equal, so nothing recomputes.
2. **Rescanned under another root and relinked.** This is a new row with a new id. The token holds neither path nor id, so it is equal and nothing recomputes.

**A master the app rebuilds in place** is resynced, which NULLs `content_hash`, so the run samples it again. A rebuild that changes the bytes in any window recomputes, as today's size and time did.

The `masterFiles` blocker is unchanged (`stacking/plan.rs:2679`). The docs to update are `stacking/run/masters.rs:35-42` and `stacking/plan.rs:402-454`.

### 4.7 Pass 1: `strict_mtime` goes

**What is removed:** `MeasureMember.strict_mtime`, `owns_calibrated_file`, and the `strict_mtime` parameter of `check_pass1_head`, `read_pass1`, `FramePass1::from_record` and `check_stamp_len`. The stamp compares the length of every calibrated file, and nothing else.

**What covers an in-place foreign file instead:**

- **An attested light:** pass 1's key is `pass1_hash_for(cfg, prepare hash)`, and the prepare hash now folds the light's sample. An edit that changes a window or the size moves the key.
- **A project replica:** blake3 is in the key, and `landed_unchanged` excludes a replica that was edited in place.

The `.athm` format still stores `modified_ns`. Nothing reads it, so there is no `PASS1_ARTIFACT_VERSION` bump.

### 4.8 The reference's measurement in every frame's key (FI-6)

The code involved:

- `registration_hash_for`, `stacking/plan.rs:811-833`
- `registration_measurement_fold`, `stacking/plan.rs:1260-1272`
- `reference_measurement_fold`, `stacking/plan.rs:1278-1297`
- `reference_measurement_term` and `member_registration_hash`, `stacking/run/register.rs:605-668`
- the gate, `stacking/plan.rs:2032-2057`
- the two-pass key, `stacking/run/two_pass.rs:207`

What the key folds:

| Term | Today, frame in the reference's group | Today, frame in another group | New, any group |
| ---- | ---- | ---- | ---- |
| `ref:<id>`, the reference's calibration hash, the frame's calibration hash | yes | yes | yes |
| `measure:<frame measurement at its group's β>` | when the frame's group reads `fits.0` | the same | the same |
| `ref_measure:<reference measurement at its group's β>` | no | when the reference's group reads `fits.0` | when the reference's group reads `fits.0` |

**The change.**

- The `reference_group.key == frame_group.key` branch (`stacking/plan.rs:1286`) goes, and so does the `frame_group` parameter.
- `reference_measurement_term` then no longer depends on the group, so it is resolved once per pass.
- A debayered-OSC reference still folds nothing, so set 270 is unaffected.

**Result.** A mono merge or split keeps every term when the reference stays (Manual, or Auto picking the same frame) and both groups' β values stay.

**What stays unstable by design:**

1. **An Auto reference re-picked from the new weights** re-registers every frame through `ref:<id>`. The gate reads the last run's reference (`stacking/plan.rs:1951-1956`), so it under-reports. That under-report is unchanged.
2. **`reference.twoPass`:** the `two_pass` key folds the membership, so a regroup is a miss. The dry pass runs once and persists nothing; the real pass is all hits.
3. **A β that moves with the membership:** a merged β is the lower median, so `measure:`, `ref_measure:` and pass 2 move with it. Set 185: L 3.49 → 3.30.
4. **Native geometry:** each group has its own reference.

The gate's over-report in the regroup finding's item (3) goes away.

### 4.9 Cost, and what is left to the time

- **Read:** 1.5 MiB per input. On the owner's SMB share that is about 0.13 s at 11.7 MB/s, about 40 s for 300 frames (research note), and about 74 s for set 270's 551 lights.
- **Paid on the run thread before Calibrate, for an input:**
  - (a) **whose row has no `content_hash`.** On the first run after the update that is every light and master, unless the content index ran; it autostarts only with sync or content grouping on (`api/content_index.rs:331-361`).
  - (b) **after a scan re-parse,** which NULLs `content_hash`: once, then banked.
  - (c) **whose row does not match its file** (relocated, not rescanned): once per app session, not banked.
- **Never paid:** in the gate, for a zip entry, for a project frame, for an own file.
- **Stat:** one per light and master per run. Today the run does not stat lights.
- **The time keeps two jobs.** It sits inside `disk_matches_row`, where it decides whether a banked sample still belongs to the file, and it keys the memo, where it decides whether to read again. It is never a verdict on a SAMPLE or on an input: a moved time with an equal sample is a hit. The one verdict the time gives is on an own working file (FI-11): there it is part of freshness, size + a time within 2 s of the row's.

### 4.10 A working folder at another path (FI-9)

Added after the approval, from the design note of 2026-11-12. Line numbers in this section are of `main` at 12ada293. Its three calls (RB-1 to RB-3, at the end of the section) were confirmed by the owner with the plan on 2026-11-13 (FI-10).

**The rule.** A cached file that moved with its working folder is reused at the new place. Rows keep absolute paths. The run points a row at the subject's current working root when its file is there as recorded. Nothing on disk is moved, copied or deleted.

**The old places** are the working folders the subject's runs recorded (`stacking_runs.working_dir`, in the run's spelling), each laid out with the subject's current slug (`paths::subject_layout`). A recorded folder that is the current one in another spelling (equal after `canonicalize` + `normalize_path`, when both exist) is not an old place.

**A row is rebased when all of these hold:**
1. its path lies under an old subject root, by `Path` components (`/w/M31` never matches `/w/M310/…`); where two old roots match, the longer wins;
2. the first component below that root is the kind's own subtree, the R4 roots: `calibrated` or `prepared` for `calibrated` and `calibrated_mosaic`; `measured`; `fits` for `fits.<plane>`; `registered`; `ln` for `ln` and `ln_reference`;
3. `<current root>/<same tail>` is a regular file, its length equals the row's `size`, and its time is within 2 s of the row's `modified_at` (`collab::storage::sweep::Stamp::matches`, `collab/storage/sweep.rs:10`, `:52-57`, the landing check's rule). A row without `size` or `modified_at` is never rebased;
4. the UPDATE still finds the row at its old path.

**Why the time.** Across two copies nothing but the time tells which file is the row's own write. A frame recomputed in copy 1 keeps its size (calibrated and registered files usually do, whatever the settings), so copy 2's older file would pass key + size. The time says "the same write, copied"; exFAT, FAT and SMB rounding stay within 2 s. Since FI-11 the time is compared in both places, by one function: the freshness check of §4.1 compares the file at the row's path, and the rebase compares the file at the MAPPED path (the current root plus the same tail) before it re-points the row. After the rebase the row is judged by key + size + time like any other (§4.1), which the copy passes because it was judged by the same time.

**Triggers.**
- **The run**, once, at the top of `stage_calibrate_with` (`stacking/run/calibrate.rs:1131`), before the Prepare dispatch and before the book of §4.4. It uses the book's three phases: read the old roots and the candidates under the connection; stat without it, with cancel checked per file; commit one transaction of point updates. It is idempotent: a second call finds no candidate.
- **The gate** writes nothing. `build_plan` builds a `RebaseView` (phases 1 and 2, under its connection) after the folders gate (`stacking/plan.rs:3384-3398`) and judges each candidate at its mapped path. One function decides for both, so they agree unless a file changes in between; the run checks again.
- **Saving the setting** does nothing. A folder copied after the setting changed, or moved while the app was closed, is found by the next plan or run.
- **The steady state adds no stat.** The candidate query takes only rows outside the current root, with the byte-range predicate of `rename_files_path_prefix` (`db/operations.rs:4298-4311`).

**Never touched.**
- A file at the old place. `replace_frame_level_file` removes only under the current owned root (`stacking/run/frames.rs:46`); cleanup and the sweep walk only the current layout. A row left at the old place keeps its file there.
- In-place rows. The light or replica they name is outside the owned subtrees (rule 2), even when it lies inside the working root (`converting_an_in_place_frame_keeps_its_landed_file_inside_the_working_folder`, `stacking/run/prepare_tests.rs:288`).
- Payload-only rows. The columns `config_hash`, `size`, `modified_at`, `payload_json` and `created_at` (Blink's `replaced` rule compares `created_at`, `api/stacking_aligned.rs:216-221`). The tables `stacking_runs`, `stacking_run_frames`, `master_lights` and `files`, and the settings. Any other subject's rows, even under a shared slug root.

**A row not rebased** keeps its path. It is read at the old place while its file matches there; otherwise it is stale.

**Code.** New module `stacking/rebase.rs`:

```rust
pub(crate) struct Candidate { row_id: i64, old_path: String, mapped: PathBuf, size: i64, modified_at: String }
pub(crate) enum Check { Moved, Missing, OtherSize, OtherTime, NotAFile, Unreadable(String) }
pub(crate) fn candidates(conn: &Connection, subject: &StackSubject, current_root: &Path) -> anyhow::Result<Vec<Candidate>>
pub(crate) fn check(c: &Candidate) -> Check                      // one stat, no connection
pub(crate) fn commit(conn: &mut Connection, moved: &[&Candidate]) -> anyhow::Result<usize>
pub(crate) struct RebaseView { by_row: HashMap<i64, String>, left_gone: usize }
impl RebaseView { pub(crate) fn for_gate(conn: &Connection, subject: &StackSubject, current_root: &Path) -> anyhow::Result<Self>;
                  pub(crate) fn none() -> Self;
                  pub(crate) fn apply(&self, row: Option<StackingArtifactRow>) -> Option<StackingArtifactRow> }
```

- `db::stacking::subject_working_dirs(conn, subject) -> Result<Vec<String>>`: the distinct `working_dir` of the subject's runs, newest first.
- `db::stacking::rebase_artifact_path(conn, row_id, old, new) -> Result<bool>`: `UPDATE stacking_artifacts SET path = ?3 WHERE id = ?1 AND path = ?2`. The new path is `current_root.join(tail)`, a separator-strict prefix swap computed per row; never `REPLACE`.
- `stacking/run/calibrate.rs`: `rebase_working_rows(rc: &mut RunContext) -> Result<(), RunError>`, the run's three phases.
- The gate applies `view.apply(…)` at its six row reads: `calibrated` and `calibrated_mosaic` (`stacking/plan.rs:3783-3829`); `registered` (`:2083`, so `compute_register_stale`, `:1902`, takes the view); `ln_reference` (`:3691`); `ln` (`:3940`); and `fits.<plane>` through a new `view` parameter of `fits_artifact_fresh` / `fits_artifact_fresh_for_planes` (`:1136-1185`). Their run callers (`run/measure.rs:694`, `:1629`, `run/register.rs:408`) pass `RebaseView::none()`.
- The gate's root, in the run's spelling and read-only: `normalize_path(canonicalize(working))` when the folder exists. No probe file is written, unlike `working_dir_as_run_spells_it` (`api/stacking.rs:845-862`). A folder that does not exist has no candidates.
- **Windows.** Prefix and subtree tests are by components. Stored paths carry no `\\?\` (`api/scan_roots.rs:58-70`), so old and new roots compare as stored, in exact case, as the directory rename does. A drive root and a UNC root map onto each other like any two roots.
- **Blink reads through the same view.** `api/stacking_aligned.rs:206-249` applies a `RebaseView` to the `registered` rows it reads, so an aligned frame is shown from the new place before the next run (RB-3).
- **The preview cache follows the current folder.** `resolve_master_light` lays the cache out under the subject's current working folder (resolved as `get_stacking_work_usage` resolves it). It falls back to the run row's folder only when none is configured, so a moved folder is never recreated.

**Logging.**
- `info!` "working folder rows rebased": `run_id`, `old_root`, `new_root`, `moved`, `left`; once per old root with candidates.
- `debug!` "cached file not rebased": `path`, `reason` (`missing`, `another size`, `another time`, `not a file`). A stat error other than NotFound is a `warn!` with `path`, `error`. The gate logs at `debug!` only.
- `old_root`, `new_root`, `moved` and `left` are new field names: add them to `docs/logging/README.md`.

**What the user sees.**
- After a move or a copy, the plan reads the stages cached and the run reuses them. While the run checks, the Calibrate row says "finding the cached files".
- When candidates are left and their files are gone at the old place too, the plan and the run carry one warning: "N cached files were not found in the working folder <root> as recorded (missing, another size, or a time more than 2 s off); their frames are recomputed."
- No command, TS type or setting changes.

**Left to §7:** a frame-set rename (RB-2); the crop-scratch masters under `runs/`.

**The calls, with the controller's defaults.**

| Id | Call | Default |
| ---- | ---- | ---- |
| RB-1 | A copy must keep its file times, within 2 s, to be reused at the new place. Without the rule, an older second copy (frames recomputed in one copy, the setting then pointed back at the other) reads as current, and the master is stacked from the old calibrated frames under the new settings. With it, a copy that drops the times (`cp` without `-p`, some sync tools) recomputes the set once. The time-free alternative is a content sample of every working file stored in its row and read once per file at a move: a new row field and about 0.13 s per file on the share. | Keep the 2 s rule. A wrong master is worse than one recompute. |
| RB-2 | A frame-set rename keeps reading the old slug folder's cached files, writes new ones into a folder with the new name, and never cleans the old folder. | Out of this cycle; one open-items line. |
| RB-3 | Blink between a move and the next run. | Blink reads through the view (above), so the frames are shown at once. |


## 5. Hash shape and versions

**What moves:**

- The stage-1 hash of every frame-set frame: the raw lights' calibration hash (the new `SourceIdentity` and the master tokens) and the attested lights' prepare hash.
- Every hash that chains on stage 1: `measurement_hash_for`, `pass1_hash_for`, `registration_hash_for`, `registered_hash_for`, the normalization hashes and the `two_pass` key.
- The registration hash also moves for the frames of a one-plane reference's own group (FI-6).

**`SourceIdentity` becomes an untagged enum:**

- `Sampled { file_id, size, sample }` for catalog lights;
- `Project { file_id, size, modified_at }`, kept only so the project hash serializes `{"file_id":0,"modified_at":"","size":N}` byte for byte.

`stage_hash` sorts by file id, size, then the third field (`stacking/config.rs:926-955`).

**Pins:**

- `stage_hashes_are_pinned_to_golden_literals` (`stacking/plan.rs:8722`). Its inputs become `100|sample:…`, `200|sample:…` and `Sampled { 42, 123456, … }`. `calib`, `meas`, `reg` and `ln` move on all four lines; `ln_ref` and `ln_cal` do not, because they read literals. The old values go into a dated comment, as the file does.
- `fit_stars_none_leaves_every_stage_hash_literal_unchanged` (`:8831`): re-pin `meas`, `reg` and `ln`.
- `pass1_hash_is_never_a_metrics_key` (`:8927`): its input is the golden default `calib`, so move the input and re-pin.
- `calibration_hash_for_is_the_pinned_formula` (`:8963`) and `an_attested_lights_hash_is_the_prepare_hash_over_its_catalog_identity` (`:9221`): rewrite these formula tests over the book.
- `the_prepare_subtree_and_the_project_hash_are_pinned` (`:9261`): add a literal recorded on `main` before the change. It must not move.
- New: a literal pin of a same-group mono registration hash.
- These do not move: `stacking/config.rs` pins with empty sources (`:1296-1300`, `:2040-2061`, `:2589`) and `render_modified_at_is_pinned_to_golden_literals` (`stacking/plan.rs:4169`).

**Versions:** no version constant moves.

- `ALIGN_VERSION`, `MEASURE_VERSION`, `FITS_ARTIFACT_VERSION`: no algorithm changes.
- `PREPARE_VERSION`: Prepare's output is the same.
- `PASS1_ARTIFACT_VERSION`: the format is the same.
- `TWO_PASS_PICK_VERSION`: the decision is the same.
- `STACKING_CONFIG_VERSION`: the document is the same.
- There is no cache schema version, and the table is unchanged.

The hashes move because the shape of their inputs moves; a version bump would add nothing.

**What a user sees.** Every frame set recomputes once on its first run after the update: Calibrate or Prepare, both measure passes, Register, LN, Integrate (FI-4, no migration). That run opens with "checking the inputs". Projects keep their Prepare and Measure caches; a project with a one-plane reference re-registers once. Together with the frame-level cache's own one-time recomputes (its spec §6, same release, FI-5), that is one recompute in all.

**The gate before that first run.** It reports Calibrate or Prepare stale for every frame set, and every stage after it, which is the verdict the run reaches. For an input with no banked sample the gate cannot form the new key at all, and reports stale for that reason.

## 6. Accepted risk (FI-1)

The risk: a same-size edit outside the three windows, with the header unchanged. Per kind:

| Kind | Not seen |
| ---- | ---- |
| Raw or attested light on disk | That edit. Also any edit that keeps both size and time (the scanner's own contract: the banked sample is trusted). |
| Catalog master on disk | The same. Details below. |
| Light or master inside a zip | Nothing new: the CRC32 covers the whole entry. An entry without a CRC is judged by its time recorded in the zip. |
| Project replica | Nothing new: blake3, and `landed_unchanged` excludes an in-place edit. |
| Own files (FI-2, FI-11) | An outside edit of a cached file at equal size that also keeps the time within 2 s. |

**The master-header check.** A FITS master that the app builds carries no build date.

- `build_master_cards` (`calibration_library/headers.rs:395-529`) and `HeaderBuilder` (`fits_writer/keywords.rs:116-122`, `:308-313`) write no `DATE`. Only XISF stamps a creation time (`fits_writer/xisf_writer.rs:454`).
- So a FITS master rebuilt in place does NOT always change its first 512 KiB of header. The header changes only when one of these changes:
  - the members: `ATH_HSH`, the xxh3 of the member uuids (`calibration_library/register.rs:44-57`), and `ATH_N`;
  - the recipe summary: `ATH_REJ`;
  - the app version: `SWCREATE`, `ATH_VER`.
- A rebuild that changes none of them (a new pre-calibration master, a recipe parameter outside the summary) keeps the header bytes. Its pixels still change everywhere, and the first window holds the image's first rows right after the header (one or two 2880-byte blocks), so the sample moves.
- An in-app rebuild also goes through `resync_catalog_rows_from_disk`, which NULLs `content_hash`, so the run reads the sample again.
- What is not seen: an outside tool's local pixel fix away from the windows that leaves the header untouched. This is the ERP case in the duplicate-detection spec §2.5. The research note's fallback (keep the time as a trigger for a full re-read of masters) is open point 2.

## 7. What it closes and what it leaves

Lines in `docs/superpowers/open-items.md`:

- **Backlog line (`:402`):** closed by this spec. Delete it when §9 passes.
- **"Known gap, a landed file edited under a cached first pass" (`:410`):**
  - The first half is closed in the run, for an edit that changes the size or a window. The run stats every input and samples again when the time moved.
  - The gate still presumes the catalog and can report cached for such a frame. That is a new residual; record it.
  - The second half (a change between Calibrate and Measure of one run) stays open. It is within one run, and the file is either Athenaeum's own (FI-2) or an in-place foreign file guarded only by `ReadGuard`. The suggested closer becomes a size + time check (the own-file rule, FI-11).
- **"Finding, what a regroup still re-registers" (`:408`):** the owner call is decided (FI-6), and item (3) is closed and pinned (§8). Items (1), (2) and (4) stay by design (§4.8). Rewrite the line.
- **"Owed before the release: the upgrade on real data" (`:403`):** the expectation changes to "the first run after the update recomputes every set (FI-4); the second reuses everything" (§9 A0).
- **New residuals to record:**
  - After a scan re-parse, the gate reads Calibrate stale until a run samples again.
  - An input that is offline and whose row never matched its file has no vouched sample, so its frame is excluded until the file is back.
- **`docs/stacking/README.md`:** rewrite the bullets "Identity check", "Pass 1 and its stamp", "The registration fold rule", "One-time recomputes" and "What a regroup does not reuse" (`:5179-5244`), and the lines at `:1889`, `:3794` and `:5268`.

**Out of scope:**

- The scanner's own `(size, modified_at)` contract (`reparse_and_update_in_place`, `disk_matches_row`). After this change a re-parse no longer reaches the stacking keys unless a window or the size changed; it costs one re-read.
- A frame-set rename: the slug folder changes, the cached files stay in the old one and are still read there, new files go to the new one, and the old folder is never cleaned (RB-2, §4.10). One open-items line.
- The crop-scratch masters under `runs/` after a moved working folder (the Results card says "not on disk").
- The collab landing stamp.
- The master-light preview cache.
- Moving the content index onto `bank_content_hash`.

## 8. Tests

Fixtures to reuse:

- `stacking::test_fixtures` (`frame_set`, `frame_set_with_conn`, `add_light`, `add_light_with_moffat_field`, `add_master_dark_and_flat`, and `file_identity` at `:565-578`, which stores the scanner's shape);
- `telemetry::capture_logs` and `fan_out_items`;
- `RegisterRig` (`stacking/run/register_tests.rs`) and `Pass1Rig` (`stacking/run/measure_tests.rs`);
- the scanner tests' `stamp_later_mtime` and `File::set_modified` (`scanner/mod.rs:3072-3082`).

Every gate is `cargo test -p athenaeum-core --all-targets`.

**Unit tests:**

- `source_identity.rs`:
  - a matching row is trusted without a read (a banked value that is not the file's own sample comes back);
  - a coarser time reads again and finds the same sample (truncate to whole seconds, then to 100 ns), with no bank;
  - an unbanked matching row is read and banked;
  - a zip row is its CRC, with no stat;
  - an offline input keeps its banked sample;
  - `CatalogOnly` never reads, and finds the memo after a `Disk` pass;
  - a file changed during the read is `NotSampled`;
  - an edit inside a window is seen, and an edit outside the windows of a 4 MiB file is not (the accepted risk, pinned so nobody "fixes" it silently).
- `db`:
  - `bank_content_hash` writes only over a matching NULL row: not after a re-parse-like UPDATE, not over a value, not on a zip row;
  - `delete_artifact` is a point delete, like `find_artifact_is_a_point_lookup_on_the_artifacts_key` (`db/stacking.rs:2386`).
- `identity_refusal_by_mtime_says_why` became `identity_is_the_size_and_a_time_within_two_seconds` (`stacking/plan.rs`, FI-11): a same-size file whose time is within 2 s of the row's (at full precision) matches, 2.001 s either side does not and the debug line says why, another size does not, a row without a time or with one that does not parse never matches, `NotFound` is silent and a stat error warns. `an_in_place_row_has_no_time_check` pins the exemption of a row that names the input.
- The strict pins in `stacking/measure.rs:2225` and `stacking/pass1_artifact.rs:635-650` now pin that a moved time never makes a stamp stale.
- `registration_hash_folds_the_reference_measurement_across_groups_only` (`stacking/plan.rs:10984`) becomes `…_whatever_its_group`. Its `:11014` assertion flips. The debayered-OSC reference still folds nothing, and `register_stale_follows_the_reference_measurement_for_an_osc_frame` (`:11045`) stays green.

**Regressions (these fail today):**

- `coarser_times_reuse_every_frame_level_stage`, in `stacking/run/normalize_tests.rs`, on the fixture of `regroup_reuses_every_frame_level_stage` (`:745`):
  - after run 1, every light's, master's and working-folder file's time is set to whole seconds (run 2) and then to 100 ns steps (run 3);
  - calibrate, pass 1, pass 2 and align fan out over 0 items on runs 2 and 3;
  - the gate reads Calibrate, Measure and Register fresh, from run 1's banked samples.
- `an_in_window_edit_is_a_miss`, same fixture: one light gets a changed byte inside a window, equal size, a moved time and no rescan.
  - Run 2 re-runs that frame only (calibrate 1, pass 1 1).
  - The gate before run 2 reads fresh. This is the documented residual, asserted so it stays visible.
- `measure_pass1_artifact_of_an_edited_in_place_source_reruns_pass_one` (`stacking/run/measure_tests.rs:822`):
  - its simulated rescan also sets `content_hash = NULL`, as `write_reparse_rows` does;
  - plus a variant with no rescan, where the run samples again on the moved time.

**Cancelled rewrite:**

- `a_cancelled_rewrite_does_not_leave_another_runs_pixels_fresh` (`stacking/run/register_tests.rs:824`) keeps its scenario and its end: the gate reads stale, the re-run rewrites 4 frames, and the master is bit-identical to run A's. Its premise becomes: after run B's cancel, the four `registered` rows are gone.
- New in `stacking/run/calibrate_tests.rs`: run B, with another calibration option, is cancelled after Calibrate's fan-out. The frames written after the first unfinished job have no row. They recalibrate at run A's config, and the planes equal run A's, compared as `recalibration_reproduces_the_planes_bit_for_bit` (`:1096`) does.
- `.athf`: with `FITS_WRITE_FAILURE_LOG` (`stacking/run/measure.rs:1972`), a failed write leaves no row.
- Blink: `aligned_file_that_is_not_the_recorded_one_is_replaced` (`api/stacking_aligned.rs:697`) became `aligned_file_is_judged_by_its_size_and_time` (FI-11): a time cut to the second or within 2 s is `Available`, a time moved by an hour is `Replaced`, another size is `Replaced` (the time put back, so the size alone decides), a row a later run deleted is `Missing`.

**Regroup:**

- `mono_regroup_keeps_every_registration_when_the_reference_stays`: the `:745` fixture with `bayerpat: None`, the β pegged at its floor, and a Manual reference in the 60 s class.
  - The sequence is `seconds` → `stops` (merge) → `seconds` (split).
  - Align and every frame-level stage fan out over 0 items on runs 2 and 3, and the gate reads Register fresh before each.
  - It fails today: run 2 re-registers the 90 s frames.

**In-place rows (§4.1):**

- `an_in_place_row_at_another_path_than_its_light_is_stale`, on the attested-light fixture of `stacking/run/prepare_tests.rs`: after run 1 the light is relocated (`files.path` changed, the bytes copied to the new path) and a different file of the same size is left at the old path. The gate reads Prepare stale for that frame only. Run 2 writes nothing, re-points the row at the new path, and pass 1 covers 0 items. It fails without the rule: the row reads fresh and names the other file.

**Working-folder rebase (§4.10):**

**Unit, `stacking/rebase.rs`:**
- `rebase_maps_by_components_onto_the_kinds_own_subtree`: a table on the pure mapping. `/w/M31/calibrated/c_a_f1.fits` → `/n/M31/calibrated/c_a_f1.fits`; a legacy `fits/<group_key>/a_f1.p0.athf` keeps its sub-folder; `/w/M310/…`, `/w/M31/lights/x.fits` as `calibrated` and `/w/M31/registered/r.fits` as `calibrated` map to nothing; `/w/_projects/p/prepared/p_a_f1.fits` maps; under `#[cfg(windows)]`, `C:\w\M31\ln\g\a_f1.athln` → `\\nas\s\M31\ln\g\a_f1.athln`.
- `rebase_needs_the_size_and_a_time_within_two_seconds`: copies at +1.9 s and −1.9 s are `Moved`; +2.5 s is `OtherTime`; one byte more is `OtherSize`; a folder at the path is `NotAFile`; no file is `Missing`; a row with `size` or `modified_at` NULL is no candidate.
- `rebase_candidates_skip_the_current_root_in_place_rows_and_other_subjects`: rows under the current root, an in-place row, payload-only rows and another set's rows under the shared slug root are no candidates. An old run folder that is the current one through a symlink gives none.
- `rebase_moves_only_the_path_and_never_touches_the_old_folder`: after `commit`, `config_hash`, `size`, `modified_at`, `payload_json` and `created_at` are equal; a walk of the old folder before and after gives the same names, bytes and times.
- `rebase_update_is_a_no_op_when_the_row_moved_meanwhile`: a path changed between `check` and `commit` gives 0 for that row.

**Regressions (`stacking/run/normalize_tests.rs`, on the fixture of `regroup_reuses_every_frame_level_stage`, `:745`):**
- `a_moved_working_folder_reuses_every_frame_level_stage` (fails today): run 1 under W1; `std::fs::rename` W1 → W2, then every file's time truncated to whole seconds; the setting points at W2.
  - Before run 2, the gate reads Calibrate, Measure, Register and Normalize fresh, and every row still names W1 after `build_plan`.
  - Run 2: calibrate, pass1, pass2, align and the LN fan-out cover 0 items; every file row names W2; W1 was not recreated.
  - Today run 2 recomputes all 6 frames.
- `a_copied_working_folder_is_used_and_the_original_is_untouched`: copy W1 → W2 with the times kept (`File::set_modified`). Run 2 under W2: 0 items, the rows name W2, W1's walk is unchanged. Remove W1; run 3: 0 items.
- `a_diverged_copy_is_recomputed_not_read`: run A under W1; copy to W2 with the times; run B under W2 with `hotPixelCorrection` flipped (assert the calibrated sizes are equal: the premise); set every W1 time 1 h back; remove W2; point at W1; run C at B's config. Calibrate covers 6 items, the warning counts the left files, and the planes equal run B's (as `recalibration_reproduces_the_planes_bit_for_bit`, `stacking/run/calibrate_tests.rs:1201`). Without the 2 s rule, run C would reuse run A's planes under run B's key.
- `an_in_place_row_inside_the_working_root_is_never_rebased`: the fixture of `stacking/run/prepare_tests.rs:288` with the debayer off; copy the working root, `landed/` included, to W2 and point at it. The in-place rows still name W1's landed files.
- `a_moved_working_folder_caches_previews_in_the_current_one` (`api/stacking.rs`): after a move, a master preview is cached under W2 and nothing is created under W1.
- `blink_shows_a_moved_folders_frames_before_the_next_run` (`api/stacking_aligned.rs`): after the move of `a_moved_working_folder_reuses_every_frame_level_stage` and before run 2, every aligned frame is `Available` and its path is under W2; the rows still name W1.

## 9. Acceptance

Headless or owner-run, on a copy of the dev catalog under `~/.athenaeum-acc/` (the disk rule: one `REGISTRY.tsv` row), with the release `athenaeum-web`. The sets are OSC 163 (16 lights) and mono 185 (67 lights), with a Manual reference. Record the `fan-out finished` items for calibrate, pass1, pass2 and align, and the count from the sampling step.

- **A0, the update.** The first run on the copy's existing caches recomputes every stage once and reports the sampling count. The second run reports 0 items for calibrate, pass1, pass2 and align. This replaces the owed open-items line `:403`.
- **A1, in place.** With the working folder on the SMB share (Universe), the second run reports 0 for calibrate, pass1 and align. The same on the exFAT drive (Fotobase).
- **A2, copies with the time kept.**
  - Copy the set's lights and one master to the share with `cp -p` or Finder, and point the catalog at them (Missing files relocate, or a rescan of the copies' root).
  - Copy the working folder off the share and back with its times kept, to the SAME absolute path, because rows name absolute paths.
  - Check with `stat` that the times now differ (100 ns, 10 ms, whole seconds).
  - The next run reports 0 for calibrate, pass1 and align, after reading at most 1.5 MiB per input.
  - Repeat on exFAT.
- **A3, regroup.** On set 185 with `measurement.psfModel = Moffat4` (the β then enters no key; under Auto this set's merged β moves, and that re-registers by design): run 1, then `stops` (merge), then `seconds` (split). Align reports 0 on both regroup runs, and the gate reads Register fresh before each.
- **A2b, the working folder at another path.** On 163 and 185, after A1:
  - copy the working folder from the share to the exFAT drive with Finder (or `cp -Rp`), keep the share copy, and point Settings → Stacking → Working folder at the copy;
  - `stat` shows the copies' times rounded (10 ms, whole seconds), within 2 s;
  - before the run, the plan reads Calibrate, Measure and Register cached; the run reports 0 for calibrate, pass1, pass2 and align, and logs `working folder rows rebased` with `left = 0`;
  - the share copy is unchanged: `find <old> -newer <marker>` lists nothing, and the file count is the same;
  - unmount the share and run again: 0 again;
  - repeat from the exFAT drive back to the share, and once for one project (Prepare and Measure kept; its in-place rows still name the Collaboration folder).
- **A2c, a copy that dropped the times** (`cp -R` without `-p`): the plan warning counts the files and the run recomputes them. Skip it if the owner rules RB-1 the other way.

## 10. Task outline

| # | Task | Files | Gate |
| ---- | ---- | ---- | ---- |
| T1 | Write order and `delete_artifact` (§4.2) | `db/stacking.rs`, `stacking/run/{calibrate,prepare,measure,register,normalize,frames}.rs` | the cancelled-rewrite tests of §8, all green; no hash moves |
| T2 | Own files by key + size; Blink (§4.1) | `stacking/plan.rs`, `api/stacking_aligned.rs` | the rewritten identity and Blink tests; no hash moves |
| T3 | `source_identity.rs`, memo, `bank_content_hash`, `disk_matches_row` split to take one stat (§4.4, §4.5) | `stacking/source_identity.rs`, `db/operations.rs`, `duplicates/backfill.rs` | unit tests; no caller; no hash moves |
| T4 | Stage-1 identity on samples; the book in the gate and the run; `strict_mtime` removed; golden re-pins (§4.3, §4.6, §4.7, §5) | `stacking/{config,plan,measure,pass1_artifact}.rs`, `stacking/run/{mod,calibrate,measure,masters}.rs` | coarse-time and in-window tests; every pin; moves stage 1 and everything after it |
| T5 | FI-6 fold (§4.8) | `stacking/plan.rs`, `stacking/run/{register,two_pass}.rs` | fold pin; mono regroup test; moves the registration hash for one-plane references |
| T6 | Docs (§4.5, §7) | `docs/stacking/README.md`, `docs/catalog/README.md`, `docs/superpowers/open-items.md` | review |
| T7 | Acceptance (§9) | `docs/stacking/README.md` (numbers) | A0–A3, A2b and A2c pass; delete open-items `:402` and `:403` |
| T8 | Working-folder rebase; the preview cache in the current folder (§4.10) | `stacking/rebase.rs` (new), `stacking/mod.rs`, `db/stacking.rs`, `stacking/plan.rs`, `stacking/run/{calibrate,measure,register}.rs`, `api/stacking.rs`; docs `docs/stacking/README.md`, `docs/logging/README.md`, `docs/superpowers/open-items.md` | the §8 rebase tests; `a_moved_working_folder_reuses_every_frame_level_stage` red before, green after; no hash moves |

**Order:** T1 → T2 → T8. T3 → T4 → T5. Then T6, then T7. T1 and T3 can run in parallel; T8 needs §4.1 (without it a copy's rounded time fails today's time string even after the rebase) and nothing of T3 to T5. The in-place rule of §4.1 is part of T2.

**What must land together:**

- T1 and T2 in one merge: the time leaves the own files only once the write order closes R7 b.
- T4 in one commit with every literal it moves.
- T4, T5 and the frame-level cache in one release (FI-4, FI-5), so users pay one recompute.
- T8 in the same release as T2, in its own merge. A2b and A2c join T7.

## 11. Points closed at the approval

The three points this draft left open, as the owner closed them on 2026-11-12. Points 1 and 2 keep their defaults (FI-7, FI-8); point 3 is in scope (FI-9, §4.10).

1. **The gate after a scan re-parse.** The scan NULLs `content_hash` on any drift (`scanner/mod.rs:1384-1396`). After a rescan of copied files, the gate therefore reads Calibrate stale until a run samples them again; the run then reuses everything. Default: accept it and document it. The alternatives are the gate reading 1.5 MiB per input (about 74 s for 551 lights on the share), or a stacking-side copy of the last sample.
2. **A FITS master rewritten in place with its header kept.** The research note proposed keeping the time as a trigger for a full re-read of masters in this case. Default: no full read (FI-1), with the risk stated in §6.
3. **A working folder moved to another path.** Rows name absolute paths, so such a move recomputes everything: the cause is the path, not the time. The draft's default was out of scope. The owner put it in scope (FI-9): §4.10.

## 12. Corrections found while planning (2026-11-12)

The plan `docs/superpowers/plans/2026-11-12-stacking-file-identity-plan.md` re-verified every line number of this spec at `0df147a7` and lists the moved ones under its "Spec corrections"; they are not repeated here. The corrections below change a rule or a claim of this spec. Where this section and an earlier one disagree, this section holds.

- **§4.2, the mosaic row.** It is always read, and it is deleted whenever the job writes its path: when the job wants a mosaic, or when the row names a file the job writes. With the debayer off, an OSC frame's calibrated file takes the mosaic's name (`c_<stem>.fits`), so a mosaic row left there at the old hash would be reused by a later Bayer run over another run's pixels.
- **§4.2, "the one write without a delete".** The note that pass 2's fallback writes under the SAME key is wrong for a member whose own β came from its `metrics` row while its `measured` row is not fresh: that member is not owed pass 1, and its pass-2 worker writes under another key. The `measured` row of every member left owed without a fresh row is deleted before pass 2's fan-out.
- **§4.2, a failed delete.** A failed `delete_artifact` stops the job; it is never a warning followed by the write. The dry pass of the two-pass reference deletes nothing, because it persists nothing.
- **§4.2, the LN reference.** Its file is written by `write_reference` at `stacking/run/normalize.rs:1766`; `:1836-1870` is its identity and upsert (`upsert_artifact` at `:1855`). The delete goes before `:1766`.
- **§4.1, "in place".** The run decides it from Prepare's action (`InPlace`, or the kept float mosaic, for a frame not in a zip). The gate, which has no action, decides it from the row lying outside the `prepared/` and `calibrated/` folders of the subject's roots, in both spellings.
- **§4.4, the run's book** also covers manually excluded frames: every stage's hash call propagates its error, and a lookup outside the book would fail the run.
- **§4.10, the 2 s rule.** The time is compared with `MTIME_TOLERANCE_SECS` at full precision, not through `Stamp::matches`, which truncates both times to whole seconds and so accepts up to 2.999 s.
- **§10, the order.** T1 → T2, T3 → T4 → T5, then T8. T8's run tests use T4's regroup rig and its trigger sits before T4's book, so T8 does not run beside T3 to T5.
- **§9 A1.** The first run in a new working folder reads the rows left at the previous folder there and computes nothing. A1 needs `rerunFrom: calibrate` (or the previous folder removed) to write its files in the new folder first.
- **§9 A2.** Lights are relocated through Missing files; a rescan of the copies' root makes new rows and new frames outside the set. Only a master may be rescanned (FI-3).
- **§9 A2c.** While the source copy exists, the left rows are read there, with no warning and no recompute. The warning appears only once the source copy is gone.

**Corrections found while implementing (2026-11-13).** The code on `main` at e8f59465 is the source of these. Where this block and an earlier section disagree, this block holds.

- **§4.2, rows of another subject.** A writer retires every row that names the file it is about to write, of any subject and kind (`db::stacking::delete_artifacts_naming_path`, a search on the new index `idx_stacking_artifacts_path`), beside the keyed `delete_artifact`. Two frame sets with one name share a working root, so the other set's row would otherwise stand over the new file. "No schema change" has this one exception: the index (the table's columns are unchanged).
- **§4.1, "nothing compares it".** The rebase of §4.10 reads the row's `modified_at` (within 2 s). It never decides freshness.
- **§4.1, the gate's own-file test for a Prepare subject.** A row under `<wd>/<one component>/(prepared|calibrated)/…`, or the same under `<wd>/_projects/`, is the subject's own file, where `<wd>` is the current working folder in both spellings or any folder the subject's runs recorded (`db::stacking::subject_working_dirs`). Any other row is in place and must name the frame's current input (`stacking::plan::prepare_row_names_its_input`).
- **§4.4 `Disk` step 2.** ANY stat error takes the catalog's last sample, not only NotFound. With no banked sample the frame is excluded with "not found, and never sampled" (NotFound) or "unreadable" (another error). A run lookup outside the book reads "<path>: linked after the run started; run again".
- **§4.5, the guard.** `bank_content_hash` also checks the row's `path` (`db::bank_content_hash(conn, file_id, path, size, modified_at, hash)`), so a relocate between the read and the UPDATE refuses the sample.
- **§4.5, the catalog README's line 36.** Zip extraction banks its sample after it has verified the entry's CRC32 and size, not under the `disk_matches_row` contract; the README says so.
- **§3 and §4.3, the scanner's move detection.** A file found at a new path and linked to its old row by the header fingerprint keeps `content_hash` only when the file's time did not move; otherwise the sample is dropped and the next run reads the file again. Without this a rewritten time would vouch for bytes nobody read. This is the one change outside stacking (`scanner/mod.rs`, both move UPDATEs). `strong_hash` is left as it was.
- **§4.10, the probes and the warning.** The view and the run probe by folder: each mapped folder that holds two or more candidates is listed once, a file it does not list is `Missing` with no stat, and a listed one (or the only candidate of its folder) gets the one-stat check. The "not found" warning counts only rows a stage reads: the gate, the rows it asked the view for; the run, the frames in scope and the kinds its config reads (`RunReads`). Blink's view covers a project's rows too. The gate logs at `debug!` only. The preview cache is written only under a working folder that exists.
- **§7, accepted residuals.** (1) The gate reads Calibrate stale after a scan re-parse until a run samples again (FI-7). (2) A light relocated through Missing files BEFORE it was ever sampled, onto a volume with coarser times, has no vouched sample while that volume is offline: its frames are excluded with "not found, and never sampled" until the volume is back or its root is rescanned. (3) A zip entry recorded without a CRC moves its token once when the CRC is recorded: one extra stage-1 recompute of those lights.

**From the acceptance (2026-11-13).** The runs and their numbers are in `docs/stacking/README.md` ("Acceptance of the file identity"). Where this block and an earlier section disagree, this block holds.

- **§9 A0.** The dev catalog held no stacking rows for sets 163 and 185, and their calibration links were made in the copy, so A0 ran cold on them: run 1 computes everything, run 2 reuses everything. The update over existing rows ran on set 182: every stage once, then 0 for calibrate, pass1 and pass2 and 9 for align (registrations that fail on every run; a failure is not cached).
- **§9 A1 to A2c.** The share and exFAT legs ran on set 163 only. Set 185 on the share and the project leg of A2b were not run.
- **§9 A3.** On set 185 `stops` merges no group (10 s and 180 s are more than a stop apart) and only renames the keys; the merge and the split ran as `none` and back to `seconds`. Align reported 0 on every regroup run.
- **§9 A2c and §4.10, the warning.** The not-found warning counts frames, not files (the plan and the run counted files differently, 35 against 114), and says the files are "from an earlier working folder" (it named the current folder as the one "recorded").
- **Outside this spec's scope, found by its acceptance.** An SMB share refuses a full sync (`F_FULLFSYNC`, os error 45), so the LN reference and the other durable writes could not be made in a working or output folder on a share. Every writer now flushes through `zip_store::fs::sync_best_effort` (`9f404957`).

**After the acceptance: FI-11 (2026-11-13).** Where this block and an earlier section disagree, this block holds.

- **The rule.** An own working file is fresh by key, size AND a modification time within 2 s of its row's (§4.1). An in-place foreign row keeps key + size and no time check. This amends FI-2 and the "Why the time, here only" argument of §4.10: the time is compared in both places now.
- **The reason.** The dev build and the release build share one working folder on purpose, with two catalogs. A writer retires only its own catalog's rows (`delete_artifact`, `delete_artifacts_naming_path`). The other catalog's row at its own key and the same size read fresh over a file the first build rewrote under another config. Before the file identity the exact time made that row stale.
- **The measure.** On the owner's disks a copy that keeps its times moves a file's time by at most 9.57 ms (exFAT) and 62 ns (the SMB share), so 2 s costs nothing there.
- **The cost.** A working folder copied onto its own path by a tool that resets the file times recomputes every frame once, and the next run recomputes nothing. A copy that keeps its times, to another place or onto the same one, is not affected.
- **What stays open.** Two writes of one file within 2 s of each other by two builds, at the same size, still read fresh. Not expected between two builds' runs.
- **Accepted (2026-11-13, `docs/stacking/README.md`, "Acceptance again after FI-11").** A file written on the SMB share or on the exFAT drive keeps the time its row recorded (difference 0, on the share also after a remount); a copy that keeps its times is within 9.5 ms; a second catalog over the same working folder recalibrates and its masters equal a clean run's in every data byte. A recompute caused by another time is not explained to the user yet (open item).
