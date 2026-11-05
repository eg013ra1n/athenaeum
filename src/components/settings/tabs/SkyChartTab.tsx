// Settings — Sky Chart tab: Sky background (the survey switch, the tile cache,
// the surveys and their offline packs). Moved out of General, which had grown
// too long.
import { SkyBackgroundSection } from '../sections/SkyBackgroundSection';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

export const SKY_CHART_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'skyChart.background', element: <SkyBackgroundSection /> },
];

export function SkyChartTab() {
  return <div className="space-y-6">{renderTabSections(SKY_CHART_SECTIONS)}</div>;
}
