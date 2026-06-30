# Auto Catalog-Tier Recommendation from Frame Metadata — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recommend the right density-tier set automatically from the LIGHT frames
already in the catalog (narrowest field → deepest tier needed) and let the user
download the recommended set or any tier directly — replacing the manual
focal/sensor FOV calculator.

**Architecture:** Extract the canonical per-frame FOV formula out of
`plate_solve::hints` into a reusable function; add a pure aggregator + a DB query
that returns an `FovSummary`; expose it via `get_frame_fov_summary` on both
backends; rewrite the frontend "Star catalog" section to show an auto
recommendation + a per-tier table with direct downloads, and drop the manual
calculator.

**Tech Stack:** Rust (`athenaeum-core`, `athenaeum-tauri`, `athenaeum-web`),
`rusqlite`, `serde`; React/TS frontend (design tokens, `lucide-react`).

**Spec:** `docs/superpowers/specs/2026-06-30-auto-catalog-tier-from-frames-design.md`

## Global Constraints

- **Two backends in sync.** `get_frame_fov_summary` needs the Tauri command
  (`crates/athenaeum-tauri/src/commands/plate_solve.rs`, registered in `lib.rs`)
  AND the mirrored Axum route (`crates/athenaeum-web/src/routes/plate_solve.rs`,
  registered in `routes/mod.rs`) in the same change; real logic in `athenaeum-core`.
- **Serde boundary.** `FovSummary` has NO `rename_all` → snake_case wire; keep the
  `src/types/plate-solve.ts` mirror snake_case (`light_count`, `computable_count`,
  `min_fov_deg`, `narrowest_instrume`).
- **No `@tauri-apps/*` outside `src/api/`.** Frontend goes through the `api` object.
- **Design tokens, not raw colours** (`bg-surface`, `text-content-muted`, `bg-accent`, …).
- **FOV formula:** `FOV = 2·atan(naxis1·(xpixsz/1000) / (2·focallen))` in degrees.
  Use `XPIXSZ` directly — binning is already folded into the saved-image pixel
  pitch; do NOT multiply by binning.
- **Recommend by the globally narrowest LIGHT field.** Download "up to" a density =
  all tiers with `density ≤ target` (additive).
- **No per-instrument breakdown in the UI** (YAGNI) — global min + narrowest
  instrument label only.

## File Structure

- Modify `crates/athenaeum-core/src/plate_solve/hints.rs` — add `fov_from_optics`,
  `frame_fov_deg`, `FovSummary`, `fov_summary`, `frame_fov_summary`; refactor
  `extract_hints` to call `frame_fov_deg`.
- Modify `crates/athenaeum-core/src/plate_solve/mod.rs` — export `frame_fov_summary` + `FovSummary`.
- Modify `crates/athenaeum-tauri/src/commands/plate_solve.rs` + `crates/athenaeum-tauri/src/lib.rs` — command + registration.
- Modify `crates/athenaeum-web/src/routes/plate_solve.rs` + `crates/athenaeum-web/src/routes/mod.rs` — route + registration.
- Modify `src/types/plate-solve.ts` — `FovSummary` type.
- Modify `src/components/plate-solve/PlateSolveSettingsPanel.tsx` — auto banner + per-tier direct download, remove manual inputs.
- Modify `src/components/plate-solve/cameraPresets.ts` — drop `CAMERA_PRESETS`/`pixelScaleArcsec`/`fovDeg`; keep `TIER_POLICY`/`recommendTier`.

---

