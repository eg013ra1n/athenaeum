# Target-Features Architecture — Mosaic, Master Library, P2P Collaboration — 2026-07-02

Systems design for the three product pillars, grounded in the v0.2.1 audit (`../plans/2026-07-02-v0.2.1-audit.md`). Each section: what exists, the design, schema changes, integration points, open decisions.

The pillars interlock: **masters library** feeds calibration for both export pipelines; **mosaic** defines the tile model that **collaboration** later uses for group campaigns (tile assignment per participant); collaboration Stage 1 UUIDs are the identity layer everything shares. Build order follows the roadmap doc.

---

## Pillar A — Mosaic objects (tiles) + WBPP-groupable export

### What exists

- Clustering (`clustering/`): seed-and-grow single-link on RA/Dec, threshold `grouping.threshold.value` (default 3°). Depending on tile spacing vs threshold, mosaic panels today either merge into one oversized frame set or land as unrelated per-tile sets — both wrong for the mosaic model.
- Per-frame RA/Dec (+ plate-solve refinement), rotation, focal length; raw headers in `fits_header` — enough to compute tile footprints.
- WBPP export (`export/file_organizer.rs`) builds keyword-nested folders and **already has grouping-keyword infrastructure**: `WbppExportConfig { keyword_order: ["CAMERA","BIAS","DARKS","FLAT"] }` + `WbppSetupInstructions`/`WbppKeywordInstruction { pre_checked }` (`export/models.rs:691-741`) — but only pre-processing (calibration-chain) keywords; no tile/post-processing keyword. (Note: the Siril script pipeline described in `docs/export/README.md` has been removed from the codebase; that doc is stale — see `../plans/2026-07-02-platform-parity-audit.md`.)
- No parent/child in `frames_set`; no OBJECT panel-token parsing.

### Design

**Data model** — self-referencing hierarchy on `frames_set`:

```sql
ALTER TABLE frames_set ADD COLUMN parent_set_id INTEGER REFERENCES frames_set(id); -- NULL = top-level
ALTER TABLE frames_set ADD COLUMN set_kind TEXT NOT NULL DEFAULT 'normal';         -- 'normal' | 'mosaic' | 'tile'
ALTER TABLE frames_set ADD COLUMN tile_label TEXT;                                  -- 'P1', 'x1y2', …
```

A `mosaic` parent set holds no frames directly; its `tile` children are ordinary frame sets (all existing lifecycle — sessions, calibration links, archive — keeps working untouched, because tiles ARE frame sets). "Object in object" falls out of one nullable FK.

**Detection** — a `mosaic/` module in core, two independent signals, run after clustering (frames already in sets are untouched, consistent with `auto_generate_frame_sets`):

1. *OBJECT token parser*: normalize OBJECT into `(base_name, tile_label)` — recognize NINA/SGP/APT conventions: `M31 Panel 1`, `M31 P1`, `M31_1of4`, `_x1y2_`, `Tile 3`, trailing `#N`. Same `base_name` + distinct labels ⇒ mosaic candidate.
2. *Footprint adjacency*: per-set footprint from solved WCS (or FOCALLEN + pixel size + sensor dims fallback); sets whose centers are within `k · field_diagonal` (k ≈ 0.5–1.2, configurable) and share an OBJECT base or overlap ⇒ candidate.

Auto-suggest, user confirms (like auto_merge today); manual "group as mosaic / assign tile label" always available. Store detection config under `settings` (`mosaic.*`).

**Export for WBPP** — WBPP groups files by custom keywords **matched against path or filename** with the `_KEYWORD_value_` convention, and a keyword can act in pre-processing (calibration) and/or post-processing (registration/integration). For a mosaic the correct setup is: calibration shared (pre: OFF), registration+integration split per tile (post: ON). So the export layer must:

1. Add `{TILE}` (and `{MOSAIC}`) template tokens to the existing token engine.
2. Extend `WbppExportConfig.keyword_order` with a `PANEL` level fed from `tile_label`, emitting `_PANEL_<tile_label>_` folder segments under the lights level (calibration folders stay shared across tiles).
3. Extend `WbppKeywordInstruction` with `post_checked` and emit setup instructions: keyword `PANEL`, pre-processing OFF, post-processing ON — so WBPP produces one master light per tile ready for mosaic assembly.

**UI**: Objects page shows mosaic parent → tiles; SkyChart renders tile footprint rectangles (it already draws FOV boxes — extract the marker layer first, see audit §2.5); coverage indicator (frames per tile, missing tiles).

**Open decisions**: threshold semantics for tile-vs-separate-object when OBJECT names are absent; whether a tile may belong to two mosaics (recommend: no, single parent FK).

---

## Pillar B — Master calibration library

### What exists

