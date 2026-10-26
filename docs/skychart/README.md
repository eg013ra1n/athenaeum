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
