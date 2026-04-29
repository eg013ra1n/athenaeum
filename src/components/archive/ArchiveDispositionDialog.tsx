import { useEffect, useState } from 'react';
import type {
  ArchiveCompression,
  ArchivePlan,
  ConflictResolution,
  Dispositions,
  FrameRole,
} from '../../types/archive';
import { planArchiveOperation } from '../../api/archive';
import { ArchiveConflictDialog } from './ArchiveConflictDialog';

interface Props {
  framesSetId: number;
  archiveRootPath: string;
  defaultCompression: ArchiveCompression;
  onCancel: () => void;
  onStart: (
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict: ConflictResolution,
  ) => void;
}

const DEFAULT_DISP: Dispositions = { flats: 'skip', darks: 'skip', bias: 'skip', darkflats: 'skip' };

export function ArchiveDispositionDialog(props: Props) {
  const [dispositions, setDispositions] = useState<Dispositions>(DEFAULT_DISP);
  const [compression, setCompression] = useState<ArchiveCompression>(props.defaultCompression);
  const [plan, setPlan] = useState<ArchivePlan | null>(null);
  const [planning, setPlanning] = useState(false);
  const [planError, setPlanError] = useState<string | null>(null);
  const [showConflict, setShowConflict] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setPlanning(true);
    setPlanError(null);
    planArchiveOperation(props.framesSetId, dispositions, compression)
      .then(p => { if (!cancelled) setPlan(p); })
      .catch(err => {
        console.error('plan failed', err);
        if (!cancelled) setPlanError(String(err));
      })
      .finally(() => { if (!cancelled) setPlanning(false); });
    return () => { cancelled = true; };
  }, [props.framesSetId, dispositions, compression]);

  const sharedRoles = new Set(plan?.shared_calibrations.map(s => s.frame_role) ?? []);

  function rowFor(role: 'flats' | 'darks' | 'bias' | 'darkflats', label: string, frameRole: FrameRole) {
    const moveDisabled = sharedRoles.has(frameRole);
    const sharedCount = plan?.shared_calibrations.find(s => s.frame_role === frameRole)?.other_frames_set_ids.length ?? 0;
    return (
      <div key={role} className="flex items-center gap-3 py-2 border-b border-border/30 last:border-0">
        <span className="w-24 text-sm font-medium text-content">{label}</span>
        {(['move', 'copy', 'skip'] as const).map(opt => (
          <label
            key={opt}
            className={`flex items-center gap-1.5 text-sm cursor-pointer ${
              moveDisabled && opt === 'move' ? 'opacity-40 cursor-not-allowed' : 'text-content'
            }`}
            title={moveDisabled && opt === 'move' ? `Used by ${sharedCount} other frame set(s) — only Copy is allowed.` : undefined}
          >
            <input
              type="radio"
              name={`disp-${role}`}
              checked={dispositions[role] === opt}
              disabled={moveDisabled && opt === 'move'}
              onChange={() => setDispositions(d => ({ ...d, [role]: opt }))}
              className="accent-accent"
            />
            <span className="capitalize">{opt}</span>
          </label>
        ))}
      </div>
    );
  }

  function handleStart() {
    if (!plan) return;
    if (plan.conflicts.length > 0) {
      setShowConflict(true);
      return;
    }
    props.onStart(dispositions, compression, 'overwrite');
  }

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
      <div className="bg-surface-elevated border border-border rounded-lg shadow-2xl p-6 max-w-2xl w-full max-h-[90vh] overflow-y-auto">
        <h2 className="text-lg font-semibold mb-1 text-content">Archive Frame Set</h2>
        <p className="text-sm text-content-muted mb-4">
          Lights will always be moved. Choose what to do with calibration frames.
        </p>

        <h3 className="text-sm font-medium text-content mb-2">Calibrations</h3>
        <div className="mb-4 rounded-lg border border-border bg-surface px-3">
          {rowFor('flats', 'Flats', 'flat')}
          {rowFor('darks', 'Darks', 'dark')}
          {rowFor('bias', 'Bias', 'bias')}
          {rowFor('darkflats', 'DarkFlats', 'darkflat')}
        </div>

        <h3 className="text-sm font-medium text-content mb-2">Compression</h3>
        <select
          value={compression}
          onChange={e => setCompression(e.target.value as ArchiveCompression)}
          className="bg-surface-hover border border-border rounded-lg px-3 py-1.5 text-sm text-content mb-4 focus:outline-none focus:border-accent"
        >
          <option value="store">Store (no compression — fastest)</option>
          <option value="deflate">Deflate (smaller, slower)</option>
        </select>

        {planning && (
          <p className="text-sm text-content-muted mb-4 flex items-center gap-2">
            <span className="inline-block w-4 h-4 border-2 border-accent border-t-transparent rounded-full animate-spin" />
            Computing plan…
          </p>
        )}

        {planError && (
          <p className="text-sm text-error mb-4">{planError}</p>
        )}

        {plan && !planning && (
          <div className="mb-4 text-sm bg-surface border border-border rounded-lg p-3 space-y-2">
            <p className="text-content font-medium">Estimated archive</p>
            <p className="text-content-muted">
              Total size: {(plan.total_size_bytes / 1024 / 1024).toFixed(1)} MB
            </p>
            {plan.zips.length > 0 && (
              <ul className="list-disc ml-5 space-y-1 text-content-muted">
                {plan.zips.map(z => (
                  <li key={z.zip_filename}>
                    <span className="font-mono text-xs">{z.zip_filename}</span>
                    {' — '}{z.file_count} file{z.file_count !== 1 ? 's' : ''},{' '}
                    {(z.total_size_bytes / 1024 / 1024).toFixed(1)} MB
                  </li>
                ))}
              </ul>
            )}
            {plan.conflicts.length > 0 && (
              <p className="text-warning text-xs pt-1">
                {plan.conflicts.length} of these zip name(s) already exist in the archive folder — you will be asked how to resolve before writing starts.
              </p>
            )}
          </div>
        )}

        <div className="flex justify-end gap-2 pt-2">
          <button
            onClick={props.onCancel}
            className="px-4 py-1.5 rounded-lg border border-border text-sm text-content hover:bg-surface-hover transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleStart}
            disabled={planning || !plan}
            className="px-4 py-1.5 rounded-lg bg-accent text-white text-sm disabled:opacity-50 disabled:cursor-not-allowed hover:brightness-110 transition-colors"
          >
            Start Archiving
          </button>
        </div>
      </div>

      {showConflict && plan && (
        <ArchiveConflictDialog
          conflicts={plan.conflicts}
          onCancel={() => setShowConflict(false)}
          onResolve={(resolution) => {
            setShowConflict(false);
            props.onStart(dispositions, compression, resolution);
          }}
        />
      )}
    </div>
  );
}
