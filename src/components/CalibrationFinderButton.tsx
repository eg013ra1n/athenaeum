import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Zap } from 'lucide-react';
import type { ProcessingStats, FramesSet, FlatGroup } from '../types/models';
import { FlatPattern } from '../types/models';
import { CalibrationProcessModal } from './CalibrationProcessModal';
import { FlatPatternModal } from './FlatPatternModal';
import { ManualFlatSelectionModal } from './ManualFlatSelectionModal';

interface CalibrationFinderButtonProps {
  frameSetId: number;
  frameSetName: string | null;
  onComplete?: (stats: ProcessingStats) => void;
}

export function CalibrationFinderButton({ frameSetId, frameSetName, onComplete }: CalibrationFinderButtonProps) {
  const [isProcessing, setIsProcessing] = useState(false);
  const [showProcessModal, setShowProcessModal] = useState(false);
  const [showPatternModal, setShowPatternModal] = useState(false);
  const [showManualSelectionModal, setShowManualSelectionModal] = useState(false);
  const [stats, setStats] = useState<ProcessingStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [frameSet, setFrameSet] = useState<FramesSet | null>(null);
  const [flatGroupOptions, setFlatGroupOptions] = useState<Record<string, FlatGroup[]>>({});
  const [selectedPattern, setSelectedPattern] = useState<FlatPattern | null>(null);

  // Load frame set data when component mounts
  useEffect(() => {
    loadFrameSet();
  }, [frameSetId]);

  const loadFrameSet = async () => {
    try {
      // Get frame set details to check if flat_pattern is set
      const sets = await invoke<Array<{ frames_set: FramesSet; member_count: number }>>('get_frames_sets_by_project', {
        projectId: 1 // Using default project
      });
      const currentSet = sets.find(s => s.frames_set.id === frameSetId);
      if (currentSet) {
        setFrameSet(currentSet.frames_set);
      }
    } catch (err) {
      console.error('Failed to load frame set:', err);
    }
  };

  const handleFindCalibration = async () => {
    try {
      setError(null);

      // Check if frame set has a flat_pattern set
      if (!frameSet?.flat_pattern) {
        // No pattern set - show pattern selection modal
        setShowPatternModal(true);
        return;
      }

      // Pattern is set - proceed with calibration
      await runCalibrationFinder(frameSet.flat_pattern, null);

    } catch (err) {
      console.error('Failed to find calibration:', err);
      setError(String(err));
    }
  };

  const handlePatternSelected = async (pattern: FlatPattern, remember: boolean) => {
    setShowPatternModal(false);
    setSelectedPattern(pattern);

    // Save pattern to frame set if remember is checked
    if (remember && frameSet) {
      try {
        await invoke('update_frame_set_flat_pattern', {
          frameSetId,
          flatPattern: pattern
        });
        // Reload frame set to get updated pattern
        await loadFrameSet();
      } catch (err) {
        console.error('Failed to save flat pattern:', err);
      }
    }

    // If manual pattern, show manual selection modal
    if (pattern === FlatPattern.Manual) {
      await loadFlatGroupOptions();
      setShowManualSelectionModal(true);
    } else {
      // Auto pattern - proceed with calibration
      await runCalibrationFinder(pattern, null);
    }
  };

  const loadFlatGroupOptions = async () => {
    try {
      const options = await invoke<Record<string, FlatGroup[]>>('get_flat_group_options_for_frame_set', {
        frameSetId
      });
      setFlatGroupOptions(options);
    } catch (err) {
      console.error('Failed to load flat group options:', err);
      setError('Failed to load available flat groups');
    }
  };

  const handleManualSelection = async (selections: Record<string, number>) => {
    setShowManualSelectionModal(false);

    // TODO: This needs to be updated to create calibration sets from the selected groups
    // For now, we'll pass the selections directly and let the backend handle it
    await runCalibrationFinder(selectedPattern || FlatPattern.Manual, selections);
  };

  const runCalibrationFinder = async (pattern: string | FlatPattern, manualSelections: Record<string, number> | null) => {
    try {
      setIsProcessing(true);
      setShowProcessModal(true);

      const result = await invoke<ProcessingStats>('find_calibration_for_frame_set', {
        frameSetId,
        // Use default tolerances
        tempDeltaCelsius: 2.0,
        flatDateWarningDays: 30,
        darkDateWarningDays: 365,
        flatPattern: pattern,
        manualFlatSelections: manualSelections
      });

      setStats(result);

      if (onComplete) {
        onComplete(result);
      }

    } catch (err) {
      console.error('Failed to find calibration:', err);
      setError(String(err));
    } finally {
      setIsProcessing(false);
    }
  };

  const handleClose = () => {
    setShowProcessModal(false);
    setStats(null);
    setError(null);
  };

  return (
    <>
      <button
        onClick={handleFindCalibration}
        disabled={isProcessing}
        className="flex items-center gap-2 px-3 py-1.5 bg-purple-600 hover:bg-purple-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white text-sm rounded transition-colors"
        title="Find and link calibration data for this frame set"
      >
        <Zap size={16} />
        {isProcessing ? 'Finding...' : 'Find Calibration'}
      </button>

      {showPatternModal && (
        <FlatPatternModal
          isOpen={showPatternModal}
          onSelect={handlePatternSelected}
          onCancel={() => setShowPatternModal(false)}
        />
      )}

      {showManualSelectionModal && (
        <ManualFlatSelectionModal
          isOpen={showManualSelectionModal}
          flatGroupOptions={flatGroupOptions}
          onSelect={handleManualSelection}
          onCancel={() => setShowManualSelectionModal(false)}
        />
      )}

      {showProcessModal && (
        <CalibrationProcessModal
          frameSetName={frameSetName || `Frame Set ${frameSetId}`}
          isProcessing={isProcessing}
          stats={stats}
          error={error}
          onClose={handleClose}
        />
      )}
    </>
  );
}
