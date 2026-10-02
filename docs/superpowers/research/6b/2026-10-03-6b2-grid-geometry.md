# 6b-2 research: the grid (step 6), registration onto the grid (step 8), drizzle and output on the grid (step 11)

Read-only research. App repo: `athenaeum`, branch `collab-wave6b2`, HEAD `e03bc48f`.
Hub repo: `athenaeum-hub`, working copy on branch `holder-device-names` (`b485578`). Everything
grid-related below is identical on hub `main` (checked with `git show main:src/routes/projects.rs`).
Line numbers are at these HEADs. The numeric experiment in §3.6 was a numpy run on stdin; no file
was written.

---

## 0. Headline findings (the things that contradict the spec or would surprise a planner)

1. **On the owner's real data, the spec's "affine + TPS residual" composed map cannot meet the spec's
   own < 0.01 px pin.** All 465 stored plate solves carry SIP order 3, with about 10–17 px of
   non-affine residual at the corners (1.87″/px, 6224×4168). With the real SIP of frame 28747
   (§3.6), TPS with 600 nodes (`TPS_MAX_NODES`, the R-M4c-5 cap) misses the exact chain by
   **0.08 px max** at 32-px cell centres (p99 0.017). With 1700 nodes it still misses by 0.034 px.
   The existing **polynomial** layer in the codebase's own shape (degree 2..=o, `fit_joint`) passes
   at **order 5**: forward ≤ 0.0035 px and inverse ≤ 0.0034 px at a 4° tangent offset. At order 4
   the inverse fails (0.09 px). The order cap is hard-coded to 2..=4 (`polynomial.rs:39-44, 76-79`;
   `eval` uses `[f64; 5]` power tables at `:56-57`). A planner needs an owner ruling: polynomial
   order 5 (cap raised for composed maps only), or a sampled displacement-grid `DistortionModel`
   variant. The sampled grid is the only option that also covers star maps that carry their own TPS
   layer.
