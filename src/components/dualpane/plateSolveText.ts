/** Text of the plate-solve block in the metadata pane. */

/**
 * The solver's decision as a reader's number: `odds 10^N to 1` for the Bayes
 * factor's decades, rounded down so the text never claims more than was
 * measured. A solve stored before the odds existed has none: a dash.
 */
export function formatSolveOdds(log10Odds: number | null | undefined): string {
  if (log10Odds == null || !Number.isFinite(log10Odds)) return '—';
  return `odds 10^${Math.floor(log10Odds)} to 1`;
}

/**
 * Matched stars over the reference stars the solved WCS puts in the frame.
 * A solve without that count (older recipe) or with a zero one shows the
 * matched count alone, never a zero denominator.
 */
export function formatInliers(matched: number, inField: number | null | undefined): string {
  return inField != null && inField > 0 ? `${matched} / ${inField}` : `${matched}`;
}
