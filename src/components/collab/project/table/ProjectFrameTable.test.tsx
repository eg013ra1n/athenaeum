import { beforeEach, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { SessionStateProvider } from '../../../../contexts/SessionStateContext';
import type { OwnFrameRow, ProjectFrameView } from '../../../../types/models';
import { fromLibrary, fromOwn } from '../frames';
import type { FrameVM } from '../frames';
import ProjectFrameTable, { type ProjectFrameTableProps } from './ProjectFrameTable';
import { ROW_H } from './model';

function own(o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return {
    frameId: 1,
    frameUuid: 'u-own-1',
    fileName: 'Light_Ha_300s_0001.fits',
    setId: 1,
    setName: 'M31',
    night: '2026-09-29',
    filter: 'Ha',
    filterMapped: true,
    camera: 'ASI2600MM Pro',
    exptimeSec: 300,
    byteSize: 42_000_000,
    fwhmArcsec: 2.4,
    eccentricity: 0.4,
    starsDetected: 1200,
    medianSnr: 18,
    segment: 'ready',
    contributorState: 'published',
    contributorReason: null,
    failures: [],
    contentVersion: 1,
    pubState: null,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    localState: null,
    publishedAt: null,
    lastError: null,
    rules: [],
    path: '/data/m31/Light_Ha_300s_0001.fits',
    accepted: null,
    calibratedPath: null,
    calibratedBytes: null,
    preparedAt: null,
    withheld: false,
    ...o,
  };
}

/** Test-local FrameVM factory for the "ready" table (spec: task-7-brief.md).
 *  `o` uses friendly short names (filter/night/fwhm) mapped onto OwnFrameRow. */
function ready(id: string, o: { filter?: string; night?: string | null; fwhm?: number | null; frameId?: number } = {}): FrameVM {
  const { fwhm, ...rest } = o;
  return fromOwn(
    own({
      frameId: Number(id),
      frameUuid: null,
      fileName: `f${id}.fits`,
      segment: 'ready',
      night: null,
      ...(fwhm !== undefined ? { fwhmArcsec: fwhm } : {}),
      ...rest,
    }),
  );
}

function lib(o: Partial<ProjectFrameView> = {}): ProjectFrameView {
  return {
    frameUuid: 'u-lib-1',
    fileName: 'Light_Ha_300s_0002.fits',
    publisher: 'Kostya',
    publisherAccountId: 'acc-kostya',
    own: false,
    filter: 'Ha',
    exptimeSec: 300,
    dateObs: '2026-09-29T22:00:00Z',
    state: 'published',
    accepted: true,
    acceptedReason: null,
    localState: 'held',
    onDisk: true,
    holdersOnline: 1,
    holdersTotal: 1,
    waitingForPublisher: false,
    newVersionWaiting: false,
    byteSize: 42_000_000,
    contentVersion: 1,
    lastError: null,
    fwhmArcsec: 2.4,
    eccentricity: 0.4,
    starsDetected: 1200,
    camera: 'ASI2600MM Pro',
    telescope: null,
    medianSnr: 18,
    night: '2026-09-29',
    contributorState: null,
    contributorReason: null,
    receivedAt: null,
    receivedFromDevice: null,
    receivedFromMember: null,
    ...o,
  };
}

/** `n` library `FrameVM` rows — one publisher, one filter, so the default
 *  Publisher ▸ Filter grouping opens them all in its first group. */
function rows(n: number): FrameVM[] {
  return Array.from({ length: n }, (_, i) =>
    fromLibrary(lib({ frameUuid: `u-lib-${i + 1}`, fileName: `Light_Ha_300s_${String(i + 1).padStart(4, '0')}.fits` }), new Map()),
  );
}

beforeEach(() => {
  localStorage.clear();
});

function renderTable(p: Partial<ProjectFrameTableProps> & { rows: FrameVM[] }) {
  const onOpen = vi.fn();
  const utils = render(
    <SessionStateProvider>
      <ProjectFrameTable tableId="ready" scope="p1" actions={[]} onOpen={onOpen} emptyText="Nothing ready." today="2026-09-30" {...p} />
    </SessionStateProvider>,
  );
  return { ...utils, onOpen };
}

it('an empty table shows only its sentence and no actions', () => {
  renderTable({ rows: [], actions: [{ id: 'pub', verb: 'Publish', eligible: () => true, primary: true, run: vi.fn() }] });
  expect(screen.getByText('Nothing ready.')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: /Publish/ })).toBeNull();
});

