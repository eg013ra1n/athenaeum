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
  /** Analysis result width (coordinate space of star x/y) */
  imageWidth: number;
  /** Analysis result height (coordinate space of star x/y) */
  imageHeight: number;
}

/** Compute color for a star based on annotation settings color scheme.
 *  Mirrors rustafits annotate.rs star_color(). */
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
    value = star.fwhm;
    good = settings.fwhm_good;
    warn = settings.fwhm_warn;
  }

  if (value <= good) return "rgba(0, 255, 0, 0.8)";
  if (value <= warn) return "rgba(255, 255, 0, 0.8)";
  return "rgba(255, 0, 0, 0.8)";
}

/**
 * Draw all star annotations onto a canvas context.
 *
 * Mirrors the coordinate transform in rustafits annotate.rs compute_annotations():
 *   scale_x = output_width / result.width
 *   x_out   = star.x * scale_x
 *   semi_a  = (fwhm_x * scale_x * 2.5).clamp(min_radius, max_radius)
 *
 * In the canvas context, "output" space is the rendered image rectangle
 * (offsetX..offsetX+renderWidth, offsetY..offsetY+renderHeight), and
 * min_radius/max_radius are in canvas pixels (not scaled by analysis ratio).
 */
export function drawStarOverlay(
  ctx: CanvasRenderingContext2D,
  stars: StarMetric[],
  settings: AnnotationSettings,
  transform: OverlayTransform,
): void {
  const scaleX = transform.renderWidth / transform.imageWidth;
  const scaleY = transform.renderHeight / transform.imageHeight;

  for (const star of stars) {
    // Position: analysis coords → canvas coords
    const cx = transform.offsetX + star.x * scaleX;
    const cy = transform.offsetY + star.y * scaleY;

    // Semi-axes: match rustafits — fwhm * scale * 2.5, clamped to fixed canvas-pixel limits.
    // min_radius / max_radius are in canvas pixels (not analysis pixels).
    const rawA = star.fwhm_x * scaleX * 2.5;
    const rawB = star.fwhm_y * scaleY * 2.5;
    const semiMajor = Math.max(settings.min_radius, Math.min(settings.max_radius, rawA));
    const semiMinor = Math.max(settings.min_radius, Math.min(settings.max_radius, rawB));

    if (semiMajor < 0.5 || semiMinor < 0.5) continue;

    const color = starColor(star, settings);

    // Draw ellipse
    ctx.beginPath();
    ctx.ellipse(cx, cy, semiMajor, semiMinor, star.theta, 0, 2 * Math.PI);
    ctx.strokeStyle = color;
    ctx.lineWidth = settings.line_width;
    ctx.stroke();

    // Direction tick along elongation axis (matches rustafits: only when ecc > 0.15)
    if (settings.show_direction_tick && star.eccentricity > 0.15) {
      const tickLen = semiMajor * 0.5;
      const ct = Math.cos(star.theta);
      const st = Math.sin(star.theta);
      // Edge of ellipse along major axis
      const edgeX = cx + semiMajor * ct;
      const edgeY = cy + semiMajor * st;
      const tipX = edgeX + tickLen * ct;
      const tipY = edgeY + tickLen * st;

      ctx.beginPath();
      ctx.moveTo(edgeX, edgeY);
      ctx.lineTo(tipX, tipY);
      ctx.strokeStyle = color;
      ctx.lineWidth = settings.line_width;
      ctx.stroke();
    }
  }
}
