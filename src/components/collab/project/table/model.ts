import type { ReactNode } from 'react';

/* ── Types ──────────────────────────────────────────────────────────────── */

export type SortDir = 1 | -1;
export interface SortState { col: string; dir: SortDir }
export type NightWindow = 'all' | '1' | '7' | '30';
export const NIGHT_WINDOWS: NightWindow[] = ['all', '1', '7', '30'];

/** One table's facet selection (spec §4.1 row 1). Empty/null = inactive. */
export interface Facets {
  filters: string[];
  camera: string | null;
  night: NightWindow;
  publisher: string | null;
  state: string | null;
  search: string;
}
export const EMPTY_FACETS: Facets = { filters: [], camera: null, night: 'all', publisher: null, state: null, search: '' };

export interface ColumnDef<R> {
  id: string;
  /** Header text, units included (`FWHM″`, `Exp / Σ`). */
  label: string;
  width: number;
  numeric?: boolean;
  /** Sort key; `null` sorts last in both directions. */
  value: (r: R) => string | number | null;
  cell: (r: R) => ReactNode;
  /** Numeric group aggregate: groups sort by it when this column is the sort column. */
  aggregate?: (rows: R[]) => number | null;
  /** What the group row shows under this column (Σ exposure, x̃ FWHM, status breakdown…). */
  renderAggregate?: (rows: R[]) => ReactNode;
}

export interface GroupDef<R> {
  id: string;
  label: string;
  key: (r: R) => string;
  renderLabel: (key: string) => ReactNode;
  /** Natural order of group keys (night newest first, filters L R G B…, reasons by BLOCKER_ORDER). */
  order: (a: string, b: string) => number;
}

export interface FacetAccess<R> {
  name: (r: R) => string;
  filter: (r: R) => string;
  camera: (r: R) => string;
  night: (r: R) => string | null;
  publisher?: (r: R) => string | null;
  /** Every state-facet value the row matches (a held-back frame matches each of its failure kinds). */
  states?: (r: R) => string[];
}

export interface FacetCounts {
  filters: Map<string, number>;
  cameras: Map<string, number>;
  publishers: Map<string, number>;
  states: Map<string, number>;
  nights: Record<NightWindow, number>;
}

export interface GroupNode<R> {
  /** Path id, unique in the tree: `/night:2026-09-29/filter:Ha`. Expansion state keys on it. */
  id: string;
  key: string;
  depth: number;
  def: GroupDef<R>;
  /** Every frame under this node, sorted. */
  rows: R[];
  /** Sub-groups, or `null` at the last level (the frames are then `rows`). */
  children: GroupNode<R>[] | null;
}

export type VisibleRow<R> = { kind: 'group'; node: GroupNode<R> } | { kind: 'frame'; row: R; depth: number };

export const ROW_H = 29;

/* ── Natural orders ─────────────────────────────────────────────────────── */

export const FILTER_ORDER = ['L', 'R', 'G', 'B', 'Ha', 'OIII', 'SII', 'OSC'];
/** Mirror of `BLOCKER_ORDER` in `crates/athenaeum-core/src/collab/gate.rs` — keep in lockstep. */
export const BLOCKER_ORDER = ['analyze', 'solve', 'linkCalibration', 'buildMasters', 'attest', 'mapFilter', 'threshold', 'uuid', 'outsideTarget'];

function rankOrder(list: string[]) {
  return (a: string, b: string): number => {
    const x = list.indexOf(a);
    const y = list.indexOf(b);
    if (x !== -1 && y !== -1) return x - y;
    if (x !== -1) return -1;
    if (y !== -1) return 1;
    return a.localeCompare(b);
  };
}
export const filterOrder = rankOrder(FILTER_ORDER);
export const reasonOrder = rankOrder(BLOCKER_ORDER);
export const alphaOrder = (a: string, b: string): number => a.localeCompare(b);
/** Newest night first; the empty key ("Unknown night") last. */
export const nightOrderDesc = (a: string, b: string): number => {
  if (a === b) return 0;
  if (a === '') return 1;
  if (b === '') return -1;
  return a < b ? 1 : -1;
};

