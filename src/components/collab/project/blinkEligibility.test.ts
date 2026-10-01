import { describe, expect, it } from 'vitest';
import { blinkBadge, blinkRef, refKey } from './blinkEligibility';
import { fromLibrary, fromOwn } from './frames';
import { ownFrameRow as own, projectFrameView as lib } from './testFixtures';

const BY_ID = { frameId: 1, frameUuid: null };
const BY_UUID = { frameId: null, frameUuid: 'u1' };

describe('blinkRef', () => {
  it('ready: needs the original on this device', () => {
    expect(blinkRef(fromOwn(own()), 'ready')).toEqual(BY_ID);
    expect(blinkRef(fromOwn(own({ path: null })), 'ready')).toBeNull();
  });
  it('held: a Black Hole failure means the file is gone', () => {
    const f = own({ segment: 'held', failures: [{ kind: 'blackHole', text: 'in the Black Hole' }] });
    expect(blinkRef(fromOwn(f), 'held')).toBeNull();
  });
  it('review: the calibrated file, or the original for an attested set', () => {
    expect(blinkRef(fromOwn(own({ segment: 'review', calibratedPath: '/c/c_a.fits' })), 'review')).toEqual(BY_ID);
    expect(blinkRef(fromOwn(own({ segment: 'review', calibratedPath: null })), 'review')).toEqual(BY_ID);
  });
  it('published: only while the file is held (or changed) on disk', () => {
    const p = (localState: string) => fromOwn(own({ segment: 'published', frameUuid: 'u1', localState }));
    expect(blinkRef(p('own_held'), 'published')).toEqual(BY_ID);
    expect(blinkRef(p('own_missing'), 'published')).toBeNull();
    const changed = p('own_changed');
    expect(blinkRef(changed, 'published')).toEqual(BY_ID);
    expect(blinkBadge(changed)).toBe('changed on disk');
  });
  it('library: by uuid, only when held here', () => {
    expect(blinkRef(fromLibrary(lib({ localState: 'held' }), new Map()), 'library')).toEqual(BY_UUID);
    expect(blinkRef(fromLibrary(lib({ localState: 'wanted' }), new Map()), 'library')).toBeNull();
    expect(blinkRef(fromLibrary(lib({ localState: 'own_held', own: true }), new Map()), 'library')).toEqual(BY_UUID);
  });
});

describe('blinkBadge', () => {
  it('names a withheld own row and an excluded frame', () => {
    expect(blinkBadge(fromOwn(own({ segment: 'held', withheld: true })))).toBe('withheld');
    expect(blinkBadge(fromOwn(own({ segment: 'published', accepted: false })))).toBe('excluded');
    expect(blinkBadge(fromOwn(own()))).toBeUndefined();
  });
  it('a library row never reads "changed on disk": Library blinks only held / own_held replicas', () => {
    expect(blinkBadge(fromLibrary(lib({ localState: 'own_changed', own: true }), new Map()))).toBeUndefined();
  });
});

describe('refKey', () => {
  it("mirrors core's entry key", () => {
    expect(refKey({ frameId: 7, frameUuid: null })).toBe('f7');
    expect(refKey({ frameId: null, frameUuid: 'u1' })).toBe('u1');
  });
});