### Task 1: FOV formula + summary in `athenaeum-core`

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/hints.rs`
- Modify: `crates/athenaeum-core/src/plate_solve/mod.rs`
- Test: inline in `hints.rs`

**Interfaces:**
- Produces:
  - `pub fn fov_from_optics(focallen: Option<f64>, xpixsz: Option<f64>, naxis1: Option<i32>) -> Option<f64>`
  - `pub fn frame_fov_deg(frame: &Frame) -> Option<f64>`
  - `pub struct FovSummary { pub light_count: u32, pub computable_count: u32, pub min_fov_deg: Option<f64>, pub narrowest_instrume: Option<String> }` (`#[derive(Clone, Debug, Serialize)]`)
  - `pub fn fov_summary<I: IntoIterator<Item = (Option<f64>, Option<f64>, Option<i32>, Option<String>)>>(rows: I) -> FovSummary`
  - `pub fn frame_fov_summary(conn: &Connection) -> rusqlite::Result<FovSummary>`

- [ ] **Step 1: Write the failing tests** (add a `#[cfg(test)] mod tests` block at the end of `hints.rs`, or extend the existing one)

```rust
#[cfg(test)]
mod fov_tests {
    use super::*;

    #[test]
    fn fov_from_optics_matches_formula_and_guards() {
        // 270mm, 3.76µm, 6248px → ~4.98° (ASI2600 at a short focal length).
        let fov = fov_from_optics(Some(270.0), Some(3.76), Some(6248)).unwrap();
        assert!((fov - 4.98).abs() < 0.02, "got {fov}");
        // Missing or non-positive inputs → None.
        assert_eq!(fov_from_optics(None, Some(3.76), Some(6248)), None);
        assert_eq!(fov_from_optics(Some(0.0), Some(3.76), Some(6248)), None);
        assert_eq!(fov_from_optics(Some(270.0), Some(3.76), Some(0)), None);
    }

    #[test]
    fn fov_summary_takes_global_narrowest_and_counts() {
        let rows = vec![
            (Some(270.0), Some(3.76), Some(6248), Some("ASI2600".to_string())),   // ~4.98°
            (Some(2491.0), Some(3.76), Some(2048), Some("SG_32".to_string())),    // ~0.18° narrowest
            (None, None, None, Some("NoOptics".to_string())),                     // not computable
        ];
        let s = fov_summary(rows);
        assert_eq!(s.light_count, 3);
        assert_eq!(s.computable_count, 2);
        assert!((s.min_fov_deg.unwrap() - 0.18).abs() < 0.02, "{:?}", s.min_fov_deg);
        assert_eq!(s.narrowest_instrume.as_deref(), Some("SG_32"));
    }

    #[test]
    fn fov_summary_none_when_nothing_computable() {
        let s = fov_summary(vec![(None, None, None, Some("x".to_string()))]);
        assert_eq!(s.light_count, 1);
        assert_eq!(s.computable_count, 0);
        assert_eq!(s.min_fov_deg, None);
        assert_eq!(s.narrowest_instrume, None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core fov_from_optics_matches_formula`
Expected: FAIL — `fov_from_optics` not defined.

- [ ] **Step 3: Implement the formula + summary** (add to `hints.rs`; add `use serde::Serialize;` at the top if not present)

