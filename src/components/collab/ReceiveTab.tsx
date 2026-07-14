import { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { Download, FolderOpen, FolderOutput, Loader2, RotateCw } from 'lucide-react';
import { api } from '../../api';
import { formatTimestamp } from '../../utils/dateFormatting';
import { formatBytes } from './format';
import ProjectExportDialog from './ProjectExportDialog';
import type { ProjectPackageView } from '../../types/models';

const POLL_MS = 3_000;

/**
 * Receive tab — the swarm-download surface for send_receive / coordinator
 * members (visibility is decided by the parent from `card.dataRole`). Lists
 * non-own announced packages; every chip derives from the stored `localStatus`
 * (S6 — never optimistic). A `download_collab_package` spawn returns instantly;
 * the terminal state arrives via `localStatus` on a re-list, so this tab polls
 * `reload` while anything is still downloading.
 */
export default function ReceiveTab({
  projectId,
  projectTitle,
  packages,
  reload,
}: {
  projectId: string;
  projectTitle?: string;
  packages: ProjectPackageView[] | null;
  reload: () => void;
}) {
  // `undefined` = still loading the folder setting; `null` = unset (banner).
  const [collabDir, setCollabDir] = useState<string | null | undefined>(undefined);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [exportOpen, setExportOpen] = useState(false);
  const reloadRef = useRef(reload);
  reloadRef.current = reload;

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

  const others = (packages ?? []).filter((p) => !p.own);
  const anyDownloading = others.some((p) => p.localStatus === 'downloading') || busy.size > 0;

  // Poll the stored package list while a download is in flight so the chip
  // settles from `downloading` → `complete`/`failed` without a manual refresh.
  useEffect(() => {
    if (!anyDownloading) return;
    const timer = setInterval(() => reloadRef.current(), POLL_MS);
    return () => clearInterval(timer);
  }, [anyDownloading]);

  // `busy` bridges the gap between the download command returning (it only
  // spawns) and the backend flipping `local_status` off `none`. Clear a
  // package from `busy` once its STORED status is observed to have moved, so
  // the spinner + polling are driven by real rows (S6), never guesswork.
  useEffect(() => {
    if (busy.size === 0 || packages === null) return;
    const settled = packages.filter((p) => busy.has(p.packageId) && p.localStatus !== 'none');
    if (settled.length === 0) return;
    setBusy((prev) => {
      const next = new Set(prev);
      for (const p of settled) next.delete(p.packageId);
      return next;
    });
  }, [packages, busy]);

  const startDownload = useCallback(
    async (packageId: string) => {
      setError(null);
      setBusy((prev) => new Set(prev).add(packageId));
      try {
        await api.invoke('download_collab_package', { projectId, packageId });
        // Re-list to pick up `downloading` promptly; the effect above releases
        // `busy` once the stored status confirms the transition.
        reloadRef.current();
      } catch (err) {
        // S6 — a failed hub/download start is surfaced, never silently caught.
        const msg = err instanceof Error ? err.message : String(err);
        console.error('[receive] download_collab_package failed:', err);
        setError(msg);
        setBusy((prev) => {
          const next = new Set(prev);
          next.delete(packageId);
          return next;
        });
      }
    },
    [projectId],
  );

  const dirUnset = collabDir === null;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm font-medium text-content">Received contributions</span>
        <button
          type="button"
          onClick={() => setExportOpen(true)}
          className="inline-flex items-center gap-1.5 rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover"
          title="Organize the project's frames into a PixInsight WBPP folder tree"
        >
          <FolderOutput size={14} /> Export for WBPP
        </button>
      </div>

      {dirUnset && (
        <div className="flex flex-wrap items-center gap-2 rounded border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-content-secondary">
          <FolderOpen size={14} className="shrink-0 text-warning" />
          <span>Set a Collaboration folder first — downloads land there.</span>
          <Link
            to="/files"
            className="ml-auto rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover"
          >
            Open File Manager
          </Link>
        </div>
      )}

      {error && <p className="text-sm text-error">{error}</p>}

      {packages === null ? (
        <p className="text-sm text-content-muted">Loading…</p>
      ) : others.length === 0 ? (
        <p className="text-sm text-content-muted">
          No packages announced yet — published contributions from other members appear here.
        </p>
      ) : (
        <ul className="space-y-2">
          {others.map((p) => (
            <li
              key={p.packageId}
              className={`rounded border border-border px-3 py-2 text-sm ${
                p.superseded ? 'opacity-50' : ''
              }`}
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium text-content">{p.publisher}</span>
                <span className="text-xs text-content-muted">
                  {p.frameCount} frames · {formatBytes(p.byteSize)}
                </span>
                {p.superseded && (
                  <span className="rounded bg-surface-hover px-1.5 py-0.5 text-[10px] text-content-muted">
                    superseded
                  </span>
                )}
                <span className="ml-auto">
                  <ReceiveAction
                    pkg={p}
                    busy={busy.has(p.packageId)}
                    disabledDir={dirUnset}
                    collabDir={collabDir ?? null}
                    onDownload={() => void startDownload(p.packageId)}
                  />
                </span>
              </div>
              <p className="mt-0.5 text-[11px] text-content-muted">
                held by {p.holderCount} ({p.onlineCount} online) · {formatTimestamp(p.createdAt)}
              </p>
            </li>
          ))}
        </ul>
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

/** Right-aligned per-package action, driven entirely by the stored localStatus. */
function ReceiveAction({
  pkg,
  busy,
  disabledDir,
  collabDir,
  onDownload,
}: {
  pkg: ProjectPackageView;
  busy: boolean;
  disabledDir: boolean;
  collabDir: string | null;
  onDownload: () => void;
}) {
  if (pkg.localStatus === 'downloading' || busy) {
    return (
      <span className="inline-flex items-center gap-1 text-xs text-accent">
        <Loader2 size={12} className="animate-spin" /> Downloading…
      </span>
    );
  }
  if (pkg.localStatus === 'complete') {
    return (
      <span
        className="inline-flex items-center gap-1 text-xs text-success"
        title={collabDir ? `Landed in ${collabDir}` : undefined}
      >
        <FolderOpen size={12} /> Downloaded
      </span>
    );
  }
  if (pkg.localStatus === 'failed') {
    return (
      <button
        type="button"
        onClick={onDownload}
        disabled={disabledDir}
        className="inline-flex items-center gap-1 rounded border border-error/50 px-2 py-1 text-xs text-error transition-colors hover:bg-error/10 disabled:cursor-not-allowed disabled:opacity-50"
        title={disabledDir ? 'Set a Collaboration folder first' : 'Download failed — retry'}
      >
        <RotateCw size={12} /> Retry
      </button>
    );
  }
  // localStatus === 'none' → available
  const noHolders = pkg.holderCount === 0;
  const disabled = disabledDir || noHolders;
  const title = disabledDir
    ? 'Set a Collaboration folder first'
    : noHolders
      ? 'No online holders'
      : undefined;
  return (
    <button
      type="button"
      onClick={onDownload}
      disabled={disabled}
      className="inline-flex items-center gap-1 rounded bg-accent px-2 py-1 text-xs text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
      title={title}
    >
      <Download size={12} /> Download
    </button>
  );
}
