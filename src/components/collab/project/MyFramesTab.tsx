import { useEffect, useRef, useState, type JSX, type ReactNode } from 'react';
import { useNavigate } from 'react-router-dom';
import { Loader2, Plus, RefreshCw } from 'lucide-react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import type { AnalysisCompleteEvent } from '../../../types/helpers';
import type { LinkedSetView, OwnFrameRow } from '../../../types/models';
import AutoPublishSwitch from '../AutoPublishSwitch';
import FilterMappingDialog from '../FilterMappingDialog';
import LinkObjectDialog from '../LinkObjectDialog';
import ExcludeDialog from './ExcludeDialog';
import ReasonGroupAction from './ReasonGroupAction';
import ProjectFrameTable, { type TableAction } from './table/ProjectFrameTable';
import type { GroupNode } from './table/model';
import { fromOwn, type FrameVM } from './frames';

export type Segment = 'ready' | 'published' | 'held';

export interface MyFramesTabProps {
  projectId: string;
  rows: OwnFrameRow[] | null; // null = loading
  error: boolean;
  links: LinkedSetView[];
  autoPublish: boolean;
  segment: Segment;
  onSegment: (s: Segment) => void;
  onReload: () => void; // re-read list_project_own_frames
  onDetailReload: () => void; // re-read get_collab_project_detail (links, card)
  onRequestPublish: (frameIds: number[]) => void; // the shell confirms, then publishes
  publishBusy: boolean;
  onRequestRepublish: (frameIds: number[] | null) => void; // null = "all"; the shell's guard dialog confirms
  republishBusy: boolean;
  canRepublish: boolean;
  coordinator: boolean;
  republishError: string | null;
  refusal: ReactNode; // the shell's publishing-device refusal box, or null
  onOpen: (vm: FrameVM) => void;
}

const SEG_BTN = 'rounded border px-3 py-1.5 text-sm transition-colors';
const SEG_BTN_ON = 'border-accent bg-accent/10 text-content';
const SEG_BTN_OFF = 'border-border text-content-secondary hover:bg-surface-hover';
const OUTLINE_BTN =
  'inline-flex items-center gap-1.5 rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50';

/**
 * My frames — the three Ready/Published/Held back tables of the redesigned
 * collab project page (Task 10, spec 2026-09-30 §"My frames"). Carries the
 * solve/analyze orchestration moved verbatim from the old `ProjectDetail`
 * page, and Held back's per-Reason fix buttons (`ReasonGroupAction`), which
 * work from each group's own rows — the scope ruling that drops
 * `evaluate_collab_gate` from this page (the frame set's Project block
 * still uses the gate; untouched here).
 */
