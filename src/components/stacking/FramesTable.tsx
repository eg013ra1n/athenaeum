import { useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, ArrowUp, ArrowDown } from 'lucide-react';
import type { StackingRunDetail, StackingRunFrameRow, RunSummary, SummaryFrame } from '../../types/stacking';

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

/** The run-scoped slice `buildFrameRows` actually needs — `groups`/`frames`
 *  only, deliberately NOT the whole `StackingRunDetail` (whose `summary` is
 *  taken as its own, separately-nullable parameter below). Splitting it this
 *  way lets a caller exercise "run active, not yet finished" (`detail`
 *  present, `summary` still `null`) independently of "no run at all"
 *  (`detail` itself `null`) — the two states `StackingRunDetail.frames`
 *  being "present throughout the run" and `.summary` being "`null` until the
 *  run finishes" actually produce. */
export type RunFrameData = Pick<StackingRunDetail, 'groups' | 'frames'>;

type StatusTone = 'success' | 'warning' | 'error';

export interface FrameRow {
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

/** One row's status chip. A registration failure wins over a plain
 *  exclusion (a failed-registration frame is always also `included =
 *  false` — verified in `crates/athenaeum-core/src/stacking/run.rs`'s
 *  registration-failure branch, which sets both together — so checking
 *  `included` first would relabel it a generic exclusion and lose the more
 *  specific reason); the manual-exclusion list is then checked
 *  independently on top of that (unless the row is already the error case)
 *  — Ruling 7's checkbox only ever reflects `excludedFrameIds` membership,
 *  never the run's own outcome, so a frame the run kept but the user has
 *  since flagged for the *next* run still shows that as its own chip. */
function rowStatus(
  included: boolean,
  exclusionReason: string | null,
  regStatus: string | null,
  manuallyExcluded: boolean,
): { statusLabel: string; statusTone: StatusTone } {
  if (regStatus === 'failed') {
    return { statusLabel: 'Registration failed', statusTone: 'error' };
  }
  if (manuallyExcluded) {
    return { statusLabel: 'Excluded (manual)', statusTone: 'warning' };
  }
  if (!included) {
    return { statusLabel: exclusionReason ?? 'Excluded', statusTone: 'warning' };
  }
  return { statusLabel: 'Included', statusTone: 'success' };
}

/** The Frames table's row source (fix round 1, Important #2): the UNION of
 *  `lightFrames` (the set's current LIGHT frames) and `detail.frames` (the
 *  selected run's own frame rows), keyed by `frameId` — not `detail.frames`
 *  alone. A light added to the set after the selected run (or present under
 *  an older run than the one currently selected) has no row in
 *  `detail.frames`; without the union it silently had no row at all in the
 *  table, and therefore no include checkbox — the tab's one frame-level
 *  write (Ruling 7) simply couldn't reach it. Such a frame gets a
 *  synthetic "no run data yet" row (group "—", every metric "—", status
 *  from the manual-exclusion list alone) — the same shape every row had
 *  before any run existed.
 *
 *  `detail`/`summary` are deliberately separate parameters, not one
 *  `StackingRunDetail` — `detail.frames` is present throughout a run,
 *  `summary` (`RunSummary`, carrying the FWHM/eccentricity/seed metrics
 *  `StackingRunFrameRow` itself has no columns for) is `null` until the run
 *  finishes; keeping them apart lets a caller (this file's own smoke
 *  self-check included) exercise "run active, not yet finished" separately
 *  from "no run at all" (`detail === null`). Pure — no React, safe to
 *  import and unit-exercise outside a component. */
export function buildFrameRows(
  lightFrames: LightFrameRef[],
  detail: RunFrameData | null,
  summary: RunSummary | null,
  excludedFrameIds: number[],
): FrameRow[] {
  const excludedSet = new Set(excludedFrameIds);
  const filenameByFrameId = new Map(lightFrames.map((f) => [f.frameId, f.filename]));
  const groupKeyById = new Map((detail?.groups ?? []).map((g) => [g.id, g.groupKey]));
  const runRowByFrameId = new Map<number, StackingRunFrameRow>(
    (detail?.frames ?? []).map((r) => [r.frameId, r]),
  );
  const summaryByFrameId = new Map<number, SummaryFrame>();
  if (summary) {
    for (const g of summary.groups) {
      for (const f of g.frames) summaryByFrameId.set(f.frameId, f);
    }
  }

  // Union of frame ids, lightFrames first (their own order) then any
  // run-row frame id lightFrames doesn't have (e.g. a frame removed from
  // the set since the selected run — still worth showing as part of that
  // run's own record).
  const frameIds: number[] = [];
  const seen = new Set<number>();
  for (const f of lightFrames) {
    if (!seen.has(f.frameId)) {
      seen.add(f.frameId);
      frameIds.push(f.frameId);
    }
  }
  for (const r of detail?.frames ?? []) {
    if (!seen.has(r.frameId)) {
      seen.add(r.frameId);
      frameIds.push(r.frameId);
    }
  }

  return frameIds.map((frameId) => {
    const runRow = runRowByFrameId.get(frameId);
    const s = summaryByFrameId.get(frameId);
    const manuallyExcluded = excludedSet.has(frameId);

    if (!runRow) {
      // No run row for this frame at all (a light added after the selected
      // run, or one that's never survived to stage 1 in any run) — the
      // same "no run data yet" shape a row had before any run existed:
      // status comes from the manual-exclusion list alone.
      const filename = filenameByFrameId.get(frameId) ?? `frame ${frameId}`;
      return {
        frameId,
        filename,
        group: '—',
        weight: null,
        fwhmPx: null,
        eccentricity: null,
        stars: null,
        regRmsPx: null,
        regInliers: null,
        statusLabel: manuallyExcluded ? 'Excluded (manual)' : 'Included',
        statusTone: manuallyExcluded ? 'warning' : 'success',
      };
    }

    const filename = s?.filename ?? filenameByFrameId.get(frameId) ?? `frame ${frameId}`;
    const { statusLabel, statusTone } = rowStatus(
      runRow.included,
      runRow.exclusionReason,
      runRow.regStatus,
      manuallyExcluded,
    );

    return {
      frameId,
      filename,
      group: groupKeyById.get(runRow.groupId) ?? '—',
      weight: s?.weight ?? runRow.weight,
      fwhmPx: s?.fwhmPx ?? null,
      eccentricity: s?.eccentricity ?? null,
      stars: s?.stars ?? null,
      regRmsPx: s?.regRmsPx ?? runRow.regRmsPx,
      regInliers: s?.regInliers ?? runRow.regInliers,
      statusLabel,
      statusTone,
    };
  });
}

/** Client-side sort, any column, numeric-aware, nulls always last regardless
 *  of direction. `Array.prototype.sort` is spec-stable since ES2019, so no
 *  extra work is needed for "stable" beyond not mutating `rows` in place
 *  (returns a new array). Pure, exported for the same reason
 *  `buildFrameRows` is. */
export function sortRows(rows: FrameRow[], column: SortKey, dir: 'asc' | 'desc'): FrameRow[] {
  const copy = [...rows];
  copy.sort((a, b) => {
    const av = a[column];
    const bv = b[column];
    const aNull = av == null;
    const bNull = bv == null;
    if (aNull && bNull) return 0;
    if (aNull) return 1; // nulls sort last regardless of direction
    if (bNull) return -1;
    const cmp =
      typeof av === 'number' && typeof bv === 'number'
        ? av - bv
        : String(av).localeCompare(String(bv));
    return dir === 'asc' ? cmp : -cmp;
  });
  return copy;
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
  /** True while a run is active for this set — disables the include
   *  checkbox the same way the rest of the tab's controls are disabled
   *  during a run (fix round 1, Important #8). */
  disabled?: boolean;
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
  disabled = false,
}: FramesTableProps) {
  const [sortKey, setSortKey] = useState<SortKey>('filename');
  const [sortDir, setSortDir] = useState<'asc' | 'desc'>('asc');

  // Only for the checkbox's own `checked` state below — `buildFrameRows`
  // builds and consumes its own `Set` internally for the status chip.
  const excludedSet = useMemo(() => new Set(excludedFrameIds), [excludedFrameIds]);

  const rows = useMemo<FrameRow[]>(
    () =>
      buildFrameRows(
        lightFrames,
        runDetail ? { groups: runDetail.groups, frames: runDetail.frames } : null,
        runDetail?.summary ?? null,
        excludedFrameIds,
      ),
    [lightFrames, runDetail, excludedFrameIds],
  );

  const sortedRows = useMemo(() => sortRows(rows, sortKey, sortDir), [rows, sortKey, sortDir]);

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
        Frames ({rows.length})
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
                        disabled={disabled}
                        aria-label={`Include ${r.filename} in the next run`}
                        className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
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
