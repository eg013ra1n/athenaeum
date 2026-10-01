import { useState } from 'react';
import { Ban, Loader2 } from 'lucide-react';
import { api } from '../../../api';
import { Button, DialogShell, TextArea } from '../../ui';
import type { FrameVM } from './frames';

const REASON_MAX = 500;

/**
 * Coordinator-only, reason-required exclusion of one or more frames
 * (spec 2026-09-30 Task 9). Calls `exclude_collab_frame` once per frame, in
 * order; a failure stops the loop, logs and reports how many succeeded
 * before it, and leaves the dialog open so the reason survives for a retry.
 * All frames succeeding closes the dialog. Reused by the frame panel (single
 * frame) and by the Published/Library table actions (Tasks 10, 11).
 */
export default function ExcludeDialog({
  projectId,
  frames,
  onClose,
  onDone,
}: {
  projectId: string;
  frames: FrameVM[];
  onClose: () => void;
  onDone: (excluded: number) => void;
}) {
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);

  const trimmed = reason.trim();
  // Unicode scalar values, not UTF-16 code units — matches the core's
  // `chars().count()` and the hub's count (an emoji is 1 char, not 2).
  const trimmedLength = [...trimmed].length;
  const valid = trimmedLength >= 1 && trimmedLength <= REASON_MAX;
  const tooLong = trimmedLength > REASON_MAX;

  const doExclude = async () => {
    setBusy(true);
    setResult(null);
    let excluded = 0;
    for (const frame of frames) {
      try {
        await api.invoke('exclude_collab_frame', {
          projectId,
          frameUuid: frame.frameUuid,
          reason: trimmed,
        });
        excluded += 1;
      } catch (err) {
        // Never swallow: log first, then stop and report the partial count
        // inline so the coordinator knows exactly how far it got.
        console.error('[exclude] failed:', err);
        const msg = err instanceof Error ? err.message : String(err);
        setResult(`Excluded ${excluded} of ${frames.length} — ${msg}`);
        setBusy(false);
        onDone(excluded);
        return;
      }
    }
    setBusy(false);
    onDone(excluded);
    onClose();
  };

  return (
    <DialogShell
      title={<span className="inline-flex items-center gap-2"><Ban size={14} className="text-error" />Exclude {frames.length} {frames.length === 1 ? 'frame' : 'frames'} from the project</span>}
      size="sm"
      onClose={onClose}
      busy={busy}
      footer={<>
        <Button onClick={onClose} disabled={busy}>Cancel</Button>
        <Button variant="dangerPrimary" onClick={() => void doExclude()} disabled={!valid || busy}>
          {busy && <Loader2 size={12} className="animate-spin" />}Exclude
        </Button>
      </>}
    >
      <p className="mb-2 text-[12px] text-content-muted">
        Excluded frames stop counting toward the project and are no longer exchanged. You can restore
        them from the frame&apos;s panel.
      </p>
      <TextArea
        value={reason}
        onChange={(e) => setReason(e.target.value)}
        rows={4}
        data-autofocus
        placeholder="Why are these frames excluded?"
        className="w-full resize-none"
      />
      <div className="mt-1 flex items-center justify-between text-[11px]">
        <span className={tooLong ? 'text-error' : 'text-content-faint'}>
          {trimmedLength} / {REASON_MAX}
        </span>
      </div>
      {result && <p className="mt-2 text-[12px] text-error">{result}</p>}
    </DialogShell>
  );
}
