// One modal, two modes (spec §5.1): `available` (notes + install flow) and
// `whatsNew` (notes + Close). Rendered at app root by Layout.

import { useEffect, useRef, useState } from 'react';
import { Copy, Download, ExternalLink, RefreshCw, X, AlertTriangle, CheckCircle2 } from 'lucide-react';
import { api } from '../../api';
import { openUrl } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { formatTimestamp } from '../../utils/dateFormatting';
import { useUpdates } from '../../contexts/UpdatesContext';
import { useTransfers } from '../../contexts/TransfersContext';
import type { ComputeQueueEntry } from '../../types/models';
import { ReleaseNotes } from './ReleaseNotes';

const isWindows = typeof navigator !== 'undefined' && /Win/i.test(navigator.platform);

function formatBytes(n: number): string {
  if (n >= 1024 * 1024 * 1024) return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024).toFixed(0)} KB`;
}

/** Soft guard (spec §5.2): what the sidebar already knows about running work. */
function useBusyReason(open: boolean): string | null {
  const { active } = useTransfers();
  const [queue, setQueue] = useState<ComputeQueueEntry[]>([]);
  useEffect(() => {
    if (!open) return;
    api.invoke<ComputeQueueEntry[]>('get_compute_queue')
      .then(setQueue)
      .catch((err) => console.error('[UpdateDialog] get_compute_queue failed:', err));
  }, [open]);
  const running = queue.find((e) => e.state === 'running');
  if (running) return `${running.label} is in progress — restarting now will cancel it.`;
  if (active.length > 0) return `${active.length} transfer${active.length > 1 ? 's are' : ' is'} in progress — restarting now will interrupt ${active.length > 1 ? 'them' : 'it'}.`;
  return null;
}

const DIALOG_TITLE_ID = 'update-dialog-title';

export function UpdateDialog() {
  const { check, whatsNew, dialog, phase, close, install, restart } = useUpdates();
  const open = dialog !== 'closed';
  const busy = useBusyReason(open);
  const [copied, setCopied] = useState(false);
  const copyTimeoutRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  // Escape closes the dialog — the download itself is never cancelled (the
  // plugin offers no cancel), but the dialog must never trap the user for
  // its whole duration: reopening from About (or the bell link) restores
  // live progress from the context.
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [open, close]);

  useEffect(() => {
    return () => {
      if (copyTimeoutRef.current) clearTimeout(copyTimeoutRef.current);
    };
  }, []);

  if (!open) return null;

  const isAvailable = dialog === 'available';
  const version = isAvailable ? check?.latestVersion : whatsNew?.version;
  const notes = isAvailable ? check?.notes : whatsNew?.notes;
  const blogUrl = isAvailable ? check?.blogUrl : whatsNew?.blogUrl;
  if (!version) return null;

  const copyDocker = async () => {
    if (!check?.dockerImage) return;
    try {
      await navigator.clipboard.writeText(`docker pull ${check.dockerImage}`);
      setCopied(true);
      copyTimeoutRef.current = setTimeout(() => setCopied(false), 1500);
    } catch (err) {
      console.error('[UpdateDialog] clipboard write failed:', err);
    }
  };

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50" role="dialog" aria-modal="true" aria-labelledby={DIALOG_TITLE_ID}>
      <div className="mx-4 flex max-h-[85vh] w-full max-w-2xl flex-col rounded-lg border border-border bg-surface-elevated shadow-xl">
        <div className="flex items-start justify-between border-b border-border px-6 py-4">
          <div>
            <h2 id={DIALOG_TITLE_ID} className="text-lg font-semibold text-content">
              {isAvailable ? `Athenaeum v${version} is available` : `What's new in v${version}`}
            </h2>
            {isAvailable && check && (
              <p className="mt-1 text-xs text-content-muted">
                You have v{check.currentVersion}
                {check.pubDate ? ` · released ${formatTimestamp(check.pubDate)}` : ''}
                {check.channel === 'beta' ? ' · beta channel' : ''}
              </p>
            )}
          </div>
          <button onClick={close} className="rounded p-1 text-content-muted hover:bg-surface-hover hover:text-content" aria-label="Close">
            <X size={18} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-6 py-4">
          {notes ? <ReleaseNotes markdown={notes} /> : <p className="text-sm text-content-muted">No release notes were published for this version.</p>}
          {blogUrl && (
            <button onClick={() => void openUrl(blogUrl)} className="mt-4 flex items-center gap-1 text-sm text-accent hover:underline">
              Read the full post <ExternalLink size={13} />
            </button>
          )}
        </div>

        <div className="space-y-3 border-t border-border px-6 py-4">
          {isAvailable && phase.kind === 'downloading' && (
            <div>
              <div className="mb-1 flex justify-between text-xs text-content-muted">
                <span>Downloading v{version}…</span>
                <span>
                  {formatBytes(phase.downloaded)}
                  {phase.total ? ` / ${formatBytes(phase.total)} · ${Math.floor((phase.downloaded / phase.total) * 100)} %` : ''}
                </span>
              </div>
              <div className="h-2 w-full overflow-hidden rounded bg-surface">
                <div
                  className="h-full bg-accent transition-all"
                  style={{ width: phase.total ? `${Math.min(100, (phase.downloaded / phase.total) * 100)}%` : '30%' }}
                />
              </div>
              {isWindows && <p className="mt-2 text-xs text-content-muted">Athenaeum will close when the installer starts and reopen when it is done.</p>}
            </div>
          )}
          {isAvailable && phase.kind === 'installing' && (
            <div>
              <div className="mb-1 flex justify-between text-xs text-content-muted">
                <span>Installing v{phase.version ?? version}…</span>
              </div>
              <div className="h-2 w-full overflow-hidden rounded bg-surface">
                <div className="h-full bg-accent transition-all" style={{ width: '100%' }} />
              </div>
              {isWindows && <p className="mt-2 text-xs text-content-muted">Athenaeum will close when the installer starts and reopen when it is done.</p>}
            </div>
          )}
          {isAvailable && (phase.kind === 'failed' || phase.kind === 'restartFailed') && (
            <div className="flex items-start gap-2 rounded-lg border border-error/40 bg-error/10 p-3 text-sm text-error">
              <AlertTriangle size={15} className="mt-0.5 flex-shrink-0" />
              <span>{phase.message}</span>
            </div>
          )}
          {isAvailable && phase.kind === 'ready' && (
            <div className="flex items-center gap-2 rounded-lg border border-success/40 bg-success/10 p-3 text-sm text-success">
              <CheckCircle2 size={15} /> v{phase.version} is installed — restart to apply.
            </div>
          )}
          {isAvailable && busy && (phase.kind === 'ready' || (isWindows && phase.kind !== 'downloading')) && (
            <div className="flex items-start gap-2 text-sm text-warning">
              <AlertTriangle size={15} className="mt-0.5 flex-shrink-0" />
              <span>{busy}</span>
            </div>
          )}

          <div className="flex flex-wrap items-center justify-end gap-2">
            {!isAvailable && (
              <button onClick={close} className="rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">Close</button>
            )}

            {isAvailable && check && !check.platformSupported && (
              <>
                {check.dockerImage && (
                  <div className="mr-auto flex items-center gap-2">
                    <code className="rounded bg-surface px-2 py-1 font-mono text-xs text-content">docker pull {check.dockerImage}</code>
                    <button onClick={copyDocker} className="rounded p-1 text-content-muted hover:bg-surface-hover hover:text-content" title="Copy">
                      {copied ? <CheckCircle2 size={15} className="text-success" /> : <Copy size={15} />}
                    </button>
                  </div>
                )}
                <button onClick={() => void openUrl(check.downloadPageUrl)} className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">
                  Open download page <ExternalLink size={13} />
                </button>
              </>
            )}

            {isAvailable && check && check.platformSupported && isTauri && (
              <>
                {(phase.kind === 'idle' || phase.kind === 'failed') && (
                  <>
                    <button onClick={close} className="rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">Later</button>
                    {phase.kind === 'failed' && (
                      <button onClick={() => void openUrl(check.downloadPageUrl)} className="flex items-center gap-2 rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">
                        Open download page <ExternalLink size={13} />
                      </button>
                    )}
                    <button onClick={() => void install()} className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">
                      {phase.kind === 'failed' ? <RefreshCw size={15} /> : <Download size={15} />}
                      {phase.kind === 'failed' ? 'Retry' : 'Download and install'}
                    </button>
                  </>
                )}
                {phase.kind === 'ready' && (
                  <>
                    <button onClick={close} className="rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">Later</button>
                    <button onClick={() => void restart()} className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">
                      <RefreshCw size={15} /> Restart now
                    </button>
                  </>
                )}
                {phase.kind === 'restartFailed' && (
                  <>
                    <button onClick={close} className="rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">Later</button>
                    <button onClick={() => void openUrl(check.downloadPageUrl)} className="flex items-center gap-2 rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">
                      Open download page <ExternalLink size={13} />
                    </button>
                    <button onClick={() => void restart()} className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">
                      <RefreshCw size={15} /> Restart now
                    </button>
                  </>
                )}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
