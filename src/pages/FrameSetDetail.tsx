import { useState, useEffect, useCallback, useMemo } from 'react';
import { useParams, useNavigate, useLocation, useSearchParams } from 'react-router-dom';
import { api } from '../api';
import { ArrowLeft, MapPin, RotateCw, AlertCircle, Scissors, BarChart3, Crosshair, History, Search, Archive as ArchiveIcon, Layers, AlignHorizontalJustifyCenter, Users } from 'lucide-react';
import type { FrameSetDetail, FileWithFrame, CalibrationHierarchyView, FrameAnalysis, FindNewFramesResult, MergeReport, FrameSetReference, LightFrameReadiness, LightCalReadiness, LightCalDetails, PortalNewProjectLink } from '../types/models';
import BlinkViewer from '../components/BlinkViewer';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';
import { CalibrationHierarchyView as CalibrationHierarchyViewComponent } from '../components/CalibrationHierarchyView';
import { LightsAnalysisView } from '../components/LightsAnalysisView';
import { FindNewImagesDialog } from '../components/FindNewImagesDialog';
import { CalibrateLightsDialog, readFlatNormPref, readFlatNormModePref, readLightCalParamsPref } from '../components/calibration/CalibrateLightsDialog';
import { FrameSetHistoryTab } from '../components/FrameSetHistoryTab';
import { useBlackholeEvents } from '../hooks/useBlackholeEvents';
import { buildCameraFilterTree, buildMergedCameraFilterTree } from '../components/calibration/utils';
import { ArchiveDispositionDialog } from '../components/archive/ArchiveDispositionDialog';
import { ArchiveProgress } from '../components/archive/ArchiveProgress';
import { RestoreDialog } from '../components/archive/RestoreDialog';
import { ExportTab } from '../components/export/ExportTab';
import { getArchiveSettings, listArchiveRoots, startArchiveOperation, listArchivedFrameSets, listArchiveZips } from '../api/archive';
import { StackingPrepTab } from '../components/StackingPrepTab';
import { revealItemInDir, openUrl } from '../api/desktop';
import { safeExternalUrl } from '../utils/externalUrl';
import { useNotifications } from '../contexts/NotificationContext';
import { isTauri } from '../utils/platform';
import { Upload, FolderOpen } from 'lucide-react';
import type { ArchiveCompression, Dispositions, ConflictResolution } from '../types/archive';
import type { ArchivedFrameSetSummary } from '../types/helpers';

type FrameSetTab = 'calibration' | 'analysis' | 'history' | 'export' | 'registration';

// The Registration (frame alignment / stacking preparation) feature is still
// under active development. It stays fully functional in dev builds so work can
// continue, but is disabled in production/release builds (the tab is shown
// greyed with an "under development" tooltip). Gated on the Vite dev flag, which
// is true for `tauri dev` / `dev:web` and false for `tauri build` / `build:web`.
const REGISTRATION_ENABLED = import.meta.env.DEV;

