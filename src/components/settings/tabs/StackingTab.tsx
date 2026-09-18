// Settings redesign (spec 2026-09-18 §2) — Stacking tab: Pipeline defaults
// (stage list + inspector) · Default folders. `StackingSection` now renders
// its own two registered sections (`stacking.pipeline`, `stacking.folders`)
// directly — Task D2 moved it onto the shared `useAutosaveDocument` hook, so
// this tab is just the card list, same registration pattern as
// `GeneralTab.tsx`/`BlinkTab.tsx`. Per-set overrides live on each frame
// set's own Stacking tab (`src/components/stacking/StackingTab.tsx`),
// unrelated to this file.
import StackingSection from '../StackingSection';

export function StackingTab() {
  return (
    <div className="space-y-6">
      <StackingSection />
    </div>
  );
}
