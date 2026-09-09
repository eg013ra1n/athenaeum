import { useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, ArrowUp, ArrowDown } from 'lucide-react';
import type { StackingRunDetail, StackingRunFrameRow, SummaryFrame } from '../../types/stacking';

/** The set's LIGHT frames, as `FrameSetDetail.tsx` derives them from its own
 *  `detail.nights` tree — plan 5b Task 4, Decisions item 3. This is the row
 *  source before any run has been recorded, and the filename fallback for a
 *  run row a summary hasn't reached yet (`StackingRunFrameRow` itself carries
 *  no filename — only `SummaryFrame`, which doesn't exist until the run's
 *  summary is written). */
export interface LightFrameRef {
  frameId: number;
  filename: string;
}

type StatusTone = 'success' | 'warning' | 'error';

interface FrameRow {
  frameId: number;
  filename: string;
  group: string;
  weight: number | null;
  fwhmPx: number | null;
  eccentricity: number | null;
  stars: number | null;
  regRmsPx: number | null;
  regInliers: number | null;
  statusLabel: string;
  statusTone: StatusTone;
}

type SortKey =
  | 'filename'
  | 'group'
  | 'weight'
  | 'fwhmPx'
  | 'eccentricity'
  | 'stars'
  | 'regRmsPx'
  | 'regInliers'
  | 'statusLabel';

const COLUMNS: { key: SortKey; label: string; numeric?: boolean }[] = [
  { key: 'filename', label: 'Filename' },
  { key: 'group', label: 'Group' },
  { key: 'weight', label: 'Weight', numeric: true },
  { key: 'fwhmPx', label: 'FWHM', numeric: true },
  { key: 'eccentricity', label: 'Ecc', numeric: true },
  { key: 'stars', label: 'Seeds', numeric: true },
  { key: 'regRmsPx', label: 'Reg RMS', numeric: true },
  { key: 'regInliers', label: 'Inliers', numeric: true },
  { key: 'statusLabel', label: 'Status' },
];

const STATUS_CHIP: Record<StatusTone, string> = {
  success: 'bg-success-muted text-success',
  warning: 'bg-warning-muted text-warning',
  error: 'bg-error-muted text-error',
};

function fmtNum(n: number | null, digits: number, suffix = ''): string {
  return n == null ? '—' : `${n.toFixed(digits)}${suffix}`;
}

function fmtInt(n: number | null): string {
  return n == null ? '—' : String(n);
}

/** Rows before any run exists: the plan's LIGHT frames, group unknown
 *  ("—" — no run has assigned one yet), status derived only from the
 *  manual-exclusion list (Ruling 7 — the checkbox's own state). */
function preRunRows(lightFrames: LightFrameRef[], excludedSet: Set<number>): FrameRow[] {
  return lightFrames.map((f) => {
    const excluded = excludedSet.has(f.frameId);
    return {
      frameId: f.frameId,
      filename: f.filename,
      group: '—',
      weight: null,
      fwhmPx: null,
      eccentricity: null,
      stars: null,
      regRmsPx: null,
      regInliers: null,
      statusLabel: excluded ? 'Excluded (manual)' : 'Included',
      statusTone: excluded ? 'warning' : 'success',
    };
  });
}

/** Rows once a run has been selected: `StackingRunDetail.frames` (present
 *  throughout the run) joined with `summary.groups[].frames[]`
 *  (`SummaryFrame`, `null` until the run finishes — Decisions item 1) for
 *  the metrics that only the finished summary carries (FWHM/eccentricity/
 *  seeds — `StackingRunFrameRow` itself has no columns for those). Status
 *  chip precedence matches the brief's tokens: a registration failure wins
 *  over a plain exclusion (a failed-registration frame is always also
 *  `included = false`, so checking `included` first would relabel it a
 *  generic exclusion and lose the more specific reason). */
function postRunRows(
  runDetail: StackingRunDetail,
  lightFrames: LightFrameRef[],
  excludedSet: Set<number>,
): FrameRow[] {
  const filenameByFrameId = new Map(lightFrames.map((f) => [f.frameId, f.filename]));
  const groupKeyById = new Map(runDetail.groups.map((g) => [g.id, g.groupKey]));
  const summaryByFrameId = new Map<number, SummaryFrame>();
  if (runDetail.summary) {
    for (const g of runDetail.summary.groups) {
      for (const f of g.frames) summaryByFrameId.set(f.frameId, f);
    }
  }

  return runDetail.frames.map((row: StackingRunFrameRow) => {
    const s = summaryByFrameId.get(row.frameId);
    const filename = s?.filename ?? filenameByFrameId.get(row.frameId) ?? `frame ${row.frameId}`;

    let statusLabel: string;
    let statusTone: StatusTone;
    if (row.regStatus === 'failed') {
      statusLabel = 'Registration failed';
      statusTone = 'error';
    } else if (!row.included) {
      statusLabel = row.exclusionReason ?? 'Excluded';
      statusTone = 'warning';
    } else {
      statusLabel = 'Included';
      statusTone = 'success';
    }
    // The manual-exclusion list is checked independently of the run's own
    // outcome (Ruling 7 — the checkbox only ever reflects `excludedFrameIds`,
    // never the run's `included`/`exclusionReason`), so a frame the run kept
    // but the user has since marked for exclusion on a *future* run still
    // shows that as its own chip, not silently overridden by "Included".
    if (excludedSet.has(row.frameId) && statusTone !== 'error') {
      statusLabel = 'Excluded (manual)';
      statusTone = 'warning';
    }

    return {
      frameId: row.frameId,
      filename,
      group: groupKeyById.get(row.groupId) ?? '—',
      weight: s?.weight ?? row.weight,
      fwhmPx: s?.fwhmPx ?? null,
      eccentricity: s?.eccentricity ?? null,
      stars: s?.stars ?? null,
      regRmsPx: s?.regRmsPx ?? row.regRmsPx,
      regInliers: s?.regInliers ?? row.regInliers,
      statusLabel,
      statusTone,
    };
  });
}

