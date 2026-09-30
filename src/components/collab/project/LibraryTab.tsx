import { useEffect, useMemo, useRef, useState, type JSX } from 'react';
import { Link } from 'react-router-dom';
import { FolderOpen, FolderOutput } from 'lucide-react';
import { api } from '../../../api';
import { useCollabExchange } from '../../../contexts/CollabExchangeContext';
import type { ExchangeState } from '../exchange/state';
import { Button } from '../../ui';
import ProjectExportDialog from '../ProjectExportDialog';
import CollabAttention from '../CollabAttention';
import ExcludeDialog from './ExcludeDialog';
import ProjectFrameTable, { type TableAction } from './table/ProjectFrameTable';
import { fromLibrary, type FrameVM } from './frames';
import type {
  CollabAttentionChanged,
  CollabFramesLanded,
  ProjectFrameView,
} from '../../../types/models';

/**
 * Library tab — every published project frame, mine and other members',
 * and its state on this device (v3 §8.1 "Lights": the project's frames are
 * what Export for WBPP organizes, own and replica alike). One `ProjectFrameTable`
 * grouped by publisher; the Collaboration-folder banner, `Export for WBPP`,
 * `CollabAttention` and the per-project reload listeners moved here verbatim
 * from the retired four-tab page's receive tab.
 *
 * The device column (`fromLibrary`) reads this device's replication state
 * off the stored `localState` (never optimistic, S6); `downloading` and its
 * percent come from the live exchange's in-flight list for this project,
 * keyed by frame uuid.
 */
