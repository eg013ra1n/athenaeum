// Settings redesign (spec 2026-09-18 §2/§7, plan Task E1 Step 2): the shape
// every tab file exports its section list as. `tabs/index.ts` concatenates
// all seven lists into one lookup so `sectionComponent(id)` (used by
// `SearchResults`) is DERIVED from the same data a tab renders from, never a
// second, hand-maintained mapping.
//
// Most registry section ids are rendered by their own dedicated component
// (one id, one element) — but a few hand-written panels
// (`AnalysisSettingsPanel`, `PlateSolveSettingsPanel`, `StackingSection`)
// each mount several registry ids' worth of real `SettingsSection`s from
// ONE component instance. Such a tab lists the SAME `element` object against
// every id it covers (a single `<AnalysisSettingsPanel />` created once at
// module scope and reused across its five entries) rather than creating a
// fresh element per id — `renderTabSections` below relies on that reference
// equality to mount the panel exactly once even though it appears several
// times in the list.
import { Fragment, type ReactElement, type ReactNode } from 'react';

export interface TabSectionEntry {
  /** A registry section id (`SETTINGS_SECTIONS[].id`). */
  sectionId: string;
  /** The real element that renders this section — the SAME object for
   *  every id a multi-section panel covers (see file comment). */
  element: ReactElement;
}

/** Renders a tab's ordered section entries for its normal (non-search) tab
 *  body, mounting each distinct `element` exactly once even when several
 *  entries share it (a multi-section panel) — mounting the same panel N
 *  times would duplicate its whole tree (and its command traffic) N times. */
export function renderTabSections(entries: readonly TabSectionEntry[]): ReactNode {
  const seen = new Set<ReactElement>();
  const out: ReactNode[] = [];
  for (const entry of entries) {
    if (seen.has(entry.element)) continue;
    seen.add(entry.element);
    out.push(<Fragment key={entry.sectionId}>{entry.element}</Fragment>);
  }
  return out;
}
