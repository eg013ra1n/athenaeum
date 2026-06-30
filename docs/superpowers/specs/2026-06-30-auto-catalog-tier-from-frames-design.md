# Auto Catalog-Tier Recommendation from Frame Metadata — Design

**Status:** approved (brainstormed 2026-06-30), ready for an implementation plan.

**Context:** Plan 3 shipped the catalog-delivery UI with a *manual* FOV helper
(the user types focal length + sensor + binning). The frames already in the
catalog carry the optics needed to compute their field of view, so the app can
**recommend the right tier automatically** — and let the user just pick which
tiers to download — without a manual calculator.

**Goal:** From the LIGHT frames in the catalog, compute the narrowest field of
view and recommend the density tier set that covers it; let the user download
the recommended set or pick any tier directly. Remove the manual focal/sensor
calculator.

**Verified on real data:** all 18 LIGHT frames in the dev DB have
`focallen`+`xpixsz`+`naxis1`; the narrowest field is 0.565° (`SG_32_Det`) →
`recommendTier(0.565)` = tier 2000 (base+Δ1).

## Decisions (from brainstorm)

1. **Auto recommendation is primary; the manual focal/sensor/binning calculator
   is removed.** The user gets an auto recommendation and a per-tier table to
   download directly — no typing.
2. **Recommend by the globally narrowest LIGHT field.** Tiers are additive and a
   deeper tier covers wider fields, so the single narrowest field determines the
   deepest tier needed; downloading "up to" that density covers every field.
3. **Direct per-tier download** is the manual path: each not-installed tier offers
   "download up to here" (all tiers `density ≤ this`).
4. **Reuse the canonical FOV formula** from `plate_solve::hints` (uses FITS
   `XPIXSZ` directly — binning is already baked into the saved-image pixel pitch,
   so it is NOT multiplied again). The removed manual helper multiplied by binning;
   the auto path is therefore *more* correct for binned frames.
5. **No per-rig breakdown in v1** (YAGNI) — only the global recommendation +
   narrowest-instrument label.

## Components & interfaces

### Backend — `athenaeum-core`

- **Extract `pub fn frame_fov_deg(frame: &Frame) -> Option<f64>`** into
  `plate_solve::hints` (where the computation already lives), moving the existing
  inline computation out of `extract_hints` and calling it from there (DRY):
  ```rust
  // FOV = 2·atan(sensor_mm / (2·focallen)); sensor_mm = naxis1 · (xpixsz/1000).
  // XPIXSZ is the effective saved-pixel pitch (binning already included).
  pub fn frame_fov_deg(frame: &Frame) -> Option<f64> {
      let (focallen, xpixsz, naxis1) = (frame.focallen?, frame.xpixsz?, frame.naxis1?);
      if focallen <= 0.0 || xpixsz <= 0.0 || naxis1 <= 0 { return None; }
      let pixel_size_mm = xpixsz / 1000.0;
      let sensor_mm = naxis1 as f64 * pixel_size_mm;
      Some(2.0 * (sensor_mm / (2.0 * focallen)).atan().to_degrees())
  }
  ```
- **New `pub fn frame_fov_summary(conn: &Connection) -> FovSummary`** — loads LIGHT
  frames, computes `frame_fov_deg` per frame, aggregates:
  ```rust
  pub struct FovSummary {
      pub light_count: u32,          // total LIGHT frames
      pub computable_count: u32,     // LIGHT frames with usable optics
      pub min_fov_deg: Option<f64>,  // narrowest field; None if computable_count == 0
      pub narrowest_instrume: Option<String>,
  }
  ```
  (`#[derive(Clone, Serialize)]`, no `rename_all` → snake_case wire, matching
  `CatalogStatusInfo`.)

### Backend — commands (two-backend cardinal rule)

- **`get_frame_fov_summary`** — Tauri command (`commands/plate_solve.rs`,
  registered in `lib.rs`) **and** the mirrored Axum route
  (`routes/plate_solve.rs`, registered in `routes/mod.rs`); both thin-wrap
  `frame_fov_summary`. Returns `FovSummary`.

### Frontend — `PlateSolveSettingsPanel` "Star catalog" section

- **Remove** the manual focal/sensor/binning inputs.
- **Auto-recommendation banner**: on mount call `api.invoke('get_frame_fov_summary')`.
  - `computable_count > 0`: "From your *N* light frames (narrowest field *X*°,
    *instrume*) → recommended: *Y* stars/deg²" + a **Download recommended set**
    button (`download_catalog_layers({ targetDensity: recommendTier(min_fov_deg, TIER_POLICY) })`).
  - `computable_count == 0`: neutral note ("No frames with usable optics yet —
    pick a tier below.") and just the table.
- **Per-tier table** (from `TIER_POLICY` merged with `get_catalog_status` install
  state, as today): each not-installed tier gets a small **Download** action that
  downloads up to that density (`download_catalog_layers({ targetDensity: tier.density })`);
  the recommended tier is highlighted.
- Keep the existing download-progress UI + `catalog-download-progress` listener.
- New TS type `FovSummary { light_count, computable_count, min_fov_deg, narrowest_instrume }`
  in `src/types/plate-solve.ts`.

### Cleanup

- `src/components/plate-solve/cameraPresets.ts`: remove `CAMERA_PRESETS`,
  `pixelScaleArcsec`, `fovDeg` (now unused). **Keep** `TIER_POLICY` + `recommendTier`.

## Data flow

panel mount → `get_frame_fov_summary` (backend: query LIGHT frames → `frame_fov_deg`
each → min) → `{ min_fov_deg, … }` → frontend `recommendTier(min_fov_deg, TIER_POLICY)`
→ banner + recommended-tier highlight. Download (recommended or per-tier) →
`download_catalog_layers(targetDensity)` (existing) → progress → `get_catalog_status`
refresh.

## Error handling

- Frames with missing/zero `focallen`/`xpixsz`/`naxis1` → excluded
  (`computable_count` < `light_count`); if none computable → neutral state, table
  still usable.
- `get_frame_fov_summary` DB error → surface to console; the panel falls back to
  the per-tier table (download still possible).

## Testing

- Unit: `frame_fov_deg` on a frame fixture (known optics → known FOV; missing
  field → None). `frame_fov_summary` on an in-memory DB with a few LIGHT frames of
  differing optics → correct `min_fov_deg` + `narrowest_instrume` + counts; a frame
  with no optics is excluded; zero computable → `min_fov_deg = None`.
- `extract_hints` still passes after the `frame_fov_deg` extraction (FOV unchanged).
- Frontend: `tsc --noEmit` clean; the recommendation uses `recommendTier` +
  `TIER_POLICY`; per-tier download invokes `download_catalog_layers` with the
  tier's density.

## Out of scope (YAGNI)

- Per-instrument breakdown in the UI (the summary returns only the global min +
  narrowest instrument).
- The manual focal/sensor calculator (removed).
- Re-recommending on solve-miss / reactive download.
