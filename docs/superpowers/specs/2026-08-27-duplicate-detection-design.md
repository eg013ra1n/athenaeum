# Duplicate detection — design

Date: 2026-08-27
Status: approved for planning

## 1. Goal

Make the Duplicates view find the duplicates that are actually in the catalog,
without ever proposing two different frames as copies of each other.

The trigger was an owner report: calibration set 628 held twenty pairs of the
same flats and the Duplicates view offered nothing. Measured on the owner's
production catalog (41 893 files), the view currently returns **zero groups**
while **2 750 groups / 5 552 files / 170.5 GiB** of true duplicates sit in it,
across 33 calibration sets.

### Non-goals

- Building the content index for the user. `content_hash` stays opt-in; this
  design's point is that the answer is already in the database without it.
- Changing what the Black Hole does, or how the keep-rule chain picks which
  copy survives.
- Making `deep verify` mandatory for raw sub-frames. Owner decision D6 below.
- Deciding a master's identity from its header. Proven impossible (§2.4) — a
  master's duplicates are settled by a full-file hash instead, D3.

## 2. Evidence

Every number here was measured against the owner's production catalog, not
reasoned about. Method notes are in §7.

### 2.1 The current key cannot work

`duplicates::compute_metadata_hash` hashes `size + modified_at + filename`, and
`find_duplicate_groups` groups by `(metadata_hash, size)`.

`modified_at` is a property of the **copy**, not of the **frame**. Two copies of
one exposure agree on everything the key measures except the one field that
copying is free to change.

In set 628 all forty files are present on disk and hash to **twenty** distinct
SHA-256 values — twenty byte-identical pairs. `DATE-OBS` matches to the
millisecond in every pair. The mtimes do not, and they diverge with a signature:

```
Astrobase  1728102106000000000   whole second, always EVEN
Unsorted   1728102104307186700   full nanosecond precision
```

All twenty Astrobase copies sit on an even whole second; all twenty Unsorted
copies carry nanoseconds; the delta is always positive and always under 2 s.
That is FAT/exFAT's two-second timestamp granularity — the copy travelled
through an exFAT volume, which is how astro data normally moves from a Windows
capture PC to a Mac. **2 189 of 2 763** same-name/same-size groups catalog-wide
carry this signature. Another 548 have mtimes months apart: copied without
`-p`, so the mtime is the copy time.

### 2.2 The failure is asymmetric but not one-sided

Adding a field to a hash key can only split groups, never merge them, so every
effect of the `modified_at` term is a **miss**. Measured today: 0 groups
returned, cache tables empty, zero `(filename, size, whole second)` collisions
across all 41 893 files, and the Black Hole holds four files, none of them from
the Duplicates view. Nothing false has ever been proposed on this catalog.

