# Backlog — v0.6.5

Opened 2026-09-18, the day after v0.6.4 was tagged. Every item below was raised
by the owner on 2026-09-18 in one list; the same day each one was traced to the
code that owns the behaviour, and what follows is that trace — what is already
*known*, the question that still has to be answered, and a proposed call. No
item here has been planned or started.

Sizes are rough: **XS** an hour, **S** a session, **M** a plan with a few tasks,
**L** a cycle of its own with a spec.

## Proposed order

1. Quick wins that need no decision — items 5, 6, 2, 4.12, 4.6 (the drizzle
   toggle), 4.10's Analysis copy fixes (the scoring itself stays as it is).
2. Item 3 (nights re-derived on open) — small Rust change, one decision below.
3. Item 1 (XISF masters) — Rust, needs the WBPP verification first.
4. Item 4 as ONE settings redesign cycle with a spec: tab order, the Blink tab,
   autosave, the shared checkbox, search, per-field reset. Frontend-only.
5. Item 4.11 (date warnings): the owner read the trace on 2026-09-18 and saw
   no problem with what is counted — no fix round; the two genuine defects
   (bias never warns, a set's span never widens) stay listed for a later call.

## 1. Master calibration files as XISF — SHIPPED

**Shipped 2026-09-18** (plan
`docs/superpowers/plans/2026-09-18-xisf-output-masters-and-calibrated-lights.md`,
acceptance `docs/superpowers/research/2026-09-18-xisf-acceptance-run.md`):
`calibration.master_format` (Settings → Calibration), XISF masters written
with `bounds="0:65535"` and the `imageType` attribute WBPP reads as
authoritative (a dark flat as `MasterDark`), registered through the scanner's
own XISF parser; a rebuild keeps the file's container. On real data the XISF
master, the lights calibrated with it and a mixed FITS+XISF stack are
bit-identical to their FITS twins. Owed: the PixInsight/WBPP open (open-items).


**Owner's ask.** A setting in the Calibration settings that picks the container
built masters are written in, FITS or XISF, "the way WBPP supports".

**What is known.** One writer, one extension literal, one FITS-only re-parse:

- `api/masters.rs:1433` calls `write_fits_f32` unconditionally; `MasterRecipe`
  (`masters.rs:71-86`) carries no format.
- `calibration_library/paths.rs:88` is the single `.fits` literal; the three
  collision resolvers are already extension-generic.
- `calibration_library/register.rs:107` re-parses the freshly written file with
  `parse_fits_with_header`, which is FITS-only — an `.xisf` fails there before
  any DB write (a clean abort, nothing corrupted, but the build fails after
  writing real bytes). The XISF pair the scanner uses (`parse_xisf` +
  `extract_xisf_header`, `fits_parser/mod.rs:580/150`) is what belongs there;
  `register.rs:133-136` already stamps `files.format = XISF` by extension, and
  `is_master` is decided in the shared `finalize_frame`, so an XISF
  `IMAGETYP = 'Master Dark'` classifies correctly.
- `rebuild_master` writes back to the catalog's own path and re-syncs through
  the format-aware `scanner::resync_catalog_rows_from_disk` — survives XISF
  unchanged once the writer is switched.
- Light calibration and the hot-pixel map read masters through
  `BandSource::open` → `spill_via_read_raw` (`integration/banded.rs:304`), which
  works for XISF — **but rustafits' reader normalizes every float sample against
  `bounds` and multiplies by 65535** (`rustafits/src/formats/xisf.rs:412-418`).
  A built calibration master is raw ADU (identity scales in
  `integration::engine`, no `ATH_CSCL`), and the existing writer hard-codes
  `bounds="0:1"` (`fits_writer/xisf_writer.rs:227`) because a stacking master is
  u16/65535. Written that way, an ADU master would read back 65535× too large
  and every dark subtraction against it would be silently wrong. Writing it with
  `bounds="0:65535"` round-trips exactly (`v/65535 × 65535 = v`).
- `xisf_writer::image_type_for` (`xisf_writer.rs:233-244`) knows only
  `MasterLight` and `WeightMap`; XISF 1.0 table 12 defines `MasterBias`,
  `MasterDark`, `MasterFlat` — `MasterDarkFlat` has no counterpart.
- The export/WBPP layout is extension-agnostic (`export/file_organizer.rs`,
  `rawWithMasters` gates on `is_master_library`, never on `.fits`); nothing there
  changes. `docs/export/README.md` and `docs/calibration-reference.md` name
  `IMAGETYP` as the only keyword WBPP reads for masters and say nothing about
  the container.
- The switching template exists in stacking: `OutputFormat` (`stacking/config.rs:322`),
  `output_extension` + `write_output` (`stacking/master_cards.rs:427-472`),
  `OutputPanel.tsx:19-22`.

**What WBPP actually requires — read from its own source, 2026-09-18**
(`/Applications/PixInsight/src/scripts/BatchPreprocessing/BPP-Helper.js`,
`BPP-StackEngine.js`, the version installed on the owner's Mac):

- **The container is not optional.** `BPP-StackEngine.js:724-728` rejects any
  master whose extension is not `.xisf`: *"master calibration files must be
  in XISF format"*. A FITS master is refused outright, whatever its header
  says — this is why the FITS masters Athenaeum writes today cannot feed WBPP.
- **Master detection** (`BPP-Helper.js:639-641`, `:734-741`): `isMaster` is
  true when the `IMAGETYP` value contains the substring `master`
  (case-insensitive); optionally also when the file PATH contains `master`
  (`isMasterFromPath`, behind the "detect master including full path"
  option). Our `Master Dark` / `Master Flat` / `Master Bias` pass.
- **Frame type from `IMAGETYP`** (`imageTypeFromKeyword`, `:605-636`):
  lower-cased with only the FIRST space removed, then matched against
  `masterbias` / `masterdark` / `masterflat` / `flatdark` / `darkflat` … So
  `Master Dark Flat` becomes `masterdark flat` → `Unknown` → smart naming
  from the path takes the LAST of BIAS/DARK/FLAT/LIGHT in the file name —
  for `master_darkflat_…` that is **FLAT**, i.e. a master dark flat would be
  ingested as a master flat. Must be prevented (next point).
- **XISF properties override the keywords** (`:866-877`): after the FITS
  keywords, WBPP calls `getXISFProperty("PCL:Image:Type",
  info[0].imageType)` — that is the `<Image imageType="…">` attribute as
  PixInsight's reader exposes it (`pjsr/ImageType.jsh`: `MasterBias = 5`,
  `MasterDark = 6`, `MasterFlat = 7`) — and it sets BOTH `imageType` and
  `isMaster`. So the attribute is the authoritative channel: write
  `imageType="MasterBias" | "MasterDark" | "MasterFlat"`, and for a dark flat
  `imageType="MasterDark"` (WBPP treats dark flats as darks matched by
  exposure) while `IMAGETYP` keeps our own `'Master Dark Flat'` for the
  scanner.
- **Matching keys it reads from the keywords**: `EXPTIME`/`EXPOSURE` (a
  master dark is matched at `MIN_EXPOSURE_TOLERANCE = 0.01` s,
  `BPP-Global.js:89`), `XBINNING`/`BINNING`/`CCDBINX`, `FILTER`/`INSFLNAM`
  (flats), `BAYERPAT`, `DATE-OBS`, `XPIXSZ`, `FOCALLEN`; group matching adds
  the image SIZE. `build_master_cards` already stamps `IMAGETYP`, `INSTRUME`,
  `EXPTIME`, `CCD-TEMP`, `XBINNING`, `FILTER`, `GAIN`/`OFFSET` (its tests pin
  them), so the keyword side needs no new card.
- **`bounds`**: mandatory for float samples (the v0.6.2 lesson). Chosen
  convention for calibration masters: samples stay raw ADU and the writer
  stamps `bounds="0:65535"` — rustafits' reader then returns `v/65535·65535
  = v`, exactly the ADU the light-calibration engine expects, and the file
  states its true range instead of pretending to be unit-scaled. Owner smoke
  owed: open one such master in PixInsight and run WBPP with it — the one
  check that proves the whole contract.

**Proposal.** New setting `calibration.master_format` (`settings/mod.rs` beside
`CALIBRATION_LIBRARY_DIR`, default `fits`), a `get/set` pair on both hosts, a
select in Settings → Calibration beside "Master Build Memory". In core:
`xisf_writer` takes `bounds` and an explicit `imageType` (the four master arms,
dark flat → `MasterDark`); `paths.rs` takes the extension; `register.rs`
dispatches the re-parse on extension. The
`direct_registration_matches_scanner_ingestion` pin must be extended to the
XISF case — it is the one test that proves the byte-identical registration
invariant, and XISF is a new path through it. **M.**

### 1b. Calibrated lights as XISF — SHIPPED for export and send

**Shipped 2026-09-18** (same plan, Tasks 5–6): `CalibratedLightOptions.format`,
an Output format radio on the Export tab's calibrated-lights section, carried
by export, summary and the frame-set send; `c_*.xisf` equals `c_*.fits` bit
for bit. **Ruling R6:** the stacking run's own intermediates stay FITS
(`PlaneReader` is FITS-only) and the Calibrate panel says so. Follow-up, not
started: XISF intermediates in the run (a `PlaneReader` XISF arm), if the
working folder's `calibrated/` tree is ever meant for WBPP directly.


The owner's note "xisf calibration files save should be in the calibrate
settings in stacking" reads as: the container for *calibrated lights* belongs
in the Stacking → Calibrate panel, next to flat-norm / hot-pixel / debayer.

**What is known.** Calibrated output is FITS by construction:
`export/models.rs:403-413` (`c_<stem>.fits`), `light_cal.rs:277-288`
(`write_calibrated_output` = `write_fits_f32` + hash), the CFA-mosaic second
write at `calibrated_generator.rs:603-610`, and the stacking run's stage-1
names at `stacking/run.rs:1571/1582`. `CalibratedLightOptions` is the natural
carrier; `CalibratePanel.tsx` has no format control; `docs/export/README.md:268`
promises "an XISF source always yields a `.fits` output". The stacking pipeline
reads its own calibrated frames through `PlaneReader`
(`integration/plane_reader.rs:22-28`), which is **FITS-only** — so an XISF
calibrated frame would break Measure/Register/Integrate, not just export.

**The question.** Whether this is wanted for the *export* output (WBPP-bound,
where XISF is the native container) or also for the stacking run's
intermediates (where it buys nothing and costs a `PlaneReader` XISF arm).
Proposed: export/send output only; the run keeps FITS intermediates. **M**, after
item 1 (shares the writer changes).

## 2. Acknowledgements — the iroh community

**What is known.** `src/pages/About.tsx:211-248` is hand-written JSX: AstroDom,
then "Standards & Data" (FITS, XISF, Gaia DR3, HEALPix). `iroh` appears nowhere
in `src/`; `backendDeps` (`About.tsx:27-40`) lists neither `iroh` nor
`iroh-blobs`, and `solvemyastro` — a workspace member — is absent from the page
entirely. **XS.** Add an iroh entry (project + community, n0 / the iroh Discord)
to Acknowledgements and the two crates to the dependency table; add
`solvemyastro` while there.

## 3. Nights re-derived on open, without the button

**Owner's ask.** "Recalculate nights" should happen automatically, e.g. every
time the user opens the object; check whether huge sets suffer.

**What is known.** `recalculate_frame_set_nights` (`api/frame_sets.rs:54-68`) is
one transaction: one `SELECT` of member ids, one `IN (…)` fetch of their rows,
`detect_sessions` in memory (sort + one linear pass, O(n log n), no queries in
the loop), `DELETE FROM imaging_nights` (sessions/members cascade), then one
`INSERT` per night, per session and **per member frame**, plus one metadata
`UPDATE`. For the 368-frame acceptance set that is ~380 statements under one
fsync — milliseconds, and it scales linearly. The page's own read
(`get_frame_set_detail` → `get_imaging_nights_with_sessions`,
`db/operations.rs:3294-3392`) already fans out one query per night and per
session, so the re-derive is not the expensive half of an open.

Two things a delete-and-reinsert DOES churn: `sessions` is a uuid table
(`schema.rs:3`, `UUID_TABLES`) so every session gets a fresh uuid and
`created_at` on every re-derive — nothing keys on `sessions.uuid` today, but
it would make the rows look new to any future sync — and the fallback night for
`DATE-OBS`-less frames is stamped `Utc::now()` (`sessions/mod.rs:79-88`), so
its span moves on every run. There is no user-editable column on
`imaging_nights`/`sessions`/`session_members` (no name, note or manual
assignment), so nothing the user typed is lost.

Nights are already re-derived by every path that adds frames: manual merge,
auto-merge (both "Find new images" paths and the monitor pass) — the button only
exists for sets stored before the 2026-09-05 fix.

**Proposal.** Make the re-derive a *reconcile*: compute `detect_sessions`, compare
with the stored nights (spans + member sets), write only on drift, and give the
fallback night a stable span (the set's own min/max, or a fixed sentinel) so it
never counts as drift. Call it from `FrameSetDetail`'s mount before `loadData`
(one extra command, no modal, a `debug!` when nothing changed and an `info!` with
counts when it wrote). Keep the toolbar button one more release as an explicit
"force", then drop it. Skip archived sets. **S.** Decision for the owner: silent
on drift, or a one-line notification ("nights recalculated — 3 nights, 4
sessions")?

**Standing decision (owner, 2026-09-18): nights and the calendar are UTC.**
Session detection is gap-only (no sunset/sunrise assumption — a "night" is a
run of frames with no gap above the threshold), the night label reads its
dates in UTC, and the Shoot Calendar keys a day on `DATE(start, '-12 hours')`
in UTC. So a night's label and calendar day are the same for every
participant of a collaboration whatever their zone, at the cost of an
observer far from UTC seeing a date shifted by one for early-evening starts
(east of about UTC+6) or a label naming the morning date (west of about
UTC−6). Accepted; do not re-flag or add local-zone logic unasked.

## 4. Settings — one redesign cycle — SHIPPED

**Shipped 2026-09-18** (spec
`docs/superpowers/specs/2026-09-18-settings-redesign-design.md`, plan
`docs/superpowers/plans/2026-09-18-settings-redesign.md`, commits
`ec95bd55..5707a317` on `main`): seven tabs in the wanted order with a new
Blink tab and Account/Sync under Transfers; every change persists on its own
(no Save button, no banner); one `Checkbox`; Frame set grouping before Session
detection with the real explanation; a metadata registry that drives search
across every tab, `?tab=…&section=…` deep links and per-field / per-section
reset; defaults from one `get_settings_defaults` command on both hosts; the
Logging card without its duplication; `docs/settings/README.md` as the
contract for the next setting. Items 4.1–4.9 below are covered by it (4.6's
drizzle toggle landed with the quick wins); 4.10–4.12 stand as written. Owed:
the desktop click-through listed in `docs/superpowers/open-items.md`.

Everything in this item is frontend; the backend commands already exist. It
should be ONE spec and one cycle, because the pieces share a shape: a settings
*registry* (section → fields → keys → defaults) is what makes search (4.8),
per-field reset (4.9) and autosave (4.5) cheap, and doing them one at a time
would build that registry three times.

### 4.1 Tab order and where Account / Sync go — covered

Today (`Settings.tsx:584-651`): General · Transfers · Stacking · Calibration ·
Analysis · Plate Solving. Wanted: **General · Blink · Analysis · Plate Solving ·
Calibration · Stacking · Transfers**, with the Account and Sync cards
(`Settings.tsx:807-833`) moving from General to Transfers. The `?tab=` deep-link
values stay valid; `blink` is the one new value.

### 4.2 The Blink tab (new) — covered

Move out of General: "Blink Viewer" (`Settings.tsx:1113-1293`), "Flat Contour
Plot" (`939-1016`), "Star Annotation Display" (`1295-1409`). All three are
already inside the one big saved card, so they carry their keys with them.

### 4.3 Checkboxes — five styles, none the house one — covered

`src/components/folders/SwitchRow.tsx` documents the house pattern
(`accent-accent`, because `@tailwindcss/forms` is not installed so `text-*` /
`border-*` never reach the native control). Settings uses five hand-written
variants instead — `w-5 h-5 rounded border-border bg-surface-hover text-accent
focus:ring-2 …` on the two Updates checkboxes (`Settings.tsx:846/862`, the
"distorted" ones the owner saw), `w-4 h-4 …` on Monitoring/Auto-merge,
a no-ring variant on Star Annotation, `+ disabled:opacity-50` on every stacking
panel, and a bare `rounded border-border` on Analysis's "Auto". **One shared
`Checkbox`/`SwitchRow`** used everywhere in Settings and the stacking panels.

### 4.4 Frame-set grouping before session detection, with a real explanation — covered

"Clustering Parameters" (`Settings.tsx:876-910`) sits after Updates; the prose
"About Frame Set Grouping" card (`1614-1630`) is at the very bottom, after the
Save bar and outside the saved card. Merge the two into one "Frame set grouping"
section placed before "Session Detection", with the explanation rewritten to say
what the threshold does (seed-and-grow single-link on LIGHT RA/Dec, great-circle
distance, only frames not already in a set) and what a change does NOT do
(existing sets are not regrouped).

### 4.5 Autosave everywhere — covered

Persist-on-change exists in exactly one place: `StackingSection.tsx:113-204`
(500 ms debounce, `dirtyRef` so a load never writes, unmount flush). Every other
surface has a Save button: "Save Settings" (21 `set_setting` calls,
`Settings.tsx:344-511`), "Save Memory Budget", the two Transfers Saves, Logging,
Analysis, Plate Solving, Calibration Matching. Three dirty-state patterns and
three success-feedback patterns coexist.

Proposal: extract StackingSection's debounce into a `useAutosave` hook and apply
it to every section; the typed configs (`set_analysis_config`,
`set_plate_solve_config`, `set_calibration_matching_config`,
`set_logging_config`, `set_stacking_defaults`) save the whole document on a
debounce, the KV sections save the one key that changed. Keep an explicit
button ONLY where the action is not a setting: device rename (a broadcast),
"Build index now", "Clean up". Invalid input (out of range) is shown inline and
not written. The "Settings saved" banner goes; a saved tick per field or
nothing.

**Logging's duplication** (`LoggingSettings.tsx`): the four-level `<select>` is
emitted six times (base + five module rows, `148-152` vs `172-176`), the
"absent = inherit" rule is coded on both the read and the write side, the
`ATHENAEUM_LOG` override is stated in the banner AND in the success toast, and
there is no dirty state. A `LevelSelect` component + autosave collapses it.

### 4.6 Stacking settings — drizzle "Off" on the left, "2×" on the right — covered

`stageSummary.ts:317` returns `'Off'` when `!config.drizzle.enabled`;
`DrizzlePanel.tsx` has **no enabled toggle by design** (its header comment: the
toggle lives on the per-set `PipelineBoard` row) and always renders the scale
select at its stored value, 2×. `StackingSection` renders its own stage list, not
`PipelineBoard`, so Settings has no way to turn drizzle on except the
`MaximumQuality` preset, which rewrites the whole config. **XS:** an "Enable
drizzle by default" toggle at the top of `DrizzlePanel` in `global` mode (and
per-set, where it mirrors the board's row toggle). Same audit for every stage
whose summary can read "Off" while the panel shows values — Normalize's LN block
is the other one.

The "xisf calibration files" note under this heading is item 1b.

### 4.7 The `?tab=` and section anchors — covered

Search (4.8) and the "→ Coverage"-style deep links need sections addressable:
`?tab=calibration&section=dates`. Cheap once the registry exists.

### 4.8 Global settings search — covered

None exists. Closest reusable pattern: the dual-pane's local filter
(`DualPaneFileBrowser.tsx:836-852`, `1761-1786` — needle, `includes`, match
counter, clear button). Design: one input above the tab bar; while it has text
the tabs are replaced by every matching section from every tab (matching on
section title, field labels, help text and setting key), with the tab name as
a chip; clearing restores the tab. Needs the section registry (4.5).

### 4.9 Per-field reset to defaults, beside the per-section one — covered

Per-section resets exist for Stacking, Analysis, Plate Solving and Calibration
Matching (each a `reset_*` command); there is **no** reset of any kind on the
General tab, and per-field reset exists only for the folder cards (`FolderCard`
"Use default"). The typed configs already return their defaults from `reset_*`
— but that WRITES; a per-field reset needs the defaults without writing. The KV
defaults live only in Rust (`settings/mod.rs` `defaults`). Proposal: one
`get_settings_defaults` command (both hosts) returning the KV default map plus
each typed config's default document; the frontend renders a `↺` beside any
field whose value differs from its default. A section reset stays as "reset
every field here".

### 4.10 Calibration scoring parameters — do the sliders reach the score?

**Owner's question.** Whether the Scoring Parameters in Settings → Calibration →
Clustering Parameters & Thresholds (Temperature Match Weight, Temperature
Sensitivity, Exposure Match Weight, Exposure Sensitivity) correlate with the
scoring that actually runs.

**Answer.** Mostly yes, with one path that ignores three of them:

- `score_match` (`calibration/configurable_matcher.rs:728-761`) reads all four
  exactly as the panel describes — `temp_score = 1/(1 + |Δ|/scale)` is 0.5 at
  `Δ = sensitivity`, blended by the weight; same for exposure. It is called at
  `:537` for every candidate the configurable matcher enumerates — light→dark,
  flat→darkflat/dark/bias, dark→bias, and master flats — and the candidates are
  sorted by that score (`:563-567`), so the top pick follows the sliders. The
  date term is a hard-coded 30-day decay (`:738`); the panel copy is honest
  about that ("these settings control how temperature and exposure time affect
  the scoring").
- **Raw flats do not go through it.** The light→flat auto path is the grouping
  matcher (`flat_matcher.rs:186-236`), which has its own formula:
  `date_score = 1 − age/max_age` (linear, against the group midpoint),
  `temp_score = 1 − |Δ|/10` (a hard-coded 10 °C scale), and
  `match = date·(1−w) + temp·w` with `w` = Temperature Match Weight only.
  Temperature Sensitivity, Exposure Match Weight and Exposure Sensitivity have
  **no effect** on which raw flat a light gets — the very slot most users tune.
- **Max Age** ("only consider frames within this many days") gates only the raw
  flat search window (`flat_matcher.rs:140-145`) and the on-demand dark/bias
  creation (`hierarchy.rs:845-847`, `:936-938`). The configurable matcher's
  candidate query (`configurable_matcher.rs:421-430`) has **no date predicate**,
  so a two-year-old dark set is a full candidate for darks/bias/darkflats; only
  the warning threshold marks it. Time Cluster and Temp Threshold are applied
  where the legend says (set creation in `scan_integration.rs:142-166`, the
  grouping paths).

**Owner ruling 2026-09-18 — no change.** Max Age and the date thresholds are
WARNINGS, not gates for auto-link; auto-link must keep working exactly as it
does, and the raw-flat path keeps its own formula (a unification was started,
stopped and reverted the same day). What stands is the trace above: the four
sliders reach every link except the raw-flat pick, which honours Temperature
Match Weight only. If that ever becomes a complaint, the fix is a
`score_match` call in `flat_matcher.rs` — a behaviour change to auto-link,
so it needs the owner's explicit call first.

**Found on the way, Settings → Analysis** (all XS): the tab copy at
`Settings.tsx:763` promises "quality scoring weights" that do not exist
(nothing computes `quality_score`; the Analysis tab ranks by a pass/fail
threshold chain in `LightsAnalysisView.tsx:445-488`) — the eleven detection /
PSF-fit fields ARE consumed by `build_analyzer`, only the sentence is wrong;
`measure_cap` has two Rust defaults (`#[serde(default)]` 500 vs `Default`
2000, `analysis/config.rs:5/65`); `mrs_layers` help says "Default: 4" for a
default of 0; "Auto" concurrency is `0` in the UI while `Default` stores the
core count so Reset unticks it; `analysis.rejection_defaults` persists a
`trail` key the four-field bar never renders; `EMPTY_THRESHOLDS` hard-codes
`eccentricity: '0.7'` beside a placeholder saying 0.8.

