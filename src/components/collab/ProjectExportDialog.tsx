import { useCallback, useEffect, useState } from 'react';
import { Folder, FolderOutput, Loader2, X } from 'lucide-react';
import { api, type UnlistenFn } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { FolderBrowserModal } from '../FolderBrowserModal';
import type { ExportProgressEvent, ExportResult } from '../../types/export';

/** The Д3 sentinel `frame_set_id` the project export runner emits under — the
 *  same key the existing `cancel_export` command cancels. */
const PROJECT_EXPORT_SENTINEL = -1;

/** Dialog-local snapshot of the running export. Because each publisher subtree
 *  is organized in its own pass, the emitted percent restarts per publisher
 *  (Д2) — `publisher` is a running subtree counter derived from `current`
 *  resets, shown alongside the bar instead of a fake monotonic total. */
interface DialogProgress {
  phase: string; // 'collecting' | 'copying'
  current: number;
  total: number;
  percent: number;
  currentFile: string | null;
  publisher: number;
}

/**
 * Project-scoped WBPP export dialog (slice 5 "processor payoff"). Organizes the
 * project's received contributions ∪ own calibrated outputs into a WBPP folder
 * tree — one subtree per publisher (Д2) — via `export_collab_project`.
 *
 * Progress is dialog-local: the project export never registers with the global
 * export indicator (that hook only tracks its own `export_to_wbpp` starts), so
 * this listens for `export-progress` filtered on the `-1` sentinel and drives a
 * per-publisher bar; the busy state comes from the invoke promise. Cancel reuses
 * the existing `cancel_export` command with `frameSetId: -1`.
 *
 * Empty-state honesty: no pre-flight readiness check — the dialog always opens,
 * and a project with nothing exportable surfaces the backend's error inline
 * (the collector is the single source of truth).
 */
