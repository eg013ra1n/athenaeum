import { useState, useEffect, useMemo, useCallback } from 'react';
import { Trash2, RotateCcw, Loader2, Filter, ChevronDown, ChevronRight, Square, CheckSquare, XSquare, Folder } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { format } from 'date-fns';
import type { BlackHoleEntry } from '../types/models';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';

interface FolderGroup {
  path: string;
  files: BlackHoleEntry[];
  totalSize: number;
}

interface ProgressState {
  current: number;
  total: number;
  action: 'restore' | 'void';
}

export default function BlackHole() {
  const [entries, setEntries] = useState<BlackHoleEntry[]>([]);
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
      const result = await invoke<BlackHoleEntry[]>('get_black_hole_files', { filter });
      setEntries(result);
      // Clear selection when reloading
      setSelectedIds(new Set());
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

  // Get folder path from full file path
  function getFolderPath(fullPath: string): string {
    const lastSlash = Math.max(fullPath.lastIndexOf('/'), fullPath.lastIndexOf('\\'));
    return lastSlash > 0 ? fullPath.substring(0, lastSlash) : fullPath;
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
  }, [entries]);

  const handleClearSelection = useCallback(() => {
    setSelectedIds(new Set());
    setLastSelectedId(null);
  }, []);

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
    const selectedCount = selectedIds.size;
    showConfirm(
      'Restore Selected Files',
      `Restore ${selectedCount} file${selectedCount !== 1 ? 's' : ''} from the black hole?`,
      async () => {
        const idsArray = Array.from(selectedIds);
        setProgress({ current: 0, total: idsArray.length, action: 'restore' });

        let successCount = 0;
        const errors: string[] = [];

        for (let i = 0; i < idsArray.length; i++) {
          try {
            await invoke('restore_from_black_hole', { fileId: idsArray[i] });
            successCount++;
          } catch (err) {
            errors.push(String(err));
          }
          setProgress({ current: i + 1, total: idsArray.length, action: 'restore' });
        }

        setProgress(null);
        await loadEntries();

        if (errors.length > 0) {
          showAlert('Restore Complete', `Restored ${successCount} files. ${errors.length} error(s).`, 'warning');
        }
      }
    );
  }, [selectedIds]);

  const handleVoidSelected = useCallback(() => {
    const selectedCount = selectedIds.size;
    showConfirm(
      'Permanently Delete Selected',
      `Permanently delete ${selectedCount} file${selectedCount !== 1 ? 's' : ''}?\n\nThis action cannot be undone!`,
      async () => {
        const idsArray = Array.from(selectedIds);
        setProgress({ current: 0, total: idsArray.length, action: 'void' });

        let successCount = 0;
        const errors: string[] = [];

        for (let i = 0; i < idsArray.length; i++) {
          try {
            await invoke('send_to_void', { fileId: idsArray[i] });
            successCount++;
          } catch (err) {
            errors.push(String(err));
          }
          setProgress({ current: i + 1, total: idsArray.length, action: 'void' });
        }

        setProgress(null);
        await loadEntries();

        if (errors.length > 0) {
          showAlert('Delete Complete', `Deleted ${successCount} files. ${errors.length} error(s).`, 'warning');
        }
      },
      true
    );
  }, [selectedIds]);

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
  const sources = Array.from(new Set(entries.map(e => e.from_where))).sort();

  // Calculate totals
  const totalSize = entries.reduce((acc, e) => acc + e.file_size, 0);
  const selectionCount = selectedIds.size;

  return (
    <div className="p-6 h-full flex flex-col">
      {/* Header */}
      <div className="mb-4 flex-shrink-0">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-3xl font-bold">Black Hole</h2>
            <p className="text-content-muted mt-1">
              {entries.length} file{entries.length !== 1 ? 's' : ''} • {formatSize(totalSize)} total
              {selectionCount > 0 && (
                <span className="text-warning ml-2">• {selectionCount} selected</span>
              )}
            </p>
          </div>

          <div className="flex items-center gap-2">
            {/* Action Buttons */}
            <button
              onClick={handleSelectAll}
              disabled={entries.length === 0 || progress !== null}
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
        ) : entries.length === 0 ? (
          <div className="bg-surface-elevated rounded-lg p-12 text-center">
            <RotateCcw className="mx-auto mb-4 text-content-muted" size={64} />
            <p className="text-content-muted text-lg font-semibold mb-2">Black hole is empty</p>
            <p className="text-content-muted text-sm">
              Files moved to the black hole will appear here
            </p>
          </div>
        ) : (
          <div className="space-y-2">
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