/* ── Facets ─────────────────────────────────────────────────────────────── */

export function localToday(now: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}`;
}

function daysBetween(today: string, night: string): number {
  return Math.round((Date.parse(`${today}T00:00:00Z`) - Date.parse(`${night}T00:00:00Z`)) / 86_400_000);
}

/** A catalog night is labelled by its evening date, so "last night" is today or yesterday. */
export function nightWithin(night: string | null, w: NightWindow, today: string): boolean {
  if (w === 'all') return true;
  if (!night) return false;
  const d = daysBetween(today, night);
  return d >= 0 && d <= Number(w);
}

export function matches<R>(r: R, f: Facets, a: FacetAccess<R>, today: string, except?: keyof Facets): boolean {
  if (except !== 'filters' && f.filters.length > 0 && !f.filters.includes(a.filter(r))) return false;
  if (except !== 'camera' && f.camera !== null && a.camera(r) !== f.camera) return false;
  if (except !== 'night' && !nightWithin(a.night(r), f.night, today)) return false;
  if (except !== 'publisher' && f.publisher !== null && a.publisher && a.publisher(r) !== f.publisher) return false;
  if (except !== 'state' && f.state !== null && a.states && !a.states(r).includes(f.state)) return false;
  const q = f.search.trim().toLowerCase();
  if (except !== 'search' && q !== '' && !a.name(r).toLowerCase().includes(q)) return false;
  return true;
}

export function applyFacets<R>(rows: R[], f: Facets, a: FacetAccess<R>, today: string): R[] {
  return rows.filter((r) => matches(r, f, a, today));
}

function inc(m: Map<string, number>, k: string) {
  m.set(k, (m.get(k) ?? 0) + 1);
}

/** Every count is computed against all OTHER active facets (spec §4.1). */
export function facetCounts<R>(rows: R[], f: Facets, a: FacetAccess<R>, today: string): FacetCounts {
  const out: FacetCounts = {
    filters: new Map(), cameras: new Map(), publishers: new Map(), states: new Map(),
    nights: { all: 0, '1': 0, '7': 0, '30': 0 },
  };
  for (const r of rows) {
    if (matches(r, f, a, today, 'filters')) inc(out.filters, a.filter(r));
    if (matches(r, f, a, today, 'camera')) inc(out.cameras, a.camera(r));
    if (a.publisher && matches(r, f, a, today, 'publisher')) {
      const p = a.publisher(r);
      if (p !== null) inc(out.publishers, p);
    }
    if (a.states && matches(r, f, a, today, 'state')) for (const s of new Set(a.states(r))) inc(out.states, s);
    if (matches(r, f, a, today, 'night')) for (const w of NIGHT_WINDOWS) if (nightWithin(a.night(r), w, today)) out.nights[w] += 1;
  }
  return out;
}

export function activeFacetCount(f: Facets): number {
  return (f.filters.length ? 1 : 0) + (f.camera !== null ? 1 : 0) + (f.night !== 'all' ? 1 : 0) +
    (f.publisher !== null ? 1 : 0) + (f.state !== null ? 1 : 0) + (f.search.trim() ? 1 : 0);
}

/* ── Aggregates ─────────────────────────────────────────────────────────── */

export function median(values: (number | null)[]): number | null {
  const v = values.filter((x): x is number => x !== null && Number.isFinite(x)).sort((a, b) => a - b);
  if (v.length === 0) return null;
  const m = v.length >> 1;
  return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2;
}

export function sum(values: (number | null)[]): number {
  let s = 0;
  for (const x of values) if (x !== null && Number.isFinite(x)) s += x;
  return s;
}

/* ── Sort and grouping ──────────────────────────────────────────────────── */

export function sortRows<R>(rows: R[], col: ColumnDef<R> | undefined, dir: SortDir, name: (r: R) => string): R[] {
  return [...rows].sort((x, y) => {
    if (col) {
      const a = col.value(x);
      const b = col.value(y);
      if (a === null || b === null) {
        if (a !== b) return a === null ? 1 : -1;
      } else if (a !== b) {
        const c = typeof a === 'number' && typeof b === 'number' ? a - b : String(a).localeCompare(String(b));
        if (c !== 0) return c * dir;
      }
    }
    return name(x).localeCompare(name(y));
  });
}

export function buildTree<R>(
  rows: R[], levels: GroupDef<R>[], col: ColumnDef<R> | undefined, dir: SortDir, name: (r: R) => string,
  parentId = '', depth = 0,
): GroupNode<R>[] {
  const [def, ...rest] = levels;
  if (!def) return [];
  const byKey = new Map<string, R[]>();
  for (const r of rows) {
    const k = def.key(r);
    const list = byKey.get(k);
    if (list) list.push(r);
    else byKey.set(k, [r]);
  }
  const nodes: GroupNode<R>[] = [...byKey].map(([key, rs]) => {
    const id = `${parentId}/${def.id}:${key}`;
    return {
      id, key, depth, def,
      rows: sortRows(rs, col, dir, name),
      children: rest.length ? buildTree(rs, rest, col, dir, name, id, depth + 1) : null,
    };
  });
  const agg = col?.aggregate;
  nodes.sort((a, b) => {
    if (agg) {
      const x = agg(a.rows);
      const y = agg(b.rows);
      if (x !== null && y !== null && x !== y) return (x - y) * dir;
      if ((x === null) !== (y === null)) return x === null ? 1 : -1;
    }
    return def.order(a.key, b.key);
  });
  return nodes;
}

export function flatten<R>(nodes: GroupNode<R>[], expanded: ReadonlySet<string>, out: VisibleRow<R>[] = []): VisibleRow<R>[] {
  for (const n of nodes) {
    out.push({ kind: 'group', node: n });
    if (!expanded.has(n.id)) continue;
    if (n.children) flatten(n.children, expanded, out);
    else for (const row of n.rows) out.push({ kind: 'frame', row, depth: n.depth + 1 });
  }
  return out;
}

export function initialExpanded<R>(nodes: GroupNode<R>[]): string[] {
  const first = nodes[0];
  if (!first) return [];
  return first.children?.[0] ? [first.id, first.children[0].id] : [first.id];
}

export function allGroupIds<R>(nodes: GroupNode<R>[]): string[] {
  const ids: string[] = [];
  const walk = (ns: GroupNode<R>[]) => ns.forEach((n) => { ids.push(n.id); if (n.children) walk(n.children); });
  walk(nodes);
  return ids;
}

/* ── Selection and actions ──────────────────────────────────────────────── */

export function checkState<R>(rows: R[], selected: ReadonlySet<string>, key: (r: R) => string): 'none' | 'some' | 'all' {
  let n = 0;
  for (const r of rows) if (selected.has(key(r))) n += 1;
  return n === 0 ? 'none' : n === rows.length ? 'all' : 'some';
}

/** An action's frames: the eligible part of the selection that is still in the
 *  filtered view, or of the whole view when nothing is selected. */
export function actionTargets<R>(view: R[], selected: ReadonlySet<string>, key: (r: R) => string, eligible: (r: R) => boolean): { targets: R[]; selectedCount: number } {
  if (selected.size === 0) return { targets: view.filter(eligible), selectedCount: 0 };
  const sel = view.filter((r) => selected.has(key(r)));
  return { targets: sel.filter(eligible), selectedCount: sel.length };
}

export function actionLabel(verb: string, eligible: number, selectedCount: number): string {
  if (selectedCount === 0) return `${verb} all ${eligible}`;
  return eligible === selectedCount ? `${verb} ${eligible}` : `${verb} ${eligible} of ${selectedCount}`;
}

/* ── Windowing ──────────────────────────────────────────────────────────── */

export function windowSlice(scrollTop: number, viewportH: number, total: number, rowH = ROW_H, overscan = 10) {
  if (viewportH <= 0) {
    const end = Math.min(total, 60);
    return { start: 0, end, padTop: 0, padBottom: (total - end) * rowH };
  }
  const start = Math.max(0, Math.floor(scrollTop / rowH) - overscan);
  const end = Math.min(total, Math.ceil((scrollTop + viewportH) / rowH) + overscan);
  return { start, end, padTop: start * rowH, padBottom: (total - end) * rowH };
}
