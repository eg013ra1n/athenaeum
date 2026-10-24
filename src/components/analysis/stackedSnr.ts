/**
 * Stacked frame SNR of a group of frames (PC-11): `frame_snr` is background / noise, a plain ratio, and independent
 * frames add in quadrature, so the stack's value is √Σ v² over the finite values. `undefined` when none is finite.
 */
export function stackedFrameSnr(values: number[]): number | undefined {
  let sumSq = 0;
  let count = 0;
  for (const v of values) {
    if (!Number.isFinite(v)) continue;
    sumSq += v * v;
    count++;
  }
  return count > 0 ? Math.sqrt(sumSq) : undefined;
}
