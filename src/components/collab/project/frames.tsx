import type { ReactNode } from 'react';
import type { ModerationFrameView, OwnFrameRow, ProjectFrameView } from '../../../types/models';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { getFilterColor } from '../../../utils/filterColors';
import { formatBytes, formatDuration } from '../format';
import {
  alphaOrder, BLOCKER_ORDER, filterOrder, median, nightOrderDesc, reasonOrder, sum,
  type ColumnDef, type FacetAccess, type GroupDef,
} from './table/model';

/* ── View-model ─────────────────────────────────────────────────────────── */

export type TableId = 'ready' | 'held' | 'published' | 'library' | 'moderation' | 'excluded';

export type DeviceState =
  | 'have' | 'downloading' | 'queued' | 'missing' | 'notKept' | 'needsChoice' | 'changed' | 'notReplicated';

export interface FrameVM {
  key: string; // frameUuid, else `id:<frameId>`
  frameId: number | null;
  frameUuid: string | null;
  setId: number | null;
  setName: string | null;
  fileName: string;
  night: string | null;
  filter: string;
  filterMapped: boolean;
  camera: string; // '' = unknown
  publisher: string | null; // display name
  publisherAccountId: string | null;
  exptimeSec: number | null;
  byteSize: number | null;
  fwhm: number | null;
  ecc: number | null;
  stars: number | null;
  snr: number | null;
  failures: { kind: string; text: string }[];
  contentVersion: number | null;
  pubState: string | null; // pending | published | rejected
  excluded: boolean; // accepted === false (coordinator exclusion)
  acceptedReason: string | null;
  holdersOnline: number | null;
  holdersTotal: number | null; // OTHER member devices (core doc) — see copies()
  disk: 'on' | 'missing' | 'changed' | null; // own published only
  publishedAt: string | null;
  device: DeviceState | null; // library only
  missingWhy: 'holder offline' | 'publisher offline' | 'gone from disk' | null;
  progress: number | null; // 0..100 while downloading
  submittedAt: string | null; // moderation only
  states: string[]; // this table's state-facet values
  own: OwnFrameRow | null;
  lib: ProjectFrameView | null;
  mod: ModerationFrameView | null;
}

/** The `FrameVM.key` for an own-frame row — the single source of truth for
 * `fromOwn`'s own key and for callers that need to match a stale VM back to
 * its fresh row (e.g. re-deriving an open drawer's frame after a reload). */
export function ownFrameKey(r: Pick<OwnFrameRow, 'frameUuid' | 'frameId'>): string {
  return r.frameUuid ?? `id:${r.frameId}`;
}

/** Own frame → view-model. `disk` and the segment-dependent `states` are
 * derived here; `copies()` (below) is used for the "single copy" state so
 * the two never drift apart. */
export function fromOwn(r: OwnFrameRow): FrameVM {
  const disk: FrameVM['disk'] =
    r.localState === 'own_held' ? 'on'
      : r.localState === 'own_missing' ? 'missing'
        : r.localState === 'own_changed' ? 'changed'
          : null;
  const excluded = r.accepted === false;

  const vm: FrameVM = {
    key: ownFrameKey(r),
    frameId: r.frameId,
    frameUuid: r.frameUuid,
    setId: r.setId,
    setName: r.setName,
    fileName: r.fileName,
    night: r.night,
    filter: r.filter,
    filterMapped: r.filterMapped,
    camera: r.camera ?? '',
    publisher: null,
    publisherAccountId: null,
    exptimeSec: r.exptimeSec,
    byteSize: r.byteSize,
    fwhm: r.fwhmArcsec,
    ecc: r.eccentricity,
    stars: r.starsDetected,
    snr: r.medianSnr,
    failures: r.failures,
    contentVersion: r.contentVersion,
    pubState: r.pubState,
    excluded,
    acceptedReason: r.acceptedReason,
    holdersOnline: r.holdersOnline,
    holdersTotal: r.holdersTotal,
    disk,
    publishedAt: r.publishedAt,
    device: null,
    missingWhy: null,
    progress: null,
    submittedAt: null,
    states: [],
    own: r,
    lib: null,
    mod: null,
  };

  if (r.segment === 'held') {
    vm.states = Array.from(new Set(r.failures.map((f) => f.kind)));
  } else if (r.segment === 'published') {
    const states = [excluded ? 'excluded' : (r.pubState ?? 'published')];
    if (copies(vm) === 1) states.push('single');
    if (disk === 'missing' || disk === 'changed') states.push('disk');
    vm.states = states;
  }
  return vm;
}

