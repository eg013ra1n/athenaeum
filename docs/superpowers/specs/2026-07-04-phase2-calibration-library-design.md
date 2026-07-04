# Phase 2 Design — Calibration Library, Master Creation, Light Calibration — 2026-07-04

Design for roadmap Phase 2 (`../plans/2026-07-02-roadmap.md` — B2/B3/B4/B5 + library
root + archive-of-originals), refining `2026-07-02-target-features-architecture.md`
Pillar B with the owner's user story. Codebase facts verified against `main` @ v0.2.4.

**Owner decisions (2026-07-04):**

- Calibration Library root holds **masters only**; raw calibration frames stay where
  they are until (optionally) archived to zip.
- Master registration is **direct** (atomic DB transaction at creation), not via a
  scanner round-trip; the library root remains a normal-scanned root so foreign
  masters dropped into it still ingest through the existing `is_master` path.
- Master-creation UI goes into the **existing Coverage tab** (no separate object tab)
  plus the Equipment library.
- **One spec, two implementation plans**: Plan A = master library (milestone M2),
  Plan B = calibrated export (milestone M2b), release + real-data shakedown between.
- **Global compute queue** with analysis migrated onto it immediately.
- Calibrated export applies **partial calibration** — "calibrate with what exists"
  is first-class; missing steps are warnings, never blockers.
- Calibration math follows the deep-research spike (§9), run during design.

---

## 1. Data model & schema

All changes use the established guarded-`ALTER TABLE` pattern in `schema.rs::init_db()`.

```sql
-- Root typing: 'normal' | 'calibration_library'
ALTER TABLE scan_roots ADD COLUMN kind TEXT NOT NULL DEFAULT 'normal';

-- A raw set that has been turned into a master leaves the matching pool
ALTER TABLE calibration_set ADD COLUMN superseded_by_set_id INTEGER
    REFERENCES calibration_set(id);

-- Provenance: row EXISTS = master built by Athenaeum; row ABSENT = imported
CREATE TABLE IF NOT EXISTS master_provenance (
  master_set_id      INTEGER PRIMARY KEY REFERENCES calibration_set(id),
  source_set_id      INTEGER REFERENCES calibration_set(id),
  recipe_json        TEXT NOT NULL,   -- combiner, sigmas, pre-cal chain, normalization, app version
  member_frame_uuids TEXT NOT NULL,   -- JSON array snapshot of source frame uuids
  member_hash        TEXT NOT NULL,   -- xxh3 over member content hashes (= ATH_HSH)
  created_at         TEXT NOT NULL
);

-- Archive operations learn a second subject kind (see §6)
ALTER TABLE archive_operations ADD COLUMN calibration_set_id INTEGER
    REFERENCES calibration_set(id);
```

- **Exactly one** `scan_roots` row may have `kind='calibration_library'`; enforced in
  `api::add_scan_root`/update (SQLite can't add a partial-unique constraint via
  guarded ALTER; code-enforced + test).
- `archive_operations`: exactly one of `frames_set_id`/`calibration_set_id` must be
  set — enforced in the planner (same ALTER limitation), `frames_set_id` becomes
  logically nullable for the new kind.
- The master → sources linkage is two-layered: **live** —
  `master_provenance.source_set_id` → existing `calibration_set_frames` → `frames`/`files`
  (works while the raw rows exist, survives archiving because archive is soft); and
  **snapshot** — `member_frame_uuids` + `member_hash`, which survives even hard
  deletion of the raw set. No duplication of per-frame metadata.
- `calibration_set.uuid` already exists (Phase 1 `UUID_TABLES`); `ATH_SRC` in the
  master's FITS header carries the *source set's* uuid, provenance carries both ids.

## 2. Calibration Library root & on-disk layout

