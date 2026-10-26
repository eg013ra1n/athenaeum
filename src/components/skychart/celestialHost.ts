/**
 * The one d3-celestial chart of the app session.
 *
 * d3-celestial is a global singleton whose `display()` re-fetches every data
 * file and whose added objects (`Celestial.add`) can never be removed one by
 * one. So the chart is displayed once, into a host element this module owns;
 * the Sky Chart page attaches the host on mount and detaches it on unmount.
 * Canvas, data, projection and zoom survive navigation untouched, and the
 * redraw hook below is registered exactly once.
 *
 * Library patches this module relies on: docs/skychart/README.md.
 */

import { api } from '../../api';
import {
  SkyView, clampFov, fovToScale, normalizeRa, scaleToFov, viewsDiffer,
} from './skyView';

declare global {
  interface Window {
    Celestial: any;
    d3: any;
  }
}

export const VIEW_SETTING_KEY = 'skychart.view';
export const STAR_LIMIT_SETTING_KEY = 'skychart.star_limit';
export const STAR_LIMITS = [4, 6, 8, 14] as const;

const HOST_ID = 'celestial-map';
const SAVE_DEBOUNCE_MS = 600;

let hostEl: HTMLDivElement | null = null;
let displayed = false;
let size = { width: 0, height: 0 };
let starLimit = 6;
let lastSaved: SkyView | null = null;
let saveTimer: number | undefined;
const redrawListeners = new Set<() => void>();

export function celestialAvailable(): boolean {
  return typeof window !== 'undefined' && typeof window.Celestial !== 'undefined';
}

export function starDataFile(limit: number): string {
  return limit > 8 ? 'stars.14.json' : 'stars.8.json';
}

/** The persistent `#celestial-map` element; created on first use. */
export function chartHost(): HTMLDivElement {
  if (!hostEl) {
    hostEl = document.createElement('div');
    hostEl.id = HOST_ID;
  }
  return hostEl;
}

export function attachChart(parent: HTMLElement): void {
  const el = chartHost();
  if (el.parentNode !== parent) parent.appendChild(el);
}

export function detachChart(parent: HTMLElement): void {
  if (hostEl && hostEl.parentNode === parent) parent.removeChild(hostEl);
}

export const isChartDisplayed = (): boolean => displayed;
export const chartSize = (): { width: number; height: number } => size;
export const currentStarLimit = (): number => starLimit;

/** Subscribes to every chart redraw (pan, zoom, resize, data load). */
export function onChartRedraw(fn: () => void): () => void {
  redrawListeners.add(fn);
  return () => redrawListeners.delete(fn);
}

export interface ChartPrefs {
  starLimit: number;
  view: SkyView | null;
}

let prefsPromise: Promise<ChartPrefs> | null = null;

/** Star limit and last view from settings; read once per app session. */
export function loadChartPrefs(parseView: (raw: string) => SkyView | null): Promise<ChartPrefs> {
  if (!prefsPromise) {
    const limit = api.invoke<string>('get_setting', { key: STAR_LIMIT_SETTING_KEY, defaultValue: '6' })
      .then((v) => {
        const n = parseFloat(v);
        return (STAR_LIMITS as readonly number[]).includes(n) ? n : 6;
      })
      .catch((err) => {
        console.error('[SkyChart] loading the star limit failed:', err);
        return 6;
      });
    const view = api.invoke<string>('get_setting', { key: VIEW_SETTING_KEY, defaultValue: '' })
      .then(parseView)
      .catch((err) => {
        console.error('[SkyChart] loading the saved view failed:', err);
        return null;
      });
    prefsPromise = Promise.all([limit, view]).then(([l, v]) => ({ starLimit: l, view: v }));
  }
  return prefsPromise;
}

/** Displays the chart once, sized to the host's slot, at `view`. */
export function displayChart(width: number, height: number, limit: number, view: SkyView): void {
  if (displayed) return;
  const C = window.Celestial;
  size = { width, height };
  starLimit = limit;
  C.display(buildConfig(width, height, limit, view));
  displayed = true;
  C.add({
    type: 'raw',
    callback: () => {},
    redraw: () => {
      for (const fn of redrawListeners) {
        try {
          fn();
        } catch (err) {
          console.error('[SkyChart] redraw listener failed:', err);
        }
      }
      scheduleSave();
    },
  });
  applyView({ fovDeg: view.fovDeg });
  lastSaved = readView();
  watchPixelRatio();
}

/** The current view, or null before the chart is displayed. */
export function readView(): SkyView | null {
  if (!displayed) return null;
  const C = window.Celestial;
  const center = C.rotate();
  const scale = C.mapProjection.scale();
  return {
    ra: normalizeRa(center[0]),
    dec: center[1],
    roll: center[2] ?? 0,
    fovDeg: scaleToFov(size.width, scale),
  };
}

/**
 * Moves the chart: the center (with roll) when ra and dec are given, the
 * field of view when fovDeg is given. Instant; the chart runs with
 * animations off so a restored or linked view never flies in.
 */
export function applyView(view: Partial<SkyView>): void {
  if (!displayed) return;
  const C = window.Celestial;
  if (view.ra !== undefined && view.dec !== undefined) {
    const roll = view.roll ?? (C.rotate()[2] ?? 0);
    C.rotate({ center: [view.ra, view.dec, roll] });
  }
  if (view.fovDeg !== undefined) {
    const target = fovToScale(size.width, clampFov(view.fovDeg));
    const factor = target / C.mapProjection.scale();
    if (Math.abs(factor - 1) > 1e-6) C.zoomBy(factor);
  }
}

