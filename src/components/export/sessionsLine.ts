/**
 * The "Sessions" line: the capture span, then the night count the backend
 * read off the catalog's imaging nights. The count is never the days between
 * the first and last frame — three nights a year apart are "3 nights", and a
 * night that crosses midnight is one night.
 */
export function formatSessions(dateRange: [string, string], nightCount: number): string {
  const span = formatDateRange(dateRange);
  if (nightCount <= 0) return span;
  return `${span} (${nightCount} night${nightCount === 1 ? '' : 's'})`;
}

/**
 * Format a date range for display
 */
function formatDateRange(dateRange: [string, string]): string {
  const [start, end] = dateRange;
  const startDate = new Date(start);
  const endDate = new Date(end);
  if (Number.isNaN(startDate.getTime()) || Number.isNaN(endDate.getTime())) {
    // Not a parseable timestamp: show what the catalog holds.
    return start === end ? start : `${start} to ${end}`;
  }
  const fmt: Intl.DateTimeFormatOptions = { month: 'short', day: 'numeric', year: 'numeric' };
  const startStr = startDate.toLocaleDateString('en-US', fmt);
  const endStr = endDate.toLocaleDateString('en-US', fmt);
  return startStr === endStr ? startStr : `${startStr} - ${endStr}`;
}
