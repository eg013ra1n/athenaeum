import type { MemberSummary, OwnFrameRow, ProjectFrameView } from '../../../types/models';
import { formatSize } from '../format';
import { fromOwn, HELD_KIND_ORDER } from './frames';

/** Where a Needs-attention action lands: a My frames segment or a tab,
 * with the table's state facet pre-set. */
export type AttentionTarget =
  | { kind: 'segment'; segment: 'ready' | 'review' | 'published' | 'held'; state?: string }
  | { kind: 'tab'; tab: 'library' | 'moderation'; state?: string }
  /** Not navigation: Overview opens the filter-mapping dialog itself. */
  | { kind: 'map' };
/** A target that navigates (what the page's `openAttention` takes). */
export type NavTarget = Exclude<AttentionTarget, { kind: 'map' }>;

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
  analyze: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} not analyzed`, detail: 'The gate needs FWHM, eccentricity and stars.', action: 'Review' },
  linkCalibration: { title: (n) => `${n} ${plural(n, 'frame has', 'frames have')} no calibration linked`, detail: null, action: 'Review' },
  buildMasters: { title: (n) => `${n} ${plural(n, 'frame waits', 'frames wait')} for masters to be built`, detail: null, action: 'Review' },
  attest: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} not calibrated`, detail: null, action: 'Review' },
  mapFilter: {
    title: (n, rows) => {
      const raws = [...new Set(rows.map((r) => r.filter))];
      if (raws.length === 1 && raws[0] === '') return `${n} ${plural(n, 'frame', 'frames')} with no FILTER header`;
      return raws.length === 1
        ? `${n} ${plural(n, 'frame', 'frames')} with an unmapped filter “${raws[0]}”`
        : `${n} ${plural(n, 'frame', 'frames')} with unmapped filters`;
    },
    detail: 'Map it once; the gate re-checks them.',
    action: 'Map',
  },
  threshold: { title: (n) => `${n} ${plural(n, 'frame fails', 'frames fail')} the quality thresholds`, detail: null, action: 'Review' },
  uuid: { title: (n) => `${n} ${plural(n, 'frame has', 'frames have')} no frame uuid`, detail: 'Re-scan the folder.', action: 'Review' },
  withheld: {
    title: (n) => `${n} ${plural(n, 'frame', 'frames')} withheld by you`,
    detail: 'They are never published. Release them to publish them.',
    action: 'Review',
  },
  blackHole: {
    title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} in the Black Hole`,
    detail: 'Restore them from the Black Hole to publish them.',
    action: 'Review',
  },
  outsideTarget: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} outside the target`, detail: null, action: 'Review' },
};

export function deriveAttention({ own, library, members, canModerate, canReceive, liveRunning, pending, now }: {
  own: OwnFrameRow[]; library: ProjectFrameView[]; members: MemberSummary[]; canModerate: boolean; canReceive: boolean;
  /** This device's live exchange is running. When it is not, holder
   *  presence reads 0 for everyone, so the holders are not to blame. */
  liveRunning: boolean;
  pending: number; now: number;
}): AttentionItem[] {
  const out: AttentionItem[] = [];
  // Counts equal what the button opens: a row is every held frame whose
  // Reason facet (`fromOwn(r).states`) includes the kind, so a frame with
  // two blockers counts in both rows.
  const review = own.filter((r) => r.segment === 'review').length;
  if (review > 0) {
    out.push({ key: 'review', tone: 'warn', count: review, title: `${review} calibrated ${plural(review, 'frame waits', 'frames wait')} for your review`, detail: 'Blink them, drop the bad ones, then publish.', action: 'Review', target: { kind: 'segment', segment: 'review' } });
  }
  const heldVms = own.filter((r) => r.segment === 'held').map((r) => ({ r, states: fromOwn(r).states }));
  const heldKinds = new Set(heldVms.flatMap((h) => h.states));
  const unknown = [...heldKinds].filter((k) => !(HELD_KIND_ORDER as readonly string[]).includes(k));
  for (const kind of [...HELD_KIND_ORDER, ...unknown]) {
    const rows = heldVms.filter((h) => h.states.includes(kind)).map((h) => h.r);
    if (rows.length === 0) continue;
    const target: AttentionTarget = kind === 'mapFilter' ? { kind: 'map' } : { kind: 'segment', segment: 'held', state: kind };
    const c = CAUSE[kind];
    if (!c) {
      console.error('[attention] unknown held-back kind', kind);
      out.push({ key: kind, tone: 'warn', count: rows.length, title: `${rows.length} ${plural(rows.length, 'frame', 'frames')} held back: ${kind}`, detail: null, action: 'Review', target: { kind: 'segment', segment: 'held', state: kind } });
      continue;
    }
    out.push({ key: kind, tone: 'warn', count: rows.length, title: c.title(rows.length, rows), detail: c.detail, action: c.action, target });
  }
  const pubVms = own.filter((r) => r.segment === 'published').map((r) => ({ r, states: fromOwn(r).states }));
  const single = pubVms.filter((p) => p.states.includes('single')).map((p) => p.r);
  if (single.length > 0) {
    const bytes = single.reduce((a, r) => a + r.byteSize, 0);
    out.push({ key: 'single', tone: 'err', count: single.length, title: `${single.length} published ${plural(single.length, 'frame exists', 'frames exist')} in one copy only · ${formatSize(bytes)}`, detail: 'If that device is lost, the project loses them.', action: 'Show', target: { kind: 'segment', segment: 'published', state: 'single' } });
  }
  const disk = pubVms.filter((p) => p.states.includes('disk')).map((p) => p.r);
  if (disk.length > 0) {
    out.push({ key: 'disk', tone: 'err', count: disk.length, title: `${disk.length} published ${plural(disk.length, 'frame is', 'frames are')} missing or changed on this device`, detail: null, action: 'Show', target: { kind: 'segment', segment: 'published', state: 'disk' } });
  }
  const missing = !canReceive ? [] : library.filter((f) => !f.own && f.state === 'published' && f.localState === 'wanted' && f.holdersOnline === 0);
  if (missing.length > 0) {
    const frames = `${missing.length} ${plural(missing.length, 'frame', 'frames')} missing here`;
    const target: AttentionTarget = { kind: 'tab', tab: 'library', state: 'missing' };
    if (!liveRunning) {
      out.push({ key: 'missing', tone: 'err', count: missing.length, title: frames, detail: 'This device is not connected to the live exchange — they download once it is.', action: 'Show', target });
    } else {
      const offline = [...new Set(missing.map((f) => f.publisherAccountId))]
        .map((id) => members.find((m) => m.accountId === id))
        .filter((m): m is MemberSummary => !!m && !m.online)
        .map((m) => `${m.displayName} offline ${offlineFor(m.lastSeenAt, now)}`);
      out.push({ key: 'missing', tone: 'err', count: missing.length, title: `${frames} because ${plural(missing.length, 'its', 'their')} holders are offline`, detail: offline.length ? `${offline.join(', ')}.` : null, action: 'Show', target });
    }
  }
  if (canModerate && pending > 0) {
    const pubs = [...new Set(library.filter((f) => !f.own && f.state === 'pending').map((f) => f.publisher))];
    out.push({ key: 'approval', tone: 'warn', count: pending, title: `${pending} ${plural(pending, 'frame waits', 'frames wait')} for your approval`, detail: pubs.length ? `First publications by ${listNames(pubs)}.` : null, action: 'Moderate', target: { kind: 'tab', tab: 'moderation' } });
  }
  return out;
}
