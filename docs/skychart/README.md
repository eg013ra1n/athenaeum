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

A sky survey image drawn under the chart. Phase 1: DSS colored, fetched on demand and kept locally. Phase 2: the offline pack, below. Phase 3: six NSNS DR0.2 surveys, served from every verified mirror (below). Stefan Ziegenbalg's written permission is in [`nsns-permission.md`](nsns-permission.md) (2026-11-03); the design is on #18.

**Rights (ruling).**
- Tiles are never redistributed: not in the installer, the Docker image, the repo, artfrom.space, the hub or Perseus.
- Every install fetches its own tiles into a private store, from the survey's master or a mirror whose own `hips_status` allows a copy.
- The survey's credit and license are shown on the chart whenever it is drawn, and in Settings → General → Sky background: the preset's `credit` / `credit_url` when it has one (NSNS), else `obs_copyright` / `obs_copyright_url`. The credit is a link (`openUrl`).
- **Copy rule per endpoint** (`mirrors::allows_copy`), from the endpoint's own record, HiPS 1.0 defaults `master` and `clonableOnce`: a `master` that is `clonable` or `clonableOnce`, or a `mirror` / `partial` that is `clonable`. Never `private`, never `unclonable`. A clone of a `clonableOnce` master is `unclonable`, so copying from it would be a clone of a clone. DSS colored (`public master clonableOnce`) is therefore served from its master and from any host that publishes the master record itself; NSNS (`public master clonable`, the GAVO copy `public mirror clonable`) from both.
- NSNS (phase 3) is CC BY-NC-SA 4.0: free Sky Chart only, credit "Northern Sky Narrowband Survey" with https://www.simg.de/nebulae3, URLs from HiPS servers' `hipslist` files and load balanced over every mirror, cache and offline pack allowed ([`nsns-permission.md`](nsns-permission.md)).

**Core (`athenaeum-core/src/hips/`, `api/hips.rs`).**
- **Presets.** `SURVEYS` is the only source of surveys and of their master URLs; there is no arbitrary-URL setting. A preset also carries its `creator_did`, the VO registry resources that may hold a copy (`registry_ivoids`), its `credit` / `credit_url` and `non_commercial`.

  | Id | Survey | Max order | Format | Typical tile |
  | ---- | ---- | ---- | ---- | ---- |
  | `dss-color` | DSS colored (CDS) | 9 (drawn to 6) | JPEG | 67 KB |
  | `nsns-rgb8` | NSNS RGB continuum | **5** | PNG | 620 KB |
  | `nsns-ohs8` | NSNS [OIII], Hα and [SII] | 6 | PNG | 610 KB |
  | `nsns-hbr8` | NSNS Hα and continuum | 6 | PNG | 690 KB |
  | `nsns-halpha8` | NSNS Hα | 6 | PNG | 134 KB |
  | `nsns-oiii8` | NSNS [OIII] | 6 | PNG | 128 KB |
  | `nsns-sii8` | NSNS [SII] | 6 | PNG | 138 KB |

  NSNS covers ≈ 65 % of the sky (north of Dec −16°); its other DR0.2 HiPS are 32-bit FITS (≈ 11–44 GB each) or variance maps, and DR0.1 is superseded, so they are not presets. `non_commercial` is true for NSNS (CC BY-NC-SA): it is never to be offered in a paid surface.