export default function FrameSetDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const location = useLocation();
  const { notify } = useNotifications();
  const [detail, setDetail] = useState<FrameSetDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Custom set creation dialog
  const [customSetName, setCustomSetName] = useState('');
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [creating, setCreating] = useState(false);

  // Blink viewer state
  const [blinkFrames, setBlinkFrames] = useState<FileWithFrame[] | null>(null);

  // Split dialog state
  const [showSplitDialog, setShowSplitDialog] = useState(false);
  const [splitName, setSplitName] = useState('');
  const [splitting, setSplitting] = useState(false);

  // Delete confirmation
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);

  // Alert dialog
  const [alertDialog, setAlertDialog] = useState<{
    isOpen: boolean;
    title: string;
    message: string;
    variant: 'error' | 'warning' | 'info';
  }>({
    isOpen: false,
    title: '',
    message: '',
    variant: 'info',
  });

  // Calibration hierarchy data (loaded on mount)
  const [calibrationHierarchy, setCalibrationHierarchy] = useState<CalibrationHierarchyView | null>(null);
  const [loadingCalibration, setLoadingCalibration] = useState(false);

  // Tab + highlight state. Initial values seed from URL params on first
  // render to avoid the analysis-tab flash when arriving via a cross-page
  // chip click. The reactive useEffect below handles in-page navigations
  // (e.g. clicking a `#setId` in the Export tab's WarningsPanel pushes
  // `?tab=calibration&highlightSet=…&kind=…` and we re-consume them).
  const [searchParams, setSearchParams] = useSearchParams();
  // 'registration' deep-link always falls back to 'analysis' on initial render
  // (we don't know the gate state yet). The searchParams useEffect below will
  // switch to 'registration' once the reference state is resolved if appropriate.
  const initialTabFromUrl: FrameSetTab | undefined =
    searchParams.get('tab') === 'calibration' ? 'calibration'
    : searchParams.get('tab') === 'history' ? 'history'
    : searchParams.get('tab') === 'analysis' ? 'analysis'
    : searchParams.get('tab') === 'export' ? 'export'
    : undefined; // 'registration' intentionally excluded — gating not known yet
  const [activeTab, setActiveTab] = useState<FrameSetTab>(initialTabFromUrl ?? 'analysis');

  const initialHighlightSetId = (() => {
    const v = searchParams.get('highlightSet');
    return v != null && /^\d+$/.test(v) ? parseInt(v, 10) : null;
  })();
  const initialHighlightKind = (() => {
    const v = searchParams.get('kind');
    return v === 'flat' || v === 'dark' || v === 'bias' ? v : null;
  })();
  const [pendingHighlightCalSet, setPendingHighlightCalSet] = useState<
    { setId: number; kind: 'flat' | 'dark' | 'bias' } | null
  >(
    initialHighlightSetId != null && initialHighlightKind != null
      ? { setId: initialHighlightSetId, kind: initialHighlightKind }
      : null
  );

  // Watch searchParams so URL-driven jumps work BOTH on initial mount and
  // on subsequent in-page updates (e.g. the Export tab pushing a new
  // `?tab=calibration&highlightSet=…&kind=…`). On match, sync state and
  // clear the params so the URL stays clean.
  useEffect(() => {
    const tabParam = searchParams.get('tab');
    const highlightSetParam = searchParams.get('highlightSet');
    const kindParam = searchParams.get('kind');

    if (!tabParam && !highlightSetParam && !kindParam) return;

    if (tabParam === 'calibration' || tabParam === 'history' || tabParam === 'analysis' || tabParam === 'export') {
      setActiveTab(tabParam);
    } else if (tabParam === 'registration') {
      // Only allow direct navigation to registration if it is enabled and not
      // gated. In production REGISTRATION_ENABLED is false, so a deep link can
      // never land on the under-development tab. registrationTabReady may not be
      // resolved yet on first render (reference still loading); fall back to
      // 'analysis' if it's clearly unavailable.
      if (REGISTRATION_ENABLED && referenceFrameId !== undefined && registrationTabReady) {
        setActiveTab('registration');
      } else {
        setActiveTab('analysis');
      }
    }

    const id = highlightSetParam != null && /^\d+$/.test(highlightSetParam)
      ? parseInt(highlightSetParam, 10)
      : null;
    const kind = kindParam === 'flat' || kindParam === 'dark' || kindParam === 'bias'
      ? kindParam
      : null;
    if (id != null && kind != null) {
      setPendingHighlightCalSet({ setId: id, kind });
    }

    const next = new URLSearchParams(searchParams);
    next.delete('tab');
    next.delete('highlightSet');
    next.delete('kind');
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams]);
  const [showFindNewDialog, setShowFindNewDialog] = useState(false);
  const [historyRefreshKey, setHistoryRefreshKey] = useState(0);
  const [findNewBusy, setFindNewBusy] = useState(false);

  // Light-calibration state: dialog visibility + per-frame readiness for the
  // status badges in the frame table (fetched once per set view, not per frame).
  const [showCalibrateDialog, setShowCalibrateDialog] = useState(false);
  const [lightCalReadiness, setLightCalReadiness] = useState<Map<number, LightFrameReadiness>>(new Map());
  // Per-frame calibration recipe (tracking-row truth) for the Coverage table's
  // status-badge tooltip. Fetched once per set view alongside readiness.
  const [lightCalDetails, setLightCalDetails] = useState<Map<number, LightCalDetails>>(new Map());

  // Archive state
  const [showArchiveDialog, setShowArchiveDialog] = useState(false);
  const [archiveCompression, setArchiveCompression] = useState<ArchiveCompression>('store');
  const [archiving, setArchiving] = useState(false);
  const [movingToArchive, setMovingToArchive] = useState(false);
  const [activeArchiveOpId, setActiveArchiveOpId] = useState<number | null>(null);
  const [restoreItem, setRestoreItem] = useState<ArchivedFrameSetSummary | null>(null);

  // Handle "Find new images" click: if the user has trusted auto-merge for
  // the button path, skip the preview dialog and merge directly; otherwise,
  // open the dialog for manual confirmation.
  const handleFindNewClick = useCallback(async () => {
    const trustSetting = await api.invoke<string>('get_setting', {
      key: 'auto_merge.on_button_click',
      defaultValue: 'false',
    });
    if (trustSetting.toLowerCase() !== 'true') {
      setShowFindNewDialog(true);
      return;
    }

    setFindNewBusy(true);
    try {
      const result = await api.invoke<FindNewFramesResult>('find_new_frames_for_set', {
        framesSetId: parseInt(id!),
        scanFirst: false,
      });
      if (result.candidates.length === 0) {
        setAlertDialog({
          isOpen: true,
          title: 'No new images',
          message: 'No unclustered lights match this target\'s coordinates.',
          variant: 'info',
        });
        return;
      }
      const report = await api.invoke<MergeReport>('auto_merge_new_frames_for_set', {
        framesSetId: parseInt(id!),
        frameIds: result.candidates.map((c) => c.frame_id),
        source: 'button',
      });
      loadData();
      setHistoryRefreshKey((k) => k + 1);
      setAlertDialog({
        isOpen: true,
        title: 'Merge complete',
        message: `Added ${report.added_count} frame${report.added_count === 1 ? '' : 's'}${report.skipped_count ? `, skipped ${report.skipped_count}` : ''}. See the History tab for details.`,
        variant: 'info',
      });
    } catch (e) {
      setAlertDialog({
        isOpen: true,
        title: 'Find new images failed',
        message: String(e),
        variant: 'error',
      });
    } finally {
      setFindNewBusy(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  const handleMoveToArchive = useCallback(async () => {
    if (!detail?.frames_set?.id) return;
    setMovingToArchive(true);
    try {
      await api.invoke('archive_frame_set', { framesSetId: detail.frames_set.id });
      // Reload so the toolbar contextual button switches from "Find new images +
      // Move to Archive" to "Move and ZIP".
      await loadData();
    } catch (e) {
      console.error('move to archive failed', e);
      alert(`Failed to move to archive: ${e}`);
    } finally {
      setMovingToArchive(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail?.frames_set?.id]);

  const handleArchiveClick = useCallback(async () => {
    // Verify there's at least one archive folder configured before opening the dialog.
    try {
      const roots = await listArchiveRoots();
      if (roots.length === 0) {
        alert('No archive folders configured yet. Add one in File Manager → Archive Folders, then come back.');
        return;
      }
    } catch (e) {
      console.error('Failed to list archive roots', e);
      alert(`Failed to load archive folders: ${e}`);
      return;
    }
    // Pull compression default from settings.
    try {
      const settings = await getArchiveSettings();
      setArchiveCompression(settings.compression);
    } catch (e) {
      console.error('Failed to load archive compression setting', e);
    }
    setShowArchiveDialog(true);
  }, []);

  const handleStartArchive = useCallback(async (
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflictResolution: ConflictResolution,
    archiveRootPath: string,
  ) => {
    if (!detail?.frames_set?.id) return;
    setShowArchiveDialog(false);
    setArchiving(true);
    try {
      const opId = await startArchiveOperation(
        detail.frames_set.id,
        dispositions,
        compression,
        conflictResolution,
        archiveRootPath,
      );
      setActiveArchiveOpId(opId);
    } catch (e) {
      console.error('start archive failed', e);
      alert(`Archive failed to start: ${e}`);
    } finally {
      setArchiving(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail?.frames_set?.id]);

  // Frame IDs collected from the active tree view (resolved by the child to handle
  // both by-night and by-camera key shapes).
  const [selectedFrameIds, setSelectedFrameIds] = useState<number[]>([]);

  // Analysis data for SNR display in tree
  const [analysisData, setAnalysisData] = useState<Map<number, FrameAnalysis>>(new Map());

  // User-chosen reference frame for Stacking Preparation gating.
  // null = none chosen, undefined = not yet loaded.
  const [referenceFrameId, setReferenceFrameId] = useState<number | null | undefined>(undefined);

  // The registration tab is enabled only when (a) analysis exists and (b) a
  // reference frame has been chosen.
  const registrationTabReady =
    analysisData.size > 0 && referenceFrameId != null && referenceFrameId !== undefined;

  // Reactive blackhole state — derives file IDs from hierarchy, fetches status, listens for events
  const allFileIds = useMemo(() => {
    if (!calibrationHierarchy) return [];
    const ids: number[] = [];
    for (const dg of calibrationHierarchy.date_groups)
      for (const cg of dg.camera_groups)
        for (const fg of cg.filter_groups)
          for (const f of fg.light_frames)
            ids.push(f.file_id);
    return ids;
  }, [calibrationHierarchy]);
  const { blackholedFileIds } = useBlackholeEvents(allFileIds);

  // Compute stacked SNR per filter group for calibration tab tree: dB→linear, sqrt(sum(linear²)), back to dB
  const calibrationFilterSnrMap = useMemo(() => {
    if (!calibrationHierarchy || analysisData.size === 0) return undefined;
    const dateTree = buildCameraFilterTree(calibrationHierarchy);
    const mergedTree = buildMergedCameraFilterTree(calibrationHierarchy);
    const map = new Map<string, number>();
    for (const tree of [dateTree, mergedTree]) {
      for (const [key, frames] of tree.framesByKey) {
        if (map.has(key)) continue;
        let sumSq = 0;
        let count = 0;
        for (const f of frames) {
          if (blackholedFileIds.has(f.file_id)) continue;
          const a = analysisData.get(f.frame_id);
          if (a) {
            const linear = Math.pow(10, a.frame_snr / 20);
            sumSq += linear * linear;
            count++;
          }
        }
        if (count > 0) {
          const stackedLinear = Math.sqrt(sumSq);
          map.set(key, 20 * Math.log10(stackedLinear));
        }
      }
    }
    return map.size > 0 ? map : undefined;
  }, [calibrationHierarchy, analysisData, blackholedFileIds]);

  // Load data on mount and when navigating back
  useEffect(() => {
    loadData();
  }, [id, location.key]);

  // Refresh analysis data when analysis completes
  useEffect(() => {
    if (!id) return;
    let unlisten: (() => void) | null = null;
    (async () => {
      unlisten = await api.listen('analysis-complete', async () => {
        try {
          const results = await api.invoke<FrameAnalysis[]>('get_analysis_for_frame_set', { frameSetId: parseInt(id) });
          const aMap = new Map<number, FrameAnalysis>();
          for (const a of results) aMap.set(a.frame_id, a);
          setAnalysisData(aMap);
        } catch { /* ignore */ }
      });
    })();
    return () => { unlisten?.(); };
  }, [id]);

  const loadData = async () => {
    if (!id) return;

    try {
      setLoading(true);
      setLoadingCalibration(true);
      setError(null);

      // Load all in parallel
      const [detailResult, hierarchyResult, analysisResult, referenceResult] = await Promise.all([
        api.invoke<FrameSetDetail>('get_frame_set_detail', {
          framesSetId: parseInt(id),
        }),
        api.invoke<CalibrationHierarchyView>('get_calibration_hierarchy_for_frame_set', {
          frameSetId: parseInt(id),
        }),
        api.invoke<FrameAnalysis[]>('get_analysis_for_frame_set', {
          frameSetId: parseInt(id),
        }).catch(() => [] as FrameAnalysis[]),
        api.invoke<FrameSetReference | null>('get_frame_set_reference', {
          framesSetId: parseInt(id),
        }).catch(() => null),
      ]);

      setDetail(detailResult);
      setCalibrationHierarchy(hierarchyResult);
      const aMap = new Map<number, FrameAnalysis>();
      for (const a of analysisResult) aMap.set(a.frame_id, a);
      setAnalysisData(aMap);
      setReferenceFrameId(referenceResult?.referenceFrameId ?? null);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
      setLoadingCalibration(false);
    }
  };

  // Refresh calibration hierarchy without showing loading spinner (keeps expanded state)
  const refreshCalibrationHierarchy = useCallback(async () => {
    if (!id) return;
    try {
      const result = await api.invoke<CalibrationHierarchyView>('get_calibration_hierarchy_for_frame_set', {
        frameSetId: parseInt(id),
      });
      setCalibrationHierarchy(result);
    } catch (err) {
      console.error('Failed to refresh calibration hierarchy:', err);
    }
  }, [id]);

  // Light-calibration readiness for the per-frame status badges. Cheap DB-only
  // call, fetched once per set view (and again after a calibration or master
  // build changes the links). Uses the persisted flat-norm preference so the
  // Stale badge matches what the dialog would report. Failures are non-fatal —
  // the badges simply don't render.
  const loadLightCalReadiness = useCallback(async () => {
    if (!id) return;
    try {
      const readiness = await api.invoke<LightCalReadiness>('get_light_calibration_readiness', {
        setId: parseInt(id),
        flatNorm: readFlatNormPref(),
        flatNormMode: readFlatNormModePref(),
        params: readLightCalParamsPref(),
      });
      const map = new Map<number, LightFrameReadiness>();
      for (const f of readiness.frames) map.set(f.frameId, f);
      setLightCalReadiness(map);
    } catch (err) {
      console.error('[FrameSetDetail] get_light_calibration_readiness failed:', err);
    }
  }, [id]);

  // Per-frame calibration recipe (CALSTAT, applied masters, normalization, cal
  // params, calibrated-at, staleness) for the Coverage table tooltip. Same
  // preferences as readiness so the "stale" flag agrees with the badge; cheap
  // DB-only join, fetched once per set view. Failures are non-fatal.
  const loadLightCalDetails = useCallback(async () => {
    if (!id) return;
    try {
      const details = await api.invoke<LightCalDetails[]>('get_light_calibration_details', {
        setId: parseInt(id),
        flatNorm: readFlatNormPref(),
        flatNormMode: readFlatNormModePref(),
        params: readLightCalParamsPref(),
      });
      const map = new Map<number, LightCalDetails>();
      for (const d of details) map.set(d.frameId, d);
      setLightCalDetails(map);
    } catch (err) {
      console.error('[FrameSetDetail] get_light_calibration_details failed:', err);
    }
  }, [id]);

  // Refresh readiness + recipe details on mount / id change, and when a
  // calibration run or a master build (which repoints links) completes anywhere
  // in the app.
  useEffect(() => {
    loadLightCalReadiness();
    loadLightCalDetails();
    const onChanged = () => { loadLightCalReadiness(); loadLightCalDetails(); };
    window.addEventListener('light-cal-updated', onChanged);
    window.addEventListener('library-updated', onChanged);
    return () => {
      window.removeEventListener('light-cal-updated', onChanged);
      window.removeEventListener('library-updated', onChanged);
    };
  }, [loadLightCalReadiness, loadLightCalDetails]);

  const showAlert = (title: string, message: string, variant: 'error' | 'warning' | 'info' = 'info') => {
    setAlertDialog({ isOpen: true, title, message, variant });
  };

  const formatExposureTime = (seconds: number | null | undefined) => {
    if (!seconds) return 'N/A';
    const hours = (seconds / 3600).toFixed(1);
    const minutes = Math.round((seconds % 3600) / 60);
    return parseFloat(hours) >= 1 ? `${hours}h` : `${minutes}m`;
  };

  // Handle blink from LightsAnalysisView - load full frame data
  const handleBlink = useCallback(async (frameIds: number[]) => {
    if (frameIds.length === 0) {
      showAlert('No Frames', 'No frames selected for blink', 'warning');
      return;
    }

    try {
      // Load full frame data for the given frame IDs
      const frames = await api.invoke<FileWithFrame[]>('get_files_with_frames_by_ids', {
        frameIds,
      });

      // Filter only LIGHT frames with FITS or XISF format
      const lightFitsFrames = frames.filter(
        f => f.frame?.imagetyp === 'Light' && (f.file.format === 'FITS' || f.file.format === 'XISF')
      );

      if (lightFitsFrames.length === 0) {
        showAlert('No LIGHT Frames', 'No LIGHT frames found for blink', 'warning');
        return;
      }

      setBlinkFrames(lightFitsFrames);
    } catch (err) {
      console.error('Failed to load frames for blink:', err);
      showAlert('Error', 'Failed to load frames for blink: ' + String(err), 'error');
    }
  }, []);

  // Handle split from CalibrationHierarchyView
  const handleOpenSplitDialog = useCallback((frameIds: number[]) => {
    if (!id || frameIds.length === 0) return;

    setSelectedFrameIds(frameIds);

    // Pre-fill split name
    const originalName = detail?.frames_set?.name || 'Untitled';
    setSplitName(`${originalName} - Split 1`);
    setShowSplitDialog(true);
  }, [id, detail]);

  // Handle create custom set from CalibrationHierarchyView
  const handleOpenCreateDialog = useCallback((frameIds: number[]) => {
    if (frameIds.length === 0) return;

    setSelectedFrameIds(frameIds);
    setShowCreateDialog(true);
  }, []);

  const handleCreateCustomSet = async () => {
    if (!customSetName.trim()) {
      showAlert('Name Required', 'Please enter a name for the custom set', 'warning');
      return;
    }

    if (selectedFrameIds.length === 0) {
      showAlert('No Selection', 'Please select at least one filter group', 'warning');
      return;
    }

    try {
      setCreating(true);
      // Use existing command that creates frame set from frame IDs
      await api.invoke('create_frame_set_from_selection', {
        name: customSetName.trim(),
        frame_ids: selectedFrameIds,
        description: null,
      });

      // Success - silent update
      setShowCreateDialog(false);
      setCustomSetName('');
      setSelectedFrameIds([]);
      navigate('/objects');
    } catch (err) {
      showAlert('Creation Failed', 'Failed to create custom set: ' + String(err), 'error');
    } finally {
      setCreating(false);
    }
  };

  const handleSplit = async () => {
    if (!id || !splitName.trim()) {
      showAlert('Name Required', 'Please enter a name for the new frame set', 'warning');
      return;
    }

    if (selectedFrameIds.length === 0) {
      showAlert('No Selection', 'Please select at least one filter group', 'warning');
      return;
    }

    try {
      setSplitting(true);
      // Use existing split_frame_set with Frames selection type
      await api.invoke('split_frame_set', {
        sourceSetId: parseInt(id),
        selection: { type: 'frames', ids: selectedFrameIds },
        newName: splitName.trim(),
      });

      setShowSplitDialog(false);
      setSplitName('');
      setSelectedFrameIds([]);

      // Reload to show updated data
      await loadData();

      // Success - silent update (no alert)
    } catch (err) {
      showAlert('Split Failed', 'Failed to split frame set: ' + String(err), 'error');
    } finally {
      setSplitting(false);
    }
  };

  const handleDeleteClick = () => {
    setShowDeleteConfirm(true);
  };

  const confirmDelete = async () => {
    setShowDeleteConfirm(false);

    if (!id) return;

    try {
      await api.invoke('delete_frames_set', { framesSetId: parseInt(id) });
      navigate('/objects');
    } catch (err) {
      showAlert('Delete Failed', 'Failed to delete: ' + String(err), 'error');
    }
  };

  const publishAsProject = async () => {
    if (!id) return;
    try {
      // Core mints the deep link (percent-encoded, Url-built) and records an
      // intent so the next poll auto-links this set to the project the portal
      // creates from it (spec §8).
      const { url } = await api.invoke<PortalNewProjectLink>('create_collab_link_intent', {
        framesSetId: parseInt(id),
      });
      const safe = safeExternalUrl(url);
      if (!safe) {
        console.error('[projects] refused non-http(s) intent url:', url);
        notify({
          title: 'Could not open the portal',
          detail: 'The configured hub address is not a valid web address.',
          kind: 'project',
          tone: 'warning',
        });
        return;
      }
      await openUrl(safe);
    } catch (err) {
      console.error('[projects] publish-as-project failed:', err);
      notify({
        title: 'Could not start project creation',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
      });
    }
  };

  if (loading) {
    return (
      <div className="p-6">
        <div className="text-center py-12 text-content-muted">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-accent mx-auto"></div>
          <p className="mt-4">Loading frame set details...</p>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-6">
        <div className="mb-4">
          <button
            onClick={() => navigate('/objects')}
            className="flex items-center gap-2 px-4 py-2 bg-surface-hover hover:bg-surface-hover rounded-lg transition"
          >
            <ArrowLeft size={18} />
            Back to Objects
          </button>
        </div>
        <div className="bg-error-muted border border-error/50 rounded-lg p-6">
          <div className="flex items-start gap-3">
            <AlertCircle size={20} className="text-error flex-shrink-0 mt-0.5" />
            <div className="flex-1">
              <h3 className="text-error font-semibold mb-2">Error Loading Frame Set</h3>
              <p className="text-error/80 text-sm">{error}</p>
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="p-6">
        <div className="mb-4">
          <button
            onClick={() => navigate('/objects')}
            className="flex items-center gap-2 px-4 py-2 bg-surface-hover hover:bg-surface-hover rounded-lg transition"
          >
            <ArrowLeft size={18} />
            Back to Objects
          </button>
        </div>
        <div className="bg-surface-elevated rounded-lg p-6 text-center text-content-muted">
          No data available
        </div>
      </div>
    );
  }

  return (
    <div className="p-4 pt-3 h-full flex flex-col">
      {/* Frame Set Header */}
      <div className="bg-surface-elevated rounded-lg p-3 mb-2 border border-border flex-shrink-0">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <button
              onClick={() => navigate('/objects')}
              className="flex items-center text-content-muted hover:text-content transition pr-3 mr-1 border-r border-border"
            >
              <ArrowLeft size={18} />
            </button>
            <div className="flex items-center gap-2 flex-wrap">
              <h1 className="text-xl font-bold">{detail.frames_set?.name || 'Untitled'}</h1>
              {detail.frames_set?.archived_at && (
                <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-warning/20 text-warning text-xs font-medium">
                  <ArchiveIcon size={12} />
                  Archived
                </span>
              )}
            </div>
            {detail.frames_set?.objctra && detail.frames_set?.objctdec && (
              <div className="flex items-center gap-2 text-content-muted">
                <MapPin size={16} />
                <span className="font-mono text-sm">
                  {detail.frames_set.objctra} / {detail.frames_set.objctdec}
                </span>
              </div>
            )}
            {detail.frames_set?.avg_rotation != null && (
              <div className="flex items-center gap-2 text-content-muted">
                <RotateCw size={16} />
                <span className="font-mono text-sm">
                  {detail.frames_set.min_rotation != null && detail.frames_set.max_rotation != null &&
                   Math.abs(detail.frames_set.max_rotation - detail.frames_set.min_rotation) >= 1
                    ? `${detail.frames_set.min_rotation.toFixed(1)}° – ${detail.frames_set.max_rotation.toFixed(1)}°`
                    : `${detail.frames_set.avg_rotation.toFixed(1)}°`
                  }
                </span>
              </div>
            )}
          </div>
          <div className="flex items-center gap-3">
            {/* Single contextual action button: depends on the frame set state.
                stage / wip (is_archived=0) → Find new images
                in archive, not yet zipped → Move and ZIP
                already zipped (archived_at set) → Unarchive */}
            {detail.frames_set?.archived_at ? (
              <>
                <button
                  type="button"
                  onClick={async () => {
                    try {
                      const items = await listArchivedFrameSets();
                      const match = items.find(it => it.frames_set_id === detail.frames_set?.id);
                      if (match) {
                        setRestoreItem(match);
                      } else {
                        alert('Could not find this frame set in the archive list. It may have been deleted.');
                      }
                    } catch (e) {
                      alert(`Failed to load archive details: ${e}`);
                    }
                  }}
                  title="Unarchive this frame set: extract files from the zip and bring it back to the active view"
                  className="flex items-center gap-2 rounded-lg border border-accent/40 bg-accent/10 text-accent px-3 py-1.5 text-sm hover:bg-accent/20"
                >
                  <Upload size={14} />
                  Unarchive
                </button>
                {isTauri && (
                  <button
                    type="button"
                    onClick={async () => {
                      const opId = detail.frames_set?.archive_operation_id;
                      if (!opId) {
                        alert('No archive operation linked to this frame set.');
                        return;
                      }
                      try {
                        const zips = await listArchiveZips(opId);
                        const target = zips.find(z => z.exists) ?? zips[0];
                        if (!target) {
                          alert('No zip files recorded for this archive operation.');
                          return;
                        }
                        await revealItemInDir(target.path);
                      } catch (e) {
                        alert(`Failed to open file manager: ${e}`);
                      }
                    }}
                    title="Reveal the archive zip(s) in the system file manager"
                    className="flex items-center justify-center rounded-lg border border-border bg-surface-hover p-1.5 text-content-muted hover:text-content hover:brightness-110"
                  >
                    <FolderOpen size={14} />
                  </button>
                )}
              </>
            ) : detail.frames_set?.is_archived ? (
              <button
                type="button"
                onClick={handleArchiveClick}
                disabled={archiving}
                title="Move this frame set's files into a zip archive"
                className="flex items-center gap-2 rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <ArchiveIcon size={14} />
                {archiving ? 'Archiving…' : 'Move and ZIP'}
              </button>
            ) : (
              <>
                <button
                  type="button"
                  onClick={handleFindNewClick}
                  disabled={
                    findNewBusy || !detail.frames_set?.objctra || !detail.frames_set?.objctdec
                  }
                  title={
                    !detail.frames_set?.objctra || !detail.frames_set?.objctdec
                      ? 'No coordinates — nothing to match against'
                      : 'Find new images for this object'
                  }
                  className="flex items-center gap-2 rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <Search size={14} />
                  {findNewBusy ? 'Merging…' : 'Find new images'}
                </button>
                <button
                  type="button"
                  onClick={handleMoveToArchive}
                  disabled={movingToArchive}
                  title="Move this frame set to the Archive tab. You can then zip it from there."
                  className="flex items-center gap-2 rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <ArchiveIcon size={14} />
                  {movingToArchive ? 'Moving…' : 'Move to Archive'}
                </button>
              </>
            )}
            <button
              onClick={() => void publishAsProject()}
              title="Publish this frame set as a collaboration project on the portal"
              className="inline-flex items-center gap-2 rounded-lg border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover"
            >
              <Users size={14} />
              Publish as project
            </button>
            <div className="flex items-center gap-1.5 text-sm text-content-muted">
              <span><span className="font-medium text-content">{calibrationHierarchy?.total_frames ?? '-'}</span> frames</span>
              <span>·</span>
              <span><span className="font-medium text-success">{calibrationHierarchy?.calibrated_frames ?? '-'}</span> calibrated</span>
              <span>·</span>
              <span><span className="font-medium text-warning">{calibrationHierarchy?.uncalibrated_frames ?? '-'}</span> uncalibrated</span>
              <span>·</span>
              <span><span className="font-medium text-accent">{calibrationHierarchy?.date_groups.length ?? '-'}</span> sessions</span>
              <span>·</span>
              <span className="font-medium text-content">{formatExposureTime(detail.frames_set?.total_exp_time)}</span>
            </div>
          </div>
        </div>
      </div>

      {/* Tab Bar */}
      <div className="flex items-center gap-1 border-b border-border mb-3 flex-shrink-0">
        {([
          { key: 'analysis' as FrameSetTab, label: 'Lights Analysis & Stats', icon: BarChart3 },
          { key: 'calibration' as FrameSetTab, label: 'Calibration Coverage', icon: Crosshair },
          { key: 'registration' as FrameSetTab, label: 'Registration', icon: AlignHorizontalJustifyCenter },
          { key: 'export' as FrameSetTab, label: 'Export', icon: Layers },
          { key: 'history' as FrameSetTab, label: 'History', icon: History },
        ]).map(({ key, label, icon: Icon }) => {
          // Registration is disabled in production (under development) and, in
          // dev, additionally gated until the lights are analyzed and a
          // reference is chosen.
          const isUnderDev = key === 'registration' && !REGISTRATION_ENABLED;
          const isGated =
            key === 'registration' && (isUnderDev || !registrationTabReady);
          const gateTooltip =
            key === 'registration'
              ? isUnderDev
                ? 'Registration is under development — available in a future release.'
                : !registrationTabReady
                  ? analysisData.size === 0
                    ? 'Analyze the lights first, then choose a reference frame in the Analysis tab'
                    : 'Choose a reference frame in the Analysis tab to enable stacking preparation'
                  : undefined
              : undefined;
          return (
            <button
              key={key}
              onClick={() => { if (!isGated) setActiveTab(key); }}
              disabled={isGated}
              title={gateTooltip}
              className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
                isGated
                  ? 'border-transparent text-content-muted opacity-40 cursor-not-allowed'
                  : activeTab === key
                    ? 'border-accent text-accent'
                    : 'border-transparent text-content-muted hover:text-content hover:border-border'
              }`}
            >
              <Icon size={16} />
              {label}
            </button>
          );
        })}
      </div>

      {/* Main Content */}
      <div className="flex-1 min-h-0">
        {loadingCalibration ? (
          <div className="text-center py-12">
            <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-accent mx-auto mb-4"></div>
            <p className="text-content-muted">Loading calibration data...</p>
          </div>
        ) : calibrationHierarchy ? (
          activeTab === 'history' ? (
            <FrameSetHistoryTab key={historyRefreshKey} frameSetId={parseInt(id!)} />
          ) : activeTab === 'registration' ? (
            <StackingPrepTab
              framesSetId={parseInt(id!)}
              frameSetName={detail?.frames_set?.name ?? undefined}
              referenceFrameId={referenceFrameId ?? null}
            />
          ) : activeTab === 'export' ? (
            <ExportTab
              frameSetId={parseInt(id!)}
              frameSetName={detail?.frames_set?.name ?? undefined}
            />
          ) : activeTab === 'calibration' ? (
            <CalibrationHierarchyViewComponent
              data={calibrationHierarchy}
              blackholedFileIds={blackholedFileIds}
              filterSnrMap={calibrationFilterSnrMap}
              analysisData={analysisData}
              frameSetId={parseInt(id!)}
              frameSetName={detail.frames_set?.name || 'Untitled'}
              onCalibrationComplete={loadData}
              onRefresh={refreshCalibrationHierarchy}
              onBlink={handleBlink}
              onSplit={handleOpenSplitDialog}
              onCreateCustomSet={handleOpenCreateDialog}
              highlightCalSet={pendingHighlightCalSet}
              onHighlightConsumed={() => setPendingHighlightCalSet(null)}
              onCalibrateLights={
                !detail.frames_set?.archived_at && !detail.frames_set?.is_archived
                  ? () => setShowCalibrateDialog(true)
                  : undefined
              }
              readinessByFrameId={lightCalReadiness}
              detailsByFrameId={lightCalDetails}
            />
          ) : (
            <LightsAnalysisView
              hierarchy={calibrationHierarchy}
              frameSetId={parseInt(id!)}
              frameSetName={detail?.frames_set?.name ?? undefined}
              blackholedFileIds={blackholedFileIds}
              onRefresh={refreshCalibrationHierarchy}
              onBlink={handleBlink}
              onSplit={handleOpenSplitDialog}
              onCreateCustomSet={handleOpenCreateDialog}
              hideLocateColumn={!!detail?.frames_set?.archived_at}
              referenceFrameId={referenceFrameId ?? null}
              onReferenceChanged={(ref) => setReferenceFrameId(ref?.referenceFrameId ?? null)}
              readinessByFrameId={lightCalReadiness}
              detailsByFrameId={lightCalDetails}
            />
          )
        ) : (
          <div className="text-center py-12 text-content-muted">
            <p>Failed to load calibration data.</p>
            <button
              onClick={handleDeleteClick}
              className="mt-4 px-4 py-2 bg-error hover:brightness-90 text-white rounded-lg transition"
            >
              Delete Frame Set
            </button>
          </div>
        )}
      </div>

      {/* Create Custom Set Dialog */}
      {showCreateDialog && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
          <div className="bg-surface-elevated rounded-lg max-w-md w-full p-6 border border-border">
            <h3 className="text-xl font-bold mb-4">Create Custom Set</h3>

            <div className="mb-4">
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Set Name
              </label>
              <input
                type="text"
                value={customSetName}
                onChange={(e) => setCustomSetName(e.target.value)}
                placeholder="Enter custom set name"
                className="w-full px-3 py-2 bg-surface-hover text-content rounded-lg border border-border focus:outline-none focus:border-accent"
                autoFocus
              />
            </div>

            <div className="mb-6 text-sm text-content-muted">
              {selectedFrameIds.length} frame{selectedFrameIds.length !== 1 ? 's' : ''} will be included in the new set
            </div>

            <div className="flex gap-3 justify-end">
              <button
                onClick={() => {
                  setShowCreateDialog(false);
                  setCustomSetName('');
                }}
                className="px-4 py-2 bg-surface-hover hover:bg-surface-hover rounded-lg transition"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateCustomSet}
                disabled={creating || !customSetName.trim()}
                className="px-4 py-2 bg-success hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition"
              >
                {creating ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Split Frame Set Dialog */}
      {showSplitDialog && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
          <div className="bg-surface-elevated rounded-lg max-w-md w-full p-6 border border-border">
            <h3 className="text-xl font-bold mb-4 flex items-center gap-2">
              <Scissors size={20} className="text-accent" />
              Split Frame Set
            </h3>

            <div className="mb-4">
              <label className="block text-sm font-medium text-content-secondary mb-2">
                New Set Name
              </label>
              <input
                type="text"
                value={splitName}
                onChange={(e) => setSplitName(e.target.value)}
                placeholder="Enter name for split set"
                className="w-full px-3 py-2 bg-surface-hover text-content rounded-lg border border-border focus:outline-none focus:border-accent"
                autoFocus
              />
            </div>

            <div className="mb-6 text-sm text-content-muted space-y-2">
              <p>{selectedFrameIds.length} frame{selectedFrameIds.length !== 1 ? 's' : ''} will be split into the new set</p>
              <p className="text-warning">
                The selected frames will be removed from "{detail?.frames_set?.name || 'this set'}" and moved to the new set.
              </p>
            </div>

            <div className="flex gap-3 justify-end">
              <button
                onClick={() => {
                  setShowSplitDialog(false);
                  setSplitName('');
                }}
                className="px-4 py-2 bg-surface-hover hover:bg-surface-hover rounded-lg transition"
              >
                Cancel
              </button>
              <button
                onClick={handleSplit}
                disabled={splitting || !splitName.trim()}
                className="px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition"
              >
                {splitting ? 'Splitting...' : 'Split'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Blink Viewer Modal */}
      {blinkFrames && (
        <BlinkViewer
          frames={blinkFrames}
          initialIndex={0}
          onClose={() => setBlinkFrames(null)}
          sourceType="light"
          frameSetId={id ? parseInt(id) : undefined}
          onFramesRemoved={() => {
            // Refresh calibration hierarchy when frames are blackholed
            refreshCalibrationHierarchy();
          }}
        />
      )}

      {/* Delete Confirmation Dialog */}
      <ConfirmDialog
        isOpen={showDeleteConfirm}
        title="Delete Frame Set"
        message="Delete this frame set? You can recreate it using 'Auto-Generate Sets'."
        onConfirm={confirmDelete}
        onCancel={() => setShowDeleteConfirm(false)}
        confirmText="Delete"
        confirmDanger={true}
      />

      {/* Alert Dialog */}
      <AlertDialog
        isOpen={alertDialog.isOpen}
        title={alertDialog.title}
        message={alertDialog.message}
        variant={alertDialog.variant}
        onClose={() => setAlertDialog({ ...alertDialog, isOpen: false })}
      />

      {/* Find New Images Dialog */}
      {showFindNewDialog && (
        <FindNewImagesDialog
          frameSetId={parseInt(id!)}
          frameSetName={detail.frames_set?.name || 'Untitled'}
          onClose={() => setShowFindNewDialog(false)}
          onMerged={(report) => {
            setShowFindNewDialog(false);
            // Refresh frame set detail + history list
            loadData();
            setHistoryRefreshKey((k) => k + 1);
            setAlertDialog({
              isOpen: true,
              title: 'Merge complete',
              message: `Added ${report.added_count} frame${report.added_count === 1 ? '' : 's'}${report.skipped_count ? `, skipped ${report.skipped_count}` : ''}. See the History tab for details.`,
              variant: 'info',
            });
          }}
        />
      )}

      {/* Calibrate Lights Dialog */}
      {showCalibrateDialog && (
        <CalibrateLightsDialog
          setId={parseInt(id!)}
          setName={detail.frames_set?.name || 'Untitled'}
          onClose={() => {
            setShowCalibrateDialog(false);
            // The flat-norm toggle or Advanced params inside the dialog may have
            // changed; re-fetch readiness + recipe so the badges/tooltips reflect
            // the current staleness view.
            loadLightCalReadiness();
            loadLightCalDetails();
          }}
        />
      )}

      {/* Archive Disposition Dialog */}
      {showArchiveDialog && detail.frames_set?.id && (
        <ArchiveDispositionDialog
          framesSetId={detail.frames_set.id}
          defaultCompression={archiveCompression}
          onCancel={() => setShowArchiveDialog(false)}
          onStart={handleStartArchive}
        />
      )}

      {/* Archive Progress */}
      {activeArchiveOpId !== null && (
        <div className="fixed bottom-4 right-4 z-40 w-80">
          <ArchiveProgress
            operationId={activeArchiveOpId}
            onClose={() => {
              setActiveArchiveOpId(null);
              // Reload detail so the archived state (badge / Restore button) appears.
              loadData();
            }}
          />
        </div>
      )}

      {/* Restore Dialog (when an archived frame set is opened) */}
      {restoreItem && (
        <RestoreDialog
          item={restoreItem}
          onCancel={() => setRestoreItem(null)}
          onStarted={(opId) => {
            setRestoreItem(null);
            // Mount the progress widget; the existing ArchiveProgress component
            // listens to the same `archive-progress` + `archive-finished` events
            // that restore now emits.
            setActiveArchiveOpId(opId);
          }}
        />
      )}
    </div>
  );
}