### 4.11 Date warnings — what is actually counted

**Owner's question.** Whether the warning counts from the acquisition date of
the light/flat to the date of the dark/bias.

**Answer, as the code stands** (`calibration/configurable_matcher.rs:687-724`,
`hierarchy.rs`, `flat_matcher.rs:192-205`):

- Dark-for-light, and every sub-calibration: `min(|light − set.date_start|,
  |light − set.date_end|)` in whole days, **unsigned** — a dark shot after the
  light warns exactly like one shot before. Strict `> threshold`.
- Flat-for-light through the grouping path: `|light − group midpoint|` — a
  different definition from the master-flat path (min-of-edges), so the same
  slot can report two different ages depending on which branch matched.
- **Bias never warns**, at any age (`"bias" => return false`).
- For a flat's own dark/bias the "source" date is **an arbitrary member** of
  the flat set (`SELECT frame_id … LIMIT 1`, no `ORDER BY`), and the message is
  gated on `dark_date_warning_days` whatever the matched type — a
  DarkFlat-for-Flat warning is flagged by one threshold and shown by another.
- A calibration set's `[date_start, date_end]` is the first/last member at
  creation and is **never widened** when later frames join the set (the reuse
  branches update `frame_count` only), so a growing set is aged against stale
  edges. Master sets collapse to a single date.