it('the primary action covers the filtered view, then the eligible part of the selection', () => {
  const run = vi.fn();
  const rows = [ready('1', { filter: 'L' }), ready('2', { filter: 'Ha' }), ready('3', { filter: 'Ha' })];
  renderTable({ rows, actions: [{ id: 'pub', verb: 'Publish', eligible: (r) => r.frameId !== 3, primary: true, run }] });
  expect(screen.getByRole('button', { name: 'Publish all 2' })).toBeEnabled();
  fireEvent.click(screen.getByRole('button', { name: /^Ha/ })); // filter chip
  expect(screen.getByRole('button', { name: 'Publish all 1' })).toBeEnabled();
  fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
  fireEvent.click(screen.getByRole('button', { name: 'Publish 1 of 2' }));
  expect(run).toHaveBeenCalledWith([rows[1]]);
});

it('selection is pruned when rows disappear', () => {
  const rows = [ready('1'), ready('2')];
  const { rerender } = renderTable({ rows, actions: [{ id: 'pub', verb: 'Publish', eligible: () => true, run: vi.fn() }] });
  fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
  expect(screen.getByRole('button', { name: 'Publish 2' })).toBeInTheDocument();
  rerender(
    <SessionStateProvider>
      <ProjectFrameTable
        tableId="ready"
        scope="p1"
        rows={[rows[1]]}
        actions={[{ id: 'pub', verb: 'Publish', eligible: () => true, run: vi.fn() }]}
        onOpen={vi.fn()}
        emptyText="x"
        today="2026-09-30"
      />
    </SessionStateProvider>,
  );
  expect(screen.getByRole('button', { name: 'Publish 1' })).toBeInTheDocument();
});

it('default grouping opens the first group and its first child; clicking a frame opens it', () => {
  const rows = [ready('1', { night: '2026-09-29', filter: 'L' }), ready('2', { night: '2026-09-28', filter: 'Ha' })];
  const { onOpen } = renderTable({ rows });
  expect(screen.getByText('f1.fits')).toBeInTheDocument();
  expect(screen.queryByText('f2.fits')).toBeNull(); // second night collapsed
  fireEvent.click(screen.getByText('f1.fits'));
  expect(onOpen).toHaveBeenCalledWith(rows[0]);
});

it('facet counts ignore their own facet', () => {
  renderTable({ rows: [ready('1', { filter: 'L' }), ready('2', { filter: 'Ha' })] });
  fireEvent.click(screen.getByRole('button', { name: /^Ha/ }));
  expect(screen.getByRole('button', { name: /^L\s*1/ })).toBeInTheDocument(); // still counted
});

it('renders a window, not 5000 rows', () => {
  const rows = Array.from({ length: 5000 }, (_, i) => ready(String(i + 1)));
  renderTable({ rows });
  fireEvent.change(screen.getByLabelText('Group by'), { target: { value: 'none' } });
  expect(screen.getAllByRole('row').length).toBeLessThan(100);
});

it('a null metric sorts after real values', () => {
  const rows = [ready('1', { fwhm: null }), ready('2', { fwhm: 3 }), ready('3', { fwhm: 1 })];
  renderTable({ rows });
  fireEvent.change(screen.getByLabelText('Group by'), { target: { value: 'none' } });
  fireEvent.click(screen.getByRole('columnheader', { name: /FWHM/ }));
  const names = screen.getAllByRole('row').map((r) => r.textContent ?? '').filter((t) => t.includes('.fits'));
  expect(names.map((t) => t.match(/f\d+\.fits/)![0])).toEqual(['f3.fits', 'f2.fits', 'f1.fits']);
});

/* ── Fix round 1 (task review, 2026-09-30) ─────────────────────────────── */

