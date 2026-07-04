// Flat Contour Plot — builds a composite (image + legend strip) PNG dataURL
// suitable for feeding through BlinkViewer's existing renderImage pipeline.
//
// The backend (`compute_flat_contour_plot`) returns the final 8-bit grayscale
// display image plus the percentile bounds and band count needed to label the
// legend strip in original-flat units. This module:
//   1. Calls the backend with the user's settings (or PI defaults).
//   2. Decodes the base64 pixel buffer into a small canvas.
//   3. Draws a legend strip to the right of it (contours+1 swatches, labelled).
//   4. Returns a `data:image/png;base64,…` URL of the composite.
//
// PixInsight FlatContourPlot v1.3.1 reference; settings persist in the app's
// settings store (`flat_contour.*` keys). Defaults match PI exactly.

import { api } from '../../api';
import type { FlatContourOpts } from '../../types/models';
import type { FlatContourPlot } from '../../types/helpers';

/** PixInsight FlatContourPlot v1.3.1 defaults — used when no settings
 *  override is provided. */
export const FLAT_CONTOUR_DEFAULTS: FlatContourOpts = {
  resolutionPct: 50,
  sigmaPx: 1.0,
  contours: 15,
  gradientPct: 50,
};

/** Settings keys (read via `get_setting`) for user-tunable defaults. */
export const FLAT_CONTOUR_SETTING_KEYS = {
  resolutionPct: 'flat_contour.resolution_pct',
  sigmaPx: 'flat_contour.sigma_px',
  contours: 'flat_contour.contours',
  gradientPct: 'flat_contour.gradient_pct',
};

/** Reads each `flat_contour.*` setting from the backend; falls back to the
 *  PI defaults for any key that isn't set yet. */
export async function loadFlatContourOpts(): Promise<FlatContourOpts> {
  const [r, s, c, g] = await Promise.all([
    safeGetNumber(FLAT_CONTOUR_SETTING_KEYS.resolutionPct, FLAT_CONTOUR_DEFAULTS.resolutionPct),
    safeGetNumber(FLAT_CONTOUR_SETTING_KEYS.sigmaPx, FLAT_CONTOUR_DEFAULTS.sigmaPx),
    safeGetNumber(FLAT_CONTOUR_SETTING_KEYS.contours, FLAT_CONTOUR_DEFAULTS.contours),
    safeGetNumber(FLAT_CONTOUR_SETTING_KEYS.gradientPct, FLAT_CONTOUR_DEFAULTS.gradientPct),
  ]);
  return {
    resolutionPct: clampInt(r, 5, 100),
    sigmaPx: clampNum(s, 0, 10),
    contours: clampInt(c, 4, 20),
    gradientPct: clampInt(g, 0, 200),
  };
}

async function safeGetNumber(key: string, fallback: number): Promise<number> {
  try {
    const raw = await api.invoke<string | null>('get_setting', {
      key,
      defaultValue: String(fallback),
    });
    if (raw == null || raw === '') return fallback;
    const n = parseFloat(raw);
    return Number.isFinite(n) ? n : fallback;
  } catch {
    return fallback;
  }
}

function clampInt(v: number, lo: number, hi: number): number {
  if (!Number.isFinite(v)) return lo;
  return Math.max(lo, Math.min(hi, Math.round(v)));
}

