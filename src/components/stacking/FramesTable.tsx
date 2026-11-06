import { useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, ArrowUp, ArrowDown, Pin } from 'lucide-react';
import type { PlanProjectFrame, StackingRunDetail } from '../../types/stacking';
import {
  buildFrameRows,
  buildProjectFrameRows,
  sortRows,
  type FrameRow,
  type LightFrameRef,
  type Outcome,
  type RowContext,
  type RunFrameData,
  type SortKey,
} from './frameRows';
import type { StackSubject } from './subject';

const COLUMNS: { key: SortKey; label: string; numeric?: boolean; title?: string }[] = [
  { key: 'filename', label: 'Filename' },
  { key: 'groupKey', label: 'Group' },
  { key: 'weight', label: 'Weight', numeric: true },
  { key: 'fwhmPx', label: 'FWHM', numeric: true },
  { key: 'eccentricity', label: 'Ecc', numeric: true },
  { key: 'stars', label: 'Seeds', numeric: true },
  { key: 'regRmsPx', label: 'Reg RMS', numeric: true },
  { key: 'lnScale', label: 'Scale', numeric: true, title: 'Local-normalization relative scale' },
  { key: 'regInliers', label: 'Inliers', numeric: true },
  { key: 'regModel', label: 'Model', title: 'Registration alignment model' },
  { key: 'status', label: 'Status' },
];

/** Project table only: the Publisher column follows Filename. */
const PUBLISHER_COLUMN: (typeof COLUMNS)[number] = { key: 'publisher', label: 'Publisher' };
const PROJECT_COLUMNS = COLUMNS.flatMap((c) => (c.key === 'filename' ? [c, PUBLISHER_COLUMN] : [c]));

const OUTCOME_CHIP: Record<Outcome['tone'], string> = {
  accent: 'bg-accent/20 text-accent',
  success: 'bg-success-muted text-success',
  warning: 'bg-warning-muted text-warning',
  error: 'bg-error-muted text-error',
};

/** Same muted-chip tokens `ProvenanceModal.tsx` uses for its own "off"
 *  state — the neutral pill in this stacking component family. */
const WCS_CHIP = 'bg-surface-hover text-content-muted';

/** The context the builders take until the plan's grouping and the set's
 *  manual reference are wired in: no frame has a planned group, none is
 *  pinned. */
const NO_PLAN_GROUPS = new Map<number, string>();
const ROW_CONTEXT: RowContext = { planGroupByFrameId: NO_PLAN_GROUPS, pinnedFrameId: null };

function fmtNum(n: number | null, digits: number, suffix = ''): string {
  return n == null ? '—' : `${n.toFixed(digits)}${suffix}`;
}

function fmtInt(n: number | null): string {
  return n == null ? '—' : String(n);
}

/** Splits a `regModel` string on its optional trailing `+wcs` (M4b) so the
 *  Model column can render the alignment model text and the WCS-seed chip
 *  separately — e.g. `homography+polynomial3+wcs` → base
 *  `homography+polynomial3`, `wcs: true`. */
function splitRegModel(model: string | null): { base: string | null; wcs: boolean } {
  if (model == null) return { base: null, wcs: false };
  if (model.endsWith('+wcs')) return { base: model.slice(0, -'+wcs'.length), wcs: true };
  return { base: model, wcs: false };
}

