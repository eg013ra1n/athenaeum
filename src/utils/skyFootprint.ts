/** Celestial longitude/right ascension and declination, in degrees. */
export type SkyPoint = [number, number];

/**
 * Project four ordered sky corners (RA/declination in degrees) to a closed SVG path.
 * Screen size is not a visibility condition: a valid zoomed footprint may
 * exceed the viewport. Projection output is canvas pixels; scaleX/Y convert
 * those pixels to SVG CSS pixels. Returns null for clipped/nonfinite geometry,
 * nonpositive scales, or a corner count other than four; inputs are not mutated.
 */
export function projectSkyFootprint(
  corners: SkyPoint[],
  project: (point: SkyPoint) => SkyPoint | null,
  clip: (point: SkyPoint) => boolean,
  scaleX: number,
  scaleY: number,
): string | null {
  if (
    corners.length !== 4 ||
    !Number.isFinite(scaleX) ||
    !Number.isFinite(scaleY) ||
    scaleX <= 0 ||
    scaleY <= 0
  )
    return null;
  const points: SkyPoint[] = [];
  for (const corner of corners) {
    if (!clip(corner)) return null;
    const p = project(corner);
    if (!p || !Number.isFinite(p[0]) || !Number.isFinite(p[1])) return null;
    const scaled: SkyPoint = [p[0] * scaleX, p[1] * scaleY];
    if (!scaled.every(Number.isFinite)) return null;
    points.push(scaled);
  }
  return points.map((p, i) => `${i ? 'L' : 'M'}${p[0]},${p[1]}`).join(' ') + ' Z';
}

/**
 * Create both SVG paths even if the initial view cannot project this field.
 * Keeping empty paths and sky coordinates lets the next pan/zoom recover it.
 */
export function initializeSkyFootprint(group: any, corners: SkyPoint[]) {
  group.node().__fovCorners = corners;
  group
    .append('path')
    .attr('class', 'fov-hit')
    .attr('d', '')
    .style('fill', 'transparent')
    .style('stroke', 'transparent')
    .style('stroke-width', '14px')
    .style('pointer-events', 'all')
    .style('cursor', 'pointer');
  group
    .append('path')
    .attr('class', 'fov-rect')
    .attr('d', '')
    .style('fill', 'currentColor')
    .style('fill-opacity', 0.28)
    .style('stroke', 'currentColor')
    .style('stroke-width', '2px')
    .style('pointer-events', 'none');
}
