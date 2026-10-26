/**
 * HEALPix NESTED geometry for HiPS tiles: a port of cdshealpix 0.9.1's
 * `nested::sph_coo` (the crate athenaeum-core uses), pinned to it by golden
 * vectors (`healpix.golden.json`, written by the core test
 * `hips::golden::healpix_golden_vectors`).
 *
 * Orders up to 12 keep every bit operation inside 31 bits.
 */

export const MAX_GEOMETRY_ORDER = 12;

const EPS_POLE = 1e-13;
const SQRT6 = 2.449489742783178;
const FRAC_PI_4 = Math.PI / 4;

/** Number of cells at `order`. */
export function cellCount(order: number): number {
  return 12 * 4 ** order;
}

function deinterleave(v: number, bits: number): [number, number] {
  let i = 0;
  let j = 0;
  for (let b = 0; b < bits; b++) {
    i |= ((v >>> (2 * b)) & 1) << b;
    j |= ((v >>> (2 * b + 1)) & 1) << b;
  }
  return [i, j];
}

function unproj(x: number, y: number): [number, number] {
  const xNeg = x < 0 || Object.is(x, -0);
  const yNeg = y < 0 || Object.is(y, -0);
  const ax = Math.abs(x);
  const ay = Math.abs(y);
  const odd = Math.trunc(ax) | 1;
  const offset = odd & 7;
  let lon = ax - odd;
  let lat: number;
  if (ay <= 1) {
    lat = Math.asin(ay * (2 / 3));
  } else {
    const l = 2 - ay;
    if (l > EPS_POLE) lon = Math.min(1, Math.max(-1, lon / l));
    lat = 2 * Math.acos(l / SQRT6) - Math.PI / 2;
  }
  lon += offset;
  if (xNeg) lon = -lon;
  if (yNeg) lat = -lat;
  return [lon * FRAC_PI_4, lat];
}

/**
 * Position on the sphere of the point (dx, dy) ∈ [0, 1]² inside cell `npix`
 * at `order`: (0.5, 0.5) is the center, dx runs from the south vertex to the
 * east one, dy from the south vertex to the west one. Returns [lon, lat] in
 * radians, lon in [0, 2π).
 */
export function sphCoo(order: number, npix: number, dx: number, dy: number): [number, number] {
  const nside = 2 ** order;
  const d0h = Math.floor(npix / 4 ** order);
  const inCell = npix - d0h * 4 ** order;
  const [i, j] = deinterleave(inCell, order);
  let x = (i - j) / nside;
  let y = (i + j - (nside - 1)) / nside;
  const offY = 1 - (d0h >> 2);
  const offX = ((d0h & 3) << 1) | (offY & 1);
  x += offX;
  y += offY;
  if (x < 0) x += 8;
  x += (dx - dy) / nside;
  y += (dx + dy - 1) / nside;
  if (x < 0) x += 8;
  return unproj(x, y);
}

/** The four children of a cell at the next order. */
export function children(npix: number): [number, number, number, number] {
  const c = npix * 4;
  return [c, c + 1, c + 2, c + 3];
}

/** Angular distance between two [lon, lat] points in radians. */
export function angularDistance(a: [number, number], b: [number, number]): number {
  const s = Math.sin((b[1] - a[1]) / 2) ** 2
    + Math.cos(a[1]) * Math.cos(b[1]) * Math.sin((b[0] - a[0]) / 2) ** 2;
  return 2 * Math.asin(Math.min(1, Math.sqrt(s)));
}

export interface CellCap {
  center: [number, number];
  /** Radius (radians) of a cap around `center` that contains the whole cell. */
  radius: number;
}

const capCache = new Map<string, CellCap>();

/**
 * A cap containing the cell: the farthest of its vertices and edge
 * midpoints, plus 5 % for the curved edges in between.
 */
export function cellCap(order: number, npix: number): CellCap {
  const key = `${order}/${npix}`;
  const hit = capCache.get(key);
  if (hit) return hit;
  const center = sphCoo(order, npix, 0.5, 0.5);
  let radius = 0;
  for (const [dx, dy] of [[0, 0], [1, 0], [0, 1], [1, 1], [0.5, 0], [0, 0.5], [1, 0.5], [0.5, 1]] as const) {
    radius = Math.max(radius, angularDistance(center, sphCoo(order, npix, Math.min(dx, 1 - 1e-12), Math.min(dy, 1 - 1e-12))));
  }
  const cap = { center, radius: radius * 1.05 };
  if (capCache.size > 20000) capCache.clear();
  capCache.set(key, cap);
  return cap;
}

/**
 * Cells at `order` that may intersect a view cap (center [lon, lat] and
 * radius in radians): a walk down from order 0 that keeps every child
 * whose cap touches the view.
 */
export function visibleCells(order: number, viewCenter: [number, number], viewRadius: number): number[] {
  let level = Array.from({ length: 12 }, (_, i) => i);
  for (let o = 0; ; o++) {
    level = level.filter((n) => {
      const cap = cellCap(o, n);
      return angularDistance(cap.center, viewCenter) <= viewRadius + cap.radius;
    });
    if (o === order) return level;
    level = level.flatMap(children);
  }
}

/** Angular size (radians) of one pixel of a `tileWidth`-px tile at `order`. */
export function tilePixelSize(order: number, tileWidth: number): number {
  return Math.sqrt((4 * Math.PI) / 12) / 2 ** order / tileWidth;
}
