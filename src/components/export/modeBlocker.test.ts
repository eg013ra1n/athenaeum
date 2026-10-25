import { describe, expect, it } from 'vitest';
import type { ExportReadiness } from '../../types/models';
import { modeBlocker } from './modeBlocker';

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