- The score decay is a hard-coded 30-day constant (`:736-740`); the
  `max_age_days` clustering setting is NOT applied by the auto-matcher's
  candidate query. `CalibrationTolerance` threaded from `api/calibration.rs`
  is dead for warnings.
- Warnings are computed at display time, never persisted.

**Decision for the owner.** Signed or unsigned? Flats are shot after the session
by the owner's own rule (memory), darks/bias either side. Proposed: keep the
age unsigned but measure it against the **nearest member frame's** date (not
stale edges), widen the span on every insert, make bias warn like the others
(default 365), use one definition for flats, and gate each sub-calibration on
its own threshold. **S–M**, as its own calibration fix round.

### 4.12 Plate solving: "500 recommended", 500 + 2000 downloaded

Two causes (`PlateSolveSettingsPanel.tsx`, `PlateSolveIndexMissingModal.tsx`,
`catalog/gaia_prebuilt.rs:101-114`):

- Downloads are **cumulative by design**: `download_catalog_layers
  { targetDensity }` fetches every not-installed tier with `density ≤ target`.
- The catalog-missing modal **hard-codes `targetDensity: 2000`**
  (`PlateSolveIndexMissingModal.tsx:32-40`) while calling it "the recommended
  set"; the panel's own banner uses `recommendTier(fov)` and may say 500. That
  is the reported symptom, ~578 MB promised, ~2.3 GB fetched.
