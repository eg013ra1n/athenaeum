import { expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { ModerationFrameView, OwnFrameRow, ProjectFrameView } from '../../../types/models';
import { copies, fromLibrary, fromModeration, fromOwn, GROUPS, TABLES } from './frames';

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
