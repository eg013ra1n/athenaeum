# 6b-2 research: plan steps 3 (project frame source + plan), 7 (groups / exposure modes), 13 (R3 stacking half)

Repo `athenaeum`, branch `collab-wave6b2`, HEAD `e03bc48f`. Read-only research. All paths are relative to
`crates/athenaeum-core/src/` unless stated. Line numbers are at HEAD.

---------------------------------------------------------------------------------------------------

## 0. Headline facts / surprises (read these first)

1. **`exptimeSec` and `dateObs` are NOT in `meta`.** They are top-level manifest-row fields
   (`FrameViewWire.exptime_sec: f64`, `date_obs: Option<String>`, `collab/hub_client.rs:186-188`). Spec §4.3
   is right ("`exptimeSec`", "`dateObs`" without `meta.`); the research brief's list put them in `meta`.
2. **The manifest focal-length key is `focalLen`, not `focallen`/`FOCALLEN`** (`collab/frame_meta.rs:120-122`).
   There is **no `aperture` key** today, and **no `ctype` in `meta.wcs`** (TAN is implied).
3. **A new content version never carries `meta`.** `new_frame_version` posts only
   `{expectedVersion, blake3, byteSize, xxh3}` (`collab/hub_client.rs:577-600`). So `meta` (WCS, naxis, scale,
   and a future `aperture`) is frozen at announce. A frame plate-solved after announce never gets `meta.wcs`.
4. **No app-side parser for `meta` exists.** `meta` is read ad hoc as `serde_json::Value`
   (`export/project_collector.rs:92-114` reads only `meta.instrume`). 6b-2 needs its own typed parser.
5. **Name clash:** `db::collab_frames::FrameOrigin { Own, Replica }` (`db/collab_frames.rs:48-51`) vs
   `stacking::origin::FrameOrigin { Catalog { file_id } }` (`stacking/origin.rs:21-26`). The project frame
   source imports both; alias one.
6. **`own_staged` hazard for the stackable predicate.** An own row between `stage_own_file` and
   `set_own_version` is `own_held` with `on_disk = 1`, NEW `xxh3`/`byte_size`/`size_mtime_seen`, but the OLD
   `blake3` (`db/collab_frames.rs:1487-1533`). `own_staged` is not in `SELECT_COLS`, so `LocalFrameRow` cannot
   see it. A Prepare hash of `blake3 + byte_size` only catches the change through `byte_size`. Decide: exclude
   `own_staged = 1` rows, or put `xxh3`/`size_mtime_seen` into the hash.
7. **`build_plan` is documented as "pure DB reads plus cheap FS probes"** (`stacking/plan.rs:4-6`). The project
   arm WRITES twice: handle minting (`db::stacking::project_frame_handles`, an IMMEDIATE txn,
   `db/stacking.rs:1034-1056`) and config seeding (spec §9.4 "seeds it ... and stores it"). The doc and the
   "never call with a txn open" rule must be honoured. Both callers (`api::stacking::get_stacking_plan`,
   `run::start_stacking`) hold no transaction.
8. **`start_stacking` calls `frame_source` a SECOND time** after `build_plan` (`stacking/run.rs:813`). For a
   project, the live exchange can change `project_frames_local` between the two calls, so the run's groups may
   differ from the plan's. Today this is tolerated (`included_by_key ... unwrap_or(g.frames.len())`,
   `run.rs:869-880`).
9. **Group-key token collision risk in `stops`/`equivalent`.** `fmt_num` rounds to 2 decimals
   (`calibration_library/paths.rs:49-52`). Two `eq` classes with `e0 = 0.005` and `0.0101` both print `0.01`.
   Two `st` classes with `t0 = 0.002` and `0.004` both print `0`. The result is a
   `UNIQUE(run_id, group_key)` failure in `stacking_run_groups`. The `seconds` mode is guarded by the
   `MIN_EXPOSURE_TOLERANCE_SEC = 0.01` clamp (`stacking/config.rs:67-72, 427-438`); the new modes need their own
   guard. Also, `stopsWidth <= 0` or NaN makes "`t <= t0·2^W`" exclude `t0` itself, so it must be clamped.
10. **`IntegrationGroup.exposure_s` has four consumers that assume `seconds` semantics:**
    - the "without EXPTIME" warning, which fires on `exposure_s.is_none()` (`plan.rs:1722-1743`);
    - `PlanGroup.exposure_s` (`plan.rs:2322`);
    - `stacking_run_groups.exposure` (`run.rs:893`);
    - `master_file_name`'s `<exp>s` token (`master_cards.rs:377-401`, called at `run.rs:7714-7721`).

    `none` mode (`all`) and `stops`/`eq` need a decision for each.
11. **`stacking_run_groups.exposure_class` is never written or read.** It is only created
    (`db/schema.rs:3096-3101`). `NewGroup`, `insert_group`, `GROUP_COLUMNS`, `row_to_group` and
    `StackingRunGroupRow` all lack it (`db/stacking.rs:154-229, 405-420, 600-621`).
12. **R3 stacking half: an attested set with no links fails Calibrate today.** `resolve_generation_cached`
    bails with `"no calibration masters available (dark/flat/bias unbuilt or unlinked)"`
    (`export/calibrated_generator.rs:314-316`). Both `HashMemo` (plan staleness) and `resolve_calibrate_job`
    go through it. So R3 needs a stage-1 dispatch for attested catalog frames, not just a plan-gate skip.
13. **The funnel rule vs existing code.** Origin/subject `match`es outside the named funnels already exist:
    - `resolve_reference`'s file-row match (`plan.rs:1006-1016`);
    - `resolve_calibrate_job`'s generation read (`run.rs:1689-1699`);
    - `build_plan`'s, `start_stacking`'s and `stage_calibrate`'s FrameSet guards (`plan.rs:1678`, `run.rs:759`,
      `run.rs:2003`);
    - log-only subject matches in the heal (`run.rs:575-608`) and in cleanup (`paths.rs:894-907`);
    - `wire_set_id` (`run.rs:528-530`).

    The funnel grep test needs an allowlist, or these sites must route through funnels.

---------------------------------------------------------------------------------------------------

## 1. `project_frames_local`

### 1.1 Schema

`db/schema.rs:2636-2677`, the CREATE (verbatim):

```sql
CREATE TABLE IF NOT EXISTS project_frames_local (
    project_id           TEXT NOT NULL,
    frame_uuid           TEXT NOT NULL,
    content_version      INTEGER NOT NULL,
    origin               TEXT NOT NULL CHECK (origin IN ('own','replica')),
    publisher_account_id TEXT NOT NULL,
    publisher_display    TEXT NOT NULL,
    file_name             TEXT NOT NULL,
    filter_canonical      TEXT NOT NULL,
    state                 TEXT NOT NULL,
    accepted              INTEGER NOT NULL DEFAULT 1,
    byte_size             INTEGER NOT NULL,
    xxh3                  TEXT NOT NULL,
    blake3                TEXT NOT NULL,
    holder_count          INTEGER NOT NULL DEFAULT 0,
    manifest_version      INTEGER NOT NULL DEFAULT 0,
    manifest_json         TEXT NOT NULL DEFAULT '{}',
    landed_path           TEXT UNIQUE,
    size_mtime_seen       TEXT,
    on_disk               INTEGER NOT NULL DEFAULT 0,
    locally_declined      INTEGER NOT NULL DEFAULT 0,
    awaiting_gc           INTEGER NOT NULL DEFAULT 0,
    source_frame_id       INTEGER,
    recipe_hash           TEXT,
    last_error            TEXT,
    updated_at            TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_id, frame_uuid),
    FOREIGN KEY (project_id) REFERENCES collab_projects(project_id) ON DELETE CASCADE
)
```

