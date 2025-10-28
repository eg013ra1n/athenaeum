import { useState, useEffect, useMemo, useCallback } from 'react';
import { Folder, File as FileIcon, ArrowLeft, AlertCircle, Copy, Check } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import type { ScanRoot, DuplicateGroup, DirectoryContents, FileWithFrame } from '../types/models';

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
  const [hoveredFile, setHoveredFile] = useState<FileWithFrame | null>(null);

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

  // Generate breadcrumb parts from current path
  const getBreadcrumbs = () => {
    if (!currentPath) return [];

    // Find which root this path belongs to
    const matchingRoot = scanRoots.find(root => currentPath.startsWith(root.path));
    if (!matchingRoot) return [];

    // Split the path into parts
    const parts = currentPath.split('/').filter(p => p);
    const rootParts = matchingRoot.path.split('/').filter(p => p);

    // Build breadcrumbs
    const breadcrumbs: { label: string; path: string; isClickable: boolean }[] = [];
    let accumulatedPath = '';

    parts.forEach((part, index) => {
      accumulatedPath += '/' + part;
      const isWithinRoot = index >= rootParts.length - 1; // Can click on root and anything after it

      breadcrumbs.push({
        label: part,
        path: accumulatedPath,
        isClickable: isWithinRoot && accumulatedPath !== currentPath, // Can't click on current
      });
    });

    return breadcrumbs;
  };

  const breadcrumbs = getBreadcrumbs();

  return (
    <div className="bg-gray-800 rounded-lg overflow-hidden">
      {/* Header with navigation */}
      <div className="bg-gray-750 border-b border-gray-700 p-4">
        {/* Root selector */}
        <div className="flex gap-2 flex-wrap mb-3">
          {scanRoots.map(root => (
            <button
              key={root.id}
              onClick={() => loadDirectory(root.path)}
              className={`px-3 py-1.5 rounded text-sm transition ${
                currentPath === root.path
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
              }`}
            >
              {root.path.split('/').pop() || root.path}
            </button>
          ))}
        </div>

        <div className="flex items-center gap-3">
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
            {breadcrumbs.length > 0 ? (
              <div className="flex items-center gap-1 font-mono text-sm min-w-0 flex-wrap">
                {breadcrumbs.map((crumb, index) => (
                  <div key={crumb.path} className="flex items-center gap-1">
                    {crumb.isClickable ? (
                      <button
                        onClick={() => loadDirectory(crumb.path)}
                        className="text-blue-400 hover:text-blue-300 hover:underline transition"
                        title={`Navigate to ${crumb.path}`}
                      >
                        {crumb.label}
                      </button>
                    ) : (
                      <span className={crumb.path === currentPath ? 'text-gray-200' : 'text-gray-500'}>
                        {crumb.label}
                      </span>
                    )}
                    {index < breadcrumbs.length - 1 && (
                      <span className="text-gray-600">/</span>
                    )}
                  </div>
                ))}
              </div>
            ) : (
              <span className="font-mono text-sm text-gray-300">Select a directory</span>
            )}
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
              <span>Error: {String(error)}</span>
            </div>
          </div>
        )}

        {!loading && contents && (
          <div className="flex gap-4 h-[calc(100vh-250px)]">
            {/* Left Column - Files and Folders List */}
            <div className="flex-1 overflow-y-auto">
              {/* Subdirectories */}
              {contents.subdirectories.length > 0 && (
                <div className="mb-4">
                  {contents.subdirectories.map((subdir) => (
                    <button
                      key={subdir}
                      onClick={() => navigateToDirectory(subdir)}
                      className="flex items-center gap-3 w-full px-3 py-2 hover:bg-gray-700 transition text-left group border-b border-gray-800"
                    >
                      <Folder size={18} className="text-blue-400 flex-shrink-0" />
                      <span className="text-sm truncate font-mono group-hover:text-blue-300">
                        {subdir.split('/').pop()}
                      </span>
                    </button>
                  ))}
                </div>
              )}

              {/* Files */}
              {contents.files.length > 0 && (
                <div>
                  {contents.files.map((item, idx) => {
                    const hasDuplicate = duplicateFilenames.has(item.file.filename);

                    return (
                      <div
                        key={item.file.id || idx}
                        onMouseEnter={() => setHoveredFile(item)}
                        onMouseLeave={() => setHoveredFile(null)}
                        className={`flex items-center gap-3 w-full px-3 py-2 transition text-left border-b border-gray-800 ${
                          hoveredFile?.file.id === item.file.id ? 'bg-gray-700' : 'hover:bg-gray-750'
                        }`}
                      >
                        <FileIcon size={16} className="text-gray-500 flex-shrink-0" />
                        {hasDuplicate && (
                          <span title="Duplicate file">
                            <AlertCircle size={14} className="text-yellow-500 flex-shrink-0" />
                          </span>
                        )}
                        <span className="text-sm truncate font-mono" title={item.file.filename}>
                          {item.file.filename}
                        </span>
                      </div>
                    );
                  })}
                </div>
              )}

              {/* Empty state */}
              {contents.subdirectories.length === 0 && contents.files.length === 0 && (
                <div className="text-center py-12 text-gray-500">
                  No folders or files found in this directory.
                </div>
              )}
            </div>

            {/* Right Panel - Metadata */}
            <div className="w-80 bg-gray-750 rounded-lg p-4 flex-shrink-0 overflow-y-auto">
              {hoveredFile ? (
                <div className="space-y-4">
                  {/* File Name */}
                  <div>
                    <div className="flex items-center gap-2 mb-2">
                      <FileIcon size={20} className="text-gray-400" />
                      <h3 className="font-semibold text-gray-200 text-sm break-all">
                        {hoveredFile.file.filename}
                      </h3>
                    </div>
                    {duplicateFilenames.has(hoveredFile.file.filename) && (
                      <div className="flex items-center gap-2 px-2 py-1 bg-yellow-900/30 border border-yellow-700 rounded text-xs text-yellow-400">
                        <AlertCircle size={14} />
                        <span>Duplicate file detected</span>
                      </div>
                    )}
                  </div>

                  {hoveredFile.frame ? (
                    <>
                      {/* Object & Type */}
                      {(hoveredFile.frame.object || hoveredFile.frame.imagetyp) && (
                        <div>
                          <div className="text-xs font-semibold text-gray-400 uppercase mb-2">Target</div>
                          <div className="space-y-1">
                            {hoveredFile.frame.object && (
                              <div className="text-sm text-gray-200 font-medium">{hoveredFile.frame.object}</div>
                            )}
                            {hoveredFile.frame.imagetyp && (
                              <span className={`inline-block px-2 py-0.5 rounded text-xs ${
                                hoveredFile.frame.imagetyp === 'Light'
                                  ? 'bg-blue-900 text-blue-200'
                                  : 'bg-gray-700 text-gray-300'
                              }`}>
                                {hoveredFile.frame.imagetyp}
                              </span>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Exposure Details */}
                      {(hoveredFile.frame.exptime || hoveredFile.frame.filter) && (
                        <div>
                          <div className="text-xs font-semibold text-gray-400 uppercase mb-2">Exposure</div>
                          <div className="space-y-1 text-sm">
                            {hoveredFile.frame.exptime && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Duration:</span>
                                <span className="text-gray-200">{hoveredFile.frame.exptime}s</span>
                              </div>
                            )}
                            {hoveredFile.frame.filter && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Filter:</span>
                                <span className="text-gray-200">{hoveredFile.frame.filter}</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Equipment */}
                      {(hoveredFile.frame.telescop || hoveredFile.frame.instrume || hoveredFile.frame.focallen) && (
                        <div>
                          <div className="text-xs font-semibold text-gray-400 uppercase mb-2">Equipment</div>
                          <div className="space-y-1 text-sm">
                            {hoveredFile.frame.telescop && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Telescope:</span>
                                <span className="text-gray-200 truncate ml-2">{hoveredFile.frame.telescop}</span>
                              </div>
                            )}
                            {hoveredFile.frame.instrume && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Camera:</span>
                                <span className="text-gray-200 truncate ml-2">{hoveredFile.frame.instrume}</span>
                              </div>
                            )}
                            {hoveredFile.frame.focallen && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Focal Length:</span>
                                <span className="text-gray-200">{hoveredFile.frame.focallen}mm</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Camera Settings */}
                      {(hoveredFile.frame.gain !== null || hoveredFile.frame.offset !== null ||
                        hoveredFile.frame.ccd_temp !== null) && (
                        <div>
                          <div className="text-xs font-semibold text-gray-400 uppercase mb-2">Camera Settings</div>
                          <div className="space-y-1 text-sm">
                            {hoveredFile.frame.gain !== null && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Gain:</span>
                                <span className="text-gray-200">{hoveredFile.frame.gain}</span>
                              </div>
                            )}
                            {hoveredFile.frame.offset !== null && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Offset:</span>
                                <span className="text-gray-200">{hoveredFile.frame.offset}</span>
                              </div>
                            )}
                            {hoveredFile.frame.ccd_temp !== null && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Temperature:</span>
                                <span className="text-gray-200">{hoveredFile.frame.ccd_temp}°C</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Coordinates */}
                      {(hoveredFile.frame.ra !== null || hoveredFile.frame.dec !== null) && (
                        <div>
                          <div className="text-xs font-semibold text-gray-400 uppercase mb-2">Coordinates</div>
                          <div className="space-y-1 text-sm font-mono">
                            {hoveredFile.frame.ra !== null && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">RA:</span>
                                <span className="text-gray-200">{hoveredFile.frame.ra.toFixed(4)}°</span>
                              </div>
                            )}
                            {hoveredFile.frame.dec !== null && (
                              <div className="flex justify-between">
                                <span className="text-gray-400">Dec:</span>
                                <span className="text-gray-200">{hoveredFile.frame.dec.toFixed(4)}°</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Date */}
                      {hoveredFile.frame.date_obs && (
                        <div>
                          <div className="text-xs font-semibold text-gray-400 uppercase mb-2">Date</div>
                          <div className="text-sm text-gray-200">
                            {new Date(hoveredFile.frame.date_obs).toLocaleString()}
                          </div>
                        </div>
                      )}
                    </>
                  ) : (
                    <div className="text-sm text-gray-500">
                      No metadata available for this file.
                    </div>
                  )}
                </div>
              ) : (
                <div className="flex items-center justify-center h-full text-gray-500 text-sm text-center">
                  Hover over a file to see its metadata
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}