```rust
/// Field of view (degrees) from optics. `XPIXSZ` is the effective saved-pixel
/// pitch (binning already folded in), so it is used directly — never multiplied
/// by binning. `FOV = 2·atan(sensor_mm / (2·focallen))`, sensor_mm = naxis1·(xpixsz/1000).
pub fn fov_from_optics(
    focallen: Option<f64>,
    xpixsz: Option<f64>,
    naxis1: Option<i32>,
) -> Option<f64> {
    let (focallen, xpixsz, naxis1) = (focallen?, xpixsz?, naxis1?);
    if focallen <= 0.0 || xpixsz <= 0.0 || naxis1 <= 0 {
        return None;
    }
    let pixel_size_mm = xpixsz / 1000.0;
    let sensor_mm = naxis1 as f64 * pixel_size_mm;
    Some(2.0 * (sensor_mm / (2.0 * focallen)).atan().to_degrees())
}

/// FOV (degrees) of a frame from its optics, or `None` if optics are missing.
pub fn frame_fov_deg(frame: &Frame) -> Option<f64> {
    fov_from_optics(frame.focallen, frame.xpixsz, frame.naxis1)
}

/// Field-of-view summary across a set of LIGHT frames (the catalog-tier
/// recommendation input).
#[derive(Clone, Debug, Serialize)]
pub struct FovSummary {
    pub light_count: u32,
    pub computable_count: u32,
    pub min_fov_deg: Option<f64>,
    pub narrowest_instrume: Option<String>,
}

/// Aggregate `(focallen, xpixsz, naxis1, instrume)` rows into an `FovSummary`,
/// keeping the globally narrowest computable field.
pub fn fov_summary<I>(rows: I) -> FovSummary
where
    I: IntoIterator<Item = (Option<f64>, Option<f64>, Option<i32>, Option<String>)>,
{
    let mut light_count = 0u32;
    let mut computable_count = 0u32;
    let mut min_fov_deg: Option<f64> = None;
    let mut narrowest_instrume: Option<String> = None;
    for (focallen, xpixsz, naxis1, instrume) in rows {
        light_count += 1;
        if let Some(fov) = fov_from_optics(focallen, xpixsz, naxis1) {
            computable_count += 1;
            if min_fov_deg.map_or(true, |m| fov < m) {
                min_fov_deg = Some(fov);
                narrowest_instrume = instrume;
            }
        }
    }
    FovSummary { light_count, computable_count, min_fov_deg, narrowest_instrume }
}

/// Query LIGHT frames and summarise their fields of view.
pub fn frame_fov_summary(conn: &Connection) -> rusqlite::Result<FovSummary> {
    let mut stmt = conn.prepare(
        "SELECT focallen, xpixsz, naxis1, instrume FROM frames WHERE imagetyp LIKE 'LIGHT%'",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<f64>>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<i32>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(fov_summary(rows))
}
```

- [ ] **Step 4: Refactor `extract_hints` to reuse `frame_fov_deg`** — replace the
  optics block (currently the `if let (Some(focallen), Some(xpixsz)) = …` that sets
  both `pixel_scale_arcsec` and `fov_deg`) with:

```rust
    if let (Some(focallen), Some(xpixsz)) = (frame.focallen, frame.xpixsz) {
        if focallen > 0.0 && xpixsz > 0.0 {
            let pixel_size_mm = xpixsz / 1000.0;
            hints.pixel_scale_arcsec =
                Some((pixel_size_mm / focallen).atan().to_degrees() * 3600.0);
        }
    }
    hints.fov_deg = frame_fov_deg(frame);
```

(Keep the long explanatory comment above the block.) This preserves behaviour:
`fov_deg` is still `None` unless focallen+xpixsz+naxis1 are all valid.

- [ ] **Step 5: Export from `plate_solve/mod.rs`** — add:

```rust
pub use hints::{frame_fov_summary, FovSummary};
```

- [ ] **Step 6: Run tests + the existing hints tests**

Run: `cargo test -p athenaeum-core plate_solve::hints && cargo test -p athenaeum-core fov_summary`
Expected: PASS (new fov tests + the existing `extract_hints` tests — FOV unchanged).

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/plate_solve/hints.rs crates/athenaeum-core/src/plate_solve/mod.rs
git commit -m "feat(plate-solve): frame_fov_deg/fov_summary + frame_fov_summary (reused from hints)"
```

---

### Task 2: `get_frame_fov_summary` command (both backends) + TS type

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/plate_solve.rs` + `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/plate_solve.rs` + `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `src/types/plate-solve.ts`

**Interfaces:**
- Consumes: `athenaeum_core::plate_solve::{frame_fov_summary, FovSummary}` (Task 1).
- Produces: Tauri command `get_frame_fov_summary() -> FovSummary`; Axum route
  `POST /api/get_frame_fov_summary`; TS `FovSummary`.

- [ ] **Step 1: Tauri command** — add to `crates/athenaeum-tauri/src/commands/plate_solve.rs`:

```rust
#[tauri::command]
pub async fn get_frame_fov_summary(
    state: State<'_, AppState>,
) -> Result<athenaeum_core::plate_solve::FovSummary, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    athenaeum_core::plate_solve::frame_fov_summary(&conn).map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Register the Tauri command** — in `crates/athenaeum-tauri/src/lib.rs`,
  in the `invoke_handler` list next to `commands::get_catalog_status,`:

