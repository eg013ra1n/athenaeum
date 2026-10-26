/**
 * The HiPS sky background (issue #18): a canvas under the d3-celestial
 * canvas, inside the persistent chart host, so stars, lines, footprints and
 * markers stay on top.
 *
 * Each tile is drawn as a mesh of textured triangles projected through
 * d3-celestial's own projection (mirrored longitude, roll and clipping
 * included): with WebGL (`hipsGl.ts`) when the webview has it, else on a 2D
 * canvas as affine-clipped triangles. Tiles come through `api.invoke('get_hips_tile')`;
 * the core serves them from the local store or the survey's master.
 *
 * Tile orientation (measured on DSS colored, issue #18): pixel column c and
 * row r from the top-left of a W-px tile sit at cell fraction
 * dx = (r + 0.5) / W, dy = (c + 0.5) / W in cdshealpix's sph_coo convention.
 */

import { api } from '../../api';
import { chartHost, chartSize, onChartRedraw } from './celestialHost';
import { GlTileRenderer, MeshPoint, createGlRenderer } from './hipsGl';
import {
  angularDistance, cellCap, sphCoo, tilePixelSize, visibleCells,
} from './healpix';

export interface HipsSurveyRef {
  id: string;
  mime: string;
  maxOrder: number;
  tileWidth: number;
}

export interface HipsLayerStatus {
  /** Tiles drawn at the wanted order / wanted. */
  drawn: number;
  wanted: number;
  loading: number;
  failed: number;
  /** Duration of the last repaint, ms. */
  paintMs: number;
  /** Triangles drawn by the last repaint. */
  triangles: number;
}

const MAX_IN_FLIGHT = 6;
const MAX_BITMAPS = 150;
const MAX_ABSENT = 5000;
/** A failed tile is retried after this long. */
const RETRY_MS = 30_000;
/** Largest distance, device px, the drawn mesh may stray from the true projection (2D canvas). */
const MESH_TOLERANCE_PX = 1;
/** The same with WebGL, where a finer mesh costs the GPU: twice the cells per side of the 2D mesh. */
const GL_MESH_TOLERANCE_PX = 0.25;
const MAX_MESH = 16;
/**
 * 2D canvas only: while the chart moves (a redraw less than this long ago)
 * every tile is one cell, a few px off but as cheap as two triangles; this
 * long after the last redraw the view is repainted once at
 * `MESH_TOLERANCE_PX`. WebGL draws the fine mesh at every frame.
 */
const SETTLE_MS = 150;
/** Cell fractions where a one-cell tile is compared with the projection: edge midpoints, center, quarter points. */
const MESH_PROBES: [number, number][] = [
  [0.5, 0], [0, 0.5], [1, 0.5], [0.5, 1], [0.5, 0.5], [0.25, 0.25], [0.75, 0.75], [0.25, 0.75], [0.75, 0.25],
];
const MAX_VIEW_RADIUS = Math.PI / 2;

const RAD = Math.PI / 180;

type Key = string;
const keyOf = (order: number, npix: number): Key => `${order}/${npix}`;

let survey: HipsSurveyRef | null = null;
let generation = 0;
let opacity = 1;
let layerCanvas: HTMLCanvasElement | null = null;
/** WebGL renderer of `layerCanvas`; null when the canvas fell back to 2D. */
let glRenderer: GlTileRenderer | null = null;
let unsubscribe: (() => void) | null = null;
let frame = 0;
let statusListener: ((s: HipsLayerStatus) => void) | null = null;

/** Decoded tiles, oldest first (Map keeps insertion order; a hit re-inserts). */
const bitmaps = new Map<Key, ImageBitmap>();
const absent = new Set<Key>();
const failedAt = new Map<Key, number>();
const inFlight = new Set<Key>();
let queue: Array<[number, number]> = [];
let wanted: Array<[number, number]> = [];
let wantedOrder = 0;

export function setHipsStatusListener(fn: ((s: HipsLayerStatus) => void) | null): void {
  statusListener = fn;
}

