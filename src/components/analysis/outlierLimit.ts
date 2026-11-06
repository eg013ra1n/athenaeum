/**
 * The Analysis tab's auto outlier threshold, shared with the Stacking Frames table so the two never disagree.
 *
 * Rule: median + 3 × max(MAD, 0.1 × median). The MAD floor keeps a very tight cluster from collapsing the limit onto
 * the median (which would reject about half the frames). Median and MAD both take `sorted[floor(n / 2)]`, the upper
 * median of an even count. `null` when there are no values. Pure; `values` is left in its order.
 */
export function autoOutlierLimit(values: number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const median = sorted[Math.floor(sorted.length / 2)]!;
  const deviations = sorted.map((v) => Math.abs(v - median)).sort((a, b) => a - b);
  const mad = deviations[Math.floor(deviations.length / 2)]!;
  return median + 3 * Math.max(mad, median * 0.1);
}
