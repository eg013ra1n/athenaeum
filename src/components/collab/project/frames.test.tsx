import { expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { ModerationFrameView, OwnFrameRow, ProjectFrameView } from '../../../types/models';
import { COLUMNS, copies, fromLibrary, fromModeration, fromOwn, GROUPS, HELD_KIND_ORDER, REASON_LABEL, TABLES } from './frames';

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

it('fromLibrary: my own frames read their on-disk state (Library lists every project frame)', () => {
  expect(fromLibrary(lib({ own: true, localState: 'own_held' }), new Map()).device).toBe('have');
  const gone = fromLibrary(lib({ own: true, localState: 'own_missing' }), new Map());
  expect([gone.device, gone.missingWhy]).toEqual(['missing', 'gone from disk']);
  expect(fromLibrary(lib({ own: true, localState: 'own_changed' }), new Map()).device).toBe('changed');
});

it('fromLibrary: wanted + in flight is downloading with a clamped percent', () => {
  const vm = fromLibrary(lib({ frameUuid: 'u1', localState: 'wanted' }), new Map([['u1', { done: 150, size: 100 }]]));
  expect([vm.device, vm.progress]).toEqual(['downloading', 100]);
});

it('fromLibrary: wanted with the publisher offline is missing, with that reason', () => {
  const vm = fromLibrary(lib({ localState: 'wanted', waitingForPublisher: true, holdersOnline: 0 }), new Map());
  expect([vm.device, vm.missingWhy]).toEqual(['missing', 'publisher offline']);
});

it('fromLibrary: wanted with no online holder is missing (holder offline); with one it is queued', () => {
  expect(fromLibrary(lib({ localState: 'wanted', holdersOnline: 0 }), new Map()).missingWhy).toBe('holder offline');
  expect(fromLibrary(lib({ localState: 'wanted', holdersOnline: 2 }), new Map()).device).toBe('queued');
});

it('fromLibrary: localState "missing" (gone from disk) reads that reason even with holdersOnline: 0 — never borrows the wanted route\'s reasons', () => {
  const vm = fromLibrary(lib({ localState: 'missing', holdersOnline: 0, waitingForPublisher: false }), new Map());
  expect([vm.device, vm.missingWhy]).toEqual(['missing', 'gone from disk']);
});

it('fromLibrary: a zero-size in-flight item never divides into NaN — progress is 0', () => {
  const vm = fromLibrary(
    lib({ frameUuid: 'u1', localState: 'wanted' }),
    new Map([['u1', { done: 0, size: 0 }]]),
  );
  expect([vm.device, vm.progress]).toEqual(['downloading', 0]);
});

it('copies counts this device when it holds the frame', () => {
  expect(copies(fromOwn(own({ segment: 'published', localState: 'own_held', holdersTotal: 0 })))).toBe(1);
  expect(copies(fromLibrary(lib({ localState: 'held', holdersTotal: 1 }), new Map()))).toBe(2);
});

it('a published own frame with one copy and a missing file matches both extra states', () => {
  const vm = fromOwn(own({ segment: 'published', pubState: 'published', localState: 'own_missing', holdersTotal: 1 }));
  expect(vm.states.sort()).toEqual(['disk', 'published', 'single']);
});

it('a held frame matches each of its failure kinds; the reason group is the first', () => {
  const vm = fromOwn(own({ segment: 'held', failures: [{ kind: 'solve', text: 'no coordinates' }, { kind: 'threshold', text: 'FWHM 3.42″ > 3.00″' }] }));
  expect(vm.states).toEqual(['solve', 'threshold']);
  expect(GROUPS.reason.key(vm)).toBe('solve');
});

it('unknown camera and night group under readable labels', () => {
  const vm = fromOwn(own({ camera: '', night: null }));
  render(<>{GROUPS.camera.renderLabel(GROUPS.camera.key(vm))}{GROUPS.night.renderLabel(GROUPS.night.key(vm))}</>);
  expect(screen.getByText('Unknown camera')).toBeInTheDocument();
  expect(screen.getByText('Unknown night')).toBeInTheDocument();
});

it('fromModeration fills manifest-only metrics when the mirror has the frame', () => {
  const m: ModerationFrameView = { frameUuid: 'u9', fileName: 'a.fits', publisher: 'Olga', publisherAccountId: 'acc-o', filter: 'Ha', exptimeSec: 300, fwhmArcsec: 2.1, createdAt: '2026-09-30T08:00:00Z' };
  const vm = fromModeration(m, new Map([['u9', lib({ frameUuid: 'u9', night: '2026-09-29', camera: 'QHY268M', eccentricity: 0.4 })]]));
  expect([vm.night, vm.camera, vm.ecc, vm.submittedAt]).toEqual(['2026-09-29', 'QHY268M', 0.4, '2026-09-30T08:00:00Z']);
});

it('fromModeration has no manifest mirror falls back to null/empty', () => {
  const m: ModerationFrameView = { frameUuid: 'u10', fileName: 'b.fits', publisher: 'Olga', publisherAccountId: 'acc-o', filter: 'Ha', exptimeSec: 300, fwhmArcsec: null, createdAt: '2026-09-30T08:00:00Z' };
  const vm = fromModeration(m, new Map());
  expect([vm.night, vm.camera, vm.ecc, vm.stars, vm.snr, vm.byteSize]).toEqual([null, '', null, null, null, null]);
});

it('hasProjectRow: an own frame has one only once published; a library frame always; a moderation row only with a mirror', () => {
  expect(fromOwn(own({ segment: 'ready', frameUuid: 'cat-uuid', pubState: null })).hasProjectRow).toBe(false);
  expect(fromOwn(own({ segment: 'published', pubState: 'published' })).hasProjectRow).toBe(true);
  expect(fromLibrary(lib(), new Map()).hasProjectRow).toBe(true);
  const m: ModerationFrameView = { frameUuid: 'u11', fileName: 'c.fits', publisher: 'Olga', publisherAccountId: 'acc-o', filter: 'Ha', exptimeSec: 300, fwhmArcsec: null, createdAt: '2026-09-30T08:00:00Z' };
  expect(fromModeration(m, new Map()).hasProjectRow).toBe(false);
  expect(fromModeration(m, new Map([['u11', lib({ frameUuid: 'u11' })]])).hasProjectRow).toBe(true);
});

it('an excluded own frame reads as excluded in Status and in the state facet', () => {
  const vm = fromOwn(own({ segment: 'published', pubState: 'published', accepted: false, acceptedReason: 'wrong target', localState: 'own_held', holdersTotal: 2 }));
  expect(vm.excluded).toBe(true);
  expect(vm.states).toContain('excluded');
  expect(GROUPS.status.key(vm)).toBe('excluded');
});

it('an excluded library frame reads accepted === false as excluded, moderation never does', () => {
  expect(fromLibrary(lib({ accepted: false }), new Map()).excluded).toBe(true);
  expect(fromLibrary(lib({ accepted: true }), new Map()).excluded).toBe(false);
  const m: ModerationFrameView = { frameUuid: 'u11', fileName: 'c.fits', publisher: 'Olga', publisherAccountId: 'acc-o', filter: 'Ha', exptimeSec: 300, fwhmArcsec: null, createdAt: '2026-09-30T08:00:00Z' };
  expect(fromModeration(m, new Map()).excluded).toBe(false);
});

it('a ready own frame has no states', () => {
  expect(fromOwn(own({ segment: 'ready' })).states).toEqual([]);
});

it('no table offers a ZP column', () => {
  for (const t of Object.values(TABLES)) expect(t.columns).not.toContain('zp');
});

it('TABLES.excluded is the Moderation tab\'s excluded-frames layout', () => {
  expect(TABLES.excluded).toEqual({
    id: 'excluded',
    columns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'exclusion', 'fwhm', 'ecc', 'size'],
    defaultColumns: ['name', 'publisher', 'night', 'filter', 'exp', 'exclusion'],
    groupings: ['publisher', 'night', 'filter', 'camera', 'none'],
    defaultGrouping: ['publisher', 'none'],
    stateFacet: null,
    publisherFacet: true,
  });
});