/** Turns the background on for `next`, or off with null. */
export function setHipsSurvey(next: HipsSurveyRef | null): void {
  if (survey?.id === next?.id) return;
  disposeTiles();
  generation++;
  survey = next;
  if (!next) {
    glRenderer?.dispose();
    glRenderer = null;
    layerCanvas?.remove();
    layerCanvas = null;
    unsubscribe?.();
    unsubscribe = null;
    if (settleTimer) clearTimeout(settleTimer);
    settleTimer = null;
    return;
  }
  if (!unsubscribe) unsubscribe = onChartRedraw(onRedraw);
  scheduleRepaint();
}

/** Tile opacity over the black sky; the canvas itself stays opaque so the page never shows through. */
export function setHipsOpacity(value: number): void {
  opacity = Math.min(1, Math.max(0, value));
  if (survey) scheduleRepaint();
}

function disposeTiles(): void {
  for (const b of bitmaps.values()) b.close();
  bitmaps.clear();
  absent.clear();
  failedAt.clear();
  inFlight.clear();
  queue = [];
  wanted = [];
}

function scheduleRepaint(): void {
  if (!frame) frame = requestAnimationFrame(paint);
}

let lastRedraw = -Infinity;
let settleTimer: ReturnType<typeof setTimeout> | null = null;

/** A chart redraw: a repaint now; on a 2D canvas a coarse one, then a fine one once the chart has been still for `SETTLE_MS`. */
function onRedraw(): void {
  lastRedraw = performance.now();
  scheduleRepaint();
  if (glRenderer) return;
  if (settleTimer) clearTimeout(settleTimer);
  settleTimer = setTimeout(() => {
    settleTimer = null;
    scheduleRepaint();
  }, SETTLE_MS + 10);
}

function ensureCanvas(): HTMLCanvasElement | null {
  const host = chartHost();
  const chartCanvas = host.querySelector('canvas:not(.hips-layer)');
  if (!chartCanvas) return null;
  if (!layerCanvas) {
    layerCanvas = document.createElement('canvas');
    layerCanvas.className = 'hips-layer';
    Object.assign(layerCanvas.style, {
      position: 'absolute', top: '0', left: '0', pointerEvents: 'none',
    });
    glRenderer = createGlRenderer(layerCanvas, scheduleRepaint);
    if (!glRenderer) console.info('[SkyChart] HiPS layer draws on a 2D canvas (no WebGL)');
  }
  // Under the chart canvas: d3-celestial's canvas is position:relative with
  // z-index 0, so a preceding absolutely positioned sibling paints below it.
  if (layerCanvas.nextSibling !== chartCanvas) host.insertBefore(layerCanvas, chartCanvas);
  return layerCanvas;
}

/** Smallest order whose tile pixel is no larger than a device pixel at the view center, capped. */
function chooseOrder(scale: number, dpr: number, s: HipsSurveyRef): number {
  // Stereographic: dθ/dρ = 2 / scale radians per CSS px at the center.
  const devicePx = 2 / (scale * dpr);
  for (let o = 0; o < s.maxOrder; o++) {
    if (tilePixelSize(o, s.tileWidth) <= devicePx) return o;
  }
  return s.maxOrder;
}

let triangles = 0;

