/**
 * Dialog component for displaying spatial selection results
 * Shows selected frames and provides options to create frame sets
 */

import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { X, Check, AlertCircle } from 'lucide-react';
import { SelectionResult } from '../types/selection';

export interface SelectionDialogProps {
  isOpen: boolean;
  result: SelectionResult | null;
  selectionType: 'rectangle' | null;
  selectionDescription?: string;
  onClose: () => void;
  onCreateFrameSet?: (frameSetName: string) => void;
}

/**
 * Dialog showing spatial selection results
 */
export function SelectionDialog({
  isOpen,
  result,
  selectionType,
  selectionDescription,
  onClose,
  onCreateFrameSet
}: SelectionDialogProps) {
  const [frameSetName, setFrameSetName] = useState('');
  const [isCreating, setIsCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  const handleCreateFrameSet = async () => {
    if (!frameSetName.trim() || !result) {
      setError('Please enter a frame set name');
      return;
    }

    setIsCreating(true);
    setError(null);

    try {
      console.log('Creating frame set with params:', {
        name: frameSetName,
        frame_ids_count: result.frameIds.length,
        frame_ids_sample: result.frameIds.slice(0, 5),
        description: selectionDescription || ''
      });

      const frameSetId = await invoke<number>('create_frame_set_from_selection', {
        frame_ids: result.frameIds,
        name: frameSetName,
        description: selectionDescription || ''
      });

      console.log('Frame set created successfully with ID:', frameSetId);

      setSuccess(true);
      setFrameSetName('');

      // Clear success message after 2 seconds and close
      setTimeout(() => {
        onClose();
      }, 2000);

      // Call callback if provided
      onCreateFrameSet?.(frameSetName);
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : String(err);
      console.error('Failed to create frame set:', errorMsg, err);
      setError(errorMsg || 'Failed to create frame set');
    } finally {
      setIsCreating(false);
    }
  };

  if (!isOpen || !result) return null;

  const totalHours = (result.totalExposureSeconds / 3600).toFixed(2);

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-gray-800 rounded-lg shadow-lg max-w-md w-full mx-4">
        {/* Header */}
        <div className="flex items-center justify-between p-6 border-b border-gray-700">
          <h2 className="text-lg font-semibold text-gray-100">
            {selectionType === 'rectangle'
              ? 'Rectangle Selection Results'
              : 'Selection Results'}
          </h2>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-200 transition"
          >
            <X size={20} />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-4">
          {/* Results Summary */}
          {!success ? (
            <>
              <div className="bg-gray-700 rounded p-4 space-y-2">
                <div className="flex justify-between text-sm">
                  <span className="text-gray-300">Frames Found:</span>
                  <span className="text-blue-400 font-semibold">{result.count}</span>
                </div>
                <div className="flex justify-between text-sm">
                  <span className="text-gray-300">Total Exposure:</span>
                  <span className="text-blue-400 font-semibold">{totalHours}h</span>
                </div>
              </div>

              {/* Description */}
              {selectionDescription && (
                <div className="text-sm text-gray-400 p-3 bg-gray-700 rounded">
                  {selectionDescription}
                </div>
              )}

              {/* Frame Set Name Input */}
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-2">
                  Create Frame Set
                </label>
                <input
                  type="text"
                  value={frameSetName}
                  onChange={(e) => {
                    setFrameSetName(e.target.value);
                    setError(null);
                  }}
                  placeholder="e.g., M31 Imaging Session"
                  className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded text-gray-100 placeholder-gray-500 focus:outline-none focus:border-blue-500 transition"
                />
              </div>

              {/* Error Message */}
              {error && (
                <div className="flex items-center gap-2 text-red-400 text-sm p-3 bg-red-950 rounded">
                  <AlertCircle size={16} />
                  <span>{error}</span>
                </div>
              )}

              {/* Action Buttons */}
              <div className="flex gap-2 pt-4">
                <button
                  onClick={onClose}
                  className="flex-1 px-4 py-2 bg-gray-700 hover:bg-gray-600 text-gray-100 rounded transition"
                >
                  Cancel
                </button>
                <button
                  onClick={handleCreateFrameSet}
                  disabled={isCreating || !frameSetName.trim()}
                  className="flex-1 px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-blue-900 text-white rounded transition flex items-center justify-center gap-2"
                >
                  {isCreating ? (
                    <>
                      <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-white"></div>
                      Creating...
                    </>
                  ) : (
                    <>
                      <Check size={16} />
                      Create Set
                    </>
                  )}
                </button>
              </div>
            </>
          ) : (
            /* Success State */
            <div className="text-center space-y-4">
              <div className="flex justify-center">
                <div className="w-12 h-12 bg-green-900 rounded-full flex items-center justify-center">
                  <Check className="text-green-400" size={24} />
                </div>
              </div>
              <div>
                <h3 className="text-lg font-semibold text-green-400">Frame Set Created</h3>
                <p className="text-sm text-gray-400 mt-1">
                  {result.count} frames grouped successfully
                </p>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
