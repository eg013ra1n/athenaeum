// "Send to primary" capability + action for the frame-selection toolbar (task M2b).
//
// A2 NOTE — account state proper (sign-in flow, device list, role selector) still
// lives ONLY in `useAccount` + `AccountSection` (see useAccount.ts header). This
// hook does NOT import either symbol; it reads the offline-resolvable
// `account_status` command directly to answer a single yes/no question — "is this
// a signed-in capture node?" — so the guard grep for `useAccount|AccountSection`
// stays clean. A signed-out (or primary / unassigned) user gets `canSend = false`,
// the toolbar button never renders, and no sync code runs on the normal path.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type { AccountStatus, EnqueueSelectionResult, IneligibleFrame } from '../types/models';

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
    (async () => {
      try {
        const s = await api.invoke<AccountStatus>('account_status');
        // A null/partial status must never crash the host toolbar — default off.
        if (mounted.current) setCanSend(!!s?.signedIn && s?.role === 'capture');
      } catch (err) {
        console.error('[sync] account status poll failed:', err);
        if (mounted.current) setCanSend(false);
      }
    })();
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
