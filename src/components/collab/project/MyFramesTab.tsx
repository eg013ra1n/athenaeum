import { useCallback, useEffect, useMemo, useRef, useState, type JSX, type ReactNode } from 'react';
import { useNavigate } from 'react-router-dom';
import { Loader2 } from 'lucide-react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import type { AnalysisCompleteEvent } from '../../../types/helpers';
import type { LinkedSetView, OwnFrameRow } from '../../../types/models';
import { Button, EmptyState, SegmentTiles } from '../../ui';
import { formatDurationPadded, formatSize } from '../format';
import FilterMappingDialog from '../FilterMappingDialog';
import LinkObjectDialog from '../LinkObjectDialog';
import ExcludeDialog from './ExcludeDialog';
import ProjectBlink from './ProjectBlink';
import { blinkRef, type BlinkTable } from './blinkEligibility';
import ReasonGroupAction from './ReasonGroupAction';
import ProjectFrameTable, { type TableAction } from './table/ProjectFrameTable';
import type { GroupNode } from './table/model';
import { fromOwn, ownFrameKey, type FrameVM } from './frames';
import { contributionTiles } from './contribution';
import PublishRunPanel from './PublishRunPanel';
import { useWithhold } from './useWithhold';
import type { PublishRunState } from './useCollabPublishRun';

export type Segment = 'ready' | 'review' | 'published' | 'held';

export interface MyFramesTabProps {
  projectId: string;
  rows: OwnFrameRow[] | null; // null = loading
  error: boolean;
  links: LinkedSetView[];
  segment: Segment;
  onSegment: (s: Segment) => void;
  onReload: () => void; // re-read list_project_own_frames
  onDetailReload: () => void; // re-read get_collab_project_detail (links, card)
  onRequestPublish: (frameIds: number[]) => void; // the shell confirms, then publishes
  publishBusy: boolean;
  onRequestRepublish: (frameIds: number[] | null) => void; // null = "all"; the shell's guard dialog confirms
  republishBusy: boolean;
  canRepublish: boolean;
  canModerate: boolean;
  republishError: string | null;
  refusal: ReactNode; // the shell's publishing-device refusal box, or null
  onOpen: (vm: FrameVM) => void;
  /** The frame whose side panel is open — its row takes the active state. */
  activeKey?: string | null;
  /** The project's publish run (the shell's one `useCollabPublishRun`). */
  run: PublishRunState;
  onCalibrate: (frameIds: number[]) => void;
  calibrateBusy: boolean;
  calibrateError: string | null;
  /** Plan F1: publish these already-published frames' update with no confirm. */
  onUpdate: (frameIds: number[]) => void;
}

const TILE_LABEL: Record<Segment, string> = {
  ready: 'Ready to calibrate',
  review: 'To review',
  published: 'Published',
  held: 'Held back',
};
const TILE_TONE = { ready: 'accent', review: 'purple', published: 'success', held: 'warning' } as const;

/**
 * My frames — the four segments of the collab project page, one table each:
 * Ready, To review, Published and Held back (spec 2026-10-01 §8.2), under the
 * publish run panel (§8.1). Carries the solve/analyze orchestration moved
 * verbatim from the old `ProjectDetail` page, and Held back's per-Reason fix
 * buttons (`ReasonGroupAction`), which work from each group's own rows — the
 * scope ruling that drops `evaluate_collab_gate` from this page (the frame
 * set's Project block still uses the gate; untouched here).
 */
