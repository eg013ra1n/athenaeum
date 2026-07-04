import React, { useEffect, useState } from 'react';
import { Calendar, Loader2 } from 'lucide-react';
import { api } from '../../../api';
import type { FrameMetadataEdits } from '../../../types/models';

interface SetDateModalProps {
  isOpen: boolean;
  eligibleCount: number;
  totalSelected: number;
  eligibleFrameIds: number[];
  /** Modified-at timestamps of the eligible files, used for the quick-fill option. */
  eligibleFileModifiedAts: string[];
  onApplied: () => void;
  onCancel: () => void;
}

/** Convert a JS Date to the local "YYYY-MM-DDTHH:MM" string required by datetime-local inputs. */
function toDatetimeLocalString(date: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0');
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    `T${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/** Parse an ISO 8601 string to a local Date, returning null on failure. */
function parseIso(s: string | null | undefined): Date | null {
  if (!s) return null;
  const d = new Date(s);
  return isNaN(d.getTime()) ? null : d;
}

export const SetDateModal: React.FC<SetDateModalProps> = ({
  isOpen,
  eligibleCount,
  totalSelected,
  eligibleFrameIds,
  eligibleFileModifiedAts,
  onApplied,
  onCancel,
}) => {
  const [dateValue, setDateValue] = useState('');
  const [useFileTime, setUseFileTime] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Reset state when the modal opens
  useEffect(() => {
    if (!isOpen) return;
    setDateValue('');
    setUseFileTime(false);
    setError(null);
  }, [isOpen]);

  // When "use file modified time" is toggled on, pre-fill with the earliest modified_at
  useEffect(() => {
    if (!useFileTime) return;
    const dates = eligibleFileModifiedAts
      .map(s => parseIso(s))
      .filter((d): d is Date => d !== null);
    if (dates.length === 0) return;
    const earliest = new Date(Math.min(...dates.map(d => d.getTime())));
    setDateValue(toDatetimeLocalString(earliest));
  }, [useFileTime, eligibleFileModifiedAts]);

  if (!isOpen) return null;

  const handleConfirm = async () => {
    if (!dateValue) {
      setError('Please enter a date and time');
      return;
    }
    // datetime-local gives "YYYY-MM-DDTHH:MM", append ":00Z" for RFC 3339
    const dateObs = `${dateValue}:00Z`;
    try {
      setApplying(true);
      setError(null);
      // isMaster is not a tri-state (absent | null | value) field like the
      // others here — it's a plain, required, nullable boolean on the wire —
      // so it must be explicitly set even though this modal never edits it.
      const edits: FrameMetadataEdits = { dateObs, isMaster: null };
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
          <Calendar size={18} className="text-accent" />
          <h3 className="text-lg font-semibold text-content">Set Date</h3>
        </div>
        <p className="text-content-secondary text-sm mb-5">
          Set date on{' '}
          <span className="font-semibold text-content">{eligibleCount}</span>
          {' '}of{' '}
          <span className="font-semibold text-content">{totalSelected}</span>
          {' '}selected frames
        </p>

        <div className="space-y-4">
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={useFileTime}
              onChange={e => setUseFileTime(e.target.checked)}
              className="rounded border-border text-accent focus:ring-accent"
            />
            <span className="text-sm text-content-secondary">
              Use file modified time (earliest of selected)
            </span>
          </label>

          <div>
            <label className="block text-xs text-content-muted mb-1.5">
              Date &amp; Time (UTC)
            </label>
            <input
              type="datetime-local"
              value={dateValue}
              onChange={e => { setDateValue(e.target.value); setUseFileTime(false); }}
              className="w-full bg-surface border border-border rounded px-3 py-2 text-sm text-content focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="text-xs text-content-muted mt-1">
              Stored as RFC 3339 UTC. Example: 2024-06-15 22:30:00
            </p>
          </div>
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
            disabled={applying || !dateValue}
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
