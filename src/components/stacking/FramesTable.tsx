// The Frames block of the Stacking tab (spec §6.1–§6.6): the header line, the reference strip, the Analysis-style
// toolbar with its status chips, and the table, grouped with collapsible header rows, with row selection. Every LIGHT
// frame of a set or every frame of a project, joined to the run selected in Results. The writes (Exclude, Include,
// the reference star) go to the caller; this component owns only the view: sort, filters, grouping, selection.
import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type MouseEvent } from 'react';
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight } from 'lucide-react';
import type {
  PlanGroup,
  PlanProjectFrame,
  PlanReference,
  StackingRunDetail,
} from '../../types/stacking';
import { formatTimestamp } from '../../utils/dateFormatting';
import {
  buildFrameRows,
  buildProjectFrameRows,
  type FrameRow,
  type LightFrameRef,
  type RowContext,
  type RunFrameData,
} from './frameRows';
import {
  computeOutliers,
  filterRows,
  groupRows,
  planGroupTitle,
  statusCounts,
  type GroupContext,
  type GroupMode,
  type StatusFilter,
} from './frameView';
import { FramesReferenceStrip } from './FramesReferenceStrip';
import { FramesToolbar } from './FramesToolbar';
import {
  FrameGroupHeader,
  FrameTableRow,
  fmtFwhm,
  frameColumns,
  groupSummary,
  sortTableRows,
  type TableSortKey,
} from './FramesTableRows';
import { readFramesView, writeFramesView, type FramesView } from './stackingPrefs';
import type { StackSubject } from './subject';

const SET_GROUP_MODES: GroupMode[] = ['group', 'night', 'status', 'none'];
const PROJECT_GROUP_MODES: GroupMode[] = ['group', 'publisher', 'status', 'none'];
const NO_KEYS: ReadonlySet<string> = new Set();

export interface FramesTableProps {
  /** Which rows the table shows: a frame set's light frames, or a project's plan frames (adds Publisher). */
  subjectKind: StackSubject['kind'];
  /** The set's LIGHT frames, independent of any run. Empty for a project. */
  lightFrames: LightFrameRef[];
  /** The project plan's frames (`StackingPlan.projectFrames`). Empty for a set. */
  projectFrames: PlanProjectFrame[];
  /** The run selected in Results, or `null` before any run: rows then come from the frames alone. */
  runDetail: StackingRunDetail | null;
  /** The plan's groups, in the plan's order: the group order, titles and pixel scales. */
  planGroups: PlanGroup[];
  /** The next run's reference, for the strip; `null` while no plan is loaded. */
  planReference: PlanReference | null;
  /** The exclusion keys: `String(frameId)` for a set, the frame uuid for a project (`SubjectConfig.excluded`). */
  excludedKeys: string[];
  /** A project's manual reference frame in effect (its uuid), else `null`. */
  referenceFrameUuid: string | null;
  /** A set's manual reference frame in effect, else `null`. */
  pinnedFrameId: number | null;
  /** The plan's group of a frame with no run row, by frame id (a project's plan handle). */
  planGroupByFrameId: Map<number, string>;
  /** Adds (`excluded`) or removes the keys in the exclusion list; the caller writes the list. */
  onSetExcluded: (keys: string[], excluded: boolean) => void;
  /** Pins the frame `key` as the next run's reference, or clears the pin when it is the pinned one. */
  onPin: (key: string) => void;
  /** Opens Blink over these rows, already in table order. */
  onBlink: (rows: FrameRow[]) => void;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** A run is active: Exclude, Include and the star are disabled; Blink is not. */
  disabled?: boolean;
}