/** A library (another member's) frame → view-model. `device` mirrors this
 * device's replication state for the frame; `inFlight` keys on `frameUuid`. */
export function fromLibrary(r: ProjectFrameView, inFlight: ReadonlyMap<string, { done: number; size: number }>): FrameVM {
  let device: DeviceState | null;
  let progress: number | null = null;
  // Holder-offline / publisher-offline are reasons for the `wanted` route
  // ONLY — core's `LocalState::Missing` means "was on disk, isn't any more"
  // (GC'd, deleted, disk truth found it gone), a different fact from "we
  // want it but nobody online has it", so it never borrows that route's
  // reasons.
  let missingWhy: FrameVM['missingWhy'] = null;
  switch (r.localState) {
    case 'held':
      device = 'have';
      break;
    case 'wanted': {
      const fl = inFlight.get(r.frameUuid);
      if (fl) {
        device = 'downloading';
        progress = fl.size > 0 ? Math.min(100, Math.max(0, Math.round((fl.done / fl.size) * 100))) : 0;
      } else if (r.waitingForPublisher || r.holdersOnline === 0) {
        device = 'missing';
        missingWhy = r.waitingForPublisher ? 'publisher offline' : 'holder offline';
      } else {
        device = 'queued';
      }
      break;
    }
    case 'missing':
      device = 'missing';
      missingWhy = 'gone from disk';
      break;
    case 'not_kept':
      device = 'notKept';
      break;
    case 'awaiting_choice':
      device = 'needsChoice';
      break;
    case 'quarantined':
      device = 'changed';
      break;
    case 'idle':
      device = 'notReplicated';
      break;
    default:
      device = null; // own_held / own_missing / own_changed
  }

  return {
    key: r.frameUuid,
    frameId: null,
    frameUuid: r.frameUuid,
    setId: null,
    setName: null,
    fileName: r.fileName,
    night: r.night,
    filter: r.filter,
    filterMapped: true,
    camera: r.camera ?? '',
    publisher: r.publisher,
    publisherAccountId: r.publisherAccountId,
    exptimeSec: r.exptimeSec,
    byteSize: r.byteSize,
    fwhm: r.fwhmArcsec,
    ecc: r.eccentricity,
    stars: r.starsDetected,
    snr: r.medianSnr,
    failures: [],
    contentVersion: r.contentVersion,
    pubState: r.state,
    excluded: !r.accepted,
    acceptedReason: r.acceptedReason,
    holdersOnline: r.holdersOnline,
    holdersTotal: r.holdersTotal,
    disk: null,
    publishedAt: null,
    device,
    missingWhy,
    progress,
    submittedAt: null,
    states: device !== null ? [device] : [],
    own: null,
    lib: r,
    mod: null,
  };
}

/** A moderation-queue row → view-model. The pending view carries only
 * filter/exp/FWHM/created; every other metric is filled from the manifest
 * mirror (`byUuid`) when it already has the frame, else null/''. */
export function fromModeration(m: ModerationFrameView, byUuid: ReadonlyMap<string, ProjectFrameView>): FrameVM {
  const mirror = byUuid.get(m.frameUuid) ?? null;
  return {
    key: m.frameUuid,
    frameId: null,
    frameUuid: m.frameUuid,
    setId: null,
    setName: null,
    fileName: m.fileName,
    night: mirror?.night ?? null,
    filter: m.filter,
    filterMapped: true,
    camera: mirror?.camera ?? '',
    publisher: m.publisher,
    publisherAccountId: m.publisherAccountId,
    exptimeSec: m.exptimeSec,
    byteSize: mirror?.byteSize ?? null,
    fwhm: m.fwhmArcsec,
    ecc: mirror?.eccentricity ?? null,
    stars: mirror?.starsDetected ?? null,
    snr: mirror?.medianSnr ?? null,
    failures: [],
    contentVersion: null,
    pubState: 'pending',
    excluded: false,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    disk: null,
    publishedAt: null,
    device: null,
    missingWhy: null,
    progress: null,
    submittedAt: m.createdAt,
    states: [],
    own: null,
    lib: null,
    mod: m,
  };
}

