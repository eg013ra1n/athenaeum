/**
 * Dialog component for displaying spatial selection results
 * Shows selected frames and provides options to create frame sets
 */

import { useState, useEffect, useRef } from 'react';
import { api } from '../api';
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

  // T1-4 — track the success-auto-close timer so we can clear it on
  // unmount, on manual close, or when a new selection arrives mid-flight.
  const successCloseTimerRef = useRef<number | undefined>(undefined);
  const inputRef = useRef<HTMLInputElement>(null);

  // Reset dialog state when opened with new selection data
  useEffect(() => {
    if (isOpen && result) {
      setFrameSetName('');
      setError(null);
      setSuccess(false);
      setIsCreating(false);
      // New selection → cancel any stale success-close timer left over
      // from a previous successful create.
      if (successCloseTimerRef.current !== undefined) {
        window.clearTimeout(successCloseTimerRef.current);
        successCloseTimerRef.current = undefined;
      }
    }
  }, [isOpen, result]);

  // Clear timer on unmount.
  useEffect(() => {
    return () => {
      if (successCloseTimerRef.current !== undefined) {
        window.clearTimeout(successCloseTimerRef.current);
      }
    };
  }, []);

  // T2-14 — auto-focus the name input when the dialog opens, and close on
  // Escape so keyboard-only users have a way out.
  useEffect(() => {
    if (!isOpen || success) return;
    inputRef.current?.focus();
  }, [isOpen, success]);

  useEffect(() => {
    if (!isOpen) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !isCreating) {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [isOpen, isCreating, onClose]);

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

      const frameSetId = await api.invoke<number>('create_frame_set_from_selection', {
        frame_ids: result.frameIds,
        name: frameSetName,
        description: selectionDescription || ''
      });

      console.log('Frame set created successfully with ID:', frameSetId);

      setSuccess(true);
      setFrameSetName('');

      // Clear success message after 2 seconds and close. Capture the timer
      // id so we can cancel it on unmount, manual close, or when a new
      // selection opens (T1-4).
      if (successCloseTimerRef.current !== undefined) {
        window.clearTimeout(successCloseTimerRef.current);
      }
      successCloseTimerRef.current = window.setTimeout(() => {
        successCloseTimerRef.current = undefined;
        onClose();
      }, 2000);

      // Call callback if provided
      onCreateFrameSet?.(frameSetName);
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : String(err);
      console.error('Failed to create frame set:', errorMsg, err);
      // T2-19 — surface a friendly prefix; raw value already in console.
      setError(`Couldn't create the frame set: ${errorMsg || 'Unknown error'}`);
    } finally {
      setIsCreating(false);
    }
  };

  if (!isOpen || !result) return null;

  const totalHours = (result.totalExposureSeconds / 3600).toFixed(2);

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-surface-elevated rounded-lg shadow-lg max-w-md w-full mx-4">
        {/* Header */}
        <div className="flex items-center justify-between p-6 border-b border-border">
          <h2 className="text-lg font-semibold text-content">
            {selectionType === 'rectangle'
              ? 'Rectangle Selection Results'
              : 'Selection Results'}
          </h2>
          <button
            onClick={onClose}
            className="text-content-muted hover:text-content transition"
          >
            <X size={20} />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-4">
          {/* Results Summary */}
          {!success ? (
            <>
              <div className="bg-surface-hover rounded p-4 space-y-2">
                <div className="flex justify-between text-sm">
                  <span className="text-content-secondary">Frames Found:</span>
                  <span className="text-accent font-semibold">{result.count}</span>
                </div>
                <div className="flex justify-between text-sm">
                  <span className="text-content-secondary">Total Exposure:</span>
                  <span className="text-accent font-semibold">{totalHours}h</span>
                </div>
              </div>

              {/* Description */}
              {selectionDescription && (
                <div className="text-sm text-content-muted p-3 bg-surface-hover rounded">
                  {selectionDescription}
                </div>
              )}

              {/* Frame Set Name Input */}
              <div>
                <label className="block text-sm font-medium text-content-secondary mb-2">
                  Create Frame Set
                </label>
                <input
                  ref={inputRef}
                  type="text"
                  value={frameSetName}
                  onChange={(e) => {
                    setFrameSetName(e.target.value);
                    setError(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !isCreating && frameSetName.trim()) {
                      e.preventDefault();
                      void handleCreateFrameSet();
                    }
                  }}
                  placeholder="e.g., M31 Imaging Session"
                  className="w-full px-3 py-2 bg-surface-hover border border-border rounded text-content placeholder-content-muted focus:outline-none focus:ring-2 focus:ring-accent focus:border-accent transition"
                />
              </div>

              {/* Error Message */}
              {error && (
                <div className="flex items-center gap-2 text-error text-sm p-3 bg-error-muted rounded">
                  <AlertCircle size={16} />
                  <span>{error}</span>
                </div>
              )}

              {/* Action Buttons */}
              <div className="flex gap-2 pt-4">
                <button
                  onClick={onClose}
                  className="flex-1 px-4 py-2 bg-surface-hover hover:bg-surface-hover text-content rounded transition"
                >
                  Cancel
                </button>
                <button
                  onClick={handleCreateFrameSet}
                  disabled={isCreating || !frameSetName.trim()}
                  className="flex-1 px-4 py-2 bg-accent hover:bg-accent-hover disabled:opacity-50 text-surface rounded transition flex items-center justify-center gap-2"
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
                <div className="w-12 h-12 bg-success-muted rounded-full flex items-center justify-center">
                  <Check className="text-success" size={24} />
                </div>
              </div>
              <div>
                <h3 className="text-lg font-semibold text-success">Frame Set Created</h3>
                <p className="text-sm text-content-muted mt-1">
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