export default function MyFramesTab({
  projectId, rows, error, links, autoPublish, segment, onSegment, onReload, onDetailReload,
  onRequestPublish, publishBusy, onRequestRepublish, republishBusy, canRepublish, coordinator,
  republishError, refusal, onOpen,
}: MyFramesTabProps): JSX.Element {
  const navigate = useNavigate();
  const { notify } = useNotifications();

  const [linkOpen, setLinkOpen] = useState(false);
  const [mapOpen, setMapOpen] = useState(false);
  const [excluding, setExcluding] = useState<FrameVM[] | null>(null);
  const [solveBusy, setSolveBusy] = useState(false);
  const [analyzeBusy, setAnalyzeBusy] = useState<Set<number>>(new Set());
  // `plate-solve-complete` is a global event — plate solving can be kicked
  // off from other pages too. Only clear `solveBusy` when THIS tab is the
  // one that started it, or an unrelated solve elsewhere would wrongly mark
  // this tab's batch done.
  const solveStartedHereRef = useRef(false);

  // Fix round 1, finding 2: a latest-value ref, not a dep, so the two
  // listener effects below subscribe exactly ONCE for the life of the tab.
  // An inline `onReload` from the parent (a new function identity every
  // render) would otherwise re-subscribe on every render; each
  // unsubscribe → resubscribe pair opens an async gap in which a
  // `plate-solve-complete` fired in between is missed entirely, leaving
  // `solveBusy` stuck forever.
  const onReloadRef = useRef(onReload);
  onReloadRef.current = onReload;

  // StrictMode-safe listener pattern (CLAUDE.md) — moved verbatim from
  // ProjectDetail, `loadGate()` replaced by `onReloadRef.current()`. Deps
  // `[]`: subscribed once, reads the latest `onReload` through the ref.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<AnalysisCompleteEvent>('analysis-complete', (payload) => {
        if (cancelled) return;
        // Clear only the SET that just finished — a still-running analyze on
        // another set must keep its Analyze control disabled.
        setAnalyzeBusy((s) => {
          const next = new Set(s);
          next.delete(payload.frame_set_id);
          return next;
        });
        onReloadRef.current();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[my-frames] analysis-complete listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen('plate-solve-complete', () => {
        if (cancelled) return;
        if (!solveStartedHereRef.current) return;
        solveStartedHereRef.current = false;
        setSolveBusy(false);
        onReloadRef.current();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[my-frames] plate-solve-complete listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const handleSolve = async (frameIds: number[]): Promise<void> => {
    setSolveBusy(true);
    solveStartedHereRef.current = true;
    try {
      await api.invoke('plate_solve_batch', { frameIds });
    } catch (err) {
      console.error('[my-frames] solve failed:', err);
      solveStartedHereRef.current = false;
      setSolveBusy(false);
      const msg = err instanceof Error ? err.message : String(err);
      notify({
        title: 'Could not start the solve',
        detail: msg,
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
        link: `/projects/${projectId}`,
        dedupeKey: `solve-failed-${projectId}-${Date.now()}`,
      });
    }
  };

  const handleAnalyze = async (setId: number): Promise<void> => {
    setAnalyzeBusy((s) => new Set(s).add(setId));
    try {
      await api.invoke('analyze_frame_set', { frameSetId: setId });
    } catch (err) {
      console.error('[my-frames] analyze failed:', err);
      setAnalyzeBusy((s) => {
        const n = new Set(s);
        n.delete(setId);
        return n;
      });
      const msg = err instanceof Error ? err.message : String(err);
      notify({
        title: 'Could not start the analysis',
        detail: msg,
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
        link: `/projects/${projectId}`,
        dedupeKey: `analyze-failed-${projectId}-${Date.now()}`,
      });
    }
  };

  const onOpenCalibration = (setId: number): void => {
    navigate(`/objects/${setId}?tab=calibration`);
  };

  const raw = rows ?? [];
  const readyRows: FrameVM[] = raw.filter((r) => r.segment === 'ready').map(fromOwn);
  const publishedRows: FrameVM[] = raw.filter((r) => r.segment === 'published').map(fromOwn);
  const heldRows: FrameVM[] = raw.filter((r) => r.segment === 'held').map(fromOwn);

  const readyActions: TableAction[] = [
    {
      id: 'publish',
      verb: 'Publish',
      eligible: () => true,
      primary: true,
      busy: publishBusy,
      run: (targets) => onRequestPublish(targets.map((v) => v.frameId!)),
    },
  ];
  const readyEmptyText =
    links.length === 0
      ? 'Link an object to start.'
      : 'Nothing ready to publish — new frames appear here once they pass the gate.';

  const heldActions: TableAction[] = [
    {
      id: 'solve',
      verb: 'Solve',
      eligible: (v) => v.failures.some((f) => f.kind === 'solve'),
      busy: solveBusy,
      run: (targets) => void handleSolve(targets.map((v) => v.frameId!)),
    },
    {
      id: 'analyze',
      verb: 'Analyze',
      // Fix round 1, finding 1: a set already running is ineligible, not
      // just "already counted" — `analyze_frame_set` is awaited for the
      // whole run and the backend refuses a second one on the same set
      // (`Conflict`). Without this, the group's own Analyze button and this
      // toolbar action can race: the second call's catch would delete
      // `setId` from `analyzeBusy` while the first run is still going,
      // re-enabling the group's button and raising a false failure toast.
      // Excluding busy sets here needs no separate `busy:` field — the
      // button naturally reads `N of M` and disables at zero.
      eligible: (v) => v.setId !== null && !analyzeBusy.has(v.setId) && v.failures.some((f) => f.kind === 'analyze'),
      run: (targets) => {
        const setIds = new Set(targets.map((v) => v.setId).filter((id): id is number => id !== null));
        for (const setId of setIds) void handleAnalyze(setId);
      },
    },
  ];

  const publishedActions: TableAction[] = [
    {
      id: 'republish',
      verb: 'Republish',
      eligible: (v) => !v.excluded,
      busy: republishBusy,
      run: (targets) => onRequestRepublish(targets.map((v) => v.frameId!)),
    },
    ...(coordinator
      ? [
          {
            id: 'exclude',
            verb: 'Exclude',
            eligible: (v: FrameVM) => !v.excluded && v.pubState === 'published' && v.frameUuid !== null,
            run: (targets: FrameVM[]) => setExcluding(targets),
          } satisfies TableAction,
        ]
      : []),
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-sm font-medium text-content">Linked objects</span>
        {links.length > 0 && (
          <ul className="flex flex-wrap gap-2">
            {links.map((l) => (
              <li key={l.framesSetId} className="rounded border border-border px-2 py-1 text-xs text-content-secondary">
                <span className="break-words">{l.name ?? `Set #${l.framesSetId}`}</span> · {l.lightCount} lights
                {l.withinRadius ? ' · on target' : ''}
              </li>
            ))}
          </ul>
        )}
        <button
          type="button"
          onClick={() => setLinkOpen(true)}
          className="inline-flex items-center gap-1 rounded border border-border px-2 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover"
        >
          <Plus size={12} /> Link an object
        </button>
        <AutoPublishSwitch projectId={projectId} enabled={autoPublish} onToggled={onDetailReload} />
      </div>

      {error && <p className="text-sm text-error">Could not load your frames — see console.</p>}

      {rows === null ? (
        <p className="text-sm text-content-muted">Loading…</p>
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              aria-pressed={segment === 'ready'}
              onClick={() => onSegment('ready')}
              className={`${SEG_BTN} ${segment === 'ready' ? SEG_BTN_ON : SEG_BTN_OFF}`}
            >
              Ready to publish {readyRows.length}
            </button>
            <button
              type="button"
              aria-pressed={segment === 'published'}
              onClick={() => onSegment('published')}
              className={`${SEG_BTN} ${segment === 'published' ? SEG_BTN_ON : SEG_BTN_OFF}`}
            >
              Published {publishedRows.length}
            </button>
            <button
              type="button"
              aria-pressed={segment === 'held'}
              onClick={() => onSegment('held')}
              className={`${SEG_BTN} ${segment === 'held' ? SEG_BTN_ON : SEG_BTN_OFF}`}
            >
              Held back {heldRows.length}
            </button>
            <span className="flex-1" />
            {canRepublish && (
              <button
                type="button"
                onClick={() => onRequestRepublish(null)}
                disabled={republishBusy}
                className={OUTLINE_BTN}
                title="Regenerate every one of your published frames as a new content version"
              >
                {republishBusy && <Loader2 size={14} className="animate-spin" />}
                <RefreshCw size={14} /> Recalibrate and republish all
              </button>
            )}
          </div>

          {republishError && <p className="text-sm text-error">{republishError}</p>}
          {refusal}

          {segment === 'ready' && (
            <ProjectFrameTable
              key={`${projectId}.ready`}
              tableId="ready"
              scope={projectId}
              rows={readyRows}
              actions={readyActions}
              onOpen={onOpen}
              emptyText={readyEmptyText}
            />
          )}
          {segment === 'held' && (
            <ProjectFrameTable
              key={`${projectId}.held`}
              tableId="held"
              scope={projectId}
              rows={heldRows}
              actions={heldActions}
              onOpen={onOpen}
              emptyText="Nothing held back."
              groupAction={(node: GroupNode<FrameVM>) =>
                node.def.id === 'reason' ? (
                  <ReasonGroupAction
                    kind={node.key}
                    rows={node.rows}
                    solveBusy={solveBusy}
                    analyzeBusy={analyzeBusy}
                    onSolve={(ids) => void handleSolve(ids)}
                    onAnalyze={(setId) => void handleAnalyze(setId)}
                    onOpenCalibration={onOpenCalibration}
                    onMapFilters={() => setMapOpen(true)}
                  />
                ) : null
              }
            />
          )}
          {segment === 'published' && (
            <ProjectFrameTable
              key={`${projectId}.published`}
              tableId="published"
              scope={projectId}
              rows={publishedRows}
              actions={publishedActions}
              onOpen={onOpen}
              emptyText="Nothing published yet."
            />
          )}
        </>
      )}

      {linkOpen && (
        <LinkObjectDialog
          projectId={projectId}
          onClose={() => setLinkOpen(false)}
          onChanged={() => {
            onDetailReload();
            onReload();
          }}
        />
      )}

      {mapOpen && (
        <FilterMappingDialog
          projectId={projectId}
          onClose={() => setMapOpen(false)}
          onSaved={() => {
            setMapOpen(false);
            onReload();
          }}
        />
      )}

      {excluding && (
        <ExcludeDialog
          projectId={projectId}
          frames={excluding}
          onClose={() => setExcluding(null)}
          onDone={() => onReload()}
        />
      )}
    </div>
  );
}
