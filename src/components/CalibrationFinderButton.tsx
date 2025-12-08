import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Zap } from 'lucide-react';
import type { ProcessingStats, FramesSet } from '../types/models';
import { FlatPattern } from '../types/models';
import { CalibrationProcessModal } from './CalibrationProcessModal';
import { FlatPatternModal } from './FlatPatternModal';

interface CalibrationFinderButtonProps {
  frameSetId: number;
  frameSetName: string | null;
  onComplete?: (stats: ProcessingStats) => void;
}

export function CalibrationFinderButton({ frameSetId, frameSetName, onComplete }: CalibrationFinderButtonProps) {
  const [isProcessing, setIsProcessing] = useState(false);
  const [showProcessModal, setShowProcessModal] = useState(false);
  const [showPatternModal, setShowPatternModal] = useState(false);
  const [stats, setStats] = useState<ProcessingStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [frameSet, setFrameSet] = useState<FramesSet | null>(null);
  const [completedStats, setCompletedStats] = useState<ProcessingStats | null>(null);

  // Load frame set data when component mounts
  useEffect(() => {
    loadFrameSet();
  }, [frameSetId]);

  const loadFrameSet = async () => {
    try {
      // Get frame set details to check if flat_pattern is set
      const sets = await invoke<Array<{ frames_set: FramesSet; member_count: number }>>('get_frames_sets', {
        projectId: 1 // Using default project (parameter ignored but kept for compatibility)
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

    // Proceed with calibration
    await runCalibrationFinder(pattern, null);
  };

  const runCalibrationFinder = async (pattern: string | FlatPattern, manualSelections: Record<string, number> | null) => {
    // Clear previous state before starting
    setShowProcessModal(true);
    setIsProcessing(true);
    setError(null);
    setStats(null);

    try {
      const result = await invoke<ProcessingStats>('find_calibration_for_frame_set', {
        frameSetId,
        // Tolerance parameters now pulled from Settings (backend will use configured values)
        flatPattern: pattern,
        manualFlatSelections: manualSelections
      });

      setStats(result);
      setError(null); // Ensure error is cleared on success
      setCompletedStats(result); // Save for onComplete callback when modal closes

    } catch (err) {
      console.error('Failed to find calibration:', err);
      setError(String(err));
      setStats(null); // Clear stats on error
      setCompletedStats(null);
    } finally {
      setIsProcessing(false);
    }
  };

  const handleClose = () => {
    setShowProcessModal(false);
    setStats(null);
    setError(null);

    // Call onComplete after modal closes (not immediately after calibration)
    // This prevents parent re-renders from interfering with modal display
    if (completedStats && onComplete) {
      onComplete(completedStats);
    }
    setCompletedStats(null);
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