```rust
            commands::get_frame_fov_summary,
```

- [ ] **Step 3: Axum route** — add to `crates/athenaeum-web/src/routes/plate_solve.rs`:

```rust
pub async fn get_frame_fov_summary(
    State(state): State<WebAppState>,
) -> Result<Json<athenaeum_core::plate_solve::FovSummary>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".to_string()))?;
    let conn = db.conn();
    athenaeum_core::plate_solve::frame_fov_summary(&conn)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
```

- [ ] **Step 4: Register the Axum route** — in `crates/athenaeum-web/src/routes/mod.rs`,
  next to the `get_catalog_status` route:

```rust
        .route("/api/get_frame_fov_summary", post(plate_solve::get_frame_fov_summary))
```

- [ ] **Step 5: TS type** — add to `src/types/plate-solve.ts`:

```typescript
export interface FovSummary {
  light_count: number;
  computable_count: number;
  min_fov_deg: number | null;
  narrowest_instrume: string | null;
}
```

- [ ] **Step 6: Build both backends + tsc**

Run: `cargo build -p athenaeum-tauri -p athenaeum-web && npx tsc --noEmit`
Expected: both backends compile; tsc clean.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-tauri crates/athenaeum-web src/types/plate-solve.ts
git commit -m "feat(plate-solve): get_frame_fov_summary command (both backends)"
```

---

### Task 3: Frontend — auto recommendation + per-tier direct download; remove manual calculator

**Files:**
- Modify: `src/components/plate-solve/PlateSolveSettingsPanel.tsx`
- Modify: `src/components/plate-solve/cameraPresets.ts`

**Interfaces:**
- Consumes: `api.invoke('get_frame_fov_summary') -> FovSummary`;
  `recommendTier(fov, TIER_POLICY)`; existing `get_catalog_status`,
  `download_catalog_layers({ targetDensity })`, `catalog-download-progress`.

- [ ] **Step 1: Trim `cameraPresets.ts`** — delete `CameraPreset`, `CAMERA_PRESETS`,
  `pixelScaleArcsec`, and `fovDeg`. **Keep** `recommendTier`, `TierPolicy`, and
  `TIER_POLICY` only. (Use the `frontend-design` skill / a `frontend-dev` agent for
  the panel JSX in the next steps — project convention for UI work.)

- [ ] **Step 2: Rewrite the "Star catalog" section of `PlateSolveSettingsPanel.tsx`:**
  - Remove the manual FOV-helper inputs (focal length, camera preset, pixel size,
    width, height, binning) and all their state/imports
    (`CAMERA_PRESETS`/`pixelScaleArcsec`/`fovDeg`, `focalMm`/`pixelUm`/`widthPx`/
    `heightPx`/`binning`/`presetIdx`).
  - Add `fovSummary` state (`FovSummary | null`) and load it on mount alongside
    `loadCatalogStatus`, via `const s = await api.invoke<FovSummary>('get_frame_fov_summary'); setFovSummary(s);` (wrap in try/catch → `setFovSummary(null)` on error, logging the error).
  - Compute `const recommended = fovSummary?.min_fov_deg != null ? recommendTier(fovSummary.min_fov_deg, TIER_POLICY) : 2000;`.
  - **Auto banner:** when `fovSummary && fovSummary.computable_count > 0`, render
    "From your {computable_count} light frame(s) — narrowest field
    {min_fov_deg.toFixed(2)}° ({narrowest_instrume}) → recommended:
    {recommended.toLocaleString()} stars/deg²" with a **Download recommended set**
    button calling `downloadStarCatalog(recommended)`. When `computable_count === 0`
    (or summary null), render a neutral line: "No frames with usable optics yet —
    pick a tier below."
  - **Per-tier table** (keep the existing `tierRows` built from `TIER_POLICY` +
    `catalogs`): the recommended tier stays highlighted; for each row that is NOT
    installed, add a small **Download** action (a button/link) that calls
    `downloadStarCatalog(tier.density)` (downloads all tiers `≤ tier.density`).
  - Keep the existing download-progress UI + the `catalog-download-progress`
    listener. The standalone "Download needed set" block can be removed (its role is
    now the banner button + the per-row downloads) or kept as the banner's button —
    either way, every download path calls `downloadStarCatalog(<targetDensity>)`.
  - Design tokens only; `api` object only.

- [ ] **Step 3: Type-check**

Run: `npx tsc --noEmit`
Expected: PASS — no references to the removed `cameraPresets` exports or removed
state remain.

- [ ] **Step 4: Commit**

```bash
git add src/components/plate-solve/PlateSolveSettingsPanel.tsx src/components/plate-solve/cameraPresets.ts
git commit -m "feat(catalog): auto tier recommendation from frames + per-tier direct download (drop manual FOV calculator)"
```

---

### Task 4: End-to-end check on real data

**Files:** none (validation)

- [ ] **Step 1: Confirm the summary command against the real DB** — start the web
  backend and call the new command:

```bash
ATHENAEUM_DB_PATH=/path/to/athenaeum.db cargo run -p athenaeum-web &
curl -s -XPOST localhost:3000/api/get_frame_fov_summary | jq
```
Expected: `{ light_count, computable_count, min_fov_deg, narrowest_instrume }` — on
the dev DB, `min_fov_deg ≈ 0.18` (`SG_32_Det` at 2491mm) or the narrowest field
present. `recommendTier(min_fov_deg)` is the tier the banner will show.

- [ ] **Step 2: Desktop smoke** — `npm run tauri dev` (with
  `ATHENAEUM_CATALOG_BASE_URL` set to a reachable catalog host). The "Star catalog"
  panel shows the auto recommendation from your frames + the per-tier table with
  per-row download; no manual calculator. Clicking a tier's Download starts a
  download up to that density.

## Self-Review

- **Spec coverage:** `frame_fov_deg` extraction + reuse → Task 1 Steps 3–4;
  `frame_fov_summary`/`FovSummary` → Task 1; `get_frame_fov_summary` both backends →
  Task 2; auto banner + per-tier direct download + remove manual calculator →
  Task 3; cleanup of `cameraPresets` → Task 3 Step 1; tests → Task 1 + Task 4.
- **Placeholders:** none — every code step has concrete code; Task 3 Step 2 is an
  inherently presentational change with the exact invoke/helper names and the exact
  state to remove (delegated to frontend-design per project convention).
- **Type consistency:** `fov_from_optics(Option<f64>, Option<f64>, Option<i32>)`,
  `frame_fov_deg(&Frame)`, `FovSummary{light_count,computable_count,min_fov_deg,
  narrowest_instrume}`, `fov_summary`, `frame_fov_summary` consistent across Tasks
  1–2; the TS `FovSummary` mirrors the Rust struct (snake_case); `recommendTier` +
  `TIER_POLICY` reused in Task 3 (defined in `cameraPresets.ts`, kept by Task 3
  Step 1).

## Risks

- `frame_fov_summary` runs one `SELECT` over LIGHT frames per panel load — cheap
  (a few-column scan), but it touches the DB; the command resolves a `conn` like the
  other plate-solve commands (no long lock).
- `extract_hints` behaviour must not change — Task 1 Step 6 runs the existing hints
  tests to confirm FOV/pixel-scale are byte-identical after the extraction.

## Follow-on (not this plan)

- Optional per-instrument breakdown in the UI.
