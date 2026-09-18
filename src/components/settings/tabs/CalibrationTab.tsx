// Settings redesign (spec 2026-09-18 §2) — Calibration tab: Matching (the
// six collapsible groups as today) · Master build memory · Master file
// format. `CalibrationMatchingConfig` keeps its own Save button until Task
// D3 — see `AnalysisTab.tsx` for the same registration pattern.
import { RegisteredSections } from '../RegisteredSections';
import { CalibrationMatchingConfig } from '../../calibration';
import { MasterBuildMemorySection } from '../sections/MasterBuildMemorySection';
import { MasterFileFormatSection } from '../sections/MasterFileFormatSection';

export function CalibrationTab() {
  return (
    <div className="space-y-6">
      <RegisteredSections ids={['calibration.matching']}>
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-xl font-semibold mb-4">Calibration Matching Configuration</h3>
          <p className="text-content-muted mb-6">
            Configure how calibration frames (Flats, Darks, Bias) are matched to source frames.
            Define which parameters must match exactly, warn on threshold, or be ignored.
          </p>
          <CalibrationMatchingConfig />
        </div>
      </RegisteredSections>

      <MasterBuildMemorySection />
      <MasterFileFormatSection />
    </div>
  );
}