export default function LibraryTab({
  projectId,
  projectTitle,
  frames,
  error,
  reload,
  canModerate,
  onOpen,
  activeKey = null,
}: {
  projectId: string;
  projectTitle: string;
  frames: ProjectFrameView[] | null;
  error: boolean;
  reload: () => void;
  canModerate: boolean;
  onOpen: (vm: FrameVM) => void;
  /** The frame whose side panel is open — its row takes the active state. */
  activeKey?: string | null;
}): JSX.Element {
  const { state } = useCollabExchange();

  // `undefined` = still loading the folder setting; `null` = unset (banner).
  const [collabDir, setCollabDir] = useState<string | null | undefined>(undefined);
  const [exportOpen, setExportOpen] = useState(false);
  const [excluding, setExcluding] = useState<FrameVM[] | null>(null);
  const [keepBusy, setKeepBusy] = useState(false);
  const [keepError, setKeepError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<string | null>('get_collaboration_dir')
      .then((d) => {
        if (!cancelled) setCollabDir(d ?? null);
      })
      .catch((err) => {
        console.error('[library] get_collaboration_dir failed:', err);
        if (!cancelled) setCollabDir(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // StrictMode-safe listener pattern (CLAUDE.md); `reload` is held in a
  // latest-value ref so this effect subscribes exactly once (deps `[projectId]`
  // only) — Task 10's listener ruling: an inline callback from the parent (a
  // new function identity every render) must never force a re-subscribe, or
  // the unsubscribe/resubscribe gap can drop an event fired in between.
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  useEffect(() => {
    let cancelled = false;
    const unlistens: (() => void)[] = [];
    const register = <T extends { projectId: string }>(event: string) =>
      api
        .listen<T>(event, (p) => {
          if (cancelled || p.projectId !== projectId) return;
          reloadRef.current();
        })
        .then((fn) => {
          if (cancelled) fn();
          else unlistens.push(fn);
        })
        .catch((err) => console.error(`[library] ${event} listen failed:`, err));
    void register<CollabAttentionChanged>('collab-attention-changed');
    void register<CollabFramesLanded>('collab-frames-landed');
    return () => {
      cancelled = true;
      unlistens.forEach((fn) => fn());
    };
  }, [projectId]);

  // This project's receiving flows' in-flight items, keyed by frame uuid —
  // `fromLibrary`'s only source for `downloading` + its percent.
  const inFlight = useMemo(() => libraryInFlight(state.projects, projectId), [state.projects, projectId]);

  // Every project frame, mine included; a frame still pending moderation is
  // not part of the project yet (Moderation tab / My frames).
  const libraryFrames = useMemo(
    () => (frames ?? []).filter((f) => f.state !== 'pending'),
    [frames],
  );
  const rows = useMemo(() => libraryFrames.map((f) => fromLibrary(f, inFlight)), [libraryFrames, inFlight]);

  const handleKeepAgain = async (frameUuids: string[]): Promise<void> => {
    setKeepBusy(true);
    setKeepError(null);
    try {
      await api.invoke('keep_collab_frames_again', { projectId, frameUuids });
      reload();
    } catch (err) {
      console.error('[library] keep_collab_frames_again failed:', err);
      setKeepError(err instanceof Error ? err.message : String(err));
    } finally {
      setKeepBusy(false);
    }
  };

  const actions: TableAction[] = [
    {
      id: 'keep',
      verb: 'Keep again',
      eligible: (v) => v.device === 'notKept',
      busy: keepBusy,
      run: (targets) => {
        const frameUuids = targets.map((v) => v.frameUuid).filter((id): id is string => id !== null);
        void handleKeepAgain(frameUuids);
      },
    },
    ...(canModerate
      ? [
          {
            id: 'exclude',
            verb: 'Exclude',
            eligible: (v: FrameVM) => !v.excluded && v.pubState === 'published',
            run: (targets: FrameVM[]) => setExcluding(targets),
          } satisfies TableAction,
        ]
      : []),
  ];

  const dirUnset = collabDir === null;

  return (
    <div className="space-y-3">
      {dirUnset && (
        <div className="flex items-center gap-2 rounded border border-warning/40 bg-warning-muted px-2.5 py-2 text-[12px] text-content-secondary">
          <FolderOpen size={14} className="shrink-0 text-warning" />
          <span>Set a Collaboration folder first — synced frames land there.</span>
          <Link to="/files" className="ml-auto">
            <Button size="sm">Open File Manager</Button>
          </Link>
        </div>
      )}

      <CollabAttention projectId={projectId} />

      {error && <p className="text-[12.5px] text-error">Could not load the library — see console.</p>}
      {keepError && <p className="text-[12.5px] text-error">{keepError}</p>}

      {frames === null ? (
        <p className="text-sm text-content-muted">Loading…</p>
      ) : (
        <ProjectFrameTable
          key={`${projectId}.library`}
          tableId="library"
          scope={projectId}
          rows={rows}
          actions={actions}
          onOpen={onOpen}
          activeKey={activeKey}
          groupRowExtra={
            <Button
              onClick={() => setExportOpen(true)}
              title="Organize the project's frames into a PixInsight WBPP folder tree"
            >
              <FolderOutput size={12} />
              Export for WBPP
            </Button>
          }
          emptyText="No frames in this project yet — published contributions appear here."
        />
      )}

      {exportOpen && (
        <ProjectExportDialog
          projectId={projectId}
          projectTitle={projectTitle}
          onClose={() => setExportOpen(false)}
        />
      )}

      {excluding && (
        <ExcludeDialog
          projectId={projectId}
          frames={excluding}
          onClose={() => setExcluding(null)}
          onDone={() => reload()}
        />
      )}
    </div>
  );
}

/** This project's receiving flows' in-flight items, keyed by frame uuid —
 * the ONE builder of the map `fromLibrary` reads for `downloading` + its
 * percent; the tab and the page's Library badge (`libraryToCome`) both use
 * it, so the two can never disagree. */
export function libraryInFlight(
  projects: ExchangeState['projects'],
  projectId: string,
): Map<string, { done: number; size: number }> {
  const map = new Map<string, { done: number; size: number }>();
  const recv = projects[projectId]?.recv ?? [];
  for (const flow of recv) {
    for (const item of flow.inFlight) {
      map.set(item.frameUuid, { done: item.done, size: item.size });
    }
  }
  return map;
}

/** The Library tab's badge count: frames whose device state (`fromLibrary`)
 * is `downloading`, `queued` or `missing` — the ones still "to come" on this
 * device. `notKept` and `have` don't count (spec mockup: "N to go"). Pending
 * frames are skipped as in the table; my own frames are skipped too — they
 * are never fetched, so none of them is "to come". */
export function libraryToCome(frames: ProjectFrameView[] | null, inFlight: ReadonlyMap<string, unknown>): number {
  if (!frames) return 0;
  const typedInFlight = inFlight as ReadonlyMap<string, { done: number; size: number }>;
  let count = 0;
  for (const f of frames) {
    if (f.own || f.state === 'pending') continue;
    const device = fromLibrary(f, typedInFlight).device;
    if (device === 'downloading' || device === 'queued' || device === 'missing') count += 1;
  }
  return count;
}
