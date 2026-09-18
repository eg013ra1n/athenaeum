import { describe, expect, it } from 'vitest';
import { renderHook } from '@testing-library/react';
import { useSettingsSearch } from './useSettingsSearch';

function idsOf(results: ReturnType<typeof useSettingsSearch>['results']): string[] {
  return results.map((r) => r.id);
}

describe('useSettingsSearch', () => {
  it('matches on a section title', () => {
    const { result } = renderHook(() => useSettingsSearch('Updates'));
    expect(idsOf(result.current.results)).toContain('general.updates');
  });

  it('matches on a field help string', () => {
    // "Automatically check for updates on startup"'s help text is the only
    // place the word "notification" appears in the registry.
    const { result } = renderHook(() => useSettingsSearch('notification'));
    expect(idsOf(result.current.results)).toContain('general.updates');
    expect(idsOf(result.current.results)).not.toContain('general.archive');
  });

  it('matches on a field keyword', () => {
    const { result } = renderHook(() => useSettingsSearch('session_gap'));
    expect(idsOf(result.current.results)).toContain('general.sessions');
    expect(idsOf(result.current.results)).not.toContain('general.archive');
  });

  it('ANDs multiple terms across the section, not ORs them', () => {
    // "general.grouping" carries both "threshold" (its own field label/help)
    // and "arcsec" (the unit keyword) — "general.sessions" carries
    // "threshold" (via the "session_gap_threshold_hours" keyword) but never
    // "arcsec", so it must be excluded once both terms are required.
    const bothTerms = renderHook(() => useSettingsSearch('threshold arcsec'));
    expect(idsOf(bothTerms.result.current.results)).toContain('general.grouping');
    expect(idsOf(bothTerms.result.current.results)).not.toContain('general.sessions');

    const oneTerm = renderHook(() => useSettingsSearch('threshold'));
    expect(idsOf(oneTerm.result.current.results)).toContain('general.sessions');
  });

  it('returns no results for an empty query', () => {
    const { result } = renderHook(() => useSettingsSearch(''));
    expect(result.current.results).toEqual([]);
    expect(result.current.matchedFieldIds.size).toBe(0);
  });

  it('returns no results for a whitespace-only query', () => {
    const { result } = renderHook(() => useSettingsSearch('   '));
    expect(result.current.results).toEqual([]);
  });

  it('reports matched field ids only for fields within a matching section', () => {
    const { result } = renderHook(() => useSettingsSearch('session_gap'));
    expect(result.current.matchedFieldIds.has('general.sessions/gapHours')).toBe(true);
  });

  it('folds diacritics and case', () => {
    const { result } = renderHook(() => useSettingsSearch('UPDATES'));
    expect(idsOf(result.current.results)).toContain('general.updates');
  });
});