it('the windowing observer re-attaches when the table mounts later with rows (fix 1)', () => {
  // jsdom has no ResizeObserver at all — install a spy-backed stub for this
  // test only, so we can assert `observe` is called against the scroll
  // element once it exists, not just once on the very first mount.
  const observe = vi.fn();
  class MockResizeObserver {
    constructor(_cb: ResizeObserverCallback) {}
    observe = observe;
    unobserve = vi.fn();
    disconnect = vi.fn();
  }
  const original = (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = MockResizeObserver;
  try {
    const { rerender } = renderTable({ rows: [] });
    expect(observe).not.toHaveBeenCalled(); // no scroll container exists yet — the <p> sentence only

    rerender(
      <SessionStateProvider>
        <ProjectFrameTable
          tableId="ready" scope="p1" actions={[]} onOpen={vi.fn()} emptyText="Nothing ready." today="2026-09-30"
          rows={[ready('1')]}
        />
      </SessionStateProvider>,
    );

    // The windowing observer watches the scroll element itself (the fill-height
    // observer also watches its parent and <body> — fix round 1 #2).
    const scroller = document.querySelector('[data-testid="frame-table-scroll"]');
    expect(scroller).toBeInstanceOf(HTMLElement);
    expect(observe.mock.calls.map((c) => c[0])).toContain(scroller);
  } finally {
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = original;
  }
});

it('initial expansion is not consumed by an empty tree (fix 2)', () => {
  const { rerender } = renderTable({ rows: [] }); // tree is empty on this first render
  rerender(
    <SessionStateProvider>
      <ProjectFrameTable
        tableId="ready" scope="p1" actions={[]} onOpen={vi.fn()} emptyText="Nothing ready." today="2026-09-30"
        rows={[ready('1', { night: '2026-09-29', filter: 'L' })]}
      />
    </SessionStateProvider>,
  );
  // Had the empty tree's `[]` latched in as "already initialised", this
  // group would still be collapsed and the frame invisible.
  expect(screen.getByText('f1.fits')).toBeInTheDocument();
});

it('a selection fully hidden by a facet still lets the primary action cover the view, and the strip says so (fix 3)', () => {
  const run = vi.fn();
  const rows = [ready('1', { filter: 'L' }), ready('2', { filter: 'Ha' })];
  renderTable({ rows, actions: [{ id: 'pub', verb: 'Publish', eligible: () => true, primary: true, run }] });

  fireEvent.click(screen.getByRole('checkbox', { name: 'Select f1.fits' })); // select the L frame
  fireEvent.click(screen.getByRole('button', { name: /^Ha/ })); // filter down to Ha only — hides the selection

  expect(screen.getByRole('button', { name: 'Publish all 1' })).toBeEnabled(); // covers the filtered view, not "0"
  expect(screen.getByText('1 selected · 1 hidden')).toBeInTheDocument();

  fireEvent.click(screen.getByRole('button', { name: 'Publish all 1' }));
  expect(run).toHaveBeenCalledWith([rows[1]]);
});

/* ── Wave 5.5 Task 6: the mockup's fixed-layout geometry ───────────────── */

it('lays out with a fixed colgroup from the column definitions, independent of the rendered rows', () => {
  const { container } = renderTable({ tableId: 'library', rows: rows(400) });
  const table = container.querySelector('table')!;
  expect(table.className).toContain('table-fixed');
  const cols = [...container.querySelectorAll('col')].map((c) => (c as HTMLElement).style.width);
  expect(cols[0]).toBe('34px'); // the mockup's checkbox column
  expect(cols[1]).toBe(''); // Frame takes the rest
  expect(cols).toContain('108px'); // Publisher
  const scroller = container.querySelector('[data-testid="frame-table-scroll"]')!;
  fireEvent.scroll(scroller, { target: { scrollTop: 5000 } });
  expect([...container.querySelectorAll('col')].map((c) => (c as HTMLElement).style.width)).toEqual(cols);
});

it('sets the min-width on a wrapper around the table so Frame never drops below 220px (review focus 3)', () => {
  const { container } = renderTable({ tableId: 'library', rows: rows(3) });
  const table = container.querySelector('table') as HTMLElement;
  const wrapper = table.parentElement as HTMLElement;
  const expected = 34 + 220 + [108, 98, 70, 122, 82, 66, 60, 88, 168, 78].reduce((a, b) => a + b, 0);
  // On the wrapper, not the <table>: min-width on a table is undefined in CSS 2.1.
  expect(wrapper.style.minWidth).toBe(`${expected}px`);
  expect(wrapper.parentElement!.dataset.testid).toBe('frame-table-scroll');
  expect(table.style.minWidth).toBe('');
});

it('right-aligns numeric headers and truncates cells instead of wrapping (review focus 1)', () => {
  const long = rows(1).map((r) => ({ ...r, fileName: `${'Light_M31_Ha_300s_'.repeat(4)}0001.fits` }));
  renderTable({ tableId: 'library', rows: long });
  expect(screen.getByRole('columnheader', { name: /Size/ }).className).toContain('text-right');
  const cell = screen.getByText(/0001\.fits/).closest('td')!;
  expect(cell.className).toContain('whitespace-nowrap');
  expect(cell.className).toContain('text-ellipsis');
});

it('every row cell is the 28 px border-box box ROW_H assumes (windowing invariant, other side)', () => {
  // `h-7` = 28 px, border-box, its 1 px `line-soft` separator inside it, no
  // vertical padding, no wrap: the rendered pitch IS ROW_H. Change one side
  // and this test or ROW_H's own test fails.
  expect(ROW_H).toBe(28);
  renderTable({ tableId: 'library', rows: rows(2) });
  const frameRow = screen.getByText('Light_Ha_300s_0001.fits').closest('tr')!;
  const groupRow = screen.getAllByRole('checkbox', { name: 'Select group' })[0].closest('tr')!;
  for (const tr of [frameRow, groupRow]) {
    for (const td of Array.from(tr.querySelectorAll('td'))) {
      const cls = td.className.split(/\s+/);
      expect(cls).toContain('h-7');
      expect(cls).toContain('border-b');
      expect(cls).toContain('whitespace-nowrap');
      expect(cls.filter((c) => /^(p[ytb]|h)-/.test(c) && c !== 'h-7')).toEqual([]);
    }
  }
});

it('marks the active row', () => {
  const r = rows(2);
  renderTable({ tableId: 'library', rows: r, activeKey: r[1].key });
  expect(screen.getByText(r[1].fileName).closest('tr')!.className).toContain('bg-accent/[0.16]');
  // An idle row hovers with the frame-table token (spec §5.1), no raw colour.
  const idle = screen.getByText(r[0].fileName).closest('tr')!.className;
  expect(idle).toContain('hover:bg-table-row-hover');
  expect(idle).not.toContain('rgba(');
});

it('renders groupRowExtra next to Columns', () => {
  renderTable({ tableId: 'library', rows: rows(2), groupRowExtra: <button>Export for WBPP</button> });
  expect(screen.getByRole('button', { name: 'Export for WBPP' })).toBeInTheDocument();
});

/* ── Task 6 fix round 1 ────────────────────────────────────────────────── */

it('toggling a column keeps <col>, <th> and every row\'s <td> in lockstep', () => {
  const r = rows(2);
  const { container } = renderTable({ tableId: 'library', rows: r });
  fireEvent.click(screen.getByRole('button', { name: 'Columns ⚙' }));
  fireEvent.click(screen.getByRole('checkbox', { name: 'Size' }));
  const colCount = container.querySelectorAll('col').length;
  expect(colCount).toBe(container.querySelectorAll('th').length);
  expect(screen.getByText(r[0].fileName).closest('tr')!.querySelectorAll('td').length).toBe(colCount);
  expect(screen.queryByRole('columnheader', { name: /Size/ })).toBeNull();
});

it('indents like the mockup: group rows 8 + 18·depth, frame rows 8 + 18·depth + 14', () => {
  // ready: Night ▸ Filter — group depth 0 and 1, frames at depth 2.
  renderTable({ rows: [ready('1', { night: '2026-09-29', filter: 'L' })] });
  const groupLabelCells = screen
    .getAllByRole('checkbox', { name: 'Select group' })
    .map((cb) => cb.closest('tr')!.querySelectorAll('td')[1] as HTMLElement);
  expect(groupLabelCells.map((td) => td.style.paddingLeft)).toEqual(['8px', '26px']);
  expect((screen.getByText('f1.fits').closest('td') as HTMLElement).style.paddingLeft).toBe('58px');

  // One grouping level → the frame sits at depth 1.
  fireEvent.change(screen.getByLabelText('Then group by'), { target: { value: 'none' } });
  expect((screen.getByText('f1.fits').closest('td') as HTMLElement).style.paddingLeft).toBe('40px');
});

it('refits the table height when the content around it resizes, and disconnects on unmount', () => {
  const instances: { cb: ResizeObserverCallback; targets: Element[]; disconnect: ReturnType<typeof vi.fn> }[] = [];
  class MockResizeObserver {
    cb: ResizeObserverCallback;
    targets: Element[] = [];
    disconnect = vi.fn();
    constructor(cb: ResizeObserverCallback) {
      this.cb = cb;
      instances.push(this);
    }
    observe = (t: Element) => { this.targets.push(t); };
    unobserve = vi.fn();
  }
  const original = (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = MockResizeObserver;
  try {
    const { unmount } = renderTable({ rows: rows(3), tableId: 'library' });
    const scroller = document.querySelector('[data-testid="frame-table-scroll"]') as HTMLElement;
    expect(scroller.style.height).toBe(`${window.innerHeight - 24}px`); // top 0 in jsdom

    const watched = instances.flatMap((i) => i.targets);
    expect(watched).toContain(scroller.parentElement);
    expect(watched).toContain(document.body);

    // The filter row wraps (the side panel opened) → the table box moves down 300 px.
    scroller.getBoundingClientRect = () => ({ top: 300, left: 0, right: 0, bottom: 0, width: 0, height: 0, x: 0, y: 300, toJSON: () => ({}) });
    act(() => { for (const i of instances) i.cb([], i as unknown as ResizeObserver); });
    expect(scroller.style.height).toBe(`${Math.max(360, window.innerHeight - 300 - 24)}px`);

    unmount();
    for (const i of instances) expect(i.disconnect).toHaveBeenCalled();
  } finally {
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = original;
  }
});