/** Total copies of the frame in the project, counting this device when it
 * holds one: own published → `holdersTotal + (disk === 'on')`; library →
 * `holdersTotal + (device === 'have')`; `null` when `holdersTotal` is null. */
export function copies(vm: FrameVM): number | null {
  if (vm.holdersTotal === null) return null;
  if (vm.own) return vm.holdersTotal + (vm.disk === 'on' ? 1 : 0);
  return vm.holdersTotal + (vm.device === 'have' ? 1 : 0);
}

/* ── Labels ─────────────────────────────────────────────────────────────── */

export const REASON_LABEL: Record<string, string> = {
  analyze: 'No analysis',
  solve: 'No coordinates or pixel scale',
  linkCalibration: 'Not calibrated — no calibration linked',
  buildMasters: 'Not calibrated — masters not built',
  attest: 'Not calibrated',
  mapFilter: 'Filter needs a mapping',
  threshold: 'Quality thresholds',
  uuid: 'No frame uuid — re-scan the folder',
  outsideTarget: 'Outside the target',
};

export const DEVICE_LABEL: Record<DeviceState, string> = {
  have: 'Have',
  downloading: 'Downloading',
  queued: 'Queued',
  missing: 'Missing',
  notKept: 'Not kept',
  needsChoice: 'Needs your choice',
  changed: 'Changed',
  notReplicated: 'Not replicated',
};

/* ── Small rendering helpers ────────────────────────────────────────────── */

type Tone = 'success' | 'warning' | 'error' | 'muted';

/** One chip-class helper for every colored chip in these tables — status,
 * holders, disk, device-missing, the held-back reason. */
function chip(tone: Tone): string {
  const tones: Record<Tone, string> = {
    success: 'bg-success/20 text-success',
    warning: 'bg-warning/20 text-warning',
    error: 'bg-error/20 text-error',
    muted: 'bg-surface-hover text-content-muted',
  };
  return `rounded px-1.5 py-0.5 text-[10px] font-medium ${tones[tone]}`;
}

function dash(): ReactNode {
  return <span className="text-content-muted">—</span>;
}

function num(text: string): ReactNode {
  return <span className="tabular-nums">{text}</span>;
}

function filterDot(f: string): ReactNode {
  return <span className="mr-1 inline-block h-2 w-2 rounded-full" style={{ backgroundColor: getFilterColor(f) }} />;
}

function statusTone(s: string): Tone {
  switch (s) {
    case 'published': return 'success';
    case 'rejected': return 'error';
    case 'excluded': return 'muted';
    case 'pending':
    default: return 'warning';
  }
}

/** The Status column and the `status` group read `excluded` before `pubState`. */
function effectiveStatus(v: FrameVM): string {
  return v.excluded ? 'excluded' : (v.pubState ?? 'published');
}

/* ── Columns ────────────────────────────────────────────────────────────── */