Indexes:
- `idx_project_frames_local_project_xxh3(project_id, xxh3)` (`:2668-2672`);
- `idx_project_frames_local_source(source_frame_id)` (`:2673-2677`);
- `idx_project_frames_local_blake3(blake3)` (`:2901`);
- `idx_project_frames_local_state(project_id, local_state)` (`:2902`).

ALTERs, all guarded by `column_exists`:
- `rejected_size_mtime TEXT` (`:2719-2724`);
- `local_state TEXT`, `frame_seq INTEGER`, `state_changed_at TEXT` (`:2796-2813`). The one-time backfill maps
  rows to `own_held | own_missing | not_kept | idle | held | wanted` (`:2823-2840`);
- `own_staged INTEGER NOT NULL DEFAULT 0`, with a backfill (`:2849-2862`);
- `announced_at TEXT`, with a backfill (`:2871-2883`);
- `prev_stamp TEXT` (`:2894-2899`).

Related 6b-1 tables (`db/schema.rs:3073-3108`):
- `stack_project_frames(id INTEGER PRIMARY KEY AUTOINCREMENT, project_id → collab_projects CASCADE, frame_uuid,
  UNIQUE(project_id, frame_uuid))`;
- `stacking_project_config(project_id TEXT PRIMARY KEY → collab_projects CASCADE, config_json TEXT NOT NULL,
  excluded_frame_uuids_json TEXT NOT NULL DEFAULT '[]', reference_frame_uuid TEXT, updated_at TEXT NOT NULL)`;
- `stacking_run_groups.exposure_class TEXT`;
- `collab_projects.canonical_grid_json TEXT`.

### 1.2 Row type and readers (`db/collab_frames.rs`)

- `SELECT_COLS` (`:39-43`):
  `project_id, frame_uuid, content_version, origin, publisher_account_id, publisher_display, file_name,
  filter_canonical, state, accepted, byte_size, xxh3, blake3, manifest_version, manifest_json, landed_path,
  size_mtime_seen, on_disk, awaiting_gc, source_frame_id, recipe_hash, last_error, updated_at, local_state,
  frame_seq`.
  **Not read:** `own_staged`, `holder_count`, `locally_declined`, `announced_at`, `prev_stamp`,
  `rejected_size_mtime`, `state_changed_at`.
- `LocalFrameRow` (`:162-210`): the fields above, typed. `origin: FrameOrigin(Own|Replica)`,
  `accepted/on_disk/awaiting_gc: bool`, `local_state: LocalState`, `frame_seq: Option<i32>`,
  `manifest_json: String`.
- `LocalState` (`:79-149`), as DB strings: `wanted, held, missing, awaiting_choice, quarantined, not_kept, idle,
  own_held, own_missing, own_changed`. `servable()` = `Held | OwnHeld`, and is kept equal to `on_disk` by every
  writer (`:144-148`).
- Readers:
  - `get(conn, pid, uuid)` (`:782`);
  - `list_for_project(conn, pid)`, ordered by `frame_uuid` (`:793-801`);
  - `list_by_state(conn, pid, &[LocalState])` (`:1220-1239`);
  - `rows_with_landed_path` (`:1206`);
  - `find_by_landed_path` (`:1173`);
  - `rows_under` (`:1187`);
  - `find_by_project_and_xxh3` (`:1244`).
- Writers the stacking side must tolerate (spec §4.1):
  - `delete_not_in`, which deletes replica rows (IMMEDIATE txn, `:806-851`);
  - `record_own`'s `INSERT OR REPLACE` (`:868-944`);
  - `upsert_from_manifest[_deferred]` (`:276-520+`), which refreshes `filter_canonical`, `state`, `accepted`,
    `byte_size`, `xxh3`, `blake3` and `manifest_json` wholesale.
- Test seeding recipe:
  - `upsert_from_manifest(conn, pid, &view)` then `set_landed(conn, pid, uuid, path, "size:mtime")` gives a
    `held` replica (`set_landed`, `:965-985`);
  - `record_own(&row)` with `on_disk = true` gives `own_held`.
  - The JSON `view` helper `db/collab_frames.rs:1836-1844` shows the minimal `FrameViewWire` JSON.
  - `db::stacking::insert_test_project` (`db/stacking.rs:1061-1070`, `#[cfg(test)]`) writes a minimal
    `collab_projects` row.

### 1.3 Column → `GroupFrame` mapping (spec §4.3)

`GroupFrame` is at `stacking/groups.rs:62-77`. Its fields are `frame_id, file_id, filename, path, size,
modified_at, exposure_s, date_obs, width, height, pixel_scale_arcsec, scale_source`.

