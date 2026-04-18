import React, { useEffect, useState } from 'react';
import { Camera, Loader2 } from 'lucide-react';
import { api } from '../../../api';

interface SetCameraModalProps {
  isOpen: boolean;
  eligibleCount: number;
  totalSelected: number;
  eligibleFrameIds: number[];
  onApplied: () => void;
  onCancel: () => void;
}

export const SetCameraModal: React.FC<SetCameraModalProps> = ({
  isOpen,
  eligibleCount,
  totalSelected,
  eligibleFrameIds,
  onApplied,
  onCancel,
}) => {
  const [cameras, setCameras] = useState<string[]>([]);
  const [loadingCameras, setLoadingCameras] = useState(false);
  const [selectedCamera, setSelectedCamera] = useState<string>('');
  const [createNew, setCreateNew] = useState(false);
  const [newCameraName, setNewCameraName] = useState('');
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    setSelectedCamera('');
    setCreateNew(false);
    setNewCameraName('');
    setError(null);
    setLoadingCameras(true);
    api.invoke<string[]>('get_distinct_instrumes')
      .then(list => setCameras(list))
      .catch(err => {
        console.error('Failed to load cameras:', err);
        setError(typeof err === 'string' ? err : 'Failed to load camera list');
      })
      .finally(() => setLoadingCameras(false));
  }, [isOpen]);

  if (!isOpen) return null;

  const handleConfirm = async () => {
    const instrume = createNew ? newCameraName.trim() : selectedCamera;
    if (!instrume) {
      setError('Please select or enter a camera name');
      return;
    }
    try {
      setApplying(true);
      setError(null);
      await api.invoke<number>('bulk_update_frame_metadata', {
        frameIds: eligibleFrameIds,
        edits: { instrume },
      });
      onApplied();
    } catch (err) {
      console.error('bulk_update_frame_metadata failed:', err);
      setError(typeof err === 'string' ? err : 'Failed to update frames');
    } finally {
      setApplying(false);
    }
  };

  const canConfirm = createNew ? newCameraName.trim().length > 0 : selectedCamera.length > 0;

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-surface-elevated rounded-lg p-6 w-full max-w-md mx-4 border border-border shadow-xl">
        <div className="flex items-center gap-2 mb-1">
          <Camera size={18} className="text-accent" />
          <h3 className="text-lg font-semibold text-content">Set Camera</h3>
        </div>
        <p className="text-content-secondary text-sm mb-5">
          Set camera on{' '}
          <span className="font-semibold text-content">{eligibleCount}</span>
          {' '}of{' '}
          <span className="font-semibold text-content">{totalSelected}</span>
          {' '}selected frames
        </p>

        {loadingCameras ? (
          <div className="flex items-center justify-center py-6 gap-2 text-content-muted">
            <Loader2 size={16} className="animate-spin" />
            <span className="text-sm">Loading cameras…</span>
          </div>
        ) : (
          <div className="space-y-3">
            {cameras.length > 0 && (
              <div className="space-y-1.5 max-h-48 overflow-y-auto pr-1">
                {cameras.map(cam => (
                  <label key={cam} className="flex items-center gap-2 cursor-pointer group">
                    <input
                      type="radio"
                      name="camera-select"
                      value={cam}
                      checked={!createNew && selectedCamera === cam}
                      onChange={() => { setSelectedCamera(cam); setCreateNew(false); }}
                      className="text-accent focus:ring-accent"
                    />
                    <span className="text-sm text-content group-hover:text-content">
                      {cam}
                    </span>
                  </label>
                ))}
              </div>
            )}

            <label className="flex items-center gap-2 cursor-pointer group pt-1 border-t border-border">
              <input
                type="radio"
                name="camera-select"
                checked={createNew}
                onChange={() => { setCreateNew(true); setSelectedCamera(''); }}
                className="text-accent focus:ring-accent"
              />
              <span className="text-sm text-content-secondary group-hover:text-content">
                Create new camera…
              </span>
            </label>

            {createNew && (
              <input
                type="text"
                value={newCameraName}
                onChange={e => setNewCameraName(e.target.value)}
                placeholder="e.g. ASI2600MM Pro"
                autoFocus
                className="w-full bg-surface border border-border rounded px-3 py-2 text-sm text-content placeholder-content-muted focus:outline-none focus:ring-2 focus:ring-accent"
              />
            )}
          </div>
        )}

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
            disabled={applying || loadingCameras || !canConfirm}
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