The false-positive surface is real but currently unreached. It comes from what
the key **omits** — content. A collision needs same filename, same size, same
whole second (`DateTime::timestamp()` truncates sub-second precision). Astro
data supplies two of three for free: every sub from one camera has the same
size, and filenames repeat across filter folders — proven 36 times in this
catalog (`C_2022_E3_ZTF_Light_001.fits` under R/G/B; `IC_2087_Light_001.fits`
under Filter#1/Filter#2). Only mtime separates them, and at capture time the
exposure cadence does that job: the tightest gap between two different-content
files here is **27 s**, median 100 s.

Two in-app paths dissolve that accidental protection by writing files at disk
speed instead of capture cadence:

- **Archive restore.** `archive/restore.rs:208` extracts with `File::create` +
  `write_all` and no mtime restoration; the restore then writes the
  extraction-time mtime into the catalog via
  `unmark_file_archived(..., Some(&new_mtime), ...)`.
- **Sync ingest.** The wire manifest carries no mtime at all;
  `sync/ingest.rs:573` copies + renames, so a received file gets receive time.

Neither has run on this catalog (`archive_operations` is empty), so this is a
reachable path read off the code, not an observed event. It is the reason the
fix must not simply swap one lossy key for another.

### 2.3 The replacement key already exists and is fully populated

`fits_header.header_fingerprint` is `xxh3_64` of the stored header blob,
computed at scan time for the relinking feature (`db/operations.rs:317`). It is
independent of mtime by construction.

| property | value |
| ---- | ---- |
| coverage | 41 893 / 41 893 files |
| index | `idx_fits_header_fingerprint` already exists |
| rows with NULL/empty fingerprint | 0 |
| groups on set 628 | 20 groups of 2 — correct |
| catalog-wide | 2 780 groups / 5 613 files |
| raw-frame groups mixing different `date_obs`/`filter`/`imagetyp` | **0 of 2 750** |

Precision, measured by full SHA-256 over sampled groups:

| bucket | groups | sampled | byte-identical | precision |
| ---- | ---- | ---- | ---- | ---- |
| raw sub-frames (`is_master = 0`, imagetyp ∈ Light/Flat/Dark/Bias/DarkFlat) | 2 750 | 80 | 80 | **100 %** |
| masters and processed files | 30 | 30 | 0 | **0 %** |

### 2.4 Why masters cannot be keyed by header

Three independent causes, all verified against the raw XISF bytes:

1. **The file misreports itself.** `Pane_2_Sii.xisf` genuinely contains
   `<FITSKeyword name="FILTER" value="'H'"/>` and the Ha stack's `DATE-OBS`.
   PixInsight propagates the *reference* image's FITS keywords through
   ImageIntegration and PixelMath, so an Sii integration inherits Ha's
   keywords. No parser can correct a file that states the wrong filter.
2. **Our XISF parser discards the `comment` attribute.** PixInsight writes
   history as `value="" comment="ImageIntegration.rejectedHigh_32: 105348"`;
   `fits_parser/mod.rs` matches only `b"name"` and `b"value"` and its `_ => {}`
   arm drops the rest, collapsing 364 HISTORY records to empty strings.
   Including `comment` separates **4 of the 30** groups. Real defect, but not a
   fix for this problem — see §6.
3. **For the remaining 26 there is nothing to separate.** `Pane_2_Sii.xisf` and
   `Pane_2_Sii_f.xisf` share all 364 FITSKeywords, all 21 `<Property>`
   elements, geometry, sampleFormat and location. Only the pixels differ — DBE
   and linear fit change data without touching metadata. XISF's optional
   `checksum` attribute, which would have given content identity for free, is
   absent (PixInsight does not write it).

Cause 3 is decisive: **no header-level key of any completeness can separate
these files.** Only the bytes can — see §2.6 for why that is affordable here.

Note what the divergence actually is. In each of the three `..._DBE_WCS.xisf` /
`..._f.xisf` pairs, **exactly one Float32 pixel differs** — 4 bytes out of
77 MiB, 0.000005 % of the file. That is the number any sampling scheme has to
beat.

### 2.5 Why not simply key on the three-part sampling hash

`duplicates::compute_xxhash` reads the first, middle and last 512 KiB of a file
and hashes them. It is already what fills `files.content_hash`, so "use the
sampling hash" is not a new design — it is the existing `Content` branch, and
the question is only whether it should be the default.

Measured, it should not:

| | header key | sampling hash |
| ---- | ---- | ---- |
| raw sub-frames — agreement with a full SHA-256 | 80 / 80 | 40 / 40 |
| masters and processed — agreement with a full SHA-256 | 0 / 30 (refuses to group) | 27 / 30 |
| bytes to read over the catalog | 0 | 61.4 GiB |
| wall clock over the catalog | 0 | ~19 min (warm, local APFS) |

- **On raw frames it buys nothing.** Both are exact. Paying 61.4 GiB of I/O —
  and paying it again for every new or changed file — to reproduce an answer
  already sitting in the database is not a trade.
- **On masters it fails, and in the dangerous direction.** Three of the thirty
  master groups are `..._DBE_WCS.xisf` / `..._DBE_WCS_f.xisf` pairs that differ
  by **three or four bytes** in a 77 MiB file, at offsets 0.5–0.9 MiB — just
  past the first sample and nowhere near the middle or the end. The hash reads
  2 % of the file and the difference lives in the other 98 %, so it reports a
  duplicate and offers one of the two for deletion. Where the header key
  refuses to guess, the sampling hash guesses wrong.
- **It adds a failure mode the header key does not have.** A FITS header sits
  inside the first 512 KiB of every file, so the first sample is
  disproportionately header bytes. Plate-solving one copy writes WCS cards into
  it and the two copies stop matching — a miss created by metadata that says
  nothing about the pixels.

Nor can the scheme be extended into precision. Measured over 20 000 trials
with one changed pixel at a random offset in a 77 MiB master, detection
probability tracks coverage **linearly**, because 4 changed bytes are only
detectable by reading them:

| scheme | reads | coverage | detects the changed pixel |
| ---- | ---- | ---- | ---- |
| current, 3 × 512 KiB | 1.5 MiB | 1.9 % | 1.7 % |
| 3 × 2 MiB | 6 MiB | 7.8 % | 8.0 % |
| 16 × 512 KiB | 8 MiB | 10.3 % | 10.1 % |
| 32 × 1 MiB | 32 MiB | 41.3 % | 41.4 % |
| full hash | 77 MiB | 100 % | 100 % |

Spending the same byte budget on *more, smaller* samples makes it strictly
worse, not better: against the 30 real master groups, 8 × 512 KiB gives 3 false
positives, while 64 × 64 KiB and 256 × 16 KiB — identical 4 MiB budgets — give
17 and 22. Coverage is not fungible; the start of the file holds both the XML
header and the first image rows, which is where these files actually diverge.

So the sampling hash cannot be tuned into a decision. The decision has to be a
full hash — §2.6 is why that is cheap enough to make.

### 2.6 A full hash is affordable exactly where it is needed

| population | files | bytes |
| ---- | ---- | ---- |
| the whole catalog | 41 893 | 2.62 TiB |
| all masters | 381 | 89.4 GiB |
| **masters the header key shortlists into a group** | **61** | **7.5 GiB** |

The header key is useless as a verdict on masters and excellent as a filter:
it takes 381 master files down to 61 candidates. A full hash over 7.5 GiB is
about a minute — three orders of magnitude below the 2.62 TiB that "hash
everything" would cost, and cheaper than the 61.4 GiB / 19 min the sampling
index would cost to produce an answer that is wrong in the deleting direction.

This is what D3 buys: masters shortlisted by header, decided by bytes.

## 3. Decisions

| # | Decision | Rationale |
| ---- | ---- | ---- |
| D1 | The cheap key becomes `(fits_header.header_fingerprint, files.size)` | Already 100 % populated and indexed, zero disk I/O, zero migration, mtime-independent by construction. Finds 2 750 groups where the current key finds 0. Not the sampling hash: same answer on raw frames for 61.4 GiB of reads, and wrong in the deleting direction on masters — §2.5. |
| D2 | `metadata_hash` stops being a duplicate key; the column is dropped in the follow-up hash cleanup (2026-08-28) | This row originally kept the column, claiming `MissingMetadataRow`'s `has_duplicate` flag read it. That was wrong: `has_duplicate` is a `LEFT JOIN` on `duplicate_group_files` and never touched the column. Once the view and folder similarity moved to the header key, nothing read it — every insert still paid for it and its index — so the cleanup cycle removed column, index, model field and `compute_metadata_hash`, with an idempotent `DROP COLUMN` migration for existing catalogs. |
| D3 | Masters and processed files are **shortlisted** by the header key and **decided** by a full-file hash | The header key measures 0/30 precision on them, so it can never be the decision (§2.4). But it is an excellent filter: it reduces 381 master files to **61 candidates / 7.5 GiB**, which a full hash settles exactly in about a minute. Header to narrow, full bytes to decide — so masters get correct duplicate detection instead of being hidden. Owner-confirmed 2026-08-27, replacing an earlier "exclude them entirely". |
| D3a | The full hash lives in a new `files.strong_hash` column, never in `content_hash` | `content_hash` is the three-part sampling hash and the transfer dedup handshake depends on that meaning. Overloading it would silently change what a peer is told about a file. |
| D3b | `strong_hash` is filled only for header-shortlisted masters, during the post-scan cache rebuild | The shortlist is what keeps the population at 61 files rather than 381 (89.4 GiB) or 41 893 (2.62 TiB). A scan has just read the whole library, so a minute of hashing is proportionate; master groups appear after the next scan, and a missing hash is a miss, never a false positive. |
| D4 | The two keys become an enum, not a bool | `duplicate_groups.hash_type` needs a third value; a `DuplicateKey` enum makes the cache mapping explicit instead of leaving `use_content_hash: bool` to mean three things. |
| D5 | The duplicate cache tables are dropped and recreated to widen their CHECK | They are derived data — `duplicate_groups`/`duplicate_group_files` hold nothing that cannot be recomputed, so a drop is correct and costs a single recompute, where the 12-step SQLite ALTER recipe costs a rebuild. |
| D6 | `deep verify` stays advisory | Owner decision. The button, progress, cancel and mismatch filtering already exist and already remove mismatched files from a group before the card renders; it simply does not gate the Move button. Not changed here. |
| D8 | The header key takes a third component, `files.filename` | The fingerprint does not separate a processed Light-derivative from its source: a GraXpert/ABE output keeps `IMAGETYP = 'Light'` and `is_master = 0`, and §2.4 cause 2 erases the processing history from the stored blob, so the two share header AND size while their bytes differ — verified as an offered duplicate pair on the owner's catalog. A byte-identical copy keeps its name across drives; a processing step renames its output. A renamed true copy becomes a deliberate miss. §4. |
| D7 | Folder similarity moves to the same key | `find_duplicate_folders` groups by `metadata_hash` too, so it is blind in exactly the same way. Same substitution, same test shape. |

## 4. The key

```
Header  →  (fits_header.header_fingerprint, files.size, files.filename)
           restricted to  frames.is_master = 0
                      AND frames.imagetyp IN
                          ('Light','Flat','Dark','Bias','DarkFlat')

Master  →  (files.strong_hash, files.size)
           the complement of Header's allowlist — masters and processed
           files, shortlisted by header, decided by a full-file hash (D3)

Content →  (files.content_hash, files.size)
           no restriction — the explicit override, and the only mode in
           which a master is decided by the sampling hash
```

In the default mode masters are decided by a full-file hash (`Master`, D3);
`Content` is the user's explicit override and applies the sampling hash to
everything, masters included — the one place where a master's identity is
settled by 1.5 MiB of samples. §2.5 measures that as wrong in the deleting
direction on 3 of 30 master groups, which is why it is not the default.

### The filename component

`Header` groups on the filename as well, because the fingerprint does not
separate a processed derivative from the frame it was made from. A
GraXpert/ABE-processed XISF keeps `IMAGETYP = 'Light'` and `is_master = 0`, so
the allowlist admits it, and it carries its source's header verbatim — the
parser defect of §2.4 cause 2 collapses the processing history to empty
`HISTORY =` lines — so `Lum.xisf` and `Lum_GraXpert.xisf` share a fingerprint
and a size while their bytes differ. Measured on the owner's production
catalog, that pair was offered as a duplicate. The filename is the signal the
header lost: a byte-identical copy keeps its name when it travels between
drives (this feature's whole candidate population is same-name/same-size),
while a processing step renames its output. The cost is a deliberate miss — a
renamed true copy is not grouped — which is the fail-safe direction and this
cycle's standing rule. Follow-up, not done here: decide name-divergent groups
by their bytes, the way `Master` already does.

Both keys keep the two predicates the current query already applies: the file
must not be in the Black Hole, and its path must sit under a `scan_roots` row
with `find_duplicates = 1`.

A file with no `fits_header` row, an empty blob, or a NULL fingerprint is
simply not grouped by the header key. That is a miss, never a false positive,
and it is the correct behaviour for the three scanner branches that insert no
header row and for sync-ingest's empty row.

## 5. What the user sees

- The Duplicates view fills up: 2 750 groups, 170.5 GiB reclaimable on the
  owner's catalog, including the 33 calibration sets that hold pairs like 628.
- Master and processed-file duplicates appear too, but only groups whose
  members are byte-identical: their headers shortlist them, a full hash
  decides. A shortlisted group whose hashes are not computed yet is not shown
  — it appears after the next scan.
- The Settings toggle stops claiming that the default groups "by size, date and
  filename" — a description of a key that returns zero groups on a real
  42 000-file catalog.

## 6. Discovered, deliberately out of scope

**The XISF parser drops `comment` (§2.4 cause 2).** It costs nothing for
duplicate detection *now*, but not for the reason first written here: masters
are kept off the header key by D3, and processed **Light**-derivatives — which
D3 does not touch, because they are `IMAGETYP = 'Light'`, `is_master = 0` — are
kept out of each other's groups by D8's filename component instead. Without
D8 the erased history is what makes `Lum.xisf` and `Lum_GraXpert.xisf`
indistinguishable to the header key.

It matters elsewhere: the stored blob is what the metadata pane's per-field
revert and light calibration's Bayer copy-through read, and for a
PixInsight-written XISF the entire processing history is currently erased.

Fixing it also carries a hazard worth stating before anyone tries: changing the
blob changes the fingerprint, so a file re-scanned after the fix stops matching
its not-yet-re-scanned copy. That is a false negative which self-heals on the
next scan of the other copy — safe, but it means the parser change and the
duplicate-key change should not ship in the same release without a full
re-scan. Recorded in `open-items.md` as its own finding.

## 7. Method

- Set 628: every one of the 40 files hashed with SHA-256 in full — 20 unique
  digests. Not a sample.
- Precision figures: stratified random samples (seeded) of fingerprint groups,
  each group's members hashed in full with SHA-256 and compared. 80 raw groups
  (10.3 GiB) and all 30 master groups (7.5 GiB).
- The 36 different-content groups: identified by disagreeing `date_obs` within
  a same-name/same-size group, then confirmed by full SHA-256 on every one.
- mtime signature: `st_mtime_ns` read from disk, not from the catalog.
- XISF findings: the monolithic header parsed straight out of the file bytes
  (8-byte signature, 4-byte little-endian length), keywords and properties
  compared element by element.
