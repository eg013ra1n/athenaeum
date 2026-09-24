import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { FolderOpen, FolderOutput, Loader2, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import ProjectExportDialog from './ProjectExportDialog';
import { formatGb } from './format';
import type { CollabReplicationPaused, LossAction, ProjectFrameView } from '../../types/models';

/**
 * Receive tab — the per-frame replication surface for send_receive /
 * coordinator members (visibility decided by the parent from `card.dataRole`).
 * Lists non-own frames grouped by publisher; every on-disk badge is read
 * straight off the stored row (`onDisk`/`awaitingGc`/`locallyDeclined` — S6,
 * never optimistic). There is no per-frame download any more — replication is
 * project-wide, so the one action here is "Sync now" (`sync_project_now`). A
 * loss-guard pause (P14) shows inline with Restore / Stop keeping
 * (`resolve_collab_loss`).
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
  const [paused, setPaused] = useState<CollabReplicationPaused | null>(null);
  const [lossBusy, setLossBusy] = useState(false);

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
  // async `listen`, never an awaited unlisten.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabReplicationPaused>('collab-replication-paused', (p) => {
        if (cancelled || p.projectId !== projectId) return;
        setPaused(p);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[receive] replication-paused listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [projectId]);

  const syncNow = async () => {
    setSyncing(true);
    setError(null);
    try {
      await api.invoke('sync_project_now', { projectId });
      reload();
    } catch (err) {
      // S6 — a failed sync surfaces inline, never silently swallowed.
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[receive] sync_project_now failed:', err);
      setError(msg);
    } finally {
      setSyncing(false);
    }
  };

  const resolveLoss = async (action: LossAction) => {
    setLossBusy(true);
    setError(null);
    try {
      await api.invoke('resolve_collab_loss', { projectId, action });
      setPaused(null);
      reload();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[receive] resolve_collab_loss failed:', err);
      setError(msg);
    } finally {
      setLossBusy(false);
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
            title="Download every published contribution this device is missing"
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

      {paused && (
        <div className="flex flex-wrap items-center gap-2 rounded border border-error/40 bg-error/10 px-3 py-2 text-sm text-content-secondary">
          <span>
            Replication paused: {paused.missing} frames missing ({formatGb(paused.missingBytes)})
          </span>
          <span className="ml-auto flex items-center gap-2">
            <button
              type="button"
              onClick={() => void resolveLoss('restore')}
              disabled={lossBusy}
              className="rounded border border-border px-2 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
            >
              Restore
            </button>
            <button
              type="button"
              onClick={() => void resolveLoss('stopHolding')}
              disabled={lossBusy}
              className="rounded border border-error/50 px-2 py-1 text-xs text-error transition-colors hover:bg-error/10 disabled:opacity-50"
            >
              Stop keeping
            </button>
          </span>
        </div>
      )}

      {error && <p className="text-sm text-error">{error}</p>}

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
                      <th className="font-normal">On disk</th>
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
                        <td className="pr-3 text-content-secondary">{f.holderCount}</td>
                        <td>
                          <OnDiskBadge frame={f} />
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

/** On-disk state, driven entirely by the stored row (S6, never optimistic). */
function OnDiskBadge({ frame }: { frame: ProjectFrameView }) {
  if (frame.locallyDeclined) return <span className="text-content-muted">Not kept</span>;
  if (frame.awaitingGc) return <span className="text-warning">Waiting for cleanup</span>;
  if (frame.onDisk)
    return (
      <span className="inline-flex items-center gap-1 text-success">
        <FolderOpen size={11} /> On disk
      </span>
    );
  return <span className="text-content-muted">Not on disk</span>;
}
