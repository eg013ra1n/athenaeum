import { describe, expect, it } from 'vitest';
import {
  EMPTY_FACETS, actionLabel, actionTargets, applyFacets, buildTree, checkState, facetCounts, filterOrder,
  flatten, initialExpanded, median, nightWithin, reasonOrder, ROW_H, sortRows, sum, windowSlice,
  type ColumnDef, type FacetAccess, type GroupDef,
} from './model';

interface Row { id: string; name: string; filter: string; camera: string; night: string | null; fwhm: number | null; exp: number; states: string[] }
const r = (id: string, o: Partial<Row> = {}): Row => ({ id, name: `f${id}.fits`, filter: 'L', camera: 'CamA', night: '2026-09-29', fwhm: 2, exp: 300, states: [], ...o });
const A: FacetAccess<Row> = { name: (x) => x.name, filter: (x) => x.filter, camera: (x) => x.camera, night: (x) => x.night, states: (x) => x.states };
const fwhm: ColumnDef<Row> = { id: 'fwhm', label: 'FWHM″', width: 60, numeric: true, value: (x) => x.fwhm, cell: () => null, aggregate: (rs) => median(rs.map((x) => x.fwhm)) };
const exp: ColumnDef<Row> = { id: 'exp', label: 'Exp / Σ', width: 60, numeric: true, value: (x) => x.exp, cell: () => null, aggregate: (rs) => sum(rs.map((x) => x.exp)) };
const byFilter: GroupDef<Row> = { id: 'filter', label: 'Filter', key: (x) => x.filter, renderLabel: (k) => k, order: filterOrder };
const byNight: GroupDef<Row> = { id: 'night', label: 'Night', key: (x) => x.night ?? '', renderLabel: (k) => k, order: (a, b) => (a < b ? 1 : a > b ? -1 : 0) };
const TODAY = '2026-09-30';

describe('nightWithin', () => {
  it('last night includes today and yesterday, not two days ago', () => {
    expect(nightWithin('2026-09-30', '1', TODAY)).toBe(true);
    expect(nightWithin('2026-09-29', '1', TODAY)).toBe(true);
    expect(nightWithin('2026-09-28', '1', TODAY)).toBe(false);
  });
  it('a row with no night only matches "all"', () => {
    expect(nightWithin(null, 'all', TODAY)).toBe(true);
    expect(nightWithin(null, '30', TODAY)).toBe(false);
  });
});

describe('facets', () => {
  const rows = [r('1', { filter: 'L' }), r('2', { filter: 'Ha' }), r('3', { filter: 'Ha', camera: 'CamB' })];
  it('filters on every active facet', () => {
    expect(applyFacets(rows, { ...EMPTY_FACETS, filters: ['Ha'], camera: 'CamB' }, A, TODAY).map((x) => x.id)).toEqual(['3']);
  });
  it('each facet count ignores its own facet but honours the others', () => {
    const c = facetCounts(rows, { ...EMPTY_FACETS, filters: ['Ha'], camera: 'CamB' }, A, TODAY);
    expect(c.filters.get('L')).toBeUndefined(); // camera CamB excludes row 1
    expect(c.filters.get('Ha')).toBe(1);
    expect(c.cameras.get('CamA')).toBe(1); // filter Ha, camera facet ignored
    expect(c.cameras.get('CamB')).toBe(1);
  });
  it('search is case-insensitive on the file name', () => {
    expect(applyFacets(rows, { ...EMPTY_FACETS, search: 'F2' }, A, TODAY).map((x) => x.id)).toEqual(['2']);
  });
  it('a state facet matches any of the row states', () => {
    const s = [r('1', { states: ['solve', 'threshold'] }), r('2', { states: ['threshold'] })];
    expect(applyFacets(s, { ...EMPTY_FACETS, state: 'solve' }, A, TODAY).map((x) => x.id)).toEqual(['1']);
  });
});

describe('median / sum', () => {
  it('ignore nulls and non-finite values', () => {
    expect(median([3, null, 1, 2])).toBe(2);
    expect(median([1, 2, 3, 4])).toBe(2.5);
    expect(median([null])).toBeNull();
    expect(sum([1, null, 2])).toBe(3);
  });
});

describe('sortRows', () => {
  it('nulls sort last in both directions, ties break on name', () => {
    const rows = [r('b', { fwhm: null }), r('a', { fwhm: 3 }), r('c', { fwhm: 1 }), r('d', { fwhm: 3 })];
    expect(sortRows(rows, fwhm, 1, (x) => x.name).map((x) => x.id)).toEqual(['c', 'a', 'd', 'b']);
    expect(sortRows(rows, fwhm, -1, (x) => x.name).map((x) => x.id)).toEqual(['a', 'd', 'c', 'b']);
  });
});

