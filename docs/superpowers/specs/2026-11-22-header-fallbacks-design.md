# FITS keyword fallbacks: user-ordered header keywords for catalog fields

Status: approved by the owner, 2026-11-22. Target release: 0.7.2 (promised in GitHub issue #21).

## 1. Intent

Some cameras write a catalog field under a keyword Athenaeum does not read. A DWARF 3 writes the sensor
temperature only as `DET-TEMP`, so the catalog's temperature stayed empty. 0.7.1 hard-coded `DET-TEMP` as a
fallback for `CCD-TEMP`. Issue #21 asked for the general version, and the owner promised it for 0.7.2: the user
says which header keywords a catalog field is read from, and in which order. Athenaeum applies that while
indexing and never writes to the user's files (their volumes may be mounted read-only).

Success means:
- a user whose camera writes a non-standard keyword fixes the whole catalog from the Settings page, with no
  rescan and no hand edits, including its calibration grouping;
- the user can share that setup as a file;
- every place that reads a header agrees on the value: scanner, zip scan, master registration, the metadata
  pane's "original", Apply.

## 2. Goals, non-goals, anti-goals

**Goals**
- One global, ordered keyword list per supported field. By default it is today's order. The user can add
  keywords and reorder all of them, the standard keyword included.
- New and changed files are read under the current rules, FITS and XISF alike, on disk and inside zips.
- **Apply to catalog**: re-read the listed fields of frames already in the catalog from their stored headers.
  No file I/O. Before it runs, a preview counts the changes per field.
- A value the user set themselves is never changed by Apply. The test is per field (§5.4), not per frame.
- Calibration frames whose grouping values change are grouped again (§5.5).
- Export and import of the user's rules as a JSON file.

**Non-goals**
- Rules limited to one camera.
- XISF `<Property>` values as sources. The existing built-in property fallbacks for pixel size and binning stay
  exactly as they are.
- Fields with their own formats or grouping role: DATE-OBS, RA/DEC/OBJCTRA/OBJCTDEC/rotation, IMAGETYP,
  binning, the CFA fields, NAXIS.
- Value transforms (units, scale, offset). A keyword's value is taken as it is.
- A warning about look-alike keywords such as `EGAIN`.
- Changing the scanner's whole-row `frames.override` rule for modified files. It stays exactly as it is.
- Re-matching lights automatically after Apply. The next matching run does that.
- Rebuilding masters or stacking results.
- An MCP tool.

**Anti-goals** (this would count as a failure even if it works)
- The preview counter, Apply and the scanner disagree on a value.
- On a catalog scanned by this build with default rules, the counter shows changes the rules do not explain
  (§9).
- Apply changes a value the user typed, or a value a plate solve wrote, or the scan root's "unique camera"
  suffix.
- Apply moves a light out of its frame set or night.
- A bad import file is applied partly.

## 3. Decisions

| # | Decision | By |
| ---- | ---- | ---- |
| O1 | One global list, not per camera. | owner |
| O2 | Every keyword in a field's list can be reordered, the standard one included. Default order = today's. | owner |
| O3 | Rules export to and import from a JSON file, so users can share them. | owner |
| O4 | XISF: only `<FITSKeyword>` values are sources, no `<Property>`. | owner |
| O5 | No hint or warning about `EGAIN`. | owner |
| O6 | Existing frames change only through an explicit Apply with a preview count. A rescan does not do it, and neither does an edit of the rules. | owner |
| O7 | After Apply, a raw calibration frame whose grouping value changed leaves its set and is grouped again like a newly scanned frame. Emptied sets are removed. Master-library sets take their master's new values. Lights keep their frame sets, nights and links. | owner |
| C1 | "Set by the user" is judged per field (§5.4), not by `frames.override`. On the owner's catalog, 1914 of the 1934 frames with `override = 1` are plate-solved lights; skipping them would leave every solved light without the fix. The owner asked for "frames I did not touch by hand". | controller |
| C2 | Stored headers are read with the scanner's own key extraction (§5.2). | controller |
| C3 | Frames that arrive by sync, and frames built by Perseus, follow the rules of the catalog they land in (§5.3). | controller |
| C4 | Text is trimmed and a blank value is absent, in XISF as in FITS. | controller |

## 4. Approaches considered

1. **Rules are an explicit input of header reading (chosen).** The functions that turn a header into a `Frame`
   take the rules as an argument. The snapshot builder and Apply call the same resolver. The compiler forces
   every production caller to decide. That matters because a master built and registered by Athenaeum must get
   rows identical to scanner ingestion (`docs/masters/README.md`). The cost is a larger diff, and tests pass the
   default rules.
2. **A fill step after parsing.** Smaller diff, but each caller has to remember the step. A forgotten caller is
   the silent drift §5.2 describes, which already happened several times.
3. **Process-wide "current rules" read inside the parser.** No signature changes. But the state is hidden:
   parallel tests see each other's rules, and both hosts must keep it fresh.

Rejected outright: computing values at display time (filters and calibration matching run SQL on the catalog
columns), and a side table of mapped values (every reader would need a join).

## 5. Design

### 5.1 Fields and default chains

The default chains are the **scanner's** current chains. These are the values the catalog holds today.

| Field id (JSON) | Column | Type | Default chain |
| ---- | ---- | ---- | ---- |
| `ccdTemp` | `ccd_temp` | number | `CCD-TEMP`, `DET-TEMP` |
| `setTemp` | `set_temp` | number | `SET-TEMP` |
| `gain` | `gain` | number | `GAIN` |
| `offset` | `offset` | number | `OFFSET` |
| `exptime` | `exptime` | number | `EXPTIME`, `EXPOSURE` |
| `focallen` | `focallen` | number | `FOCALLEN` |
| `xpixsz` | `xpixsz` | number | `XPIXSZ`, `PIXSIZE1` |
| `ypixsz` | `ypixsz` | number | `YPIXSZ`, `PIXSIZE2` |
| `object` | `object` | text | `OBJECT` |
| `filter` | `filter` | text | `FILTER` |
| `instrume` | `instrume` | text | `INSTRUME` |
| `telescop` | `telescop` | text | `TELESCOP` |

- **Pixel size in XISF.** For XISF, the built-in `<Property>` fallback (`Instrument:Sensor:XPixelSize` /
  `YPixelSize`) stays last, after the whole keyword chain. It is not part of the user's list and cannot be
  moved.
- **INSTRUME.** The scan root's "unique camera" suffix (` N<root id>`) is applied after resolution, exactly as
  today. Today that logic exists as `instrume_for_root` (private, `zip_store/scan.rs:296`) and as three inline
  copies in `scanner/mod.rs` (about l.680, 1209, 2261). The builder makes it one shared function, and Apply
  uses that function too.

### 5.2 One resolver, one key reader

Today the chains are written out three times, and the copies have drifted:
- the FITS frame builder, `fits_parser::build_frame_from_header`;
- the XISF frame builder, `parse_xisf_xml`: it has no `EXPOSURE` fallback;
- the metadata pane's "original", `stored_header::snapshot_from_keys`: it reads `CCDTEMP` for temperature and
  `EGAIN` for gain, and has no `EXPOSURE`, `PIXSIZE1` or `PIXSIZE2`.

The stored-header key reader also differs from the scanner's:
- **FITS:**
  - it skips HIERARCH cards (`stored_header.rs` ~l.312);
  - it cuts a value at the first doubled quote `''` (~l.321);
  - it does not join CONTINUE cards.
- **XISF:**
  - the stored header is the `NAME = value` dump written by `fits_parser::xisf_header_text`, not XML, so it
    falls through to `parse_keyword_eq_text`, which splits a value at `" / "`;
  - it keeps the first duplicate, where the scanner keeps the last.

After this change:

- **One resolver.** It takes a field, an ordered keyword list and a key map, and returns a value. The FITS and
  XISF frame builders, the snapshot, `recompute_override_flag_for_frames` and Apply all call it.
- **One key reader per stored format (C2).**
  - **FITS:** stored FITS text, 80-column cards joined by `\n`, goes through the scanner's own card parser
    (`fits_header_reader.rs`, including HIERARCH and CONTINUE).
  - **XISF:** the stored XISF dump gets a reader that inverts `xisf_header_text` exactly: split at the first
    `" = "`, no comment split, last duplicate wins. An older raw-XML stored blob, if one exists, is read with
    the scanner's XML keyword extraction.
  - **Other readers.** The metadata pane, `db/repair.rs`, sync ingest's CFA back-fill and the aperture read use
    the same readers. Each is meant to mirror the scanner already, so aligning them is a fix.
- **Key names.** Keys are compared after one normalisation function, applied both to header keys and to user
  input. The builder defines it from how the scanner stores keys (HIERARCH names keep case and inner spaces
  today). User input is valid if that function accepts it: non-empty, no `=`.
- **Numbers** parse with the FITS card number parser (`parse_fits_f64`) on every path. The XISF and
  stored-header paths use `str::parse::<f64>` today.
- **Text (C4).** Text is trimmed, and an empty or blank value counts as absent. This is new for XISF, which
  stores untrimmed text and lands a blank as `""` today (`fits_parser/mod.rs` ~l.764).
- **Wrong type.** A value that does not parse as the field's type counts as absent, and the next keyword is
  tried.
- **`CCDTEMP` and `EGAIN` leave the snapshot.** This is a fix. `EGAIN` is electrons per ADU, not the camera's
  gain setting. On the owner's catalog, 331 XISF masters carry `EGAIN` and no `GAIN`. Their "revert to
  original" would write about 0.24 into `gain`, and no light shot at gain 100 would ever match them. A user
  who wants `CCDTEMP` adds it.
- **The XISF `EXPOSURE` change.** XISF gaining the `EXPOSURE` fallback is intended: one table, both formats.
- **The snapshot's fields.** `FrameOriginalSnapshot` has no `set_temp` or `ypixsz`. They are not added; the
  pane does not edit them.

### 5.3 Rules: storage, semantics, callers

- **Storage.** A typed config `HeaderFallbackConfig` (`docs/settings/README.md` §1), stored as JSON under the
  settings key `catalog.header_fallbacks`, with `get_header_fallbacks` / `set_header_fallbacks` /
  `reset_header_fallbacks` on both hosts.
- **Shape.** For each field id:
  - `order`: the full ordered keyword list;
  - `retired`: the keywords the user has removed from that field, newest last. They are not shown, not
    exported, and used only by §5.4.

  A field absent from the document uses its default chain with no retired keywords. Defaults are not written
  to the DB.
- **Fixed keywords.** The keywords of the default chain are always in `order`: they can be moved but not
  removed. Moving one to the end is how it is turned down. User keywords can be removed, which moves them to
  `retired`. Re-adding a retired keyword takes it out of `retired`.
- **Saving.** `set_header_fallbacks` rejects a document that drops a default keyword, names an unknown field,
  repeats a keyword within a field, or carries an invalid keyword. The same validation function serves import
  (§5.7).
- **One rule set per run.** A scan reads the rules once at its start (`SettingsManager`) and uses that one set
  for every file. A change during a scan applies from the next scan, and Apply covers the rest.
- **Production callers that pass the catalog's rules:**
  - the scanner: serial and parallel walks, re-parse in place;
  - zip scan (`zip_store/scan.rs`) and the zip entry read in `api/zip.rs`;
  - master registration (`calibration_library/register.rs`);
  - the snapshot command;
  - `recompute_override_flag_for_frames` (`db/operations.rs` ~l.3993). It compares a frame with its snapshot,
    so it must read under the same rules or it flips `override` wrongly.
- **Callers that discard the Frame.** These are `calibration_library/light_cal.rs` ~l.755 and ~l.983 and
  `stacking/origin.rs` ~l.104/121: they read only ATH_* or CFA cards. They pass
  `HeaderFallbackConfig::default()` explicitly, with a comment saying why, or call a header-only function if
  one exists.
- **Sync ingest (C3).** Ingest keeps storing the sender's `frame_meta` as it came (the wire is frozen, and the
  sender's edits travel with it). The received frame then follows the receiver's rules through Apply like any
  other frame: the per-field test of §5.4 sees which values the receiver's header keywords explain.
- **Perseus (C3).** Perseus (`crates/perseus/src/run.rs` ~l.601) builds `frame_meta` with no catalog
  settings. It passes `HeaderFallbackConfig::default()` explicitly.
- **Tests** pass `HeaderFallbackConfig::default()`.

### 5.4 Apply to catalog: which values change (C1)

Two commands on both hosts:
- `preview_header_fallbacks` returns:
  - the count of frames per field whose value would change, and the total;
  - the count of values kept as set by the user;
  - the count of calibration sets that would be regrouped (§5.5);
  - the count of frames without a usable stored header.
- `apply_header_fallbacks` writes the changes with a `ProgressEmitter` and returns the same counts as applied.

The test runs for every frame and every listed field F, whatever `frames.override` says:

1. Read the frame's stored header through §5.2's key reader.
2. **Candidates.** Take F's `order` plus F's `retired`, and resolve each of those keywords on its own against the
   header. These are the values the header can explain.
3. **Is the value from the header?** The current column value counts as "from the header" if it is NULL, or if
   it equals one of those candidate values. For `instrume`, the comparison is made after the unique-camera
   suffix. Numbers compare exactly after `parse_fits_f64`. The builder may allow a relative tolerance of about
   1e-12 for values cataloged by the old `str::parse` path.
4. **From the header:** the new value is the resolver's answer under the current `order`, which may be NULL.
   If it differs from the column, it is written.
5. **Not from the header:** it is a value the user typed, or one a plate solve or another writer put there on
   purpose. It is kept and counted as "kept, set by you".

**What Apply writes**
- Apply writes only those columns. It never touches hashes (the bytes did not change), `override` or any other
  column.
- Removing a keyword or moving it down, then applying, undoes what it filled, because the retired keyword
  still counts as a candidate.
- A typed value that happens to equal one of the header's candidate values cannot be told apart and will
  follow the rules. This is an accepted limitation, written in the docs.

**When Apply runs**
- Writes run in batches, each batch a `BEGIN IMMEDIATE` transaction, so a running scan or monitor cycle
  interleaves safely.
- Frames without a stored header, or whose stored header has no keywords (the "No FITS keywords found"
  placeholder), are counted apart and never changed.
- **Nights.** For every frame set with a member whose `instrume` or `exptime` changed, the set's nights are
  re-derived with `sessions::rederive_for_frame_set` (nights are derived, never stitched;
  `docs/catalog/README.md`). Sessions group by `instrume`, and `total_exp_time` is kept by membership triggers
  only. Lights keep their frame-set membership.

### 5.5 Apply to catalog: calibration sets (O7)

A `calibration_set` row keeps its own copy of grouping values: `exptime`, `filter`, `ccd_temp`, `gain`,
`offset`, `instrume`, `telescop`, `focallen`, `temp_min` / `temp_max` (`db/schema.rs` ~l.941). The matcher reads
those copies (`calibration/configurable_matcher.rs` ~l.446, `dark_bias_groups.rs` ~l.1021).

After the frame writes, in the same Apply:

1. **Raw calibration frames** (non-master dark, flat, bias, dark-flat) whose value changed in a field their set
   is grouped by:
   - they leave their set (`calibration_set_frames`);
   - emptied sets are pruned with the shared helper the bulk edit uses, which spares master-library,
     superseded and provenance-anchor sets;
   - the frames are then grouped again through the scanner's own path,
     `calibration::scan_integration::create_calibration_sets_from_scan_with_masters`, as if newly scanned.

   Consumer links from lights to a pruned set go with it, as they do after a bulk edit. Those lights are matched
   again by the next matching run.
2. **A set that keeps other members** is treated exactly as `bulk_update_frame_metadata` treats a set that lost
   members. If that leaves its stored aggregates (`frame_count`, `temp_min` / `temp_max`) stale, that is a
   pre-existing finding to report, not to fix silently in this change.
3. **Master-library sets** (`is_master_library = 1`, one master per set) whose master's values changed take the
   master's new values in their stored columns. These are the values a fresh registration would write, which
   keeps "a built master gets rows identical to scanner ingestion".
4. **Built-master provenance.** The builder checks that the supersede/relink chain of masters built by Athenaeum
   is unaffected (`docs/masters/README.md`).
5. **Lights** keep their frame sets, nights membership and consumer links. Their new values are seen by the
   next matching run.

The confirmation dialog states what will happen: "Darks, flats and bias in K calibration sets will be grouped
again. Lights linked to them are matched again at the next matching."

### 5.6 Settings UI

A new section in **Settings → General**: `general.headerKeywords`, titled "FITS keywords". It follows the
Settings contract: registry entry, `useAutosaveDocument`, no Save button, no banner, `notify()` on failure
only.

```
FITS keywords                                   [Export…] [Import…] [Reset all]
Which header keyword each catalog field is read from, first match wins.
Your files are never changed.

Sensor temperature  ⠿ CCD-TEMP  ⠿ DET-TEMP  ⠿ MY-TEMP ×        [+ keyword]  ↺
Exposure            ⠿ EXPTIME   ⠿ EXPOSURE                     [+ keyword]  ↺
Gain                ⠿ GAIN                                      [+ keyword]  ↺
…
312 frames read differently under these rules:
Temperature 290 · Exposure 22                             [ Apply to catalog ]
9 values you set yourself are kept.
```

- **Keyword chips.** Default keywords are plain chips (no ×), user keywords have ×.
- **Reordering.** Chips reorder by drag and by keyboard (focus a chip, arrow keys move it). No new dependency:
  native drag events plus buttons. The builder may use an accessible pattern from `src/components/ui/` if one
  exists.
- **Resets.** ↺ resets one field to its default chain; "Reset all" resets every field. A reset moves removed
  user keywords to `retired`, so Apply can undo what they filled.
- **Preview line.** The preview loads when the section opens and again after each saved change (debounced).
  Zero changes hides the line.
- **Apply.** Apply confirms through the shared dialog (`ConfirmDialog` / `DialogShell`). The dialog shows the
  per-field counts and the calibration line of §5.5. Apply runs with progress and ends with one `notify()`
  naming the number of frames changed and sets regrouped.

### 5.7 Export and import

The file:
```json
{ "format": "athenaeum.header-fallbacks", "version": 1,
  "fields": { "ccdTemp": ["DET-TEMP", "CCD-TEMP", "MY-TEMP"] } }
```

- **Export** writes only the fields whose `order` differs from the default; `retired` never leaves the machine.
  The default file name is `athenaeum-fits-keywords.json`. The file holds no catalog data and no paths.
- **Import** replaces only the fields the file names. Other fields keep their current lists. Keywords that a
  replaced field loses go to its `retired`.
  - A confirmation lists each field that changes (old order → new order).
  - An imported list missing a default keyword gets it appended after the keyword that precedes it in the
    default chain, so a file from a later version that adds a default keyword still imports.
- **Validation** is the core function of §5.3. An unknown `format`, a `version` above 1, an unknown field, a
  non-string or invalid keyword, or a duplicate rejects the whole file with a message that names the problem
  (`notify()` error). Nothing is applied.
- **Hosts.** Desktop uses the existing `@tauri-apps/plugin-dialog` save/open dialogs and `@tauri-apps/plugin-fs`
  (or a thin core command), inside `src/api/desktop.ts` only. Web uses a browser download and a file input.
- Import changes the rules only. The preview line then shows what Apply would change.

### 5.8 Logging

- `tracing` only.
- Apply logs one summary line: frames changed per field, values kept, frames without a header, sets regrouped.
- An import rejection logs the reason.
- Any new field names go into the logging dictionary (`docs/logging/README.md`).
- Every new command wears `#[tracing::instrument(skip_all, err)]`, and the preview command wears
  `level = "debug"`.

## 6. Constraints (from `CLAUDE.md`)

- **Two backends in sync.** Every command is on Tauri and Axum in the same change. Logic lives in
  `athenaeum-core/src/api/`.
- **Serde.** camelCase at the boundary. `HeaderFallbackConfig` and the preview/apply result types go into
  `ts_export.rs`, and the TS types into `src/types/models.ts`.
- **Scanner.** The scanner's `frames.override = 1` whole-row rule for a modified file is untouched.
- **Disk rule.** Test catalogs and copies go under `~/.athenaeum-acc/` with a `REGISTRY.tsv` row.
- **Builds.** This Mac is shared with another job: one compiler at a time, `CARGO_BUILD_JOBS=4`,
  `--test-threads=4`, and wait while `pgrep -fl 'cargo|rustc'` shows another build.
- **No third-party program names** in code, comments or docs.

## 7. Docs to update with the code

- `docs/catalog/README.md`:
  - the sensor-temperature bullet becomes "FITS keyword fallbacks": the field table, the rules, Apply and its
    per-field test, and the accepted limitation;
  - the stored-header readers.
- `docs/calibration/matching.md`: Apply regroups changed raw calibration frames.
- `docs/settings/README.md`: the new section in the General list.
- `docs/architecture/commands.md`: the command count.
- **Release notes:**
  - the user-facing feature;
  - the XISF `EXPOSURE` fallback and trimmed XISF text;
  - the metadata pane no longer offering `EGAIN` as the original gain.

## 8. Risks the builder must look at

- **Many readers change.** Aligning the stored-header readers changes what the metadata pane, `db/repair.rs`,
  sync ingest's CFA back-fill and the aperture read see on headers with HIERARCH, CONTINUE, doubled quotes or
  `" / "` inside an XISF value. Each change must be a move toward the scanner's reading; a test pins one example
  per difference.
- **Regrouping cost.** Regrouping calls the scanner's grouping path on an arbitrary frame list. The builder
  confirms it accepts already-cataloged frames and does not assume "new in this scan" elsewhere.

## 9. Acceptance

1. **Unit tests.**
   - The resolver: order, type rejection, blank text, a user-first order, a default keyword demoted to the end.
   - The per-field test: NULL filled; a header value replaced after reordering; a typed value kept; a
     plate-solved frame filled; a retired keyword's value cleared; the unique-camera suffix kept.
   - Parity: one header fixture read through the FITS builder, the XISF builder and the stored-header path
     gives the same value for every field present in each, under default and custom rules. That includes a
     HIERARCH card, a CONTINUE value, a doubled quote, and an XISF value containing `" / "`.
   - Import validation, one test per rejection reason. The export/import round trip.
   - Calibration: darks grouped with NULL temperature, then Apply with temperatures:
     - they are regrouped by temperature;
     - the emptied set is gone;
     - a master-library set carries its master's new values;
     - a light's frame-set membership is unchanged.
2. **The real dev catalog** (a copy under `~/.athenaeum-acc/`, never the live DB). Run `preview_header_fallbacks`
   with default rules and explain every non-zero count. Expected:
   - frames cataloged before 0.7.1 with only `DET-TEMP`;
   - XISF frames with `EXPOSURE` but no `EXPTIME`;
   - XISF text with padding or blanks;
   - values changed by the aligned stored-header reader.

   Anything else is a parity bug to fix before merge.
3. **Same catalog, temperature order reversed** (`DET-TEMP` first).
   - The preview counts the frames that carry both keywords with different values, and Apply writes them.
   - A second preview reads 0.
   - Typed values and plate-solve values are unchanged; check with SQL before and after.
   - Light frame-set memberships are unchanged.
4. **The owner's smoke** in `npm run tauri dev` on the dev DB: add a keyword, see the counter, apply, see the
   values in the file browser and the metadata pane, export and re-import. Recorded in
   `docs/superpowers/open-items.md` until it passes.

## 10. Left to the builder

- Module placement of the resolver and the config, plus the exact command and type names, within the shapes
  above.
- How the parser functions take the rules: a new argument versus `_with_rules` variants that keep default-rule
  wrappers for tests. The rule is that no production caller may use the wrapper.
- Batch size and progress granularity of Apply.
- Debounce timing of the preview, and whether the preview is cancelled when a newer one starts.
- The section's exact copy, within the sketch's meaning.
