import { useState, useEffect, useCallback } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import { ArrowLeft, Clock, MapPin, AlertCircle, Scissors } from 'lucide-react';
import type { FrameSetDetail, FileWithFrame, CalibrationHierarchyView } from '../types/models';
import BlinkViewer from '../components/BlinkViewer';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';
import { CalibrationFinderButton } from '../components/CalibrationFinderButton';
import { CalibrationHierarchyView as CalibrationHierarchyViewComponent } from '../components/CalibrationHierarchyView';

export default function FrameSetDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
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

  // Selected filter keys from CalibrationHierarchyView (format: "dateKey:cameraKey:filterKey")
  const [selectedFilterKeys, setSelectedFilterKeys] = useState<Set<string>>(new Set());

  // Load both detail and calibration data on mount
  useEffect(() => {
    loadData();
  }, [id]);

  const loadData = async () => {
    if (!id) return;

    try {
      setLoading(true);
      setLoadingCalibration(true);
      setError(null);

      // Load both in parallel
      const [detailResult, hierarchyResult] = await Promise.all([
        invoke<FrameSetDetail>('get_frame_set_detail', {
          framesSetId: parseInt(id),
        }),
        invoke<CalibrationHierarchyView>('get_calibration_hierarchy_for_frame_set', {
          frameSetId: parseInt(id),
        }),
      ]);

      setDetail(detailResult);
      setCalibrationHierarchy(hierarchyResult);
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
      const result = await invoke<CalibrationHierarchyView>('get_calibration_hierarchy_for_frame_set', {
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

  // Build a unique filter key that includes exptime to differentiate same filter with different exposures
  const buildFilterKey = useCallback((filterGroup: { filter: string | null; exptime: number | null }): string => {
    const baseFilter = filterGroup.filter ?? '__no_filter__';
    return filterGroup.exptime !== null
      ? `${baseFilter}:${filterGroup.exptime}`
      : baseFilter;
  }, []);

  // Get frame IDs from selected filter keys
  const getFrameIdsFromFilterKeys = useCallback((filterKeys: Set<string>): number[] => {
    if (!calibrationHierarchy) return [];

    const frameIds: number[] = [];
    for (const dateGroup of calibrationHierarchy.date_groups) {
      for (const cameraGroup of dateGroup.camera_groups) {
        for (const filterGroup of cameraGroup.filter_groups) {
          const filterKey = buildFilterKey(filterGroup);
          const fullKey = `${dateGroup.date}:${cameraGroup.instrume}:${filterKey}`;
          if (filterKeys.has(fullKey)) {
            frameIds.push(...filterGroup.light_frames.map(f => f.frame_id));
          }
        }
      }
    }
    return frameIds;
  }, [calibrationHierarchy, buildFilterKey]);

  // Handle blink from CalibrationHierarchyView - load full frame data
  const handleBlink = useCallback(async (frameIds: number[]) => {
    if (frameIds.length === 0) {
      showAlert('No Frames', 'No frames selected for blink', 'warning');
      return;
    }

    try {
      // Load full frame data for the given frame IDs
      const frames = await invoke<FileWithFrame[]>('get_files_with_frames_by_ids', {
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
      await invoke('create_frame_set_from_selection', {
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
      await invoke('split_frame_set', {
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
      await invoke('delete_frames_set', { framesSetId: parseInt(id) });
      navigate('/objects');
    } catch (err) {
      showAlert('Delete Failed', 'Failed to delete: ' + String(err), 'error');
    }
  };

  if (loading) {
    return (
      <div className="p-6">
        <div className="text-center py-12 text-gray-400">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-500 mx-auto"></div>
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
            className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
          >
            <ArrowLeft size={18} />
            Back to Objects
          </button>
        </div>
        <div className="bg-red-900/20 border border-red-800 rounded-lg p-6">
          <div className="flex items-start gap-3">
            <AlertCircle size={20} className="text-red-400 flex-shrink-0 mt-0.5" />
            <div className="flex-1">
              <h3 className="text-red-400 font-semibold mb-2">Error Loading Frame Set</h3>
              <p className="text-red-300 text-sm">{error}</p>
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
            className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
          >
            <ArrowLeft size={18} />
            Back to Objects
          </button>
        </div>
        <div className="bg-gray-800 rounded-lg p-6 text-center text-gray-400">
          No data available
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 h-full flex flex-col">
      {/* Back Button */}
      <div className="mb-6 flex items-center justify-between flex-shrink-0">
        <button
          onClick={() => navigate('/objects')}
          className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
        >
          <ArrowLeft size={18} />
          Back to Objects
        </button>
      </div>

      {/* Frame Set Header - Combined with Stats */}
      <div className="bg-gray-800 rounded-lg p-5 mb-4 border border-gray-700 flex-shrink-0">
        <div className="flex items-start justify-between mb-4">
          <div className="flex items-center gap-4">
            <h1 className="text-2xl font-bold">{detail.frames_set?.name || 'Untitled'}</h1>
            {detail.frames_set?.objctra && detail.frames_set?.objctdec && (
              <div className="flex items-center gap-2 text-gray-400">
                <MapPin size={16} />
                <span className="font-mono text-sm">
                  {detail.frames_set.objctra} / {detail.frames_set.objctdec}
                </span>
              </div>
            )}
          </div>
          <CalibrationFinderButton
            frameSetId={parseInt(id!)}
            frameSetName={detail.frames_set?.name || 'Untitled'}
            onComplete={loadData}
          />
        </div>

        {/* Stats Row */}
        <div className="grid grid-cols-5 gap-4 text-center">
          <div className="bg-gray-900/50 rounded p-3">
            <div className="text-2xl font-bold text-gray-100">
              {calibrationHierarchy?.total_frames ?? '-'}
            </div>
            <div className="text-xs text-gray-400 mt-1">Total Frames</div>
          </div>
          <div className="bg-gray-900/50 rounded p-3">
            <div className="text-2xl font-bold text-emerald-400">
              {calibrationHierarchy?.calibrated_frames ?? '-'}
            </div>
            <div className="text-xs text-gray-400 mt-1">Calibrated</div>
          </div>
          <div className="bg-gray-900/50 rounded p-3">
            <div className="text-2xl font-bold text-amber-400">
              {calibrationHierarchy?.uncalibrated_frames ?? '-'}
            </div>
            <div className="text-xs text-gray-400 mt-1">Uncalibrated</div>
          </div>
          <div className="bg-gray-900/50 rounded p-3">
            <div className="text-2xl font-bold text-blue-400">
              {calibrationHierarchy?.date_groups.length ?? '-'}
            </div>
            <div className="text-xs text-gray-400 mt-1">Sessions</div>
          </div>
          <div className="bg-gray-900/50 rounded p-3">
            <div className="text-2xl font-bold text-gray-200">
              {formatExposureTime(detail.frames_set?.total_exp_time)}
            </div>
            <div className="text-xs text-gray-400 mt-1 flex items-center justify-center gap-1">
              <Clock size={12} />
              Exposure
            </div>
          </div>
        </div>
      </div>

      {/* Main Content - CalibrationHierarchyView */}
      <div className="flex-1 min-h-0">
        {loadingCalibration ? (
          <div className="text-center py-12">
            <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-500 mx-auto mb-4"></div>
            <p className="text-gray-400">Loading calibration data...</p>
          </div>
        ) : calibrationHierarchy ? (
          <CalibrationHierarchyViewComponent
            data={calibrationHierarchy}
            onRefresh={refreshCalibrationHierarchy}
            onBlink={handleBlink}
            onSplit={handleOpenSplitDialog}
            onCreateCustomSet={handleOpenCreateDialog}
          />
        ) : (
          <div className="text-center py-12 text-gray-400">
            <p>Failed to load calibration data.</p>
            <button
              onClick={handleDeleteClick}
              className="mt-4 px-4 py-2 bg-red-600 hover:bg-red-700 text-white rounded-lg transition"
            >
              Delete Frame Set
            </button>
          </div>
        )}
      </div>

      {/* Create Custom Set Dialog */}
      {showCreateDialog && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
          <div className="bg-gray-800 rounded-lg max-w-md w-full p-6 border border-gray-700">
            <h3 className="text-xl font-bold mb-4">Create Custom Set</h3>

            <div className="mb-4">
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Set Name
              </label>
              <input
                type="text"
                value={customSetName}
                onChange={(e) => setCustomSetName(e.target.value)}
                placeholder="Enter custom set name"
                className="w-full px-3 py-2 bg-gray-700 text-gray-100 rounded-lg border border-gray-600 focus:outline-none focus:border-blue-500"
                autoFocus
              />
            </div>

            <div className="mb-6 text-sm text-gray-400">
              {selectedFilterKeys.size} filter group{selectedFilterKeys.size !== 1 ? 's' : ''} ({getFrameIdsFromFilterKeys(selectedFilterKeys).length} frames) will be included in the new set
            </div>

            <div className="flex gap-3 justify-end">
              <button
                onClick={() => {
                  setShowCreateDialog(false);
                  setCustomSetName('');
                }}
                className="px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateCustomSet}
                disabled={creating || !customSetName.trim()}
                className="px-4 py-2 bg-green-600 hover:bg-green-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition"
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
          <div className="bg-gray-800 rounded-lg max-w-md w-full p-6 border border-gray-700">
            <h3 className="text-xl font-bold mb-4 flex items-center gap-2">
              <Scissors size={20} className="text-blue-400" />
              Split Frame Set
            </h3>

            <div className="mb-4">
              <label className="block text-sm font-medium text-gray-300 mb-2">
                New Set Name
              </label>
              <input
                type="text"
                value={splitName}
                onChange={(e) => setSplitName(e.target.value)}
                placeholder="Enter name for split set"
                className="w-full px-3 py-2 bg-gray-700 text-gray-100 rounded-lg border border-gray-600 focus:outline-none focus:border-blue-500"
                autoFocus
              />
            </div>

            <div className="mb-6 text-sm text-gray-400 space-y-2">
              <p>{selectedFilterKeys.size} filter group{selectedFilterKeys.size !== 1 ? 's' : ''} ({getFrameIdsFromFilterKeys(selectedFilterKeys).length} frames) will be split into the new set</p>
              <p className="text-yellow-400">
                The selected frames will be removed from "{detail?.frames_set?.name || 'this set'}" and moved to the new set.
              </p>
            </div>

            <div className="flex gap-3 justify-end">
              <button
                onClick={() => {
                  setShowSplitDialog(false);
                  setSplitName('');
                }}
                className="px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
              >
                Cancel
              </button>
              <button
                onClick={handleSplit}
                disabled={splitting || !splitName.trim()}
                className="px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition"
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