export interface FramesTableProps {
  /** Which rows the table shows: a frame set's light frames, or a project's
   *  plan frames. A project adds the Publisher and Reference columns. */
  subjectKind: StackSubject['kind'];
  /** The set's LIGHT frames (id + filename), independent of any run. Empty
   *  for a project. */
  lightFrames: LightFrameRef[];
  /** The project plan's frames (`StackingPlan.projectFrames`). Empty for a
   *  set. */
  projectFrames: PlanProjectFrame[];
  /** The results panel's currently-selected run, or `null` before any run
   *  has been picked / recorded — the table then falls back to
   *  `lightFrames` / `projectFrames` alone. */
  runDetail: StackingRunDetail | null;
  /** The subject's exclusion keys: `String(frameId)` for a set, the frame
   *  uuid for a project (`SubjectConfig.excluded`). */
  excludedKeys: string[];
  /** A project's manual reference frame in effect (its uuid), else `null`. */
  referenceFrameUuid: string | null;
  /** Ruling 7 — the include checkbox, a frame-level write of the exclusion
   *  list. Toggles `key`'s membership in `excludedKeys`; the caller
   *  (`StackingTab`) is responsible for routing the resulting list through
   *  the existing gated persist path. */
  onToggleExclude: (key: string) => void;
  /** The project's reference pin (D2): pins the frame `key`, or clears the
   *  pin when it is the pinned one. Every usable row has one. */
  onPin: (key: string) => void;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** True while a run is active for this subject — disables the include
   *  checkbox and the pin the same way the rest of the tab's controls are
   *  disabled during a run (fix round 1, Important #8). */
  disabled?: boolean;
}

/** The Frames table (spec §11.2, §12) — every LIGHT frame of a set or every
 *  frame of a project, with the manual-inclusion checkbox and, once a run has
 *  produced rows, its per-frame measurement/registration outcome. A
 *  project's table adds the publisher and the reference pin. Read-only
 *  besides the checkbox (Ruling 7) and the pin. */
export function FramesTable({
  subjectKind,
  lightFrames,
  projectFrames,
  runDetail,
  excludedKeys,
  referenceFrameUuid,
  onToggleExclude,
  onPin,
  collapsed,
  onToggleCollapsed,
  disabled = false,
}: FramesTableProps) {
  const [sortKey, setSortKey] = useState<SortKey>('filename');
  const [sortDir, setSortDir] = useState<'asc' | 'desc'>('asc');
  const isProject = subjectKind === 'project';
  const columns = isProject ? PROJECT_COLUMNS : COLUMNS;

  // For the checkbox's own `checked` state below — the row builders build
  // and consume their own sets for the status chip.
  const excludedSet = useMemo(() => new Set(excludedKeys), [excludedKeys]);

  const rows = useMemo<FrameRow[]>(() => {
    const runData: RunFrameData | null = runDetail
      ? { groups: runDetail.groups, frames: runDetail.frames, run: runDetail.run }
      : null;
    const summary = runDetail?.summary ?? null;
    return isProject
      ? buildProjectFrameRows(projectFrames, runData, summary, excludedSet, referenceFrameUuid, ROW_CONTEXT)
      : buildFrameRows(lightFrames, runData, summary, excludedKeys.map(Number), ROW_CONTEXT);
  }, [isProject, lightFrames, projectFrames, runDetail, excludedKeys, excludedSet, referenceFrameUuid]);

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
            <p className="text-sm text-content-muted px-1">
              {isProject ? 'No frames in this project.' : 'No light frames in this set.'}
            </p>
          ) : (
            <table className="w-full text-xs">
              <thead>
                <tr className="text-content-muted text-left border-b border-border">
                  {columns.map((c) => (
                    <th key={c.key} className="py-1.5 px-2 font-medium" title={c.title}>
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
                  {isProject && <th className="py-1.5 px-2 font-medium text-center">Reference</th>}
                </tr>
              </thead>
              <tbody>
                {sortedRows.map((r) => (
                  <tr key={r.key} className="border-b border-border/50 last:border-0">
                    <td className="py-1.5 px-2 text-content font-mono truncate max-w-[220px]" title={r.filename}>
                      {r.filename}
                      {r.unavailable && (
                        <div className="text-[10px] text-warning truncate" title={r.unavailable}>
                          {r.unavailable}
                        </div>
                      )}
                    </td>
                    {isProject && (
                      <td className="py-1.5 px-2 text-content-secondary truncate max-w-[140px]" title={r.publisher ?? undefined}>
                        {r.publisher ?? '—'}
                      </td>
                    )}
                    <td
                      className="py-1.5 px-2 text-content-secondary font-mono"
                      title={r.groupKey == null ? 'assigned by the next run' : undefined}
                    >
                      {r.groupKey ?? '—'}
                    </td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.weight, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.fwhmPx, 2, ' px')}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.eccentricity, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtInt(r.stars)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.regRmsPx, 3, ' px')}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.lnScale, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtInt(r.regInliers)}</td>
                    <td className="py-1.5 px-2 text-content-secondary font-mono">
                      {(() => {
                        const { base, wcs } = splitRegModel(r.regModel);
                        if (base == null) return '—';
                        return (
                          <span className="inline-flex items-center gap-1">
                            <span>{base}</span>
                            {wcs && (
                              <span
                                title="seeded from the plate solves"
                                className={`inline-block px-1.5 py-0.5 rounded-full text-[10px] font-semibold ${WCS_CHIP}`}
                              >
                                WCS
                              </span>
                            )}
                          </span>
                        );
                      })()}
                    </td>
                    <td className="py-1.5 px-2">
                      <div className="flex items-center gap-1">
                        {r.outcome && (
                          <span
                            className={`inline-block px-2 py-0.5 rounded-full text-[10px] font-semibold ${OUTCOME_CHIP[r.outcome.tone]}`}
                          >
                            {r.outcome.label}
                          </span>
                        )}
                        {r.outOfNextRun && (
                          <span
                            className={`inline-block px-2 py-0.5 rounded-full text-[10px] font-semibold ${OUTCOME_CHIP.warning}`}
                          >
                            Out of next run
                          </span>
                        )}
                        {/* "Not stackable here" repeats the reason already printed under the filename. */}
                        {r.outcome?.reason && r.outcome.kind !== 'notStackable' && (
                          <span
                            className="text-[10px] text-content-muted truncate max-w-[200px]"
                            title={r.outcome.reason}
                          >
                            {r.outcome.reason}
                          </span>
                        )}
                      </div>
                    </td>
                    <td className="py-1.5 px-2 text-center">
                      {r.editable && (
                        <input
                          type="checkbox"
                          checked={!excludedSet.has(r.key)}
                          onChange={() => onToggleExclude(r.key)}
                          disabled={disabled}
                          aria-label={`Include ${r.filename} in the next run`}
                          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
                        />
                      )}
                    </td>
                    {isProject && (
                      <td className="py-1.5 px-2 text-center">
                        {r.editable && (
                          // A pinned frame can always be unpinned, even one that has become unusable since.
                          <button
                            type="button"
                            onClick={() => onPin(r.key)}
                            disabled={disabled || (!r.pinnable && !r.pinned)}
                            aria-pressed={r.pinned}
                            aria-label={
                              r.pinned
                                ? `Clear the reference frame ${r.filename}`
                                : `Use ${r.filename} as the reference frame`
                            }
                            title={
                              r.pinned
                                ? // A pinned frame that is no longer usable says why, in brackets after the clear hint.
                                  !r.pinnable && r.pinNote
                                  ? `The reference frame — click to clear (${r.pinNote})`
                                  : 'The reference frame — click to clear'
                                : (r.pinNote ?? 'Use as the reference frame')
                            }
                            className={`p-1 rounded transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
                              r.pinned ? 'text-accent' : 'text-content-muted hover:text-content'
                            }`}
                          >
                            <Pin size={14} fill={r.pinned ? 'currentColor' : 'none'} />
                          </button>
                        )}
                      </td>
                    )}
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
