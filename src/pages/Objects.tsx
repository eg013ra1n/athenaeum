import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import { Sparkles, Trash2, Eye, Clock, MapPin, AlertCircle, Target, Pencil, Check, X, Star, AlertTriangle, Grip } from 'lucide-react';
import type { FramesSetWithCount, AutoGenerateResult } from '../types/models';

export default function Objects() {
  const navigate = useNavigate();
  const [frameSets, setFrameSets] = useState<FramesSetWithCount[]>([]);
  const [loading, setLoading] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [generateResult, setGenerateResult] = useState<AutoGenerateResult | null>(null);
  const [editingSetId, setEditingSetId] = useState<number | null>(null);
  const [editingName, setEditingName] = useState<string>('');
  const [draggedSetId, setDraggedSetId] = useState<number | null>(null);
  const [dropTargetId, setDropTargetId] = useState<number | null>(null);
  const [merging, setMerging] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const [dragStartPos, setDragStartPos] = useState<{ x: number; y: number } | null>(null);
  const [mousePos, setMousePos] = useState<{ x: number; y: number }>({ x: 0, y: 0 });
  const [showMergeDialog, setShowMergeDialog] = useState(false);
  const [pendingMerge, setPendingMerge] = useState<{
    sourceId: number;
    targetId: number;
    sourceName: string;
    targetName: string;
  } | null>(null);
  const [isMergeMode, setIsMergeMode] = useState(false);

  // For now, using project_id = 1 as default
  const PROJECT_ID = 1;

  useEffect(() => {
    loadFrameSets();
  }, []);

  const loadFrameSets = async () => {
    try {
      setLoading(true);
      setError(null);
      const sets = await invoke<FramesSetWithCount[]>('get_frames_sets', {
        projectId: PROJECT_ID,
      });
      setFrameSets(sets);
    } catch (err) {
      setError(err as string);
      console.error('Failed to load frame sets:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleAutoGenerate = async () => {
    if (!confirm('Auto-generate frame sets from LIGHT frames? This will cluster frames by sky coordinates.')) {
      return;
    }

    try {
      setGenerating(true);
      setError(null);
      setGenerateResult(null);

      const result = await invoke<AutoGenerateResult>('auto_generate_frame_sets', {
        projectId: PROJECT_ID,
      });

      setGenerateResult(result);

      if (result.sets_created > 0) {
        await loadFrameSets();
      }
    } catch (err) {
      setError(err as string);
      console.error('Failed to auto-generate frame sets:', err);
    } finally {
      setGenerating(false);
    }
  };

  const handleDelete = async (setId: number, setName: string | null) => {
    if (!confirm(`Delete frame set "${setName || 'Untitled'}"? This will not delete the frames themselves.`)) {
      return;
    }

    try {
      await invoke('delete_frames_set', { framesSetId: setId });
      await loadFrameSets();
    } catch (err) {
      setError(err as string);
      console.error('Failed to delete frame set:', err);
    }
  };

  const startEditing = (setId: number, currentName: string | null) => {
    setEditingSetId(setId);
    setEditingName(currentName || '');
  };

  const cancelEditing = () => {
    setEditingSetId(null);
    setEditingName('');
  };

  const saveRename = async (setId: number) => {
    if (!editingName.trim()) {
      setError('Name cannot be empty');
      return;
    }

    try {
      await invoke('rename_frames_set', {
        framesSetId: setId,
        newName: editingName.trim()
      });
      await loadFrameSets();
      setEditingSetId(null);
      setEditingName('');
    } catch (err) {
      setError(err as string);
      console.error('Failed to rename frame set:', err);
    }
  };

  // Mouse-based drag handlers (more reliable than HTML5 drag/drop in Tauri/WebView)
  const handleMouseDown = (e: React.MouseEvent, setId: number) => {
    // Only start drag on left click, not on button clicks
    if (e.button !== 0) return;

    const target = e.target as HTMLElement;
    // Don't start drag if clicking on buttons or inputs
    if (target.tagName === 'BUTTON' || target.tagName === 'INPUT' || target.closest('button')) {
      return;
    }

    // Prevent text selection during drag
    e.preventDefault();

    console.log('[MouseDown] Starting drag for set:', setId);
    setDragStartPos({ x: e.clientX, y: e.clientY });
    setDraggedSetId(setId);
  };

  const handleMouseMove = (e: MouseEvent) => {
    if (draggedSetId === null || dragStartPos === null) return;

    // Update mouse position for drag preview
    setMousePos({ x: e.clientX, y: e.clientY });

    // Check if we've moved enough to start dragging (5px threshold)
    const dx = e.clientX - dragStartPos.x;
    const dy = e.clientY - dragStartPos.y;
    if (!isDragging && (Math.abs(dx) > 5 || Math.abs(dy) > 5)) {
      console.log('[MouseMove] Drag threshold exceeded, starting drag - visual feedback should now show');
      setIsDragging(true);

      // Clear any text selection that might have occurred
      if (window.getSelection) {
        window.getSelection()?.removeAllRanges();
      }
    }

    if (isDragging) {
      // Find which card is under the mouse
      const elements = document.elementsFromPoint(e.clientX, e.clientY);
      const cardElement = elements.find(el => el.hasAttribute('data-set-id'));

      if (cardElement) {
        const hoveredSetId = parseInt(cardElement.getAttribute('data-set-id') || '');
        if (hoveredSetId && hoveredSetId !== draggedSetId) {
          if (dropTargetId !== hoveredSetId) {
            console.log('[MouseMove] Hovering over set:', hoveredSetId, '- green highlight should show');
            setDropTargetId(hoveredSetId);
          }
        } else if (hoveredSetId === draggedSetId) {
          if (dropTargetId !== null) {
            console.log('[MouseMove] Back over dragged card, clearing drop target');
            setDropTargetId(null);
          }
        }
      } else {
        if (dropTargetId !== null) {
          console.log('[MouseMove] Not over any card, clearing drop target');
          setDropTargetId(null);
        }
      }
    }
  };

  const handleMouseUp = () => {
    if (draggedSetId === null) return;

    // Clear any text selection
    if (window.getSelection) {
      window.getSelection()?.removeAllRanges();
    }

    console.log('[MouseUp]', 'draggedSetId:', draggedSetId, 'dropTargetId:', dropTargetId, 'isDragging:', isDragging);

    if (isDragging && dropTargetId !== null && dropTargetId !== draggedSetId) {
      // Show merge confirmation dialog
      const sourceSet = frameSets.find(fs => fs.frames_set.id === draggedSetId);
      const targetSet = frameSets.find(fs => fs.frames_set.id === dropTargetId);

      if (sourceSet && targetSet) {
        console.log('[MouseUp] Showing merge dialog');
        setPendingMerge({
          sourceId: draggedSetId,
          targetId: dropTargetId,
          sourceName: sourceSet.frames_set.name || 'Untitled',
          targetName: targetSet.frames_set.name || 'Untitled',
        });
        setShowMergeDialog(true);
      }
    }

    // Reset drag state
    console.log('[MouseUp] Resetting drag state');
    setDraggedSetId(null);
    setDropTargetId(null);
    setIsDragging(false);
    setDragStartPos(null);
  };

  const handleConfirmMerge = async () => {
    if (!pendingMerge) return;

    console.log('[ConfirmMerge] User confirmed merge, executing...');
    try {
      setMerging(true);
      setError(null);
      await invoke('merge_frame_sets', {
        sourceId: pendingMerge.sourceId,
        targetId: pendingMerge.targetId
      });
      await loadFrameSets();
      console.log('[ConfirmMerge] Merge completed successfully');
      setShowMergeDialog(false);
      setPendingMerge(null);
    } catch (err) {
      setError(err as string);
      console.error('Failed to merge frame sets:', err);
    } finally {
      setMerging(false);
    }
  };

  const handleCancelMerge = () => {
    console.log('[CancelMerge] User cancelled merge');
    setShowMergeDialog(false);
    setPendingMerge(null);
  };

  // Set up global mouse listeners for drag
  useEffect(() => {
    if (draggedSetId !== null) {
      document.addEventListener('mousemove', handleMouseMove);
      document.addEventListener('mouseup', handleMouseUp);

      return () => {
        document.removeEventListener('mousemove', handleMouseMove);
        document.removeEventListener('mouseup', handleMouseUp);
      };
    }
  }, [draggedSetId, dropTargetId, isDragging, dragStartPos, frameSets]);

  // Set up keyboard listener for merge mode toggle
  useEffect(() => {
    const handleKeyPress = (e: KeyboardEvent) => {
      if (e.key === 'M' || e.key === 'm') {
        // Don't toggle if user is typing in an input field
        const target = e.target as HTMLElement;
        if (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA') {
          return;
        }
        setIsMergeMode(prev => !prev);
      }
    };

    document.addEventListener('keydown', handleKeyPress);

    return () => {
      document.removeEventListener('keydown', handleKeyPress);
    };
  }, []);

  const formatExposureTime = (seconds: number | null) => {
    if (!seconds) return 'N/A';
    const hours = (seconds / 3600).toFixed(1);
    return `${hours}h`;
  };

  return (
    <div className="p-6">
      <div className="mb-6">
        <div className="flex items-center justify-between mb-2">
          <div>
            <h2 className="text-3xl font-bold">Objects Library</h2>
            <p className="text-gray-400">
              Frame sets grouped by sky coordinates
            </p>
          </div>
          <div className="flex items-center gap-3">
            <button
              onClick={() => setIsMergeMode(prev => !prev)}
              className={`flex items-center gap-2 px-4 py-2 rounded-lg transition-colors ${
                isMergeMode
                  ? 'bg-green-600 hover:bg-green-700 text-white'
                  : 'bg-gray-700 hover:bg-gray-600 text-gray-300'
              }`}
              title={`${isMergeMode ? 'Exit' : 'Enter'} Merge Mode (M)`}
            >
              <Grip size={18} />
              {isMergeMode ? 'Exit Merge Mode' : 'Merge Mode'}
            </button>
            <button
              onClick={handleAutoGenerate}
              disabled={generating}
              className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <Sparkles size={18} />
              {generating ? 'Generating...' : 'Auto-Generate Sets'}
            </button>
          </div>
        </div>
      </div>

      {isMergeMode && (
        <div className="mb-4 p-3 bg-green-900/20 border border-green-800 rounded-lg flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Grip className="text-green-500" size={20} />
            <div>
              <p className="font-medium text-green-400">Merge Mode Active</p>
              <p className="text-sm text-green-300">Drag and drop frame sets to merge them</p>
            </div>
          </div>
          <button
            onClick={() => setIsMergeMode(false)}
            className="text-green-400 hover:text-green-300 text-sm underline"
          >
            Exit (M)
          </button>
        </div>
      )}

      {merging && (
        <div className="mb-4 p-4 bg-blue-900/20 border border-blue-800 rounded-lg flex items-start gap-3">
          <div className="animate-spin rounded-full h-5 w-5 border-b-2 border-blue-500 flex-shrink-0 mt-0.5"></div>
          <div className="flex-1">
            <p className="font-medium text-blue-400">Merging Frame Sets</p>
            <p className="text-sm text-blue-300">Please wait while the frame sets are being merged...</p>
          </div>
        </div>
      )}

      {error && (
        <div className="mb-4 p-4 bg-red-900/20 border border-red-800 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-red-500 flex-shrink-0 mt-0.5" size={20} />
          <div className="flex-1">
            <p className="font-medium text-red-400">Error</p>
            <p className="text-sm text-red-300">{String(error)}</p>
          </div>
        </div>
      )}

      {generateResult && (
        <div className="mb-4 p-4 bg-green-900/20 border border-green-800 rounded-lg">
          <p className="font-medium text-green-400 mb-2">Generation Complete</p>
          <div className="text-sm text-green-300 space-y-1">
            <p>Sets created: {generateResult.sets_created}</p>
            <p>Frames clustered: {generateResult.frames_clustered}</p>
            {generateResult.frames_already_in_sets > 0 && (
              <p>Frames already in sets (skipped): {generateResult.frames_already_in_sets}</p>
            )}
            {generateResult.frames_excluded > 0 && (
              <p>Frames excluded: {generateResult.frames_excluded}</p>
            )}
          </div>
          {generateResult.exclusion_reasons.length > 0 && (
            <details className="mt-3">
              <summary className="text-sm text-green-400 cursor-pointer">
                View exclusion reasons ({generateResult.exclusion_reasons.length})
              </summary>
              <div className="mt-2 text-xs text-gray-400 max-h-32 overflow-y-auto">
                {generateResult.exclusion_reasons.slice(0, 10).map((reason, i) => (
                  <p key={i} className="truncate">{reason}</p>
                ))}
                {generateResult.exclusion_reasons.length > 10 && (
                  <p className="text-gray-500 italic">
                    ... and {generateResult.exclusion_reasons.length - 10} more
                  </p>
                )}
              </div>
            </details>
          )}
        </div>
      )}

      {loading ? (
        <div className="text-center py-12 text-gray-400">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-500 mx-auto"></div>
          <p className="mt-4">Loading frame sets...</p>
        </div>
      ) : frameSets.length === 0 ? (
        <div className="bg-gray-800 rounded-lg p-8 text-center">
          <Target className="mx-auto mb-4 text-gray-600" size={48} />
          <p className="text-gray-400 mb-4">
            No frame sets yet. Use "Auto-Generate Sets" to cluster your LIGHT frames by sky coordinates.
          </p>
        </div>
      ) : (
        <div className={`grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4 ${isDragging || isMergeMode ? 'select-none' : ''}`}>
          {frameSets.map(({ frames_set, member_count }) => (
            <div
              key={frames_set.id}
              data-set-id={frames_set.id}
              onMouseDown={(e) => !editingSetId && isMergeMode && handleMouseDown(e, frames_set.id!)}
              className={`bg-gray-800 rounded-lg p-4 border-2 transition-all duration-200 group ${
                isDragging && draggedSetId === frames_set.id
                  ? 'opacity-40 border-blue-500 shadow-lg shadow-blue-500/50 cursor-grabbing select-none'
                  : dropTargetId === frames_set.id
                  ? 'border-green-500 bg-green-900/30 scale-105 shadow-lg shadow-green-500/50'
                  : 'border-gray-700 hover:border-gray-600'
              } ${!editingSetId && isMergeMode && !isDragging ? 'cursor-grab' : ''} ${isDragging ? 'select-none' : ''}`}
            >
              <div className="flex items-start justify-between mb-3">
                <div className="flex-1 min-w-0">
                  {editingSetId === frames_set.id ? (
                    <div className="flex items-center gap-2">
                      <input
                        type="text"
                        value={editingName}
                        onChange={(e) => setEditingName(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') {
                            saveRename(frames_set.id!);
                          } else if (e.key === 'Escape') {
                            cancelEditing();
                          }
                        }}
                        className="flex-1 px-2 py-1 bg-gray-700 text-gray-100 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                        autoFocus
                      />
                      <button
                        onClick={() => saveRename(frames_set.id!)}
                        className="p-1 text-green-400 hover:text-green-300"
                        title="Save"
                      >
                        <Check size={18} />
                      </button>
                      <button
                        onClick={cancelEditing}
                        className="p-1 text-red-400 hover:text-red-300"
                        title="Cancel"
                      >
                        <X size={18} />
                      </button>
                    </div>
                  ) : (
                    <div className="flex items-center gap-2">
                      {frames_set.is_custom && (
                        <span title="Custom Set">
                          <Star size={16} className="text-orange-500 fill-orange-500 flex-shrink-0" />
                        </span>
                      )}
                      <h3 className="text-lg font-semibold text-gray-100 truncate">
                        {frames_set.name || 'Untitled'}
                      </h3>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          startEditing(frames_set.id!, frames_set.name);
                        }}
                        className="p-1 text-gray-400 hover:text-gray-200 opacity-0 group-hover:opacity-100 transition-opacity"
                        title="Rename"
                      >
                        <Pencil size={14} />
                      </button>
                    </div>
                  )}
                  {frames_set.objctra && frames_set.objctdec && (
                    <div className="flex items-center gap-1 text-sm text-gray-400 mt-1">
                      <MapPin size={14} />
                      <span className="font-mono text-xs">
                        RA {frames_set.objctra} / Dec {frames_set.objctdec}
                      </span>
                    </div>
                  )}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3 mb-3 text-sm">
                <div className="bg-gray-900/50 rounded p-2">
                  <p className="text-gray-500 text-xs">Frames</p>
                  <p className="text-gray-200 font-medium">{member_count}</p>
                </div>
                <div className="bg-gray-900/50 rounded p-2">
                  <p className="text-gray-500 text-xs flex items-center gap-1">
                    <Clock size={12} />
                    Total Exp.
                  </p>
                  <p className="text-gray-200 font-medium">
                    {formatExposureTime(frames_set.total_exp_time)}
                  </p>
                </div>
              </div>

              {frames_set.date_obs_start && (
                <p className="text-xs text-gray-500 mb-3">
                  {frames_set.date_obs_end && frames_set.date_obs_start !== frames_set.date_obs_end
                    ? `${new Date(frames_set.date_obs_start).toLocaleDateString()} - ${new Date(frames_set.date_obs_end).toLocaleDateString()}`
                    : new Date(frames_set.date_obs_start).toLocaleDateString()
                  }
                </p>
              )}

              <div className="flex gap-2">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    navigate(`/objects/${frames_set.id}`);
                  }}
                  className="flex-1 flex items-center justify-center gap-2 px-3 py-2 bg-gray-700 hover:bg-gray-600 text-gray-200 rounded transition-colors text-sm"
                  title="View members"
                >
                  <Eye size={16} />
                  View
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleDelete(frames_set.id!, frames_set.name);
                  }}
                  className="px-3 py-2 bg-red-900/20 hover:bg-red-900/40 text-red-400 rounded transition-colors"
                  title="Delete set"
                >
                  <Trash2 size={16} />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Merge Confirmation Dialog */}
      {showMergeDialog && pendingMerge && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
          <div className="bg-gray-800 rounded-lg max-w-md w-full p-6 border border-gray-700">
            <h3 className="text-xl font-bold mb-4 flex items-center gap-2">
              <AlertTriangle size={20} className="text-yellow-500" />
              Merge Frame Sets?
            </h3>

            <div className="mb-4 text-gray-300">
              <p className="mb-3">
                Merge <span className="font-semibold text-blue-400">"{pendingMerge.sourceName}"</span> into <span className="font-semibold text-blue-400">"{pendingMerge.targetName}"</span>?
              </p>

              <div className="text-sm space-y-1 mb-3">
                <p>This will:</p>
                <ul className="list-disc list-inside space-y-1 text-gray-400">
                  <li>Combine all imaging nights and sessions</li>
                  <li>Deduplicate frames</li>
                  <li>Delete "{pendingMerge.sourceName}"</li>
                  <li>Mark "{pendingMerge.targetName}" as custom</li>
                </ul>
              </div>

              <p className="text-sm text-yellow-400 font-medium">
                This action cannot be undone.
              </p>
            </div>

            <div className="flex gap-3 justify-end">
              <button
                onClick={handleCancelMerge}
                disabled={merging}
                className="px-4 py-2 bg-gray-700 hover:bg-gray-600 disabled:bg-gray-600 disabled:cursor-not-allowed rounded-lg transition"
              >
                Cancel
              </button>
              <button
                onClick={handleConfirmMerge}
                disabled={merging}
                className="px-4 py-2 bg-yellow-600 hover:bg-yellow-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition flex items-center gap-2"
              >
                {merging ? (
                  <>
                    <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-white"></div>
                    Merging...
                  </>
                ) : (
                  'Merge'
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Drag preview that follows cursor */}
      {isDragging && draggedSetId !== null && (() => {
        const draggedSet = frameSets.find(fs => fs.frames_set.id === draggedSetId);
        if (!draggedSet) return null;

        const { frames_set, member_count } = draggedSet;

        return (
          <div
            className="fixed pointer-events-none z-50 transition-none"
            style={{
              left: mousePos.x,
              top: mousePos.y,
              transform: 'translate(-50%, -50%)',
            }}
          >
            <div className="bg-gray-800 rounded-lg p-4 border-2 border-blue-500 shadow-2xl shadow-blue-500/50 opacity-80 w-80">
              <div className="flex items-start justify-between mb-3">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    {frames_set.is_custom && (
                      <span title="Custom Set">
                        <Star size={16} className="text-orange-500 fill-orange-500 flex-shrink-0" />
                      </span>
                    )}
                    <h3 className="text-lg font-semibold text-gray-100 truncate">
                      {frames_set.name || 'Untitled'}
                    </h3>
                  </div>
                  {frames_set.objctra && frames_set.objctdec && (
                    <div className="flex items-center gap-1 text-sm text-gray-400 mt-1">
                      <MapPin size={14} />
                      <span className="font-mono text-xs">
                        RA {frames_set.objctra} / Dec {frames_set.objctdec}
                      </span>
                    </div>
                  )}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3 mb-3 text-sm">
                <div className="bg-gray-900/50 rounded p-2">
                  <p className="text-gray-500 text-xs">Frames</p>
                  <p className="text-gray-200 font-medium">{member_count}</p>
                </div>
                <div className="bg-gray-900/50 rounded p-2">
                  <p className="text-gray-500 text-xs flex items-center gap-1">
                    <Clock size={12} />
                    Total Exp.
                  </p>
                  <p className="text-gray-200 font-medium">
                    {formatExposureTime(frames_set.total_exp_time)}
                  </p>
                </div>
              </div>

              {frames_set.date_obs_start && (
                <p className="text-xs text-gray-500">
                  {frames_set.date_obs_end && frames_set.date_obs_start !== frames_set.date_obs_end
                    ? `${new Date(frames_set.date_obs_start).toLocaleDateString()} - ${new Date(frames_set.date_obs_end).toLocaleDateString()}`
                    : new Date(frames_set.date_obs_start).toLocaleDateString()
                  }
                </p>
              )}
            </div>
          </div>
        );
      })()}
    </div>
  );
}
