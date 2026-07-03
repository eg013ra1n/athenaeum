# Phase 1 Foundation Design — UUIDs, ts-rs, Shared Command Layer, FITS Writer — 2026-07-04

Design for roadmap Phase 1 (`../plans/2026-07-02-roadmap.md`), implementing
collaboration-readiness Stage 1 + Stage 4 (`../plans/2026-06-10-collaboration-readiness.md`)
plus the FITS writer that unblocks all of Phase 2. Codebase facts verified against the
tree as of `main` @ v0.2.3.

**Owner decisions (2026-07-04):**
- FITS writer lives in **athenaeum-core** (`src/fits_writer/`), not rustafits.
- `uuid`/`updated_at` maintained by **SQLite triggers**, not write-path discipline. UUID **v4**.
- Shared command layer = **plain core handler fns + thin manual wrappers** (no proc-macro,
  no transport unification).
- Typed **keyword vocabulary ships in Phase 1** together with the writer mechanism.
- Command-layer pilot: **calibration, files, scan_roots, analysis** (churn-ranked;
  analysis is the worst duplicate). Remaining ~116 pairs convert opportunistically;
  new commands must use the layer from day one.
- ts-rs covers **all 6 hand-written type files** in one pass (`models.ts`, `archive.ts`,
  `export.ts`, `calibration-config.ts`, `plate-solve.ts`, `analysis-config.ts`).
- Release: version branch **`0.2.4`**, straight to stable (no beta stage).

---

## 1. Schema — `catalog_meta` + `uuid`/`updated_at`

### catalog_meta (one-row)

```sql
CREATE TABLE IF NOT EXISTS catalog_meta (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  catalog_uuid TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  created_at TEXT NOT NULL
);
```

Seeded idempotently in `schema.rs::init_db()` (`INSERT OR IGNORE`, `catalog_uuid` =
`Uuid::new_v4()` generated in Rust, `schema_version` = 1). `schema_version` is
*informational* (for future package/sync formats); the migration mechanism remains the
existing `pragma_table_info` column-exists guards — do not introduce `PRAGMA user_version`.

### Entity tables

`files`, `frames`, `frames_set`, `sessions`, `calibration_set`, `tags`,
`export_templates` each gain `uuid TEXT` and `updated_at TEXT` via the established
guarded `ALTER TABLE ADD COLUMN` pattern. SQLite constraint that shapes the whole
design: **`ALTER TABLE ADD COLUMN` cannot take a non-constant DEFAULT**, so UUIDs
cannot be declarative — hence triggers + backfill.

Order of operations inside `init_db` (all idempotent):

1. Guarded `ADD COLUMN uuid` / `ADD COLUMN updated_at` per table.
2. **Backfill** (Rust, one transaction per table): per-row `uuid = Uuid::new_v4()`
   where NULL; `updated_at` from the best existing timestamp (`created_at` /
   `modified_at`) else `now`.
3. `CREATE UNIQUE INDEX IF NOT EXISTS idx_<table>_uuid ON <table>(uuid)`.
4. Two triggers per table (identical for fresh and migrated catalogs):

```sql
CREATE TRIGGER IF NOT EXISTS <t>_identity AFTER INSERT ON <t>
FOR EACH ROW WHEN NEW.uuid IS NULL
BEGIN
  UPDATE <t> SET uuid = lower(hex(randomblob(4))||'-'||hex(randomblob(2))||'-4'||
                          substr(hex(randomblob(2)),2)||'-'||
                          substr('89ab', abs(random()) % 4 + 1, 1)||
                          substr(hex(randomblob(2)),2)||'-'||hex(randomblob(6))),
                 updated_at = COALESCE(NEW.updated_at, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
  WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS <t>_touch AFTER UPDATE ON <t>
FOR EACH ROW WHEN NEW.updated_at IS OLD.updated_at    -- IS (NULL-safe), not =
BEGIN
  UPDATE <t> SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = NEW.id;
END;
```

- The `WHEN NEW.updated_at IS OLD.updated_at` guard lets a future sync importer SET an
  explicit remote `updated_at` without it being clobbered.
- `recursive_triggers` is OFF by default in SQLite — the self-UPDATE inside the trigger
  does not re-fire it.
- The v4-in-SQL expression sets the version nibble to `4` and the variant nibble to
  `8/9/a/b` — a spec-valid UUIDv4. (This is why v4 over v7: v7's timestamp bits are not
  expressible in a plain SQL default/trigger, and v7's ordering buys nothing here —
  `updated_at` provides ordering.)

