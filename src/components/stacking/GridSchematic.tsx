// The canonical grid's schematic (wave 6c, D11): the grid rectangle, the project target and every accepted frame's
// outline on one tangent plane, drawn grid-up as the master will look. Nothing here is measured or saved; it is the
// editor's picture of what a grid choice does to the frames. Used by the grid card (the grid shown) and by the grid
// dialog (the draft as it is edited).

import { useMemo } from 'react';
import { useMemberColor } from '../collab/project/MemberColorsContext';
import type { CanonicalGrid } from '../../types/models';
import type { PlanGridFootprint, PlanGridTarget } from '../../types/stacking';
import { northArrowAngleDeg, skyToGridPixel, toSchematic } from './gridProjection';

export interface GridSchematicProps {
  /** The grid to draw; `null` for a project with none yet: the target circle and the outlines only, on a tangent
   *  plane at the target. */
  grid: CanonicalGrid | null;
  target: PlanGridTarget;
  footprints: PlanGridFootprint[];
  /** Accepted frames the plan could not outline (no WCS in the manifest). */
  framesNotDrawn: number;
}

/** The tallest the schematic grows; its width is the card's. */
const MAX_HEIGHT_PX = 220;
/** Extra room around everything drawn, a fraction of each side. */
const VIEW_PAD = 0.05;
/** The synthetic plane for a project with no grid: a square of this many pixels on a side, scaled so the target
 *  circle fills most of it. */
const PLANE_PX = 1000;
const PLANE_CIRCLE_PX = 450;
/** Angular radii at or past this have no useful circle on a tangent plane. */
const MAX_CIRCLE_RADIUS_DEG = 89;

const NORTH_BOX = 36;

interface Box {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

const grow = (box: Box | null, x: number, y: number): Box =>
  box
    ? { minX: Math.min(box.minX, x), minY: Math.min(box.minY, y), maxX: Math.max(box.maxX, x), maxY: Math.max(box.maxY, y) }
    : { minX: x, minY: y, maxX: x, maxY: y };

/** The tangent plane at the target, for a project with no grid. */
function planeAtTarget(target: PlanGridTarget): CanonicalGrid {
  const radius = target.radiusDeg > 0 && target.radiusDeg < MAX_CIRCLE_RADIUS_DEG ? target.radiusDeg : 1;
  const arcsecAtRadius = Math.tan((radius * Math.PI) / 180) * (180 / Math.PI) * 3600;
  return {
    scaleArcsec: arcsecAtRadius / PLANE_CIRCLE_PX,
    widthPx: PLANE_PX,
    heightPx: PLANE_PX,
    crval1: target.raDeg,
    crval2: target.decDeg,
    rotationDeg: 0,
  };
}

const pointsAttr = (pts: { x: number; y: number }[]) => pts.map((p) => `${p.x.toFixed(2)},${p.y.toFixed(2)}`).join(' ');

export function GridSchematic({ grid, target, footprints, framesNotDrawn }: GridSchematicProps) {
  const colorOf = useMemberColor();

  const drawing = useMemo(() => {
    const plane = grid ?? planeAtTarget(target);
    let box: Box | null = null;

    // The grid rectangle spans the pixel edges, -0.5 ... W-0.5 and -0.5 ... H-0.5.
    let rect: { x: number; y: number; width: number; height: number } | null = null;
    if (grid) {
      const a = toSchematic({ x: -0.5, y: -0.5 }, grid);
      const b = toSchematic({ x: grid.widthPx - 0.5, y: grid.heightPx - 0.5 }, grid);
      rect = { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), width: Math.abs(b.x - a.x), height: Math.abs(b.y - a.y) };
      box = grow(grow(box, rect.x, rect.y), rect.x + rect.width, rect.y + rect.height);
    }

    // The target circle: its centre through the TAN, its radius the tangent-plane radius in pixels.
    let circle: { cx: number; cy: number; r: number } | null = null;
    const centre = skyToGridPixel(target.raDeg, target.decDeg, plane);
    if (centre && target.radiusDeg > 0 && target.radiusDeg < MAX_CIRCLE_RADIUS_DEG) {
      const at = toSchematic(centre, plane);
      const r = (Math.tan((target.radiusDeg * Math.PI) / 180) * (180 / Math.PI) * 3600) / plane.scaleArcsec;
      circle = { cx: at.x, cy: at.y, r };
      if (!grid) box = grow(grow(box, at.x - r, at.y - r), at.x + r, at.y + r);
    }

    // Every outline point through the TAN: never interpolated and never reduced to a min/max in RA (an outline
    // across RA 0 jumps 359.9 -> 0.1). An outline with one point behind the plane is not drawn at all.
    const polygons: { footprint: PlanGridFootprint; points: string }[] = [];
    for (const footprint of footprints) {
      const projected = footprint.outline.map(([ra, dec]) => skyToGridPixel(ra, dec, plane));
      if (projected.length < 3 || projected.some((p) => p === null)) continue;
      const pts = projected.map((p) => toSchematic(p!, plane));
      for (const p of pts) box = grow(box, p.x, p.y);
      polygons.push({ footprint, points: pointsAttr(pts) });
    }

    // Nothing to frame (no grid, no circle, no outline): the synthetic plane itself.
    const framed = box ?? { minX: 0, minY: 0, maxX: PLANE_PX, maxY: PLANE_PX };
    const w = Math.max(framed.maxX - framed.minX, 1);
    const h = Math.max(framed.maxY - framed.minY, 1);
    const view = { x: framed.minX - w * VIEW_PAD, y: framed.minY - h * VIEW_PAD, width: w * (1 + 2 * VIEW_PAD), height: h * (1 + 2 * VIEW_PAD) };

    return { rect, circle, polygons, view, north: grid ? northArrowAngleDeg(grid) : null };
  }, [grid, target, footprints]);

