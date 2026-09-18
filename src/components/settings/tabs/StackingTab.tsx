// Settings redesign (spec 2026-09-18 §2) — Stacking tab: Pipeline defaults
// (stage list + inspector) · Default folders. `StackingSection` keeps its
// own Reset button/load-dirty discipline until Task D2 moves it onto the
// shared `useAutosaveDocument` hook — see `AnalysisTab.tsx` for the same
// registration pattern. Per-set overrides live on each frame set's own
// Stacking tab (`src/components/stacking/StackingTab.tsx`), unrelated to
// this file.
import { SquareStack } from 'lucide-react';
import { RegisteredSections } from '../RegisteredSections';
import StackingSection from '../StackingSection';

export function StackingTab() {
  return (
    <RegisteredSections ids={['stacking.pipeline', 'stacking.folders']}>
      <div className="bg-surface-elevated rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
          <SquareStack size={20} />
          Stacking
        </h3>
        <p className="text-xs text-content-muted mb-4">
          Pipeline defaults and the two default folders every frame set uses unless it sets its own.
        </p>
        <StackingSection />
      </div>
    </RegisteredSections>
  );
}