export const COLUMNS: Record<string, ColumnDef<FrameVM>> = {
  name: {
    id: 'name',
    label: 'Frame',
    width: 260,
    value: (v) => v.fileName,
    cell: (v) => (
      <span className="font-mono text-xs">
        {v.fileName}
        {v.excluded && <span title={v.acceptedReason ?? undefined} className={`ml-1.5 ${chip('muted')}`}>excluded</span>}
      </span>
    ),
  },
  publisher: {
    id: 'publisher',
    label: 'Publisher',
    width: 108,
    value: (v) => v.publisher,
    cell: (v) => v.publisher ?? dash(),
    renderAggregate: (rows) => {
      const names = new Set(rows.map((r) => r.publisher ?? ''));
      if (names.size <= 1) return rows[0]?.publisher ?? dash();
      return `${names.size} members`;
    },
  },
  night: {
    id: 'night',
    label: 'Night',
    width: 98,
    value: (v) => v.night,
    cell: (v) => v.night ?? dash(),
    renderAggregate: (rows) => {
      const nights = Array.from(new Set(rows.map((r) => r.night ?? '')));
      if (nights.length <= 1) return nights[0] || dash();
      return `${nights.length} nights`;
    },
  },
  filter: {
    id: 'filter',
    label: 'Filter',
    width: 70,
    value: (v) => v.filter,
    cell: (v) => <span>{filterDot(v.filter)}{v.filter}</span>,
    renderAggregate: (rows) => {
      const filters = Array.from(new Set(rows.map((r) => r.filter)));
      if (filters.length <= 1) {
        const f = filters[0] ?? '';
        return <span>{filterDot(f)}{f}</span>;
      }
      return `${filters.length} filters`;
    },
  },
  camera: {
    id: 'camera',
    label: 'Camera',
    width: 122,
    value: (v) => v.camera,
    cell: (v) => <span>{v.camera === '' ? 'Unknown camera' : v.camera}</span>,
    renderAggregate: (rows) => {
      const cams = Array.from(new Set(rows.map((r) => r.camera)));
      if (cams.length <= 1) {
        const c = cams[0] ?? '';
        return <span>{c === '' ? 'Unknown camera' : c}</span>;
      }
      return `${cams.length} cameras`;
    },
  },
  exp: {
    id: 'exp',
    label: 'Exp / Σ',
    width: 82,
    numeric: true,
    value: (v) => v.exptimeSec,
    cell: (v) => (v.exptimeSec === null ? dash() : num(`${v.exptimeSec}s`)),
    aggregate: (rows) => sum(rows.map((r) => r.exptimeSec)),
    renderAggregate: (rows) => num(formatDuration(sum(rows.map((r) => r.exptimeSec)))),
  },
  fwhm: {
    id: 'fwhm',
    label: 'FWHM″',
    width: 66,
    numeric: true,
    value: (v) => v.fwhm,
    cell: (v) => (v.fwhm === null ? dash() : num(v.fwhm.toFixed(2))),
    aggregate: (rows) => median(rows.map((r) => r.fwhm)),
    renderAggregate: (rows) => {
      const m = median(rows.map((r) => r.fwhm));
      return m === null ? dash() : num(`x̃ ${m.toFixed(2)}`);
    },
  },
  ecc: {
    id: 'ecc',
    label: 'Ecc',
    width: 60,
    numeric: true,
    value: (v) => v.ecc,
    cell: (v) => (v.ecc === null ? dash() : num(v.ecc.toFixed(2))),
    aggregate: (rows) => median(rows.map((r) => r.ecc)),
    renderAggregate: (rows) => {
      const m = median(rows.map((r) => r.ecc));
      return m === null ? dash() : num(`x̃ ${m.toFixed(2)}`);
    },
  },
  stars: {
    id: 'stars',
    label: 'Stars',
    width: 64,
    numeric: true,
    value: (v) => v.stars,
    cell: (v) => (v.stars === null ? dash() : num(v.stars.toLocaleString('en-US'))),
    aggregate: (rows) => median(rows.map((r) => r.stars)),
    renderAggregate: (rows) => {
      const m = median(rows.map((r) => r.stars));
      return m === null ? dash() : num(`x̃ ${Math.round(m).toLocaleString('en-US')}`);
    },
  },
  snr: {
    id: 'snr',
    label: 'SNR',
    width: 58,
    numeric: true,
    value: (v) => v.snr,
    cell: (v) => (v.snr === null ? dash() : num(v.snr.toFixed(1))),
    aggregate: (rows) => median(rows.map((r) => r.snr)),
    renderAggregate: (rows) => {
      const m = median(rows.map((r) => r.snr));
      return m === null ? dash() : num(`x̃ ${m.toFixed(1)}`);
    },
  },
  size: {
    id: 'size',
    label: 'Size',
    width: 78,
    numeric: true,
    value: (v) => v.byteSize,
    cell: (v) => (v.byteSize === null ? dash() : num(formatBytes(v.byteSize))),
    aggregate: (rows) => sum(rows.map((r) => r.byteSize)),
    renderAggregate: (rows) => num(formatBytes(sum(rows.map((r) => r.byteSize)))),
  },
  reason: {
    id: 'reason',
    label: 'Why held back',
    width: 230,
    value: (v) => v.failures[0]?.kind ?? null,
    cell: (v) => {
      const first = v.failures[0];
      if (!first) return dash();
      const tone: Tone = first.kind === 'threshold' ? 'error' : 'warning';
      return (
        <span>
          <span className={chip(tone)}>{first.text}</span>
          {v.failures.length > 1 && <span className="ml-1.5 text-[11px] text-content-muted">+{v.failures.length - 1}</span>}
        </span>
      );
    },
    renderAggregate: (rows) => {
      const kinds = new Set(rows.map((r) => r.failures[0]?.kind ?? ''));
      return kinds.size > 1 ? <span className="text-content-muted">{kinds.size} causes</span> : null;
    },
  },
  version: {
    id: 'version',
    label: 'Ver',
    width: 44,
    numeric: true,
    value: (v) => v.contentVersion,
    cell: (v) => (v.contentVersion === null ? dash() : num(`v${v.contentVersion}`)),
  },
  status: {
    id: 'status',
    label: 'Status',
    width: 112,
    value: (v) => effectiveStatus(v),
    cell: (v) => {
      const s = effectiveStatus(v);
      return <span title={v.excluded ? (v.acceptedReason ?? undefined) : undefined} className={chip(statusTone(s))}>{s}</span>;
    },
    renderAggregate: (rows) => {
      const counts = new Map<string, number>();
      for (const r of rows) {
        const s = effectiveStatus(r);
        counts.set(s, (counts.get(s) ?? 0) + 1);
      }
      const nonPublished = [...counts.entries()].filter(([k]) => k !== 'published');
      if (nonPublished.length === 0) {
        return <span className={chip('success')}>{counts.get('published') ?? 0} published</span>;
      }
      return (
        <span className="inline-flex gap-1">
          {nonPublished.map(([k, n]) => <span key={k} className={chip(statusTone(k))}>{n} {k}</span>)}
        </span>
      );
    },
  },
  holders: {
    id: 'holders',
    label: 'Holders',
    width: 88,
    numeric: true,
    value: (v) => copies(v),
    cell: (v) => {
      const c = copies(v);
      if (c === null) return dash();
      if (c === 1) return <span title="Only one copy in the project" className={chip('warning')}>1 copy</span>;
      return num(`${v.holdersOnline ?? 0} on / ${c}`);
    },
    aggregate: (rows) => {
      const vals = rows.map(copies).filter((x): x is number => x !== null);
      return vals.length ? Math.min(...vals) : null;
    },
    renderAggregate: (rows) => {
      const singles = rows.filter((r) => copies(r) === 1).length;
      if (singles > 0) return <span className={chip('warning')}>{singles} single</span>;
      const vals = rows.map(copies).filter((x): x is number => x !== null);
      return vals.length ? num(`min ${Math.min(...vals)}`) : dash();
    },
  },
  disk: {
    id: 'disk',
    label: 'On disk',
    width: 84,
    value: (v) => v.disk,
    cell: (v) => {
      if (v.disk === 'on') return <span className="text-content-muted">yes</span>;
      if (v.disk === 'missing') return <span className={chip('error')}>missing</span>;
      if (v.disk === 'changed') return <span className={chip('warning')}>changed</span>;
      return dash();
    },
    renderAggregate: (rows) => {
      const n = rows.filter((r) => r.disk === 'missing' || r.disk === 'changed').length;
      return n > 0 ? <span className={chip('error')}>{n}</span> : null;
    },
  },
  publishedAt: {
    id: 'publishedAt',
    label: 'Published',
    width: 142,
    value: (v) => v.publishedAt,
    cell: (v) => (v.publishedAt === null ? dash() : <span className="text-content-muted">{formatTimestamp(v.publishedAt)}</span>),
  },
  device: {
    id: 'device',
    label: 'On this device',
    width: 168,
    value: (v) => v.device,
    cell: (v) => {
      switch (v.device) {
        case 'have':
          return <span className="text-success">● have</span>;
        case 'downloading':
          return (
            <span className="inline-flex w-full items-center gap-1.5">
              <span className="h-1 flex-1 overflow-hidden rounded bg-surface-hover">
                <span className="block h-full bg-accent" style={{ width: `${v.progress ?? 0}%` }} />
              </span>
              <span className="text-accent">{v.progress ?? 0}%</span>
            </span>
          );
        case 'queued':
          return <span className="text-content-muted">○ queued</span>;
        case 'missing':
          return (
            <span>
              <span title={v.missingWhy ?? undefined} className={chip('error')}>missing</span>
              {v.missingWhy && <span className="ml-1.5 text-[11px] text-content-muted">{v.missingWhy}</span>}
            </span>
          );
        case 'notKept':
          return <span className="text-content-muted">not kept</span>;
        case 'needsChoice':
          return <span className="text-warning">needs your choice</span>;
        case 'changed':
          return <span className="text-warning">changed</span>;
        case 'notReplicated':
          return <span className="text-content-muted">not replicated</span>;
        default:
          return dash();
      }
    },
    renderAggregate: (rows) => {
      const counts: Record<string, number> = { have: 0, downloading: 0, queued: 0, missing: 0, notKept: 0 };
      for (const r of rows) if (r.device && r.device in counts) counts[r.device] += 1;
      const total = rows.length || 1;
      const seg = (n: number, cls: string, key: string) =>
        n > 0 ? <i key={key} className={`block h-full ${cls}`} style={{ width: `${(n / total) * 100}%` }} /> : null;
      let text: ReactNode;
      if (counts.missing > 0) text = <span className="text-error">{counts.missing} missing</span>;
      else if (counts.downloading + counts.queued > 0) text = <span className="text-accent">{counts.downloading + counts.queued} to go</span>;
      else text = <span className="text-content-muted">{counts.have}/{rows.length}</span>;
      return (
        <span className="inline-flex w-full items-center gap-1.5">
          <span className="flex h-1.5 flex-1 overflow-hidden rounded bg-surface-hover">
            {seg(counts.have, 'bg-success', 'have')}
            {seg(counts.downloading, 'bg-accent', 'downloading')}
            {seg(counts.queued, 'bg-accent-muted', 'queued')}
            {seg(counts.missing, 'bg-error', 'missing')}
            {seg(counts.notKept, 'border border-border', 'notKept')}
          </span>
          <span className="text-[11px]">{text}</span>
        </span>
      );
    },
  },
  submitted: {
    id: 'submitted',
    label: 'Submitted',
    width: 142,
    value: (v) => v.submittedAt,
    cell: (v) => (v.submittedAt === null ? dash() : <span className="text-content-muted">{formatTimestamp(v.submittedAt)}</span>),
  },
  exclusion: {
    id: 'exclusion',
    label: 'Why excluded',
    width: 240,
    value: (v) => v.acceptedReason,
    cell: (v) => v.acceptedReason ?? dash(),
  },
};