function clampNum(v: number, lo: number, hi: number): number {
  if (!Number.isFinite(v)) return lo;
  return Math.max(lo, Math.min(hi, v));
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Fetch the contour plot for `fileId` with the given options and return a
 *  PNG dataURL of the composite (plot + legend strip). The composite is
 *  designed to be fed straight into BlinkViewer's `renderImage` so the
 *  existing zoom/pan machinery just works. */
export async function fetchAndComposeFlatContour(
  fileId: number,
  opts: FlatContourOpts,
): Promise<string> {
  const result = await api.invoke<FlatContourPlot>('compute_flat_contour_plot', {
    fileId,
    opts,
  });
  return composeFlatContourDataUrl(result);
}

/** Builds the (image + legend strip) composite as a PNG dataURL. */
export function composeFlatContourDataUrl(result: FlatContourPlot): string {
  const bytes = base64ToBytes(result.pixelsB64);
  const expected = result.width * result.height;
  if (bytes.length !== expected) {
    throw new Error(
      `Flat contour pixel buffer size mismatch: got ${bytes.length}, expected ${expected}`,
    );
  }

  // ── plot canvas (matches the backend's output dimensions exactly) ────
  const plot = document.createElement('canvas');
  plot.width = result.width;
  plot.height = result.height;
  const pctx = plot.getContext('2d');
  if (!pctx) throw new Error('Could not acquire 2D context for plot canvas');
  const img = pctx.createImageData(result.width, result.height);
  for (let i = 0; i < bytes.length; i++) {
    const g = bytes[i];
    const off = i * 4;
    img.data[off] = g;
    img.data[off + 1] = g;
    img.data[off + 2] = g;
    img.data[off + 3] = 255;
  }
  pctx.putImageData(img, 0, 0);

  // ── legend strip (PixInsight layout: contours+1 swatches, top = max) ─
  // Sized so the labels actually fit. Width is computed from font size +
  // the longest label text (using ctx.measureText), not from image width —
  // image-width-based sizing was clipping the decimal portion of labels
  // for under-saturated flats with small value spans (e.g., a flat with
  // values 326..344 has step 1.2, labels "326.0/327.2/…" but the decimals
  // got chopped, so the user only saw integers and apparent gaps).
  const cBands = Math.max(1, result.contours);
  const totalSlabs = cBands + 1;
  const valueSpan = result.maxQuantile - result.minQuantile;
  const stepSize = Math.max(valueSpan / cBands, 1e-9);
  const decimals = Math.max(0, Math.min(6, Math.ceil(-Math.log10(stepSize)) + 1));

  // Font size: scale to image height but cap so labels stay reasonable
  // when the composite is fit-to-view in the Blink viewport.
  const fontPx = Math.max(14, Math.min(64, Math.round(result.height / (totalSlabs * 2.4))));
  const swatchWidth = Math.max(48, Math.round(fontPx * 1.5));

  // Measure the widest label so the legend canvas is sized accordingly.
  // The widest label is whichever of `minQuantile` / `maxQuantile` produces
  // a longer string. A trailing " ADU" suffix is added so the unit is
  // unambiguous (PI shows normalized 0..1 values; Athenaeum reads raw
  // 16-bit ADU counts directly, so a flat at half-well is ~32768 here).
  const labelOf = (v: number) => `${v.toFixed(decimals)} ADU`;
  const measureCtx = document.createElement('canvas').getContext('2d');
  if (!measureCtx) throw new Error('Could not acquire 2D context for legend label measurement');
  measureCtx.font = `${fontPx}px system-ui, -apple-system, sans-serif`;
  const labelPx = Math.ceil(
    Math.max(
      measureCtx.measureText(labelOf(result.minQuantile)).width,
      measureCtx.measureText(labelOf(result.maxQuantile)).width,
    ),
  );
  const labelGap = Math.round(fontPx * 0.5);
  const rightPad = Math.round(fontPx * 0.6);
  const legendWidth = swatchWidth + labelGap + labelPx + rightPad;

  const legend = document.createElement('canvas');
  legend.width = legendWidth;
  legend.height = result.height;
  const lctx = legend.getContext('2d');
  if (!lctx) throw new Error('Could not acquire 2D context for legend canvas');

  // Black background so dark labels stay readable inside Blink's dark UI.
  lctx.fillStyle = '#000000';
  lctx.fillRect(0, 0, legend.width, legend.height);

  const minTone = 0.05;
  const maxTone = 0.95;
  const toneSpan = maxTone - minTone;
  const slabH = result.height / totalSlabs;

  lctx.font = `${fontPx}px system-ui, -apple-system, sans-serif`;
  lctx.textBaseline = 'middle';

  for (let i = 0; i <= cBands; i++) {
    const slabFromTop = cBands - i; // i = cBands at top, i = 0 at bottom
    const y = slabFromTop * slabH;
    const tone = Math.round(255 * ((i / cBands) * toneSpan + minTone));
    lctx.fillStyle = `rgb(${tone}, ${tone}, ${tone})`;
    lctx.fillRect(0, y, swatchWidth, Math.ceil(slabH) + 1);

    const value = result.minQuantile + valueSpan * (i / cBands);
    lctx.fillStyle = '#e5e9f0';
    lctx.fillText(labelOf(value), swatchWidth + labelGap, y + slabH / 2);
  }

  // ── composite (plot + small gap + legend strip) ──────────────────────
  const gap = Math.max(8, Math.round(result.width * 0.005));
  const composite = document.createElement('canvas');
  composite.width = result.width + gap + legendWidth;
  composite.height = result.height;
  const cctx = composite.getContext('2d');
  if (!cctx) throw new Error('Could not acquire 2D context for composite canvas');
  cctx.fillStyle = '#000000';
  cctx.fillRect(0, 0, composite.width, composite.height);
  cctx.drawImage(plot, 0, 0);
  cctx.drawImage(legend, result.width + gap, 0);

  return composite.toDataURL('image/png');
}
