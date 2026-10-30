import { useEffect, useState } from 'react';
import type {
  ArchiveCompression,
  ArchivePlan,
  ConflictResolution,
  Dispositions,
  FrameRole,
} from '../../types/archive';
import type { ArchiveRoot } from '../../types/helpers';
import { listArchiveRoots, planArchiveOperation } from '../../api/archive';
import { Button, DialogShell, Select } from '../ui';
import { ArchiveConflictDialog } from './ArchiveConflictDialog';

interface Props {
  framesSetId: number;
  defaultCompression: ArchiveCompression;
  onCancel: () => void;
  onStart: (
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict: ConflictResolution,
    archiveRootPath: string,
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

  // Multi-folder destination support
  const [archiveRoots, setArchiveRoots] = useState<ArchiveRoot[]>([]);
  const [selectedRootPath, setSelectedRootPath] = useState<string | null>(null);
  const [rootsLoaded, setRootsLoaded] = useState(false);

  // Load archive roots once on mount
  useEffect(() => {
    listArchiveRoots()
      .then((roots) => {
        setArchiveRoots(roots);
        const def = roots.find(r => r.is_default) ?? roots[0];
        setSelectedRootPath(def?.path ?? null);
      })
      .catch((e) => {
        console.error('Failed to load archive roots', e);
      })
      .finally(() => setRootsLoaded(true));
  }, []);

  // Re-plan whenever dispositions, compression, or selected root change
  useEffect(() => {
    if (!rootsLoaded || !selectedRootPath) return;
    let cancelled = false;
    setPlanning(true);
    setPlanError(null);
    planArchiveOperation(props.framesSetId, dispositions, compression, selectedRootPath)
      .then(p => { if (!cancelled) setPlan(p); })
      .catch(err => {
        console.error('plan failed', err);
        if (!cancelled) setPlanError(String(err));
      })
      .finally(() => { if (!cancelled) setPlanning(false); });
    return () => { cancelled = true; };
  }, [props.framesSetId, dispositions, compression, selectedRootPath, rootsLoaded]);

  const sharedRoles = new Set(plan?.shared_calibrations.map(s => s.frame_role) ?? []);

  function rowFor(role: 'flats' | 'darks' | 'bias' | 'darkflats', label: string, frameRole: FrameRole) {
    const moveDisabled = sharedRoles.has(frameRole);
    const sharedCount = plan?.shared_calibrations.find(s => s.frame_role === frameRole)?.other_frames_set_ids.length ?? 0;
    return (
      <div key={role} className="flex items-center gap-4 py-1.5 border-b border-border/30 last:border-0">
        <span className="w-20 font-medium text-content">{label}</span>
        {(['move', 'copy', 'skip'] as const).map(opt => (
          <label
            key={opt}
            className={`flex items-center gap-1.5 ${
              moveDisabled && opt === 'move' ? 'opacity-40 cursor-not-allowed' : 'cursor-pointer text-content-secondary'
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
    if (!plan || !selectedRootPath) return;
    if (plan.conflicts.length > 0) {
      setShowConflict(true);
      return;
    }
    props.onStart(dispositions, compression, 'overwrite', selectedRootPath);
  }

  const canStart = !planning && !!plan && !!selectedRootPath && archiveRoots.length > 0;

  return (
    <>
      <DialogShell
        title="Archive frame set"
        size="md"
        onClose={props.onCancel}
        footer={
          <>
            <Button onClick={props.onCancel}>Cancel</Button>
            <Button variant="primary" onClick={handleStart} disabled={!canStart} data-autofocus>Start archiving</Button>
          </>
        }
      >
        <div className="space-y-3">
          <p>Lights are always moved. Choose what to do with calibration frames.</p>

          {archiveRoots.length === 0 ? (
            <p className="text-warning">
              No archive folders configured. Add one in File Manager → Archive Folders before archiving.
            </p>
          ) : archiveRoots.length === 1 ? (
            <p>
              Destination: <span className="font-mono text-content" title={archiveRoots[0].path}>{archiveRoots[0].path}</span>
            </p>
          ) : (
            <div>
              <label htmlFor="archive-dest" className="mb-1 block text-content-secondary">Destination archive folder</label>
              <Select
                id="archive-dest"
                value={selectedRootPath ?? ''}
                onChange={(e) => setSelectedRootPath(e.target.value)}
                className="w-full"
              >
                {archiveRoots.map((r) => (
                  <option key={r.id} value={r.path} title={r.path}>
                    {r.path}{r.is_default ? '  (default)' : ''}
                  </option>
                ))}
              </Select>
            </div>
          )}

          <div>
            <div className="mb-1 text-content-secondary">Calibrations</div>
            <div className="rounded border border-border bg-surface px-2.5">
              {rowFor('flats', 'Flats', 'flat')}
              {rowFor('darks', 'Darks', 'dark')}
              {rowFor('bias', 'Bias', 'bias')}
              {rowFor('darkflats', 'DarkFlats', 'darkflat')}
            </div>
          </div>

          <div>
            <label htmlFor="archive-compression" className="mb-1 block text-content-secondary">Compression</label>
            <Select
              id="archive-compression"
              value={compression}
              onChange={e => setCompression(e.target.value as ArchiveCompression)}
            >
              <option value="store">Store (no compression — fastest)</option>
              <option value="deflate">Deflate (smaller, slower)</option>
            </Select>
          </div>

          {planning && (
            <p role="status" className="flex items-center gap-2">
              <span className="inline-block w-3 h-3 border-2 border-accent border-t-transparent rounded-full animate-spin" />
              Computing plan…
            </p>
          )}

          {planError && (
            <p role="alert" className="break-words text-error">{planError}</p>
          )}

          {plan && !planning && (
            <div className="space-y-1.5 rounded border border-border bg-surface p-2.5">
              <p className="font-medium text-content">
                Estimated archive · {(plan.total_size_bytes / 1024 / 1024).toFixed(1)} MB
              </p>
              {plan.zips.length > 0 && (
                <ul className="max-h-40 space-y-1 overflow-y-auto">
                  {plan.zips.map(z => (
                    <li key={z.zip_filename} className="flex items-baseline gap-2">
                      <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-content-secondary" title={z.zip_filename}>{z.zip_filename}</span>
                      <span className="whitespace-nowrap">{z.file_count} file{z.file_count !== 1 ? 's' : ''}</span>
                      <span className="whitespace-nowrap">{(z.total_size_bytes / 1024 / 1024).toFixed(1)} MB</span>
                    </li>
                  ))}
                </ul>
              )}
              {plan.conflicts.length > 0 && (
                <p className="text-warning">
                  {plan.conflicts.length} of these zip name(s) already exist in the archive folder — you will be asked how to resolve before writing starts.
                </p>
              )}
            </div>
          )}
        </div>
      </DialogShell>

      {showConflict && plan && selectedRootPath && (
        <ArchiveConflictDialog
          conflicts={plan.conflicts}
          onCancel={() => setShowConflict(false)}
          onResolve={(resolution) => {
            setShowConflict(false);
            props.onStart(dispositions, compression, resolution, selectedRootPath);
          }}
        />
      )}
    </>
  );
}