  const { view } = drawing;
  return (
    <div>
      <div className="relative">
        <svg
          data-testid="grid-schematic"
          role="img"
          aria-label="The canonical grid with the outlines of the accepted frames"
          viewBox={`${view.x} ${view.y} ${view.width} ${view.height}`}
          preserveAspectRatio="xMidYMid meet"
          className="block w-full rounded border border-line bg-surface"
          style={{ aspectRatio: `${view.width} / ${view.height}`, maxHeight: MAX_HEIGHT_PX }}
        >
          {drawing.circle && (
            <circle
              data-testid="grid-target"
              cx={drawing.circle.cx}
              cy={drawing.circle.cy}
              r={drawing.circle.r}
              fill="none"
              strokeWidth={1}
              strokeDasharray="5 4"
              vectorEffect="non-scaling-stroke"
              className="stroke-content-muted"
              opacity={0.6}
            />
          )}
          {drawing.rect && (
            <rect
              data-testid="grid-rect"
              x={drawing.rect.x}
              y={drawing.rect.y}
              width={drawing.rect.width}
              height={drawing.rect.height}
              fill="none"
              strokeWidth={2}
              vectorEffect="non-scaling-stroke"
              className="stroke-accent"
            />
          )}
          {drawing.polygons.map(({ footprint, points }) => {
            const colour = colorOf(footprint.publisherAccountId);
            return (
              <polygon
                key={footprint.frameUuid}
                data-testid="grid-outline"
                data-frame-uuid={footprint.frameUuid}
                points={points}
                fill="none"
                strokeWidth={1}
                strokeOpacity={0.85}
                strokeLinejoin="round"
                vectorEffect="non-scaling-stroke"
                // A publisher the project page does not know keeps a neutral outline, never a palette slot.
                {...(colour ? { stroke: colour } : { stroke: 'currentColor', className: 'text-content-muted' })}
              />
            );
          })}
        </svg>
        {drawing.north !== null && <NorthArrow angleDeg={drawing.north} />}
      </div>
      {framesNotDrawn > 0 && (
        <p className="mt-1 text-[11px] text-content-faint">{framesNotDrawn} frames are not drawn: no WCS in the manifest</p>
      )}
    </div>
  );
}

/** A small `N` arrow in the schematic's top-right corner, along the projection of north from the grid centre. The
 *  `N` stays upright at the arrow's tip. */
function NorthArrow({ angleDeg }: { angleDeg: number }) {
  const c = NORTH_BOX / 2;
  const a = (angleDeg * Math.PI) / 180;
  const dir = { x: Math.sin(a), y: -Math.cos(a) };
  const label = { x: c + dir.x * 13, y: c + dir.y * 13 };
  // `+ 0` turns a rounded -0 into 0.
  const shown = (Math.round(angleDeg * 10) / 10 + 0).toFixed(1);
  return (
    <svg
      data-testid="grid-north"
      data-angle={shown}
      width={NORTH_BOX}
      height={NORTH_BOX}
      viewBox={`0 0 ${NORTH_BOX} ${NORTH_BOX}`}
      aria-hidden="true"
      className="pointer-events-none absolute right-1.5 top-1.5 text-content-muted"
    >
      <g transform={`rotate(${shown} ${c} ${c})`} stroke="currentColor" fill="currentColor" strokeWidth={1.25} strokeLinecap="round">
        <line x1={c} y1={c + 7} x2={c} y2={c - 5} />
        <polygon points={`${c},${c - 9} ${c - 3},${c - 4} ${c + 3},${c - 4}`} stroke="none" />
      </g>
      <text x={label.x} y={label.y} textAnchor="middle" dominantBaseline="central" fontSize={9} fill="currentColor" stroke="none">
        N
      </text>
    </svg>
  );
}