function paint(): void {
  frame = 0;
  const started = performance.now();
  triangles = 0;
  const C = window.Celestial;
  const s = survey;
  if (!s || !C?.mapProjection) return;
  const canvas = ensureCanvas();
  if (!canvas) return;
  const { width, height } = chartSize();
  const dpr = window.devicePixelRatio || 1;
  if (canvas.width !== Math.round(width * dpr) || canvas.height !== Math.round(height * dpr)) {
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
  }
  const gl = glRenderer;
  if (gl?.lost) return;
  const ctx = gl ? null : canvas.getContext('2d');
  if (!gl && !ctx) return;
  const moving = !gl && performance.now() - lastRedraw < SETTLE_MS;
  if (gl) {
    gl.begin(canvas.width, canvas.height);
  } else if (ctx) {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
  }

  const projection = C.mapProjection;
  const scale: number = projection.scale();
  const center = C.rotate() as number[];
  const viewCenter: [number, number] = [center[0] * RAD, center[1] * RAD];
  const halfDiagonal = Math.hypot(width, height) / 2;
  const viewRadius = Math.min(MAX_VIEW_RADIUS, 2 * Math.atan(halfDiagonal / scale));

  const order = chooseOrder(scale, dpr, s);
  const cells = visibleCells(order, viewCenter, viewRadius);
  wanted = cells
    .map((n) => [order, n] as [number, number])
    .sort((a, b) => angularDistance(cellCap(a[0], a[1]).center, viewCenter)
      - angularDistance(cellCap(b[0], b[1]).center, viewCenter));
  wantedOrder = order;

  const project = (lonRad: number, latRad: number): [number, number] | null => {
    if (angularDistance([lonRad, latRad], viewCenter) >= MAX_VIEW_RADIUS) return null;
    const p = projection([lonRad / RAD, latRad / RAD]);
    return p && Number.isFinite(p[0]) && Number.isFinite(p[1]) ? [p[0] * dpr, p[1] * dpr] : null;
  };

  const draw = (bmp: ImageBitmap, o: number, n: number) => {
    if (gl) {
      const mesh = meshSize(o, n, project, GL_MESH_TOLERANCE_PX);
      triangles += gl.drawTile(bmp, mesh, meshPoints(o, n, mesh, project), opacity);
    } else if (ctx) {
      drawTile(ctx, bmp, o, n, project, moving);
    }
  };

  // Ancestors first (a stand-in while the wanted tile loads), then the wanted tiles on top.
  const drawnAncestors = new Set<Key>();
  let drawn = 0;
  for (const [o, n] of wanted) {
    if (bitmaps.has(keyOf(o, n))) continue;
    for (let ao = o - 1, an = n >> 2; ao >= 0; ao--, an >>= 2) {
      const k = keyOf(ao, an);
      const bmp = bitmaps.get(k);
      if (bmp) {
        if (!drawnAncestors.has(k)) {
          drawnAncestors.add(k);
          draw(bmp, ao, an);
        }
        break;
      }
    }
  }
  for (const [o, n] of wanted) {
    const k = keyOf(o, n);
    const bmp = bitmaps.get(k);
    if (bmp) {
      bitmaps.delete(k);
      bitmaps.set(k, bmp);
      draw(bmp, o, n);
      drawn++;
    } else if (absent.has(k)) {
      drawn++;
    }
  }

  // Opacity on a 2D canvas is a black veil over opaque tiles: blending each
  // grown triangle at partial alpha would light up the seams between them.
  // WebGL scales the tile colour instead.
  if (gl) gl.retain(new Set(bitmaps.values()));
  if (ctx && opacity < 1) {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1 - opacity;
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.globalAlpha = 1;
  }

  queue = wanted.filter(([o, n]) => {
    const k = keyOf(o, n);
    if (bitmaps.has(k) || absent.has(k) || inFlight.has(k)) return false;
    const t = failedAt.get(k);
    return t === undefined || Date.now() - t > RETRY_MS;
  });
  pump();
  statusListener?.({
    drawn, wanted: wanted.length, loading: inFlight.size + queue.length, failed: failedAt.size,
    paintMs: performance.now() - started, triangles,
  });
}

function pump(): void {
  const s = survey;
  if (!s) return;
  while (inFlight.size < MAX_IN_FLIGHT && queue.length) {
    const [order, npix] = queue.shift()!;
    const k = keyOf(order, npix);
    inFlight.add(k);
    const gen = generation;
    loadTile(s, order, npix)
      .then((bmp) => {
        if (gen !== generation) {
          bmp?.close();
          return;
        }
        failedAt.delete(k);
        if (bmp) {
          bitmaps.set(k, bmp);
          evict();
        } else {
          absent.add(k);
          if (absent.size > MAX_ABSENT) absent.delete(absent.values().next().value!);
        }
      })
      .catch((err) => {
        if (gen !== generation) return;
        console.error(`[SkyChart] HiPS tile ${k} failed:`, err);
        failedAt.set(k, Date.now());
      })
      .finally(() => {
        if (gen !== generation) return;
        inFlight.delete(k);
        scheduleRepaint();
      });
  }
}

