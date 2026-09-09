import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useNavigate } from 'react-router-dom';
import { Play, Square, ChevronDown, FolderOpen, AlertTriangle, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useStackingContext } from '../../contexts/StackingContext';
import { useNotifications } from '../../contexts/NotificationContext';
import type {
  Stage,
  StackingConfig,
  StackingPlan,
  StackingPreset,
  StackingPresets,
  StackingSetConfig,
} from '../../types/stacking';
import { PipelineBoard } from './PipelineBoard';
import { GroupsTable } from './GroupsTable';
import { StageInspector } from './StageInspector';
import { stableStringify, type BoardStage } from './stageSummary';
import { readSelectedStage, writeSelectedStage } from './stackingPrefs';

export interface StackingTabProps {
  framesSetId: number;
  frameSetName?: string;
}

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

function formatGB(bytes: number): string {
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

/** Every field except `paths` — the preset comparison (and `applyPreset`)
 *  ignore the per-set folder override, which is never part of what makes a
 *  config "Default"/"Fast preview"/"Maximum quality" (plan 5b Task 3
 *  "Decisions" item 3). */
function withoutPaths(config: StackingConfig): Omit<StackingConfig, 'paths'> {
  const { paths: _paths, ...rest } = config;
  return rest;
}

/**
 * The Stacking tab (spec §11) — plans, configures, runs and watches an M1
 * stacking run for one frame set. Owns the plan fetch, the (unsaved) config
 * draft used to preview the plan, the run state from `useStackingContext`,
 * and the board/inspector layout. Task 3 fills the inspector; Task 4 adds
 * the Frames table and Results panel.
 */
// `frameSetName` is part of the props contract FrameSetDetail.tsx passes
// (matching every sibling tab's signature) but, fix round 1 item 6, is not
// rendered as a `title` on the tab's root — a `title` on a page-sized `div`
// pops a native tooltip over the ENTIRE tab on any hover, not just the
// header. Nothing else in this tab needs the set name (FrameSetDetail's own
// header above the tab bar already shows it), so it is simply not
// destructured here.
export function StackingTab({ framesSetId }: StackingTabProps) {
  const navigate = useNavigate();
  const { notify } = useNotifications();
  const { progress, lastOutcome, startRun, cancelRun, isRunning } = useStackingContext();

  const [draftConfig, setDraftConfig] = useState<StackingConfig | null>(null);
  const [excludedFrameIds, setExcludedFrameIds] = useState<number[]>([]);
  const [presets, setPresets] = useState<StackingPresets | null>(null);
  const [plan, setPlan] = useState<StackingPlan | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedStage, setSelectedStage] = useState<BoardStage>(readSelectedStage);
  const [starting, setStarting] = useState(false);
  const [rerunMenuOpen, setRerunMenuOpen] = useState(false);
  const [presetMenuOpen, setPresetMenuOpen] = useState(false);

  const runProgress = progress.get(framesSetId);
  const runOutcome = lastOutcome.get(framesSetId);
  const running = isRunning(framesSetId) || plan?.activeRunId != null;

  // `refetchPlan` is called from four overlapping triggers (the mount
  // effect below calls the endpoint directly, not through here; but the
  // `library-updated` listener, the 300 ms debounce, and the outcome effect
  // all go through this one function) — an older response can resolve
  // after a newer one already landed, and after navigating to a different
  // frame set (`StackingTab` carries no `key` tied to `framesSetId`, so a
  // reused instance is possible) a stale response for the PREVIOUS set can
  // still be in flight. `planSeqRef` drops a response that is no longer the
  // latest call; `framesSetIdRef` (always current, unlike the `framesSetId`
  // this closure captured at creation) drops one whose set has since
  // changed (fix round 1, Important #2).
  const planSeqRef = useRef(0);
  const framesSetIdRef = useRef(framesSetId);
  useEffect(() => { framesSetIdRef.current = framesSetId; }, [framesSetId]);

  const refetchPlan = useCallback(async (configOverride?: StackingConfig) => {
    const seq = ++planSeqRef.current;
    const forSetId = framesSetId;
    try {
      const p = await api.invoke<StackingPlan>('get_stacking_plan', {
        setId: forSetId,
        config: configOverride,
      });
      if (seq !== planSeqRef.current || forSetId !== framesSetIdRef.current) {
        return null; // superseded by a later call, or the set changed
      }
      setPlan(p);
      return p;
    } catch (err) {
      console.error('[StackingTab] failed to refresh the stacking plan:', err);
      return null;
    }
  }, [framesSetId]);

  // Mount / frame-set-change: load the set's config, then the plan built
  // from it.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    (async () => {
      try {
        const cfg = await api.invoke<StackingSetConfig>('get_stacking_config', { setId: framesSetId });
        if (cancelled) return;
        setDraftConfig(cfg.config);
        setExcludedFrameIds(cfg.excludedFrameIds);
        const presetsResult = await api.invoke<StackingPresets>('get_stacking_presets', {});
        if (cancelled) return;
        setPresets(presetsResult);
        const p = await api.invoke<StackingPlan>('get_stacking_plan', {
          setId: framesSetId,
          config: cfg.config,
        });
        if (cancelled) return;
        setPlan(p);
      } catch (err) {
        console.error('[StackingTab] failed to load stacking config/plan:', err);
        if (!cancelled) setLoadError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [framesSetId]);

  // Always-current mirror of `draftConfig` for effects that must react to
  // something ELSE (a DOM event, a finished run) without themselves
  // re-running on every keystroke of config editing.
  const draftConfigRef = useRef<StackingConfig | null>(draftConfig);
  useEffect(() => { draftConfigRef.current = draftConfig; }, [draftConfig]);

  // A master build (or another completed stacking run) changed the catalog
  // shape underneath us — re-plan.
  useEffect(() => {
    const handler = () => { void refetchPlan(draftConfigRef.current ?? undefined); };
    window.addEventListener('library-updated', handler);
    return () => window.removeEventListener('library-updated', handler);
  }, [refetchPlan]);

  // Re-plan (debounced) whenever the draft config changes — skip the very
  // first assignment, which is the mount effect's own initial fetch.
  const skipFirstDraftEffect = useRef(true);
  useEffect(() => {
    if (skipFirstDraftEffect.current) {
      skipFirstDraftEffect.current = false;
      return;
    }
    if (!draftConfig) return;
    const t = setTimeout(() => { void refetchPlan(draftConfig); }, 300);
    return () => clearTimeout(t);
  }, [draftConfig, refetchPlan]);

  // A run just finished (success or otherwise) — the plan's stale-stage /
  // cached-artifact state is now out of date.
  const lastOutcomeRunId = runOutcome?.runId;
  useEffect(() => {
    if (lastOutcomeRunId == null) return;
    void refetchPlan(draftConfigRef.current ?? undefined);
  }, [lastOutcomeRunId, refetchPlan]);

  // Persist (debounced) whenever the draft config or the excluded-frame list
  // changes — skip the very first assignment (the mount effect's own seed).
  // Submit state, never a re-read (spec §11.2): a failed write logs and
  // warns but never rolls the draft back or re-fetches.
  const skipFirstPersistEffect = useRef(true);
  useEffect(() => {
    if (skipFirstPersistEffect.current) {
      skipFirstPersistEffect.current = false;
      return;
    }
    if (!draftConfig) return;
    const t = setTimeout(() => {
      api
        .invoke('set_stacking_config', { setId: framesSetId, config: draftConfig, excludedFrameIds })
        .catch((err) => {
          console.error('[StackingTab] set_stacking_config failed:', err);
          notify({
            tone: 'warning',
            kind: 'stacking',
            toast: true,
            title: 'Stacking settings not saved',
            detail: String(err),
          });
        });
    }, 500);
    return () => clearTimeout(t);
  }, [draftConfig, excludedFrameIds, framesSetId, notify]);

  const handleConfigChange = useCallback((next: StackingConfig) => {
    setDraftConfig(next);
  }, []);

  const handleToggleWriteRegisteredFrames = useCallback((checked: boolean) => {
    setDraftConfig((prev) =>
      prev ? { ...prev, registration: { ...prev.registration, writeRegisteredFrames: checked } } : prev,
    );
  }, []);

  const handleSelectStage = useCallback((stage: BoardStage) => {
    setSelectedStage(stage);
    writeSelectedStage(stage);
  }, []);

  // Both toolbar dropdowns (preset selector, Re-run from) close on an
  // outside click or Escape (fix round 1, Minor #8 — named for the Re-run
  // menu specifically, applied to both since they share the exact same
  // open/close pattern in this same file).
  const presetMenuRef = useRef<HTMLDivElement>(null);
  const rerunMenuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!presetMenuOpen && !rerunMenuOpen) return;
    const handlePointerDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (presetMenuOpen && presetMenuRef.current && !presetMenuRef.current.contains(target)) {
        setPresetMenuOpen(false);
      }
      if (rerunMenuOpen && rerunMenuRef.current && !rerunMenuRef.current.contains(target)) {
        setRerunMenuOpen(false);
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      setPresetMenuOpen(false);
      setRerunMenuOpen(false);
    };
    document.addEventListener('mousedown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [presetMenuOpen, rerunMenuOpen]);

  // Preset selector (plan Ruling 1): the label is computed, never stored —
  // comparing the draft (minus its per-set folder override) against each
  // built-in preset via canonical JSON.
  const presetLabel = useMemo<string>(() => {
    if (!draftConfig || !presets) return 'Custom';
    const draftKey = stableStringify(withoutPaths(draftConfig));
    if (draftKey === stableStringify(withoutPaths(presets.default))) return 'Default';
    if (draftKey === stableStringify(withoutPaths(presets.fastPreview))) return 'Fast preview';
    if (draftKey === stableStringify(withoutPaths(presets.maximumQuality))) return 'Maximum quality';
    return 'Custom';
  }, [draftConfig, presets]);

  const applyPreset = useCallback((preset: StackingPreset) => {
    setDraftConfig((prev) => {
      if (!presets || !prev) return prev;
      return { ...presets[preset], paths: prev.paths };
    });
    setPresetMenuOpen(false);
  }, [presets]);

  // `starting` bridges the click-to-run gap. `startRun`'s invoke does not
  // resolve until the backend has synchronously built the WHOLE plan
  // (gate + per-frame hashing) and spawned the run thread (see
  // `routes/stacking.rs`'s doc comment on `start_stacking`) — clearing
  // `starting` the instant that promise resolves (the old behavior) left a
  // real gap, between the invoke resolving and the run's first
  // `stacking-progress` event actually landing, where BOTH `starting` and
  // `running` (`isRunning`, which only becomes true once a progress event
  // arrives) read false — re-enabling the Run button and allowing a
  // double-start (fix round 1, Minor #5). `starting` now stays true until
  // either a progress event or an outcome for the SPECIFIC run just started
  // arrives (tracked by `startingRunIdRef`, not just "any progress/outcome
  // for this set", since `lastOutcome` never clears and would otherwise
  // read as "already finished" the instant a NEW run starts).
  const startingRunIdRef = useRef<number | null>(null);

  const beginStarting = useCallback(async (invoke: () => Promise<number>) => {
    setStarting(true);
    try {
      startingRunIdRef.current = await invoke();
    } catch {
      // `useStackingRuns`'s `startRun` already logs + notifies before
      // rethrowing (fix round 1, Minor #4) — nothing left to do here but
      // stop showing "starting".
      startingRunIdRef.current = null;
      setStarting(false);
    }
  }, []);

  useEffect(() => {
    if (!starting) return;
    const waitingFor = startingRunIdRef.current;
    if (waitingFor == null) return; // still awaiting `startRun`'s invoke itself
    if (runProgress?.runId === waitingFor || runOutcome?.runId === waitingFor) {
      startingRunIdRef.current = null;
      setStarting(false);
    }
  }, [starting, runProgress, runOutcome]);

  const handleRun = useCallback(() => {
    if (!plan || plan.blockers.length > 0 || running || starting) return;
    void beginStarting(() => startRun(framesSetId, draftConfig ?? undefined));
  }, [plan, running, starting, beginStarting, startRun, framesSetId, draftConfig]);

  const handleCancel = useCallback(async () => {
    const runId = runProgress?.runId ?? plan?.activeRunId ?? null;
    if (runId == null) return;
    try {
      await cancelRun(runId);
    } catch {
      // `useStackingRuns`'s `cancelRun` already logs + notifies.
    }
  }, [runProgress, plan, cancelRun]);

  const handleRerunFrom = useCallback((stage: Stage) => {
    setRerunMenuOpen(false);
    if (running || starting) return;
    void beginStarting(() => startRun(framesSetId, draftConfig ?? undefined, stage));
  }, [running, starting, beginStarting, startRun, framesSetId, draftConfig]);

  // Measure panel's own "Re-measure" button (spec: rerunFrom: 'measure').
  // Independent disabled logic from the toolbar's "Re-run from" menu, which
  // gates on staleness — this one gates on the plan's blockers directly,
  // per the brief.
  const remeasureDisabled = running || starting || (plan?.blockers.length ?? 0) > 0;
  const handleRemeasure = useCallback(() => {
    void handleRerunFrom('measure');
  }, [handleRerunFrom]);

  // `FrameSetDetail.tsx`'s searchParams effect only highlights a Coverage
  // row when BOTH `highlightSet` and `kind` are present and `kind` parses
  // as `'flat' | 'dark' | 'bias'` — `&kind=` was missing (fix round 1,
  // Minor #7). `StackingPlan.readiness` (`ExportReadiness`) carries no
  // set→kind map the way `ExportTab.tsx`'s own `summary`-derived
  // `setKindMap` does, so the real kind of the first set without a master
  // can't be resolved here without a second fetch — `'dark'` is exactly
  // ExportTab's OWN fallback (`setKindMap.get(setId) ?? 'dark'`) for a set
  // its map doesn't know either, so this matches its behavior rather than
  // inventing a new default.
  const handleCoverageClick = useCallback(() => {
    const setId = plan && plan.readiness.rawSetsWithoutMaster > 0
      ? plan.readiness.rawSetIdsWithoutMaster[0]
      : undefined;
    if (setId !== undefined) {
      navigate(`?tab=calibration&highlightSet=${setId}&kind=dark`, { replace: true });
    } else {
      navigate('?tab=calibration', { replace: true });
    }
  }, [navigate, plan]);

  if (loading) {
    return (
      <div className="text-center py-12">
        <Loader2 size={28} className="animate-spin mx-auto mb-3 text-content-muted" />
        <p className="text-content-muted">Loading stacking plan…</p>
      </div>
    );
  }

  if (loadError || !plan || !draftConfig || !presets) {
    return (
      <div className="text-center py-12 text-content-muted">
        <p>Failed to load the stacking plan{loadError ? `: ${loadError}` : '.'}</p>
      </div>
    );
  }

  const rerunOptions = Array.from(new Set<Stage>([...plan.staleStages, 'integrate']));
  const rerunDisabled = plan.staleStages.length === 0 || running || starting;
  const runDisabled = plan.blockers.length > 0 || running || starting;
  const runTooltip = plan.blockers.length > 0 ? plan.blockers[0].message : undefined;
  const freeLabel = plan.freeBytes == null ? 'free space unknown' : `Free ${formatGB(plan.freeBytes)}`;

  return (
    <div className="space-y-3">
      {/* Toolbar */}
      <div className="flex flex-wrap items-center justify-between gap-3 bg-surface-elevated rounded-lg px-4 py-3">
        <div className="flex flex-wrap items-center gap-4 text-sm text-content-secondary">
          <div className="relative" ref={presetMenuRef}>
            <button
              type="button"
              onClick={() => setPresetMenuOpen((v) => !v)}
              className="flex items-center gap-1 font-medium text-content hover:text-content-secondary transition-colors"
            >
              {presetLabel}
              <ChevronDown size={14} />
            </button>
            {presetMenuOpen && (
              <div className="absolute left-0 mt-1 w-40 bg-surface-elevated border border-border rounded-lg shadow-lg z-10 py-1">
                {(Object.keys(PRESET_LABEL) as StackingPreset[]).map((p) => (
                  <button
                    key={p}
                    type="button"
                    onClick={() => applyPreset(p)}
                    className="w-full text-left px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover"
                  >
                    {PRESET_LABEL[p]}
                  </button>
                ))}
              </div>
            )}
          </div>
          <span className="flex items-center gap-1.5">
            <FolderOpen size={14} className="text-content-muted" />
            {plan.workingDir ?? 'Choose a working folder'}
          </span>
          <span className="flex items-center gap-1.5">
            <FolderOpen size={14} className="text-content-muted" />
            {plan.outputDir ?? 'Choose an output folder'}
          </span>
          <span className="text-content-muted">
            {freeLabel} · estimate {formatGB(plan.estimateBytes)}
          </span>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void handleRun()}
            disabled={runDisabled}
            title={runTooltip}
            className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors ${
              runDisabled
                ? 'bg-surface text-content-muted cursor-not-allowed'
                : 'bg-accent text-surface hover:bg-accent-hover'
            }`}
          >
            {starting ? <Loader2 size={14} className="animate-spin" /> : <Play size={14} />}
            Run stacking
          </button>

          {running && (
            <button
              type="button"
              onClick={() => void handleCancel()}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium border border-border text-content-secondary hover:bg-surface-hover transition-colors"
            >
              <Square size={14} />
              Cancel
            </button>
          )}

          <div className="relative" ref={rerunMenuRef}>
            <button
              type="button"
              onClick={() => setRerunMenuOpen((v) => !v)}
              disabled={rerunDisabled}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium border transition-colors ${
                rerunDisabled
                  ? 'border-border text-content-muted cursor-not-allowed'
                  : 'border-border text-content-secondary hover:bg-surface-hover'
              }`}
            >
              Re-run from
              <ChevronDown size={14} />
            </button>
            {rerunMenuOpen && !rerunDisabled && (
              <div className="absolute right-0 mt-1 w-48 bg-surface-elevated border border-border rounded-lg shadow-lg z-10 py-1">
                {rerunOptions.map((stage) => (
                  <button
                    key={stage}
                    type="button"
                    onClick={() => void handleRerunFrom(stage)}
                    className="w-full text-left px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover"
                  >
                    {STAGE_LABEL[stage]}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Blockers */}
      {plan.blockers.length > 0 && (
        <div className="space-y-1">
          {plan.blockers.map((b, i) => (
            <div key={`${b.code}-${i}`} className="flex items-center gap-2 text-sm text-error">
              <AlertTriangle size={14} className="shrink-0" />
              <span>{b.message}</span>
              {(b.code === 'masters' || b.code === 'links' || b.code === 'masterFiles') && (
                <button
                  type="button"
                  className="underline hover:no-underline text-content-secondary"
                  onClick={handleCoverageClick}
                >
                  → Coverage
                </button>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Warnings */}
      {plan.warnings.length > 0 && (
        <div className="space-y-1">
          {plan.warnings.map((w, i) => (
            <p key={i} className="text-sm text-warning">{w}</p>
          ))}
        </div>
      )}

      {/* Board (62%) / Inspector (38%) */}
      <div className="flex flex-col lg:flex-row gap-4">
        <div className="lg:w-[62%] min-w-0 space-y-3">
          <PipelineBoard
            plan={plan}
            config={draftConfig}
            progress={runProgress}
            outcome={runOutcome}
            selectedStage={selectedStage}
            onSelectStage={handleSelectStage}
            onToggleWriteRegisteredFrames={handleToggleWriteRegisteredFrames}
          />
          <div className="bg-surface-elevated rounded-lg p-3">
            <h4 className="text-sm font-medium text-content mb-2">Groups ({plan.groups.length})</h4>
            <GroupsTable groups={plan.groups} />
          </div>
        </div>

        <div className="lg:w-[38%] min-w-0">
          <StageInspector
            stage={selectedStage}
            config={draftConfig}
            onChange={handleConfigChange}
            plan={plan}
            disabled={running || starting}
            presetDefault={presets.default}
            onRemeasure={handleRemeasure}
            remeasureDisabled={remeasureDisabled}
          />
        </div>
      </div>
    </div>
  );
}
