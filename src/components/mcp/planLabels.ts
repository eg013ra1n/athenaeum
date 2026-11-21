// The plan window speaks the user's words, never FITS keywords. The backend names its columns by header keyword
// (`IMAGETYP`, `DATE-OBS`, ...); this is the one place that turns a key into the label the Duplicates page and the
// rest of the app use.
import type { McpPlanView, PlanGroup, PlanItem } from '../../types/mcp';

const LABELS: Record<string, string> = {
  IMAGETYP: 'Type',
  OBJECT: 'Object',
  FILTER: 'Filter',
  EXPTIME: 'Exposure',
  'DATE-OBS': 'Date',
  INSTRUME: 'Camera',
  size: 'Size',
  modified: 'Modified',
  field: 'Field',
  current: 'Now',
  proposed: 'Proposed',
  object: 'Object',
  filter: 'Filter',
  exposure: 'Exposure',
  coordinates: 'Position hint',
};

/** A keyword in capitals (`XBINNING`) becomes a word (`Xbinning`); anything else is the backend's own label. */
export function humanLabel(key: string, backendLabel?: string): string {
  const known = LABELS[key] ?? LABELS[key.toUpperCase()];
  if (known) return known;
  const text = backendLabel ?? key;
  return /^[A-Z0-9_-]{2,}$/.test(text) ? text.charAt(0) + text.slice(1).toLowerCase().replace(/[_-]/g, ' ') : text;
}

/** A value as the window shows it: an exposure that is a bare number gets its unit. */
export function humanValue(key: string, value: string): string {
  if (key.toUpperCase() === 'EXPTIME' && /^\d+(\.\d+)?$/.test(value.trim())) return `${value.trim()} s`;
  return value;
}

/** Keys that have a place of their own in a row; the others are facts on the muted second line. */
const OWN_PLACE = new Set(['size', 'modified', 'field', 'current', 'proposed', 'coordinates']);

export interface Fact {
  key: string;
  label: string;
  value: string;
}

/** The descriptive facts of a plan's items, in the backend's column order. */
export function factKeys(plan: McpPlanView): { key: string; label: string }[] {
  return plan.columns.filter((c) => !OWN_PLACE.has(c.key)).map((c) => ({ key: c.key, label: humanLabel(c.key, c.label) }));
}

/**
 * Facts every item of a group shares are said once for the group; the others stay with their row.
 * `shared` keeps the column order; `own(item)` is what is left for that item.
 */
export function splitFacts(
  keys: { key: string; label: string }[],
  group: PlanGroup,
): { shared: Fact[]; own: (item: PlanItem) => Fact[] } {
  const fact = (k: { key: string; label: string }, item: PlanItem): Fact | null => {
    const v = item.values[k.key];
    return v ? { key: k.key, label: k.label, value: humanValue(k.key, v) } : null;
  };
  const sharedKeys = new Set<string>();
  const shared: Fact[] = [];
  for (const k of keys) {
    if (group.items.length < 2) break;
    const first = group.items[0] ? fact(k, group.items[0]) : null;
    if (first && group.items.every((i) => fact(k, i)?.value === first.value)) {
      sharedKeys.add(k.key);
      shared.push(first);
    }
  }
  return {
    shared,
    own: (item) => keys.filter((k) => !sharedKeys.has(k.key)).map((k) => fact(k, item)).filter((f): f is Fact => f !== null),
  };
}
