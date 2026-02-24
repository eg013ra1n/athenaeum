import { useState, useEffect, useMemo, useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../api';
import { revealItemInDir } from '../api/desktop';
import { ArrowLeft, FolderOpen, ChevronDown, ChevronRight, AlertCircle, FileX, Play, Plus } from 'lucide-react';
import type { ExcludedFrameEntry, FileWithFrame, ReclassifyResult } from '../types/models';
import BlinkViewer from '../components/BlinkViewer';

interface FolderGroup {
  folder: string;
  files: ExcludedFrameEntry[];
  fileIds: Set<number>;
}

export default function ExcludedFrames() {
  const navigate = useNavigate();
  const [entries, setEntries] = useState<ExcludedFrameEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expandedFolders, setExpandedFolders] = useState<Set<string>>(new Set());
  const [blinkFrames, setBlinkFrames] = useState<FileWithFrame[] | null>(null);
  const [blinkLoading, setBlinkLoading] = useState<string | null>(null);
  const [selectedFileIds, setSelectedFileIds] = useState<Set<number>>(new Set());
  const [reclassifying, setReclassifying] = useState(false);
  const [showCreateForm, setShowCreateForm] = useState(false);
  const [frameSetName, setFrameSetName] = useState('');
  const [creating, setCreating] = useState(false);
  const [createSuccess, setCreateSuccess] = useState(false);

  useEffect(() => {
    loadExcludedFrames();
  }, []);

  const loadExcludedFrames = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await api.invoke<ExcludedFrameEntry[]>('get_excluded_frames');
      setEntries(result);
      setSelectedFileIds(new Set());
    } catch (err) {
      console.error('Failed to load excluded frames:', err);
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const folderGroups = useMemo((): FolderGroup[] => {
    const map = new Map<string, ExcludedFrameEntry[]>();
    for (const entry of entries) {
      const lastSep = entry.path.lastIndexOf('/');
      const folder = lastSep >= 0 ? entry.path.substring(0, lastSep) : '.';
      if (!map.has(folder)) {
        map.set(folder, []);
      }
      map.get(folder)!.push(entry);
    }
    return Array.from(map.entries())
      .map(([folder, files]) => ({
        folder,
        files,
        fileIds: new Set(files.map(f => f.file_id)),
      }))
      .sort((a, b) => a.folder.localeCompare(b.folder));
  }, [entries]);

  const toggleFolder = (folder: string) => {
    setExpandedFolders(prev => {
      const next = new Set(prev);
      if (next.has(folder)) {
        next.delete(folder);
      } else {
        next.add(folder);
      }
      return next;
    });
  };

  const handleReveal = async (folderPath: string) => {
    try {
      await revealItemInDir(folderPath);
    } catch (err) {
      console.error('Failed to reveal folder:', err);
    }
  };

  const handleBlink = async (folder: string, excludedFileIds: Set<number>) => {
    try {
      setBlinkLoading(folder);
      const dirContents = await api.invoke<{ files: FileWithFrame[] }>('get_directory_contents', {
        directoryPath: folder,
      });
      const excluded = dirContents.files.filter(
        fwf => fwf.file && fwf.file.id !== null && excludedFileIds.has(fwf.file.id)
      );
      if (excluded.length > 0) {
        setBlinkFrames(excluded);
      }
    } catch (err) {
      console.error('Failed to load files for blink:', err);
    } finally {
      setBlinkLoading(null);
    }
  };

  const toggleFileSelection = useCallback((fileId: number) => {
    setSelectedFileIds(prev => {
      const next = new Set(prev);
      if (next.has(fileId)) {
        next.delete(fileId);
      } else {
        next.add(fileId);
      }
      return next;
    });
  }, []);

  const toggleFolderSelection = useCallback((folderFiles: ExcludedFrameEntry[]) => {
    setSelectedFileIds(prev => {
      const next = new Set(prev);
      const folderIds = folderFiles.map(f => f.file_id);
      const allSelected = folderIds.every(id => prev.has(id));
      if (allSelected) {
        for (const id of folderIds) next.delete(id);
      } else {
        for (const id of folderIds) next.add(id);
      }
      return next;
    });
  }, []);

  const handleReclassify = async (newImagetyp: string) => {
    if (selectedFileIds.size === 0) return;
    try {
      setReclassifying(true);
      setError(null);
      const result = await api.invoke<ReclassifyResult>('reclassify_excluded_frames', {
        fileIds: Array.from(selectedFileIds),
        newImagetyp,
      });
      console.log('Reclassify result:', result);
      // Reload the list
      await loadExcludedFrames();
    } catch (err) {
      console.error('Failed to reclassify frames:', err);
      setError(String(err));
    } finally {
      setReclassifying(false);
    }
  };

  const handleCreateFrameSet = async () => {
    if (selectedFileIds.size === 0 || !frameSetName.trim()) return;
    try {
      setCreating(true);
      setError(null);
      await api.invoke<number>('create_frame_set_from_excluded', {
        fileIds: Array.from(selectedFileIds),
        name: frameSetName.trim(),
      });
      setCreateSuccess(true);
      setTimeout(async () => {
        setShowCreateForm(false);
        setFrameSetName('');
        setCreateSuccess(false);
        await loadExcludedFrames();
      }, 1500);
    } catch (err) {
      console.error('Failed to create frame set:', err);
      setError(String(err));
    } finally {
      setCreating(false);
    }
  };

  // Collect unique reasons with counts
  const reasonSummary = useMemo(() => {
    const counts = new Map<string, number>();
    for (const entry of entries) {
      let key = entry.reason;
      if (key.startsWith('Invalid coordinates:')) key = 'Invalid/missing coordinates';
      if (key.startsWith('Not a LIGHT frame')) key = 'Not a LIGHT frame';
      counts.set(key, (counts.get(key) || 0) + 1);
    }
    return Array.from(counts.entries())
      .sort((a, b) => b[1] - a[1]);
  }, [entries]);

  return (
    <div className="p-6">
      {/* Header */}
      <div className="mb-6">
        <button
          onClick={() => navigate('/objects')}
          className="flex items-center gap-2 text-content-muted hover:text-content transition-colors mb-3"
        >
          <ArrowLeft size={18} />
          Back to Objects
        </button>
        <h2 className="text-3xl font-bold">Excluded Frames</h2>
        <p className="text-content-muted">
          {entries.length} frame{entries.length !== 1 ? 's' : ''} excluded during auto-generation, grouped by folder
        </p>
      </div>

      {error && (
        <div className="mb-4 p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-error flex-shrink-0 mt-0.5" size={20} />
          <p className="text-sm text-error/80">{error}</p>
        </div>
      )}

      {/* Action bar - appears when files are selected */}
      {selectedFileIds.size > 0 && (
        <div className="mb-4 bg-accent/10 border border-accent/30 rounded-lg">
          <div className="p-3 flex items-center gap-3">
            <span className="text-sm text-content-secondary">
              {selectedFileIds.size} file{selectedFileIds.size !== 1 ? 's' : ''} selected
            </span>
            <div className="flex-1" />
            <button
              onClick={() => {
                setShowCreateForm(true);
                setFrameSetName('');
                setCreateSuccess(false);
              }}
              disabled={reclassifying || creating}
              className="px-3 py-1.5 text-sm bg-success hover:brightness-110 text-white rounded transition-colors disabled:opacity-50 flex items-center gap-1.5"
            >
              <Plus size={14} />
              Create Frame Set
            </button>
            <button
              onClick={() => handleReclassify('Flat')}
              disabled={reclassifying || creating}
              className="px-3 py-1.5 text-sm bg-blue-600 hover:bg-blue-500 text-white rounded transition-colors disabled:opacity-50"
            >
              {reclassifying ? 'Processing...' : 'Treat as Flat'}
            </button>
            <button
              onClick={() => handleReclassify('Dark')}
              disabled={reclassifying || creating}
              className="px-3 py-1.5 text-sm bg-gray-600 hover:bg-gray-500 text-white rounded transition-colors disabled:opacity-50"
            >
              {reclassifying ? 'Processing...' : 'Treat as Dark'}
            </button>
            <button
              onClick={() => setSelectedFileIds(new Set())}
              disabled={reclassifying || creating}
              className="px-3 py-1.5 text-sm text-content-muted hover:text-content transition-colors disabled:opacity-50"
            >
              Clear
            </button>
          </div>

          {/* Inline create frame set form */}
          {showCreateForm && (
            <div className="px-3 pb-3 border-t border-accent/20 pt-3">
              {createSuccess ? (
                <div className="text-center text-success py-2 text-sm">
                  Frame set created successfully!
                </div>
              ) : (
                <>
                  <h5 className="text-sm font-medium text-content mb-2">
                    Create Frame Set from {selectedFileIds.size} frame{selectedFileIds.size !== 1 ? 's' : ''}
                  </h5>
                  <input
                    type="text"
                    value={frameSetName}
                    onChange={(e) => setFrameSetName(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && frameSetName.trim() && !creating) {
                        handleCreateFrameSet();
                      }
                    }}
                    placeholder="Enter frame set name..."
                    className="w-full px-3 py-2 bg-surface-hover border border-border rounded text-sm text-content focus:outline-none focus:ring-1 focus:ring-accent mb-2"
                    autoFocus
                  />
                  <div className="flex gap-2">
                    <button
                      onClick={() => {
                        setShowCreateForm(false);
                        setFrameSetName('');
                      }}
                      className="flex-1 px-3 py-1.5 bg-surface-hover hover:bg-surface-hover text-content rounded text-sm transition-colors"
                    >
                      Cancel
                    </button>
                    <button
                      onClick={handleCreateFrameSet}
                      disabled={!frameSetName.trim() || creating}
                      className="flex-1 px-3 py-1.5 bg-success hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded text-sm font-medium transition-colors"
                    >
                      {creating ? 'Creating...' : 'Create'}
                    </button>
                  </div>
                </>
              )}
            </div>
          )}
        </div>
      )}

      {loading ? (
        <div className="text-center py-12 text-content-muted">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-accent mx-auto"></div>
          <p className="mt-4">Loading excluded frames...</p>
        </div>
      ) : entries.length === 0 ? (
        <div className="bg-surface-elevated rounded-lg p-8 text-center">
          <FileX className="mx-auto mb-4 text-content-muted" size={48} />
          <p className="text-content-muted">
            No excluded frames. Run auto-generate from the Objects page to see exclusions here.
          </p>
        </div>
      ) : (
        <>
          {/* Reason summary */}
          <div className="mb-6 p-4 bg-surface-elevated border border-border rounded-lg">
            <h3 className="text-sm font-medium text-content-secondary mb-2">Exclusion reasons</h3>
            <div className="space-y-1">
              {reasonSummary.map(([reason, count]) => (
                <div key={reason} className="flex items-center justify-between text-sm">
                  <span className="text-content-muted truncate mr-4">{reason}</span>
                  <span className="text-content-secondary font-mono flex-shrink-0">{count}</span>
                </div>
              ))}
            </div>
          </div>

          {/* Folder groups */}
          <div className="space-y-1">
            {folderGroups.map(({ folder, files, fileIds }) => {
              const isExpanded = expandedFolders.has(folder);
              const folderAllSelected = files.length > 0 && files.every(f => selectedFileIds.has(f.file_id));
              const folderSomeSelected = files.some(f => selectedFileIds.has(f.file_id));
              return (
                <div key={folder} className="bg-surface-elevated border border-border rounded-lg overflow-hidden">
                  {/* Folder header */}
                  <div
                    className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-surface-hover transition-colors"
                    onClick={() => toggleFolder(folder)}
                  >
                    {isExpanded ? (
                      <ChevronDown size={16} className="text-content-muted flex-shrink-0" />
                    ) : (
                      <ChevronRight size={16} className="text-content-muted flex-shrink-0" />
                    )}
                    <span className="text-sm text-content font-mono truncate flex-1">{folder}</span>
                    <span className="text-xs text-content-muted flex-shrink-0">
                      {files.length} file{files.length !== 1 ? 's' : ''}
                    </span>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleBlink(folder, fileIds);
                      }}
                      disabled={blinkLoading === folder}
                      className="p-1.5 text-content-muted hover:text-accent transition-colors rounded hover:bg-surface-hover disabled:opacity-50"
                      title="Blink excluded files"
                    >
                      {blinkLoading === folder ? (
                        <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-accent" />
                      ) : (
                        <Play size={16} />
                      )}
                    </button>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleReveal(folder);
                      }}
                      className="p-1.5 text-content-muted hover:text-content transition-colors rounded hover:bg-surface-hover"
                      title="Reveal in file explorer"
                    >
                      <FolderOpen size={16} />
                    </button>
                  </div>

                  {/* Expanded file list */}
                  {isExpanded && (
                    <div className="border-t border-border">
                      <table className="w-full text-sm">
                        <thead>
                          <tr className="text-xs text-content-muted border-b border-border">
                            <th className="w-10 px-4 py-2">
                              <input
                                type="checkbox"
                                checked={folderAllSelected}
                                ref={el => {
                                  if (el) el.indeterminate = folderSomeSelected && !folderAllSelected;
                                }}
                                onChange={() => toggleFolderSelection(files)}
                                className="rounded border-border"
                                title="Select all in folder"
                              />
                            </th>
                            <th className="text-left px-4 py-2 font-medium">Filename</th>
                            <th className="text-left px-4 py-2 font-medium">Reason</th>
                          </tr>
                        </thead>
                        <tbody>
                          {files.map((file) => (
                            <tr key={file.file_id} className="border-b border-border/50 last:border-0">
                              <td className="w-10 px-4 py-2">
                                <input
                                  type="checkbox"
                                  checked={selectedFileIds.has(file.file_id)}
                                  onChange={() => toggleFileSelection(file.file_id)}
                                  className="rounded border-border"
                                />
                              </td>
                              <td className="px-4 py-2 font-mono text-content truncate max-w-xs">
                                {file.filename}
                              </td>
                              <td className="px-4 py-2 text-content-muted truncate">
                                {file.reason}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </>
      )}

      {/* Blink Viewer Overlay */}
      {blinkFrames && blinkFrames.length > 0 && (
        <BlinkViewer
          frames={blinkFrames}
          initialIndex={0}
          onClose={() => setBlinkFrames(null)}
        />
      )}
    </div>
  );
}
