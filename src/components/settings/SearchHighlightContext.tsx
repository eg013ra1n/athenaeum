// Settings redesign (spec 2026-09-18 §7, plan Task E1 Step 2): while
// `SearchResults` renders a matching section outside its normal tab,
// `useSettingsSearch(query).matchedFieldIds` (`${section.id}/${field.id}`
// strings) is threaded down through this context so the KV field
// components (`SettingToggle`/`SettingSelect`/`SettingNumber`/`SettingText`)
// can ring-highlight their own label row, and `SettingsSection` can
// highlight the whole card for a hand-written panel that doesn't use those
// components (spec: "for hand-written panels highlight at section level
// only"). Outside `SearchResults` (the normal tab view) there is no
// provider — every reader falls back to the shared empty set, so nothing
// highlights and no component needs to guard against a missing provider.
import { createContext, useContext } from 'react';

const EMPTY: ReadonlySet<string> = new Set();

const SearchHighlightContext = createContext<ReadonlySet<string>>(EMPTY);

export const SearchHighlightProvider = SearchHighlightContext.Provider;

/** The current set of matched `${sectionId}/${fieldId}` strings — empty
 *  outside `SearchResults`. */
export function useSearchHighlight(): ReadonlySet<string> {
  return useContext(SearchHighlightContext);
}

/** Whether one field's own label row should ring-highlight. */
export function isFieldHighlighted(matched: ReadonlySet<string>, section: string, field: string): boolean {
  return matched.has(`${section}/${field}`);
}

/** Whether ANY field registered under `sectionId` matched — drives the
 *  section-level (whole-card) highlight `SettingsSection` uses, which is
 *  the only highlight a hand-written panel gets. */
export function isSectionHighlighted(matched: ReadonlySet<string>, sectionId: string): boolean {
  if (matched.size === 0) return false;
  const prefix = `${sectionId}/`;
  for (const id of matched) {
    if (id.startsWith(prefix)) return true;
  }
  return false;
}