A single designated scan root (`kind='calibration_library'`), created from Settings
or via a first-run prompt inside the Create Master dialog. Fixed v1 layout (no token
engine — export templates don't exist yet, see platform-parity audit):

```text
<LibraryRoot>/<INSTRUME>/<MasterType>/
    master_dark_300s_-10C_g100_bin1_2026-06-28.fits
    master_flat_L_1.2s_g100_bin1_2026-06-28.fits      # flats include FILTER
```

- `INSTRUME` sanitized with the existing `sanitize_folder_name`; name collisions get
  `_2`, `_3` suffixes.
- The library root is scanned like any other root: files written by the app are
  already registered (scan finds them by path — no-op); foreign masters dropped in
  by hand/sync ingest through the existing scanner `is_master` path
  (`create_master_sets_from_frames`, 1 file = 1 set) and show as **imported**
  (no provenance row).

## 3. Master creation operation

One user-visible operation; runs as a `MasterBuild` job on the compute queue (§5).

1. **Preconditions:** raw set not already superseded; frame count ≥ configurable
   minimum (default 3); library root configured (else prompt). Flats require their
   pre-calibration master(s) per the existing sub-cal links — the batch scheduler
   orders dependencies (bias/darkflat → dark → flat); a solo flat build with missing
   pre-cal masters fails with an actionable message.
2. **Integrate** (§4) → f32 planes.
3. **Write FITS** into the library via `fits_writer::write_fits_f32` (atomic
   tmp+rename). Header consolidated from member frames via `HeaderBuilder`:
   - `IMAGETYP` = `Master Dark`/`Master Flat`/`Master Bias`/`Master Dark Flat`;
   - copied/aggregated acquisition params: `INSTRUME`, `GAIN`, `OFFSET`, `EXPTIME`,
     `XBINNING`/`YBINNING`, `FILTER`, `FOCALLEN`, `TELESCOP`, `XPIXSZ`/`YPIXSZ`,
     `EGAIN`, `ROWORDER`; `CCD-TEMP` = mean + `ATH_TMIN`/`ATH_TMAX` = span;
     `DATE-OBS` = midpoint; `BAYERPAT`(+offsets) for OSC;
   - provenance: `SWCREATE='Athenaeum <semver>'`, `ATH_SRC` (source set uuid),
     `ATH_N` (frame count), `ATH_REJ` (recipe summary), `ATH_VER`, `ATH_HSH`.
   The built-in-Athenaeum marker therefore lives in BOTH the DB (provenance row) and
   the file itself — self-describing on a foreign machine.
4. **One DB transaction:**
   - insert `files` + `frames` (`is_master=1`) + `calibration_set`
     (`is_master_library=1`) rows **through a shared helper reused by the scanner
     path** (equivalence pinned by a test — the two paths must not drift);
   - insert `master_provenance`;
   - **relink**: `UPDATE calibration_set_to_frames SET calibration_set_id=:master
     WHERE calibration_set_id=:raw` — repoints light links AND sub-cal links that
     targeted the raw set, preserving `is_manual_override`/`match_score` history;
   - mark raw set `superseded_by_set_id = :master`.
5. **Post:** superseded sets are excluded from matcher candidates and auto-link
   (`WHERE superseded_by_set_id IS NULL`), and collapse in UI to a "source" block
   under their master. Optional chained archive job (§6).

**Rebuild** is a distinct action on a built master: re-runs integration, atomically
replaces the file, updates provenance (recipe/hash/created_at). Requires source
pixels on disk — if originals were archived, prompt to restore first. "Create
Master" on an already-superseded set is blocked (the master exists; offer Rebuild).

## 4. Integration engine (`athenaeum-core/src/integration/`)

- **Decode:** `astroimage::ImageConverter::read_raw` already yields linear full-res
  pixels (BZERO/BSCALE applied, no stretch, CFA left mosaiced) — the roadmap's
  "linear decode API" item reduces to a u16→f32 conversion plus a **banded reader**:
  FITS data after the header is a raw big-endian array, so reading a horizontal band
  of each frame is a cheap seek+read (reuses the existing SIMD byteswap kernels).
- **Memory model — streaming by bands:** the integrator holds `N × band` in RAM,
  never `N × frame` (100 darks × 60 MB ≈ hundreds of MB working set, not 6 GB).
  Band height chosen from a memory budget (default ~256 MB). Sigma-clip sees all N
  values per pixel within the band — single pass, no two-pass statistics.
- **Combiners:** mean, median, sigma-clip (low/high sigmas, iterations); winsorized
  sigma-clip per §9 recommendation. Defaults per frame type from §9; UI exposes an
  "Auto" preset + advanced knobs.
- **Recipes:** bias = plain combine; dark/darkflat = plain combine of **raw**
  frames (bias retained — raw-master-dark convention, §9); flat = per-frame
  pre-calibration with the matched master via the existing fallback chain
  (darkflat → exposure-matched dark → bias → synthetic constant, §9),
  multiplicative per-frame normalization to the frame's central-third mean,
  then combine.
- **Parallelism:** shared rayon `image_pool` (`min(vCPU,16)`); `par_chunks` across
  pixels within a band; hot accumulate/clip loops follow the established
  `std::arch` runtime-dispatch precedent (AVX2+FMA / NEON, as in `convolution.rs`).
  No new dependencies.

## 5. Global compute queue

New `ComputeQueue` in core services, alongside (not replacing) the disk-serial
`operation_queue`:

- **Job kinds:** `Analysis`, `MasterBuild`, `LightCalibration` (later
  `Registration`). FIFO; `compute.max_concurrent` setting, default **1** — one heavy
  CPU job at a time, owning the whole pool. This is what delivers "analysis started
  in another tab doesn't fight calibration for cores".
- Per-job cancel: queued → removed from queue; running → cooperative cancel flag
  (the `active_analyses` pattern). Progress stays on the existing per-domain
  `ProgressEmitter` events; new `compute-queue-changed` event + a
  `get_compute_queue` inspection command feed a global queue indicator (sidebar)
  showing running + waiting jobs with cancel buttons.
- **Analysis migrates now:** `api::analyze_frame_set` enqueues instead of running
  directly inside the wrapper's `spawn_blocking`; the per-frame-set Conflict guard
  stays; `analysis-progress`/`analysis-complete` event names and payloads are
  unchanged (frontend context simplifies but is not rewritten).
- The queue is in-memory (jobs are user-initiated; no persistence in v1). Both
  transports (Tauri + web SSE) get identical behavior via the shared core layer.
- Disk work (zip/move/archive) stays on `operation_queue` — CPU and disk classes
  never serialize against each other.

## 6. Archiving originals

Reuses the archive feature end-to-end (planner → staging → hash verify → zip →
verify → delete sources → finalize; resume/rollback; restore) with a new operation
subject: **a calibration set** instead of a frame set.

- **Layout:** `<archive_root>/Calibration_Archive/<INSTRUME>/<date_start>/`
  containing `<Camera>_<Type>_<params>_<daterange>.zip`; inside the zip the existing
  `<ScanRootName>/<rel path>` convention is kept so restore machinery works
  unchanged.
- **Triggers:** (a) "Archive originals after" checkbox in the Create Master dialog —
  on build success the compute job enqueues the archive job on the disk worker;
  (b) standalone "Archive originals" action on any superseded set (Equipment /
  Coverage).
- **Safe by construction:** only superseded sets can be archived this way — after
  relink the raw set has zero consumers, so the shared-calibration guard can't
  trigger. `files` rows get the standard archive markers; frames/set rows remain as
  provenance; restore rewires `files.path` as today (provenance is uuid-based and
  unaffected).

## 7. UI

**Equipment → CameraDetail** (existing tabs: files / darks / flats / master-darks /
master-flats):

- Raw tables (darks/flats): **Create Master** row action in `CalibrationSetTable`
  (next to View/Sub-Cal); superseded sets render dimmed with a link to their master.
- Master tables: **built / imported** badge (provenance presence); expanded row adds
  a provenance block — source set, N frames, temp span, recipe, `ATH_HSH`, originals
  status (on disk / archived → zip path) and the Archive originals action.
- The existing "Create Master Library" (clustering of already-scanned masters) is
  untouched.

**Object → Coverage tab** (extended, per owner decision):

- Flats/Darks/Bias tables: **Create Master** button on raw-set rows + a master
  status column (raw / queued / building / built-M).
- Toolbar: **Create all masters** — batch enqueue for the object's raw sets;
  dependency-ordered by the engine (bias/darkflat → dark → flat).
- After a build the set row becomes the master (M badge); the raw set shows in the
  expandable "source" block.
- Shared Create Master dialog (both surfaces): Auto preset + advanced recipe knobs,
  archive-originals checkbox, plan preview (what will be built, target paths,
  size/time estimate).

**Progress:** new `CalibrationProgressContext` + sidebar queue indicator mirroring
the analysis wiring (`master-build-progress`/`-complete` events, `notify()` on
completion). The global compute-queue indicator (§5) lists analysis and master
builds together.

## 8. Calibrated export ("Calibrated lights" variant)

**ExportTab** gains a variant selector: **WBPP (raw + calibration)** — today's
behavior, unchanged — and **Calibrated lights**:

- **Pre-flight per group** (filter × exposure × camera): shows exactly which
  calibration steps will be applied from the lights' linked masters. Partial
  coverage is normal — **"calibrate with what exists"**: flat-only, dark-only, any
  combination. Missing steps surface in the existing WarningsPanel with a jump to
  "Create missing masters" (Coverage); they never block. Frames with no masters at
  all export as-is (listed in the report); an off-by-default toggle can exclude
  uncalibrated frames.
- **Layout (flat, per user story):**
  `<output>/<Object>/<camera_instrume>/<original_stem>_c.fits`. Filter/exposure
  stay in headers — WBPP groups by keywords, no folder nesting needed.
- **Per-frame pipeline:** `read_raw` (linear, CFA kept mosaiced — debayer remains
  WBPP's job) → apply masters per the frame's links (formulas, order, pedestal,
  negative handling per §9) → `write_fits_f32`: original header cards preserved +
  `CALSTAT` (flags for applied steps), `PEDESTAL` (if applied),
  `SWMODIFY='Athenaeum <semver>'`, `ATH_*` uuids of masters used. `IMAGETYP`
  remains `Light Frame` (§9 interop).
- **Execution:** a `LightCalibration` job on the compute queue. Group masters load
  into RAM once; lights processed by K parallel workers (analysis worker model);
  atomic writes. Progress emits the existing `export-progress`/`export-complete`
  payloads so `ExportProgressIndicator` and cancel work without frontend changes.

## 9. Calibration math (decisions from the deep-research spike)

Full verified report with sources, evidence and votes:
`../research/2026-07-04-calibration-math-research.md`. Decisions below cite its
finding numbers; four interop questions the research could not verify are turned
into explicit v1 policies + empirical Plan B gates.

**Master recipes (the "Auto" preset):**

- **Bias:** average combine, NO normalization, NO weighting (the pedestal must be
  preserved); rejection = Winsorized sigma-clip 3σ/3σ for N ≥ 15, plain median
  below (findings 5).
- **Dark / DarkFlat:** identical combine/rejection; frames combined **raw** (bias
  retained). Athenaeum adopts the raw-master-dark convention (finding 6): on
  modern CMOS `(L − D_raw)` removes bias + dark in one subtraction and no bias
  master enters the light equation. **Dark scaling/optimization is OFF and not
  implemented in v1** — it is documented to be harmful on amp-glow CMOS and would
  require the calibrated-dark convention (findings 3, 4); matched darks come from
  the matcher instead.
- **Flat:** per-frame pre-calibration via fallback chain **master darkflat →
  exposure-matched master dark (valid only because the matcher enforced exptime
  when it created the link — it is then a darkflat in all but name) → master
  bias → synthetic constant bias (user-set ADU, finding 8) → none + warning**
  (scaled-dark pre-calibration deliberately omitted along with dark scaling);
  per-frame multiplicative normalization to the frame's **central-third mean**
  (Siril convention, findings 2, 7); rejection = percentile clipping
  (low 0.2 / high 0.02) for N < 15, Winsorized 3σ for N ≥ 15 (finding 7).
  The master flat is therefore stored **illumination-only** (already calibrated);
  its normalization constant N (central-third mean) is stamped as `ATH_FNRM` so
  light calibration doesn't recompute it (recomputed on the fly for imported
  masters lacking it).

**Light calibration (per frame, applied steps per available masters — §8):**

```text
L_c = (L − D_raw) / (F / N)          # full: raw master dark + master flat
L_c = (L − B)     / (F / N)          # no dark: bias master or synthetic constant
L_c =  L          / (F / N)          # flat-only (warn: offset left in L
                                     #   slightly under-corrects vignetting)
```

Order fixed: subtract, then divide (finding 1). `CALSTAT` records the applied
steps; a raw master dark legitimately sets both `B` and `D` (bias is inside it),
so full calibration writes `CALSTAT='BDF'` — required by consumers like VPhot
(finding 10).

**v1 policies for the research's open questions (each with a Plan B empirical
gate before the calibrated export ships):**

