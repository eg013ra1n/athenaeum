import type { ExportMode } from '../../types/export';
import type { ExportReadiness, GeometryMismatch } from '../../types/models';

const TYPE_WORD: Record<string, string> = { Dark: 'dark', Flat: 'flat', Bias: 'bias', DarkFlat: 'dark flat' };
const typeWord = (t: string): string => TYPE_WORD[t] ?? 'calibration';

/** Mirrors core `calibration::geometry::mismatch_parts`. */
function geometryMismatchParts(m: GeometryMismatch[]): string {
  const parts = m.slice(0, 3).map(x =>
    x.sourceSetId == null
      ? `${typeWord(x.calibrationType)} set #${x.calibrationSetId} is ${x.calibrationGeometry}, ${x.lightCount} light${x.lightCount === 1 ? '' : 's'} ${x.lightCount === 1 ? 'is' : 'are'} ${x.sourceGeometry}`
      : `flat set #${x.sourceSetId} is ${x.sourceGeometry}, its ${typeWord(x.calibrationType)} #${x.calibrationSetId} is ${x.calibrationGeometry}`,
  );
  const more = m.length > 3 ? `; +${m.length - 3} more` : '';
  return `${parts.join('; ')}${more}`;
}

/** Mirrors core `calibration::geometry::mismatch_sentence` verbatim. */
export function geometryMismatchSentence(m: GeometryMismatch[]): string {
  return `Calibration does not fit the frame size — ${geometryMismatchParts(m)}; re-link with Find Calibration`;
}

/** Mirrors core `calibration::geometry::origin_sentence` verbatim (G-7: an
 *  advisory line, never a blocker). */
export function geometryOriginSentence(m: GeometryMismatch[]): string {
  return `Calibration at another sub-frame origin — ${geometryMismatchParts(m)}`;
}

/** Why `mode` is not ready, or null. Mirrors core `check_mode_ready` verbatim —
 *  wording included: with nothing linked at all the two raw modes are blocked
 *  first (they would land exactly what Lights only lands, and the masters rule
 *  is vacuously true with no sets); `rawWithMasters` and the calibrated mode's
 *  masters-missing blocker read different sentences on the backend even though
 *  both gate on `rawSetsWithoutMaster`, and the calibrated mode's blocker ORDER
 *  is masters-missing → unlinked-lights → geometry → missing master FILES (a build is what
 *  can also change where a light's links resolve to; a missing FILE is a
 *  distinct failure from "not built yet"). The sets mode lands the raw
 *  originals behind every built master, so it is the one mode a raw original
 *  missing on disk (archived after the build) blocks. An attested set (calibrated
 *  by an external tool) passes the calibrated mode with no masters at all. */
export function modeBlocker(r: ExportReadiness, mode: ExportMode): string | null {
  const nothingLinked = r.total > 0 && r.unlinkedLights === r.total;
  if ((mode === 'rawWithCalibrationSets' || mode === 'rawWithMasters') && nothingLinked) {
    return 'No calibration is linked to this set — only the lights would land';
  }
  if (mode === 'rawWithCalibrationSets' && r.missingRawCalibrationFiles > 0) {
    const n = r.missingRawCalibrationFiles;
    return `${n} raw calibration file(s) missing on disk — restore from archive first`;
  }
  if (mode === 'rawWithMasters' && r.rawSetsWithoutMaster > 0) {
    const n = r.rawSetsWithoutMaster;
    return `${n} calibration set${n === 1 ? '' : 's'} ${n === 1 ? 'has' : 'have'} no master — build masters first`;
  }
  if (mode === 'calibratedLights') {
    // R3: an attested set's lights are copied as they are — no masters needed.
    if (r.attested) return null;
    if (r.rawSetsWithoutMaster > 0) {
      const n = r.rawSetsWithoutMaster;
      return `Build masters first — ${n} set${n === 1 ? '' : 's'} without a master`;
    }
    if (r.unlinkedLights > 0) {
      const n = r.unlinkedLights;
      return `${n} light${n === 1 ? '' : 's'} ${n === 1 ? 'has' : 'have'} no calibration links`;
    }
    if (r.geometryMismatches.length > 0) return geometryMismatchSentence(r.geometryMismatches);
    if (r.missingMasterFiles > 0) {
      const n = r.missingMasterFiles;
      return `${n} master file(s) missing on disk — restore from archive first`;
    }
  }
  return null;
}