- **Mirrors** (`hips/mirrors.rs`). Candidates come from the CDS aggregate of HiPS lists (MocServer, `ID` = the `creator_did` without `ivo://`), the author's `https://www.simg.de/hipslist`, and one RegTAP query (`https://dc.g-vo.org/tap/sync`) for the HiPS capabilities of the presets' `registry_ivoids`: GAVO publishes no `hipslist`, only registry records. Every candidate's own `properties`, fetched from the candidate URL, must name the preset's `creator_did`, pass the preset's checks, allow a copy, and carry the release date of the store's `properties` (else the master's). The address is the URL the `properties` came from, never the record's `hips_service_url`: GAVO's `rgb8` record names `/nsns/q/rgb8/hips`, a 404; the registry has `/nsns/cont/rgb8/hips`. URLs are moved to `https`. The result is `<app_data>/hips/mirrors.json`, refreshed at most once a day: in the background on the first tile fetch while online, on the pack's own thread before a pack runs. The first failed CDS query ends the CDS part of that search (the others would wait out the same timeout). A search where every source failed keeps the stored list and is not repeated within the hour; with no list, the master alone serves.
  - Checked live 2026-11-03: GAVO mirrors all six NSNS products (same `creator_did`, release dates and sizes, tiles byte-identical, the same 404s south of the coverage). CDS was unreachable that day from both the cloud container and the owner's machine, so whether it holds NSNS copies, and the status of DSS colored's second CDS host, is still to be read.
- **Balancing** (`hips/balance.rs`), one pool per survey, master first. A request goes to the endpoint that is up with the fewest requests in flight, then the lower latency (EWMA, an unmeasured endpoint counts as fastest). A request error takes the endpoint down 30 s, doubling to 10 min, and the tile goes to the next one; a 429 / 503 takes it down for `Retry-After` ([5 s, 300 s], else 30 s). The caller sees `ServerBusy` only when every endpoint is busy, so the pack's busy wait runs then. A mirror's 404 sends the tile to the master; only the master's 404, or every mirror's while the master is down, is absent (a busy master is waited for instead). `get_hips_surveys` lists the hosts as `servers`.
- **Store.** `<app_data>/hips/<survey-id>/` holds the standard layout `Norder{k}/Dir{(npix/10000)*10000}/Npix{n}.{jpg|png}` and the survey's original `properties`.
  - An absent tile (see **Balancing**) leaves a zero-byte `Npix{n}.absent` marker, so that tile is never asked for again.
  - Writes are atomic (`.part` + rename).
  - A cached file without the format's signature is deleted and fetched again.
- **Fetch.**
  - `get_hips_tile` serves the store first.
  - A missing tile is fetched only when `skychart.hips_online` (default `true`) is on, after the survey's `properties` were read (from the master, else a mirror) and validated: equatorial frame, 512 px tiles, the preset's format, the preset's `creator_did`, an order at least the drawn one (`min(max_order, 6)`).
  - The HTTP client identifies as `Athenaeum/<version> (+https://artfrom.space)`, with a 10 s connect and 20 s total timeout and a 4 MB cap per tile.
  - A failed fetch is an error and is not remembered; the chart retries after 30 s.
- **Commands.** `get_hips_surveys`, `get_hips_tile` (bytes as `tauri::ipc::Response` / an `image/*` POST body; an empty payload means no coverage) and `clear_hips_cache` exist on both hosts.

**Frontend.**
- **Layer.** `hipsLayer.ts` draws on a canvas inserted *before* the d3-celestial canvas in the host, so it paints below it. While a survey is on, `setSkyBackdrop(true)` makes the chart's sky disc transparent and hides the Milky Way, through `Celestial.apply` with the current size.
- **Geometry.** `healpix.ts` is a port of cdshealpix 0.9.1 `nested::sph_coo`, pinned by `healpix.golden.json`. The core test `hips::golden` regenerates the file with `HIPS_GOLDEN_WRITE=1`; the TS test checks it to 1e-12 rad.
- **Visible tiles.** A cap walk from order 0. The order is the smallest whose tile pixel is ≤ one device pixel at the view center, capped at the survey's max (≤ 6).
- **Drawing.** Each tile is a mesh of textured triangles projected through `Celestial.mapProjection` on the CPU. Vertices beyond 90° from the center are dropped.
  - **WebGL (`hipsGl.ts`)** when the webview has it: one texture per tile bitmap (deleted with the bitmap), the mesh drawn as one `drawArrays` per tile, opacity as a colour scale. The fine mesh (0.25 px tolerance) is drawn at every frame, so panning shows no bend and nothing snaps after a stop.
  - **2D canvas fallback** when `getContext('webgl')` fails (logged once): affine-clipped triangles, grown 0.5 px against seams; a black veil for opacity, since blending each grown triangle at partial alpha would light up the seams between them. While the chart moves (a redraw less than 150 ms ago) every tile is one cell; one fine repaint (1 px tolerance) follows 150 ms after the last redraw.
  - **Mesh size** (`meshSize`) follows each tile's own bend. The tile drawn as one cell is compared with the projection at 9 probe points; that error falls with the square of the cell size, so n = ⌈√(error / tolerance)⌉, capped at 16 (also for a tile with a point that does not project). At 1 px a 12° view at M 31 needs 2–4 cells per tile, ≈ 600 triangles, every tile within 1.5 px (`hipsLayer.test.ts`); WebGL's 0.25 px doubles the cells per side.
  - **Split along the anti-diagonal** (dx + dy = const), in both paths. At z = ±2/3 (|Dec| ≈ 41.8°) HEALPix turns from its equatorial to its polar projection, a kink in the tile's (dx, dy) that runs along that diagonal in every tile it crosses.
  - **Fixed 2026-10-26.** The mesh used to be one cell per 4° of tile, split along dx = dy. On the order-4 tile under M 31 (a 12° view at DPR 1) that bent the image by up to 98 px. M 110 sat 0.3° from its place while M 31 and M 32 looked right, so a correctly sized frame-set box looked half the size of the galaxy. Error by cells per side on that tile, old split / new split: 1 → 98 / 4.3 px, 4 → 26 / 0.28 px, 16 → 6.5 / 0.02 px. At DPR 3 the chart picks order 6, whose tiles are small enough that the bend was hidden.
  - **Cost** (headless Chromium with software WebGL, 30 pan frames at 12°, tile-layer JS per frame):

    | Build | DPR 1 | DPR 2 |
    | ---- | ---- | ---- |
    | Before the mesh fix (2D, 2 triangles per tile, bent) | 0.9 ms | 117 ms |
    | Screen-sized 2D mesh at every frame (`7d2da893`, slow on a real machine) | 25 ms | — |
    | 2D, one cell while moving + fine repaint after a stop (`308f7b55`, bends while dragging) | 0.8 ms | 99 ms |
    | WebGL, fine mesh at every frame (≈ 1150 triangles at DPR 1) | 3.0 ms | 3.9 ms |

- **Tile orientation (measured, #18).** Column c and row r from the top-left of a W-px JPEG tile sit at `dx = (r + 0.5) / W`, `dy = (c + 0.5) / W`.
  - It was found by scoring all 8 square symmetries on two real tiles, the Pleiades (order 3, npix 7) and Orion (order 2, npix 89). Only this mapping puts every catalogue star on a bright peak.
  - The HEALPix port used for the search agrees with an independent HEALPix implementation to 4 mas.
- **Loading.**
  - Up to 6 requests in flight, nearest the view center first.
  - At most 150 decoded bitmaps (LRU, `close()` on evict, never one in view).
  - Absent tiles are kept in a set of up to 5000 entries.
  - Cached ancestors are drawn under tiles still loading. A tile the store refuses (offline and not stored) asks for its nearest ancestor not known to fail, one order per failure, at the back of the queue; an ancestor drawn as a stand-in is kept from eviction and refreshed in the LRU, so the offline pack's order-k sky shows under finer views instead of black.
  - A survey change disposes everything and drops late responses.
- **Opacity.** The opacity slider draws a black veil of `1 − opacity` over opaque tiles: blending each grown triangle at partial alpha would light up the seams between them.
- **PNG alpha** (NSNS is transparent outside its coverage). Bitmaps are decoded premultiplied (`createImageBitmap(…, { premultiplyAlpha: 'premultiply' })`) and uploaded with `UNPACK_PREMULTIPLY_ALPHA_WEBGL`, so a transparent texel is black over the black sky on WebGL; the 2D path composites over the black clear on its own.
- **Settings.** `skychart.background` (`none` | survey id) and `skychart.background_opacity` are written by the chart's controls; `skychart.hips_online` is a KV setting with its default in `defaults::all()`.

**Sizes (DSS colored, live `hips_estsize` / tiles, ≈ 67 KB per tile).**

| Order | Pixel | Whole sky |
| ---- | ---- | ---- |
| 3 | 51″ | ≈ 55–70 MB |
| 4 | 26″ | ≈ 0.22–0.28 GB |
| 5 | 13″ | ≈ 0.9–1.1 GB |
| 6 | 6.4″ | ≈ 3.6–4.5 GB |

Browsing can never store more than the whole sky to order 6.

**Offline pack (phase 2, owner ruling 2026-10-26 on #18).** A user can download the sky ahead of time in Settings → General → Sky background, so the chart works in the field without internet. The chart only links there ("Download for offline…" in its credit strip).
- **What a pack covers** (`hips/pack.rs::pack_tiles`): the whole sky at orders `0..=k`, plus, around each chosen region, the cone coverage at orders `k+1..=` the survey's drawn maximum (6, or 5 for `nsns-rgb8`; k is clamped to it too). The renderer falls back to ancestors only, so a detail region needs every order above k, not order 6 alone.
  - k is selectable 0–5 (default **4**, 26″/px). At the chart's narrowest view (11.72°) `chooseOrder` asks for order 4 up to 1 635 device px of window width, order 5 from 1 636 and order 6 from 3 271 (any Retina window from 818 CSS px wants order 5). Outside the detail cones such a view draws the stored order-k ancestor, scaled up; inside them it is sharp. A whole-sky order 6 is never a pack (3.6–4.5 GB); browsing still caches it.
  - Regions (checkboxes): **imaging locations** (default on; every location the chart draws, radius = half the FOV diagonal + 1°, or 2° without a FOV), **Messier** (on; 108 objects, 3°), **NGC/IC brighter than 10ᵐ** (off; 491 objects, 3°, ≈ 0.6 GB at order 6). Every catalog object was ruled out: the chart's 15 809 DSOs at 2–3° cover 88–98 % of the sky.
  - Cone coverage goes through `catalog::healpix::cone_pixels_at`, the one wrapper around cdshealpix's knife-edge panic near |Dec| ≈ 41.8° (M 31 sits there).
- **Estimate** (`estimate_hips_pack`, never touches the network): tiles total / already stored (images and `.absent` markers) / missing; size per tile from the stored `properties` (`hips_estsize` KB × 1024 / `hips_nb_tiles`), else the preset's `approx_tile_bytes` (67 000 for DSS, see **Presets** for NSNS), times `moc_sky_fraction` from the same `properties`, else the preset's `approx_sky_fraction` (tiles outside the coverage are 404 markers and weigh nothing; DSS 1, NSNS 0.65); free space from `disk::free_bytes`. A start whose missing tiles plus a 512 MiB reserve do not fit is refused; nothing missing skips the check.
- **Runner** (`run_pack`): 4 workers over one deduplicated tile list; tiles already stored are counted `present` without network. An explicit pack runs with `skychart.hips_online` off (the toggle governs browsing only), and fetches and validates `properties` first.
  - **Busy survey** (429 / 503, `hips::ServerBusy`): wait `Retry-After` seconds clamped to [5 s, 300 s] (the clamp is applied before any arithmetic: a header of `u64::MAX` seconds is real input), else back off 5 s → 300 s; a tile still busy after 6 waits is `failed`. 4 such tiles in a row (≈ 5 min of solid 429/503) stop the run as `failed` ("survey busy"); a fetched tile or a 404 resets the streak.
  - **Network down**: 20 consecutive plain errors (connection, HTTP 5xx other than 503, a non-image body, a write error) stop the run as `failed`, finished tiles kept.
  - **Cancel** finishes the requests in flight and keeps every finished tile; sleeps are ≤ 200 ms steps, so a cancel lands within about a second even in a 300 s wait.
  - **Progress**: `PackProgress` (state, selection, total, present, fetched, absent, failed, bytes, `waiting_s`, error) is emitted as `hips-download-progress` at most every 250 ms plus once at the end, and a busy wait is shown while it lasts.
- **One pack per process** (`api/hips.rs`, a static slot beside `validated()`, not a `ServiceContext` field: that struct has 26 literal construction sites, and one pack at a time is the rule anyway). A start claims the slot under its mutex before spawning; a guard frees it on every exit, a panic included (the panic is written and emitted as `failed` unless the run had already ended). A slot whose run reached a terminal state counts as free.
- **`<store>/pack.json`** holds the last `PackProgress`, written at the start (`running`) and the end (the final counts). `get_hips_pack_status` returns the live run, else that file (a stored `running` after a restart reads `interrupted`, with its selection, so Settings offers Resume), else `idle`; an unreadable file is a warning and `idle`.
- **Clear** is refused while a pack of that survey runs. Under the slot lock it renames the store to a sibling tombstone (`<id>.clearing-<pid>-<n>`) and sizes and deletes it after the lock is released; tombstones a crash left behind (other pids) are swept by the next Clear. On Windows a rename refused for an open file reads "Some sky tiles are in use; try again in a moment".
- **Build gate**: the pack (`hips::pack` and its four commands) is `cfg(all(feature = "render", feature = "solver"))`, the gate of the DSO catalog and `catalog::healpix` it uses; tile serving (phase 1) stays ungated.
- **Commands** (both hosts): `estimate_hips_pack { selection }`, `start_hips_pack { selection }` (returns at once), `cancel_hips_pack` (NotFound once the run ended; the UI ignores it), `get_hips_pack_status { surveyId }`.

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

**Acceptance, phase 2 (offline pack; owner: works, 2026-10-31).** Estimate against what landed, measured 2026-10-27: order 4 + Messier + locations (dev app) estimated 685 MB from `properties`, 840 MB landed (+23 %, inside the 30 % bar); a whole-sky order 3 on an empty store (web) estimated 68.3 MB from `approx_tile_bytes`, 44.1 MB landed (−35 %, an over-estimate, so the disk check errs on the safe side). Not provoked live: a real 429 / 503 busy wait, so the "Survey busy, waiting N s" line clearing after the wait (`Run::settle`) is covered by neither a test nor a run.

**Acceptance, phase 3 (2026-11-03, live servers through the session's proxy, `athenaeum-web` serving the built frontend, headless Chromium 1600 × 900, empty `mirrors.json`).**

| Check | Result |
| ---- | ---- |
| Background select | None, DSS colored and the six NSNS presets |
| First tile request with no mirror list | tiles served from the master at once; the background search found the six GAVO mirrors, `rgb8` at `/nsns/cont/rgb8/hips` (the registry's URL, not the record's 404), while CDS timed out (asked once) |
| NSNS Hα at the North America Nebula (12°) | drawn; credit "NSNS Hα: Northern Sky Narrowband Survey, Stefan Ziegenbalg · CC BY-NC-SA 4.0", a link to the project page |
| NSNS RGB at Dec −14°, 30° (Seagull Nebula, Sirius), mirrors known | 36 tiles from www.simg.de and 10 from dc.g-vo.org; 20 tiles south of the coverage absent on the master's word; the coverage edge drawn black |
| Settings → Sky background | the seven surveys, "Servers: www.simg.de, dc.g-vo.org" for each NSNS, a pack estimate each |
| DSS colored while CDS is down | its tiles fail (500 to the chart, retried after 30 s); no mirror can stand in until the CDS list says which host may |

Not provoked live: an endpoint going down mid-run and the failover to the other (covered by `hips::balance` tests), a 429 from either server.

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