/** Drops the least recently drawn bitmaps beyond the cap, never one in the current view. */
function evict(): void {
  if (bitmaps.size <= MAX_BITMAPS) return;
  const keep = new Set(wanted.map(([o, n]) => keyOf(o, n)));
  for (const [k, b] of bitmaps) {
    if (bitmaps.size <= MAX_BITMAPS) break;
    if (keep.has(k)) continue;
    b.close();
    bitmaps.delete(k);
  }
}

async function loadTile(s: HipsSurveyRef, order: number, npix: number): Promise<ImageBitmap | null> {
  const payload = await api.invoke<ArrayBuffer | Uint8Array>('get_hips_tile', { surveyId: s.id, order, npix });
  const bytes = payload instanceof Uint8Array ? payload : new Uint8Array(payload);
  if (bytes.byteLength === 0) return null;
  return createImageBitmap(new Blob([bytes as BlobPart], { type: s.mime }));
}

type TileProjector = (lon: number, lat: number) => [number, number] | null;

/**
 * Mesh cells per tile side, from the tile's own bend on screen: the
 * distance between the projection and the tile drawn as one cell (two
 * triangles, split as `drawTile` splits them) at `MESH_PROBES`. That error
 * falls with the square of the cell size, so n = ⌈√(error / tolerance)⌉.
 * Most tiles of a 12° view need 1 or 2; the 3.7° tile under M 31 (4.3 px
 * as one cell) needs 3. A tile with a point that does not project gets
 * `MAX_MESH`.
 */
export function meshSize(order: number, npix: number, project: TileProjector, tolerancePx = MESH_TOLERANCE_PX): number {
  const at = (dx: number, dy: number) => {
    const [lon, lat] = sphCoo(order, npix, Math.min(dx, 1 - 1e-12), Math.min(dy, 1 - 1e-12));
    return project(lon, lat);
  };
  const p00 = at(0, 0);
  const p10 = at(1, 0);
  const p01 = at(0, 1);
  const p11 = at(1, 1);
  if (!p00 || !p10 || !p01 || !p11) return MAX_MESH;
  let error = 0;
  for (const [s, t] of MESH_PROBES) {
    const truth = at(s, t);
    if (!truth) return MAX_MESH;
    const drawn = s + t <= 1
      ? [0, 1].map((k) => p00[k] + s * (p10[k] - p00[k]) + t * (p01[k] - p00[k]))
      : [0, 1].map((k) => p11[k] + (1 - s) * (p01[k] - p11[k]) + (1 - t) * (p10[k] - p11[k]));
    error = Math.max(error, Math.hypot(drawn[0] - truth[0], drawn[1] - truth[1]));
  }
  return Math.min(MAX_MESH, Math.max(1, Math.ceil(Math.sqrt(error / tolerancePx))));
}

/** The (n + 1)² projected mesh points of a tile; point (a, b) is the cell fraction dx = a / n, dy = b / n. */
function meshPoints(order: number, npix: number, n: number, project: TileProjector): MeshPoint[] {
  const pts: MeshPoint[] = [];
  for (let a = 0; a <= n; a++) {
    for (let b = 0; b <= n; b++) {
      const [lon, lat] = sphCoo(order, npix, Math.min(a / n, 1 - 1e-12), Math.min(b / n, 1 - 1e-12));
      pts.push(project(lon, lat));
    }
  }
  return pts;
}

/**
 * Draws one tile as an n×n mesh (one cell while the chart moves). Mesh point (a, b) is the cell fraction
 * dx = a / n, dy = b / n; its texture point is column dy·W, row dx·W.
 */
