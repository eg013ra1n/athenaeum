import { useEffect, useState, useCallback } from 'react';
import {
  Archive as ArchiveIcon,
  FolderPlus,
  Star,
  Trash2,
  ChevronRight,
  ChevronDown,
  ExternalLink,
} from 'lucide-react';
import {
  listArchiveRoots,
  addArchiveRoot,
  deleteArchiveRoot,
  setDefaultArchiveRoot,
  listArchivedFrameSets,
  listArchiveZips,
} from '../../api/archive';
import { pickDirectory, revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import type { ArchiveRoot, ArchivedFrameSetSummary, ArchiveZip } from '../../types/archive';

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(2)} GB`;
}

interface RootRowProps {
  root: ArchiveRoot;
  archivedSets: ArchivedFrameSetSummary[];
  busy: boolean;
  onSetDefault: (id: number) => void;
  onDelete: (id: number, path: string) => void;
}

function ArchiveRootRow({ root, archivedSets, busy, onSetDefault, onDelete }: RootRowProps) {
  const [expanded, setExpanded] = useState(false);
  const [zipsBySet, setZipsBySet] = useState<Record<number, ArchiveZip[]>>({});
  const [loadingZips, setLoadingZips] = useState<Set<number>>(new Set());

  const setsInThisRoot = archivedSets.filter(
    s => (s.archive_root_path ?? '') === root.path
  );

  const ensureZipsLoaded = async (opId: number) => {
    if (zipsBySet[opId] || loadingZips.has(opId)) return;
    setLoadingZips(prev => new Set(prev).add(opId));
    try {
      const zips = await listArchiveZips(opId);
      setZipsBySet(prev => ({ ...prev, [opId]: zips }));
    } catch (e) {
      console.error('list zips failed', e);
    } finally {
      setLoadingZips(prev => {
        const next = new Set(prev);
        next.delete(opId);
        return next;
      });
    }
  };

  const toggleExpanded = async () => {
    const next = !expanded;
    setExpanded(next);
    if (next) {
      // Pre-load zips for each archived set in this root.
      for (const s of setsInThisRoot) {
        if (s.operation_id) ensureZipsLoaded(s.operation_id);
      }
    }
  };

  return (
    <div className="bg-surface-elevated rounded-lg border border-border">
      <div className="p-3 flex items-center gap-3">
        <button
          onClick={toggleExpanded}
          className="p-1 text-content-muted hover:text-content rounded transition-colors"
          title={expanded ? 'Collapse' : 'Expand'}
        >
          {expanded ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
        </button>
        <button
          onClick={() => !root.is_default && onSetDefault(root.id)}
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
          <div className="text-xs text-content-muted">
            {setsInThisRoot.length} archived frame set{setsInThisRoot.length !== 1 ? 's' : ''}
            {root.label ? ` · ${root.label}` : ''}
          </div>
        </div>
        {root.is_default && (
          <span className="text-xs px-2 py-0.5 rounded-full bg-warning/20 text-warning">
            Default
          </span>
        )}
        {isTauri && (
          <button
            onClick={() => revealItemInDir(root.path).catch(e => alert(`Failed: ${e}`))}
            disabled={busy}
            title="Reveal folder in file manager"
            className="p-1.5 rounded text-content-muted hover:text-accent hover:bg-surface-hover transition-colors"
          >
            <ExternalLink size={16} />
          </button>
        )}
        <button
          onClick={() => onDelete(root.id, root.path)}
          disabled={busy}
          title="Remove from list"
          className="p-1.5 rounded text-content-muted hover:text-error hover:bg-surface-hover transition-colors disabled:opacity-50"
        >
          <Trash2 size={16} />
        </button>
      </div>

      {expanded && (
        <div className="border-t border-border bg-surface px-3 py-2">
          {setsInThisRoot.length === 0 ? (
            <p className="text-xs text-content-muted py-2 px-2">
              No archived frame sets stored in this folder yet.
            </p>
          ) : (
            <div className="space-y-2">
              {setsInThisRoot.map(set => {
                const zips = set.operation_id ? zipsBySet[set.operation_id] : undefined;
                const loading = set.operation_id ? loadingZips.has(set.operation_id) : false;
                const totalSize = zips?.reduce((s, z) => s + z.size_bytes, 0) ?? 0;
                return (
                  <div
                    key={set.frames_set_id}
                    className="rounded border border-border/60 bg-surface-elevated"
                  >
                    <div className="px-3 py-2 flex items-center gap-3">
                      <ArchiveIcon size={14} className="text-content-muted" />
                      <div className="flex-1 min-w-0">
                        <div className="text-sm text-content font-medium truncate">
                          {set.name ?? `Frame Set #${set.frames_set_id}`}
                        </div>
                        <div className="text-xs text-content-muted">
                          {set.archived_at?.slice(0, 10) ?? ''}
                          {' · '}
                          {set.lights_count} lights / {set.flats_count} flats / {set.darks_count} darks / {set.bias_count} bias / {set.darkflats_count} darkflats
                          {totalSize > 0 ? ` · ${formatBytes(totalSize)}` : ''}
                        </div>
                      </div>
                    </div>
                    {loading ? (
                      <div className="px-3 pb-2 text-xs text-content-muted">Loading zips…</div>
                    ) : zips && zips.length > 0 ? (
                      <ul className="px-3 pb-2 space-y-1">
                        {zips.map(z => (
                          <li
                            key={z.path}
                            className="flex items-center gap-2 text-xs"
                          >
                            <span className="font-mono text-content-muted truncate flex-1">
                              {z.filename}
                            </span>
                            <span className="text-content-muted whitespace-nowrap">
                              {formatBytes(z.size_bytes)}
                            </span>
                            {!z.exists && (
                              <span className="text-error whitespace-nowrap">missing</span>
                            )}
                            {isTauri && z.exists && (
                              <button
                                onClick={() => revealItemInDir(z.path).catch(e => alert(`Failed: ${e}`))}
                                title="Reveal in file manager"
                                className="p-1 rounded text-content-muted hover:text-accent hover:bg-surface-hover transition-colors"
                              >
                                <ExternalLink size={12} />
                              </button>
                            )}
                          </li>
                        ))}
                      </ul>
                    ) : null}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export function ArchiveFoldersSection() {
  const [roots, setRoots] = useState<ArchiveRoot[]>([]);
  const [archivedSets, setArchivedSets] = useState<ArchivedFrameSetSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  const reload = useCallback(async () => {
    try {
      setLoading(true);
      const [rootsRes, setsRes] = await Promise.all([
        listArchiveRoots(),
        listArchivedFrameSets(),
      ]);
      setRoots(rootsRes);
      setArchivedSets(setsRes);
    } catch (e) {
      console.error('Failed to load archive folders', e);
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
            Destination folders for ZIP archives produced by &ldquo;Move and ZIP&rdquo;. Add as many as you like &mdash; one is marked as the default. Expand a row to see what&rsquo;s archived inside it.
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
          {roots.map(root => (
            <ArchiveRootRow
              key={root.id}
              root={root}
              archivedSets={archivedSets}
              busy={busy}
              onSetDefault={handleSetDefault}
              onDelete={handleDelete}
            />
          ))}
        </div>
      )}
    </div>
  );
}
