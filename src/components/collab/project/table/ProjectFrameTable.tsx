import { useEffect, useMemo, useRef, useState, type JSX, type ReactNode } from 'react';
import { Loader2 } from 'lucide-react';
import { useSessionState } from '../../../../contexts/SessionStateContext';
import { getFilterColor } from '../../../../utils/filterColors';
import { formatBytes, formatDuration } from '../../format';
import { COLUMNS, FRAME_ACCESS, GROUPS, TABLES, type FrameVM, type TableId } from '../frames';
import {
  activeFacetCount, allGroupIds, applyFacets, buildTree, checkState, EMPTY_FACETS, facetCounts, filterOrder,
  flatten, initialExpanded, localToday, median, NIGHT_WINDOWS, sortRows, sum, windowSlice,
  actionLabel, actionTargets,
  type ColumnDef, type Facets, type GroupNode, type NightWindow, type SortDir, type SortState, type VisibleRow,
} from './model';

/* ── Public interface (spec §4.1, Task 7) ──────────────────────────────── */

export interface TableAction {
  id: string;
  verb: string; // 'Publish', 'Solve', 'Keep again', 'Approve', 'Reject'
  eligible: (r: FrameVM) => boolean;
  primary?: boolean; // accent button
  busy?: boolean; // disables + spinner
  run: (targets: FrameVM[]) => void; // receives actionTargets(...).targets
}

export interface ProjectFrameTableProps {
  tableId: TableId;
  scope: string; // projectId — the session-state key prefix
  rows: FrameVM[];
  actions: TableAction[];
  onOpen: (r: FrameVM) => void;
  emptyText: string; // shown when rows is empty (not when facets hide everything)
  groupAction?: (node: GroupNode<FrameVM>) => ReactNode; // Held back Reason headers
  toolbarExtra?: ReactNode; // e.g. the moderation trust checkbox
  today?: string; // test seam; default localToday()
}

/* ── Small helpers ──────────────────────────────────────────────────────── */

const rowKey = (r: FrameVM) => r.key;
/** Sentinel for a select's "All …" option — never a legal facet value (camera
 *  and publisher can legitimately be `''`/unknown, so `''` can't serve). */
const ALL = '\u0000all';

const NIGHT_LABEL: Record<NightWindow, string> = {
  all: 'All nights',
  '1': 'Last night',
  '7': 'Last 7 nights',
  '30': 'Last 30 nights',
};

function colStorageKey(tableId: TableId): string {
  return `collab.table.${tableId}.cols`;
}

function loadColumns(tableId: TableId, allColumns: string[], defaultColumns: string[]): string[] {
  try {
    const raw = localStorage.getItem(colStorageKey(tableId));
    if (raw) {
      const parsed: unknown = JSON.parse(raw);
      if (Array.isArray(parsed) && parsed.every((x) => typeof x === 'string')) {
        return (parsed as string[]).filter((id) => allColumns.includes(id));
      }
    }
  } catch (err) {
    console.warn('[collab-table] column prefs unavailable:', err);
  }
  return defaultColumns;
}

function saveColumns(tableId: TableId, cols: string[]): void {
  try {
    localStorage.setItem(colStorageKey(tableId), JSON.stringify(cols));
  } catch (err) {
    console.warn('[collab-table] column prefs unavailable:', err);
  }
}

const linkBtn = 'text-[11px] text-accent hover:underline disabled:text-content-muted disabled:no-underline disabled:cursor-not-allowed';
const outlineBtn = 'inline-flex items-center gap-1 rounded border border-border px-2.5 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50';
const primaryBtn = 'inline-flex items-center gap-1 rounded bg-accent px-2.5 py-1 text-xs text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50';

/* ── Component ──────────────────────────────────────────────────────────── */