export function FramesTable({
  subjectKind,
  lightFrames,
  projectFrames,
  runDetail,
  planGroups,
  planReference,
  excludedKeys,
  referenceFrameUuid,
  pinnedFrameId,
  planGroupByFrameId,
  onSetExcluded,
  onPin,
  onBlink,
  collapsed,
  onToggleCollapsed,
  disabled = false,
}: FramesTableProps) {
  const isProject = subjectKind === 'project';
  const [view, setView] = useState<FramesView>(readFramesView);
  const [sortKey, setSortKey] = useState<TableSortKey>('filename');
  const [sortDir, setSortDir] = useState<'asc' | 'desc'>('asc');
  const [status, setStatus] = useState<StatusFilter>('all');
  const [outliersOnly, setOutliersOnly] = useState(false);
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<ReadonlySet<string>>(NO_KEYS);
  // Group collapse lives with this render of the table only; a new grouping starts expanded.
  const [collapsedGroups, setCollapsedGroups] = useState<ReadonlySet<string>>(NO_KEYS);
  // Range-select anchor: the row last clicked without Shift, resolved against the displayed order at click time.
  const anchorRef = useRef<string | null>(null);

  const groupModes = isProject ? PROJECT_GROUP_MODES : SET_GROUP_MODES;
  const groupMode = groupModes.includes(view.groupMode) ? view.groupMode : 'group';
  const showUnits = planGroups.some((g) => g.pixelScaleArcsec != null);
  const arcsec = view.arcsec && showUnits;
  const columns = useMemo(() => frameColumns(isProject, view.columns), [isProject, view.columns]);

  const updateView = (patch: Partial<FramesView>) => {
    const next = { ...view, ...patch };
    setView(next);
    writeFramesView(next);
  };

  const ctx = useMemo<RowContext>(() => ({ planGroupByFrameId, pinnedFrameId }), [planGroupByFrameId, pinnedFrameId]);
  const rows = useMemo<FrameRow[]>(() => {
    const runData: RunFrameData | null = runDetail
      ? { groups: runDetail.groups, frames: runDetail.frames, run: runDetail.run }
      : null;
    const summary = runDetail?.summary ?? null;
    return isProject
      ? buildProjectFrameRows(projectFrames, runData, summary, new Set(excludedKeys), referenceFrameUuid, ctx)
      : buildFrameRows(lightFrames, runData, summary, excludedKeys.map(Number), ctx);
  }, [isProject, lightFrames, projectFrames, runDetail, excludedKeys, referenceFrameUuid, ctx]);

  const outliers = useMemo(() => computeOutliers(rows, subjectKind), [rows, subjectKind]);
  const counts = useMemo(() => statusCounts(rows), [rows]);
  const groupTitle = useCallback((key: string) => planGroupTitle(planGroups, key), [planGroups]);
  const groupCtx = useMemo<GroupContext>(
    () => ({ groupOrder: planGroups.map((g) => g.key), groupTitle }),
    [planGroups, groupTitle],
  );
  const groups = useMemo(() => {
    const visible = filterRows(rows, { query, status, outliersOnly }, outliers);
    return groupRows(sortTableRows(visible, sortKey, sortDir), rows, groupMode, groupCtx);
  }, [rows, query, status, outliersOnly, outliers, sortKey, sortDir, groupMode, groupCtx]);
  /** The rows on screen, in table order: a collapsed group's rows are hidden. */
  const displayed = useMemo(
    () => groups.flatMap((g) => (collapsedGroups.has(g.key) ? [] : g.rows)),
    [groups, collapsedGroups],
  );
  const selectedRows = useMemo(() => displayed.filter((r) => selected.has(r.key)), [displayed, selected]);

  // A row a filter, the grouping or a collapse hides leaves the selection, so no action ever touches it. Everything
  // below reads `selectedRows` (already the visible part), so this only makes the drop stick.
  useEffect(() => {
    if (selectedRows.length !== selected.size) setSelected(new Set(selectedRows.map((r) => r.key)));
  }, [selectedRows, selected]);

  const excludeKeys = selectedRows.filter((r) => r.editable && !r.outOfNextRun).map((r) => r.key);
  const includeKeys = selectedRows.filter((r) => r.editable && r.outOfNextRun).map((r) => r.key);

  // The summary frames' file names by run frame id, then the rows' (a run still in flight has no summary).
  const filenameOf = useMemo(() => {
    const byId = new Map<number, string>();
    for (const r of rows) if (r.frameId !== null) byId.set(r.frameId, r.filename);
    for (const g of runDetail?.summary?.groups ?? []) for (const f of g.frames) byId.set(f.frameId, f.filename);
    return (runFrameId: number): string | null => byId.get(runFrameId) ?? null;
  }, [rows, runDetail]);

  const scaleByGroup = useMemo(() => new Map(planGroups.map((g) => [g.key, g.pixelScaleArcsec])), [planGroups]);
  const fwhmScaleOf = (groupKey: string | null) => (arcsec && groupKey !== null ? (scaleByGroup.get(groupKey) ?? null) : null);

  const handleRowSelect = (key: string, e: MouseEvent) => {
    const next = new Set(selected);
    const anchorIdx = anchorRef.current === null ? -1 : displayed.findIndex((r) => r.key === anchorRef.current);
    const clickedIdx = displayed.findIndex((r) => r.key === key);
    if (e.shiftKey && anchorIdx >= 0 && clickedIdx >= 0) {
      // The clicked row decides the direction: selecting if it was unselected.
      const selecting = !next.has(key);
      for (let i = Math.min(anchorIdx, clickedIdx); i <= Math.max(anchorIdx, clickedIdx); i++) {
        if (selecting) next.add(displayed[i]!.key);
        else next.delete(displayed[i]!.key);
      }
    } else {
      if (next.has(key)) next.delete(key);
      else next.add(key);
      anchorRef.current = key;
    }
    setSelected(next);
  };

  const allSelected = displayed.length > 0 && selectedRows.length === displayed.length;
  const toggleAll = () => {
    setSelected(allSelected ? NO_KEYS : new Set(displayed.map((r) => r.key)));
    anchorRef.current = null;
  };

  const handleSort = (key: TableSortKey) => {
    if (key === sortKey) {
      setSortDir((d) => (d === 'asc' ? 'desc' : 'asc'));
    } else {
      setSortKey(key);
      setSortDir('asc');
    }
  };

  const toggleGroup = (key: string) =>
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  /** The run's own references of a stacking group, for its header. */
  const groupReferences = (groupKey: string): string[] => {
    const summary = runDetail?.summary;
    const sg = summary?.groups.find((s) => s.key === groupKey);
    if (!summary || !sg) return [];
    const name = (id: number) => filenameOf(id) ?? `#${id}`;
    const refs: string[] = [];
    if (summary.config.registration.geometry === 'native' && sg.referenceFrameId != null) {
      refs.push(`Reference: ${name(sg.referenceFrameId)}`);
    }
    if (sg.normalizationReferenceFrameId != null) refs.push(`LN ref: ${name(sg.normalizationReferenceFrameId)}`);
    return refs;
  };

  const planGroupOf = (key: string) => planGroups.find((g) => g.key === key);
  const colSpan = columns.length + 1;

  return (
    // The card hugs its widest part (the table, as a rule) instead of spanning the page.
    <div className="w-fit max-w-full bg-surface-elevated rounded-lg p-3 space-y-2">
      <div className="flex items-center gap-2 flex-wrap">
        <button
          type="button"
          onClick={onToggleCollapsed}
          aria-expanded={!collapsed}
          className="flex items-center gap-1.5 text-sm font-medium text-content hover:text-content-secondary transition-colors"
        >
          {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
          Frames ({rows.length})
        </button>
        <span className="text-xs text-content-muted">
          {runDetail
            ? `metrics from run #${runDetail.run.id} · ${formatTimestamp(runDetail.run.startedAt)}`
            : 'no run yet'}
        </span>
      </div>

      {!collapsed && (
        <>
          <FramesReferenceStrip
            runDetail={runDetail}
            planReference={planReference}
            groupLabel={groupTitle}
            filenameOf={filenameOf}
          />
          {rows.length === 0 ? (
            <p className="text-sm text-content-muted px-1">
              {isProject ? 'No frames in this project.' : 'No light frames in this set.'}
            </p>
          ) : (
            <>
              <FramesToolbar
                isProject={isProject}
                selectedCount={selectedRows.length}
                visibleCount={displayed.length}
                onClearSelection={() => setSelected(NO_KEYS)}
                onInvertSelection={() => setSelected(new Set(displayed.filter((r) => !selected.has(r.key)).map((r) => r.key)))}
                onBlink={() => onBlink(selectedRows.length > 0 ? selectedRows : displayed)}
                excludeCount={excludeKeys.length}
                includeCount={includeKeys.length}
                onExclude={() => onSetExcluded(excludeKeys, true)}
                onInclude={() => onSetExcluded(includeKeys, false)}
                disabled={disabled}
                groupModes={groupModes}
                groupMode={groupMode}
                onGroupMode={(mode) => {
                  updateView({ groupMode: mode });
                  setCollapsedGroups(NO_KEYS);
                }}
                showUnits={showUnits}
                arcsec={arcsec}
                onToggleArcsec={() => updateView({ arcsec: !arcsec })}
                outliersOnly={outliersOnly}
                onToggleOutliers={() => setOutliersOnly((v) => !v)}
                columns={view.columns}
                onToggleColumns={() => updateView({ columns: !view.columns })}
                query={query}
                onQuery={setQuery}
                status={status}
                counts={counts}
                onStatus={setStatus}
              />
              {/* The table is as wide as its columns, never stretched to the card: on a wide window the spare room
                  stays to its right instead of opening gaps between the columns. */}
              <div className="w-fit max-w-full max-h-[70vh] overflow-y-auto overflow-x-auto border border-border rounded-lg">
                <table className="text-xs">
                  <thead className="sticky top-0 z-10 bg-surface-elevated">
                    <tr className="text-content-muted text-left border-b border-border">
                      <th className="w-8 py-1.5 px-2 text-center">
                        <input
                          type="checkbox"
                          checked={allSelected}
                          ref={(el) => {
                            if (el) el.indeterminate = selectedRows.length > 0 && !allSelected;
                          }}
                          onChange={toggleAll}
                          aria-label="Select all visible frames"
                          className="rounded border-border text-accent focus:ring-accent cursor-pointer"
                        />
                      </th>
                      {columns.map((c) => (
                        <th
                          key={c.id}
                          className="py-1.5 px-3 font-medium whitespace-nowrap"
                          title={c.title}
                          aria-sort={sortKey !== c.sortKey ? 'none' : sortDir === 'asc' ? 'ascending' : 'descending'}
                        >
                          <button
                            type="button"
                            onClick={() => handleSort(c.sortKey)}
                            className={`flex items-center gap-1 hover:text-content transition-colors ${
                              c.numeric ? 'tabular-nums' : ''
                            }`}
                          >
                            {c.label}
                            {sortKey === c.sortKey &&
                              (sortDir === 'asc' ? <ArrowUp size={10} /> : <ArrowDown size={10} />)}
                          </button>
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {groups.length === 0 ? (
                      <tr>
                        <td colSpan={colSpan} className="py-3 px-2 text-center text-content-muted">
                          No frames match these filters
                        </td>
                      </tr>
                    ) : (
                      groups.map((g) => {
                        const isCollapsed = collapsedGroups.has(g.key);
                        const scale = groupMode === 'group' ? fwhmScaleOf(g.key) : null;
                        return (
                          <Fragment key={g.key}>
                            {groupMode !== 'none' && (
                              <FrameGroupHeader
                                group={g}
                                colSpan={colSpan}
                                collapsed={isCollapsed}
                                onToggle={() => toggleGroup(g.key)}
                                filter={groupMode === 'group' ? (planGroupOf(g.key)?.filter ?? null) : null}
                                summary={groupSummary(g.info, groupMode, (px) => fmtFwhm(px, scale))}
                                references={groupMode === 'group' ? groupReferences(g.key) : []}
                              />
                            )}
                            {!isCollapsed &&
                              g.rows.map((r) => (
                                <FrameTableRow
                                  key={r.key}
                                  row={r}
                                  columns={columns}
                                  selected={selected.has(r.key)}
                                  outlier={outliers.get(r.key)}
                                  fwhmScale={fwhmScaleOf(r.groupKey)}
                                  groupContext={
                                    groupMode !== 'group' && r.groupKey !== null ? groupTitle(r.groupKey) : null
                                  }
                                  disabled={disabled}
                                  onSelect={handleRowSelect}
                                  onPin={onPin}
                                />
                              ))}
                          </Fragment>
                        );
                      })
                    )}
                  </tbody>
                </table>
              </div>
            </>
          )}
        </>
      )}
    </div>
  );
}
