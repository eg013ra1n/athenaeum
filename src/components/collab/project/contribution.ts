import type { OwnFrameRow } from '../../../types/models';
import type { Segment } from './MyFramesTab';
import { filterOrder } from './table/model';
import { HELD_KIND_ORDER, REASON_LABEL } from './frames';

export interface ContributionTile {
  segment: Segment;
  count: number;
  /** Σ exptimeSec (null rows add 0). */
  seconds: number;
  /** Distinct non-null `night`. */
  nights: number;
  /** Raw byteSize for ready/held, calibratedBytes ?? byteSize for review/published. */
  bytes: number;
  filters: { filter: string; seconds: number }[];
  footer: string;
}

const ORDER: Segment[] = ['ready', 'review', 'published', 'held'];

/** Spec 2026-10-01 §7.2 — the four My contribution tiles, derived from own rows. */
export function contributionTiles(own: OwnFrameRow[]): ContributionTile[] {
  return ORDER.map((segment) => {
    const rows = own.filter((r) => r.segment === segment);
    const seconds = rows.reduce((s, r) => s + (r.exptimeSec ?? 0), 0);
    const nights = new Set(rows.map((r) => r.night).filter((n): n is string => !!n)).size;
    const calibrated = segment === 'review' || segment === 'published';
    const bytes = rows.reduce((s, r) => s + (calibrated ? (r.calibratedBytes ?? r.byteSize) : r.byteSize), 0);
    // An unset FILTER stays the raw '' the project tables show for it.
    const perFilter = new Map<string, number>();
    for (const r of rows) perFilter.set(r.filter, (perFilter.get(r.filter) ?? 0) + (r.exptimeSec ?? 0));
    const filters = [...perFilter.entries()]
      .map(([filter, s]) => ({ filter, seconds: s }))
      .sort((a, b) => filterOrder(a.filter, b.filter));
    return { segment, count: rows.length, seconds, nights, bytes, filters, footer: footerOf(segment, rows) };
  });
}

function kindRank(k: string): number {
  const i = (HELD_KIND_ORDER as readonly string[]).indexOf(k);
  return i === -1 ? HELD_KIND_ORDER.length : i;
}

function footerOf(segment: Segment, rows: OwnFrameRow[]): string {
  switch (segment) {
    case 'ready':
      return 'Calibrate →';
    case 'review':
      return 'Review and publish →';
    case 'published': {
      const accepted = rows.filter((r) => r.pubState === 'published' && r.accepted !== false).length;
      const pending = rows.filter((r) => r.pubState === 'pending').length;
      return `${accepted} accepted · ${pending} pending`;
    }
    case 'held': {
      const byKind = new Map<string, number>();
      for (const r of rows) {
        const k = r.failures[0]?.kind ?? 'threshold';
        byKind.set(k, (byKind.get(k) ?? 0) + 1);
      }
      // Count descending; ties by the held-kind order, unknown kinds last (ledger P1).
      return [...byKind.entries()]
        .sort((a, b) => b[1] - a[1] || kindRank(a[0]) - kindRank(b[0]))
        .slice(0, 3)
        .map(([k, n]) => `${n} ${(REASON_LABEL[k] ?? k).toLowerCase()}`)
        .join(' · ');
    }
  }
}
