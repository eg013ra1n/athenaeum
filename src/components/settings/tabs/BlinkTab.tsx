// Settings redesign (spec 2026-09-18 §2) — Blink tab: Blink viewer · Flat
// contour plot · Star annotation display.
import { BlinkViewerSection } from '../sections/BlinkViewerSection';
import { FlatContourSection } from '../sections/FlatContourSection';
import { StarAnnotationSection } from '../sections/StarAnnotationSection';

export function BlinkTab() {
  return (
    <div className="space-y-6">
      <BlinkViewerSection />
      <FlatContourSection />
      <StarAnnotationSection />
    </div>
  );
}
