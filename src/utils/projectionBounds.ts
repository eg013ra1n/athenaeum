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
 * Clamp a pixel coordinate to the nearest point on the projection boundary
 * Uses binary search along the ray from start point to target point
 *
 * @param x - Target x coordinate (may be outside projection)
 * @param y - Target y coordinate (may be outside projection)
 * @param startX - Starting x coordinate (should be inside projection)
 * @param startY - Starting y coordinate (should be inside projection)
 * @param pixelToSky - Conversion function that returns null for invalid coordinates
 * @returns Clamped coordinates on or inside the projection boundary
 */
export function clampToProjectionBoundary(
  x: number,
  y: number,
  startX: number,
  startY: number,
  pixelToSky: (x: number, y: number) => [number, number] | null
): [number, number] {
  // If target is already in projection, return as-is
  if (isPixelInProjection(x, y, pixelToSky)) {
    return [x, y];
  }

  // Binary search along the ray from start to target
  // Find the point on the boundary where projection becomes invalid
  let left = 0;    // Start point (inside)
  let right = 1;   // Target point (outside)
  const MAX_ITERATIONS = 20;  // Sufficient for sub-pixel accuracy
  const EPSILON = 0.5;  // Stop when within 0.5 pixels

  for (let i = 0; i < MAX_ITERATIONS; i++) {
    const mid = (left + right) / 2;
    const testX = startX + (x - startX) * mid;
    const testY = startY + (y - startY) * mid;

    if (isPixelInProjection(testX, testY, pixelToSky)) {
      // Mid point is inside, move left bound forward
      left = mid;
    } else {
      // Mid point is outside, move right bound back
      right = mid;
    }

    // Check if we've converged
    const dx = (x - startX) * (right - left);
    const dy = (y - startY) * (right - left);
    const distance = Math.sqrt(dx * dx + dy * dy);

    if (distance < EPSILON) {
      break;
    }
  }

  // Return the point at the left bound (last known valid point)
  return [
    startX + (x - startX) * left,
    startY + (y - startY) * left
  ];
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

/**
 * Calculate the rectangular bounding box of the projection in pixel space
 * Useful for quick rejection testing before detailed checks
 *
 * @param canvasWidth - Width of the canvas/SVG
 * @param canvasHeight - Height of the canvas/SVG
 * @param pixelToSky - Conversion function
 * @returns Bounding box {minX, minY, maxX, maxY} or null if cannot determine
 */
export function getProjectionBoundingBox(
  canvasWidth: number,
  canvasHeight: number,
  pixelToSky: (x: number, y: number) => [number, number] | null
): { minX: number; minY: number; maxX: number; maxY: number } | null {
  // For Aitoff projection centered in canvas, the bounding box is typically:
  // - Horizontally: some margin from edges
  // - Vertically: full height or slightly inset

  // Sample points to find the actual bounds
  const centerX = canvasWidth / 2;
  const centerY = canvasHeight / 2;

  // Find left edge
  let minX = 0;
  for (let x = 0; x < centerX; x += 10) {
    if (isPixelInProjection(x, centerY, pixelToSky)) {
      minX = x;
      break;
    }
  }

  // Find right edge
  let maxX = canvasWidth;
  for (let x = canvasWidth; x > centerX; x -= 10) {
    if (isPixelInProjection(x, centerY, pixelToSky)) {
      maxX = x;
      break;
    }
  }

  // Find top edge
  let minY = 0;
  for (let y = 0; y < centerY; y += 10) {
    if (isPixelInProjection(centerX, y, pixelToSky)) {
      minY = y;
      break;
    }
  }

  // Find bottom edge
  let maxY = canvasHeight;
  for (let y = canvasHeight; y > centerY; y -= 10) {
    if (isPixelInProjection(centerX, y, pixelToSky)) {
      maxY = y;
      break;
    }
  }

  return { minX, minY, maxX, maxY };
}