Rust model structs for the seven entities gain `uuid: String` + `updated_at: String`
(post-backfill they are always present); row mappers and explicit SELECT column lists
updated accordingly. Blast radius = 7 structs + their queries. `sessions` note: the
`imaging_nights` parent table is NOT in scope (not in the roadmap list); junction
tables keep composite integer PKs by design (UUIDs are portable identity for entities,
integer PKs stay for FK performance).

### Tests

Extend the `archive_schema_tests` precedent in `schema.rs`:
- fresh DB: columns/indexes/triggers exist; insert without uuid → trigger fills v4-shaped
  uuid + updated_at; update → `updated_at` bumps; update with explicit `updated_at` →
  preserved.
- simulated legacy DB (create old-shape table, then `init_db`) → backfill populates every
  row, unique index builds, re-running `init_db` is a no-op.
- `catalog_meta` seeded once; second `init_db` does not regenerate `catalog_uuid`.

## 2. ts-rs codegen — all 6 type files

- Dependency: `ts-rs` v10 (`serde-compat` on by default, `chrono-impl` feature) in
  athenaeum-core. `#[derive(TS)]` on every serde model struct/enum backing the 6 TS
  files. Sources: `src/models.rs`, `src/archive/models.rs`, `src/export/models.rs`,
  `src/file_op/models.rs`, `src/db/analysis.rs`, `src/db/calibration_links.rs`, plus
  the config structs behind `calibration-config.ts` / `plate-solve.ts` /
  `analysis-config.ts`.
