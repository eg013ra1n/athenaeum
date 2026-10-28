import { useState, useEffect, useMemo, useCallback } from 'react';
import { Trash2, RotateCcw, Loader2, Filter, ChevronDown, ChevronRight, Square, CheckSquare, XSquare, Folder, FileArchive } from 'lucide-react';
import { api } from '../api';
import { HistoryNav } from '../components/HistoryNav';
import { format } from 'date-fns';
import type { BlackHoleEntry, BlackHoleZipEntry } from '../types/models';
import type { BlackholeChangedEvent } from '../types/helpers';
import { Chip } from '../components/ui';
import { findZipInPath } from '../components/dualpane/zipView';
import { useNotifications } from '../contexts/NotificationContext';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';

interface FolderGroup {
  path: string;
  files: BlackHoleEntry[];
  totalSize: number;
}

/** Every distinct refusal text, so a single zip frame's "only the whole zip can be removed" reaches the owner. */
function refusalLines(errors: string[], max = 5): string {
  const unique = Array.from(new Set(errors.map((e) => e.replace(/^Error:\s*/, ''))));
  const shown = unique.slice(0, max).join('\n');
  return unique.length > max ? `${shown}\n…and ${unique.length - max} more` : shown;
}

interface ProgressState {
  current: number;
  total: number;
  action: 'restore' | 'void';
}

