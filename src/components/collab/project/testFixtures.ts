import type { OwnFrameRow, ProjectCard, ProjectFrameView } from '../../../types/models';

/** A full `ProjectCard` for tests; override what the case needs. */
export function projectCard(overrides: Partial<ProjectCard> = {}): ProjectCard {
  return {
    projectId: 'proj-1',
    slug: 'm42-mosaic',
    title: 'M42 Mosaic',
    dataRole: 'send_receive',
    coordinator: false,
    canModerate: false,
    requireApproval: false,
    pendingFrames: 0,
    projectStatus: 'open',
    targetName: 'M42',
    targetRaDeg: 83.8,
    targetDecDeg: -5.4,
    targetRadiusDeg: 1.5,
    membershipVersion: 1,
    linkedSets: 1,
    candidates: 2,
    publishable: 2,
    autoReplicate: true,
    publishMode: 'manual',
    syncedAt: null,
    fetchedAt: '2026-09-24T00:00:00Z',
    publishingDevice: null,
    publishingHere: false,
    ...overrides,
  };
}

/** A full own-frame row (`list_project_own_frames`) for tests: a Ready frame on disk. */
export function ownFrameRow(o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return {
    frameId: 1, frameUuid: null, fileName: 'f.fits', setId: null, setName: null, night: '2026-09-29',
    filter: 'Ha', filterMapped: true, camera: 'ASI2600MM Pro', exptimeSec: 300, byteSize: 42_000_000,
    fwhmArcsec: 2.4, eccentricity: 0.4, starsDetected: 1200, medianSnr: 18, segment: 'ready',
    contributorState: 'published', contributorReason: null, failures: [], contentVersion: null,
    pubState: null, acceptedReason: null, holdersOnline: null, holdersTotal: null, localState: null,
    publishedAt: null, lastError: null, rules: [], path: '/r/a.fits', accepted: null,
    calibratedPath: null, calibratedBytes: null, preparedAt: null, withheld: false, ...o,
  };
}

/** A full library row (`list_collab_frames`) for tests: another member's frame held here. */
export function projectFrameView(o: Partial<ProjectFrameView> = {}): ProjectFrameView {
  return {
    frameUuid: 'u1', fileName: 'l.fits', publisher: 'Anna', publisherAccountId: 'acc', own: false,
    filter: 'Ha', exptimeSec: 300, dateObs: null, state: 'published', accepted: true, acceptedReason: null,
    localState: 'held', onDisk: true, holdersOnline: 1, holdersTotal: 1, waitingForPublisher: false,
    newVersionWaiting: false, byteSize: 1024, contentVersion: 1, lastError: null, fwhmArcsec: null,
    eccentricity: null, starsDetected: null, camera: null, telescope: null, night: null, medianSnr: null,
    contributorState: null, contributorReason: null, receivedAt: null, receivedFromDevice: null,
    receivedFromMember: null, ...o,
  };
}
