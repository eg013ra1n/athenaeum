import { useState, useEffect, useCallback, useRef } from 'react';
import { File as FileIcon, Folder, ArrowUp, Loader2, X } from 'lucide-react';
import { api } from '../api';
import { formatBytes } from './folders/format';

interface BrowseDirectoryEntry {
  name: string;
  path: string;
}

interface BrowseFileEntry {
  name: string;
  path: string;
  size: number;
}

interface BrowseDirectoriesResponse {
  current: string;
  parent: string | null;
  directories: BrowseDirectoryEntry[];
  /** Present only when the request asked for files (`includeFiles`). */
  files?: BrowseFileEntry[];
}

interface FolderBrowserModalProps {
  isOpen: boolean;
  onSelect: (path: string) => void;
  onClose: () => void;
  /** `"scan"` (default) browses scan roots; `"export"` browses the export
   *  directory; `"stacking"` browses the same roots as `"scan"` (the Stacking
   *  tab's Output panel folder picker, plan 5b Task 1/3, plan Ruling 6). */
  scope?: 'scan' | 'export' | 'stacking';
  /** `'folder'` (default) picks the folder being viewed; `'file'` lists files
   *  too and picks one of them. */
  mode?: 'folder' | 'file';
  /** Header text; defaults to `Select Directory` / `Select File`. */
  title?: string;
  /** File mode only: the extension filters, in the shape `pickFile` takes. A
   *  filter containing `'*'` lists every file; with more than one filter a
   *  selector appears under the path box. */
  fileFilters?: { name: string; extensions: string[] }[];
}

export const FolderBrowserModal: React.FC<FolderBrowserModalProps> = ({
  isOpen,
  onSelect,
  onClose,
  scope,
  mode = 'folder',
  title,
  fileFilters,
}) => {
  const [currentPath, setCurrentPath] = useState('');
  const [inputPath, setInputPath] = useState('');
  const [directories, setDirectories] = useState<BrowseDirectoryEntry[]>([]);
  const [parentPath, setParentPath] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [files, setFiles] = useState<BrowseFileEntry[]>([]);
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [activeFilter, setActiveFilter] = useState(0);
  const isFileMode = mode === 'file';

  // A caller may pass the filter list inline; reading it through a ref keeps
  // `browse` (and with it the open-effect below) from re-running every render.
  const filtersRef = useRef(fileFilters);
  filtersRef.current = fileFilters;

  // Orders the answers: every request takes a number, and an answer whose
  // number is no longer the latest (a newer request, or the modal closed and
  // reopened since) is dropped without touching state.
  const seq = useRef(0);

  const browse = useCallback(async (path: string, filterIndex: number) => {
    const mine = ++seq.current;
    setLoading(true);
    setError(null);
    setFiles([]);
    setSelectedFile(null);
    try {
      const request: Record<string, unknown> = {
        path: path || undefined,
        scope: scope || undefined,
      };
      if (mode === 'file') {
        request.includeFiles = true;
        const extensions = filtersRef.current?.[filterIndex]?.extensions;
        if (extensions && !extensions.includes('*')) {
          request.extensions = extensions;
        }
      }
      const result = await api.invoke<BrowseDirectoriesResponse>('browse_directories', request);
      if (mine !== seq.current) return;
      setCurrentPath(result.current);
      setInputPath(result.current);
      setParentPath(result.parent);
      setDirectories(result.directories);
      setFiles(result.files ?? []);
    } catch (err) {
      if (mine !== seq.current) {
        console.warn('[FolderBrowserModal] superseded browse failed:', err);
        return;
      }
      console.error('[FolderBrowserModal] browse failed:', err);
      setError(typeof err === 'string' ? err : 'Failed to browse directory');
    } finally {
      if (mine === seq.current) setLoading(false);
    }
  }, [scope, mode]);

  // Load root listing when modal opens; closing drops any answer still in flight.
  useEffect(() => {
    if (isOpen) {
      setActiveFilter(0);
      browse('', 0);
    } else {
      seq.current += 1;
      setLoading(false);
    }
  }, [isOpen, browse]);

  const handleNavigate = (path: string) => {
    browse(path, activeFilter);
  };

  const handleFilterChange = (index: number) => {
    setActiveFilter(index);
    browse(currentPath, index);
  };

  const handleInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      browse(inputPath, activeFilter);
    }
  };

  const handleConfirm = () => {
    if (isFileMode) {
      if (selectedFile) onSelect(selectedFile);
      return;
    }
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
          <h3 className="text-lg font-semibold">
            {title ?? (isFileMode ? 'Select File' : 'Select Directory')}
          </h3>
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
          {isFileMode && fileFilters && fileFilters.length > 1 && (
            <select
              value={activeFilter}
              onChange={(e) => handleFilterChange(Number(e.target.value))}
              aria-label="File type"
              className="mt-2 px-2 py-1 bg-surface border border-border rounded text-sm"
            >
              {fileFilters.map((filter, index) => (
                <option key={filter.name} value={index}>
                  {filter.name}
                </option>
              ))}
            </select>
          )}
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

              {directories.length === 0 && !loading && !isFileMode && (
                <div className="px-4 py-8 text-center text-content-muted text-sm">
                  No subdirectories
                </div>
              )}
              {isFileMode && directories.length === 0 && files.length === 0 && !loading && (
                <div className="px-4 py-8 text-center text-content-muted text-sm">
                  No matching files
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

              {isFileMode &&
                files.map((file) => {
                  const selected = selectedFile === file.path;
                  return (
                    <button
                      key={file.path}
                      onClick={() => setSelectedFile(file.path)}
                      onDoubleClick={() => onSelect(file.path)}
                      aria-selected={selected}
                      className={`w-full flex items-center gap-3 px-4 py-3 hover:bg-surface-hover transition text-left ${
                        selected ? 'bg-accent/30 text-accent' : ''
                      }`}
                    >
                      <FileIcon size={18} className="text-content-muted flex-shrink-0" />
                      <span className="truncate flex-1">{file.name}</span>
                      <span className="text-xs text-content-muted">{formatBytes(file.size)}</span>
                    </button>
                  );
                })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between p-4 border-t border-border">
          <span
            className="text-xs text-content-muted truncate mr-4 font-mono"
            title={isFileMode ? (selectedFile ?? undefined) : isRoot ? undefined : currentPath}
          >
            {isFileMode
              ? (selectedFile ?? 'Select a file')
              : isRoot
                ? 'Select a directory'
                : currentPath}
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
              disabled={isFileMode ? selectedFile === null : isRoot}
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