export interface FramesTableProps {
  /** The set's LIGHT frames (id + filename), independent of any run. */
  lightFrames: LightFrameRef[];
  /** The results panel's currently-selected run, or `null` before any run
   *  has been picked / recorded — the table then falls back to
   *  `lightFrames` alone. */
  runDetail: StackingRunDetail | null;
  excludedFrameIds: number[];
  /** Ruling 7 — the ONLY frame-level write this tab makes. Toggles
   *  `frameId`'s membership in `excludedFrameIds`; the caller (`StackingTab`)
   *  is responsible for routing the resulting list through the existing
   *  gated persist path. */
  onToggleExclude: (frameId: number) => void;
  collapsed: boolean;
  onToggleCollapsed: () => void;
}

/** The Frames table (spec §11.2) — every LIGHT frame in the set, with the
 *  manual-inclusion checkbox and, once a run has produced rows, its
 *  per-frame measurement/registration outcome. Read-only besides the
 *  checkbox (Ruling 7). */
export function FramesTable({
  lightFrames,
  runDetail,
  excludedFrameIds,
  onToggleExclude,
  collapsed,
  onToggleCollapsed,
}: FramesTableProps) {
  const [sortKey, setSortKey] = useState<SortKey>('filename');
  const [sortDir, setSortDir] = useState<'asc' | 'desc'>('asc');

  const excludedSet = useMemo(() => new Set(excludedFrameIds), [excludedFrameIds]);

  const rows = useMemo<FrameRow[]>(
    () =>
      runDetail
        ? postRunRows(runDetail, lightFrames, excludedSet)
        : preRunRows(lightFrames, excludedSet),
    [runDetail, lightFrames, excludedSet],
  );

  const sortedRows = useMemo(() => {
    const copy = [...rows];
    copy.sort((a, b) => {
      const av = a[sortKey];
      const bv = b[sortKey];
      const aNull = av == null;
      const bNull = bv == null;
      if (aNull && bNull) return 0;
      if (aNull) return 1; // nulls sort last regardless of direction
      if (bNull) return -1;
      const cmp =
        typeof av === 'number' && typeof bv === 'number'
          ? av - bv
          : String(av).localeCompare(String(bv));
      return sortDir === 'asc' ? cmp : -cmp;
    });
    return copy;
  }, [rows, sortKey, sortDir]);

  const handleSort = (key: SortKey) => {
    if (key === sortKey) {
      setSortDir((d) => (d === 'asc' ? 'desc' : 'asc'));
    } else {
      setSortKey(key);
      setSortDir('asc');
    }
  };

  return (
    <div className="bg-surface-elevated rounded-lg p-3">
      <button
        type="button"
        onClick={onToggleCollapsed}
        aria-expanded={!collapsed}
        className="flex items-center gap-1.5 text-sm font-medium text-content hover:text-content-secondary transition-colors"
      >
        {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
        Frames ({lightFrames.length})
      </button>

      {!collapsed && (
        <div className="mt-2 overflow-x-auto">
          {rows.length === 0 ? (
            <p className="text-sm text-content-muted px-1">No light frames in this set.</p>
          ) : (
            <table className="w-full text-xs">
              <thead>
                <tr className="text-content-muted text-left border-b border-border">
                  {COLUMNS.map((c) => (
                    <th key={c.key} className="py-1.5 px-2 font-medium">
                      <button
                        type="button"
                        onClick={() => handleSort(c.key)}
                        className={`flex items-center gap-1 hover:text-content transition-colors ${
                          c.numeric ? 'tabular-nums' : ''
                        }`}
                      >
                        {c.label}
                        {sortKey === c.key &&
                          (sortDir === 'asc' ? <ArrowUp size={10} /> : <ArrowDown size={10} />)}
                      </button>
                    </th>
                  ))}
                  <th className="py-1.5 px-2 font-medium text-center">Include</th>
                </tr>
              </thead>
              <tbody>
                {sortedRows.map((r) => (
                  <tr key={r.frameId} className="border-b border-border/50 last:border-0">
                    <td className="py-1.5 px-2 text-content font-mono truncate max-w-[220px]" title={r.filename}>
                      {r.filename}
                    </td>
                    <td className="py-1.5 px-2 text-content-secondary font-mono">{r.group}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.weight, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.fwhmPx, 2, ' px')}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.eccentricity, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtInt(r.stars)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.regRmsPx, 3, ' px')}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtInt(r.regInliers)}</td>
                    <td className="py-1.5 px-2">
                      <span
                        className={`inline-block px-2 py-0.5 rounded-full text-[10px] font-semibold ${STATUS_CHIP[r.statusTone]}`}
                      >
                        {r.statusLabel}
                      </span>
                    </td>
                    <td className="py-1.5 px-2 text-center">
                      <input
                        type="checkbox"
                        checked={!excludedSet.has(r.frameId)}
                        onChange={() => onToggleExclude(r.frameId)}
                        aria-label={`Include ${r.filename} in the next run`}
                        className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}
    </div>
  );
}