export default function MyFramesTab({
  projectId, rows, error, links, segment, onSegment, onReload, onDetailReload,
  onRequestPublish, publishBusy, onRequestRepublish, republishBusy, canRepublish, canModerate,
  republishError, refusal, onOpen, activeKey = null, run, onCalibrate, calibrateBusy, calibrateError, onUpdate,
}: MyFramesTabProps): JSX.Element {
  const navigate = useNavigate();
  const { notify } = useNotifications();

  const withhold = useWithhold(projectId, onReload);
  const [linkOpen, setLinkOpen] = useState(false);
  const [mapOpen, setMapOpen] = useState(false);
  const [excluding, setExcluding] = useState<FrameVM[] | null>(null);
  // `open` counts the Blink clicks: `ProjectBlink` is keyed on it, so a click
  // while the previous selection still resolves (from this or another segment)
  // mounts a fresh one with the right table.
  const [blinking, setBlinking] = useState<{ table: BlinkTable; vms: FrameVM[]; open: number } | null>(null);
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

  // Memoized on `rows` alone: `ProjectPage` re-renders on every 1 Hz exchange
  // event, and without this each segment's table would re-derive its
  // whole `FrameVM[]` on every one of those renders even though `rows` (from
  // `list_project_own_frames`) hasn't changed.
  const readyRows: FrameVM[] = useMemo(
    () => (rows ?? []).filter((r) => r.segment === 'ready').map(fromOwn),
    [rows],
  );
  const reviewRows: FrameVM[] = useMemo(
    () => (rows ?? []).filter((r) => r.segment === 'review').map(fromOwn),
    [rows],
  );
  const tiles = useMemo(() => contributionTiles(rows ?? []), [rows]);
  const publishedRows: FrameVM[] = useMemo(
    () => (rows ?? []).filter((r) => r.segment === 'published').map(fromOwn),
    [rows],
  );
  const heldRows: FrameVM[] = useMemo(
    () => (rows ?? []).filter((r) => r.segment === 'held').map(fromOwn),
    [rows],
  );

  // Every segment, so an action that moves a frame (Don't publish → Held back)
  // still finds its fresh row while Blink stays open.
  const ownByKey = useMemo(
    () => new Map((rows ?? []).map((r) => [ownFrameKey(r), fromOwn(r)] as const)),
    [rows],
  );
  const lookup = useCallback((k: string) => ownByKey.get(k), [ownByKey]);
  const blinkAction = (table: BlinkTable): TableAction => ({
    id: 'blink',
    verb: 'Blink',
    eligible: (v) => blinkRef(v, table) !== null,
    run: (t) => setBlinking((b) => ({ table, vms: t, open: (b?.open ?? 0) + 1 })),
  });

  const asTargets = (vs: FrameVM[]) => vs.map((v) => ({ frameId: v.frameId!, prepared: v.own?.calibratedPath != null }));
  const readyActions: TableAction[] = [
    {
      id: 'calibrate',
      verb: 'Calibrate',
      eligible: () => true,
      primary: true,
      busy: calibrateBusy,
      run: (targets) => onCalibrate(targets.map((v) => v.frameId!)),
    },
    {
      id: 'withhold',
      verb: "Don't publish",
      eligible: () => true,
      busy: withhold.busy,
      run: (targets) => void withhold.dontPublish(asTargets(targets)),
    },
    blinkAction('ready'),
  ];
  const reviewActions: TableAction[] = [
    {
      id: 'publish',
      verb: 'Publish',
      eligible: () => true,
      primary: true,
      busy: publishBusy,
      run: (targets) => onRequestPublish(targets.map((v) => v.frameId!)),
    },
    {
      id: 'withhold',
      verb: "Don't publish",
      eligible: () => true,
      busy: withhold.busy,
      run: (targets) => void withhold.dontPublish(asTargets(targets)),
    },
    blinkAction('review'),
  ];
  const readyEmptyText =
    links.length === 0
      ? 'Link an object to start.'
      : 'Nothing ready — new frames appear here once they pass the gate.';

  const heldActions: TableAction[] = [
    {
      id: 'release',
      verb: 'Release',
      eligible: (v) => v.own?.withheld === true,
      primary: true,
      busy: withhold.busy,
      run: (targets) => void withhold.release(targets.map((v) => v.frameId!)),
    },
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
    blinkAction('held'),
  ];

  const publishedActions: TableAction[] = [
    {
      id: 'update',
      verb: 'Update',
      eligible: (v) => v.own?.contributorState === 'updatePending',
      busy: publishBusy,
      run: (targets) => onUpdate(targets.map((v) => v.frameId!)),
    },
    {
      id: 'republish',
      verb: 'Republish',
      eligible: (v) => !v.excluded,
      busy: republishBusy,
      run: (targets) => onRequestRepublish(targets.map((v) => v.frameId!)),
    },
    ...(canModerate
      ? [
          {
            id: 'exclude',
            verb: 'Exclude',
            needsSelection: true,
            eligible: (v: FrameVM) => !v.excluded && v.pubState === 'published' && v.frameUuid !== null,
            run: (targets: FrameVM[]) => setExcluding(targets),
          } satisfies TableAction,
        ]
      : []),
    blinkAction('published'),
  ];

  const summary = rows === null ? null : tiles.find((t) => t.segment === segment) ?? null;

  return (
    <div>
      {error && <p className="mb-2.5 text-[12.5px] text-error">Could not load your frames — see console.</p>}

      <PublishRunPanel run={run} />

      <div className="mb-2.5 flex flex-wrap items-center gap-2">
        {rows !== null && (
          <SegmentTiles
            tiles={tiles.map((t) => ({
              value: t.segment,
              n: t.count,
              label: TILE_LABEL[t.segment],
              tone: TILE_TONE[t.segment],
              sub: `${formatDurationPadded(t.seconds)} · ${t.nights} ${t.nights === 1 ? 'night' : 'nights'}`,
            }))}
            value={segment}
            onChange={onSegment}
          />
        )}
        <span className="flex-1" />
        <Button size="tile" onClick={() => setLinkOpen(true)}>+ Link an object</Button>
        <Button
          size="tile"
          onClick={() => onRequestRepublish(null)}
          disabled={!canRepublish || republishBusy}
          title="Regenerate every one of your published frames as a new content version"
        >
          {republishBusy && <Loader2 size={12} className="animate-spin" />}
          Recalibrate and republish all
        </Button>
      </div>

      {rows === null ? (
        <EmptyState>Loading…</EmptyState>
      ) : (
        <>
          {summary && (
            <p className="mb-2 text-[12px] text-content-faint">
              {`${summary.count} ${summary.count === 1 ? 'frame' : 'frames'} · ${formatDurationPadded(summary.seconds)} · ${formatSize(summary.bytes)}`}
            </p>
          )}
          {calibrateError && <p className="mb-2.5 text-[12.5px] text-error">{calibrateError}</p>}
          {republishError && <p className="mb-2.5 text-[12.5px] text-error">{republishError}</p>}
          {refusal}

          {segment === 'ready' && (
            <ProjectFrameTable
              key={`${projectId}.ready`}
              tableId="ready"
              scope={projectId}
              rows={readyRows}
              actions={readyActions}
              onOpen={onOpen}
              activeKey={activeKey}
              emptyText={readyEmptyText}
            />
          )}
          {segment === 'review' && (
            <ProjectFrameTable
              key={`${projectId}.review`}
              tableId="review"
              scope={projectId}
              rows={reviewRows}
              actions={reviewActions}
              onOpen={onOpen}
              activeKey={activeKey}
              emptyText="Nothing to review — calibrated frames wait here until you publish them."
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
              activeKey={activeKey}
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
              activeKey={activeKey}
              emptyText="Nothing published yet."
            />
          )}
        </>
      )}

      {withhold.dialog}

      {linkOpen && (
        <LinkObjectDialog
          projectId={projectId}
          links={links}
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

      {blinking && (
        <ProjectBlink
          key={blinking.open}
          projectId={projectId}
          table={blinking.table}
          vms={blinking.vms}
          lookup={lookup}
          canModerate={canModerate}
          onClose={() => setBlinking(null)}
          onChanged={onReload}
        />
      )}
    </div>
  );
}
