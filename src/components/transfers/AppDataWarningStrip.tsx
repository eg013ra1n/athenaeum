import { useCallback, useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AlertTriangle, FolderOpen, X } from 'lucide-react';
import { api } from '../../api';
import { useTransfers } from '../../contexts/TransfersContext';

/**
 * Standing warning strip on `/transfers` (audit UX-1). When the receiver has
 * ever received something but no Sync Incoming Folder is designated, received
 * frames fall back to the app's data folder — the "where did my files go"
 * cliff. Unlike the one-shot toast in `useSyncStatus`, this stays visible until
 * the user either designates a folder or dismisses it for the session.
 *
 * The incoming-folder config (`get_sync_incoming_dir` → the designated path or
 * `null`) is re-read on mount and after every `sync-finished`, so the strip
 * clears itself the moment a folder is chosen. Dismissal is session-only (held
 * in `TransfersContext`, NOT localStorage) so it comes back on the next launch.
 */
export function AppDataWarningStrip() {
  const { status, appDataWarningDismissed, dismissAppDataWarning } = useTransfers();
  const navigate = useNavigate();
  // `undefined` = not yet read (don't flash the strip before the first resolve);
  // `null` = read resolved and no folder configured; string = configured.
  const [incomingDir, setIncomingDir] = useState<string | null | undefined>(undefined);

  const checkIncoming = useCallback(() => {
    api
      .invoke<string | null>('get_sync_incoming_dir')
      .then(setIncomingDir)
      .catch((err) =>
        console.error('[AppDataWarningStrip] get_sync_incoming_dir failed:', err),
      );
  }, []);

  // Initial read + re-check after each sync-finished (cheap invoke).
  // StrictMode-safe cancelled-flag listener per CLAUDE.md.
  useEffect(() => {
    checkIncoming();
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<unknown>('sync-finished', () => {
        if (!cancelled) checkIncoming();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[AppDataWarningStrip] sync-finished listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [checkIncoming]);

  const everReceived =
    (status?.receiver.receivedTotal ?? 0) > 0 || (status?.receiver.active.length ?? 0) > 0;
  const unconfigured = incomingDir === null;

  if (appDataWarningDismissed || !everReceived || !unconfigured) return null;

  return (
    <div className="mb-3 flex shrink-0 items-start gap-3 rounded-lg border border-warning/40 bg-warning/10 px-4 py-3 text-sm">
      <AlertTriangle size={18} className="mt-0.5 shrink-0 text-warning" />
      <div className="min-w-0 flex-1">
        <p className="font-medium text-content">
          Received files are going to the app&apos;s data folder
        </p>
        <p className="mt-0.5 text-content-muted">
          Set a Sync Incoming Folder so new frames land in your library.
        </p>
      </div>
      <button
        type="button"
        onClick={() => navigate('/files', { state: { focusSyncIncoming: true } })}
        className="inline-flex shrink-0 items-center gap-1.5 rounded bg-accent px-3 py-1.5 text-xs font-medium text-surface transition-colors hover:bg-accent-hover"
      >
        <FolderOpen size={14} />
        Choose folder…
      </button>
      <button
        type="button"
        onClick={dismissAppDataWarning}
        aria-label="Dismiss"
        title="Dismiss until next launch"
        className="shrink-0 rounded p-1 text-content-muted transition-colors hover:bg-warning/20 hover:text-content"
      >
        <X size={16} />
      </button>
    </div>
  );
}
