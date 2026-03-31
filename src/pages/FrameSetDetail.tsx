import { useState, useEffect, useCallback, useMemo } from 'react';
import { useParams, useNavigate, useLocation } from 'react-router-dom';
import { api } from '../api';
import { ArrowLeft, MapPin, RotateCw, AlertCircle, Scissors, BarChart3, Crosshair } from 'lucide-react';
import type { FrameSetDetail, FileWithFrame, CalibrationHierarchyView, FrameAnalysis } from '../types/models';
import BlinkViewer from '../components/BlinkViewer';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';
import { CalibrationFinderButton } from '../components/CalibrationFinderButton';
import { CalibrationHierarchyView as CalibrationHierarchyViewComponent } from '../components/CalibrationHierarchyView';
import { LightsAnalysisView } from '../components/LightsAnalysisView';
import { useBlackholeEvents } from '../hooks/useBlackholeEvents';
import { buildCameraFilterTree, buildMergedCameraFilterTree } from '../components/calibration/utils';

type FrameSetTab = 'calibration' | 'analysis';

export default function FrameSetDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const location = useLocation();
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

  // Tab state
  const [activeTab, setActiveTab] = useState<FrameSetTab>('analysis');

  // Selected filter keys from CalibrationHierarchyView (format: "dateKey:cameraKey:filterKey")
  const [selectedFilterKeys, setSelectedFilterKeys] = useState<Set<string>>(new Set());

  // Analysis data for SNR display in tree
  const [analysisData, setAnalysisData] = useState<Map<number, FrameAnalysis>>(new Map());

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
      const [detailResult, hierarchyResult, analysisResult] = await Promise.all([
        api.invoke<FrameSetDetail>('get_frame_set_detail', {
          framesSetId: parseInt(id),
        }),
        api.invoke<CalibrationHierarchyView>('get_calibration_hierarchy_for_frame_set', {
          frameSetId: parseInt(id),
        }),
        api.invoke<FrameAnalysis[]>('get_analysis_for_frame_set', {
          frameSetId: parseInt(id),
        }).catch(() => [] as FrameAnalysis[]),
      ]);

      setDetail(detailResult);
      setCalibrationHierarchy(hierarchyResult);
      const aMap = new Map<number, FrameAnalysis>();
      for (const a of analysisResult) aMap.set(a.frame_id, a);
      setAnalysisData(aMap);
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

  const showAlert = (title: string, message: string, variant: 'error' | 'warning' | 'info' = 'info') => {
    setAlertDialog({ isOpen: true, title, message, variant });
  };

  const formatExposureTime = (seconds: number | null | undefined) => {
    if (!seconds) return 'N/A';
    const hours = (seconds / 3600).toFixed(1);
    const minutes = Math.round((seconds % 3600) / 60);
    return parseFloat(hours) >= 1 ? `${hours}h` : `${minutes}m`;
  };

  // Get frame IDs from selected filter keys (must match key format in buildCameraFilterTree)
  const getFrameIdsFromFilterKeys = useCallback((filterKeys: Set<string>): number[] => {
    if (!calibrationHierarchy) return [];

    const frameIds: number[] = [];
    for (const dateGroup of calibrationHierarchy.date_groups) {
      for (const cameraGroup of dateGroup.camera_groups) {
        for (const filterGroup of cameraGroup.filter_groups) {
          const filterName = filterGroup.filter ?? 'No Filter';
          const exptime = filterGroup.exptime;
          const fullKey = `${dateGroup.date}::${cameraGroup.instrume}::${filterName}::${exptime ?? 'any'}`;
          if (filterKeys.has(fullKey)) {
            frameIds.push(...filterGroup.light_frames.map(f => f.frame_id));
          }
        }
      }
    }
    return frameIds;
  }, [calibrationHierarchy]);

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
  const handleOpenSplitDialog = useCallback((filterKeys: Set<string>) => {
    if (!id || filterKeys.size === 0) return;

    setSelectedFilterKeys(filterKeys);

    // Pre-fill split name
    const originalName = detail?.frames_set?.name || 'Untitled';
    setSplitName(`${originalName} - Split 1`);
    setShowSplitDialog(true);
  }, [id, detail]);

  // Handle create custom set from CalibrationHierarchyView
  const handleOpenCreateDialog = useCallback((filterKeys: Set<string>) => {
    if (filterKeys.size === 0) return;

    setSelectedFilterKeys(filterKeys);
    setShowCreateDialog(true);
  }, []);

  const handleCreateCustomSet = async () => {
    if (!customSetName.trim()) {
      showAlert('Name Required', 'Please enter a name for the custom set', 'warning');
      return;
    }

    if (selectedFilterKeys.size === 0) {
      showAlert('No Selection', 'Please select at least one filter group', 'warning');
      return;
    }

    const frameIds = getFrameIdsFromFilterKeys(selectedFilterKeys);
    if (frameIds.length === 0) {
      showAlert('No Frames', 'No frames found in selected filter groups', 'warning');
      return;
    }

    try {
      setCreating(true);
      // Use existing command that creates frame set from frame IDs
      await api.invoke('create_frame_set_from_selection', {
        name: customSetName.trim(),
        frame_ids: frameIds,
        description: null,
      });

      // Success - silent update
      setShowCreateDialog(false);
      setCustomSetName('');
      setSelectedFilterKeys(new Set());
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

    if (selectedFilterKeys.size === 0) {
      showAlert('No Selection', 'Please select at least one filter group', 'warning');
      return;
    }

    const frameIds = getFrameIdsFromFilterKeys(selectedFilterKeys);
    if (frameIds.length === 0) {
      showAlert('No Frames', 'No frames found in selected filter groups', 'warning');
      return;
    }

    try {
      setSplitting(true);
      // Use existing split_frame_set with Frames selection type
      await api.invoke('split_frame_set', {
        sourceSetId: parseInt(id),
        selection: { type: 'frames', ids: frameIds },
        newName: splitName.trim(),
      });

      setShowSplitDialog(false);
      setSplitName('');
      setSelectedFilterKeys(new Set());

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
            <h1 className="text-xl font-bold">{detail.frames_set?.name || 'Untitled'}</h1>
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

      {/* Tab Bar */}
      <div className="flex items-center gap-1 border-b border-border mb-3 flex-shrink-0">
        {([
          { key: 'analysis' as FrameSetTab, label: 'Lights Analysis & Stats', icon: BarChart3 },
          { key: 'calibration' as FrameSetTab, label: 'Calibration Coverage', icon: Crosshair },
        ]).map(({ key, label, icon: Icon }) => (
          <button
            key={key}
            onClick={() => setActiveTab(key)}
            className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
              activeTab === key
                ? 'border-accent text-accent'
                : 'border-transparent text-content-muted hover:text-content hover:border-border'
            }`}
          >
            <Icon size={16} />
            {label}
          </button>
        ))}
      </div>

      {/* Main Content */}
      <div className="flex-1 min-h-0">
        {loadingCalibration ? (
          <div className="text-center py-12">
            <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-accent mx-auto mb-4"></div>
            <p className="text-content-muted">Loading calibration data...</p>
          </div>
        ) : calibrationHierarchy ? (
          activeTab === 'calibration' ? (
            <div className="flex flex-col h-full">
              <div className="flex items-center gap-3 mb-3 flex-shrink-0">
                <CalibrationFinderButton
                  frameSetId={parseInt(id!)}
                  frameSetName={detail.frames_set?.name || 'Untitled'}
                  onComplete={loadData}
                />
              </div>
              <div className="flex-1 min-h-0">
                <CalibrationHierarchyViewComponent
                  data={calibrationHierarchy}
                  blackholedFileIds={blackholedFileIds}
                  filterSnrMap={calibrationFilterSnrMap}
                  onRefresh={refreshCalibrationHierarchy}
                  onSplit={handleOpenSplitDialog}
                  onCreateCustomSet={handleOpenCreateDialog}
                />
              </div>
            </div>
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
              {selectedFilterKeys.size} filter group{selectedFilterKeys.size !== 1 ? 's' : ''} ({getFrameIdsFromFilterKeys(selectedFilterKeys).length} frames) will be included in the new set
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
              <p>{selectedFilterKeys.size} filter group{selectedFilterKeys.size !== 1 ? 's' : ''} ({getFrameIdsFromFilterKeys(selectedFilterKeys).length} frames) will be split into the new set</p>
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
    </div>
  );
}
