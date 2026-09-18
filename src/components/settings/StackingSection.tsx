// Settings → Stacking (Plan 5b Task 5; on the shared autosave hook since the
// settings redesign's Task D2). Mounted directly by `StackingTab.tsx` — this
// component now renders its own two registered sections (`stacking.pipeline`,
// `stacking.folders`) rather than being wrapped in a caller-supplied card,
// since each area is its own `SettingsSection` (title/description from the
// registry, the pipeline section's preset picker as its `actions`, its
// "Reset all" wired to `reset_stacking_defaults`).
//
// Two things live here: the GLOBAL stacking defaults (every field
// `StageInspector`'s nine panels expose, minus the per-set folder override —
// spec §9.2's whole-config precedence: a frame set with no stored override
// runs exactly this document) and the two DEFAULT working/output folders
// every frame set falls back to absent its own override. The document half
// goes through `useAutosaveDocument` (load/debounce/unmount-flush — same
// discipline as before, generalized); the folders keep their own
// load/persist cycle, unrelated to the document (`set_stacking_paths` is a
// full-replace command, not part of `StackingConfig`).

import { useCallback, useEffect, useState } from 'react';
import { Loader2 } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useNotifications } from '../../contexts/NotificationContext';
import { useAutosaveDocument } from '../../hooks/useAutosaveDocument';
import { useSettingsDefaults } from '../../settings/SettingsDefaultsContext';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { FolderCard } from '../stacking/FolderCard';
import { StageInspector } from '../stacking/StageInspector';
import { stageSummary, stableStringify, withoutPaths, type BoardStage } from '../stacking/stageSummary';
import { SettingsSection } from './SettingsSection';
import type { StackingConfig, StackingPaths, StackingPreset, StackingPresets } from '../../types/stacking';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Which card a `set_stacking_paths` error belongs under. Task 5 fix round
 *  1, Important #1: error routing is prefix-only by default — the pair-rule
 *  messages ("working and output folders must differ", "the working folder
 *  may not sit inside the output folder") and any other non-prefixed
 *  message carry no folder name at all, so before this fix they always fell
 *  through to the "not working-prefixed → output" branch regardless of
 *  which card the user actually touched. Now: a message that explicitly
 *  names ONE folder (`"Stacking working/output folder: …"`,
 *  `crates/athenaeum-core/src/api/sync.rs::validate_transfer_dir`) always
 *  routes there — that is the more specific, more correct answer whichever
 *  card was clicked; anything else (the pair-rule messages, or any
 *  unprefixed message) routes to whichever card the user actually touched. */
function routeFolderError(
  msg: string,
  touched: 'working' | 'output',
): { working: string | null; output: string | null } {
  if (msg.startsWith('Stacking working folder')) return { working: msg, output: null };
  if (msg.startsWith('Stacking output folder')) return { working: null, output: msg };
  return touched === 'working' ? { working: msg, output: null } : { working: null, output: msg };
}

// The nine board stages (Task 3's `StageInspector` switch). Task 8's new
// `masters` stage is DELIBERATELY excluded here (unlike `StackingTab`'s own
// board): stage 0.5 has no config of its own to edit — its inspector reads
// `plan.mastersToBuild`, and there is no per-set plan in the GLOBAL defaults
// editor (this component edits config independent of any frame set). Same
// order as `StackingTab`'s own `STAGE_LABEL`/board otherwise.
const STAGES: readonly BoardStage[] = [
  'calibrate',
  'debayer',
  'measure',
  'reference',
  'register',
  'normalize',
  'integrate',
  'drizzle',
  'output',
];

const STAGE_LABEL: Record<BoardStage, string> = {
  // Never rendered — `masters` is excluded from `STAGES` above — but
  // `Record<BoardStage, string>` requires every key.
  masters: 'Masters',
  calibrate: 'Calibrate',
  debayer: 'Debayer',
  measure: 'Measure & select',
  reference: 'Reference',
  register: 'Register',
  normalize: 'Local normalization',
  integrate: 'Integrate',
  drizzle: 'Drizzle',
  output: 'Output',
};

const PRESET_LABEL: Record<StackingPreset, string> = {
  default: 'Default',
  fastPreview: 'Fast preview',
  maximumQuality: 'Maximum quality',
};

export default function StackingSection() {
  const { notify } = useNotifications();
  const { defaults } = useSettingsDefaults();

  const {
    doc: config,
    patch,
    error: configError,
    resetAll,
  } = useAutosaveDocument<StackingConfig>({
    load: () => api.invoke<StackingConfig>('get_stacking_defaults', {}),
    save: (c) => api.invoke('set_stacking_defaults', { config: c }),
    resetAll: async () => {
      await api.invoke('reset_stacking_defaults', {});
    },
    defaults: defaults?.stacking ?? null,
    label: 'Stacking defaults',
  });

  const [presets, setPresets] = useState<StackingPresets | null>(null);
  const [presetsError, setPresetsError] = useState<string | null>(null);
  const [selectedStage, setSelectedStage] = useState<BoardStage>('calibrate');

  // Folders.
  const [paths, setPaths] = useState<StackingPaths | null>(null);
  const [pathsLoadError, setPathsLoadError] = useState<string | null>(null);
  const [pathError, setPathError] = useState<{ working: string | null; output: string | null }>({
    working: null,
    output: null,
  });
  const [savingPaths, setSavingPaths] = useState(false);
  const [browsing, setBrowsing] = useState<'working' | 'output' | null>(null);

  // Reference data for the preset picker — unrelated to the document above,
  // so it's its own effect rather than folded into `useAutosaveDocument`'s
  // load (which owns exactly one document: `StackingConfig`).
  useEffect(() => {
    let cancelled = false;
    api
      .invoke<StackingPresets>('get_stacking_presets', {})
      .then((result) => {
        if (!cancelled) setPresets(result);
      })
      .catch((err) => {
        console.error('[StackingSection] get_stacking_presets failed:', err);
        if (!cancelled) setPresetsError(errMsg(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const refreshPaths = useCallback(async () => {
    try {
      const p = await api.invoke<StackingPaths>('get_stacking_paths', {});
      setPaths(p);
      setPathsLoadError(null);
    } catch (err) {
      console.error('[StackingSection] get_stacking_paths failed:', err);
      setPathsLoadError(errMsg(err));
    }
  }, []);

  useEffect(() => { void refreshPaths(); }, [refreshPaths]);

  const presetLabel = (() => {
    if (!config || !presets) return 'Custom';
    const key = stableStringify(withoutPaths(config));
    if (key === stableStringify(withoutPaths(presets.default))) return 'Default';
    if (key === stableStringify(withoutPaths(presets.fastPreview))) return 'Fast preview';
    if (key === stableStringify(withoutPaths(presets.maximumQuality))) return 'Maximum quality';
    return 'Custom';
  })();

  const applyPreset = useCallback((preset: StackingPreset) => {
    if (!presets) return;
    // Global defaults have no per-set folder override to preserve (`paths`
    // stays `null`/`null` here, unlike `StackingTab`'s draft) — every
    // built-in preset already carries `paths: null/null` itself
    // (`preset()` starts from `StackingConfig::default()`), so applying one
    // verbatim is correct. `presets[preset]` is the SAME object every time
    // this is called (`presets` state, fetched once) — assigning it
    // straight to the document would alias it, so a later in-place mutation
    // of the draft (none of this file's own code does that, but every
    // panel's `onChange` receives whatever object `config` currently is)
    // could corrupt the built-in preset a second "apply" would read from.
    // Clone before it becomes the document.
    patch(() => structuredClone(presets[preset]));
  }, [presets, patch]);

  const handleConfigChange = useCallback((next: StackingConfig) => {
    patch(() => next);
  }, [patch]);

  // Folders. `set_stacking_paths(working, output)` is a full replace, not a
  // per-field patch — both arguments are plain `Option<String>` with
  // `#[serde(default)]` on the wire (`crates/athenaeum-web/src/routes/
  // stacking.rs::SetStackingPathsArgs`), so an OMITTED field and an
  // EXPLICIT `null` decode identically to `None`, and
  // `api::set_stacking_paths` (`crates/athenaeum-core/src/api/stacking.rs`)
  // writes `unwrap_or_default()` (empty string = unset) for whichever
  // argument is `None`. Sending only the field being changed would silently
  // erase the OTHER folder if one was already configured. `applyPaths`
  // below always resends both, resolving `undefined` (not touched) to the
  // current `configured` value — the exact pattern `TransfersSection.tsx`'s
  // own `applyPaths` already uses for the identical full-replace contract on
  // `set_transfer_paths`. `touched` names which CARD the caller is acting
  // on — used only for error routing (`routeFolderError` above), never for
  // the request payload itself, since both folders always travel together
  // regardless of which one changed.
  const applyPaths = useCallback(async (
    touched: 'working' | 'output',
    working: string | null | undefined,
    output: string | null | undefined,
  ) => {
    if (!paths) {
      // A silent return here (the folders haven't loaded yet, or
      // `refreshPaths` failed) would drop the user's action on the floor
      // with no trace — log and warn like every other failure path in this
      // file, never swallow.
      console.error('[StackingSection] applyPaths called before paths loaded');
      notify({
        tone: 'warning',
        kind: 'stacking',
        toast: true,
        title: 'Stacking folder not saved',
        detail: 'The current folders have not finished loading — try again in a moment.',
      });
      return;
    }
    setSavingPaths(true);
    setPathError({ working: null, output: null });
    try {
      const next = await api.invoke<StackingPaths>('set_stacking_paths', {
        working: working === undefined ? paths.working.configured : working,
        output: output === undefined ? paths.output.configured : output,
      });
      setPaths(next);
    } catch (err) {
      console.error('[StackingSection] set_stacking_paths failed:', err);
      setPathError(routeFolderError(errMsg(err), touched));
    } finally {
      setSavingPaths(false);
    }
  }, [paths, notify]);

  const choose = useCallback(async (which: 'working' | 'output') => {
    if (isTauri) {
      try {
        const picked = await pickDirectory();
        if (!picked) return;
        await applyPaths(which, which === 'working' ? picked : undefined, which === 'output' ? picked : undefined);
      } catch (err) {
        console.error('[StackingSection] folder picker failed:', err);
        setPathError(routeFolderError(errMsg(err), which));
      }
    } else {
      setBrowsing(which);
    }
  }, [applyPaths]);

  return (
    <div className="space-y-6">
      <SettingsSection
        id="stacking.pipeline"
        onResetAll={resetAll}
        resetScopeLabel="This resets every stacking pipeline setting on this page back to the built-in default. Frame sets with their own override are unaffected. Folders are unaffected."
        actions={
          <select
            value=""
            onChange={(e) => {
              if (e.target.value) applyPreset(e.target.value as StackingPreset);
            }}
            aria-label="Apply a built-in preset"
            className="rounded-md border border-border bg-surface-hover px-2 py-1 text-xs text-content-secondary focus:outline-none focus:border-accent"
          >
            <option value="">{presetLabel} — apply preset…</option>
            {(Object.keys(PRESET_LABEL) as StackingPreset[]).map((p) => (
              <option key={p} value={p}>{PRESET_LABEL[p]}</option>
            ))}
          </select>
        }
      >
        {!config || !presets ? (
          configError ? (
            <p className="text-sm text-content-muted">Failed to load the stacking defaults: {configError}</p>
          ) : presetsError ? (
            <p className="text-sm text-content-muted">Failed to load the stacking presets: {presetsError}</p>
          ) : (
            <div className="text-center py-8">
              <Loader2 size={24} className="animate-spin mx-auto mb-2 text-content-muted" />
              <p className="text-content-muted text-sm">Loading stacking defaults…</p>
            </div>
          )
        ) : (
          <div className="flex flex-col md:flex-row gap-4 xl:gap-6">
            <div className="md:w-56 xl:w-64 shrink-0">
              <div className="flex flex-row md:flex-col gap-1 overflow-x-auto md:overflow-visible">
                {STAGES.map((stage, i) => (
                  <button
                    key={stage}
                    type="button"
                    onClick={() => setSelectedStage(stage)}
                    className={`shrink-0 text-left px-3 py-2 rounded-lg transition-colors ${
                      selectedStage === stage ? 'bg-surface-hover' : 'hover:bg-surface-hover/50'
                    }`}
                  >
                    <div className="flex items-center gap-2">
                      <span className="text-xs text-content-muted tabular-nums w-4">{i + 1}</span>
                      <span className="text-sm font-medium text-content whitespace-nowrap md:whitespace-normal">
                        {STAGE_LABEL[stage]}
                      </span>
                    </div>
                    <p className="ml-6 text-xs text-content-muted truncate max-w-[220px] md:max-w-none">
                      {stageSummary(stage, config)}
                    </p>
                  </button>
                ))}
              </div>
            </div>

            <div className="flex-1 min-w-0">
              <StageInspector
                stage={selectedStage}
                config={config}
                onChange={handleConfigChange}
                plan={null}
                presetDefault={presets.default}
                mode="global"
              />
            </div>
          </div>
        )}
      </SettingsSection>

      <SettingsSection id="stacking.folders">
        <div className="space-y-3">
          {paths && (
            <>
              <FolderCard
                title="Working folder"
                hint="Where a stacking run stages registered/intermediate frames, unless a frame set overrides it."
                setting={paths.working}
                onChoose={() => choose('working')}
                onReset={() => applyPaths('working', null, undefined)}
                error={pathError.working}
                busy={savingPaths}
              />
              <FolderCard
                title="Output folder"
                hint="Where a stacking run writes its master(s), unless a frame set overrides it."
                setting={paths.output}
                onChoose={() => choose('output')}
                onReset={() => applyPaths('output', undefined, null)}
                error={pathError.output}
                busy={savingPaths}
              />
            </>
          )}
          {!paths && pathsLoadError && (
            <p className="text-xs text-error">Could not read the stacking folders: {pathsLoadError}</p>
          )}
        </div>
      </SettingsSection>

      <FolderBrowserModal
        isOpen={browsing !== null}
        scope="stacking"
        onSelect={(path) => {
          const which = browsing;
          setBrowsing(null);
          if (!which) {
            console.error('[StackingSection] folder selected with no target — dropping', path);
            return;
          }
          void applyPaths(which, which === 'working' ? path : undefined, which === 'output' ? path : undefined);
        }}
        onClose={() => setBrowsing(null)}
      />
    </div>
  );
}
