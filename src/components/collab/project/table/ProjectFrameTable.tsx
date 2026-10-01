import { useEffect, useLayoutEffect, useMemo, useRef, useState, type JSX, type ReactNode } from 'react';
import { Loader2 } from 'lucide-react';
import { useSessionState } from '../../../../contexts/SessionStateContext';
import { Button, EmptyState, FilterChip, Popover, Select, TextInput } from '../../../ui';
import { formatDurationPadded, formatSize } from '../../format';
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
  /** The row whose side panel is open — it gets the active background. */
  activeKey?: string | null;
  /** Rendered left of "Columns ⚙" on the group row (Library: Export for WBPP). */
  groupRowExtra?: ReactNode;
  today?: string; // test seam; default localToday()
}

/** The checkbox column's width — the mockup measures 34 px. */
const CHECK_COL_W = 34;

/** Checkbox column + Frame at its 220 px minimum + every other column's
 *  fixed width (spec §5.1): below this the table scrolls horizontally. */
export function tableMinWidth(columns: ColumnDef<FrameVM>[]): number {
  return CHECK_COL_W + 220 + columns.filter((c) => c.id !== 'name').reduce((a, c) => a + c.width, 0);
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

/** Mockup `table.ft th` — sticky 30 px header on `table-head`. */
const TH = 'sticky top-0 z-[2] h-[30px] select-none overflow-hidden text-ellipsis whitespace-nowrap border-b border-border bg-table-head px-2 text-[11.5px] font-medium';
/** Mockup `table.ft td` — a 28 px border-box cell whose 1 px `line-soft`
 *  separator sits inside it, so the row pitch is 28 px (`ROW_H`); never
 *  wraps, truncates with an ellipsis. */
const TD = 'h-7 overflow-hidden text-ellipsis whitespace-nowrap border-b border-line-soft px-2';

/** Spec §5.2 item 4 — the table box reaches the window bottom, minimum `min`px.
 *  Refits on a window resize and whenever the table's own block or the page
 *  body changes size (the filter row wraps once the side panel opens, the
 *  totals bar wraps, a card appears above), not only once at mount. */
function useFillHeight(el: HTMLElement | null, min: number): number | undefined {
  const [h, setH] = useState<number | undefined>(undefined);
  useLayoutEffect(() => {
    if (!el) return undefined;
    const fit = () => setH(Math.max(min, Math.floor(window.innerHeight - el.getBoundingClientRect().top - 24)));
    fit();
    window.addEventListener('resize', fit);
    let ro: ResizeObserver | null = null;
    if (typeof ResizeObserver !== 'undefined') {
      ro = new ResizeObserver(() => fit());
      if (el.parentElement) ro.observe(el.parentElement);
      ro.observe(document.body);
    }
    return () => {
      window.removeEventListener('resize', fit);
      ro?.disconnect();
    };
  }, [el, min]);
  return h;
}

/* ── Component ──────────────────────────────────────────────────────────── */

export default function ProjectFrameTable(props: ProjectFrameTableProps): JSX.Element {
  const { tableId, scope, rows, actions, onOpen, emptyText, groupAction, toolbarExtra, activeKey = null, groupRowExtra } = props;
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
  const fillHeight = useFillHeight(scrollEl, 360);

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
    return <EmptyState>{emptyText}</EmptyState>;
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
    <div className="flex flex-col text-[13px] leading-[1.4]">
      {/* ── Filter row — mockup .toolbar ───────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5 py-2">
        <span className="-mr-1 text-[11.5px] text-content-faint">Filter</span>
        {presentFilters.map((f) => (
          <FilterChip key={f} filter={f} count={counts.filters.get(f) ?? 0} on={facets.filters.includes(f)} onClick={() => toggleFilterChip(f)} />
        ))}

        <span aria-hidden className="h-[18px] w-px bg-line" />

        <Select
          aria-label="Camera"
          value={facets.camera ?? ALL}
          onChange={(e) => setFacets((prev) => ({ ...prev, camera: e.target.value === ALL ? null : e.target.value }))}
        >
          <option value={ALL}>All cameras</option>
          {presentCameras.map((c) => (
            <option key={c} value={c}>
              {c === '' ? 'Unknown camera' : c} ({counts.cameras.get(c) ?? 0})
            </option>
          ))}
        </Select>

        <Select
          aria-label="Night"
          value={facets.night}
          onChange={(e) => setFacets((prev) => ({ ...prev, night: e.target.value as NightWindow }))}
        >
          {NIGHT_WINDOWS.map((w) => (
            <option key={w} value={w}>
              {NIGHT_LABEL[w]} ({counts.nights[w]})
            </option>
          ))}
        </Select>

        {config.publisherFacet && (
          <Select
            aria-label="Publisher"
            value={facets.publisher ?? ALL}
            onChange={(e) => setFacets((prev) => ({ ...prev, publisher: e.target.value === ALL ? null : e.target.value }))}
          >
            <option value={ALL}>All publishers</option>
            {presentPublishers.map((p) => (
              <option key={p} value={p}>
                {p} ({counts.publishers.get(p) ?? 0})
              </option>
            ))}
          </Select>
        )}

        {config.stateFacet && (
          <Select
            aria-label={config.stateFacet.label}
            value={facets.state ?? ALL}
            onChange={(e) => setFacets((prev) => ({ ...prev, state: e.target.value === ALL ? null : e.target.value }))}
          >
            <option value={ALL}>{config.stateFacet.label}: any</option>
            {config.stateFacet.options.map(([v, l]) => (
              <option key={v} value={v}>
                {l} ({counts.states.get(v) ?? 0})
              </option>
            ))}
          </Select>
        )}

        <TextInput
          placeholder="Search file name"
          aria-label="Search file name"
          value={facets.search}
          onChange={(e) => setFacets((prev) => ({ ...prev, search: e.target.value }))}
          className="w-[170px]"
        />

        <Button variant="link" disabled={active === 0} onClick={clearFacets}>
          Clear filters
        </Button>
      </div>

      {/* ── Group row ──────────────────────────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5 pb-2">
        <span className="-mr-1 text-[11.5px] text-content-faint">Group by</span>
        <Select aria-label="Group by" value={group[0]} onChange={(e) => setGroup0(e.target.value)}>
          {config.groupings.map((id) => (
            <option key={id} value={id}>
              {GROUPS[id].label}
            </option>
          ))}
        </Select>
        {group[0] !== 'none' && (
          <>
            <span className="text-[13px] text-content-faint">▸</span>
            <Select aria-label="Then group by" value={group[1]} onChange={(e) => setGroup1(e.target.value)}>
              {config.groupings.filter((id) => id !== group[0]).map((id) => (
                <option key={id} value={id}>
                  {GROUPS[id].label}
                </option>
              ))}
            </Select>
          </>
        )}
        <Button variant="link" disabled={levelDefs.length === 0} onClick={() => setExpanded(allGroupIds(tree))}>
          Expand all
        </Button>
        <Button variant="link" disabled={levelDefs.length === 0} onClick={() => setExpanded([])}>
          Collapse all
        </Button>
        <span className="flex-1" />
        {groupRowExtra}
        <div className="relative">
          <Button onClick={() => setColumnsOpen((o) => !o)}>Columns ⚙</Button>
          <Popover open={columnsOpen} onClose={() => setColumnsOpen(false)}>
            {config.columns.filter((id) => id !== 'name').map((id) => (
              <CbBox
                key={id}
                checked={visibleColIds.includes(id)}
                onChange={() => toggleColumn(id)}
                label={COLUMNS[id].label}
                className="flex items-center gap-2 whitespace-nowrap text-[12px] text-content-muted"
              >
                {COLUMNS[id].label}
              </CbBox>
            ))}
          </Popover>
        </div>
      </div>

      {/* ── Totals — mockup .totals ────────────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-x-3.5 gap-y-1.5 rounded-t-md border border-b-0 border-line bg-surface-elevated px-2.5 py-[7px] text-[12px] text-content-muted">
        <span>
          Showing <strong className="font-semibold text-content">{filtered.length.toLocaleString('en-US')}</strong> of {rows.length.toLocaleString('en-US')} frames
        </span>
        <span>
          Σ <strong className="font-semibold text-content">{formatDurationPadded(totalExp)}</strong>
        </span>
        <strong className="font-semibold text-content">{formatSize(totalBytes)}</strong>
        <span>
          FWHM x̃ <strong className="font-semibold text-content">{medFwhm === null ? '—' : `${medFwhm.toFixed(2)}″`}</strong>
        </span>
        {selected.size > 0 && (
          <span className="flex items-center gap-1 text-[12px] text-accent">
            <span>
              {selected.size} selected{hiddenSelected > 0 ? ` · ${hiddenSelected} hidden` : ''}
            </span>
            <span>·</span>
            <Button variant="link" size="sm" onClick={() => setSelected(new Set())}>
              Clear
            </Button>
          </span>
        )}
        <span className="flex-1" />
        {actions.map((a) => {
          const { targets, selectedCount } = actionTargets(filtered, inViewSelected, rowKey, a.eligible);
          const disabled = targets.length === 0 || !!a.busy;
          return (
            <Button key={a.id} variant={a.primary ? 'primary' : 'default'} disabled={disabled} onClick={() => a.run(targets)}>
              {a.busy && <Loader2 size={12} className="animate-spin" />}
              {actionLabel(a.verb, targets.length, selectedCount)}
            </Button>
          );
        })}
        {toolbarExtra}
      </div>

      {/* ── Table box — mockup .tscroll ────────────────────────────────── */}
      <div
        ref={scrollRef}
        data-testid="frame-table-scroll"
        onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
        style={{ height: fillHeight }}
        className="overflow-auto rounded-b-md border border-line bg-surface"
      >
        {/* min-width on a wrapper, not the <table> (undefined on tables in
            CSS 2.1; WebKit may ignore it): Frame never drops below 220 px,
            then the box scrolls horizontally. The table fills this div. */}
        <div style={{ minWidth: tableMinWidth(columns) }}>
          <table className="w-full table-fixed border-separate border-spacing-0">
            {/* Widths come only from the column definitions, never from the
                rendered rows, so windowing can never move a column (spec §5.1). */}
            <colgroup>
              <col style={{ width: CHECK_COL_W }} />
              {columns.map((c) => (
                <col key={c.id} style={c.id === 'name' ? undefined : { width: c.width }} />
              ))}
            </colgroup>
            <thead>
              <tr>
                <th scope="col" className={`${TH} text-left text-content-faint`}>
                  <CbBox state={headerState} onChange={toggleSelectAllShown} label="Select all shown" />
                </th>
                {columns.map((col) => (
                  <th
                    key={col.id}
                    scope="col"
                    onClick={() => onSortClick(col.id)}
                    className={`${TH} cursor-pointer hover:text-content ${col.numeric ? 'text-right' : 'text-left'} ${sort.col === col.id ? 'text-accent' : 'text-content-faint'}`}
                  >
                    {col.label}
                    {sort.col === col.id ? (sort.dir === 1 ? ' ↑' : ' ↓') : ''}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {visibleRows.length === 0 ? (
                <tr>
                  <td colSpan={colSpan} className="px-3 py-[18px] text-[12.5px] text-content-faint">
                    No frames match these filters ·{' '}
                    <Button variant="link" onClick={clearFacets}>
                      Clear filters
                    </Button>
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
                        active={activeKey !== null && activeKey === vr.row.key}
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
    </div>
  );
}

/* ── Row components ─────────────────────────────────────────────────────── */

type CheckState = 'none' | 'some' | 'all';

/** Mockup `.cb` / `.cb.on` / `.cb.mid` over a real, visually hidden checkbox
 *  (keyboard, label and a11y stay native). The table's own box — the app's
 *  `Checkbox` does not expose the indeterminate state. `children` render
 *  inside the same `<label>` (the Columns popover rows), so labels never nest. */
function CbBox({
  state,
  checked,
  onChange,
  label,
  className = '',
  children,
}: {
  state?: CheckState;
  checked?: boolean;
  onChange: () => void;
  label: string;
  className?: string;
  children?: ReactNode;
}) {
  const s: CheckState = state ?? (checked ? 'all' : 'none');
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = s === 'some';
  }, [s]);
  const tone = s === 'all' ? 'border-accent bg-accent' : s === 'some' ? 'border-accent-muted bg-accent-muted' : 'border-border bg-surface';
  return (
    <label className={`relative cursor-pointer ${className}`}>
      <input ref={ref} type="checkbox" className="peer sr-only" checked={s === 'all'} onChange={onChange} aria-label={label} />
      <span
        aria-hidden
        className={`relative inline-block h-[13px] w-[13px] shrink-0 rounded-[3px] border align-[-2px] peer-focus-visible:outline peer-focus-visible:outline-2 peer-focus-visible:outline-accent ${tone}`}
      >
        {s === 'all' && (
          <svg width="9" height="9" viewBox="0 0 9 9" className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 text-surface">
            <path d="M1.5 4.5 3.5 6.5 7.5 2" stroke="currentColor" strokeWidth="2" fill="none" />
          </svg>
        )}
        {s === 'some' && <span className="absolute left-[2px] top-[5px] h-[2px] w-[7px] bg-content" />}
      </span>
      {children}
    </label>
  );
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
  const isOpen = expandedIds.includes(node.id);

  return (
    <tr
      className={`cursor-pointer ${node.depth === 0 ? 'bg-table-group' : 'bg-table-group-l1'} hover:bg-table-group-hover`}
      onClick={() => onToggleExpand(node)}
    >
      <td className={TD} onClick={(e) => e.stopPropagation()}>
        <CbBox state={state} onChange={() => onToggleSelect(node)} label="Select group" />
      </td>
      {columns.map((col, i) => {
        if (i === 0) {
          return (
            <td key={col.id} style={{ paddingLeft: 8 + node.depth * 18 }} className={`${TD} font-medium text-content`}>
              <span className="inline-block w-3.5 text-[13px] text-content-faint">{isOpen ? '▾' : '▸'}</span>
              {node.def.renderLabel(node.key)}
              <span className="ml-1.5 text-[11px] font-normal text-content-faint">{node.rows.length} fr</span>
              {groupAction && (
                <span onClick={(e) => e.stopPropagation()} className="ml-2 inline-block align-middle">
                  {groupAction(node)}
                </span>
              )}
            </td>
          );
        }
        if (col.id === node.def.id) return <td key={col.id} className={TD} />;
        // Spec §5.1: a group row is weight 500 `content`; its numeric cells
        // (x̃ 2.42, Σ 18h 42m, size sums) are `content-muted` weight 400.
        return (
          <td
            key={col.id}
            className={`${TD} ${col.numeric ? 'text-right font-normal text-content-muted' : 'font-medium text-content'}`}
          >
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
  active,
  onToggleSelect,
  onOpen,
}: {
  row: FrameVM;
  depth: number;
  columns: ColumnDef<FrameVM>[];
  selected: boolean;
  active: boolean;
  onToggleSelect: (row: FrameVM) => void;
  onOpen: (row: FrameVM) => void;
}) {
  return (
    <tr
      // Mockup order: the active and selected backgrounds win over hover.
      className={`cursor-pointer ${active ? 'bg-accent/[0.16]' : selected ? 'bg-accent/[0.08]' : 'hover:bg-table-row-hover'}`}
      onClick={() => onOpen(row)}
    >
      <td className={TD} onClick={(e) => e.stopPropagation()}>
        <CbBox checked={selected} onChange={() => onToggleSelect(row)} label={`Select ${row.fileName}`} />
      </td>
      {columns.map((col, i) => (
        <td
          key={col.id}
          // Mockup: 8 + 18 per level + the 14 px caret column of its group.
          style={i === 0 ? { paddingLeft: 8 + depth * 18 + 14 } : undefined}
          title={col.id === 'name' ? row.fileName : undefined}
          className={`${TD} text-content-secondary ${col.numeric ? 'text-right' : ''}`}
        >
          {col.cell(row)}
        </td>
      ))}
    </tr>
  );
}
