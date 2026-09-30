import { useEffect, useMemo, useRef, useState, type JSX } from 'react';
import { Link } from 'react-router-dom';
import { FolderOpen, FolderOutput } from 'lucide-react';
import { api } from '../../../api';
import { useCollabExchange } from '../../../contexts/CollabExchangeContext';
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
 * Library tab — other members' published frames and their state on this
 * device (Task 11, spec 2026-09-30 "Library"). Replaces `ReceiveTab`'s
 * grouped-by-publisher table with `ProjectFrameTable`; the Collaboration-
 * folder banner, `Export for WBPP`, `CollabAttention` and the per-project
 * reload listeners move here VERBATIM from `ReceiveTab` — `ReceiveTab` itself
 * is untouched (Task 16 deletes it once the redesigned page is wired up).
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
  coordinator,
  onOpen,
}: {
  projectId: string;
  projectTitle: string;
  frames: ProjectFrameView[] | null;
  error: boolean;
  reload: () => void;
  coordinator: boolean;
  onOpen: (vm: FrameVM) => void;
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
  const inFlight = useMemo(() => {
    const map = new Map<string, { done: number; size: number }>();
    const recv = state.projects[projectId]?.recv ?? [];
    for (const flow of recv) {
      for (const item of flow.inFlight) {
        map.set(item.frameUuid, { done: item.done, size: item.size });
      }
    }
    return map;
  }, [state.projects, projectId]);

  // Own frames are the Contribute/My-frames tabs' business; a frame still
  // pending moderation belongs to the Moderation tab, not here.
  const libraryFrames = useMemo(
    () => (frames ?? []).filter((f) => !f.own && f.state !== 'pending'),
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
    ...(coordinator
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
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm font-medium text-content">Received frames</span>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => setExportOpen(true)}
            className="inline-flex items-center gap-1.5 rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover"
            title="Organize the project's frames into a PixInsight WBPP folder tree"
          >
            <FolderOutput size={14} /> Export for WBPP
          </button>
        </div>
      </div>

      {dirUnset && (
        <div className="flex flex-wrap items-center gap-2 rounded border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-content-secondary">
          <FolderOpen size={14} className="shrink-0 text-warning" />
          <span>Set a Collaboration folder first — synced frames land there.</span>
          <Link
            to="/files"
            className="ml-auto rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover"
          >
            Open File Manager
          </Link>
        </div>
      )}

      <CollabAttention projectId={projectId} />

      {error && <p className="text-sm text-error">Could not load the library — see console.</p>}
      {keepError && <p className="text-sm text-error">{keepError}</p>}

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
          emptyText="No frames from other members yet — published contributions appear here."
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

/** The Library tab's badge count: frames whose device state (`fromLibrary`)
 * is `downloading`, `queued` or `missing` — the ones still "to come" on this
 * device. `notKept` and `have` don't count (spec mockup: "N to go"). Applies
 * the same own/pending exclusion as the tab's own table, so the badge always
 * matches what the table would show. */
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
