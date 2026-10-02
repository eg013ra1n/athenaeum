import { useCallback, useEffect, useRef, useState, type JSX } from 'react';
import { Loader2 } from 'lucide-react';
import { api } from '../../../api';
import { Button, DialogShell, Select } from '../../ui';
import type { CollabRemapFinished, CollabRemapProgress, FilterBucket, FilterBucketSheet } from '../../../types/models';

const HEADER = 'mb-1.5 mt-5 text-[13px] font-semibold text-content first:mt-0';
const bucketKey = (b: FilterBucket) => `${b.publisherAccountId}|${b.instrume}|${b.filterRaw}`;

function finishedText(f: CollabRemapFinished): string {
  if (f.error) return `Remap stopped after ${f.done} of ${f.total}: ${f.error}`;
  if (f.cancelled) return `Remap cancelled — ${f.done} of ${f.total} frames remapped to ${f.canonical}.`;
  const failed = f.failed > 0 ? ` (${f.failed} failed)` : '';
  return `${f.done} of ${f.total} frames remapped to ${f.canonical}${failed}.`;
}

export default function FilterRemapSection({ projectId, syncToken }: { projectId: string; syncToken?: number }): JSX.Element {
  const [sheet, setSheet] = useState<FilterBucketSheet | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [choice, setChoice] = useState<Record<string, string>>({});
  const [confirm, setConfirm] = useState<{ bucket: FilterBucket; canonical: string } | null>(null);
  const [running, setRunning] = useState<CollabRemapProgress | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [startError, setStartError] = useState<string | null>(null);

  // Request sequence: a response older than the latest request is ignored, so a
  // load started just before the finished event can never re-set `running`.
  const loadSeq = useRef(0);

  const load = useCallback(async () => {
    const seq = ++loadSeq.current;
    try {
      const s = await api.invoke<FilterBucketSheet>('get_collab_filter_buckets', { projectId });
      if (seq !== loadSeq.current) return;
      setSheet(s);
      setRunning(s.running);
      setLoadError(null);
    } catch (err) {
      console.error('[FilterRemapSection] get_collab_filter_buckets failed:', err);
      if (seq !== loadSeq.current) return;
      setLoadError(String(err));
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load, syncToken]);

  useEffect(() => {
    let cancelled = false;
    let unProgress: (() => void) | undefined;
    let unFinished: (() => void) | undefined;
    api.listen<CollabRemapProgress>('collab-remap-progress', (p) => {
      if (cancelled || p.projectId !== projectId) return;
      setRunning(p);
    })
      .then((fn) => { if (cancelled) fn(); else unProgress = fn; })
      .catch((err) => console.error('[FilterRemapSection] listen progress failed:', err));
    api.listen<CollabRemapFinished>('collab-remap-finished', (f) => {
      if (cancelled || f.projectId !== projectId) return;
      setRunning(null);
      setResult(finishedText(f));
      void load();
    })
      .then((fn) => { if (cancelled) fn(); else unFinished = fn; })
      .catch((err) => console.error('[FilterRemapSection] listen finished failed:', err));
    return () => {
      cancelled = true;
      unProgress?.();
      unFinished?.();
    };
  }, [projectId, load]);

  const start = useCallback(async () => {
    if (!confirm) return;
    const { bucket, canonical } = confirm;
    setConfirm(null);
    setResult(null);
    setStartError(null);
    try {
      await api.invoke('remap_collab_filter', { projectId, frameUuids: bucket.frameUuids, canonical });
    } catch (err) {
      console.error('[FilterRemapSection] remap_collab_filter failed:', err);
      setStartError(String(err));
    }
  }, [confirm, projectId]);

  const cancel = useCallback(async () => {
    try {
      await api.invoke('cancel_collab_remap', { projectId });
    } catch (err) {
      console.error('[FilterRemapSection] cancel_collab_remap failed:', err);
    }
  }, [projectId]);

  return (
    <>
      <h3 className={HEADER}>Filters</h3>
      <p className="mb-2 text-[12px] text-content-faint">
        Remap a publisher's raw filter name to a canonical filter. Stacking groups and exports follow on the next plan; no file changes.
      </p>
      {loadError && <p className="text-[12.5px] text-error">{loadError}</p>}
      {running && (
        <div className="mb-2 flex items-center gap-2 text-[12px] text-content">
          <Loader2 size={12} className="animate-spin" />
          Remapping… {running.done + running.failed} of {running.total}
          <Button onClick={() => void cancel()}>Cancel</Button>
        </div>
      )}
      {result && <p className="mb-2 text-[12px] text-content">{result}</p>}
      {startError && <p className="mb-2 text-[12.5px] text-error">{startError}</p>}
      {sheet && sheet.buckets.length === 0 && <p className="text-[12px] text-content-faint">No frames in this project yet.</p>}
      {sheet && sheet.buckets.length > 0 && (
        <table className="w-full text-[12px]">
          <thead className="text-left text-content-faint">
            <tr>
              <th className="py-1 font-medium">Publisher</th>
              <th className="py-1 font-medium">Camera</th>
              <th className="py-1 font-medium">Raw filter</th>
              <th className="py-1 font-medium">Canonical</th>
              <th className="py-1 text-right font-medium">Frames</th>
              <th className="py-1" />
            </tr>
          </thead>
          <tbody>
            {sheet.buckets.map((b) => {
              const k = bucketKey(b);
              const picked = choice[k] ?? b.canonical ?? '';
              const unchanged = picked === '' || picked === b.canonical;
              return (
                <tr key={k} className="border-t border-border">
                  <td className="py-1 text-content">{b.publisher}</td>
                  <td className="py-1 text-content">{b.instrume || 'Unknown camera'}</td>
                  <td className="py-1 text-content">{b.filterRaw || '(no FILTER)'}</td>
                  <td className="py-1 text-content">{b.canonical ?? 'mixed'}</td>
                  <td className="py-1 text-right tabular-nums text-content">{b.frameUuids.length}</td>
                  <td className="py-1 text-right">
                    <span className="inline-flex items-center gap-1.5">
                      <Select aria-label={`Canonical filter for ${b.filterRaw || 'no FILTER'}`} value={picked} onChange={(e) => setChoice((c) => ({ ...c, [k]: e.target.value }))}>
                        {b.canonical === null && <option value="">Choose…</option>}
                        {sheet.dictionary.map((d) => (
                          <option key={d.canonical} value={d.canonical}>{d.canonical}</option>
                        ))}
                      </Select>
                      <Button disabled={unchanged || running !== null} onClick={() => setConfirm({ bucket: b, canonical: picked })}>Apply</Button>
                    </span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      {confirm && (
        <DialogShell
          title="Remap filter"
          onClose={() => setConfirm(null)}
          footer={
            <>
              <Button onClick={() => setConfirm(null)}>Cancel</Button>
              <Button variant="primary" data-autofocus onClick={() => void start()}>Remap</Button>
            </>
          }
        >
          <p className="text-[12.5px] text-content">
            Remap {confirm.bucket.frameUuids.length} frames from '{confirm.bucket.filterRaw || '(no FILTER)'}' to {confirm.canonical}?
            Stacking groups and exports follow on the next plan; no file changes.
          </p>
        </DialogShell>
      )}
    </>
  );
}