1. **Negatives / pedestal:** f32 output preserves negative pixels — NO clipping.
   Optional pedestal knob (adds a constant, writes `PEDESTAL`), default off.
   Gate: narrowband dataset through WBPP decides whether an auto-pedestal is
   needed and how WBPP expects `PEDESTAL` to be encoded.
2. **Output scale:** v1 keeps the input ADU scale (no rescale). Gate: feed real
   calibrated frames to WBPP and Siril; if PixInsight requires [0,1]
   normalization, add rescale-on-export + document the FITS input hints.
3. **OSC flat normalization:** global constant in v1 (matches Siril); calibration
   happens on the mosaiced CFA (debayer stays downstream), `BAYERPAT`/`XBAYROFF`/
   `YBAYROFF` cards copied through verbatim. Gate: compare per-CFA-plane vs
   global normalization for color-balance shift; promote per-plane to an
   advanced knob only if measurable.
4. **WBPP consumption recipe:** the exact WBPP settings for
   debayer-and-register-only runs are established empirically during Plan B and
   shipped as a "WBPP setup" instruction block in the export UI (pattern already
   exists for the raw WBPP variant).

**Cosmetic correction** (hot-pixel replacement) belongs strictly *after*
calibration if ever added (finding 9) — out of scope v1 (Non-goals).