- The tier rows come from the hard-coded `TIER_POLICY` (500/2000/5000/8000,
  `cameraPresets.ts:24-29`), not the manifest, which publishes two tiers — a
  click on 5000 or 8000 downloads 500 + 2000 and the row stays "Download"
  forever; `needsDownload` keeps offering the banner after a successful
  install.

**XS–S:** the modal takes the same recommendation the panel computes; the rows
come from the manifest; the tooltip says "this tier and every lower one";
`needsDownload` keys on the recommended tier being installed.

## 4b. Web host: the account and sync routes can wedge the whole server — OPEN

Found 2026-09-18 while clicking through the redesigned Settings in the web
build (`athenaeum-web` release binary on a catalog copy, no keychain, no
iroh peer): `account_status`, `get_sync_status` and
`get_sync_pairing_ticket` never answered (8 s timeouts), and after the
Transfers tab had fired them a few times even the static `/settings` and
`get_settings_defaults` stopped answering — the whole server had to be
killed. The shape points at blocking work (keychain / transport start-up)
running on the async runtime's workers inside those three handlers, so a
handful of pending calls starve every other route. Not this cycle's code:
the Settings page only made the tab easier to reach. To do: reproduce with
the web build alone (`curl -m 8 -X POST /api/account_status`), find the
blocking call, move it to `spawn_blocking` or give it a timeout, and make
the two Settings cards show a "not available in this build" state instead
of "Loading…" forever.

