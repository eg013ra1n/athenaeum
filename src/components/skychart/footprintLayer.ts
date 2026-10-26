/**
 * The frame-set footprint layer: an SVG over the chart canvas, kept inside
 * the persistent chart host, so it survives navigation with the chart.
 *
 * `setFootprints` rebuilds the elements when the data or the date filter
 * changes; `redrawFootprints` (one chart redraw hook) moves them on every
 * pan and zoom. Each box is projected once per redraw and the label pass
 * reuses that projection.
 */

import type { ImagingLocation } from '../../types/models';
import { chartHost, chartSize, onChartRedraw } from './celestialHost';
import {
  FootprintHidden, LonLat, Point, footprintCorners, pathData, projectFootprint,
} from './footprint';

export interface FootprintHandlers {
  /** Double-click on a footprint that belongs to a frame set. */
  openFrameSet: (frameSetId: number) => void;
  /** The selected target's visibility after a redraw (null = drawn). */
  onSelectedHidden: (hidden: FootprintHidden) => void;
}

interface FootprintDatum {
  loc: ImagingLocation;
  /** Point the marker sits on, in d3-celestial's equatorial coordinates. */
  center: LonLat;
  /** Sky corners when the location has a field of view. */
  corners: LonLat[] | null;
  points: Point[];
  hidden: FootprintHidden;
  shown: boolean;
}

const FOUR_POINTED_STAR = 'M0,-10 L2,-2 L10,0 L2,2 L0,10 L-2,2 L-10,0 L-2,-2 Z';
const SPARKLE = 'M0,-6 L1,-1 L6,0 L1,1 L0,6 L-1,1 L-6,0 L-1,-1 Z';
const CLIP_ID = 'markers-globe-clip';

const LABEL_FONT_SIZE = 11;
const LABEL_CHAR_W = 6.2; // approx average char width at 11px Helvetica
const LABEL_PAD_X = 6;
const LABEL_PAD_Y = 3;
const LABEL_OFFSET_Y = 4; // gap above the FOV box

let handlers: FootprintHandlers | null = null;
let selectedId: number | null = null;
let lastSelectedHidden: FootprintHidden | undefined;
let unsubscribe: (() => void) | null = null;

export function setFootprintHandlers(h: FootprintHandlers | null): void {
  handlers = h;
  lastSelectedHidden = undefined;
}

export function setSelectedFootprint(id: number | null): void {
  selectedId = id;
  lastSelectedHidden = undefined;
  redrawFootprints();
}

function markerColor(loc: ImagingLocation): string {
  if (loc.locationType === 'cluster') return '#22c55e';
  return loc.isCustom ? '#ef4444' : '#3b82f6';
}

function markerStroke(loc: ImagingLocation): string {
  if (loc.locationType === 'cluster') return '#16a34a';
  return loc.isCustom ? '#dc2626' : '#2563eb';
}

