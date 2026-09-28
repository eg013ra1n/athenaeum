import { useState } from 'react';
import { api } from '../../api';
import { formatBytes } from './format';

/**
 * D3 §3.3 project bar: the per-project auto-replication toggle and the
 * project's published byte total.
 *
 * The toggle is a LOCAL preference (`set_project_auto_replicate` writes the
 * `collab_projects.auto_replicate` column; the hub never learns of it). It is
 * saved then re-read — the parent's `onToggled` reloads the detail from the
 * catalog, so the rendered state is the stored one (S6), never optimistic.
 *
 * "Sync now" is not here: it is one global command of the live exchange
 * (`collab_sync_now`, L10), and its one button is the page header's
 * (`CollabLiveStatus`).
 *
 * The auto-publish preference moved out to `AutoPublishSwitch` (contributor-
 * path cycle, spec §7.2/§9): it is rendered for every member of the
 * Contribute tab, not gated on `canReceive` the way this bar is.
 */
export default function AutoReplicateBar({
  projectId,
  autoReplicate,
  publishedBytes,
  onToggled,
}: {
  projectId: string;
  autoReplicate: boolean;
  /** Sum of the published, non-superseded frames; `null` while unknown. */
  publishedBytes: number | null;
  onToggled: () => void;
}) {
  const [saving, setSaving] = useState(false);
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
              New approved contributions download automatically. Every member who has a frame
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
        </div>
      </div>
      {error && <p className="text-sm text-error">{error}</p>}
    </div>
  );
}