/**
 * Resizes the canvas and keeps the view: d3-celestial's resize resets the
 * zoom to its base scale, so the field of view is re-applied right after,
 * before the browser paints.
 */
export function resizeChart(width: number, height: number, force = false): void {
  if (!displayed || width <= 0 || height <= 0) return;
  if (!force && width === size.width && height === size.height) return;
  const view = readView();
  window.Celestial.resize({ width, projectionRatio: width / height });
  size = { width, height };
  if (view) applyView({ fovDeg: view.fovDeg });
}

/** Swaps the star catalog; the view is untouched. */
export function setStarLimit(limit: number): void {
  if (!displayed || limit === starLimit) return;
  starLimit = limit;
  window.Celestial.reload({
    stars: { limit, data: starDataFile(limit) },
    // reload() re-applies the config d3-celestial last stored, which holds
    // the size the chart was displayed at; pass the current one.
    width: size.width,
    projectionRatio: size.width / size.height,
  });
}

function scheduleSave(): void {
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    const view = readView();
    if (!view || (lastSaved && !viewsDiffer(view, lastSaved))) return;
    lastSaved = view;
    api.invoke('set_setting', { key: VIEW_SETTING_KEY, value: JSON.stringify(view) })
      .catch((err) => console.error('[SkyChart] saving the view failed:', err));
  }, SAVE_DEBOUNCE_MS);
}

let pixelRatio = 1;

/**
 * Re-sizes the canvas backing store when the window moves to a screen with
 * another pixel ratio. The resolution media query is the primary signal; a
 * window resize re-checks too, for webviews that only report the move that way.
 */
function watchPixelRatio(): void {
  pixelRatio = window.devicePixelRatio || 1;
  window.addEventListener('resize', checkPixelRatio);
  listenResolution();
}

function listenResolution(): void {
  if (typeof window.matchMedia !== 'function') return;
  window.matchMedia(`(resolution: ${pixelRatio}dppx)`)
    .addEventListener('change', () => { checkPixelRatio(); listenResolution(); }, { once: true });
}

function checkPixelRatio(): void {
  const dpr = window.devicePixelRatio || 1;
  if (dpr === pixelRatio) return;
  pixelRatio = dpr;
  resizeChart(size.width, size.height, true);
}

function buildConfig(width: number, height: number, limit: number, view: SkyView) {
  return {
    container: HOST_ID,
    width,
    projectionRatio: width / height,
    projection: 'stereographic',
    transform: 'equatorial',
    center: [view.ra, view.dec, view.roll],
    disableAnimations: true,
    orientationfixed: false,
    follow: 'center',
    zoomlevel: null,
    zoomextend: 20,
    adaptable: true,
    interactive: true,
    form: false,
    controls: true,
    datapath: '/data/',
    stars: {
      show: true,
      limit,
      colors: true,
      style: { fill: '#ffffff', opacity: 0.8 },
      designation: true,
      designationLimit: 2.5,
      propername: true,
      propernameLimit: 1.5,
      size: 7,
      exponent: -0.28,
      data: starDataFile(limit),
    },
    dsos: {
      show: true,
      limit: 6,
      colors: true,
      style: { fill: '#cccccc', opacity: 1 },
      names: true,
      namesLimit: 6,
      size: null,
      exponent: 1.4,
      data: 'dsos.bright.json',
    },
    constellations: {
      show: true,
      names: true,
      namesType: 'name',
      nameStyle: { fill: '#cccc99', font: '14px Helvetica, Arial, sans-serif', align: 'center', baseline: 'middle' },
      lines: true,
      lineStyle: { stroke: '#cccccc', width: 1, opacity: 0.4 },
      bounds: false,
      boundStyle: { stroke: '#cccc00', width: 0.5, opacity: 0.8, dash: [2, 4] },
    },
    mw: {
      show: true,
      style: { fill: '#ffffff', opacity: 0.15 },
    },
    lines: {
      graticule: {
        show: true,
        stroke: '#cccccc',
        width: 0.6,
        opacity: 0.3,
        lon: { pos: [''], fill: '#eee', font: '10px Helvetica, Arial, sans-serif' },
        lat: { pos: [''], fill: '#eee', font: '10px Helvetica, Arial, sans-serif' },
      },
      equatorial: { show: true, stroke: '#aaaaaa', width: 1.3, opacity: 0.7 },
      ecliptic: { show: false, stroke: '#66cc66', width: 1.3, opacity: 0.7 },
      galactic: { show: false, stroke: '#cc6666', width: 1.3, opacity: 0.7 },
      supergalactic: { show: false, stroke: '#cc66cc', width: 1.3, opacity: 0.7 },
    },
    background: {
      fill: '#000000',
      opacity: 1,
      stroke: '#000000',
      // 0, not d3-celestial's default: a non-zero width makes the canvas
      // that many px wider than the chart and shifts the projection center.
      width: 0,
    },
    horizon: {
      show: false,
    },
  };
}

// The chart is a session singleton; a hot update of this module would
// display a second one. Reload the page instead.
if (import.meta.hot) import.meta.hot.accept(() => window.location.reload());