/* ── Groups ─────────────────────────────────────────────────────────────── */

const WEEKDAY = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

export const GROUPS: Record<string, GroupDef<FrameVM>> = {
  night: {
    id: 'night',
    label: 'Night',
    key: (v) => v.night ?? '',
    renderLabel: (k) => {
      if (k === '') return <span>Unknown night</span>;
      const wd = WEEKDAY[new Date(`${k}T00:00:00Z`).getUTCDay()];
      return <span>{k} · {wd}</span>;
    },
    order: nightOrderDesc,
  },
  filter: {
    id: 'filter',
    label: 'Filter',
    key: (v) => v.filter,
    renderLabel: (k) => <span>{filterDot(k)}{k}</span>,
    order: filterOrder,
  },
  camera: {
    id: 'camera',
    label: 'Camera',
    key: (v) => v.camera,
    renderLabel: (k) => <span>{k === '' ? 'Unknown camera' : k}</span>,
    order: alphaOrder,
  },
  object: {
    id: 'object',
    label: 'Object',
    key: (v) => v.setName ?? '',
    renderLabel: (k) => <span>{k === '' ? 'No object' : k}</span>,
    order: alphaOrder,
  },
  reason: {
    id: 'reason',
    label: 'Reason',
    key: (v) => v.failures[0]?.kind ?? '',
    renderLabel: (k) => REASON_LABEL[k] ?? k,
    order: reasonOrder,
  },
  status: {
    id: 'status',
    label: 'Status',
    key: (v) => effectiveStatus(v),
    renderLabel: (k) => <span className={chip(statusTone(k))}>{k}</span>,
    order: alphaOrder,
  },
  publisher: {
    id: 'publisher',
    label: 'Publisher',
    key: (v) => v.publisher ?? '',
    renderLabel: (k) => <span>{k === '' ? 'Unknown publisher' : k}</span>,
    order: alphaOrder,
  },
  none: {
    id: 'none',
    label: 'None',
    key: () => '',
    renderLabel: () => <span>None</span>,
    order: alphaOrder,
  },
};

