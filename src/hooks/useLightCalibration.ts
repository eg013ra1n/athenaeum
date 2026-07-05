import { useState, useEffect, useCallback, useRef } from 'react';
import { api } from '../api';
import type { LightCalScope } from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';

/** Backend event payload for `calibration-progress` (snake_case on the wire —
 *  mirrors the master-build progress event shape). */
export interface CalibrationProgressEvent {
  set_id: number;
  frame_id: number;
  index: number;
  total: number;
  filename: string;
}

export interface CalibrationFailedFrame {
  frame_id: number;
  reason: string;
}

/** Backend event payload for `calibration-finished`. ALWAYS emitted exactly
 *  once per `start_light_calibration` run regardless of outcome. */
export interface CalibrationFinishedEvent {
  set_id: number;
  outcome: 'success' | 'partial' | 'cancelled' | 'error';
  ok_count: number;
  failed: CalibrationFailedFrame[];
}

export type LightCalState =
  | { phase: 'starting' }
  | { phase: 'running'; progress: CalibrationProgressEvent }
  | { phase: 'done'; result: CalibrationFinishedEvent };

/**
 * Tracks in-flight light-calibration runs by source frame-set id. Mirrors
 * `useMasterBuilds`' listener discipline exactly, but keyed by frame set rather
 * than calibration set. Mounted once at app root (see `LightCalibrationProvider`)
 * so the completion notification fires regardless of which page — or whether the
 * dialog — is still open.
 *
 * `startCalibration` fire-and-forgets the invoke; all state transitions arrive
 * via `calibration-progress` / `calibration-finished`. The ComputeQueue sidebar
 * indicator surfaces the running job independently.
 */
export function useLightCalibration() {
  const [calStates, setCalStates] = useState<Map<number, LightCalState>>(new Map());
  const { notify } = useNotifications();

  // Per-run dedupe tokens, keyed by frame-set id. Set when WE start a run so the
  // finish notification is deduped against a double-delivered event (StrictMode
  // / leaked listener) WITHOUT permanently suppressing later re-runs of the same
  // set. A finish for a run started elsewhere (e.g. the web backend, another
  // window) has no local token and falls back to a always-fresh key.
  const runTokensRef = useRef<Map<number, string>>(new Map());

  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenFinished: (() => void) | undefined;

    api
      .listen<CalibrationProgressEvent>('calibration-progress', (payload) => {
        if (cancelled) return;
        setCalStates(prev => new Map(prev).set(payload.set_id, { phase: 'running', progress: payload }));
      })
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[useLightCalibration] listen failed:', err));

    api
      .listen<CalibrationFinishedEvent>('calibration-finished', (payload) => {
        if (cancelled) return;
        setCalStates(prev => new Map(prev).set(payload.set_id, { phase: 'done', result: payload }));

        const runToken = runTokensRef.current.get(payload.set_id);
        runTokensRef.current.delete(payload.set_id);

        const { outcome, ok_count: okCount, failed } = payload;
        notify({
          title:
            outcome === 'cancelled'
              ? 'Light calibration cancelled'
              : outcome === 'error'
                ? 'Light calibration failed'
                : outcome === 'partial'
                  ? `Lights calibrated — ${failed.length} failed`
                  : `${okCount} light${okCount === 1 ? '' : 's'} calibrated`,
          detail:
            outcome === 'error'
              ? (failed[0]?.reason ?? 'No frames were calibrated.')
              : outcome === 'cancelled'
                ? `${okCount} calibrated before cancel`
                : failed.length > 0
                  ? `${okCount} written · ${failed.length} failed`
                  : 'Calibrated files written to the calibration library.',
          kind: 'calibration',
          hasErrors: outcome === 'error' || outcome === 'partial',
          tone: outcome === 'success' ? 'success' : outcome === 'cancelled' ? 'info' : 'warning',
          dedupeKey: `lightcal-${payload.set_id}-${runToken ?? Date.now()}`,
        });

        // Tracking rows changed — open views can refresh their readiness badges.
        window.dispatchEvent(new CustomEvent('light-cal-updated', { detail: { setId: payload.set_id } }));
      })
      .then((fn) => { if (cancelled) fn(); else unlistenFinished = fn; })
      .catch((err) => console.error('[useLightCalibration] listen failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenFinished?.();
    };
  }, [notify]);

  const startCalibration = useCallback(async (setId: number, scope: LightCalScope, flatNorm: boolean) => {
    const runToken = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    runTokensRef.current.set(setId, runToken);
    setCalStates(prev => new Map(prev).set(setId, { phase: 'starting' }));
    try {
      await api.invoke('start_light_calibration', { setId, scope, flatNorm });
    } catch (err) {
      // The invoke itself rejected — no `calibration-finished` event will arrive,
      // so reconcile the optimistic 'starting' state here. The caller surfaces the
      // error string inline (no notification for a start-time failure).
      runTokensRef.current.delete(setId);
      setCalStates(prev => new Map(prev).set(setId, {
        phase: 'done',
        result: { set_id: setId, outcome: 'error', ok_count: 0, failed: [{ frame_id: 0, reason: String(err) }] },
      }));
      throw err;
    }
  }, []);

  const cancelCalibration = useCallback(async (setId: number) => {
    try {
      await api.invoke('cancel_light_calibration', { setId });
    } catch {
      // May already have finished — safe to ignore.
    }
  }, []);

  const isCalibrating = useCallback((setId: number): boolean => {
    const s = calStates.get(setId);
    return !!s && s.phase !== 'done';
  }, [calStates]);

  return { calStates, startCalibration, cancelCalibration, isCalibrating };
}
