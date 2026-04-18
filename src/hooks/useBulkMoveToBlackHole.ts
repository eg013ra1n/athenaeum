import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { BulkMoveProgressEvent, BulkMoveResult } from '../types/models';

export interface BulkMoveState {
  isRunning: boolean;
  progress: BulkMoveProgressEvent | null;
  result: BulkMoveResult | null;
  error: string | null;
}

/**
 * Invokes `bulk_move_to_black_hole` and tracks progress via the
 * `bulk-move-to-black-hole-progress` event. The progress listener is attached
 * only while a batch is in flight to avoid leaking subscriptions.
 */
export function useBulkMoveToBlackHole() {
  const [state, setState] = useState<BulkMoveState>({
    isRunning: false,
    progress: null,
    result: null,
    error: null,
  });
  const unlistenRef = useRef<(() => void) | null>(null);

  const cleanupListener = useCallback(() => {
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }
  }, []);

  useEffect(() => cleanupListener, [cleanupListener]);

  const start = useCallback(
    async (fileIds: number[], fromWhere: string = 'duplicates'): Promise<BulkMoveResult> => {
      if (fileIds.length === 0) {
        const empty: BulkMoveResult = { moved: 0, failed: [] };
        setState({ isRunning: false, progress: null, result: empty, error: null });
        return empty;
      }

      setState({ isRunning: true, progress: null, result: null, error: null });

      // Subscribe to progress before invoking so we don't miss early events.
      const unlisten = await api.listen<BulkMoveProgressEvent>(
        'bulk-move-to-black-hole-progress',
        (payload) => {
          setState((prev) => ({ ...prev, progress: payload }));
        },
      );
      unlistenRef.current = unlisten;

      try {
        const result = await api.invoke<BulkMoveResult>('bulk_move_to_black_hole', {
          fileIds,
          fromWhere,
        });
        setState((prev) => ({ ...prev, isRunning: false, result }));
        return result;
      } catch (err) {
        const msg = typeof err === 'string' ? err : (err as Error).message ?? String(err);
        console.error('bulk_move_to_black_hole failed:', err);
        setState((prev) => ({ ...prev, isRunning: false, error: msg }));
        throw err;
      } finally {
        cleanupListener();
      }
    },
    [cleanupListener],
  );

  const reset = useCallback(() => {
    setState({ isRunning: false, progress: null, result: null, error: null });
  }, []);

  return { ...state, start, reset };
}