- Master *consumption* is complete: `ImageType::Master*` parsing, `frames.is_master`, `calibration_set.is_master_library` (1 master = 1 set, `scan_integration.rs`), `MasterPreference` in the configurable matcher, manual linking includes masters, hierarchy builder skips sub-calibration for masters (`docs/masters/masters.md`).
- Master *creation* exists only externally: export generates Siril scripts (`00_create_masters.ssf`).
- **The app has no FITS write capability at all** (reader is hand-rolled; rustafits is render-oriented). This is the enabling gap.

### Design

**Stage B1 — FITS writer** (`fits_writer` in core, or in rustafits next to the reader): minimal spec-compliant writer — 2880-byte header blocks, BITPIX=-32 (f32) primary HDU, mandatory + arbitrary keyword cards. No compression, no extensions. Symmetric to the hand-rolled reader; unit-test round-trip through the reader.

**Stage B2 — calibration integrator** (`integration/` in core). Key insight: **calibration frames need no registration** — master creation is pixel-wise robust combination, so this ships long before the full stacking engine (it needs only the "Phase A-lite" linear decode: full-res f32, BZERO/BSCALE applied, no stretch, CFA left mosaiced for OSC):

- Combiners: mean, median, sigma-clip (winsorized later). Streaming/chunked accumulation, never N full frames in RAM.
- Recipes: **bias** = plain combine; **dark** = combine (optional bias subtract if optimization enabled); **flat** = subtract matched master darkflat/dark/bias (reuse the existing fallback-chain logic from the export data collector), normalize multiplicatively per frame, combine; **darkflat** = as dark.
- Runs on the existing global `operation_queue` with `ProgressEmitter`; cancellation cooperative like archive.

**Stage B3 — header vocabulary & provenance.** Master FITS headers carry everything the matcher scores on, copied/aggregated from the source `calibration_set` + member frames:

- Standard: `IMAGETYP` (`MASTER DARK` … — parser already recognizes), `INSTRUME`, `GAIN`, `OFFSET`, `EXPTIME`, `CCD-TEMP` (mean; also `ATH_TMIN`/`ATH_TMAX`), `XBINNING`/`YBINNING`, `FILTER`, `FOCALLEN`, `TELESCOP`, `DATE-OBS` (midpoint), `BAYERPAT` if OSC.
- Custom namespace `ATH_*` (was `ATHM_*` — renamed 2026-07-04: `ATHM_TMIN`/`ATHM_TMAX` were 9 chars, over the FITS 8-char keyword limit): `ATH_SRC` (source calibration_set uuid — needs collab Stage 1), `ATH_N` (frame count), `ATH_REJ` (algorithm+sigmas), `ATH_VER` (app version), `ATH_HSH` (xxh3 of member-hash list, for dedup/provenance), `ATH_TMIN`/`ATH_TMAX` (temperature span).

Because headers are self-describing, **the library is just files**: masters are written into a managed library folder (template-named via the existing token engine, e.g. `{IMAGETYP}/{INSTRUME}/{DATE-OBS:%Y-%m}/master_dark_{EXPTIME}s_{CCD-TEMP}C_g{GAIN}.fits`) that is itself a scan root — the existing scanner ingests them through the established is_master path and the matcher picks them up with zero new matching code. Sharing a master with a teammate = sending one file (pillar C synergy).

DB provenance (queryable side of `ATH_*`):

```sql
CREATE TABLE IF NOT EXISTS master_provenance (
  master_frame_id INTEGER NOT NULL REFERENCES frames(id),
  source_calibration_set_id INTEGER,            -- NULL for imported/foreign masters
  params_json TEXT NOT NULL,                    -- recipe, rejection, member hashes
  created_at TEXT NOT NULL
);
```

