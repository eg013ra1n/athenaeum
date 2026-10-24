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
  /** The frame's sky sigma (`FrameAnalysis.noise`), in the same working units as
   *  `StarMetric.peak`. Missing or <= 0 falls back to the FWHM-only radius. */
  noise?: number | null;
}

/** Signal-to-noise level (in frame sigmas) at which the brightness radius ends. */
const BRIGHTNESS_RADIUS_K = 3;
const DEFAULT_BETA = 2.5;

/** Radius (image px) at which a Moffat profile of the given FWHM drops to `k` sigma,
 *  or 0 when the star never reaches that level (R <= 1) or the inputs are unusable. */
function brightnessRadius(fwhm: number, ratio: number, beta: number): number {
  if (!(ratio > 1) || !(fwhm > 0)) return 0;
  const alpha = fwhm / (2 * Math.sqrt(Math.pow(2, 1 / beta) - 1));
  return alpha * Math.sqrt(Math.pow(ratio, 1 / beta) - 1);
}

/**
 * Ellipse semi-axes of one star in CANVAS px.
 *
 * Image-px semi-axis = max(ellipse_scale * fwhm_axis, brightness radius), where the
 * brightness radius is where the star's Moffat profile falls to 3x the frame noise.
 * The upper clamp `max_radius` is applied in image px (so zooming never shrinks an
 * ellipse relative to its star); the lower clamp `min_radius` stays in canvas px.
 */
export function starSemiAxes(
  star: Pick<StarMetric, "peak" | "fwhm_x" | "fwhm_y" | "beta">,
  noise: number | null | undefined,
  settings: Pick<AnnotationSettings, "ellipse_scale" | "min_radius" | "max_radius">,
  scaleX: number,
  scaleY: number = scaleX,
): { a: number; b: number } {
  const ellipseScale = settings.ellipse_scale ?? 1.2;
  let ratio = 0;
  if (
    typeof noise === "number" && Number.isFinite(noise) && noise > 0 &&
    Number.isFinite(star.peak) && star.peak > 0
  ) {
    ratio = star.peak / (BRIGHTNESS_RADIUS_K * noise);
  }
  const beta = Math.min(10, Math.max(1.5, star.beta ?? DEFAULT_BETA));
  const axis = (fwhm: number, scale: number): number => {
    const imagePx = Math.max(ellipseScale * fwhm, brightnessRadius(fwhm, ratio, beta));
    return Math.max(settings.min_radius, Math.min(settings.max_radius, imagePx) * scale);
  };
  return { a: axis(star.fwhm_x, scaleX), b: axis(star.fwhm_y, scaleY) };
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
 *
 * In the canvas context, "output" space is the rendered image rectangle
 * (offsetX..offsetX+renderWidth, offsetY..offsetY+renderHeight). Semi-axes come
 * from `starSemiAxes`: min_radius is in canvas px, max_radius in image px.
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

    // Semi-axes: sized by the star's brightness (see starSemiAxes).
    const { a: semiMajor, b: semiMinor } = starSemiAxes(star, transform.noise, settings, scaleX, scaleY);

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
