/**
 * Utility functions for handling Aitoff projection boundaries
 * The Aitoff projection is elliptical - pixels outside the ellipse are invalid
 */

/**
 * Check if a pixel coordinate is within the valid projection area
 * Uses pixelToSky conversion - if it returns null, the pixel is outside the map
 */
export function isPixelInProjection(
  x: number,
  y: number,
  pixelToSky: (x: number, y: number) => [number, number] | null
): boolean {
  return pixelToSky(x, y) !== null;
}

/**
 * Clamp a rectangle so ALL corners and edges remain within the projection
 * Ensures the entire rectangle stays within the valid Aitoff projection area
 *
 * @param currentX - Target x coordinate for opposite corner
 * @param currentY - Target y coordinate for opposite corner
 * @param startX - Starting x coordinate (where mouse was pressed)
 * @param startY - Starting y coordinate (where mouse was pressed)
 * @param pixelToSky - Conversion function that returns null for invalid coordinates
 * @param samplesPerEdge - Number of points to sample along each edge (default 5)
 * @returns Clamped coordinates where all corners and edges are valid
 */
export function clampRectangleToProjection(
  currentX: number,
  currentY: number,
  startX: number,
  startY: number,
  pixelToSky: (x: number, y: number) => [number, number] | null,
  samplesPerEdge: number = 5
): [number, number] {
  /**
   * Helper: Check if all points along a rectangle edge are valid
   */
  function isEdgeValid(x1: number, y1: number, x2: number, y2: number): boolean {
    for (let i = 0; i <= samplesPerEdge; i++) {
      const t = i / samplesPerEdge;
      const x = x1 + (x2 - x1) * t;
      const y = y1 + (y2 - y1) * t;
      if (!isPixelInProjection(x, y, pixelToSky)) {
        return false;
      }
    }
    return true;
  }

  /**
   * Helper: Check if entire rectangle (all edges) is valid
   */
  function isRectangleValid(cx: number, cy: number): boolean {
    // Check all 4 edges
    // Top edge: (startX, startY) to (cx, startY)
    if (!isEdgeValid(startX, startY, cx, startY)) return false;

    // Right edge: (cx, startY) to (cx, cy)
    if (!isEdgeValid(cx, startY, cx, cy)) return false;

    // Bottom edge: (cx, cy) to (startX, cy)
    if (!isEdgeValid(cx, cy, startX, cy)) return false;

    // Left edge: (startX, cy) to (startX, startY)
    if (!isEdgeValid(startX, cy, startX, startY)) return false;

    return true;
  }

  // Fast path: if unclamped rectangle is already valid, return as-is
  if (isRectangleValid(currentX, currentY)) {
    return [currentX, currentY];
  }

  // Binary search for maximum valid X (with current Y)
  function findMaxValidX(targetX: number, y: number): number {
    let left = startX;
    let right = targetX;
    const MAX_ITERATIONS = 15;
    const EPSILON = 1;  // 1 pixel accuracy

    for (let i = 0; i < MAX_ITERATIONS; i++) {
      const mid = (left + right) / 2;

      // Check if rectangle with this X is valid
      const validTop = isEdgeValid(startX, startY, mid, startY);
      const validRight = isEdgeValid(mid, startY, mid, y);
      const validBottom = isEdgeValid(mid, y, startX, y);

      if (validTop && validRight && validBottom) {
        left = mid;
      } else {
        right = mid;
      }

      if (Math.abs(right - left) < EPSILON) {
        break;
      }
    }

    return left;
  }

  // Binary search for maximum valid Y (with current X)
  function findMaxValidY(x: number, targetY: number): number {
    let left = startY;
    let right = targetY;
    const MAX_ITERATIONS = 15;
    const EPSILON = 1;  // 1 pixel accuracy

    for (let i = 0; i < MAX_ITERATIONS; i++) {
      const mid = (left + right) / 2;

      // Check if rectangle with this Y is valid
      const validRight = isEdgeValid(x, startY, x, mid);
      const validBottom = isEdgeValid(x, mid, startX, mid);
      const validLeft = isEdgeValid(startX, mid, startX, startY);

      if (validRight && validBottom && validLeft) {
        left = mid;
      } else {
        right = mid;
      }

      if (Math.abs(right - left) < EPSILON) {
        break;
      }
    }

    return left;
  }

  // Iterative refinement to handle X/Y interdependence
  let clampedX = currentX;
  let clampedY = currentY;
  const MAX_REFINEMENTS = 3;

  for (let iteration = 0; iteration < MAX_REFINEMENTS; iteration++) {
    const prevX = clampedX;
    const prevY = clampedY;

    // Clamp X with current Y
    clampedX = findMaxValidX(currentX, clampedY);

    // Clamp Y with clamped X
    clampedY = findMaxValidY(clampedX, currentY);

    // Check for convergence
    const dx = Math.abs(clampedX - prevX);
    const dy = Math.abs(clampedY - prevY);

    if (dx < 1 && dy < 1) {
      break;  // Converged
    }
  }

  return [clampedX, clampedY];
}
