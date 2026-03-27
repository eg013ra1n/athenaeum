import type { StarMetric } from "../../types/models";
import type { AnnotationSettings } from "../../types/analysis-config";

/** Parameters needed to map FITS pixel coords to canvas coords */
export interface OverlayTransform {
  /** Canvas pixel offset X (top-left of rendered image) */
  offsetX: number;
  /** Canvas pixel offset Y (top-left of rendered image) */
  offsetY: number;
  /** Rendered image width in canvas pixels (after zoom) */
  renderWidth: number;
  /** Rendered image height in canvas pixels (after zoom) */
  renderHeight: number;
  /** Original FITS image width in pixels */
  imageWidth: number;
  /** Original FITS image height in pixels */
  imageHeight: number;
}

/** Compute color for a star based on annotation settings color scheme */
function starColor(star: StarMetric, settings: AnnotationSettings): string {
  if (settings.color_scheme === "uniform") {
    return "rgba(0, 255, 0, 0.8)";
  }

  let value: number;
  let good: number;
  let warn: number;

  if (settings.color_scheme === "eccentricity") {
    value = star.eccentricity;
    good = settings.ecc_good;
    warn = settings.ecc_warn;
  } else {
    // fwhm
    value = star.fwhm;
    good = settings.fwhm_good;
    warn = settings.fwhm_warn;
  }

  if (value <= good) return "rgba(0, 255, 0, 0.8)";
  if (value <= warn) return "rgba(255, 255, 0, 0.8)";
  return "rgba(255, 0, 0, 0.8)";
}

/** Draw all star annotations on the overlay canvas */
export function drawStarOverlay(
  ctx: CanvasRenderingContext2D,
  _canvasWidth: number,
  _canvasHeight: number,
  stars: StarMetric[],
  settings: AnnotationSettings,
  transform: OverlayTransform,
): void {
  const scaleX = transform.renderWidth / transform.imageWidth;
  const scaleY = transform.renderHeight / transform.imageHeight;

  for (const star of stars) {
    const cx = transform.offsetX + star.x * scaleX;
    const cy = transform.offsetY + star.y * scaleY;

    const rawRadiusX = (star.fwhm_x / 2) * scaleX;
    const rawRadiusY = (star.fwhm_y / 2) * scaleY;
    const minR = settings.min_radius * Math.min(scaleX, scaleY);
    const maxR = settings.max_radius * Math.min(scaleX, scaleY);
    const radiusX = Math.max(minR, Math.min(maxR, rawRadiusX));
    const radiusY = Math.max(minR, Math.min(maxR, rawRadiusY));

    if (radiusX < 0.5 || radiusY < 0.5) continue;

    const color = starColor(star, settings);

    ctx.beginPath();
    ctx.ellipse(cx, cy, radiusX, radiusY, star.theta, 0, 2 * Math.PI);
    ctx.strokeStyle = color;
    ctx.lineWidth = settings.line_width;
    ctx.stroke();

    if (settings.show_direction_tick) {
      const tickLen = radiusX * 0.5;
      const edgeX = cx + radiusX * Math.cos(star.theta);
      const edgeY = cy + radiusY * Math.sin(star.theta);
      const tipX = edgeX + tickLen * Math.cos(star.theta);
      const tipY = edgeY + tickLen * Math.sin(star.theta);

      ctx.beginPath();
      ctx.moveTo(edgeX, edgeY);
      ctx.lineTo(tipX, tipY);
      ctx.strokeStyle = color;
      ctx.lineWidth = settings.line_width;
      ctx.stroke();
    }
  }
}

/** Clear the overlay canvas */
export function clearStarOverlay(
  ctx: CanvasRenderingContext2D,
  canvasWidth: number,
  canvasHeight: number,
): void {
  ctx.clearRect(0, 0, canvasWidth, canvasHeight);
}
