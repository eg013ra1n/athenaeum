// Settings redesign (spec 2026-09-18 §7): search across every tab. The
// index is built once from the registry (`SETTINGS_SECTIONS`) — title,
// description, tab label, every field's label/help/keywords — normalized
// (lower-cased, diacritics folded) so it never needs to be rebuilt per
// keystroke. A query is AND-ed over whitespace-separated terms against each
// SECTION's combined haystack; `matchedFieldIds` additionally names which
// individual fields (within a matching section) contain any one term, for
// `SearchResults`/`SettingsSection` to highlight (Task E1).
//
// A section is searchable only if it is in the registry (spec §7) — this
// file reads nothing else.

import { useMemo } from 'react';
import { SETTINGS_SECTIONS, SETTINGS_TABS, type SettingsSectionMeta } from '../settings/registry';

export interface UseSettingsSearchResult {
  /** Matching sections, in registry order. */
  results: SettingsSectionMeta[];
  /** `${section.id}/${field.id}` for every field whose own text matched at
   *  least one query term, restricted to fields of a section in `results`. */
  matchedFieldIds: Set<string>;
}

/** Lower-case, diacritics-folded (spec §7's exact recipe). */
function normalize(text: string): string {
  return text
    .toLowerCase()
    .normalize('NFD')
    .replace(/\p{M}/gu, '');
}

interface IndexedField {
  id: string;
  text: string;
}

interface IndexedSection {
  section: SettingsSectionMeta;
  haystack: string;
  fields: IndexedField[];
}

const TAB_LABEL: Record<string, string> = Object.fromEntries(SETTINGS_TABS.map((t) => [t.id, t.label]));

function buildIndex(): IndexedSection[] {
  return SETTINGS_SECTIONS.map((section) => {
    const fields: IndexedField[] = section.fields.map((field) => ({
      id: `${section.id}/${field.id}`,
      text: normalize([field.label, field.help, ...(field.keywords ?? [])].filter(Boolean).join(' ')),
    }));
    const haystack = normalize(
      [section.title, section.description, TAB_LABEL[section.tab], ...fields.map((f) => f.text)]
        .filter(Boolean)
        .join(' '),
    );
    return { section, haystack, fields };
  });
}

export function useSettingsSearch(query: string): UseSettingsSearchResult {
  // Built once — the registry is static for the lifetime of the page.
  const index = useMemo(() => buildIndex(), []);

  return useMemo(() => {
    const terms = normalize(query)
      .split(/\s+/)
      .filter(Boolean);

    if (terms.length === 0) {
      return { results: [], matchedFieldIds: new Set<string>() };
    }

    const results: SettingsSectionMeta[] = [];
    const matchedFieldIds = new Set<string>();

    for (const entry of index) {
      if (!terms.every((term) => entry.haystack.includes(term))) continue;
      results.push(entry.section);
      for (const field of entry.fields) {
        if (terms.some((term) => field.text.includes(term))) {
          matchedFieldIds.add(field.id);
        }
      }
    }

    return { results, matchedFieldIds };
  }, [index, query]);
}