it('COLUMNS.exclusion reads acceptedReason, dashing a null reason', () => {
  const withReason = fromLibrary(lib({ accepted: false, acceptedReason: 'wrong target' }), new Map());
  const withoutReason = fromLibrary(lib({ accepted: false, acceptedReason: null }), new Map());
  expect(COLUMNS.exclusion.value(withReason)).toBe('wrong target');
  expect(COLUMNS.exclusion.value(withoutReason)).toBeNull();

  render(
    <>
      <span data-testid="with-reason">{COLUMNS.exclusion.cell(withReason)}</span>
      <span data-testid="without-reason">{COLUMNS.exclusion.cell(withoutReason)}</span>
    </>,
  );
  expect(screen.getByTestId('with-reason')).toHaveTextContent('wrong target');
  expect(screen.getByTestId('without-reason')).toHaveTextContent('—');
});

/* ── Wave 5.5 Task 6: the mockup's cell formats ────────────────────────── */

it('formats cells like the mockup: "180 s", decimal sizes, padded group durations', () => {
  const vm = fromLibrary(lib({ exptimeSec: 180, byteSize: 121_920_698 }), new Map());
  render(<>{COLUMNS.exp.cell(vm)}|{COLUMNS.size.cell(vm)}|{COLUMNS.exp.renderAggregate!([vm, vm, vm])}</>);
  expect(screen.getByText(/180 s/)).toBeInTheDocument();
  expect(screen.getByText(/122 MB/)).toBeInTheDocument();
  expect(screen.getByText(/9m/)).toBeInTheDocument();
});

