import { useState, useEffect, useMemo, useCallback } from 'react';
import { Folder, File as FileIcon, ArrowLeft, AlertCircle, FolderOpen, AlertTriangle, Play } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import type { ScanRootWithAvailability, DuplicateGroup, DirectoryContents, FileWithFrame } from '../types/models';
import BlinkViewer from './BlinkViewer';

/** Split a file path by either / or \ */
function splitPath(p: string): string[] {
  return p.split(/[/\\]/).filter(Boolean);
}

/** Get parent directory, preserving the original separator */
function getParentPath(p: string): string {
  const sep = p.includes('\\') ? '\\' : '/';
  const parts = p.split(/[/\\]/);
  parts.pop();
  return parts.join(sep);
}

/** Get the last segment (basename) of a path */
function getBasename(p: string): string {
  const parts = p.split(/[/\\]/);
  return parts[parts.length - 1] || p;
}

interface DirectoryTreeProps {
  scanRoots: ScanRootWithAvailability[];
  duplicates: DuplicateGroup[];
  refreshTrigger: number;
  instrume?: string;
  cameraDirectories?: string[];
}

export default function DirectoryTree({ scanRoots, duplicates, refreshTrigger, instrume, cameraDirectories }: DirectoryTreeProps) {
  const [currentPath, setCurrentPath] = useState<string>('');
  const [contents, setContents] = useState<DirectoryContents | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hoveredFile, setHoveredFile] = useState<FileWithFrame | null>(null);
  const [showBlinkViewer, setShowBlinkViewer] = useState(false);
  const [blackholedFileIds, setBlackholedFileIds] = useState<Set<number>>(new Set());

  // When camera-filtered, only show scan roots that contain files for this camera
  const effectiveRoots = useMemo(() => {
    if (!instrume || !cameraDirectories) return scanRoots;
    return scanRoots.filter(root =>
      cameraDirectories.some(dir => dir.startsWith(root.path))
    );
  }, [scanRoots, instrume, cameraDirectories]);

  // Create a set of filenames that have duplicates for quick lookup
  const duplicateFilenames = useMemo(() => {
    const set = new Set<string>();
    duplicates.forEach(group => {
      // The content_hash field now contains the filename
      set.add(group.content_hash);
    });
    return set;
  }, [duplicates]);

  // Filter to only FITS/XISF image files for blink functionality
  const imageFiles = useMemo(() => {
    if (!contents) return [];
    return contents.files.filter(f =>
      f.file.format === 'FITS' || f.file.format === 'XISF'
    );
  }, [contents]);

  // Fetch blackholed file IDs when files change
  useEffect(() => {
    const fetchBlackholedIds = async () => {
      if (!contents || contents.files.length === 0) {
        setBlackholedFileIds(new Set());
        return;
      }
      const fileIds = contents.files
        .map(f => f.file.id)
        .filter((id): id is number => id !== null);
      if (fileIds.length === 0) return;

      try {
        const blackholed = await invoke<number[]>('get_blackholed_file_ids', { fileIds });
        setBlackholedFileIds(new Set(blackholed));
      } catch (err) {
        console.error('Failed to fetch blackholed file IDs:', err);
      }
    };
    fetchBlackholedIds();
  }, [contents]);

  // Load directory contents
  const loadDirectory = useCallback(async (path: string) => {
    if (!path) return;

    setLoading(true);
    setError(null);

    try {
      const result = instrume && cameraDirectories
        ? await invoke<DirectoryContents>('get_camera_directory_contents', {
            directoryPath: path,
            instrume,
            cameraDirectories,
          })
        : await invoke<DirectoryContents>('get_directory_contents', {
            directoryPath: path,
          });
      setContents(result);
      setCurrentPath(path);
    } catch (e) {
      setError(e as string);
      setContents(null);
    } finally {
      setLoading(false);
    }
  }, [instrume, cameraDirectories]);

  // Initialize with first scan root
  useEffect(() => {
    if (effectiveRoots.length > 0 && !currentPath) {
      loadDirectory(effectiveRoots[0].path);
    }
  }, [effectiveRoots, currentPath, loadDirectory]);

  // Refresh when trigger changes
  useEffect(() => {
    if (currentPath && refreshTrigger > 0) {
      loadDirectory(currentPath);
    }
  }, [refreshTrigger, currentPath, loadDirectory]);

  // Navigate up one level
  const goUp = () => {
    const parentPath = getParentPath(currentPath);
    if (parentPath) {
      loadDirectory(parentPath);
    }
  };

  // Navigate to subdirectory
  const navigateToDirectory = (dirPath: string) => {
    loadDirectory(dirPath);
  };

  // Reveal path in system file explorer
  const revealPath = async () => {
    try {
      await revealItemInDir(currentPath);
    } catch (error) {
      console.error('Failed to reveal path:', error);
    }
  };

  // Check if current path is a root path
  const isAtRoot = effectiveRoots.some(root => root.path === currentPath);

  // Generate breadcrumb parts from current path
  const getBreadcrumbs = () => {
    if (!currentPath) return [];

    // Find which root this path belongs to
    const matchingRoot = effectiveRoots.find(root => currentPath.startsWith(root.path));
    if (!matchingRoot) return [];

    // Split the path into parts
    const parts = splitPath(currentPath);
    const rootParts = splitPath(matchingRoot.path);
    const sep = currentPath.includes('\\') ? '\\' : '/';

    // Build breadcrumbs
    const breadcrumbs: { label: string; path: string; isClickable: boolean }[] = [];
    let accumulatedPath = '';

    parts.forEach((part, index) => {
      if (index === 0) {
        accumulatedPath = currentPath.startsWith(sep) ? sep + part : part;
      } else {
        accumulatedPath += sep + part;
      }
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
    <div className="overflow-hidden">
      {/* Header with navigation */}
      <div className="bg-surface px-3 py-1.5 space-y-1">
        {/* Root selector */}
        <div className="flex items-center gap-3 flex-wrap">
          {effectiveRoots.map(root => (
            <button
              key={root.id}
              onClick={() => loadDirectory(root.path)}
              disabled={!root.is_available}
              title={!root.is_available ? 'Directory not available - go to Monitored Directories to relink' : root.path}
              className={`flex items-center gap-1.5 px-2 py-1 text-sm transition border-b-2 ${
                currentPath.startsWith(root.path)
                  ? 'border-accent text-accent'
                  : root.is_available
                    ? 'border-transparent text-content-muted hover:text-content'
                    : 'border-transparent text-content-muted cursor-not-allowed opacity-50'
              }`}
            >
              {root.is_available ? (
                <Folder size={14} className={currentPath.startsWith(root.path) ? 'text-accent' : 'text-content-muted'} />
              ) : (
                <AlertTriangle size={14} className="text-warning" />
              )}
              {getBasename(root.path) || root.path}
            </button>
          ))}
        </div>

        {/* Breadcrumb bar */}
        <div className="flex items-center gap-2">
          <button
            onClick={goUp}
            disabled={isAtRoot || loading}
            className="flex items-center gap-1 px-2 py-1 bg-surface-hover rounded transition text-sm disabled:opacity-40 disabled:cursor-not-allowed hover:brightness-110"
          >
            <ArrowLeft size={14} />
            <span>Back</span>
          </button>
          <div className="flex-1 flex items-center gap-1.5 min-w-0">
            {breadcrumbs.length > 0 ? (
              <div className="flex items-center gap-0.5 font-mono text-sm min-w-0 flex-wrap">
                {breadcrumbs.map((crumb, index) => (
                  <div key={crumb.path} className="flex items-center gap-0.5">
                    {crumb.isClickable ? (
                      <button
                        onClick={() => loadDirectory(crumb.path)}
                        className="text-accent hover:brightness-110 hover:underline transition"
                        title={`Navigate to ${crumb.path}`}
                      >
                        {crumb.label}
                      </button>
                    ) : (
                      <span className={crumb.path === currentPath ? 'text-content' : 'text-content-muted'}>
                        {crumb.label}
                      </span>
                    )}
                    {index < breadcrumbs.length - 1 && (
                      <span className="text-content-muted">/</span>
                    )}
                  </div>
                ))}
              </div>
            ) : (
              <span className="font-mono text-sm text-content-muted">Select a directory</span>
            )}
            {currentPath && (
              <button
                onClick={revealPath}
                className="flex items-center gap-1 px-1.5 py-0.5 bg-surface-hover hover:brightness-110 rounded text-xs transition flex-shrink-0"
                title="Reveal in file explorer"
              >
                <FolderOpen size={12} className="text-content-muted" />
              </button>
            )}
          </div>
          {contents && (
            <span className="text-xs text-content-muted flex-shrink-0">
              {contents.subdirectories.length} folders, {contents.files.length} files
            </span>
          )}
        </div>
      </div>

      {/* Content area */}
      <div className="p-4 bg-surface-elevated border border-border rounded-lg">
        {loading && (
          <div className="text-center py-12 text-content-muted">
            Loading directory...
          </div>
        )}

        {error && (
          <div className="bg-error-muted border border-error/50 rounded-lg p-4 mb-4">
            <div className="flex items-center gap-2 text-error">
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
                      className="flex items-center gap-3 w-full px-3 py-2 hover:bg-surface-hover transition text-left group border-b border-border"
                    >
                      <Folder size={18} className="text-accent flex-shrink-0" />
                      <span className="text-sm truncate font-mono group-hover:text-accent">
                        {getBasename(subdir)}
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
                    const isBlackholed = item.file.id !== null && blackholedFileIds.has(item.file.id);

                    return (
                      <div
                        key={item.file.id || idx}
                        onMouseEnter={() => setHoveredFile(item)}
                        onMouseLeave={() => setHoveredFile(null)}
                        className={`flex items-center gap-3 w-full px-3 py-2 transition text-left border-b border-border ${
                          hoveredFile?.file.id === item.file.id ? 'bg-surface-hover' : 'hover:bg-surface'
                        } ${isBlackholed ? 'opacity-60' : ''}`}
                      >
                        <FileIcon size={16} className={`flex-shrink-0 ${isBlackholed ? 'text-error' : 'text-content-muted'}`} />
                        {hasDuplicate && (
                          <span title="Duplicate file">
                            <AlertCircle size={14} className="text-warning flex-shrink-0" />
                          </span>
                        )}
                        <span
                          className={`text-sm truncate font-mono ${isBlackholed ? 'line-through text-error' : ''}`}
                          title={isBlackholed ? `${item.file.filename} (blackholed)` : item.file.filename}
                        >
                          {item.file.filename}
                        </span>
                      </div>
                    );
                  })}
                </div>
              )}

              {/* Empty state */}
              {contents.subdirectories.length === 0 && contents.files.length === 0 && (
                <div className="text-center py-12 text-content-muted">
                  No folders or files found in this directory.
                </div>
              )}
            </div>

            {/* Right Panel - Metadata */}
            <div className="w-80 bg-surface rounded-lg p-4 flex-shrink-0 overflow-y-auto flex flex-col">
              {/* Blink button */}
              {imageFiles.length > 0 && (
                <button
                  onClick={() => setShowBlinkViewer(true)}
                  className="mb-4 flex items-center justify-center gap-2 w-full px-4 py-2 bg-accent hover:brightness-110 text-white rounded-lg transition"
                >
                  <Play size={18} />
                  <span>Blink ({imageFiles.length})</span>
                </button>
              )}

              <div className="flex-1 overflow-y-auto">
              {hoveredFile ? (
                <div className="space-y-4">
                  {/* File Name */}
                  <div>
                    <div className="flex items-center gap-2 mb-2">
                      <FileIcon size={20} className="text-content-muted" />
                      <h3 className="font-semibold text-content text-sm break-all">
                        {hoveredFile.file.filename}
                      </h3>
                    </div>
                    {duplicateFilenames.has(hoveredFile.file.filename) && (
                      <div className="flex items-center gap-2 px-2 py-1 bg-warning-muted border border-warning/50 rounded text-xs text-warning">
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
                          <div className="text-xs font-semibold text-content-muted uppercase mb-2">Target</div>
                          <div className="space-y-1">
                            {hoveredFile.frame.object && (
                              <div className="text-sm text-content font-medium">{hoveredFile.frame.object}</div>
                            )}
                            {hoveredFile.frame.imagetyp && (
                              <span className={`inline-block px-2 py-0.5 rounded text-xs ${
                                hoveredFile.frame.imagetyp === 'Light'
                                  ? 'bg-info-muted text-accent'
                                  : 'bg-surface-hover text-content-secondary'
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
                          <div className="text-xs font-semibold text-content-muted uppercase mb-2">Exposure</div>
                          <div className="space-y-1 text-sm">
                            {hoveredFile.frame.exptime && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Duration:</span>
                                <span className="text-content">{hoveredFile.frame.exptime}s</span>
                              </div>
                            )}
                            {hoveredFile.frame.filter && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Filter:</span>
                                <span className="text-content">{hoveredFile.frame.filter}</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Equipment */}
                      {(hoveredFile.frame.telescop || hoveredFile.frame.instrume || hoveredFile.frame.focallen) && (
                        <div>
                          <div className="text-xs font-semibold text-content-muted uppercase mb-2">Equipment</div>
                          <div className="space-y-1 text-sm">
                            {hoveredFile.frame.telescop && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Telescope:</span>
                                <span className="text-content truncate ml-2">{hoveredFile.frame.telescop}</span>
                              </div>
                            )}
                            {hoveredFile.frame.instrume && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Camera:</span>
                                <span className="text-content truncate ml-2">{hoveredFile.frame.instrume}</span>
                              </div>
                            )}
                            {hoveredFile.frame.focallen && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Focal Length:</span>
                                <span className="text-content">{hoveredFile.frame.focallen}mm</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Camera Settings */}
                      {(hoveredFile.frame.gain !== null || hoveredFile.frame.offset !== null ||
                        hoveredFile.frame.ccd_temp !== null) && (
                        <div>
                          <div className="text-xs font-semibold text-content-muted uppercase mb-2">Camera Settings</div>
                          <div className="space-y-1 text-sm">
                            {hoveredFile.frame.gain !== null && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Gain:</span>
                                <span className="text-content">{hoveredFile.frame.gain}</span>
                              </div>
                            )}
                            {hoveredFile.frame.offset !== null && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Offset:</span>
                                <span className="text-content">{hoveredFile.frame.offset}</span>
                              </div>
                            )}
                            {hoveredFile.frame.ccd_temp !== null && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Temperature:</span>
                                <span className="text-content">{hoveredFile.frame.ccd_temp}°C</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Coordinates */}
                      {(hoveredFile.frame.ra !== null || hoveredFile.frame.dec !== null) && (
                        <div>
                          <div className="text-xs font-semibold text-content-muted uppercase mb-2">Coordinates</div>
                          <div className="space-y-1 text-sm font-mono">
                            {hoveredFile.frame.ra !== null && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">RA:</span>
                                <span className="text-content">{hoveredFile.frame.ra.toFixed(4)}°</span>
                              </div>
                            )}
                            {hoveredFile.frame.dec !== null && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Dec:</span>
                                <span className="text-content">{hoveredFile.frame.dec.toFixed(4)}°</span>
                              </div>
                            )}
                            {hoveredFile.frame.rotation !== null && hoveredFile.frame.rotation !== undefined && (
                              <div className="flex justify-between">
                                <span className="text-content-muted">Rotation:</span>
                                <span className="text-content">{hoveredFile.frame.rotation.toFixed(1)}°</span>
                              </div>
                            )}
                          </div>
                        </div>
                      )}

                      {/* Date */}
                      {hoveredFile.frame.date_obs && (
                        <div>
                          <div className="text-xs font-semibold text-content-muted uppercase mb-2">Date</div>
                          <div className="text-sm text-content">
                            {new Date(hoveredFile.frame.date_obs).toLocaleString('en-US', {
                              year: 'numeric',
                              month: 'short',
                              day: 'numeric',
                              hour: '2-digit',
                              minute: '2-digit',
                              second: '2-digit',
                              hour12: false,
                            })}
                          </div>
                        </div>
                      )}
                    </>
                  ) : (
                    <div className="text-sm text-content-muted">
                      No metadata available for this file.
                    </div>
                  )}
                </div>
              ) : (
                <div className="flex items-center justify-center h-full text-content-muted text-sm text-center">
                  Hover over a file to see its metadata
                </div>
              )}
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Blink Viewer Overlay */}
      {showBlinkViewer && imageFiles.length > 0 && (
        <BlinkViewer
          frames={imageFiles}
          onClose={() => setShowBlinkViewer(false)}
          onFramesRemoved={() => loadDirectory(currentPath)}
        />
      )}
    </div>
  );
}