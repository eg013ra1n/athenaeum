import { useState } from 'react';
import { Loader2, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { formatBytes } from './format';

/**
 * D3 §3.3 project bar: the per-project auto-replication toggle, the project's
 * published byte total, and "Sync now".
 *
 * The toggle is a LOCAL preference (`set_project_auto_replicate` writes the
 * `collab_projects.auto_replicate` column; the hub never learns of it). It is
 * saved then re-read — the parent's `onToggled` reloads the detail from the
 * catalog, so the rendered state is the stored one (S6), never optimistic.
 *
 * "Sync now" runs one replication pass immediately with the toggle FORCED on,
 * so it is deliberately not gated on `autoReplicate` here: a project with
 * auto-download off still downloads when the user asks explicitly. The pass is
 * spawned, not awaited — the downloads surface themselves on the package rows,
 * so only a failure to start needs a notification.
 */
export default function AutoReplicateBar({
  projectId,
  autoReplicate,
  publishedBytes,
  onToggled,
  onSynced,
}: {
  projectId: string;
  autoReplicate: boolean;
  /** Sum of the published, non-superseded packages; `null` while unknown. */
  publishedBytes: number | null;
  onToggled: () => void;
  onSynced: () => void;
}) {
  const { notify } = useNotifications();
  const [saving, setSaving] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const setEnabled = async (enabled: boolean) => {
    setSaving(true);
    setError(null);
    try {
      await api.invoke('set_project_auto_replicate', { projectId, enabled });
      onToggled();
    } catch (err) {
      // S6 — a failed preference write surfaces inline, never silently caught.
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] set_project_auto_replicate failed:', err);
      setError(msg);
    } finally {
      setSaving(false);
    }
  };

  const syncNow = async () => {
    setSyncing(true);
    setError(null);
    try {
      await api.invoke('sync_project_now', { projectId });
      onSynced();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] sync_project_now failed:', err);
      notify({
        title: 'Sync now failed',
        detail: msg,
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
        link: `/projects/${projectId}`,
        dedupeKey: `sync-now-failed-${projectId}`,
      });
      setError(msg);
    } finally {
      setSyncing(false);
    }
  };

  return (
    <div className="space-y-1">
      <div className="flex flex-wrap items-start gap-x-4 gap-y-2 rounded-lg border border-border bg-surface px-3 py-2">
        <label className="flex max-w-xl cursor-pointer items-start gap-2.5">
          <input
            type="checkbox"
            checked={autoReplicate}
            disabled={saving}
            onChange={(e) => void setEnabled(e.target.checked)}
            className="mt-0.5 h-4 w-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span>
            <span className="block text-sm font-medium text-content-secondary">
              Auto-download contributions
            </span>
            <span className="mt-0.5 block text-xs text-content-muted">
              New approved contributions download automatically. Every member who has a package
              helps distribute it.
            </span>
          </span>
        </label>

        <div className="ml-auto flex items-center gap-3">
          {publishedBytes !== null && (
            <span
              className="text-xs text-content-muted"
              title="Total size of the project's published contributions"
            >
              {formatBytes(publishedBytes)} published
            </span>
          )}
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
        </div>
      </div>
      {error && <p className="text-sm text-error">{error}</p>}
    </div>
  );
}
