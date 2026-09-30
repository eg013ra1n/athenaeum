import type { MemberSummary, OwnFrameRow, ProjectFrameView } from '../../../types/models';
import { BLOCKER_ORDER } from './table/model';
import { formatSize } from '../format';

/** Where a Needs-attention action lands: a My frames segment or a tab,
 * with the table's state facet pre-set. */
export type AttentionTarget =
  | { kind: 'segment'; segment: 'ready' | 'published' | 'held'; state?: string }
  | { kind: 'tab'; tab: 'library' | 'moderation'; state?: string };

export interface AttentionItem {
  key: string;
  tone: 'warn' | 'err';
  count: number;
  title: string;
  detail: string | null;
  action: string; // button label
  target: AttentionTarget;
}

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);
const listNames = (names: string[]) =>
  names.length <= 1 ? (names[0] ?? '') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;

export function offlineFor(iso: string | null, now: number): string {
  const t = iso ? Date.parse(iso) : NaN;
  if (Number.isNaN(t)) return 'a while';
  const h = Math.max(1, Math.round((now - t) / 3_600_000));
  if (h < 24) return `${h} h`;
  const d = Math.round(h / 24);
  return `${d} ${plural(d, 'day', 'days')}`;
}

const CAUSE: Record<string, { title: (n: number, rows: OwnFrameRow[]) => string; detail: string | null; action: string }> = {
  solve: {
    title: (n, rows) => {
      const nights = new Set(rows.map((r) => r.night));
      const when = nights.size === 1 && rows[0].night ? ` from ${rows[0].night}` : '';
      return `${n} ${plural(n, 'frame', 'frames')}${when} ${plural(n, 'is', 'are')} not plate-solved`;
    },
    detail: 'They cannot be published until solved.',
    action: 'Review',
  },
  analyze: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} not analyzed`, detail: 'The gate needs FWHM, eccentricity and stars.', action: 'Analyze' },
  linkCalibration: { title: (n) => `${n} ${plural(n, 'frame has', 'frames have')} no calibration linked`, detail: null, action: 'Review' },
  buildMasters: { title: (n) => `${n} ${plural(n, 'frame waits', 'frames wait')} for masters to be built`, detail: null, action: 'Review' },
  attest: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} not calibrated`, detail: null, action: 'Review' },
  mapFilter: {
    title: (n, rows) => {
      const raws = [...new Set(rows.map((r) => r.filter))];
      return raws.length === 1
        ? `${n} ${plural(n, 'frame', 'frames')} with an unmapped filter “${raws[0]}”`
        : `${n} ${plural(n, 'frame', 'frames')} with unmapped filters`;
    },
    detail: 'Map it once; the gate re-checks them.',
    action: 'Map',
  },
  threshold: { title: (n) => `${n} ${plural(n, 'frame fails', 'frames fail')} the quality thresholds`, detail: null, action: 'Review' },
  uuid: { title: (n) => `${n} ${plural(n, 'frame has', 'frames have')} no frame uuid`, detail: 'Re-scan the folder.', action: 'Review' },
  outsideTarget: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} outside the target`, detail: null, action: 'Review' },
};

export function deriveAttention({ own, library, members, canModerate, pending, now }: {
  own: OwnFrameRow[]; library: ProjectFrameView[]; members: MemberSummary[]; canModerate: boolean; pending: number; now: number;
}): AttentionItem[] {
  const out: AttentionItem[] = [];
  const heldRows = own.filter((r) => r.segment === 'held');
  for (const kind of BLOCKER_ORDER) {
    const rows = heldRows.filter((r) => r.failures[0]?.kind === kind);
    if (rows.length === 0) continue;
    const c = CAUSE[kind];
    if (!c) {
      console.error('[attention] no copy for blocker kind', kind);
      continue;
    }
    out.push({ key: kind, tone: 'warn', count: rows.length, title: c.title(rows.length, rows), detail: c.detail, action: c.action, target: { kind: 'segment', segment: 'held', state: kind } });
  }
  const pubRows = own.filter((r) => r.segment === 'published' && r.accepted !== false);
  const single = pubRows.filter((r) => r.holdersTotal === 0 && r.localState === 'own_held');
  if (single.length > 0) {
    const bytes = single.reduce((a, r) => a + r.byteSize, 0);
    out.push({ key: 'single', tone: 'err', count: single.length, title: `${single.length} published ${plural(single.length, 'frame exists', 'frames exist')} in one copy only · ${formatSize(bytes)}`, detail: 'If that device is lost, the project loses them.', action: 'Show', target: { kind: 'segment', segment: 'published', state: 'single' } });
  }
  const disk = pubRows.filter((r) => r.localState === 'own_missing' || r.localState === 'own_changed');
  if (disk.length > 0) {
    out.push({ key: 'disk', tone: 'err', count: disk.length, title: `${disk.length} published ${plural(disk.length, 'frame is', 'frames are')} missing or changed on this device`, detail: null, action: 'Show', target: { kind: 'segment', segment: 'published', state: 'disk' } });
  }
  const missing = library.filter((f) => !f.own && f.state === 'published' && f.localState === 'wanted' && f.holdersOnline === 0);
  if (missing.length > 0) {
    const offline = [...new Set(missing.map((f) => f.publisherAccountId))]
      .map((id) => members.find((m) => m.accountId === id))
      .filter((m): m is MemberSummary => !!m && !m.online)
      .map((m) => `${m.displayName} offline ${offlineFor(m.lastSeenAt, now)}`);
    out.push({ key: 'missing', tone: 'err', count: missing.length, title: `${missing.length} ${plural(missing.length, 'frame', 'frames')} missing here because ${plural(missing.length, 'its', 'their')} holders are offline`, detail: offline.length ? `${offline.join(', ')}.` : null, action: 'Show', target: { kind: 'tab', tab: 'library', state: 'missing' } });
  }
  if (canModerate && pending > 0) {
    const pubs = [...new Set(library.filter((f) => f.state === 'pending').map((f) => f.publisher))];
    out.push({ key: 'approval', tone: 'warn', count: pending, title: `${pending} ${plural(pending, 'frame waits', 'frames wait')} for your approval`, detail: pubs.length ? `First publications by ${listNames(pubs)}.` : null, action: 'Moderate', target: { kind: 'tab', tab: 'moderation' } });
  }
  return out;
}
