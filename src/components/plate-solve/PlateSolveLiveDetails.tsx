import { lazy, Suspense, useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { api } from '../../api';
import type { FileWithFrame } from '../../types/models';
import type { ActivePlateSolveBatch } from '../../hooks/usePlateSolveQueue';
const BlinkViewer = lazy(() => import('../BlinkViewer'));

export function remainingSolveTime(
  startedAt: number | null,
  completed: number,
  total: number,
  now: number,
): string {
  if (startedAt === null || completed < 10 || now <= startedAt) return 'Calculating…';
  const seconds = Math.ceil(
    ((now - startedAt) / 1000 / completed) * Math.max(0, total - completed),
  );
  if (seconds < 60) return `~${seconds}s`;
  if (seconds < 3600) return `~${Math.ceil(seconds / 60)}m`;
  const minutes = Math.ceil(seconds / 60);
  return `~${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/** Uses completed throughput across workers, not the duration of the last file. */
export function PlateSolveLiveDetails({ batch }: { batch: ActivePlateSolveBatch }) {
  const [now, setNow] = useState(Date.now());
  const [blink, setBlink] = useState<FileWithFrame[] | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  const statuses = [...batch.frameStatuses.entries()];
  const solved = statuses.filter(([, s]) => s.kind === 'solved').length;
  const failed = statuses.filter(([, s]) => s.kind === 'failed').length;
  const timeouts = statuses.filter(([, s]) => s.kind === 'failed' && s.code === 'TIMEOUT').length;
  const active = statuses.filter(([, s]) => s.kind === 'solving');
  const openBlink = async (frameId: number) => {
    setLoading(true);
    setError('');
    try {
      const files = await api.invoke<FileWithFrame[]>('get_files_with_frames_by_ids', {
        frameIds: [frameId],
      });
      if (!files.length) throw Error('This file is no longer in the catalog.');
      setBlink(files);
    } catch (err) {
      console.error('Open solving frame failed:', err);
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };
  return (
    <div
      className="text-xs space-y-2 text-content-secondary"
      aria-label="Live plate-solving details"
    >
      <p>
        {solved} solved · {failed} failed{timeouts > 0 ? ` (${timeouts} timed out)` : ''}
      </p>
      <p title="Approximate, based on average completed frames per elapsed second; difficult fields may take longer.">
        Estimated remaining:{' '}
        {batch.isCancelling
          ? 'Cancelling…'
          : remainingSolveTime(batch.startedAt, solved + failed, batch.frameIds.length, now)}
      </p>
      <ul className="space-y-1 max-h-40 overflow-y-auto">
        {active.map(([id, status]) => (
          <li key={id} className="flex gap-2 items-center min-w-0">
            <button
              disabled={loading}
              onClick={() => openBlink(id)}
              title={`Blink ${status.kind === 'solving' ? (status.filename ?? `Frame #${id}`) : id}`}
              className="text-accent underline truncate text-left disabled:opacity-50"
            >
              {status.kind === 'solving' ? (status.filename ?? `Frame #${id}`) : id}
            </button>
            <span className="shrink-0">
              {status.kind === 'solving'
                ? Math.max(0, Math.floor((now - status.startedAt) / 1000))
                : 0}
              s
            </span>
          </li>
        ))}
      </ul>
      {error && (
        <p role="alert" className="text-error">
          {error}
        </p>
      )}
      {blink &&
        createPortal(
          <Suspense fallback={<div role="status">Opening Blink…</div>}>
            <BlinkViewer frames={blink} onClose={() => setBlink(null)} sourceType="light" />
          </Suspense>,
          document.body,
        )}
    </div>
  );
}
