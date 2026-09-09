import { FileLocationActions } from '../FileLocationActions';
import { lazy, Suspense, useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { api } from '../../api';
import type { SolveAttempt } from '../../types/plate-solve';
import type { FileWithFrame } from '../../types/models';
import { usePlateSolveProgressContext } from '../../contexts/PlateSolveProgressContext';
import { ToolbarButton } from '../Toolbar';
const BlinkViewer = lazy(() => import('../BlinkViewer'));

/** Latest non-cancelled attempt follows the frame's catalog identity. */
export function SolveFailureBadge({ frameId }: { frameId?: number | null }) {
  const [attempt, setAttempt] = useState<SolveAttempt | null>(null);
  const { activeBatches } = usePlateSolveProgressContext();
  const completed = [...activeBatches.values()].filter(b => b.isComplete).length;
  useEffect(() => {
    let cancelled = false;
    setAttempt(null);
    if (frameId != null)
      api
        .invoke<SolveAttempt[]>('get_plate_solve_attempts', {
          frameIds: [frameId],
          failedOnly: true,
        })
        .then(rows => {
          if (!cancelled) setAttempt(rows[0] ?? null);
        })
        .catch(error => console.error('Could not load solve label:', error));
    return () => {
      cancelled = true;
    };
  }, [frameId, completed]);
  if (!attempt) return null;
  return (
    <div className="px-4 py-1 bg-error-muted text-error text-xs" role="status">
      {attempt.code === 'TIMEOUT' ? 'Plate solve timed out' : 'Plate solve failed'} ·{' '}
      {new Date(attempt.attemptedAt).toLocaleString()} · {attempt.error}
    </div>
  );
}

export function FailedSolvesReview({ onClose }: { onClose: () => void }) {
  const [rows, setRows] = useState<SolveAttempt[]>([]);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [blink, setBlink] = useState<FileWithFrame[] | null>(null);
  const [blinkIndex, setBlinkIndex] = useState(0);
  const [opening, setOpening] = useState(false);
  const [page, setPage] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const { activeBatches, enqueuePlateSolve, hasActiveBatches } = usePlateSolveProgressContext();
  const completed = [...activeBatches.values()].filter(b => b.isComplete).length;
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError('');
    api
      .invoke<SolveAttempt[]>('get_plate_solve_attempts', { frameIds: [], failedOnly: true })
      .then(value => {
        if (!cancelled) {
          setRows(value);
          setPage(0);
        }
      })
      .catch(err => {
        console.error('Failed solve review:', err);
        if (!cancelled) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [completed, refresh]);
  const timedOut = rows.filter(row => row.code === 'TIMEOUT');
  const openBlink = async (id?: number) => {
    setOpening(true);
    setError('');
    try {
      // Fetch in bounded chunks, then preserve review order across all pages.
      const found = new Map<number, FileWithFrame>();
      for (let offset = 0; offset < rows.length; offset += 400) {
        const files = await api.invoke<FileWithFrame[]>('get_files_with_frames_by_ids', {
          frameIds: rows.slice(offset, offset + 400).map(row => row.frameId),
        });
        for (const file of files) if (file.frame?.id != null) found.set(file.frame.id, file);
      }
      const files = rows.flatMap(row => (found.has(row.frameId) ? [found.get(row.frameId)!] : []));
      if (!files.length) throw Error('These frames are no longer in the catalog.');
      if (id != null && !found.has(id))
        throw Error('The selected frame is no longer in the catalog. Refresh the review.');
      setBlinkIndex(id == null ? 0 : files.findIndex(file => file.frame?.id === id));
      setBlink(files);
    } catch (err) {
      console.error('Failed to open frames:', err);
      setError(String(err));
    } finally {
      setOpening(false);
    }
  };
  return createPortal(
    <div className="fixed inset-0 z-50 bg-overlay/70 flex items-center justify-center p-4">
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="failed-solves-title"
        className="w-full max-w-5xl max-h-[90vh] overflow-auto bg-surface-elevated text-content rounded-lg p-5 space-y-4"
      >
        <div className="flex justify-between gap-4">
          <h2 id="failed-solves-title" className="text-lg font-semibold">
            Failed solves ({rows.length})
          </h2>
          <ToolbarButton onClick={onClose}>Close</ToolbarButton>
        </div>
        <p className="text-sm text-content-muted">
          All cataloged frames whose latest completed attempt failed. Labels survive restarting and
          regrouping. A successful retry clears the failure; cancelling keeps the previous result.
        </p>
        <div className="flex gap-3">
          <ToolbarButton disabled={loading || opening} onClick={() => setRefresh(x => x + 1)}>
            Refresh
          </ToolbarButton>
          <ToolbarButton disabled={loading || opening || !rows.length} onClick={() => openBlink()}>
            {opening ? 'Opening Blink…' : `Blink all failed (${rows.length})`}
          </ToolbarButton>
          <ToolbarButton
            disabled={loading || !!error || !rows.length || hasActiveBatches}
            onClick={() =>
              enqueuePlateSolve(
                rows.map(r => r.frameId),
                'Retry failed frames',
              )
            }
          >
            Retry all failed ({rows.length})
          </ToolbarButton>
          <ToolbarButton
            disabled={loading || !!error || !timedOut.length || hasActiveBatches}
            onClick={() =>
              enqueuePlateSolve(
                timedOut.map(r => r.frameId),
                'Retry timed out sequentially',
                true,
              )
            }
          >
            Retry timed out sequentially ({timedOut.length})
          </ToolbarButton>
        </div>
        <p className="text-xs text-content-muted">
          Sequential retry tries each timed-out frame once, one at a time, using your configured
          time limit. You can cancel the batch.
        </p>
        {loading && <p role="status">Loading solve labels…</p>}
        {error && (
          <p role="alert" className="text-error">
            {error}
          </p>
        )}
        {!loading && !error && !rows.length && <p>No failed solves recorded.</p>}
        <ul className="space-y-3">
          {rows.slice(page * 50, (page + 1) * 50).map(row => (
            <li key={row.frameId} className="border border-border rounded p-3 space-y-1">
              <div className="flex gap-3 justify-between">
                <button
                  disabled={opening}
                  className="text-accent underline text-left break-all disabled:opacity-50"
                  onClick={() => openBlink(row.frameId)}
                  title={`Blink ${row.filename}`}
                >
                  {row.filename}
                </button>
                <span className="text-error shrink-0">
                  {row.code === 'TIMEOUT' ? 'Timed out' : 'Failed'}
                </span>
              </div>
              <p className="text-xs text-content-muted break-all">{row.path}</p>
              <FileLocationActions compact paths={[row.path]} />
              <p className="text-sm">{row.error ?? row.code ?? 'No solution'}</p>
              <p className="text-xs text-content-muted">
                {new Date(row.attemptedAt).toLocaleString()} · Frame #{row.frameId}
              </p>
            </li>
          ))}
        </ul>
        {rows.length > 50 && (
          <div className="flex gap-4 items-center">
            <ToolbarButton disabled={!page} onClick={() => setPage(x => x - 1)}>
              Previous
            </ToolbarButton>
            <span>
              {page + 1} / {Math.ceil(rows.length / 50)}
            </span>
            <ToolbarButton
              disabled={(page + 1) * 50 >= rows.length}
              onClick={() => setPage(x => x + 1)}
            >
              Next
            </ToolbarButton>
          </div>
        )}
      </div>
      {blink && (
        <Suspense fallback={<p role="status">Opening Blink…</p>}>
          <BlinkViewer
            frames={blink}
            initialIndex={blinkIndex}
            sourceType="light"
            onClose={() => setBlink(null)}
          />
        </Suspense>
      )}
    </div>,
    document.body,
  );
}