function hexToRgba(hex: string, alpha: number): string {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

function tooltip(loc: ImagingLocation, hasFov: boolean): string {
  const hours = (loc.totalExposure / 3600).toFixed(2);
  let type = '[Unorganized]';
  if (loc.locationType === 'frameset') type = loc.isCustom ? '[Custom Frame Set]' : '[Auto Frame Set]';
  const cameras = loc.cameras ? `\nCameras: ${loc.cameras}` : '';
  const focal = loc.focalLengths ? `\nFocal Lengths: ${loc.focalLengths}mm` : '';
  const dates = loc.dateRange?.[0] && loc.dateRange?.[1]
    ? `\nDates: ${loc.dateRange[0].split('T')[0]} to ${loc.dateRange[1].split('T')[0]}`
    : '';
  const fov = hasFov ? `\nFOV: ${loc.fovWidth!.toFixed(2)}° × ${loc.fovHeight!.toFixed(2)}°` : '';
  const name = loc.objectName || '[No Name]';
  return `${type} ${name}\nFrames: ${loc.frameCount}\nExposure: ${hours}h\nFilters: ${loc.filters.join(', ')}${cameras}${focal}${dates}${fov}`;
}

/** The overlay SVG and its marker group, created on first use. */
function layerGroup(): any {
  const d3 = window.d3;
  const host = d3.select(chartHost());
  let svg = host.select('svg.imaging-markers-overlay');
  if (svg.empty()) {
    svg = host.append('svg')
      .attr('class', 'imaging-markers-overlay')
      .style('position', 'absolute')
      .style('top', '0')
      .style('left', '0')
      .style('width', '100%')
      .style('height', '100%')
      .style('pointer-events', 'none');
    svg.append('defs').append('clipPath').attr('id', CLIP_ID).append('path');
    svg.append('g')
      .attr('class', 'imaging-markers-layer')
      .attr('clip-path', `url(#${CLIP_ID})`)
      .style('pointer-events', 'auto');
  }
  return svg.select('g.imaging-markers-layer');
}

/** Rebuilds the footprint elements for `locations`; an empty list clears them. */
export function setFootprints(locations: ImagingLocation[]): void {
  const d3 = window.d3;
  const C = window.Celestial;
  if (!d3 || !C) return;
  if (!unsubscribe) unsubscribe = onChartRedraw(redrawFootprints);

  const group = layerGroup();
  group.selectAll('.fov-box').remove();

  const valid = locations.filter((l) => Number.isFinite(l.ra) && Number.isFinite(l.dec));
  const geo = C.getData({
    type: 'FeatureCollection',
    features: valid.map((l) => ({ type: 'Feature', id: l.id, properties: {}, geometry: { type: 'Point', coordinates: [l.ra, l.dec] } })),
  }, C.settings().transform);

  const data: FootprintDatum[] = valid.map((loc, i) => {
    const hasFov = !!(loc.fovWidth && loc.fovHeight);
    return {
      loc,
      center: geo.features[i].geometry.coordinates as LonLat,
      corners: hasFov ? footprintCorners(loc.ra, loc.dec, loc.fovWidth!, loc.fovHeight!, loc.rotation ?? 0) : null,
      points: [],
      hidden: 'clip',
      shown: true,
    };
  });

  group.selectAll('.fov-box')
    .data(data)
    .enter().append('g')
    .attr('class', 'fov-box')
    // The group is transparent to pointer events so wheel/zoom reaches the
    // canvas; only the painted shapes below take clicks and hover.
    .style('pointer-events', 'none')
    .each(function (this: SVGGElement, d: FootprintDatum) {
      const g = d3.select(this);
      const color = markerColor(d.loc);
      const clickable = d.loc.frameSetId != null;
      if (d.corners) {
        if (clickable) {
          // Wide transparent stroke + fill: the whole box and its border are a click target.
          g.append('path')
            .attr('class', 'fov-hit')
            .style('fill', 'transparent')
            .style('stroke', 'transparent')
            .style('stroke-width', '14px')
            .style('pointer-events', 'all')
            .style('cursor', 'pointer');
        }
        g.append('path')
          .attr('class', 'fov-rect')
          .style('fill', hexToRgba(color, 0.28))
          .style('stroke', color)
          .style('stroke-width', '2px')
          .style('pointer-events', 'none');
        const label = g.append('g')
          .attr('class', 'fov-label')
          .style('pointer-events', clickable ? 'visiblePainted' : 'none')
          .style('cursor', clickable ? 'pointer' : 'default')
          .style('display', 'none');
        label.append('rect')
          .attr('class', 'fov-label-bg')
          .attr('rx', 3)
          .attr('ry', 3)
          .style('fill', 'rgba(0,0,0,0.7)')
          .style('stroke', color)
          .style('stroke-width', '1px');
        label.append('text')
          .attr('class', 'fov-label-text')
          .attr('text-anchor', 'middle')
          .attr('dominant-baseline', 'middle')
          .style('font-size', `${LABEL_FONT_SIZE}px`)
          .style('font-family', 'Helvetica, Arial, sans-serif')
          .style('fill', '#ffffff')
          .style('user-select', 'none');
      } else {
        g.append('path')
          .attr('class', 'imaging-marker')
          .attr('d', d.loc.locationType === 'cluster' ? SPARKLE : FOUR_POINTED_STAR)
          .style('fill', color)
          .style('stroke', markerStroke(d.loc))
          .style('stroke-width', '2px')
          .style('pointer-events', 'visiblePainted')
          .style('cursor', clickable ? 'pointer' : 'default');
      }

      // Double-click opens the frame set (single click stays free for panning).
      g.on('dblclick', () => {
        const evt = d3.event;
        if (evt) {
          evt.stopPropagation();
          evt.preventDefault();
        }
        if (d.loc.frameSetId != null) handlers?.openFrameSet(d.loc.frameSetId);
      });

      // Wheel over a footprint still zooms: re-dispatch on the canvas, where
      // d3-celestial's zoom behaviour listens (d3 v3 puts the event on d3.event).
      g.on('wheel', () => {
        const evt = d3.event as WheelEvent | undefined;
        const canvas = chartHost().querySelector('canvas');
        if (!canvas || !evt) return;
        evt.preventDefault();
        evt.stopPropagation();
        canvas.dispatchEvent(new WheelEvent('wheel', {
          bubbles: true,
          cancelable: true,
          deltaX: evt.deltaX,
          deltaY: evt.deltaY,
          deltaZ: evt.deltaZ,
          deltaMode: evt.deltaMode,
          clientX: evt.clientX,
          clientY: evt.clientY,
          ctrlKey: evt.ctrlKey,
          shiftKey: evt.shiftKey,
          altKey: evt.altKey,
          metaKey: evt.metaKey,
        }));
      });

      g.append('title').text(tooltip(d.loc, !!d.corners));
    });

  lastSelectedHidden = undefined;
  redrawFootprints();
}

function show(el: SVGElement, d: FootprintDatum, visible: boolean): void {
  if (d.shown === visible) return;
  d.shown = visible;
  el.style.display = visible ? '' : 'none';
}

/** Re-projects every footprint and places the labels. Runs on each chart redraw. */
export function redrawFootprints(): void {
  const d3 = window.d3;
  const C = window.Celestial;
  if (!d3 || !C?.mapProjection) return;
  const svg = d3.select(chartHost()).select('svg.imaging-markers-overlay');
  if (svg.empty()) return;

  const projection = C.mapProjection;
  svg.select(`#${CLIP_ID} path`).attr('d', d3.geo.path().projection(projection)({ type: 'Sphere' }));

  const canvas = chartSize();
  const project = (p: LonLat) => projection(p) as Point | null;
  const isVisible = (p: LonLat) => !!C.clip(p);
  let selectedHidden: FootprintHidden | undefined;

  const boxes = svg.selectAll('.fov-box');
  boxes.each(function (this: SVGGElement, d: FootprintDatum) {
    if (d.corners) {
      const res = projectFootprint(d.corners, project, isVisible, canvas, {
        width: d.loc.fovWidth!, height: d.loc.fovHeight!, paDeg: d.loc.rotation ?? 0,
      });
      d.points = res.points;
      d.hidden = res.hidden;
      if (!res.hidden) {
        const path = pathData(res.points);
        for (const child of this.querySelectorAll('.fov-rect, .fov-hit')) child.setAttribute('d', path);
      }
    } else {
      const p = project(d.center);
      d.hidden = p && Number.isFinite(p[0]) && Number.isFinite(p[1]) && isVisible(d.center) ? null : 'clip';
      if (!d.hidden) this.querySelector('.imaging-marker')?.setAttribute('transform', `translate(${p![0]},${p![1]})`);
    }
    show(this, d, d.hidden === null);
    if (selectedId != null && d.loc.id === selectedId) {
      // Several footprints can share an id across location types; any drawn one counts.
      selectedHidden = selectedHidden === null ? null : d.hidden;
    }
  });

  if (selectedId != null && selectedHidden !== undefined && selectedHidden !== lastSelectedHidden) {
    lastSelectedHidden = selectedHidden;
    handlers?.onSelectedHidden(selectedHidden);
  }

  placeLabels(boxes);
}

/**
 * Object-name labels: one per object name (on its largest box), placed
 * largest-first and skipped when it would overlap a placed one, so zoomed
 * out the big targets win and the rest appear as the user zooms in.
 */
function placeLabels(boxes: any): void {
  const d3 = window.d3;
  type Candidate = { el: SVGGElement; name: string; area: number; cx: number; topY: number; w: number; h: number };
  const byName = new Map<string, Candidate>();

  boxes.each(function (this: SVGGElement, d: FootprintDatum) {
    const name = d.loc.objectName;
    if (!d.corners || d.hidden || !name) return;
    const xs = d.points.map((p) => p[0]);
    const ys = d.points.map((p) => p[1]);
    const minX = Math.min(...xs);
    const maxX = Math.max(...xs);
    const c: Candidate = {
      el: this,
      name,
      area: (maxX - minX) * (Math.max(...ys) - Math.min(...ys)),
      cx: (minX + maxX) / 2,
      topY: Math.min(...ys),
      w: name.length * LABEL_CHAR_W + LABEL_PAD_X * 2,
      h: LABEL_FONT_SIZE + LABEL_PAD_Y * 2,
    };
    const existing = byName.get(name);
    if (!existing || c.area > existing.area) byName.set(name, c);
  });

  boxes.selectAll('g.fov-label').style('display', 'none');

  const placed: Array<[number, number, number, number]> = [];
  const overlaps = (a: number[], b: number[]) => !(a[2] < b[0] || b[2] < a[0] || a[3] < b[1] || b[3] < a[1]);
  for (const c of [...byName.values()].sort((a, b) => b.area - a.area)) {
    const x1 = c.cx - c.w / 2;
    const y1 = c.topY - LABEL_OFFSET_Y - c.h;
    const rect: [number, number, number, number] = [x1, y1, x1 + c.w, y1 + c.h];
    if (placed.some((p) => overlaps(p, rect))) continue;
    placed.push(rect);
    const label = d3.select(c.el).select('g.fov-label').style('display', null);
    label.select('rect.fov-label-bg').attr('x', x1).attr('y', y1).attr('width', c.w).attr('height', c.h);
    label.select('text.fov-label-text').attr('x', c.cx).attr('y', y1 + c.h / 2).text(c.name);
  }
}
