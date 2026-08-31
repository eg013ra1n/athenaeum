// Explicit app→app send (Phase 3) — enqueue a frame selection to one or more
// chosen Athenaeum destinations.
//
// A2 NOTE — account state proper (sign-in flow, device list, name editor) still
// lives ONLY in `useAccount` + `AccountSection`. This hook does NOT import either
// symbol, so the guard grep for `useAccount|AccountSection` stays clean. The send
// destinations are read directly via `api.invoke('list_account_devices')` +
// `api.invoke('account_status')` in `SendToNodeDialog` (the same offline-resolvable
// precedent `SyncSection` uses).
//
// This hook is the primitive-looper: it fans the single-destination
// `enqueue_sync_selection` command out over every chosen device, catching each
// call independently so one offline node never aborts the others. It does NOT
// notify — the calling dialog aggregates the per-destination results into a single
// outcome notification.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { ExportMode } from '../types/export';
import type {
  EnqueueSelectionResult,
  FlatNormMode,
  IneligibleFrame,
  LightCalParams,
} from '../types/models';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
export function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Summarize the ineligible frames into a short, honest reason line for the
 * outcome notification. Reasons are the backend's verbatim strings (e.g. "file
 * missing on disk", "frame not found in catalog"); we group identical reasons and
 * count them, capping the list so a large mixed failure can't produce a giant
 * toast. Exported so `SendToNodeDialog` reuses the exact same aggregation.
 */
export function summarizeIneligible(ineligible: IneligibleFrame[]): string {
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

/** One destination's outcome — either the backend result or the transport error. */
export interface SendResult {
  deviceId: string;
  result?: EnqueueSelectionResult;
  error?: string;
}

/** Optional per-send naming/context (Transfers Status Model v2, §D1). A blank
 *  `batchName` is omitted so the backend auto-names (frame-set name / common-dir
 *  basename / "N files — date"); `frameSetId` enables the WBPP rel_path layout
 *  server-side for an Object-page send. */
export interface SendOptions {
  batchName?: string | null;
  frameSetId?: number | null;
}

/** Frame-set send (spec 2026-08-28): the export mode + the light-calibration
 *  preferences. They no longer feed the readiness gate (readiness is
 *  input-shaped now — masters built, lights linked); they are the options the
 *  calibrated-lights mode generates its files with. */
export interface FrameSetSendOptions {
  batchName?: string | null;
  flatNorm: boolean;
  flatNormMode: FlatNormMode;
  params: LightCalParams;
}

export interface UseSyncSend {
  /** An enqueue fan-out is in flight. */
  sending: boolean;
  /**
   * Enqueue `frameIds` to EACH `deviceId`. The backend command takes one
   * destination per call, so we loop it, catching each destination independently
   * — a per-destination failure is returned as `{ deviceId, error }` and does NOT
   * abort the other destinations. Returns the per-destination results; the caller
   * aggregates + notifies. `opts` carries the optional batch name + frame-set id.
   */
  sendSelection: (frameIds: number[], deviceIds: string[], opts?: SendOptions) => Promise<SendResult[]>;
  /**
   * Enqueue a whole frame set, resolved server-side by `mode`, to EACH `deviceId`.
   * Same fan-out contract as `sendSelection` — one destination per backend call,
   * each caught independently. `opts` carries the optional batch name plus the
   * light-calibration preferences the calibrated-lights mode generates with.
   */
  sendFrameSet: (
    frameSetId: number,
    mode: ExportMode,
    deviceIds: string[],
    opts: FrameSetSendOptions,
  ) => Promise<SendResult[]>;
}

export function useSyncSend(): UseSyncSend {
  const [sending, setSending] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const sendSelection = useCallback(
    async (frameIds: number[], deviceIds: string[], opts?: SendOptions): Promise<SendResult[]> => {
      if (frameIds.length === 0 || deviceIds.length === 0) return [];
      // Blank/whitespace name → omit so the backend auto-names (§D1).
      const trimmed = opts?.batchName?.trim();
      const batchName = trimmed ? trimmed : null;
      const frameSetId = opts?.frameSetId ?? null;
      setSending(true);
      try {
        // Fan out in parallel, each destination independently caught so one
        // offline/rejecting node can't take down the rest of the batch.
        return await Promise.all(
          deviceIds.map(async (deviceId): Promise<SendResult> => {
            try {
              const result = await api.invoke<EnqueueSelectionResult>('enqueue_sync_selection', {
                frameIds,
                destinationDeviceId: deviceId,
                batchName,
                frameSetId,
              });
              return { deviceId, result };
            } catch (err) {
              // Never swallow — log before returning the per-destination error.
              console.error('[sync] enqueue_sync_selection failed:', deviceId, err);
              return { deviceId, error: errMsg(err) };
            }
          }),
        );
      } finally {
        if (mounted.current) setSending(false);
      }
    },
    [],
  );

  const sendFrameSet = useCallback(
    async (
      frameSetId: number,
      mode: ExportMode,
      deviceIds: string[],
      opts: FrameSetSendOptions,
    ): Promise<SendResult[]> => {
      if (deviceIds.length === 0) return [];
      const trimmed = opts.batchName?.trim();
      const batchName = trimmed ? trimmed : null;
      setSending(true);
      try {
        return await Promise.all(
          deviceIds.map(async (deviceId): Promise<SendResult> => {
            try {
              const result = await api.invoke<EnqueueSelectionResult>('enqueue_frame_set_send', {
                frameSetId,
                mode,
                destinationDeviceId: deviceId,
                batchName,
                flatNorm: opts.flatNorm,
                flatNormMode: opts.flatNormMode,
                params: opts.params,
              });
              return { deviceId, result };
            } catch (err) {
              console.error('[sync] enqueue_frame_set_send failed:', deviceId, err);
              return { deviceId, error: errMsg(err) };
            }
          }),
        );
      } finally {
        if (mounted.current) setSending(false);
      }
    },
    [],
  );

  return { sending, sendSelection, sendFrameSet };
}
