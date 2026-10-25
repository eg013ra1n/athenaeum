import { describe, expect, it } from 'vitest';
import type { ExportReadiness, GeometryMismatch } from '../../types/models';
import { geometryMismatchSentence, geometryOriginSentence, modeBlocker } from './modeBlocker';

const ready: ExportReadiness = {
  total: 4,
  unlinkedLights: 0,
  rawSetsWithoutMaster: 0,
  rawSetIdsWithoutMaster: [],
  missingMasterFiles: 0,
  missingRawCalibrationFiles: 0,
  fileCounts: { lightsOnly: 4, rawWithCalibrationSets: 4, rawWithMasters: 4, calibratedLights: 4 },
  rawSetsBuildable: [],
  rawSetsUnbuildable: [],
  mastersRebuildable: [],
  mastersUnrebuildable: [],
  attested: false,
  calibrationWarnings: [],
  geometryMismatches: [],
  geometryWarnings: [],
};

describe('modeBlocker', () => {
  it('an attested set passes calibratedLights without masters', () => {
    const r = { ...ready, rawSetsWithoutMaster: 1, rawSetIdsWithoutMaster: [7], unlinkedLights: 2, missingMasterFiles: 1, attested: true };
    expect(modeBlocker(r, 'calibratedLights')).toBeNull();
  });

  it('a set that is not attested still needs its masters first', () => {
    const r = { ...ready, rawSetsWithoutMaster: 1, rawSetIdsWithoutMaster: [7] };
    expect(modeBlocker(r, 'calibratedLights')).toBe('Build masters first — 1 set without a master');
  });

  it('attestation does not open rawWithMasters', () => {
    const r = { ...ready, rawSetsWithoutMaster: 1, rawSetIdsWithoutMaster: [7], attested: true };
    expect(modeBlocker(r, 'rawWithMasters')).toContain('build masters first');
  });
});

const mm = (calibrationType: string, calibrationSetId: number, calibrationGeometry: string,
  sourceSetId: number | null, sourceGeometry: string, lightCount: number): GeometryMismatch =>
  ({ calibrationType, calibrationSetId, calibrationGeometry, sourceSetId, sourceGeometry, lightCount });

describe('geometry blocker', () => {
  it('mirrors the core sentence verbatim', () => {
    // Same vector as core `calibration::geometry::tests::mismatch_sentence_vectors`.
    expect(geometryMismatchSentence([
      mm('Dark', 1843, '6384×4258', null, '9576×6388', 151),
      mm('Dark', 1267, '6384×4258 @ 1584,1064', 558, '9576×6388', 0),
    ])).toBe('Calibration does not fit the frame size — dark set #1843 is 6384×4258, 151 lights are 9576×6388; flat set #558 is 9576×6388, its dark #1267 is 6384×4258 @ 1584,1064; re-link with Find Calibration');
    expect(geometryMismatchSentence([
      mm('Flat', 1, 'mixed sizes', null, '9576×6388', 1),
      mm('Bias', 2, '6384×4258', null, '9576×6388', 2),
      mm('DarkFlat', 3, '6384×4258', 9, '9576×6388', 0),
      mm('Dark', 4, '6384×4258', null, '9576×6388', 3),
    ])).toBe('Calibration does not fit the frame size — flat set #1 is mixed sizes, 1 light is 9576×6388; bias set #2 is 6384×4258, 2 lights are 9576×6388; flat set #9 is 9576×6388, its dark flat #3 is 6384×4258; +1 more; re-link with Find Calibration');
  });
  it('blocks only the calibrated mode, after unlinked lights and before missing files', () => {
    const geo = [mm('Dark', 1843, '6384×4258', null, '9576×6388', 1)];
    expect(modeBlocker({ ...ready, geometryMismatches: geo }, 'calibratedLights')).toContain('does not fit the frame size');
    expect(modeBlocker({ ...ready, geometryMismatches: geo, unlinkedLights: 1 }, 'calibratedLights')).toBe('1 light has no calibration links');
    expect(modeBlocker({ ...ready, geometryMismatches: geo, missingMasterFiles: 1 }, 'calibratedLights')).toContain('does not fit the frame size');
    expect(modeBlocker({ ...ready, geometryMismatches: geo }, 'rawWithMasters')).toBeNull();
    expect(modeBlocker({ ...ready, geometryMismatches: geo, attested: true }, 'calibratedLights')).toBeNull();
  });
  it('another sub-frame origin is a warning sentence, never a blocker (G-7)', () => {
    // Same vector as core `calibration::geometry::tests::origin_sentence_vectors`.
    expect(geometryOriginSentence([
      mm('Dark', 1518, '6384×4258 @ 0,0', null, '6384×4258 @ 1584,1064', 151),
      mm('Bias', 7, '6384×4258 @ 0,0', 558, '6384×4258 @ 1584,1064', 0),
    ])).toBe('Calibration at another sub-frame origin — dark set #1518 is 6384×4258 @ 0,0, 151 lights are 6384×4258 @ 1584,1064; flat set #558 is 6384×4258 @ 1584,1064, its bias #7 is 6384×4258 @ 0,0');
    const warn = [mm('Dark', 1518, '6384×4258 @ 0,0', null, '6384×4258 @ 1584,1064', 1)];
    expect(modeBlocker({ ...ready, geometryWarnings: warn }, 'calibratedLights')).toBeNull();
  });
});
