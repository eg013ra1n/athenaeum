import { useState, useEffect, useMemo } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../api';
import { Sparkles, Trash2, Eye, Clock, MapPin, AlertCircle, Target, Pencil, Check, X, Star, AlertTriangle, Grip, RotateCcw, Filter, LayoutGrid, Table2, RotateCw, FileX, Archive } from 'lucide-react';
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
import type { ObjectsTab } from '../components/ObjectsTableView';
import { ToolbarContainer, ToolbarButton, ToolbarDivider, ToolbarInfo } from '../components/Toolbar';
import { useNotifications } from '../contexts/NotificationContext';

export default function Objects() {
  const navigate = useNavigate();
  const { notify } = useNotifications();
  const [frameSets, setFrameSets] = useState<FramesSetWithCount[]>([]);
  const [loading, setLoading] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
  const [customThreshold, setCustomThreshold] = useState<string>('');
  const [thresholdUnit, setThresholdUnit] = useState<'deg' | 'arcmin' | 'arcsec'>('deg');
  const [defaultThreshold, setDefaultThreshold] = useState<number>(3.0);
  const [deletingAutoSets, setDeletingAutoSets] = useState(false);
  const [showDeleteAutoSetsConfirm, setShowDeleteAutoSetsConfirm] = useState(false);
  const [showFilterPanel, setShowFilterPanel] = useState(false);
  const [filters, setFilters] = useState<ObjectsFilterState>(emptyFilterState);
  const [viewMode, setViewMode] = useState<'cards' | 'table'>('cards');
  const [activeTab, setActiveTab] = useState<ObjectsTab>('stage');
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
        setCustomThreshold('3.00');
        setThresholdUnit('deg');
        return;
      }

      // Convert stored value to degrees for internal defaultThreshold reference
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

      const resolvedUnit: 'deg' | 'arcmin' | 'arcsec' =
        unit === 'arcmin' || unit === 'arcsec' ? unit : 'deg';

      setDefaultThreshold(thresholdDeg);
      // Display value in the unit stored in settings
      setCustomThreshold(value.toFixed(resolvedUnit === 'deg' ? 2 : 1));
      setThresholdUnit(resolvedUnit);
    } catch (err) {
      console.error('Failed to load threshold settings:', err);
      setDefaultThreshold(3.0);
      setCustomThreshold('3.00');
      setThresholdUnit('deg');
    }
  };

  const handleThresholdUnitChange = async (newUnit: 'deg' | 'arcmin' | 'arcsec') => {
    // Convert current display value to degrees, then to the new unit
    const currentValueDeg = (() => {
      const v = parseFloat(customThreshold);
      if (isNaN(v)) return defaultThreshold;
      switch (thresholdUnit) {
        case 'arcsec': return v / 3600;
        case 'arcmin': return v / 60;
        default: return v;
      }
    })();

    let newValue: number;
    switch (newUnit) {
      case 'arcsec': newValue = currentValueDeg * 3600; break;
      case 'arcmin': newValue = currentValueDeg * 60; break;
      default: newValue = currentValueDeg;
    }

    setThresholdUnit(newUnit);
    setCustomThreshold(newValue.toFixed(newUnit === 'deg' ? 2 : 1));

    try {
      await api.invoke('set_setting', { key: 'grouping.threshold.value', value: String(newValue) });
      await api.invoke('set_setting', { key: 'grouping.threshold.unit', value: newUnit });
    } catch (err) {
      console.error('Failed to save threshold unit:', err);
    }
  };

  const handleThresholdValueChange = async (raw: string) => {
    setCustomThreshold(raw);
    const v = parseFloat(raw);
    if (isNaN(v) || v < 0) return;
    try {
      await api.invoke('set_setting', { key: 'grouping.threshold.value', value: String(v) });
      await api.invoke('set_setting', { key: 'grouping.threshold.unit', value: thresholdUnit });
    } catch (err) {
      console.error('Failed to save threshold value:', err);
    }
  };

  const handleResetThreshold = async () => {
    // Reset to 3 degrees (the hardcoded default)
    setCustomThreshold('3.00');
    setThresholdUnit('deg');
    setDefaultThreshold(3.0);
    try {
      await api.invoke('set_setting', { key: 'grouping.threshold.value', value: '3.0' });
      await api.invoke('set_setting', { key: 'grouping.threshold.unit', value: 'deg' });
    } catch (err) {
      console.error('Failed to reset threshold:', err);
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

  // Helper function to detect suggested merges between new and existing sets.
  //
  // A suggestion is emitted only when both sides agree:
  //   1. Names match exactly after normalization (lowercase + collapsed whitespace).
  //      Substring matching is wrong here — "M 4" is a prefix of "M 42" and they
  //      are different objects.
  //   2. Centroid coordinates are within the user's grouping threshold.
  //      Two unrelated targets occasionally share a name string; coord
  //      proximity is the authoritative same-object signal.
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

    const normalize = (n: string | null) =>
      (n || '').toLowerCase().replace(/\s+/g, ' ').trim();

    // Find sets that are in newSets but not in oldSets (newly created)
    const oldSetIds = new Set(oldSets.map(s => s.frames_set.id));
    const recentSets = newSets.filter(s => !oldSetIds.has(s.frames_set.id!));

    for (const recentSet of recentSets) {
      const recentName = normalize(recentSet.frames_set.name);
      if (!recentName) continue;

      const recentRa = parseRa(recentSet.frames_set.objctra || '');
      const recentDec = parseDec(recentSet.frames_set.objctdec || '');
      if (recentRa === null || recentDec === null) continue;

      for (const existingSet of oldSets) {
        const existingName = normalize(existingSet.frames_set.name);
        if (existingName !== recentName) continue;

        const existingRa = parseRa(existingSet.frames_set.objctra || '');
        const existingDec = parseDec(existingSet.frames_set.objctdec || '');
        if (existingRa === null || existingDec === null) continue;

        const distDeg = angularDistance(recentRa, recentDec, existingRa, existingDec);
        if (distDeg > defaultThreshold) continue;

        if (recentSet.frames_set.id && existingSet.frames_set.id) {
          const distArcmin = distDeg * 60;
          suggestions.push({
            sourceId: recentSet.frames_set.id,
            targetId: existingSet.frames_set.id,
            sourceName: recentSet.frames_set.name || 'Untitled',
            targetName: existingSet.frames_set.name || 'Untitled',
            reason: `Same object "${existingName}" · ${distArcmin.toFixed(1)}′ apart`,
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
      setSuggestedMerges([]);

      // Store current frame sets before auto-generate
      const setsBeforeGenerate = frameSets;

      // Resolve customThreshold (displayed in thresholdUnit) to degrees
      const parsedDisplay = parseFloat(customThreshold);
      let resolvedDeg: number = defaultThreshold;
      if (!isNaN(parsedDisplay) && parsedDisplay > 0) {
        switch (thresholdUnit) {
          case 'arcsec': resolvedDeg = parsedDisplay / 3600; break;
          case 'arcmin': resolvedDeg = parsedDisplay / 60; break;
          default: resolvedDeg = parsedDisplay;
        }
      }

      // Only pass threshold if different from the persisted default
      let thresholdDeg: number | null = null;
      if (!isNaN(resolvedDeg) && resolvedDeg > 0 && Math.abs(resolvedDeg - defaultThreshold) > 0.001) {
        thresholdDeg = resolvedDeg;
      }

      const result = await api.invoke<AutoGenerateResult>('auto_generate_frame_sets', {
        projectId: PROJECT_ID,
        thresholdDeg,
      });

      // Notify via the notification system
      const detailParts: string[] = [];
      if (result.frames_already_in_sets > 0) detailParts.push(`${result.frames_already_in_sets} already in sets`);
      if (result.frames_excluded > 0) detailParts.push(`${result.frames_excluded} excluded`);
      const detail = detailParts.length > 0 ? detailParts.join(', ') : 'No changes';

      notify({
        title: `Frame sets generated — ${result.sets_created} set${result.sets_created === 1 ? '' : 's'}, ${result.frames_clustered} frames`,
        detail,
        kind: 'merge',
        tone: result.sets_created > 0 ? 'success' : 'info',
        hasErrors: false,
        link: result.frames_excluded > 0 ? '/excluded' : undefined,
      });

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

  const handleArchive = async (setId: number) => {
    try {
      await api.invoke('archive_frame_set', { framesSetId: setId });
      await loadFrameSets();
    } catch (err) {
      setError(err as string);
      console.error('Failed to archive frame set:', err);
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
          // On WIP tab, only allow custom (WIP) sets as drop targets — Stage sets are sources only
          if (activeTab === 'wip') {
            const hoveredSet = frameSets.find(fs => fs.frames_set.id === hoveredSetId);
            if (hoveredSet && !hoveredSet.frames_set.is_custom) {
              if (dropTargetId !== null) setDropTargetId(null);
              return;
            }
          }
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

  // Filter frame sets based on active tab, then user filters
  const filteredFrameSets = useMemo(() => {
    // Tab filter first
    let result = frameSets.filter(fs => {
      switch (activeTab) {
        case 'stage':
          return !fs.frames_set.is_custom && !fs.frames_set.is_archived;
        case 'wip':
          // In merge mode, show Stage sets alongside WIP sets so users can drag them onto WIP targets
          if (isMergeMode) return !fs.frames_set.is_archived;
          return fs.frames_set.is_custom && !fs.frames_set.is_archived;
        case 'archive':
          return fs.frames_set.is_archived;
        default:
          return true;
      }
    });

    // Name search (case-insensitive contains)
    if (filters.nameSearch.trim()) {
      const search = filters.nameSearch.toLowerCase();
      result = result.filter(fs =>
        (fs.frames_set.name || '').toLowerCase().includes(search)
      );
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
  }, [frameSets, filters, activeTab, isMergeMode]);

  // Tab counts for badges
  const tabCounts = useMemo(() => ({
    stage: frameSets.filter(fs => !fs.frames_set.is_custom && !fs.frames_set.is_archived).length,
    wip: frameSets.filter(fs => fs.frames_set.is_custom && !fs.frames_set.is_archived).length,
    archive: frameSets.filter(fs => fs.frames_set.is_archived).length,
  }), [frameSets]);

  const activeFilterCount = countActiveFilters(filters);

  return (
    <div className="p-4 pt-3">
      {/* Title block + compact action buttons (kept ~h2 height so the tab
          row still aligns with the File Manager tab strip) */}
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-2xl font-bold">
          Objects Library
          <span className="text-sm font-normal text-content-muted ml-3">Frame sets grouped by sky coordinates</span>
        </h2>
        <div className="flex items-center gap-2">
          {excludedCount > 0 && (
            <button
              onClick={() => navigate('/excluded')}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg bg-surface-hover hover:brightness-110 text-warning transition-colors"
              title="View excluded frames"
            >
              <FileX size={15} />
              {excludedCount} excluded
            </button>
          )}
          {/* Merge mode — available in Stage and WIP */}
          {(activeTab === 'stage' || activeTab === 'wip') && (
            <>
              <button
                onClick={() => setIsMergeMode(prev => !prev)}
                className={`flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg transition-colors ${
                  isMergeMode
                    ? 'bg-success hover:brightness-90 text-surface'
                    : 'bg-surface-hover hover:bg-surface-hover text-content-secondary'
                }`}
                title={`${isMergeMode ? 'Exit' : 'Enter'} Merge Mode (M)`}
              >
                <Grip size={15} />
                {isMergeMode ? 'Exit Merge Mode' : 'Merge Mode'}
              </button>
              <div className="h-5 w-px bg-border" />
            </>
          )}
          {/* View mode toggle (all tabs) */}
          <div className="flex items-center bg-surface-hover rounded-lg p-0.5">
            <button
              onClick={() => handleViewModeChange('cards')}
              className={`p-1 rounded transition-colors ${
                viewMode === 'cards'
                  ? 'bg-accent text-surface'
                  : 'text-content-muted hover:text-content'
              }`}
              title="Card view"
            >
              <LayoutGrid size={16} />
            </button>
            <button
              onClick={() => handleViewModeChange('table')}
              className={`p-1 rounded transition-colors ${
                viewMode === 'table'
                  ? 'bg-accent text-surface'
                  : 'text-content-muted hover:text-content'
              }`}
              title="Table view"
            >
              <Table2 size={16} />
            </button>
          </div>
          <button
            onClick={() => setShowFilterPanel(prev => !prev)}
            className={`relative flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg transition-colors ${
              showFilterPanel || activeFilterCount > 0
                ? 'bg-accent hover:bg-accent-hover text-surface'
                : 'bg-surface-hover hover:bg-surface-hover text-content-secondary'
            }`}
            title="Filter frame sets"
          >
            <Filter size={15} />
            Filter
            {activeFilterCount > 0 && (
              <span className="absolute -top-1 -right-1 bg-orange text-surface text-xs font-bold rounded-full w-5 h-5 flex items-center justify-center">
                {activeFilterCount}
              </span>
            )}
          </button>
        </div>
      </div>

      {/* Tab bar — tabs only; sized + positioned to match File Manager */}
      <div className="flex gap-2 mb-3 border-b border-border">
          {([
            { key: 'stage' as ObjectsTab, label: 'Stage', icon: Sparkles },
            { key: 'wip' as ObjectsTab, label: 'Work In Progress', icon: Star },
            { key: 'archive' as ObjectsTab, label: 'Archive', icon: Archive },
          ]).map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              onClick={() => { setActiveTab(key); if (key === 'archive') setIsMergeMode(false); }}
              className={`px-4 py-2 transition relative ${
                activeTab === key
                  ? 'text-accent border-b-2 border-accent'
                  : 'text-content-muted hover:text-content'
              }`}
            >
              <div className="flex items-center gap-2">
                <Icon size={16} />
                {label}
                {tabCounts[key] > 0 && (
                  <span className={`text-xs px-1.5 py-0.5 rounded-full ${
                    activeTab === key
                      ? 'bg-accent/20 text-accent'
                      : 'bg-surface-hover text-content-muted'
                  }`}>
                    {tabCounts[key]}
                  </span>
                )}
              </div>
            </button>
          ))}
      </div>

      {/* Stage toolbar — always visible when on the Stage tab */}
      {activeTab === 'stage' && (
        <div className="mt-3 mb-2">
          <ToolbarContainer>
            <div className="flex items-center gap-1.5 flex-shrink-0">
              <span className="text-xs text-content-muted">Threshold</span>
              <input
                type="number"
                value={customThreshold}
                onChange={(e) => handleThresholdValueChange(e.target.value)}
                step="0.1"
                min="0"
                className="h-7 w-20 text-xs bg-surface-hover border border-border rounded-lg px-2 text-content focus:outline-none focus:ring-2 focus:ring-accent"
              />
            </div>
            <select
              value={thresholdUnit}
              onChange={(e) => handleThresholdUnitChange(e.target.value as 'deg' | 'arcmin' | 'arcsec')}
              title="Threshold unit"
              className="h-7 text-xs bg-surface-hover border border-border rounded-lg px-2 text-content focus:outline-none focus:ring-2 focus:ring-accent"
            >
              <option value="deg">degrees</option>
              <option value="arcmin">arcmin</option>
              <option value="arcsec">arcsec</option>
            </select>
            <ToolbarButton
              variant="default"
              icon={RotateCcw}
              onClick={handleResetThreshold}
              title="Reset to default (3°)"
            />
            <ToolbarDivider />
            <ToolbarButton
              variant="primary"
              icon={Sparkles}
              onClick={handleAutoGenerate}
              disabled={generating}
            >
              {generating ? 'Generating…' : 'Auto Generate'}
            </ToolbarButton>
            <ToolbarButton
              variant="danger"
              icon={RotateCcw}
              onClick={handleDeleteAutoGenerated}
              disabled={
                deletingAutoSets ||
                frameSets.filter(fs => !fs.frames_set.is_custom && !fs.frames_set.is_archived).length === 0
              }
              title={`Purge ${frameSets.filter(fs => !fs.frames_set.is_custom && !fs.frames_set.is_archived).length} auto-generated sets (custom sets are kept)`}
            >
              Purge Auto
            </ToolbarButton>
            {isMergeMode && (
              <>
                <ToolbarDivider />
                <ToolbarInfo icon={Grip}>
                  Merge Mode — drag and drop frame sets to merge them
                </ToolbarInfo>
                <ToolbarButton
                  variant="default"
                  onClick={() => setIsMergeMode(false)}
                  title="Exit Merge Mode (M)"
                >
                  Exit (M)
                </ToolbarButton>
              </>
            )}
          </ToolbarContainer>
        </div>
      )}

      {/* Filter Panel */}
      <ObjectsFilterPanel
        filters={filters}
        onChange={setFilters}
        isOpen={showFilterPanel}
      />

      {/* Results Summary */}
      {activeFilterCount > 0 && (
        <div className="mb-4 text-sm text-content-muted">
          Showing {filteredFrameSets.length} of {tabCounts[activeTab]} frame set{tabCounts[activeTab] !== 1 ? 's' : ''}
        </div>
      )}

      {activeTab === 'wip' && isMergeMode && (
        <div className="mb-4">
          <ToolbarContainer>
            <ToolbarInfo icon={Grip}>
              Merge Mode — drag Stage sets onto WIP sets to merge new data
            </ToolbarInfo>
            <ToolbarButton
              variant="default"
              onClick={() => setIsMergeMode(false)}
              title="Exit Merge Mode (M)"
            >
              Exit (M)
            </ToolbarButton>
          </ToolbarContainer>
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
      ) : filteredFrameSets.length === 0 && activeFilterCount > 0 ? (
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
      ) : filteredFrameSets.length === 0 ? (
        <div className="bg-surface-elevated rounded-lg p-8 text-center">
          {activeTab === 'stage' ? (
            <>
              <Target className="mx-auto mb-4 text-content-muted" size={48} />
              <p className="text-content-muted mb-4">
                No staged frame sets. Use "Auto-Generate Sets" to cluster your LIGHT frames by sky coordinates.
              </p>
            </>
          ) : activeTab === 'wip' ? (
            <>
              <Star className="mx-auto mb-4 text-content-muted" size={48} />
              <p className="text-content-muted mb-4">
                No work in progress. Promote sets from Stage by marking them as custom.
              </p>
            </>
          ) : (
            <>
              <Archive className="mx-auto mb-4 text-content-muted" size={48} />
              <p className="text-content-muted mb-4">
                No archived sets.
              </p>
            </>
          )}
        </div>
      ) : viewMode === 'table' ? (
        <ObjectsTableView
          frameSets={filteredFrameSets}
          activeTab={activeTab}
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
          onArchive={handleArchive}
        />
      ) : (
        <div className={`grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 2xl:grid-cols-5 gap-3 ${isDragging || isMergeMode ? 'select-none' : ''}`}>
          {filteredFrameSets.map(({ frames_set, member_count }) => (
            <div
              key={frames_set.id}
              data-set-id={frames_set.id}
              onMouseDown={(e) => !editingSetId && isMergeMode && handleMouseDown(e, frames_set.id!)}
              className={`bg-surface-elevated rounded-lg p-3 border border-l-4 transition-all duration-200 group ${
                isDragging && draggedSetId === frames_set.id
                  ? 'opacity-40 border-accent shadow-lg shadow-accent/50 cursor-grabbing select-none'
                  : dropTargetId === frames_set.id
                  ? 'border-success bg-success-muted scale-105 shadow-lg shadow-success/50'
                  : activeTab === 'wip' && !frames_set.is_custom
                  ? 'border-dashed border-border border-l-accent opacity-60'
                  : `border-border ${frames_set.is_custom ? 'border-l-orange' : 'border-l-accent'}`
              } ${!editingSetId && isMergeMode && !isDragging ? 'cursor-grab' : ''} ${isDragging ? 'select-none' : ''}`}
            >
              {/* Name row */}
              <div className="flex-1 min-w-0 mb-1.5">
                {editingSetId === frames_set.id ? (
                  <div className="flex items-center gap-1">
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
                      className="flex-1 px-1.5 py-0.5 bg-surface-hover text-content rounded border border-border focus:outline-none focus:border-accent text-sm"
                      autoFocus
                    />
                    <button
                      onClick={() => saveRename(frames_set.id!)}
                      className="p-0.5 text-success hover:text-success/90"
                      title="Save"
                    >
                      <Check size={14} />
                    </button>
                    <button
                      onClick={cancelEditing}
                      className="p-0.5 text-error hover:text-error/90"
                      title="Cancel"
                    >
                      <X size={14} />
                    </button>
                  </div>
                ) : (
                  <div className="flex items-center gap-1.5">
                    {frames_set.is_custom ? (
                      <span title="Custom Set">
                        <Star size={13} className="text-orange fill-orange flex-shrink-0" />
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
                        <Star size={13} className="flex-shrink-0" />
                      </button>
                    )}
                    <h3 className="text-sm font-semibold text-content truncate">
                      {frames_set.name || 'Untitled'}
                    </h3>
                    {activeTab === 'wip' && !frames_set.is_custom && (
                      <span className="px-1 py-0.5 text-[9px] font-semibold uppercase rounded bg-surface-hover text-content-muted flex-shrink-0">
                        Stage
                      </span>
                    )}
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        startEditing(frames_set.id!, frames_set.name);
                      }}
                      className="p-0.5 text-content-muted hover:text-content opacity-0 group-hover:opacity-100 transition-opacity"
                      title="Rename"
                    >
                      <Pencil size={12} />
                    </button>
                  </div>
                )}
              </div>

              {/* Date range */}
              {frames_set.date_obs_start && (
                <div className="flex items-center gap-1 text-content-secondary text-xs mb-1">
                  <Clock size={11} className="text-content-muted flex-shrink-0" />
                  <span>
                    {frames_set.date_obs_end && frames_set.date_obs_start !== frames_set.date_obs_end
                      ? `${new Date(frames_set.date_obs_start).toLocaleDateString()} – ${new Date(frames_set.date_obs_end).toLocaleDateString()}`
                      : new Date(frames_set.date_obs_start).toLocaleDateString()
                    }
                  </span>
                </div>
              )}

              {/* Coords + rotation on one line */}
              <div className="flex items-center gap-2 text-content-muted text-[10px] mb-2">
                {frames_set.objctra && frames_set.objctdec && (
                  <span className="flex items-center gap-0.5 font-mono">
                    <MapPin size={10} />
                    {frames_set.objctra} / {frames_set.objctdec}
                  </span>
                )}
                {frames_set.avg_rotation != null && (
                  <span className="flex items-center gap-0.5 font-mono">
                    <RotateCw size={10} />
                    {frames_set.min_rotation != null && frames_set.max_rotation != null &&
                     Math.abs(frames_set.max_rotation - frames_set.min_rotation) >= 1
                      ? `${frames_set.min_rotation.toFixed(0)}°–${frames_set.max_rotation.toFixed(0)}°`
                      : `${frames_set.avg_rotation.toFixed(0)}°`
                    }
                  </span>
                )}
              </div>

              {/* Accented stats row */}
              <div className="flex items-center justify-between mb-2 text-xs">
                <span className="flex items-center gap-1 text-accent font-semibold">
                  <Clock size={12} />
                  {formatExposureTime(frames_set.total_exp_time)}
                </span>
                <span className="text-content-muted">
                  {member_count} frame{member_count !== 1 ? 's' : ''}
                </span>
              </div>

              <div className="flex gap-1.5">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    navigate(`/objects/${frames_set.id}`);
                  }}
                  className="flex-1 flex items-center justify-center gap-1 px-2 py-1 bg-surface-hover hover:bg-surface-hover text-content rounded transition-colors text-xs"
                  title="View members"
                >
                  <Eye size={13} />
                  View
                </button>
                {activeTab !== 'archive' && (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleArchive(frames_set.id!);
                    }}
                    className="px-2 py-1 bg-surface-hover hover:bg-surface-hover text-content-muted hover:text-content rounded transition-colors"
                    title="Archive"
                  >
                    <Archive size={13} />
                  </button>
                )}
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleDelete(frames_set.id!, frames_set.name);
                  }}
                  className="px-2 py-1 bg-error-muted hover:bg-error/30 text-error rounded transition-colors"
                  title="Delete set"
                >
                  <Trash2 size={13} />
                </button>
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

      {/* Purge Auto-Generated Sets Confirmation Dialog */}
      <ConfirmDialog
        isOpen={showDeleteAutoSetsConfirm}
        title="Purge Auto-Generated Sets?"
        message={`This will purge all ${frameSets.filter(fs => !fs.frames_set.is_custom).length} auto-generated frame sets.\n\nCustom sets and the underlying frames are kept — only the auto-generated groupings are removed.`}
        onConfirm={confirmDeleteAutoGenerated}
        onCancel={() => setShowDeleteAutoSetsConfirm(false)}
        confirmText="Purge Auto-Generated"
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
