// "Send to primary" capability + action for the frame-selection toolbar (task M2b).
//
// A2 NOTE — account state proper (sign-in flow, device list, name editor) still
// lives ONLY in `useAccount` + `AccountSection` (see useAccount.ts header). This
// hook does NOT import either symbol, so the guard grep for
// `useAccount|AccountSection` stays clean.
//
// PHASE 1 — the app has no send UI yet (explicit app→app send is Phase 3, after
// the capability model lands). `canSend` is hard `false`, so the toolbar button
// never renders and no sync code runs on the normal path. The full hook shape
// (`canSend` / `sending` / `sendToPrimary`) is retained so Phase 3 can restore
// per-target gating without touching call sites.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type { EnqueueSelectionResult, IneligibleFrame } from '../types/models';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Summarize the ineligible frames into a short, honest reason line for the
 * outcome notification. Reasons are the backend's verbatim strings (e.g. "file
 * missing on disk", "frame not found in catalog"); we group identical reasons and
 * count them, capping the list so a large mixed failure can't produce a giant
 * toast.
 */
function summarizeIneligible(ineligible: IneligibleFrame[]): string {
  const counts = new Map<string, number>();
  for (const { reason } of ineligible) {
    counts.set(reason, (counts.get(reason) ?? 0) + 1);
  }
  const parts = [...counts.entries()].map(([reason, n]) => `${n} × ${reason}`);
  const MAX = 3;
  if (parts.length > MAX) {
    return `${parts.slice(0, MAX).join(', ')}, +${parts.length - MAX} more`;
  }
  return parts.join(', ');
}

export interface UseSyncSend {
  /** True only for a signed-in `capture` node — gates the toolbar button. */
  canSend: boolean;
  /** An enqueue is in flight. */
  sending: boolean;
  /** Enqueue the selection to the paired primary and notify the outcome. */
  sendToPrimary: (frameIds: number[]) => Promise<void>;
}

export function useSyncSend(): UseSyncSend {
  const { notify } = useNotifications();
  const [canSend, setCanSend] = useState(false);
  const [sending, setSending] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    // Phase 1: the app has no send UI. Explicit app→app send arrives in Phase 3;
    // until then the toolbar button never renders. The hook shape is kept so
    // Phase 3 can restore per-target gating without touching call sites.
    setCanSend(false);
    return () => {
      mounted.current = false;
    };
  }, []);

  const sendToPrimary = useCallback(
    async (frameIds: number[]): Promise<void> => {
      if (frameIds.length === 0 || sending) return;
      setSending(true);
      try {
        const res = await api.invoke<EnqueueSelectionResult>('enqueue_sync_selection', {
          frameIds,
        });
        const { enqueuedCount, eligibleCount, totalCount, ineligible } = res;

        if (enqueuedCount === 0) {
          // Nothing sent — every frame was ineligible. Actionable, warning tone.
          notify({
            title: `Queued 0 of ${totalCount} for sync`,
            detail: summarizeIneligible(ineligible) || 'No eligible frames to send.',
            kind: 'sync',
            tone: 'warning',
            hasErrors: true,
          });
        } else if (ineligible.length > 0) {
          // Partial — owner eligible-subset convention: "Queued N of M for sync".
          notify({
            title: `Queued ${eligibleCount} of ${totalCount} for sync`,
            detail: summarizeIneligible(ineligible),
            kind: 'sync',
            tone: 'warning',
          });
        } else {
          // Full success.
          notify({
            title: `Queued ${enqueuedCount} frame${enqueuedCount === 1 ? '' : 's'} for sync`,
            detail: 'Sending to your paired primary device.',
            kind: 'sync',
            tone: 'success',
          });
        }
      } catch (err) {
        // Pairing disabled / invalidated / transport failure → actionable warning.
        console.error('[sync] enqueue_sync_selection failed:', err);
        notify({
          title: 'Send to primary failed',
          detail: errMsg(err),
          kind: 'sync',
          tone: 'warning',
          hasErrors: true,
        });
      } finally {
        if (mounted.current) setSending(false);
      }
    },
    [notify, sending],
  );

  return { canSend, sending, sendToPrimary };
}
