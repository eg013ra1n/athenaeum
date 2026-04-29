import { useState } from 'react';
import { startRestoreOperation } from '../../api/archive';
import { pickDirectory } from '../../api/desktop';
import type { ArchivedFrameSetSummary } from '../../types/archive';

interface Props {
  item: ArchivedFrameSetSummary;
  onCancel: () => void;
  onCompleted: () => void;
}

export function RestoreDialog({ item, onCancel, onCompleted }: Props) {
  const [target, setTarget] = useState<string>('');
  const [overwrite, setOverwrite] = useState(false);
  const [keepZip, setKeepZip] = useState(false);
  const [busy, setBusy] = useState(false);

  async function pickTarget() {
    const picked = await pickDirectory();
    if (picked) setTarget(picked);
  }

  async function start() {
    if (!item.operation_id || !target) return;
    setBusy(true);
    try {
      await startRestoreOperation(item.operation_id, target, overwrite, keepZip);
      onCompleted();
    } catch (e) {
      console.error('restore failed', e);
      alert(`Restore failed: ${e}`);
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
      <div className="bg-surface-elevated border border-border rounded-lg shadow-2xl p-6 max-w-lg w-full">
        <h2 className="text-lg font-semibold mb-2 text-content">Restore archive</h2>
        <p className="text-sm text-content-muted mb-4">
          {item.name ?? `Frame Set #${item.frames_set_id}`}
        </p>

        <label className="block text-sm font-medium text-content mb-1">Restore target folder</label>
        <div className="flex gap-2 mb-4">
          <input
            type="text"
            value={target}
            onChange={e => setTarget(e.target.value)}
            placeholder="Choose a folder…"
            className="flex-1 px-3 py-1.5 bg-surface-hover border border-border rounded-lg text-sm text-content focus:outline-none focus:border-accent"
          />
          <button
            onClick={pickTarget}
            className="px-3 py-1.5 border border-border rounded-lg text-sm text-content hover:bg-surface-hover transition-colors"
          >
            Browse…
          </button>
        </div>

        <label className="flex items-center gap-2 text-sm text-content mb-2 cursor-pointer">
          <input
            type="checkbox"
            checked={overwrite}
            onChange={e => setOverwrite(e.target.checked)}
            className="accent-accent"
          />
          Overwrite existing files at target
        </label>
        <label className="flex items-center gap-2 text-sm text-content mb-5 cursor-pointer">
          <input
            type="checkbox"
            checked={keepZip}
            onChange={e => setKeepZip(e.target.checked)}
            className="accent-accent"
          />
          Keep zip file after restore (default: delete)
        </label>

        <div className="flex justify-end gap-2">
          <button
            onClick={onCancel}
            disabled={busy}
            className="px-4 py-1.5 rounded-lg border border-border text-sm text-content hover:bg-surface-hover transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={start}
            disabled={busy || !target}
            className="px-4 py-1.5 rounded-lg bg-accent text-white text-sm disabled:opacity-50 disabled:cursor-not-allowed hover:brightness-110 transition-colors"
          >
            {busy ? 'Restoring…' : 'Restore'}
          </button>
        </div>
      </div>
    </div>
  );
}
