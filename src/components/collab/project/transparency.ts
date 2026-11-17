/**
 * The two ways the UI writes a frame's transparency (wave 6.5): the loss in
 * magnitudes against the equipment's clear sky, and the zero point it was
 * measured from. Both frame tables, the frame panel and the stacking Frames
 * block read from here, so the text is the same everywhere.
 */

import { pluralize } from '../format';

const MINUS = '−';

/** `−0.77 mag`; `0.00 mag` for a loss under half a hundredth; `—` for no value. */
export function formatTransparency(loss: number | null | undefined): string {
  if (loss === null || loss === undefined || !Number.isFinite(loss)) return '—';
  if (loss < 0.005) return '0.00 mag';
  return `${MINUS}${loss.toFixed(2)} mag`;
}

/**
 * `21.34 · 87 stars`; `—` when there is no zero point. The value is the reduced zero point (what the loss is
 * judged from). `zpSigma` is the field's per-star scatter, not the zero point's error, so it is not shown.
 */
export function formatZeroPoint(
  t: { zeroPoint: number | null; zpStars: number | null; zpSigma: number | null } | null | undefined,
): string {
  if (!t || t.zeroPoint === null || !Number.isFinite(t.zeroPoint)) return '—';
  const parts = [t.zeroPoint.toFixed(2)];
  if (t.zpStars !== null) parts.push(`${t.zpStars} ${pluralize(t.zpStars, 'star')}`);
  return parts.join(' · ');
}