export default function ProjectFrameTable(props: ProjectFrameTableProps): JSX.Element {
  const { tableId, scope, rows, actions, onOpen, emptyText, groupAction, toolbarExtra } = props;
  const today = props.today ?? localToday();
  const config = TABLES[tableId];

  const [facets, setFacets] = useSessionState<Facets>(`collab.${scope}.${tableId}.facets`, EMPTY_FACETS);
  const [group, setGroup] = useSessionState<[string, string]>(`collab.${scope}.${tableId}.group`, () => config.defaultGrouping);
  const [sort, setSort] = useSessionState<SortState>(`collab.${scope}.${tableId}.sort`, { col: 'name', dir: 1 });
  const [expanded, setExpanded] = useSessionState<string[] | null>(`collab.${scope}.${tableId}.expanded`, null);

  const [visibleColIds, setVisibleColIds] = useState<string[]>(() => loadColumns(tableId, config.columns, config.defaultColumns));
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [columnsOpen, setColumnsOpen] = useState(false);

  // A callback ref (not a plain `useRef`) so measurement re-attaches whenever
  // the scroll element itself changes — including "doesn't exist yet" (the
  // table is behind the `rows.length === 0` early return below) → "exists"
  // once rows arrive, and detach → reattach if rows empty out and refill.
  // A `useRef` object never changes identity, so an effect keyed on `[]`
  // would only ever see the FIRST element (or none), and never re-run.
  const [scrollEl, setScrollEl] = useState<HTMLDivElement | null>(null);
  const scrollRef = (node: HTMLDivElement | null) => setScrollEl(node);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(0);

  // Prune the selection whenever the underlying rows change (a frame that left
  // the table — republished, excluded, downloaded — stops being selectable).
  useEffect(() => {
    setSelected((prev) => {
      if (prev.size === 0) return prev;
      const live = new Set(rows.map(rowKey));
      let changed = false;
      const next = new Set<string>();
      for (const k of prev) {
        if (live.has(k)) next.add(k);
        else changed = true;
      }
      return changed ? next : prev;
    });
  }, [rows]);

  useEffect(() => {
    if (!scrollEl) return undefined;
    setViewportH(scrollEl.clientHeight);
    if (typeof ResizeObserver === 'undefined') return undefined;
    const ro = new ResizeObserver((entries) => {
      for (const entry of entries) setViewportH(entry.contentRect.height);
    });
    ro.observe(scrollEl);
    return () => ro.disconnect();
  }, [scrollEl]);

  const counts = useMemo(() => facetCounts(rows, facets, FRAME_ACCESS, today), [rows, facets, today]);
  const filtered = useMemo(() => applyFacets(rows, facets, FRAME_ACCESS, today), [rows, facets, today]);

  const presentFilters = useMemo(
    () => Array.from(new Set(rows.map((r) => FRAME_ACCESS.filter(r)))).sort(filterOrder),
    [rows],
  );
  const presentCameras = useMemo(
    () => Array.from(new Set(rows.map((r) => FRAME_ACCESS.camera(r)))).sort(),
    [rows],
  );
  const presentPublishers = useMemo(() => {
    if (!config.publisherFacet || !FRAME_ACCESS.publisher) return [];
    const s = new Set<string>();
    for (const r of rows) {
      const p = FRAME_ACCESS.publisher(r);
      if (p !== null) s.add(p);
    }
    return Array.from(s).sort();
  }, [rows, config.publisherFacet]);

  const sortCol: ColumnDef<FrameVM> | undefined = COLUMNS[sort.col];
  const nameOf = FRAME_ACCESS.name;

  const levelDefs = useMemo(() => {
    if (group[0] === 'none' || !GROUPS[group[0]]) return [];
    const defs = [GROUPS[group[0]]];
    if (group[1] !== 'none' && GROUPS[group[1]]) defs.push(GROUPS[group[1]]);
    return defs;
  }, [group]);

  const tree = useMemo(
    () => (levelDefs.length ? buildTree(filtered, levelDefs, sortCol, sort.dir, nameOf) : []),
    [filtered, levelDefs, sortCol, sort.dir, nameOf],
  );

  // `expanded === null` means "not initialised for this grouping" — open the
  // first group (and its first child) once the tree for it is known.
  useEffect(() => {
    if (levelDefs.length === 0) return;
    if (expanded !== null) return;
    // An empty tree (no rows yet, or the facets currently hide everything) has
    // no "first group" to open — leave `expanded` at `null` so this effect
    // runs again once real groups exist, instead of latching `[]` in as
    // "already initialised" for the rest of the session.
    if (tree.length === 0) return;
    setExpanded(initialExpanded(tree));
  }, [levelDefs.length, expanded, tree, setExpanded]);

  const visibleRows: VisibleRow<FrameVM>[] = useMemo(() => {
    if (levelDefs.length === 0) {
      return sortRows(filtered, sortCol, sort.dir, nameOf).map((row) => ({ kind: 'frame' as const, row, depth: 0 }));
    }
    return flatten(tree, new Set(expanded ?? []));
  }, [levelDefs.length, filtered, sortCol, sort.dir, nameOf, tree, expanded]);

  const { start, end, padTop, padBottom } = windowSlice(scrollTop, viewportH, visibleRows.length);
  const windowed = visibleRows.slice(start, end);

  const columns: ColumnDef<FrameVM>[] = useMemo(
    () => config.columns.filter((id) => id === 'name' || visibleColIds.includes(id)).map((id) => COLUMNS[id]),
    [config.columns, visibleColIds],
  );

  function toggleColumn(id: string): void {
    if (id === 'name') return;
    setVisibleColIds((prev) => {
      const next = prev.includes(id) ? prev.filter((c) => c !== id) : [...prev, id];
      saveColumns(tableId, next);
      return next;
    });
  }

  function toggleFilterChip(f: string): void {
    setFacets((prev) => ({
      ...prev,
      filters: prev.filters.includes(f) ? prev.filters.filter((x) => x !== f) : [...prev.filters, f],
    }));
  }

  function clearFacets(): void {
    setFacets(EMPTY_FACETS);
  }

  function onSortClick(colId: string): void {
    setSort((prev): SortState => (prev.col === colId ? { col: colId, dir: (prev.dir === 1 ? -1 : 1) as SortDir } : { col: colId, dir: 1 }));
  }

  function setGroup0(g0: string): void {
    setGroup(([, g1]) => [g0, g1 === g0 ? 'none' : g1]);
    setExpanded(null);
  }
  function setGroup1(g1: string): void {
    setGroup(([g0]) => [g0, g1]);
    setExpanded(null);
  }

  const headerState = checkState(filtered, selected, rowKey);
  function toggleSelectAllShown(): void {
    setSelected((prev) => {
      const next = new Set(prev);
      if (headerState === 'all') for (const r of filtered) next.delete(rowKey(r));
      else for (const r of filtered) next.add(rowKey(r));
      return next;
    });
  }

  function toggleGroupSelection(node: GroupNode<FrameVM>): void {
    const state = checkState(node.rows, selected, rowKey);
    setSelected((prev) => {
      const next = new Set(prev);
      if (state === 'all') for (const r of node.rows) next.delete(rowKey(r));
      else for (const r of node.rows) next.add(rowKey(r));
      return next;
    });
  }

  function toggleFrameSelection(r: FrameVM): void {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(rowKey(r))) next.delete(rowKey(r));
      else next.add(rowKey(r));
      return next;
    });
  }

  function toggleExpand(node: GroupNode<FrameVM>): void {
    setExpanded((prev) => {
      const cur = new Set(prev ?? []);
      if (cur.has(node.id)) cur.delete(node.id);
      else cur.add(node.id);
      return Array.from(cur);
    });
  }

  // Empty states (spec §4.1 item 7): no rows at all → just the sentence.
  if (rows.length === 0) {
    return <p className="text-sm text-content-muted">{emptyText}</p>;
  }

  // A selection entirely hidden by the current facets counts as NO selection
  // for the actions (controller ruling, fix round 1 #3): `actionTargets` gets
  // only the in-view part of `selected`, so when that part is empty it takes
  // its own `selected.size === 0` branch and the primary action falls back
  // to covering the whole filtered view, instead of reporting "0 of 0"
  // against frames the user can no longer see or reach.
  const inViewSelected = new Set(filtered.filter((r) => selected.has(rowKey(r))).map(rowKey));
  const hiddenSelected = selected.size - inViewSelected.size;
  const totalExp = sum(filtered.map((r) => r.exptimeSec));
  const totalBytes = sum(filtered.map((r) => r.byteSize));
  const medFwhm = median(filtered.map((r) => r.fwhm));
  const colSpan = columns.length + 1;
  const active = activeFacetCount(facets);

  return (
    <div className="flex flex-col gap-2 text-xs">
      {/* ── Facet row ──────────────────────────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
        <span className="text-content-muted">Filter</span>
        {presentFilters.map((f) => {
          const on = facets.filters.includes(f);
          const c = counts.filters.get(f) ?? 0;
          return (
            <button
              key={f}
              type="button"
              onClick={() => toggleFilterChip(f)}
              className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 ${
                on ? 'border-accent bg-accent/10 text-content' : 'border-border text-content-secondary'
              } ${c === 0 ? 'opacity-40' : ''}`}
            >
              <span className="inline-block h-2 w-2 rounded-full" style={{ backgroundColor: getFilterColor(f) }} />
              {f}
              {' '}
              <span className="text-[10px] text-content-muted">{c}</span>
            </button>
          );
        })}

        <span className="mx-1 h-4 w-px bg-border" />

        <select
          aria-label="Camera"
          value={facets.camera ?? ALL}
          onChange={(e) => setFacets((prev) => ({ ...prev, camera: e.target.value === ALL ? null : e.target.value }))}
          className="h-6 rounded border border-border bg-surface-elevated px-1.5 text-content"
        >
          <option value={ALL}>All cameras</option>
          {presentCameras.map((c) => (
            <option key={c} value={c}>
              {c === '' ? 'Unknown camera' : c} ({counts.cameras.get(c) ?? 0})
            </option>
          ))}
        </select>

        <select
          aria-label="Night"
          value={facets.night}
          onChange={(e) => setFacets((prev) => ({ ...prev, night: e.target.value as NightWindow }))}
          className="h-6 rounded border border-border bg-surface-elevated px-1.5 text-content"
        >
          {NIGHT_WINDOWS.map((w) => (
            <option key={w} value={w}>
              {NIGHT_LABEL[w]} ({counts.nights[w]})
            </option>
          ))}
        </select>

        {config.publisherFacet && (
          <select
            aria-label="Publisher"
            value={facets.publisher ?? ALL}
            onChange={(e) => setFacets((prev) => ({ ...prev, publisher: e.target.value === ALL ? null : e.target.value }))}
            className="h-6 rounded border border-border bg-surface-elevated px-1.5 text-content"
          >
            <option value={ALL}>All publishers</option>
            {presentPublishers.map((p) => (
              <option key={p} value={p}>
                {p} ({counts.publishers.get(p) ?? 0})
              </option>
            ))}
          </select>
        )}

        {config.stateFacet && (
          <select
            aria-label={config.stateFacet.label}
            value={facets.state ?? ALL}
            onChange={(e) => setFacets((prev) => ({ ...prev, state: e.target.value === ALL ? null : e.target.value }))}
            className="h-6 rounded border border-border bg-surface-elevated px-1.5 text-content"
          >
            <option value={ALL}>{config.stateFacet.label}: any</option>
            {config.stateFacet.options.map(([v, l]) => (
              <option key={v} value={v}>
                {l} ({counts.states.get(v) ?? 0})
              </option>
            ))}
          </select>
        )}

        <input
          type="text"
          placeholder="Search frames"
          aria-label="Search frames"
          value={facets.search}
          onChange={(e) => setFacets((prev) => ({ ...prev, search: e.target.value }))}
          className="h-6 w-40 rounded border border-border bg-surface-elevated px-1.5 text-content placeholder:text-content-muted"
        />

        <button type="button" disabled={active === 0} onClick={clearFacets} className={linkBtn}>
          Clear filters
        </button>
      </div>

      {/* ── Group row ──────────────────────────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
        <span className="text-content-muted">Group by</span>
        <select
          aria-label="Group by"
          value={group[0]}
          onChange={(e) => setGroup0(e.target.value)}
          className="h-6 rounded border border-border bg-surface-elevated px-1.5 text-content"
        >
          {config.groupings.map((id) => (
            <option key={id} value={id}>
              {GROUPS[id].label}
            </option>
          ))}
        </select>
        {group[0] !== 'none' && (
          <>
            <span className="text-content-muted">▸</span>
            <select
              aria-label="Then group by"
              value={group[1]}
              onChange={(e) => setGroup1(e.target.value)}
              className="h-6 rounded border border-border bg-surface-elevated px-1.5 text-content"
            >
              {config.groupings.filter((id) => id !== group[0]).map((id) => (
                <option key={id} value={id}>
                  {GROUPS[id].label}
                </option>
              ))}
            </select>
          </>
        )}
        <button type="button" disabled={levelDefs.length === 0} onClick={() => setExpanded(allGroupIds(tree))} className={linkBtn}>
          Expand all
        </button>
        <button type="button" disabled={levelDefs.length === 0} onClick={() => setExpanded([])} className={linkBtn}>
          Collapse all
        </button>
        <span className="flex-1" />
        <div className="relative">
          <button type="button" onClick={() => setColumnsOpen((o) => !o)} className={outlineBtn}>
            Columns
          </button>
          {columnsOpen && (
            <div className="absolute right-0 top-full z-20 mt-1 grid gap-1 rounded border border-border bg-surface-elevated p-2 shadow-lg">
              {config.columns.filter((id) => id !== 'name').map((id) => (
                <label key={id} className="flex items-center gap-1.5 whitespace-nowrap text-content-secondary">
                  <input type="checkbox" checked={visibleColIds.includes(id)} onChange={() => toggleColumn(id)} />
                  {COLUMNS[id].label}
                </label>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* ── Totals strip ───────────────────────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-x-3.5 gap-y-1.5 rounded-t border border-b-0 border-border bg-surface-elevated px-2.5 py-1.5 text-content-secondary">
        <span>
          {filtered.length.toLocaleString('en-US')} of {rows.length.toLocaleString('en-US')} frames
        </span>
        <span>Σ {formatDuration(totalExp)}</span>
        <span>{formatBytes(totalBytes)}</span>
        <span>FWHM x̃ {medFwhm === null ? '—' : medFwhm.toFixed(2)}</span>
        {selected.size > 0 && (
          <span className="flex items-center gap-1 text-accent">
            <span>
              {selected.size} selected{hiddenSelected > 0 ? ` · ${hiddenSelected} hidden` : ''}
            </span>
            <span>·</span>
            <button type="button" onClick={() => setSelected(new Set())} className="hover:underline">
              Clear
            </button>
          </span>
        )}
        <span className="flex-1" />
        {actions.map((a) => {
          const { targets, selectedCount } = actionTargets(filtered, inViewSelected, rowKey, a.eligible);
          const disabled = targets.length === 0 || !!a.busy;
          return (
            <button
              key={a.id}
              type="button"
              disabled={disabled}
              onClick={() => a.run(targets)}
              className={a.primary ? primaryBtn : outlineBtn}
            >
              {a.busy && <Loader2 size={12} className="animate-spin" />}
              {actionLabel(a.verb, targets.length, selectedCount)}
            </button>
          );
        })}
        {toolbarExtra}
      </div>

      {/* ── Table ──────────────────────────────────────────────────────── */}
      <div
        ref={scrollRef}
        onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
        className="max-h-[calc(100vh-22rem)] overflow-auto rounded-b border border-border"
      >
        <table className="w-full border-collapse">
          <thead>
            <tr className="sticky top-0 z-10 bg-surface">
              <th className="w-7 border-b border-border px-2 py-1.5">
                <HeaderCheckbox state={headerState} onChange={toggleSelectAllShown} />
              </th>
              {columns.map((col) => {
                const sorted = sort.col === col.id;
                return (
                  <th
                    key={col.id}
                    onClick={() => onSortClick(col.id)}
                    className={`cursor-pointer select-none whitespace-nowrap border-b border-border px-2 py-1.5 text-left font-medium text-content-muted hover:text-content ${
                      col.numeric ? 'text-right' : ''
                    } ${sorted ? 'text-accent' : ''}`}
                  >
                    {col.label}
                    {sorted ? (sort.dir === 1 ? ' ↑' : ' ↓') : ''}
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {visibleRows.length === 0 ? (
              <tr>
                <td colSpan={colSpan} className="p-3 text-content-muted">
                  No frames match these filters ·{' '}
                  <button type="button" onClick={clearFacets} className="text-accent hover:underline">
                    Clear filters
                  </button>
                </td>
              </tr>
            ) : (
              <>
                {padTop > 0 && (
                  <tr style={{ height: padTop }}>
                    <td colSpan={colSpan} style={{ padding: 0, border: 0, height: padTop }} />
                  </tr>
                )}
                {windowed.map((vr) =>
                  vr.kind === 'group' ? (
                    <GroupRow
                      key={vr.node.id}
                      node={vr.node}
                      columns={columns}
                      selected={selected}
                      expandedIds={expanded ?? []}
                      groupAction={groupAction}
                      onToggleSelect={toggleGroupSelection}
                      onToggleExpand={toggleExpand}
                    />
                  ) : (
                    <FrameRow
                      key={vr.row.key}
                      row={vr.row}
                      depth={vr.depth}
                      columns={columns}
                      selected={selected.has(vr.row.key)}
                      onToggleSelect={toggleFrameSelection}
                      onOpen={onOpen}
                    />
                  ),
                )}
                {padBottom > 0 && (
                  <tr style={{ height: padBottom }}>
                    <td colSpan={colSpan} style={{ padding: 0, border: 0, height: padBottom }} />
                  </tr>
                )}
              </>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/* ── Row components ─────────────────────────────────────────────────────── */

function HeaderCheckbox({ state, onChange }: { state: 'none' | 'some' | 'all'; onChange: () => void }) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = state === 'some';
  }, [state]);
  return <input ref={ref} type="checkbox" checked={state === 'all'} onChange={onChange} aria-label="Select all shown" />;
}

function GroupRow({
  node,
  columns,
  selected,
  expandedIds,
  groupAction,
  onToggleSelect,
  onToggleExpand,
}: {
  node: GroupNode<FrameVM>;
  columns: ColumnDef<FrameVM>[];
  selected: ReadonlySet<string>;
  expandedIds: string[];
  groupAction?: (node: GroupNode<FrameVM>) => ReactNode;
  onToggleSelect: (node: GroupNode<FrameVM>) => void;
  onToggleExpand: (node: GroupNode<FrameVM>) => void;
}) {
  const state = checkState(node.rows, selected, rowKey);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = state === 'some';
  }, [state]);
  const isOpen = expandedIds.includes(node.id);

  return (
    <tr
      className="h-[29px] cursor-pointer border-b border-border/40 bg-surface-elevated hover:bg-surface-hover"
      onClick={() => onToggleExpand(node)}
    >
      <td className="px-2" onClick={(e) => e.stopPropagation()}>
        <input ref={ref} type="checkbox" checked={state === 'all'} onChange={() => onToggleSelect(node)} aria-label="Select group" />
      </td>
      {columns.map((col, i) => {
        if (i === 0) {
          return (
            <td key={col.id} style={{ paddingLeft: node.depth * 16 }} className="whitespace-nowrap px-2 font-medium text-content">
              <span className="mr-1 inline-block w-3 text-content-muted">{isOpen ? '▾' : '▸'}</span>
              {node.def.renderLabel(node.key)}
              <span className="ml-1.5 text-[11px] font-normal text-content-muted">{node.rows.length}</span>
              {groupAction && (
                <span onClick={(e) => e.stopPropagation()} className="ml-2 inline-block align-middle">
                  {groupAction(node)}
                </span>
              )}
            </td>
          );
        }
        if (col.id === node.def.id) return <td key={col.id} className="px-2" />;
        return (
          <td key={col.id} className={`px-2 font-normal text-content-secondary ${col.numeric ? 'text-right tabular-nums' : ''}`}>
            {col.renderAggregate?.(node.rows) ?? null}
          </td>
        );
      })}
    </tr>
  );
}

function FrameRow({
  row,
  depth,
  columns,
  selected,
  onToggleSelect,
  onOpen,
}: {
  row: FrameVM;
  depth: number;
  columns: ColumnDef<FrameVM>[];
  selected: boolean;
  onToggleSelect: (row: FrameVM) => void;
  onOpen: (row: FrameVM) => void;
}) {
  return (
    <tr
      className={`h-[29px] cursor-pointer border-b border-border/40 hover:bg-surface-hover ${selected ? 'bg-accent/5' : ''}`}
      onClick={() => onOpen(row)}
    >
      <td className="px-2" onClick={(e) => e.stopPropagation()}>
        <input type="checkbox" checked={selected} onChange={() => onToggleSelect(row)} aria-label={`Select ${row.fileName}`} />
      </td>
      {columns.map((col, i) => (
        <td
          key={col.id}
          style={i === 0 ? { paddingLeft: depth * 16 } : undefined}
          className={`px-2 text-content-secondary ${col.numeric ? 'text-right tabular-nums' : ''}`}
        >
          {col.cell(row)}
        </td>
      ))}
    </tr>
  );
}