2. **Every stored plate solve has det(CD) > 0, and every solved frame is `ROWORDER = 'TOP-DOWN'`**
   (465/465, read-only query on the dev catalog). A canonical grid built with the standard CD
   (det < 0; CDELT1 = −s, north up) is therefore a **mirror** of every frame's raw array. The
   composed maps all become `flipped`, which works. But:
   (a) A grid master must not inherit the reference's `ROWORDER = TOP-DOWN` card. Today it does,
       through the copy-through at `master_cards.rs:138` and the registered writer's allowlist. If
       it keeps that card, viewers that honour `ROWORDER` show it upside-down and mirrored.
   (b) Acceptance run A (§15.3: "the grid set to the reference frame's own scale, rotation and
       centre") cannot reproduce the reference's geometry. The hub's `CanonicalGrid` has no parity
       field, so the project master is the vertical mirror of the personal master. The Tier C rows
       (median, MAD, noise, FWHM) do not change under a flip. `imgcmp.py` has no flip option, so
       pixel-level diffs would need one.
   (c) With `output.format = xisf`, every grid master triggers the existing "bottom-up XISF displays
       mirrored" warning (R-T2-1).
3. **Drizzle does not use the single-parallelogram plan for a map with a distortion layer.**
   `SquareOverlapPlan::resolve` (`drizzle/mod.rs:450-455`) sends any `map.distortion.is_some()`
   to the **per-256-px-tile Jacobian arm** (`SquareOverlap::Tiled`). The spec's statement that
   "C-3's single-parallelogram plan is valid for affine plus a displacement layer"
   (§5.5 / §5.8, citing `:896-925`) misreads that block. Every project frame would take the Tiled
   arm. That is correct, just slower than the `Frame` arm.
4. **`set_collab_project_grid` cannot return "the stored grid" from the hub.** `PUT /grid` answers
   **204 No Content**. The app has to echo its own normalized request. Hash stability then needs
   normalization **before** the PUT: Postgres `numeric` drops the sign of −0.0, so a client that
   sent `-0.0` would hash differently from the feed's echo.
5. **Cleanup erases the spec's star-map cache.** The spec keeps the star map in the `registered`
   artifact's payload (§4.2/§5.5). `CleanupWhat::Registered` deletes `registered` rows
   (`paths.rs:883-884`), and so do `deleteIntermediates` and the `output.cleanup =
   deleteRegistered` run policy (`INTERMEDIATE_ARTIFACT_KINDS`, `paths.rs:817-826`). After any
   cleanup a project re-runs star matching, while frame sets keep `registration_results`. The disk
   risk in §16 pushes users toward exactly these cleanups. Recommendation: a payload-only artifact
   kind (e.g. `registration`) that is not in `INTERMEDIATE_ARTIFACT_KINDS`.
6. **The 0-based / 1-based mix-up in §5.9.** `PlateSolveRecord.crpix1/2` is **0-based**: the card
   writer adds +1 (`fits_writer/wcs.rs:1-2, 205-212`). The synthetic record must therefore carry
   `crpix = ((W−1)/2, (H−1)/2)`, not "the grid centre pixel (1-based)". Manifest `meta.wcs.crpix`
   **is** 1-based (`collab/frame_meta.rs:139-142`), so reading it needs −1. No parser for
   `meta.wcs` exists yet; steps 5, 6 and 8 all need one.
7. **`reference_coverage` / `MIN_REFERENCE_COVERAGE = 0.97`** (`weights.rs:640, 655-682`, used by
   the output stage's LN-anchor pick at `run.rs:~6948-6980`) measures the fraction of the
   **output geometry** a frame covers. On a grid larger than a frame almost every member fails, so
   the "coverage filter dropped" warning fires for every group. This is a step-9 concern, but it is
   triggered by step 8's geometry change.
8. Minor spec drift: §3 cites "§6.4", which does not exist (§6 has 6.1–6.3). §15.1 says "the
   grid→reference displacement grid", while §5.5 describes one composed `PixelMap`.

---

## 1. The grid on the hub

### 1.1 Storage

- Migration: `athenaeum-hub/migrations/0022_per_frame_model.sql:29-31`
  ```sql
  ALTER TABLE projects
      ADD COLUMN IF NOT EXISTS version bigint NOT NULL DEFAULT 1,
      ADD COLUMN IF NOT EXISTS canonical_grid jsonb;
  ```
- `ProjectRow.canonical_grid: Option<Value>` (`src/routes/projects.rs:193-195`). It is included in
  `PROJECT_COLUMNS` (`:198-202`).
- Shape: the hub's own typed struct (`projects.rs:1465-1476`):
  ```rust
  #[derive(Deserialize, Serialize)]
  #[serde(rename_all = "camelCase")]
  pub struct CanonicalGrid {
      pub scale_arcsec: f64, pub width_px: i64, pub height_px: i64,
      pub crval1: f64, pub crval2: f64, pub rotation_deg: f64,
  }
  ```
  The column is `jsonb`. Postgres therefore re-orders the keys (jsonb sorts by key length, then
  bytes), and numbers come back as `numeric` text. The app must **never hash the raw JSON text**
  it receives.

### 1.2 `put_grid`: route, capability, validation, response

- Route: `PUT /api/v1/projects/{id}/grid` (`src/routes/mod.rs:309-312`; README.md:127 documents it
  as `cap:project.edit`).
- Handler (`projects.rs:1478-1513`), verbatim validation:
  ```rust
  crate::collab_auth::require_cap(&state.db, id, auth.account_id, "project.edit").await?;
  let ok = g.scale_arcsec.is_finite()
      && g.scale_arcsec > 0.0
      && g.scale_arcsec <= 100.0
      && (64..=65536).contains(&g.width_px)
      && (64..=65536).contains(&g.height_px)
      && (0.0..360.0).contains(&g.crval1)
      && (-90.0..=90.0).contains(&g.crval2)
      && g.rotation_deg.is_finite();
  // !ok → 400 "grid needs scaleArcsec in (0,100], widthPx/heightPx in [64,65536], crval1 in [0,360), crval2 in [-90,90], finite rotationDeg"
  ```
  - On success it runs `UPDATE projects SET canonical_grid = $2` (the value is
    `serde_json::to_value(&g)` of the **typed** struct, so extra fields are dropped).
  - It returns 404 when the project does not exist.
  - It calls `bump_project_version_tx`, then `feed.bump(id, version, Kind::Grid)`, and records the
    event `grid_changed` with the grid as detail.
  - It responds **`204 NO_CONTENT`** with no body.
- `require_cap` → `Member::has_cap` = `is_coordinator || gov_caps ∋ cap` (`collab_auth.rs:53-55,
  163-174`). This is the same rule as the app's `has_cap` (`api/collab.rs:2126`), and
  `ProjectCard.can_edit_grid` already uses it (`api/collab.rs:2170`, added by 6a).
- No status check: a closed project still accepts a grid.
- Hub tests to mirror in the fake hub: `tests/trust_and_grid.rs:65-77` (400 on scale 0, 204 on a
  valid grid, read back as `project.canonicalGrid.scaleArcsec`), `tests/events.rs:99`, and
  `tests/feed_publish.rs:102` (the bump kind is `"grid"`).

### 1.3 Where the project wire returns it

- `ProjectView.canonical_grid` (`projects.rs:222`, mapped at `:248`), returned inside
  `project` on `GET /projects/{id_or_slug}`.
- It is mirrored at the page's top level as `ProjectPage.canonical_grid` (`:761-762`, captured at
  `:899-900`, set at `:1212`).
- The feed kind is `Kind::Grid` → `"grid"` (`src/feed/wire.rs:18-25`, lowercase).

### 1.4 The app hub client

- **No `put_grid` exists.** `patch_frame_filter` already exists (6a, `collab/hub_client.rs:717-737`).
- Template for a PUT: `set_publishing_device` (`hub_client.rs:875-899`) and `report_holders`
  (`:818-841`). The 2xx-without-body template is `set_frame_acceptance` / `patch_frame_filter`:
  ```rust
  let resp = self.http.patch(self.url(&format!("/projects/{project_id}/frames/{frame_uuid}")))
      .bearer_auth(token).json(&serde_json::json!({ "filterCanonical": canonical }))
      .send().await.map_err(net)?;
  let status = resp.status();
  if status == StatusCode::OK || status == StatusCode::NO_CONTENT { return Ok(()); }
  Err(classify(status, resp, "remap frame filter").await)
  ```
  `classify` (`:316`) maps 401/403/429 and the typed 409/410 errors. Everything else becomes
  `Http{status,…}`, so the API layer should turn a 400 into `ApiError::Invalid`.
- The API-level template (credentials, live project check, client, call, best-effort follow-up) is
  `exclude_collab_frame` (`api/collab.rs:7435-7465`).

### 1.5 `ProjectWire`, `ProjectCard` and the refresh path

- `ProjectWire` (`hub_client.rs:78-97`) has no `canonical_grid`. Add
  `#[serde(default)] pub canonical_grid: Option<serde_json::Value>` beside `goals`.
  `ProjectPageWire` is at `:113-121`.
- `ProjectCard` (`api/collab.rs:136-188`) already has `can_moderate`, `can_remap` and
  `can_edit_grid` (`:146-154`). It needs `canonical_grid: Option<CanonicalGrid>`, plus the ts
  export (`ts_export.rs:216`) and a TS regen. Note from the 6b-1 README: exported fields use `//`
  comments, not `///`, so the generated files stay byte-identical.
- The refresh path: `refresh_projects_reporting` (`api/collab.rs:2607-…`) →
  `fetch_one_project` (`:2333-2457`). That function already fetches `client.project_page(&p.id,
  Some(token))` (`:2335`), so the grid costs no extra HTTP. It returns `FetchedProject`
  (`:2459-2465`). The success arm (`:~2662-2700`) does `upsert_project`, then
  `set_page_extras`, then `set_dictionary`.
- `db::collab::upsert_project` (`db/collab.rs:187-238`) deliberately leaves out local and
  dedicated columns; the doc at `:170-186` lists them. Add the dedicated pair, following the
  pattern of `set_page_extras` / `page_extras` (`db/collab.rs:388-410`):
  ```rust
  pub fn set_canonical_grid(conn, project_id, grid_json: Option<&str>) -> Result<bool /*changed*/>
  pub fn canonical_grid(conn, project_id) -> Result<Option<String>>
  ```
  The column already exists (`db/schema.rs:3102-3108`, ALTER-added by 6b-1).
  `upsert_project_preserves_local_columns` (`db/collab.rs:1012`) should be extended to cover it.
- Store the **canonical serialization** of the typed grid, never the hub text. Report a change
  (`RefreshReport.grid_moved: Vec<String>`, beside `gate_moved`, at `api/collab.rs:2564-2575`) only
  when the canonical string changes.

### 1.6 The feed consumer

`api/collab_live/feed.rs`:

- `apply_contiguous` (`:733-756`): `small_docs` = Meta | Members | Thresholds | Dictionary
  triggers `refresh_projects_reporting`. Grid only logs:
  ```rust
  if ev.kinds.contains(&ChangeKind::Grid) {
      tracing::debug!(project_id = %pid, version = ev.version, "grid change seen; no consumer in this wave");
  }
  ```
  The hub's `put_grid` bumps with **only** `Kind::Grid`, so today a grid edit never refreshes
  anything.
- `catch_up_project` (`:907-1000`, its own `small_docs` at `:922-930`) and `reload_one_project`
  (`:1378-1470`, which always refreshes) are the other two refresh sites.
- `ALL_KINDS` (`:31-37`) omits Grid. Gaps catch up with Meta, so the refresh already happens there;
  only the semantics are missing.
- `note_card` (`:253-262`) decides `SyncChanges.card`, which feeds `collab-project-synced`.
- The emission pattern inside the feed is
  `if let Some(em) = self.emitter.as_deref() { crate::events::emit_event(em, NAME, &payload) }`
  (`:871-877`). There are precedent event constants and payload structs:
  `COLLAB_PEERS_CHANGED_EVENT` + `CollabPeersChanged {project_id}` (`api/collab_exchange.rs:
  1236-1245`), ts-exported at `ts_export.rs:244`.
- `ChangeKind` (`collab/live/wire.rs:189-200`, lowercase) already has `Grid`.
- **Recommended consumer:**
  - add `ChangeKind::Grid` to both `small_docs` matches and to `ALL_KINDS` (5 → 6);
  - add Grid to `note_card`;
  - after each of the three refresh calls, emit `collab-grid-changed {projectId}` for
    `report.grid_moved`;
  - delete the debug line.
  Driving the event from `grid_moved` rather than from `ev.kinds` means a manual refresh, a gap
  catch-up and an epoch reload all emit it once, and a no-op echo emits nothing.
- The fake hub needs grid support:
  - a `canonical_grid` field on `FakeProject` (`collab/fake_hub.rs:191-230`);
  - `"canonicalGrid"` in `project_page` (`:2130-2163`; the target is hard-coded
    `{"raDeg":10.68,"decDeg":41.27,"radiusDeg":1.5}` at `:2158`);
  - a `("PUT", ["projects", pid, "grid"])` arm in the dispatch (`:2095-2120`), modelled on
    `put_publishing_device` (`:2692-2714`), with the hub's validation and cap check, a version
    bump, and `st.publish_bump(pid, prev, &["grid"], &[])` (`:497`).

---

## 2. The proposal math (§6.2)

### 2.1 WCS and TAN utilities that exist

| Utility | Where | Notes |
| ---- | ---- | ---- |
| `WcsSolution { crpix (0-based), crval, cd, sip_forward, sip_reverse }`, `pixel_to_sky`, `sky_to_pixel`, `pixel_scale_arcsec`, `field_rotation_deg` | `solvemyastro/src/wcs.rs:65-122` | `sky_to_pixel` uses SIP **reverse** (AP/BP) only when present. The `tan` helpers are `pub(crate)` (`:35`), so they are not callable from core; use `WcsSolution` with `sip: None` as the grid's TAN. `field_rotation_deg = atan2(-cd12, cd22)` normalized to [0,360). |
| `PlateSolveRecord::to_solution()` | `plate_solve/storage.rs:~62-…` | 0-based on both sides; refuses half-stored SIP. Gated render+solver. |
| `wcs_seed::seed_from_solves` | `stacking/register/wcs_seed.rs:96-127` | **The template for the chain**: a 5×5 sample grid → `pixel_to_sky` → `sky_to_pixel` → `fit_affine`. Its test helper `cd(scale, rot)` (`:~134-139`) gives `[[-s cos, s sin],[s sin, s cos]]`, whose `field_rotation_deg` is **−rot**. Do not reuse it for the grid convention. |
| `fits_writer::wcs::{wcs_cards, scale_plate_solve, scaled_crpix}` | `fits_writer/wcs.rs:151-175, 182-…` | CRPIX card = record + 1. Drizzle: `crpix' = s·crpix + (s−1)/2`, `CD' = CD/s`, SIP `A'pq = Apq·s^(1−p−q)`. |
| `geometry::linear::{fit_affine, fit_homography, fit_similarity}` | `geometry/linear.rs:182, 278, 365` | `Linear` is a 3×3 matrix; homography is supported end to end. |
| `geometry::polynomial::{Distortion::fit, fit_joint}` | `geometry/polynomial.rs:302, 351` | Least squares over any number of samples. Terms of degree 2..=order only. **Order 2..=4 hard cap.** |
| `geometry::tps::{ThinPlateSpline::fit, select_nodes}` + `align.rs::fit_tps_on` (private) | `tps.rs:109, 364`; `align.rs:684-713` | Interpolating at the nodes; dense O((n+3)³); `TPS_MAX_NODES = 600`, a 30×20 stratification. |
| `registration` module | `registration/{db,mod}.rs` | Only `registration_results` I/O. Nothing for WCS. |

### 2.2 Projecting a footprint from `meta.wcs` into a TAN centred on the target

**Manifest `meta.wcs`.** Writer: `collab/frame_meta.rs:134-160`. Keys:
- `crval1`, `crval2`;
- `crpix1`, `crpix2` (**1-based**);
- `cd: [cd1_1, cd1_2, cd2_1, cd2_2]`;
- optional `sip: {order, a, b, ap?, bp?}` with square `(order+1)²` arrays.

Other `meta` keys used: `naxis1`, `naxis2`, `pixelScaleArcsec`.

**Rows.** The rows come from `project_frames_local` (`schema.rs:2637-2665`):
- `manifest_json` is the whole `FrameViewWire` (`hub_client.rs:159-198`, round-tripped by
  `upsert_from_manifest_deferred`, `db/collab_frames.rs:311`);
- `meta` is verbatim;
- filter on `state = 'published' AND accepted = 1`;
- the target comes from `collab_projects.target_{ra,dec,radius}_deg`.

**Parser.** Add `wcs_from_meta(&Value) -> Option<PlateSolveRecord>` with `frame_id = 0`,
`crpix − 1`, `sip_*_coeffs` re-encoded as JSON strings. Steps 5 and 8 need this same function, so
whichever step lands first should own it.

**Recipe** (deterministic):
1. `scale` = median of `meta.pixelScaleArcsec` over published ∧ accepted rows.
   - Sort with `total_cmp`.
   - Define the even-count rule (mean of the two middle values).
   - Round with `(x*100).round()/100`.
   - No row with a scale → `proposal: None`, with a reason.
2. Build the grid TAN as a `WcsSolution`:
   - `crval = target`;
   - `crpix = (0,0)` for the extents pass;
   - **CD in the FITS CROTA2 convention**, so that `field_rotation_deg()` returns `rotationDeg`:
     `CD = [[-s·cosρ, -s·sinρ], [-s·sinρ, s·cosρ]]`, with `s = scale/3600` (det < 0, east left,
     north up when rows are bottom-up). See finding 2 for the parity consequence.
3. For each row with `meta.wcs` and `naxis1/2`:
   - take its boundary points: the 4 corners plus about 8 points per edge (SIP and TAN-to-TAN
     curve the edges slightly);
   - map each with `frame_wcs.pixel_to_sky` and then `grid.sky_to_pixel`;
   - track min/max in grid pixels.
4. The grid is centred on the target, so the size is **symmetric**:
   - `hx = min(max(|xmin|,|xmax|), R)` and likewise `hy`;
   - `R = tan(radius) [rad → deg] · 3600 / scale`, the TAN radius of the target circle;
   - `W = ceil64(2·hx)`, `H = ceil64(2·hy)`, clamped to the hub's `[64, 65536]`.
   Flag: frames offset from the target inflate the grid on both sides. An owner might prefer a
   bbox-centred grid, but the spec says centre = target.
5. No frame has a WCS → `W = H = ceil64(2R)`.
6. `framesInside` = accepted frames with at least half their footprint inside the rectangle.
   - Use a deterministic sampled fraction (e.g. 16×16 points over the frame) or a polygon clip.
     `drizzle/geom.rs::clip_area` only clips against a unit pixel.
   - Frames without a WCS cannot be judged; report them separately (`framesWithoutWcs`).
     The spec is silent on this.
   - The same helper serves the §6.3 plan warning ("N frames lie outside the grid") against the
     **current** grid. The `{current, proposal, framesInside}` shape in §9.1 is ambiguous about
     which grid `framesInside` counts. Suggest `framesInside` (proposal) plus
     `currentFramesInside`.

### 2.3 Determinism and the hash

- The inputs are order-independent: median after sorting, min/max for the bbox. Sort rows by
  `frame_uuid` anyway, so logs and tests are stable.
- Across devices, libm `atan2`/`asin` may differ in the last ulp between macOS, Windows and Linux.
  The rounding (0.01″, multiple of 64) absorbs that except at exact boundaries.
- **Hash.** Follow the repo's established canonicalization (`stacking/config.rs:580-596`;
  `serde_json` has **no** `preserve_order` in `Cargo.lock`, so a `Value` map serializes with
  sorted keys):
  ```rust
  let v = serde_json::to_value(&grid /* typed CanonicalGrid */)?;
  let json = serde_json::to_string(&v)?;               // {"crval1":..,"crval2":..,"heightPx":..,"rotationDeg":..,"scaleArcsec":..,"widthPx":..}
  format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))   // ATH_GRID, 16 hex
  ```
  - Use the typed struct (`f64` + `i64`) so that `6000` vs `6000.0` cannot differ.
  - `serde_json` prints `f64` in shortest round-trip form, and the hub round-trips its values
    through the same `f64` serde.
  - **Normalize before the PUT and before hashing:** `-0.0` → `0.0` (Postgres `numeric` loses the
    sign); `rotationDeg` into one canonical range (the hub only checks that it is finite);
    `crval1` into `[0, 360)`.
