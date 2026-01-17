import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import {
  AlertTriangle,
  RefreshCw,
  Trash2,
  EyeOff,
  Eye,
  FolderSearch,
  Loader2,
  CheckCircle2,
} from 'lucide-react';
import type { MissingFileRecord } from '../types/models';

interface MissingFilesPanelProps {
  rootId: number;
  missingFiles: MissingFileRecord[];
  onRefresh: () => void;
}

export function MissingFilesPanel({ rootId, missingFiles, onRefresh }: MissingFilesPanelProps) {
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
  const [isRechecking, setIsRechecking] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [actionInProgress, setActionInProgress] = useState<number | null>(null);

  const activeMissing = missingFiles.filter(f => f.status === 'missing');
  const ignoredFiles = missingFiles.filter(f => f.status === 'ignored');

  const handleSelectAll = () => {
    if (selectedIds.size === activeMissing.length) {
      setSelectedIds(new Set());
    } else {
      setSelectedIds(new Set(activeMissing.map(f => f.file_id)));
    }
  };

  const handleToggleSelect = (fileId: number) => {
    const newSelected = new Set(selectedIds);
    if (newSelected.has(fileId)) {
      newSelected.delete(fileId);
    } else {
      newSelected.add(fileId);
    }
    setSelectedIds(newSelected);
  };

  const handleRecheckAll = async () => {
    setIsRechecking(true);
    try {
      await invoke('recheck_missing_files', { rootId });
      onRefresh();
    } catch (error) {
      console.error('Failed to recheck missing files:', error);
    } finally {
      setIsRechecking(false);
    }
  };

  const handleIgnore = async (fileId: number) => {
    setActionInProgress(fileId);
    try {
      await invoke('ignore_missing_file', { fileId });
      onRefresh();
    } catch (error) {
      console.error('Failed to ignore file:', error);
    } finally {
      setActionInProgress(null);
    }
  };

  const handleUnignore = async (fileId: number) => {
    setActionInProgress(fileId);
    try {
      await invoke('unignore_missing_file', { fileId });
      onRefresh();
    } catch (error) {
      console.error('Failed to unignore file:', error);
    } finally {
      setActionInProgress(null);
    }
  };

  const handleDelete = async (fileId: number) => {
    setActionInProgress(fileId);
    try {
      await invoke('delete_missing_files', { fileIds: [fileId] });
      onRefresh();
    } catch (error) {
      console.error('Failed to delete file:', error);
    } finally {
      setActionInProgress(null);
    }
  };

  const handleDeleteSelected = async () => {
    if (selectedIds.size === 0) return;
    setIsDeleting(true);
    try {
      await invoke('delete_missing_files', { fileIds: Array.from(selectedIds) });
      setSelectedIds(new Set());
      onRefresh();
    } catch (error) {
      console.error('Failed to delete files:', error);
    } finally {
      setIsDeleting(false);
    }
  };

  const handleIgnoreSelected = async () => {
    if (selectedIds.size === 0) return;
    try {
      for (const fileId of selectedIds) {
        await invoke('ignore_missing_file', { fileId });
      }
      setSelectedIds(new Set());
      onRefresh();
    } catch (error) {
      console.error('Failed to ignore files:', error);
    }
  };

  const handleLocate = async (file: MissingFileRecord) => {
    try {
      const selected = await open({
        title: `Locate ${file.filename}`,
        filters: [
          { name: 'FITS/XISF Files', extensions: ['fits', 'fit', 'xisf'] },
          { name: 'All Files', extensions: ['*'] },
        ],
      });

      if (selected && typeof selected === 'string') {
        setActionInProgress(file.file_id);
        await invoke('relocate_missing_file', {
          fileId: file.file_id,
          newPath: selected,
        });
        onRefresh();
        setActionInProgress(null);
      }
    } catch (error) {
      console.error('Failed to relocate file:', error);
      setActionInProgress(null);
    }
  };

  const formatDate = (dateStr: string) => {
    try {
      return new Date(dateStr).toLocaleDateString();
    } catch {
      return dateStr;
    }
  };

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  };

  if (missingFiles.length === 0) {
    return (
      <div className="flex items-center gap-2 p-3 bg-green-900/20 border border-green-700 rounded-lg text-green-400">
        <CheckCircle2 size={16} />
        <span className="text-sm">All files present</span>
      </div>
    );
  }

  return (
    <div className="border border-orange-700/50 rounded-lg bg-orange-900/10 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 bg-orange-900/20 border-b border-orange-700/50">
        <div className="flex items-center gap-2">
          <AlertTriangle className="text-orange-400" size={18} />
          <span className="font-medium text-orange-400">
            {activeMissing.length} Missing File{activeMissing.length !== 1 ? 's' : ''}
            {ignoredFiles.length > 0 && (
              <span className="text-gray-400 font-normal ml-2">
                ({ignoredFiles.length} ignored)
              </span>
            )}
          </span>
        </div>
        <button
          onClick={handleRecheckAll}
          disabled={isRechecking}
          className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-orange-600 hover:bg-orange-700 disabled:opacity-50 rounded-lg transition"
        >
          {isRechecking ? (
            <Loader2 className="animate-spin" size={14} />
          ) : (
            <RefreshCw size={14} />
          )}
          Recheck All
        </button>
      </div>

      {/* Missing files list */}
      {activeMissing.length > 0 && (
        <div className="max-h-64 overflow-y-auto">
          <table className="w-full text-sm">
            <thead className="bg-gray-800/50 sticky top-0">
              <tr>
                <th className="w-8 p-2">
                  <input
                    type="checkbox"
                    checked={selectedIds.size === activeMissing.length && activeMissing.length > 0}
                    onChange={handleSelectAll}
                    className="rounded"
                  />
                </th>
                <th className="text-left p-2 font-medium text-gray-400">File</th>
                <th className="text-left p-2 font-medium text-gray-400">Object</th>
                <th className="text-left p-2 font-medium text-gray-400">Size</th>
                <th className="text-left p-2 font-medium text-gray-400">Detected</th>
                <th className="w-32 p-2"></th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-700/50">
              {activeMissing.map((file) => (
                <tr key={file.file_id} className="hover:bg-gray-800/30">
                  <td className="p-2">
                    <input
                      type="checkbox"
                      checked={selectedIds.has(file.file_id)}
                      onChange={() => handleToggleSelect(file.file_id)}
                      className="rounded"
                    />
                  </td>
                  <td className="p-2">
                    <div className="font-mono text-xs text-gray-300 truncate max-w-xs" title={file.path}>
                      {file.filename}
                    </div>
                  </td>
                  <td className="p-2 text-gray-400">
                    {file.object || '-'}
                  </td>
                  <td className="p-2 text-gray-400">
                    {formatSize(file.size)}
                  </td>
                  <td className="p-2 text-gray-400">
                    {formatDate(file.detected_at)}
                  </td>
                  <td className="p-2">
                    <div className="flex items-center gap-1 justify-end">
                      <button
                        onClick={() => handleLocate(file)}
                        disabled={actionInProgress === file.file_id}
                        className="p-1.5 hover:bg-blue-600/30 rounded text-blue-400 transition"
                        title="Locate file"
                      >
                        <FolderSearch size={14} />
                      </button>
                      <button
                        onClick={() => handleIgnore(file.file_id)}
                        disabled={actionInProgress === file.file_id}
                        className="p-1.5 hover:bg-gray-600/30 rounded text-gray-400 transition"
                        title="Ignore"
                      >
                        <EyeOff size={14} />
                      </button>
                      <button
                        onClick={() => handleDelete(file.file_id)}
                        disabled={actionInProgress === file.file_id}
                        className="p-1.5 hover:bg-red-600/30 rounded text-red-400 transition"
                        title="Delete from database"
                      >
                        <Trash2 size={14} />
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Ignored files section */}
      {ignoredFiles.length > 0 && (
        <div className="border-t border-gray-700/50">
          <details className="group">
            <summary className="flex items-center gap-2 p-3 cursor-pointer text-gray-400 hover:text-gray-300">
              <EyeOff size={14} />
              <span className="text-sm">{ignoredFiles.length} ignored file{ignoredFiles.length !== 1 ? 's' : ''}</span>
            </summary>
            <div className="max-h-32 overflow-y-auto border-t border-gray-700/30">
              {ignoredFiles.map((file) => (
                <div
                  key={file.file_id}
                  className="flex items-center justify-between p-2 px-3 hover:bg-gray-800/30 text-sm"
                >
                  <span className="font-mono text-xs text-gray-500 truncate flex-1" title={file.path}>
                    {file.filename}
                  </span>
                  <button
                    onClick={() => handleUnignore(file.file_id)}
                    disabled={actionInProgress === file.file_id}
                    className="flex items-center gap-1 px-2 py-1 text-xs text-gray-400 hover:text-gray-300 hover:bg-gray-700 rounded transition"
                  >
                    <Eye size={12} />
                    Unignore
                  </button>
                </div>
              ))}
            </div>
          </details>
        </div>
      )}

      {/* Bulk actions */}
      {selectedIds.size > 0 && (
        <div className="flex items-center gap-2 p-3 bg-gray-800/50 border-t border-gray-700/50">
          <span className="text-sm text-gray-400">
            {selectedIds.size} selected
          </span>
          <div className="flex-1" />
          <button
            onClick={handleIgnoreSelected}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-gray-300 hover:bg-gray-700 rounded-lg transition"
          >
            <EyeOff size={14} />
            Ignore
          </button>
          <button
            onClick={handleDeleteSelected}
            disabled={isDeleting}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-red-400 hover:bg-red-900/30 rounded-lg transition"
          >
            {isDeleting ? (
              <Loader2 className="animate-spin" size={14} />
            ) : (
              <Trash2 size={14} />
            )}
            Delete from DB
          </button>
        </div>
      )}
    </div>
  );
}
