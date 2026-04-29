import { useEffect, useState, useCallback } from 'react';
import { Archive as ArchiveIcon, FolderPlus, Star, Trash2 } from 'lucide-react';
import {
  listArchiveRoots,
  addArchiveRoot,
  deleteArchiveRoot,
  setDefaultArchiveRoot,
} from '../../api/archive';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import type { ArchiveRoot } from '../../types/archive';

export function ArchiveFoldersSection() {
  const [roots, setRoots] = useState<ArchiveRoot[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  const reload = useCallback(async () => {
    try {
      setLoading(true);
      setRoots(await listArchiveRoots());
    } catch (e) {
      console.error('Failed to list archive roots', e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  const handleAdd = async () => {
    if (!isTauri) {
      alert('Adding archive folders requires the desktop app.');
      return;
    }
    const picked = await pickDirectory();
    if (!picked) return;
    setBusy(true);
    try {
      await addArchiveRoot(picked, null);
      await reload();
    } catch (e) {
      alert(`Failed to add archive folder: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async (id: number, path: string) => {
    if (!window.confirm(`Remove archive folder "${path}" from the configured list? Files in that folder are not deleted.`)) return;
    setBusy(true);
    try {
      await deleteArchiveRoot(id);
      await reload();
    } catch (e) {
      alert(`Failed to delete archive folder: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const handleSetDefault = async (id: number) => {
    setBusy(true);
    try {
      await setDefaultArchiveRoot(id);
      await reload();
    } catch (e) {
      alert(`Failed to set default: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-8">
      <div className="flex items-center justify-between mb-4">
        <div>
          <h3 className="text-xl font-semibold flex items-center gap-2">
            <ArchiveIcon size={20} />
            Archive Folders
          </h3>
          <p className="text-sm text-content-muted mt-1">
            Destination folders for ZIP archives produced by &ldquo;Move and ZIP&rdquo;. Add as many as you like &mdash; one is marked as the default.
          </p>
        </div>
        <button
          onClick={handleAdd}
          disabled={busy}
          className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover rounded-lg transition disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <FolderPlus size={18} />
          Add Archive Folder
        </button>
      </div>

      {loading ? (
        <div className="bg-surface-elevated rounded-lg p-6 text-center text-content-muted text-sm">
          Loading archive folders…
        </div>
      ) : roots.length === 0 ? (
        <div className="bg-surface-elevated rounded-lg p-6 text-center">
          <p className="text-content-muted text-sm">
            No archive folders configured yet. Click &ldquo;Add Archive Folder&rdquo; to start.
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          {roots.map((root) => (
            <div
              key={root.id}
              className="bg-surface-elevated rounded-lg p-3 border border-border flex items-center gap-3"
            >
              <button
                onClick={() => !root.is_default && handleSetDefault(root.id)}
                disabled={busy || root.is_default}
                title={root.is_default ? 'Default archive folder' : 'Set as default'}
                className={`p-1.5 rounded transition-colors ${
                  root.is_default
                    ? 'text-warning'
                    : 'text-content-muted hover:text-warning hover:bg-surface-hover cursor-pointer'
                }`}
              >
                <Star size={16} fill={root.is_default ? 'currentColor' : 'none'} />
              </button>
              <div className="flex-1 min-w-0">
                <div className="font-mono text-sm text-content truncate">{root.path}</div>
                {root.label && (
                  <div className="text-xs text-content-muted">{root.label}</div>
                )}
              </div>
              {root.is_default && (
                <span className="text-xs px-2 py-0.5 rounded-full bg-warning/20 text-warning">
                  Default
                </span>
              )}
              <button
                onClick={() => handleDelete(root.id, root.path)}
                disabled={busy}
                title="Remove from list"
                className="p-1.5 rounded text-content-muted hover:text-error hover:bg-surface-hover transition-colors disabled:opacity-50"
              >
                <Trash2 size={16} />
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