- Put the pure type, validation (mirroring the hub's bounds exactly), normalize, canonical JSON and
  hash in an **ungated** module (e.g. `collab::grid`), because `db`/feed code needs them. Put the
  proposal math in a gated module (render+solver, like `api::collab`), because it uses
  `PlateSolveRecord::to_solution()` / `WcsSolution`. That keeps one TAN implementation, shared with
  the WCS seed and the composition.

---

## 3. Registration today

### 3.1 `PixelMap`

- Defined in `geometry/pixel_map.rs:697-703`:
  ```rust
  pub struct PixelMap { pub linear: Linear, pub linear_inv: Linear, pub distortion: Option<DistortionModel> }
  ```
- `DistortionModel` (`:397-420`) is either `Polynomial(Distortion)` or
  `Tps { forward, inverse: ThinPlateSpline, domain: [f64;4], grids: Arc<TpsGrids> }`.
  - The wire tag is `kind`; an untagged value means polynomial (`:452-480`).
  - `from_json` validates `is_well_formed` (`:836-853`).
- Forward (`:776-786`): `p = linear(s)`, then `p + d_fwd(p)`. The displacement is evaluated in
  **reference** space.
- Inverse (`:791-801`): `linear_inv(r + d_inv(r))`.
- `forward_exact` / `inverse_exact` (`:808-829`) use the exact spline or polynomial.
- `forward_eval()` / `inverse_eval()` (`:737-751`) capture the TPS displacement grid once per
  burst.
- For TPS, the pixel path reads a **bilinear `TpsGrid` sampled every `TPS_GRID_PX = 8` px, stored
  as f32** (`:71-145`, `tps.rs:34`), clamped to the domain.
- The polynomial arm has no grid: it evaluates exactly per pixel and clamps to its normalized
  domain.

### 3.2 How the star map is fitted and cached today

- `register_group_pass` (`stacking/run.rs:4290-5064`).
- Per frame, the hash is:
  ```rust
  let expected_hash = registration_hash_for(&cfg, reference_frame_id, reference_hash, &frame_hash, &frame_measurement_hash);
  ```
  `registration_hash_for` (`plan.rs:494-512`) =
  `stage_hash(registration_subtree, ["ref:{id}", ref calibrated hash, frame calibrated hash, "measure:{h}"], [])`.
  **There is no grid in it**, which matches §5.5's "star map cached under today's registration
  hash".
- Cache: `by_frame` = `stored_registrations(conn, subject)` (`plan.rs:893-903`). The **Project arm
  returns `Vec::new()`**, and `store_registration` (`:905-916`) is a no-op for projects (6b-1, B10).
  6b-2 must fill both; this is open item (2) of the 6b-1 README.
- Freshness: `registration_row_is_fresh(row, reference, expected_hash)` (`plan.rs:1177-1188`).
- The registered artifact: `record_registered_artifact` (`run.rs:5849-5875`) upserts
  `kind: "registered"`, `config_hash: rec.config_hash` (= the registration hash), and
  **`payload_json: None`**. Its freshness check is `registered_artifact_is_fresh(rc, group, frame,
  hash)` (`:5881-5897`), which calls `plan::is_fresh` (`plan.rs:715-722`: hash equal and the file
  size matches).
- The cache-hit branch (`run.rs:4358-4440`) queues a stale or missing artifact into
  `needs_artifact_rewrite`, which carries `map` + `rec` and runs a pixel-only fan-out
  (`:4580-4648`). **This is where a grid change slots in**: recompose from the cached star map,
  push a rewrite item, and do no star matching.
- The main fan-out (`:4716-4860`) aligns, then calls `to_record(wire_set_id(&subject), …)`, then
  `write_one_registered_frame(&item.path, &alignment.map, ref_w, ref_h, …)` inside the worker.
  The composition belongs there: the inputs are pure data (star map, reference WCS, grid WCS),
  which is `Send`.
- The reference itself goes through `identity_registration(ref_stars)`
  (`register/frame.rs:398-…`). In grid mode it also needs a composed map (identity ∘ WCS → grid),
  so the reference is resampled once, as §15.3 says.

### 3.3 The registered writer

- `register/writer.rs:174-219`: `write_registered_frame(subject, map, ref_w, ref_h, …)`.
  - The output buffer is `vec![f32::NAN; ref_w*ref_h]` per plane.
  - `warp_rows` → `sample_at`, which is NaN outside the source (`resample/warp.rs:295-337,
    405-433`; pinned by `pixels_mapping_outside_the_source_are_nan`, `:603`).
  - So **NaN outside already holds**. Today's size is the group geometry
    (`rc.geometry_of(&group.key)`, `run.rs:4512-4528`).
  - The warp visits **every** output pixel, so its cost scales with grid area, not frame area.
    A footprint-bbox row/column window is an optional optimization.
- Copy-through: `REGISTERED_COPY_THROUGH` (`writer.rs:38-42`) includes `FILTER` and `ROWORDER`
  and does not include `CALSTAT`/`ATH_CSRC`/`ATH_PRJ`.
  `RegisteredCards::reference_roworder: Option<&str>` (`:44-55, 84-91`) **already replaces**
  `ROWORDER`, so grid frames can pass `Some("BOTTOM-UP")`.
- `ATH_REGT` = `rec.transform_json` (the star map). For a grid frame it should be the composed map
  that produced the pixels, plus a new `ATH_GRID` card.

### 3.4 The on-the-fly fallback and LN

- `integration/registered_source.rs:23-57`: `RegisteredFrame { path, map }` and
  `MaterializedFrame { registered_path, frame_id, fallback }`. The fallback warps through
  `map.inverse` into `(width, height)`.
- `ln/mod.rs:654-668` builds the same `MaterializedFrame` with `reference.width/height`.
- Both take one `PixelMap` plus output dims. If `RegisteredFrameOutcome::Aligned.map` holds the
  **composed** map and the group geometry holds the **grid** dims, neither needs a code change.

### 3.5 Geometry plumbing that has to change

- `GroupGeometry { reference_frame_id, width, height, calibrated, hash }` (`run.rs:396-402`) holds
  both the warp target and the reference frame's own dims. These are set from `ref_stars` by
  `adopt_co_registered_geometry` (`:5348-5360`) and `set_group_geometry` (`:5555-5574`).
- In grid mode, `width/height` must become the grid's, while `rc.reference_width/height` and
  `ref_stars` stay the frame's own.
  - The two-pass diagonal already uses `ref_stars.width/height`, so the dry pass and scale
    statistics stay in reference space as §5.5 says.
  - `registration_gate_and_hint` (`:4070-4097`) uses the frame/reference pixel scales; unchanged.
- Consumers of `geometry_of(..).width/height` (`run.rs:4516, 5448, 6937, 7424, 8517`) then
  automatically target the grid: Register writer, the output anchor pick, integration rows, LN.

### 3.6 What "fit the composed chain as one PixelMap" needs, and whether it can meet the pin

**The exact chain** (forward only, never `sky_to_pixel` through the reference's SIP):
```
s (subject px) → star_map.forward_exact(s) → ref_wcs.pixel_to_sky (SIP A/B + CD + TAN)
               → grid_wcs.sky_to_pixel (pure TAN, no SIP) → g
```
- Sample `s` every 32 px **including the last row and column**.
- Fit both directions from the same `(s, g)` pairs, as `fit_tps_on` / `Distortion::fit` already
  do:
  - forward nodes sit at `L(s)` and carry `g − L(s)`;
  - inverse nodes sit at `g` and carry `L(s) − g`.
- This sidesteps SIP reverse (AP/BP), which may be absent or approximate. It also sidesteps the
  star map's separately fitted inverse.
- Define the "inverse pin" as round-trip consistency:
  - `inverse(forward(s)) ≈ s` at cell centres;
  - `forward(inverse(g)) ≈ g` at grid cell centres inside the footprint.

**Existing helpers:**
- `fit_affine`, `fit_homography`;
- `Distortion::fit` / `fit_joint` (polynomial, least squares on all samples);
- `ThinPlateSpline::fit` + `select_nodes` + the private `fit_tps_on` / `tps_domain` in `align.rs`;
- `wcs_seed::seed_from_solves` as the sampling template;
- `PixelMap::with_distortion_model`.

**Real-data numbers** (dev catalog, read-only):
- 465/465 plate solves have `sip_order = 3`, with forward and reverse tables. Cubic terms are about
  3.6e-10, so roughly 11 px at u = 3124.
- The ASI2600 rigs at 414 mm give 1.87″/px.

**Experiment.** The exact chain for frame 28747's real solve (ATR2600M, 6224×4168, `crpix
(3111.5, 2083.5)`, SIP A/B above, identity star map). Grid: 1.87″/px, rotation 0, centre offset
0° / 1.5° / 4°. Error = max distance to the exact chain at the 32-px cell centres (16 + 32k).

| Composed model | 0° offset | 4° offset |
| ---- | ---- | ---- |
| affine-only residual (what the layer must absorb) | 10.1 px | 17.0 px |
| affine + TPS, 600 lattice nodes (λ = 0) | **0.079 px** fwd (p99 0.017) | 0.094 px |
| affine + TPS, 1080 nodes | 0.047 px | 0.055 px |
| affine + TPS, 1700 nodes | 0.034 px | 0.036 px |
| `fit_joint` affine + poly degree 2..4 | fwd 3e-5 / **inv 0.092** | fwd 0.0055 / inv 0.095 |
| `fit_joint` affine + poly degree 2..5 | fwd 2e-5 / inv 0.00056 | **fwd 0.0035 / inv 0.0034** |
| `fit_joint` affine + poly degree 2..6 | fwd 2e-5 / inv 0.00054 | fwd 0.0035 / inv 0.0030 |

**What follows from it:**
- The TPS route fails the pin by about 8× at the R-M4c-5 cap. Raising the node count does not
  rescue it, and the solve is dense O(n³).
- Polynomial order ≥ 5 passes in both directions. The leftover 0.003 px at 4° is probably the
  inverse's missing linear terms; a homography linear part would likely shrink it.
- **Required code change for order 5:**
  - `Polynomial2D::is_well_formed` and `fit` accept order 2..=4 only (`polynomial.rs:39-44,
    76-79`);
  - `eval` sizes `pu/pv` as `[f64; 5]` (`:56-57`);
  - the `Distortion` docs say "≤ 12 unknowns".
  Raise the cap for composed maps only, keep `DistortionChoice` at ≤ 4 for star registration, and
  pin that orders ≤ 4 evaluate bit-identically.
- **The open case: the star map itself carries a TPS layer** (`registration.distortion = tps`,
  opt-in; the default is `Off`, `register/mod.rs:249-270`). A polynomial cannot follow the star
  TPS, and a TPS re-fit fails on the SIP part. Options for an owner ruling:
  - (a) a `DistortionModel::Sampled` variant: a bilinear displacement grid at 8 px, sampled
    straight from the exact chain. It needs no fitting, the sampling error is about 5e-5 px, and it
    is still one `PixelMap` with consumers unchanged. It is **not serialized**: it is re-derived
    deterministically from star map + reference WCS + grid (about 400k chain evaluations, tens of
    ms);
  - (b) ignore or hide `distortion = tps` for projects;
  - (c) accept a looser pin when the star map is TPS.
- **Representation note:** the TPS displacement grid is f32 (`pixel_map.rs:84`). That is fine for
  amplitudes of about 20 px (error around 1e-6 px).

### 3.7 The two-pass dry pass and the scale statistics

- `two_pass_refine` (`run.rs:5097-…`) runs `register_group_pass(persist = false)` on the
  reference's group. It collects `(rotation, translation)` and calls `two_pass_pick`
  (`weights.rs:591-…`) with `diagonal_px = hypot(ref_stars.width, ref_stars.height)` (`run.rs:
  ~5152`). Everything stays in reference space and is unaffected by the grid.
- The scale gate is per frame (`scale_gate_for`, `ratio_wants_seed`, `wcs_seed.rs:61-71`);
  unchanged.
- For a project, the WCS seed's `solve_of` → `stored_solve` (`stacking/origin.rs:41-50`) needs the
  Project arm (manifest `meta.wcs` with crpix −1, or the step-5 `solve` artifact).

---

## 4. Drizzle

- **The overlap plan.** At `drizzle/mod.rs:896-925` the call site asks `SquareOverlapPlan::resolve`
  (`:421-471`), which decides:
  ```rust
  if kernel != DrizzleKernel::Square || force_exact || !phase_table_applies(scale, drop_shrink) { /* Exact */ }
  if map.distortion.is_some() { return SquareOverlapPlan { table: None, tiled: true }; }   // Tiled
  let table = geom::map_drop_at(map, (src_w-1)/2, (src_h-1)/2, drop_shrink, scale)          // Frame
      .and_then(|q| PhaseTable::build(&q, scale, phase_table::PHASES));
  ```
  - `SquareOverlap` doc (`:363-382`): `Frame` is "one table for the whole frame: **with no
    distortion layer** the mapped drop is one parallelogram". `Tiled` is "rebuilt per
    `phase_table::TILE` (256 px) source tile from the map's local Jacobian".
  - A composed map **with** a distortion layer therefore takes `Tiled`. A pure homography takes
    `Frame`, with the table built at the frame centre.
  - Only if an owner ruling allowed a linear-only composed map (no SIP; not the case for the
    owner's data) would the Frame arm apply. In that case, large projective terms (a wide field far
    from the grid tangent point) vary the drop shape across the frame by about
    `(L/f)·tanθ·2`, for example 2.6 % at 5″/px with a 10° offset. That is above the C-21 per-pixel
    bound.
- **Forward map and scaling.** Deposits use `frame.map.forward_eval()` (`:899`), and
  `geom::to_output(u, s) = s·u + (s−1)/2` (`geom.rs:38-51`). `map_drop_at` (`:194`) scales the
  local Jacobian by `s`, and `drop_bound_half_diag` uses `forward_exact`. Nothing is
  scale-specific to the grid.
- **Grid × scale.** `DrizzleInput.width/height` (`mod.rs:110-121`) is "reference geometry … the
  output grid is this, scaled by `scale`". `run.rs:8086-8104` fills it from the group geometry, so
  with grid dims and composed maps the output is grid × s automatically.
  - `.rej` bitmaps and LN grids must be in the grid geometry; the guard is at `mod.rs:~872-881`.
  - The Bayer drizzle's `calibrated_mosaic` has the same pixel geometry as the frame, so the
    composed map applies unchanged.
- **WCS.** `run.rs:8180`: `wcs.map(|w| scale_plate_solve(w, scale))`, then `build_drizzle_cards`
  (`master_cards.rs:310-367`), which strips every `is_wcs_keyword` card and `ATH_RGEO` and
  re-adds them from its argument. A synthetic grid record scales exactly.
- **Weight map.** `write_weight_map` (`mod.rs:131-135`) is `W / max(W)` and goes to
  `master_lights` as kind `weight_map` (`run.rs:8251-8280`). The coverage plane for §5.8 is a
  step-10 addition; the raw `W` accumulator is already used for `coverage` inside drizzle.

---

## 5. Output

- **Today's master WCS.** `stage_output` (`run.rs:9677-9790`) resolves, per distinct reference,
  `stored_solve(&conn, origin_of(..)?, reference_frame_id)?` (`:9748-9757`). It passes that to
  `process_group_output(rc, group, &measure_opts, wcs.as_ref())`, and when the master is written
  with no WCS it warns once ("no plate solve on the reference frame; the master has no WCS").
  - The master cards are built at `run.rs:7679-7695` with `MasterCardInputs { reference_cards, wcs,
    …, geometry: rc.config.registration.geometry }`.
  - Drizzle uses `scale_plate_solve` (`:8180`).
  - **A project needs an arm here** that skips `stored_solve` and passes the synthetic grid record
    (crop-shifted).
- **`PlateSolveRecord` fields** (`plate_solve/storage.rs:11-41`) for the synthetic TAN:

  | Field | Value |
  | ---- | ---- |
  | `id` | `None` |
  | `frame_id` | 0 |
  | `crpix1`, `crpix2` | **0-based** `(W−1)/2`, `(H−1)/2`, minus the crop origin |
  | `crval1`, `crval2` | the grid's |
  | `cd1_1`, `cd1_2`, `cd2_1`, `cd2_2` | from (scale, rotation) in the CROTA2 convention (§2.2) |
  | `sip_*` | `None` |
  | `matched_stars`, `total_detected` | 0 |
  | `rms_residual_px`, `rms_residual_arcsec` | 0.0 |
  | `pixel_scale_arcsec` | `scaleArcsec` |
  | `field_rotation_deg` | `rotationDeg` (normalized as `WcsSolution::field_rotation_deg` would report it) |
  | `solve_time_ms` | 0 |
  | `catalog_used`, `algorithm_used` | e.g. `"grid"` |
  | `solved_at` | the run time |
  | `expected_catalog_stars_in_fov`, `inlier_ratio` | `None` |

  `wcs_cards` will also write `PLTSOLVD = T` "plate solved by Athenaeum" (`fits_writer/wcs.rs:
  ~276-279`), which is arguable for a synthetic WCS.
- **The FILTER copy.** `build_master_light_cards` (`master_cards.rs:127-…`) copies the
  reference's cards at `:134-141`:
  ```rust
  inputs.reference_cards.iter()
      .filter(|c| REGISTERED_COPY_THROUGH.contains(&c.keyword.as_str()))
      .filter(|c| c.keyword != "EXPTIME")
      .filter(|c| inputs.date_obs_first.is_none() || c.keyword != "DATE-OBS")
  ```
  `reference_cards` comes from `source_cards_from_file(&group_reference_calibrated)`
  (`run.rs:7630`). For a project that is the group anchor's prepared or landed file, which carries
  the **publisher's raw FILTER** and `ROWORDER = TOP-DOWN`. Add `filter_override: Option<&str>`
  (the canonical filter) and a ROWORDER override (or drop the card) to `MasterCardInputs`.
- **Where the new cards go:**
  - `ATH_RGEO` is built from `RegistrationGeometry` (`master_cards.rs:214-221`, the enum is
    `CoRegistered | Native` at `register/mod.rs:163-167`). `'grid'` is not a variant, so add a
    label or override in `MasterCardInputs` and in `build_drizzle_cards(…, geometry)`, which
    re-stamps it.
  - Add `ATH_SPRJ` (project id; a UUID fits in one 80-byte card) and `ATH_GRID` (16 hex) to the
    master cards. `build_drizzle_cards` keeps non-WCS cards, so they flow into the drizzle output
    and the weight map.
  - The scanner skips anything carrying the exact keys `ATH_REG`/`ATH_STK`
    (`scanner/mod.rs:492`) and diverts on the exact key `ATH_PRJ` via `calibrated_light_identity`
    (`:473-480`). `ATH_SPRJ` cannot collide. The copy-through allowlist already excludes
    `ATH_PRJ`.
- **Naming and folder.**
  - `master_file_name(&rc.set_name, group.filter, color_mode, binning, exposure_s, n)`
    (`master_cards.rs:377-…`; `run.rs:7714`) produces
    `<slug>_<filter>_<mono|osc>[_bin<n>]_<exp>s_<n>x.fits`. For a project: `set_name` = title,
    filter = canonical, binning token suppressed (W7, `binning = 1`).
  - `write_master_light(&rc.output_dir, …)` (`master_cards.rs:680-690`) does `create_dir_all` and
    `resolve_collision`. Set `rc.output_dir = <output_dir>/sanitize_for_filename(title)`
    (`archive/path_layout.rs:8-23`) at start.
  - Personal masters go to the output root today, not a per-set folder.
- **Crop: no machinery exists** (no "crop" anywhere in `stacking`, `integration` or
  `fits_writer`). Step 10 builds the coverage, the rectangle and the plane crop. Step 11 then
  applies:
  - the `crpix − origin` shift on the synthetic record;
  - for drizzle, the rectangle × s. Check: `scaled_crpix(c − x0, s) = (s·c + (s−1)/2) − s·x0`,
    consistent with cropping the drizzle output at `s·x0`.
  - In coRegistered mode the rectangle is run-wide, so it must be resolved after every group's
    coverage and before any master is written. Today `stage_output` writes per group inside the
    loop, so this is a restructuring point.
- **`master_lights` kinds.** `MasterLightKind::{Master, Drizzle, WeightMap}` ↔
  `"master" | "drizzle" | "weight_map"` (`api/stacking.rs:897-923`). The column `kind TEXT NOT
  NULL` has **no CHECK constraint** (`schema.rs:386-403`, `UNIQUE(run_id, group_key, kind)`),
  so `coverage` needs only the enum arm. `from_db_str` returns `None` for unknown kinds, which
  makes older builds tolerant.

---

## 6. Registration modes and the per-group reference (§5.4)

- **Stage 4** (`run.rs:3703-3870`, `stage_reference`):
  - **Manual:** `stored_reference_frame(&conn, &rc.subject)`; the project arm is
    `stacking_project_config.reference_frame_uuid`.
  - **Auto:** the largest group by included count, ties broken by total exposure, then
    `best_included_idx` → `best_by_weight(weights, included, star_counts)` (`weights.rs:333-346`).
  - Then `resolve_group_geometry` (`run.rs:3583-3656`):
    - coRegistered: every group takes the run reference;
    - native: `group_reference_idx` (`:3673-3690`) = the Manual pin within its own group, else
      `best_included_idx` per group.
- **Two-pass** (stage 5): `two_pass_pick` over `TwoPassCandidate`s, the top
  `TWO_PASS_CANDIDATES = 10` by weight.
- **Where "candidates must have a WCS" plugs in:**
  1. The `included` mask handed to `best_by_weight`, applied in two tiers: (a) included ∧ manifest
     WCS; (b) if none, included ∧ local solve; (c) if none, fail the stage with the spec's
     message.
  2. Auto's "largest group" choice. If the largest group has no WCS frame, the spec is silent.
     Suggest taking the next-largest group that has one.
  3. The two-pass switch candidates. Keep **all** aligned frames for the median, but mark
     non-WCS frames ineligible to win. That needs an `eligible` flag on `TwoPassCandidate`; do not
     remove them from `usable`.
  4. Native groups with no WCS frame at all. The spec is silent; options are to fall back to the
     run reference for that group (co-registered), or to skip the group with a reason.
  5. Manual: a pin without a WCS becomes a plan blocker.
  The WCS status is known by stage 4, because step 5's Solve runs at the start of Measure.

---

## 7. Recommended task decomposition (steps 6, 8, 11)

The dependencies come first, then each step in order. Each item is meant to be one commit.

### Shared prerequisite (owned by whichever of steps 3/5/6 lands first)

- **P1.** `wcs_from_meta(&Value) -> Option<PlateSolveRecord>` (crpix −1, SIP JSON) and the
  `stored_solve` Project arm (manifest WCS first, else the step-5 `solve` artifact).
  - Test: round-trip `build_frame_meta` → `wcs_from_meta` gives the original record's
    `to_solution()` to 1e-12.

### Step 6: the grid

- **6.1** `collab::grid` (ungated, pure).
  - Contents: `CanonicalGrid` (serde camelCase, the hub's field names), `validate()` (the hub's
    exact bounds), `normalized()` (−0.0, rotation range, crval1 range), `canonical_json()`,
    `hash()`.
  - Tests:
    - a golden hash literal;
    - a jsonb-reordered input parses to the same hash;
    - −0.0 and 0.0 hash the same;
    - the boundary values the hub accepts or refuses (64/65536, crval1 = 360 refused).
- **6.2** DB setter and getter (`set_canonical_grid` returns "changed", `canonical_grid`); the
  `upsert_project` doc; extend `upsert_project_preserves_local_columns`.
- **6.3** Wire and refresh.
  - Contents:
    - `ProjectWire.canonical_grid`;
    - `FetchedProject` carries the canonical grid string (malformed JSON → `warn!` and keep the
      old value; never swallow);
    - the success arm writes it and fills `RefreshReport.grid_moved`;
    - `ProjectCard.canonical_grid` plus the ts regen;
    - fake hub: the grid on the project page plus `PUT …/grid`.
  - Tests: a refresh stores the grid; an echo with re-ordered keys does not move it.
- **6.4** The feed consumer.
  - Contents: Grid in both `small_docs` and in `ALL_KINDS`; `note_card`;
    `COLLAB_GRID_CHANGED_EVENT` + the `CollabGridChanged` payload (ts-exported); emit from all
    three refresh sites; delete the "no consumer" line.
  - Tests (fake hub): a PUT grid → event → column updated → exactly one emit; a Members-only event
    emits no grid event; a gap catch-up picks up the grid.
  - Existing feed tests to re-run: `catch_up_project_prunes_when_its_own_small_document_refresh_reveals_a_caps_change`
    (`feed.rs:2765`) and the `effects.contains(&FeedEffect::MembersChanged)` pins (`:2128, 3078`).
- **6.5** `CollabClient::put_grid` (200/204 → Ok; 400 → `Http`, mapped to `Invalid` at the API).
- **6.6** The proposal (gated).
  - Contents: `grid_wcs(&CanonicalGrid) -> WcsSolution`, `propose(..)`, `frames_inside(..)`.
  - Tests:
    - permutation invariance;
    - the even-count median;
    - 0.01″ rounding;
    - multiple of 64 and the clamps;
    - the circle clip;
    - the no-WCS square;
    - the half-inside rule;
    - the `field_rotation_deg()` round trip;
    - `grid_wcs.pixel_to_sky(crpix) == crval`.
- **6.7** Commands `get_collab_grid_proposal` / `set_collab_project_grid`.
  - Wiring: core `api`, Tauri `commands/collab.rs`, web `routes/collab.rs` + `mod.rs`, then
    `invoke_handler`, the TS api.
  - `set` validates and normalizes, PUTs, writes the local copy, and returns the normalized grid.
  - Command count +2.

### Step 8: registration onto the grid (depends on 6.1, 6.6's `grid_wcs`, and P1)

- **8.0** WCS-candidate reference selection (§6 above). It can live in step 3, 5 or 8.
  - Test: Auto, Manual, two-pass and native never pick a non-WCS frame, and the two tiers are
    ordered.
- **8.1 (gate the design first)** `geometry::compose` (pure): `compose_to_grid(star_map,
  subject_dims, ref_wcs, grid_wcs) -> Result<ComposedMap>`.
  - Algorithm:
    - sample every 32 px including the edges;
    - `fit_joint` affine plus a polynomial of order 5 (after raising the cap), or the
      owner-ruled alternative from §3.6;
    - self-check at every cell centre, both directions;
    - `Err` above 0.01 px → exclude the frame with a reason (never write a silently wrong frame).
  - **Hard pins** (fixtures: the real catalog SIP from §3.6; offsets 0° and 4°; star maps
    identity / homography / polynomial-3; a parity flip):
    - forward vs exact chain < 0.01 px at all cell centres;
    - `inverse(forward(s))` < 0.01 px;
    - `forward(inverse(g))` < 0.01 px inside the footprint;
    - pixel-path `forward_eval` == `forward_exact` for the poly arm.
  - Polynomial cap raise:
    - pin orders ≤ 4 bit-identical, with golden eval values;
    - `DistortionChoice` still stops at 4;
    - `from_json` accepts stored order-5 maps.
- **8.2** Run wiring.
  - Context and geometry:
    - `RunContext.grid: Option<GridCtx { grid, hash, wcs, record }>`;
    - `GroupGeometry` output dims = grid; `rc.reference_*` = the frame's own;
    - the composition runs inside both fan-outs (the identity row included).
  - Hash and cards:
    - registered artifact `config_hash` = `stage_hash({grid, composeVersion}, [registration hash,
      ref-WCS hash], [])`. Include a hash of the reference's WCS record, because a re-solve could
      change it independently of content;
    - `ATH_REGT` = the composed map, `ATH_GRID`, `ROWORDER` = BOTTOM-UP.
  - Caches:
    - the star map in a payload-only `registration` artifact kind (cleanup-proof; see finding 5),
      or in the registered payload per the spec;
    - the `stored_registrations` / `store_registration` Project arms;
    - the cache-hit path recomposes on a grid change (no star detection);
    - LN gets the composed hash as its registration upstream (`normalization_hash_for`,
      `plan.rs:601-631`);
    - plan `compute_register_stale` (`plan.rs:1290`) includes the grid hash.
  - Tests:
    - a synthetic project: registered dims = grid, NaN outside;
    - a grid change → no detection (a counter or `dry_pass_stars` untouched), files rewritten, LN
      stale;
    - a frame-set run byte-identical (every existing stacking test unchanged).

### Step 11: drizzle and output on the grid (depends on 8.2 and step 10's coverage and crop)

- **11.1** `grid_plate_solve(&CanonicalGrid, crop_origin) -> PlateSolveRecord`.
  - Pins:
    - `wcs_cards` CRPIX card = (W+1)/2 − x0;
    - `to_solution().pixel_to_sky(crpix) == crval`;
    - `field_rotation_deg()` == rotation;
    - `scale_plate_solve(·, 2)` is consistent with cropping at 2·x0.
- **11.2** Master and drizzle cards.
  - `filter_override` (canonical), a ROWORDER override, the `ATH_RGEO = 'grid'` label path through
    `build_drizzle_cards`, `ATH_SPRJ`, `ATH_GRID`.
  - Tests:
    - exactly one `ATH_RGEO`;
    - **no file the run writes carries `ATH_PRJ`** (parse the master, drizzle, weight map,
      registered and prepared outputs);
    - the scanner classifies them as stacking artifacts.
- **11.3** The `stage_output` / `process_group_output` project arm.
  - Synthetic WCS (no `stored_solve`, no "missing WCS" warning), the output folder
    `<output>/<sanitized title>/`, the title in the name, the binning token suppressed.
  - Drizzle input dims = grid; crop applied as rect × s; `master_lights` rows on the project
    subject.
  - Test: a synthetic project with injected stars at known RA/Dec. Master and drizzle 2× pixel →
    sky through the written WCS matches within 0.5 px (an analog of the §15.3 WCS check). Crop
    shifts CRPIX.

### Existing tests and harness pieces that constrain these steps

- Config and hash pins: `stage_hashes_are_pinned_to_golden_literals`; only
  `config_hash_is_canonical_and_pinned` may move (6b-1 README "The config pin").
- `geometry::pixel_map` pins: `the_tps_grid_agrees_with_the_exact_spline` (`:1122`),
  `only_the_pixel_path_builds_a_grid_and_only_its_own_direction`.
- Polynomial fit tests.
- `register/writer.rs` Pin 1 (writer vs `RegisteredSource` bit-identity across similarity,
  homography and TPS). Add an order-5 polynomial case.
- Drizzle C2 pins: `the_phase_table_tracks_the_exact_clip_on_a_rotated_frame`, its TPS sibling,
  `scale_one_with_a_full_drop_keeps_the_exact_clip`. Add a polynomial-arm sibling, because the
  composed map goes Tiled.
- `fits_writer/wcs.rs::scale_plate_solve_scales_crpix_cd_pixel_scale_rms_and_sip` (`:474`).
- `master_cards` ATH_RGEO and `is_wcs_keyword` tests.
- The M4b pin "the reference's own master is bit-identical between modes" (frame sets only;
  §15.3 says grid mode is exempt).
- Feed tests; `db/collab.rs:1012`; the fake hub's `kinds` assertions (`fake_hub.rs:3779+`).
- Acceptance harness (`docs/superpowers/research/scripts/acceptance/`):
  - `prepare-catalog.sh`, `server.sh` and `api.sh` work as is, but runs A and B need step 12's
    `start_project_stacking` command.
  - `tier1/checkpoint.sh` / `tier1-run.sh` are frame-set-only and need a project variant.
  - `tier1-extract.py` / `tier1-compare.py` hold the Tier C §8 rows; those rows do not change
    under a flip.
  - `imgcmp.py` (pixel diff) has **no flip**; the run-A grid master mirrors the personal master
    for TOP-DOWN data.
  - The "independent plate solve of a grid master within 0.5 px" check has no tool today. Masters
    are never cataloged, so it needs a small example probe calling
    `plate_solve::solve_frame_with_hints` on a file.
