# Sky Chart

Reference for `src/pages/SkyChart.tsx` and `src/components/skychart/`. Issue #19 set this lifecycle up; #18 (HiPS background) builds on it.

## Lifecycle: one chart per app session

d3-celestial is a global singleton:
- `display()` re-fetches and re-parses every data file in `public/data` (24 MB, up to 118 k stars);
- objects added with `Celestial.add` live in a module-global list (`Celestial.data`) that only `Celestial.clear()` empties.

So the chart is displayed **once** and kept alive:

- `celestialHost.ts` owns the `#celestial-map` element.
  - The page attaches it into its slot in a layout effect and detaches it on unmount. Canvas, data, projection and zoom survive navigation untouched: no reload, no re-init, no fly-in.
  - `displayChart()` runs on the first visit, once the slot has a size, and registers **one** `raw` object. Its `redraw` fans out to `onChartRedraw` listeners and schedules the view save. `Celestial.data.length` stays 1 for the whole session.
  - React overlays (error, hints, spinner, empty state) are siblings of the slot. `#celestial-map` holds only what the library and the layers put there: canvas, zoom buttons, the library's `<container>`, the footprint SVG and the selection SVG.
  - A hot update of `celestialHost.ts` reloads the page (a second `display()` would duplicate the chart).
- `footprintLayer.ts` is the frame-set footprint SVG inside the host.
  - `setFootprints()` rebuilds the elements when locations or the date filter change; an empty list clears them.
  - `redrawFootprints()` is a redraw listener: one projection per box per frame, reused by the label pass. Visibility is a flag on the datum, never a computed-style read.
  - Page callbacks (open a frame set, selected-target visibility) are bound on mount and cleared on unmount.
- `useSvgOverlay` finds the host by id because the host is attached in a layout effect, before the page's effects run.

## View model

`skyView.ts`: `{ ra, dec, roll, fovDeg }`, where `fovDeg` is the **horizontal field of view**.

- **Never store the absolute projection scale.** d3-celestial's stereographic scale is `500 · width / 1024 · zoom`, so a stored scale frames a different area at another window width.
- **Conversion.** Stereographic maps an angle θ from the center to ρ = scale · tan(θ/2), so FOV = 4 · atan(width / (2 · scale)).
- **Zoom bounds** (`zoomextend: 20`) are width-independent: 11.72° … 182.72°.
- **Persistence.** The view is saved 600 ms after the last redraw to the setting `skychart.view` (JSON; generic `set_setting`), so it survives an app restart on both hosts. It is loaded once per session with `skychart.star_limit`.
- **Links.** `/skychart?ra=…&dec=…[&fov=…]` moves the chart once (roll 0), and the params are then dropped with `replace`. The URL is never written while panning. Shoot Calendar links with `fov=60`. The old absolute `zoom` param is ignored.
- **Jump to target** frames 4 × the footprint's larger side, the same at any width. A target without a field of view only re-centers.
- **No animations.** The chart runs with `disableAnimations: true`. Every programmatic move (restore, link, jump, +/−) is instant, the same with or without a saved view. Mouse pan and zoom are the library's zoom behaviour and are unaffected.
  - Before #19 the re-enable wrote to `Celestial.settings()`, the defaults template, not the live config. Restored sessions therefore had no animations while first runs did.

## Resize and pixel ratio

- **Resize keeps the view.** A `ResizeObserver` on the slot, coalesced to one `requestAnimationFrame`, calls `resizeChart()`. That reads the view, runs `Celestial.resize({ width, projectionRatio })` (which resets zoom to the base scale) and re-applies `fovDeg` before the browser paints. Center, roll and FOV stay; nothing is observed while the host is detached, and the first frame after attach re-syncs.
- **Pixel ratio.** `matchMedia('(resolution: Ndppx)')` plus a window `resize` re-check force a resize when `devicePixelRatio` changes. Patch P1 below makes the library re-read the ratio.
- **`background.width` is 0.** A non-zero width makes the library size the canvas `width + background.width` px and shifts the projection center. With 0, canvas CSS size = slot size, so pixel ↔ projection is the identity: no `getCanvasScaling`. The tile canvas of #18 relies on this.

