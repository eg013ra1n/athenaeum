import { useCallback, useEffect, useRef, useState } from 'react';
import { Folder } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useNotifications } from '../../contexts/NotificationContext';
import { Button, DialogShell, TextInput } from '../ui';
import { FolderBrowserModal } from '../FolderBrowserModal';
import type { UnpackOutcome, UnpackTarget } from '../../types/archive';
import { defaultUnpackDir, unpackResultLine, unpackSummary } from './unpackDialogModel';

interface ProgressPayload {
  current: number;
  total: number;
  message: string;
}

interface FinishedPayload {
  operation_id: number;
  outcome: string;
  kind?: string;
  unpacked?: number;
  adopted?: number;
  conflicts?: number;
}

/** One id per Unpack click. Every unpack finishes with `archive-finished {operation_id: 0, kind: "unpack"}`,
 *  so the notification is keyed on the click, never on the operation id (which would swallow all but the
 *  first unpack ever made). */
let unpackRunCounter = 0;

/**
 * "Unpack from zip": moves the frames of a set that live inside foreign zips out onto disk, keeping every
 * catalog row. The zips are not changed. There is no Cancel: the work runs to the end (frames already
 * unpacked stay unpacked either way), so the dialog cannot be closed while it runs.
 */
export function UnpackZipDialog({ target, frameCount, zipCount, defaultZipPath, onClose, onDone }: {
  target: UnpackTarget;
  /** Frames the owner can see are in zips (lights); calibration frames of the set are unpacked too. */
  frameCount: number;
  zipCount: number;
  /** The zip holding most of the frames; the default folder is named after it. */
  defaultZipPath: string | null;
  onClose: () => void;
  /** Called when the dialog closes after a run that moved at least one frame, so the page reloads its
   *  tables (reloading earlier would unmount the dialog before the owner reads the result). */
  onDone: () => void;
}) {
  const { notify } = useNotifications();
  const [destDir, setDestDir] = useState(() => (defaultZipPath ? defaultUnpackDir(defaultZipPath) : ''));
  const [showBrowser, setShowBrowser] = useState(false);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<ProgressPayload | null>(null);
  const [outcome, setOutcome] = useState<UnpackOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const runRef = useRef<number | null>(null);
  const notifiedRef = useRef<number | null>(null);
  const movedRef = useRef(false);
  const notifyRef = useRef(notify);
  notifyRef.current = notify;

  const notifyOnce = useCallback((run: number, summary: { title: string; detail?: string }, tone: 'success' | 'warning' | 'info', hasErrors: boolean) => {
    if (notifiedRef.current === run) return;
    notifiedRef.current = run;
    notifyRef.current({
      title: summary.title,
      detail: summary.detail ?? '',
      kind: 'archive',
      tone,
      hasErrors,
      dedupeKey: `zip-unpack-${run}`,
    });
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenFinished: (() => void) | undefined;
    api.listen<ProgressPayload>('zip-unpack-progress', (p) => {
      if (cancelled || runRef.current == null) return;
      setProgress(p);
    })
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[UnpackZipDialog] listen failed:', err));
    api.listen<FinishedPayload>('archive-finished', (p) => {
      if (cancelled || p.kind !== 'unpack') return;
      const run = runRef.current;
      if (run == null) return;
      const counts = { unpacked: p.unpacked ?? 0, adopted: p.adopted ?? 0, conflicts: p.conflicts ?? 0 };
      const failed = p.outcome === 'failed';
      notifyOnce(
        run,
        unpackSummary(counts, p.outcome),
        failed ? 'warning' : counts.conflicts > 0 ? 'warning' : 'success',
        failed,
      );
    })
      .then((fn) => { if (cancelled) fn(); else unlistenFinished = fn; })
      .catch((err) => console.error('[UnpackZipDialog] listen failed:', err));
    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenFinished?.();
    };
  }, [notifyOnce]);

  const browse = useCallback(async () => {
    if (!isTauri) {
      setShowBrowser(true);
      return;
    }
    try {
      const picked = await pickDirectory();
      if (picked) setDestDir(picked);
    } catch (err) {
      console.error('[UnpackZipDialog] folder picker failed:', err);
    }
  }, []);

  const start = useCallback(async () => {
    const dir = destDir.trim();
    if (!dir || busy) return;
    const run = ++unpackRunCounter;
    runRef.current = run;
    setBusy(true);
    setError(null);
    setOutcome(null);
    setProgress(null);
    try {
      const result = await api.invoke<UnpackOutcome>('unpack_zip_frames', { target, destDir: dir });
      setOutcome(result);
      // The finished event normally got here first; this covers a transport that drops it.
      notifyOnce(
        run,
        unpackSummary(
          { unpacked: result.unpacked, adopted: result.adopted, conflicts: result.conflicts.length },
          result.conflicts.length > 0 ? 'completed_with_conflicts' : 'completed',
        ),
        result.conflicts.length > 0 ? 'warning' : 'success',
        false,
      );
      if (result.unpacked + result.adopted > 0) movedRef.current = true;
    } catch (err) {
      const message = typeof err === 'string' ? err : (err as Error)?.message ?? String(err);
      console.error('[UnpackZipDialog] unpack_zip_frames failed:', err);
      // A failure part-way (database, cancel) can leave frames already moved: reload on close.
      movedRef.current = true;
      setError(message);
      notifyOnce(run, { title: 'Unpack failed', detail: message }, 'warning', true);
    } finally {
      setBusy(false);
    }
  }, [destDir, busy, target, notifyOnce]);

  const close = useCallback(() => {
    if (movedRef.current) onDone();
    onClose();
  }, [onClose, onDone]);

  const percent = progress && progress.total > 0 ? Math.round((progress.current / progress.total) * 100) : 0;
  const finished = outcome != null;

  return (
    <>
      <DialogShell
        title="Unpack from zip"
        size="md"
        busy={busy}
        onClose={close}
        footer={
          <>
            <Button onClick={close} disabled={busy}>{finished ? 'Close' : 'Cancel'}</Button>
            {!finished && (
              <Button variant="primary" onClick={() => void start()} disabled={busy || destDir.trim() === ''} data-autofocus>
                {busy ? 'Unpacking…' : 'Unpack'}
              </Button>
            )}
          </>
        }
      >
        <div className="space-y-3">
          <p>
            {frameCount.toLocaleString()} frame{frameCount === 1 ? '' : 's'} of this set {frameCount === 1 ? 'is' : 'are'} inside{' '}
            {zipCount === 1 ? 'a zip' : `${zipCount} zips`}. Unpacking extracts {frameCount === 1 ? 'it' : 'them'} to a folder and
            keeps {frameCount === 1 ? 'it' : 'them'} in the catalog with their analysis and plate solves. Calibration frames of
            the set that are inside zips are unpacked with them. The zips are not changed.
          </p>

          <div>
            <label htmlFor="unpack-zip-dest" className="mb-1 block text-content-secondary">Unpack to</label>
            <div className="flex gap-2">
              <TextInput
                id="unpack-zip-dest"
                value={destDir}
                onChange={(e) => setDestDir(e.target.value)}
                disabled={busy || finished}
                placeholder="Choose a folder…"
                title={destDir || undefined}
                className="min-w-0 flex-1"
              />
              <Button onClick={() => void browse()} disabled={busy || finished}>
                <Folder size={12} aria-hidden="true" />
                Browse
              </Button>
            </div>
          </div>

          {busy && (
            <div role="status" aria-live="polite" className="space-y-1">
              <div className="flex items-center justify-between gap-3">
                <span className="truncate" title={progress?.message}>{progress?.message ?? 'Starting…'}</span>
                {progress && <span className="tabular-nums">{progress.current} / {progress.total}</span>}
              </div>
              <div className="h-1.5 w-full overflow-hidden rounded-full bg-surface">
                <div className="h-full bg-accent transition-[width] duration-300" style={{ width: `${percent}%` }} />
              </div>
              <p className="text-content-faint">This can't be cancelled. Frames already unpacked stay unpacked.</p>
            </div>
          )}

          {error && (
            <p role="alert" className="break-words text-error">{error}</p>
          )}

          {outcome && (
            <div role="status" className="space-y-1.5">
              <p className={outcome.conflicts.length > 0 ? 'text-warning' : 'text-success'}>{unpackResultLine(outcome)}</p>
              {outcome.conflicts.length > 0 && (
                <>
                  <p>These frames stay in their zip and in the catalog as they were:</p>
                  <ul className="max-h-40 space-y-1 overflow-y-auto rounded border border-border bg-surface p-2">
                    {outcome.conflicts.map((c, i) => (
                      <li key={i} className="break-words font-mono text-[11px] text-content-secondary">{c}</li>
                    ))}
                  </ul>
                </>
              )}
            </div>
          )}
        </div>
      </DialogShell>
      <FolderBrowserModal
        isOpen={showBrowser}
        scope="scan"
        onSelect={(path) => { setDestDir(path); setShowBrowser(false); }}
        onClose={() => setShowBrowser(false)}
      />
    </>
  );
}
