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
  /** Whether the display image was vertically flipped during processing.
   *  Mirrors rustafits ProcessedImage.flip_vertical. */
  flipVertical: boolean;
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
 *   semi_a  = (fwhm_x * scale_x * ellipse_scale).clamp(min_radius, max_radius)
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
    // Mirrors rustafits annotate.rs compute_annotations():
    //   x_out = star.x * scale_x
    //   y_out = if flip_vertical { output_height - 1.0 - star.y * scale_y } else { star.y * scale_y }
    const cx = transform.offsetX + star.x * scaleX;
    const cy = transform.flipVertical
      ? transform.offsetY + transform.renderHeight - 1 - star.y * scaleY
      : transform.offsetY + star.y * scaleY;

    // Semi-axes: match rustafits — fwhm * scale * ellipse_scale, clamped to
    // fixed canvas-pixel limits (min_radius / max_radius are canvas pixels).
    const ellipseScale = settings.ellipse_scale ?? 1.2;
    const rawA = star.fwhm_x * scaleX * ellipseScale;
    const rawB = star.fwhm_y * scaleY * ellipseScale;
    const semiMajor = Math.max(settings.min_radius, Math.min(settings.max_radius, rawA));
    const semiMinor = Math.max(settings.min_radius, Math.min(settings.max_radius, rawB));

    if (semiMajor < 0.5 || semiMinor < 0.5) continue;

    const color = starColor(star, settings);

    // Theta: negate when flipped (mirrors rustafits annotate.rs)
    const theta = transform.flipVertical ? -star.theta : star.theta;

    // Draw ellipse
    ctx.beginPath();
    ctx.ellipse(cx, cy, semiMajor, semiMinor, theta, 0, 2 * Math.PI);
    ctx.strokeStyle = color;
    ctx.lineWidth = settings.line_width;
    ctx.stroke();

    // Direction tick along elongation axis (matches rustafits: only when ecc > 0.15)
    if (settings.show_direction_tick && star.eccentricity > 0.15) {
      const tickLen = semiMajor * 0.5;
      const ct = Math.cos(theta);
      const st = Math.sin(theta);
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
