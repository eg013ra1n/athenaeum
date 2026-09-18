// Settings redesign (spec 2026-09-18): the page shell only — tab bar,
// search field and the tab switch. Every setting lives in a registered
// `SettingsSection` inside one of the seven tab files
// (`src/components/settings/tabs/`); see `docs/settings/README.md` (Task F1)
// for how a new one is added.
import { useEffect, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { HistoryNav } from '../components/HistoryNav';
import { SettingsDefaultsProvider, useSettingsDefaults } from '../settings/SettingsDefaultsContext';
import { SETTINGS_TABS, type SettingsTabId } from '../settings/registry';
import { SettingsSearch } from '../components/settings/SettingsSearch';
import { SearchResults } from '../components/settings/SearchResults';
import {
  GeneralTab,
  BlinkTab,
  AnalysisTab,
  PlateSolvingTab,
  CalibrationTab,
  StackingTab,
  TransfersTab,
} from '../components/settings/tabs';

const VALID_TABS: readonly SettingsTabId[] = SETTINGS_TABS.map((t) => t.id);

/** How long the `?section=` deep-link's highlight ring stays visible. */
const SECTION_FLASH_MS = 1200;

function isSettingsTabId(v: string): v is SettingsTabId {
  return (VALID_TABS as readonly string[]).includes(v);
}

/** Scrolls a registered section into view and flashes its header once —
 *  `?section=<id>` (spec §2's deep-link form). Applied directly to the DOM
 *  node rather than through a `SettingsSection` prop: the highlight is a
 *  one-shot navigation effect, not a piece of that component's own state. */
function useSectionDeepLink(activeTab: SettingsTabId) {
  const [searchParams] = useSearchParams();
  const section = searchParams.get('section');

  useEffect(() => {
    if (!section) return;
    // The target section only exists in the DOM once its owning tab is
    // active — wait a tick for the tab body to mount/re-render before
    // looking it up.
    const raf = requestAnimationFrame(() => {
      const el = document.getElementById(`settings-${section}`);
      if (!el) return;
      el.scrollIntoView({ block: 'start', behavior: 'smooth' });
      el.classList.add('ring-2', 'ring-accent', 'transition-shadow');
      const t = setTimeout(() => {
        el.classList.remove('ring-2', 'ring-accent', 'transition-shadow');
      }, SECTION_FLASH_MS);
      return () => clearTimeout(t);
    });
    return () => cancelAnimationFrame(raf);
    // Re-run whenever the section id changes, or the active tab changes
    // (the same `?section=` can be visited again after switching away and
    // back — e.g. a repeated "→ Coverage"-style deep link).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [section, activeTab]);
}

function SettingsContent() {
  const { error: defaultsError } = useSettingsDefaults();
  const [searchParams, setSearchParams] = useSearchParams();
  const [query, setQuery] = useState('');
  const isSearching = query.trim().length > 0;

  const tabFromUrl = searchParams.get('tab') ?? '';
  const initialTab: SettingsTabId = isSettingsTabId(tabFromUrl) ? tabFromUrl : 'general';
  const [activeTab, _setActiveTab] = useState<SettingsTabId>(initialTab);

  const setActiveTab = (tab: SettingsTabId) => {
    _setActiveTab(tab);
    // `replace` so switching tabs doesn't pollute the history stack.
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        next.set('tab', tab);
        return next;
      },
      { replace: true },
    );
  };

  // If the URL `?tab=` changes while Settings is already mounted (e.g. a
  // "→ Coverage"-style deep link from another page), follow it.
  useEffect(() => {
    if (isSettingsTabId(tabFromUrl) && tabFromUrl !== activeTab) {
      _setActiveTab(tabFromUrl);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tabFromUrl]);

  useSectionDeepLink(activeTab);

  return (
    <div className="p-6 max-w-4xl">
      <div className="mb-6 flex items-center gap-2">
        <HistoryNav />
        <div>
          <h2 className="text-3xl font-bold">Settings</h2>
          <p className="text-content-muted">Configure application settings</p>
        </div>
      </div>

      <SettingsSearch value={query} onChange={setQuery} />
      {defaultsError && (
        <p className="text-xs text-content-muted mt-2">
          Defaults could not be loaded — per-field reset is unavailable this session.
        </p>
      )}

      {/* Spec §7: with a query active the tab bar stays visible but inert —
          switching "tabs" would be meaningless while the body shows search
          results gathered from every tab. Clearing the query returns to
          whichever tab was already selected (`activeTab` is untouched by
          search). */}
      <div
        className={`flex gap-1 mb-6 mt-4 border-b border-border overflow-x-auto ${
          isSearching ? 'opacity-50 pointer-events-none' : ''
        }`}
        aria-disabled={isSearching}
      >
        {SETTINGS_TABS.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            onClick={() => setActiveTab(id)}
            disabled={isSearching}
            tabIndex={isSearching ? -1 : undefined}
            className={`flex items-center gap-2 px-4 py-2 rounded-t-lg transition-colors whitespace-nowrap ${
              activeTab === id
                ? 'bg-surface-elevated text-white border-b-2 border-accent'
                : 'text-content-muted hover:text-content hover:bg-surface-elevated/50'
            }`}
          >
            <Icon size={18} />
            {label}
          </button>
        ))}
      </div>

      {isSearching ? (
        <SearchResults query={query} />
      ) : (
        <>
          {activeTab === 'general' && <GeneralTab />}
          {activeTab === 'blink' && <BlinkTab />}
          {activeTab === 'analysis' && <AnalysisTab />}
          {activeTab === 'plate_solving' && <PlateSolvingTab />}
          {activeTab === 'calibration' && <CalibrationTab />}
          {activeTab === 'stacking' && <StackingTab />}
          {activeTab === 'transfers' && <TransfersTab />}
        </>
      )}
    </div>
  );
}

export default function Settings() {
  return (
    <SettingsDefaultsProvider>
      <SettingsContent />
    </SettingsDefaultsProvider>
  );
}