/* ── Facet access ───────────────────────────────────────────────────────── */

export const FRAME_ACCESS: FacetAccess<FrameVM> = {
  name: (v) => v.fileName,
  filter: (v) => v.filter,
  camera: (v) => v.camera,
  night: (v) => v.night,
  publisher: (v) => v.publisher,
  states: (v) => v.states,
};

/* ── Per-tab table configs (spec §4.2, ZP dropped) ─────────────────────── */

export interface TableConfig {
  id: TableId;
  columns: string[];
  defaultColumns: string[];
  groupings: string[];
  defaultGrouping: [string, string];
  stateFacet: { label: string; options: [string, string][] } | null;
  publisherFacet: boolean;
}

export const TABLES: Record<TableId, TableConfig> = {
  ready: {
    id: 'ready',
    columns: ['name', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'size'],
    defaultColumns: ['name', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'size'],
    groupings: ['night', 'filter', 'camera', 'object', 'none'],
    defaultGrouping: ['night', 'filter'],
    stateFacet: null,
    publisherFacet: false,
  },
  held: {
    id: 'held',
    columns: ['name', 'reason', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'size'],
    defaultColumns: ['name', 'reason', 'night', 'filter', 'exp', 'fwhm', 'ecc', 'stars'],
    groupings: ['reason', 'night', 'filter', 'camera', 'object', 'none'],
    defaultGrouping: ['reason', 'night'],
    stateFacet: { label: 'Reason', options: BLOCKER_ORDER.map((k): [string, string] => [k, REASON_LABEL[k]]) },
    publisherFacet: false,
  },
  published: {
    id: 'published',
    columns: ['name', 'night', 'filter', 'camera', 'exp', 'version', 'status', 'holders', 'disk', 'publishedAt', 'fwhm', 'ecc', 'size'],
    defaultColumns: ['name', 'filter', 'exp', 'version', 'status', 'holders', 'disk', 'publishedAt', 'size'],
    groupings: ['night', 'filter', 'status', 'camera', 'none'],
    defaultGrouping: ['night', 'filter'],
    stateFacet: {
      label: 'Status',
      options: [
        ['published', 'Published'], ['pending', 'Pending approval'], ['rejected', 'Rejected'],
        ['excluded', 'Excluded'], ['single', 'Only one copy'], ['disk', 'Not on disk / changed'],
      ],
    },
    publisherFacet: false,
  },
  library: {
    id: 'library',
    columns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'holders', 'device', 'size'],
    defaultColumns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'holders', 'device', 'size'],
    groupings: ['publisher', 'filter', 'camera', 'night', 'none'],
    defaultGrouping: ['publisher', 'filter'],
    stateFacet: {
      label: 'On this device',
      options: (['have', 'downloading', 'queued', 'missing', 'notKept', 'needsChoice', 'changed', 'notReplicated'] as DeviceState[])
        .map((k): [string, string] => [k, DEVICE_LABEL[k]]),
    },
    publisherFacet: true,
  },
  moderation: {
    id: 'moderation',
    columns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'size', 'submitted'],
    defaultColumns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr'],
    groupings: ['publisher', 'night', 'filter', 'camera', 'none'],
    defaultGrouping: ['publisher', 'night'],
    stateFacet: null,
    publisherFacet: true,
  },
  excluded: {
    id: 'excluded',
    columns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'exclusion', 'fwhm', 'ecc', 'size'],
    defaultColumns: ['name', 'publisher', 'night', 'filter', 'exp', 'exclusion'],
    groupings: ['publisher', 'night', 'filter', 'camera', 'none'],
    defaultGrouping: ['publisher', 'none'],
    stateFacet: null,
    publisherFacet: true,
  },
};
