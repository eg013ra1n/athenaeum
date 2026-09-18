// Settings redesign (spec 2026-09-18 §2) — Analysis tab. `AnalysisSettingsPanel`
// spans five registry sections (analysis.detection/measurement/psf/batch/
// rejection) and keeps its own Save button + internal layout until Task D1
// rebuilds it on `useAutosaveDocument` — this tab only supplies the card
// chrome and registers the five ids so search/`?section=` can already find
// them.
import { RegisteredSections } from '../RegisteredSections';
import { AnalysisSettingsPanel } from '../../analysis/AnalysisSettingsPanel';

const ANALYSIS_SECTION_IDS = [
  'analysis.detection',
  'analysis.measurement',
  'analysis.psf',
  'analysis.batch',
  'analysis.rejection',
] as const;

export function AnalysisTab() {
  return (
    <RegisteredSections ids={ANALYSIS_SECTION_IDS}>
      <div className="bg-surface-elevated rounded-lg p-6">
        <h3 className="text-xl font-semibold mb-4">Star Analysis Configuration</h3>
        <p className="text-content-muted mb-6">
          Configure star detection, PSF fitting and batch processing for the Lights Analysis tab, plus the
          default rejection thresholds its threshold bar starts from.
          Changes here affect new analyses — existing results keep their original settings until re-analyzed.
        </p>
        <AnalysisSettingsPanel />
      </div>
    </RegisteredSections>
  );
}
