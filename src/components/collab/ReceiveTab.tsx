import { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { FolderOpen, FolderOutput, Loader2, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import ProjectExportDialog from './ProjectExportDialog';
import CollabAttention from './CollabAttention';
import type {
  CollabAttentionChanged,
  CollabFramesLanded,
  ProjectFrameView,
} from '../../types/models';

/**
 * Receive tab — the per-frame replication surface for send_receive /
 * coordinator members (visibility decided by the parent from `card.dataRole`).
 * Lists non-own frames grouped by publisher; every state badge is read
 * straight off the stored row (`localState`, spec §9.4 — S6, never
 * optimistic) and the holders column counts the live holders
 * (`holdersOnline` / `holdersTotal`). The live exchange fetches on events;
 * "Sync now" is the one global command (`collab_sync_now`, L10). The files
 * that need the user — changed, waiting for a choice, not kept, other files —
 * sit above the list (`CollabAttention`). The list re-reads when this
 * project's attention changes or frames land.
 */
export default function ReceiveTab({
  projectId,
  projectTitle,
  frames,
  reload,
}: {
  projectId: string;
  projectTitle?: string;
  frames: ProjectFrameView[] | null;
  reload: () => void;
}) {
  // `undefined` = still loading the folder setting; `null` = unset (banner).
  const [collabDir, setCollabDir] = useState<string | null | undefined>(undefined);
  const [syncing, setSyncing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [exportOpen, setExportOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<string | null>('get_collaboration_dir')
      .then((d) => {
        if (!cancelled) setCollabDir(d ?? null);
      })
      .catch((err) => {
        console.error('[receive] get_collaboration_dir failed:', err);
        if (!cancelled) setCollabDir(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // StrictMode-safe listener pattern (CLAUDE.md): a cancelled flag guards the
  // async `listen`, never an awaited unlisten. A local-state change of this
  // project (a choice, a quarantine, a landing) re-reads the frame list.
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
        .catch((err) => console.error(`[receive] ${event} listen failed:`, err));
    void register<CollabAttentionChanged>('collab-attention-changed');
    void register<CollabFramesLanded>('collab-frames-landed');
    return () => {
      cancelled = true;
      unlistens.forEach((fn) => fn());
    };
  }, [projectId]);

  const syncNow = async () => {
    setSyncing(true);
    setError(null);
    try {
      await api.invoke('collab_sync_now');
      reload();
    } catch (err) {
      // S6 — a failed sync surfaces inline, never silently swallowed.
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[receive] collab_sync_now failed:', err);
      setError(msg);
    } finally {
      setSyncing(false);
    }
  };

  const dirUnset = collabDir === null;
  const others = (frames ?? []).filter((f) => !f.own);
  const groups = groupByPublisher(others);

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm font-medium text-content">Received frames</span>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void syncNow()}
            disabled={syncing}
            className="inline-flex items-center gap-1.5 rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
            title="Reconnect to the hub and check every project at once"
          >
            {syncing ? (
              <Loader2 size={14} className="animate-spin" />
            ) : (
              <RefreshCw size={14} />
            )}
            Sync now
          </button>
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

      {error && <p className="text-sm text-error">{error}</p>}

      <CollabAttention projectId={projectId} />

      {frames === null ? (
        <p className="text-sm text-content-muted">Loading…</p>
      ) : others.length === 0 ? (
        <p className="text-sm text-content-muted">
          No frames from other members yet — published contributions appear here.
        </p>
      ) : (
        <div className="space-y-4">
          {groups.map(([publisher, rows]) => (
            <div key={publisher}>
              <p className="mb-1 text-xs font-medium text-content-secondary">{publisher}</p>
              <div className="overflow-x-auto">
                <table className="w-full text-left text-xs">
                  <thead className="text-content-muted">
                    <tr>
                      <th className="py-1 pr-3 font-normal">File</th>
                      <th className="pr-3 font-normal">Filter</th>
                      <th className="pr-3 font-normal">Exposure</th>
                      <th className="pr-3 font-normal">Holders</th>
                      <th className="font-normal">State</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((f) => (
                      <tr key={f.frameUuid} className="border-t border-border/50">
                        <td
                          className="max-w-[16rem] truncate py-1 pr-3 text-content"
                          title={f.fileName}
                        >
                          {f.fileName}
                        </td>
                        <td className="pr-3 text-content-secondary">{f.filter}</td>
                        <td className="pr-3 text-content-secondary">
                          {f.exptimeSec.toFixed(1)}s
                        </td>
                        <td
                          className="pr-3 text-content-secondary"
                          title="Other members holding the current version: online now / in total"
                        >
                          {f.holdersOnline} online / {f.holdersTotal}
                        </td>
                        <td>
                          <LocalStateBadge frame={f} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          ))}
        </div>
      )}

      {exportOpen && (
        <ProjectExportDialog
          projectId={projectId}
          projectTitle={projectTitle}
          onClose={() => setExportOpen(false)}
        />
      )}
    </div>
  );
}

function groupByPublisher(frames: ProjectFrameView[]): [string, ProjectFrameView[]][] {
  const map = new Map<string, ProjectFrameView[]>();
  for (const f of frames) {
    const list = map.get(f.publisher);
    if (list) list.push(f);
    else map.set(f.publisher, [f]);
  }
  return Array.from(map.entries());
}

/** This device's state for the frame (spec §9.4), driven entirely by the
 *  stored row (S6, never optimistic). */
function LocalStateBadge({ frame }: { frame: ProjectFrameView }) {
  switch (frame.localState) {
    case 'held':
      return (
        <span className="inline-flex items-center gap-1 text-success">
          <FolderOpen size={11} /> On disk
        </span>
      );
    case 'wanted':
      return frame.waitingForPublisher ? (
        <span className="text-content-muted">v{frame.contentVersion} waiting for the publisher</span>
      ) : (
        <span className="text-content-muted">Waiting</span>
      );
    case 'missing':
      return <span className="text-warning">Missing</span>;
    case 'awaiting_choice':
      return <span className="text-warning">Waiting for your choice</span>;
    case 'quarantined':
      return (
        <span className="text-warning">
          <span>Changed</span>
          {frame.newVersionWaiting && <span className="text-content-muted"> · new version waiting</span>}
        </span>
      );
    case 'not_kept':
      return <span className="text-content-muted">Not kept</span>;
    case 'idle':
      return <span className="text-content-muted">Not replicated</span>;
    // Own frames (v3 R17) are listed on the Contribute tab; named here too so
    // the mapping is total.
    case 'own_held':
      return <span className="text-success">Published · on disk</span>;
    case 'own_missing':
      return <span className="text-warning">Published · not on disk</span>;
    case 'own_changed':
      return <span className="text-warning">Published · file changed</span>;
  }
}
