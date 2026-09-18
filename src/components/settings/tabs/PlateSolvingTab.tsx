// Settings redesign (spec 2026-09-18 §2) — Plate Solving tab.
// `PlateSolveSettingsPanel` spans three registry sections (catalog/solver/
// inputGate) and keeps its own Save button until Task D1 — see
// `AnalysisTab.tsx` for the same pattern.
import { RegisteredSections } from '../RegisteredSections';
import { PlateSolveSettingsPanel } from '../../plate-solve';

const PLATE_SOLVING_SECTION_IDS = [
  'plateSolving.catalog',
  'plateSolving.solver',
  'plateSolving.inputGate',
] as const;

export function PlateSolvingTab() {
  return (
    <RegisteredSections ids={PLATE_SOLVING_SECTION_IDS}>
      <div className="bg-surface-elevated rounded-lg p-6">
        <h3 className="text-xl font-semibold mb-4">Plate Solving Configuration</h3>
        <p className="text-content-muted mb-6">
          Configure the astrometric plate solver used to determine sky coordinates for frames
          that are missing RA/Dec metadata. The solver matches detected stars against the
          downloadable Gaia DR3 density-tier catalog to compute a full WCS solution.
        </p>
        <PlateSolveSettingsPanel />
      </div>
    </RegisteredSections>
  );
}
