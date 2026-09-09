// Settings → Stacking (Plan 5b Task 5). Renders inner content only — the
// host card/heading are supplied by `Settings.tsx`, matching the
// `TransfersSection` pattern.
//
// Two things live here: the GLOBAL stacking defaults (every field
// `StageInspector`'s nine panels expose, minus the per-set folder override —
// spec §9.2's whole-config precedence: a frame set with no stored override
// runs exactly this document) and the two DEFAULT working/output folders
// every frame set falls back to absent its own override. Both are edited
// independently of any particular frame set, so this component owns its own
// load/persist/reset cycle rather than reusing `StackingTab`'s (which is
// keyed to one `framesSetId`).

import { useCallback, useEffect, useRef, useState } from 'react';
import { Loader2, RotateCcw } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useNotifications } from '../../contexts/NotificationContext';
import { ConfirmDialog } from '../ConfirmDialog';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { FolderCard } from '../stacking/FolderCard';
import { StageInspector } from '../stacking/StageInspector';
import { stageSummary, stableStringify, withoutPaths, type BoardStage } from '../stacking/stageSummary';
import type { StackingConfig, StackingPaths, StackingPreset, StackingPresets } from '../../types/stacking';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// The nine board stages (Task 3's `StageInspector` switch) — the `masters`
// stage does not exist yet (Task 8 adds it). Same order as `StackingTab`'s
// own `STAGE_LABEL`/board.
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

  const [config, setConfig] = useState<StackingConfig | null>(null);
  const [presets, setPresets] = useState<StackingPresets | null>(null);
  const [selectedStage, setSelectedStage] = useState<BoardStage>('calibrate');
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [resetting, setResetting] = useState(false);
  const [confirmResetOpen, setConfirmResetOpen] = useState(false);

  // Folders.
  const [paths, setPaths] = useState<StackingPaths | null>(null);
  const [pathsLoadError, setPathsLoadError] = useState<string | null>(null);
  const [pathError, setPathError] = useState<{ working: string | null; output: string | null }>({
    working: null,
    output: null,
  });
  const [savingPaths, setSavingPaths] = useState(false);
  const [browsing, setBrowsing] = useState<'working' | 'output' | null>(null);

  // Same "load must never write" discipline `StackingTab.tsx` established
  // (Task 3 fix round 1, Critical #1): `dirtyRef` is set ONLY by
  // `setUserConfig`, never by the mount-load effect, so opening this page
  // can never materialize a `stacking.defaults` row that wasn't already
  // there.
  const dirtyRef = useRef(false);
  const pendingConfigRef = useRef<StackingConfig | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [cfg, presetsResult] = await Promise.all([
          api.invoke<StackingConfig>('get_stacking_defaults', {}),
          api.invoke<StackingPresets>('get_stacking_presets', {}),
        ]);
        if (cancelled) return;
        setConfig(cfg);
        setPresets(presetsResult);
      } catch (err) {
        console.error('[StackingSection] failed to load stacking defaults:', err);
        if (!cancelled) setLoadError(errMsg(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
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

  /** The only way `config` should change as a result of a USER action —
   *  mirrors `StackingTab.tsx`'s `setUserConfig`. The load effect above
   *  calls `setConfig` directly, bypassing this, so it can never mark the
   *  draft dirty. */
  const setUserConfig = useCallback((next: StackingConfig) => {
    dirtyRef.current = true;
    setConfig(next);
  }, []);

  const sendPendingConfig = useCallback((notifyOnFailure: boolean) => {
    const payload = pendingConfigRef.current;
    if (!payload) return;
    dirtyRef.current = false;
    pendingConfigRef.current = null;
    api.invoke('set_stacking_defaults', { config: payload })
      .catch((err) => {
        console.error('[StackingSection] set_stacking_defaults failed:', err);
        if (notifyOnFailure) {
          notify({
            tone: 'warning',
            kind: 'stacking',
            toast: true,
            title: 'Stacking defaults not saved',
            detail: errMsg(err),
          });
        }
      });
  }, [notify]);

  // Persist (debounced), gated on `dirtyRef` — a load, and the Reset flow
  // below (which clears `dirtyRef` itself), never trigger a write.
  useEffect(() => {
    if (!config) return;
    if (!dirtyRef.current) return;
    pendingConfigRef.current = config;
    const t = setTimeout(() => sendPendingConfig(true), 500);
    return () => clearTimeout(t);
  }, [config, sendPendingConfig]);

  // Flush a still-pending write on unmount (e.g. the user switches Settings
  // tabs inside the 500 ms window) instead of silently dropping it — same
  // reasoning as `StackingTab.tsx`'s own flush effect, no toast for a page
  // the user has already left.
  useEffect(() => {
    return () => {
      if (dirtyRef.current && pendingConfigRef.current) {
        sendPendingConfig(false);
      }
    };
  }, [sendPendingConfig]);

  const handleConfigChange = useCallback((next: StackingConfig) => {
    setUserConfig(next);
  }, [setUserConfig]);

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
    // verbatim is correct.
    setUserConfig(presets[preset]);
  }, [presets, setUserConfig]);

  const handleResetClick = useCallback(() => {
    setConfirmResetOpen(true);
  }, []);

  const handleResetConfirmed = useCallback(async () => {
    setResetting(true);
    try {
      const next = await api.invoke<StackingConfig>('reset_stacking_defaults', {});
      // The server's own canonical state, not a user edit — clear any
      // pending write and leave the draft NOT dirty (brief's literal
      // wording), so the persist effect above does not immediately re-send
      // what was just reset.
      dirtyRef.current = false;
      pendingConfigRef.current = null;
      setConfig(next);
    } catch (err) {
      console.error('[StackingSection] reset_stacking_defaults failed:', err);
      notify({
        tone: 'warning',
        kind: 'stacking',
        toast: true,
        title: 'Stacking defaults not saved',
        detail: errMsg(err),
      });
    } finally {
      setResetting(false);
      setConfirmResetOpen(false);
    }
  }, [notify]);

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
  // `set_transfer_paths`.
  const applyPaths = useCallback(async (
    working: string | null | undefined,
    output: string | null | undefined,
  ) => {
    if (!paths) return;
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
      const msg = errMsg(err);
      setPathError(
        msg.startsWith('Stacking working folder')
          ? { working: msg, output: null }
          : { working: null, output: msg },
      );
    } finally {
      setSavingPaths(false);
    }
  }, [paths]);

  const choose = useCallback(async (which: 'working' | 'output') => {
    if (isTauri) {
      try {
        const picked = await pickDirectory();
        if (!picked) return;
        await applyPaths(which === 'working' ? picked : undefined, which === 'output' ? picked : undefined);
      } catch (err) {
        console.error('[StackingSection] folder picker failed:', err);
        const msg = errMsg(err);
        setPathError(which === 'working' ? { working: msg, output: null } : { working: null, output: msg });
      }
    } else {
      setBrowsing(which);
    }
  }, [applyPaths]);

  if (loading) {
    return (
      <div className="text-center py-8">
        <Loader2 size={24} className="animate-spin mx-auto mb-2 text-content-muted" />
        <p className="text-content-muted text-sm">Loading stacking defaults…</p>
      </div>
    );
  }

  if (loadError || !config || !presets) {
    return (
      <p className="text-sm text-content-muted">
        Failed to load the stacking defaults{loadError ? `: ${loadError}` : '.'}
      </p>
    );
  }

  return (
    <div className="space-y-6">
      {/* Global defaults: the same nine-panel inspector the Stacking tab
       *  uses, bound to `get_stacking_defaults`/`set_stacking_defaults`
       *  instead of one frame set's config, `plan={null}` (there is no plan
       *  outside a frame set), no Re-measure (nothing to re-measure), and
       *  `mode="global"` so the Output panel hides its per-set folder
       *  override cards — the two folders below are the DEFAULTS those
       *  overrides fall back to, not the same thing. */}
      <div>
        <div className="flex items-center justify-between gap-3 mb-2">
          <h4 className="text-sm font-medium text-content-secondary">Pipeline defaults</h4>
          <div className="flex items-center gap-2">
            <div className="relative">
              <select
                value=""
                onChange={(e) => {
                  if (e.target.value) applyPreset(e.target.value as StackingPreset);
                }}
                disabled={resetting}
                aria-label="Apply a built-in preset"
                className="rounded-md border border-border bg-surface-hover px-2 py-1 text-xs text-content-secondary focus:outline-none focus:border-accent disabled:opacity-50"
              >
                <option value="">{presetLabel} — apply preset…</option>
                {(Object.keys(PRESET_LABEL) as StackingPreset[]).map((p) => (
                  <option key={p} value={p}>{PRESET_LABEL[p]}</option>
                ))}
              </select>
            </div>
            <button
              type="button"
              onClick={handleResetClick}
              disabled={resetting}
              className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
            >
              {resetting ? <Loader2 size={13} className="animate-spin" /> : <RotateCcw size={13} />}
              Reset to built-in defaults
            </button>
          </div>
        </div>

        <div className="flex flex-col md:flex-row gap-4">
          <div className="md:w-56 shrink-0">
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
              disabled={resetting}
              presetDefault={presets.default}
              mode="global"
            />
          </div>
        </div>
      </div>

      {/* Default folders: what a frame set with no override falls back to. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Default folders</h4>
        <div className="space-y-3">
          {paths && (
            <>
              <FolderCard
                title="Working folder"
                hint="Where a stacking run stages registered/intermediate frames, unless a frame set overrides it."
                setting={paths.working}
                onChoose={() => choose('working')}
                onReset={() => applyPaths(null, undefined)}
                error={pathError.working}
                busy={savingPaths}
              />
              <FolderCard
                title="Output folder"
                hint="Where a stacking run writes its master(s), unless a frame set overrides it."
                setting={paths.output}
                onChoose={() => choose('output')}
                onReset={() => applyPaths(undefined, null)}
                error={pathError.output}
                busy={savingPaths}
              />
            </>
          )}
          {!paths && pathsLoadError && (
            <p className="text-xs text-error">Could not read the stacking folders: {pathsLoadError}</p>
          )}
        </div>
      </div>

      <ConfirmDialog
        isOpen={confirmResetOpen}
        title="Reset stacking defaults?"
        message="This resets every stacking pipeline setting on this page back to the built-in default. Frame sets with their own override are unaffected. Folders are unaffected."
        confirmText="Reset"
        confirmDanger
        onConfirm={() => void handleResetConfirmed()}
        onCancel={() => setConfirmResetOpen(false)}
      />

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
          void applyPaths(which === 'working' ? path : undefined, which === 'output' ? path : undefined);
        }}
        onClose={() => setBrowsing(null)}
      />
    </div>
  );
}