## Library patches (`public/lib/celestial.min.js`, d3-celestial 0.7.35)

The bundled file is **not** the npm 0.7.35 build. `src/components/skychart/celestialPatches.test.ts` pins each patch; re-apply them when the file is replaced.

| # | Where | Change | Why |
| ---- | ---- | ---- | ---- |
| P1 | `f(t)` (resize) | `t&&(Ht=window.devicePixelRatio\|\|1);$.projectionRatio&&(Gt=$.projectionRatio);` at the top | A forced resize picks up a new pixel ratio, and the canvas height follows the slot's aspect instead of the projection's fixed ratio |
| P2 | `this.resize` | accepts `{ projectionRatio }` | Lets one resize call set width and aspect |

The `projectionRatio` half of P1 and P2 predate #19; #19 added the pixel-ratio re-read and wrote all of them down.

## Library facts the code depends on

- **`Celestial.settings()`** returns the defaults template, not the live config. Only `display` and `reload` change the live config.
- **`reload(config)`** re-applies the config last given to `display`, so `setStarLimit()` passes the current `width` and `projectionRatio`. It removes only the library's own `<container>` children; the SVG layers survive.
- **`raw` objects draw after stars and DSOs.** Anything that must sit *under* the stars needs its own canvas below the chart canvas (#18).
- **`Celestial.clip(p)`** is the visible-hemisphere test; `Celestial.zoomBy()` with no argument returns scale / base scale.

## Footprint rules (`footprint.ts`)

One routine, `projectFootprint`, used for every redraw:

| Hidden | When | Hint for the selected target |
| ---- | ---- | ---- |
| `clip` | every corner beyond the visible hemisphere, or a corner does not project | pan toward it |
| `span` | the box is wider or taller than half the canvas | zoom out |
| `distortion` | projected aspect off by more than 3× from the true one | pan toward it |

A box with only some corners beyond the hemisphere is drawn; the globe clip path trims it.

**Rotation convention (fixed 2026-10-26).** `frames.rotation` is CROTA2, or the plate solve's `field_rotation_deg` = atan2(−CD1_2, CD2_2). In that convention the image's +Y axis points at position angle **−rotation** (north through east), because the +Y column of the CD matrix is (CD1_2, CD2_2) in (east, north). Until this fix the box applied +rotation, a mirrored tilt: wrong by 2 × (rotation mod 90°), e.g. 27° for M 31 at 283.5°. It became visible once the DSS background showed the real sky under the box. Mirroring (det CD > 0) does not change a rectangle, only the +Y direction matters. Pinned by `footprint.test.ts`.

**Box size (fixed 2026-10-26).** `fovWidth` / `fovHeight` come from the backend: per light, the plate solve's CD matrix when solved, else XPIXSZ / FOCALLEN / NAXIS; per set, the median over the lights the pointing rule keeps (`docs/frame_sets/README.md`, Pointing). The corners invert it exactly: half-sides are tan(fov/2) = NAXIS · scale / 2 in the tangent plane. The M 31 report that prompted this turned out to be the background, not the box: the HEALPix mesh bug under **Drawing** below. With it fixed, M 31, M 32 and M 110 sit on their catalogue places at DPR 1 and 3, and a 2.63° × 1.75° box holds the galaxy the way the frame does.

## HiPS sky background (#18)

A sky survey image drawn under the chart. Phase 1 (this cycle): DSS colored, fetched on demand and kept locally. Phase 2 (offline pack download) and phase 3 (NSNS, gated on the author's agreement) are in #18.

**Rights (ruling).**
- Tiles are never redistributed: not in the installer, the Docker image, the repo, artfrom.space, the hub or Perseus.
- Every install fetches its own tiles from the survey's master URL into a private store.
- The survey's `obs_copyright` and license are shown on the chart whenever it is drawn, and in Settings → General → Sky background.
- DSS colored is `hips_status = public master clonableOnce`: a copy only from the master, never from a mirror.

**Core (`athenaeum-core/src/hips/`, `api/hips.rs`).**
- **Presets.** `SURVEYS` is the only source of survey URLs; there is no arbitrary-URL setting.
- **Store.** `<app_data>/hips/<survey-id>/` holds the standard layout `Norder{k}/Dir{(npix/10000)*10000}/Npix{n}.{jpg|png}` and the survey's original `properties`.
  - A 404 from the master leaves a zero-byte `Npix{n}.absent` marker, so that tile is never asked for again.
  - Writes are atomic (`.part` + rename).
  - A cached file without the format's signature is deleted and fetched again.
- **Fetch.**
  - `get_hips_tile` serves the store first.
  - A missing tile is fetched only when `skychart.hips_online` (default `true`) is on, after the survey's `properties` were read and validated: equatorial frame, 512 px tiles, the preset's format, order ≥ 6.
  - The HTTP client identifies as `Athenaeum/<version> (+https://artfrom.space)`, with a 10 s connect and 20 s total timeout and a 4 MB cap per tile.
  - A failed fetch is an error and is not remembered; the chart retries after 30 s.
- **Commands.** `get_hips_surveys`, `get_hips_tile` (bytes as `tauri::ipc::Response` / an `image/*` POST body; an empty payload means no coverage) and `clear_hips_cache` exist on both hosts.

**Frontend.**
- **Layer.** `hipsLayer.ts` draws on a canvas inserted *before* the d3-celestial canvas in the host, so it paints below it. While a survey is on, `setSkyBackdrop(true)` makes the chart's sky disc transparent and hides the Milky Way, through `Celestial.apply` with the current size.
- **Geometry.** `healpix.ts` is a port of cdshealpix 0.9.1 `nested::sph_coo`, pinned by `healpix.golden.json`. The core test `hips::golden` regenerates the file with `HIPS_GOLDEN_WRITE=1`; the TS test checks it to 1e-12 rad.
- **Visible tiles.** A cap walk from order 0. The order is the smallest whose tile pixel is ≤ one device pixel at the view center, capped at the survey's max (≤ 6).
- **Drawing.** Each tile is a mesh of affine-textured triangles drawn through `Celestial.mapProjection`, grown 0.5 px against seams. Vertices beyond 90° from the center are dropped.
  - **Mesh size** (`meshSize`) follows each tile's own bend. The tile drawn as one cell is compared with the projection at 9 probe points; that error falls with the square of the cell size, so n = ⌈√(error / 1 px)⌉, capped at 16 (also for a tile with a point that does not project). A 12° view at M 31 needs 2–4 cells per tile, ≈ 600 triangles, every tile within 1.5 px (`hipsLayer.test.ts`).
  - **While the chart moves** (a redraw less than 150 ms ago) every tile is one cell: a few px off, invisible in motion, and as cheap as before the fix. One fine repaint follows 150 ms after the last redraw.
  - **Cost** (headless Chromium, 30 pan frames at 12°, tile-layer JS): before the fix 28 ms at DPR 1 / 3.5 s at DPR 2 (software rasterization); a screen-sized mesh at every frame, briefly shipped in `7d2da893`, 749 ms at DPR 1 and a slow UI on a real machine; now 24 ms / 3.0 s, plus one fine repaint after a stop (3 ms at DPR 1).
  - **Split along the anti-diagonal** (dx + dy = const). At z = ±2/3 (|Dec| ≈ 41.8°) HEALPix turns from its equatorial to its polar projection, a kink in the tile's (dx, dy) that runs along that diagonal in every tile it crosses.
  - **Fixed 2026-10-26.** The mesh used to be one cell per 4° of tile, split along dx = dy. On the order-4 tile under M 31 (a 12° view at DPR 1) that bent the image by up to 98 px. M 110 sat 0.3° from its place while M 31 and M 32 looked right, so a correctly sized frame-set box looked half the size of the galaxy. Error by cells per side on that tile, old split / new split: 1 → 98 / 4.3 px, 4 → 26 / 0.28 px, 16 → 6.5 / 0.02 px. At DPR 3 the chart picks order 6, whose tiles are small enough that the bend was hidden.
- **Tile orientation (measured, #18).** Column c and row r from the top-left of a W-px JPEG tile sit at `dx = (r + 0.5) / W`, `dy = (c + 0.5) / W`.
  - It was found by scoring all 8 square symmetries on two real tiles, the Pleiades (order 3, npix 7) and Orion (order 2, npix 89). Only this mapping puts every catalogue star on a bright peak.
  - The HEALPix port used for the search agrees with an independent HEALPix implementation to 4 mas.
- **Loading.**
  - Up to 6 requests in flight, nearest the view center first.
  - At most 150 decoded bitmaps (LRU, `close()` on evict, never one in view).
  - Absent tiles are kept in a set of up to 5000 entries.
  - Cached ancestors are drawn under tiles still loading.
  - A survey change disposes everything and drops late responses.
- **Opacity.** The opacity slider draws a black veil of `1 − opacity` over opaque tiles: blending each grown triangle at partial alpha would light up the seams between them.
- **Settings.** `skychart.background` (`none` | survey id) and `skychart.background_opacity` are written by the chart's controls; `skychart.hips_online` is a KV setting with its default in `defaults::all()`.

**Sizes (DSS colored, live `hips_estsize` / tiles, ≈ 67 KB per tile).**

| Order | Pixel | Whole sky |
| ---- | ---- | ---- |
| 3 | 51″ | ≈ 55–70 MB |
| 4 | 26″ | ≈ 0.22–0.28 GB |
| 5 | 13″ | ≈ 0.9–1.1 GB |
| 6 | 6.4″ | ≈ 3.6–4.5 GB |

Browsing can never store more than the whole sky to order 6.

**Acceptance, phase 1 (2026-10-26, live CDS through the session's proxy, `athenaeum-web` serving the built frontend, headless Chromium 1600 × 900).**

| Check | Result |
| ---- | ---- |
| `get_hips_tile` 3/7 first request | 200 `image/jpeg`, 43 360 B in 1.3 s, bytes identical to a direct download |
| Same tile again | 2 ms, from the store |
| No API key (key set) | 401 |
| Unknown survey / order 7 / npix 768 at order 3 | 404 / 400 / 400 |
| Online off: cached tile / uncached tile | 200 / 404, no request leaves |
| `clear_hips_cache` | the store is gone; `"hips cache cleared"` logs `survey_id`, `bytes` |
| Chart at the Pleiades (12°), DSS colored on | 22 tile requests, 23 tiles stored (873 KB), the survey's Pleiades on the chart's own star marks, credit line shown |
| Leave the page and come back | the layer is still there, no new tile requests for the same view |
| HEALPix TS port vs cdshealpix golden vectors (728 points, orders 0–6) | ≤ 1e-12 rad |
| Tile-layer JS per frame (profiler, 15 pans) | ≈ 1 ms at 12°, ≈ 19 ms at 180°; the rest of a headless frame is software rasterization |

The desktop host was not compiled in this environment (no webkit2gtk); its three wrappers follow the existing byte-returning commands (`get_master_light_preview`).

## Acceptance (#19, 2026-10-26, web build + mock API, headless Chromium 1600 × 900)

| Check | Before | After |
| ---- | ---- | ---- |
| `Celestial.data.length` after 3 / 5 visits | 8 / 12 | 1 / 1 |
| Data files fetched per return to the page | 9 | 0 |
| View after leaving and returning | same (sessionStorage) | same (live chart) |
| FOV after resize while on another page (12.2° view) | 11.74° | 12.192° (unchanged) |
| Jump to M 31 at 1300 / 2000 px | 41.5° / 68.4° | 12.8° / 12.8° |
| View after reload (restart) | lost | restored from `skychart.view` |
| Footprint redraw hooks, 507 boxes, fresh / after 5 visits | 19.9 / 130 ms per frame | 9.3 / 10.2 ms per frame |
| Pixel ratio 1 → 2 | canvas stays 1× | canvas 2658 px wide (2×), view unchanged |

The pixel-ratio row was driven with a window `resize` event: headless Chromium's emulated ratio change fires neither the media query nor `resize`. On real hardware the media query is the primary signal; check it on a Mac moving the window between a Retina and a 1× screen.
