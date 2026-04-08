import { useState } from 'react';
import { api } from '../api';
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
  const [showProcessModal, setShowProcessModal] = useState(false);
  const [stats, setStats] = useState<ProcessingStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [completedStats, setCompletedStats] = useState<ProcessingStats | null>(null);

  const handleFindCalibration = async () => {
    setShowProcessModal(true);
    setIsProcessing(true);
    setError(null);
    setStats(null);

    try {
      const result = await api.invoke<ProcessingStats>('find_calibration_for_frame_set', {
        frameSetId,
        flatPattern: "automatic",
        manualFlatSelections: null
      });

      setStats(result);
      setError(null);
      setCompletedStats(result);

    } catch (err) {
      console.error('Failed to find calibration:', err);
      setError(String(err));
      setStats(null);
      setCompletedStats(null);
    } finally {
      setIsProcessing(false);
    }
  };

  const handleClose = () => {
    setShowProcessModal(false);
    setStats(null);
    setError(null);

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
        className="h-7 inline-flex items-center gap-1.5 px-3 text-xs font-medium bg-purple hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
        title="Find and link calibration data for this frame set"
      >
        <Zap size={12} />
        {isProcessing ? 'Finding...' : 'Find Calibration'}
      </button>

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
