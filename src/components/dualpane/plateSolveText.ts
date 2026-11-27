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
 * Only a recipe-3 row (one that carries the solver's odds) has that count: an
 * older build can store a different number in the same column after the
 * catalog's migration, so a row without finite odds, or with a zero count,
 * shows the matched count alone, never a denominator.
 */
export function formatInliers(
  matched: number,
  inField: number | null | undefined,
  log10Odds: number | null | undefined,
): string {
  const recipe3 = log10Odds != null && Number.isFinite(log10Odds);
  return recipe3 && inField != null && inField > 0 ? `${matched} / ${inField}` : `${matched}`;
}