## 10. Testing

- **Combiners:** unit tests on synthetic frames with exact expected outputs
  (outliers rejected by sigma-clip; mean/median agreement on clean data).
- **Golden reference test:** small real calibration set → our master vs a
  Siril-built master (primary reference — its formulas are verified from source
  in the research doc) and a WBPP-built master (secondary), per-pixel diff within
  a documented tolerance.
- **Registration-path equivalence:** rows from direct registration ==
  rows from scanner ingestion (shared helper + pinning test).
- **Relink transaction:** all consumers repointed; superseded set excluded from
  matcher; operation idempotent on re-run; manual-override flags preserved.
- **Compute queue:** FIFO order, cancel (queued + running), duplicate guards,
  analysis regression (event names/payloads unchanged on both transports).
- **Archive of a calibration set:** extends the existing archive suite
  (cancel / resume / restore / conflict).
- **Headers:** round-trip through both readers; consolidation rules (mean temp,
  midpoint date, span keywords); astropy verification script (Phase 1 precedent).
- All new commands built on `core::api` + ts-rs registry from day one (Phase 1
  convention) — web parity included.

## 11. Delivery — one spec, two plans

**Prerequisite (from Phase 1 follow-ups, hard gate):** FITS-writer hardening —
unique temp-file suffix, `sync_all` before rename, `checked_mul` on dims, zero-dim
rejection, `format_card` re-validation. Lands at the start of Plan A.

- **Plan A — Master library (milestone M2):** schema + library root; integration
  engine; compute queue + analysis migration; master creation from Equipment +
  Coverage; provenance + relink; archive-of-originals. Proof: master dark built
  from a set → lights relinked → originals zipped → restore works.
- **Plan B — Calibrated export (milestone M2b):** Calibrated variant in ExportTab;
  partial-calibration application; CALSTAT/PEDESTAL; **empirical interop gates
  from §9** (pedestal behavior, f32 scale, OSC normalization, WBPP
  debayer-only recipe) resolved with real WBPP/Siril runs before release. Proof:
  a batch of lights calibrated in-app → WBPP consumes them with calibration
  disabled, straight to registration/normalization.

Release + real-data shakedown of masters between the plans.

## Non-goals (Phase 2)

- No user-configurable naming templates (fixed v1 layout; token engine is a
  separate future feature).
- No stacking/integration of lights (stacking roadmap Phases B–D), no drizzle.
- No cosmetic correction / defect maps in v1 (WBPP's cosmetic step still applies).
- No XISF writing, no FITS compression, no BITPIX other than -32 for outputs.
- No master sharing/acceptance flows (Pillar C, Phase 6).
- No persistence of the compute queue across restarts.
