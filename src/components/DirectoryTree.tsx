import { useState, useEffect, useMemo, useCallback } from 'react';
import { Folder, File as FileIcon, ArrowLeft, AlertCircle, Copy, Check } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import type { ScanRoot, DuplicateGroup, DirectoryContents } from '../types/models';

interface DirectoryTreeProps {
  scanRoots: ScanRoot[];
  duplicates: DuplicateGroup[];
  refreshTrigger: number;
}

export default function DirectoryTree({ scanRoots, duplicates, refreshTrigger }: DirectoryTreeProps) {
  const [currentPath, setCurrentPath] = useState<string>('');
  const [contents, setContents] = useState<DirectoryContents | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // Create a set of filenames that have duplicates for quick lookup
  const duplicateFilenames = useMemo(() => {
    const set = new Set<string>();
    duplicates.forEach(group => {
      // The content_hash field now contains the filename
      set.add(group.content_hash);
    });
    return set;
  }, [duplicates]);

  // Load directory contents
  const loadDirectory = useCallback(async (path: string) => {
    if (!path) return;

    setLoading(true);
    setError(null);

    try {
      const result = await invoke<DirectoryContents>('get_directory_contents', {
        directoryPath: path
      });
      setContents(result);
      setCurrentPath(path);
    } catch (e) {
      setError(e as string);
      setContents(null);
    } finally {
      setLoading(false);
    }
  }, []);

  // Initialize with first scan root
  useEffect(() => {
    if (scanRoots.length > 0 && !currentPath) {
      loadDirectory(scanRoots[0].path);
    }
  }, [scanRoots, currentPath, loadDirectory]);

  // Refresh when trigger changes
  useEffect(() => {
    if (currentPath && refreshTrigger > 0) {
      loadDirectory(currentPath);
    }
  }, [refreshTrigger, currentPath, loadDirectory]);

  // Navigate up one level
  const goUp = () => {
    const parentPath = currentPath.split('/').slice(0, -1).join('/');
    if (parentPath) {
      loadDirectory(parentPath);
    }
  };

  // Navigate to subdirectory
  const navigateToDirectory = (dirPath: string) => {
    loadDirectory(dirPath);
  };

  // Copy path to clipboard
  const copyPathToClipboard = async () => {
    try {
      await navigator.clipboard.writeText(currentPath);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (error) {
      console.error('Failed to copy path:', error);
    }
  };

  // Check if current path is a root path
  const isAtRoot = scanRoots.some(root => root.path === currentPath);

  return (
    <div className="bg-gray-800 rounded-lg overflow-hidden">
      {/* Header with navigation */}
      <div className="bg-gray-750 border-b border-gray-700 p-4">
        <div className="flex items-center gap-3 mb-3">
          <button
            onClick={goUp}
            disabled={isAtRoot || loading}
            className="flex items-center gap-2 px-3 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <ArrowLeft size={16} />
            Back
          </button>
          <div className="flex-1 flex items-center gap-2 min-w-0">
            <Folder size={20} className="text-blue-400 flex-shrink-0" />
            <span className="font-mono text-sm text-gray-300 truncate" title={currentPath}>
              {currentPath || 'Select a directory'}
            </span>
            {currentPath && (
              <button
                onClick={copyPathToClipboard}
                className="flex items-center gap-1 px-2 py-1 bg-gray-700 hover:bg-gray-600 rounded text-xs transition flex-shrink-0"
                title="Copy path to clipboard"
              >
                {copied ? (
                  <>
                    <Check size={14} className="text-green-400" />
                    <span className="text-green-400">Copied!</span>
                  </>
                ) : (
                  <>
                    <Copy size={14} />
                    <span>Copy</span>
                  </>
                )}
              </button>
            )}
          </div>
          {contents && (
            <span className="text-sm text-gray-400 flex-shrink-0">
              {contents.subdirectories.length} folders, {contents.files.length} files
            </span>
          )}
        </div>

        {/* Root selector */}
        <div className="flex gap-2 flex-wrap">
          {scanRoots.map(root => (
            <button
              key={root.id}
              onClick={() => loadDirectory(root.path)}
              className={`px-3 py-1 rounded text-sm transition ${
                currentPath === root.path
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
              }`}
            >
              {root.path.split('/').pop() || root.path}
            </button>
          ))}
        </div>
      </div>

      {/* Content area */}
      <div className="p-4">
        {loading && (
          <div className="text-center py-12 text-gray-400">
            Loading directory...
          </div>
        )}

        {error && (
          <div className="bg-red-900/30 border border-red-700 rounded-lg p-4 mb-4">
            <div className="flex items-center gap-2 text-red-400">
              <AlertCircle size={20} />
              <span>Error: {error}</span>
            </div>
          </div>
        )}

        {!loading && contents && (
          <div className="space-y-4">
            {/* Subdirectories */}
            {contents.subdirectories.length > 0 && (
              <div>
                <h3 className="text-sm font-semibold text-gray-400 mb-2 uppercase">Folders</h3>
                <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-2">
                  {contents.subdirectories.map((subdir) => (
                    <button
                      key={subdir}
                      onClick={() => navigateToDirectory(subdir)}
                      className="flex items-center gap-3 p-3 bg-gray-750 hover:bg-gray-700 rounded-lg transition text-left group"
                    >
                      <Folder size={20} className="text-blue-400 flex-shrink-0" />
                      <span className="text-sm truncate font-mono group-hover:text-blue-300">
                        {subdir.split('/').pop()}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            )}

            {/* Files */}
            {contents.files.length > 0 && (
              <div>
                <h3 className="text-sm font-semibold text-gray-400 mb-2 uppercase">Files</h3>
                <div className="bg-gray-750 rounded-lg overflow-hidden">
                  {/* Table header */}
                  <div className="grid grid-cols-12 gap-4 px-4 py-3 bg-gray-800 text-xs font-semibold text-gray-400 uppercase border-b border-gray-700">
                    <div className="col-span-4">Filename</div>
                    <div className="col-span-2">Object</div>
                    <div className="col-span-2">Camera</div>
                    <div className="col-span-1">Filter</div>
                    <div className="col-span-1 text-right">Exposure</div>
                    <div className="col-span-1 text-right">Type</div>
                    <div className="col-span-1 text-right">Focal Length</div>
                    <div className="col-span-1 text-right">Temperature</div>
                  </div>

                  {/* File rows */}
                  <div className="divide-y divide-gray-700">
                    {contents.files.map((item, idx) => {
                      const hasDuplicate = duplicateFilenames.has(item.file.filename);

                      return (
                        <div
                          key={item.file.id || idx}
                          className="grid grid-cols-12 gap-4 px-4 py-3 hover:bg-gray-700 transition items-center"
                        >
                          <div className="col-span-4 flex items-center gap-2 min-w-0">
                            <FileIcon size={14} className="text-gray-500 flex-shrink-0" />
                            {hasDuplicate && (
                              <span title="Duplicate file">
                                <AlertCircle size={14} className="text-yellow-500 flex-shrink-0" />
                              </span>
                            )}
                            <span className="font-mono text-sm truncate" title={item.file.filename}>
                              {item.file.filename}
                            </span>
                          </div>
                          <div className="col-span-2 truncate text-sm text-gray-300">
                            {item.frame?.object || '-'}
                          </div>
                          <div className="col-span-2 truncate text-sm text-gray-400">
                            {item.frame?.instrume || '-'}
                          </div>
                          <div className="col-span-1 truncate text-sm text-gray-400">
                            {item.frame?.filter || '-'}
                          </div>
                          <div className="col-span-1 text-right text-sm text-gray-400">
                            {item.frame?.exptime ? `${item.frame.exptime}s` : '-'}
                          </div>
                          <div className="col-span-1 text-right">
                            {item.frame?.imagetyp && (
                              <span className={`px-2 py-0.5 rounded text-xs ${
                                item.frame.imagetyp === 'Light'
                                  ? 'bg-blue-900 text-blue-200'
                                  : 'bg-gray-700 text-gray-300'
                              }`}>
                                {item.frame.imagetyp}
                              </span>
                            )}
                          </div>
                          <div className="col-span-1 text-right text-sm text-gray-500">
                            {item.frame?.focallen ? `${item.frame.focallen}mm` : '-'}
                          </div>
                          <div className="col-span-1 text-right text-sm text-gray-500">
                            {item.frame?.ccd_temp ? `${item.frame.ccd_temp}°C` : '-'}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              </div>
            )}

            {/* Empty state */}
            {contents.subdirectories.length === 0 && contents.files.length === 0 && (
              <div className="text-center py-12 text-gray-500">
                No folders or files found in this directory.
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}