**Stage B4 — UI**: "Create Master" action on a calibration set (Equipment page) → recipe dialog → queue; library browser can grow out of the existing `MasterDarkLibrary.tsx` patterns (audit says it's ready).

**Stage B5 (core deliverable — promoted from "later, optional" by owner decision 2026-07-04) — in-app light calibration** ("calibrate lights inside objects"). Explicitly two-staged per the owner's model: **stage 1** = master creation via B2 (flats, darks, and optionally biases — the bias recipe stays first-class so both dark-based and bias-based calibration paths work); **stage 2** = apply master flats + darks/biases to lights producing calibrated f32 FITS, exported so WBPP receives frames ready for registration + normalization with its calibration step skipped. **Prerequisite: a research spike** on correct calibration paths, algorithms and mechanisms incl. the math — bias vs dark(+bias) workflows, dark optimization/scaling, flat normalization, order of operations, output pedestal / negative clipping, OSC/CFA handling — producing a short spec both B2 and B5 follow. This is exactly stacking-roadmap Phase A; B1+B2 build most of its machinery, so the stacking engine gets cheaper as a side effect.

---

## Pillar C — P2P collaboration (group imaging campaigns)

### What exists

Nothing network-facing. The 2026-06-10 collaboration-readiness plan (Stages 1–4) is written but **0/4 implemented**: integer PKs collide across catalogs, paths are absolute+UNIQUE, no change journal, no catalog identity, hand-mirrored types. Runtime architecture is already multi-client-shaped (verdict unchanged).

### Design principles

1. **Never sync the SQLite file.** Sync *content* (FITS files) + *metadata manifests*; each catalog imports manifests into its own DB. Multi-writer SQLite over synced folders is explicitly unsupported.
2. **Append-only contributions.** Each participant writes only under their own contribution directory → no file-level conflicts by construction. Metadata merge = manifest import keyed by UUIDs (collision-free after Stage 1).
3. **Transport-agnostic core.** A `SharingTransport` trait with two implementations; project logic doesn't care which:
   - **Syncthing (primary, MVP)** — continuous bidirectional folder sync; runs as a sidecar daemon controlled via its REST API (`X-API-Key`), device IDs + shared-folder model maps 1:1 onto "group project". Right fit for an ongoing campaign where members contribute frames over weeks.
   - **Torrent (later)** — librqbit is an embeddable Rust session API with DHT (trackerless); right fit for one-shot immutable publication of a finished dataset or a master-calibration pack. Not suited to continuously growing folders.

### Shared-project layout (a Syncthing shared folder)

```
project-root/
├── athenaeum-project.json      # project uuid, object/mosaic definition, tile plan, members
├── contributions/
│   └── <catalog_uuid>/         # one dir per participant — append-only
│       ├── manifest.ndjson     # frame metadata rows (uuid-keyed) + content hashes
│       └── lights/…            # FITS/XISF payload (template-organized)
└── masters/                    # optional shared master library (self-describing files, pillar B)
```

- `manifest.ndjson`: one JSON object per frame — frame uuid, origin `catalog_uuid`, rel path, xxh3 content hash, full header-derived metadata (the same shape as a `frames` row). Written by the exporter, consumed by the importer; re-import is idempotent by uuid.
- The **existing scanner/monitor** watches the project folder as a special scan root; the importer layers manifest metadata over scanned files and records provenance:

```sql
-- on frames (Stage-1 uuid columns assumed)
ALTER TABLE frames ADD COLUMN origin_catalog_uuid TEXT;   -- NULL = local
CREATE TABLE IF NOT EXISTS shared_projects (
  uuid TEXT PRIMARY KEY, name TEXT, root_path TEXT, transport TEXT,
  frames_set_uuid TEXT,        -- link to local (mosaic) set
  joined_at TEXT
);
CREATE TABLE IF NOT EXISTS project_members (project_uuid TEXT, catalog_uuid TEXT, display_name TEXT, PRIMARY KEY (project_uuid, catalog_uuid));
```

- Identity = `catalog_meta.catalog_uuid` + free display name. No accounts, no server.
- Conflict semantics: content conflicts impossible (append-only dirs); metadata rows are owned by their origin catalog (only the contributor edits their rows; others get updates via manifest re-import — LWW per row by `updated_at`, journaled via Stage-3 `change_log`).

### Staged delivery

1. **Package export/import (transport-free MVP):** "Export frame set as package" (files + manifest.ndjson, optionally zipped via existing `zip_writer`) and "Import package". Works over *any* channel — a Syncthing folder the user sets up by hand, a torrent, a USB stick. Ships value immediately after Stage 1 UUIDs; validates the manifest format before any daemon integration.
2. **Syncthing integration:** detect/manage sidecar (bundle or "point me at your Syncthing"), REST-API config of device IDs + shared folder per project, sync-status surface (`SyncContext` in frontend per audit §2.5), auto-import on `monitor` events.
3. **Group mosaic campaigns:** `athenaeum-project.json` carries the tile plan (pillar A model); members claim tiles; coverage view shows who shot what; WBPP export of the combined, multi-contributor mosaic.
4. **Torrent publishing:** librqbit-based "publish dataset" (create torrent + magnet from a package; seed from within the app) for public/one-shot distribution.

### Hard prerequisites (from the collaboration-readiness plan)

Stage 1 (catalog uuid + row uuids + `updated_at`) — before anything above. Stage 3 (change journal) — before manifest *re*-import/update semantics. Stage 2 (portable paths) — before a synced folder can be a scan root that survives machine differences. Stage 4 (ts-rs + shared command helper) — not a hard blocker but every pillar adds ~10–20 new commands; pay the duplication tax down first.

**Open decisions:** hash for cross-user content verification (xxh3 is fine for integrity-vs-accident; add BLAKE3 if tamper-evidence ever matters); bundling Syncthing binary vs requiring an existing install (recommend: detect existing first, bundle later); whether masters/ in a project is auto-trusted by the matcher (recommend: imported masters require one-click acceptance into the library).