export default function BlackHole() {
  const { notify } = useNotifications();
  const [entries, setEntries] = useState<BlackHoleEntry[]>([]);
  /** Whole zips in the Black Hole (their own rows; restore and Void take the row `id`). */
  const [zips, setZips] = useState<BlackHoleZipEntry[]>([]);
  const [selectedZipIds, setSelectedZipIds] = useState<Set<number>>(new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<string | null>(null);

  // Selection state
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
  const [lastSelectedId, setLastSelectedId] = useState<number | null>(null);
  const [expandedFolders, setExpandedFolders] = useState<Set<string>>(new Set());

  // Progress state for bulk operations
  const [progress, setProgress] = useState<ProgressState | null>(null);

  const [confirmDialog, setConfirmDialog] = useState<{
    isOpen: boolean;
    title: string;
    message: string;
    onConfirm: () => void;
    confirmDanger?: boolean;
  }>({
    isOpen: false,
    title: '',
    message: '',
    onConfirm: () => {},
    confirmDanger: false,
  });
  const [alertDialog, setAlertDialog] = useState<{
    isOpen: boolean;
    title: string;
    message: string;
    variant: 'error' | 'warning' | 'info';
  }>({
    isOpen: false,
    title: '',
    message: '',
    variant: 'info',
  });

  const showConfirm = (title: string, message: string, onConfirm: () => void, confirmDanger = false) => {
    setConfirmDialog({ isOpen: true, title, message, onConfirm, confirmDanger });
  };

  const showAlert = (title: string, message: string, variant: 'error' | 'warning' | 'info' = 'info') => {
    setAlertDialog({ isOpen: true, title, message, variant });
  };

  // Load black hole entries
  const loadEntries = async () => {
    try {
      setLoading(true);
      setError(null);
      const [result, zipRows] = await Promise.all([
        api.invoke<BlackHoleEntry[]>('get_black_hole_files', { filter }),
        api.invoke<BlackHoleZipEntry[]>('get_black_hole_zips'),
      ]);
      setEntries(result);
      setZips(filter ? zipRows.filter((z) => z.fromWhere === filter) : zipRows);
      // Clear selection when reloading
      setSelectedIds(new Set());
      setSelectedZipIds(new Set());
      setLastSelectedId(null);
    } catch (err) {
      console.error('Failed to load black hole entries:', err);
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadEntries();
  }, [filter]);

  // A zip moved, restored or voided elsewhere (the file browser, another window): reload the zip rows.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<BlackholeChangedEvent>('blackhole-changed', (payload) => {
      if (cancelled || !payload.action.startsWith('zip-')) return;
      api.invoke<BlackHoleZipEntry[]>('get_black_hole_zips')
        .then((rows) => {
          if (cancelled) return;
          setZips(filter ? rows.filter((z) => z.fromWhere === filter) : rows);
          setSelectedZipIds(new Set());
        })
        .catch((err) => console.error('[BlackHole] reloading zips failed:', err));
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[BlackHole] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, [filter]);

  // Initialize expanded folders when entries change
  useEffect(() => {
    if (entries.length > 0) {
      const folders = new Set(entries.map(e => getFolderPath(e.original_path)));
      setExpandedFolders(folders);
    }
  }, [entries]);

  // Group entries by folder
  const folderGroups = useMemo((): FolderGroup[] => {
    const groups = new Map<string, BlackHoleEntry[]>();

    entries.forEach(entry => {
      const folderPath = getFolderPath(entry.original_path);
      if (!groups.has(folderPath)) {
        groups.set(folderPath, []);
      }
      groups.get(folderPath)!.push(entry);
    });

    // Convert to array, sort by folder path
    return Array.from(groups.entries())
      .map(([path, files]) => ({
        path,
        files: files.sort((a, b) => a.filename.localeCompare(b.filename)),
        totalSize: files.reduce((sum, f) => sum + f.file_size, 0),
      }))
      .sort((a, b) => a.path.localeCompare(b.path));
  }, [entries]);

  function describeSelection(files: number, zipCount: number): string {
    const parts: string[] = [];
    if (files > 0) parts.push(`${files} file${files !== 1 ? 's' : ''}`);
    if (zipCount > 0) parts.push(`${zipCount} zip${zipCount !== 1 ? 's' : ''}`);
    return parts.join(' and ');
  }

  // Get folder path from full file path. Root edges: 'C:\a.fits' → 'C:\',
  // '/a.fits' → '/' (the old `lastSlash > 0` check returned the whole path
  // for POSIX-root files and drive-relative 'C:' for drive-root files).
  function getFolderPath(fullPath: string): string {
    const lastSlash = Math.max(fullPath.lastIndexOf('/'), fullPath.lastIndexOf('\\'));
    if (lastSlash < 0) return fullPath;
    if (lastSlash === 0) return fullPath[0];
    const head = fullPath.substring(0, lastSlash);
    return /^[A-Za-z]:$/.test(head) ? head + '\\' : head;
  }

  // Format file size
  const formatSize = (bytes: number): string => {
    const kb = bytes / 1024;
    const mb = kb / 1024;
    const gb = mb / 1024;

    if (gb >= 1) return `${gb.toFixed(2)} GB`;
    if (mb >= 1) return `${mb.toFixed(2)} MB`;
    if (kb >= 1) return `${kb.toFixed(2)} KB`;
    return `${bytes} bytes`;
  };

  // Selection handlers
  const handleCheckboxClick = useCallback((fileId: number, folderFiles: BlackHoleEntry[], e: React.MouseEvent) => {
    e.stopPropagation();

    if (e.shiftKey && lastSelectedId !== null) {
      // Range selection within folder
      const fileIds = folderFiles.map(f => f.file_id);
      const lastIndex = fileIds.indexOf(lastSelectedId);
      const currentIndex = fileIds.indexOf(fileId);

      if (lastIndex !== -1 && currentIndex !== -1) {
        const [start, end] = [Math.min(lastIndex, currentIndex), Math.max(lastIndex, currentIndex)];
        setSelectedIds(prev => {
          const newSet = new Set(prev);
          for (let i = start; i <= end; i++) {
            newSet.add(fileIds[i]);
          }
          return newSet;
        });
      }
    } else {
      // Toggle single item
      setSelectedIds(prev => {
        const newSet = new Set(prev);
        if (newSet.has(fileId)) {
          newSet.delete(fileId);
        } else {
          newSet.add(fileId);
        }
        return newSet;
      });
    }
    setLastSelectedId(fileId);
  }, [lastSelectedId]);

  const handleSelectAllInFolder = useCallback((files: BlackHoleEntry[]) => {
    const fileIds = files.map(f => f.file_id);
    const allSelected = fileIds.every(id => selectedIds.has(id));

    setSelectedIds(prev => {
      const newSet = new Set(prev);
      if (allSelected) {
        // Deselect all in folder
        fileIds.forEach(id => newSet.delete(id));
      } else {
        // Select all in folder
        fileIds.forEach(id => newSet.add(id));
      }
      return newSet;
    });
  }, [selectedIds]);

  const handleSelectAll = useCallback(() => {
    const allFileIds = entries.map(e => e.file_id);
    setSelectedIds(new Set(allFileIds));
    setSelectedZipIds(new Set(zips.map((z) => z.id)));
  }, [entries, zips]);

  const handleClearSelection = useCallback(() => {
    setSelectedIds(new Set());
    setSelectedZipIds(new Set());
    setLastSelectedId(null);
  }, []);

  const toggleZip = useCallback((id: number) => {
    setSelectedZipIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleSelectAllZips = useCallback(() => {
    setSelectedZipIds((prev) => (zips.every((z) => prev.has(z.id)) ? new Set() : new Set(zips.map((z) => z.id))));
  }, [zips]);

  const toggleFolder = useCallback((folderPath: string) => {
    setExpandedFolders(prev => {
      const newSet = new Set(prev);
      if (newSet.has(folderPath)) {
        newSet.delete(folderPath);
      } else {
        newSet.add(folderPath);
      }
      return newSet;
    });
  }, []);

  // Bulk actions with progress
  const handleRestoreSelected = useCallback(() => {
    const fileCount = selectedIds.size;
    const zipCount = selectedZipIds.size;
    showConfirm(
      'Restore Selected',
      `Restore ${describeSelection(fileCount, zipCount)} from the black hole?`,
      async () => {
        const idsArray = Array.from(selectedIds);
        const zipIds = Array.from(selectedZipIds);
        const total = idsArray.length + zipIds.length;
        setProgress({ current: 0, total, action: 'restore' });

        let successCount = 0;
        const errors: string[] = [];
        let done = 0;

        for (const id of zipIds) {
          try {
            await api.invoke('restore_zip_from_black_hole', { id });
            successCount++;
          } catch (err) {
            console.error('restore_zip_from_black_hole failed:', err);
            errors.push(String(err));
          }
          setProgress({ current: ++done, total, action: 'restore' });
        }
        for (const fileId of idsArray) {
          try {
            await api.invoke('restore_from_black_hole', { fileId });
            successCount++;
          } catch (err) {
            console.error('restore_from_black_hole failed:', err);
            errors.push(String(err));
          }
          setProgress({ current: ++done, total, action: 'restore' });
        }

        setProgress(null);
        await loadEntries();

        if (errors.length > 0) {
          showAlert('Restore Complete', `Restored ${successCount} of ${total}. ${errors.length} error(s):\n${refusalLines(errors)}`, 'warning');
        } else if (zipIds.length > 0) {
          notify({ title: `${zipIds.length} zip${zipIds.length === 1 ? '' : 's'} restored`, detail: 'Their frames are back in use.', kind: 'files', tone: 'success' });
        }
      }
    );
  }, [selectedIds, selectedZipIds, notify]);

  const handleVoidSelected = useCallback(() => {
    const fileCount = selectedIds.size;
    const zipCount = selectedZipIds.size;
    const zipFrames = zips.filter((z) => selectedZipIds.has(z.id)).reduce((n, z) => n + z.frameEntries, 0);
    showConfirm(
      'Permanently Delete Selected',
      `Permanently delete ${describeSelection(fileCount, zipCount)}?` +
        (zipCount > 0
          ? `\n\nDeleting a zip removes the zip file and its ${zipFrames.toLocaleString()} frame${zipFrames === 1 ? '' : 's'} from the catalog.`
          : '') +
        '\n\nThis action cannot be undone!',
      async () => {
        const idsArray = Array.from(selectedIds);
        const zipIds = Array.from(selectedZipIds);
        const total = idsArray.length + zipIds.length;
        setProgress({ current: 0, total, action: 'void' });

        let successCount = 0;
        const errors: string[] = [];
        let done = 0;

        for (const id of zipIds) {
          try {
            await api.invoke('send_zip_to_void', { id });
            successCount++;
          } catch (err) {
            console.error('send_zip_to_void failed:', err);
            errors.push(String(err));
          }
          setProgress({ current: ++done, total, action: 'void' });
        }
        for (const fileId of idsArray) {
          try {
            await api.invoke('send_to_void', { fileId });
            successCount++;
          } catch (err) {
            // A single frame inside a zip is refused here with the reason; it is shown below.
            console.error('send_to_void failed:', err);
            errors.push(String(err));
          }
          setProgress({ current: ++done, total, action: 'void' });
        }

        setProgress(null);
        await loadEntries();

        if (errors.length > 0) {
          showAlert('Delete Complete', `Deleted ${successCount} of ${total}. ${errors.length} error(s):\n${refusalLines(errors)}`, 'warning');
        }
      },
      true
    );
  }, [selectedIds, selectedZipIds, zips]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;

      if ((e.ctrlKey || e.metaKey) && e.key === 'a') {
        e.preventDefault();
        handleSelectAll();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [handleSelectAll]);

  // Get unique sources for filter
  const sources = Array.from(new Set([...entries.map(e => e.from_where), ...zips.map((z) => z.fromWhere)])).sort();

  // Calculate totals
  const totalSize = entries.reduce((acc, e) => acc + e.file_size, 0) + zips.reduce((acc, z) => acc + z.size, 0);
  const selectionCount = selectedIds.size + selectedZipIds.size;
  const allZipsSelected = zips.length > 0 && zips.every((z) => selectedZipIds.has(z.id));
  const someZipsSelected = zips.some((z) => selectedZipIds.has(z.id));

  return (
    <div className="p-6 h-full flex flex-col">
      {/* Header */}
      <div className="mb-4 flex-shrink-0">
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
          <div className="flex items-center gap-2 shrink-0">
            <HistoryNav />
            <div>
              <h2 className="text-3xl font-bold">Black Hole</h2>
              <p className="text-content-muted mt-1 whitespace-nowrap">
                {entries.length} file{entries.length !== 1 ? 's' : ''}
                {zips.length > 0 && <> • {zips.length} zip{zips.length !== 1 ? 's' : ''}</>} • {formatSize(totalSize)} total
                {selectionCount > 0 && (
                  <span className="text-warning ml-2">• {selectionCount} selected</span>
                )}
              </p>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2 [&_button]:whitespace-nowrap">
            {/* Action Buttons */}
            <button
              onClick={handleSelectAll}
              disabled={entries.length + zips.length === 0 || progress !== null}
              className="flex items-center gap-1.5 px-3 py-1.5 bg-surface-hover hover:brightness-90 rounded transition disabled:opacity-50 disabled:cursor-not-allowed text-sm"
            >
              <CheckSquare size={16} />
              Select All
            </button>
            <button
              onClick={handleClearSelection}
              disabled={selectionCount === 0 || progress !== null}
              className="flex items-center gap-1.5 px-3 py-1.5 bg-surface-hover hover:brightness-90 rounded transition disabled:opacity-50 disabled:cursor-not-allowed text-sm"
            >
              <XSquare size={16} />
              Clear
            </button>
            <button
              onClick={handleRestoreSelected}
              disabled={selectionCount === 0 || progress !== null}
              className="flex items-center gap-1.5 px-3 py-1.5 bg-success hover:brightness-90 rounded transition disabled:opacity-50 disabled:cursor-not-allowed text-sm"
            >
              <RotateCcw size={16} />
              Restore
            </button>
            <button
              onClick={handleVoidSelected}
              disabled={selectionCount === 0 || progress !== null}
              className="flex items-center gap-1.5 px-3 py-1.5 bg-error hover:brightness-90 rounded transition disabled:opacity-50 disabled:cursor-not-allowed text-sm"
            >
              <Trash2 size={16} />
              Send to Void
            </button>

            {/* Filter */}
            <div className="flex items-center gap-2 ml-2 pl-2 border-l border-border">
              <Filter size={20} className="text-content-muted" />
              <select
                value={filter || ''}
                onChange={(e) => setFilter(e.target.value || null)}
                className="bg-surface-elevated border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
              >
                <option value="">All sources</option>
                {sources.map(source => (
                  <option key={source} value={source}>
                    {source}
                  </option>
                ))}
              </select>
            </div>
          </div>
        </div>
      </div>

      {/* Progress indicator */}
      {progress && (
        <div className="mb-4 flex-shrink-0 bg-info-muted border border-info/50 rounded-lg p-3">
          <div className="flex items-center gap-3">
            <Loader2 className="animate-spin text-accent" size={20} />
            <span className="text-info/80">
              {progress.action === 'restore' ? 'Restoring' : 'Deleting'} {progress.current}/{progress.total}...
            </span>
          </div>
          <div className="mt-2 h-2 bg-surface-hover rounded-full overflow-hidden">
            <div
              className="h-full bg-accent transition-all duration-200"
              style={{ width: `${(progress.current / progress.total) * 100}%` }}
            />
          </div>
        </div>
      )}

      {/* Error Alert */}
      {error && (
        <div className="mb-4 flex-shrink-0 p-4 bg-error-muted border border-error/50 rounded-lg">
          <p className="text-error">Error: {error}</p>
        </div>
      )}

      {/* Main Content */}
      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Loader2 className="animate-spin mr-2" size={24} />
            <span className="text-content-muted">Loading...</span>
          </div>
        ) : entries.length === 0 && zips.length === 0 ? (
          <div className="bg-surface-elevated rounded-lg p-12 text-center">
            <RotateCcw className="mx-auto mb-4 text-content-muted" size={64} />
            <p className="text-content-muted text-lg font-semibold mb-2">Black hole is empty</p>
            <p className="text-content-muted text-sm">
              Files moved to the black hole will appear here
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            {zips.length > 0 && (
              <div className="bg-surface-elevated rounded-lg border border-border overflow-hidden" data-testid="zip-group">
                <div className="flex items-center gap-3 px-4 py-3 bg-surface-hover">
                  <FileArchive size={18} className="text-info flex-shrink-0" />
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-semibold">Zip archives</div>
                    <div className="text-xs text-content-muted">
                      Frames in a zip here stay in the catalog but are left out of stacking, export and masters. Restore puts the zip back in use.
                    </div>
                  </div>
                  <span className="text-content-muted text-sm flex-shrink-0">
                    {zips.length} zip{zips.length !== 1 ? 's' : ''} • {formatSize(zips.reduce((n, z) => n + z.size, 0))}
                  </span>
                  <button
                    onClick={handleSelectAllZips}
                    className={`flex-shrink-0 p-1 rounded transition-colors ${
                      allZipsSelected ? 'text-warning' : someZipsSelected ? 'text-warning/50' : 'text-content-muted hover:text-content-secondary'
                    }`}
                    title={allZipsSelected ? 'Deselect all zips' : 'Select all zips'}
                    aria-label={allZipsSelected ? 'Deselect all zips' : 'Select all zips'}
                  >
                    {allZipsSelected ? <CheckSquare size={18} /> : <Square size={18} />}
                  </button>
                </div>
                <div className="divide-y divide-border">
                  {zips.map((zip) => {
                    const isSelected = selectedZipIds.has(zip.id);
                    const name = zip.zipPath.split(/[/\\]/).filter(Boolean).pop() ?? zip.zipPath;
                    return (
                      <div
                        key={zip.id}
                        className={`flex items-center gap-3 px-4 py-2 transition ${isSelected ? 'bg-warning/20' : 'hover:bg-surface-hover'}`}
                      >
                        <button
                          onClick={() => toggleZip(zip.id)}
                          aria-label={`${isSelected ? 'Deselect' : 'Select'} ${name}`}
                          className={`flex-shrink-0 p-0.5 rounded transition-colors ${
                            isSelected ? 'text-warning' : 'text-content-muted hover:text-content-secondary'
                          }`}
                        >
                          {isSelected ? <CheckSquare size={16} /> : <Square size={16} />}
                        </button>
                        <div className="min-w-0 flex-1">
                          <div className="font-mono text-sm truncate" title={zip.zipPath}>{name}</div>
                          <div className="text-xs text-content-muted truncate" title={zip.zipPath}>{zip.zipPath}</div>
                        </div>
                        <Chip tone="info">{zip.frameEntries.toLocaleString()} frame{zip.frameEntries === 1 ? '' : 's'}</Chip>
                        {zip.archiveOperationId != null && (
                          <Chip tone="warn" title="Move and ZIP wrote this archive: it holds the only copy of the frames">only copy</Chip>
                        )}
                        <span className="px-2 py-0.5 bg-purple/25 text-purple text-xs rounded flex-shrink-0">{zip.fromWhere}</span>
                        <span className="text-content-muted text-xs w-20 text-right flex-shrink-0">{formatSize(zip.size)}</span>
                        <span className="text-content-muted text-xs w-32 text-right flex-shrink-0">
                          {format(new Date(zip.movedAt), 'MMM d, yyyy')}
                        </span>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}
            {folderGroups.map((group) => {
              const isExpanded = expandedFolders.has(group.path);
              const allSelected = group.files.every(f => selectedIds.has(f.file_id));
              const someSelected = group.files.some(f => selectedIds.has(f.file_id));

              return (
                <div key={group.path} className="bg-surface-elevated rounded-lg border border-border overflow-hidden">
                  {/* Folder Header */}
                  <div
                    className="flex items-center gap-3 px-4 py-3 bg-surface-hover cursor-pointer hover:bg-surface-hover transition"
                    onClick={() => toggleFolder(group.path)}
                  >
                    {isExpanded ? (
                      <ChevronDown size={18} className="text-content-muted flex-shrink-0" />
                    ) : (
                      <ChevronRight size={18} className="text-content-muted flex-shrink-0" />
                    )}
                    <Folder size={18} className="text-accent flex-shrink-0" />
                    <span className="font-mono text-sm truncate flex-1" title={group.path}>
                      {group.path}
                    </span>
                    <span className="text-content-muted text-sm flex-shrink-0">
                      {group.files.length} file{group.files.length !== 1 ? 's' : ''} • {formatSize(group.totalSize)}
                    </span>
                    {/* Select All checkbox */}
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleSelectAllInFolder(group.files);
                      }}
                      className={`flex-shrink-0 p-1 rounded transition-colors ${
                        allSelected
                          ? 'text-warning'
                          : someSelected
                            ? 'text-warning/50'
                            : 'text-content-muted hover:text-content-secondary'
                      }`}
                      title={allSelected ? 'Deselect all in folder' : 'Select all in folder'}
                    >
                      {allSelected ? <CheckSquare size={18} /> : <Square size={18} />}
                    </button>
                  </div>

                  {/* File List */}
                  {isExpanded && (
                    <div className="divide-y divide-border">
                      {group.files.map((entry) => {
                        const isSelected = selectedIds.has(entry.file_id);

                        return (
                          <div
                            key={entry.id}
                            className={`flex items-center gap-3 px-4 py-2 transition ${
                              isSelected ? 'bg-warning/20' : 'hover:bg-surface-hover'
                            }`}
                          >
                            {/* Checkbox */}
                            <button
                              onClick={(e) => handleCheckboxClick(entry.file_id, group.files, e)}
                              className={`flex-shrink-0 p-0.5 rounded transition-colors ${
                                isSelected
                                  ? 'text-warning'
                                  : 'text-content-muted hover:text-content-secondary'
                              }`}
                            >
                              {isSelected ? <CheckSquare size={16} /> : <Square size={16} />}
                            </button>

                            {/* Filename */}
                            <span className="font-mono text-sm truncate flex-1" title={entry.filename}>
                              {entry.filename}
                            </span>

                            {findZipInPath(entry.original_path) && (
                              <Chip tone="info" title="A frame inside a zip: only the whole zip can be deleted">in zip</Chip>
                            )}

                            {/* Source badge */}
                            <span className="px-2 py-0.5 bg-purple/25 text-purple text-xs rounded flex-shrink-0">
                              {entry.from_where}
                            </span>

                            {/* Size */}
                            <span className="text-content-muted text-xs w-20 text-right flex-shrink-0">
                              {formatSize(entry.file_size)}
                            </span>

                            {/* Date */}
                            <span className="text-content-muted text-xs w-32 text-right flex-shrink-0">
                              {format(new Date(entry.moved_at), 'MMM d, yyyy')}
                            </span>
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Confirm Dialog */}
      <ConfirmDialog
        isOpen={confirmDialog.isOpen}
        title={confirmDialog.title}
        message={confirmDialog.message}
        onConfirm={() => {
          setConfirmDialog({ ...confirmDialog, isOpen: false });
          confirmDialog.onConfirm();
        }}
        onCancel={() => setConfirmDialog({ ...confirmDialog, isOpen: false })}
        confirmDanger={confirmDialog.confirmDanger}
      />

      {/* Alert Dialog */}
      <AlertDialog
        isOpen={alertDialog.isOpen}
        title={alertDialog.title}
        message={alertDialog.message}
        variant={alertDialog.variant}
        onClose={() => setAlertDialog({ ...alertDialog, isOpen: false })}
      />
    </div>
  );
}
