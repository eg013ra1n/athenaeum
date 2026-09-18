// Settings redesign (spec 2026-09-18 §2) — Analysis tab. `AnalysisSettingsPanel`
// (Task D1) now renders its own five registered `SettingsSection`s
// (analysis.detection/measurement/psf/batch/rejection) directly — this tab
// only supplies the outer spacing, matching `GeneralTab.tsx`'s pattern.
import { AnalysisSettingsPanel } from '../../analysis/AnalysisSettingsPanel';

export function AnalysisTab() {
  return (
    <div className="space-y-6">
      <AnalysisSettingsPanel />
    </div>
  );
}
