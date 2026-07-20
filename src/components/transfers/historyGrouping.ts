// History grouping for the unified Transfers list (Transfers Status Model v2,
// §D8). Extracted verbatim from the former `TransfersHistoryTab` so the
// master-detail page can fold "completed" transfers into the same list as the
// live rows — Completed is a filter over one list, not a separate screen. The
// slide-over keeps its own compact (flat) history; this module is the grouped
// page-side model.

import type { Direction, HistoryRow } from '../../types/models';

export interface OutcomeChip {
  key: string;
  label: string;
  /** Design-token text tone class. */
  tone: string;
  /** Raw / explanatory string surfaced on hover; omitted when the label says it all. */
  title?: string;
}

/**
 * Collapse a group's per-outcome counts into honest header chips:
 * - `ingested` + `duplicate` merge into one green `N delivered`. When that set
 *   is entirely `duplicate` the chip reads `N already on peer`.
 * - unconfirmed `sent` start-markers → amber `N awaiting confirmation`.
 * - `rejected` / `failed*` fold into two stable red buckets.
 * - anything else (cancelled, confirmed, replayed, audit rows) passes through.
 */
export function summarizeOutcomeChips(outcomeCounts: Record<string, number>): OutcomeChip[] {
  const chips: OutcomeChip[] = [];

  const ingested = outcomeCounts.ingested ?? 0;
  const duplicate = outcomeCounts.duplicate ?? 0;
  const delivered = ingested + duplicate;
  if (delivered > 0) {
    const allDuplicate = ingested === 0;
    chips.push({
      key: 'delivered',
      label: allDuplicate ? `${delivered} already on peer` : `${delivered} delivered`,
      tone: 'text-success',
      title: allDuplicate ? 'Peer already had every frame — nothing was transferred' : undefined,
    });
  }

  const sent = outcomeCounts.sent ?? 0;
  if (sent > 0) {
    chips.push({
      key: 'sent',
      label: `${sent} awaiting confirmation`,
      tone: 'text-warning',
      title: 'sent — not yet confirmed by the peer',
    });
  }

  let failed = 0;
  let rejected = 0;
  const passthrough: OutcomeChip[] = [];
  for (const [outcome, count] of Object.entries(outcomeCounts)) {
    if (outcome === 'ingested' || outcome === 'duplicate' || outcome === 'sent') continue;
    if (outcome.startsWith('failed')) failed += count;
    else if (outcome.startsWith('rejected')) rejected += count;
    else passthrough.push({ key: outcome, label: `${count} ${outcome}`, tone: passthroughTone(outcome) });
  }
  if (rejected > 0) chips.push({ key: 'rejected', label: `${rejected} rejected`, tone: 'text-error' });
  if (failed > 0) chips.push({ key: 'failed', label: `${failed} failed`, tone: 'text-error' });
  chips.push(...passthrough);

  return chips;
}

function passthroughTone(outcome: string): string {
  if (outcome === 'cancelled') return 'text-content-muted';
  return 'text-success'; // confirmed / replayed / other settled verdicts
}

/**
 * Collapse a group's rows per `frameUuid`: a frame's `sent` start-marker
 * (`finishedAt == null`) is superseded by its settled verdict the moment the
 * receiver's ack lands. Rows arrive newest-first, so the first settled row is
 * the newest verdict; a frame that only has a start-marker keeps the marker.
 */
export function collapseSentGroup(rows: HistoryRow[]): HistoryRow[] {
  const byFrame = new Map<string, HistoryRow[]>();
  const order: string[] = [];
  for (const r of rows) {
    if (!byFrame.has(r.frameUuid)) {
      byFrame.set(r.frameUuid, []);
      order.push(r.frameUuid);
    }
    byFrame.get(r.frameUuid)!.push(r);
  }
  return order.map((uuid) => {
    const frameRows = byFrame.get(uuid)!;
    return frameRows.find((r) => r.finishedAt != null) ?? frameRows[0];
  });
}

/** A batch of `HistoryRow`s sharing the same `(direction, packageId)` key —
 *  rows with `packageId: null` (legacy) all fall into one "earlier" bucket per
 *  direction. */
export interface HistoryGroup {
  groupKey: string;
  packageId: string | null;
  /** The human batch name (§D1), from the first row that carries one; `null` for
   *  legacy rows written before the column existed. */
  batchName: string | null;
  direction: Direction;
  peerDevice: string;
  project: string | null;
  startedAt: string;
  /** `null` while any row in the group is still in flight (`finishedAt == null`). */
  finishedAt: string | null;
  totalBytes: number;
  outcomeCounts: Record<string, number>;
  rows: HistoryRow[];
}

export function groupHistory(rows: HistoryRow[]): HistoryGroup[] {
  const byKey = new Map<string, HistoryRow[]>();
  const order: string[] = [];
  for (const r of rows) {
    const key = `${r.direction}:${r.packageId ?? '__earlier__'}`;
    if (!byKey.has(key)) {
      byKey.set(key, []);
      order.push(key);
    }
    byKey.get(key)!.push(r);
  }
  return order.map((key) => {
    const grouped = collapseSentGroup(byKey.get(key)!);
    const first = grouped[0];
    const totalBytes = grouped.reduce((sum, r) => sum + r.bytes, 0);
    const anyInFlight = grouped.some((r) => !r.finishedAt);
    const finishedAt = anyInFlight
      ? null
      : grouped.reduce<string>((max, r) => (r.finishedAt! > max ? r.finishedAt! : max), grouped[0].finishedAt!);
    const startedAt = grouped.reduce<string>((min, r) => (r.startedAt < min ? r.startedAt : min), first.startedAt);
    const outcomeCounts: Record<string, number> = {};
    for (const r of grouped) outcomeCounts[r.outcome] = (outcomeCounts[r.outcome] ?? 0) + 1;
    // The batch name (§D1) travels on every row of the group; take the first
    // non-null so a legacy start-marker without a name can't blank a named batch.
    const batchName = grouped.find((r) => r.batchName != null)?.batchName ?? null;
    return {
      groupKey: key,
      packageId: first.packageId,
      batchName,
      direction: first.direction,
      peerDevice: first.peerDevice,
      project: first.project,
      startedAt,
      finishedAt,
      totalBytes,
      outcomeCounts,
      rows: grouped,
    };
  });
}

/** Does a completed group carry any failed/rejected frame? Drives the `Failed`
 *  filter membership (§D8: "Failed = ... history groups with failed/rejected
 *  outcomes"). */
export function groupHasFailure(g: HistoryGroup): boolean {
  return Object.keys(g.outcomeCounts).some((o) => o.startsWith('failed') || o.startsWith('rejected'));
}

/** Does a completed group carry any non-failed (delivered/awaiting/cancelled/…)
 *  frame? A group can be BOTH (mixed) — filters are predicates over one list,
 *  so a mixed batch honestly appears under both Completed and Failed. */
export function groupHasSuccess(g: HistoryGroup): boolean {
  return Object.keys(g.outcomeCounts).some((o) => !o.startsWith('failed') && !o.startsWith('rejected'));
}
