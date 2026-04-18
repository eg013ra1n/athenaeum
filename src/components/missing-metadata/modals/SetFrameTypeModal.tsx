import React, { useEffect, useState } from 'react';
import { Tag, Loader2 } from 'lucide-react';
import { api } from '../../../api';
import type { FrameMetadataEdits } from '../../../types/models';

type FrameTypeChoice =
  | 'LIGHT'
  | 'DARK'
  | 'FLAT'
  | 'BIAS'
  | 'DARKFLAT'
  | 'MASTERDARK'
  | 'MASTERFLAT';

interface SetFrameTypeModalProps {
  isOpen: boolean;
  eligibleCount: number;
  totalSelected: number;
  eligibleFrameIds: number[];
  onApplied: () => void;
  onCancel: () => void;
}

const FRAME_TYPES: { value: FrameTypeChoice; label: string }[] = [
  { value: 'LIGHT', label: 'LIGHT' },
  { value: 'DARK', label: 'DARK' },
  { value: 'FLAT', label: 'FLAT' },
  { value: 'BIAS', label: 'BIAS' },
  { value: 'DARKFLAT', label: 'DARKFLAT' },
  { value: 'MASTERDARK', label: 'MASTERDARK' },
  { value: 'MASTERFLAT', label: 'MASTERFLAT' },
];

function toEdits(choice: FrameTypeChoice): FrameMetadataEdits {
  switch (choice) {
    case 'MASTERDARK':
      return { imagetyp: 'DARK', isMaster: true };
    case 'MASTERFLAT':
      return { imagetyp: 'FLAT', isMaster: true };
    default:
      return { imagetyp: choice, isMaster: false };
  }
}

export const SetFrameTypeModal: React.FC<SetFrameTypeModalProps> = ({
  isOpen,
  eligibleCount,
  totalSelected,
  eligibleFrameIds,
  onApplied,
  onCancel,
}) => {
  const [selected, setSelected] = useState<FrameTypeChoice | null>(null);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    setSelected(null);
    setError(null);
  }, [isOpen]);

  if (!isOpen) return null;

  const handleConfirm = async () => {
    if (!selected) {
      setError('Please select a frame type');
      return;
    }
    try {
      setApplying(true);
      setError(null);
      const edits = toEdits(selected);
      await api.invoke<number>('bulk_update_frame_metadata', {
        frameIds: eligibleFrameIds,
        edits,
      });
      onApplied();
    } catch (err) {
      console.error('bulk_update_frame_metadata failed:', err);
      setError(typeof err === 'string' ? err : 'Failed to update frames');
    } finally {
      setApplying(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-surface-elevated rounded-lg p-6 w-full max-w-md mx-4 border border-border shadow-xl">
        <div className="flex items-center gap-2 mb-1">
          <Tag size={18} className="text-accent" />
          <h3 className="text-lg font-semibold text-content">Set Frame Type</h3>
        </div>
        <p className="text-content-secondary text-sm mb-5">
          Set frame type on{' '}
          <span className="font-semibold text-content">{eligibleCount}</span>
          {' '}of{' '}
          <span className="font-semibold text-content">{totalSelected}</span>
          {' '}selected frames
        </p>

        <div className="space-y-2">
          {FRAME_TYPES.map(ft => (
            <label key={ft.value} className="flex items-center gap-2 cursor-pointer group">
              <input
                type="radio"
                name="frame-type-select"
                value={ft.value}
                checked={selected === ft.value}
                onChange={() => setSelected(ft.value)}
                className="text-accent focus:ring-accent"
              />
              <span className="text-sm text-content group-hover:text-content font-mono">
                {ft.label}
              </span>
            </label>
          ))}
        </div>

        {error && (
          <p className="mt-3 text-error text-xs">{error}</p>
        )}

        <div className="flex gap-3 justify-end mt-6">
          <button
            onClick={onCancel}
            disabled={applying}
            className="px-4 py-2 bg-surface-hover text-content rounded hover:brightness-110 transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={handleConfirm}
            disabled={applying || !selected}
            className="px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
          >
            {applying && <Loader2 size={14} className="animate-spin" />}
            Apply
          </button>
        </div>
      </div>
    </div>
  );
};
