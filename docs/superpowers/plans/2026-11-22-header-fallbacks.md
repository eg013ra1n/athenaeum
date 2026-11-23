# FITS keyword fallbacks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task, run under the `lean-execution` skill (it overrides SDD where they conflict). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The user orders the header keywords each of twelve catalog fields is read from; scanning reads under those
rules, and an explicit, previewed Apply re-reads frames already in the catalog from their stored headers, regrouping
the raw calibration frames whose grouping values change (GitHub issue #21, release 0.7.2).

**Architecture:** One typed config `HeaderFallbackConfig` (settings key `catalog.header_fallbacks`) is an explicit
argument of every function that turns a header into a `Frame`. One key reader per stored format (the scanner's own card
parser and XISF keyword extraction) feeds one resolver that the FITS builder, the XISF builder, the metadata pane's
snapshot, `recompute_override_flag_for_frames` and Apply all call. Apply is a per-field test over stored headers, written
in `BEGIN IMMEDIATE` batches, followed by one night re-derivation per affected frame set and one calibration
transaction that detaches, prunes and regroups through the scanner's own grouping path.

**Tech Stack:** Rust (rusqlite, serde, ts-rs 12, quick-xml), Tauri 2, Axum, React/TS (vitest, Testing Library).

**Spec:** `docs/superpowers/specs/2026-11-22-header-fallbacks-design.md` (approved; owner decisions O1–O7 and controller
calls C1–C4 are settled). Read it beside this plan. Section numbers below (§5.4 …) are the spec's.

## Decisions this plan makes (spec §10 and gaps found in the code)

| # | Decision | Reason |
| ---- | ---- | ---- |
| D1 | Module `header_fallbacks` (`mod.rs` config, `resolve.rs`, `apply.rs`, `calibration.rs`, `transfer.rs`); key readers in `fits_parser/header_keys.rs`; command handlers in `api/header_fallbacks.rs`; host wrappers in the `settings` modules of both hosts. | The rules are a catalog setting, the readers belong to the parser, and one domain module keeps the seven commands together. |
| D2 | Every frame-building parser takes a new last argument `rules: &HeaderFallbackConfig`; no default-rule wrapper survives Task 3. Scanner, zip-scan, `register_master` and `api` entry points that already hold the catalog `conn` load the rules from it once at their start; their internals take `rules`. | The compiler forces every parser caller to decide (spec §4 approach 1) while scanner/zip tests keep their current calls; "one rule set per run" holds because loading happens once per entry point. |
| D3 | Apply pages are 500 frames by `frames.id`, one `SavepointGuard` (= `BEGIN IMMEDIATE` on an autocommit connection, `db/operations.rs:478-499`) per page, one progress event per page; after the last page the nights of the affected frame sets are re-derived once per set (one transaction per set), then the calibration writes run in one more transaction. | A page only writes frame columns (about 500 `UPDATE`s), so it holds the lock briefly and a scan or monitor cycle interleaves; night re-derivation deletes and recreates every night of a set (`sessions/mod.rs:83`), so it runs once per set after the pages, never inside a page. Deferring calibration frames to the final transaction means a crash before it leaves those changes still visible in the next preview, so a re-Apply finishes them; a crash between the last page and the night pass leaves those sets' nights as they were until their next re-derive (a rescan or bulk edit). |
| D4 | The preview runs once on mount and 800 ms after each successful save (`savedAt`); at most one preview is in flight and a save during it schedules exactly one follow-up; no server-side cancel. | The preview only reads, so a stale one costs time, not data; coalescing bounds the load to one blocking task, and a generation check discards any stale answer. |
| D5 | Copy of the section: see Task 7 (exact strings). | Within the sketch's meaning (§5.6). |
| D6 | `normalize_keyword(raw)` = `raw.trim().to_uppercase()`, `None` when the result is empty or contains `=`. Applied to every header key when a key map is built and to every user keyword. | FITS keywords are case-insensitive; the scanner already upper-cases standard keys; upper-casing both sides lets a user match a HIERARCH name (`ESO DET TEMP`) whatever its case, inner spaces kept. |
| D7 | A non-finite number (`NaN`, `inf`) counts as absent, like a value of the wrong type. | SQLite stores NaN as NULL, so a NaN answer would make the preview count the same frame forever. |
| D8 | In Apply's test the current text value is trimmed before comparison and a blank column counts as NULL; the write compares the new value with the untrimmed column. | Old XISF rows hold untrimmed text (`fits_parser/mod.rs:764-767`); without the trim they would read as "set by you" and never get C4's fix. |
| D9 | `instrume`: the column's own ` N<digits>` suffix is preserved by Apply (base compared with the candidates, suffix re-attached); only a NULL column takes the suffix the frame's scan root would give (`instrume_for_scan`). | Apply must never change the unique-camera suffix (anti-goal), and a master registered without a suffix inside a unique-camera root keeps its state. |
| D10 | XISF `xpixsz`/`ypixsz`: when none of the field's candidate keywords is in the stored header and the column has a value, Apply leaves it and does not count it as kept. | The stored XISF header records `<FITSKeyword>` lines only (`fits_parser/mod.rs:197-278`), so the `<Property>` fallback (`:833-839`) is invisible to Apply; see spec conflict S1. |
| D11 | A raw calibration frame whose set is frozen lineage (`superseded_by_set_id IS NOT NULL`, or a `master_provenance.source_set_id`) keeps its membership; its frame columns are still written. Logged as `lineage_kept`. | The superseded guard keys on membership (`calibration/superseded_guard.rs:38-47`); taking a frame out would make the regroup mint a duplicate of a lineage a master replaced (§5.5 step 4); see spec conflict S2. |
| D12 | The server keeps `retired`: `set_header_fallbacks` merges `retired = (stored.retired ++ incoming.retired ++ (stored.order − incoming.order)) − incoming.order`, newest last; reset and import use the same merge. | The frontend never shows `retired`, so it cannot be trusted to keep it; one merge makes ↺, Reset all, × and Import all retire what they drop (§5.3, §5.6). |
| D13 | `HeaderFallbackConfig::default()` is the full document (all twelve fields, default chains, empty `retired`); the DB stores the compact form (fields with a non-default order or a non-empty `retired`), and an empty compact form deletes the settings row. | The UI needs the default chains for ↺ through `get_settings_defaults`; "defaults are not written to the DB" (§5.3). |
| D14 | Loading the stored rules inserts a default keyword the document lacks after its predecessor in the default chain (first if it has none), the import rule of §5.7; an unknown field id is skipped with a `warn!`. A document that still fails to decode: `load` errs (Settings, preview and Apply show the error); `load_for_scan` logs `error!` and scans with the defaults. | A later version adding a default keyword must not throw away the user's rules; a scan must not stop on a bad settings row. |
| D15 | Import files: at most 1 MiB, a UTF-8 BOM is stripped, `version` must be exactly `1`, a missing `fields` object is refused. Desktop reads/writes the file in core by path (`PathPolicy::AllowAll`); the web passes text and its routes use `PathPolicy::AllowedRoots(vec![])`, so a path is refused (403). | `fs:default` (`crates/athenaeum-tauri/capabilities/default.json`) grants no write to user-chosen paths; a thin core command is the spec's allowed alternative (§5.7). |
| D16 | Apply mirrors the bulk edit's auto-publish trigger (`api/files.rs:118-137`) for the frame sets of changed frames. | Apply is a bulk metadata edit by rules; a project's gate may read the changed values. |
| D17 | A second Apply on the same `ServiceContext` while one runs is refused with `ApiError::Conflict` (HTTP 409); the flag lives on `ServiceContext`, not in a process static. | Double click or a second window of one host would otherwise run two Applies at once. Desktop and web sharing one catalog are protected by `BEGIN IMMEDIATE` (each page and the calibration transaction serialize on the write lock, and Apply is idempotent), not by the flag; a context-held flag also keeps api tests from racing under `--test-threads=4`. |

## Spec conflicts and gaps (decided above, flagged for the controller)

- **S1** §5.1 vs §5.4 on the XISF `<Property>` pixel size: decided in D10.
- **S2** O7 vs the lineage chain of §5.5 step 4: decided in D11.
- **S3** §5.1 counts three inline unique-camera copies in `scanner/mod.rs`; there are five sites plus one in
  `zip_store/scan.rs` with two behaviours: strip-then-append (`scanner/mod.rs:677-690`, `:1209-1227`,
  `zip_store/scan.rs:296-309`) and append-only (`scanner/mod.rs:740-744`, `:2261-2266`, `:2355-2359`,
  `zip_store/scan.rs:557-561`). Task 3 routes all through one pair; the only behaviour change is that a header INSTRUME
  already ending in ` N<digits>` in a unique-camera root no longer gets a second suffix.
- **S4** The dark/dark-flat set reuse query (`calibration/scan_integration.rs:711-805`) ignores temperature, so when only
  some darks of a set change temperature and their time window overlaps the surviving set, the scanner's path puts them
  back into it. Pre-existing; reported in Task 9 with the stale-aggregate finding of §5.5 step 2 and the stale `focallen`
  copy of a dark set (`scan_integration.rs:840-843`).
- **S5** `rustafits_processor/mod.rs:52` and `plate_solve/hints.rs:138` also build frames / snapshots; neither is in
  §5.3's list. Task 3 handles both.

## Global Constraints

- Worktree only: `W=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.claude/worktrees/header-fallbacks`
  (branch `header-fallbacks`). Never touch the main checkout or another worktree.
- **One build at a time (16 GB Mac, shared with another job).** Before every cargo command wait (Monitor until-loop)
  while `pgrep -fl 'cargo|rustc'` lists a process. Every cargo command is
  `cd "$W" && CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR="$W/target-agent" cargo …` and every test run
  ends with `-- --test-threads=4`. In the steps below, `CT <filter>` means
  `… cargo test -p athenaeum-core --lib <filter> -- --test-threads=4` and `CC <crate>` means `… cargo check -p <crate>`.
  Scope tests to the module; only Task 9 runs whole suites.
- TS types are generated: `… cargo test -p athenaeum-core --test ts_contract` with `TS_RS_WRITE=1` added to the env
  (regenerates `src/types/*.ts`), then the same without it (must pass). Never hand-edit `src/types/models.ts`; new
  types go in the `models.ts` list of `ts_export.rs:70-322`.
- Tests pass on Windows, Linux and macOS: no bare path compared with log text, no OS separator in an assert (build
  expected paths with `Path::join`). Test names, and names of files tests create, use letters, digits, `_` and `-` only.
- Acceptance runs on a COPY of `~/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db`, never the live
  file; scratch data under `~/.athenaeum-acc/` with a `REGISTRY.tsv` row.
- Commits: author is the user (`eg013ra1n`, the repo's git config); every message ends with the attribution trailer
  lines the session's system reminder gives (never a hard-coded copy).
- Implementers: `rust-engineer` (sonnet) for Rust tasks, `frontend-dev` for UI tasks (marked per task). UI copies the
  look of `LoggingSettings.tsx` / `SettingsSection` and uses `ConfirmDialog` for confirmations.
- Hosts: each new command is a Tauri command in `crates/athenaeum-tauri/src/commands/settings.rs` (registered in
  `invoke_handler![]`, `crates/athenaeum-tauri/src/lib.rs:310-318`) and an Axum route in
  `crates/athenaeum-web/src/routes/settings.rs` (registered in `build_router`, `routes/mod.rs:106-115`) in the same
  task; logic in `athenaeum-core/src/api/header_fallbacks.rs`; `routes/parity_tests.rs` must pass.
  `preview_header_fallbacks` adds `level = "debug"` to the instrument attribute. Long commands (preview, apply) run in
  `spawn_blocking` on both hosts (pattern: `commands/analysis.rs:53-68`, `routes/analysis.rs:127-145`).
- New commands, exact names (330 → 337): `get_header_fallbacks`, `set_header_fallbacks`, `reset_header_fallbacks`,
  `preview_header_fallbacks`, `apply_header_fallbacks`, `export_header_fallbacks`, `plan_header_fallbacks_import`.
  Progress event: `header-fallbacks-progress`.
- Settings key `catalog.header_fallbacks` (`settings::keys::CATALOG_HEADER_FALLBACKS`); typed config, not in
  `defaults::all()`.
- Default chains (spec §5.1, verbatim): `ccdTemp` → `CCD-TEMP`, `DET-TEMP`; `setTemp` → `SET-TEMP`; `gain` → `GAIN`;
  `offset` → `OFFSET`; `exptime` → `EXPTIME`, `EXPOSURE`; `focallen` → `FOCALLEN`; `xpixsz` → `XPIXSZ`, `PIXSIZE1`;
  `ypixsz` → `YPIXSZ`, `PIXSIZE2`; `object` → `OBJECT`; `filter` → `FILTER`; `instrume` → `INSTRUME`;
  `telescop` → `TELESCOP`. Columns are the snake_case names (`ccd_temp`, `set_temp`, …). Number fields: the first
  eight; text fields: the last four.
- New log field names (added to the logging spec's dictionary in Task 4): `no_header`, `sets_regrouped`,
  `master_sets`, `lineage_kept`, `by_field`. Existing names used: `count`, `kept`, `key`, `error`, `reason`,
  `duration_ms`, `frame_id`, `path`.
- Untouched by this work: the scanner's whole-row `frames.override = 1` rule for a modified file; the sync wire and
  `frame_meta` (C3); `files` hashes; `frames.override` (Apply never writes it).
- No new dependency (frontend drag and drop is native events).

The review-focus items (reordered chain with two differing values, `NaN`/`INF`/`n/a`/blank values, case or space
variants of a default keyword, a BOM/CRLF/oversized/binary import file, a second Apply or a scan during Apply) are
pinned by named tests in Tasks 1, 2, 5, 6 and 7.

---

### Task 1: One key reader per stored format, one resolver, parity (`rust-engineer`)

**Files:**
- Create: `crates/athenaeum-core/src/header_fallbacks/mod.rs`, `crates/athenaeum-core/src/header_fallbacks/resolve.rs`
- Create: `crates/athenaeum-core/src/fits_parser/header_keys.rs`, `crates/athenaeum-core/src/fits_parser/parity_tests.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (`pub mod header_fallbacks;` beside `fits_parser`, l.10)
- Modify: `crates/athenaeum-core/src/fits_parser/fits_header_reader.rs` (struct 10-15, `parse_from_reader` 30-125,
  `parse_fits_f64` 248-255)
- Modify: `crates/athenaeum-core/src/fits_parser/mod.rs` (mod list 3-5; `frame_and_text_from_fits_header` 171-175;
  `xisf_header_text` 197-278; `parse_fits` 282-286; `build_frame_from_header` 443-627, chains at 456-492;
  `parse_xisf` 630-635; `parse_xisf_xml` 639-1001, keyword loop 674-701, chains 763-839)
- Modify: `crates/athenaeum-core/src/fits_parser/stored_header.rs` (dispatcher 72-122, snapshot 125-231, old readers
  296-391 removed, tests 408-731)
- Modify: `crates/athenaeum-core/src/db/operations.rs:4095-4096, 4214-4215` (snapshot call sites)

**Interfaces:**
- Produces (`crate::header_fallbacks`):
  - `pub enum HeaderField { CcdTemp, SetTemp, Gain, Offset, Exptime, Focallen, Xpixsz, Ypixsz, Object, Filter, Instrume, Telescop }`
    — `Copy, Ord, Hash, Serialize, Deserialize, ts_rs::TS`, `#[serde(rename_all = "camelCase")]` (ids `ccdTemp` …
    `telescop`); declaration order = spec table order. Methods: `pub const ALL: [HeaderField; 12]`,
    `pub fn id(self) -> &'static str`, `pub fn column(self) -> &'static str`, `pub fn kind(self) -> FieldKind`,
    `pub fn default_chain(self) -> &'static [&'static str]`.
  - `pub enum FieldKind { Number, Text }`
  - `pub struct FieldRule { pub order: Vec<String>, #[serde(default)] pub retired: Vec<String> }` (camelCase, TS)
  - `pub struct HeaderFallbackConfig { #[serde(default)] pub fields: BTreeMap<HeaderField, FieldRule> }` (camelCase,
    TS); `Default` = all twelve fields, default chains, empty `retired` (D13);
    `pub fn order(&self, f: HeaderField) -> &[String]` (absent field → its default chain, from a
    `static DEFAULT: LazyLock<HeaderFallbackConfig>`); `pub fn retired(&self, f: HeaderField) -> &[String]`.
- Produces (`crate::header_fallbacks::resolve`, re-exported from `header_fallbacks`):
  - `pub enum FieldValue { Number(f64), Text(String) }`; `pub fn same_value(a: &FieldValue, b: &FieldValue) -> bool`
    (text exact; numbers `a == b || |a − b| <= 1e-12 · max(|a|, |b|)`).
  - `pub fn resolve_keyword(field: HeaderField, keyword: &str, keys: &HeaderKeyMap) -> Option<FieldValue>`
  - `pub fn resolve_field(field: HeaderField, order: &[String], keys: &HeaderKeyMap) -> Option<FieldValue>`
  - `pub struct ResolvedFields { pub ccd_temp, set_temp, gain, offset, exptime, focallen, xpixsz, ypixsz: Option<f64>, pub object, filter, instrume, telescop: Option<String> }`
    (`Default, Clone, Debug, PartialEq`) with `pub fn get(&self, f: HeaderField) -> Option<FieldValue>` and
    `pub fn set(&mut self, f: HeaderField, v: Option<FieldValue>)`.
  - `pub fn resolve_all(rules: &HeaderFallbackConfig, keys: &HeaderKeyMap) -> ResolvedFields`
- Produces (`crate::fits_parser::header_keys`, `pub mod`):
  - `pub fn normalize_keyword(raw: &str) -> Option<String>` (D6)
  - `pub struct HeaderKeyMap` (normalised key → raw value): `pub fn get(&self, key: &str) -> Option<&str>`,
    `pub fn has_keywords(&self) -> bool` (any key other than `COMMENT`/`HISTORY`), `pub fn into_legacy_map(self) -> HashMap<String, String>`,
    `pub(crate) fn from_fits_header(h: &FitsHeader) -> Self` (card order, first wins),
    `pub(crate) fn from_last_wins(pairs: impl IntoIterator<Item = (String, String)>) -> Self`,
    `pub(crate) fn from_first_wins(pairs: impl IntoIterator<Item = (String, String)>) -> Self`.
  - `pub fn stored_header_key_map(format: FileFormat, text: &str) -> HeaderKeyMap`
  - `pub(crate) const XISF_DUMP_BANNER: &str = "XISF FITS Keywords:\n==================\n\n";`
- Produces (`fits_header_reader`): `pub(crate) fn FitsHeader::from_header_text(text: &str) -> FitsHeader`,
  `pub(crate) fn FitsHeader::keywords_in_order(&self) -> impl Iterator<Item = (&str, &str)>`,
  `pub(crate) fn parse_fits_f64(s: &str) -> Option<f64>` (visibility only).
- Produces (`fits_parser`): `pub(crate) fn build_frame_from_header(header: &FitsHeader, file_id: i64, path: &Path, rules: &HeaderFallbackConfig) -> Result<Frame>`;
  `fn parse_xisf_xml(content: &[u8], file_id: i64, path: &Path, rules: &HeaderFallbackConfig) -> Result<Frame>`;
  `pub(crate) fn xisf_keyword_pairs(content: &[u8], label: &Path) -> Result<Vec<(String, String)>>`. The public
  parse functions (`parse_fits_with_header`, `parse_fits_header_bytes`, `parse_xisf_header_bytes`, `parse_xisf`,
  `parse_fits`) keep their signatures in this task and pass `&HeaderFallbackConfig::default()` with a
  `// Task 3 makes the rules a parameter` comment — the one intended temporary default.
- Produces (`stored_header`): `parse_stored_header_keys(format, text) -> HashMap<String, String>` keeps its signature
  and becomes `stored_header_key_map(format, text).into_legacy_map()`;
  `pub fn snapshot_from_keys(frame_id: i64, keys: &HeaderKeyMap, rules: &HeaderFallbackConfig) -> FrameOriginalSnapshot`.

**Approach notes (the parts the signatures do not fix):**
- `FitsHeader` gains `order: Vec<String>` (a key is pushed when first inserted). The card loop of `parse_from_reader`
  moves into a private `CardAccumulator`; `from_header_text` feeds it the text line by line (one trailing `\r` stripped,
  short lines padded to 80 bytes, stopping at `END`, never failing). Same accumulator ⇒ same values as the scan (C2).
- `HeaderKeyMap` keys go through `normalize_keyword`; a key it refuses is dropped. `into_legacy_map` drops the keys
  `COMMENT` and `HISTORY`, trims every value and drops blank ones — the old contract (UPPERCASE keys, non-empty
  values) with the scanner's values (`get_str` semantics).
- `stored_header_key_map` dispatch:
  - FITS: `HeaderKeyMap::from_fits_header(&FitsHeader::from_header_text(text))`; if it `!has_keywords()`, the old
    `parse_keyword_eq_text` (kept, renamed `legacy_eq_text_pairs`) through `from_first_wins` — hand-written
    `KEY = value` text, never a scanner output.
  - XISF: text starting with `XISF_DUMP_BANNER` → the dump inverse: each later line split at the first `" = "`
    (a line ending in `" ="` is an empty value), name normalised, value verbatim, no comment split, last duplicate
    wins (`from_last_wins`); else text containing `<?xml` → `xisf_keyword_pairs` (the scanner's extraction) through
    `from_last_wins`; else the legacy eq-text. The placeholder `Header extracted from: …\n(No FITS keywords found in XISF)`
    yields no keyword.
- `xisf_keyword_pairs` = the `<?xml`…`</xisf>` slice plus the `FITSKeyword` `Event::Empty` loop now in
  `xisf_header_text`, returning `(name, value)` with outer `'…'` stripped, in document order. `xisf_header_text`
  formats those pairs (output byte-identical to today, `XISF_DUMP_BANNER` first). `parse_xisf_xml` keeps its single
  event loop, with its `FITSKeyword` arm calling the same per-element helper; `fits_keywords` (the case-sensitive map
  every non-listed field still reads) is built from the pairs with last-wins inserts, exactly as today.
- Both builders compute `let r = resolve_all(rules, &keys)` (FITS: `HeaderKeyMap::from_fits_header(header)`; XISF:
  `HeaderKeyMap::from_last_wins(pairs.clone())`) and take the twelve fields from `r`; XISF `xpixsz`/`ypixsz` then
  `.or_else(...)` the existing `Instrument:Sensor:XPixelSize`/`YPixelSize` property fallback. Every other field's code
  is unchanged.
- Resolver: Number = `parse_fits_f64(raw.trim())` filtered `is_finite()` (D7); Text = `raw.trim()`, `None` when empty
  (C4). Wrong type ⇒ `None` ⇒ next keyword.
- `snapshot_from_keys` fills `object, filter, instrume, telescop, focallen, xpixsz, gain, offset, exptime, ccd_temp`
  from `resolve_all`; `imagetyp, binning, date_obs, ra, dec, rotation` keep their current code, reading
  `keys.get(..)` (no `CCDTEMP`, no `EGAIN`). `db/operations.rs:4095-4096` and `:4214-4215` call
  `stored_header_key_map` + `snapshot_from_keys(.., &HeaderFallbackConfig::default())` (Task 3 threads the rules).

- [ ] **Step 1: Write the failing tests**

`fits_header_reader.rs` tests:
```rust
#[test] fn from_header_text_matches_from_bytes_for_the_same_cards() {
    // block with: HIERARCH ESO DET TEMP = -7.25 / c, OBJECT = 'O''Brien', FILTER = 'Long filter &' + CONTINUE '  two',
    // a duplicate GAIN (first wins), END. text = FitsHeader::from_bytes(&block)?.to_header_text()
    let a = FitsHeader::from_bytes(&block).unwrap();
    let b = FitsHeader::from_header_text(&a.to_header_text());
    assert_eq!(a.keywords_in_order().collect::<Vec<_>>(), b.keywords_in_order().collect::<Vec<_>>());
    assert_eq!(b.get_str("OBJECT").as_deref(), Some("O'Brien"));
}
#[test] fn from_header_text_pads_short_lines_and_strips_crlf() {
    let h = FitsHeader::from_header_text("EXPTIME = 300.0\r\nEND");
    assert_eq!(h.get_f64("EXPTIME"), Some(300.0));
}
```
`header_keys.rs` tests:
```rust
#[test] fn normalize_keyword_trims_uppercases_and_refuses_empty_or_equals() {
    assert_eq!(normalize_keyword(" det-temp "), Some("DET-TEMP".into()));
    assert_eq!(normalize_keyword("eso det temp"), Some("ESO DET TEMP".into()));
    assert_eq!(normalize_keyword("   "), None);
    assert_eq!(normalize_keyword("A=B"), None);
}
#[test] fn xisf_dump_reader_inverts_xisf_header_text() {
    let dump = format!("{XISF_DUMP_BANNER}FILTER = Ha / 7nm\nGAIN = 100\nGAIN = 120\nOBJECT = \n");
    let m = stored_header_key_map(FileFormat::XISF, &dump);
    assert_eq!(m.get("FILTER"), Some("Ha / 7nm"));
    assert_eq!(m.get("GAIN"), Some("120"));
    assert_eq!(m.get("OBJECT"), Some(""));
}
#[test] fn xisf_placeholder_has_no_keywords() {
    let p = "Header extracted from: /a=b/x.xisf\n(No FITS keywords found in XISF)";
    assert!(!stored_header_key_map(FileFormat::XISF, p).has_keywords());
}
#[test] fn fits_hand_dump_falls_back_to_eq_text() {
    assert_eq!(stored_header_key_map(FileFormat::FITS, "SIMPLE = T\nFOCALLEN = 140.6").get("FOCALLEN"), Some("140.6"));
}
#[test] fn hierarch_keys_match_case_insensitively() { /* HIERARCH Eso Det Temp = -7.25 → get("ESO DET TEMP") == Some("-7.25") */ }
```
`resolve.rs` tests (keys built with `HeaderKeyMap::from_first_wins`):
```rust
#[test] fn resolver_takes_the_first_usable_keyword_in_order() { /* CCD-TEMP -10, DET-TEMP -5.5 → Number(-10.0) */ }
#[test] fn resolver_skips_a_value_of_the_wrong_type() { /* CCD-TEMP "n/a", DET-TEMP "-5.5" → Number(-5.5) */ }
#[test] fn resolver_treats_nan_inf_text_and_blank_as_absent() {
    for bad in ["NaN", "inf", "-INF", "n/a", "", "   "] { /* CCD-TEMP=bad, DET-TEMP=12 → Some(Number(12.0)) */ }
}
#[test] fn user_first_order_wins_over_the_standard_keyword() { /* order [MY-TEMP, CCD-TEMP, DET-TEMP] → MY-TEMP's value */ }
#[test] fn default_keyword_demoted_to_the_end_is_tried_last() { /* order [DET-TEMP, CCD-TEMP] with both → DET-TEMP */ }
#[test] fn text_values_are_trimmed_and_blank_is_absent() { /* FILTER "  Ha  " → Text("Ha"); OBJECT "  " → None */ }
#[test] fn fortran_d_notation_parses_on_every_path() { /* EXPTIME "1.2D+02" → Number(120.0) */ }
```
`fits_parser/mod.rs` tests:
```rust
#[test] fn xisf_builder_gains_the_exposure_fallback() { /* only EXPOSURE=120 → frame.exptime == Some(120.0) */ }
#[test] fn xisf_builder_trims_text_and_drops_blank() { /* FILTER "'Ha  '" → Some("Ha"); OBJECT "''" → None */ }
#[test] fn xisf_pixel_size_property_stays_after_the_whole_chain() {
    // <Property id="Instrument:Sensor:XPixelSize" value="3.76"/> and PIXSIZE1=4.63 → 4.63; Property alone → 3.76;
    // rules xpixsz [MY-PX, XPIXSZ, PIXSIZE1] with MY-PX=2.4 and the Property → 2.4
}
```
`parity_tests.rs` (spec §9.1 parity):
```rust
// K = the shared keyword set of both formats: DET-TEMP -5.5, ESO DET TEMP -7.25, EXPOSURE 120, OBJECT M42,
// FILTER 'Ha / 7nm', GAIN 100, PIXSIZE1 3.76, INSTRUME Cam (no duplicates: FITS keeps the first, XISF the last).
#[test] fn parity_fits_builder_and_stored_text_agree_under_default_and_custom_rules() {
    // 2880-byte block: K, plus HIERARCH spelling of ESO DET TEMP, OBJECT='O''Brien' (doubled quote),
    // FILTER='Long filter &' CONTINUE '  two', END.
    for rules in [HeaderFallbackConfig::default(), custom()] { // custom: ccdTemp [ESO DET TEMP, CCD-TEMP, DET-TEMP], exptime [EXPOSURE, EXPTIME]
        let header = FitsHeader::from_bytes(&block).unwrap();
        let frame = build_frame_from_header(&header, 0, Path::new("p.fits"), &rules).unwrap();
        let stored = resolve_all(&rules, &stored_header_key_map(FileFormat::FITS, &header.to_header_text()));
        assert_eq!(fields_of(&frame), stored); // fields_of: the twelve fields of a Frame as ResolvedFields
    }
}
#[test] fn parity_xisf_builder_dump_and_raw_xml_agree_under_default_and_custom_rules() {
    // XML with FITSKeyword: K, plus GAIN 100 then GAIN 120 and OBJECT '  M42  '. frame via parse_xisf_xml; dump via
    // xisf_header_text; raw XML text itself.
    // assert fields_of(&frame) == resolve_all(rules, dump map) == resolve_all(rules, xml map) for both rule sets;
    // assert filter == Some("Ha / 7nm"), gain == Some(120.0)
}
#[test] fn parity_one_keyword_set_gives_the_same_fields_through_fits_and_xisf() {
    // exactly K, once as a FITS block through build_frame_from_header, once as FITSKeyword elements through
    // parse_xisf_xml, and each through its stored text (stored_header_key_map): for both rule sets
    // fields_of(fits) == fields_of(xisf) == resolve_all(rules, stored fits map) == resolve_all(rules, stored xisf map),
    // compared on the fields K carries (spec 9.1)
}
```
`stored_header.rs` tests (existing ones adapted to `HeaderKeyMap` + default rules; the XISF-dump test at
`stored_header.rs:531-552` renamed `parses_the_xisf_keyword_dump`, its comment rewritten to say the dump is Athenaeum's
own `xisf_header_text`, and its `parse_xisf_xml_text(dump).is_empty()` line removed), plus:
```rust
#[test] fn snapshot_ignores_ccdtemp_and_egain() { /* CCDTEMP=-10, EGAIN=0.24 only → ccd_temp None, gain None */ }
#[test] fn snapshot_reads_exposure_and_pixsize1() { /* EXPOSURE=60, PIXSIZE1=3.76 → exptime 60, xpixsz 3.76 */ }
#[test] fn legacy_map_reads_hierarch_cards() { /* parse_stored_header_keys(FITS, text).get("ESO DET TEMP") == Some("-7.25") */ }
#[test] fn legacy_map_joins_continue_cards() { /* FILTER = "Long filter  two" (FitsHeader's own join) */ }
#[test] fn legacy_map_keeps_a_doubled_quote() { /* OBJECT == "O'Brien" (was "O") */ }
#[test] fn legacy_map_keeps_an_xisf_value_with_slash() { /* dump FILTER = Ha / 7nm → "Ha / 7nm" (was "Ha") */ }
#[test] fn legacy_map_xisf_last_duplicate_wins() { /* GAIN 100 then 120 → "120" (was "100") */ }
#[test] fn legacy_map_drops_comment_history_and_blank_values() { /* no COMMENT/HISTORY key, OBJECT = '' absent */ }
```

- [ ] **Step 2: Run them and see them fail** — `CT fits_parser::` → FAIL (missing items: `header_keys`, `from_header_text`, …).

- [ ] **Step 3: Implement** the interfaces above in the files listed. Remove `parse_fits_card_text` and
  `parse_xisf_xml_text` from `stored_header.rs`; update the `stored_header.rs` module doc (stored formats: FITS cards,
  the XISF `NAME = value` dump, older raw XML) and the comment at `fits_parser/mod.rs:467-470`.

- [ ] **Step 4: Run them and see them pass** — `CT fits_parser::` and `CT header_fallbacks::` → PASS.

- [ ] **Step 5: Run the consumers of the changed readers** — `CT stored_header`, `CT db::repair`, `CT db::operations`,
  `CT sync::ingest`, `CT calibration::geometry`, `CT calibration_library::headers`, `CT calibration_library::light_cal`,
  `CT calibration_library::light_resolve`, `CT stacking::master_cards`, `CT stacking::run::output_tests`,
  `CT collab::frame_meta`, `CT fits_parser::calibrated_light`, `CT zip_store::scan`, `CT scanner::` → PASS. A failure
  that is a move toward the scanner's reading (HIERARCH now present, doubled quote, XISF " / ", last duplicate) is fixed
  in the test with a one-line comment naming the difference; any other failure is a bug in this task.

- [ ] **Step 6: Commit** — `git add` the files above;
  message `feat(catalog): one key reader per stored header format and one keyword resolver (issue #21)` + trailers.

---

### Task 2: Rules config, validation, storage and the three settings commands on both hosts (`rust-engineer`)

**Files:**
- Modify: `crates/athenaeum-core/src/header_fallbacks/mod.rs` (validation, merge, compaction, load/store)
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (`keys`: `CATALOG_HEADER_FALLBACKS` beside the calibration keys
  at l.359-364; not added to `defaults::all()`)
- Create: `crates/athenaeum-core/src/api/header_fallbacks.rs`; Modify: `crates/athenaeum-core/src/api/mod.rs` (`pub mod header_fallbacks;`)
- Modify: `crates/athenaeum-core/src/api/settings.rs` (`SettingsDefaults` 18-27, `get_settings_defaults` 31-43, test 144-168)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (`models.ts` list: `HeaderField`, `FieldRule`, `HeaderFallbackConfig` before `SettingsDefaults`, l.321)
- Modify: `crates/athenaeum-tauri/src/commands/settings.rs`, `crates/athenaeum-tauri/src/lib.rs:310-318`
- Modify: `crates/athenaeum-web/src/routes/settings.rs`, `crates/athenaeum-web/src/routes/mod.rs:106-115`
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: Task 1's `HeaderField`, `FieldRule`, `HeaderFallbackConfig`, `normalize_keyword`.
- Produces (`crate::header_fallbacks`):
  - `pub enum RulesError { InvalidKeyword { field: HeaderField, keyword: String }, Duplicate { field: HeaderField, keyword: String }, MissingDefault { field: HeaderField, keyword: String } }`
    with `Display`: `"{label}: \"{keyword}\" is not a valid keyword (empty or contains '=')"`,
    `"{label}: {keyword} appears twice"`, `"{label}: the standard keyword {keyword} cannot be removed"` where `label` is
    the field id.
  - `pub fn validate(cfg: &HeaderFallbackConfig) -> Result<HeaderFallbackConfig, RulesError>` — expanded result,
    every keyword normalised, repeats checked after normalisation, every default keyword present, each field's
    `retired` deduplicated (last occurrence kept) and stripped of its `order` keywords.
  - `pub(crate) fn fill_missing_defaults(field: HeaderField, order: Vec<String>) -> Vec<String>` (D14 rule; Task 6 reuses it).
  - `impl HeaderFallbackConfig { pub fn compact(&self) -> HeaderFallbackConfig; pub fn with_retirements_from(self, previous: &HeaderFallbackConfig) -> HeaderFallbackConfig; pub fn reset_orders(&self) -> HeaderFallbackConfig }`
    (`with_retirements_from` = D12's merge; `reset_orders` = every order back to its default chain, then the merge).
  - `pub fn load(conn: &Connection) -> anyhow::Result<HeaderFallbackConfig>` (no row → default; D14);
    `pub fn load_for_scan(conn: &Connection) -> HeaderFallbackConfig` (`error!(key = "catalog.header_fallbacks", error, "stored FITS keyword rules do not decode; using the default")` then default);
    `pub fn store(conn: &Connection, cfg: &HeaderFallbackConfig) -> anyhow::Result<()>` (compact JSON; empty → `db::delete_setting`).
- Produces (`crate::api::header_fallbacks`):
  - `pub fn get_header_fallbacks(ctx: &ServiceContext) -> Result<HeaderFallbackConfig, ApiError>`
  - `pub fn set_header_fallbacks(ctx: &ServiceContext, config: HeaderFallbackConfig) -> Result<(), ApiError>`
    (`validate` → `ApiError::Invalid(err.to_string())`; then `with_retirements_from(&load(conn)?)`, `store`)
  - `pub fn reset_header_fallbacks(ctx: &ServiceContext) -> Result<HeaderFallbackConfig, ApiError>` (returns the stored, expanded doc)
- Produces (hosts): `get_header_fallbacks`, `set_header_fallbacks { config }`, `reset_header_fallbacks`; web args
  `SetHeaderFallbacksArgs { config: HeaderFallbackConfig }` (`#[serde(rename_all = "camelCase")]`), the other two take
  `Json<serde_json::Value>` like `routes/calibration.rs:215-245`.
- Produces: `SettingsDefaults.header_fallbacks: HeaderFallbackConfig` (TS `headerFallbacks`).

- [ ] **Step 1: Write the failing tests** (in `header_fallbacks/mod.rs` and `api/header_fallbacks.rs`; the api ones use
  `ServiceContext::new_for_tests` over a tempdir catalog, as `api/settings.rs:135-139`):
```rust
#[test] fn default_config_lists_every_field_with_its_default_chain() {
    let d = HeaderFallbackConfig::default();
    assert_eq!(d.fields.len(), 12);
    assert_eq!(d.order(HeaderField::CcdTemp), ["CCD-TEMP", "DET-TEMP"]);
    assert_eq!(d.order(HeaderField::Exptime), ["EXPTIME", "EXPOSURE"]);
    assert_eq!(d.order(HeaderField::Ypixsz), ["YPIXSZ", "PIXSIZE2"]);
    assert_eq!(d.order(HeaderField::Telescop), ["TELESCOP"]);
}
#[test] fn validate_refuses_a_dropped_default_keyword() { /* ccdTemp order [CCD-TEMP] → MissingDefault{CcdTemp, "DET-TEMP"} */ }
#[test] fn validate_refuses_an_invalid_keyword() { /* "" and "A=B" → InvalidKeyword */ }
#[test] fn validate_normalises_then_refuses_a_repeat() {
    // ccdTemp [CCD-TEMP, DET-TEMP, "ccd-temp "] → Duplicate { field: CcdTemp, keyword: "CCD-TEMP" }
    // ccdTemp [CCD-TEMP, DET-TEMP, "my-temp"] → Ok, order [.., "MY-TEMP"]
}
#[test] fn unknown_field_id_does_not_deserialize() {
    assert!(serde_json::from_str::<HeaderFallbackConfig>(r#"{"fields":{"bogus":{"order":[]}}}"#).is_err());
}
#[test] fn validate_takes_order_keywords_out_of_retired() { /* retired [MY-TEMP], order has MY-TEMP → retired [] */ }
#[test] fn set_moves_a_removed_user_keyword_to_retired() {
    // set ccdTemp [CCD-TEMP, DET-TEMP, MY-TEMP]; then set the default doc → get(): order default, retired ["MY-TEMP"]
}
#[test] fn reset_retires_user_keywords_and_restores_default_orders() { /* returns defaults orders; retired keeps MY-TEMP */ }
#[test] fn defaults_are_not_written() { /* set(default) on a fresh catalog → db::get_setting(conn, "catalog.header_fallbacks") == None */ }
#[test] fn load_inserts_a_default_keyword_the_stored_doc_lacks() {
    // stored {"fields":{"ccdTemp":{"order":["MY-TEMP","CCD-TEMP"],"retired":[]}}} → order [MY-TEMP, CCD-TEMP, DET-TEMP]
    // stored exptime ["X","EXPOSURE"] → [EXPTIME, X, EXPOSURE]
}
#[test] fn load_skips_an_unknown_field_id_with_a_warning() { /* {"fields":{"future":{..},"gain":{"order":["G2","GAIN"]}}} → gain kept */ }
#[test] fn load_for_scan_falls_back_to_defaults_on_garbage() { /* row "not json" → load() errs, load_for_scan() == default */ }
```
Extend `typed_defaults_equal_what_reset_writes` (`api/settings.rs:144`) with
`d.header_fallbacks == HeaderFallbackConfig::default()` (serde values) and `reset_header_fallbacks` on a fresh catalog
returning the same. Web: `set_header_fallbacks_then_get_reflects_change` in `routes/settings.rs` tests, shaped as
`routes/calibration.rs:542`.

- [ ] **Step 2: Run them and see them fail** — `CT header_fallbacks::` and `CT api::header_fallbacks` → FAIL.

- [ ] **Step 3: Implement** the interfaces; `load` parses a `serde_json::Value`, maps known field ids, warns
  `warn!(key = "catalog.header_fallbacks", reason = %id, "unknown field id in stored FITS keyword rules; skipped")` for
  the rest, applies `fill_missing_defaults`, then `validate`. Host wrappers are three lines each.

- [ ] **Step 4: Regenerate TS and run** — ts_contract (write, then check); `CT header_fallbacks::`, `CT api::header_fallbacks`,
  `CT api::settings` → PASS; `… cargo test -p athenaeum-web settings -- --test-threads=4` and
  `… cargo test -p athenaeum-web parity -- --test-threads=4` → PASS; `CC athenaeum-tauri` → OK; `npx tsc --noEmit` → OK.

- [ ] **Step 5: Commit** — message `feat(settings): FITS keyword rules, stored as catalog.header_fallbacks, with get/set/reset on both hosts` + trailers.

---

### Task 3: Every production header reader takes the catalog's rules (`rust-engineer`)

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/mod.rs` (public parse functions 148-193, 282-286, 630-635 gain `rules`)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` (the loading entry points and the instrume sites listed under
  Approach notes; every other parser caller in it follows from the compiler), `crates/athenaeum-core/src/zip_store/scan.rs`
  (`instrume_for_root` 294-309 moves to the scanner; `scan_zip` loads, `scan_zip_with` takes `rules`)
- Modify: `crates/athenaeum-core/src/api/zip.rs:592-614` (`get_zip_entry_header` takes `ctx`), `crates/athenaeum-tauri/src/commands/zip.rs:73-74`, `crates/athenaeum-web/src/routes/zip.rs:97-102`
- Modify: `crates/athenaeum-core/src/calibration_library/register.rs:74-130` (loads from `conn`)
- Modify: `crates/athenaeum-core/src/db/operations.rs` — `bulk_update_frame_metadata` 2118-2124 (loads),
  `recompute_override_flag_for_frames` 3993-4119, `get_frame_metadata_originals` 4170-4227, new
  `latest_header_subquery`
- Modify: `crates/athenaeum-core/src/api/files.rs:144-151`, `crates/athenaeum-core/src/plate_solve/hints.rs:137-138`
- Modify (frame discarded / header-only): `crates/athenaeum-core/src/calibration_library/light_cal.rs:755, 983`,
  `crates/athenaeum-core/src/stacking/origin.rs:105, 122`, `crates/athenaeum-core/src/rustafits_processor/mod.rs:52`,
  `crates/perseus/src/run.rs:599-611`, `crates/athenaeum-web/src/routes/scan_roots.rs:491` (test)
- Modify (tests only, pass `&HeaderFallbackConfig::default()`): every other caller the compiler lists
  (`fits_parser/mod.rs` tests, `scanner/mod.rs` tests ~3619-3807, `zip_store/scan.rs` tests 1158-2013,
  `calibration_library/register.rs:555, 601`, `api/masters.rs:5096`, `api/lights.rs:3381`, `sync/ingest_tests.rs:2840`,
  `fits_writer/xisf_patch.rs:522`)

**Interfaces:**
- Consumes: Task 1 builders; Task 2 `header_fallbacks::{load, load_for_scan}`.
- Produces (`crate::fits_parser`): `parse_fits_with_header(path, file_id, rules: &HeaderFallbackConfig)`,
  `parse_fits_header_bytes(bytes, file_id, display_path, rules)`, `parse_xisf_header_bytes(bytes, file_id, display_path, rules)`,
  `parse_xisf(path, file_id, rules)`, `parse_fits(path, file_id, rules)` — return types unchanged.
- Produces (`crate::scanner`): `pub(crate) fn split_unique_camera_suffix(s: &str) -> (&str, Option<&str>)` (a trailing
  ` N<digits>`, digits non-empty); `pub(crate) fn instrume_for_root(stored: Option<String>, unique_camera: bool, root_id: i64) -> Option<String>`
  (strip, then append ` N<root_id>` when `unique_camera` — today's zip function);
  `pub(crate) fn instrume_for_scan(parsed: Option<String>, unique_camera: bool, root_id: i64) -> Option<String>`
  (`if unique_camera { instrume_for_root(parsed, true, root_id) } else { parsed }`). Relocated stored values
  (`scanner/mod.rs:677-690`, zip adopt 385) use `instrume_for_root`; freshly parsed values (740, 1209, 2261, 2355,
  zip 557) use `instrume_for_scan`.
- Produces (`crate::zip_store::scan`): `pub fn scan_zip(conn, zip, root_id, unique_camera)` (unchanged signature, loads
  with `load_for_scan`) and `pub(crate) fn scan_zip_with(conn, zip, root_id, unique_camera, rules: &HeaderFallbackConfig)`.
- Produces (`crate::api::zip`): `pub fn get_zip_entry_header(ctx: &ServiceContext, policy: &PathPolicy, zip_path: &str, entry: &str) -> Result<ZipEntryHeader, ApiError>`
  (loads with `header_fallbacks::load`, error → `ApiError::Internal`). Both host wrappers move `state.ctx.clone()` into
  their `spawn_blocking`.
- Produces (`crate::db`): `recompute_override_flag_for_frames(conn, frame_ids, rules: &HeaderFallbackConfig)`,
  `get_frame_metadata_originals(conn, frame_ids, rules: &HeaderFallbackConfig)`;
  `pub(crate) fn latest_header_subquery(file_id_expr: &str) -> String` =
  `(SELECT header FROM fits_header WHERE file_id = {file_id_expr} ORDER BY id DESC LIMIT 1)` — the one choice of the
  `fits_header` row (`file_id` is not unique). The snapshot, the override recompute (both currently a plain
  `LEFT JOIN`, `operations.rs:4012, 4192`) and Task 4's `read_rows` all use it, so the pane's "original" and Apply read
  the same row.

**Approach notes:**
- Rule loading points: `scan_directory`, `scan_directory_parallel`, `rescan_directory_shallow`, `rescan_zips`,
  `resync_catalog_rows_from_disk`, `scan_zip` and `register_master` call `load_for_scan(conn)` once at their start;
  `run_registered_scans` loads once before its root loop (`ctx.db` conn) and hands `&rules` to `scan_files_parallel`;
  `PendingZips` gains `rules: HeaderFallbackConfig` so `finish_zip_pass` and the zip pass use the run's set.
  `run_registered_scan` reaches `scan_directory_parallel`, which loads.
- `bulk_update_frame_metadata` loads with `load(conn)` before its recompute; on `Err`, `warn!(key = "catalog.header_fallbacks", error, "stored FITS keyword rules do not decode; override flags not recomputed")` and skip (override stays 1 — the safe side).
- `api::files::get_frame_metadata_originals` loads with `load(&conn)` (error → `ApiError::Internal`).
- Discarding callers: `light_cal.rs:755, 983` → `crate::fits_parser::extract_fits_header(flat_path).ok()?` (header
  text only, no frame); `rustafits_processor/mod.rs:52` → `FitsHeader::from_path(path).map(|h| h.get_str("BAYERPAT"))`
  (the builder's own read of that card); `stacking/origin.rs:105, 122`, `plate_solve/hints.rs:138` and
  `perseus/src/run.rs` pass `&HeaderFallbackConfig::default()` with a one-line comment: origin reads only the CFA
  fields, hints only ra/dec (neither is governed by the rules), Perseus builds `frame_meta` with no catalog and the
  receiving catalog applies its own rules through Apply (C3). Web test `scan_roots.rs:491` → `extract_fits_header`.

- [ ] **Step 1: Write the failing tests**
```rust
// scanner/mod.rs (new #[cfg(test)] mod header_rules_tests) — fixtures via crate::fits_writer::{write_fits_f32, write_xisf_f32, Card, CardValue}
#[test] fn serial_scan_reads_a_user_keyword_first() {
    // rules ccdTemp [MY-TEMP, CCD-TEMP, DET-TEMP] stored with header_fallbacks::store; FITS with CCD-TEMP=-10, MY-TEMP=-3
    // scan_directory(..) → frames.ccd_temp == -3.0; an XISF beside it with the same cards → -3.0
}
#[test] fn parallel_scan_reads_a_user_keyword_first() { /* same through run_registered_scan */ }
#[test] fn in_place_reparse_reads_the_catalog_rules() { /* scan, store rules, touch the file (new bytes), rescan → new value */ }
#[test] fn instrume_for_scan_appends_once_and_leaves_a_plain_root_alone() {
    assert_eq!(instrume_for_scan(Some("Cam N3".into()), true, 3), Some("Cam N3".into()));
    assert_eq!(instrume_for_scan(Some("Cam".into()), true, 3), Some("Cam N3".into()));
    assert_eq!(instrume_for_scan(Some("Cam N3".into()), false, 3), Some("Cam N3".into()));
    assert_eq!(instrume_for_root(Some("Cam N3".into()), false, 5), Some("Cam".into()));
}
// zip_store/scan.rs
#[test] fn zip_scan_reads_the_catalog_rules() { /* zipped FITS with MY-TEMP; stored rules; scan_zip → ccd_temp from MY-TEMP */ }
// api/zip.rs
#[test] fn zip_entry_header_reads_the_catalog_rules() { /* get_zip_entry_header(&ctx, ..).frame.ccd_temp == MY-TEMP value */ }
// calibration_library/register.rs
#[test] fn register_master_reads_the_catalog_rules() { /* master FITS with MY-GAIN=139 first in gain; reg frame gain == 139 */ }
// db/operations.rs
#[test] fn metadata_originals_follow_the_catalog_rules() { /* header CCD-TEMP=-10, MY-TEMP=-3; custom rules → snapshot ccd_temp -3; default → -10 */ }
#[test] fn snapshot_override_recompute_and_apply_read_the_same_header_row() {
    // one file with two fits_header rows (older CCD-TEMP=-10, newer CCD-TEMP=-3): get_frame_metadata_originals gives
    // ccd_temp -3; recompute_override_flag_for_frames judges against -3 (override clears for a frame holding -3)
}
#[test] fn override_recompute_reads_under_the_catalog_rules() {
    // frame override=1 with every field equal to the header under custom rules (ccd_temp = MY-TEMP value):
    // recompute(.., &custom) clears override; recompute(.., &default) leaves it 1
}
```
The existing `direct_registration_matches_scanner_ingestion_*` tests (`register.rs` ~470-500) must keep passing.

- [ ] **Step 2: Run them and see them fail** — `CT scanner::header_rules_tests`, `CT zip_store::scan`, `CT api::zip`,
  `CT calibration_library::register`, `CT db::operations` → FAIL (compile errors first).

- [ ] **Step 3: Implement** per the interfaces and notes; fix every caller the compiler lists.

- [ ] **Step 4: Run** — the filters of Step 2 plus `CT fits_parser::`, `CT stacking::origin`, `CT calibration_library::light_cal`,
  `CT rustafits_processor`, `CT plate_solve::hints`, `CT sync::ingest` → PASS; `CC perseus`, `CC athenaeum-tauri` → OK;
  `… cargo test -p athenaeum-web zip -- --test-threads=4` and `… parity …` → PASS.

- [ ] **Step 5: Default-rules gate** — `rg -n 'HeaderFallbackConfig::default\(\)' "$W/crates" --glob '!**/tests/**'`
  lists only: `header_fallbacks/` itself, `stacking/origin.rs` (2), `plate_solve/hints.rs` (1),
  `crates/perseus/src/run.rs` (1), and lines inside `#[cfg(test)]` modules / `*_tests.rs` files. Paste the list into
  the task report; any other production line is a bug.

- [ ] **Step 6: Commit** — message `feat(scanner): scans, zip reads, master registration and the metadata pane read under the catalog's FITS keyword rules` + trailers.

---

### Task 4: The per-field test and the preview command (`rust-engineer`)

**Files:**
- Create: `crates/athenaeum-core/src/header_fallbacks/apply.rs`, `crates/athenaeum-core/src/header_fallbacks/calibration.rs`
- Modify: `crates/athenaeum-core/src/api/header_fallbacks.rs`, `crates/athenaeum-core/src/ts_export.rs`
- Modify: `crates/athenaeum-tauri/src/commands/settings.rs`, `crates/athenaeum-tauri/src/lib.rs`,
  `crates/athenaeum-web/src/routes/settings.rs`, `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary extension paragraph "FITS keyword fallbacks")
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: Task 1 resolver and readers; Task 2 `load`; Task 3 `split_unique_camera_suffix`, `instrume_for_scan`,
  `latest_header_subquery`, `crate::scanner::path_has_root_prefix` (`scanner/mod.rs:60`).
- Produces (`crate::header_fallbacks::apply`):
  - `pub struct FieldCount { pub field: HeaderField, pub frames: usize }` (camelCase, TS)
  - `pub struct HeaderFallbackCounts { pub by_field: Vec<FieldCount>, pub frames_changed: usize, pub values_kept: usize, pub sets_regrouped: usize, pub frames_without_header: usize }`
    (camelCase, TS; `by_field` holds non-zero counts only, in `HeaderField::ALL` order)
  - `pub struct CatalogRow { pub frame_id: i64, pub file_path: String, pub format: String, pub imagetyp: Option<String>, pub values: ResolvedFields, pub header: Option<String> }`
  - `pub fn read_rows(conn: &Connection, after_frame_id: i64, limit: usize) -> anyhow::Result<Vec<CatalogRow>>`
    (`frames JOIN files`, the twelve columns, `latest_header_subquery("fr.file_id")` — `idx_fits_header_file_id`
    exists, `db/schema.rs:3418`; keyset `WHERE fr.id > ?1 ORDER BY fr.id LIMIT ?2`)
  - `pub struct ScanRoots` with `pub fn load(conn: &Connection) -> anyhow::Result<ScanRoots>` and
    `pub fn root_of(&self, file_path: &str) -> Option<(i64, bool)>` (longest separator-strict prefix via `path_has_root_prefix`; `(id, unique_camera)`)
  - `pub struct FramePlan { pub changes: Vec<(HeaderField, Option<FieldValue>)>, pub kept: usize, pub no_header: bool }`
  - `pub fn plan_frame(rules: &HeaderFallbackConfig, row: &CatalogRow, roots: &ScanRoots) -> FramePlan`
  - `pub const PAGE_SIZE: usize = 500;`
  - `pub fn preview(conn: &Connection, rules: &HeaderFallbackConfig) -> anyhow::Result<HeaderFallbackCounts>`
- Produces (`crate::header_fallbacks::calibration`):
  - `pub(crate) fn grouping_fields(imagetyp: &str) -> &'static [HeaderField]` — the non-temperature group keys:
    `"Bias"`: Instrume, Gain, Offset; `"Dark"`/`"DarkFlat"`: Instrume, Gain, Offset, Exptime; `"Flat"`: Instrume,
    Filter, Gain, Offset, Exptime, Focallen; anything else: `&[]` (from the group keys at
    `calibration/scan_integration.rs:59-86`). The temperature is not a field here: the scanner clusters on
    `effective_temp(ccd_temp, set_temp)` (`models.rs:37-39`; `scan_integration.rs:541-544, 598-601, 652-655`).
  - `pub(crate) fn needs_regroup(imagetyp: Option<&str>, old: &ResolvedFields, changes: &[(HeaderField, Option<FieldValue>)]) -> bool`
    — true for a raw type (`Bias`, `Dark`, `DarkFlat`, `Flat`) when a change is in `grouping_fields(imagetyp)` or when
    `effective_temp(ccd_temp, set_temp)` computed from `old` differs (`same_value` on the `Option`) from the one
    computed from `old` with `changes` applied (so a `setTemp` change counts only for a frame whose `ccd_temp` is NULL).
  - `pub(crate) fn movable_sets_of(conn: &Connection, frame_ids: &[i64]) -> anyhow::Result<BTreeSet<i64>>` — sets
    holding those frames with `COALESCE(is_master_library,0) = 0 AND superseded_by_set_id IS NULL AND id NOT IN (SELECT source_set_id FROM master_provenance WHERE source_set_id IS NOT NULL)`
    (the lineage guard of `db/schema.rs:110-115`), ids chunked by 500.
- Produces (`crate::api::header_fallbacks`): `pub fn preview_header_fallbacks(ctx: &ServiceContext) -> Result<HeaderFallbackCounts, ApiError>`;
  hosts: `preview_header_fallbacks` (no args; `level = "debug"`; `spawn_blocking`).

**The per-field test (`plan_frame`) — the algorithm the signatures do not fix:**
```text
header blank/absent, or format not "FITS"/"XISF", or !stored_header_key_map(fmt, text).has_keywords()
    → FramePlan { no_header: true, .. }                    // counted apart, never changed (§5.4)
for f in HeaderField::ALL:
    raw  = row.values.get(f)                               // the column
    cur  = raw with text trimmed, blank → None             // D8
    cands = order(f) ++ retired(f), each resolve_keyword(f, kw, keys), the Some ones
    if fmt == XISF && f ∈ {Xpixsz, Ypixsz} && cands.is_empty() && cur.is_some(): continue   // D10
    resolved = resolve_field(f, order(f), keys)
    if f == Instrume:                                      // D9
        cur None                                  → from header, new = instrume_for_scan(resolved, root.uc, root.id)  (no root: uc = false)
        cands ∋ cur                               → from header, new = resolved
        split_unique_camera_suffix(cur) = (base, Some(n)), cands ∋ base → from header, new = resolved + " N" + n
        else                                      → kept
    else:
        from header = cur is None || cands.any(|c| same_value(c, cur))
        new = resolved
    if !from header: kept += 1; continue
    if new differs from raw (same_value; None vs Some differs): changes.push((f, new))
```
`preview` pages `read_rows` by `PAGE_SIZE`, sums `by_field` / `frames_changed` (frames with ≥ 1 change) / `values_kept`
/ `frames_without_header`, collects frame ids where `needs_regroup`, then `sets_regrouped = movable_sets_of(..).len()`,
and logs `debug!(count, kept, no_header, sets_regrouped, by_field = %by_field_text, duration_ms, "header fallbacks previewed")`
(`by_field_text` like `ccdTemp=290 exptime=22`).

- [ ] **Step 1: Write the failing tests** (in `apply.rs`; catalog = `init_db` in-memory or tempdir, rows inserted with
  header text produced by `FitsHeader::from_bytes(block).to_header_text()` / `xisf_header_text`)
```rust
#[test] fn needs_regroup_follows_the_effective_temperature() {
    // Dark, ccd_temp NULL, set_temp -10: a setTemp change to -20 → true; same Dark with ccd_temp -10: a setTemp change
    // → false; a ccdTemp change → true; a Light with any change → false; a Dark with a focallen-only change → false
}
#[test] fn null_value_is_filled() { /* ccd_temp NULL, header DET-TEMP=-5.5 → changes [(CcdTemp, Some(-5.5))] */ }
#[test] fn header_value_replaced_after_reordering() { /* column -10 (CCD-TEMP), DET-TEMP=12, order [DET-TEMP, CCD-TEMP] → Some(12.0) */ }
#[test] fn typed_value_is_kept() { /* column -20, header CCD-TEMP=-10 → no change, kept == 1 */ }
#[test] fn plate_solved_frame_is_filled() { /* override = 1, focallen typed 812 (kept), ccd_temp NULL + DET-TEMP → filled */ }
#[test] fn retired_keyword_value_is_cleared() { /* column = MY-TEMP's -3, MY-TEMP in retired, absent from order, no other keyword → Some change to None */ }
#[test] fn unique_camera_suffix_is_kept() {
    // column "Cam N7", header INSTRUME=Cam, CAMERA=Cam2, instrume order [CAMERA, INSTRUME] → Some(Text("Cam2 N7"))
    // column NULL under a unique-camera root id 7 → "Cam2 N7"; under a plain root → "Cam2"
}
#[test] fn padded_xisf_text_is_rewritten_trimmed() { /* filter column "Ha  " from an old XISF row, dump FILTER = Ha → change to "Ha" */ }
#[test] fn xisf_property_pixel_size_is_left_and_not_counted() { /* XISF, xpixsz 3.76, no XPIXSZ/PIXSIZE1 in dump → no change, kept == 0 */ }
#[test] fn placeholder_header_is_counted_apart() { /* XISF "(No FITS keywords found in XISF)" and a NULL header → no_header, counts.frames_without_header == 2 */ }
#[test] fn preview_counts_fields_frames_kept_and_sets() {
    // 3 darks of one raw set with ccd_temp NULL + DET-TEMP; 1 light with typed gain; 1 dark in a superseded set
    // → by_field [{CcdTemp, 4}], frames_changed 4, values_kept 1, sets_regrouped 1 (the superseded set not counted)
}
#[test] fn default_rules_on_a_freshly_scanned_catalog_preview_zero() {
    // anti-goal §2: scan (Task 3 scanner) a dir with FITS (DET-TEMP only, HIERARCH, CONTINUE, doubled quote) and XISF
    // (EXPOSURE only, padded FILTER, blank OBJECT, D-notation EXPTIME) → preview(default) == HeaderFallbackCounts::default()
}
```

- [ ] **Step 2: Run them and see them fail** — `CT header_fallbacks::apply` → FAIL.

- [ ] **Step 3: Implement** `apply.rs` (read + plan + preview), `calibration.rs` (the helpers above), the api handler and
  both host wrappers; add the five new log field names to the logging spec's dictionary in one paragraph
  ("Dictionary extension (FITS keyword fallbacks), 2026-11-22").

- [ ] **Step 4: Run** — ts_contract (write, then check); `CT header_fallbacks::`, `CT api::header_fallbacks` → PASS;
  web `settings` + `parity` → PASS; `CC athenaeum-tauri` → OK.

- [ ] **Step 5: Commit** — message `feat(catalog): preview which catalog values the FITS keyword rules would change` + trailers.

---

### Task 5: Apply — batched writes, nights, calibration regrouping, masters (`rust-engineer`)

**Files:**
- Modify: `crates/athenaeum-core/src/header_fallbacks/apply.rs`, `crates/athenaeum-core/src/header_fallbacks/calibration.rs`
- Modify: `crates/athenaeum-core/src/services/mod.rs` (`ServiceContext` gains the Apply flag),
  `crates/athenaeum-core/src/api/header_fallbacks.rs`, `crates/athenaeum-core/src/ts_export.rs`
- Modify: `crates/athenaeum-tauri/src/commands/settings.rs`, `crates/athenaeum-tauri/src/lib.rs`,
  `crates/athenaeum-web/src/routes/settings.rs`, `crates/athenaeum-web/src/routes/mod.rs`
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: Task 4 `read_rows`, `plan_frame`, `ScanRoots`, `needs_regroup`, `movable_sets_of`, `PAGE_SIZE`; `crate::db::SavepointGuard`; `crate::sessions::rederive_for_frame_set(conn, set_id, &[], gap_hours)`;
  `crate::db::calibration_links::frame_set_ids_for_frames`; `crate::db::schema::prune_orphaned_calibration_sets`;
  `crate::calibration::scan_integration::{create_calibration_sets_from_scan_with_masters, MasterFrameIds}`;
  `crate::events::emit_event`.
- Produces (`crate::header_fallbacks::calibration`; moved here from Task 4 so nothing sits unused):
  `pub(crate) const MASTER_TYPES: [&str; 4] = ["MasterDark", "MasterFlat", "MasterBias", "MasterDarkFlat"];` (the
  scanner classifies masters by `imagetyp` only, `scanner/mod.rs:472-484`) and
  `pub(crate) const MASTER_SET_FIELDS: [HeaderField; 7] = [Exptime, Filter, CcdTemp, Gain, Offset, Instrume, Focallen];`
  (the columns `create_master_sets_from_frames` writes, `scan_integration.rs:932-953`).
- Produces (`crate::header_fallbacks::apply`):
  - `pub const PROGRESS_EVENT: &str = "header-fallbacks-progress";`
  - `pub struct HeaderFallbackProgress { pub phase: String, pub done: usize, pub total: usize }` (camelCase, TS; `phase` is `"frames"` or `"calibration"`)
  - `pub struct ApplyReport { pub counts: HeaderFallbackCounts, pub frame_sets_touched: Vec<i64> }`
  - `pub fn apply(conn: &Connection, rules: &HeaderFallbackConfig, gap_hours: f64, emitter: &dyn ProgressEmitter) -> anyhow::Result<ApplyReport>`
    = `apply_in_pages(conn, rules, gap_hours, emitter, PAGE_SIZE)`; `pub(crate) fn apply_in_pages(.., page_size: usize)`.
- Produces (`crate::api::header_fallbacks`): `pub fn apply_header_fallbacks(ctx: &ServiceContext, emitter: &dyn ProgressEmitter) -> Result<HeaderFallbackCounts, ApiError>`
  — `ServiceContext` gains `pub(crate) apply_running: Arc<AtomicBool>` (D17; not a process static), taken by
  compare-exchange with a drop guard; when already set: `warn!("header fallbacks apply refused: one is running")`
  and `ApiError::Conflict("FITS keyword rules are already being applied")`; gap hours from
  `ctx.settings.get_session_gap_threshold_hours(&conn)`; after `apply`, `#[cfg(all(feature = "render", feature = "solver"))]`
  `crate::api::collab_autopublish::request_auto_publish_for_sets(&report.frame_sets_touched)` (D16). Hosts:
  `apply_header_fallbacks` (no args; desktop `TauriProgressEmitter(app_handle)`, web `SseProgressEmitter::new(event_tx)`,
  both inside `spawn_blocking`).

**Apply's order of work:**
1. `total` = `SELECT COUNT(*) FROM frames`; `ScanRoots::load`.
2. **Pages** (`read_rows` by `page_size`, `plan_frame` for every row, so preview and Apply cannot differ). A frame is
   **deferred** to step 4 when `needs_regroup(imagetyp, &row.values, changes)` or (`imagetyp ∈ MASTER_TYPES` and a change
   is in `MASTER_SET_FIELDS`); every other changed frame is written now, in one `SavepointGuard` per page, by
   `UPDATE frames SET <col> = ?, … WHERE id = ?` over `HeaderField::column()` only (never `override`, never a `files`
   row). Commit; emit `HeaderFallbackProgress { phase: "frames", done, total }`. Across pages collect
   `frame_sets_touched` (the frame sets of changed frames, `frame_set_ids_for_frames`) and `night_sets` (those with an
   `Instrume` or `Exptime` change).
3. **Nights**: after the last page, `rederive_for_frame_set(conn, id, &[], gap_hours)` once per set in `night_sets`,
   one `SavepointGuard` per set (D3). Never inside a page.
4. **Calibration** (one `SavepointGuard`): write the deferred frames' columns. Raw ones: `movable_sets_of(ids)`; delete
   their `calibration_set_frames` rows in those sets (the `calibration_set_empty_prune` trigger removes an emptied set),
   `prune_orphaned_calibration_sets`, then `create_calibration_sets_from_scan_with_masters(.., MasterFrameIds::default())`
   with the moved frames split by imagetyp; raw frames in a frozen set keep their row (D11, counted `lineage_kept`).
   Master frames: update the `is_master_library = 1` set holding the frame, writing only the `MASTER_SET_FIELDS`
   columns whose value changed (`temp_min` and `temp_max` follow `ccd_temp`, as `create_master_sets_from_frames` writes
   them). Commit; emit `{ phase: "calibration", done: total, total }`.
5. `counts.sets_regrouped` = distinct sets left in step 4; log
   `info!(count = frames_changed, kept, no_header, sets_regrouped, master_sets, lineage_kept, by_field = %…, duration_ms, "header fallbacks applied")`.
   A set that kept other members is left as the bulk edit leaves it (§5.5 step 2): note in the task report whether its
   `frame_count`/`temp_min`/`temp_max` went stale in the tests, and whether a dark set's own `focallen` copy
   (`scan_integration.rs:840-843`) went stale when `focallen` changed without a regroup (Task 9 records both findings).

- [ ] **Step 1: Write the failing tests** (in `apply.rs` / `calibration.rs`; calibration fixtures as in
  `calibration/scan_integration.rs` tests 971+, frame-set fixtures as in `sessions/mod.rs` tests)
```rust
#[test] fn darks_with_null_temperature_are_regrouped_by_temperature() {
    // 4 darks 60 s gain 100, 22:00/22:05/22:10/22:15, ccd_temp NULL, one raw set of 4 (scanner path);
    // headers DET-TEMP -10, -10, -20, -20. apply(default) →
    // 2 sets: {d1,d2} ccd_temp≈-10, {d3,d4} ≈-20; the old set id no longer exists; counts.sets_regrouped == 1
}
#[test] fn master_library_set_takes_its_masters_new_values() { /* MasterDark frame gain NULL + GAIN=100 → its set.gain == 100; an unchanged column (exptime) is not rewritten */ }
#[test] fn lights_keep_frame_set_nights_and_links() {
    // frame set with 2 lights in one night, linked to a dark set; instrume rule change →
    // get_frame_ids_for_frame_set unchanged; sessions.instrume == new name; calibration_set_to_frames rows unchanged
}
#[test] fn a_frame_set_spanning_pages_is_rederived_once() {
    // 5 lights of one frame set, page_size 2, exptime change on all: nights are rebuilt once, not three times
    // (the nights' row ids advance by one rebuild's night count only); sessions.exptime is new
}
#[test] fn frames_in_a_superseded_set_keep_their_membership() { /* seed_superseded_dark_lineage; temp change → membership unchanged, frame ccd_temp written, no new raw set */ }
#[test] fn apply_never_touches_override_or_hashes() { /* override and files.content_hash/strong_hash identical before/after */ }
#[test] fn second_preview_after_apply_reads_zero() {
    // CCD-TEMP=-10, DET-TEMP=12 on 3 lights, column -10; rules ccdTemp [DET-TEMP, CCD-TEMP]
    // preview → ccdTemp 3; apply → counts.frames_changed == 3, columns == 12; preview again == default counts
}
#[test] fn nan_header_value_converges_after_apply() { /* CCD-TEMP=NaN, DET-TEMP=-5 → apply writes -5; preview == 0 */ }
#[test] fn apply_counts_equal_the_preview_counts() { /* mixed fixture: preview(rules) == apply(rules).counts */ }
#[test] fn each_page_commits_before_the_next() {
    // 5 changed lights, page_size 2; emitter callback on the first "frames" event opens a second connection to the same
    // tempdir catalog, runs "BEGIN IMMEDIATE; ROLLBACK" successfully and sees the first 2 frames already changed
}
// api/header_fallbacks.rs
#[test] fn second_apply_while_one_runs_is_a_conflict() {
    // set this ctx's apply_running flag (test helper) → apply_header_fallbacks(&ctx, &NullEmitter) is
    // Err(ApiError::Conflict(_)); released → Ok; a second ctx over another catalog is not affected (tests do not race)
}
```

- [ ] **Step 2: Run them and see them fail** — `CT header_fallbacks::`, `CT api::header_fallbacks` → FAIL.

- [ ] **Step 3: Implement** the order of work above, the `ServiceContext` flag, the api handler and both host wrappers.

- [ ] **Step 4: Run** — ts_contract (write, then check); `CT header_fallbacks::`, `CT api::header_fallbacks`,
  `CT calibration::scan_integration`, `CT sessions::` → PASS; web `settings` + `parity` → PASS; `CC athenaeum-tauri` → OK.

- [ ] **Step 5: Commit** — message `feat(catalog): apply the FITS keyword rules to the catalog, regrouping changed raw calibration frames` + trailers.

---

### Task 6: Export and import of the rules — core and both hosts (`rust-engineer`)

**Files:**
- Create: `crates/athenaeum-core/src/header_fallbacks/transfer.rs`
- Modify: `crates/athenaeum-core/src/api/header_fallbacks.rs`, `crates/athenaeum-core/src/ts_export.rs`
- Modify: `crates/athenaeum-tauri/src/commands/settings.rs`, `crates/athenaeum-tauri/src/lib.rs`,
  `crates/athenaeum-web/src/routes/settings.rs`, `crates/athenaeum-web/src/routes/mod.rs`
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: Task 2 `validate`, `fill_missing_defaults`, `with_retirements_from`, `load`; `crate::api::PathPolicy::check`.
- Produces (`crate::header_fallbacks::transfer`):
  - `pub const FILE_FORMAT: &str = "athenaeum.header-fallbacks";` `pub const FILE_VERSION: u64 = 1;`
    `pub const MAX_IMPORT_BYTES: usize = 1_048_576;` `pub const DEFAULT_FILE_NAME: &str = "athenaeum-fits-keywords.json";`
  - `pub fn export_text(cfg: &HeaderFallbackConfig) -> String` — pretty JSON in the shape its test fixes.
  - `pub struct FieldOrderChange { pub field: HeaderField, pub before: Vec<String>, pub after: Vec<String> }` (camelCase, TS)
  - `pub struct HeaderFallbackImportPlan { pub config: HeaderFallbackConfig, pub changes: Vec<FieldOrderChange> }` (camelCase, TS)
  - `pub enum ImportError` with `Display` naming the problem: `TooLarge(usize)` "the file is larger than 1 MB",
    `NotJson(String)` "not a JSON file: {serde message}", `UnknownFormat(String)` "not a FITS keyword file (format \"{x}\")",
    `UnsupportedVersion(String)` "version {x} is not supported (this Athenaeum reads version 1)", `NoFields`
    "the file has no \"fields\" object", `UnknownField(String)` "unknown field \"{id}\"", `NotAList(String)`
    "{id}: expected a list of keywords", `NotAString { field, index }` "{id}: entry {index} is not text",
    `Rules(RulesError)` (invalid keyword, repeat, …: `RulesError`'s own text).
  - `pub fn plan_import(current: &HeaderFallbackConfig, text: &str) -> Result<HeaderFallbackImportPlan, ImportError>`
    — size check on `text.len()`, strip a leading `\u{feff}`, parse `serde_json::Value`, check `format`, `version` (must
    be the integer 1), `fields`; for each named field: every entry a string, then `fill_missing_defaults`; build
    `current` with those orders replaced, `.with_retirements_from(current)`, `validate`; `changes` = fields whose order
    differs from `current`'s, in `HeaderField::ALL` order. Nothing of a refused file reaches the result.
- Produces (`crate::api::header_fallbacks`):
  - `pub fn export_header_fallbacks(ctx: &ServiceContext, path: Option<String>, policy: &PathPolicy) -> Result<String, ApiError>`
    — the text; with `path`: `policy.check`, `std::fs::write` (error → `ApiError::Internal` naming the file name).
  - `pub fn plan_header_fallbacks_import(ctx: &ServiceContext, text: Option<String>, path: Option<String>, policy: &PathPolicy) -> Result<HeaderFallbackImportPlan, ApiError>`
    — exactly one of `text` / `path` (else `ApiError::Invalid("give the file's text or its path")`); a path is checked
    by `policy`, its length by `std::fs::metadata` against `MAX_IMPORT_BYTES` before reading, and read with
    `read_to_string` (not UTF-8 → `ImportError::NotJson`); any `ImportError` → `warn!(reason = %e, "header fallbacks import refused")`
    and `ApiError::Invalid(e.to_string())`. Import changes the rules only after the user confirms: the frontend then
    saves `plan.config` through the normal autosave (Task 8).
- Hosts: desktop passes `&PathPolicy::AllowAll`; web routes pass `&PathPolicy::AllowedRoots(Vec::new())` (a path is
  refused, D15). Args: `ExportHeaderFallbacksArgs { path: Option<String> }`, `PlanHeaderFallbacksImportArgs { text: Option<String>, path: Option<String> }`.

- [ ] **Step 1: Write the failing tests** (`transfer.rs`, one per rejection reason, and `api/header_fallbacks.rs`)
```rust
#[test] fn export_writes_only_changed_fields_and_never_retired() {
    // ccdTemp [DET-TEMP, CCD-TEMP, MY-TEMP], retired [OLD]; others default →
    // JSON == {"format":"athenaeum.header-fallbacks","version":1,"fields":{"ccdTemp":["DET-TEMP","CCD-TEMP","MY-TEMP"]}}; no "OLD" in text
}
#[test] fn export_then_import_round_trips() { /* plan_import(&default, &export_text(&c)).config orders == c orders */ }
#[test] fn import_replaces_only_named_fields_and_retires_lost_keywords() {
    // current gain [MY-G, GAIN]; file names only ccdTemp → gain unchanged; current ccdTemp [CCD-TEMP, DET-TEMP, X], file
    // ccdTemp [DET-TEMP, CCD-TEMP] → retired(ccdTemp) contains X; changes == [{ccdTemp, before, after}]
}
#[test] fn import_appends_a_missing_default_after_its_predecessor() { /* file exptime ["EXPTIME","X"] → [EXPTIME, EXPOSURE, X] */ }
#[test] fn import_accepts_a_bom_and_crlf() { /* "\u{feff}{\r\n \"format\": …\r\n}" → Ok */ }
#[test] fn import_refuses_a_file_over_one_mib() { /* 1_048_577 bytes → TooLarge */ }
#[test] fn import_refuses_bytes_that_are_not_json() { /* "\u{0}PK\u{3}" → NotJson */ }
// one test per remaining refusal, each asserting the matching `ImportError` and that its message names the problem:
// import_refuses_an_unknown_format, import_refuses_version_two (and a missing or non-integer version),
// import_refuses_a_file_without_fields, import_refuses_an_unknown_field (message contains the id),
// import_refuses_a_field_that_is_not_a_list, import_refuses_a_non_string_keyword,
// import_refuses_an_invalid_keyword ("" and "A=B"), import_refuses_a_repeated_keyword (["CCD-TEMP","DET-TEMP","ccd-temp"])
#[test] fn refused_import_changes_nothing() { /* after any refusal, load(conn) == before */ }
#[test] fn import_by_path_is_refused_on_a_restricted_policy() { /* PathPolicy::AllowedRoots(vec![]) + path → ApiError::Forbidden */ }
#[test] fn export_to_a_path_writes_the_same_text() { /* tempdir join "rules.json"; file content == returned text */ }
```

- [ ] **Step 2: Run them and see them fail** — `CT header_fallbacks::transfer`, `CT api::header_fallbacks` → FAIL.

- [ ] **Step 3: Implement**, plus both host wrappers.

- [ ] **Step 4: Run** — ts_contract (write, then check); the Step 2 filters → PASS; web `settings` + `parity` → PASS; `CC athenaeum-tauri` → OK.

- [ ] **Step 5: Commit** — message `feat(settings): export and import FITS keyword rules as a JSON file` + trailers.

---

### Task 7: Settings → General → "FITS keywords": editing, preview, Apply (`frontend-dev`)

**Files:**
- Create: `src/components/settings/sections/HeaderKeywordsSection.tsx`, `src/components/settings/sections/KeywordChain.tsx`,
  `src/hooks/useHeaderFallbackPreview.ts`, `src/components/settings/sections/HeaderKeywordsSection.test.tsx`
- Modify: `src/settings/registry.ts` (new section `general.headerKeywords` after `general.sessions`),
  `src/components/settings/tabs/GeneralTab.tsx` (entry after `general.sessions`),
  `src/components/settings/tabs/registry-coverage.test.tsx` (mock `get_header_fallbacks`, `preview_header_fallbacks`)

**Interfaces:**
- Consumes (TS, generated): `HeaderFallbackConfig`, `HeaderField`, `FieldRule`, `HeaderFallbackCounts`, `FieldCount`,
  `HeaderFallbackProgress`, `SettingsDefaults.headerFallbacks`; commands `get_header_fallbacks`,
  `set_header_fallbacks { config }`, `reset_header_fallbacks`, `preview_header_fallbacks`, `apply_header_fallbacks`;
  event `header-fallbacks-progress`.
- Produces: `export function HeaderKeywordsSection()`; `export const HEADER_FIELD_LABELS: Record<HeaderField, string>`
  (exported for Task 8); `useHeaderFallbackPreview({ enabled, savedAt }): { counts: HeaderFallbackCounts | null; error: string | null; refresh(): void }`;
  `KeywordChain` props `{ field: HeaderField; order: string[]; defaults: string[]; onChange(order: string[]): void }`.
  `HeaderKeywordsSection` exposes a slot for Task 8: it renders `actions={<HeaderKeywordsTransfer … />}` once Task 8
  lands; in this task `actions` is omitted.

**Behaviour and copy (D5):**
- Document: `useAutosaveDocument<HeaderFallbackConfig>({ load: () => api.invoke('get_header_fallbacks'), save: (config) => api.invoke('set_header_fallbacks', { config }), resetAll: () => api.invoke('reset_header_fallbacks').then(() => {}), defaults: defaults?.headerFallbacks ?? null, label: 'FITS keywords' })`.
  The preview is keyed on `savedAt`, which only a save sets (`useAutosaveDocument.ts:146`; `resetAll` reloads without
  touching it, `:220-234`), so the section wraps `resetAll`: `const onResetAll = async () => { await resetAll(); refresh(); }`.
  `SettingsSection id="general.headerKeywords"` renders unconditionally (loading text inside while `doc` is null), with
  `onResetAll={onResetAll}` and `resetScopeLabel="This puts every field's keywords back in the default order. Catalog values change only when you apply."`.
- Registry: title `FITS keywords`; description `Which header keyword each catalog field is read from, first match wins. Your files are never changed.`;
  one field per `HeaderField` (`id` = the field id, `label` = `HEADER_FIELD_LABELS[f]`,
  `help` = `Tried in order; the first keyword with a usable value fills the field.`, `keywords` = the default chain
  lower-cased plus the column name, e.g. `['ccd-temp', 'det-temp', 'ccd_temp', 'temperature']`); section keywords
  `fits`, `header`, `keyword`, `fallback`, `xisf`, `catalog.header_fallbacks`.
- Labels, in `HeaderField` order: `ccdTemp` Sensor temperature · `setTemp` Set temperature · `gain` Gain · `offset`
  Offset · `exptime` Exposure · `focallen` Focal length · `xpixsz` Pixel size X · `ypixsz` Pixel size Y · `object`
  Object · `filter` Filter · `instrume` Camera · `telescop` Telescope.
- One row per field: label (fixed width), `KeywordChain`, an add input (placeholder `+ keyword`, commits on Enter or
  blur), and `ResetButton` (`visible={!isDefault(\`fields.${f}.order\`)}`, `defaultLabel` = the default chain joined by
  `, `, `onReset={() => resetField(\`fields.${f}.order\`)}`).
- Chips: `Chip tone="mute"` for a default keyword (no ×), `tone="info"` with a × button (`aria-label="Remove {KW}"`) for
  a user keyword; a `GripVertical` icon. Keyboard and native drag move a chip, focus follows the chip; Delete/Backspace
  remove a user keyword.
- Add: `trim().toUpperCase()`; empty or containing `=` → inline `Keywords can't be empty or contain "=".`; already in
  this field's list → inline `{KW} is already in this list.`; nothing is patched while an inline error shows (the
  server's `validate` stays the authority; its refusal arrives through the hook's `FITS keywords not saved` notify).
- Preview (`useHeaderFallbackPreview`, D4): first call when `doc` loads, then 800 ms after each `savedAt` change; one
  call in flight; a change during it queues one follow-up; a result whose generation is older than the latest request
  is dropped; failure → `notify({ kind: 'generic', tone: 'warning', title: 'FITS keyword preview failed', detail, dedupeKey: 'header-fallbacks-preview' })`.
- Preview line, hidden when `framesChanged === 0`: `{n} frames read differently under these rules:` (1 → `1 frame reads
  differently under these rules:`), then `by_field` as `{label} {frames}` joined ` · `, the `Apply to catalog` button,
  and when `valuesKept > 0` `{k} values you set yourself are kept.` (1 → `1 value you set yourself is kept.`).
- Apply: `ConfirmDialog` title `Apply FITS keywords to the catalog?`, `confirmText="Apply"`, message lines:
  `{label}: {frames} frames` per field; when `valuesKept > 0` `Values you set yourself stay as they are ({k}).`; when
  `setsRegrouped > 0` `Darks, flats and bias in {K} calibration sets will be grouped again. Lights linked to them are matched again at the next matching.`;
  when `framesWithoutHeader > 0` `{m} frames have no stored header and are not changed.` While it runs: the button is
  disabled and reads `Applying… {done} of {total} frames` (phase `frames`) or `Grouping calibration frames…` (phase
  `calibration`), fed by `api.listen('header-fallbacks-progress')` with the StrictMode-safe pattern of `CLAUDE.md`.
  Success → one `notify({ kind: 'generic', tone: 'success', title: 'FITS keywords applied', detail: '{n} frames changed, {K} calibration sets grouped again.' })`
  then `refresh()`; failure → `notify({ kind: 'generic', tone: 'warning', hasErrors: true, title: 'FITS keywords not applied', detail })`.
  A 409 shows the server text (`FITS keyword rules are already being applied`).
- Design tokens only (`text-content-muted`, `bg-surface-hover`, `text-error`); no Save button, no banner.

- [ ] **Step 1: Write the failing tests** (`HeaderKeywordsSection.test.tsx`, `vi.mock('../../../api')` with
  `routeInvoke` as in `registry-coverage.test.tsx`, fake timers for the debounces)
```ts
it('renders one row per field with default chips that have no remove button', ...) // 12 rows; no "Remove CCD-TEMP"
it('moves a chip with the arrow keys and saves the new order', ...)               // ArrowRight on CCD-TEMP → set_header_fallbacks with ccdTemp.order ['DET-TEMP','CCD-TEMP']
it('moves a chip by drag and drop', ...)                                           // dragStart/dragOver/drop with a dataTransfer stub
it('adds a keyword upper-cased and removes it with its button', ...)               // 'my-temp' + Enter → 'MY-TEMP' chip; "Remove MY-TEMP" → order back to default
it('adding_a_case_variant_of_a_default_shows_an_inline_error', ...)                // 'ccd-temp ' → 'CCD-TEMP is already in this list.'; no save
it('shows the reset button only for a field off its default', ...)
it('hides the preview line when nothing changes', ...)                             // framesChanged 0 → no 'Apply to catalog'
it('shows counts and opens the confirm with the calibration line', ...)           // setsRegrouped 3 → 'Darks, flats and bias in 3 calibration sets…'
it('applies, shows progress and notifies once', ...)                               // emits header-fallbacks-progress; one success notify; preview refreshed
it('reset_all_refreshes_the_preview', ...)                                          // Reset all → reset_header_fallbacks called, then preview_header_fallbacks called again (count 2 → new counts shown), without a save
it('re-runs the preview once after a burst of saves', ...)                         // 3 saves inside 800 ms → preview_header_fallbacks called twice in total (mount + 1)
```

- [ ] **Step 2: Run them and see them fail** — `cd "$W" && npx vitest run src/components/settings/sections/HeaderKeywordsSection.test.tsx` → FAIL.

- [ ] **Step 3: Implement** the section, chain, hook, registry entry and tab entry.

- [ ] **Step 4: Run** — the Step 2 command, `npx vitest run src/components/settings src/settings src/hooks/useSettingsSearch.test.ts`
  and `npx tsc --noEmit` → PASS.

- [ ] **Step 5: Look at it** — `cargo run -p athenaeum-web` (build rules above) with
  `ATHENAEUM_DB_PATH=$HOME/.athenaeum-acc/header-fallbacks/ui-scratch.db`, and `npm run dev:web`; first add the
  `~/.athenaeum-acc/header-fallbacks` row of Task 9 Step 3 to `REGISTRY.tsv` if it is not there. Check the section in
  dark and light themes, keyboard reorder and the empty-preview state; report what was checked. The real-catalog look
  is the owner's smoke (Task 9).

- [ ] **Step 6: Commit** — message `feat(settings): FITS keywords section — reorder header keywords, preview and apply` + trailers.

---

### Task 8: Export and import buttons (`frontend-dev`)

**Files:**
- Create: `src/components/settings/sections/HeaderKeywordsTransfer.tsx`, `src/utils/browserFile.ts`,
  `src/components/settings/sections/HeaderKeywordsTransfer.test.tsx`
- Modify: `src/api/desktop.ts` (add `pickSaveFile`), `src/components/settings/sections/HeaderKeywordsSection.tsx`
  (pass `actions`)

**Interfaces:**
- Consumes: Task 6 commands `export_header_fallbacks { path? }` → `string`, `plan_header_fallbacks_import { text?, path? }`
  → `HeaderFallbackImportPlan`; Task 7's `patch` from the section's `useAutosaveDocument`, `HEADER_FIELD_LABELS`.
- Produces: `export async function pickSaveFile(options: { defaultPath?: string; filters?: { name: string; extensions: string[] }[] }): Promise<string | null>`
  in `src/api/desktop.ts` (Tauri: `@tauri-apps/plugin-dialog` `save()`; web: `null`); `downloadTextFile(fileName: string, text: string): void`
  and `readFileText(file: File): Promise<string>` in `src/utils/browserFile.ts`;
  `HeaderKeywordsTransfer({ onImport(config: HeaderFallbackConfig): void })`.

**Behaviour and copy:**
- `Export…` (`Button` small): desktop → `pickSaveFile({ defaultPath: 'athenaeum-fits-keywords.json', filters: [{ name: 'JSON', extensions: ['json'] }] })`,
  cancel → nothing, else `api.invoke('export_header_fallbacks', { path })`; web →
  `downloadTextFile('athenaeum-fits-keywords.json', await api.invoke('export_header_fallbacks', {}))`. Success is quiet;
  failure → `notify({ kind: 'generic', tone: 'warning', hasErrors: true, title: 'FITS keywords not exported', detail })`.
- `Import…`: desktop → `pickFile({ title: 'Import FITS keywords', filters: [{ name: 'JSON', extensions: ['json'] }] })` →
  `plan_header_fallbacks_import { path }`; web → a hidden `<input type="file" accept=".json,application/json">` →
  `readFileText` → `plan_header_fallbacks_import { text }`. Refusal → `notify({ kind: 'generic', tone: 'warning', hasErrors: true, title: 'FITS keywords not imported', detail: <server text> })`.
- Confirm (`ConfirmDialog`, title `Import FITS keywords?`, `confirmText="Import"`): one line per change
  `{label}: {before joined ', '} → {after joined ', '}`, then `Catalog values change only when you apply.`; with no
  change the message is `This file matches your current keywords.` and confirming does nothing. Confirm →
  `onImport(plan.config)` (the section calls `patch(() => config)`, so autosave stores it and the preview reruns).
- `src/api/desktop.ts` is the only file importing `@tauri-apps/plugin-dialog`; the section imports `pickSaveFile` /
  `pickFile` from it and `isTauri` from `src/utils/platform`.

- [ ] **Step 1: Write the failing tests** (`HeaderKeywordsTransfer.test.tsx`)
```ts
it('exports through a browser download on the web', ...)        // export_header_fallbacks called with {}; downloadTextFile got the text
it('imports a file and confirms the field changes', ...)         // plan with ccdTemp change → dialog line 'Sensor temperature: CCD-TEMP, DET-TEMP → DET-TEMP, CCD-TEMP'; Import → onImport(plan.config)
it('says when a file changes nothing', ...)                      // changes [] → 'This file matches your current keywords.'
it('notifies a refused file and applies nothing', ...)           // invoke rejects 'unknown field "bogus"' → one warning notify; onImport not called
```

- [ ] **Step 2: Run them and see them fail** — `npx vitest run src/components/settings/sections/HeaderKeywordsTransfer.test.tsx` → FAIL.

- [ ] **Step 3: Implement.**

- [ ] **Step 4: Run** — the Step 2 command, the Task 7 test file and `npx tsc --noEmit` → PASS;
  `rg -n "@tauri-apps" "$W/src" --glob '!src/api/**'` → no match.

- [ ] **Step 5: Commit** — message `feat(settings): export and import FITS keyword rules from the Settings page` + trailers.

---

### Task 9: Docs, acceptance on a copy of the dev catalog, full gates (`rust-engineer`)

**Files:**
- Create: `crates/athenaeum-core/examples/header_fallbacks_probe.rs`
- Modify: `docs/catalog/README.md` (the sensor-temperature bullet, l.36, becomes "FITS keyword fallbacks"; a paragraph
  on the stored-header readers), `docs/calibration/matching.md`, `docs/settings/README.md` (General row of the §2 table,
  l.58, and a paragraph like the `general.compute` one), `docs/architecture/commands.md` (count 330 → 337 with the
  seven names, l.12), `CLAUDE.md` (command count 330 → 337; one clause in the Catalog block: header fields are read
  through the catalog's FITS keyword rules, passed explicitly; Apply never writes `override`), `docs/superpowers/open-items.md`

**Interfaces:**
- Consumes: everything above. The probe uses only public core items: `athenaeum_core::db::Database::new`,
  `header_fallbacks::{validate, HeaderFallbackConfig, apply::{read_rows, plan_frame, ScanRoots, preview, apply, PAGE_SIZE}}`,
  `events::NullEmitter`.
- Produces: `cargo run --release -p athenaeum-core --example header_fallbacks_probe -- --db <copy> [--rules <json>] (preview | apply | both <field> <KW1> <KW2>)`.
  `preview` prints the counts, then per (field, format, old value NULL or not) the number of changes and up to 5
  samples `frame_id  file name  folder  old → new`; `apply` runs `apply(..)` with `NullEmitter` and prints the counts;
  `both` counts frames whose stored header resolves both keywords to different values. Output: counts first, then
  the per-class samples, ≤ 60 lines; `preview` also prints the number of FITS rows whose stored header has a line
  that is not an 80-column card (a hand dump; Step 4).

- [ ] **Step 1: Docs.** `docs/catalog/README.md`: the field table of §5.1, the rules (O1–O3, fixed keywords, retired,
  one rule set per run, sync and Perseus follow the receiving catalog), Apply's per-field test of §5.4 with the accepted
  limitation (a typed value equal to a header candidate follows the rules), D9 (suffix kept), D10 (XISF Property pixel
  size), the readers (FITS cards through the scanner's parser incl. HIERARCH and CONTINUE; the XISF dump inverse; the
  metadata pane no longer reads `CCDTEMP` or `EGAIN`). `docs/calibration/matching.md`: Apply regroups changed raw
  calibration frames through the scanner path, frozen-lineage sets keep their frames (D11), master-library sets take
  their master's values. `docs/superpowers/open-items.md`: the owner smoke of §9.4 under "Unverified by hand"; the S4
  reuse-ignores-temperature finding, the stale-aggregate finding of §5.5 step 2 and the stale `focallen` copy of a dark
  set (`scan_integration.rs:840-843`), with what Task 5's tests showed, under the findings section.

- [ ] **Step 2: Release-note lines** — put these three lines in the task report for the 0.7.2 notes (the release skill
  rewrites `RELEASE_NOTES.md`; do not edit it here):
  - "**Choose which header keyword fills each field.** Settings → General → FITS keywords: add your camera's keyword
    (for example a temperature written only as `DET-TEMP`), reorder them, preview what changes and apply it to frames
    already in your catalog — your files are never touched. Share your setup as a file."
  - "XISF files now read the exposure from `EXPOSURE` when `EXPTIME` is missing, and their text values are trimmed."
  - "The metadata pane no longer offers a master's `EGAIN` (electrons per ADU) as its original gain."

- [ ] **Step 3: Copy the dev catalog** (never the live file):
```bash
mkdir -p ~/.athenaeum-acc/header-fallbacks
sqlite3 "$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db" ".backup '$HOME/.athenaeum-acc/header-fallbacks/athenaeum.db'"
printf '%s\tjob\theader-fallbacks\t2026-11-22\t2026-12-06\tdev catalog copy for FITS keyword fallbacks acceptance (spec 9.2-9.3)\n' '~/.athenaeum-acc/header-fallbacks' >> ~/.athenaeum-acc/REGISTRY.tsv
```

- [ ] **Step 4: Acceptance §9.2 — default rules.** Run the probe `preview` on the copy. Every non-zero class must be
  one of: frames cataloged before 0.7.1 with only `DET-TEMP`; XISF frames with `EXPOSURE` and no `EXPTIME`; XISF text
  with padding or blanks; XISF numbers the old `str::parse` refused (padding, D-notation) or keywords in non-upper case;
  values from the aligned stored-header reader. Name the class of every count with its samples in the report. Also
  report the FITS rows whose stored header is not 80-column cards (a hand dump such as `EXPTIME = 300`: its 7-letter keys
  parse as cards, so `has_keywords()` is true, the eq-text fallback is skipped and the other keys are lost; the values
  count as kept and nothing is written, so `values_kept` is inflated by these rows). Anything
  else is a parity bug: STOP and report it with samples, do not continue to Step 5. Record the preview's duration.

- [ ] **Step 5: Acceptance §9.3 — reversed temperature order.** On the same copy: `apply` with default rules, then
  `preview` reads 0. SQL snapshot before (into `~/.athenaeum-acc/header-fallbacks/`): typed values
  (`SELECT id, ccd_temp, focallen, object FROM frames WHERE override = 1`), light memberships
  (`SELECT sm.frame_id, n.frames_set_id FROM session_members sm JOIN sessions s ON s.id = sm.session_id JOIN imaging_nights n ON n.id = s.imaging_night_id ORDER BY 1, 2`).
  Rules `{"fields":{"ccdTemp":{"order":["DET-TEMP","CCD-TEMP"],"retired":[]}}}`: `both ccdTemp DET-TEMP CCD-TEMP` = N;
  `preview` ccdTemp count = N minus the typed ones (explain any difference); `apply`; `preview` again reads 0; the SQL
  snapshot after: memberships identical (`diff` empty); typed `focallen`/`object` identical; typed `ccd_temp` identical
  except frames whose value equalled a header candidate (the documented limitation — list them).

- [ ] **Step 6: Full gates** (each waits for other builds first):
  `… cargo test -p athenaeum-core -- --test-threads=4`, `… cargo test -p athenaeum-web -- --test-threads=4`,
  `… cargo check -p athenaeum-tauri`, `… cargo check -p perseus`, ts_contract check, `npm test`, `npx tsc --noEmit` → all
  PASS. Known flakes: `docs/superpowers/open-items.md` "Known flaky test"; name any you hit.

- [ ] **Step 7: Clean up** — delete the scratch catalogs under `~/.athenaeum-acc/header-fallbacks/` that the report does
  not need, keep the copy until the owner's smoke, and update its registry row's purpose; run
  `python3 -I ~/.claude/skills/lean-execution/disk-report.py` and paste its line for this job.

- [ ] **Step 8: Commit** — the probe and docs; message `docs(catalog): FITS keyword fallbacks — rules, Apply, readers; acceptance probe` + trailers.