export default function ProjectExportDialog({
  projectId,
  projectTitle,
  onClose,
}: {
  projectId: string;
  projectTitle?: string;
  onClose: () => void;
}) {
  const [outputDir, setOutputDir] = useState('');
  const [useSymlinks, setUseSymlinks] = useState(false);
  const [showFolderBrowser, setShowFolderBrowser] = useState(false);
  const [busy, setBusy] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [progress, setProgress] = useState<DialogProgress | null>(null);
  const [result, setResult] = useState<ExportResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Web mode: pull the server-configured export directory once (desktop uses a
  // native picker instead).
  useEffect(() => {
    if (isTauri) return;
    api
      .invoke<string | null>('get_export_dir', {})
      .then((dir) => {
        if (dir) setOutputDir(dir);
      })
      .catch((err) => console.error('[ProjectExportDialog] get_export_dir failed:', err));
  }, []);

  // Dialog-local progress. StrictMode-safe cancelled-flag listener; filtered on
  // the project export sentinel so a concurrent frame-set export never bleeds in.
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    api
      .listen<ExportProgressEvent>('export-progress', (payload) => {
        if (cancelled) return;
        if (payload.frameSetId !== PROJECT_EXPORT_SENTINEL) return;
        setProgress((prev) => {
          let publisher = prev?.publisher ?? 0;
          if (payload.phase === 'copying') {
            // A new publisher subtree starts either from the first copy event or
            // whenever `current` drops below the previous (it is monotonic within
            // a single subtree's organize pass).
            const startedNew = !prev || prev.phase !== 'copying' || payload.current < prev.current;
            if (startedNew) publisher += 1;
          }
          return {
            phase: payload.phase,
            current: payload.current,
            total: payload.total,
            percent: payload.percent,
            currentFile: payload.currentFile,
            publisher,
          };
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[ProjectExportDialog] export-progress listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const handleSelectFolder = useCallback(async () => {
    if (!isTauri) {
      setShowFolderBrowser(true);
      return;
    }
    const selected = await pickDirectory();
    if (selected && typeof selected === 'string') {
      setOutputDir(selected);
    }
  }, []);

  const handleExport = useCallback(async () => {
    if (!outputDir) return;
    setError(null);
    setResult(null);
    setProgress(null);
    setCancelling(false);
    setBusy(true);
    try {
      const res = await api.invoke<ExportResult>('export_collab_project', {
        projectId,
        outputDir,
        useSymlinks,
      });
      if (res.success) {
        setResult(res);
      } else {
        // Cancellation and per-publisher aborts arrive as success:false + error.
        setError(res.error ?? 'Export did not complete.');
      }
    } catch (err) {
      // Never swallow — a thrown invoke (e.g. "nothing to export for this
      // project") surfaces inline.
      console.error('[ProjectExportDialog] export_collab_project failed:', err);
      setError(typeof err === 'string' ? err : (err as Error)?.message ?? String(err));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  }, [outputDir, useSymlinks, projectId]);

  const handleCancel = useCallback(async () => {
    setCancelling(true);
    try {
      await api.invoke('cancel_export', { frameSetId: PROJECT_EXPORT_SENTINEL });
    } catch (err) {
      console.error('[ProjectExportDialog] cancel_export failed:', err);
    }
  }, []);

  // Symlink toggle eligibility — Tauri on macOS / Linux only (mirror ExportTab).
  const isWindows = typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows');
  const symlinksAvailable = isTauri && !isWindows;
  const symlinkUnavailableReason = !isTauri
    ? 'Files will be copied (web mode always copies; symbolic links are not supported in the Docker build).'
    : isWindows
      ? 'Files will be copied (symbolic links are only available on macOS and Linux).'
      : null;

  const canExport = outputDir !== '' && !busy;
  const barPercent = progress ? Math.min(100, Math.max(0, progress.percent)) : 0;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={() => !busy && onClose()}
    >
      <div
        className="w-[32rem] max-w-[90vw] rounded-lg border border-border bg-surface p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center gap-2">
          <FolderOutput size={16} className="text-accent" />
          <h2 className="flex-1 truncate font-medium text-content">
            Export {projectTitle ? `“${projectTitle}”` : 'project'} for WBPP
          </h2>
          <button
            type="button"
            onClick={() => !busy && onClose()}
            disabled={busy}
            className="text-content-muted transition-colors hover:text-content disabled:opacity-40"
            title="Close"
          >
            <X size={16} />
          </button>
        </div>

        <p className="mb-3 text-xs text-content-muted">
          Organizes every contributor’s frames into a PixInsight WBPP folder tree — one subtree per
          publisher under the project title.
        </p>

        {/* Output directory */}
        <label htmlFor="collab-export-dir" className="mb-1 block text-sm text-content-muted">
          Output Directory
        </label>
        <div className="flex gap-2">
          <input
            id="collab-export-dir"
            type="text"
            value={outputDir}
            readOnly
            placeholder="Select output folder…"
            title={outputDir || undefined}
            className="flex-1 truncate rounded-lg border border-border bg-surface-hover px-3 py-2 text-content placeholder-content-muted"
          />
          <button
            type="button"
            onClick={() => void handleSelectFolder()}
            disabled={busy}
            title="Pick the destination folder"
            className={`flex items-center gap-2 rounded-lg px-4 py-2 transition-colors disabled:opacity-50 ${
              outputDir
                ? 'border border-border bg-surface-hover hover:brightness-110'
                : 'border border-accent/40 bg-accent/10 text-accent hover:bg-accent/20'
            }`}
          >
            <Folder size={16} />
            Browse
          </button>
        </div>

        {/* Symlinks toggle (macOS / Linux Tauri only) — hidden states explained. */}
        <div className="mt-3">
          {symlinksAvailable ? (
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={useSymlinks}
                disabled={busy}
                onChange={(e) => setUseSymlinks(e.target.checked)}
                className="h-4 w-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
              />
              <span className="text-content-secondary">Use symbolic links instead of copying files</span>
            </label>
          ) : symlinkUnavailableReason ? (
            <p className="text-xs text-content-muted">{symlinkUnavailableReason}</p>
          ) : null}
        </div>

        {/* Progress — dialog-local, per publisher (the bar restarts each subtree). */}
        {busy && (
          <div className="mt-4 space-y-1.5">
            <div className="flex items-center justify-between text-xs text-content-secondary">
              <span>
                {progress?.phase === 'copying'
                  ? `Copying — publisher ${progress.publisher} · file ${progress.current} of ${progress.total}`
                  : 'Collecting files…'}
              </span>
              {progress?.phase === 'copying' && <span>{Math.round(barPercent)}%</span>}
            </div>
            <div className="h-2 overflow-hidden rounded-full bg-surface-hover">
              <div
                className="h-full rounded-full bg-accent transition-[width] duration-150"
                style={{ width: `${barPercent}%` }}
              />
            </div>
            {progress?.currentFile && (
              <p className="truncate text-[11px] text-content-muted" title={progress.currentFile}>
                {progress.currentFile}
              </p>
            )}
          </div>
        )}

        {/* Success */}
        {result && result.success && (
          <div className="mt-4 rounded-lg border border-success/30 bg-success/10 p-3 text-sm">
            <p className="font-medium text-success">
              Export complete — {result.filesOrganized}{' '}
              {result.filesOrganized === 1 ? 'file' : 'files'} organized
            </p>
            <p className="mt-0.5 break-all text-xs text-content-muted">{result.outputDir}</p>
            {result.warnings.length > 0 && (
              <p className="mt-1 text-xs text-warning">
                {result.warnings.length} warning{result.warnings.length === 1 ? '' : 's'} — see the
                notification history.
              </p>
            )}
          </div>
        )}

        {/* Inline error (bad path, nothing to export, cancelled, per-publisher abort). */}
        {error && <p className="mt-4 text-sm text-error">{error}</p>}

        {/* Actions */}
        <div className="mt-4 flex justify-end gap-2">
          {busy ? (
            <button
              type="button"
              onClick={() => void handleCancel()}
              disabled={cancelling}
              className="inline-flex items-center gap-1.5 rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
            >
              {cancelling && <Loader2 size={12} className="animate-spin" />}
              {cancelling ? 'Cancelling…' : 'Cancel'}
            </button>
          ) : (
            <>
              <button
                type="button"
                onClick={onClose}
                className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover"
              >
                Close
              </button>
              <button
                type="button"
                onClick={() => void handleExport()}
                disabled={!canExport}
                title={outputDir ? undefined : 'Select an output folder first'}
                className="inline-flex items-center gap-1.5 rounded bg-accent px-4 py-1.5 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                <FolderOutput size={14} /> Export
              </button>
            </>
          )}
        </div>
      </div>

      {/* Web mode: folder browser for the export directory. */}
      <FolderBrowserModal
        isOpen={showFolderBrowser}
        scope="export"
        onSelect={(path) => {
          setOutputDir(path);
          setShowFolderBrowser(false);
        }}
        onClose={() => setShowFolderBrowser(false)}
      />
    </div>
  );
}
