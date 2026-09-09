import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useNavigate } from 'react-router-dom';
import { Play, Square, ChevronDown, ChevronRight, FolderOpen, AlertTriangle, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useStackingContext } from '../../contexts/StackingContext';
import { useNotifications } from '../../contexts/NotificationContext';
import type {
  Stage,
  StackingConfig,
  StackingPlan,
  StackingPreset,
  StackingPresets,
  StackingRunDetail,
  StackingSetConfig,
} from '../../types/stacking';
import { PipelineBoard } from './PipelineBoard';
import { GroupsTable } from './GroupsTable';
import { StageInspector } from './StageInspector';
import { FramesTable, type LightFrameRef } from './FramesTable';
import { ResultsPanel } from './ResultsPanel';
import { stableStringify, withoutPaths, type BoardStage } from './stageSummary';
import {
  readSelectedStage,
  writeSelectedStage,
  readFramesCollapsed,
  writeFramesCollapsed,
  readInspectorCollapsed,
  writeInspectorCollapsed,
} from './stackingPrefs';

export interface StackingTabProps {
  framesSetId: number;
  frameSetName?: string;
  /** The set's LIGHT frames (Task 4, Decisions item 3) — `FrameSetDetail.tsx`
   *  derives this from its own `detail.nights` tree, the same source every
   *  other tab on this page reads. Used by the Frames table both before any
   *  run exists (the only frame list available) and after one, as the
   *  filename fallback for a row a run's summary hasn't reached yet. */
  lightFrames: LightFrameRef[];
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
export function StackingTab({ framesSetId, lightFrames }: StackingTabProps) {
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
  const [framesCollapsed, setFramesCollapsed] = useState<boolean>(readFramesCollapsed);
  // Task 5, Decisions item 3: below the `lg` breakpoint the inspector moves
  // under the board as its own collapsible disclosure (the board/inspector
  // side-by-side split no longer has the width for both) — same
  // collapsed/expanded convention as the Frames table above, persisted the
  // same way.
  const [inspectorCollapsed, setInspectorCollapsed] = useState<boolean>(readInspectorCollapsed);
  // Lifted up from `ResultsPanel` (a sibling of `FramesTable`, not its
  // parent) so the Frames table can join its rows against the Results
  // panel's currently-selected run without either component reaching into
  // the other directly.
  const [selectedRunDetail, setSelectedRunDetail] = useState<StackingRunDetail | null>(null);

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

  // Fix round 1 (Critical #1/#2, Important #3): which set `draftConfig`
  // belongs to, and whether its most recent change was USER-originated
  // (an inspector edit / preset apply / toggle) as opposed to the load
  // effect's own seed/reset. `dirtyRef` is set ONLY by `setUserConfig`
  // below — never by the load effect — and cleared only once a write for
  // it is actually sent (by the persist effect's timer, or by the flush
  // effect on a set switch/unmount), not merely scheduled, so a pending
  // edit survives long enough to be flushed instead of silently lost.
  const dirtyRef = useRef(false);
  const draftForSetRef = useRef<number | null>(null);

  /** The only way `draftConfig` should change as a result of a USER action.
   *  The load effect calls `setDraftConfig` directly (bypassing this) so it
   *  can never mark the draft dirty — visiting a set with no stored
   *  override must never materialize one (Critical #1). */
  const setUserConfig = useCallback<typeof setDraftConfig>((value) => {
    dirtyRef.current = true;
    setDraftConfig(value);
  }, []);

  /** The exclusion-list analogue of `setUserConfig` above — Ruling 7's ONE
   *  frame-level write. The load effect calls `setExcludedFrameIds`
   *  directly (bypassing this), so opening a set never marks the draft
   *  dirty on its own; only the Frames table's include checkbox, through
   *  `handleToggleExcludeFrame` below, goes through here. */
  const setUserExcludedFrameIds = useCallback<typeof setExcludedFrameIds>((value) => {
    dirtyRef.current = true;
    setExcludedFrameIds(value);
  }, []);

  const handleToggleExcludeFrame = useCallback((frameId: number) => {
    setUserExcludedFrameIds((prev) =>
      prev.includes(frameId) ? prev.filter((id) => id !== frameId) : [...prev, frameId],
    );
  }, [setUserExcludedFrameIds]);

  const handleToggleFramesCollapsed = useCallback(() => {
    setFramesCollapsed((prev) => {
      const next = !prev;
      writeFramesCollapsed(next);
      return next;
    });
  }, []);

  const handleToggleInspectorCollapsed = useCallback(() => {
    setInspectorCollapsed((prev) => {
      const next = !prev;
      writeInspectorCollapsed(next);
      return next;
    });
  }, []);

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
  // from it. Fix round 1, Critical #2: `draftConfig`/`excludedFrameIds` are
  // reset to null/[] FIRST (synchronously, before the async fetch even
  // starts) so nothing on screen — and nothing the persist effect could act
  // on — still points at the PREVIOUS set while this fetch is in flight;
  // `draftForSetRef` is set only once the config response actually lands,
  // guarded by this effect's own `cancelled` flag (a fresh effect run for a
  // newer `framesSetId` already flips the older run's `cancelled` to `true`
  // before its `await` can resume — the standard React guard, sufficient
  // here since only this one effect ever calls `get_stacking_config`).
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    setDraftConfig(null);
    setExcludedFrameIds([]);
    (async () => {
      try {
        const cfg = await api.invoke<StackingSetConfig>('get_stacking_config', { setId: framesSetId });
        if (cancelled) return;
        setDraftConfig(cfg.config);
        setExcludedFrameIds(cfg.excludedFrameIds);
        draftForSetRef.current = framesSetId;
        const presetsResult = await api.invoke<StackingPresets>('get_stacking_presets', {});
        if (cancelled) return;
        setPresets(presetsResult);
        // This plan fetch is one of four call sites that can write `plan`
        // (the other three go through `refetchPlan`) — guarded by the same
        // `planSeqRef`/`framesSetIdRef` those share, so whichever response
        // is actually the most recent wins regardless of which code path
        // issued it (a `library-updated` event or the outcome effect can
        // race against this very fetch).
        const seq = ++planSeqRef.current;
        const forSetId = framesSetId;
        const p = await api.invoke<StackingPlan>('get_stacking_plan', {
          setId: forSetId,
          config: cfg.config,
        });
        if (cancelled) return;
        if (seq !== planSeqRef.current || forSetId !== framesSetIdRef.current) {
          return; // superseded by a later refetchPlan/mount call
        }
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

  // The most recent user-originated write still waiting to go out — set by
  // the persist effect below when it schedules the 500 ms timer, read by
  // both that timer and the flush effect (Important #3), cleared by
  // whichever of them actually sends it.
  const pendingWriteRef = useRef<{ setId: number; config: StackingConfig; excludedFrameIds: number[] } | null>(null);

  const sendPendingWrite = useCallback((notifyOnFailure: boolean) => {
    const payload = pendingWriteRef.current;
    if (!payload) return;
    dirtyRef.current = false;
    pendingWriteRef.current = null;
    api.invoke('set_stacking_config', payload)
      .then(() => {
        // `build_plan` computes `includedCount` (and everything else the
        // excluded-frame list affects) from the STORED row, never a client
        // override (`crates/athenaeum-core/src/stacking/plan.rs`) — a
        // config edit alone already re-plans via the 300 ms draft-change
        // effect using a client override, but the Frames table's include
        // checkbox only ever changes `excludedFrameIds`, which that effect
        // does not watch. Re-plan once the write that actually changed the
        // stored row has landed (Task 4 brief, Decisions item 2).
        void refetchPlan(payload.config);
      })
      .catch((err) => {
        console.error('[StackingTab] set_stacking_config failed:', err);
        if (notifyOnFailure) {
          notify({
            tone: 'warning',
            kind: 'stacking',
            toast: true,
            title: 'Stacking settings not saved',
            detail: String(err),
          });
        }
      });
  }, [notify, refetchPlan]);

  // Persist (debounced) whenever the draft config or the excluded-frame list
  // changes. Fix round 1, Critical #1: writes only when `dirtyRef.current`
  // is true — the load effect's own seed/reset never sets it, so opening
  // the tab on a set with no stored override can never materialize one.
  // Critical #2: also skipped when `draftForSetRef` still names a
  // DIFFERENT set than the current `framesSetId` — the one-commit window
  // between a set switch and the new set's load effect resetting the
  // draft, where this effect would otherwise still see the OLD set's
  // draftConfig alongside the NEW framesSetId and write the wrong set.
  // Submit state, never a re-read (spec §11.2): a failed write logs and
  // warns but never rolls the draft back or re-fetches.
  useEffect(() => {
    if (!draftConfig) return;
    if (!dirtyRef.current) return;
    if (draftForSetRef.current !== framesSetId) return;
    pendingWriteRef.current = { setId: framesSetId, config: draftConfig, excludedFrameIds };
    const t = setTimeout(() => sendPendingWrite(true), 500);
    return () => clearTimeout(t);
  }, [draftConfig, excludedFrameIds, framesSetId, sendPendingWrite]);

  // Flush instead of cancel (Important #3): a set switch or unmount must
  // not silently drop an edit the 500 ms timer above hadn't reached yet.
  // This effect's own cleanup — which fires exactly when `framesSetId` is
  // about to change, or on true unmount — sends whatever the persist
  // effect last scheduled, fire-and-forget (no `notify`; the tab the user
  // is leaving isn't the place to toast a failure).
  useEffect(() => {
    return () => {
      if (dirtyRef.current && pendingWriteRef.current) {
        sendPendingWrite(false);
      }
    };
  }, [framesSetId, sendPendingWrite]);

  const handleConfigChange = useCallback((next: StackingConfig) => {
    setUserConfig(next);
  }, [setUserConfig]);

  const handleToggleWriteRegisteredFrames = useCallback((checked: boolean) => {
    setUserConfig((prev) =>
      prev ? { ...prev, registration: { ...prev.registration, writeRegisteredFrames: checked } } : prev,
    );
  }, [setUserConfig]);

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
    setUserConfig((prev) => {
      if (!presets || !prev) return prev;
      return { ...presets[preset], paths: prev.paths };
    });
    setPresetMenuOpen(false);
  }, [presets, setUserConfig]);

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
  // Fix round 1, Minor #8: the preset selector edits the config the same
  // way every inspector panel does, so it is disabled while a run is
  // active for the same reason those panels are (`StageInspector`'s own
  // `disabled` prop below).
  const presetSelectorDisabled = running || starting;
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
              disabled={presetSelectorDisabled}
              className={`flex items-center gap-1 font-medium transition-colors ${
                presetSelectorDisabled
                  ? 'text-content-muted cursor-not-allowed'
                  : 'text-content hover:text-content-secondary'
              }`}
            >
              {presetLabel}
              <ChevronDown size={14} />
            </button>
            {presetMenuOpen && !presetSelectorDisabled && (
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

        <div className="lg:w-[38%] min-w-0 space-y-2">
          {/* Below `lg` only: a summary button naming the selected stage,
           *  toggling the panel below it — `lg` and up hides this button
           *  entirely and the panel is always shown (Task 2/3's original
           *  side-by-side shape, unchanged). One `StageInspector` mount
           *  either way, so a panel with its own fetch on mount (Output's
           *  `get_stacking_paths`) never runs twice. */}
          <button
            type="button"
            onClick={handleToggleInspectorCollapsed}
            aria-expanded={!inspectorCollapsed}
            className="lg:hidden w-full flex items-center gap-1.5 px-4 py-2.5 rounded-lg bg-surface-elevated text-sm font-medium text-content hover:text-content-secondary transition-colors"
          >
            {inspectorCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
            {STAGE_LABEL[selectedStage]}
          </button>
          <div className={`${inspectorCollapsed ? 'hidden' : 'block'} lg:block`}>
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

      {/* Frames + Results — full width, below the board/inspector split
       *  (the Frames table's nine columns need the room; the Results
       *  panel's master cards read better at full width too). */}
      <FramesTable
        lightFrames={lightFrames}
        runDetail={selectedRunDetail}
        excludedFrameIds={excludedFrameIds}
        onToggleExclude={handleToggleExcludeFrame}
        collapsed={framesCollapsed}
        onToggleCollapsed={handleToggleFramesCollapsed}
      />

      <ResultsPanel
        setId={framesSetId}
        running={running}
        onSelectedRunDetailChange={setSelectedRunDetail}
      />
    </div>
  );
}
