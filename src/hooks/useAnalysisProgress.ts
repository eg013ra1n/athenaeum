import { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../api';
import type { AnalysisProgressEvent, AnalysisCompleteEvent, AnalyzeFrameSetResult } from '../types/models';

export interface QueueItem {
  frameSetId: number;
  frameSetName?: string;
  force?: boolean;
}

export interface ActiveAnalysis {
  frameSetId: number;
  frameSetName?: string;
  progress: AnalysisProgressEvent | null;
  isComplete: boolean;
  isCancelling: boolean;
  result: AnalyzeFrameSetResult | null;
}

export function useAnalysisProgress() {
  const [queue, setQueue] = useState<QueueItem[]>([]);
  const [activeAnalyses, setActiveAnalyses] = useState<Map<number, ActiveAnalysis>>(new Map());
  const processingRef = useRef(false);
  const queueRef = useRef(queue);
  queueRef.current = queue;

  // The actual async work — reads from ref, not state
  const runNext = useCallback(async () => {
    if (processingRef.current) return;

    const next = queueRef.current[0];
    if (!next) return;

    processingRef.current = true;

    try {
      await api.invoke<AnalyzeFrameSetResult>('analyze_frame_set', {
        frameSetId: next.frameSetId,
        force: next.force ?? false,
      });
    } catch (err) {
      console.error(`Analysis failed for frame set ${next.frameSetId}:`, err);
      setActiveAnalyses(prev => {
        const updated = new Map(prev);
        const entry = updated.get(next.frameSetId);
        if (entry) {
          updated.set(next.frameSetId, {
            ...entry,
            isComplete: true,
            result: { analyzed: 0, skipped: 0, failed: 0, errors: [String(err)], cancelled: false },
          });
        }
        return updated;
      });
    }

    processingRef.current = false;
    setQueue(q => q.slice(1));
    // After state settles, React will re-render, the effect below will call runNext again
  }, []);

  // Kick the processor when queue has items and nothing is running
  useEffect(() => {
    if (queue.length > 0 && !processingRef.current) {
      runNext();
    }
  }, [queue, runNext]);

  // Listen for progress and completion events (once on mount)
  useEffect(() => {
    let unlistenProgress: (() => void) | null = null;
    let unlistenComplete: (() => void) | null = null;

    (async () => {
      unlistenProgress = await api.listen<AnalysisProgressEvent>('analysis-progress', (payload) => {
        setActiveAnalyses(prev => {
          const entry = prev.get(payload.frame_set_id);
          if (!entry) return prev;
          const updated = new Map(prev);
          updated.set(payload.frame_set_id, { ...entry, progress: payload });
          return updated;
        });
      });

      unlistenComplete = await api.listen<AnalysisCompleteEvent>('analysis-complete', (payload) => {
        setActiveAnalyses(prev => {
          const entry = prev.get(payload.frame_set_id);
          if (!entry) return prev;
          const updated = new Map(prev);
          updated.set(payload.frame_set_id, {
            ...entry,
            isComplete: true,
            isCancelling: false,
            result: {
              analyzed: payload.analyzed,
              skipped: payload.skipped,
              failed: payload.failed,
              errors: payload.errors,
              cancelled: payload.cancelled,
            },
          });
          return updated;
        });
      });
    })();

    return () => {
      unlistenProgress?.();
      unlistenComplete?.();
    };
  }, []);

  const enqueueAnalysis = useCallback((frameSetId: number, frameSetName?: string, force?: boolean) => {
    setActiveAnalyses(prev => {
      if (prev.has(frameSetId) && !prev.get(frameSetId)!.isComplete) return prev;
      const updated = new Map(prev);
      updated.set(frameSetId, {
        frameSetId,
        frameSetName,
        progress: null,
        isComplete: false,
        isCancelling: false,
        result: null,
      });
      return updated;
    });
    setQueue(q => {
      if (q.some(item => item.frameSetId === frameSetId)) return q;
      return [...q, { frameSetId, frameSetName, force }];
    });
  }, []);

  const cancelAnalysis = useCallback(async (frameSetId: number) => {
    setQueue(q => q.filter(item => item.frameSetId !== frameSetId));
    setActiveAnalyses(prev => {
      const entry = prev.get(frameSetId);
      if (!entry || entry.isComplete) return prev;
      const updated = new Map(prev);
      updated.set(frameSetId, { ...entry, isCancelling: true });
      return updated;
    });
    try {
      await api.invoke('cancel_analysis', { frameSetId });
    } catch {
      // May fail if already finished
    }
  }, []);

  const cancelAll = useCallback(async () => {
    const current = queueRef.current[0];
    setQueue([]);
    if (current) {
      try {
        await api.invoke('cancel_analysis', { frameSetId: current.frameSetId });
      } catch { /* ignore */ }
    }
    setActiveAnalyses(prev => {
      const updated = new Map(prev);
      for (const [, entry] of updated) {
        if (!entry.isComplete) {
          updated.set(entry.frameSetId, { ...entry, isCancelling: true });
        }
      }
      return updated;
    });
  }, []);

  const dismissCompleted = useCallback((frameSetId: number) => {
    setActiveAnalyses(prev => {
      const updated = new Map(prev);
      updated.delete(frameSetId);
      return updated;
    });
  }, []);

  const isAnalyzing = useCallback((frameSetId: number): boolean => {
    const entry = activeAnalyses.get(frameSetId);
    return !!entry && !entry.isComplete;
  }, [activeAnalyses]);

  const currentAnalysis = queue.length > 0 ? activeAnalyses.get(queue[0].frameSetId) ?? null : null;
  const queueLength = queue.length;
  const hasActiveAnalyses = Array.from(activeAnalyses.values()).some(a => !a.isComplete);

  return {
    enqueueAnalysis,
    cancelAnalysis,
    cancelAll,
    dismissCompleted,
    isAnalyzing,
    activeAnalyses,
    currentAnalysis,
    queueLength,
    hasActiveAnalyses,
  };
}
