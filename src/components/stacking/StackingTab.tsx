import { useState, useEffect, useCallback, useRef } from 'react';
import { useNavigate } from 'react-router-dom';
import { Play, Square, ChevronDown, FolderOpen, AlertTriangle, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useStackingContext } from '../../contexts/StackingContext';
import { useNotifications } from '../../contexts/NotificationContext';
import type { Stage, StackingConfig, StackingPlan, StackingSetConfig } from '../../types/stacking';
import { PipelineBoard } from './PipelineBoard';
import { GroupsTable } from './GroupsTable';
import type { BoardStage } from './stageSummary';

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
export function StackingTab({ framesSetId, frameSetName }: StackingTabProps) {
  const navigate = useNavigate();
  const { notify } = useNotifications();
  const { progress, lastOutcome, startRun, cancelRun, isRunning } = useStackingContext();

  const [draftConfig, setDraftConfig] = useState<StackingConfig | null>(null);
  const [plan, setPlan] = useState<StackingPlan | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedStage, setSelectedStage] = useState<BoardStage>('calibrate');
  const [starting, setStarting] = useState(false);
  const [rerunMenuOpen, setRerunMenuOpen] = useState(false);

  const runProgress = progress.get(framesSetId);
  const runOutcome = lastOutcome.get(framesSetId);
  const running = isRunning(framesSetId) || plan?.activeRunId != null;

  const refetchPlan = useCallback(async (configOverride?: StackingConfig) => {
    try {
      const p = await api.invoke<StackingPlan>('get_stacking_plan', {
        setId: framesSetId,
        config: configOverride,
      });
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

  const handleToggleWriteRegisteredFrames = useCallback((checked: boolean) => {
    setDraftConfig((prev) =>
      prev ? { ...prev, registration: { ...prev.registration, writeRegisteredFrames: checked } } : prev,
    );
  }, []);

  const handleRun = useCallback(async () => {
    if (!plan || plan.blockers.length > 0 || running || starting) return;
    setStarting(true);
    try {
      await startRun(framesSetId, draftConfig ?? undefined);
    } catch (err) {
      console.error('[StackingTab] start_stacking failed:', err);
      notify({
        title: 'Failed to start stacking',
        detail: String(err),
        kind: 'stacking',
        hasErrors: true,
        tone: 'warning',
      });
    } finally {
      setStarting(false);
    }
  }, [plan, running, starting, startRun, framesSetId, draftConfig, notify]);

  const handleCancel = useCallback(async () => {
    const runId = runProgress?.runId ?? plan?.activeRunId ?? null;
    if (runId == null) return;
    try {
      await cancelRun(runId);
    } catch (err) {
      console.error('[StackingTab] cancel_stacking failed:', err);
    }
  }, [runProgress, plan, cancelRun]);

  const handleRerunFrom = useCallback(async (stage: Stage) => {
    setRerunMenuOpen(false);
    if (running || starting) return;
    setStarting(true);
    try {
      await startRun(framesSetId, draftConfig ?? undefined, stage);
    } catch (err) {
      console.error('[StackingTab] rerun start_stacking failed:', err);
      notify({
        title: 'Failed to start stacking',
        detail: String(err),
        kind: 'stacking',
        hasErrors: true,
        tone: 'warning',
      });
    } finally {
      setStarting(false);
    }
  }, [running, starting, startRun, framesSetId, draftConfig, notify]);

  const handleCoverageClick = useCallback(() => {
    const setId = plan && plan.readiness.rawSetsWithoutMaster > 0
      ? plan.readiness.rawSetIdsWithoutMaster[0]
      : undefined;
    if (setId !== undefined) {
      navigate(`?tab=calibration&highlightSet=${setId}`, { replace: true });
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

  if (loadError || !plan || !draftConfig) {
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
    <div className="space-y-3" title={frameSetName}>
      {/* Toolbar */}
      <div className="flex flex-wrap items-center justify-between gap-3 bg-surface-elevated rounded-lg px-4 py-3">
        <div className="flex flex-wrap items-center gap-4 text-sm text-content-secondary">
          <span className="font-medium text-content">Default</span>
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

          <div className="relative">
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
            onSelectStage={setSelectedStage}
            onToggleWriteRegisteredFrames={handleToggleWriteRegisteredFrames}
          />
          <div className="bg-surface-elevated rounded-lg p-3">
            <h4 className="text-sm font-medium text-content mb-2">Groups ({plan.groups.length})</h4>
            <GroupsTable groups={plan.groups} />
          </div>
        </div>

        <div className="lg:w-[38%] min-w-0">
          {/* Task 3 fills this in with `StageInspector`. */}
          <div className="bg-surface-elevated rounded-lg p-4 h-full">
            <p className="text-sm text-content-muted">{STAGE_LABEL[selectedStage]}</p>
          </div>
        </div>
      </div>
    </div>
  );
}
