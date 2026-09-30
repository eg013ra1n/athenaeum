import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { SessionStateProvider } from '../../../../contexts/SessionStateContext';
import type { OwnFrameRow } from '../../../../types/models';
import { fromOwn } from '../frames';
import type { FrameVM } from '../frames';
import ProjectFrameTable, { type ProjectFrameTableProps } from './ProjectFrameTable';

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