| GroupFrame / origin field | Source | Notes |
| ---- | ---- | ---- |
| `frame_id` | `stack_project_frames.id` via `db::stacking::project_frame_handles(conn, pid, &[uuid])` (`db/stacking.rs:1034`) | IMMEDIATE txn; idempotent (`INSERT OR IGNORE` + `SELECT`) |
| `file_id` | `0` | only `calibration_hash_for` reads it (`plan.rs:338-342`), on the Catalog arm |
| `filename` | `file_name` column | the landing writes `<publisher dir>/<file_name>` (`api/collab_live/landing.rs:277-293`); `calibrated_file_stem` takes the file stem, and two publishers' same-named files get `_f<handle>` (`run.rs:1569-1587`) |
| `path` | `landed_path` | `GroupFrame.path` is NOT read by any non-test run code today (the generation reads the catalog's light path) |
| `size`, `modified_at` | `size_mtime_seen` = `"size:mtime_secs"` (`api/collab_exchange.rs:1249-1258`, `scanner/mod.rs:809-818`) | may be NULL on an `own_held` row (`record_own` with `size_mtime_seen(path)` returning None, `api/collab.rs:4419-4428`); fall back to `byte_size`. `size`/`modified_at` are read only by `calibration_hash_for` (Catalog) |
| `exposure_s` | manifest top-level `exptimeSec` | the hub requires `0 < t <= 86400` (`api/collab.rs:~3250-3258`), so it is always `Some` for a project |
| `date_obs` | manifest top-level `dateObs` | an own row's `manifest_json` is the FrameInWire JSON, where `dateObs` is skipped when None (`hub_client.rs:233-234`) |
| `width`, `height` | `meta.naxis1` / `meta.naxis2` | the RAW source frame's `frames.naxis1/2`; publish ships OSC as CFA, so the dims match. Absent gives 0, which breaks admission sizing (`run.rs:2078-2089, 2905-2911`) and the space estimate (`paths.rs:629-662`); consider a header probe fallback |
| `pixel_scale_arcsec` | `meta.pixelScaleArcsec` | solve scale if a solve existed at publish, else header `atan(xpixsz/focallen)` (`collab/gate.rs:67-74`; slightly different formula from `groups::header_pixel_scale_arcsec`, `groups.rs:183-190`) |
| `scale_source` | `Solve` iff `meta.wcs` present, else `Header` (when a scale exists) | `meta.wcs` is present iff a `plate_solves` row existed, so this is consistent |
| colour mode | `meta.bayerpat` present → OSC (spec §5.1). Equivalently top-level `channel` (`"osc"` iff a Bayer pattern parsed, `frame_meta.rs:90-91`; the hub admits `mono\|osc\|osc-r\|osc-g\|osc-b`, `api/collab.rs:3270-3275`) | |
| filter | `filter_canonical` column | current value, remaps included (upsert refreshes it) |
| origin `Project{…}` | `frame_uuid`, `publisher_account_id`/`publisher_display`, `landed_path`, `blake3`, `size_mtime_seen`, parsed manifest | |
| state legs | `state`, `accepted`, `on_disk`, `awaiting_gc`, `local_state`, `origin` | see §2 |

### 1.4 The manifest `meta` at publish (`collab/frame_meta.rs:83-167`, `build_frame_meta`)

Inputs:
- the `frames` row: `filter, exptime, date_obs, instrume, telescop, xbinning, naxis1, naxis2, focallen, xpixsz,
  bayerpat` (`:54-76`);
- `frame_analysis` (`get_frame_analyses_by_ids`);
- `plate_solves` (`get_plate_solve`).

Top-level returned: `filter_raw` (trimmed), `channel` (`"osc"` if `Bayer::parse(bayerpat)` else `"mono"`),
`exptime_sec` (`unwrap_or(0.0)`), `date_obs`.

`meta` keys, exact names. Each is omitted (never `null`) when its source is absent:

| key | from |
| ---- | ---- |
| `instrume` | `frames.instrume` |
| `telescope` | `frames.telescop` |
| `xbinning` | `frames.xbinning` |
| `naxis1`, `naxis2` | `frames.naxis1/2` |
| `pixelScaleArcsec` | solve `pixel_scale_arcsec`, else `collab::gate::header_pixel_scale_arcsec(xpixsz, focallen)` |
| `bayerpat` | `Bayer::parse(bayerpat).as_str()` |
| `focalLen` | `frames.focallen` |
| `fwhmArcsec`, `eccentricity`, `starsDetected`, `medianSnr`, `snrWeight`, `frameSnr` | analysis (display only) |
| `wcs` | only with a `plate_solves` row |
| `calibration` | NOT built here; stamped by `api::collab::run_publish_generation` (`{dark, flat, bias}` bools, or `external: true`) |

`meta.wcs` shape, verbatim from `:133-158`:

```rust
wcs.insert("crval1".into(), json!(s.crval1));
wcs.insert("crval2".into(), json!(s.crval2));
// FITS 1-based reference pixel — the stored record is 0-based
wcs.insert("crpix1".into(), json!(s.crpix1 + 1.0));
wcs.insert("crpix2".into(), json!(s.crpix2 + 1.0));
wcs.insert("cd".into(), json!([s.cd1_1, s.cd1_2, s.cd2_1, s.cd2_2]));
if let Some(order) = s.sip_order {
    if let (Some(a_json), Some(b_json)) = (&s.sip_a_coeffs, &s.sip_b_coeffs) {
        sip.insert("order".into(), json!(order));
        sip.insert("a".into(), serde_json::from_str::<Value>(a_json)?);   // 2-D (order+1)x(order+1) table
        sip.insert("b".into(), serde_json::from_str::<Value>(b_json)?);
        if let Some(ap) = &s.sip_ap_coeffs { sip.insert("ap".into(), …) }
        if let Some(bp) = &s.sip_bp_coeffs { sip.insert("bp".into(), …) }
        wcs.insert("sip".into(), Value::Object(sip));
```

So `meta.wcs = {crval1, crval2, crpix1 (1-based), crpix2, cd: [cd1_1, cd1_2, cd2_1, cd2_2], sip?: {order, a, b,
ap?, bp?}}`. There is no CTYPE, so TAN is implicit. The SIP tables are the stored JSON 2-D arrays
(`frame_meta.rs:241-249` test seeds `(order+1)×(order+1)`). The hub cap is 8192 bytes, and a full SIP order-5 is
pinned under it (`frame_meta.rs:337-352`).

Where the manifest row gets into `manifest_json`:
- **Replica:** `serde_json::to_string(&FrameViewWire)` in `upsert_from_manifest_deferred`
  (`db/collab_frames.rs:311`). Keys are camelCase per `FrameViewWire`:
  `frameUuid, frameSeq, publisherAccountId, publisherDisplayName, own, publisherDeviceId, fileName,
  contentVersion, blake3, byteSize, xxh3, filterRaw, filterCanonical, channel, exptimeSec, dateObs, meta,
  gateVersion, accepted, acceptedReason, state, rejectReason, manifestVersion, createdAt`
  (`collab/hub_client.rs:157-198`).
- **Own (until the next manifest pull):** `serde_json::to_value(FrameInWire)` plus inserted
  `publisherAccountId, publisherDisplayName, own, contentVersion, accepted, state, manifestVersion, createdAt,
  holderCount` (`api/collab.rs:7030-7051`). Same keys for `exptimeSec`, `dateObs`, `channel`, `meta`.
  `set_own_version` updates only `$.contentVersion, $.blake3, $.xxh3, $.byteSize` via `json_set`
  (`db/collab_frames.rs:1459-1468`); `meta` is never refreshed.

Publish options: `debayer_osc: false`, FITS (`api/collab.rs:445-458`). An in-app published OSC frame is
therefore a float32 CFA FITS. Attested/external frames are copied as is, so they can be XISF or compressed.

**`meta.aperture`: absent today.** Where to add it is `build_frame_meta` (`collab/frame_meta.rs:83`), which
already has `conn` and `frame_id`, and is used for both generated and attested/external NEW frames via
`new_frame_meta_or_hold_back` (`api/collab.rs:3288-3308`). The reader pattern to copy:
- `calibration_library/headers.rs:170-205` (`bayerpat_from_stored_header`); or
- `calibration_library/light_resolve.rs:153-183` (`source_cards_for_file`).

Both do `SELECT fi.format, fh.header FROM … JOIN fits_header fh ON fh.file_id = f.file_id … LIMIT 1`, then
`fits_parser::stored_header::parse_stored_header_keys(format, &header)` (`fits_parser/stored_header.rs:72-85`,
keys UPPERCASE; handles FITS cards, XISF XML and ASIAIR "KEY = value" dumps) `.get("APTDIA")`, parsed to f64,
finite and positive. `fits_header` has no UNIQUE on `file_id` (`db/schema.rs:1002-1010`), so use `LIMIT 1`. Only
frames announced after the change carry it; new versions never add it (headline 3).

### 1.5 Fake hub seeding (`collab/fake_hub.rs`, `#[cfg(test)]`, `collab/mod.rs:11-13`)

- `seed_frames(pid, publisher_account, &[uuid], state)` (`:1154-1162`) and
  `seed_frames_with(pid, publisher, uuids, state, |v: &mut FrameViewWire| …)` (`:1167-1253`).
  Defaults are `file_name "<uuid>.fits"`, `blake3 = hex_of(uuid,64)`, `byte_size 1000`, `filter_raw/canonical
  "L"`, `channel "mono"`, `exptime_sec 300.0`, `date_obs None`, **`meta: json!({})`**, `accepted true`,
  `content_version 1`.
- The closure can set ANY `FrameViewWire` field. `meta` is a free `serde_json::Value`, so a test can put
  `naxis1/2`, `pixelScaleArcsec`, `bayerpat`, `focalLen`, `aperture`, `instrume`, `wcs{…}` in it. The size cap
  (`MAX_META_BYTES = 8192`, `:74-75`) is enforced only on the announce route (`:2485-2490`), not on
  `seed_frames_with`.
- `update_frame(pid, uuid, |f| …)` (`:1257-1275`), for example for a remap or a new `meta`;
  `set_accepted(pid, uuid, bool, reason)` (`:1278-1287`).
- No grid support yet (no `canonical_grid`/`put_grid` in the fake; that is step 6).
- The fake delivers manifest rows only. Landing needs a real fetch. For plan/frame-source unit tests it is
  simpler to write rows directly (`upsert_from_manifest` + `set_landed`, §1.2) with a real FITS at
  `landed_path`.

---------------------------------------------------------------------------------------------------

## 2. The spec §7 stackable predicate

Spec: `state = 'published' ∧ accepted ∧ on_disk ∧ ¬awaiting_gc ∧ landed_path IS NOT NULL ∧ local_state ∈ {held,
own_held}`.

Mapped to `LocalFrameRow`:

```rust
r.state == "published" && r.accepted && r.on_disk && !r.awaiting_gc && r.landed_path.is_some()
    && r.local_state.servable()          // LocalState::Held | LocalState::OwnHeld
```

- The `local_state` DB values are `'held'` and `'own_held'` (`LocalState::as_db_str`, `db/collab_frames.rs:106-118`).
- The closest existing helper is `export/project_collector.rs:146-148` (private), verbatim:

  ```rust
  fn exportable(r: &LocalFrameRow) -> bool {
      r.state == "published" && r.accepted && r.on_disk && !r.awaiting_gc && r.landed_path.is_some()
  }
  ```

  Stackable = `exportable(r) && r.local_state.servable()`. `LocalState::servable()` is at
  `db/collab_frames.rs:146-148`.
- Since `on_disk == servable` by invariant, the extra leg is belt-and-braces. It keeps out a stale `on_disk = 1`
  row, which the backfill comment (`db/schema.rs:2814-2822`) says existed historically.
- Own pending: the `state` column is `'pending'` while moderation holds it, so it is out (R31).
  Coordinator-excluded: `accepted = 0`. The manifest upsert moves such a row to `idle`, `on_disk = 0`
  (`db/collab_frames.rs:263-268, 594`).
- **Open:** the `own_staged = 1` own rows (headline 6). They are not visible in `LocalFrameRow`; add the column
  to `SELECT_COLS`/`row_from_sql`, or query it separately.
- **Suggestion:** move `exportable` to `db::collab_frames` as `pub fn exportable(&LocalFrameRow)` with a
  sibling `pub fn stackable`, so export and stacking share one definition. Today it lives in the ungated
  `export` module, which is fine for both callers.

---------------------------------------------------------------------------------------------------

## 3. `build_plan` today (`stacking/plan.rs:1671-2449`)

### 3.1 Structure, step by step, with the project arm per step

| # | Lines | Today | Frame-set-only? | Project arm |
| ---- | ---- | ---- | ---- | ---- |
| 0 | 1678-1681 | `let StackSubject::FrameSet(frames_set_id) = subject else { return Err(Invalid(PROJECT_STACKING_UNAVAILABLE)) }` | guard | replace with a subject match |
| 1 | 1682 | `set_name = subject_label(conn, subject)?` (funnel, both arms exist, `:850-862`) | no | done |
| 2 | 1683-1685 | `resolve_config_and_exclusions(conn, settings, frames_set_id, config_override)`, then `config_hash` | YES (`get_set_config`, `:941-964`) | `stacking_project_config` read; seed on first plan; uuid→handle exclusions |
| 3 | 1687 | `let (groups, origins) = frame_source(conn, subject, &cfg.grouping)?` (funnel, `groups.rs:605-620`, refuses Project at `:610-611`) | refuses | project frame source |
| 4 | 1699-1707 | `excluded_set`; `light_frame_ids`; `compute_export_readiness_for_frames(conn, frames_set_id, &light_frame_ids)`; `collect_masters_to_build` | YES | readiness = empty default; `masters_to_build = []` |
| 5 | 1712-1743 | "N frames without EXPTIME grouped separately", fired by `g.exposure_s.is_none()` | no | projects never lack exptime; step 7 must re-key this on the class token |
| 6 | 1745-1805 | Gate 1, `masters`/`links`/`masterFiles`, from `readiness.raw_sets_unbuildable`, `dropped_masters`, `readiness.unlinked_lights`, `readiness.masters_unrebuildable` | YES | never (spec §5.0); also skipped for an attested set (step 13) |
| 7 | 1808 | Gate 2: `resolve_reference(conn, subject, &origins, &cfg, &mut blockers)`. Manual reads the `files` row by origin (`:1006-1016`, match with no wildcard) | match arm | Project arm: `landed_path`/`file_name` from the origin; spec §5.4 also wants "has WCS" |
| 8 | 1829-1891 | scale warnings. `reference_scale` = manual reference's scale, else `last_run_reference_scale(conn, frames_set_id, &groups)` (`:1068-1083`, wraps `StackSubject::FrameSet`), else `largest_group_scale` | `last_run_reference_scale` takes `frames_set_id` | project: compare against the grid's `scaleArcsec` (spec §7 "M4b scale spread, measured against the grid's scale"); make the helper subject-generic |
| 9 | 1893-1933 | Gate 3 folders: `paths::resolve_dirs(conn, settings, &cfg.paths)` + `validate_dirs(..., ValidateMode::Plan)`; `free_bytes(working)` | no | unchanged |
| 10 | 1935-1984 | estimate (in-scope frames) via `paths::estimate_bytes(&EstimateInputs{…})`; Gate 4 `space` if `free < estimate` | no, but the estimate shape is frame-set-specific | grid-size registered + prepared conversions (§3.4) |
| 11 | 1986-2365 | `HashMemo::new(origins)`; per-group loop: `find_artifact(conn, subject, key, "calibrated"\|"metrics"\|"calibrated_mosaic"\|"ln", Some(frame_id))`, `memo.calibration_hash` (origin dispatch, `:380-391`), measure/fits/ln freshness, `PlanGroup` rows | subject-aware already; the memo dispatch needs a Project arm | Project arm of `hash_by_origin` = the Prepare hash (step 4 defines `prepare_subtree`) |
| 12 | 2367-2373 | Gate 5 `frames`: "At least 3 included frames in one group" | no | spec: `frames` = "nothing stackable"; keep the ≥3 rule too |
| 13 | 2375-2376 | `compute_register_stale(conn, &cfg, subject, &groups, &reference, &mut memo)`; Project `stored_registrations` returns `[]` (`:893-901`), so it is always stale | subject-aware | fine until step 8 |
| 14 | 2394 | `cleanup_stale_note(conn, frames_set_id, &plan_groups, &stale_stages)` (`:1203-1257`, wraps `StackSubject::FrameSet`) | takes `frames_set_id` | make it subject-generic (open item) |
| 15 | 2407-2412 | Gate 6 `unsupported`: "drizzle scale must be 1, 2 or 3" | no | unchanged (+ any project-only refusals) |
| 16 | 2416 | `active_run_for_subject(conn, subject)` | no | done |
| 17 | 2428-2448 | `StackingPlan { set_id: frames_set_id, … }` | `set_id` | add the new fields; `set_id: 0` via `run::wire_set_id` (`run.rs:528-530`) or make it optional |

New, project only:
- the `grid` blocker (read `collab_projects.canonical_grid_json`; nothing reads that column today);
- `projectFrames` rows;
- warnings: not on this device per canonical filter; will be solved locally (no `meta.wcs`); outside the grid
  (needs step 6 geometry); exposure equivalence unknown (step 7).

Gate order is pinned for sets by `blocker_order_is_stable` (`plan.rs` tests, around `:6219-6226`). Give
projects a stable order too, suggested `grid, reference, folders, space, frames, unsupported`.

### 3.2 `StackingPlan` struct and its TS export

`plan.rs:230-258` (verbatim fields):
`set_id: i64, set_name: String, config: StackingConfig, config_hash: String, groups: Vec<PlanGroup>, blockers:
Vec<PlanBlocker>, warnings: Vec<String>, readiness: ExportReadiness, masters_to_build: Vec<PlanMaster>,
reference: PlanReference, frame_count: usize, included_count: usize, excluded_frame_ids: Vec<i64>,
estimate_bytes: u64, free_bytes: Option<u64>, working_dir: Option<String>, output_dir: Option<String>,
stale_stages: Vec<Stage>, active_run_id: Option<i64>`.

- TS: registered in `ts_export.rs:422` (`crate::stacking::plan::StackingPlan`), generated into
  `src/types/stacking.ts:373`. It is checked by `tests/ts_contract.rs`; regenerate with
  `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.
- A `///` doc on an exported field leaks into the TS (README "For future editors"). Use `//` comments or accept
  the regenerated docs.
- Adding `subject`, the grid state and `projectFrames` (spec §7) requires:
  - `StackSubject` (`db/stacking.rs:31-36`, serde camelCase, externally tagged `{"frameSet": 7}` /
    `{"project": "p"}`) to derive `ts_rs::TS` and join the registry. Alternatively, use a flat wire shape
    `{kind, framesSetId?, projectId?}` to match spec §12's frontend subject;
  - new wire types (for example `PlanGrid { state: unset|proposed|confirmed, grid: Option<CanonicalGrid>,
    proposal: Option<…> }`, `PlanProjectFrame { handle, frameUuid, fileName, publisher, filterCanonical,
    exptimeSec, colorMode, hasWcs, onDisk }`) registered in `ts_export.rs`;
  - `readiness: ExportReadiness` is non-optional, and the frontend reads it
    (`src/components/stacking/StackingTab.tsx:649-650`, `panels/CalibratePanel.tsx:19-42`). Recommend
    `#[derive(Default)]` on `ExportReadiness` (`api/lights.rs:61-131`; `ExportFileCounts` already derives
    `Default`, `export/models.rs:966`) and an empty value for a project, which keeps the TS unchanged;
  - `set_id`: B8 in the 6b-1 plan says 6b-2's commands step makes the wire subject-aware with "the minimal
    frontend guard". Keeping `set_id: i64` (0 for a project) plus `subject` avoids frontend churn now.
- There are no frontend object literals of `StackingPlan`/`StackingConfig` (grep), so new TS fields do not break
  `tsc`. Only spread updates exist (`OutputPanel.tsx:121, 145`).
- `PlanGroup` (`plan.rs:154-208`) should gain `exposure_class` (spec §12 "GroupsTable shows the
  exposure-class token").

### 3.3 Blocker codes today and where produced

`PlanBlocker { code, message }` (`plan.rs:131-141`). Codes in the doc: `masters | links | masterFiles | reference
| folders | space | frames | unsupported`.

- `masters`: `:1757-1767` (unbuildable raw sets); `dropped_masters_blocker`, `:1650-1663`, pushed at `:1768`.
- `links`: `:1771-1781`.
- `masterFiles`: `:1782-1805`.
- `reference`: `resolve_reference`, `:986-997` ("Choose a reference frame in Analysis") and `:1022-1028`
  ("<file> is not on disk").
- `folders`: `:1911-1932`.
- `space`: `:1972-1984`.
- `frames`: `:2367-2373`.
- `unsupported`: `:2407-2412`.

`start_stacking` refuses on `plan.blockers.first()` (`run.rs:773-775`).

New for a project (spec §7): `grid` (a new code), plus `unsupported`, `frames` and `space` with project
meanings. `reference` stays (Manual mode).

### 3.4 How `space` is estimated for a set

`paths::estimate_bytes(&EstimateInputs)` (`paths.rs:565-589` struct, `:624-…` fn). Per group:
- `planes` = 3 for OSC, else 1, regardless of `debayer_osc`;
- calibrated = Σ frames `planes·w·h·4`;
- `.athf` per (frame, plane);
- registered = `n·planes·max_w·max_h·4` (`max_w/h` = the largest member);
- Bayer mosaic;
- master (+2 rejection maps);
- rejection bitmaps;
- drizzle output and weight.

`build_plan` passes the in-scope groups (`:1938-1970`), compared against `free_bytes(validated.working)`.

For a project (spec §7, "`space` estimates the registered artifacts at grid size × planes × frames, plus the
prepared conversions"), `EstimateInputs` needs:
- a registered/master geometry override (grid `widthPx × heightPx`, × drizzle scale for drizzle);
- a calibrated term that counts only the PREPARED files (in-place files cost 0).

Which frames convert is unknown without probing. Cheap options: by extension (`.xisf`, `.fz`), or a
first-2880-byte header probe (BITPIX/ZIMAGE/NAXIS3). OSC with `debayer_osc` on always writes a 3-plane debayered
file.

---------------------------------------------------------------------------------------------------

## 4. Config store (spec §9.4)

- **Frame set:**
  - `db::stacking::get_set_config(conn, set_id) -> Option<SetConfigRow{config_json, excluded_frame_ids:
    Vec<i64>, updated_at}>` (`db/stacking.rs:460-464, 1076-1102`);
  - `set_set_config(conn, set_id, config_json, &[i64])` (upsert, `:1104-1121`);
  - plan side: `resolve_config_and_exclusions` (`plan.rs:941-964`, verbatim logic): override wins outright;
    else `resolve_config(set_json, read_global_config_json(conn, settings))` (`config.rs:408-471`:
    whole-config, set JSON > global JSON > built-in; clamps `exposureToleranceSec`, `detectionSigma`,
    `tpsSmoothing`, large-scale ints);
  - API side: `resolve_set_stacking_config`, `get_stacking_config` (`isDefault = set_row.is_none()`),
    `set_stacking_config` (`api/stacking.rs:315-373`);
  - work usage and cleanup reuse `resolve_set_stacking_config` for the `paths` override (`api/stacking.rs:820,
    855`).
  - **There is no seeding for sets**: a set with no row resolves to the global default on every read and
    stores nothing.
- **Project, needed:**
  - `db::stacking::{ProjectConfigRow{config_json, excluded_frame_uuids: Vec<String>, reference_frame_uuid:
    Option<String>, updated_at}, get_project_config, set_project_config}` over `stacking_project_config`
    (`updated_at` is NOT NULL);
  - the plan's project arm: no row → `resolve_config(None, global)` → set `grouping.exposure_mode = Stops`,
    `output.coverage_map = true` → store (`INSERT OR IGNORE` or IMMEDIATE, so two concurrent first plans
    cannot clobber) → resolve. When a `config_override` is supplied the override still wins; decide whether
    the seed is still written (recommend: yes, seed the default, never the override);
  - exclusions: map uuids → handles. Either join `stack_project_frames` (as `stored_reference_frame` does,
    `plan.rs:875-884`, verbatim:
    `SELECT h.id FROM stacking_project_config c JOIN stack_project_frames h ON h.project_id = c.project_id AND
    h.frame_uuid = c.reference_frame_uuid WHERE c.project_id = ?1`) or reuse the frame source's uuid→handle
    map. An excluded uuid with no handle is ignored;
  - API (step 12): `get_project_stacking_config` → `{config, excludedFrameUuids, referenceFrameUuid,
    isDefault, updatedAt}`, `set_project_stacking_config`, and a `resolve_project_stacking_config` for work
    usage and cleanup paths.
- **Presets** (`api/stacking.rs:379-645`):
  - built-in `preset(Default|FastPreview|MaximumQuality)` = `StackingConfig::default()` + transforms
    (`config.rs:370-394`);
  - user presets in the `stacking.presets` setting, with `paths` stripped on save.
  - **Spec: "Presets stay subject-free."**
  - **Caveat:** applying a built-in preset to a project yields `exposureMode = seconds`, `coverageMap = false`
    (the frame-set defaults). The project tab (6c) should re-apply the project seed fields after a preset, or
    the presets need a subject overlay. Flag for the planner and 6c.

---------------------------------------------------------------------------------------------------

## 5. Groups (spec §8)

### 5.1 `groups.rs` today

- Key: `group_key(color, filter, binning, exposure_s)` → `compose_key` → verbatim
  `format!("{color_tok}__{filter_token}__bin{binning}__{}", exposure_token(exposure_s))` (`:131-160`).
  `exposure_token` = `format!("{}s", fmt_num(e))` or `"unknown"` (`:165-170`).
- Filter token: `sanitized_or(filter, "NoFilter")` (`sanitize_for_filename`, `:195-208`). Bucketing keys on the
  sanitized token ("one key ⇔ one group", `:464-474`).
- Members: `load_group_members(conn, set_id)` (`:257-293`), from session_members → sessions → imaging_nights →
  frames(Light) → files, with `f.focallen, f.xpixsz` already loaded. `frames.focallen REAL` exists
  (`db/schema.rs:822`).
- Solved scales: one chunked query over `plate_solves` (`:305-326`).
- Colour: `color_mode_of(bayerpat)`; any non-blank → OSC (`:344-349`).
- Bucket: `(is_osc, filter_token, xbinning.unwrap_or(1))` (`:506-517`).
- Exposure: `cluster_indices(members, cfg)` (`:403-442`) takes known exptimes sorted ascending and starts a new
  cluster when `e - first_of_cluster > exposure_tolerance_sec`. Unknown exptimes form one cluster labelled
  `None` → `unknown`. Indices are restored to member order. It works on `&[LightMember]`, which is
  catalog-specific; refactor it to source-agnostic inputs so the project source shares it.
- Group sort: frames desc, total exposure desc, key (`:580-590`).
- `IntegrationGroup` (`:86-110`): `key, instrume (anchor), color_mode, filter (raw), binning, cameras,
  exposure_s (cluster label), frames, total_exposure_s, pixel_scale_arcsec (median)`.
- Struct-literal sites: `IntegrationGroup` at `groups.rs:565` and in `paths.rs` tests; `GroupFrame` at
  `groups.rs:364` and in `paths.rs` tests.

### 5.2 `GroupingConfig` (`config.rs:100-114`)

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupingConfig { pub exposure_tolerance_sec: f64 }
impl Default for GroupingConfig { fn default() -> Self { GroupingConfig { exposure_tolerance_sec: 2.0 } } }
```

`OutputConfig` (`config.rs:309-315`) is `Copy + Eq + Default`, with `format, cleanup`. Adding
`coverage_map: bool` and `crop_to_coverage: u32` keeps it `Copy + Eq`.

### 5.3 What adding `exposureMode` + `stopsWidth` touches

- `GroupingConfig`: add `exposure_mode: ExposureMode` (`#[default] Seconds`, `Stops`, `Equivalent`, `None`;
  serde camelCase gives `"seconds" | "stops" | "equivalent" | "none"`; consider the Rust name `NoSplit` with
  `#[serde(rename = "none")]` to avoid `None` shadowing) and `stops_width: f64` (default 1.0). Register
  `ExposureMode` in `ts_export.rs` (near `:400`, beside `GroupingConfig`).
- `resolve_config`: clamp `stops_width` (NaN, <= 0) like `exposureToleranceSec` (`config.rs:427-438`).
- `calibration_subtree` (`config.rs:654-676`) already emits only
  `"grouping": { "exposureToleranceSec": cfg.grouping.exposure_tolerance_sec }`. **No change; the golden
  literals hold.**
- **The golden pin test** `stage_hashes_are_pinned_to_golden_literals` (`plan.rs:6277-6348`) computes
  calibration/measure/registration/LN hashes for `default`, `fastPreview`, `maximumQuality` and `custom`
  (`exposure_tolerance_sec = 5.0`, LN on, TPS), and asserts
  `calibration_subtree(default)["grouping"] == {"exposureToleranceSec":2.0}`. The literals must stay
  byte-identical. The spec asks to "extend it with new config fields": add a case with `exposure_mode = Stops`,
  `stops_width = 2.0`, `coverage_map = true`, `crop_to_coverage = 3` and assert hashes EQUAL the `default` row.
- `config_hash_is_canonical_and_pinned` (`config.rs:1477-…`) WILL move (whole-config JSON). Re-pin once with a
  comment, as every earlier field addition did. Adding the grouping and output fields in one commit moves it
  once.
- `serde_names_follow_the_spec` (`config.rs:997-1020`): add needles.
- `groups.rs::exposure_outside_the_tolerance_splits` (`:732-736`) uses the struct literal
  `GroupingConfig { exposure_tolerance_sec: 2.0 }` and needs `..Default::default()`. It is the only literal
  site.
- TS: `src/types/stacking.ts:195` `GroupingConfig` and `:295` `OutputConfig` regenerate (ts_contract).

### 5.4 Class tokens and their consumers

Spec tokens: seconds `<t>s` (today's), stops `st<t0>`, equivalent `eq<e0>` (unknown N → `<t>s`), none `all`.

- Project key (spec): `"<mono|osc>__<canonical>__<class token>"`, with no binning and no camera.
- Frame-set key: `<color>__<filter>__bin<n>__<token>` (in seconds mode exactly today's).
- Token formatting: `fmt_num` is 2-decimal (headline 9). Use a significant-digit formatter for `st`/`eq`, or
  add a collision guard; pin it with a test.
- Consumers to update for non-seconds modes:
  - `IntegrationGroup.exposure_s`;
  - the EXPTIME warning (`plan.rs:1722-1743`; re-key it on the `unknown` class, not `exposure_s == None`);
  - `PlanGroup.exposure_s`;
  - `stacking_run_groups.exposure` (`run.rs:893`);
  - `master_file_name` (`master_cards.rs:377-401`, `<exp>s` today); without a class token, two `stops`
    classes of one filter would differ only by the `_2` collision suffix. Same issue M11 solved for binning
    (`master_cards.rs:370-376`).
- Spec §8 has an `equivalent` warning: "exposure equivalence unknown for N frames — grouped by seconds".

### 5.5 FOCALLEN and APTDIA

- **Frame set:**
  - FOCALLEN is the `frames.focallen` column (already in `LightMember`, `groups.rs:253, 261`);
  - APTDIA has **no `frames` column**. It is in the stored `fits_header.header` blob, read by
    `fits_parser::stored_header::parse_stored_header_keys(FileFormat, &str) -> HashMap<UPPERCASE, String>`
    (`fits_parser/stored_header.rs:72-85`) and needs `files.format` ("FITS"/"XISF");
  - APTDIA is also on the register writer's copy-through allowlist (`stacking/register/writer.rs:38-42`) and
    in `calibration_library/light_headers.rs:38`;
  - do a bulk chunked query (500/chunk, like `load_solved_scales`) only when `exposure_mode == Equivalent`.
- **Project:** FOCALLEN = `meta.focalLen`; APTDIA = `meta.aperture` (new, §1.4).
- `N = focal / aperture` when both are finite and > 0. `e = t / N²`.

### 5.6 Where `stacking_run_groups.exposure_class` gets written

Nowhere today. It is needed in:
- `NewGroup` (`db/stacking.rs:405-420`);
- `insert_group` (`:600-621`);
- `GROUP_COLUMNS` / `row_to_group` / `StackingRunGroupRow` (`:154-229`), whose TS regenerates;
- `run::start_stacking`'s insert (`run.rs:882-897`);
- `test_context` (`run.rs:10975`).

`NewGroup` literal sites: `run.rs:884, 10975`; `plan.rs:4398, 5300, 5739` (tests); `db/stacking.rs:1212, 1607`
(tests). Seven sites to edit.

---------------------------------------------------------------------------------------------------

## 6. R3 stacking half (spec §13)

- **The flag:**
  - `db::collab::frames_set_attested(conn, id) -> Result<bool>` (`db/collab.rs:783-792`), verbatim
    `SELECT calibrated_externally FROM frames_set WHERE id = ?1`, missing → false;
  - written by `db/collab.rs:769-776`;
  - column `frames_set.calibrated_externally INTEGER NOT NULL DEFAULT 0` (`db/schema.rs:1727-1729`).
- **Readiness already carries it.** `ExportReadiness.attested: bool` (`api/lights.rs:127-130`) is filled at
  `:520-523`, with a read failure warned and treated as false. `check_mode_ready(CalibratedLights)` returns
  `Ok` when `r.attested` (`:194-196`); that is 6a, merged. The export pass-through is in
  `export/data_collector.rs:625-645` (it returns early, no generation).
- **The plan gate** builds `masters | links | masterFiles` from `compute_export_readiness_for_frames(conn,
  frames_set_id, &light_frame_ids)` (`plan.rs:1706`, `api/lights.rs:272-538`) and `collect_masters_to_build`
  (`plan.rs:1551-1644`, from `raw_sets_buildable` / `masters_rebuildable`).
- **For an attested set:**
  - wrap gate 1 (`plan.rs:1757-1805`) in `if !readiness.attested`;
  - force `masters_to_build = []` (otherwise `stage_masters` would build masters nobody needs; it is a no-op
    only when the list is empty, `run.rs:1352-1356`).
- **The run, Masters:** `stage_masters` is a no-op on an empty list.
- **The run, Calibrate:** `stage_calibrate` (`run.rs:2001-…`) refuses projects (`:2002-2005`). For each frame it
  calls `resolve_calibrate_job` (`:1604-1741`), which:
  - gets the stage-1 hash via `rc.memo.calibration_hash_checked` → `HashMemo::hash_by_origin`
    (`plan.rs:380-391`) → `calibration_hash_for` → `resolve_generation_cached`. This **bails for an unlinked
    attested light** ("no calibration masters available"), so the frame is excluded as "calibration failed";
  - checks freshness of `calibrated` (+ `calibrated_mosaic`) with `is_fresh` (`plan.rs:715-722`: hash
    equality + live size == row size);
  - resolves `GenerationSpec` by origin (`run.rs:1689-1699`);
  - writes `calibrated/<group>/c_<stem>[_d].fits` through `execute_generation` + `commit_calibrated`
    (`:1873-1951`).
- **What R3 needs:**
  1. a stage-1 hash for attested catalog frames that does not resolve masters:
     `stage_hash(&prepare_subtree(cfg), &[], &[SourceIdentity{file_id, size, modified_at}])`, the catalog
     identity, since a catalog frame has no blake3;
  2. a Prepare branch in `stage_calibrate` (shared with the project path from step 4):
     - in-place `calibrated` artifact = `GroupFrame.path` (`files.path`) when the plane reader decodes it;
     - else convert, or debayer into `layout.prepared` (step 4's `debayer_file` extracted from
       `export/calibrated_generator.rs:~519`);
     - `calibrated_mosaic` → the in-place file for Bayer drizzle.
- **Where "attested" lives in the run.** Options:
  - (a) carry it on the origin: `FrameOrigin::Catalog { file_id, attested: bool }` (or a separate variant),
    filled by `frame_source` from one `frames_set_attested` read. Then `HashMemo::hash_by_origin` and
    `resolve_calibrate_job`'s origin match are the dispatch points, and the funnel rule holds;
  - (b) a run-level `Stage1Mode` on `RunContext` and `HashMemo::new(origins, mode)`.

  **My recommendation is (a).** The plan's gate also needs `readiness.attested`, which gives two reads of one
  flag per call (consistent).
- **Test constraint for (a):** `frame_source_returns_one_catalog_origin_per_frame` (`plan.rs:~6560-6597`)
  asserts the exact `FrameOrigin::Catalog { file_id }` value.
- **The in-place `calibrated` artifact is safe against cleanup.** Cleanup only `remove_dir_all`s layout
  subtrees (`paths.rs:875`; run-end rej dir `run.rs:1180-1194`) and drops rows. No artifact path is ever
  `remove_file`d (grep). A landed or catalog file referenced in place is never deleted (spec §9.3).
- **Colour-mode caveat for attested sets:** colour comes from `frames.bayerpat`. An externally calibrated light
  that is already debayered (3-plane, no BAYERPAT) groups as "mono" while being 3-plane. The Prepare/LN channel
  logic (`plan.rs:824-830 expected_ln_channels`) would then disagree with the file. This edge is not covered by
  the spec.

---------------------------------------------------------------------------------------------------

## 7. Run-path consumers of `GroupFrame` fields (non-test `run.rs`, `plan.rs`, `paths.rs`)

| Field | Consumer |
| ---- | ---- |
| `frame_id` | everywhere: artifact keys, origins, maps |
| `file_id` | `calibration_hash_for` `SourceIdentity` (`plan.rs:338-342`); origin construction (`groups.rs:617`, `run.rs:9883` test_context) |
| `filename` | `calibrated_file_stem` (`run.rs:1569-1587`; also used for `r_`, `.athf`, `.athln`, `.rej` stems); provenance/summary rows (`run.rs:3777, 3822, 5188, 6166, 6878`); LN stem fallback (`run.rs:8876-8879`); plan EXPTIME warning names (`plan.rs:1724`) |
| `path` | **no non-test reader.** The light path comes from the catalog via `resolve_generation_cached` (`resolve_frame_inputs`) |
| `size`, `modified_at` | only `calibration_hash_for` (`plan.rs:340-341`) |
| `exposure_s` | weights `WeightInput.exposure_s` (`run.rs:3359`); `GroupMember.exposure_s` (`run.rs:6883`) → `StackFrame.exposure_s` (`run.rs:7031`) → integration / weighted EXPTIME; group total (`groups.rs:531`) |
| `date_obs` | `GroupMember` → `StackFrame.date_obs` (`run.rs:6884, 7032`) → master DATE-OBS (`master_cards.rs:143`) |
| `width`, `height` | `group_anchor_geometry` (`run.rs:507-512`, → `stacking_run_groups.width/height`, `PlanGroup.anchor_*`); `calibrate_bytes_total` (`run.rs:1503-1505`); Calibrate admission (`run.rs:2078-2089`); Measure admission (`run.rs:2905-2911`); `measured_geometry` fallback (`run.rs:3559-3563`); WCS seed (`seed_from_solves`, `run.rs:4092`); `paths::estimate_bytes` (`paths.rs:629-672`) |
| `pixel_scale_arcsec` | registration scale gate and seed policy (`run.rs:4078-4079`); reference scale (`run.rs:4335`); plan scale warnings and median (`plan.rs:1093-1166, 1829-1891`); group median (`groups.rs:536-542`) |
| `scale_source` | `PlanGroup.scale_source` (`plan.rs:2346-2363`) only |

Origin-dispatched catalog reads by frame id (6b-1 funnels; each needs a Project arm when the variant is added):
- `stored_solve` (`origin.rs:42-51`);
- `cfa_geometry` (`origin.rs:54-65`);
- `HashMemo::hash_by_origin` (`plan.rs:380-391`);
- `resolve_reference`'s match (`plan.rs:1006-1016`);
- `resolve_calibrate_job`'s match (`run.rs:1689-1699`, which already has an `Err(e)` arm).

Through them:
- `solve_of` (`run.rs:4016-4040`);
- the Output solve (`run.rs:9746-9752`);
- the Bayer drizzle phase (`run.rs:7960-7990`).

Adding `FrameOrigin::Project` breaks compilation at exactly these five matches.

---------------------------------------------------------------------------------------------------

## 8. Recommended task decomposition (steps 3, 7, 13)

Compile-order facts:
- the project config seed needs `ExposureMode` and `coverageMap` (spec step 7 / step 10 fields), so the config
  fields must land before the plan's seeding;
- the project frame source needs a class key, so either share step 7's classing or implement seconds-only first
  and refactor later;
- R3's Prepare branch needs step 4 (Prepare engine, `debayer_file`, `prepare_subtree`).

Suggested order: **C0 → 7a → 3a → 3b → 3c → 3d → 3e → 7b → 7c → 7d … (step 4) … 13a → 13b**. 13a can land any
time after 3e.

### C0. Config fields (prerequisite)

- `ExposureMode` + `stops_width` (+ clamp), `OutputConfig.coverage_map` / `crop_to_coverage`, all
  `#[serde(default)]`, no version bump.
- `calibration_subtree` untouched.
- Tests:
  - the golden pin is unchanged, plus a new-fields case proving the stage hashes equal `default`;
  - `config_hash_is_canonical_and_pinned` re-pinned once;
  - `serde_names_follow_the_spec` needles;
  - `exposure_outside_the_tolerance_splits` literal fixed;
  - `resolve_config` clamp test for `stopsWidth`;
  - ts_contract regenerated.

### 7a. Pure exposure classing

- A source-agnostic `classify(inputs: &[ClassInput{exposure: Option<f64>, f_ratio: Option<f64>}], cfg) ->
  Vec<(token, Vec<usize>)>`.
- Seconds mode = today's `cluster_indices` byte-for-byte; `group_frames` uses it.
- A safe token formatter.
- Tests: the four modes, tokens, unknown-N fallback count, `all`, the W clamp, a token collision guard.
- Constrained by every `groups.rs` test and the plan EXPTIME test `plan_warns_about_frames_without_exptime`.

### 3a. Manifest facts parser (pure)

- Typed parse of `exptimeSec`, `dateObs`, `channel`, and `meta.{naxis1, naxis2, pixelScaleArcsec, bayerpat,
  instrume, focalLen, aperture, xbinning, wcs{crval1, crval2, crpix1, crpix2, cd[4], sip{order, a, b, ap?,
  bp?}}}` from `LocalFrameRow.manifest_json`.
- Tests on both own (FrameInWire-shaped) and replica (FrameViewWire) JSON; missing keys; malformed → error naming
  the uuid.

### 3b. Stackable predicate

- `exportable ∧ servable`, plus the `own_staged` decision.
- One test per leg: own pending out, `accepted = 0` out, `awaiting_gc` out, not landed out, quarantined /
  not_kept / wanted out, held and own_held in.
- Constrained by `export::project_collector` tests if `exportable` moves.

### 3c. Project config store + seeding

- `db::stacking::{ProjectConfigRow, get_project_config, set_project_config}`.
- The plan resolver with seed-once (stops, coverageMap) and uuid → handle exclusions.
- Tests: the first plan seeds and stores; a stored doc is whole-config; the override is not stored; exclusions
  map to handles.

### 3d. `FrameOrigin::Project` + project frame source

- The variant (alias the `collab_frames::FrameOrigin` clash).
- The `frame_source` Project arm: list → stackable → parse → mint handles → `GroupFrame` mapping → buckets
  `(color, canonical token)` × classes → origins.
- The compile-forced arms:
  - `stored_solve` / `cfa_geometry`: interim `Ok(None)` or a clear error until steps 5 and 4;
  - `hash_by_origin`: interim `Err` → the plan reads "stale" until step 4 adds `prepare_subtree`, or define
    the Prepare hash here;
  - `resolve_reference`: the project file row from the origin;
  - `resolve_calibrate_job`: `Excluded`, unreachable while the start refuses.
- Tests:
  - replace the project-refusal half of `frame_source_returns_one_catalog_origin_per_frame`;
  - handles stable across two calls;
  - the spec §15.1 "Seam" test: catalog `frames` rows whose ids equal the handles, with different data; assert
    every `GroupFrame` field comes from the manifest.

### 3e. `build_plan` project arm + wire

- Subject-generic `last_run_reference_scale` / `cleanup_stale_note` (the open item).
- Project gates `grid` (read `canonical_grid_json`; unset or confirmed now, `proposed` in step 6), folders,
  `space` (`EstimateInputs` grid/prepared extension), `frames`, `unsupported`.
- Warnings: not on device per filter, solved locally, scale vs grid.
- `StackingPlan.{subject, grid, project_frames}`, `ExportReadiness: Default`, ts_contract regenerated.
- Replace `a_project_plan_is_refused_until_6b2` (`plan.rs:6406-6422`). KEEP `a_project_start_is_refused_until_6b2`
  (`run.rs:19861-…`) until the run can start projects.
- All frame-set plan tests must pass unchanged, including `blocker_order_is_stable` and the scale-warning suite.

### 7b. Classes wired into both sources

- `IntegrationGroup.exposure_class`, `PlanGroup.exposure_class`.
- Frame-set Equivalent reads APTDIA from the blob (bulk).
- The EXPTIME warning re-keyed; the equivalence warning.
- `master_file_name` class token, or defer it to step 11. Constrained by its `master_cards` tests.

### 7c. `exposure_class` persisted

- `NewGroup` / `insert_group` / `GROUP_COLUMNS` / `row_to_group` / `StackingRunGroupRow`; seven literal sites;
  ts_contract.

### 7d. Publication `meta.aperture`

- In `build_frame_meta` from the stored blob.
- Tests next to `frame_meta.rs:285-352`, including the 8192-byte cap test.

### 13a. Plan gate for attested sets

- No masters / links / masterFiles; `masters_to_build` empty.
- A plan-only test with unlinked lights.
- The non-attested masters tests unchanged (`plan_blocks_without_masters`, `plan_lists_buildable_raw_set_*`,
  `plan_blocks_missing_master_with_no_provenance`, …).

### 13b. Attested Prepare (after step 4)

- The origin carries `attested`; HashMemo gives the Prepare hash over the catalog `SourceIdentity`;
  `stage_calibrate` routes to Prepare; the in-place artifact is `files.path`.
- Tests:
  - an attested set with no links stacks end-to-end without masters (spec §15.1 R3);
  - a non-attested set byte-identical (`calibrate_fan_out_matches_the_sequential_stage_byte_for_byte`, the
    golden pins);
  - the plan reports Calibrate cached after the first run;
  - cleanup leaves the light file on disk.
- Constrained by `api/lights.rs` `an_attested_set_is_ready_for_calibrated_lights_without_masters`
  (`:1068-1083`) and the export attested pass-through tests.

Other existing tests that constrain these steps:
- `origin.rs` `the_catalog_arms_read_what_they_read_today`;
- `plan.rs` `the_hash_memo_refuses_a_frame_it_has_no_origin_for`,
  `resolve_reference_reads_the_catalog_only_for_a_frame_with_an_origin`,
  `subject_label_and_stored_reference_by_subject`, `calibration_hash_for_is_the_pinned_formula`;
- `db/stacking.rs` `project_frame_handles_are_stable_and_never_reused` (`:1560`),
  `frame_set_and_project_artifacts_never_collide` (`:1658`);
- `run.rs` `the_heal_marks_an_interrupted_project_run_failed_and_leaves_frame_sets_alone` (`:19895`),
  `work_usage_and_cleanup_of_a_frame_set_ignore_project_rows` (`:19970`);
- `paths.rs` `subject_layout_puts_a_project_under_projects_slug` (`:1888`).

Open items touching these steps:
- **Working-folder namespace overlap.** A frame set named `projects` lays out at `<working>/projects/`, and a
  project whose slug is `calibrated`, `registered`, `ln`, `fits`, `runs`, `rej`, `tmp` or `prepared` lands at
  `<working>/projects/<slug>/` (`paths.rs:34-60`). The frame set's "All" cleanup `remove_dir_all`s its fixed
  subtrees, so it would delete that project's whole tree. This must be resolved before the first project write
  (step 4's `prepared/`).
- **The funnel grep test** needs an allowlist for log-only subject matches and the FrameSet guards (headline
  13).