function drawTile(
  ctx: CanvasRenderingContext2D,
  bmp: ImageBitmap,
  order: number,
  npix: number,
  project: TileProjector,
  moving: boolean,
): void {
  const n = moving ? 1 : meshSize(order, npix, project);
  const w = bmp.width;
  const pts = meshPoints(order, npix, n, project);
  const at = (a: number, b: number) => pts[a * (n + 1) + b];
  const tex = (a: number, b: number): [number, number] => [(b / n) * w, (a / n) * w];
  for (let a = 0; a < n; a++) {
    for (let b = 0; b < n; b++) {
      const p00 = at(a, b);
      const p10 = at(a + 1, b);
      const p01 = at(a, b + 1);
      const p11 = at(a + 1, b + 1);
      // Split along the anti-diagonal: at z = ±2/3 HEALPix turns from the
      // equatorial to the polar projection, a kink that runs along
      // dx + dy = const in every tile it crosses (M 31 and M 110 sit on it).
      // Splitting along dx = dy instead bent the image across the kink by up
      // to 98 px on a 3.7° tile.
      if (p00 && p10 && p01) drawTriangle(ctx, bmp, [p00, p10, p01], [tex(a, b), tex(a + 1, b), tex(a, b + 1)]);
      if (p11 && p01 && p10) drawTriangle(ctx, bmp, [p11, p01, p10], [tex(a + 1, b + 1), tex(a, b + 1), tex(a + 1, b)]);
    }
  }
}

/** Maps texture triangle `t` onto screen triangle `p` (both 3 points) and draws it, grown by ~0.5 px against seams. */
export function drawTriangle(
  ctx: CanvasRenderingContext2D,
  img: CanvasImageSource,
  p: [number, number][],
  t: [number, number][],
): void {
  const m = triangleAffine(p, t);
  if (!m) return;
  triangles++;
  const cx = (p[0][0] + p[1][0] + p[2][0]) / 3;
  const cy = (p[0][1] + p[1][1] + p[2][1]) / 3;
  ctx.save();
  ctx.beginPath();
  p.forEach(([x, y], i) => {
    const d = Math.hypot(x - cx, y - cy) || 1;
    const gx = x + ((x - cx) / d) * 0.5;
    const gy = y + ((y - cy) / d) * 0.5;
    if (i === 0) ctx.moveTo(gx, gy);
    else ctx.lineTo(gx, gy);
  });
  ctx.closePath();
  ctx.clip();
  ctx.setTransform(m[0], m[1], m[2], m[3], m[4], m[5]);
  // Only the texture rectangle under this triangle (+1 px for the grown
  // edge): drawing the whole tile per triangle costs the full tile's pixels.
  const size = textureSize(img);
  const sx = Math.max(0, Math.floor(Math.min(t[0][0], t[1][0], t[2][0])) - 1);
  const sy = Math.max(0, Math.floor(Math.min(t[0][1], t[1][1], t[2][1])) - 1);
  const ex = Math.min(size, Math.ceil(Math.max(t[0][0], t[1][0], t[2][0])) + 1);
  const ey = Math.min(size, Math.ceil(Math.max(t[0][1], t[1][1], t[2][1])) + 1);
  if (ex > sx && ey > sy) ctx.drawImage(img, sx, sy, ex - sx, ey - sy, sx, sy, ex - sx, ey - sy);
  ctx.restore();
}

function textureSize(img: CanvasImageSource): number {
  return (img as { width?: number }).width ?? 0;
}

/**
 * The affine transform [a, b, c, d, e, f] (canvas `setTransform` order)
 * taking texture points `t` to screen points `p`; null when degenerate.
 */
export function triangleAffine(p: [number, number][], t: [number, number][]): number[] | null {
  const [[u0, v0], [u1, v1], [u2, v2]] = t;
  const [[x0, y0], [x1, y1], [x2, y2]] = p;
  const du1 = u1 - u0;
  const dv1 = v1 - v0;
  const du2 = u2 - u0;
  const dv2 = v2 - v0;
  const det = du1 * dv2 - du2 * dv1;
  if (Math.abs(det) < 1e-12) return null;
  const dx1 = x1 - x0;
  const dy1 = y1 - y0;
  const dx2 = x2 - x0;
  const dy2 = y2 - y0;
  const a = (dx1 * dv2 - dx2 * dv1) / det;
  const b = (dy1 * dv2 - dy2 * dv1) / det;
  const c = (dx2 * du1 - dx1 * du2) / det;
  const d = (dy2 * du1 - dy1 * du2) / det;
  const e = x0 - a * u0 - c * v0;
  const f = y0 - b * u0 - d * v0;
  return [a, b, c, d, e, f];
}

/** For tests: the order the layer last asked for. */
export const lastWantedOrder = () => wantedOrder;
