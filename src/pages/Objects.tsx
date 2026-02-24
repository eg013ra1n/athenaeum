import { useState, useEffect, useMemo } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../api';
import { Sparkles, Trash2, Eye, Clock, MapPin, AlertCircle, Target, Pencil, Check, X, Star, AlertTriangle, Grip, Sliders, RefreshCw, Filter, LayoutGrid, Table2, RotateCw, FileX } from 'lucide-react';
import type { FramesSetWithCount, AutoGenerateResult } from '../types/models';
import { ConfirmDialog } from '../components/ConfirmDialog';
import {
  ObjectsFilterPanel,
  ObjectsFilterState,
  emptyFilterState,
  parseRa,
  parseDec,
  angularDistance,
  countActiveFilters,
} from '../components/ObjectsFilterPanel';
import { ObjectsTableView } from '../components/ObjectsTableView';

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
  const [suggestedMerges, setSuggestedMerges] = useState<Array<{
    sourceId: number;
    targetId: number;
    sourceName: string;
    targetName: string;
    reason: string;
  }>>([]);
  const [showAutoGenerateConfirm, setShowAutoGenerateConfirm] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ id: number; name: string | null } | null>(null);
  const [showThresholdPanel, setShowThresholdPanel] = useState(false);
  const [customThreshold, setCustomThreshold] = useState<string>('');
  const [defaultThreshold, setDefaultThreshold] = useState<number>(3.0);
  const [deletingAutoSets, setDeletingAutoSets] = useState(false);
  const [showDeleteAutoSetsConfirm, setShowDeleteAutoSetsConfirm] = useState(false);
  const [showFilterPanel, setShowFilterPanel] = useState(false);
  const [filters, setFilters] = useState<ObjectsFilterState>(emptyFilterState);
  const [viewMode, setViewMode] = useState<'cards' | 'table'>('cards');
  const [excludedCount, setExcludedCount] = useState<number>(0);

  // For now, using project_id = 1 as default
  const PROJECT_ID = 1;

  useEffect(() => {
    loadFrameSets();
    loadDefaultThreshold();
    loadViewMode();
    loadExcludedCount();
  }, []);

  const loadExcludedCount = async () => {
    try {
      const count = await api.invoke<number>('get_excluded_frames_count');
      setExcludedCount(count);
    } catch (err) {
      console.error('Failed to load excluded frames count:', err);
    }
  };

  const loadViewMode = async () => {
    try {
      const savedMode = await api.invoke<string>('get_setting', {
        key: 'objects.view_mode',
        defaultValue: 'cards'
      });
      if (savedMode === 'cards' || savedMode === 'table') {
        setViewMode(savedMode);
      }
    } catch (err) {
      console.error('Failed to load view mode:', err);
    }
  };

  const handleViewModeChange = async (mode: 'cards' | 'table') => {
    setViewMode(mode);
    try {
      await api.invoke('set_setting', {
        key: 'objects.view_mode',
        value: mode
      });
    } catch (err) {
      console.error('Failed to save view mode:', err);
    }
  };

  const loadDefaultThreshold = async () => {
    try {
      const valueStr = await api.invoke<string>('get_setting', {
        key: 'grouping.threshold.value',
        defaultValue: '3.0'
      });
      const unit = await api.invoke<string>('get_setting', {
        key: 'grouping.threshold.unit',
        defaultValue: 'deg'
      });

      const value = parseFloat(valueStr);
      if (isNaN(value)) {
        setDefaultThreshold(3.0);
        return;
      }

      // Convert to degrees based on unit
      let thresholdDeg = value;
      switch (unit) {
        case 'arcsec':
          thresholdDeg = value / 3600;
          break;
        case 'arcmin':
          thresholdDeg = value / 60;
          break;
        case 'deg':
          thresholdDeg = value;
          break;
      }

      setDefaultThreshold(thresholdDeg);
      setCustomThreshold(thresholdDeg.toFixed(2));
    } catch (err) {
      console.error('Failed to load threshold settings:', err);
      setDefaultThreshold(3.0);
      setCustomThreshold('3.00');
    }
  };

  const loadFrameSets = async () => {
    try {
      setLoading(true);
      setError(null);
      const sets = await api.invoke<FramesSetWithCount[]>('get_frames_sets', {
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

  // Helper function to detect suggested merges between new and existing sets
  const detectSuggestedMerges = (
    oldSets: FramesSetWithCount[],
    newSets: FramesSetWithCount[]
  ) => {
    const suggestions: Array<{
      sourceId: number;
      targetId: number;
      sourceName: string;
      targetName: string;
      reason: string;
    }> = [];

    // Find sets that are in newSets but not in oldSets (newly created)
    const oldSetIds = new Set(oldSets.map(s => s.frames_set.id));
    const recentSets = newSets.filter(s => !oldSetIds.has(s.frames_set.id!));

    // For each recently created set, find potential matches in existing sets
    for (const recentSet of recentSets) {
      const recentName = (recentSet.frames_set.name || '').toLowerCase();

      for (const existingSet of oldSets) {
        const existingName = (existingSet.frames_set.name || '').toLowerCase();

        // Check if names match (case-insensitive, contains)
        const nameMatch = recentName && existingName &&
          (recentName.includes(existingName) || existingName.includes(recentName));

        if (nameMatch && recentSet.frames_set.id && existingSet.frames_set.id) {
          suggestions.push({
            sourceId: recentSet.frames_set.id,
            targetId: existingSet.frames_set.id,
            sourceName: recentSet.frames_set.name || 'Untitled',
            targetName: existingSet.frames_set.name || 'Untitled',
            reason: `Same object: "${existingName}"`
          });
          break; // Only suggest one match per new set
        }
      }
    }

    return suggestions;
  };

  const handleAutoGenerate = async () => {
    setShowAutoGenerateConfirm(true);
  };

  const confirmAutoGenerate = async () => {
    setShowAutoGenerateConfirm(false);

    try {
      setGenerating(true);
      setError(null);
      setGenerateResult(null);
      setSuggestedMerges([]);

      // Store current frame sets before auto-generate
      const setsBeforeGenerate = frameSets;

      // Prepare threshold parameter - only pass if different from default
      let thresholdDeg: number | null = null;
      const parsed = parseFloat(customThreshold);
      if (!isNaN(parsed) && parsed > 0 && Math.abs(parsed - defaultThreshold) > 0.001) {
        thresholdDeg = parsed;
      }

      const result = await api.invoke<AutoGenerateResult>('auto_generate_frame_sets', {
        projectId: PROJECT_ID,
        thresholdDeg,
      });

      setGenerateResult(result);

      // Refresh excluded count (always, even if no sets created)
      loadExcludedCount();

      if (result.sets_created > 0) {
        // Reload frame sets
        const updatedSets = await api.invoke<FramesSetWithCount[]>('get_frames_sets', {
          projectId: PROJECT_ID,
        });
        setFrameSets(updatedSets);

        // Detect merge suggestions
        const suggestions = detectSuggestedMerges(setsBeforeGenerate, updatedSets);
        setSuggestedMerges(suggestions);
      }
    } catch (err) {
      setError(err as string);
      console.error('Failed to auto-generate frame sets:', err);
    } finally {
      setGenerating(false);
    }
  };

  const handleDelete = async (setId: number, setName: string | null) => {
    setDeleteTarget({ id: setId, name: setName });
    setShowDeleteConfirm(true);
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;

    setShowDeleteConfirm(false);

    try {
      await api.invoke('delete_frames_set', { framesSetId: deleteTarget.id });
      await loadFrameSets();
    } catch (err) {
      setError(err as string);
      console.error('Failed to delete frame set:', err);
    } finally {
      setDeleteTarget(null);
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
      await api.invoke('rename_frames_set', {
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

  const handleMarkAsCustom = async (setId: number) => {
    try {
      await api.invoke('mark_frame_set_custom', {
        framesSetId: setId
      });
      await loadFrameSets();
    } catch (err) {
      setError(err as string);
      console.error('Failed to mark frame set as custom:', err);
    }
  };

  const handleDeleteAutoGenerated = () => {
    setShowDeleteAutoSetsConfirm(true);
  };

  const confirmDeleteAutoGenerated = async () => {
    setShowDeleteAutoSetsConfirm(false);

    try {
      setDeletingAutoSets(true);
      setError(null);

      const deletedCount = await api.invoke<number>('delete_auto_generated_frame_sets');

      // Reload frame sets to reflect deletions
      await loadFrameSets();

      console.log(`Deleted ${deletedCount} auto-generated frame set(s)`);
    } catch (err) {
      setError(err as string);
      console.error('Failed to delete auto-generated frame sets:', err);
    } finally {
      setDeletingAutoSets(false);
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
      // Find which card/row is under the mouse
      const elements = document.elementsFromPoint(e.clientX, e.clientY);
      // Check direct attribute first, then traverse up to find parent with data-set-id
      // This handles table rows where <td> cells are hit but <tr> has the attribute
      let cardElement: Element | null = elements.find(el => el.hasAttribute('data-set-id')) || null;
      if (!cardElement && elements[0]) {
        cardElement = elements[0].closest('[data-set-id]');
      }

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
      await api.invoke('merge_frame_sets', {
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

  const handleSuggestedMerge = async (sourceId: number, targetId: number) => {
    try {
      setMerging(true);
      setError(null);
      await api.invoke('merge_frame_sets', { sourceId, targetId });
      await loadFrameSets();
      // Remove this suggestion from the list
      setSuggestedMerges(prev => prev.filter(s => s.sourceId !== sourceId));
    } catch (err) {
      setError(err as string);
      console.error('Failed to merge suggested sets:', err);
    } finally {
      setMerging(false);
    }
  };

  const handleMergeAllSuggestions = async () => {
    for (const suggestion of suggestedMerges) {
      await handleSuggestedMerge(suggestion.sourceId, suggestion.targetId);
    }
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

  // Filter frame sets based on active filters
  const filteredFrameSets = useMemo(() => {
    let result = frameSets;

    // Name search (case-insensitive contains)
    if (filters.nameSearch.trim()) {
      const search = filters.nameSearch.toLowerCase();
      result = result.filter(fs =>
        (fs.frames_set.name || '').toLowerCase().includes(search)
      );
    }

    // Custom sets only
    if (filters.showOnlyCustom) {
      result = result.filter(fs => fs.frames_set.is_custom);
    }

    // Date range filter
    if (filters.dateFrom || filters.dateTo) {
      result = result.filter(fs => {
        const setStart = fs.frames_set.date_obs_start;
        const setEnd = fs.frames_set.date_obs_end;
        if (!setStart) return false;

        const startDate = setStart.split('T')[0];
        const endDate = (setEnd || setStart).split('T')[0];

        if (filters.dateFrom && endDate < filters.dateFrom) return false;
        if (filters.dateTo && startDate > filters.dateTo) return false;
        return true;
      });
    }

    // Coordinate filter
    if (filters.coordEnabled && filters.coordRa && filters.coordDec) {
      const targetRa = parseRa(filters.coordRa);
      const targetDec = parseDec(filters.coordDec);
      const radiusValue = parseFloat(filters.coordRadius) || 60;
      const radiusDeg = filters.coordRadiusUnit === 'arcmin' ? radiusValue / 60 : radiusValue;

      if (targetRa !== null && targetDec !== null && radiusDeg > 0) {
        result = result.filter(fs => {
          const setRa = parseRa(fs.frames_set.objctra || '');
          const setDec = parseDec(fs.frames_set.objctdec || '');
          if (setRa === null || setDec === null) return false;

          const distance = angularDistance(targetRa, targetDec, setRa, setDec);
          return distance <= radiusDeg;
        });
      }
    }

    return result;
  }, [frameSets, filters]);

  const activeFilterCount = countActiveFilters(filters);

  return (
    <div className="p-6">
      <div className="mb-6">
        <div className="flex items-center justify-between mb-2">
          <div>
            <h2 className="text-3xl font-bold">Objects Library</h2>
            <p className="text-content-muted">
              Frame sets grouped by sky coordinates
            </p>
            {excludedCount > 0 && (
              <button
                onClick={() => navigate('/excluded')}
                className="mt-1 flex items-center gap-1.5 text-sm text-warning hover:text-warning/80 transition-colors"
              >
                <FileX size={14} />
                {excludedCount} excluded frame{excludedCount !== 1 ? 's' : ''}
              </button>
            )}
          </div>
          <div className="flex items-center gap-3">
            <button
              onClick={handleAutoGenerate}
              disabled={generating}
              className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
            >
              <Sparkles size={18} />
              {generating ? 'Generating...' : 'Auto-Generate Sets'}
            </button>
            <button
              onClick={() => setShowThresholdPanel(prev => !prev)}
              className={`flex items-center justify-center py-2 px-2 rounded-lg transition-colors ${
                showThresholdPanel
                  ? 'bg-accent hover:bg-accent-hover text-surface'
                  : 'bg-surface-hover hover:bg-surface-hover text-content-muted'
              }`}
              title="Grouping threshold settings"
            >
              <Sliders size={18} />
            </button>
            <div className="h-6 w-px bg-border" />
            <div className="flex items-center bg-surface-hover rounded-lg p-1">
              <button
                onClick={() => handleViewModeChange('cards')}
                className={`p-1.5 rounded transition-colors ${
                  viewMode === 'cards'
                    ? 'bg-accent text-surface'
                    : 'text-content-muted hover:text-content'
                }`}
                title="Card view"
              >
                <LayoutGrid size={18} />
              </button>
              <button
                onClick={() => handleViewModeChange('table')}
                className={`p-1.5 rounded transition-colors ${
                  viewMode === 'table'
                    ? 'bg-accent text-surface'
                    : 'text-content-muted hover:text-content'
                }`}
                title="Table view"
              >
                <Table2 size={18} />
              </button>
            </div>
            <button
              onClick={() => setShowFilterPanel(prev => !prev)}
              className={`relative flex items-center gap-2 px-3 py-2 rounded-lg transition-colors ${
                showFilterPanel || activeFilterCount > 0
                  ? 'bg-accent hover:bg-accent-hover text-surface'
                  : 'bg-surface-hover hover:bg-surface-hover text-content-secondary'
              }`}
              title="Filter frame sets"
            >
              <Filter size={18} />
              Filter
              {activeFilterCount > 0 && (
                <span className="absolute -top-1 -right-1 bg-orange text-surface text-xs font-bold rounded-full w-5 h-5 flex items-center justify-center">
                  {activeFilterCount}
                </span>
              )}
            </button>
            <button
              onClick={() => setIsMergeMode(prev => !prev)}
              className={`flex items-center gap-2 px-4 py-2 rounded-lg transition-colors ${
                isMergeMode
                  ? 'bg-success hover:brightness-90 text-surface'
                  : 'bg-surface-hover hover:bg-surface-hover text-content-secondary'
              }`}
              title={`${isMergeMode ? 'Exit' : 'Enter'} Merge Mode (M)`}
            >
              <Grip size={18} />
              {isMergeMode ? 'Exit Merge Mode' : 'Merge Mode'}
            </button>
          </div>
        </div>

        {/* Threshold Settings Panel */}
        {showThresholdPanel && (
          <div className="mt-4 p-4 bg-surface-elevated border border-border rounded-lg">
            {/* Header with buttons */}
            <div className="flex items-center justify-between mb-3">
              <div className="flex items-center gap-2">
                <label className="text-sm font-medium text-content-secondary">
                  Grouping Threshold
                </label>
                <button
                  onClick={() => setCustomThreshold(defaultThreshold.toFixed(2))}
                  className="p-1 text-content-muted hover:text-content transition-colors"
                  title="Reset to default"
                >
                  <RefreshCw size={14} />
                </button>
              </div>
              {frameSets.filter(fs => !fs.frames_set.is_custom).length > 0 && (
                <button
                  onClick={handleDeleteAutoGenerated}
                  disabled={deletingAutoSets}
                  className="flex items-center gap-2 px-3 py-1.5 bg-error hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white text-xs rounded transition-colors"
                  title={`Delete ${frameSets.filter(fs => !fs.frames_set.is_custom).length} auto-generated set${frameSets.filter(fs => !fs.frames_set.is_custom).length !== 1 ? 's' : ''}`}
                >
                  <Trash2 size={14} />
                  {deletingAutoSets ? 'Deleting...' : `Delete ${frameSets.filter(fs => !fs.frames_set.is_custom).length} Auto-Generated`}
                </button>
              )}
            </div>

            {/* Input field */}
            <div className="flex items-center gap-3">
              <input
                type="number"
                value={customThreshold}
                onChange={(e) => setCustomThreshold(e.target.value)}
                step="0.1"
                min="0"
                className="w-32 px-3 py-2 bg-surface-hover border border-border rounded text-sm text-content focus:outline-none focus:ring-2 focus:ring-accent"
              />
              <span className="text-sm text-content-muted">degrees</span>
            </div>

            {/* Conversion helper */}
            <p className="mt-2 text-xs text-content-muted">
              {!isNaN(parseFloat(customThreshold))
                ? `${parseFloat(customThreshold).toFixed(2)}° = ${(parseFloat(customThreshold) * 60).toFixed(1)} arcmin`
                : `${defaultThreshold.toFixed(2)}° = ${(defaultThreshold * 60).toFixed(1)} arcmin`
              }
            </p>
          </div>
        )}

        {/* Filter Panel */}
        <ObjectsFilterPanel
          filters={filters}
          onChange={setFilters}
          isOpen={showFilterPanel}
        />
      </div>

      {/* Results Summary */}
      {activeFilterCount > 0 && (
        <div className="mb-4 text-sm text-content-muted">
          Showing {filteredFrameSets.length} of {frameSets.length} frame set{frameSets.length !== 1 ? 's' : ''}
        </div>
      )}

      {isMergeMode && (
        <div className="mb-4 p-3 bg-success-muted border border-success/50 rounded-lg flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Grip className="text-success" size={20} />
            <div>
              <p className="font-medium text-success">Merge Mode Active</p>
              <p className="text-sm text-success/80">Drag and drop frame sets to merge them</p>
            </div>
          </div>
          <button
            onClick={() => setIsMergeMode(false)}
            className="text-success hover:brightness-110 text-sm underline"
          >
            Exit (M)
          </button>
        </div>
      )}

      {merging && (
        <div className="mb-4 p-4 bg-info-muted border border-info/50 rounded-lg flex items-start gap-3">
          <div className="animate-spin rounded-full h-5 w-5 border-b-2 border-info flex-shrink-0 mt-0.5"></div>
          <div className="flex-1">
            <p className="font-medium text-info">Merging Frame Sets</p>
            <p className="text-sm text-info/80">Please wait while the frame sets are being merged...</p>
          </div>
        </div>
      )}

      {error && (
        <div className="mb-4 p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-error flex-shrink-0 mt-0.5" size={20} />
          <div className="flex-1">
            <p className="font-medium text-error">Error</p>
            <p className="text-sm text-error/80">{String(error)}</p>
          </div>
        </div>
      )}

      {generateResult && (
        <div className="mb-4 p-4 bg-success-muted border border-success/50 rounded-lg">
          <div className="flex items-start justify-between mb-2">
            <p className="font-medium text-success">Generation Complete</p>
            <button
              onClick={() => setGenerateResult(null)}
              className="p-1 text-success hover:brightness-110 transition-colors"
              title="Dismiss"
            >
              <X size={18} />
            </button>
          </div>
          <div className="text-sm text-success/80 space-y-1">
            <p>Sets created: {generateResult.sets_created}</p>
            <p>Frames clustered: {generateResult.frames_clustered}</p>
            {generateResult.frames_already_in_sets > 0 && (
              <p>Frames already in sets (skipped): {generateResult.frames_already_in_sets}</p>
            )}
            {generateResult.frames_excluded > 0 && (
              <p>Frames excluded: {generateResult.frames_excluded}</p>
            )}
          </div>
          {generateResult.frames_excluded > 0 && (
            <div className="mt-3">
              <button
                onClick={() => navigate('/excluded')}
                className="text-sm text-success hover:text-success/80 underline cursor-pointer"
              >
                View {generateResult.frames_excluded} excluded frames
              </button>
            </div>
          )}
        </div>
      )}

      {suggestedMerges.length > 0 && (
        <div className="mb-4 p-4 bg-warning-muted border border-warning/50 rounded-lg">
          <div className="flex items-center justify-between mb-3">
            <p className="font-medium text-warning">Suggested Merges</p>
            <div className="flex items-center gap-2">
              <button
                onClick={() => setSuggestedMerges([])}
                className="px-3 py-1 bg-surface-hover hover:bg-surface-hover text-content-secondary text-sm rounded transition-colors"
                title="Dismiss all suggestions"
              >
                Dismiss
              </button>
              <button
                onClick={handleMergeAllSuggestions}
                disabled={merging}
                className="px-3 py-1 bg-warning hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-surface text-sm rounded transition-colors"
              >
                Merge All
              </button>
            </div>
          </div>
          <div className="space-y-2">
            {suggestedMerges.map((suggestion, i) => (
              <div key={i} className="flex items-center justify-between p-3 bg-surface-elevated/50 rounded">
                <div className="flex-1">
                  <p className="text-sm text-content-secondary">
                    Merge <span className="font-semibold text-accent">"{suggestion.sourceName}"</span>
                    {' '}into{' '}
                    <span className="font-semibold text-accent">"{suggestion.targetName}"</span>
                  </p>
                  <p className="text-xs text-content-muted mt-1">{suggestion.reason}</p>
                </div>
                <button
                  onClick={() => handleSuggestedMerge(suggestion.sourceId, suggestion.targetId)}
                  disabled={merging}
                  className="ml-3 px-3 py-1 bg-success hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-surface text-sm rounded transition-colors"
                >
                  Merge
                </button>
              </div>
            ))}
          </div>
        </div>
      )}

      {loading ? (
        <div className="text-center py-12 text-content-muted">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-accent mx-auto"></div>
          <p className="mt-4">Loading frame sets...</p>
        </div>
      ) : frameSets.length === 0 ? (
        <div className="bg-surface-elevated rounded-lg p-8 text-center">
          <Target className="mx-auto mb-4 text-content-muted" size={48} />
          <p className="text-content-muted mb-4">
            No frame sets yet. Use "Auto-Generate Sets" to cluster your LIGHT frames by sky coordinates.
          </p>
        </div>
      ) : filteredFrameSets.length === 0 ? (
        <div className="bg-surface-elevated rounded-lg p-8 text-center">
          <Filter className="mx-auto mb-4 text-content-muted" size={48} />
          <p className="text-content-muted mb-4">
            No frame sets match your filters.
          </p>
          <button
            onClick={() => setFilters(emptyFilterState)}
            className="text-accent hover:text-accent-hover underline text-sm"
          >
            Clear all filters
          </button>
        </div>
      ) : viewMode === 'table' ? (
        <ObjectsTableView
          frameSets={filteredFrameSets}
          isMergeMode={isMergeMode}
          isDragging={isDragging}
          draggedSetId={draggedSetId}
          dropTargetId={dropTargetId}
          editingSetId={editingSetId}
          editingName={editingName}
          onMouseDown={handleMouseDown}
          onView={(setId) => navigate(`/objects/${setId}`)}
          onDelete={handleDelete}
          onStartEditing={startEditing}
          onSaveRename={saveRename}
          onCancelEditing={cancelEditing}
          onEditingNameChange={setEditingName}
          onMarkAsCustom={handleMarkAsCustom}
        />
      ) : (
        <div className={`grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4 ${isDragging || isMergeMode ? 'select-none' : ''}`}>
          {filteredFrameSets.map(({ frames_set, member_count }) => (
            <div
              key={frames_set.id}
              data-set-id={frames_set.id}
              onMouseDown={(e) => !editingSetId && isMergeMode && handleMouseDown(e, frames_set.id!)}
              className={`bg-surface-elevated rounded-lg p-4 border-2 transition-all duration-200 group ${
                isDragging && draggedSetId === frames_set.id
                  ? 'opacity-40 border-accent shadow-lg shadow-accent/50 cursor-grabbing select-none'
                  : dropTargetId === frames_set.id
                  ? 'border-success bg-success-muted scale-105 shadow-lg shadow-success/50'
                  : 'border-border hover:border-border'
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
                        className="flex-1 px-2 py-1 bg-surface-hover text-content rounded border border-border focus:outline-none focus:border-accent"
                        autoFocus
                      />
                      <button
                        onClick={() => saveRename(frames_set.id!)}
                        className="p-1 text-success hover:text-success/90"
                        title="Save"
                      >
                        <Check size={18} />
                      </button>
                      <button
                        onClick={cancelEditing}
                        className="p-1 text-error hover:text-error/90"
                        title="Cancel"
                      >
                        <X size={18} />
                      </button>
                    </div>
                  ) : (
                    <div className="flex items-center gap-2">
                      {frames_set.is_custom ? (
                        <span title="Custom Set">
                          <Star size={16} className="text-orange fill-orange flex-shrink-0" />
                        </span>
                      ) : (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            handleMarkAsCustom(frames_set.id!);
                          }}
                          className="p-0 text-content-muted hover:text-orange transition-colors"
                          title="Mark as Custom Set"
                        >
                          <Star size={16} className="flex-shrink-0" />
                        </button>
                      )}
                      <h3 className="text-lg font-semibold text-content truncate">
                        {frames_set.name || 'Untitled'}
                      </h3>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          startEditing(frames_set.id!, frames_set.name);
                        }}
                        className="p-1 text-content-muted hover:text-content opacity-0 group-hover:opacity-100 transition-opacity"
                        title="Rename"
                      >
                        <Pencil size={14} />
                      </button>
                    </div>
                  )}
                  {frames_set.objctra && frames_set.objctdec && (
                    <div className="flex items-center gap-1 text-sm text-content-muted mt-1">
                      <MapPin size={14} />
                      <span className="font-mono text-xs">
                        RA {frames_set.objctra} / Dec {frames_set.objctdec}
                      </span>
                    </div>
                  )}
                  {frames_set.avg_rotation != null && (
                    <div className="flex items-center gap-1 text-sm text-content-muted mt-1">
                      <RotateCw size={14} />
                      <span className="font-mono text-xs">
                        {frames_set.min_rotation != null && frames_set.max_rotation != null &&
                         Math.abs(frames_set.max_rotation - frames_set.min_rotation) >= 1
                          ? `${frames_set.min_rotation.toFixed(1)}° – ${frames_set.max_rotation.toFixed(1)}°`
                          : `${frames_set.avg_rotation.toFixed(1)}°`
                        }
                      </span>
                    </div>
                  )}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3 mb-3 text-sm">
                <div className="bg-surface/50 rounded p-2">
                  <p className="text-content-muted text-xs">Frames</p>
                  <p className="text-content font-medium">{member_count}</p>
                </div>
                <div className="bg-surface/50 rounded p-2">
                  <p className="text-content-muted text-xs flex items-center gap-1">
                    <Clock size={12} />
                    Total Exp.
                  </p>
                  <p className="text-content font-medium">
                    {formatExposureTime(frames_set.total_exp_time)}
                  </p>
                </div>
              </div>

              {frames_set.date_obs_start && (
                <p className="text-xs text-content-muted mb-3">
                  {frames_set.date_obs_end && frames_set.date_obs_start !== frames_set.date_obs_end
                    ? `${new Date(frames_set.date_obs_start).toLocaleDateString()} - ${new Date(frames_set.date_obs_end).toLocaleDateString()}`
                    : new Date(frames_set.date_obs_start).toLocaleDateString()
                  }
                </p>
              )}

              <div className="space-y-2">
                <div className="flex gap-2">
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      navigate(`/objects/${frames_set.id}`);
                    }}
                    className="flex-1 flex items-center justify-center gap-2 px-3 py-2 bg-surface-hover hover:bg-surface-hover text-content rounded transition-colors text-sm"
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
                    className="px-3 py-2 bg-error-muted hover:bg-error/30 text-error rounded transition-colors"
                    title="Delete set"
                  >
                    <Trash2 size={16} />
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Merge Confirmation Dialog */}
      {showMergeDialog && pendingMerge && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
          <div className="bg-surface-elevated rounded-lg max-w-md w-full p-6 border border-border">
            <h3 className="text-xl font-bold mb-4 flex items-center gap-2">
              <AlertTriangle size={20} className="text-warning" />
              Merge Frame Sets?
            </h3>

            <div className="mb-4 text-content-secondary">
              <p className="mb-3">
                Merge <span className="font-semibold text-accent">"{pendingMerge.sourceName}"</span> into <span className="font-semibold text-accent">"{pendingMerge.targetName}"</span>?
              </p>

              <div className="text-sm space-y-1 mb-3">
                <p>This will:</p>
                <ul className="list-disc list-inside space-y-1 text-content-muted">
                  <li>Combine all imaging nights and sessions</li>
                  <li>Deduplicate frames</li>
                  <li>Delete "{pendingMerge.sourceName}"</li>
                  <li>Mark "{pendingMerge.targetName}" as custom</li>
                </ul>
              </div>

              <p className="text-sm text-warning font-medium">
                This action cannot be undone.
              </p>
            </div>

            <div className="flex gap-3 justify-end">
              <button
                onClick={handleCancelMerge}
                disabled={merging}
                className="px-4 py-2 bg-surface-hover hover:bg-surface-hover disabled:bg-surface-hover disabled:cursor-not-allowed rounded-lg transition"
              >
                Cancel
              </button>
              <button
                onClick={handleConfirmMerge}
                disabled={merging}
                className="px-4 py-2 bg-warning hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition flex items-center gap-2"
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

      {/* Auto-Generate Confirmation Dialog */}
      <ConfirmDialog
        isOpen={showAutoGenerateConfirm}
        title="Auto-Generate Frame Sets"
        message="Auto-generate frame sets from LIGHT frames? This will cluster frames by sky coordinates."
        onConfirm={confirmAutoGenerate}
        onCancel={() => setShowAutoGenerateConfirm(false)}
        confirmText="Generate"
      />

      {/* Delete Confirmation Dialog */}
      <ConfirmDialog
        isOpen={showDeleteConfirm}
        title="Delete Frame Set"
        message={`Delete frame set "${deleteTarget?.name || 'Untitled'}"?\n\nThis will not delete the frames themselves.`}
        onConfirm={confirmDelete}
        onCancel={() => {
          setShowDeleteConfirm(false);
          setDeleteTarget(null);
        }}
        confirmText="Delete"
        confirmDanger={true}
      />

      {/* Delete Auto-Generated Sets Confirmation Dialog */}
      <ConfirmDialog
        isOpen={showDeleteAutoSetsConfirm}
        title="Delete All Auto-Generated Sets?"
        message={`This will delete all ${frameSets.filter(fs => !fs.frames_set.is_custom).length} auto-generated frame sets.\n\nCustom sets will be preserved. The frames themselves will not be deleted.`}
        onConfirm={confirmDeleteAutoGenerated}
        onCancel={() => setShowDeleteAutoSetsConfirm(false)}
        confirmText="Delete All Auto-Generated"
        confirmDanger={true}
      />

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
            <div className="bg-surface-elevated rounded-lg p-4 border-2 border-accent shadow-2xl shadow-accent/50 opacity-80 w-80">
              <div className="flex items-start justify-between mb-3">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    {frames_set.is_custom ? (
                      <span title="Custom Set">
                        <Star size={16} className="text-orange fill-orange flex-shrink-0" />
                      </span>
                    ) : (
                      <span title="Auto-Generated Set">
                        <Star size={16} className="text-content-muted flex-shrink-0" />
                      </span>
                    )}
                    <h3 className="text-lg font-semibold text-content truncate">
                      {frames_set.name || 'Untitled'}
                    </h3>
                  </div>
                  {frames_set.objctra && frames_set.objctdec && (
                    <div className="flex items-center gap-1 text-sm text-content-muted mt-1">
                      <MapPin size={14} />
                      <span className="font-mono text-xs">
                        RA {frames_set.objctra} / Dec {frames_set.objctdec}
                      </span>
                    </div>
                  )}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3 mb-3 text-sm">
                <div className="bg-surface/50 rounded p-2">
                  <p className="text-content-muted text-xs">Frames</p>
                  <p className="text-content font-medium">{member_count}</p>
                </div>
                <div className="bg-surface/50 rounded p-2">
                  <p className="text-content-muted text-xs flex items-center gap-1">
                    <Clock size={12} />
                    Total Exp.
                  </p>
                  <p className="text-content font-medium">
                    {formatExposureTime(frames_set.total_exp_time)}
                  </p>
                </div>
              </div>

              {frames_set.date_obs_start && (
                <p className="text-xs text-content-muted">
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