- **Generator assembles the same 6 files** (not ts-rs's default file-per-type): a small
  harness using `TS::export_to_string()` with an explicit registry mapping
  type → target file, deterministic order, header
  `// AUTO-GENERATED from Rust — do not edit. Regenerate: cargo test ts_contract`.
  Frontend import paths do not change.
- **Regenerate-and-diff test**: `cargo test ts_contract` (athenaeum-core) regenerates
  in memory and diffs against the on-disk files; mismatch fails with a
  regenerate hint. A deliberate Rust field rename must fail this test (Stage 4
  acceptance). Writing mode: the same harness behind an env var
  (`TS_RS_WRITE=1 cargo test ts_contract`) rewrites the files.
- Hand-written helpers (`isMasterType`, …) move to a new non-generated
  `src/types/helpers.ts`. Existing TS `enum`s become union types (serde's string
  serialization); `helpers.ts` exports `as const` companion objects
  (`export const ImageType = { Light: 'Light', … } as const`) where the frontend
  referenced enum members; call sites adjusted.
- Mixed naming conventions are reproduced automatically from serde attributes
  (default snake_case; the 15 `rename_all = "camelCase"` structs; the literal
  `override_` field). No serde attribute changes as part of this work — for existing
  fields the generated files must match today's wire format exactly; the only additive
  difference is the new `uuid`/`updated_at` fields from §1.
- Unregistered-new-type gap: a type added with `#[derive(TS)]` but not in the registry
  is caught by `tsc --noEmit` when the frontend uses it; registry addition is part of
  the add-a-command checklist.

## 3. Shared command layer — core handlers + thin wrappers

New module `crates/athenaeum-core/src/api/{calibration,files,scan_roots,analysis}.rs`.

```rust
pub enum ApiError {
    NotFound(String), Invalid(String), Conflict(String),
    Forbidden(String), Internal(String),
}

pub async fn add_scan_root(ctx: &ServiceContext, path: String, policy: &PathPolicy)
    -> Result<ScanRoot, ApiError>;
```

- Handlers take `&ServiceContext` + typed args; progress via the existing
  `&dyn ProgressEmitter` (both transports already implement it); transport-specific
  policy (web `allowed_paths` sandboxing) passed explicitly as a `PathPolicy` argument
  — desktop passes the permissive policy. Concurrency knobs (thread budgets,
  semaphores) are plain args from the wrappers.
- **Tauri wrapper** (3–4 lines): `#[tauri::command]` +
  `#[tracing::instrument(skip_all, err)]`, `ApiError → String` via Display.
- **Web wrapper** (3–5 lines): the `{config}` wrapper extractor structs stay on the
  web side; one `impl From<ApiError> for (StatusCode, String)` — NotFound→404,
  Invalid→400, Conflict→409, Forbidden→403, Internal→500.
- **Logging contract unchanged**: boundary spans stay on the wrappers (span name = fn
  name, `FmtSpan::CLOSE` duration, `err`/`err(Debug)`); core handlers get no
  `#[instrument]` of their own (no double spans). Hot-path `level = "debug"`
  exceptions stay as-is.
- `analyze_frame_set` is the proof case: the ~200-line near-verbatim duplicate
  (`commands/analysis.rs:83-294` / `routes/analysis.rs:155-375`) collapses into one
  core handler emitting through `ProgressEmitter`; event names
  (`analysis-progress`/`-complete`) unchanged. The copy-pasted helpers
  (`recreate_calibration_sets_for_root`) merge into core alongside.
- Definition of done (pilot): in the 4 modules, command/route bodies contain only
  extraction + error mapping; the `routes/analysis.rs` "mirrors …" comment is deleted;
  the `{config}`-wrapper regression tests keep passing; CLAUDE.md's add-a-command
  checklist gains "implement core `api::` handler + two wrappers + ts-rs registry".

## 4. FITS writer — mechanism (`athenaeum-core/src/fits_writer/`)

Files: `card.rs` (card model + grammar), `writer.rs` (serialization),
`keywords.rs` (vocabulary, §5). No dependency on the rustafits render pipeline; the
data-plane input is a plain `&[f32]`.

```rust
pub enum CardValue { Logical(bool), Integer(i64), Real(f64), Str(String) }
pub struct Card { pub keyword: String, pub value: Option<CardValue>, pub comment: Option<String> }

pub fn write_fits_f32(path: &Path, width: usize, height: usize, channels: usize, // 1 | 3
                      data: &[f32], cards: &[Card]) -> Result<(), FitsWriteError>;
// plus write_fits_f32_to(w: impl Write, ...) for tests/streaming
```

Structural cards are writer-owned and auto-generated: `SIMPLE = T`, `BITPIX = -32`,
`NAXIS = 2|3`, `NAXIS1..NAXIS3`, `END`. Header padded with ASCII spaces, data padded
with zeros to 2880-byte multiples; f32 written big-endian. `BZERO`/`BSCALE` are NOT
written (identity for floating BITPIX per the standard). IEEE NaN in data is legal
and passes through untouched.

**FITS 4.0 grammar enforcement — validation errors, never silent fixes**
(`FitsWriteError` variants carry the offending keyword):

- Keyword: 1–8 chars from `[A-Z0-9-_]`; lowercase input is normalized to uppercase;
  anything else rejected. Structural keywords (`SIMPLE`, `BITPIX`, `NAXIS*`, `END`,
  `BZERO`, `BSCALE`) rejected in user cards.
- Strings: printable ASCII `0x20–0x7E` only (reject, don't transliterate); embedded
  `'` doubled; values ≤68 chars emit one card, longer values emit a spec-compliant
  **CONTINUE chain** (`&` continuation, `CONTINUE` keyword with no value indicator) —
  officially part of FITS 4.0, and already parsed by our `fits_parser` reader.
- Fixed-format values: logical `T`/`F` in column 30; integers right-justified to
  column 30; reals right-justified to column 30, always containing a decimal point,
  uppercase `E` exponent; strings open at column 11.
- Comments: appended as `/ comment`; a comment that does not fit the card is an error
  (no silent truncation). Unit conventions in comments use the standard bracket form
  (`/ [degC] sensor temperature`).
- `COMMENT` / `HISTORY`: dedicated constructors, auto-split at 72 chars into multiple
  cards.

**Round-trip tests** (both readers already exist):
1. writer → `fits_parser::FitsHeader` (athenaeum's full header reader): every
   keyword/value survives, including a >68-char CONTINUE string and doubled quotes.
2. writer → `rustafits` `ImageConverter::read_raw`: `PixelData::Float32` bit-exact,
   dims/channels correct (mono NAXIS=2 and RGB NAXIS3=3 both).
3. Edge cases: 68/69-char boundary, NaN pixels, non-ASCII rejection, 9-char keyword
   rejection, reserved-keyword rejection, comment-overflow rejection.
4. Optional dev-only validation script (not CI): `astropy.io.fits.open(...).verify('exception')`
   against a generated file, documented in the module docs as the reference-implementation
   cross-check.

## 5. Keyword vocabulary (`keywords.rs`)

A typed `HeaderBuilder` is the canonical way to assemble master/calibrated-frame
headers (raw `Card` remains the escape hatch). Every method documents units and emits
the standards-based canonical form:

| Keyword | Type / format | Canonical values / notes |
|---|---|---|
| `IMAGETYP` | Str | SBFITSEXT-style: `Light Frame`, `Dark Frame`, `Bias Frame`, `Flat Field`, `Dark Flat`; masters: `Master Light`, `Master Dark`, `Master Bias`, `Master Flat`, `Master Dark Flat`. All contain "Master" (WBPP substring detection) and all round-trip through our `ImageType::from_str`. |
| `EXPTIME` | Real, seconds | SBFITSEXT. No `EXPOSURE` duplicate is written. |
| `DATE-OBS` | Str, ISO-8601 `CCYY-MM-DDThh:mm:ss.sss` UTC | For masters: midpoint of member frames (B3). |
| `CCD-TEMP` / `SET-TEMP` | Real, °C | Unit comment `/ [degC] …`. Mean for masters; span goes to `ATH_TMIN`/`ATH_TMAX`. |
| `GAIN` / `OFFSET` | Integer | Camera driver settings (NINA convention) — distinct from EGAIN. |
| `EGAIN` | Real, e-/ADU | SBFITSEXT electronic gain. |
| `XBINNING` / `YBINNING` | Integer ≥ 1 | |
| `XPIXSZ` / `YPIXSZ` | Real, µm | After binning (SBFITSEXT). |
| `BAYERPAT` | enum `RGGB\|BGGR\|GBRG\|GRBG` | OSC only; with `XBAYROFF`/`YBAYROFF` (Integer). |
| `RA` / `DEC` | Real, degrees | Builder takes decimal degrees once and also emits `OBJCTRA` `'HH MM SS.SSS'` / `OBJCTDEC` `'+DD MM SS.SS'` (SBFITSEXT string forms). |
| `INSTRUME`, `TELESCOP`, `FOCALLEN` (mm), `FILTER`, `OBJECT`, `ROWORDER` (`TOP-DOWN`/`BOTTOM-UP`) | | NINA/SBFITSEXT conventions. |
| `SWCREATE` | Str | `Athenaeum <semver>`; `SWMODIFY` for re-writes of foreign files. |
| `CALSTAT` | Str, flag chars `B`/`D`/`F` | Reserved for B5 calibrated lights (MaxIm convention). |
| `PEDESTAL` | Integer | Reserved for B5. |

**`ATH_` namespace (spec correction).** The B3 draft used `ATHM_*`, but `ATHM_TMIN` /
`ATHM_TMAX` are 9 chars — over the FITS 8-char keyword limit. The namespace is now
**`ATH_`**: `ATH_SRC` (source calibration_set uuid — as a CONTINUE-capable string),
`ATH_N` (frame count), `ATH_REJ` (rejection algorithm+sigmas), `ATH_VER` (app version),
`ATH_HSH` (xxh3 of member-hash list), `ATH_TMIN` / `ATH_TMAX` (temperature span, °C).
All ≤8 chars. `2026-07-02-target-features-architecture.md` §B3 amended accordingly.

**Opportunistic reader fix** (in scope, tiny): the scanner gains an `EXPOSURE` fallback
for `EXPTIME` (the stored-header snapshot path already has one; the scanner does not).
The GAIN←EGAIN snapshot asymmetry is intentionally NOT touched — different semantics;
deferred.

## 6. Sequencing, release, acceptance

- Branch **`0.2.4`**, straight to stable (owner decision), standard release workflow
  (EN release notes → version bump ×5 → ff-merge to main → tag).
- Order: §1 schema → §2 ts-rs → §3 command layer → §4+§5 writer+vocabulary.
  Schema and writer are independent; ts-rs runs after schema so `uuid`/`updated_at`
  land in the generated types; the command layer runs after ts-rs so pilot modules
  are written against generated types once.
- Acceptance (= roadmap milestone M1):
  - Fresh AND migrated catalogs carry `catalog_uuid` + per-row `uuid`/`updated_at`;
    all existing tests green.
  - A deliberate Rust field rename fails `ts_contract`; `models.ts` and the other 5
    files are generated, not hand-edited.
  - In the 4 pilot modules a command is written once (core handler + 2 thin wrappers);
    logging spans unchanged.
  - A FITS file with the full vocabulary + a >68-char CONTINUE string round-trips
    through both existing readers; f32 data bit-exact.

## Non-goals (Phase 1)

- No `change_log`, no portable paths (Phase 4), no master integration (Phase 2), no
  mosaic schema (Phase 3).
- No conversion of the remaining ~116 command pairs (opportunistic later).
- No serde attribute changes, no renames of existing fields anywhere (new `uuid`/`updated_at` fields are additive only).
- No `imaging_nights` UUIDs, no junction-table UUIDs.
- No writer support for BITPIX other than -32, no FITS extensions, no compression.
