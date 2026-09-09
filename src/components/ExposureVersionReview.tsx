import { lazy, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { FileWithFrame, VersionReview } from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';
import { ToolbarButton } from './Toolbar';
import { formatTimestamp } from '../utils/dateFormatting';

const BlinkViewer = lazy(() => import('./BlinkViewer'));

interface Props {
  objectIds: number[];
  onClose: () => void;
  onChanged: () => void;
}

/** Reviews only catalog metadata; confirmation changes relationships, never files. */
export function ExposureVersionReview({ objectIds, onClose, onChanged }: Props) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const [review, setReview] = useState<VersionReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [page, setPage] = useState(0);
  const [blink, setBlink] = useState<{ frames: FileWithFrame[]; index: number } | null>(null);
  const [opening, setOpening] = useState(false);
  const { notify } = useNotifications();
  const load = useCallback(async () => {
    const result = await api.invoke<VersionReview>('get_exposure_version_review', {
      framesSetIds: objectIds,
    });
    setReview(result);
  }, [objectIds]);
  useEffect(() => {
    let cancelled = false;
    setBusy(true);
    api
      .invoke<VersionReview>('get_exposure_version_review', { framesSetIds: objectIds })
      .then(value => {
        if (!cancelled) setReview(value);
      })
      .catch(err => {
        console.error('Exposure review load failed:', err);
        if (!cancelled) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [objectIds]);
  useEffect(() => {
    if (blink || opening) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !busy) {
        event.stopPropagation();
        onClose();
      }
    };
    window.addEventListener('keydown', close, true);
    return () => window.removeEventListener('keydown', close, true);
  }, [busy, onClose, blink, opening]);
  useEffect(() => {
    if (blink) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.focus();
    const trap = (event: KeyboardEvent) => {
      if (event.key !== 'Tab') return;
      const elements = Array.from(
        dialogRef.current?.querySelectorAll<HTMLElement>(
          'button:not(:disabled), select:not(:disabled), [tabindex="0"]',
        ) ?? [],
      );
      const first = elements[0];
      const last = elements[elements.length - 1];
      if (!first) {
        event.preventDefault();
        return;
      }
      if (
        event.shiftKey &&
        (document.activeElement === first || document.activeElement === dialogRef.current)
      ) {
        event.preventDefault();
        last.focus();
      } else if (
        !event.shiftKey &&
        (document.activeElement === last || document.activeElement === dialogRef.current)
      ) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener('keydown', trap, true);
    return () => {
      window.removeEventListener('keydown', trap, true);
      previous?.focus();
    };
  }, [blink]);
  const change = async (command: string, args: Record<string, unknown>) => {
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      await api.invoke(command, args);
      window.dispatchEvent(new Event('exposure-versions-changed'));
      await load();
      onChanged();
    } catch (err) {
      console.error('Exposure relationship update failed:', err);
      setError(String(err));
      notify({
        title: 'Exposure review update failed',
        detail: String(err),
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setBusy(false);
    }
  };
  const openBlink = async (ids: number[], startId = ids[0]) => {
    if (opening || busy || !ids.length) return;
    setOpening(true);
    setError('');
    try {
      const found = new Map<number, FileWithFrame>();
      for (let offset = 0; offset < ids.length; offset += 400) {
        const files = await api.invoke<FileWithFrame[]>('get_files_with_frames_by_ids', {
          frameIds: ids.slice(offset, offset + 400),
        });
        for (const file of files) if (file.frame?.id != null) found.set(file.frame.id, file);
      }
      if (ids.some(id => !found.has(id)))
        throw Error('Some files are no longer in the catalog. Close and reopen the review.');
      setBlink({ frames: ids.map(id => found.get(id)!), index: Math.max(0, ids.indexOf(startId)) });
    } catch (err) {
      console.error('Processing review Blink failed:', err);
      setError(String(err));
    } finally {
      setOpening(false);
    }
  };
  const allIds = review?.versions.map(v => v.frameId) ?? [];
  const unknownIds =
    review?.versions.filter(v => v.classification.stage === 'unknown').map(v => v.frameId) ?? [];
  const label = (id: number) =>
    `${review?.versions.find(v => v.frameId === id)?.filename ?? 'Frame'} (#${id})`;
  return (
    <div className="fixed inset-0 z-50 bg-surface/90 flex items-center justify-center p-4">
      <div
        ref={dialogRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-labelledby="exposure-review-title"
        className="w-full max-w-6xl max-h-[90vh] overflow-y-auto rounded-lg border border-border bg-surface-elevated p-4 space-y-4"
      >
        <div className="flex justify-between gap-3">
          <h3 id="exposure-review-title" className="text-lg font-semibold">
            Processing and exposure versions
          </h3>
          <ToolbarButton disabled={busy || opening} onClick={onClose}>
            Close review
          </ToolbarButton>
        </div>
        <p className="text-sm text-content-secondary">
          These are available files; an original raw file may be absent. Confirm a link only when
          both files represent the same exposure. Integrated products have multiple or unknown
          inputs and are excluded from single-exposure totals.
        </p>
        <p className="text-xs text-content-muted">
          Unknown means the stored header has no supported evidence of processing. It does not mean
          a bad image or prove that it is raw. A combined assessment may suggest likely uncalibrated
          data; this remains Unknown until you confirm Raw. Check the evidence before assigning a
          stage.
        </p>
        {review && (
          <div className="flex gap-3">
            <ToolbarButton
              disabled={busy || opening || !allIds.length}
              onClick={() => openBlink(allIds)}
            >
              Blink all ({allIds.length})
            </ToolbarButton>
            <ToolbarButton
              disabled={busy || opening || !unknownIds.length}
              onClick={() => openBlink(unknownIds)}
            >
              Blink Unknown ({unknownIds.length})
            </ToolbarButton>
          </div>
        )}
        {opening && <p role="status">Opening Blink…</p>}
        {busy && <p role="status">Updating review…</p>}
        {error && (
          <p role="alert" className="text-error">
            {error}
          </p>
        )}
        {review && (
          <>
            <p className="text-sm">
              {review.versions.length} available files · {review.exposureCount} exposure candidates
              after confirmed links · {(review.exposureSeconds / 3600).toFixed(2)} h. Unconfirmed
              matches are still counted separately.
            </p>
            <div className="overflow-x-auto">
              <table className="w-full text-sm text-left">
                <thead>
                  <tr className="text-content-muted">
                    <th>Available file</th>
                    <th>Observation metadata</th>
                    <th>Processing stage</th>
                    <th>Evidence / relationship</th>
                  </tr>
                </thead>
                <tbody>
                  {review.versions.slice(page * 50, (page + 1) * 50).map(v => (
                    <tr key={v.frameId} className="border-t border-border align-top">
                      <td className="p-2 max-w-xs break-all" title={v.path}>
                        <button
                          disabled={busy || opening}
                          className="text-accent underline text-left"
                          onClick={() => openBlink(allIds, v.frameId)}
                        >
                          {v.filename}
                        </button>
                        <br />
                        <span className="text-xs text-content-muted">
                          #{v.frameId} · {v.path}
                        </span>
                      </td>
                      <td className="p-2 text-xs">
                        <span title={v.dateObs ?? undefined}>
                          {v.dateObs ? formatTimestamp(v.dateObs) : 'Time unknown'}
                        </span>
                        <br />
                        {v.camera ?? 'Camera unknown'} · {v.filter ?? 'Filter unknown'}
                        <br />
                        {v.exposureSeconds ?? '?'} s · {v.width ?? '?'} × {v.height ?? '?'}
                      </td>
                      <td className="p-2">
                        <select
                          aria-label={`Processing stage for frame ${v.frameId}`}
                          disabled={busy || opening}
                          value={v.manualStage ? v.classification.stage : ''}
                          onChange={e =>
                            change('set_processing_stage', {
                              frameId: v.frameId,
                              stage: e.target.value || null,
                            })
                          }
                          className="bg-surface border border-border rounded p-1 text-content"
                        >
                          <option value="">
                            {v.manualStage
                              ? 'Use detected stage'
                              : `Detected: ${v.classification.stage}`}
                          </option>
                          {[
                            'unknown',
                            'raw',
                            'calibrated',
                            'debayered',
                            'registered',
                            'integrated',
                          ].map(stage => (
                            <option key={stage} value={stage}>
                              {stage} (user specified)
                            </option>
                          ))}
                        </select>
                        <p className="text-xs text-content-muted">{v.classification.confidence}</p>
                        {v.assessment && (
                          <div className="mt-2 space-y-1">
                            <p className="font-medium">{v.assessment.label}</p>
                            <p className="text-xs text-content-muted">{v.assessment.confidence}</p>
                            {!v.manualStage && v.assessment.candidateStage === 'raw' && (
                              <ToolbarButton
                                disabled={busy || opening}
                                onClick={() =>
                                  change('set_processing_stage', {
                                    frameId: v.frameId,
                                    stage: 'raw',
                                  })
                                }
                              >
                                Confirm Raw
                              </ToolbarButton>
                            )}
                          </div>
                        )}
                      </td>
                      <td className="p-2 text-xs space-y-1">
                        {(v.assessment?.evidence ?? v.classification.evidence).map((e, i) => (
                          <p key={i}>{e}</p>
                        ))}
                        {v.exposureId && (
                          <>
                            <p>Confirmed exposure group: {v.exposureId.slice(0, 8)}</p>
                            <ToolbarButton
                              disabled={busy || opening}
                              onClick={() =>
                                change('unlink_exposure_version', { frameId: v.frameId })
                              }
                            >
                              Unlink version
                            </ToolbarButton>
                          </>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {review.versions.length > 50 && (
              <div className="flex gap-2">
                <ToolbarButton disabled={page === 0} onClick={() => setPage(p => p - 1)}>
                  Previous files
                </ToolbarButton>
                <span>Page {page + 1}</span>
                <ToolbarButton
                  disabled={(page + 1) * 50 >= review.versions.length}
                  onClick={() => setPage(p => p + 1)}
                >
                  Next files
                </ToolbarButton>
              </div>
            )}
            <h4 className="font-semibold">Suggested links ({review.suggestionCount})</h4>
            {review.suggestionCount > 200 && (
              <p>
                Showing the first 200 suggestions. Confirmed groups will be consolidated on refresh.
              </p>
            )}
            {!review.suggestions.length && (
              <p className="text-sm text-content-muted">
                No unconfirmed matches with sufficient metadata evidence.
              </p>
            )}
            <div className="max-h-80 overflow-y-auto space-y-2">
              {review.suggestions.map(s => (
                <div
                  key={`${s.leftId}-${s.rightId}`}
                  className="rounded border border-border p-3 text-sm"
                >
                  <p>
                    {label(s.leftId)} ↔ {label(s.rightId)} · {s.confidence}
                  </p>
                  <ul className="text-xs text-content-muted list-disc pl-4 my-2">
                    {s.evidence.map((e, i) => (
                      <li key={i}>{e}</li>
                    ))}
                  </ul>
                  <ToolbarButton
                    disabled={busy || opening}
                    onClick={() =>
                      change('confirm_exposure_version_link', {
                        leftId: s.leftId,
                        rightId: s.rightId,
                      })
                    }
                  >
                    Confirm same exposure
                  </ToolbarButton>
                </div>
              ))}
            </div>
          </>
        )}
      </div>
      {blink && (
        <Suspense fallback={<p role="status">Opening Blink…</p>}>
          <BlinkViewer
            frames={blink.frames}
            initialIndex={blink.index}
            sourceType="light"
            onClose={() => setBlink(null)}
          />
        </Suspense>
      )}
    </div>
  );
}