describe('buildTree / flatten', () => {
  const rows = [
    r('1', { night: '2026-09-28', filter: 'Ha', exp: 300 }),
    r('2', { night: '2026-09-29', filter: 'L', exp: 60 }),
    r('3', { night: '2026-09-29', filter: 'Ha', exp: 600 }),
  ];
  it('groups by level, natural order when the sort column has no aggregate', () => {
    const t = buildTree(rows, [byNight, byFilter], undefined, 1, (x) => x.name);
    expect(t.map((n) => n.key)).toEqual(['2026-09-29', '2026-09-28']); // newest night first
    expect(t[0].children!.map((n) => n.key)).toEqual(['L', 'Ha']); // L before Ha
  });
  it('groups sort by the sort column aggregate when it has one', () => {
    const t = buildTree(rows, [byFilter], exp, -1, (x) => x.name);
    expect(t.map((n) => n.key)).toEqual(['Ha', 'L']); // Σ 900 > 60
  });
  it('flatten shows only expanded groups; initialExpanded opens the first group and its first child', () => {
    const t = buildTree(rows, [byNight, byFilter], undefined, 1, (x) => x.name);
    const open = new Set(initialExpanded(t));
    const v = flatten(t, open);
    expect(v.map((x) => (x.kind === 'group' ? `g:${x.node.key}` : `f:${x.row.id}`))).toEqual([
      'g:2026-09-29', 'g:L', 'f:2', 'g:Ha', 'g:2026-09-28',
    ]);
  });
});

describe('selection and actions', () => {
  const view = [r('1'), r('2'), r('3')];
  const key = (x: Row) => x.id;
  it('tri-state group checkbox', () => {
    expect(checkState(view, new Set(), key)).toBe('none');
    expect(checkState(view, new Set(['1']), key)).toBe('some');
    expect(checkState(view, new Set(['1', '2', '3']), key)).toBe('all');
  });
  it('targets are the eligible part of the selection inside the view, or of the whole view with no selection', () => {
    const elig = (x: Row) => x.id !== '2';
    expect(actionTargets(view, new Set(['1', '2', 'gone']), key, elig)).toEqual({ targets: [view[0]], selectedCount: 2 });
    expect(actionTargets(view, new Set(), key, elig)).toEqual({ targets: [view[0], view[2]], selectedCount: 0 });
  });
  it('labels say what they act on', () => {
    expect(actionLabel('Publish', 34, 50)).toBe('Publish 34 of 50');
    expect(actionLabel('Publish', 50, 50)).toBe('Publish 50');
    expect(actionLabel('Publish', 214, 0)).toBe('Publish all 214');
  });
});

describe('windowSlice', () => {
  it('the row pitch is the rendered 28 px: a 28 px border-box cell holds its own 1 px separator (fix round 1)', () => {
    expect(ROW_H).toBe(28);
  });
  it('slices around the viewport with overscan and pads the rest', () => {
    expect(windowSlice(ROW_H * 100, ROW_H * 20, 5000, ROW_H, 10)).toEqual({ start: 90, end: 130, padTop: 90 * ROW_H, padBottom: (5000 - 130) * ROW_H });
  });
  it('an unmeasured viewport (jsdom) renders the first 60 rows', () => {
    expect(windowSlice(0, 0, 5000)).toMatchObject({ start: 0, end: 60 });
    expect(windowSlice(0, 0, 3)).toMatchObject({ start: 0, end: 3, padBottom: 0 });
  });
  it('a stale scrollTop past a shrunk row count clamps to a valid, non-empty slice', () => {
    const s = windowSlice(20000, ROW_H * 10, 10, ROW_H, 10);
    expect(s.start).toBeLessThanOrEqual(s.end);
    expect(s.end).toBeGreaterThan(s.start);
    expect(s.padTop + (s.end - s.start) * ROW_H + s.padBottom).toBe(10 * ROW_H);
  });
  it('padTop + visible*rowH + padBottom always accounts for the whole row count', () => {
    const rowH = ROW_H;
    const cases: Array<[number, number]> = [
      [0, 5000], [rowH * 100, 5000], [1_000_000, 5000], [0, 0], [10_000, 1], [rowH * 3, 1],
    ];
    for (const [scrollTop, total] of cases) {
      const s = windowSlice(scrollTop, rowH * 10, total, rowH, 10);
      expect(s.start).toBeLessThanOrEqual(s.end);
      expect(s.padTop + (s.end - s.start) * rowH + s.padBottom).toBe(total * rowH);
    }
  });
});

describe('orders', () => {
  it('filters follow L R G B Ha OIII SII OSC, unknown after, alphabetical', () => {
    expect(['Zz', 'Ha', 'L', 'OSC', 'Aa'].sort(filterOrder)).toEqual(['L', 'Ha', 'OSC', 'Aa', 'Zz']);
  });
  it('reasons follow the core BLOCKER_ORDER', () => {
    expect(['threshold', 'solve', 'analyze'].sort(reasonOrder)).toEqual(['analyze', 'solve', 'threshold']);
  });
});
