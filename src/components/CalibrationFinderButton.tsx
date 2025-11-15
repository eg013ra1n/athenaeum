import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Zap } from 'lucide-react';
import type { ProcessingStats } from '../types/models';
import { CalibrationProcessModal } from './CalibrationProcessModal';

interface CalibrationFinderButtonProps {
  frameSetId: number;
  frameSetName: string | null;
  onComplete?: (stats: ProcessingStats) => void;
}

export function CalibrationFinderButton({ frameSetId, frameSetName, onComplete }: CalibrationFinderButtonProps) {
  const [isProcessing, setIsProcessing] = useState(false);
  const [showModal, setShowModal] = useState(false);
  const [stats, setStats] = useState<ProcessingStats | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleFindCalibration = async () => {
    try {
      setError(null);
      setIsProcessing(true);
      setShowModal(true);

      const result = await invoke<ProcessingStats>('find_calibration_for_frame_set', {
        frameSetId,
        // Use default tolerances
        tempDeltaCelsius: 2.0,
        flatDateWarningDays: 30,
        darkDateWarningDays: 365
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
    setShowModal(false);
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

      {showModal && (
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
