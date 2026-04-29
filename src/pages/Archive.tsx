import { useEffect, useState, useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { Archive as ArchiveIcon, Trash2, Upload } from 'lucide-react';
import { listArchivedFrameSets, deleteArchive } from '../api/archive';
import type { ArchivedFrameSetSummary } from '../types/archive';
import { RestoreDialog } from '../components/archive/RestoreDialog';

export default function Archive() {
  const [items, setItems] = useState<ArchivedFrameSetSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [restoreFor, setRestoreFor] = useState<ArchivedFrameSetSummary | null>(null);
  const navigate = useNavigate();

  const reload = useCallback(() => {
    setLoading(true);
    listArchivedFrameSets()
      .then(setItems)
      .catch(err => {
        console.error('list archives failed', err);
        alert(`Failed to load archives: ${err}`);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => { reload(); }, [reload]);

  if (loading) {
    return (
      <div className="p-6 text-content-muted flex items-center gap-2">
        <span className="inline-block w-4 h-4 border-2 border-accent border-t-transparent rounded-full animate-spin" />
        Loading archives…
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className="p-6 text-content-muted space-y-2">
        <ArchiveIcon size={32} className="mb-2 opacity-40" />
        <p className="text-sm">No archived frame sets yet.</p>
        <p className="text-sm">Use "Move and ZIP" on a frame set to archive it.</p>
      </div>
    );
  }

  return (
    <div className="p-6">
      <h1 className="text-xl font-semibold mb-4 flex items-center gap-2 text-content">
        <ArchiveIcon size={20} />
        Archive
      </h1>

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-border">
              <th className="text-left py-2 pr-3 font-medium text-content-muted">Object</th>
              <th className="text-left py-2 pr-3 font-medium text-content-muted">Archived at</th>
              <th className="text-right py-2 pr-3 font-medium text-content-muted" title="Lights / Flats / Darks / Bias / DarkFlats">L / F / D / B / DF</th>
              <th className="text-left py-2 pr-3 font-medium text-content-muted">Location</th>
              <th className="text-right py-2 font-medium text-content-muted">Actions</th>
            </tr>
          </thead>
          <tbody>
            {items.map(item => (
              <tr
                key={item.frames_set_id}
                className="border-b border-border/40 hover:bg-surface-hover transition-colors"
              >
                <td className="py-2 pr-3 text-content">
                  {item.name ?? `Frame Set #${item.frames_set_id}`}
                </td>
                <td className="py-2 pr-3 font-mono text-xs text-content-muted">
                  {item.archived_at
                    ? item.archived_at.slice(0, 19).replace('T', ' ')
                    : '—'
                  }
                </td>
                <td className="py-2 pr-3 text-right font-mono text-xs text-content-muted">
                  {item.lights_count}/{item.flats_count}/{item.darks_count}/{item.bias_count}/{item.darkflats_count}
                </td>
                <td className="py-2 pr-3 font-mono text-xs text-content-muted max-w-xs truncate">
                  {item.archive_root_path ?? '—'}
                </td>
                <td className="py-2 text-right space-x-1.5">
                  <button
                    onClick={() => navigate(`/objects/${item.frames_set_id}`)}
                    className="text-xs px-2 py-1 border border-border rounded hover:bg-surface-hover transition-colors text-content"
                  >
                    Open
                  </button>
                  <button
                    disabled={!item.operation_id}
                    onClick={() => setRestoreFor(item)}
                    className="text-xs px-2 py-1 border border-border rounded inline-flex items-center gap-1 hover:bg-surface-hover transition-colors text-content disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    <Upload size={12} />
                    Restore
                  </button>
                  <button
                    disabled={!item.operation_id}
                    onClick={async () => {
                      if (!item.operation_id) return;
                      if (!window.confirm(
                        'Permanently delete this archive (zip files + catalog rows)? This cannot be undone.'
                      )) return;
                      try {
                        await deleteArchive(item.operation_id);
                        reload();
                      } catch (e) {
                        alert(`Delete failed: ${e}`);
                      }
                    }}
                    className="text-xs px-2 py-1 border border-border rounded inline-flex items-center gap-1 hover:bg-error/10 text-error transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    <Trash2 size={12} />
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {restoreFor && restoreFor.operation_id && (
        <RestoreDialog
          item={restoreFor}
          onCancel={() => setRestoreFor(null)}
          onCompleted={() => {
            setRestoreFor(null);
            reload();
          }}
        />
      )}
    </div>
  );
}
