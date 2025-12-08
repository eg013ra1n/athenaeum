import { useState, useEffect } from 'react';
import { Trash2, RotateCcw, AlertTriangle, Loader2, Filter } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { format } from 'date-fns';
import type { BlackHoleEntry } from '../types/models';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';

export default function BlackHole() {
  const [entries, setEntries] = useState<BlackHoleEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<string | null>(null);
  const [actionLoading, setActionLoading] = useState<Record<number, boolean>>({});
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

  // Restore a file from black hole
  const handleRestore = async (fileId: number) => {
    showConfirm(
      'Restore File',
      'Restore this file from the black hole?',
      async () => {
        try {
          setActionLoading(prev => ({ ...prev, [fileId]: true }));
          await invoke('restore_from_black_hole', { fileId });
          await loadEntries();
        } catch (err) {
          console.error('Failed to restore file:', err);
          showAlert('Restore Failed', `Failed to restore file: ${String(err)}`, 'error');
        } finally {
          setActionLoading(prev => ({ ...prev, [fileId]: false }));
        }
      }
    );
  };

  // Send a file to void (permanent deletion)
  const handleSendToVoid = async (fileId: number, filename: string) => {
    showConfirm(
      'Permanent Delete',
      `Permanently delete "${filename}"?\n\nThis action cannot be undone!`,
      async () => {
        try {
          setActionLoading(prev => ({ ...prev, [fileId]: true }));
          await invoke('send_to_void', { fileId });
          await loadEntries();
        } catch (err) {
          console.error('Failed to send file to void:', err);
          showAlert('Delete Failed', `Failed to delete file: ${String(err)}`, 'error');
        } finally {
          setActionLoading(prev => ({ ...prev, [fileId]: false }));
        }
      },
      true
    );
  };

  // Send all files to void
  const handleSendAllToVoid = async () => {
    showConfirm(
      'Permanent Delete All',
      `Permanently delete ALL ${entries.length} files in the black hole?\n\nThis action cannot be undone!`,
      async () => {
        try {
          setLoading(true);
          await invoke<number>('send_all_to_void');
          // Success - silent update
          await loadEntries();
        } catch (err) {
          console.error('Failed to send all to void:', err);
          showAlert('Delete Failed', `Failed to delete files: ${String(err)}`, 'error');
        } finally {
          setLoading(false);
        }
      },
      true
    );
  };

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

  // Get unique sources for filter
  const sources = Array.from(new Set(entries.map(e => e.from_where))).sort();

  return (
    <div className="p-6">
      <div className="mb-6">
        <h2 className="text-3xl font-bold mb-2">Black Hole</h2>
        <p className="text-gray-400">
          Soft-deleted files are held here. Restore them or send them to the void (permanent deletion).
        </p>
      </div>

      {/* Filter Bar */}
      <div className="mb-6 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Filter size={20} className="text-gray-400" />
          <select
            value={filter || ''}
            onChange={(e) => setFilter(e.target.value || null)}
            className="bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          >
            <option value="">All sources ({entries.length})</option>
            {sources.map(source => (
              <option key={source} value={source}>
                From: {source}
              </option>
            ))}
          </select>
        </div>

        {entries.length > 0 && (
          <button
            onClick={handleSendAllToVoid}
            disabled={loading}
            className="flex items-center gap-2 px-4 py-2 bg-red-600 hover:bg-red-700 rounded transition disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Trash2 size={18} />
            Send All to Void
          </button>
        )}
      </div>

      {/* Error Alert */}
      {error && (
        <div className="mb-6 p-4 bg-red-900/30 border border-red-700 rounded-lg">
          <p className="text-red-400">Error: {error}</p>
        </div>
      )}

      {/* Loading State */}
      {loading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="animate-spin mr-2" size={24} />
          <span className="text-gray-400">Loading...</span>
        </div>
      ) : entries.length === 0 ? (
        /* Empty State */
        <div className="bg-gray-800 rounded-lg p-12 text-center">
          <RotateCcw className="mx-auto mb-4 text-gray-600" size={64} />
          <p className="text-gray-500 text-lg font-semibold mb-2">Black hole is empty</p>
          <p className="text-gray-600 text-sm">
            Files moved to the black hole will appear here
          </p>
        </div>
      ) : (
        /* Entries List */
        <div className="space-y-3">
          {entries.map((entry) => {
            const isActionLoading = entry.file_id ? actionLoading[entry.file_id] : false;

            return (
              <div
                key={entry.id}
                className="bg-gray-800 rounded-lg p-4 border border-gray-700 hover:border-gray-600 transition"
              >
                <div className="flex items-start justify-between">
                  <div className="flex-1 min-w-0">
                    {/* Filename */}
                    <div className="flex items-center gap-2 mb-2">
                      <span className="font-semibold text-lg truncate">{entry.filename}</span>
                      <span className="px-2 py-0.5 bg-purple-900/40 text-purple-300 text-xs rounded">
                        {entry.from_where}
                      </span>
                    </div>

                    {/* Original Path */}
                    <div className="text-sm text-gray-400 mb-2">
                      <span className="font-mono truncate block" title={entry.original_path}>
                        {entry.original_path}
                      </span>
                    </div>

                    {/* Metadata */}
                    <div className="flex items-center gap-4 text-xs text-gray-500">
                      <span>{formatSize(entry.file_size)}</span>
                      <span>•</span>
                      <span>
                        Moved: {format(new Date(entry.moved_at), 'MMM d, yyyy HH:mm:ss')}
                      </span>
                    </div>
                  </div>

                  {/* Actions */}
                  <div className="flex items-center gap-2 ml-4">
                    <button
                      onClick={() => handleRestore(entry.file_id)}
                      disabled={isActionLoading}
                      className="flex items-center gap-2 px-3 py-2 bg-green-600 hover:bg-green-700 rounded transition disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      {isActionLoading ? (
                        <Loader2 className="animate-spin" size={16} />
                      ) : (
                        <RotateCcw size={16} />
                      )}
                      Restore
                    </button>
                    <button
                      onClick={() => handleSendToVoid(entry.file_id, entry.filename)}
                      disabled={isActionLoading}
                      className="flex items-center gap-2 px-3 py-2 bg-red-600 hover:bg-red-700 rounded transition disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <AlertTriangle size={16} />
                      Send to Void
                    </button>
                  </div>
                </div>
              </div>
            );
          })}

          {/* Summary */}
          <div className="bg-purple-900/20 border border-purple-700 rounded-lg p-4 mt-6">
            <div className="flex items-center justify-between">
              <div>
                <p className="font-semibold text-purple-300">Black Hole Summary</p>
                <p className="text-sm text-gray-400 mt-1">
                  {entries.length} file{entries.length !== 1 ? 's' : ''} awaiting decision
                </p>
              </div>
              <div className="text-right">
                <p className="text-2xl font-bold text-purple-300">
                  {formatSize(entries.reduce((acc, e) => acc + e.file_size, 0))}
                </p>
                <p className="text-sm text-gray-400">total size</p>
              </div>
            </div>
          </div>
        </div>
      )}

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