## 5. Blink: Space starts the blink, then selects the file

**What is known.** In the dual-pane browser Space opens the Blink viewer on the
selection (`DualPaneFileBrowser.tsx:1074-1078`) and is not a select-toggle.
Inside `BlinkViewer` the convention flips: **Space = toggle the current frame's
selection**, **Enter = play/pause** (`BlinkViewer.tsx:627-634`). So the second
Space the owner pressed landed in the viewer and marked the frame. `S` and `B`
are unbound everywhere. **XS:** in the viewer, Space = play/pause, `S` = toggle
selection, Enter stays as a play/pause alias; add Space to the browser's
status-bar hint line (`:1385`), which lists every key but this one. Note the
viewer and the browser both listen on `window` — Escape already fires in both.

## 6. Shoot calendar forgets its view

**What is known.** `ShootCalendar.tsx:18-34` holds `viewMode`, `currentDate`
and `selectedDate` in plain `useState`; `App.tsx` unmounts pages on every route
change. `useSessionState` (`src/contexts/SessionStateContext.tsx`) exists for
exactly this and is already used by Objects, File Manager and Project Detail
tabs. **XS:** three `useSessionState` keys (`calendar.viewMode`,
`calendar.date`, `calendar.selected`); session-scoped, like the others — a
restart starts on today's month.
