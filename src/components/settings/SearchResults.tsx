// Settings redesign (spec 2026-09-18 §7, plan Task E1 Step 2): renders every
// section `useSettingsSearch(query)` matched, in registry order, each by its
// REAL element (via `tabs/index.ts::sectionComponent`) so editing a result
// commits through the exact same component as its own tab — never a copy.
// Each result gets a small tab-chip header above it (which tab it lives on)
// and `matchedFieldIds` rides a `SearchHighlightProvider` so the field
// components (and, for a hand-written panel, the section card itself) can
// ring-highlight what matched.
import type { ReactElement } from 'react';
import { SETTINGS_TABS, type SettingsSectionMeta } from '../../settings/registry';
import { useSettingsSearch } from '../../hooks/useSettingsSearch';
import { sectionComponent } from './tabs';
import { SearchHighlightProvider } from './SearchHighlightContext';

export interface SearchResultsProps {
  query: string;
}

const TAB_BY_ID = new Map(SETTINGS_TABS.map((t) => [t.id, t]));

interface ResultRow {
  section: SettingsSectionMeta;
  element: ReactElement;
}

/** Collapses `results` down to one row per DISTINCT underlying element — a
 *  multi-section panel (`AnalysisSettingsPanel`, `PlateSolveSettingsPanel`,
 *  `StackingSection`, `TransfersSection`) can have several of its ids match
 *  independently, and `sectionComponent` returns the SAME element object for
 *  all of them (see `tabSectionEntry.tsx`) — mounting it more than once
 *  would duplicate its whole tree (and its command traffic). The row keeps
 *  the FIRST matching section's id/tab for its header chip. */
function toRows(results: readonly SettingsSectionMeta[]): ResultRow[] {
  const seen = new Set<ReactElement>();
  const rows: ResultRow[] = [];
  for (const section of results) {
    const element = sectionComponent(section.id);
    if (seen.has(element)) continue;
    seen.add(element);
    rows.push({ section, element });
  }
  return rows;
}

export function SearchResults({ query }: SearchResultsProps) {
  const { results, matchedFieldIds } = useSettingsSearch(query);

  if (results.length === 0) {
    return <p className="text-sm text-content-muted py-8 text-center">No settings match.</p>;
  }

  const rows = toRows(results);

  return (
    <SearchHighlightProvider value={matchedFieldIds}>
      <div className="space-y-6">
        {rows.map(({ section, element }) => {
          const tab = TAB_BY_ID.get(section.tab);
          const Icon = tab?.icon;
          return (
            <div key={section.id}>
              <div className="flex items-center gap-1.5 mb-2 text-xs text-content-muted uppercase tracking-wide">
                {Icon && <Icon size={12} aria-hidden="true" />}
                <span>{tab?.label ?? section.tab}</span>
              </div>
              {element}
            </div>
          );
        })}
      </div>
    </SearchHighlightProvider>
  );
}