it('status and disk cells use Chip tones', () => {
  const vm = fromOwn(own({ segment: 'published', pubState: 'pending', localState: 'own_missing', holdersTotal: 1 }));
  render(<>{COLUMNS.status.cell(vm)}{COLUMNS.disk.cell(vm)}</>);
  expect(screen.getByText('pending').className).toContain('bg-warning-muted');
  expect(screen.getByText('missing').className).toContain('bg-error-muted');
});

/* ── Task 6 fix round 1: the mockup's pixels ───────────────────────────── */

it('device cells: the progress bar and the group bar fill the cell, % is cell-sized, "not kept" is ghost', () => {
  const dl = fromLibrary(lib({ frameUuid: 'u1', localState: 'wanted' }), new Map([['u1', { done: 50, size: 100 }]]));
  const nk = fromLibrary(lib({ frameUuid: 'u2', localState: 'not_kept' }), new Map());
  render(
    <>
      <span data-testid="dl">{COLUMNS.device.cell(dl)}</span>
      <span data-testid="nk">{COLUMNS.device.cell(nk)}</span>
      <span data-testid="agg">{COLUMNS.device.renderAggregate!([dl, nk])}</span>
    </>,
  );
  const pbar = screen.getByTestId('dl').querySelector('i')!.parentElement as HTMLElement; // the .pbar track
  expect(pbar.className).toContain('flex-1');
  expect(pbar.className).not.toMatch(/\bw-\[/);
  expect(screen.getByText('50%').className).not.toContain('text-[11px]');
  expect(screen.getByText('not kept').className).toContain('text-content-ghost');
  const bar = screen.getByTestId('agg').querySelector('i')!.parentElement as HTMLElement; // the .bar track
  expect(bar.className).not.toContain('max-w-');
  expect(bar.className).toContain('flex-1');
});

it('held-back "+N" uses the faint group-count style; an empty publisher group reads "Unknown publisher"', () => {
  const vm = fromOwn(own({ segment: 'held', failures: [{ kind: 'solve', text: 'no coordinates' }, { kind: 'analyze', text: 'no analysis' }] }));
  render(<>{COLUMNS.reason.cell(vm)}{GROUPS.publisher.renderLabel('')}</>);
  expect(screen.getByText('+1').className).toContain('text-content-faint');
  expect(screen.getByText('+1').className).toContain('text-[11px]');
  expect(screen.getByText('Unknown publisher')).toBeInTheDocument();
});

it('a review row has no states and a withheld held row carries the withheld kind', () => {
  const review = fromOwn(own({ segment: 'review', calibratedPath: '/c/c_a.fits', calibratedBytes: 64 }));
  expect(review.states).toEqual([]);
  const w = fromOwn(own({ segment: 'held', withheld: true, failures: [{ kind: 'withheld', text: 'Withheld by you' }] }));
  expect(w.states).toEqual(['withheld']);
  expect(REASON_LABEL.withheld).toBe('Withheld by you');
  expect(REASON_LABEL.blackHole).toBe('In the Black Hole');
  expect(TABLES.held.stateFacet?.options.map(([k]) => k)).toEqual([...HELD_KIND_ORDER]);
  expect(TABLES.review.stateFacet).toBeNull();
});
