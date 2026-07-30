import { useState, useEffect, useCallback } from 'react';
import { Folder, ArrowUp, Loader2, X } from 'lucide-react';
import { api } from '../api';

interface BrowseDirectoryEntry {
  name: string;
  path: string;
}

interface BrowseDirectoriesResponse {
  current: string;
  parent: string | null;
  directories: BrowseDirectoryEntry[];
}

interface FolderBrowserModalProps {
  isOpen: boolean;
  onSelect: (path: string) => void;
  onClose: () => void;
  /** `"scan"` (default) browses scan roots; `"export"` browses the export directory. */
  scope?: 'scan' | 'export';
}

export const FolderBrowserModal: React.FC<FolderBrowserModalProps> = ({
  isOpen,
  onSelect,
  onClose,
  scope,
}) => {
  const [currentPath, setCurrentPath] = useState('');
  const [inputPath, setInputPath] = useState('');
  const [directories, setDirectories] = useState<BrowseDirectoryEntry[]>([]);
  const [parentPath, setParentPath] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const browse = useCallback(async (path: string) => {
    setLoading(true);
    setError(null);
    try {
      const result = await api.invoke<BrowseDirectoriesResponse>('browse_directories', {
        path: path || undefined,
        scope: scope || undefined,
      });
      setCurrentPath(result.current);
      setInputPath(result.current);
      setParentPath(result.parent);
      setDirectories(result.directories);
    } catch (err) {
      setError(typeof err === 'string' ? err : 'Failed to browse directory');
    } finally {
      setLoading(false);
    }
  }, [scope]);

  // Load root listing when modal opens
  useEffect(() => {
    if (isOpen) {
      browse('');
    }
  }, [isOpen, browse]);

  const handleNavigate = (path: string) => {
    browse(path);
  };

  const handleInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      browse(inputPath);
    }
  };

  const handleConfirm = () => {
    if (currentPath && currentPath !== '/') {
      onSelect(currentPath);
    }
  };

  if (!isOpen) return null;

  const isRoot = currentPath === '/' || currentPath === '';

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-surface-elevated rounded-lg w-full max-w-xl mx-4 border border-border shadow-xl flex flex-col max-h-[80vh]">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h3 className="text-lg font-semibold">Select Directory</h3>
          <button
            onClick={onClose}
            className="p-1 hover:bg-surface-hover rounded transition"
          >
            <X size={20} />
          </button>
        </div>

        {/* Path input */}
        <div className="p-4 border-b border-border">
          <input
            type="text"
            value={inputPath}
            onChange={(e) => setInputPath(e.target.value)}
            onKeyDown={handleInputKeyDown}
            placeholder="Enter path and press Enter"
            className="w-full px-3 py-2 bg-surface border border-border rounded font-mono text-sm focus:outline-none focus:ring-2 focus:ring-accent"
          />
        </div>

        {/* Directory listing */}
        <div className="flex-1 overflow-y-auto min-h-0">
          {error && (
            <div className="m-4 p-3 bg-error-muted border border-error/50 rounded">
              <p className="text-error text-sm">{error}</p>
            </div>
          )}

          {loading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="animate-spin mr-2" size={20} />
              <span className="text-content-muted">Loading...</span>
            </div>
          ) : (
            <div className="divide-y divide-border">
              {/* Parent directory row */}
              {parentPath !== null && (
                <button
                  onClick={() => handleNavigate(parentPath)}
                  className="w-full flex items-center gap-3 px-4 py-3 hover:bg-surface-hover transition text-left"
                >
                  <ArrowUp size={18} className="text-content-muted flex-shrink-0" />
                  <span className="text-content-secondary">..</span>
                </button>
              )}
              {/* Back to root when parent is null but not at root */}
              {parentPath === null && !isRoot && (
                <button
                  onClick={() => handleNavigate('')}
                  className="w-full flex items-center gap-3 px-4 py-3 hover:bg-surface-hover transition text-left"
                >
                  <ArrowUp size={18} className="text-content-muted flex-shrink-0" />
                  <span className="text-content-secondary">..</span>
                </button>
              )}

              {directories.length === 0 && !loading && (
                <div className="px-4 py-8 text-center text-content-muted text-sm">
                  No subdirectories
                </div>
              )}

              {directories.map((dir) => (
                <button
                  key={dir.path}
                  onClick={() => handleNavigate(dir.path)}
                  className="w-full flex items-center gap-3 px-4 py-3 hover:bg-surface-hover transition text-left"
                >
                  <Folder size={18} className="text-accent flex-shrink-0" />
                  <span className="truncate">{dir.name}</span>
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between p-4 border-t border-border">
          <span
            className="text-xs text-content-muted truncate mr-4 font-mono"
            title={isRoot ? undefined : currentPath}
          >
            {isRoot ? 'Select a directory' : currentPath}
          </span>
          <div className="flex gap-3 flex-shrink-0">
            <button
              onClick={onClose}
              className="px-4 py-2 bg-surface-hover text-content rounded hover:brightness-110 transition"
            >
              Cancel
            </button>
            <button
              onClick={handleConfirm}
              disabled={isRoot}
              className="px-4 py-2 bg-accent text-white rounded hover:bg-accent-hover transition disabled:opacity-50 disabled:cursor-not-allowed"
            >
              Select
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
